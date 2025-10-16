use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use http::Request;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::{Notify, Mutex};
use tokio::task::JoinHandle;
use tokio::time::Duration;
use tokio_tungstenite::{connect_async, tungstenite::Message, WebSocketStream, MaybeTlsStream};
use tokio::net::TcpStream;

use crate::domain::{
    AudioChunk, ErrorCallback, SttConfig, SttError, SttProvider, SttResult, Transcription, TranscriptionCallback,
};

/// Deepgram cloud STT provider
///
/// Endpoint: wss://api.deepgram.com/v1/listen
/// Pricing: ~$0.0077/min for Nova-3, ~$0.0043/min for Nova-2
/// Models:
/// - Nova-3: английский, испанский, французский, немецкий, португальский, итальянский, голландский
/// - Nova-2: русский и другие языки
/// Модель выбирается автоматически в зависимости от языка
///
/// Protocol:
/// 1. Connect with Authorization: Token API_KEY header
/// 2. Pass encoding, sample_rate, model, language as query params
/// 3. Stream raw PCM binary audio data
/// 4. Receive JSON messages: type=Results, is_final, speech_final
const DEEPGRAM_WS_URL: &str = "wss://api.deepgram.com/v1/listen";

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub struct DeepgramProvider {
    config: Option<SttConfig>,
    is_streaming: bool,
    is_paused: bool, // для keep-alive: true когда соединение живо но не обрабатываем аудио
    api_key: Option<String>,
    ws_write: Option<Arc<Mutex<futures_util::stream::SplitSink<WsStream, Message>>>>,
    receiver_task: Option<JoinHandle<()>>,
    keepalive_task: Option<JoinHandle<()>>, // отдельная задача для отправки KeepAlive
    session_ready: Arc<Notify>,
    audio_buffer: Vec<i16>,
    on_partial_callback: Option<TranscriptionCallback>, // сохраняем для resume
    on_final_callback: Option<TranscriptionCallback>,
    on_error_callback: Option<ErrorCallback>,
    sent_chunks_count: usize, // счетчик отправленных чанков для диагностики
    sent_bytes_total: usize, // общее количество отправленных байт
}

impl DeepgramProvider {
    pub fn new() -> Self {
        Self {
            config: None,
            is_streaming: false,
            is_paused: false,
            api_key: None,
            ws_write: None,
            receiver_task: None,
            keepalive_task: None,
            session_ready: Arc::new(Notify::new()),
            audio_buffer: Vec::new(),
            on_partial_callback: None,
            on_final_callback: None,
            on_error_callback: None,
            sent_chunks_count: 0,
            sent_bytes_total: 0,
        }
    }
}

impl Default for DeepgramProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SttProvider for DeepgramProvider {
    async fn initialize(&mut self, config: &SttConfig) -> SttResult<()> {
        log::info!("DeepgramProvider: Initializing");

        if config.api_key.is_none() {
            return Err(SttError::Configuration(
                "API key is required for Deepgram".to_string(),
            ));
        }

        self.api_key = config.api_key.clone();
        self.config = Some(config.clone());
        Ok(())
    }

    async fn start_stream(
        &mut self,
        on_partial: TranscriptionCallback,
        on_final: TranscriptionCallback,
        on_error: ErrorCallback,
    ) -> SttResult<()> {
        log::info!("DeepgramProvider: Starting stream");

        if self.is_streaming {
            return Err(SttError::Processing(
                "Stream already active".to_string(),
            ));
        }

        let api_key = self.api_key.as_ref()
            .ok_or_else(|| SttError::Configuration("API key not set".to_string()))?
            .clone();

        let language = self.config.as_ref()
            .and_then(|c| Some(c.language.clone()))
            .unwrap_or_else(|| "en".to_string());

        // Определяем модель из конфига
        // Nova-3 поддерживает только английский и несколько основных языков
        // Nova-2 поддерживает больше языков включая русский
        let model = self.config.as_ref()
            .and_then(|c| c.model.clone())
            .unwrap_or_else(|| {
                // Автоматически выбираем модель в зависимости от языка
                match language.as_str() {
                    "en" | "es" | "fr" | "de" | "pt" | "it" | "nl" => "nova-3",
                    _ => "nova-2", // для остальных языков используем nova-2
                }.to_string()
            });

        log::info!("Using Deepgram model '{}' for language '{}'", model, language);

        // Собираем URL с параметрами (добавляем channels=1 для mono)
        let url = format!(
            "{}?encoding=linear16&sample_rate=16000&channels=1&model={}&language={}&punctuate=true&interim_results=true",
            DEEPGRAM_WS_URL,
            model,
            language
        );

        log::debug!("Connecting to Deepgram: {}", url);

        // Формируем WebSocket запрос с заголовком авторизации
        let request = Request::builder()
            .method("GET")
            .uri(&url)
            .header("Host", "api.deepgram.com")
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", tokio_tungstenite::tungstenite::handshake::client::generate_key())
            .header("Authorization", format!("Token {}", api_key))
            .body(())
            .map_err(|e| SttError::Connection(format!("Failed to build WS request: {}", e)))?;

        let (ws_stream, _response) = connect_async(request)
            .await
            .map_err(|e| SttError::Connection(format!("WS connection failed: {}", e)))?;

        log::info!("Deepgram WebSocket connected");

        let (write, mut read) = ws_stream.split();

        // Оборачиваем write в Arc<Mutex<>> для совместного использования в задачах
        let ws_write = Arc::new(Mutex::new(write));

        // Пересоздаем Notify для новой сессии (фикс повторного использования)
        self.session_ready = Arc::new(Notify::new());

        // Клонируем callbacks для передачи в receiver задачу
        let on_partial_for_receiver = on_partial.clone();
        let on_final_for_receiver = on_final.clone();
        let on_error_for_receiver = on_error.clone();

        // Запускаем фоновую задачу для приема сообщений
        let session_notify = self.session_ready.clone();
        let receiver_task = tokio::spawn(async move {
            log::debug!("Deepgram receiver task started");

            while let Some(msg_result) = read.next().await {
                match msg_result {
                    Ok(Message::Text(text)) => {
                        log::debug!("Deepgram received text: {}", text);

                        match serde_json::from_str::<Value>(&text) {
                            Ok(json) => {
                                let msg_type = json["type"].as_str();

                                // Уведомляем что сессия готова при получении Metadata
                                if msg_type == Some("Metadata") {
                                    log::info!("Deepgram session ready, metadata received");
                                    session_notify.notify_one();
                                }

                                Self::handle_message(json, &on_partial_for_receiver, &on_final_for_receiver);
                            }
                            Err(e) => {
                                log::error!("Failed to parse Deepgram message: {}", e);
                                log::error!("Raw message: {}", text);
                            }
                        }
                    }
                    Ok(Message::Close(frame)) => {
                        log::info!("Deepgram WebSocket closed: {:?}", frame);

                        // Проверяем тип закрытия - если это ошибка, уведомляем UI
                        if let Some(close_frame) = &frame {
                            // Определяем тип ошибки по сообщению
                            let reason = close_frame.reason.to_string();
                            let error_type = if reason.contains("timeout") || reason.contains("net0001") {
                                "timeout"
                            } else if reason.contains("auth") || reason.contains("401") {
                                "authentication"
                            } else {
                                "connection"
                            };

                            // Вызываем error callback если это не нормальное закрытие
                            if close_frame.code != tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Normal {
                                log::error!("Deepgram connection closed with error: {} (type: {})", reason, error_type);
                                on_error_for_receiver(reason.clone(), error_type.to_string());
                            }
                        }

                        break;
                    }
                    Ok(Message::Binary(data)) => {
                        log::debug!("Deepgram received binary: {} bytes", data.len());
                    }
                    Ok(Message::Ping(_)) => {
                        log::trace!("Deepgram received Ping");
                    }
                    Ok(Message::Pong(_)) => {
                        log::trace!("Deepgram received Pong");
                    }
                    Err(e) => {
                        log::error!("Deepgram WebSocket error: {}", e);
                        break;
                    }
                    Ok(msg) => {
                        log::warn!("Deepgram unexpected message: {:?}", msg);
                    }
                }
            }

            log::debug!("Deepgram receiver task ended");
        });

        // Запускаем отдельную задачу для отправки KeepAlive (каждые 5 секунд)
        // Это нужно для keep-alive функционала - держать соединение живым между записями
        let ws_write_for_keepalive = ws_write.clone();
        let keepalive_task = tokio::spawn(async move {
            log::debug!("Deepgram KeepAlive task started");

            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;

                let keepalive_msg = json!({"type": "KeepAlive"});
                let mut write = ws_write_for_keepalive.lock().await;
                match write.send(Message::Text(keepalive_msg.to_string())).await {
                    Ok(_) => {
                        log::trace!("Sent KeepAlive to Deepgram");
                    },
                    Err(e) => {
                        log::debug!("KeepAlive failed, connection closed: {}", e);
                        break;
                    }
                }
            }

            log::debug!("Deepgram KeepAlive task ended");
        });

        self.ws_write = Some(ws_write);
        self.receiver_task = Some(receiver_task);
        self.keepalive_task = Some(keepalive_task);
        self.is_streaming = true;
        self.is_paused = false;

        // Сбрасываем счетчики при новом соединении
        self.sent_chunks_count = 0;
        self.sent_bytes_total = 0;

        // Сохраняем callbacks для возможности resume
        self.on_partial_callback = Some(on_partial);
        self.on_final_callback = Some(on_final);
        self.on_error_callback = Some(on_error);

        // Примечание: Deepgram отправляет Metadata только после получения аудио данных
        // Поэтому мы не ждем Metadata здесь, а считаем что соединение установлено успешно
        log::info!("Deepgram WebSocket stream started successfully");
        log::info!("Note: Metadata will be received after sending first audio chunk");
        Ok(())
    }

    async fn send_audio(&mut self, chunk: &AudioChunk) -> SttResult<()> {
        if !self.is_streaming {
            return Err(SttError::Processing("Not streaming".to_string()));
        }

        // Если на паузе - не обрабатываем аудио (keep-alive режим)
        if self.is_paused {
            return Ok(());
        }

        let write = self.ws_write.as_ref()
            .ok_or_else(|| SttError::Processing("WebSocket write handle not available".to_string()))?;

        // KeepAlive теперь отправляется отдельной задачей, не нужно здесь

        // Добавляем в буфер
        self.audio_buffer.extend_from_slice(&chunk.data);

        // Отправляем чанки по 50ms для более быстрой реакции
        // 50ms @ 16kHz = 800 samples (накапливается за ~2-3 чанка)
        const MIN_SAMPLES: usize = 800;

        if self.audio_buffer.len() >= MIN_SAMPLES {
            // Конвертируем i16 семплы в байты (little-endian PCM)
            let bytes: Vec<u8> = self.audio_buffer
                .iter()
                .flat_map(|&sample| sample.to_le_bytes())
                .collect();

            // Очищаем буфер ПЕРЕД отправкой (фикс утечки памяти)
            self.audio_buffer.clear();

            // Отправляем бинарные данные (обрабатываем ошибку если соединение закрыто)
            let send_start = std::time::Instant::now();
            let bytes_len = bytes.len();

            let mut write_guard = write.lock().await;
            match write_guard.send(Message::Binary(bytes)).await {
                Ok(_) => {
                    let send_duration = send_start.elapsed();

                    // Обновляем счетчики
                    self.sent_chunks_count += 1;
                    self.sent_bytes_total += bytes_len;

                    // Логируем каждый 10-й чанк для диагностики
                    if self.sent_chunks_count % 10 == 0 {
                        log::debug!("Sent chunk #{} to Deepgram: {} bytes ({:.2} KB total, took {:.1}ms)",
                            self.sent_chunks_count, bytes_len,
                            self.sent_bytes_total as f64 / 1024.0,
                            send_duration.as_millis());
                    }

                    // Предупреждаем если отправка медленная (>100ms может быть проблемой сети)
                    if send_duration.as_millis() > 100 {
                        log::warn!("Slow WebSocket send detected: chunk #{} took {:.1}ms (network issue?)",
                            self.sent_chunks_count, send_duration.as_millis());
                    }
                },
                Err(e) => {
                    log::debug!("Could not send audio data (connection closed): {}", e);
                    // Соединение закрыто - отмечаем что больше не стримим
                    self.is_streaming = false;
                    return Err(SttError::Connection("WebSocket connection closed".to_string()));
                }
            }
        }

        Ok(())
    }

    async fn stop_stream(&mut self) -> SttResult<()> {
        log::info!("DeepgramProvider: Stopping stream");

        if !self.is_streaming {
            log::warn!("Stream not active");
            return Ok(());
        }

        // Логируем статистику отправки перед остановкой
        log::info!("Deepgram session stats: sent {} chunks, {:.2} KB total",
            self.sent_chunks_count,
            self.sent_bytes_total as f64 / 1024.0);

        // Отправляем остатки буфера (игнорируем ошибки если соединение уже закрыто)
        if !self.audio_buffer.is_empty() {
            if let Some(write) = self.ws_write.as_ref() {
                let bytes: Vec<u8> = self.audio_buffer
                    .iter()
                    .flat_map(|&sample| sample.to_le_bytes())
                    .collect();

                log::debug!("Flushing remaining {} samples from buffer", self.audio_buffer.len());

                // Игнорируем ошибку если WebSocket уже закрыт
                let mut write_guard = write.lock().await;
                match write_guard.send(Message::Binary(bytes)).await {
                    Ok(_) => {},
                    Err(e) => log::debug!("Could not send final buffer (connection may be closed): {}", e),
                }
                self.audio_buffer.clear();
            }
        }

        // Отправляем CloseStream сообщение (graceful shutdown по документации Deepgram)
        if let Some(write) = self.ws_write.as_ref() {
            let close_msg = json!({"type": "CloseStream"});

            // Игнорируем ошибки отправки если соединение уже закрыто
            let mut write_guard = write.lock().await;
            match write_guard.send(Message::Text(close_msg.to_string())).await {
                Ok(_) => {
                    log::debug!("CloseStream sent, waiting for final results...");
                    // Даем больше времени на получение финальных результатов (1 секунда)
                    tokio::time::sleep(Duration::from_millis(1000)).await;
                },
                Err(e) => log::debug!("Could not send CloseStream (connection may be closed): {}", e),
            }

            // Не отправляем Message::Close - Deepgram сам закрывает соединение после CloseStream
        }

        // Даем receiver task еще немного времени на обработку последних сообщений
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Останавливаем keepalive задачу
        if let Some(task) = self.keepalive_task.take() {
            task.abort();
            let _ = task.await;
        }

        // Останавливаем фоновую задачу receiver
        if let Some(task) = self.receiver_task.take() {
            task.abort();
            let _ = task.await;
        }

        self.ws_write = None;
        self.is_streaming = false;
        self.is_paused = false;
        self.on_partial_callback = None;
        self.on_final_callback = None;
        self.on_error_callback = None;
        self.sent_chunks_count = 0;
        self.sent_bytes_total = 0;

        log::info!("Deepgram stream stopped");
        Ok(())
    }

    async fn abort(&mut self) -> SttResult<()> {
        log::info!("DeepgramProvider: Aborting stream (sent {} chunks, {:.2} KB)",
            self.sent_chunks_count,
            self.sent_bytes_total as f64 / 1024.0);

        // Останавливаем keepalive задачу
        if let Some(task) = self.keepalive_task.take() {
            task.abort();
            let _ = task.await;
        }

        // Останавливаем receiver задачу
        if let Some(task) = self.receiver_task.take() {
            task.abort();
            let _ = task.await;
        }

        self.ws_write = None;
        self.is_streaming = false;
        self.is_paused = false;
        self.audio_buffer.clear();
        self.on_partial_callback = None;
        self.on_final_callback = None;
        self.on_error_callback = None;
        self.sent_chunks_count = 0;
        self.sent_bytes_total = 0;

        log::info!("Deepgram stream aborted");
        Ok(())
    }

    /// Ставит стрим на паузу (keep-alive режим)
    /// Соединение остается живым, KeepAlive продолжает отправляться,
    /// но аудио не обрабатывается
    async fn pause_stream(&mut self) -> SttResult<()> {
        log::info!("DeepgramProvider: Pausing stream (keep-alive mode)");

        if !self.is_streaming {
            return Err(SttError::Processing(
                "Cannot pause - stream not active".to_string(),
            ));
        }

        if self.is_paused {
            log::debug!("Stream already paused");
            return Ok(());
        }

        self.is_paused = true;
        self.audio_buffer.clear(); // Очищаем буфер при паузе

        log::info!("Deepgram stream paused, connection kept alive");
        Ok(())
    }

    /// Возобновляет стрим после паузы
    /// Обновляет callbacks и сбрасывает буфер
    async fn resume_stream(
        &mut self,
        on_partial: TranscriptionCallback,
        on_final: TranscriptionCallback,
        on_error: ErrorCallback,
    ) -> SttResult<()> {
        log::info!("DeepgramProvider: Resuming stream from pause");

        if !self.is_streaming {
            return Err(SttError::Processing(
                "Cannot resume - stream not active".to_string(),
            ));
        }

        if !self.is_paused {
            return Err(SttError::Processing(
                "Cannot resume - stream not paused".to_string(),
            ));
        }

        self.is_paused = false;
        self.audio_buffer.clear();

        // Обновляем callbacks
        self.on_partial_callback = Some(on_partial);
        self.on_final_callback = Some(on_final);
        self.on_error_callback = Some(on_error);

        // Пересоздаем session_ready для новой сессии записи
        self.session_ready = Arc::new(Notify::new());

        log::info!("Deepgram stream resumed, ready to process audio");
        Ok(())
    }

    fn name(&self) -> &str {
        "Deepgram (Nova-2/Nova-3)"
    }

    fn supports_keep_alive(&self) -> bool {
        true
    }

    fn is_connection_alive(&self) -> bool {
        // Соединение живо если стрим активен и на паузе (keep-alive режим)
        self.is_streaming && self.is_paused
    }

    fn is_online(&self) -> bool {
        true
    }
}

impl DeepgramProvider {
    /// Обрабатываем входящее сообщение от Deepgram
    fn handle_message(
        json: Value,
        on_partial: &TranscriptionCallback,
        on_final: &TranscriptionCallback,
    ) {
        let msg_type = json["type"].as_str();

        match msg_type {
            Some("Results") => {
                // Получаем флаги финальности
                let is_final = json["is_final"].as_bool().unwrap_or(false);
                let speech_final = json["speech_final"].as_bool().unwrap_or(false);

                // Получаем временные метки сегмента
                let start = json["start"].as_f64().unwrap_or(0.0);
                let duration = json["duration"].as_f64().unwrap_or(0.0);

                log::debug!("Processing Results: is_final={}, speech_final={}, start={:.2}s, duration={:.2}s",
                    is_final, speech_final, start, duration);

                // Извлекаем транскрипцию из первой альтернативы
                // Структура Streaming API: channel.alternatives[0]
                if let Some(channel) = json.get("channel") {
                    if let Some(alternatives) = channel.get("alternatives").and_then(|a| a.as_array()) {
                        log::trace!("Found {} alternative(s)", alternatives.len());
                        if let Some(first_alt) = alternatives.first() {
                            let text = first_alt["transcript"].as_str().unwrap_or("");
                            log::debug!("Extracted transcript: '{}' (start={:.2}s)", text, start);

                            if !text.is_empty() {
                                let confidence = first_alt["confidence"].as_f64().map(|v| v as f32);

                                // Извлекаем язык из alternatives[0].languages (по документации)
                                let detected_language = first_alt.get("languages")
                                    .and_then(|l| l.as_array())
                                    .and_then(|arr| arr.first())
                                    .and_then(|lang| lang.as_str())
                                    .map(|s| s.to_string());

                                // Deepgram отправляет:
                                // - is_final=false: промежуточный результат внутри сегмента
                                // - is_final=true, speech_final=false: сегмент завершен, но речь продолжается
                                // - is_final=true, speech_final=true: вся речь завершена

                                let transcription = Transcription {
                                    text: text.to_string(),
                                    confidence,
                                    is_final, // передаем оригинальный флаг is_final из Deepgram
                                    language: detected_language,
                                    timestamp: std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_else(|_| std::time::Duration::from_secs(0))
                                        .as_secs() as i64,
                                };

                                // Отправляем как final только когда ВСЯ речь завершена (speech_final=true)
                                if is_final && speech_final {
                                    log::info!("✅ Final transcript: '{}' (confidence: {:?})", text, confidence);
                                    on_final(transcription);
                                } else {
                                    // Все остальные (промежуточные и финализированные сегменты) - как partial
                                    // UI различит по флагу is_final
                                    if is_final {
                                        log::info!("🔒 Segment finalized: '{}' (confidence: {:?})", text, confidence);
                                    } else {
                                        log::info!("📝 Partial transcript: '{}' (confidence: {:?})", text, confidence);
                                    }
                                    on_partial(transcription);
                                }
                            } else {
                                log::trace!("Skipping empty transcript");
                            }
                        } else {
                            log::trace!("No alternatives found");
                        }
                    } else {
                        log::trace!("No alternatives array");
                    }
                } else {
                    log::trace!("No channel field in message");
                }
            }

            Some("Metadata") => {
                log::debug!("Deepgram metadata received");
                if let Some(request_id) = json["request_id"].as_str() {
                    log::debug!("Request ID: {}", request_id);
                }
            }

            Some("Error") => {
                log::error!("Deepgram error received: {:?}", json);
                if let Some(err_msg) = json.get("err_msg").and_then(|e| e.as_str()) {
                    log::error!("Error message: {}", err_msg);
                }
                if let Some(err_code) = json.get("err_code").and_then(|c| c.as_str()) {
                    log::error!("Error code: {}", err_code);
                }
            }

            Some(other) => {
                log::debug!("Deepgram message type: {}", other);
            }

            None => {
                log::warn!("Deepgram message without type field");
            }
        }
    }
}
