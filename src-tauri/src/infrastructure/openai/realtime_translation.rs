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

use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use base64::Engine;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use http::header::AUTHORIZATION;
use http::HeaderValue;
use serde::Deserialize;
use serde_json::json;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

const OPENAI_REALTIME_TRANSLATION_URL: &str =
    "wss://api.openai.com/v1/realtime/translations?model=gpt-realtime-translate";

/// Категории ошибок OpenAI, на которые UI/Service реагируют по-разному.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAIErrorKind {
    Authentication,
    RateLimited,
    Connection,
    Protocol,
    Internal,
}

#[derive(Debug, thiserror::Error)]
pub enum OpenAITranslationError {
    #[error("Authentication: {0}")]
    Authentication(String),
    #[error("Rate limited: {0}")]
    RateLimited(String),
    #[error("Connection: {0}")]
    Connection(String),
    #[error("Protocol: {0}")]
    Protocol(String),
    #[error("Internal: {0}")]
    Internal(String),
}

impl OpenAITranslationError {
    pub fn kind(&self) -> OpenAIErrorKind {
        match self {
            Self::Authentication(_) => OpenAIErrorKind::Authentication,
            Self::RateLimited(_) => OpenAIErrorKind::RateLimited,
            Self::Connection(_) => OpenAIErrorKind::Connection,
            Self::Protocol(_) => OpenAIErrorKind::Protocol,
            Self::Internal(_) => OpenAIErrorKind::Internal,
        }
    }
}

/// События, которые клиент кидает выше (LiveTranslationService).
#[derive(Debug, Clone)]
pub enum OpenAIRealtimeEvent {
    SessionCreated,
    SessionUpdated,
    /// 24 kHz mono PCM16 переведённое аудио (декодированное из base64).
    AudioDelta(Vec<i16>),
    /// Target-language transcript delta.
    TranscriptDelta(String),
    /// Source-language transcript delta (для дебага/логов).
    InputTranscriptDelta(String),
    Error {
        code: Option<String>,
        message: String,
        kind: OpenAIErrorKind,
    },
    Closed,
}

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
type WsSink = SplitSink<WsStream, Message>;
type WsSource = SplitStream<WsStream>;

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
/// 1. `new(api_key, target_language)`
/// 2. `connect()` — открывает WS, отправляет `session.update`, запускает reader task,
///    возвращает receiver событий.
/// 3. `append_input_audio()` — отправка mic chunk'ов.
/// 4. `close(drain_timeout)` — отправляет `session.close`, ждёт `session.closed` или таймаут.
/// 5. `abort()` — hard cleanup (если connect не успел или мы хотим без drain).
pub struct OpenAIRealtimeTranslationClient {
    api_key: String,
    target_language: String,
    sink: Arc<Mutex<Option<WsSink>>>,
    reader_task: Option<JoinHandle<()>>,
    /// Канал из reader_task в верхний слой. Receiver отдаётся через connect(); Sender хранится здесь
    /// чтобы close() мог послать synthetic Closed для случая когда WS оборвался без `session.closed`.
    event_tx: Option<mpsc::UnboundedSender<OpenAIRealtimeEvent>>,
}

impl OpenAIRealtimeTranslationClient {
    pub fn new(api_key: String, target_language: String) -> Self {
        Self {
            api_key,
            target_language,
            sink: Arc::new(Mutex::new(None)),
            reader_task: None,
            event_tx: None,
        }
    }

    pub fn target_language(&self) -> &str {
        &self.target_language
    }

    /// Открывает WebSocket, отправляет `session.update`, запускает reader task.
    /// Возвращает receiver событий — caller должен читать его в цикле.
    pub async fn connect(
        &mut self,
    ) -> Result<mpsc::UnboundedReceiver<OpenAIRealtimeEvent>, OpenAITranslationError> {
        if self.api_key.trim().is_empty() {
            return Err(OpenAITranslationError::Authentication(
                "OPENAI_API_KEY не задан".to_string(),
            ));
        }

        let mut req = OPENAI_REALTIME_TRANSLATION_URL
            .into_client_request()
            .map_err(|e| OpenAITranslationError::Internal(format!("invalid url: {}", e)))?;
        let auth_value = HeaderValue::from_str(&format!("Bearer {}", self.api_key))
            .map_err(|e| OpenAITranslationError::Internal(format!("invalid auth header: {}", e)))?;
        req.headers_mut().insert(AUTHORIZATION, auth_value);

        log::info!(
            "Connecting to OpenAI realtime translation: target_language={}",
            self.target_language
        );

        let (ws, _resp) = match connect_async(req).await {
            Ok(pair) => pair,
            Err(err) => {
                let mapped = map_connect_error(&err);
                return Err(mapped);
            }
        };

        let (mut sink, source) = ws.split();

        // Отправляем session.update сразу
        let session_update = json!({
            "type": "session.update",
            "session": {
                "audio": {
                    "input": {
                        "transcription": { "model": "gpt-realtime-whisper" },
                        "noise_reduction": { "type": "near_field" }
                    },
                    "output": {
                        "language": self.target_language
                    }
                }
            }
        });
        if let Err(e) = sink.send(Message::Text(session_update.to_string())).await {
            return Err(OpenAITranslationError::Connection(format!(
                "failed to send session.update: {}",
                e
            )));
        }

        // Запускаем reader task
        let (tx, rx) = mpsc::unbounded_channel::<OpenAIRealtimeEvent>();
        let reader_tx = tx.clone();
        let reader_task = tokio::spawn(async move {
            run_reader(source, reader_tx).await;
        });

        {
            let mut guard = self.sink.lock().await;
            *guard = Some(sink);
        }
        self.event_tx = Some(tx);
        self.reader_task = Some(reader_task);
        log::info!("OpenAI realtime translation: WebSocket connected, session.update sent");
        Ok(rx)
    }

    /// Отправка чанка PCM16 24 kHz mono. base64 кодирование внутри.
    pub async fn append_input_audio(&self, pcm16: &[i16]) -> Result<(), OpenAITranslationError> {
        if pcm16.is_empty() {
            return Ok(());
        }
        let bytes: Vec<u8> = pcm16.iter().flat_map(|s| s.to_le_bytes()).collect();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let msg = json!({
            "type": "session.input_audio_buffer.append",
            "audio": b64,
        });

        let mut guard = self.sink.lock().await;
        let Some(sink) = guard.as_mut() else {
            return Err(OpenAITranslationError::Connection(
                "WebSocket sink не инициализирован".to_string(),
            ));
        };
        sink.send(Message::Text(msg.to_string()))
            .await
            .map_err(|e| OpenAITranslationError::Connection(format!("send audio failed: {}", e)))
    }

    /// Graceful close: посылаем `session.close`, ждём `session.closed` от сервера до таймаута.
    /// Если таймаут — закрываем WS принудительно и эмитим synthetic Closed event.
    pub async fn close(&mut self, drain_timeout: Duration) -> Result<(), OpenAITranslationError> {
        // 1. Translation session close. WS Close frame alone can skip the API-level
        // shutdown path and cut the final translated tail.
        {
            let mut guard = self.sink.lock().await;
            if let Some(sink) = guard.as_mut() {
                let close_msg = json!({ "type": "session.close" });
                if let Err(e) = sink.send(Message::Text(close_msg.to_string())).await {
                    return Err(OpenAITranslationError::Connection(format!(
                        "failed to send session.close: {}",
                        e
                    )));
                }
                if let Err(e) = sink.flush().await {
                    return Err(OpenAITranslationError::Connection(format!(
                        "failed to flush session.close: {}",
                        e
                    )));
                }
            }
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
                    {
                        let mut guard = self.sink.lock().await;
                        if let Some(sink) = guard.as_mut() {
                            let _ = sink.send(Message::Close(None)).await;
                            let _ = sink.flush().await;
                        }
                    }
                    task.abort();
                    if let Some(tx) = self.event_tx.as_ref() {
                        let _ = tx.send(OpenAIRealtimeEvent::Closed);
                    }
                }
            }
        }

        // 3. Прибиваем sink
        {
            let mut guard = self.sink.lock().await;
            *guard = None;
        }
        self.event_tx = None;
        log::info!("OpenAI realtime translation: closed");
        Ok(())
    }

    /// Жёсткий abort без drain. Используется когда start_translation упал на полпути.
    pub async fn abort(&mut self) {
        if let Some(task) = self.reader_task.take() {
            task.abort();
        }
        let mut guard = self.sink.lock().await;
        *guard = None;
        if let Some(tx) = self.event_tx.take() {
            let _ = tx.send(OpenAIRealtimeEvent::Closed);
        }
    }
}

fn map_connect_error(err: &tokio_tungstenite::tungstenite::Error) -> OpenAITranslationError {
    use tokio_tungstenite::tungstenite::Error as E;
    match err {
        E::Http(resp) => {
            let status = resp.status();
            let msg = format!("HTTP {} during WS handshake", status);
            if status.as_u16() == 401 || status.as_u16() == 403 {
                OpenAITranslationError::Authentication(msg)
            } else if status.as_u16() == 429 {
                OpenAITranslationError::RateLimited(msg)
            } else {
                OpenAITranslationError::Connection(msg)
            }
        }
        E::Io(io) => OpenAITranslationError::Connection(io.to_string()),
        E::Tls(t) => OpenAITranslationError::Connection(t.to_string()),
        E::Url(u) => OpenAITranslationError::Internal(u.to_string()),
        E::HttpFormat(hf) => OpenAITranslationError::Internal(hf.to_string()),
        other => OpenAITranslationError::Connection(other.to_string()),
    }
}

async fn run_reader(mut source: WsSource, tx: mpsc::UnboundedSender<OpenAIRealtimeEvent>) {
    while let Some(next) = source.next().await {
        match next {
            Ok(Message::Text(text)) => {
                if handle_server_text(&text, &tx) {
                    break;
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
                let _ = tx.send(OpenAIRealtimeEvent::Closed);
                break;
            }
            Err(e) => {
                let _ = tx.send(OpenAIRealtimeEvent::Error {
                    code: None,
                    message: format!("ws stream error: {}", e),
                    kind: OpenAIErrorKind::Connection,
                });
                break;
            }
        }
    }
    let _ = tx.send(OpenAIRealtimeEvent::Closed);
}

fn handle_server_text(text: &str, tx: &mpsc::UnboundedSender<OpenAIRealtimeEvent>) -> bool {
    match serde_json::from_str::<ServerEvent>(text) {
        Ok(ServerEvent::SessionCreated { .. }) => {
            let _ = tx.send(OpenAIRealtimeEvent::SessionCreated);
            false
        }
        Ok(ServerEvent::SessionUpdated { .. }) => {
            let _ = tx.send(OpenAIRealtimeEvent::SessionUpdated);
            false
        }
        Ok(ServerEvent::OutputAudioDelta { delta, audio }) => {
            // Cookbook использует `delta`, но некоторые сборки могут отдавать `audio` —
            // принимаем оба, чтобы выживать в разных версиях.
            let payload = if !delta.is_empty() { &delta } else { &audio };
            if payload.is_empty() {
                return false;
            }
            match base64::engine::general_purpose::STANDARD.decode(payload) {
                Ok(bytes) => {
                    let pcm16: Vec<i16> = bytes
                        .chunks_exact(2)
                        .map(|b| i16::from_le_bytes([b[0], b[1]]))
                        .collect();
                    let _ = tx.send(OpenAIRealtimeEvent::AudioDelta(pcm16));
                }
                Err(e) => {
                    let _ = tx.send(OpenAIRealtimeEvent::Error {
                        code: None,
                        message: format!("audio base64 decode: {}", e),
                        kind: OpenAIErrorKind::Protocol,
                    });
                }
            }
            false
        }
        Ok(ServerEvent::OutputTranscriptDelta { delta }) => {
            if !delta.is_empty() {
                let _ = tx.send(OpenAIRealtimeEvent::TranscriptDelta(delta));
            }
            false
        }
        Ok(ServerEvent::InputTranscriptDelta { delta }) => {
            if !delta.is_empty() {
                let _ = tx.send(OpenAIRealtimeEvent::InputTranscriptDelta(delta));
            }
            false
        }
        Ok(ServerEvent::SessionClosed { .. }) => {
            let _ = tx.send(OpenAIRealtimeEvent::Closed);
            true
        }
        Ok(ServerEvent::Error { error }) => {
            let kind = classify_server_error(&error);
            let _ = tx.send(OpenAIRealtimeEvent::Error {
                code: error.code,
                message: if error.message.is_empty() {
                    error.err_type.unwrap_or_else(|| "unknown".to_string())
                } else {
                    error.message
                },
                kind,
            });
            false
        }
        Ok(ServerEvent::Unknown) => {
            log::debug!(
                "OpenAI realtime translation: ignored unknown server event: {}",
                truncate_for_log(text, 256)
            );
            false
        }
        Err(e) => {
            log::warn!(
                "OpenAI realtime translation: failed to parse server event ({}). raw: {}",
                e,
                truncate_for_log(text, 256)
            );
            false
        }
    }
}

fn classify_server_error(err: &ServerErrorBody) -> OpenAIErrorKind {
    let code = err.code.as_deref().unwrap_or("");
    let msg = err.message.to_lowercase();
    let ty = err.err_type.as_deref().unwrap_or("");

    if code.contains("invalid_api_key")
        || code.contains("auth")
        || msg.contains("invalid api key")
        || msg.contains("unauthorized")
        || ty.contains("auth")
    {
        OpenAIErrorKind::Authentication
    } else if code.contains("rate")
        || msg.contains("rate limit")
        || code.contains("429")
        || ty.contains("rate")
    {
        OpenAIErrorKind::RateLimited
    } else {
        OpenAIErrorKind::Protocol
    }
}

fn truncate_for_log(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut out = s[..max].to_string();
        out.push('…');
        out
    }
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
    fn error_classification_routes_auth() {
        let err = ServerErrorBody {
            code: Some("invalid_api_key".to_string()),
            message: "Invalid API key".to_string(),
            err_type: Some("auth".to_string()),
        };
        assert_eq!(classify_server_error(&err), OpenAIErrorKind::Authentication);
    }

    #[test]
    fn error_classification_routes_rate_limit() {
        let err = ServerErrorBody {
            code: Some("rate_limit_exceeded".to_string()),
            message: "Rate limit exceeded".to_string(),
            err_type: Some("rate_limit".to_string()),
        };
        assert_eq!(classify_server_error(&err), OpenAIErrorKind::RateLimited);
    }

    #[test]
    fn empty_api_key_returns_authentication_error() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let mut client = OpenAIRealtimeTranslationClient::new(String::new(), "en".to_string());
            let err = client.connect().await.unwrap_err();
            assert_eq!(err.kind(), OpenAIErrorKind::Authentication);
        });
    }
}
