//! WebSocket-клиент для OpenAI Realtime Translation API.
//!
//! Endpoint и event-имена сверены с official cookbook:
//! <https://developers.openai.com/cookbook/examples/voice_solutions/realtime_translation_guide>
//!
//! Контракт:
//! - URL: wss://api.openai.com/v1/realtime/translations?model=gpt-realtime-translate
//! - Auth: Bearer ${OPENAI_API_KEY}
//! - Client → server:
//!   - `session.update` с `session.audio.output.language = "<target>"`
//!   - `session.input_audio_buffer.append` с base64 24 kHz PCM16 mono
//! - Server → client:
//!   - `session.output_audio.delta` (base64 24 kHz PCM16 mono)
//!   - `session.output_transcript.delta` (incremental text)
//!   - `session.input_transcript.delta` (source language, для дебага)
//!   - `session.closed`
//!   - `error`
//!
//! Все неизвестные event.type ловим в `Unknown` и логируем без падения — кукбук может
//! ввести новые поля, парсер должен оставаться устойчивым.

use std::future::Future;
use std::time::Duration;
use std::time::Instant;

use anyhow::anyhow;
use async_trait::async_trait;
use base64::Engine;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use http::header::AUTHORIZATION;
use http::HeaderValue;
use serde::Deserialize;
use serde_json::json;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{protocol::WebSocketConfig, Message};
use tokio_tungstenite::{connect_async_with_config, MaybeTlsStream, WebSocketStream};

use crate::domain::{
    RealtimeInputNoiseReduction, RealtimeTranslationConfig, RealtimeTranslationError,
    RealtimeTranslationErrorKind, RealtimeTranslationEvent, RealtimeTranslationFactory,
    RealtimeTranslationSession,
};

const OPENAI_REALTIME_TRANSLATION_URL: &str =
    "wss://api.openai.com/v1/realtime/translations?model=gpt-realtime-translate";
const OPENAI_EVENT_QUEUE_CAPACITY: usize = 128;
const WS_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const SESSION_READY_TIMEOUT: Duration = Duration::from_secs(10);
const WS_SEND_TIMEOUT: Duration = Duration::from_secs(5);
const WS_FORCE_CLOSE_TIMEOUT: Duration = Duration::from_secs(1);
const WS_MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
const WS_MAX_WRITE_BUFFER_BYTES: usize = 4 * 1024 * 1024;

fn realtime_websocket_config() -> WebSocketConfig {
    WebSocketConfig {
        max_write_buffer_size: WS_MAX_WRITE_BUFFER_BYTES,
        max_message_size: Some(WS_MAX_MESSAGE_BYTES),
        max_frame_size: Some(WS_MAX_MESSAGE_BYTES),
        ..WebSocketConfig::default()
    }
}

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
type WsSink = SplitSink<WsStream, Message>;
type WsSource = SplitStream<WsStream>;

/// Owns transport resources until `connect()` can publish them to the client.
/// Dropping a pending `connect()` must cancel the reader instead of detaching it.
struct PendingConnection {
    sink: Option<WsSink>,
    reader_task: Option<JoinHandle<()>>,
}

impl PendingConnection {
    fn new(sink: WsSink, reader_task: JoinHandle<()>) -> Self {
        Self {
            sink: Some(sink),
            reader_task: Some(reader_task),
        }
    }

    fn sink_mut(&mut self) -> &mut WsSink {
        self.sink
            .as_mut()
            .expect("pending connection must own its websocket sink")
    }

    async fn abort(mut self) {
        if let Some(task) = self.reader_task.take() {
            task.abort();
            let _ = task.await;
        }
        self.sink.take();
    }

    fn into_parts(mut self) -> (WsSink, JoinHandle<()>) {
        let sink = self
            .sink
            .take()
            .expect("pending connection must own its websocket sink");
        let reader_task = self
            .reader_task
            .take()
            .expect("pending connection must own its reader task");
        (sink, reader_task)
    }
}

impl Drop for PendingConnection {
    fn drop(&mut self) {
        if let Some(task) = self.reader_task.take() {
            task.abort();
        }
    }
}

/// Defensive parser для server events.
/// `#[serde(other)]` ловит любые event.type, которые мы пока не моделируем — без падения.
#[derive(Deserialize, Debug)]
#[serde(tag = "type")]
enum ServerEvent {
    #[serde(rename = "session.created")]
    SessionCreated {},
    #[serde(rename = "session.updated")]
    SessionUpdated {},
    #[serde(rename = "session.output_audio.delta")]
    OutputAudioDelta {
        #[serde(default)]
        delta: String,
        #[serde(default)]
        audio: String,
    },
    #[serde(rename = "session.output_transcript.delta")]
    OutputTranscriptDelta {
        #[serde(default)]
        delta: String,
    },
    #[serde(rename = "session.input_transcript.delta")]
    InputTranscriptDelta {
        #[serde(default)]
        delta: String,
    },
    #[serde(rename = "session.closed")]
    SessionClosed {},
    #[serde(rename = "error")]
    Error {
        #[serde(default)]
        error: ServerErrorBody,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize, Debug, Default)]
struct ServerErrorBody {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: String,
    #[serde(default, rename = "type")]
    err_type: Option<String>,
}

/// OpenAI realtime translation client.
///
/// Жизненный цикл:
/// 1. `new()`
/// 2. `connect(config)` — открывает WS, отправляет `session.update` и ждёт подтверждение.
/// 3. `append_pcm16()` — отправка input chunk'ов.
/// 4. `finish(drain_timeout)` — отправляет `session.close`, ждёт `session.closed` или таймаут.
/// 5. `abort()` — hard cleanup (если connect не успел или мы хотим без drain).
pub struct OpenAIRealtimeTranslationClient {
    endpoint: String,
    connect_timeout: Duration,
    session_ready_timeout: Duration,
    send_timeout: Duration,
    force_close_timeout: Duration,
    target_language: Option<String>,
    sink: Option<WsSink>,
    reader_task: Option<JoinHandle<()>>,
    /// Канал из reader_task в верхний слой. Receiver отдаётся через connect(); Sender хранится здесь
    /// чтобы close() мог послать synthetic Closed для случая когда WS оборвался без `session.closed`.
    event_tx: Option<mpsc::Sender<RealtimeTranslationEvent>>,
}

impl OpenAIRealtimeTranslationClient {
    pub fn new() -> Self {
        Self::with_endpoint(OPENAI_REALTIME_TRANSLATION_URL)
    }

    /// Overrides only the transport destination. This keeps the protocol adapter testable
    /// against a local WebSocket server without global environment state.
    pub fn with_endpoint(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            connect_timeout: WS_CONNECT_TIMEOUT,
            session_ready_timeout: SESSION_READY_TIMEOUT,
            send_timeout: WS_SEND_TIMEOUT,
            force_close_timeout: WS_FORCE_CLOSE_TIMEOUT,
            target_language: None,
            sink: None,
            reader_task: None,
            event_tx: None,
        }
    }

    /// Overrides bounded transport waits for deterministic fault tests and constrained hosts.
    pub fn with_timeouts(
        mut self,
        connect_timeout: Duration,
        session_ready_timeout: Duration,
        send_timeout: Duration,
        force_close_timeout: Duration,
    ) -> Self {
        self.connect_timeout = connect_timeout;
        self.session_ready_timeout = session_ready_timeout;
        self.send_timeout = send_timeout;
        self.force_close_timeout = force_close_timeout;
        self
    }

    pub fn target_language(&self) -> Option<&str> {
        self.target_language.as_deref()
    }

    /// Returns only after OpenAI confirms the configuration with `session.updated`.
    pub async fn connect(
        &mut self,
        config: RealtimeTranslationConfig,
    ) -> Result<mpsc::Receiver<RealtimeTranslationEvent>, RealtimeTranslationError> {
        if self.sink.is_some() || self.reader_task.is_some() || self.event_tx.is_some() {
            return Err(RealtimeTranslationError::Connection(
                "realtime translation client already has an active session".to_string(),
            ));
        }
        if config.credential.trim().is_empty() {
            return Err(RealtimeTranslationError::Authentication(
                "OPENAI_API_KEY не задан".to_string(),
            ));
        }
        if config.target_language.trim().is_empty() {
            return Err(RealtimeTranslationError::Protocol(
                "target language must not be empty".to_string(),
            ));
        }

        let mut req = self
            .endpoint
            .as_str()
            .into_client_request()
            .map_err(|e| RealtimeTranslationError::Internal(format!("invalid url: {}", e)))?;
        let auth_value = build_authorization_header_value(&config.credential)?;
        req.headers_mut().insert(AUTHORIZATION, auth_value);

        log::info!(
            "Connecting to OpenAI realtime translation: target_language={}",
            config.target_language
        );
        let handshake_started = Instant::now();

        let connect = connect_async_with_config(req, Some(realtime_websocket_config()), false);
        let (ws, _resp) = match timeout(self.connect_timeout, connect).await {
            Ok(Ok(pair)) => pair,
            Ok(Err(err)) => {
                let mapped = map_connect_error(&err);
                return Err(mapped);
            }
            Err(_) => {
                return Err(RealtimeTranslationError::Timeout(format!(
                    "WebSocket connect timed out after {} ms",
                    self.connect_timeout.as_millis()
                )));
            }
        };

        let (sink, source) = ws.split();

        let (tx, rx) = mpsc::channel::<RealtimeTranslationEvent>(OPENAI_EVENT_QUEUE_CAPACITY);
        let (ready_tx, ready_rx) = oneshot::channel();
        let reader_tx = tx.clone();
        let reader_task = tokio::spawn(async move {
            run_reader(source, reader_tx, ready_tx).await;
        });
        let mut pending = PendingConnection::new(sink, reader_task);

        let noise_reduction = match config.input_noise_reduction {
            RealtimeInputNoiseReduction::Disabled => serde_json::Value::Null,
            RealtimeInputNoiseReduction::NearField => json!({ "type": "near_field" }),
            RealtimeInputNoiseReduction::FarField => json!({ "type": "far_field" }),
        };
        let session_update = json!({
            "type": "session.update",
            "session": {
                "audio": {
                    "input": {
                        "transcription": { "model": "gpt-realtime-whisper" },
                        "noise_reduction": noise_reduction
                    },
                    "output": {
                        "language": config.target_language
                    }
                }
            }
        });
        if let Err(error) = await_ws_operation(
            pending
                .sink_mut()
                .send(Message::Text(session_update.to_string())),
            self.send_timeout,
            "session.update send",
        )
        .await
        {
            pending.abort().await;
            return Err(error);
        }

        let ready_result = timeout(self.session_ready_timeout, ready_rx).await;
        match ready_result {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(error))) => {
                pending.abort().await;
                return Err(error);
            }
            Ok(Err(_)) => {
                pending.abort().await;
                return Err(RealtimeTranslationError::Connection(
                    "OpenAI reader stopped before session.updated".to_string(),
                ));
            }
            Err(_) => {
                pending.abort().await;
                return Err(RealtimeTranslationError::Timeout(format!(
                    "session.updated was not received within {} ms",
                    self.session_ready_timeout.as_millis()
                )));
            }
        }

        let (sink, reader_task) = pending.into_parts();
        self.sink = Some(sink);
        self.reader_task = Some(reader_task);
        self.event_tx = Some(tx);
        self.target_language = Some(config.target_language);
        log::info!("OpenAI realtime translation: session.updated confirmed");
        log::info!(
            "openai_realtime_handshake target_language={} duration_ms={}",
            self.target_language.as_deref().unwrap_or("unknown"),
            handshake_started.elapsed().as_millis()
        );
        Ok(rx)
    }

    /// Отправка чанка PCM16 24 kHz mono. base64 кодирование внутри.
    pub async fn append_pcm16(&mut self, pcm16: &[i16]) -> Result<(), RealtimeTranslationError> {
        if pcm16.is_empty() {
            return Ok(());
        }
        let bytes: Vec<u8> = pcm16.iter().flat_map(|s| s.to_le_bytes()).collect();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let msg = json!({
            "type": "session.input_audio_buffer.append",
            "audio": b64,
        });

        let Some(sink) = self.sink.as_mut() else {
            return Err(RealtimeTranslationError::Connection(
                "WebSocket sink не инициализирован".to_string(),
            ));
        };
        await_ws_operation(
            sink.send(Message::Text(msg.to_string())),
            self.send_timeout,
            "audio send",
        )
        .await
    }

    /// Graceful close: посылаем `session.close`, ждём `session.closed` от сервера до таймаута.
    /// Если таймаут — закрываем WS принудительно и эмитим synthetic Closed event.
    pub async fn finish(
        &mut self,
        drain_timeout: Duration,
    ) -> Result<(), RealtimeTranslationError> {
        // 1. Translation session close. WS Close frame alone can skip the API-level
        // shutdown path and cut the final translated tail.
        if let Some(sink) = self.sink.as_mut() {
            let close_msg = json!({ "type": "session.close" });
            await_ws_operation(
                async {
                    sink.send(Message::Text(close_msg.to_string())).await?;
                    sink.flush().await
                },
                self.send_timeout,
                "session.close send",
            )
            .await?;
        }

        // 2. Ждём пока reader task завершится при `session.closed`/Close/ошибке.
        if let Some(task) = self.reader_task.take() {
            let mut task = task;
            tokio::select! {
                join_result = &mut task => {
                    if let Err(e) = join_result {
                        log::warn!("OpenAI realtime translation: reader task join failed: {}", e);
                    }
                }
                _ = sleep(drain_timeout) => {
                    log::warn!(
                        "OpenAI realtime translation: session.close timeout {} ms exceeded, closing websocket",
                        drain_timeout.as_millis()
                    );
                    if let Some(sink) = self.sink.as_mut() {
                        if timeout(self.force_close_timeout, async {
                            let _ = sink.send(Message::Close(None)).await;
                            let _ = sink.flush().await;
                        })
                        .await
                        .is_err()
                        {
                            log::warn!(
                                "OpenAI realtime translation: websocket force-close timed out after {} ms",
                                self.force_close_timeout.as_millis()
                            );
                        }
                    }
                    task.abort();
                    let _ = task.await;
                    if let Some(tx) = self.event_tx.as_ref() {
                        let _ = tx.try_send(RealtimeTranslationEvent::Closed);
                    }
                }
            }
        }

        // 3. Прибиваем sink
        self.sink = None;
        self.event_tx = None;
        self.target_language = None;
        log::info!("OpenAI realtime translation: closed");
        Ok(())
    }

    /// Жёсткий abort без drain. Используется когда start_translation упал на полпути.
    pub async fn abort(&mut self) {
        if let Some(task) = self.reader_task.take() {
            task.abort();
            let _ = task.await;
        }
        self.sink = None;
        if let Some(tx) = self.event_tx.take() {
            let _ = tx.try_send(RealtimeTranslationEvent::Closed);
        }
        self.target_language = None;
    }
}

impl Default for OpenAIRealtimeTranslationClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RealtimeTranslationSession for OpenAIRealtimeTranslationClient {
    async fn connect(
        &mut self,
        config: RealtimeTranslationConfig,
    ) -> Result<mpsc::Receiver<RealtimeTranslationEvent>, RealtimeTranslationError> {
        OpenAIRealtimeTranslationClient::connect(self, config).await
    }

    async fn append_pcm16(&mut self, samples: &[i16]) -> Result<(), RealtimeTranslationError> {
        OpenAIRealtimeTranslationClient::append_pcm16(self, samples).await
    }

    async fn finish(&mut self, timeout: Duration) -> Result<(), RealtimeTranslationError> {
        OpenAIRealtimeTranslationClient::finish(self, timeout).await
    }

    async fn abort(&mut self) {
        OpenAIRealtimeTranslationClient::abort(self).await
    }
}

pub struct OpenAIRealtimeTranslationFactory;

impl RealtimeTranslationFactory for OpenAIRealtimeTranslationFactory {
    fn create(&self) -> Box<dyn RealtimeTranslationSession> {
        Box::new(OpenAIRealtimeTranslationClient::new())
    }
}

impl Drop for OpenAIRealtimeTranslationClient {
    fn drop(&mut self) {
        if let Some(task) = self.reader_task.take() {
            task.abort();
        }
        self.event_tx = None;
    }
}

async fn await_ws_operation<T, E, F>(
    operation: F,
    operation_timeout: Duration,
    label: &str,
) -> Result<T, RealtimeTranslationError>
where
    F: Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    match timeout(operation_timeout, operation).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(err)) => Err(RealtimeTranslationError::Connection(format!(
            "{} failed: {}",
            label, err
        ))),
        Err(_) => Err(RealtimeTranslationError::Timeout(format!(
            "{} timed out after {} ms",
            label,
            operation_timeout.as_millis()
        ))),
    }
}

fn map_connect_error(err: &tokio_tungstenite::tungstenite::Error) -> RealtimeTranslationError {
    use tokio_tungstenite::tungstenite::Error as E;
    match err {
        E::Http(resp) => {
            let status = resp.status();
            let msg = format!("HTTP {} during WS handshake", status);
            if status.as_u16() == 401 || status.as_u16() == 403 {
                RealtimeTranslationError::Authentication(msg)
            } else if status.as_u16() == 429 {
                RealtimeTranslationError::RateLimited(msg)
            } else {
                RealtimeTranslationError::Connection(msg)
            }
        }
        E::Io(io) => RealtimeTranslationError::Connection(io.to_string()),
        E::Tls(t) => RealtimeTranslationError::Connection(t.to_string()),
        E::Url(u) => RealtimeTranslationError::Internal(u.to_string()),
        E::HttpFormat(hf) => RealtimeTranslationError::Internal(hf.to_string()),
        other => RealtimeTranslationError::Connection(other.to_string()),
    }
}

fn build_authorization_header_value(
    api_key: &str,
) -> Result<HeaderValue, RealtimeTranslationError> {
    HeaderValue::from_str(&format!("Bearer {}", api_key.trim()))
        .map_err(|e| RealtimeTranslationError::Internal(format!("invalid auth header: {}", e)))
}

struct ReaderHandshake {
    sender: Option<oneshot::Sender<Result<(), RealtimeTranslationError>>>,
    confirmed: bool,
}

impl ReaderHandshake {
    fn new(sender: oneshot::Sender<Result<(), RealtimeTranslationError>>) -> Self {
        Self {
            sender: Some(sender),
            confirmed: false,
        }
    }

    fn confirm(&mut self) -> bool {
        if self.confirmed {
            return true;
        }
        self.confirmed = true;
        self.sender
            .take()
            .map(|sender| sender.send(Ok(())).is_ok())
            .unwrap_or(false)
    }

    fn fail_startup(&mut self, error: RealtimeTranslationError) {
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(Err(error));
        }
    }
}

async fn run_reader(
    mut source: WsSource,
    tx: mpsc::Sender<RealtimeTranslationEvent>,
    ready_tx: oneshot::Sender<Result<(), RealtimeTranslationError>>,
) {
    let mut handshake = ReaderHandshake::new(ready_tx);
    while let Some(next) = source.next().await {
        match next {
            Ok(Message::Text(text)) => {
                if handle_server_text(&text, &tx, &mut handshake).await {
                    return;
                }
            }
            Ok(Message::Binary(bin)) => {
                log::debug!(
                    "OpenAI realtime translation: ignored binary message ({} bytes)",
                    bin.len()
                );
            }
            Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => {
                // tokio-tungstenite сам отвечает на ping
            }
            Ok(Message::Frame(_)) => {}
            Ok(Message::Close(_)) => {
                close_reader(
                    &tx,
                    &mut handshake,
                    "websocket closed before session.updated",
                )
                .await;
                return;
            }
            Err(e) => {
                fail_reader(
                    &tx,
                    &mut handshake,
                    RealtimeTranslationError::Connection(format!("ws stream error: {}", e)),
                )
                .await;
                return;
            }
        }
    }
    close_reader(
        &tx,
        &mut handshake,
        "websocket ended before session.updated",
    )
    .await;
}

async fn send_reader_event(
    tx: &mpsc::Sender<RealtimeTranslationEvent>,
    event: RealtimeTranslationEvent,
) -> bool {
    tx.send(event).await.is_err()
}

async fn close_reader(
    tx: &mpsc::Sender<RealtimeTranslationEvent>,
    handshake: &mut ReaderHandshake,
    startup_message: &str,
) {
    if handshake.confirmed {
        let _ = tx.send(RealtimeTranslationEvent::Closed).await;
    } else {
        handshake.fail_startup(RealtimeTranslationError::Connection(
            startup_message.to_string(),
        ));
    }
}

async fn fail_reader(
    tx: &mpsc::Sender<RealtimeTranslationEvent>,
    handshake: &mut ReaderHandshake,
    error: RealtimeTranslationError,
) {
    if handshake.confirmed {
        let _ = tx.send(RealtimeTranslationEvent::Failed(error)).await;
    } else {
        handshake.fail_startup(error);
    }
}

async fn handle_server_text(
    text: &str,
    tx: &mpsc::Sender<RealtimeTranslationEvent>,
    handshake: &mut ReaderHandshake,
) -> bool {
    match serde_json::from_str::<ServerEvent>(text) {
        Ok(ServerEvent::SessionCreated { .. }) => false,
        Ok(ServerEvent::SessionUpdated { .. }) => {
            if handshake.confirm() {
                false
            } else {
                log::debug!("OpenAI realtime translation: readiness receiver was dropped");
                true
            }
        }
        Ok(ServerEvent::OutputAudioDelta { delta, audio }) => {
            // Cookbook использует `delta`, но некоторые сборки могут отдавать `audio` —
            // принимаем оба, чтобы выживать в разных версиях.
            let payload = if !delta.is_empty() { &delta } else { &audio };
            if payload.is_empty() {
                return false;
            }
            match decode_pcm16_base64_payload(payload) {
                Ok(pcm16) => {
                    send_reader_event(
                        tx,
                        RealtimeTranslationEvent::TranslatedAudio {
                            pcm16,
                            sample_rate: 24_000,
                            channels: 1,
                        },
                    )
                    .await
                }
                Err(message) => {
                    fail_reader(tx, handshake, RealtimeTranslationError::Protocol(message)).await;
                    true
                }
            }
        }
        Ok(ServerEvent::OutputTranscriptDelta { delta }) => {
            if !delta.is_empty() {
                return send_reader_event(tx, RealtimeTranslationEvent::TranslatedTextDelta(delta))
                    .await;
            }
            false
        }
        Ok(ServerEvent::InputTranscriptDelta { delta }) => {
            if !delta.is_empty() {
                return send_reader_event(tx, RealtimeTranslationEvent::SourceTextDelta(delta))
                    .await;
            }
            false
        }
        Ok(ServerEvent::SessionClosed { .. }) => {
            close_reader(
                tx,
                handshake,
                "OpenAI session closed before session.updated",
            )
            .await;
            true
        }
        Ok(ServerEvent::Error { error }) => {
            let kind = classify_server_error(&error);
            let message = if error.message.is_empty() {
                error.err_type.unwrap_or_else(|| "unknown".to_string())
            } else {
                error.message
            };
            if let Some(code) = error.code.as_deref() {
                log::warn!(
                    "OpenAI realtime translation error included a server code (bytes={})",
                    code.len()
                );
            }
            fail_reader(tx, handshake, error_from_kind(kind, message)).await;
            true
        }
        Ok(ServerEvent::Unknown) => {
            log::debug!(
                "OpenAI realtime translation: ignored unknown server event (payload_bytes={})",
                text.len()
            );
            false
        }
        Err(e) => {
            let parse_context = json_parse_error_context(&e, text.len());
            let message = format!("invalid OpenAI realtime server event: {parse_context}");
            log::warn!(
                "OpenAI realtime translation: failed to parse server event ({})",
                parse_context
            );
            fail_reader(tx, handshake, RealtimeTranslationError::Protocol(message)).await;
            true
        }
    }
}

fn decode_pcm16_base64_payload(payload: &str) -> Result<Vec<i16>, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .map_err(|e| format!("audio base64 decode: {}", e))?;

    if bytes.len() % 2 != 0 {
        return Err(format!(
            "audio PCM16 payload has odd byte length: {}",
            bytes.len()
        ));
    }

    Ok(bytes
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect())
}

fn classify_server_error(err: &ServerErrorBody) -> RealtimeTranslationErrorKind {
    let code = err.code.as_deref().unwrap_or("");
    let msg = err.message.to_lowercase();
    let ty = err.err_type.as_deref().unwrap_or("");

    if code.contains("invalid_api_key")
        || code.contains("auth")
        || msg.contains("invalid api key")
        || msg.contains("unauthorized")
        || ty.contains("auth")
    {
        RealtimeTranslationErrorKind::Authentication
    } else if code.contains("rate")
        || msg.contains("rate limit")
        || code.contains("429")
        || code.contains("quota")
        || msg.contains("quota")
        || msg.contains("billing")
        || msg.contains("maximum monthly spend")
        || ty.contains("rate")
        || ty.contains("quota")
    {
        RealtimeTranslationErrorKind::RateLimited
    } else {
        RealtimeTranslationErrorKind::Protocol
    }
}

fn error_from_kind(
    kind: RealtimeTranslationErrorKind,
    message: String,
) -> RealtimeTranslationError {
    match kind {
        RealtimeTranslationErrorKind::Authentication => {
            RealtimeTranslationError::Authentication(message)
        }
        RealtimeTranslationErrorKind::RateLimited => RealtimeTranslationError::RateLimited(message),
        RealtimeTranslationErrorKind::Connection => RealtimeTranslationError::Connection(message),
        RealtimeTranslationErrorKind::Timeout => RealtimeTranslationError::Timeout(message),
        RealtimeTranslationErrorKind::Protocol => RealtimeTranslationError::Protocol(message),
        RealtimeTranslationErrorKind::Internal => RealtimeTranslationError::Internal(message),
    }
}

fn json_parse_error_context(error: &serde_json::Error, payload_bytes: usize) -> String {
    format!(
        "category={:?}, line={}, column={}, payload_bytes={}",
        error.classify(),
        error.line(),
        error.column(),
        payload_bytes
    )
}

/// Утилита для генерации `session.input_audio_buffer.append` JSON — открытая для тестов
/// чтобы можно было проверить кодирование без сети.
#[doc(hidden)]
pub fn build_append_audio_json_for_test(pcm16: &[i16]) -> serde_json::Value {
    let bytes: Vec<u8> = pcm16.iter().flat_map(|s| s.to_le_bytes()).collect();
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    json!({
        "type": "session.input_audio_buffer.append",
        "audio": b64,
    })
}

// Чтобы избежать unused warning, если выше у нас сложился импорт anyhow::anyhow.
#[allow(dead_code)]
fn _force_anyhow_used() -> anyhow::Error {
    anyhow!("placeholder")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ReaderDropSignal(Option<tokio::sync::oneshot::Sender<()>>);

    impl Drop for ReaderDropSignal {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }
    use std::future::pending;

    #[test]
    fn append_audio_payload_is_base64_pcm16_le() {
        let pcm: Vec<i16> = vec![0, 1, -1, 256, -256];
        let value = build_append_audio_json_for_test(&pcm);

        assert_eq!(value["type"], "session.input_audio_buffer.append");
        let audio = value["audio"].as_str().expect("audio must be string");

        let bytes = base64::engine::general_purpose::STANDARD
            .decode(audio)
            .expect("must decode");
        assert_eq!(bytes.len(), pcm.len() * 2);
        let decoded: Vec<i16> = bytes
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        assert_eq!(decoded, pcm);
    }

    #[test]
    fn decode_pcm16_base64_payload_rejects_odd_byte_count() {
        let payload = base64::engine::general_purpose::STANDARD.encode([1_u8, 2, 3]);
        let err = decode_pcm16_base64_payload(&payload).unwrap_err();

        assert!(err.contains("odd byte length: 3"));
    }

    #[test]
    fn server_event_parsing_handles_known_types() {
        let texts: &[(&str, &str)] = &[
            (r#"{"type":"session.created"}"#, "session.created"),
            (r#"{"type":"session.updated"}"#, "session.updated"),
            (r#"{"type":"session.closed"}"#, "session.closed"),
        ];
        for (raw, label) in texts {
            let ev: ServerEvent = serde_json::from_str(raw).unwrap_or_else(|e| {
                panic!("failed to parse {}: {}", label, e);
            });
            // Проверяем что это не вариант Unknown.
            if let ServerEvent::Unknown = ev {
                panic!("{} unexpectedly parsed as Unknown", label);
            }
        }
    }

    #[test]
    fn unknown_event_does_not_panic() {
        let raw = r#"{"type":"some.future.event.type","payload":42}"#;
        let ev: ServerEvent = serde_json::from_str(raw).expect("must parse defensively");
        assert!(matches!(ev, ServerEvent::Unknown));
    }

    #[test]
    fn malformed_event_log_context_contains_metadata_but_not_payload() {
        let secret = "private transcript and base64 audio";
        let raw = format!(r#"{{"type":"session.updated","secret":"{secret}""#);
        let error = serde_json::from_str::<ServerEvent>(&raw).unwrap_err();

        let context = json_parse_error_context(&error, raw.len());

        assert!(context.contains("category=Eof"));
        assert!(context.contains(&format!("payload_bytes={}", raw.len())));
        assert!(!context.contains(secret));
        assert!(!context.contains("transcript"));
        assert!(!context.contains("base64"));
    }

    #[test]
    fn realtime_websocket_config_bounds_reads_and_failed_writes() {
        let config = realtime_websocket_config();

        assert_eq!(config.max_message_size, Some(WS_MAX_MESSAGE_BYTES));
        assert_eq!(config.max_frame_size, Some(WS_MAX_MESSAGE_BYTES));
        assert_eq!(config.max_write_buffer_size, WS_MAX_WRITE_BUFFER_BYTES);
        assert!(config.max_write_buffer_size > config.write_buffer_size);
    }

    #[test]
    fn authorization_header_trims_api_key_whitespace() {
        let value = build_authorization_header_value("  test-key\n").expect("valid header");

        assert_eq!(value.to_str().unwrap(), "Bearer test-key");
    }

    #[tokio::test]
    async fn websocket_operation_timeout_returns_timeout_error() {
        let err = await_ws_operation(
            pending::<Result<(), std::io::Error>>(),
            Duration::from_millis(10),
            "test send",
        )
        .await
        .expect_err("pending websocket operation must time out");

        assert!(matches!(
            err,
            RealtimeTranslationError::Timeout(message)
                if message.contains("test send timed out after 10 ms")
        ));
    }

    #[tokio::test]
    async fn client_drop_aborts_pending_reader_task() {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        let mut client = OpenAIRealtimeTranslationClient::new();
        client.reader_task = Some(tokio::spawn(async move {
            let _drop_signal = ReaderDropSignal(Some(dropped_tx));
            let _ = started_tx.send(());
            futures_util::future::pending::<()>().await;
        }));
        started_rx.await.expect("reader task started");

        drop(client);

        tokio::time::timeout(Duration::from_secs(1), dropped_rx)
            .await
            .expect("reader future must be dropped")
            .expect("reader drop signal");
    }

    #[tokio::test]
    async fn connect_rejects_a_second_active_session_without_replacing_its_reader() {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (dropped_tx, mut dropped_rx) = tokio::sync::oneshot::channel();
        let mut client = OpenAIRealtimeTranslationClient::with_endpoint("ws://127.0.0.1:9");
        client.reader_task = Some(tokio::spawn(async move {
            let _drop_signal = ReaderDropSignal(Some(dropped_tx));
            let _ = started_tx.send(());
            pending::<()>().await;
        }));
        started_rx.await.expect("active reader task must start");

        let error = client
            .connect(RealtimeTranslationConfig::new(
                "test-key".to_string(),
                "ru".to_string(),
                RealtimeInputNoiseReduction::NearField,
            ))
            .await
            .expect_err("an active client must reject a second paid session");

        assert!(matches!(
            error,
            RealtimeTranslationError::Connection(message)
                if message.contains("already has an active session")
        ));
        assert!(client
            .reader_task
            .as_ref()
            .is_some_and(|task| !task.is_finished()));
        assert!(matches!(
            dropped_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));

        client.abort().await;
        timeout(Duration::from_secs(1), dropped_rx)
            .await
            .expect("active reader future must be dropped by abort")
            .expect("reader drop signal");
    }

    #[tokio::test]
    async fn cancelling_connect_after_reader_spawn_closes_websocket() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local websocket server");
        let endpoint = format!(
            "ws://{}",
            listener.local_addr().expect("local websocket address")
        );
        let (session_update_tx, session_update_rx) = oneshot::channel();
        let server_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept websocket client");
            let mut websocket = tokio_tungstenite::accept_async(stream)
                .await
                .expect("complete websocket handshake");
            let message = timeout(Duration::from_secs(1), websocket.next())
                .await
                .expect("client must send session.update")
                .expect("websocket must remain open for session.update")
                .expect("read session.update");
            let Message::Text(text) = message else {
                panic!("expected text session.update, got {message:?}");
            };
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&text).expect("parse session.update")
                    ["type"],
                "session.update"
            );
            session_update_tx
                .send(())
                .expect("signal that reader task was spawned");

            match timeout(Duration::from_secs(1), websocket.next()).await {
                Ok(None | Some(Err(_)) | Some(Ok(Message::Close(_)))) => {}
                Ok(Some(Ok(message))) => {
                    panic!("unexpected websocket message after connect cancellation: {message:?}")
                }
                Err(_) => panic!("cancelled connect left its websocket reader detached"),
            }
        });

        let mut client = OpenAIRealtimeTranslationClient::with_endpoint(endpoint).with_timeouts(
            Duration::from_secs(1),
            Duration::from_secs(30),
            Duration::from_secs(1),
            Duration::from_secs(1),
        );
        let config = RealtimeTranslationConfig::new(
            "test-key".to_string(),
            "ru".to_string(),
            RealtimeInputNoiseReduction::NearField,
        );
        let mut connect = Box::pin(client.connect(config));
        tokio::select! {
            result = &mut connect => panic!("connect completed before cancellation: {result:?}"),
            signal = session_update_rx => signal.expect("server received session.update"),
        }

        drop(connect);

        timeout(Duration::from_secs(2), server_task)
            .await
            .expect("server must observe websocket cleanup")
            .expect("local websocket server task must not panic");
        assert!(client.reader_task.is_none());
        assert!(client.sink.is_none());
    }

    #[tokio::test]
    async fn server_event_send_backpressures_when_event_queue_is_full() {
        let (tx, mut rx) = mpsc::channel::<RealtimeTranslationEvent>(1);
        tx.send(RealtimeTranslationEvent::SourceTextDelta("held".into()))
            .await
            .unwrap();
        let (ready_tx, ready_rx) = oneshot::channel();
        let mut handshake = ReaderHandshake::new(ready_tx);
        assert!(handshake.confirm());
        ready_rx.await.unwrap().unwrap();

        let audio = base64::engine::general_purpose::STANDARD.encode(1_i16.to_le_bytes());
        let raw = format!(
            r#"{{"type":"session.output_audio.delta","delta":"{}"}}"#,
            audio
        );
        let task = tokio::spawn(async move { handle_server_text(&raw, &tx, &mut handshake).await });

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !task.is_finished(),
            "full OpenAI event queue should backpressure the reader"
        );

        assert!(matches!(
            rx.recv().await,
            Some(RealtimeTranslationEvent::SourceTextDelta(text)) if text == "held"
        ));
        let should_stop = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("reader send should resume after queue space is available")
            .expect("handler task must not panic");
        assert!(!should_stop);

        assert!(matches!(
            rx.recv().await,
            Some(RealtimeTranslationEvent::TranslatedAudio {
                pcm16,
                sample_rate: 24_000,
                channels: 1,
            }) if pcm16 == vec![1]
        ));
    }

    #[tokio::test]
    async fn server_audio_delta_with_odd_pcm_byte_count_emits_protocol_error() {
        let (tx, mut rx) = mpsc::channel::<RealtimeTranslationEvent>(1);
        let (ready_tx, ready_rx) = oneshot::channel();
        let mut handshake = ReaderHandshake::new(ready_tx);
        assert!(handshake.confirm());
        ready_rx.await.unwrap().unwrap();
        let audio = base64::engine::general_purpose::STANDARD.encode([1_u8, 2, 3]);
        let raw = format!(
            r#"{{"type":"session.output_audio.delta","delta":"{}"}}"#,
            audio
        );

        let should_stop = handle_server_text(&raw, &tx, &mut handshake).await;

        assert!(should_stop);
        assert!(matches!(
            rx.recv().await,
            Some(RealtimeTranslationEvent::Failed(
                RealtimeTranslationError::Protocol(message)
            )) if message.contains("odd byte length: 3")
        ));
    }

    #[tokio::test]
    async fn malformed_server_event_emits_protocol_error() {
        let (tx, mut rx) = mpsc::channel::<RealtimeTranslationEvent>(1);
        let (ready_tx, ready_rx) = oneshot::channel();
        let mut handshake = ReaderHandshake::new(ready_tx);
        assert!(handshake.confirm());
        ready_rx.await.unwrap().unwrap();

        let should_stop =
            handle_server_text(r#"{"event":"missing type"}"#, &tx, &mut handshake).await;

        assert!(should_stop);
        assert!(matches!(
            rx.recv().await,
            Some(RealtimeTranslationEvent::Failed(
                RealtimeTranslationError::Protocol(message)
            )) if message.contains("invalid OpenAI realtime server event")
        ));
    }

    #[tokio::test]
    async fn server_error_event_stops_reader_after_emitting_error() {
        let (tx, mut rx) = mpsc::channel::<RealtimeTranslationEvent>(1);
        let (ready_tx, ready_rx) = oneshot::channel();
        let mut handshake = ReaderHandshake::new(ready_tx);
        assert!(handshake.confirm());
        ready_rx.await.unwrap().unwrap();
        let raw = r#"{"type":"error","error":{"code":"invalid_api_key","message":"bad key","type":"auth"}}"#;

        let should_stop = handle_server_text(raw, &tx, &mut handshake).await;

        assert!(should_stop);
        assert!(matches!(
            rx.recv().await,
            Some(RealtimeTranslationEvent::Failed(
                RealtimeTranslationError::Authentication(message)
            )) if message == "bad key"
        ));
    }

    #[tokio::test]
    async fn session_created_does_not_confirm_readiness_or_emit_public_event() {
        let (tx, mut rx) = mpsc::channel::<RealtimeTranslationEvent>(1);
        let (ready_tx, mut ready_rx) = oneshot::channel();
        let mut handshake = ReaderHandshake::new(ready_tx);

        let should_stop =
            handle_server_text(r#"{"type":"session.created"}"#, &tx, &mut handshake).await;

        assert!(!should_stop);
        assert!(matches!(
            ready_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn session_updated_confirms_readiness_without_public_handshake_event() {
        let (tx, mut rx) = mpsc::channel::<RealtimeTranslationEvent>(1);
        let (ready_tx, ready_rx) = oneshot::channel();
        let mut handshake = ReaderHandshake::new(ready_tx);

        let should_stop =
            handle_server_text(r#"{"type":"session.updated"}"#, &tx, &mut handshake).await;

        assert!(!should_stop);
        ready_rx.await.unwrap().unwrap();
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn confirmed_server_events_map_to_neutral_runtime_contract() {
        let (tx, mut rx) = mpsc::channel::<RealtimeTranslationEvent>(4);
        let (ready_tx, ready_rx) = oneshot::channel();
        let mut handshake = ReaderHandshake::new(ready_tx);
        assert!(handshake.confirm());
        ready_rx.await.unwrap().unwrap();
        let audio = base64::engine::general_purpose::STANDARD.encode(42_i16.to_le_bytes());

        assert!(
            !handle_server_text(
                r#"{"type":"session.output_transcript.delta","delta":"hello"}"#,
                &tx,
                &mut handshake,
            )
            .await
        );
        assert!(
            !handle_server_text(
                r#"{"type":"session.input_transcript.delta","delta":"hola"}"#,
                &tx,
                &mut handshake,
            )
            .await
        );
        assert!(
            !handle_server_text(
                &format!(
                    r#"{{"type":"session.output_audio.delta","delta":"{}"}}"#,
                    audio
                ),
                &tx,
                &mut handshake,
            )
            .await
        );
        assert!(
            !handle_server_text(
                r#"{"type":"future.event","value":"ignored"}"#,
                &tx,
                &mut handshake,
            )
            .await
        );

        assert_eq!(
            rx.recv().await,
            Some(RealtimeTranslationEvent::TranslatedTextDelta(
                "hello".into()
            ))
        );
        assert_eq!(
            rx.recv().await,
            Some(RealtimeTranslationEvent::SourceTextDelta("hola".into()))
        );
        assert_eq!(
            rx.recv().await,
            Some(RealtimeTranslationEvent::TranslatedAudio {
                pcm16: vec![42],
                sample_rate: 24_000,
                channels: 1,
            })
        );
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn close_before_session_updated_is_startup_failure_not_runtime_event() {
        let (tx, mut rx) = mpsc::channel::<RealtimeTranslationEvent>(1);
        let (ready_tx, ready_rx) = oneshot::channel();
        let mut handshake = ReaderHandshake::new(ready_tx);

        close_reader(&tx, &mut handshake, "closed during startup").await;

        assert!(matches!(
            ready_rx.await.unwrap(),
            Err(RealtimeTranslationError::Connection(message))
                if message == "closed during startup"
        ));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn server_error_before_session_updated_is_typed_startup_failure() {
        let (tx, mut rx) = mpsc::channel::<RealtimeTranslationEvent>(1);
        let (ready_tx, ready_rx) = oneshot::channel();
        let mut handshake = ReaderHandshake::new(ready_tx);
        let raw =
            r#"{"type":"error","error":{"code":"rate_limit_exceeded","message":"quota exceeded"}}"#;

        let should_stop = handle_server_text(raw, &tx, &mut handshake).await;

        assert!(should_stop);
        assert!(matches!(
            ready_rx.await.unwrap(),
            Err(RealtimeTranslationError::RateLimited(message)) if message == "quota exceeded"
        ));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn error_classification_routes_auth() {
        let err = ServerErrorBody {
            code: Some("invalid_api_key".to_string()),
            message: "Invalid API key".to_string(),
            err_type: Some("auth".to_string()),
        };
        assert_eq!(
            classify_server_error(&err),
            RealtimeTranslationErrorKind::Authentication
        );
    }

    #[test]
    fn error_classification_routes_rate_limit() {
        let err = ServerErrorBody {
            code: Some("rate_limit_exceeded".to_string()),
            message: "Rate limit exceeded".to_string(),
            err_type: Some("rate_limit".to_string()),
        };
        assert_eq!(
            classify_server_error(&err),
            RealtimeTranslationErrorKind::RateLimited
        );
    }

    #[test]
    fn error_classification_routes_quota_and_billing_to_rate_limited() {
        let err = ServerErrorBody {
            code: Some("insufficient_quota".to_string()),
            message: "You exceeded your current quota, please check your plan and billing details"
                .to_string(),
            err_type: Some("insufficient_quota".to_string()),
        };
        assert_eq!(
            classify_server_error(&err),
            RealtimeTranslationErrorKind::RateLimited
        );
    }

    #[test]
    fn empty_api_key_returns_authentication_error() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let mut client = OpenAIRealtimeTranslationClient::new();
            let err = client
                .connect(RealtimeTranslationConfig::new(
                    String::new(),
                    "en".to_string(),
                    RealtimeInputNoiseReduction::NearField,
                ))
                .await
                .unwrap_err();
            assert_eq!(err.kind(), RealtimeTranslationErrorKind::Authentication);
        });
    }
}
