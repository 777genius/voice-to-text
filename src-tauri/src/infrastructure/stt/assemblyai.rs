use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use http::Request;
use serde_json::{json, Value};
use std::future::Future;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;
use tokio_tungstenite::{
    connect_async_with_config, tungstenite::Message, MaybeTlsStream, WebSocketStream,
};

use crate::domain::{
    AudioChunk, ErrorCallback, SttConfig, SttConnectionCategory, SttConnectionError, SttError,
    SttProvider, SttResult, Transcription, TranscriptionCallback,
};
use crate::infrastructure::embedded_keys;

/// AssemblyAI Universal-Streaming STT provider (v3)
///
/// Endpoint: wss://streaming.assemblyai.com/v3/ws
/// Pricing: ~$0.0025/min ($0.15/hour session-based)
/// Features: Ultra-low latency, unlimited concurrent streams
///
/// Protocol:
/// 1. Connect with Authorization header (NOT Bearer, just raw API key)
/// 2. Select the streaming model in the connection query.
/// 3. Stream raw PCM16 in binary WebSocket frames.
/// 4. Receive Begin/Turn events, then send Terminate and drain through Termination.
const ASSEMBLYAI_WS_URL: &str = "wss://streaming.assemblyai.com/v3/ws";
const ASSEMBLYAI_SESSION_READY_TIMEOUT: Duration = Duration::from_secs(5);
const ASSEMBLYAI_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const ASSEMBLYAI_SEND_TIMEOUT: Duration = Duration::from_secs(5);
const ASSEMBLYAI_TERMINATION_TIMEOUT: Duration = Duration::from_secs(5);
const ASSEMBLYAI_MIN_AUDIO_SAMPLES: usize = 800; // 50 ms @ 16 kHz
const ASSEMBLYAI_MAX_AUDIO_SAMPLES: usize = 16_000; // 1 s @ 16 kHz

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

fn pcm_amplitude_stats(samples: &[i16]) -> (i32, i32) {
    let mut peak = 0i32;
    let mut sum = 0i64;
    for &sample in samples {
        let amplitude = (sample as i32).abs();
        peak = peak.max(amplitude);
        sum = sum.saturating_add(amplitude as i64);
    }

    let average = if samples.is_empty() {
        0
    } else {
        (sum / samples.len() as i64).min(i32::MAX as i64) as i32
    };
    (peak, average)
}

fn assemblyai_server_error(message: impl Into<String>) -> SttError {
    let message = message.into();
    let lower = message.to_lowercase();
    if lower.contains("auth")
        || lower.contains("api key")
        || lower.contains("unauthorized")
        || lower.contains("401")
        || lower.contains("403")
    {
        return SttError::Authentication(message);
    }

    let category = if lower.contains("rate limit") || lower.contains("429") {
        SttConnectionCategory::RateLimited
    } else if lower.contains("quota") || lower.contains("billing") {
        SttConnectionCategory::ProviderQuotaExceeded
    } else {
        SttConnectionCategory::Unknown
    };
    SttError::Connection(SttConnectionError::with_category(message, category))
}

fn assemblyai_closed_error(message: impl Into<String>) -> SttError {
    SttError::Connection(SttConnectionError::with_category(
        message,
        SttConnectionCategory::Closed,
    ))
}

fn assemblyai_timeout_error(message: impl Into<String>) -> SttError {
    SttError::Connection(SttConnectionError::with_category(
        message,
        SttConnectionCategory::Timeout,
    ))
}

fn assemblyai_speech_model(language: &str) -> &'static str {
    let base_language = language
        .trim()
        .split(['-', '_'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();

    match base_language.as_str() {
        "en" | "es" | "de" | "fr" | "pt" | "it" => "u3-rt-pro",
        _ => "whisper-rt",
    }
}

fn assemblyai_stream_url(base_url: &str, language: &str) -> String {
    format!(
        "{}?sample_rate=16000&encoding=pcm_s16le&speech_model={}&language_detection=true",
        base_url,
        assemblyai_speech_model(language)
    )
}

fn assemblyai_next_audio_frame_samples(buffered_samples: usize) -> Option<usize> {
    if buffered_samples < ASSEMBLYAI_MIN_AUDIO_SAMPLES {
        return None;
    }

    let mut samples_to_send = buffered_samples.min(ASSEMBLYAI_MAX_AUDIO_SAMPLES);
    let remaining = buffered_samples - samples_to_send;
    if remaining > 0 && remaining < ASSEMBLYAI_MIN_AUDIO_SAMPLES {
        samples_to_send -= ASSEMBLYAI_MIN_AUDIO_SAMPLES - remaining;
    }
    Some(samples_to_send)
}

async fn await_assemblyai_send<F>(future: F, timeout: Duration, operation: &str) -> SttResult<()>
where
    F: Future<Output = Result<(), tokio_tungstenite::tungstenite::Error>>,
{
    match tokio::time::timeout(timeout, future).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(assemblyai_closed_error(format!(
            "AssemblyAI {} failed: {}",
            operation, error
        ))),
        Err(_) => Err(assemblyai_timeout_error(format!(
            "AssemblyAI {} timed out after {} ms",
            operation,
            timeout.as_millis()
        ))),
    }
}

fn call_assemblyai_callback(label: &str, callback: impl FnOnce()) {
    if catch_unwind(AssertUnwindSafe(callback)).is_err() {
        log::error!("AssemblyAI {} callback panicked", label);
    }
}

async fn report_assemblyai_receiver_error(
    startup_error: &Arc<Mutex<Option<SttError>>>,
    session_ready: &Arc<Notify>,
    on_error: &ErrorCallback,
    error: SttError,
) {
    *startup_error.lock().await = Some(error.clone());
    session_ready.notify_one();
    call_assemblyai_callback("error", || on_error(error));
}

pub struct AssemblyAIProvider {
    config: Option<SttConfig>,
    is_streaming: bool,
    api_key: Option<String>,
    ws_write: Option<futures_util::stream::SplitSink<WsStream, Message>>,
    receiver_task: Option<JoinHandle<()>>,
    session_ready: Arc<Notify>,
    startup_error: Arc<Mutex<Option<SttError>>>,
    stop_requested: Arc<AtomicBool>,
    audio_buffer: Vec<i16>, // Буфер для накопления аудио до минимального размера
    ws_base_url: String,
}

impl AssemblyAIProvider {
    pub fn new() -> Self {
        Self {
            config: None,
            is_streaming: false,
            api_key: None,
            ws_write: None,
            receiver_task: None,
            session_ready: Arc::new(Notify::new()),
            startup_error: Arc::new(Mutex::new(None)),
            stop_requested: Arc::new(AtomicBool::new(false)),
            audio_buffer: Vec::new(),
            ws_base_url: ASSEMBLYAI_WS_URL.to_string(),
        }
    }

    #[cfg(test)]
    fn with_ws_base_url(ws_base_url: String) -> Self {
        let mut provider = Self::new();
        provider.ws_base_url = ws_base_url;
        provider
    }
}

impl Default for AssemblyAIProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for AssemblyAIProvider {
    fn drop(&mut self) {
        self.stop_requested.store(true, Ordering::SeqCst);
        super::abort_background_task(&mut self.receiver_task);
    }
}

#[async_trait]
impl SttProvider for AssemblyAIProvider {
    async fn initialize(&mut self, config: &SttConfig) -> SttResult<()> {
        log::info!("AssemblyAI Provider: Initializing (v3)");

        // Приоритет: пользовательский ключ → встроенный ключ
        let user_api_key = config
            .assemblyai_api_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(str::to_string);
        let using_user_key = user_api_key.is_some();
        let api_key = user_api_key
            .or_else(|| {
                // Fallback на встроенный ключ
                if embedded_keys::has_embedded_assemblyai_key() {
                    Some(embedded_keys::EMBEDDED_ASSEMBLYAI_KEY.to_string())
                } else {
                    None
                }
            })
            .ok_or_else(|| {
                SttError::Configuration(
                    "AssemblyAI API key is required (either user key or embedded key)".to_string(),
                )
            })?;

        log::info!(
            "AssemblyAI Provider: Using {} API key",
            if using_user_key { "user" } else { "embedded" }
        );

        self.api_key = Some(api_key);
        self.config = Some(config.clone());
        Ok(())
    }

    async fn start_stream(
        &mut self,
        on_partial: TranscriptionCallback,
        on_final: TranscriptionCallback,
        on_error: ErrorCallback,
        _on_connection_quality: crate::domain::ConnectionQualityCallback,
    ) -> SttResult<()> {
        log::info!("AssemblyAI Provider: Starting stream (v3 endpoint)");

        if self.is_streaming {
            return Err(SttError::Processing("Stream already active".to_string()));
        }

        let api_key = self
            .api_key
            .as_ref()
            .ok_or_else(|| SttError::Configuration("API key not set".to_string()))?
            .clone();

        // Получаем язык из конфига для использования в транскрипциях
        let configured_language = self
            .config
            .as_ref()
            .map(|config| config.language.clone())
            .unwrap_or_else(|| "en".to_string());

        let url = assemblyai_stream_url(&self.ws_base_url, &configured_language);

        log::debug!("Connecting to {}", url);

        let request = Request::builder()
            .method("GET")
            .uri(&url)
            .header("Host", "streaming.assemblyai.com")
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header(
                "Sec-WebSocket-Key",
                tokio_tungstenite::tungstenite::handshake::client::generate_key(),
            )
            .header("Authorization", &api_key)
            .body(())
            .map_err(|e| {
                SttError::Connection(SttConnectionError::simple(format!(
                    "Failed to build WS request: {}",
                    e
                )))
            })?;

        let (ws_stream, _response) = super::await_streaming_websocket_connect(
            connect_async_with_config(request, Some(super::streaming_websocket_config()), false),
            ASSEMBLYAI_CONNECT_TIMEOUT,
            "AssemblyAI",
        )
        .await?;

        log::info!("AssemblyAI WebSocket connected");

        // Split stream for concurrent read/write
        let (write, mut read) = ws_stream.split();

        // Пересоздаем Notify для новой сессии (фикс повторного использования)
        self.session_ready = Arc::new(Notify::new());
        *self.startup_error.lock().await = None;
        self.stop_requested.store(false, Ordering::SeqCst);

        // 2. Spawn background task for receiving messages
        let session_notify = self.session_ready.clone();
        let startup_error = self.startup_error.clone();
        let stop_requested = self.stop_requested.clone();
        let lang_for_transcription = configured_language.clone();
        let receiver_task = tokio::spawn(async move {
            log::debug!("AssemblyAI receiver task started");
            let mut terminal_error_reported = false;

            while let Some(msg_result) = read.next().await {
                match msg_result {
                    Ok(Message::Text(text)) => {
                        log::debug!("AssemblyAI received text message: {}", text);
                        // Parse JSON message
                        match serde_json::from_str::<Value>(&text) {
                            Ok(json) => {
                                let msg_type = json["type"].as_str();
                                log::debug!("AssemblyAI message type: {:?}", msg_type);

                                // Уведомляем что сессия готова при получении Begin
                                if msg_type == Some("Begin") {
                                    log::info!("AssemblyAI session began, ready to send audio");
                                    session_notify.notify_one();
                                }

                                if msg_type == Some("Error") {
                                    let message = json
                                        .get("error")
                                        .and_then(Value::as_str)
                                        .or_else(|| json.get("message").and_then(Value::as_str))
                                        .unwrap_or("AssemblyAI server error")
                                        .to_string();
                                    let error = assemblyai_server_error(message);
                                    log::error!("AssemblyAI server error: {}", error);
                                    report_assemblyai_receiver_error(
                                        &startup_error,
                                        &session_notify,
                                        &on_error,
                                        error,
                                    )
                                    .await;
                                    terminal_error_reported = true;
                                    break;
                                }

                                let is_termination = matches!(
                                    msg_type,
                                    Some("Termination") | Some("End") | Some("SessionTerminated")
                                );
                                Self::handle_message(
                                    json,
                                    &on_partial,
                                    &on_final,
                                    &lang_for_transcription,
                                );
                                if is_termination {
                                    break;
                                }
                            }
                            Err(e) => {
                                log::error!("Failed to parse AssemblyAI message: {}", e);
                                log::error!("Raw message: {}", text);
                            }
                        }
                    }
                    Ok(Message::Close(frame)) => {
                        log::info!("AssemblyAI WebSocket closed: {:?}", frame);
                        if !stop_requested.load(Ordering::SeqCst) {
                            let detail = frame
                                .as_ref()
                                .map(|frame| {
                                    format!(
                                        "AssemblyAI WebSocket closed (code={}, reason={})",
                                        u16::from(frame.code),
                                        frame.reason
                                    )
                                })
                                .unwrap_or_else(|| {
                                    "AssemblyAI WebSocket closed unexpectedly".to_string()
                                });
                            report_assemblyai_receiver_error(
                                &startup_error,
                                &session_notify,
                                &on_error,
                                assemblyai_closed_error(detail),
                            )
                            .await;
                            terminal_error_reported = true;
                        }
                        break;
                    }
                    Ok(Message::Binary(data)) => {
                        log::debug!("AssemblyAI received binary message: {} bytes", data.len());
                    }
                    Ok(Message::Ping(_)) => {
                        log::trace!("AssemblyAI received Ping");
                    }
                    Ok(Message::Pong(_)) => {
                        log::trace!("AssemblyAI received Pong");
                    }
                    Err(e) => {
                        log::error!("AssemblyAI WebSocket error: {}", e);
                        if !stop_requested.load(Ordering::SeqCst) {
                            report_assemblyai_receiver_error(
                                &startup_error,
                                &session_notify,
                                &on_error,
                                SttError::Connection(SttConnectionError::simple(format!(
                                    "AssemblyAI WebSocket error: {}",
                                    e
                                ))),
                            )
                            .await;
                            terminal_error_reported = true;
                        }
                        break;
                    }
                    Ok(msg) => {
                        log::warn!("AssemblyAI received unexpected message type: {:?}", msg);
                    }
                }
            }

            if !terminal_error_reported && !stop_requested.load(Ordering::SeqCst) {
                report_assemblyai_receiver_error(
                    &startup_error,
                    &session_notify,
                    &on_error,
                    assemblyai_closed_error("AssemblyAI WebSocket ended unexpectedly"),
                )
                .await;
            }

            log::debug!("AssemblyAI receiver task ended");
        });

        self.ws_write = Some(write);
        self.receiver_task = Some(receiver_task);
        self.is_streaming = true;

        // Ждем пока сессия будет готова (получим SessionBegins)
        log::info!("Waiting for session to be ready...");
        let ready_result = tokio::time::timeout(
            ASSEMBLYAI_SESSION_READY_TIMEOUT,
            self.session_ready.notified(),
        )
        .await
        .map_err(|_| {
            assemblyai_timeout_error(format!(
                "Timeout waiting for AssemblyAI Begin after {} ms",
                ASSEMBLYAI_SESSION_READY_TIMEOUT.as_millis()
            ))
        });

        let startup_result = match ready_result {
            Ok(()) => match self.startup_error.lock().await.take() {
                Some(error) => Err(error),
                None => Ok(()),
            },
            Err(error) => Err(error),
        };
        if let Err(error) = startup_result {
            if let Err(cleanup_error) = self.abort().await {
                log::warn!(
                    "AssemblyAI startup cleanup failed after {}: {}",
                    error,
                    cleanup_error
                );
            }
            return Err(error);
        }

        log::info!("AssemblyAI stream started successfully");
        Ok(())
    }

    async fn send_audio(&mut self, chunk: &AudioChunk) -> SttResult<()> {
        if !self.is_streaming {
            return Err(SttError::Processing("Not streaming".to_string()));
        }

        let write = self.ws_write.as_mut().ok_or_else(|| {
            SttError::Processing("WebSocket write handle not available".to_string())
        })?;

        // Проверяем уровень сигнала для детекции тишины
        let (max_amplitude, avg_amplitude) = pcm_amplitude_stats(&chunk.data);

        if max_amplitude > 1000 {
            log::debug!(
                "Audio signal detected: max={}, avg={}",
                max_amplitude,
                avg_amplitude
            );
        }

        // Добавляем чанк в буфер
        self.audio_buffer.extend_from_slice(&chunk.data);

        const MAX_RETAINED_SAMPLES: usize = 16_000 * 10;

        while let Some(samples_to_send) =
            assemblyai_next_audio_frame_samples(self.audio_buffer.len())
        {
            if self.audio_buffer.len() > MAX_RETAINED_SAMPLES {
                let excess = self.audio_buffer.len() - MAX_RETAINED_SAMPLES;
                log::warn!(
                    "AssemblyAI retry buffer overflow, dropping {} oldest samples",
                    excess
                );
                self.audio_buffer.drain(..excess);
            }

            let bytes: Vec<u8> = self
                .audio_buffer
                .iter()
                .take(samples_to_send)
                .flat_map(|&sample| sample.to_le_bytes())
                .collect();

            let duration_ms = (samples_to_send * 1000) / 16000;
            log::debug!(
                "Sending {} samples (~{}ms, {} bytes) to AssemblyAI",
                samples_to_send,
                duration_ms,
                bytes.len()
            );

            // Send as binary message (AssemblyAI v3 expects raw PCM binary data)
            write
                .send(Message::Binary(bytes))
                .await
                .map_err(|e| SttError::Processing(format!("Failed to send audio: {}", e)))?;
            self.audio_buffer.drain(..samples_to_send);
        }

        Ok(())
    }

    async fn stop_stream(&mut self) -> SttResult<()> {
        log::info!("AssemblyAI Provider: Stopping stream");
        self.stop_requested.store(true, Ordering::SeqCst);

        if !self.is_streaming {
            log::warn!("Stream not active");
            return Ok(());
        }

        let mut stop_result = Ok(());

        // Flush the sub-50 ms tail before termination. Only clear it after the
        // WebSocket confirms the frame was accepted by the sink.
        if !self.audio_buffer.is_empty() {
            if let Some(write) = self.ws_write.as_mut() {
                let mut padded_tail = self.audio_buffer.clone();
                if padded_tail.len() < ASSEMBLYAI_MIN_AUDIO_SAMPLES {
                    padded_tail.resize(ASSEMBLYAI_MIN_AUDIO_SAMPLES, 0);
                }
                let bytes: Vec<u8> = padded_tail
                    .iter()
                    .flat_map(|&sample| sample.to_le_bytes())
                    .collect();

                log::debug!(
                    "Flushing remaining {} samples from buffer",
                    self.audio_buffer.len()
                );
                match await_assemblyai_send(
                    write.send(Message::Binary(bytes)),
                    ASSEMBLYAI_SEND_TIMEOUT,
                    "final audio send",
                )
                .await
                {
                    Ok(()) => self.audio_buffer.clear(),
                    Err(error) => stop_result = Err(error),
                }
            } else {
                stop_result = Err(assemblyai_closed_error(
                    "AssemblyAI final audio could not be sent: WebSocket writer is missing",
                ));
            }
        }

        if stop_result.is_ok() {
            if let Some(write) = self.ws_write.as_mut() {
                let terminate_msg = json!({ "type": "Terminate" });
                if let Err(error) = await_assemblyai_send(
                    write.send(Message::Text(terminate_msg.to_string())),
                    ASSEMBLYAI_SEND_TIMEOUT,
                    "Terminate send",
                )
                .await
                {
                    stop_result = Err(error);
                }
            } else {
                stop_result = Err(assemblyai_closed_error(
                    "AssemblyAI Terminate could not be sent: WebSocket writer is missing",
                ));
            }
        }

        // Keep the receiver alive so final Turn events are delivered before the
        // server's Termination event ends the task.
        if stop_result.is_ok() {
            if let Some(mut task) = self.receiver_task.take() {
                match tokio::time::timeout(ASSEMBLYAI_TERMINATION_TIMEOUT, &mut task).await {
                    Ok(Ok(())) => {}
                    Ok(Err(join_error)) => {
                        stop_result = Err(SttError::Internal(format!(
                            "AssemblyAI receiver task failed during termination: {}",
                            join_error
                        )));
                    }
                    Err(_) => {
                        task.abort();
                        let _ = task.await;
                        stop_result = Err(assemblyai_timeout_error(format!(
                            "AssemblyAI Termination timed out after {} ms",
                            ASSEMBLYAI_TERMINATION_TIMEOUT.as_millis()
                        )));
                    }
                }
            } else {
                stop_result = Err(SttError::Internal(
                    "AssemblyAI receiver task is missing during termination".to_string(),
                ));
            }
        }

        if let Some(task) = self.receiver_task.take() {
            task.abort();
            let _ = task.await;
        }

        self.ws_write = None;
        self.is_streaming = false;
        self.audio_buffer.clear();
        *self.startup_error.lock().await = None;

        match &stop_result {
            Ok(()) => log::info!("AssemblyAI stream stopped after graceful termination"),
            Err(error) => log::warn!("AssemblyAI stream stop failed: {}", error),
        }
        stop_result
    }

    async fn abort(&mut self) -> SttResult<()> {
        log::info!("AssemblyAI Provider: Aborting stream");
        self.stop_requested.store(true, Ordering::SeqCst);

        // Immediate shutdown - abort task without graceful close
        if let Some(task) = self.receiver_task.take() {
            task.abort();
            let _ = task.await;
        }

        self.ws_write = None;
        self.is_streaming = false;
        self.audio_buffer.clear();
        *self.startup_error.lock().await = None;

        log::info!("AssemblyAI stream aborted");
        Ok(())
    }

    fn name(&self) -> &str {
        "AssemblyAI Universal-Streaming (v3)"
    }

    fn is_online(&self) -> bool {
        true
    }
}

impl AssemblyAIProvider {
    /// Обрабатываем входящее сообщение от AssemblyAI
    fn handle_message(
        json: Value,
        on_partial: &TranscriptionCallback,
        on_final: &TranscriptionCallback,
        configured_language: &str,
    ) {
        let msg_type = json["type"].as_str();

        match msg_type {
            Some("Begin") => {
                log::info!("AssemblyAI session began");
                if let Some(session_id) = json["id"].as_str() {
                    log::debug!("Session ID: {}", session_id);
                }
            }

            Some("Turn") => {
                // AssemblyAI v3 использует тип "Turn" для всех транскрипций
                let is_end_of_turn = json["end_of_turn"].as_bool().unwrap_or(false);

                // Извлекаем язык из ответа (если есть) или используем сконфигурированный
                let detected_language = json
                    .get("language_code")
                    .and_then(|l| l.as_str())
                    .map(|s| s.to_string())
                    .or_else(|| {
                        json.get("language")
                            .and_then(|l| l.as_str())
                            .map(|s| s.to_string())
                    })
                    .or_else(|| Some(configured_language.to_string()));

                // Берем текст из transcript (utterance часто пуст)
                let text = json["transcript"].as_str();

                if let Some(text) = text {
                    if !text.is_empty() {
                        if is_end_of_turn {
                            log::info!("Final transcript: {}", text);

                            let transcription = Transcription {
                                text: text.to_string(),
                                confidence: json["end_of_turn_confidence"]
                                    .as_f64()
                                    .map(|v| v as f32),
                                is_final: true,
                                language: detected_language,
                                timestamp: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_else(|_| std::time::Duration::from_secs(0))
                                    .as_millis() as i64,
                                start: 0.0,    // AssemblyAI не предоставляет start время
                                duration: 0.0, // AssemblyAI не предоставляет duration
                            };

                            call_assemblyai_callback("final transcription", || {
                                on_final(transcription)
                            });
                        } else {
                            log::debug!("Partial transcript: {}", text);

                            let transcription = Transcription {
                                text: text.to_string(),
                                confidence: json["end_of_turn_confidence"]
                                    .as_f64()
                                    .map(|v| v as f32),
                                is_final: false,
                                language: detected_language.clone(),
                                timestamp: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_else(|_| std::time::Duration::from_secs(0))
                                    .as_millis() as i64,
                                start: 0.0,    // AssemblyAI не предоставляет start время
                                duration: 0.0, // AssemblyAI не предоставляет duration
                            };

                            call_assemblyai_callback("partial transcription", || {
                                on_partial(transcription)
                            });
                        }
                    }
                }
            }

            Some("Termination") | Some("End") | Some("SessionTerminated") => {
                log::info!("AssemblyAI session terminated");
            }

            Some("Error") => {
                log::error!("AssemblyAI error received: {:?}", json);
                if let Some(err_msg) = json.get("error").and_then(|e| e.as_str()) {
                    log::error!("Error message: {}", err_msg);
                }
            }

            Some(other) => {
                log::debug!("AssemblyAI message type: {}", other);
            }

            None => {
                log::warn!("AssemblyAI message without type field");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::SttProviderType;
    use std::sync::Mutex as StdMutex;
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;

    async fn spawn_assemblyai_termination_server() -> (String, JoinHandle<(Vec<u8>, Value)>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock AssemblyAI server");
        let address = listener.local_addr().expect("mock server address");
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept websocket client");
            let mut websocket = accept_async(stream).await.expect("accept websocket");
            websocket
                .send(Message::Text(
                    json!({ "type": "Begin", "id": "test-session" }).to_string(),
                ))
                .await
                .expect("send Begin");

            let mut audio = Vec::new();
            let terminate = loop {
                match websocket.next().await.expect("client websocket message") {
                    Ok(Message::Binary(bytes)) => audio.extend_from_slice(&bytes),
                    Ok(Message::Text(text)) => {
                        let value: Value = serde_json::from_str(&text).expect("valid client JSON");
                        if value.get("type").and_then(Value::as_str) == Some("Terminate") {
                            websocket
                                .send(Message::Text(
                                    json!({
                                        "type": "Turn",
                                        "end_of_turn": false,
                                        "transcript": "partial tail",
                                        "language_code": "ru"
                                    })
                                    .to_string(),
                                ))
                                .await
                                .expect("send partial Turn");
                            websocket
                                .send(Message::Text(
                                    json!({
                                        "type": "Turn",
                                        "end_of_turn": true,
                                        "transcript": "final tail",
                                        "language_code": "ru",
                                        "end_of_turn_confidence": 0.99
                                    })
                                    .to_string(),
                                ))
                                .await
                                .expect("send final Turn");
                            websocket
                                .send(Message::Text(
                                    json!({
                                        "type": "Termination",
                                        "audio_duration_seconds": 0.025,
                                        "session_duration_seconds": 0.1
                                    })
                                    .to_string(),
                                ))
                                .await
                                .expect("send Termination");
                            break value;
                        }
                    }
                    Ok(Message::Close(_)) => panic!("client closed before Terminate"),
                    Ok(_) => {}
                    Err(error) => panic!("mock websocket failed: {error}"),
                }
            };
            (audio, terminate)
        });

        (format!("ws://{address}/v3/ws"), task)
    }

    #[test]
    fn amplitude_stats_handle_full_pcm16_range_without_overflow() {
        let samples = [i16::MIN, i16::MAX, -1, 0, 1];

        let (peak, average) = pcm_amplitude_stats(&samples);

        assert_eq!(peak, 32_768);
        assert_eq!(average, 13_107);
    }

    #[test]
    fn amplitude_stats_handle_empty_audio() {
        assert_eq!(pcm_amplitude_stats(&[]), (0, 0));
    }

    #[test]
    fn server_error_classifies_auth_and_quota() {
        assert!(matches!(
            assemblyai_server_error("Invalid API key"),
            SttError::Authentication(_)
        ));
        assert!(matches!(
            assemblyai_server_error("Provider quota exceeded"),
            SttError::Connection(connection)
                if connection.details.category
                    == Some(SttConnectionCategory::ProviderQuotaExceeded)
        ));
    }

    #[test]
    fn unexpected_close_has_closed_category() {
        assert!(matches!(
            assemblyai_closed_error("socket ended"),
            SttError::Connection(connection)
                if connection.details.category == Some(SttConnectionCategory::Closed)
        ));
    }

    #[test]
    fn streaming_model_matches_current_language_support() {
        assert_eq!(assemblyai_speech_model("en-US"), "u3-rt-pro");
        assert_eq!(assemblyai_speech_model("de"), "u3-rt-pro");
        assert_eq!(assemblyai_speech_model("ru"), "whisper-rt");
        assert_eq!(assemblyai_speech_model("ja-JP"), "whisper-rt");

        let russian_url = assemblyai_stream_url(ASSEMBLYAI_WS_URL, "ru");
        assert!(russian_url.contains("speech_model=whisper-rt"));
        assert!(russian_url.contains("language_detection=true"));
        assert!(!russian_url.contains("language_code="));
    }

    #[test]
    fn audio_frames_stay_within_assemblyai_protocol_bounds() {
        assert_eq!(assemblyai_next_audio_frame_samples(799), None);
        assert_eq!(assemblyai_next_audio_frame_samples(800), Some(800));
        assert_eq!(assemblyai_next_audio_frame_samples(16_000), Some(16_000));
        assert_eq!(assemblyai_next_audio_frame_samples(16_500), Some(15_700));
        assert_eq!(assemblyai_next_audio_frame_samples(32_000), Some(16_000));
    }

    #[tokio::test]
    async fn initialize_trims_user_api_key() {
        let mut provider = AssemblyAIProvider::new();
        let mut config = SttConfig::new(SttProviderType::AssemblyAI);
        config.assemblyai_api_key = Some("  test-key  \n".to_string());

        provider
            .initialize(&config)
            .await
            .expect("initialize provider");

        assert_eq!(provider.api_key.as_deref(), Some("test-key"));
    }

    #[tokio::test]
    async fn graceful_stop_flushes_tail_and_waits_for_final_turn() {
        let (ws_base_url, server) = spawn_assemblyai_termination_server().await;
        let mut provider = AssemblyAIProvider::with_ws_base_url(ws_base_url);
        let mut config = SttConfig::new(SttProviderType::AssemblyAI).with_language("ru");
        config.assemblyai_api_key = Some("test-key".to_string());
        provider
            .initialize(&config)
            .await
            .expect("initialize provider");

        let finals = Arc::new(StdMutex::new(Vec::<Transcription>::new()));
        let finals_for_callback = finals.clone();
        provider
            .start_stream(
                Arc::new(|_| panic!("simulated partial callback panic")),
                Arc::new(move |transcription| {
                    finals_for_callback
                        .lock()
                        .expect("final callback lock")
                        .push(transcription);
                }),
                Arc::new(|error| panic!("unexpected AssemblyAI error: {error}")),
                Arc::new(|_, _| {}),
            )
            .await
            .expect("start mock stream");

        let tail_samples = vec![321i16; 400];
        provider
            .send_audio(&AudioChunk::new(tail_samples.clone(), 16_000, 1))
            .await
            .expect("buffer short audio tail");
        provider.stop_stream().await.expect("graceful stop");

        let (received_audio, terminate) = server.await.expect("mock server task");
        assert_eq!(received_audio.len(), ASSEMBLYAI_MIN_AUDIO_SAMPLES * 2);
        let received_samples: Vec<i16> = received_audio
            .chunks_exact(2)
            .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
            .collect();
        assert_eq!(
            &received_samples[..tail_samples.len()],
            tail_samples.as_slice()
        );
        assert!(received_samples[tail_samples.len()..]
            .iter()
            .all(|sample| *sample == 0));
        assert_eq!(terminate, json!({ "type": "Terminate" }));

        let finals = finals.lock().expect("finals lock");
        assert_eq!(finals.len(), 1);
        assert_eq!(finals[0].text, "final tail");
        assert_eq!(finals[0].language.as_deref(), Some("ru"));
    }
}
