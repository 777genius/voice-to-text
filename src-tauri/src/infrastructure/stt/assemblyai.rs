use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use http::Request;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

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
/// 2. Send session config: sample_rate, encoding, language_code
/// 3. Stream audio_data as base64-encoded PCM
/// 4. Receive: SessionBegins, PartialTranscript, FinalTranscript, SessionTerminated
const ASSEMBLYAI_WS_URL: &str = "wss://streaming.assemblyai.com/v3/ws";

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

async fn report_assemblyai_receiver_error(
    startup_error: &Arc<Mutex<Option<SttError>>>,
    session_ready: &Arc<Notify>,
    on_error: &ErrorCallback,
    error: SttError,
) {
    *startup_error.lock().await = Some(error.clone());
    on_error(error);
    session_ready.notify_one();
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
        }
    }
}

impl Default for AssemblyAIProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SttProvider for AssemblyAIProvider {
    async fn initialize(&mut self, config: &SttConfig) -> SttResult<()> {
        log::info!("AssemblyAI Provider: Initializing (v3)");

        // Приоритет: пользовательский ключ → встроенный ключ
        let api_key = config
            .assemblyai_api_key
            .clone()
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
            if config.assemblyai_api_key.is_some() {
                "user"
            } else {
                "embedded"
            }
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
            .and_then(|c| Some(c.language.clone()))
            .unwrap_or_else(|| "en".to_string());

        // 1. Build URL with query parameters
        let language = configured_language.clone();

        // Конвертируем короткие коды языков в полные BCP-47 для AssemblyAI
        let language_code = match language.as_str() {
            "ru" => "ru",   // Russian
            "en" => "en",   // English (global)
            "es" => "es",   // Spanish
            "fr" => "fr",   // French
            "de" => "de",   // German
            "it" => "it",   // Italian
            "pt" => "pt",   // Portuguese
            "nl" => "nl",   // Dutch
            "ja" => "ja",   // Japanese
            "ko" => "ko",   // Korean
            "zh" => "zh",   // Chinese
            other => other, // Pass as-is
        };

        let url = format!(
            "{}?sample_rate=16000&encoding=pcm_s16le&language_code={}",
            ASSEMBLYAI_WS_URL, language_code
        );

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

        let (ws_stream, _response) = connect_async(request).await.map_err(|e| {
            SttError::Connection(SttConnectionError::simple(format!(
                "WS connection failed: {}",
                e
            )))
        })?;

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

                                Self::handle_message(
                                    json,
                                    &on_partial,
                                    &on_final,
                                    &lang_for_transcription,
                                );
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
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.session_ready.notified(),
        )
        .await
        .map_err(|_| {
            SttError::Connection(SttConnectionError::with_category(
                "Timeout waiting for SessionBegins".to_string(),
                SttConnectionCategory::Timeout,
            ))
        })?;

        if let Some(error) = self.startup_error.lock().await.take() {
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

        // AssemblyAI требует минимум 50ms аудио
        // 50ms @ 16kHz = 800 samples
        const MIN_SAMPLES: usize = 800;
        const MAX_RETAINED_SAMPLES: usize = 16_000 * 10;

        // Отправляем когда накопилось достаточно
        if self.audio_buffer.len() >= MIN_SAMPLES {
            if self.audio_buffer.len() > MAX_RETAINED_SAMPLES {
                let excess = self.audio_buffer.len() - MAX_RETAINED_SAMPLES;
                log::warn!(
                    "AssemblyAI retry buffer overflow, dropping {} oldest samples",
                    excess
                );
                self.audio_buffer.drain(..excess);
            }

            // Convert i16 samples to bytes (little-endian PCM)
            let bytes: Vec<u8> = self
                .audio_buffer
                .iter()
                .flat_map(|&sample| sample.to_le_bytes())
                .collect();

            let duration_ms = (self.audio_buffer.len() * 1000) / 16000;
            log::debug!(
                "Sending {} samples (~{}ms, {} bytes) to AssemblyAI",
                self.audio_buffer.len(),
                duration_ms,
                bytes.len()
            );

            // Send as binary message (AssemblyAI v3 expects raw PCM binary data)
            write
                .send(Message::Binary(bytes))
                .await
                .map_err(|e| SttError::Processing(format!("Failed to send audio: {}", e)))?;
            // Keep buffered audio intact if send is cancelled or fails so a retry does
            // not silently lose the user's speech.
            self.audio_buffer.clear();
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

        // Отправляем остатки из буфера если есть
        if !self.audio_buffer.is_empty() {
            if let Some(write) = self.ws_write.as_mut() {
                let bytes: Vec<u8> = self
                    .audio_buffer
                    .iter()
                    .flat_map(|&sample| sample.to_le_bytes())
                    .collect();

                log::debug!(
                    "Flushing remaining {} samples from buffer",
                    self.audio_buffer.len()
                );
                let _ = write.send(Message::Binary(bytes)).await;
                self.audio_buffer.clear();
            }
        }

        // Send terminate message (optional for v3, but good practice)
        if let Some(write) = self.ws_write.as_mut() {
            let terminate_msg = json!({
                "terminate_session": true
            });

            let _ = write.send(Message::Text(terminate_msg.to_string())).await;
            let _ = write.send(Message::Close(None)).await;
        }

        // Abort receiver task
        if let Some(task) = self.receiver_task.take() {
            task.abort();
            let _ = task.await; // Ignore cancellation error
        }

        self.ws_write = None;
        self.is_streaming = false;

        log::info!("AssemblyAI stream stopped");
        Ok(())
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
                    .get("language")
                    .and_then(|l| l.as_str())
                    .map(|s| s.to_string())
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

                            on_final(transcription);
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

                            on_partial(transcription);
                        }
                    }
                }
            }

            Some("End") | Some("SessionTerminated") => {
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
}
