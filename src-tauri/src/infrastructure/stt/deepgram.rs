use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use http::Request;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{Notify, Mutex};
use tokio::task::JoinHandle;
use tokio::time::Duration;
use tokio_tungstenite::{connect_async, tungstenite::Message, WebSocketStream, MaybeTlsStream};
use tokio::net::TcpStream;

use crate::domain::{
    AudioChunk, ConnectionQualityCallback, ErrorCallback, SttConfig, SttError, SttProvider, SttResult, Transcription, TranscriptionCallback,
};
use crate::infrastructure::embedded_keys;

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
    is_paused_flag: Arc<Mutex<bool>>, // shared флаг для receiver_task чтобы игнорировать сообщения во время паузы
    api_key: Option<String>,
    ws_write: Option<Arc<Mutex<futures_util::stream::SplitSink<WsStream, Message>>>>,
    receiver_task: Option<JoinHandle<()>>,
    keepalive_task: Option<JoinHandle<()>>, // отдельная задача для отправки KeepAlive
    session_ready: Arc<Notify>,
    audio_buffer: Vec<i16>,
    on_partial_callback: Option<TranscriptionCallback>, // сохраняем для resume
    on_final_callback: Option<TranscriptionCallback>,
    on_error_callback: Option<ErrorCallback>,
    on_connection_quality_callback: Option<ConnectionQualityCallback>,
    sent_chunks_count: usize, // счетчик отправленных чанков для диагностики
    sent_bytes_total: usize, // общее количество отправленных байт

    // Поля для мониторинга качества связи
    consecutive_errors: usize, // счётчик последовательных ошибок
    last_successful_send: Option<Instant>, // время последней успешной отправки
    last_server_response: Arc<Mutex<Option<Instant>>>, // время последнего ответа от сервера (shared с receiver task)
    current_quality: Arc<Mutex<String>>, // текущее состояние качества связи (Good/Poor/Recovering)

    // Поля для автоматического переподключения
    is_reconnecting: bool, // флаг что идёт процесс переподключения
    reconnect_attempts: usize, // количество попыток переподключения
    audio_buffer_during_reconnect: Arc<Mutex<Vec<AudioChunk>>>, // буфер аудио во время reconnect
}

impl DeepgramProvider {
    pub fn new() -> Self {
        Self {
            config: None,
            is_streaming: false,
            is_paused: false,
            is_paused_flag: Arc::new(Mutex::new(false)),
            api_key: None,
            ws_write: None,
            receiver_task: None,
            keepalive_task: None,
            session_ready: Arc::new(Notify::new()),
            audio_buffer: Vec::new(),
            on_partial_callback: None,
            on_final_callback: None,
            on_error_callback: None,
            on_connection_quality_callback: None,
            sent_chunks_count: 0,
            sent_bytes_total: 0,
            consecutive_errors: 0,
            last_successful_send: None,
            last_server_response: Arc::new(Mutex::new(None)),
            current_quality: Arc::new(Mutex::new("Good".to_string())),
            is_reconnecting: false,
            reconnect_attempts: 0,
            audio_buffer_during_reconnect: Arc::new(Mutex::new(Vec::new())),
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

        // Приоритет: пользовательский ключ → встроенный ключ
        let api_key = config.deepgram_api_key.clone()
            .or_else(|| {
                // Fallback на встроенный ключ
                if embedded_keys::has_embedded_deepgram_key() {
                    Some(embedded_keys::EMBEDDED_DEEPGRAM_KEY.to_string())
                } else {
                    None
                }
            })
            .ok_or_else(|| SttError::Configuration(
                "Deepgram API key is required (either user key or embedded key)".to_string(),
            ))?;

        log::info!("DeepgramProvider: Using {} API key",
            if config.deepgram_api_key.is_some() { "user" } else { "embedded" });

        self.api_key = Some(api_key);
        self.config = Some(config.clone());
        Ok(())
    }

    async fn start_stream(
        &mut self,
        on_partial: TranscriptionCallback,
        on_final: TranscriptionCallback,
        on_error: ErrorCallback,
        on_connection_quality: ConnectionQualityCallback,
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
        let on_connection_quality_for_receiver = on_connection_quality.clone();

        // Инициализируем мониторинг качества связи
        self.consecutive_errors = 0;
        self.last_successful_send = Some(Instant::now());
        *self.last_server_response.lock().await = Some(Instant::now());
        *self.current_quality.lock().await = "Good".to_string();

        // Запускаем фоновую задачу для приема сообщений
        let session_notify = self.session_ready.clone();
        let last_server_response_for_receiver = self.last_server_response.clone();
        let current_quality_for_receiver = self.current_quality.clone();
        let is_paused_flag_for_receiver = self.is_paused_flag.clone(); // клон для receiver task
        let receiver_task = tokio::spawn(async move {
            log::debug!("Deepgram receiver task started");

            // Запускаем отдельную задачу для мониторинга качества связи
            let last_server_response_monitor = last_server_response_for_receiver.clone();
            let current_quality_monitor = current_quality_for_receiver.clone();
            let on_connection_quality_monitor = on_connection_quality_for_receiver.clone();
            let is_paused_flag_for_monitor = is_paused_flag_for_receiver.clone();

            let monitor_task = tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(1)).await;

                    // В режиме паузы не мониторим качество - Deepgram не отправляет сообщения
                    if *is_paused_flag_for_monitor.lock().await {
                        continue;
                    }

                    let last_response = *last_server_response_monitor.lock().await;
                    let mut current_quality = current_quality_monitor.lock().await;

                    if let Some(last_time) = last_response {
                        let elapsed = last_time.elapsed();

                        // Если нет ответа от сервера больше 3 секунд - плохая связь
                        if elapsed > Duration::from_secs(3) && *current_quality == "Good" {
                            log::warn!("Connection quality degraded: no server response for {:.1}s", elapsed.as_secs_f64());
                            *current_quality = "Poor".to_string();
                            on_connection_quality_monitor("Poor".to_string(), Some("No server response for 3+ seconds".to_string()));
                        }
                        // Если связь восстановилась (получили ответ после плохой связи)
                        else if elapsed <= Duration::from_secs(2) && *current_quality == "Poor" {
                            log::info!("Connection quality recovering: server responding again");
                            *current_quality = "Recovering".to_string();
                            on_connection_quality_monitor("Recovering".to_string(), None);

                            // Через 2 секунды стабильной работы считаем что всё хорошо
                            let quality_for_check = current_quality_monitor.clone();
                            let callback_for_check = on_connection_quality_monitor.clone();
                            tokio::spawn(async move {
                                tokio::time::sleep(Duration::from_secs(2)).await;
                                let mut q = quality_for_check.lock().await;
                                if *q == "Recovering" {
                                    log::info!("Connection fully recovered");
                                    *q = "Good".to_string();
                                    callback_for_check("Good".to_string(), None);
                                }
                            });
                        }
                    }
                }
            });

            while let Some(msg_result) = read.next().await {
                // Проверяем флаг паузы и игнорируем сообщения если на паузе
                if *is_paused_flag_for_receiver.lock().await {
                    log::trace!("Ignoring message from Deepgram - stream is paused (keep-alive mode)");
                    continue;
                }

                // Обновляем время последнего ответа от сервера
                *last_server_response_for_receiver.lock().await = Some(Instant::now());

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

            // Останавливаем задачу мониторинга качества связи
            monitor_task.abort();
            let _ = monitor_task.await;

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
        self.on_connection_quality_callback = Some(on_connection_quality);

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

        // Если идёт переподключение - буферизуем аудио и не пытаемся отправлять
        if self.is_reconnecting {
            let mut buffer = self.audio_buffer_during_reconnect.lock().await;

            // Ограничиваем размер буфера (макс 80 чанков = ~4 секунды @ 50ms)
            if buffer.len() < 80 {
                buffer.push(chunk.clone());
                log::debug!("Buffering audio chunk during reconnect ({} buffered)", buffer.len());
            } else {
                log::warn!("Reconnect buffer full, dropping oldest chunk");
                buffer.remove(0);
                buffer.push(chunk.clone());
            }

            return Ok(());
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

                    // Сбрасываем счетчик ошибок при успешной отправке
                    let had_errors = self.consecutive_errors > 0;
                    self.consecutive_errors = 0;
                    self.last_successful_send = Some(Instant::now());

                    // Если были ошибки и теперь отправка успешна - связь восстанавливается
                    if had_errors {
                        let mut current_quality = self.current_quality.lock().await;
                        if *current_quality == "Poor" {
                            log::info!("Connection quality recovering after {} errors", had_errors);
                            *current_quality = "Recovering".to_string();
                            if let Some(callback) = &self.on_connection_quality_callback {
                                callback("Recovering".to_string(), None);
                            }

                            // Через 2 секунды стабильной работы считаем что всё хорошо
                            let quality_arc = self.current_quality.clone();
                            let callback_clone = self.on_connection_quality_callback.clone();
                            tokio::spawn(async move {
                                tokio::time::sleep(Duration::from_secs(2)).await;
                                let mut q = quality_arc.lock().await;
                                if *q == "Recovering" {
                                    log::info!("Connection fully recovered");
                                    *q = "Good".to_string();
                                    if let Some(cb) = callback_clone {
                                        cb("Good".to_string(), None);
                                    }
                                }
                            });
                        }
                    }

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
                    log::warn!("Could not send audio data (connection error): {}", e);

                    // Инкрементируем счетчик последовательных ошибок
                    self.consecutive_errors += 1;

                    // Если 3 или более ошибок подряд - пытаемся переподключиться
                    if self.consecutive_errors >= 3 {
                        log::warn!("Connection lost after {} errors, attempting reconnect", self.consecutive_errors);

                        // Освобождаем write_guard перед вызовом reconnect (иначе будет ошибка borrow checker)
                        drop(write_guard);

                        // Буферизуем текущий чанк перед попыткой reconnect
                        self.audio_buffer_during_reconnect.lock().await.push(chunk.clone());

                        // Пытаемся переподключиться
                        match self.reconnect().await {
                            Ok(_) => {
                                log::info!("Reconnected successfully, resuming audio processing");
                                // Reconnect успешен - продолжаем обработку
                                return Ok(());
                            }
                            Err(reconnect_error) => {
                                // Все попытки reconnect провалились - это критическая ошибка
                                log::error!("Failed to reconnect: {}", reconnect_error);

                                // Уведомляем UI об ошибке
                                if let Some(callback) = &self.on_error_callback {
                                    callback(
                                        format!("Connection lost: {}", reconnect_error),
                                        "connection".to_string()
                                    );
                                }

                                self.is_streaming = false;
                                return Err(reconnect_error);
                            }
                        }
                    }

                    // Меньше 3 ошибок - просто возвращаем ошибку без reconnect
                    return Err(SttError::Connection(format!("WebSocket send failed: {}", e)));
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
        self.on_connection_quality_callback = None;
        self.sent_chunks_count = 0;
        self.sent_bytes_total = 0;
        self.consecutive_errors = 0;
        self.last_successful_send = None;

        // Очищаем reconnect state
        self.is_reconnecting = false;
        self.reconnect_attempts = 0;
        self.audio_buffer_during_reconnect.lock().await.clear();

        // Очищаем shared state
        *self.last_server_response.lock().await = None;
        *self.current_quality.lock().await = "Good".to_string();

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
        self.on_connection_quality_callback = None;
        self.sent_chunks_count = 0;
        self.sent_bytes_total = 0;
        self.consecutive_errors = 0;
        self.last_successful_send = None;

        // Очищаем reconnect state
        self.is_reconnecting = false;
        self.reconnect_attempts = 0;
        self.audio_buffer_during_reconnect.lock().await.clear();

        // Очищаем shared state
        *self.last_server_response.lock().await = None;
        *self.current_quality.lock().await = "Good".to_string();

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
        *self.is_paused_flag.lock().await = true; // устанавливаем флаг для receiver_task
        self.audio_buffer.clear(); // Очищаем буфер при паузе

        // Очищаем reconnect state
        self.is_reconnecting = false;
        self.reconnect_attempts = 0;
        self.audio_buffer_during_reconnect.lock().await.clear();

        log::info!("Deepgram stream paused, connection kept alive (messages will be ignored)");
        Ok(())
    }

    /// Возобновляет стрим после паузы
    /// Обновляет callbacks и сбрасывает буфер
    async fn resume_stream(
        &mut self,
        on_partial: TranscriptionCallback,
        on_final: TranscriptionCallback,
        on_error: ErrorCallback,
        on_connection_quality: ConnectionQualityCallback,
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

        // Проверяем реальное состояние соединения перед resume
        let (is_healthy, reason) = self.check_connection_health().await;
        if !is_healthy {
            let error_msg = format!(
                "Cannot resume - connection is not healthy: {}",
                reason.unwrap_or_else(|| "Unknown reason".to_string())
            );
            log::warn!("{}", error_msg);

            // Сбрасываем флаги чтобы система создала новое соединение
            self.is_streaming = false;
            self.is_paused = false;

            // Очищаем мёртвые tasks и handles
            if let Some(task) = self.receiver_task.take() {
                task.abort();
                let _ = task.await;
            }
            if let Some(task) = self.keepalive_task.take() {
                task.abort();
                let _ = task.await;
            }
            self.ws_write = None;

            return Err(SttError::Connection(error_msg));
        }

        self.is_paused = false;
        *self.is_paused_flag.lock().await = false; // снимаем флаг для receiver_task
        self.audio_buffer.clear();

        // Обновляем callbacks
        self.on_partial_callback = Some(on_partial);
        self.on_final_callback = Some(on_final);
        self.on_error_callback = Some(on_error);
        self.on_connection_quality_callback = Some(on_connection_quality);

        // Сбрасываем мониторинг качества связи
        self.consecutive_errors = 0;
        *self.current_quality.lock().await = "Good".to_string();
        // Обновляем время последнего ответа чтобы дать кредит на первые секунды
        *self.last_server_response.lock().await = Some(Instant::now());

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
        // Базовая проверка (синхронная)
        if !(self.is_streaming && self.is_paused && self.ws_write.is_some()) {
            return false;
        }

        // Проверяем что tasks не завершились
        if let Some(task) = &self.receiver_task {
            if task.is_finished() {
                return false;
            }
        }
        if let Some(task) = &self.keepalive_task {
            if task.is_finished() {
                return false;
            }
        }

        true
    }

    fn is_online(&self) -> bool {
        true
    }
}

impl DeepgramProvider {
    /// Проверяет реальное состояние соединения
    /// Возвращает (is_healthy, reason_if_unhealthy)
    async fn check_connection_health(&self) -> (bool, Option<String>) {
        // Проверка 1: WebSocket write handle должен существовать
        if self.ws_write.is_none() {
            return (false, Some("WebSocket write handle not available".to_string()));
        }

        // Проверка 2: Receiver task должен быть живым
        if let Some(task) = &self.receiver_task {
            if task.is_finished() {
                return (false, Some("Receiver task has terminated".to_string()));
            }
        }

        // Проверка 3: KeepAlive task должен быть живым
        if let Some(task) = &self.keepalive_task {
            if task.is_finished() {
                return (false, Some("KeepAlive task has terminated".to_string()));
            }
        }

        // Проверка 4: Проверяем время последнего ответа от сервера
        // В режиме паузы Deepgram не отправляет сообщения - это нормально.
        // Достаточно что receiver и keepalive tasks живы.
        if !self.is_paused {
            if let Some(last_response) = *self.last_server_response.lock().await {
                let elapsed = last_response.elapsed();

                // Если прошло больше 10 секунд с последнего ответа - соединение мёртвое
                if elapsed > Duration::from_secs(10) {
                    return (false, Some(format!(
                        "No server response for {} seconds",
                        elapsed.as_secs()
                    )));
                }
            }
        }

        // Проверка 5: Стрим должен быть активен
        if !self.is_streaming {
            return (false, Some("Stream not active".to_string()));
        }

        (true, None)
    }

    /// Пытается переподключиться к Deepgram после разрыва соединения
    /// Делает до 3 попыток с exponential backoff (0.5s, 1s, 2s)
    async fn reconnect(&mut self) -> SttResult<()> {
        log::warn!("Connection lost, attempting to reconnect...");

        // Устанавливаем флаг переподключения
        self.is_reconnecting = true;
        self.reconnect_attempts = 0;

        // Отправляем событие Poor с reason
        if let Some(callback) = &self.on_connection_quality_callback {
            callback("Poor".to_string(), Some("Connection lost, reconnecting...".to_string()));
        }

        // Останавливаем старые задачи
        if let Some(task) = self.keepalive_task.take() {
            task.abort();
            let _ = task.await;
        }
        if let Some(task) = self.receiver_task.take() {
            task.abort();
            let _ = task.await;
        }

        // Сохраняем callbacks для восстановления
        let on_partial = self.on_partial_callback.clone().ok_or_else(|| {
            SttError::Internal("on_partial callback not set during reconnect".to_string())
        })?;
        let on_final = self.on_final_callback.clone().ok_or_else(|| {
            SttError::Internal("on_final callback not set during reconnect".to_string())
        })?;
        let _on_error = self.on_error_callback.clone().ok_or_else(|| {
            SttError::Internal("on_error callback not set during reconnect".to_string())
        })?;
        let on_connection_quality = self.on_connection_quality_callback.clone().ok_or_else(|| {
            SttError::Internal("on_connection_quality callback not set during reconnect".to_string())
        })?;

        let config = self.config.clone().ok_or_else(|| {
            SttError::Internal("config not set during reconnect".to_string())
        })?;

        let api_key = self.api_key.clone().ok_or_else(|| {
            SttError::Configuration("API key not set".to_string())
        })?;

        // Делаем до 3 попыток переподключения
        const MAX_ATTEMPTS: usize = 3;
        let delays_ms = [500u64, 1000, 2000]; // exponential backoff

        for attempt in 1..=MAX_ATTEMPTS {
            self.reconnect_attempts = attempt;
            log::info!("Reconnecting (attempt {}/{})...", attempt, MAX_ATTEMPTS);

            // Отправляем обновление о попытке
            if let Some(callback) = &self.on_connection_quality_callback {
                callback(
                    "Poor".to_string(),
                    Some(format!("Reconnecting (attempt {}/{})...", attempt, MAX_ATTEMPTS))
                );
            }

            // Задержка перед попыткой (кроме первой)
            if attempt > 1 {
                tokio::time::sleep(Duration::from_millis(delays_ms[attempt - 2])).await;
            }

            // Пытаемся создать новое WebSocket соединение
            let url = format!(
                "{}?encoding=linear16&sample_rate=16000&channels=1&language={}&model={}",
                DEEPGRAM_WS_URL,
                config.language,
                config.model.as_deref().unwrap_or("nova-2")
            );

            let request = match Request::builder()
                .method("GET")
                .uri(&url)
                .header("Host", "api.deepgram.com")
                .header("Connection", "Upgrade")
                .header("Upgrade", "websocket")
                .header("Sec-WebSocket-Version", "13")
                .header("Sec-WebSocket-Key", tokio_tungstenite::tungstenite::handshake::client::generate_key())
                .header("Authorization", format!("Token {}", api_key))
                .body(())
            {
                Ok(req) => req,
                Err(e) => {
                    log::warn!("Failed to build request (attempt {}/{}): {}", attempt, MAX_ATTEMPTS, e);
                    continue;
                }
            };

            let ws_stream = match connect_async(request).await {
                Ok((stream, _)) => stream,
                Err(e) => {
                    log::warn!("Failed to connect (attempt {}/{}): {}", attempt, MAX_ATTEMPTS, e);
                    continue;
                }
            };

            log::info!("WebSocket reconnected successfully (attempt {}/{})", attempt, MAX_ATTEMPTS);

            // Разделяем стрим на read/write
            let (write, mut read) = ws_stream.split();
            let ws_write = Arc::new(Mutex::new(write));

            // Пересоздаем Notify для новой сессии
            self.session_ready = Arc::new(Notify::new());

            // Клонируем callbacks для receiver задачи
            let on_partial_for_receiver = on_partial.clone();
            let on_final_for_receiver = on_final.clone();
            let on_connection_quality_for_receiver = on_connection_quality.clone();

            // Запускаем фоновую задачу для приема сообщений
            let session_notify = self.session_ready.clone();
            let last_server_response_for_receiver = self.last_server_response.clone();
            let current_quality_for_receiver = self.current_quality.clone();
            let is_paused_flag_for_receiver = self.is_paused_flag.clone(); // клон для receiver task

            let receiver_task = tokio::spawn(async move {
                log::debug!("Deepgram receiver task started after reconnect");

                // Мониторинг качества связи
                let last_server_response_monitor = last_server_response_for_receiver.clone();
                let current_quality_monitor = current_quality_for_receiver.clone();
                let on_connection_quality_monitor = on_connection_quality_for_receiver.clone();
                let is_paused_flag_for_monitor = is_paused_flag_for_receiver.clone();

                let monitor_task = tokio::spawn(async move {
                    loop {
                        tokio::time::sleep(Duration::from_secs(1)).await;

                        // В режиме паузы не мониторим качество - Deepgram не отправляет сообщения
                        if *is_paused_flag_for_monitor.lock().await {
                            continue;
                        }

                        let last_response = *last_server_response_monitor.lock().await;
                        let mut current_quality = current_quality_monitor.lock().await;

                        if let Some(last_time) = last_response {
                            let elapsed = last_time.elapsed();

                            if elapsed > Duration::from_secs(3) && *current_quality == "Good" {
                                log::warn!("Connection quality degraded after reconnect: no server response for {:.1}s", elapsed.as_secs_f64());
                                *current_quality = "Poor".to_string();
                                on_connection_quality_monitor("Poor".to_string(), Some("No server response for 3+ seconds".to_string()));
                            } else if elapsed <= Duration::from_secs(2) && *current_quality == "Poor" {
                                log::info!("Connection quality recovering after reconnect");
                                *current_quality = "Recovering".to_string();
                                on_connection_quality_monitor("Recovering".to_string(), None);

                                let quality_for_check = current_quality_monitor.clone();
                                let callback_for_check = on_connection_quality_monitor.clone();
                                tokio::spawn(async move {
                                    tokio::time::sleep(Duration::from_secs(2)).await;
                                    let mut q = quality_for_check.lock().await;
                                    if *q == "Recovering" {
                                        log::info!("Connection fully recovered");
                                        *q = "Good".to_string();
                                        callback_for_check("Good".to_string(), None);
                                    }
                                });
                            }
                        }
                    }
                });

                while let Some(msg_result) = read.next().await {
                    // Проверяем флаг паузы и игнорируем сообщения если на паузе
                    if *is_paused_flag_for_receiver.lock().await {
                        log::trace!("Ignoring message from Deepgram after reconnect - stream is paused");
                        continue;
                    }

                    *last_server_response_for_receiver.lock().await = Some(Instant::now());

                    match msg_result {
                        Ok(Message::Text(text)) => {
                            log::debug!("Deepgram received text after reconnect: {}", text);

                            match serde_json::from_str::<Value>(&text) {
                                Ok(json) => {
                                    let msg_type = json["type"].as_str();

                                    if msg_type == Some("Metadata") {
                                        log::info!("Deepgram session ready after reconnect");
                                        session_notify.notify_one();
                                    }

                                    Self::handle_message(json, &on_partial_for_receiver, &on_final_for_receiver);
                                }
                                Err(e) => {
                                    log::error!("Failed to parse Deepgram message after reconnect: {}", e);
                                }
                            }
                        }
                        Ok(Message::Close(frame)) => {
                            log::info!("Deepgram WebSocket closed after reconnect: {:?}", frame);
                            break;
                        }
                        Err(e) => {
                            log::warn!("WebSocket error after reconnect: {}", e);
                            break;
                        }
                        _ => {}
                    }
                }

                monitor_task.abort();
                let _ = monitor_task.await;

                log::debug!("Deepgram receiver task ended after reconnect");
            });

            // Запускаем keepalive задачу
            let ws_write_for_keepalive = ws_write.clone();
            let keepalive_task = tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(5)).await;

                    let keepalive_msg = json!({"type": "KeepAlive"});
                    let mut write_guard = ws_write_for_keepalive.lock().await;

                    if let Err(e) = write_guard.send(Message::Text(keepalive_msg.to_string())).await {
                        log::debug!("Could not send KeepAlive after reconnect (connection closed): {}", e);
                        break;
                    }
                }
            });

            // Сохраняем новое соединение
            self.ws_write = Some(ws_write);
            self.receiver_task = Some(receiver_task);
            self.keepalive_task = Some(keepalive_task);

            // Сбрасываем счетчики ошибок
            self.consecutive_errors = 0;
            self.last_successful_send = Some(Instant::now());
            *self.last_server_response.lock().await = Some(Instant::now());
            *self.current_quality.lock().await = "Recovering".to_string();

            // Переподключение успешно! Отправляем буферизованное аудио
            log::info!("Reconnected successfully after {} attempts", attempt);

            let buffered_chunks: Vec<AudioChunk> = {
                let mut buffer = self.audio_buffer_during_reconnect.lock().await;
                let chunks = buffer.clone();
                buffer.clear();
                chunks
            };

            if !buffered_chunks.is_empty() {
                log::info!("Sending {} buffered audio chunks", buffered_chunks.len());

                for chunk in buffered_chunks {
                    // Отправляем через send_audio но НЕ через рекурсию
                    // Просто отправляем напрямую через WebSocket
                    let bytes: Vec<u8> = chunk.data.iter()
                        .flat_map(|&sample| sample.to_le_bytes())
                        .collect();

                    if let Some(write) = self.ws_write.as_ref() {
                        let mut write_guard = write.lock().await;
                        if let Err(e) = write_guard.send(Message::Binary(bytes)).await {
                            log::warn!("Failed to send buffered chunk: {}", e);
                            // Не критично - продолжаем
                        }
                    }
                }
            }

            // Отправляем событие Recovering
            if let Some(callback) = &self.on_connection_quality_callback {
                callback("Recovering".to_string(), None);
            }

            // Сбрасываем флаг переподключения
            self.is_reconnecting = false;
            self.reconnect_attempts = 0;

            return Ok(());
        }

        // Все попытки провалились
        log::error!("Failed to reconnect after {} attempts", MAX_ATTEMPTS);
        self.is_reconnecting = false;
        self.is_streaming = false;

        Err(SttError::Connection(format!(
            "Failed to reconnect after {} attempts",
            MAX_ATTEMPTS
        )))
    }

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
                                    start, // передаем start время из Deepgram
                                    duration, // передаем duration из Deepgram
                                };

                                // Детальное логирование для отладки
                                log::info!("🔍 DEEPGRAM MSG: is_final={}, speech_final={}, text='{}', confidence={:?}, start={:.2}s, duration={:.2}s",
                                    is_final, speech_final, text, confidence, start, duration);

                                // Отправляем как final только когда ВСЯ речь завершена (speech_final=true)
                                if is_final && speech_final {
                                    log::info!("✅ Final transcript (speech_final=true): '{}' → вызываем on_final callback", text);
                                    on_final(transcription);
                                } else {
                                    // Все остальные (промежуточные и финализированные сегменты) - как partial
                                    // UI различит по флагу is_final
                                    if is_final {
                                        log::info!("🔒 Segment finalized (is_final=true, speech_final=false): '{}' → вызываем on_partial callback", text);
                                    } else {
                                        log::info!("📝 Partial transcript (is_final=false): '{}' → вызываем on_partial callback", text);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::SttProviderType;

    #[test]
    fn test_provider_creation() {
        let provider = DeepgramProvider::new();
        assert!(!provider.is_streaming);
        assert!(!provider.is_paused);
        assert_eq!(provider.audio_buffer.len(), 0);
        assert_eq!(provider.sent_chunks_count, 0);
    }

    #[test]
    fn test_provider_default() {
        let provider = DeepgramProvider::default();
        assert!(!provider.is_streaming);
    }

    #[test]
    fn test_provider_name() {
        let provider = DeepgramProvider::new();
        assert!(provider.name().contains("Deepgram"));
    }

    #[test]
    fn test_provider_is_online() {
        let provider = DeepgramProvider::new();
        assert!(provider.is_online());
    }

    #[test]
    fn test_provider_supports_keep_alive() {
        let provider = DeepgramProvider::new();
        assert!(provider.supports_keep_alive());
    }

    #[test]
    fn test_connection_alive_requires_streaming_and_paused() {
        let mut provider = DeepgramProvider::new();

        // Изначально не живо
        assert!(!provider.is_connection_alive());

        // Только streaming - не живо
        provider.is_streaming = true;
        assert!(!provider.is_connection_alive());

        // Streaming + paused - живо!
        provider.is_paused = true;
        assert!(provider.is_connection_alive());

        // Только paused - не живо
        provider.is_streaming = false;
        assert!(!provider.is_connection_alive());
    }

    #[tokio::test]
    async fn test_initialize_uses_embedded_key() {
        // Тест проверяет что при отсутствии пользовательского ключа
        // используется встроенный ключ из embedded_keys
        let mut provider = DeepgramProvider::new();
        let config = SttConfig::default();

        let result = provider.initialize(&config).await;
        assert!(result.is_ok(), "Should succeed with embedded key");
        assert!(provider.api_key.is_some(), "API key should be set from embedded key");
    }

    #[tokio::test]
    async fn test_initialize_with_api_key() {
        let mut provider = DeepgramProvider::new();

        let mut config = SttConfig::new(SttProviderType::Deepgram);
        config.deepgram_api_key = Some("test-key-123".to_string());

        let result = provider.initialize(&config).await;
        assert!(result.is_ok());
        assert_eq!(provider.api_key, Some("test-key-123".to_string()));
        assert!(provider.config.is_some());
    }

    #[tokio::test]
    async fn test_abort_clears_state() {
        let mut provider = DeepgramProvider::new();

        // Устанавливаем состояние
        provider.is_streaming = true;
        provider.is_paused = true;
        provider.audio_buffer = vec![1, 2, 3];
        provider.sent_chunks_count = 10;
        provider.sent_bytes_total = 1000;

        provider.abort().await.unwrap();

        // Проверяем что всё очистилось
        assert!(!provider.is_streaming);
        assert!(!provider.is_paused);
        assert_eq!(provider.audio_buffer.len(), 0);
        assert_eq!(provider.sent_chunks_count, 0);
        assert_eq!(provider.sent_bytes_total, 0);
    }

    #[tokio::test]
    async fn test_pause_stream_requires_streaming() {
        let mut provider = DeepgramProvider::new();

        // Попытка паузы без streaming
        let result = provider.pause_stream().await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), SttError::Processing(_)));
    }

    #[tokio::test]
    async fn test_pause_stream_when_streaming() {
        let mut provider = DeepgramProvider::new();
        provider.is_streaming = true;
        provider.audio_buffer = vec![1, 2, 3];

        let result = provider.pause_stream().await;
        assert!(result.is_ok());
        assert!(provider.is_paused);
        assert_eq!(provider.audio_buffer.len(), 0); // Буфер очищен
    }

    #[tokio::test]
    async fn test_resume_requires_streaming_and_paused() {
        let mut provider = DeepgramProvider::new();

        let on_partial = Arc::new(|_: Transcription| {});
        let on_final = Arc::new(|_: Transcription| {});
        let on_error = Arc::new(|_: String, _: String| {});
        let on_connection_quality = Arc::new(|_: String, _: Option<String>| {});

        // Не streaming - ошибка
        let result = provider.resume_stream(on_partial.clone(), on_final.clone(), on_error.clone(), on_connection_quality.clone()).await;
        assert!(result.is_err());

        // Streaming но не paused - ошибка
        provider.is_streaming = true;
        let result = provider.resume_stream(on_partial.clone(), on_final.clone(), on_error.clone(), on_connection_quality.clone()).await;
        assert!(result.is_err());

        // Streaming + paused - успех!
        provider.is_paused = true;
        provider.audio_buffer = vec![1, 2, 3];
        let result = provider.resume_stream(on_partial, on_final, on_error, on_connection_quality).await;
        assert!(result.is_ok());
        assert!(!provider.is_paused);
        assert_eq!(provider.audio_buffer.len(), 0); // Буфер очищен
    }

    #[test]
    fn test_handle_message_results() {
        let partial_called = Arc::new(std::sync::Mutex::new(false));
        let final_called = Arc::new(std::sync::Mutex::new(false));

        let p_called = partial_called.clone();
        let on_partial: TranscriptionCallback = Arc::new(move |_: Transcription| {
            *p_called.lock().unwrap() = true;
        });

        let f_called = final_called.clone();
        let on_final: TranscriptionCallback = Arc::new(move |_: Transcription| {
            *f_called.lock().unwrap() = true;
        });

        // Тест с partial result (is_final=false)
        let json = json!({
            "type": "Results",
            "is_final": false,
            "speech_final": false,
            "channel": {
                "alternatives": [
                    {
                        "transcript": "test",
                        "confidence": 0.95
                    }
                ]
            }
        });

        DeepgramProvider::handle_message(json, &on_partial, &on_final);
        assert!(*partial_called.lock().unwrap());
        assert!(!*final_called.lock().unwrap());
    }

    #[test]
    fn test_handle_message_final() {
        let final_called = Arc::new(std::sync::Mutex::new(false));

        let on_partial: TranscriptionCallback = Arc::new(|_: Transcription| {});

        let f_called = final_called.clone();
        let on_final: TranscriptionCallback = Arc::new(move |_: Transcription| {
            *f_called.lock().unwrap() = true;
        });

        // Тест с final result (is_final=true, speech_final=true)
        let json = json!({
            "type": "Results",
            "is_final": true,
            "speech_final": true,
            "channel": {
                "alternatives": [
                    {
                        "transcript": "final text",
                        "confidence": 0.98
                    }
                ]
            }
        });

        DeepgramProvider::handle_message(json, &on_partial, &on_final);
        assert!(*final_called.lock().unwrap());
    }

    #[test]
    fn test_handle_message_empty_text() {
        let called = Arc::new(std::sync::Mutex::new(false));
        let c = called.clone();

        let on_partial: TranscriptionCallback = Arc::new(move |_: Transcription| {
            *c.lock().unwrap() = true;
        });
        let on_final: TranscriptionCallback = Arc::new(|_: Transcription| {});

        // Пустой текст не должен вызывать callbacks
        let json = json!({
            "type": "Results",
            "is_final": false,
            "channel": {
                "alternatives": [
                    {"transcript": ""}
                ]
            }
        });

        DeepgramProvider::handle_message(json, &on_partial, &on_final);
        assert!(!*called.lock().unwrap());
    }

    #[test]
    fn test_handle_message_metadata() {
        let on_partial: TranscriptionCallback = Arc::new(|_: Transcription| {});
        let on_final: TranscriptionCallback = Arc::new(|_: Transcription| {});

        // Metadata сообщение не должно паниковать
        let json = json!({
            "type": "Metadata",
            "request_id": "test-123"
        });

        DeepgramProvider::handle_message(json, &on_partial, &on_final);
        // Просто проверяем что не упали
    }
}
