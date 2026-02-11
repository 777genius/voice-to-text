//! Backend STT Provider
//!
//! Подключается к нашему API (api.voicetext.site) вместо прямого подключения к Deepgram.
//! Все транскрипции идут через наш бэкенд с лицензией и usage tracking.

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use http::Request;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::Duration;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};
use tokio::net::TcpStream;

use crate::domain::{
    AudioChunk, ConnectionQualityCallback, ErrorCallback, SttConfig, SttConnectionCategory,
    SttConnectionDetails, SttConnectionError, SttError, SttProvider, SttResult, Transcription,
    TranscriptionCallback,
};

use super::backend_messages::{ClientMessage, ServerMessage};

/// URL бэкенда для production
const PROD_BACKEND_URL: &str = "wss://api.voicetext.site";

/// URL бэкенда для development (localhost)
const DEV_BACKEND_URL: &str = "ws://localhost:8080";

// Таймауты: критичны для стабильности при плохом интернете.
// Без них connect/send могут "подвиснуть" и UI будет бесконечно ждать.
const WS_CONNECT_TIMEOUT_SECS: u64 = 8;
const WS_SEND_TIMEOUT_SECS: u64 = 3;

/// Проверяем, что URL указывает на локальный бэкенд (localhost/loopback).
///
/// Нужен для dev-режима: если у пользователя сохранён "боевой" токен, но он запускает
/// локальный бэкенд, тот токен почти наверняка невалиден для local БД/pepper → получаем 401.
fn is_local_backend_url(url: &str) -> bool {
    // Пытаемся распарсить как URI (надёжнее, чем substring).
    if let Ok(uri) = url.parse::<http::Uri>() {
        if let Some(host) = uri.host() {
            return matches!(host, "localhost" | "127.0.0.1" | "::1");
        }
    }

    // Фоллбек на случай нестандартного формата.
    url.contains("localhost") || url.contains("127.0.0.1") || url.contains("[::1]")
}

/// Получить URL бэкенда с учётом окружения
/// Приоритет: env VOICE_TO_TEXT_BACKEND_URL > auto-detect (debug/release)
fn get_default_backend_url() -> String {
    // 1. Проверяем env переменную (для staging, тестов и т.д.)
    if let Ok(url) = std::env::var("VOICE_TO_TEXT_BACKEND_URL") {
        if !url.is_empty() {
            log::info!("Using backend URL from env: {}", url);
            return url;
        }
    }

    // 2. Auto-detect по типу сборки
    if cfg!(debug_assertions) {
        log::info!("Debug build: using dev backend {}", DEV_BACKEND_URL);
        DEV_BACKEND_URL.to_string()
    } else {
        log::info!("Release build: using prod backend {}", PROD_BACKEND_URL);
        PROD_BACKEND_URL.to_string()
    }
}

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Callback для обновления usage (seconds_used, seconds_remaining_total_or_plan)
pub type UsageUpdateCallback = Arc<dyn Fn(f32, f32) + Send + Sync>;

/// Backend STT provider — подключается к нашему API вместо прямого Deepgram
pub struct BackendProvider {
    config: Option<SttConfig>,
    is_streaming: bool,
    is_paused: bool,
    auth_token: Option<String>,
    backend_url: String,
    session_id: Option<String>,
    ws_write: Option<Arc<Mutex<futures_util::stream::SplitSink<WsStream, Message>>>>,
    receiver_task: Option<JoinHandle<()>>,
    keepalive_task: Option<JoinHandle<()>>,

    /// Флаг закрытия соединения (атомарный для thread-safety)
    /// Используется для предотвращения race condition при закрытии WebSocket
    is_closed: Arc<AtomicBool>,

    /// Последний известный остаток секунд (из UsageUpdate), хранится как f32 bits.
    /// Доступен и из receiver task, и из send_audio() — нужен чтобы при закрытии
    /// отличать limit_exceeded от обычного обрыва.
    last_remaining_secs: Arc<AtomicU32>,

    // Callbacks: active/pending (для keep-alive режима).
    //
    // Важно: receiver task живёт дольше одной "записи" (мы держим WS живым между старт/стопами).
    // Поэтому нельзя захватывать callbacks в spawn при start_stream — иначе при resume_stream
    // они не обновятся и события будут уходить в старую "сессию" UI.
    callbacks: Arc<Mutex<CallbackState>>,
    on_usage_update_callback: Option<UsageUpdateCallback>,

    // Статистика
    sent_chunks_count: usize,
    sent_bytes_total: usize,

    audio_batch: Vec<u8>,
    audio_batch_frames: usize,

    next_send_at: Option<std::time::Instant>,
    batch_started_at: Option<std::time::Instant>,
}

#[derive(Clone)]
struct CallbackSet {
    on_partial: TranscriptionCallback,
    on_final: TranscriptionCallback,
    on_error: ErrorCallback,
    on_connection_quality: ConnectionQualityCallback,
}

#[derive(Default)]
struct CallbackState {
    active: Option<CallbackSet>,
    pending: Option<CallbackSet>,
    // При keep-alive: новые callbacks активируем только после первого ACK,
    // чтобы "поздние" сообщения от предыдущей записи не попадали в новую UI-сессию.
    swap_on_next_ack: bool,
    // Защита от "поздних" ACK старой записи:
    // активируем pending только когда получили ACK с seq БОЛЬШЕ последнего отправленного seq на момент resume_stream.
    swap_after_seq: u64,
}

impl BackendProvider {
    pub fn new() -> Self {
        Self {
            config: None,
            is_streaming: false,
            is_paused: false,
            auth_token: None,
            backend_url: get_default_backend_url(),
            session_id: None,
            ws_write: None,
            receiver_task: None,
            keepalive_task: None,
            is_closed: Arc::new(AtomicBool::new(true)), // Изначально закрыто
            last_remaining_secs: Arc::new(AtomicU32::new(f32::MAX.to_bits())),
            callbacks: Arc::new(Mutex::new(CallbackState::default())),
            on_usage_update_callback: None,
            sent_chunks_count: 0,
            sent_bytes_total: 0,
            audio_batch: Vec::new(),
            audio_batch_frames: 0,
            next_send_at: None,
            batch_started_at: None,
        }
    }

    /// Установить callback для UsageUpdate сообщений
    pub fn set_usage_callback(&mut self, callback: UsageUpdateCallback) {
        self.on_usage_update_callback = Some(callback);
    }

    /// Отправить JSON сообщение через WebSocket
    async fn send_json(&self, msg: &ClientMessage) -> SttResult<()> {
        // Не пытаемся отправить если соединение уже закрыто
        if self.is_closed.load(Ordering::SeqCst) {
            return Ok(()); // Игнорируем — соединение уже закрыто
        }

        if let Some(ref ws_write) = self.ws_write {
            let json = serde_json::to_string(msg)
                .map_err(|e| SttError::Processing(format!("JSON serialize error: {}", e)))?;

            let send_fut = async {
                let mut guard = ws_write.lock().await;
                guard.send(Message::Text(json)).await
            };

            match tokio::time::timeout(Duration::from_secs(WS_SEND_TIMEOUT_SECS), send_fut).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    // Если не можем отправлять — считаем соединение "поломанным", чтобы send_audio быстро фейлился.
                    self.is_closed.store(true, Ordering::SeqCst);
                    return Err(SttError::Connection(SttConnectionError {
                        message: format!("WS send error: {}", e),
                        details: SttConnectionDetails::default(),
                    }));
                }
                Err(_) => {
                    self.is_closed.store(true, Ordering::SeqCst);
                    return Err(SttError::Connection(SttConnectionError {
                        message: "WS send timeout".to_string(),
                        details: SttConnectionDetails {
                            category: Some(SttConnectionCategory::Timeout),
                            ..Default::default()
                        },
                    }));
                }
            }

            Ok(())
        } else {
            Err(SttError::Processing("WebSocket not connected".to_string()))
        }
    }
}

impl Default for BackendProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SttProvider for BackendProvider {
    async fn initialize(&mut self, config: &SttConfig) -> SttResult<()> {
        log::info!("BackendProvider: Initializing");

        // Получаем URL бэкенда (из конфига или авто-детект по окружению)
        let backend_url = config
            .backend_url
            .clone()
            .unwrap_or_else(get_default_backend_url);

        log::info!("BackendProvider: Using backend URL: {}", backend_url);

        // Получаем auth token из конфига
        //
        // В dev режиме для локального бэкенда (localhost) всегда используем dev-local-token.
        // Это защищает от ситуации "я уже логинился в прод, а сейчас запускаю local" → 401.
        log::info!(
            "BackendProvider: config.backend_auth_token present: {}, len: {}",
            config.backend_auth_token.is_some(),
            config.backend_auth_token.as_ref().map(|t| t.len()).unwrap_or(0)
        );

        let auth_token = if cfg!(debug_assertions) {
            if is_local_backend_url(&backend_url) {
                if config.backend_auth_token.as_deref() != Some("dev-local-token") {
                    log::info!(
                        "DEV MODE: Local backend detected ({}). Using dev-local-token instead of saved token",
                        backend_url
                    );
                } else {
                    log::info!(
                        "DEV MODE: Local backend detected ({}). Using dev-local-token",
                        backend_url
                    );
                }
                "dev-local-token".to_string()
            } else {
                config.backend_auth_token.clone().unwrap_or_else(|| {
                    log::info!("DEV MODE: Using dev-local-token (no real token configured)");
                    "dev-local-token".to_string()
                })
            }
        } else {
            config.backend_auth_token.clone().ok_or_else(|| {
                SttError::Configuration(
                    "Backend auth token is required. Please activate your license.".to_string(),
                )
            })?
        };

        log::info!("BackendProvider: auth_token len: {}", auth_token.len());

        self.auth_token = Some(auth_token);
        self.backend_url = backend_url;
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
        log::info!("BackendProvider: Starting stream");

        if self.is_streaming {
            return Err(SttError::Processing("Stream already active".to_string()));
        }

        let auth_token = self
            .auth_token
            .as_ref()
            .ok_or_else(|| SttError::Configuration("Auth token not set".to_string()))?
            .clone();

        let config = self
            .config
            .as_ref()
            .ok_or_else(|| SttError::Configuration("Config not set".to_string()))?
            .clone();

        // WebSocket URL
        let ws_url = format!("{}/api/v1/transcribe/stream", self.backend_url);

        log::debug!("Connecting to backend: {}", ws_url);

        // Формируем WebSocket запрос с Authorization header
        let request = Request::builder()
            .method("GET")
            .uri(&ws_url)
            .header("Host", self.backend_url.replace("wss://", "").replace("ws://", ""))
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header(
                "Sec-WebSocket-Key",
                tokio_tungstenite::tungstenite::handshake::client::generate_key(),
            )
            .header("Authorization", format!("Bearer {}", auth_token))
            .body(())
            .map_err(|e| {
                SttError::Connection(SttConnectionError::simple(format!(
                    "Failed to build WS request: {}",
                    e
                )))
            })?;

        let (ws_stream, _response) = tokio::time::timeout(
            Duration::from_secs(WS_CONNECT_TIMEOUT_SECS),
            connect_async(request),
        )
        .await
        .map_err(|_| {
            SttError::Connection(SttConnectionError {
                message: "WS connection timeout".to_string(),
                details: SttConnectionDetails {
                    category: Some(SttConnectionCategory::Timeout),
                    ..Default::default()
                },
            })
        })?
        .map_err(|e| match e {
            tokio_tungstenite::tungstenite::Error::Http(resp) => {
                let status = resp.status();

                if status == http::StatusCode::UNAUTHORIZED {
                    // В dev режиме это почти всегда означает, что local backend не принял dev токен
                    // (например, не выставлен SECURITY_ALLOW_DEV_TOKEN=true).
                    if cfg!(debug_assertions) && is_local_backend_url(&self.backend_url) {
                        return SttError::Authentication(
                            "401 Unauthorized от локального бэкенда. Проверь, что backend запущен с SECURITY_ALLOW_DEV_TOKEN=true (и APP_ENV=local). Если хочешь использовать свой сохранённый токен — укажи VOICE_TO_TEXT_BACKEND_URL=wss://api.voicetext.site"
                                .to_string(),
                        );
                    }

                    return SttError::Authentication(
                        "401 Unauthorized. Токен недействителен/истёк — попробуй перелогиниться."
                            .to_string(),
                    );
                }

                if status == http::StatusCode::TOO_MANY_REQUESTS {
                    // Парсим body от сервера для точной причины (rate_limit vs too_many_sessions).
                    //
                    // Важно: backend API ошибки имеют форму:
                    // { success:false, error:{ code, message, details? } }
                    // Но некоторые WS/proxy могут вернуть { code, message } без envelope.
                    let mut server_message: Option<String> = None;
                    let mut server_code: Option<String> = None;
                    let mut retry_after_secs: Option<u64> = None;

                    if let Some(body) = resp.body().as_ref() {
                        if let Ok(text) = std::str::from_utf8(body) {
                            if let Ok(json) = serde_json::from_str::<serde_json::Value>(text) {
                                // API envelope: { error: { code, message, details } }
                                if let Some(err) = json.get("error") {
                                    server_message = err
                                        .get("message")
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string());
                                    server_code = err
                                        .get("code")
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string());
                                    retry_after_secs = err
                                        .get("details")
                                        .and_then(|d| d.get("retry_after_seconds"))
                                        .and_then(|v| v.as_u64());
                                } else {
                                    // Fallback: { code, message }
                                    server_message = json
                                        .get("message")
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string());
                                    server_code = json
                                        .get("code")
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string());
                                }
                            }
                        }
                    }

                    // Для WS-handshake ошибок tungstenite часто не отдаёт body, поэтому
                    // backend дублирует код в заголовке.
                    if server_code.is_none() {
                        server_code = resp
                            .headers()
                            .get("x-voicetext-error-code")
                            .and_then(|v| v.to_str().ok())
                            .map(|s| s.to_string());
                    }

                    // Иногда retry-after приходит только хедером (например, глобальный rate limit middleware).
                    if retry_after_secs.is_none() {
                        retry_after_secs = resp
                            .headers()
                            .get("Retry-After")
                            .and_then(|v| v.to_str().ok())
                            .and_then(|s| s.parse::<u64>().ok());
                    }

                    let display_message = match (&server_message, &server_code, retry_after_secs) {
                        (Some(msg), Some(code), Some(secs)) => {
                            format!("WS connection failed: 429 ({}): {} (retry after {}s)", code, msg, secs)
                        }
                        (Some(msg), Some(code), None) => {
                            format!("WS connection failed: 429 ({}): {}", code, msg)
                        }
                        (Some(msg), None, Some(secs)) => {
                            format!("WS connection failed: 429 — {} (retry after {}s)", msg, secs)
                        }
                        (Some(msg), None, None) => format!("WS connection failed: 429 — {}", msg),
                        (None, Some(code), Some(secs)) => {
                            format!("WS connection failed: 429 ({}) (retry after {}s)", code, secs)
                        }
                        (None, Some(code), None) => format!("WS connection failed: 429 ({})", code),
                        (None, None, Some(secs)) => {
                            format!("WS connection failed: HTTP error: {} (retry after {}s)", status, secs)
                        }
                        (None, None, None) => format!("WS connection failed: HTTP error: {}", status),
                    };

                    let category = match server_code.as_deref() {
                        // Важно: backend использует HTTP 429 и для limit_exceeded и для rate limiting,
                        // поэтому определяем категорию по коду.
                        Some("LIMIT_EXCEEDED") => SttConnectionCategory::LimitExceeded,
                        Some("TOO_MANY_SESSIONS") | Some("RATE_LIMIT_EXCEEDED") => {
                            SttConnectionCategory::RateLimited
                        }
                        _ => SttConnectionCategory::RateLimited,
                    };

                    return SttError::Connection(SttConnectionError {
                        message: display_message,
                        details: SttConnectionDetails {
                            category: Some(category),
                            http_status: Some(429),
                            server_code,
                            ..Default::default()
                        },
                    });
                }

                {
                    let status_u16 = status.as_u16();
                    let category = if matches!(status_u16, 502 | 503 | 504) {
                        SttConnectionCategory::ServerUnavailable
                    } else {
                        SttConnectionCategory::Http
                    };
                    SttError::Connection(SttConnectionError {
                        message: format!("WS connection failed: HTTP error: {}", status),
                        details: SttConnectionDetails {
                            category: Some(category),
                            http_status: Some(status_u16),
                            ..Default::default()
                        },
                    })
                }
            }
            tokio_tungstenite::tungstenite::Error::Tls(other) => SttError::Connection(SttConnectionError {
                message: format!("WS connection failed: {}", other),
                details: SttConnectionDetails {
                    category: Some(SttConnectionCategory::Tls),
                    ..Default::default()
                },
            }),
            tokio_tungstenite::tungstenite::Error::Io(ioe) => {
                let kind = ioe.kind();
                let kind_str = format!("{:?}", kind);
                let os_error = ioe.raw_os_error();
                let category = match kind {
                    std::io::ErrorKind::ConnectionRefused => SttConnectionCategory::Refused,
                    std::io::ErrorKind::ConnectionReset => SttConnectionCategory::Reset,
                    std::io::ErrorKind::NotConnected
                    | std::io::ErrorKind::NetworkUnreachable
                    | std::io::ErrorKind::HostUnreachable
                    | std::io::ErrorKind::AddrNotAvailable => SttConnectionCategory::Offline,
                    std::io::ErrorKind::TimedOut => SttConnectionCategory::Timeout,
                    _ => SttConnectionCategory::Unknown,
                };
                SttError::Connection(SttConnectionError {
                    message: format!("WS connection failed: {}", ioe),
                    details: SttConnectionDetails {
                        category: Some(category),
                        io_error_kind: Some(kind_str),
                        os_error,
                        ..Default::default()
                    },
                })
            }
            other => SttError::Connection(SttConnectionError {
                message: format!("WS connection failed: {}", other),
                details: SttConnectionDetails::default(),
            }),
        })?;

        log::info!("Backend WebSocket connected");

        // Сбрасываем флаг закрытия — соединение установлено
        self.is_closed.store(false, Ordering::SeqCst);

        let (write, mut read) = ws_stream.split();
        let ws_write = Arc::new(Mutex::new(write));
        self.ws_write = Some(ws_write.clone());

        // Сохраняем callbacks как "active" (для receiver task).
        {
            let mut state = self.callbacks.lock().await;
            state.active = Some(CallbackSet {
                on_partial: on_partial.clone(),
                on_final: on_final.clone(),
                on_error: on_error.clone(),
                on_connection_quality: on_connection_quality.clone(),
            });
            state.pending = None;
            state.swap_on_next_ack = false;
            state.swap_after_seq = 0;
        }

        // Отправляем Config message
        let provider_name = match config.provider {
            crate::domain::SttProviderType::Deepgram => "deepgram",
            crate::domain::SttProviderType::AssemblyAI => "assemblyai",
            _ => "deepgram", // fallback
        };

        let config_msg = ClientMessage::Config {
            protocol_v: 1,
            provider: provider_name.to_string(),
            language: config.language.clone(),
            sample_rate: 16000,
            channels: 1,
            encoding: "pcm_s16le".to_string(),
        };

        self.send_json(&config_msg).await?;
        log::debug!("Config message sent");

        // Запускаем receiver task для обработки сообщений от сервера.
        // Берём callbacks из self.callbacks, чтобы они могли обновляться при resume_stream.
        let callbacks_state = self.callbacks.clone();
        let on_usage_cb = self.on_usage_update_callback.clone();
        let is_closed_flag = self.is_closed.clone();
        let shared_remaining = self.last_remaining_secs.clone();

        // Сбрасываем remaining на старте нового соединения
        shared_remaining.store(f32::MAX.to_bits(), Ordering::SeqCst);

        let receiver_task = tokio::spawn(async move {
            log::debug!("Backend receiver task started");

            const LIMIT_REMAINING_THRESHOLD: f32 = 5.0;

            while let Some(msg_result) = read.next().await {
                match msg_result {
                    Ok(Message::Text(text)) => {
                        match serde_json::from_str::<ServerMessage>(&text) {
                            Ok(server_msg) => {
                                match server_msg {
                                    ServerMessage::Ready { session_id } => {
                                        log::info!("Session ready: {}", session_id);
                                        // Уведомляем о хорошем качестве связи
                                        let cb = {
                                            let state = callbacks_state.lock().await;
                                            state
                                                .active
                                                .as_ref()
                                                .map(|c| c.on_connection_quality.clone())
                                        };
                                        if let Some(cb) = cb {
                                            cb("Good".to_string(), None);
                                        }
                                    }

                                    ServerMessage::Ack { seq } => {
                                        log::trace!("Ack received: seq={}", seq);
                                        // Если есть pending callbacks (новая UI-сессия) — активируем их на первом ACK.
                                        // Это даёт чёткую границу между "старыми" и "новыми" результатами.
                                        let swapped = {
                                            let mut state = callbacks_state.lock().await;
                                            if state.swap_on_next_ack && seq > state.swap_after_seq {
                                                state.swap_on_next_ack = false;
                                                state.swap_after_seq = 0;
                                                if state.pending.is_some() {
                                                    state.active = state.pending.take();
                                                }
                                                true
                                            } else {
                                                false
                                            }
                                        };
                                        if swapped {
                                            log::debug!("Callbacks switched after first ACK (new recording session)");
                                        }
                                    }

                                    ServerMessage::Partial { text, confidence } => {
                                        log::debug!("Partial: {} (conf: {:?})", text, confidence);
                                        let mut transcription = Transcription::partial(text);
                                        if let Some(conf) = confidence {
                                            transcription = transcription.with_confidence(conf);
                                        }
                                        let cb = {
                                            let state = callbacks_state.lock().await;
                                            state.active.as_ref().map(|c| c.on_partial.clone())
                                        };
                                        if let Some(cb) = cb {
                                            cb(transcription);
                                        }
                                    }

                                    ServerMessage::Final {
                                        text,
                                        confidence,
                                        duration_ms,
                                    } => {
                                        log::debug!(
                                            "Final: {} (conf: {:?}, dur: {}ms)",
                                            text,
                                            confidence,
                                            duration_ms
                                        );
                                        let mut transcription = Transcription::final_result(text)
                                            .with_timing(0.0, duration_ms as f64 / 1000.0);
                                        if let Some(conf) = confidence {
                                            transcription = transcription.with_confidence(conf);
                                        }
                                        let cb = {
                                            let state = callbacks_state.lock().await;
                                            state.active.as_ref().map(|c| c.on_final.clone())
                                        };
                                        if let Some(cb) = cb {
                                            cb(transcription);
                                        }
                                    }

                                    ServerMessage::UsageUpdate {
                                        seconds_used,
                                        seconds_remaining_plan,
                                        seconds_remaining_total,
                                        ..
                                    } => {
                                        let remaining = seconds_remaining_total
                                            .unwrap_or(seconds_remaining_plan);
                                        shared_remaining.store(remaining.to_bits(), Ordering::SeqCst);
                                        log::debug!(
                                            "Usage: used={:.1}s, remaining={:.1}s",
                                            seconds_used,
                                            remaining
                                        );
                                        if let Some(ref cb) = on_usage_cb {
                                            cb(seconds_used, remaining);
                                        }
                                    }

                                    ServerMessage::Resumed {
                                        session_id,
                                        last_seq_acked,
                                    } => {
                                        log::info!(
                                            "Session resumed: {}, last_seq: {}",
                                            session_id,
                                            last_seq_acked
                                        );
                                        let cb = {
                                            let state = callbacks_state.lock().await;
                                            state
                                                .active
                                                .as_ref()
                                                .map(|c| c.on_connection_quality.clone())
                                        };
                                        if let Some(cb) = cb {
                                            cb("Good".to_string(), None);
                                        }
                                    }

                                    ServerMessage::Error { code, message } => {
                                        log::error!("Server error: {} - {}", code, message);
                                        let cb = {
                                            let state = callbacks_state.lock().await;
                                            state.active.as_ref().map(|c| c.on_error.clone())
                                        };
                                        if let Some(cb) = cb {
                                            let category = match code.as_str() {
                                                "timeout" => Some(SttConnectionCategory::Timeout),
                                                "rate_limit" | "too_many_sessions" => Some(SttConnectionCategory::RateLimited),
                                                "LIMIT_EXCEEDED" => Some(SttConnectionCategory::LimitExceeded),
                                                _ => Some(SttConnectionCategory::Unknown),
                                            };
                                            cb(SttError::Connection(SttConnectionError {
                                                message,
                                                details: SttConnectionDetails {
                                                    category,
                                                    server_code: Some(code),
                                                    ..Default::default()
                                                },
                                            }));
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                log::warn!("Failed to parse server message: {} - {}", e, text);
                            }
                        }
                    }

                    Ok(Message::Close(frame)) => {
                        log::info!("WebSocket closed by server: {:?}", frame);
                        // Если мы сами инициировали закрытие (stop_stream) — не эмитим ошибку в UI.
                        if is_closed_flag.load(Ordering::SeqCst) {
                            break;
                        }
                        is_closed_flag.store(true, Ordering::SeqCst);
                        let cb = {
                            let state = callbacks_state.lock().await;
                            state.active.as_ref().map(|c| c.on_error.clone())
                        };
                        if let Some(cb) = cb {
                            let code_u16 = frame.as_ref().map(|f| u16::from(f.code));
                            let mut category = match code_u16 {
                                Some(1008) => SttConnectionCategory::LimitExceeded,
                                Some(1012) | Some(1013) | Some(1014) => SttConnectionCategory::ServerUnavailable,
                                Some(1000) => SttConnectionCategory::Closed,
                                _ => SttConnectionCategory::ServerUnavailable,
                            };

                            // Fallback: сервер может закрыть WS без кода 1008 (race condition между
                            // отправкой LIMIT_EXCEEDED и close frame). Если последний UsageUpdate
                            // показывал почти нулевой остаток — это лимит, а не обрыв связи.
                            let remaining = f32::from_bits(shared_remaining.load(Ordering::SeqCst));
                            if category != SttConnectionCategory::LimitExceeded
                                && remaining < LIMIT_REMAINING_THRESHOLD
                            {
                                log::warn!(
                                    "Close frame without 1008, but last remaining={:.1}s < {:.0}s → treating as limit_exceeded",
                                    remaining,
                                    LIMIT_REMAINING_THRESHOLD
                                );
                                category = SttConnectionCategory::LimitExceeded;
                            }

                            cb(SttError::Connection(SttConnectionError {
                                message: "WebSocket closed by server".to_string(),
                                details: SttConnectionDetails {
                                    category: Some(category),
                                    ws_close_code: code_u16,
                                    ..Default::default()
                                },
                            }));
                        }
                        break;
                    }

                    Ok(Message::Ping(data)) => {
                        log::trace!("Ping received");
                        // Pong отправляется автоматически tokio-tungstenite
                        let _ = data;
                    }

                    Ok(_) => {
                        // Binary или другие сообщения — игнорируем
                    }

                    Err(e) => {
                        log::error!("WebSocket error: {}", e);
                        // Если закрытие инициировано нами — не поднимаем "ошибку соединения" в UI.
                        if is_closed_flag.load(Ordering::SeqCst) {
                            break;
                        }
                        is_closed_flag.store(true, Ordering::SeqCst);
                        let cb = {
                            let state = callbacks_state.lock().await;
                            state.active.as_ref().map(|c| c.on_error.clone())
                        };
                        if let Some(cb) = cb {
                            let mut details = match &e {
                                tokio_tungstenite::tungstenite::Error::Io(ioe) => {
                                    let kind = ioe.kind();
                                    let kind_str = format!("{:?}", kind);
                                    let os_error = ioe.raw_os_error();
                                    let category = match kind {
                                        std::io::ErrorKind::ConnectionRefused => SttConnectionCategory::Refused,
                                        std::io::ErrorKind::ConnectionReset => SttConnectionCategory::Reset,
                                        std::io::ErrorKind::BrokenPipe => SttConnectionCategory::ServerUnavailable,
                                        std::io::ErrorKind::NotConnected
                                        | std::io::ErrorKind::NetworkUnreachable
                                        | std::io::ErrorKind::HostUnreachable
                                        | std::io::ErrorKind::AddrNotAvailable => SttConnectionCategory::Offline,
                                        std::io::ErrorKind::TimedOut => SttConnectionCategory::Timeout,
                                        _ => SttConnectionCategory::Unknown,
                                    };
                                    SttConnectionDetails {
                                        category: Some(category),
                                        io_error_kind: Some(kind_str),
                                        os_error,
                                        ..Default::default()
                                    }
                                }
                                tokio_tungstenite::tungstenite::Error::Tls(_) => SttConnectionDetails {
                                    category: Some(SttConnectionCategory::Tls),
                                    ..Default::default()
                                },
                                tokio_tungstenite::tungstenite::Error::ConnectionClosed
                                | tokio_tungstenite::tungstenite::Error::AlreadyClosed => SttConnectionDetails {
                                    category: Some(SttConnectionCategory::Closed),
                                    ..Default::default()
                                },
                                _ => SttConnectionDetails {
                                    category: Some(SttConnectionCategory::Unknown),
                                    ..Default::default()
                                },
                            };

                            // Fallback: обрыв соединения (reset/closed) при почти нулевом остатке
                            // — скорее всего сервер закрыл из-за лимита без нормального close frame.
                            let remaining = f32::from_bits(shared_remaining.load(Ordering::SeqCst));
                            if details.category != Some(SttConnectionCategory::LimitExceeded)
                                && remaining < LIMIT_REMAINING_THRESHOLD
                            {
                                log::warn!(
                                    "WS error with last remaining={:.1}s < {:.0}s → treating as limit_exceeded",
                                    remaining,
                                    LIMIT_REMAINING_THRESHOLD
                                );
                                details.category = Some(SttConnectionCategory::LimitExceeded);
                            }

                            cb(SttError::Connection(SttConnectionError {
                                message: e.to_string(),
                                details,
                            }));
                        }
                        break;
                    }
                }
            }

            // На выходе из loop всегда помечаем соединение закрытым
            is_closed_flag.store(true, Ordering::SeqCst);
            log::info!("Backend receiver task finished");
        });

        self.receiver_task = Some(receiver_task);

        // KeepAlive task (best-effort): поддерживает соединение живым, когда пользователь
        // быстро старт/стопит запись или просто прячет окно на пару секунд.
        //
        // Важно: само наличие открытого WS-соединения может держать ресурсы провайдера (Deepgram) на сервере.
        // Поэтому держим TTL коротким и всегда закрываем соединение по таймеру в TranscriptionService.
        let ws_write_for_keepalive = ws_write.clone();
        let is_closed_for_keepalive = self.is_closed.clone();
        let keepalive_task = tokio::spawn(async move {
            log::debug!("Backend keepalive task started");
            loop {
                tokio::time::sleep(Duration::from_secs(20)).await;
                if is_closed_for_keepalive.load(Ordering::SeqCst) {
                    break;
                }
                let ping_fut = async {
                    let mut guard = ws_write_for_keepalive.lock().await;
                    guard.send(Message::Ping(Vec::new())).await
                };

                if tokio::time::timeout(Duration::from_secs(WS_SEND_TIMEOUT_SECS), ping_fut)
                    .await
                    .ok()
                    .and_then(|r| r.ok())
                    .is_none()
                {
                    // Пинг не смогли отправить → считаем соединение закрытым/битым.
                    is_closed_for_keepalive.store(true, Ordering::SeqCst);
                    break;
                }
            }
            log::debug!("Backend keepalive task ended");
        });
        self.keepalive_task = Some(keepalive_task);

        self.is_streaming = true;
        self.is_paused = false;
        self.sent_chunks_count = 0;
        self.sent_bytes_total = 0;

        log::info!("BackendProvider: Stream started");
        Ok(())
    }

    async fn send_audio(&mut self, chunk: &AudioChunk) -> SttResult<()> {
        // Быстрая проверка атомарного флага (без async lock)
        if self.is_closed.load(Ordering::SeqCst) {
            // Если соединение закрыто И остаток был < порога — это лимит, а не обрыв.
            // Без этого audio processor loop будет 10 раз ретраить "connection" ошибку,
            // перезатирая корректный limit_exceeded с receiver task.
            let remaining = f32::from_bits(self.last_remaining_secs.load(Ordering::SeqCst));
            let category = if remaining < 5.0 {
                SttConnectionCategory::LimitExceeded
            } else {
                SttConnectionCategory::Closed
            };
            return Err(SttError::Connection(SttConnectionError::with_category(
                "Connection closed".to_string(),
                category,
            )));
        }

        if !self.is_streaming {
            return Err(SttError::Processing("Stream not active".to_string()));
        }

        if let Some(ref ws_write) = self.ws_write {
            const SAMPLE_RATE_HZ: usize = 16_000;
            const FRAME_MS: usize = 30;
            const SAMPLES_PER_FRAME: usize = SAMPLE_RATE_HZ * FRAME_MS / 1000; // 480
            const BYTES_PER_SAMPLE: usize = 2;
            const FRAME_BYTES: usize = SAMPLES_PER_FRAME * BYTES_PER_SAMPLE; // 960

            const MIN_FRAMES_PER_MESSAGE: usize = 1; // ~30ms
            const MAX_FRAMES_PER_MESSAGE: usize = 10; // ~300ms, чтобы догонять беклог без роста msg/sec
            const MAX_BATCH_WAIT_MS: u64 = 30; // верхняя граница задержки перед отправкой
            const MIN_SEND_INTERVAL_MS: u64 = 25; // 40 msg/s верхняя граница на клиенте

            self.audio_batch.reserve(chunk.data.len() * 2);
            let now = std::time::Instant::now();
            if self.audio_batch_frames == 0 {
                self.batch_started_at = Some(now);
            }
            for &sample in &chunk.data {
                self.audio_batch.extend_from_slice(&sample.to_le_bytes());
            }
            self.audio_batch_frames += 1;

            let batch_age_ms = self
                .batch_started_at
                .map(|t| now.saturating_duration_since(t).as_millis() as u64)
                .unwrap_or(0);
            let ready_to_send = self.audio_batch_frames >= MIN_FRAMES_PER_MESSAGE || batch_age_ms >= MAX_BATCH_WAIT_MS;
            if !ready_to_send {
                return Ok(());
            }

            let frames_to_send = self.audio_batch_frames.min(MAX_FRAMES_PER_MESSAGE);
            let bytes_to_send = frames_to_send * FRAME_BYTES;
            if self.audio_batch.len() < bytes_to_send {
                return Ok(());
            }

            let remainder = self.audio_batch.split_off(bytes_to_send);
            let bytes = std::mem::replace(&mut self.audio_batch, remainder);
            self.audio_batch_frames -= frames_to_send;

            let now2 = std::time::Instant::now();
            let next_at = self.next_send_at.unwrap_or(now2);
            if next_at > now2 {
                tokio::time::sleep_until(tokio::time::Instant::from_std(next_at)).await;
            }
            self.next_send_at = Some(std::time::Instant::now() + std::time::Duration::from_millis(MIN_SEND_INTERVAL_MS));

            self.sent_chunks_count += 1;
            self.sent_bytes_total += bytes.len();

            if self.sent_chunks_count % 50 == 0 {
                log::debug!(
                    "Backend: sent {} chunks, {} bytes total",
                    self.sent_chunks_count,
                    self.sent_bytes_total
                );
            }

            let send_fut = async {
                let mut guard = ws_write.lock().await;
                guard.send(Message::Binary(bytes)).await
            };

            match tokio::time::timeout(Duration::from_secs(WS_SEND_TIMEOUT_SECS), send_fut).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    self.is_closed.store(true, Ordering::SeqCst);
                    return Err(SttError::Connection(SttConnectionError::simple(format!(
                        "Failed to send audio: {}",
                        e
                    ))));
                }
                Err(_) => {
                    self.is_closed.store(true, Ordering::SeqCst);
                    return Err(SttError::Connection(SttConnectionError::with_category(
                        "WS send timeout".to_string(),
                        SttConnectionCategory::Timeout,
                    )));
                }
            }

            if self.audio_batch_frames == 0 {
                self.batch_started_at = None;
            }

            Ok(())
        } else {
            Err(SttError::Processing("WebSocket not connected".to_string()))
        }
    }

    async fn stop_stream(&mut self) -> SttResult<()> {
        log::info!("BackendProvider: Stopping stream");

        if !self.audio_batch.is_empty() && !self.is_closed.load(Ordering::SeqCst) {
            if let Some(ref ws_write) = self.ws_write {
                let bytes = std::mem::take(&mut self.audio_batch);
                self.audio_batch_frames = 0;
                self.next_send_at = None;
                self.batch_started_at = None;
                self.sent_chunks_count += 1;
                self.sent_bytes_total += bytes.len();
                let flush_fut = async {
                    let mut guard = ws_write.lock().await;
                    guard.send(Message::Binary(bytes)).await
                };
                let _ = tokio::time::timeout(Duration::from_secs(WS_SEND_TIMEOUT_SECS), flush_fut).await;
            }
        }

        // ПЕРВЫМ ДЕЛОМ ставим флаг закрытия — это предотвращает race condition
        self.is_closed.store(true, Ordering::SeqCst);

        if !self.is_streaming {
            return Ok(());
        }

        // Отправляем Close message
        if self.ws_write.is_some() {
            let close_msg = ClientMessage::Close;
            let _ = self.send_json(&close_msg).await;
        }

        // Закрываем WebSocket
        if let Some(ref ws_write) = self.ws_write {
            let close_fut = async {
                let mut guard = ws_write.lock().await;
                guard.close().await
            };
            let _ = tokio::time::timeout(Duration::from_secs(WS_SEND_TIMEOUT_SECS), close_fut).await;
        }

        // Останавливаем receiver task
        if let Some(task) = self.receiver_task.take() {
            task.abort();
            let _ = task.await;
        }

        // Останавливаем keepalive task
        if let Some(task) = self.keepalive_task.take() {
            task.abort();
            let _ = task.await;
        }

        self.ws_write = None;
        self.is_streaming = false;
        self.is_paused = false;
        self.session_id = None;
        self.next_send_at = None;
        self.batch_started_at = None;
        {
            let mut state = self.callbacks.lock().await;
            state.active = None;
            state.pending = None;
            state.swap_on_next_ack = false;
            state.swap_after_seq = 0;
        }

        log::info!(
            "BackendProvider: Stream stopped (sent {} chunks, {} bytes)",
            self.sent_chunks_count,
            self.sent_bytes_total
        );

        Ok(())
    }

    async fn abort(&mut self) -> SttResult<()> {
        log::info!("BackendProvider: Aborting");

        // ПЕРВЫМ ДЕЛОМ ставим флаг закрытия
        self.is_closed.store(true, Ordering::SeqCst);

        if let Some(task) = self.keepalive_task.take() {
            task.abort();
        }

        // Принудительно закрываем без отправки Close
        if let Some(ref ws_write) = self.ws_write {
            let close_fut = async {
                let mut guard = ws_write.lock().await;
                guard.close().await
            };
            let _ = tokio::time::timeout(Duration::from_secs(WS_SEND_TIMEOUT_SECS), close_fut).await;
        }

        if let Some(task) = self.receiver_task.take() {
            task.abort();
        }

        self.ws_write = None;
        self.is_streaming = false;
        self.is_paused = false;
        self.session_id = None;
        {
            let mut state = self.callbacks.lock().await;
            state.active = None;
            state.pending = None;
            state.swap_on_next_ack = false;
            state.swap_after_seq = 0;
        }

        Ok(())
    }

    async fn pause_stream(&mut self) -> SttResult<()> {
        if !self.is_streaming {
            return Err(SttError::Processing("Stream not active".to_string()));
        }
        if self.is_paused {
            return Ok(());
        }

        // Флашим хвост батча, чтобы не потерять последние миллисекунды аудио перед паузой.
        if !self.audio_batch.is_empty() && !self.is_closed.load(Ordering::SeqCst) {
            if let Some(ref ws_write) = self.ws_write {
                let bytes = std::mem::take(&mut self.audio_batch);
                self.audio_batch_frames = 0;
                self.next_send_at = None;
                self.batch_started_at = None;
                self.sent_chunks_count += 1;
                self.sent_bytes_total += bytes.len();
                let flush_fut = async {
                    let mut guard = ws_write.lock().await;
                    guard.send(Message::Binary(bytes)).await
                };
                let _ = tokio::time::timeout(Duration::from_secs(WS_SEND_TIMEOUT_SECS), flush_fut).await;
            }
        }

        self.is_paused = true;
        Ok(())
    }

    async fn resume_stream(
        &mut self,
        on_partial: TranscriptionCallback,
        on_final: TranscriptionCallback,
        on_error: ErrorCallback,
        on_connection_quality: ConnectionQualityCallback,
    ) -> SttResult<()> {
        if !self.is_streaming {
            return Err(SttError::Processing("Stream not active".to_string()));
        }
        if self.is_closed.load(Ordering::SeqCst) {
            return Err(SttError::Connection(SttConnectionError::with_category(
                "Connection closed".to_string(),
                SttConnectionCategory::Closed,
            )));
        }

        // Готовим pending callbacks. Активируем их только после первого ACK на новое аудио,
        // чтобы не словить "поздние" результаты от предыдущей записи в новую UI-сессию.
        {
            let mut state = self.callbacks.lock().await;
            state.pending = Some(CallbackSet {
                on_partial,
                on_final,
                on_error,
                on_connection_quality,
            });
            state.swap_on_next_ack = true;
            state.swap_after_seq = self.sent_chunks_count as u64;
        }

        self.is_paused = false;
        Ok(())
    }

    fn name(&self) -> &str {
        "backend"
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_keep_alive(&self) -> bool {
        true
    }

    fn is_connection_alive(&self) -> bool {
        if !(self.is_streaming && self.is_paused && self.ws_write.is_some()) {
            return false;
        }
        if self.is_closed.load(Ordering::SeqCst) {
            return false;
        }
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
        true // Backend всегда онлайн (облачный сервис)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backend_provider_new() {
        let provider = BackendProvider::new();
        assert!(!provider.is_streaming);
        assert!(provider.auth_token.is_none());
        // В debug сборке (тесты) должен быть dev URL
        #[cfg(debug_assertions)]
        assert_eq!(provider.backend_url, DEV_BACKEND_URL);
        #[cfg(not(debug_assertions))]
        assert_eq!(provider.backend_url, PROD_BACKEND_URL);
    }

    #[test]
    fn test_backend_provider_name() {
        let provider = BackendProvider::new();
        assert_eq!(provider.name(), "backend");
    }

    #[test]
    fn test_backend_provider_is_online() {
        let provider = BackendProvider::new();
        assert!(provider.is_online());
    }

    #[test]
    fn test_backend_provider_supports_streaming() {
        let provider = BackendProvider::new();
        assert!(provider.supports_streaming());
    }
}
