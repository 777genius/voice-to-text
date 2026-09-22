//! Backend STT Provider
//!
//! Подключается к нашему API (api.voicetext.site) вместо прямого подключения к STT provider.
//! Все транскрипции идут через наш бэкенд с лицензией и usage tracking.

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use http::Request;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::Duration;
use tokio_tungstenite::{
    connect_async_with_config, tungstenite::Message, MaybeTlsStream, WebSocketStream,
};

use crate::domain::{
    AudioChunk, AudioDeliveryProgress, ConnectionQualityCallback, ErrorCallback, FinalizeReason,
    ProviderFinalizeReport, SttConfig, SttConnectionCategory, SttConnectionDetails,
    SttConnectionError, SttError, SttProvider, SttProviderType, SttResult, Transcription,
    TranscriptionCallback,
};

use super::backend_messages::{ClientMessage, ServerMessage};
use crate::domain::{
    ContinuationControlResult, ContinuationOperation, ContinuationSession, ContinuationStatusResult,
};

const CAPABILITY_PAUSE_CONTINUE: &str = "el_pause_continue_v1";
pub(crate) fn continuation_opt_in() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("VOICETEXT_EL_PAUSE_CONTINUE_V1")
            .map(|value| value == "true")
            .unwrap_or(false)
    })
}

#[derive(Debug)]
enum ControlReply {
    Mutation(ContinuationControlResult),
    Status(ContinuationStatusResult),
}
#[derive(Clone, Copy, Default)]
enum ContinuationAudioGate {
    #[default]
    Active,
    Paused,
    First {
        pause_epoch: u64,
    },
}
#[derive(Default)]
struct ContinuationTransport {
    offered: bool,
    ready_seen: bool,
    #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
    native_e2e_error_seen: bool,
    audio_gate: ContinuationAudioGate,
    session: Option<ContinuationSession>,
    last_operation: Option<(String, ContinuationOperation)>,
    not_started: Option<crate::domain::ContinuationNotStarted>,
    not_started_deadline: Option<tokio::time::Instant>,
    continue_operation: Option<(String, u64)>,
    changed: Arc<tokio::sync::Notify>,
    waiters: std::collections::HashMap<String, tokio::sync::oneshot::Sender<ControlReply>>,
}
impl ContinuationTransport {
    fn deliver(&mut self, reply: ControlReply) {
        let (key, session) = match &reply {
            ControlReply::Mutation(r) => (&r.request_id, &r.provider_session_id),
            ControlReply::Status(r) => (&r.query_id, &r.provider_session_id),
        };
        if self.session.as_ref().map(|s| &s.provider_session_id) != Some(session) {
            return;
        }
        self.changed.notify_waiters();
        if let Some(waiter) = self.waiters.remove(key) {
            let _ = waiter.send(reply);
        }
    }
}

/// URL бэкенда для production
const PROD_BACKEND_URL: &str = "wss://api.voicetext.site";

/// URL бэкенда для development (localhost)
const DEV_BACKEND_URL: &str = "ws://localhost:8080";

// Таймауты: критичны для стабильности при плохом интернете.
// Без них connect/send могут "подвиснуть" и UI будет бесконечно ждать.
const WS_CONNECT_TIMEOUT_SECS: u64 = 8;
const WS_SEND_TIMEOUT_SECS: u64 = 3;
// The backend can spend up to 5s waiting for an ElevenLabs VAD commit and then
// perform one bounded manual fallback. Keep the desktop deadline above that
// provider-side contract so a valid stop tail is not mistaken for a dead
// connection. Fast acknowledgements still return immediately.
const FINALIZE_DRAIN_ACK_TIMEOUT: Duration = Duration::from_secs(12);
const FINALIZE_POST_ACK_TEXT_GRACE_MS: u64 = 350;
const MAX_AUDIO_DEBT_BYTES: u64 = 30 * 16_000 * 2;
const CAPABILITY_FINALIZE_OUTCOME: &str = "finalize_outcome_v1";
const CAPABILITY_FINALIZE_ACK: &str = "finalize_ack";

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
        let url = url.trim();
        if !url.is_empty() {
            log::info!("Using backend URL from env: {}", url);
            return url.to_string();
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

fn backend_streaming_provider_name(config: &SttConfig) -> &'static str {
    match config.provider {
        SttProviderType::Backend => config.backend_streaming_provider.as_protocol_name(),
        SttProviderType::Deepgram => "deepgram",
        SttProviderType::AssemblyAI => "assemblyai",
        _ => "deepgram",
    }
}

/// Callback для обновления usage (seconds_used, seconds_remaining_total_or_plan)
pub type UsageUpdateCallback = Arc<dyn Fn(f32, f32) + Send + Sync>;

/// Backend STT provider — подключается к нашему API вместо прямого STT provider
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
    finalize_waiter: Arc<Mutex<Option<tokio::sync::oneshot::Sender<FinalizeDrainComplete>>>>,
    on_usage_update_callback: Option<UsageUpdateCallback>,

    // Статистика
    sent_chunks_count: usize,
    sent_bytes_total: usize,

    audio_batch: Vec<u8>,

    next_send_at: Option<std::time::Instant>,
    el_catchup_disabled: bool,
    batch_started_at: Option<std::time::Instant>,

    finalize_drain_ack_timeout: Duration,
    finalize_report: Arc<std::sync::Mutex<Option<ProviderFinalizeReport>>>,
    outcome_negotiated: Arc<AtomicBool>,
    continuation: Arc<std::sync::Mutex<ContinuationTransport>>,
    connection_generation: u64,
    continuation_opted_in: bool,
    control_seq: u64,
    delivery: Arc<std::sync::Mutex<DeliveryLedger>>,
    ack_changed: Arc<tokio::sync::Notify>,
    #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
    native_e2e_lifecycle_active: bool,
}

struct ControlWaiterLease {
    state: Arc<std::sync::Mutex<ContinuationTransport>>,
    key: String,
}
impl Drop for ControlWaiterLease {
    fn drop(&mut self) {
        self.state.lock().unwrap().waiters.remove(&self.key);
    }
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
    // Negotiated Pause/Continue retains its callbacks. Observe the next capture
    // generation at the first ACK for audio issued after Continue instead.
    generation_fence_after_seq: Option<u64>,
}

#[derive(Debug)]
struct FinalizeDrainComplete {
    status: String,
    saw_result: bool,
    text_results_seen: usize,
    outcome: Option<ProviderFinalizeReport>,
}

#[derive(Default)]
struct DeliveryLedger {
    progress: AudioDeliveryProgress,
    pending: std::collections::VecDeque<(u64, u64)>,
    last_ack: u64,
    highest_issued: u64,
    unsent_range: Option<(u64, u64)>,
}
impl DeliveryLedger {
    /// Reserve before polling the write future: an actual ACK may race its completion.
    fn issue(&mut self) -> u64 {
        self.highest_issued += 1;
        self.highest_issued
    }

    fn begin_run(&mut self) {
        self.progress = AudioDeliveryProgress::default();
        self.pending.clear();
        self.unsent_range = None;
        // The WS sequence continues across Deepgram keep-alive runs. A late ACK
        // for the old run must not advance the new run or activate its callbacks.
        self.last_ack = self.highest_issued;
    }

    fn debt_bytes(&self, buffered: usize) -> u64 {
        self.progress
            .sent_bytes
            .saturating_sub(self.progress.acked_bytes)
            .saturating_add(buffered as u64)
    }

    fn admits(&self, buffered: usize, incoming: usize) -> bool {
        self.debt_bytes(buffered).saturating_add(incoming as u64) <= MAX_AUDIO_DEBT_BYTES
            && self.pending.len() < 2048
    }

    fn sent(&mut self, seq: u64, bytes: usize) {
        debug_assert!(seq > 0 && seq <= self.highest_issued);
        self.progress.sent_bytes += bytes as u64;
        if seq <= self.last_ack {
            self.progress.acked_bytes += bytes as u64;
        } else {
            self.pending.push_back((seq, bytes as u64));
        }
    }
    fn record_not_started(&mut self, first: u64, last: u64) -> bool {
        if first == 0 || first > last || last > self.highest_issued || first <= self.last_ack {
            return false;
        }
        if self
            .unsent_range
            .is_some_and(|(old_first, old_last)| first != old_first || last < old_last)
        {
            return false;
        }
        self.unsent_range = Some((first, last));
        true
    }
    fn known_unsent_bytes(&self) -> u64 {
        self.unsent_range.map_or(0, |(first, last)| {
            self.pending
                .iter()
                .filter(|(seq, _)| *seq >= first && *seq <= last)
                .map(|(_, bytes)| *bytes)
                .sum()
        })
    }

    fn ack(&mut self, seq: u64) -> bool {
        if seq == 0
            || seq > self.highest_issued
            || seq <= self.last_ack
            || self.unsent_range.is_some_and(|(first, _)| seq >= first)
        {
            return false;
        }
        self.last_ack = seq;
        while self
            .pending
            .front()
            .map(|(s, _)| *s <= self.last_ack)
            .unwrap_or(false)
        {
            self.progress.acked_bytes += self.pending.pop_front().unwrap().1;
        }
        true
    }
}

impl Drop for BackendProvider {
    fn drop(&mut self) {
        self.record_native_e2e_stream_stopped();
        self.is_closed.store(true, Ordering::SeqCst);
        super::abort_background_task(&mut self.keepalive_task);
        super::abort_background_task(&mut self.receiver_task);
    }
}

impl CallbackState {
    fn error_callback(&self) -> Option<ErrorCallback> {
        if self.swap_on_next_ack {
            if let Some(pending) = self.pending.as_ref() {
                return Some(pending.on_error.clone());
            }
        }
        self.active.as_ref().map(|c| c.on_error.clone())
    }
}

fn category_for_server_error(code: &str) -> SttConnectionCategory {
    match code {
        "timeout" | "TIMEOUT" => SttConnectionCategory::Timeout,
        "rate_limit" | "too_many_sessions" | "RATE_LIMIT_EXCEEDED" | "TOO_MANY_SESSIONS" => {
            SttConnectionCategory::RateLimited
        }
        "LIMIT_EXCEEDED" => SttConnectionCategory::LimitExceeded,
        "PROVIDER_QUOTA_EXCEEDED" => SttConnectionCategory::ProviderQuotaExceeded,
        "PROVIDER_UNAVAILABLE" | "PROVIDER_ERROR" => SttConnectionCategory::ServerUnavailable,
        "INTERNAL_ERROR" => SttConnectionCategory::ServerError,
        _ => SttConnectionCategory::Unknown,
    }
}

fn server_error_closes_stream(code: &str) -> bool {
    matches!(
        code,
        "RATE_LIMIT_EXCEEDED"
            | "TOO_MANY_SESSIONS"
            | "LIMIT_EXCEEDED"
            | "PROVIDER_QUOTA_EXCEEDED"
            | "PROVIDER_UNAVAILABLE"
            | "PROVIDER_ERROR"
            | "INTERNAL_ERROR"
    )
}

fn call_backend_callback(label: &str, callback: impl FnOnce()) {
    if catch_unwind(AssertUnwindSafe(callback)).is_err() {
        log::error!("Backend {} callback panicked", label);
    }
}

async fn report_backend_unexpected_eof(
    callbacks_state: &Arc<Mutex<CallbackState>>,
    is_closed: &Arc<AtomicBool>,
    server_error_reported: bool,
) {
    if server_error_reported
        || is_closed
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
    {
        return;
    }

    log::warn!("Backend WebSocket stream ended without a close frame");
    let callback = {
        let state = callbacks_state.lock().await;
        state.error_callback()
    };
    if let Some(callback) = callback {
        call_backend_callback("error", || {
            callback(SttError::Connection(SttConnectionError {
                message: "WebSocket stream ended without a close frame".to_string(),
                details: SttConnectionDetails {
                    category: Some(SttConnectionCategory::Closed),
                    ..Default::default()
                },
            }))
        });
    }
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
            finalize_waiter: Arc::new(Mutex::new(None)),
            on_usage_update_callback: None,
            sent_chunks_count: 0,
            sent_bytes_total: 0,
            audio_batch: Vec::new(),
            next_send_at: None,
            el_catchup_disabled: false,
            batch_started_at: None,
            finalize_drain_ack_timeout: FINALIZE_DRAIN_ACK_TIMEOUT,
            finalize_report: Arc::new(std::sync::Mutex::new(None)),
            outcome_negotiated: Arc::new(AtomicBool::new(false)),
            continuation: Arc::new(std::sync::Mutex::new(ContinuationTransport::default())),
            connection_generation: 0,
            continuation_opted_in: continuation_opt_in(),
            control_seq: 0,
            delivery: Arc::new(std::sync::Mutex::new(DeliveryLedger::default())),
            ack_changed: Arc::new(tokio::sync::Notify::new()),
            #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
            native_e2e_lifecycle_active: false,
        }
    }

    fn record_native_e2e_stream_started(&mut self) {
        #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
        if !self.native_e2e_lifecycle_active {
            crate::presentation::native_e2e::record_live_provider_started();
            self.native_e2e_lifecycle_active = true;
        }
    }

    fn record_native_e2e_stream_stopped(&mut self) {
        #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
        if self.native_e2e_lifecycle_active {
            crate::presentation::native_e2e::record_live_provider_stopped(
                self.sent_chunks_count == 0,
            );
            self.native_e2e_lifecycle_active = false;
        }
    }

    fn control_waiter(
        &self,
        key: String,
    ) -> SttResult<(
        tokio::sync::oneshot::Receiver<ControlReply>,
        ControlWaiterLease,
    )> {
        let mut state = self.continuation.lock().unwrap();
        if state.waiters.len() >= 8 || state.waiters.contains_key(&key) {
            self.is_closed.store(true, Ordering::SeqCst);
            return Err(SttError::Processing(
                "Continuation control queue saturated".into(),
            ));
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        state.waiters.insert(key.clone(), tx);
        Ok((
            rx,
            ControlWaiterLease {
                state: self.continuation.clone(),
                key,
            },
        ))
    }

    async fn mutate_continuation(
        &mut self,
        session: &ContinuationSession,
        operation: ContinuationOperation,
        lifetime_deadline: tokio::time::Instant,
    ) -> SttResult<ContinuationControlResult> {
        if tokio::time::Instant::now() >= lifetime_deadline {
            return Err(SttError::Processing(
                "Continuation lifetime expired before control".into(),
            ));
        }
        if self.continuation_session().as_ref() != Some(session) {
            return Err(SttError::Unsupported(
                "Continuation session no longer eligible".into(),
            ));
        }
        self.control_seq = self
            .control_seq
            .checked_add(1)
            .filter(|n| *n < u64::MAX)
            .ok_or_else(|| SttError::Processing("Control sequence exhausted".into()))?;
        let control_seq = self.control_seq;
        let request_id = format!("{}-{}", session.connection_generation, control_seq);
        let provider_session_id = session.provider_session_id.clone();
        self.continuation.lock().unwrap().last_operation =
            Some((request_id.clone(), operation.clone()));
        if let ContinuationOperation::Continue { pause_epoch } = &operation {
            self.continuation.lock().unwrap().continue_operation =
                Some((request_id.clone(), *pause_epoch));
        }
        let message = match operation {
            ContinuationOperation::Pause { logical_run_id } => {
                self.flush_pending_audio_batch("continuation pause").await?;
                ClientMessage::Pause {
                    request_id: request_id.clone(),
                    control_seq,
                    logical_run_id: logical_run_id.to_string(),
                }
            }
            ContinuationOperation::Continue { pause_epoch } => ClientMessage::Continue {
                request_id: request_id.clone(),
                control_seq,
                provider_session_id: provider_session_id.clone(),
                pause_epoch,
            },
            ContinuationOperation::Restore {
                pause_epoch,
                continue_request_id,
            } => ClientMessage::PauseRestore {
                request_id: request_id.clone(),
                control_seq,
                provider_session_id: provider_session_id.clone(),
                pause_epoch,
                continue_request_id,
            },
        };
        let (rx, _lease) = self.control_waiter(request_id.clone())?;
        if tokio::time::Instant::now() >= lifetime_deadline {
            return Err(SttError::Processing(
                "Continuation lifetime expired before write".into(),
            ));
        }
        let deadline =
            (tokio::time::Instant::now() + Duration::from_millis(750)).min(lifetime_deadline);
        match tokio::time::timeout_at(deadline, self.send_json(&message)).await {
            Ok(result) => result?,
            Err(_) => {
                self.is_closed.store(true, Ordering::SeqCst);
                return Err(SttError::Processing(
                    "Continuation control write unknown".into(),
                ));
            }
        }
        if let Ok(Ok(ControlReply::Mutation(result))) = tokio::time::timeout_at(deadline, rx).await
        {
            return Ok(result);
        }
        // A single read-only recovery query. Never retransmit the mutation.
        if tokio::time::Instant::now() >= lifetime_deadline {
            return Err(SttError::Processing(
                "Continuation lifetime expired before status".into(),
            ));
        }
        let query_id = format!("q-{request_id}");
        let (rx, _query_lease) = self.control_waiter(query_id.clone())?;
        let query = ClientMessage::ControlStatus {
            query_id,
            operation_request_id: request_id.clone(),
            provider_session_id,
        };
        let recovery = async {
            self.send_json(&query).await?;
            match rx.await {
                Ok(ControlReply::Status(status)) if status.operation_request_id == request_id => {
                    let decision = status.original_decision.ok_or_else(|| {
                        SttError::Processing("Continuation decision unknown".into())
                    })?;
                    Ok(ContinuationControlResult {
                        request_id,
                        provider_session_id: status.provider_session_id,
                        pause_epoch: status.pause_epoch,
                        decision,
                        current_phase: status.current_phase,
                        eligible_now: status.eligible_now,
                        reason: None,
                        continue_window_ms: None,
                    })
                }
                _ => Err(SttError::Processing(
                    "Continuation status unavailable".into(),
                )),
            }
        };
        let recovery_deadline =
            (tokio::time::Instant::now() + Duration::from_millis(500)).min(lifetime_deadline);
        match tokio::time::timeout_at(recovery_deadline, recovery).await {
            Ok(result) => result,
            Err(_) => {
                self.is_closed.store(true, Ordering::SeqCst);
                Err(SttError::Processing(
                    "Continuation decision unknown after bounded status".into(),
                ))
            }
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
            log::warn!(
                "[ReconnectDiag] BackendProvider::send_json skipped because connection is closed: msg={:?}, streaming={}, paused={}, has_ws={}",
                msg,
                self.is_streaming,
                self.is_paused,
                self.ws_write.is_some()
            );
            return Err(SttError::Connection(SttConnectionError::with_category(
                "Backend WebSocket is already closed".to_string(),
                SttConnectionCategory::Closed,
            )));
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

    fn negotiated_audio_interval(&self, bytes: usize) -> Duration {
        // Rollback changes transport pacing only. Outcome, ACK accounting and
        // terminal delivery remain negotiated and keep their correctness fences.
        let bytes_per_second = if self.el_catchup_disabled {
            32_000
        } else {
            128_000
        };
        Duration::from_nanos(bytes as u64 * 1_000_000_000 / bytes_per_second)
    }

    async fn wait_for_audio_window(&self, bytes: usize) -> SttResult<()> {
        // No new ACK deadline applies to a peer until its Ready accepts this capability.
        if !self.outcome_negotiated.load(Ordering::SeqCst) {
            return Ok(());
        }
        let wait = async {
            loop {
                let changed = self.ack_changed.notified();
                if self.is_closed.load(Ordering::SeqCst) {
                    return Err(SttError::Connection(SttConnectionError::simple(
                        "Connection closed while waiting for audio ACK",
                    )));
                }
                let progress = self.delivery.lock().unwrap().progress;
                if progress.sent_bytes.saturating_sub(progress.acked_bytes) + bytes as u64 <= 32_000
                {
                    return Ok(());
                }
                changed.await;
            }
        };
        match tokio::time::timeout(Duration::from_secs(WS_SEND_TIMEOUT_SECS), wait).await {
            Ok(result) => result,
            Err(_) => {
                self.is_closed.store(true, Ordering::SeqCst);
                Err(SttError::Connection(SttConnectionError::with_category(
                    "Backend audio ACK window timed out",
                    SttConnectionCategory::Timeout,
                )))
            }
        }
    }

    async fn flush_pending_audio_batch(&mut self, context: &'static str) -> SttResult<()> {
        if self.audio_batch.is_empty() {
            return Ok(());
        }
        if self.is_closed.load(Ordering::SeqCst) {
            return Err(SttError::Connection(SttConnectionError::with_category(
                format!(
                    "Backend connection closed with {} pending audio bytes during {}",
                    self.audio_batch.len(),
                    context
                ),
                SttConnectionCategory::Closed,
            )));
        }

        if let Some(ref ws_write) = self.ws_write {
            while !self.audio_batch.is_empty() {
                let bytes = self.audio_batch[..self.audio_batch.len().min(9600)].to_vec();
                let bytes_len = bytes.len();
                self.wait_for_audio_window(bytes_len).await?;
                if self.outcome_negotiated.load(Ordering::SeqCst) {
                    if let Some(next) = self.next_send_at {
                        tokio::time::sleep_until(tokio::time::Instant::from_std(next)).await;
                    }
                    self.next_send_at =
                        Some(std::time::Instant::now() + self.negotiated_audio_interval(bytes_len));
                }
                let issued_seq = self.delivery.lock().unwrap().issue();
                let flush_fut = async {
                    let mut guard = ws_write.lock().await;
                    {
                        let state = self.continuation.lock().unwrap();
                        if state.not_started.is_some() {
                            return Err(SttError::ContinuationAudioNotStarted);
                        }
                        if state.session.is_some()
                            && (self.is_closed.load(Ordering::Acquire)
                                || !matches!(state.audio_gate, ContinuationAudioGate::Active))
                        {
                            return Err(SttError::Processing(
                                "Negotiated audio revoked before sink poll".into(),
                            ));
                        }
                    }
                    guard
                        .send(Message::Binary(bytes))
                        .await
                        .map_err(|error| SttError::Processing(error.to_string()))
                };

                match tokio::time::timeout(Duration::from_secs(WS_SEND_TIMEOUT_SECS), flush_fut)
                    .await
                {
                    Ok(Ok(())) => {
                        self.audio_batch.drain(..bytes_len);
                        self.batch_started_at = None;
                        self.sent_chunks_count += 1;
                        self.sent_bytes_total += bytes_len;
                        self.delivery.lock().unwrap().sent(issued_seq, bytes_len);
                        log::debug!(
                            "[ReconnectDiag] BackendProvider {} flushed pending audio batch",
                            context
                        );
                    }
                    Ok(Err(SttError::ContinuationAudioNotStarted)) => {
                        return Err(SttError::ContinuationAudioNotStarted)
                    }
                    Ok(Err(e)) => {
                        self.is_closed.store(true, Ordering::SeqCst);
                        return Err(SttError::Connection(SttConnectionError::with_category(
                            format!(
                                "Backend {} failed to flush pending audio batch: {}",
                                context, e
                            ),
                            SttConnectionCategory::Closed,
                        )));
                    }
                    Err(_) => {
                        self.is_closed.store(true, Ordering::SeqCst);
                        return Err(SttError::Connection(SttConnectionError::with_category(
                            format!("Backend {} timed out flushing pending audio batch", context),
                            SttConnectionCategory::Timeout,
                        )));
                    }
                }
            }
            Ok(())
        } else {
            Err(SttError::Connection(SttConnectionError::with_category(
                format!(
                    "Backend WebSocket writer missing with {} pending audio bytes during {}",
                    self.audio_batch.len(),
                    context
                ),
                SttConnectionCategory::Closed,
            )))
        }
    }

    async fn finalize_and_wait_for_drain(&self, context: &'static str) -> SttResult<()> {
        if self.continuation.lock().unwrap().session.is_some() {
            if let Some(report) = self.finalize_report.lock().unwrap().as_ref() {
                return if matches!(
                    report.reason,
                    FinalizeReason::Drained | FinalizeReason::NoAudio
                ) && report.error.is_none()
                    && report.provider_release == crate::domain::ProviderRelease::Released
                {
                    Ok(())
                } else {
                    Err(SttError::Processing(format!(
                        "Continuation terminal {:?}: {}",
                        report.reason,
                        report
                            .error
                            .as_deref()
                            .unwrap_or("incomplete terminal or provider release")
                    )))
                };
            }
        }
        if self.is_closed.load(Ordering::SeqCst) || self.ws_write.is_none() {
            return Err(SttError::Connection(SttConnectionError::with_category(
                format!(
                    "Backend cannot finalize during {}: connection is closed",
                    context
                ),
                SttConnectionCategory::Closed,
            )));
        }

        let finalize_rx = {
            let (tx, rx) = tokio::sync::oneshot::channel();
            *self.finalize_waiter.lock().await = Some(tx);
            rx
        };

        log::info!(
            "[ReconnectDiag] BackendProvider sending Finalize on {}: closed_before_finalize={}, sent_chunks={}",
            context,
            self.is_closed.load(Ordering::SeqCst),
            self.sent_chunks_count
        );
        if let Err(e) = self.send_json(&ClientMessage::Finalize).await {
            let _ = self.finalize_waiter.lock().await.take();
            return Err(e);
        }

        match tokio::time::timeout(self.finalize_drain_ack_timeout, finalize_rx).await {
            Ok(Ok(done)) => {
                log::info!(
                    "[ReconnectDiag] BackendProvider finalize drain ack received on {}: status={}, saw_result={}, text_results_seen={}",
                    context,
                    done.status,
                    done.saw_result,
                    done.text_results_seen
                );
                if self.outcome_negotiated.load(Ordering::SeqCst) && done.outcome.is_none() {
                    return Err(SttError::Processing(
                        "Negotiated finalize outcome missing".into(),
                    ));
                }
                if let Some(outcome) = done.outcome.as_ref() {
                    if matches!(
                        outcome.reason,
                        FinalizeReason::Deadline
                            | FinalizeReason::ProviderError
                            | FinalizeReason::Cancelled
                    ) {
                        return Err(SttError::Processing(format!(
                            "Backend finalize {:?}: {}",
                            outcome.reason,
                            outcome.error.as_deref().unwrap_or("incomplete recognition")
                        )));
                    }
                }
                if !matches!(
                    done.status.as_str(),
                    "drained" | "flushed" | "no_audio" | "unconfirmed"
                ) {
                    return Err(SttError::Processing(format!(
                        "Backend finalize status: {}",
                        done.status
                    )));
                }
                if done.outcome.is_none() && done.saw_result && done.text_results_seen == 0 {
                    log::info!(
                        "[ReconnectDiag] BackendProvider waiting {}ms for post-ack finalize text on {}",
                        FINALIZE_POST_ACK_TEXT_GRACE_MS,
                        context
                    );
                    tokio::time::sleep(Duration::from_millis(FINALIZE_POST_ACK_TEXT_GRACE_MS))
                        .await;
                }
            }
            Ok(Err(_)) => {
                let _ = self.finalize_waiter.lock().await.take();
                self.is_closed.store(true, Ordering::SeqCst);
                log::warn!(
                    "[ReconnectDiag] BackendProvider finalize drain waiter dropped on {}",
                    context
                );
                return Err(SttError::Connection(SttConnectionError::with_category(
                    format!("Backend finalize drain waiter dropped during {context}"),
                    SttConnectionCategory::Closed,
                )));
            }
            Err(_) => {
                let _ = self.finalize_waiter.lock().await.take();
                self.is_closed.store(true, Ordering::SeqCst);
                log::warn!(
                    "[ReconnectDiag] BackendProvider finalize drain ack timeout on {}",
                    context
                );
                return Err(SttError::Connection(SttConnectionError::with_category(
                    format!("Backend finalize drain ack timed out during {context}"),
                    SttConnectionCategory::Timeout,
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
impl BackendProvider {
    pub(crate) fn with_continuation_for_test() -> Self {
        let mut provider = Self::new();
        provider.continuation_opted_in = true;
        provider
    }
}

impl Default for BackendProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SttProvider for BackendProvider {
    fn continuation_delivery_mode(&self) -> Option<bool> {
        let negotiation = self.continuation.lock().unwrap();
        (!negotiation.offered || negotiation.ready_seen).then_some(negotiation.session.is_some())
    }

    fn continuation_session(&self) -> Option<ContinuationSession> {
        // Live eligibility is independent of paused idle reuse and keepalive policy.
        // Retain lifecycle identity below even after EOF, abort, or receiver failure.
        if self.is_closed.load(Ordering::SeqCst)
            || self.ws_write.is_none()
            || self
                .receiver_task
                .as_ref()
                .is_some_and(|task| task.is_finished())
        {
            return None;
        }
        self.continuation.lock().unwrap().session.clone()
    }

    fn continuation_lifecycle_session(&self) -> Option<ContinuationSession> {
        self.continuation.lock().unwrap().session.clone()
    }

    fn continuation_lifecycle_notify(&self) -> Option<Arc<tokio::sync::Notify>> {
        Some(self.continuation.lock().unwrap().changed.clone())
    }

    async fn send_first_continuation_audio(
        &mut self,
        session: &ContinuationSession,
        pause_epoch: u64,
        chunk: &AudioChunk,
        fence: &crate::domain::ContinuationWriteFence,
    ) -> SttResult<crate::domain::ContinuationFirstWrite> {
        use crate::domain::ContinuationFirstWrite::{NotStarted, Written};
        use std::future::Future;
        if self.continuation_session().as_ref() != Some(session)
            || self.finalize_evidence().is_some()
            || fence.revoked()
            || !self.audio_batch.is_empty()
            || chunk.data.is_empty()
        {
            return Ok(NotStarted);
        }
        let Some(ws_write) = self.ws_write.clone() else {
            return Ok(NotStarted);
        };
        let bytes: Vec<u8> = chunk.data.iter().flat_map(|s| s.to_le_bytes()).collect();
        let first_len = bytes.len().min(9600);
        let before_write = async {
            self.wait_for_audio_window(first_len).await?;
            if let Some(next) = self.next_send_at {
                tokio::time::sleep_until(tokio::time::Instant::from_std(next)).await;
            }
            Ok::<_, SttError>(ws_write.lock().await)
        };
        let mut sink = match tokio::time::timeout_at(fence.deadline, before_write).await {
            Ok(Ok(sink)) => sink,
            _ => return Ok(NotStarted),
        };
        let mut send = Box::pin(sink.send(Message::Binary(bytes[..first_len].to_vec())));
        let mut issued_seq = None;
        let send_result = tokio::time::timeout(Duration::from_secs(WS_SEND_TIMEOUT_SECS),
            std::future::poll_fn(|cx| {
                if issued_seq.is_none() {
                    // The receiver takes this same lock to revoke lifecycle eligibility.
                    // No await occurs between this decision and the first sink poll.
                    let mut state = self.continuation.lock().unwrap();
                    if fence.revoked() || self.is_closed.load(Ordering::Acquire)
                        || state.session.as_ref() != Some(session)
                        || !matches!(state.audio_gate, ContinuationAudioGate::First { pause_epoch: e } if e == pause_epoch)
                    {
                        return std::task::Poll::Ready(Ok(false));
                    }
                    issued_seq = Some(self.delivery.lock().unwrap().issue());
                    fence.attempted.store(true, Ordering::Release);
                    state.audio_gate = ContinuationAudioGate::Active;
                    return send.as_mut().poll(cx).map(|r| r.map(|()| true));
                }
                send.as_mut().poll(cx).map(|r| r.map(|()| true))
            })).await;
        drop(send);
        drop(sink);
        match send_result {
            Ok(Ok(false)) => return Ok(NotStarted),
            Ok(Ok(true)) => {
                self.delivery
                    .lock()
                    .unwrap()
                    .sent(issued_seq.unwrap(), first_len);
                self.sent_chunks_count += 1;
                self.sent_bytes_total += first_len;
                self.next_send_at =
                    Some(std::time::Instant::now() + self.negotiated_audio_interval(first_len));
            }
            _ => {
                self.is_closed.store(true, Ordering::Release);
                return Err(SttError::Processing(
                    "First continuation sink write unknown".into(),
                ));
            }
        }
        self.audio_batch.extend_from_slice(&bytes[first_len..]);
        self.flush_pending_audio_batch("first continuation audio")
            .await?;
        Ok(Written)
    }

    async fn continuation_control(
        &mut self,
        session: &ContinuationSession,
        operation: ContinuationOperation,
        deadline: tokio::time::Instant,
    ) -> SttResult<ContinuationControlResult> {
        let continuation_epoch = match operation {
            ContinuationOperation::Continue { pause_epoch } => Some(pause_epoch),
            _ => None,
        };
        let result = match tokio::time::timeout_at(
            deadline,
            self.mutate_continuation(session, operation, deadline),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(SttError::Processing(
                "Continuation operation lifetime exhausted; outcome unknown".into(),
            )),
        };
        let gate = match (&result, continuation_epoch) {
            (Ok(result), Some(epoch))
                if result.decision == crate::domain::ControlDecision::Accepted
                    && result.eligible_now
                    && result.pause_epoch == Some(epoch)
                    && result.current_phase
                        == crate::domain::ContinuationPhase::ActiveAwaitingAudio =>
            {
                ContinuationAudioGate::First { pause_epoch: epoch }
            }
            _ => ContinuationAudioGate::Paused,
        };
        if matches!(gate, ContinuationAudioGate::First { .. }) {
            self.callbacks.lock().await.generation_fence_after_seq =
                Some(self.sent_chunks_count as u64);
        }
        self.continuation.lock().unwrap().audio_gate = gate;
        if result.is_err() {
            // The retained session remains observable after local transport death.
            // Logical finalization must still publish an incomplete terminal; no
            // local close is promoted to server release evidence.
            self.is_closed.store(true, Ordering::Release);
            self.continuation.lock().unwrap().changed.notify_waiters();
        }
        result
    }

    fn continuation_not_started(&self) -> Option<crate::domain::ContinuationNotStartedEvidence> {
        let state = self.continuation.lock().unwrap();
        Some(crate::domain::ContinuationNotStartedEvidence {
            disposition: state.not_started.clone()?,
            known_unsent_bytes: self.delivery.lock().unwrap().known_unsent_bytes(),
        })
    }

    fn continuation_operation_identity(&self) -> Option<(String, ContinuationOperation)> {
        self.continuation.lock().unwrap().last_operation.clone()
    }

    async fn initialize(&mut self, config: &SttConfig) -> SttResult<()> {
        log::info!("BackendProvider: Initializing");

        // Получаем URL бэкенда (из конфига или авто-детект по окружению)
        let backend_url = config
            .backend_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .map(str::to_string)
            .unwrap_or_else(get_default_backend_url);
        let configured_auth_token = config
            .backend_auth_token
            .as_deref()
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(str::to_string);

        log::info!("BackendProvider: Using backend URL: {}", backend_url);

        // Получаем auth token из конфига
        //
        // В dev режиме для локального бэкенда (localhost) всегда используем dev-local-token.
        // Это защищает от ситуации "я уже логинился в прод, а сейчас запускаю local" → 401.
        log::info!(
            "BackendProvider: config.backend_auth_token present: {}, len: {}",
            configured_auth_token.is_some(),
            configured_auth_token.as_ref().map(|t| t.len()).unwrap_or(0)
        );

        let auth_token = if cfg!(debug_assertions) {
            if is_local_backend_url(&backend_url) {
                if configured_auth_token.as_deref() != Some("dev-local-token") {
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
                configured_auth_token.clone().unwrap_or_else(|| {
                    log::info!("DEV MODE: Using dev-local-token (no real token configured)");
                    "dev-local-token".to_string()
                })
            }
        } else {
            configured_auth_token.ok_or_else(|| {
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
        log::info!(
            "[ReconnectDiag] BackendProvider start_stream: streaming={}, paused={}, closed={}, has_ws={}, receiver_finished={:?}, keepalive_finished={:?}, backend_url={}",
            self.is_streaming,
            self.is_paused,
            self.is_closed.load(Ordering::SeqCst),
            self.ws_write.is_some(),
            self.receiver_task.as_ref().map(|t| t.is_finished()),
            self.keepalive_task.as_ref().map(|t| t.is_finished()),
            self.backend_url
        );

        if self.is_streaming {
            return Err(SttError::Processing("Stream already active".to_string()));
        }

        // Snapshot once before Config; changing the environment cannot change an
        // already active stream. This flag does not affect legacy/DG pacing.
        self.el_catchup_disabled = matches!(
            std::env::var("VOICETEXT_EL_CATCHUP_DISABLED")
                .ok()
                .as_deref(),
            Some("1" | "true")
        );
        *self.finalize_report.lock().unwrap() = None;
        *self.delivery.lock().unwrap() = DeliveryLedger::default();
        self.outcome_negotiated.store(false, Ordering::SeqCst);
        self.connection_generation = self
            .connection_generation
            .checked_add(1)
            .ok_or_else(|| SttError::Internal("Connection generation exhausted".into()))?;
        self.control_seq = 0;
        // Old receiver tasks cannot satisfy a new connection's waiters.
        self.continuation = Arc::new(std::sync::Mutex::new(ContinuationTransport::default()));
        self.sent_chunks_count = 0;
        self.sent_bytes_total = 0;
        self.audio_batch.clear();
        self.next_send_at = None;
        self.batch_started_at = None;

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
            .header(
                "Host",
                self.backend_url.replace("wss://", "").replace("ws://", ""),
            )
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
            connect_async_with_config(
                request,
                Some(super::streaming_websocket_config()),
                false,
            ),
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
            state.generation_fence_after_seq = None;
        }

        // Отправляем Config message
        let provider_name = backend_streaming_provider_name(&config);

        // Парсим keyterms из конфига (строка через запятую → Vec<String>)
        let keyterms = config.streaming_keyterms.as_ref().and_then(|raw| {
            let terms: Vec<String> = raw
                .split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect();
            if terms.is_empty() {
                None
            } else {
                Some(terms)
            }
        });

        let offered_continuation = provider_name == "elevenlabs"
            && self.continuation_opted_in
            && config.continuation_target_eligible;
        self.continuation.lock().unwrap().offered = offered_continuation;
        let config_msg = ClientMessage::Config {
            protocol_v: 2,
            provider: provider_name.to_string(),
            language: config.language.clone(),
            sample_rate: 16000,
            channels: 1,
            encoding: "pcm_s16le".to_string(),
            keyterms,
            capabilities: if provider_name == "elevenlabs" {
                let mut offered = vec![
                    CAPABILITY_FINALIZE_ACK.to_string(),
                    CAPABILITY_FINALIZE_OUTCOME.to_string(),
                ];
                if offered_continuation {
                    offered.push(CAPABILITY_PAUSE_CONTINUE.to_string());
                }
                offered
            } else {
                vec![CAPABILITY_FINALIZE_ACK.to_string()]
            },
        };

        self.send_json(&config_msg).await?;
        log::debug!("Config message sent");

        // Запускаем receiver task для обработки сообщений от сервера.
        // Берём callbacks из self.callbacks, чтобы они могли обновляться при resume_stream.
        let finalize_report = self.finalize_report.clone();
        let outcome_negotiated = self.outcome_negotiated.clone();
        let offered_outcome = provider_name == "elevenlabs";
        let continuation = self.continuation.clone();
        let connection_generation = self.connection_generation;
        let delivery = self.delivery.clone();
        let ack_changed = self.ack_changed.clone();
        let callbacks_state = self.callbacks.clone();
        let finalize_waiter = self.finalize_waiter.clone();
        let on_usage_cb = self.on_usage_update_callback.clone();
        let is_closed_flag = self.is_closed.clone();
        let shared_remaining = self.last_remaining_secs.clone();

        // Сбрасываем remaining на старте нового соединения
        shared_remaining.store(f32::MAX.to_bits(), Ordering::SeqCst);

        let receiver_task = tokio::spawn(async move {
            log::debug!("Backend receiver task started");

            const LIMIT_REMAINING_THRESHOLD: f32 = 5.0;
            let mut server_error_reported = false;
            let mut finalize_text_results_seen = 0usize;
            let mut last_delivery_seq = 0u64;
            let mut continuation_delivery = false;

            while let Some(msg_result) = read.next().await {
                match msg_result {
                    Ok(Message::Text(text)) => {
                        match serde_json::from_str::<ServerMessage>(&text) {
                            Ok(server_msg) => {
                                match server_msg {
                                    ServerMessage::Ready {
                                        session_id,
                                        accepted_capabilities,
                                    } => {
                                        {
                                            let mut negotiation = continuation.lock().unwrap();
                                            if negotiation.ready_seen {
                                                continue;
                                            }
                                            negotiation.ready_seen = true;
                                            negotiation.changed.notify_waiters();
                                            if offered_continuation
                                                && accepted_capabilities
                                                    .iter()
                                                    .any(|c| c == CAPABILITY_PAUSE_CONTINUE)
                                                && accepted_capabilities
                                                    .iter()
                                                    .any(|c| c == CAPABILITY_FINALIZE_OUTCOME)
                                            {
                                                continuation_delivery = true;
                                                negotiation.session = Some(ContinuationSession {
                                                    connection_generation,
                                                    provider_session_id: session_id.clone(),
                                                });
                                            }
                                        }
                                        outcome_negotiated.store(
                                            offered_outcome
                                                && accepted_capabilities
                                                    .iter()
                                                    .any(|c| c == CAPABILITY_FINALIZE_OUTCOME),
                                            Ordering::SeqCst,
                                        );
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
                                            call_backend_callback("connection quality", || {
                                                cb("Good".to_string(), None)
                                            });
                                        }
                                    }

                                    ServerMessage::PauseAccepted {
                                        mut result,
                                        continue_window_ms,
                                    } => {
                                        result.continue_window_ms = Some(continue_window_ms);
                                        continuation
                                            .lock()
                                            .unwrap()
                                            .deliver(ControlReply::Mutation(result));
                                    }
                                    ServerMessage::PauseRejected { result }
                                    | ServerMessage::ContinueResult { result }
                                    | ServerMessage::PauseRestoreResult { result } => {
                                        continuation
                                            .lock()
                                            .unwrap()
                                            .deliver(ControlReply::Mutation(result));
                                    }
                                    ServerMessage::ControlStatusResult { result } => {
                                        continuation
                                            .lock()
                                            .unwrap()
                                            .deliver(ControlReply::Status(result));
                                    }
                                    ServerMessage::Ack { seq } => {
                                        log::trace!("Ack received: seq={}", seq);
                                        let ack_result = {
                                            let mut ledger = delivery.lock().unwrap();
                                            if seq == 0 || seq > ledger.highest_issued {
                                                Err(ledger.highest_issued)
                                            } else {
                                                Ok(ledger.ack(seq))
                                            }
                                        };
                                        match ack_result {
                                            Err(highest_issued) => {
                                                is_closed_flag.store(true, Ordering::SeqCst);
                                                ack_changed.notify_one();
                                                server_error_reported = true;
                                                let cb =
                                                    callbacks_state.lock().await.error_callback();
                                                if let Some(cb) = cb {
                                                    call_backend_callback("invalid ACK", || {
                                                        cb(SttError::Connection(SttConnectionError {
                                                        message: format!("Backend ACK seq {} is outside issued range 1..={}", seq, highest_issued),
                                                        details: SttConnectionDetails {
                                                            category: Some(SttConnectionCategory::ServerError),
                                                            server_code: Some("INVALID_ACK".into()),
                                                            ..Default::default()
                                                        },
                                                    }))
                                                    });
                                                }
                                                break;
                                            }
                                            Ok(false) => continue,
                                            Ok(true) => ack_changed.notify_one(),
                                        }
                                        // Если есть pending callbacks (новая UI-сессия) — активируем их на первом ACK.
                                        // Это даёт чёткую границу между "старыми" и "новыми" результатами.
                                        let (swapped, generation_fenced) = {
                                            let mut state = callbacks_state.lock().await;
                                            let generation_fenced = state
                                                .generation_fence_after_seq
                                                .is_some_and(|after| seq > after);
                                            if generation_fenced {
                                                state.generation_fence_after_seq = None;
                                            }
                                            if state.swap_on_next_ack && seq > state.swap_after_seq
                                            {
                                                state.swap_on_next_ack = false;
                                                state.swap_after_seq = 0;
                                                if state.pending.is_some() {
                                                    state.active = state.pending.take();
                                                }
                                                (true, generation_fenced)
                                            } else {
                                                (false, generation_fenced)
                                            }
                                        };
                                        if swapped || generation_fenced {
                                            #[cfg(all(
                                                debug_assertions,
                                                feature = "native-window-e2e"
                                            ))]
                                            crate::presentation::native_e2e::record_live_provider_callback_swap();
                                            log::debug!(
                                                "Provider generation fenced after first ACK (callbacks_swapped={swapped})"
                                            );
                                        }
                                    }

                                    ServerMessage::Partial {
                                        text,
                                        confidence,
                                        is_segment_final,
                                        start_ms,
                                        duration_ms,
                                    } => {
                                        log::debug!("Partial: {} (conf: {:?})", text, confidence);
                                        let has_text = !text.trim().is_empty();
                                        if has_text && finalize_waiter.lock().await.is_some() {
                                            finalize_text_results_seen =
                                                finalize_text_results_seen.saturating_add(1);
                                        }
                                        let mut transcription = Transcription::new(
                                            text,
                                            is_segment_final.unwrap_or(false),
                                        )
                                        .with_timing(
                                            start_ms.unwrap_or(0) as f64 / 1000.0,
                                            duration_ms.unwrap_or(0) as f64 / 1000.0,
                                        );
                                        transcription.continuation_delivery = continuation_delivery;
                                        transcription.completion_v1 =
                                            outcome_negotiated.load(Ordering::SeqCst);
                                        if let Some(conf) = confidence {
                                            transcription = transcription.with_confidence(conf);
                                        }
                                        let cb = {
                                            let state = callbacks_state.lock().await;
                                            state.active.as_ref().map(|c| c.on_partial.clone())
                                        };
                                        if let Some(cb) = cb {
                                            call_backend_callback("partial transcription", || {
                                                cb(transcription)
                                            });
                                        }
                                    }

                                    ServerMessage::Stable {
                                        text,
                                        confidence,
                                        delivery_seq,
                                    } => {
                                        if !outcome_negotiated.load(Ordering::SeqCst)
                                            || delivery_seq <= last_delivery_seq
                                        {
                                            continue;
                                        }
                                        last_delivery_seq = delivery_seq;
                                        let mut transcription = Transcription::final_result(text);
                                        transcription.delivery_seq = Some(delivery_seq);
                                        transcription.continuation_delivery = continuation_delivery;
                                        transcription.completion_v1 = true;
                                        transcription.confidence = confidence;
                                        let cb = callbacks_state
                                            .lock()
                                            .await
                                            .active
                                            .as_ref()
                                            .map(|c| c.on_final.clone());
                                        if let Some(cb) = cb {
                                            call_backend_callback("stable transcription", || {
                                                cb(transcription)
                                            });
                                        }
                                    }

                                    ServerMessage::Final {
                                        text,
                                        confidence,
                                        start_ms,
                                        duration_ms,
                                    } => {
                                        log::debug!(
                                            "Final: {} (conf: {:?}, dur: {}ms)",
                                            text,
                                            confidence,
                                            duration_ms
                                        );
                                        let has_text = !text.trim().is_empty();
                                        if has_text && finalize_waiter.lock().await.is_some() {
                                            finalize_text_results_seen =
                                                finalize_text_results_seen.saturating_add(1);
                                        }
                                        let mut transcription = Transcription::final_result(text)
                                            .with_timing(
                                                start_ms.unwrap_or(0) as f64 / 1000.0,
                                                duration_ms as f64 / 1000.0,
                                            );
                                        transcription.continuation_delivery = continuation_delivery;
                                        transcription.completion_v1 =
                                            outcome_negotiated.load(Ordering::SeqCst);
                                        if let Some(conf) = confidence {
                                            transcription = transcription.with_confidence(conf);
                                        }
                                        let cb = {
                                            let state = callbacks_state.lock().await;
                                            state.active.as_ref().map(|c| c.on_final.clone())
                                        };
                                        if let Some(cb) = cb {
                                            call_backend_callback("final transcription", || {
                                                cb(transcription)
                                            });
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
                                        shared_remaining
                                            .store(remaining.to_bits(), Ordering::SeqCst);
                                        log::debug!(
                                            "Usage: used={:.1}s, remaining={:.1}s",
                                            seconds_used,
                                            remaining
                                        );
                                        if let Some(ref cb) = on_usage_cb {
                                            call_backend_callback("usage", || {
                                                cb(seconds_used, remaining)
                                            });
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
                                            call_backend_callback("connection quality", || {
                                                cb("Good".to_string(), None)
                                            });
                                        }
                                    }

                                    ServerMessage::Error {
                                        code,
                                        message,
                                        not_started,
                                    } => {
                                        if code == "CONTINUE_AUDIO_NOT_STARTED" {
                                            if let Some(disposition) = not_started {
                                                let mut state = continuation.lock().unwrap();
                                                let correlated =
                                                    state.session.as_ref().is_some_and(|session| {
                                                        session.provider_session_id
                                                            == disposition.provider_session_id
                                                    }) && state
                                                        .continue_operation
                                                        .as_ref()
                                                        .is_some_and(|(id, epoch)| {
                                                            *epoch == disposition.pause_epoch
                                                                && disposition
                                                                    .continue_request_id
                                                                    .as_ref()
                                                                    .is_none_or(|request| {
                                                                        request == id
                                                                    })
                                                        });
                                                if correlated
                                                    && delivery.lock().unwrap().record_not_started(
                                                        disposition.first_unsent_seq,
                                                        disposition.last_unsent_seq,
                                                    )
                                                {
                                                    state.not_started = Some(disposition);
                                                    state.not_started_deadline.get_or_insert(
                                                        tokio::time::Instant::now()
                                                            + FINALIZE_DRAIN_ACK_TIMEOUT,
                                                    );
                                                    state.audio_gate =
                                                        ContinuationAudioGate::Paused;
                                                    state.changed.notify_waiters();
                                                    // Keep A's original drain and receiver alive. FinalizeComplete
                                                    // carries the failed terminal outcome; no synthetic replay.
                                                    continue;
                                                }
                                            }
                                        }
                                        #[cfg(all(
                                            debug_assertions,
                                            feature = "native-window-e2e"
                                        ))]
                                        {
                                            continuation.lock().unwrap().native_e2e_error_seen =
                                                true;
                                        }
                                        log::error!("Server error: {} - {}", code, message);
                                        server_error_reported = server_error_closes_stream(&code);
                                        // Negotiated EL observers must see a terminal error even
                                        // when the peer leaves its socket open after reporting it.
                                        if server_error_reported {
                                            // Serialize terminal closure with the first B sink poll.
                                            let state = continuation.lock().unwrap();
                                            if state.session.is_some() {
                                                is_closed_flag.store(true, Ordering::SeqCst);
                                                state.changed.notify_waiters();
                                            }
                                        }
                                        let cb = {
                                            let state = callbacks_state.lock().await;
                                            state.error_callback()
                                        };
                                        if let Some(cb) = cb {
                                            call_backend_callback("error", || {
                                                cb(SttError::Connection(SttConnectionError {
                                                    message,
                                                    details: SttConnectionDetails {
                                                        category: Some(category_for_server_error(
                                                            &code,
                                                        )),
                                                        server_code: Some(code),
                                                        ..Default::default()
                                                    },
                                                }))
                                            });
                                        }
                                    }

                                    ServerMessage::FinalizeComplete {
                                        status,
                                        saw_result,
                                        outcome,
                                    } => {
                                        let outcome = if outcome_negotiated.load(Ordering::SeqCst) {
                                            outcome
                                        } else {
                                            None
                                        };
                                        log::debug!(
                                            "Finalize drain complete: status={}, saw_result={}, text_results_seen={}",
                                            status,
                                            saw_result,
                                            finalize_text_results_seen
                                        );
                                        let text_results_seen = finalize_text_results_seen;
                                        finalize_text_results_seen = 0;
                                        // Pause may reach terminal without a legacy Finalize waiter.
                                        // Preserve its actual server release/tail evidence for the
                                        // logical owner and wake terminal/readiness observers.
                                        if continuation.lock().unwrap().session.is_some() {
                                            continuation.lock().unwrap().audio_gate =
                                                ContinuationAudioGate::Paused;
                                            if let Some(report) = outcome.as_ref() {
                                                *finalize_report.lock().unwrap() =
                                                    Some(report.clone());
                                            }
                                            continuation.lock().unwrap().changed.notify_waiters();
                                        }
                                        let waiter = finalize_waiter.lock().await.take();
                                        if let Some(waiter) = waiter {
                                            if let Some(report) = outcome.as_ref() {
                                                *finalize_report.lock().unwrap() =
                                                    Some(report.clone());
                                            }
                                            let _ = waiter.send(FinalizeDrainComplete {
                                                status,
                                                saw_result,
                                                text_results_seen,
                                                outcome,
                                            });
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
                        log::warn!(
                            "[ReconnectDiag] Backend receiver saw close frame: frame={:?}, local_closed={}, server_error_reported={}",
                            frame,
                            is_closed_flag.load(Ordering::SeqCst),
                            server_error_reported
                        );
                        // Если мы сами инициировали закрытие или уже отдали точную ServerMessage::Error,
                        // не эмитим вторую обобщённую ошибку в UI.
                        if is_closed_flag.load(Ordering::SeqCst) || server_error_reported {
                            break;
                        }
                        is_closed_flag.store(true, Ordering::SeqCst);
                        let cb = {
                            let state = callbacks_state.lock().await;
                            state.error_callback()
                        };
                        if let Some(cb) = cb {
                            let code_u16 = frame.as_ref().map(|f| u16::from(f.code));
                            let mut category = match code_u16 {
                                Some(1008) => SttConnectionCategory::LimitExceeded,
                                Some(1012) | Some(1013) | Some(1014) => {
                                    SttConnectionCategory::ServerUnavailable
                                }
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

                            call_backend_callback("error", || {
                                cb(SttError::Connection(SttConnectionError {
                                    message: "WebSocket closed by server".to_string(),
                                    details: SttConnectionDetails {
                                        category: Some(category),
                                        ws_close_code: code_u16,
                                        ..Default::default()
                                    },
                                }))
                            });
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
                        log::warn!(
                            "[ReconnectDiag] Backend receiver saw websocket error: {}, local_closed={}, server_error_reported={}",
                            e,
                            is_closed_flag.load(Ordering::SeqCst),
                            server_error_reported
                        );
                        // Если закрытие инициировано нами или уже отдали точную ServerMessage::Error,
                        // не поднимаем вторую обобщённую ошибку в UI.
                        if is_closed_flag.load(Ordering::SeqCst) || server_error_reported {
                            break;
                        }
                        is_closed_flag.store(true, Ordering::SeqCst);
                        let cb = {
                            let state = callbacks_state.lock().await;
                            state.error_callback()
                        };
                        if let Some(cb) = cb {
                            let mut details = match &e {
                                tokio_tungstenite::tungstenite::Error::Io(ioe) => {
                                    let kind = ioe.kind();
                                    let kind_str = format!("{:?}", kind);
                                    let os_error = ioe.raw_os_error();
                                    let category = match kind {
                                        std::io::ErrorKind::ConnectionRefused => {
                                            SttConnectionCategory::Refused
                                        }
                                        std::io::ErrorKind::ConnectionReset => {
                                            SttConnectionCategory::Reset
                                        }
                                        std::io::ErrorKind::BrokenPipe => {
                                            SttConnectionCategory::ServerUnavailable
                                        }
                                        std::io::ErrorKind::NotConnected
                                        | std::io::ErrorKind::NetworkUnreachable
                                        | std::io::ErrorKind::HostUnreachable
                                        | std::io::ErrorKind::AddrNotAvailable => {
                                            SttConnectionCategory::Offline
                                        }
                                        std::io::ErrorKind::TimedOut => {
                                            SttConnectionCategory::Timeout
                                        }
                                        _ => SttConnectionCategory::Unknown,
                                    };
                                    SttConnectionDetails {
                                        category: Some(category),
                                        io_error_kind: Some(kind_str),
                                        os_error,
                                        ..Default::default()
                                    }
                                }
                                tokio_tungstenite::tungstenite::Error::Tls(_) => {
                                    SttConnectionDetails {
                                        category: Some(SttConnectionCategory::Tls),
                                        ..Default::default()
                                    }
                                }
                                tokio_tungstenite::tungstenite::Error::ConnectionClosed
                                | tokio_tungstenite::tungstenite::Error::AlreadyClosed => {
                                    SttConnectionDetails {
                                        category: Some(SttConnectionCategory::Closed),
                                        ..Default::default()
                                    }
                                }
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

                            call_backend_callback("error", || {
                                cb(SttError::Connection(SttConnectionError {
                                    message: e.to_string(),
                                    details,
                                }))
                            });
                        }
                        break;
                    }
                }
            }

            // Terminal server errors were already reported above, but they
            // must still close the transport fence before finalize cleanup.
            // For an unexpected EOF, report_backend_unexpected_eof performs
            // the same atomic close before its first await.
            if server_error_reported {
                is_closed_flag.store(true, Ordering::SeqCst);
            }
            report_backend_unexpected_eof(&callbacks_state, &is_closed_flag, server_error_reported)
                .await;

            // Wake an in-flight pause/stop immediately when transport dies.
            // Dropping the sender makes the oneshot receiver fail closed
            // instead of sleeping until the provider acknowledgement timeout.
            let _ = finalize_waiter.lock().await.take();

            log::info!("[ReconnectDiag] Backend receiver task finished, marking connection closed");
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
                    log::warn!(
                        "[ReconnectDiag] Backend keepalive ping failed, marking connection closed"
                    );
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
        self.record_native_e2e_stream_started();

        log::info!("BackendProvider: Stream started");
        Ok(())
    }

    async fn send_audio(&mut self, chunk: &AudioChunk) -> SttResult<()> {
        if self.continuation.lock().unwrap().not_started.is_some() {
            return Err(SttError::ContinuationAudioNotStarted);
        }
        if !matches!(
            self.continuation.lock().unwrap().audio_gate,
            ContinuationAudioGate::Active
        ) {
            return Err(SttError::Processing(
                "Continuation audio remains upstream-disabled".into(),
            ));
        }
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

            if !self
                .delivery
                .lock()
                .unwrap()
                .admits(self.audio_batch.len(), chunk.data.len().saturating_mul(2))
            {
                self.is_closed.store(true, Ordering::SeqCst);
                return Err(SttError::Processing(
                    "Backend audio delivery buffer capacity exceeded".into(),
                ));
            }
            self.audio_batch.reserve(chunk.data.len() * 2);
            let now = std::time::Instant::now();
            if self.audio_batch.is_empty() {
                self.batch_started_at = Some(now);
            }
            for &sample in &chunk.data {
                self.audio_batch.extend_from_slice(&sample.to_le_bytes());
            }

            let batch_age_ms = self
                .batch_started_at
                .map(|t| now.saturating_duration_since(t).as_millis() as u64)
                .unwrap_or(0);
            let available_frames = self.audio_batch.len() / FRAME_BYTES;
            let ready_to_send =
                available_frames >= MIN_FRAMES_PER_MESSAGE || batch_age_ms >= MAX_BATCH_WAIT_MS;
            if !ready_to_send {
                return Ok(());
            }

            let frames_to_send = available_frames.min(MAX_FRAMES_PER_MESSAGE);
            let bytes_to_send = frames_to_send * FRAME_BYTES;
            if bytes_to_send == 0 {
                return Ok(());
            }

            // Keep the authoritative batch intact until the sink accepts the frame.
            // A cancelled or failed send must not silently discard speech.
            let bytes = self.audio_batch[..bytes_to_send].to_vec();

            self.wait_for_audio_window(bytes_to_send).await?;
            let now2 = std::time::Instant::now();
            let next_at = self.next_send_at.unwrap_or(now2);
            if next_at > now2 {
                tokio::time::sleep_until(tokio::time::Instant::from_std(next_at)).await;
            }
            self.next_send_at = Some(
                std::time::Instant::now()
                    + if self.outcome_negotiated.load(Ordering::SeqCst) {
                        // PCM16 mono 16kHz, capped at 4x real time.
                        self.negotiated_audio_interval(bytes_to_send)
                    } else {
                        Duration::from_millis(MIN_SEND_INTERVAL_MS)
                    },
            );

            let issued_seq = self.delivery.lock().unwrap().issue();
            let send_fut = async {
                let mut guard = ws_write.lock().await;
                {
                    let state = self.continuation.lock().unwrap();
                    if state.not_started.is_some() {
                        return Err(SttError::ContinuationAudioNotStarted);
                    }
                    if state.session.is_some()
                        && (self.is_closed.load(Ordering::Acquire)
                            || !matches!(state.audio_gate, ContinuationAudioGate::Active))
                    {
                        return Err(SttError::Processing(
                            "Negotiated audio revoked before sink poll".into(),
                        ));
                    }
                }
                guard
                    .send(Message::Binary(bytes))
                    .await
                    .map_err(|error| SttError::Processing(error.to_string()))
            };

            match tokio::time::timeout(Duration::from_secs(WS_SEND_TIMEOUT_SECS), send_fut).await {
                Ok(Ok(())) => {
                    self.audio_batch.drain(..bytes_to_send);
                    self.sent_chunks_count += 1;
                    self.sent_bytes_total += bytes_to_send;
                    self.delivery
                        .lock()
                        .unwrap()
                        .sent(issued_seq, bytes_to_send);
                    if self.sent_chunks_count % 50 == 0 {
                        log::debug!(
                            "Backend: sent {} chunks, {} bytes total",
                            self.sent_chunks_count,
                            self.sent_bytes_total
                        );
                    }
                }
                Ok(Err(SttError::ContinuationAudioNotStarted)) => {
                    return Err(SttError::ContinuationAudioNotStarted)
                }
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

            if self.audio_batch.is_empty() {
                self.batch_started_at = None;
            }

            Ok(())
        } else {
            Err(SttError::Processing("WebSocket not connected".to_string()))
        }
    }

    async fn stop_stream(&mut self) -> SttResult<()> {
        log::info!("BackendProvider: Stopping stream");

        let original_drain_deadline = self.continuation.lock().unwrap().not_started_deadline;
        if let Some(deadline) = original_drain_deadline {
            // A server NotStarted disposition resumes A's existing drain. Never
            // send another Finalize or replay the local batch in this branch.
            loop {
                if self.finalize_evidence().is_some()
                    || tokio::time::Instant::now() >= deadline
                    || self.is_closed.load(Ordering::Acquire)
                {
                    break;
                }
                let changed = self.continuation.lock().unwrap().changed.clone();
                let _ = tokio::time::timeout_at(
                    deadline.min(tokio::time::Instant::now() + Duration::from_millis(100)),
                    changed.notified(),
                )
                .await;
            }
            self.audio_batch.clear();
            self.abort().await?;
            return Err(SttError::ContinuationAudioNotStarted);
        }

        if !self.is_streaming {
            self.is_closed.store(true, Ordering::SeqCst);
            self.record_native_e2e_stream_stopped();
            return Ok(());
        }

        let continuation_terminal = self.continuation.lock().unwrap().session.is_some()
            && self.finalize_report.lock().unwrap().is_some();
        if continuation_terminal && self.audio_batch.is_empty() {
            // Server already froze the logical terminal. Do not manufacture a
            // second irreversible Finalize merely because the UI now observes it.
            let terminal = self
                .finalize_and_wait_for_drain("observed continuation terminal")
                .await;
            let cleanup = self.abort().await;
            return terminal.and(cleanup);
        }
        let mut stop_result = self.flush_pending_audio_batch("stop").await;
        if stop_result.is_ok() && self.sent_chunks_count > 0 {
            stop_result = self.finalize_and_wait_for_drain("stop").await;
        }

        let _ = self.finalize_waiter.lock().await.take();

        // Отправляем Close message
        if !self.is_closed.load(Ordering::SeqCst) && self.ws_write.is_some() {
            let close_msg = ClientMessage::Close;
            if let Err(error) = self.send_json(&close_msg).await {
                if stop_result.is_ok() {
                    stop_result = Err(error);
                }
            }
        }

        self.is_closed.store(true, Ordering::SeqCst);

        // Закрываем WebSocket
        if let Some(ref ws_write) = self.ws_write {
            let close_fut = async {
                let mut guard = ws_write.lock().await;
                guard.close().await
            };
            match tokio::time::timeout(Duration::from_secs(WS_SEND_TIMEOUT_SECS), close_fut).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) if stop_result.is_ok() => {
                    stop_result = Err(SttError::Connection(SttConnectionError::with_category(
                        format!("Backend WebSocket close failed: {}", error),
                        SttConnectionCategory::Closed,
                    )));
                }
                Err(_) if stop_result.is_ok() => {
                    stop_result = Err(SttError::Connection(SttConnectionError::with_category(
                        "Backend WebSocket close timed out".to_string(),
                        SttConnectionCategory::Timeout,
                    )));
                }
                _ => {}
            }
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
        self.record_native_e2e_stream_stopped();
        self.session_id = None;
        self.audio_batch.clear();
        self.next_send_at = None;
        self.batch_started_at = None;
        {
            let mut state = self.callbacks.lock().await;
            state.active = None;
            state.pending = None;
            state.swap_on_next_ack = false;
            state.swap_after_seq = 0;
            state.generation_fence_after_seq = None;
        }

        log::info!(
            "BackendProvider: Stream stopped (sent {} chunks, {} bytes)",
            self.sent_chunks_count,
            self.sent_bytes_total
        );

        stop_result
    }

    async fn abort(&mut self) -> SttResult<()> {
        log::info!("BackendProvider: Aborting");

        // ПЕРВЫМ ДЕЛОМ ставим флаг закрытия
        self.is_closed.store(true, Ordering::SeqCst);
        let _ = self.finalize_waiter.lock().await.take();

        if let Some(task) = self.keepalive_task.take() {
            task.abort();
            let _ = task.await;
        }

        if let Some(task) = self.receiver_task.take() {
            task.abort();
            let _ = task.await;
        }

        // Hard teardown must not call SinkExt::close: it can flush a frame left
        // half-written by a cancelled send. Both halves and their task owners
        // are dropped instead, with no further protocol or transport writes.
        self.ws_write = None;
        self.is_streaming = false;
        self.is_paused = false;
        self.record_native_e2e_stream_stopped();
        self.session_id = None;
        self.audio_batch.clear();
        self.next_send_at = None;
        self.batch_started_at = None;
        {
            let mut state = self.callbacks.lock().await;
            state.active = None;
            state.pending = None;
            state.swap_on_next_ack = false;
            state.swap_after_seq = 0;
            state.generation_fence_after_seq = None;
        }

        Ok(())
    }

    async fn pause_stream(&mut self) -> SttResult<()> {
        log::info!(
            "[ReconnectDiag] BackendProvider pause_stream requested: streaming={}, paused={}, closed={}, has_ws={}, sent_chunks={}, sent_bytes={}, receiver_finished={:?}, keepalive_finished={:?}",
            self.is_streaming,
            self.is_paused,
            self.is_closed.load(Ordering::SeqCst),
            self.ws_write.is_some(),
            self.sent_chunks_count,
            self.sent_bytes_total,
            self.receiver_task.as_ref().map(|t| t.is_finished()),
            self.keepalive_task.as_ref().map(|t| t.is_finished())
        );
        if !self.is_streaming {
            return Err(SttError::Processing("Stream not active".to_string()));
        }
        if self.is_paused {
            return Ok(());
        }

        self.flush_pending_audio_batch("pause").await?;
        self.finalize_and_wait_for_drain("pause").await?;

        self.is_paused = true;
        log::info!(
            "[ReconnectDiag] BackendProvider pause_stream completed: streaming={}, paused={}, closed={}, alive={}",
            self.is_streaming,
            self.is_paused,
            self.is_closed.load(Ordering::SeqCst),
            self.is_connection_alive()
        );
        Ok(())
    }

    async fn resume_stream(
        &mut self,
        on_partial: TranscriptionCallback,
        on_final: TranscriptionCallback,
        on_error: ErrorCallback,
        on_connection_quality: ConnectionQualityCallback,
    ) -> SttResult<()> {
        log::info!(
            "[ReconnectDiag] BackendProvider resume_stream requested: streaming={}, paused={}, closed={}, has_ws={}, sent_chunks={}, receiver_finished={:?}, keepalive_finished={:?}",
            self.is_streaming,
            self.is_paused,
            self.is_closed.load(Ordering::SeqCst),
            self.ws_write.is_some(),
            self.sent_chunks_count,
            self.receiver_task.as_ref().map(|t| t.is_finished()),
            self.keepalive_task.as_ref().map(|t| t.is_finished())
        );
        if !self.is_streaming {
            return Err(SttError::Processing("Stream not active".to_string()));
        }
        if self.is_closed.load(Ordering::SeqCst) {
            return Err(SttError::Connection(SttConnectionError::with_category(
                "Connection closed".to_string(),
                SttConnectionCategory::Closed,
            )));
        }

        *self.finalize_report.lock().unwrap() = None;
        {
            let mut ledger = self.delivery.lock().unwrap();
            ledger.begin_run();
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
            state.generation_fence_after_seq = None;
        }

        self.is_paused = false;
        log::info!(
            "[ReconnectDiag] BackendProvider resume_stream accepted: swap_after_seq={}, closed={}, alive_after_resume={}",
            self.sent_chunks_count,
            self.is_closed.load(Ordering::SeqCst),
            self.is_connection_alive()
        );
        Ok(())
    }

    fn preferred_audio_batch_samples(&self) -> Option<usize> {
        (self.outcome_negotiated.load(Ordering::SeqCst) && !self.el_catchup_disabled)
            .then_some(4800)
    }

    fn finalize_evidence(&self) -> Option<ProviderFinalizeReport> {
        self.finalize_report.lock().unwrap().clone()
    }

    fn audio_delivery_progress(&self) -> Option<AudioDeliveryProgress> {
        Some(self.delivery.lock().unwrap().progress)
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
            log::debug!(
                "[ReconnectDiag] BackendProvider is_connection_alive=false: streaming={}, paused={}, has_ws={}",
                self.is_streaming,
                self.is_paused,
                self.ws_write.is_some()
            );
            return false;
        }
        if self.is_closed.load(Ordering::SeqCst) {
            log::debug!(
                "[ReconnectDiag] BackendProvider is_connection_alive=false: is_closed=true"
            );
            return false;
        }
        if let Some(task) = &self.receiver_task {
            if task.is_finished() {
                log::debug!(
                    "[ReconnectDiag] BackendProvider is_connection_alive=false: receiver_task finished"
                );
                return false;
            }
        }
        if let Some(task) = &self.keepalive_task {
            if task.is_finished() {
                log::debug!(
                    "[ReconnectDiag] BackendProvider is_connection_alive=false: keepalive_task finished"
                );
                return false;
            }
        }
        true
    }

    #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
    fn native_e2e_transport_observation(&self) -> Option<(bool, bool)> {
        // is_connection_alive is a *paused reuse* probe, not active transport readiness.
        let retained = self.ws_write.is_some();
        let ready = retained
            && self.is_streaming
            && !self.is_closed.load(Ordering::SeqCst)
            && self
                .receiver_task
                .as_ref()
                .is_some_and(|task| !task.is_finished())
            && {
                let transport = self.continuation.lock().unwrap();
                transport.ready_seen && !transport.native_e2e_error_seen
            };
        Some((ready, retained))
    }

    #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
    fn native_e2e_transport_observation_at_boundary(
        &self,
        boundary: &mut dyn FnMut(),
    ) -> Option<(bool, bool)> {
        // Ready is written under this same mutex by the receiver. Holding it
        // across synchronous gesture acceptance makes the observed ordering
        // authoritative even if gesture/fixture mutexes are briefly contended.
        let transport = self.continuation.lock().unwrap();
        let retained = self.ws_write.is_some();
        let ready = retained
            && self.is_streaming
            && !self.is_closed.load(Ordering::SeqCst)
            && self
                .receiver_task
                .as_ref()
                .is_some_and(|task| !task.is_finished())
            && transport.ready_seen
            && !transport.native_e2e_error_seen;
        boundary();
        Some((ready, retained))
    }

    fn is_online(&self) -> bool {
        true // Backend всегда онлайн (облачный сервис)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tokio::net::TcpListener;
    use tokio::task::JoinHandle;
    use tokio_tungstenite::accept_async;

    #[tokio::test]
    async fn continuation_live_query_rejects_finished_receiver_and_missing_sink() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            accept_async(stream).await.unwrap()
        });
        let (socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        let _peer = peer.await.unwrap();
        let (sink, _reader) = socket.split();
        let mut provider = BackendProvider::new();
        provider.ws_write = Some(Arc::new(Mutex::new(sink)));
        provider.is_closed.store(false, Ordering::SeqCst);
        provider.is_streaming = true;
        let session = ContinuationSession {
            connection_generation: 1,
            provider_session_id: "retained".into(),
        };
        provider.continuation.lock().unwrap().session = Some(session.clone());
        // No keepalive policy is not evidence of closure; paused is not required.
        for paused in [false, true] {
            provider.is_paused = paused;
            assert_eq!(provider.continuation_session(), Some(session.clone()));
        }
        provider.receiver_task = Some(tokio::spawn(async {}));
        while !provider.receiver_task.as_ref().unwrap().is_finished() {
            tokio::task::yield_now().await;
        }
        assert!(!provider.is_closed.load(Ordering::SeqCst));
        assert!(provider.continuation_session().is_none());
        assert_eq!(
            provider.continuation_lifecycle_session(),
            Some(session.clone())
        );
        provider.receiver_task = None;
        provider.ws_write = None;
        assert!(provider.continuation_session().is_none());
        assert_eq!(provider.continuation_lifecycle_session(), Some(session));
    }

    #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
    #[tokio::test]
    async fn native_e2e_ready_observation_requires_actual_ready_and_rejects_error_or_closed() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let (send, mut messages) = tokio::sync::mpsc::channel::<serde_json::Value>(4);
        let peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let _config = socket.next().await.unwrap().unwrap();
            while let Some(message) = messages.recv().await {
                socket
                    .send(Message::Text(message.to_string().into()))
                    .await
                    .unwrap();
            }
        });
        let mut provider = BackendProvider::new();
        let mut config = SttConfig::new(SttProviderType::Backend);
        config.backend_url = Some(url);
        config.backend_auth_token = Some("test-token".into());
        provider.initialize(&config).await.unwrap();
        provider
            .start_stream(
                Arc::new(|_| {}),
                Arc::new(|_| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
            )
            .await
            .unwrap();
        // This is exactly the point where the service is allowed to set Recording.
        assert_eq!(
            provider.native_e2e_transport_observation(),
            Some((false, true))
        );
        let mut boundary_held = false;
        let mut boundary = || {
            boundary_held = provider.continuation.try_lock().is_err();
        };
        assert_eq!(
            provider.native_e2e_transport_observation_at_boundary(&mut boundary),
            Some((false, true))
        );
        assert!(boundary_held);
        send.send(serde_json::json!({"type":"ready", "session_id":"r2-ready", "accepted_capabilities":[]})).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while provider.native_e2e_transport_observation() != Some((true, true)) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        send.send(
            serde_json::json!({"type":"error", "code":"test_error", "message":"test failure"}),
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !provider.continuation.lock().unwrap().native_e2e_error_seen {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            provider.native_e2e_transport_observation(),
            Some((false, true))
        );
        // A stale Ready bit must not overcome explicit closure either.
        provider.is_closed.store(true, Ordering::SeqCst);
        assert_eq!(
            provider.native_e2e_transport_observation(),
            Some((false, true))
        );
        provider.abort().await.unwrap();
        assert_eq!(
            provider.native_e2e_transport_observation(),
            Some((false, false))
        );
        peer.abort();
        let _ = peer.await;
    }

    /// Uses the production websocket sink; no substitute first-write implementation.
    #[tokio::test]
    async fn first_b_sink_wait_revokes_before_attempt_for_cancel_terminal_and_deadline() {
        for refusal in 0..4 {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("ws://{}", listener.local_addr().unwrap());
            let peer = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                accept_async(stream).await.unwrap()
            });
            let (socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
            let mut peer = peer.await.unwrap();
            let (sink, _reader) = socket.split();
            let sink = Arc::new(Mutex::new(sink));
            let held = sink.lock().await;
            let mut provider = BackendProvider::new();
            provider.ws_write = Some(sink.clone());
            provider.is_closed.store(false, Ordering::Release);
            let session = ContinuationSession {
                connection_generation: 1,
                provider_session_id: "sink-fence".into(),
            };
            {
                let mut state = provider.continuation.lock().unwrap();
                state.session = Some(session.clone());
                state.audio_gate = ContinuationAudioGate::First { pause_epoch: 1 };
            }
            let fence = crate::domain::ContinuationWriteFence::new(
                Default::default(),
                tokio::time::Instant::now()
                    + Duration::from_millis(if refusal == 2 { 20 } else { 1000 }),
            );
            let state = provider.continuation.clone();
            let closed = provider.is_closed.clone();
            let chunk = AudioChunk::new(vec![200; 100], 16000, 1);
            let write = provider.send_first_continuation_audio(&session, 1, &chunk, &fence);
            tokio::pin!(write);
            // Poll the production path into its socket-lock wait before revocation.
            assert!(tokio::time::timeout(Duration::from_millis(5), &mut write)
                .await
                .is_err());
            assert!(!fence.attempted.load(Ordering::Acquire));
            match refusal {
                0 => fence.cancelled.store(true, Ordering::Release),
                1 => state.lock().unwrap().audio_gate = ContinuationAudioGate::Paused,
                2 => tokio::time::sleep_until(fence.deadline).await,
                _ => {
                    let state = state.lock().unwrap();
                    assert!(state.session.is_some());
                    closed.store(true, Ordering::SeqCst);
                    state.changed.notify_waiters();
                }
            }
            drop(held);
            assert_eq!(
                write.await.unwrap(),
                crate::domain::ContinuationFirstWrite::NotStarted
            );
            assert!(!fence.attempted.load(Ordering::Acquire));
            assert!(tokio::time::timeout(Duration::from_millis(10), peer.next())
                .await
                .is_err());
        }
    }

    #[tokio::test]
    async fn control_recovery_clips_absolute_lifetime_and_retains_unknown_identity() {
        for operation in [
            ContinuationOperation::Continue { pause_epoch: 3 },
            ContinuationOperation::Pause {
                logical_run_id: 101,
            },
        ] {
            for lifetime_ms in [40, 3000] {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let url = format!("ws://{}", listener.local_addr().unwrap());
                let peer = tokio::spawn(async move {
                    let (stream, _) = listener.accept().await.unwrap();
                    accept_async(stream).await.unwrap()
                });
                let (socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
                let mut peer = peer.await.unwrap();
                let (sink, _reader) = socket.split();
                let mut provider = BackendProvider::new();
                provider.ws_write = Some(Arc::new(Mutex::new(sink)));
                provider.is_closed.store(false, Ordering::Release);
                let session = ContinuationSession {
                    connection_generation: 7,
                    provider_session_id: "lost-control".into(),
                };
                provider.continuation.lock().unwrap().session = Some(session.clone());
                let started = tokio::time::Instant::now();
                assert!(provider
                    .continuation_control(
                        &session,
                        operation.clone(),
                        started + Duration::from_millis(lifetime_ms)
                    )
                    .await
                    .is_err());
                assert!(started.elapsed() < Duration::from_millis(lifetime_ms + 500));
                assert!(provider.is_closed.load(Ordering::Acquire));
                assert_eq!(provider.continuation_lifecycle_session(), Some(session));
                let (id, retained_operation) = provider.continuation_operation_identity().unwrap();
                assert_eq!(id, "7-1");
                match (&retained_operation, &operation) {
                    (
                        ContinuationOperation::Continue {
                            pause_epoch: actual,
                        },
                        ContinuationOperation::Continue {
                            pause_epoch: expected,
                        },
                    ) => assert_eq!(actual, expected),
                    (
                        ContinuationOperation::Pause {
                            logical_run_id: actual,
                        },
                        ContinuationOperation::Pause {
                            logical_run_id: expected,
                        },
                    ) => assert_eq!(actual, expected),
                    _ => panic!("unknown operation identity changed"),
                }
                assert!(provider.continuation.lock().unwrap().waiters.is_empty());
                let message = tokio::time::timeout(Duration::from_secs(1), peer.next())
                    .await
                    .expect("expected control frame was not sent before the fixture deadline")
                    .expect("control peer closed before expected frame")
                    .expect("control frame receive failed");
                assert!(matches!(message, Message::Text(text) if text.contains("7-1")));
                if lifetime_ms > 750 {
                    let message = tokio::time::timeout(Duration::from_secs(1), peer.next())
                        .await
                        .expect("expected control frame was not sent before the fixture deadline")
                        .expect("control peer closed before expected frame")
                        .expect("control frame receive failed");
                    let Message::Text(text) = message else {
                        panic!("status must be a JSON text frame")
                    };
                    let status: serde_json::Value = serde_json::from_str(&text).unwrap();
                    assert_eq!(status["type"], "control_status");
                    assert_eq!(status["operation_request_id"], "7-1");
                    assert_eq!(status["query_id"], "q-7-1");
                }
                assert!(tokio::time::timeout(Duration::from_millis(10), peer.next())
                    .await
                    .is_err());
            }
        }
    }

    #[test]
    fn cumulative_server_not_started_accounts_only_unacked_range_without_replay_or_ack() {
        let mut ledger = DeliveryLedger::default();
        for bytes in [100, 200, 300] {
            let seq = ledger.issue();
            ledger.sent(seq, bytes);
        }
        assert!(ledger.ack(1));
        assert!(ledger.record_not_started(2, 2));
        assert_eq!(ledger.known_unsent_bytes(), 200);
        assert!(ledger.record_not_started(2, 3));
        assert_eq!(ledger.known_unsent_bytes(), 500);
        assert!(!ledger.record_not_started(2, 2));
        assert!(!ledger.record_not_started(1, 3));
        assert!(!ledger.record_not_started(2, 4));
        assert!(!ledger.ack(3));
        assert_eq!(ledger.progress.sent_bytes, 600);
        assert_eq!(ledger.progress.acked_bytes, 100);
    }

    #[tokio::test]
    async fn observed_continuation_terminal_preserves_failure_and_needs_no_new_finalize() {
        for reason in [FinalizeReason::Drained, FinalizeReason::Deadline] {
            let mut provider = BackendProvider::new();
            provider.is_streaming = true;
            provider.is_closed.store(true, Ordering::SeqCst);
            provider.continuation.lock().unwrap().session = Some(ContinuationSession {
                connection_generation: 1,
                provider_session_id: "terminal".into(),
            });
            *provider.finalize_report.lock().unwrap() = Some(ProviderFinalizeReport {
                reason,
                tail_evidence: crate::domain::TailEvidence::Unconfirmed,
                provider_release: crate::domain::ProviderRelease::Released,
                last_delivery_seq: 2,
                stable_snapshot: "retained stable".into(),
                error: None,
            });
            let stopped = provider.stop_stream().await;
            assert_eq!(stopped.is_ok(), reason == FinalizeReason::Drained);
            assert_eq!(
                provider.finalize_evidence().unwrap().stable_snapshot,
                "retained stable"
            );
            assert_eq!(provider.finalize_evidence().unwrap().reason, reason);
            assert!(provider.ws_write.is_none());
        }
    }

    #[tokio::test]
    async fn continuation_waiters_ignore_wrong_session_and_release_on_cancel() {
        let provider = BackendProvider::new();
        provider.continuation.lock().unwrap().session = Some(ContinuationSession {
            connection_generation: 9,
            provider_session_id: "current".into(),
        });
        let (mut rx, lease) = provider.control_waiter("9-1".into()).unwrap();
        let mut result = ContinuationControlResult {
            request_id: "9-1".into(),
            provider_session_id: "old".into(),
            pause_epoch: Some(1),
            decision: crate::domain::ControlDecision::Accepted,
            current_phase: crate::domain::ContinuationPhase::PausedReclaimable,
            eligible_now: true,
            reason: None,
            continue_window_ms: None,
        };
        provider
            .continuation
            .lock()
            .unwrap()
            .deliver(ControlReply::Mutation(result.clone()));
        assert!(matches!(
            rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        result.provider_session_id = "current".into();
        result.request_id = "9-stale".into();
        provider
            .continuation
            .lock()
            .unwrap()
            .deliver(ControlReply::Mutation(result.clone()));
        assert!(matches!(
            rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        result.request_id = "9-1".into();
        provider
            .continuation
            .lock()
            .unwrap()
            .deliver(ControlReply::Mutation(result));
        assert!(matches!(rx.await.unwrap(), ControlReply::Mutation(_)));
        drop(lease);
        let (_rx, lease) = provider.control_waiter("9-2".into()).unwrap();
        assert_eq!(provider.continuation.lock().unwrap().waiters.len(), 1);
        drop(lease);
        assert!(provider.continuation.lock().unwrap().waiters.is_empty());
    }

    #[test]
    fn continuation_waiter_capacity_is_bounded_and_fails_closed() {
        let provider = BackendProvider::new();
        provider.is_closed.store(false, Ordering::SeqCst);
        let leases: Vec<_> = (0..8)
            .map(|n| provider.control_waiter(n.to_string()).unwrap())
            .collect();
        assert!(provider.control_waiter("overflow".into()).is_err());
        assert!(provider.is_closed.load(Ordering::SeqCst));
        drop(leases);
        assert!(provider.continuation.lock().unwrap().waiters.is_empty());
    }

    #[tokio::test]
    async fn ready_metadata_precedes_first_stable_callback_without_projection() {
        use crate::presentation::commands::initial_continuation_target_eligible;
        let copy_only = initial_continuation_target_eligible(true, false, None);
        let unqualified_paste = initial_continuation_target_eligible(true, true, None);
        let no_delivery = initial_continuation_target_eligible(false, false, None);
        assert!(copy_only);
        assert!(!unqualified_paste);
        assert!(!no_delivery);
        for (continuation, eligible, opted_in) in [
            // Exercise the frozen policy qualification through actual Config/Ready.
            (true, copy_only, true),
            (true, copy_only, false),
            (true, unqualified_paste, true),
            (true, no_delivery, true),
            (false, true, true),
            (true, true, true),
            (true, false, true),
            (true, true, false),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("ws://{}", listener.local_addr().unwrap());
            let (release, held) = tokio::sync::oneshot::channel::<()>();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                let wire = ws.next().await.unwrap().unwrap();
                let wire: serde_json::Value =
                    serde_json::from_str(wire.to_text().unwrap()).unwrap();
                assert_eq!(
                    wire["capabilities"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|c| c == CAPABILITY_PAUSE_CONTINUE),
                    eligible && opted_in
                );
                let mut capabilities = vec![CAPABILITY_FINALIZE_OUTCOME];
                if continuation {
                    capabilities.push(CAPABILITY_PAUSE_CONTINUE);
                }
                ws.send(Message::Text(serde_json::json!({
                    "type": "ready", "session_id": "ordered", "accepted_capabilities": capabilities
                }).to_string().into())).await.unwrap();
                // No capture, coordinator, or projection observer gets to select
                // delivery mode between Ready and this first output callback.
                ws.send(Message::Text(
                    r#"{"type":"stable","delivery_seq":1,"text":"first"}"#.into(),
                ))
                .await
                .unwrap();
                let _ = held.await;
            });
            let mut config = SttConfig::new(SttProviderType::Backend);
            config.backend_url = Some(url);
            config.backend_auth_token = Some("fake-only".into());
            config.backend_streaming_provider = crate::domain::BackendStreamingProvider::ElevenLabs;
            let mut provider = BackendProvider::new();
            provider.continuation_opted_in = opted_in;
            config.continuation_target_eligible = eligible;
            provider.initialize(&config).await.unwrap();
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            provider
                .start_stream(
                    Arc::new(|_| {}),
                    Arc::new(move |t| {
                        tx.send(t).unwrap();
                    }),
                    Arc::new(|_| {}),
                    Arc::new(|_, _| {}),
                )
                .await
                .unwrap();
            let first = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(first.text, "first");
            assert!(first.completion_v1);
            assert_eq!(
                provider.continuation_delivery_mode(),
                Some(first.continuation_delivery)
            );
            assert_eq!(
                first.continuation_delivery,
                continuation && eligible && opted_in
            );
            let _ = release.send(());
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn continuation_fake_backend_preserves_socket_callbacks_and_ack_ledger() {
        for recover in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("ws://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                let mut audio = Vec::new();
                let mut controls = Vec::new();
                let mut seq = 0;
                let mut queries = Vec::new();
                while let Some(Ok(message)) = ws.next().await {
                    match message {
                        Message::Text(text) => {
                            let value: serde_json::Value = serde_json::from_str(&text).unwrap();
                            let kind = value["type"].as_str().unwrap();
                            if kind == "config" {
                                assert!(value["capabilities"]
                                    .as_array()
                                    .unwrap()
                                    .contains(&serde_json::json!(CAPABILITY_PAUSE_CONTINUE)));
                                ws.send(Message::Text(r#"{"type":"ready","session_id":"same","accepted_capabilities":["finalize_outcome_v1","el_pause_continue_v1"]}"#.into())).await.unwrap();
                            } else if kind == "pause" || kind == "continue" {
                                controls.push(value.clone());
                                let pause = kind == "pause";
                                let mut reply = serde_json::json!({
                                    "type": if pause { "pause_accepted" } else { "continue_result" },
                                    "request_id": value["request_id"], "provider_session_id":"same",
                                    "pause_epoch":1,"decision":"accepted", "eligible_now":true,"reason":null,
                                    "current_phase": if pause { "paused_reclaimable" } else { "active_awaiting_audio" }
                                });
                                if pause {
                                    reply["continue_window_ms"] = 5000.into();
                                }
                                if !recover {
                                    ws.send(Message::Text(reply.to_string().into()))
                                        .await
                                        .unwrap();
                                }
                                if !pause {
                                    ws.send(Message::Text(
                                        r#"{"type":"stable","delivery_seq":2,"text":"late A"}"#
                                            .into(),
                                    ))
                                    .await
                                    .unwrap();
                                }
                            } else if kind == "control_status" {
                                assert!(recover);
                                let operation = controls.last().unwrap();
                                assert_eq!(value["operation_request_id"], operation["request_id"]);
                                let pause = operation["type"] == "pause";
                                // Before either recovery decision there is only A PCM on the socket.
                                assert_eq!(audio.len(), 200);
                                queries.push(value.clone());
                                ws.send(Message::Text(serde_json::json!({
                                "type":"control_status_result", "query_id":value["query_id"],
                                "operation_request_id":value["operation_request_id"],
                                "provider_session_id":"same", "pause_epoch":1,
                                "original_decision":"accepted", "eligible_now":true,
                                "current_phase":if pause { "paused_reclaimable" } else { "active_awaiting_audio" }
                            }).to_string().into())).await.unwrap();
                            } else if kind == "close" {
                                break;
                            }
                        }
                        Message::Binary(bytes) => {
                            audio.extend(bytes);
                            seq += 1;
                            ws.send(Message::Text(
                                serde_json::json!({"type":"ack","seq":seq})
                                    .to_string()
                                    .into(),
                            ))
                            .await
                            .unwrap();
                            if seq == 1 {
                                ws.send(Message::Text(
                                    r#"{"type":"stable","delivery_seq":1,"text":"A"}"#.into(),
                                ))
                                .await
                                .unwrap();
                            }
                        }
                        Message::Close(_) => break,
                        _ => {}
                    }
                }
                (audio, controls, queries)
            });
            let mut config = SttConfig::new(SttProviderType::Backend);
            config.backend_url = Some(url);
            config.backend_auth_token = Some("fake-only".into());
            config.backend_streaming_provider = crate::domain::BackendStreamingProvider::ElevenLabs;
            let mut provider = BackendProvider::new();
            provider.continuation_opted_in = true;
            config.continuation_target_eligible = true;
            provider.initialize(&config).await.unwrap();
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            provider
                .start_stream(
                    Arc::new(|_| {}),
                    Arc::new(move |t| {
                        tx.send(t).unwrap();
                    }),
                    Arc::new(|_| {}),
                    Arc::new(|_, _| {}),
                )
                .await
                .unwrap();
            let session = tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if let Some(session) = provider.continuation_session() {
                        break session;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            provider
                .send_audio(&AudioChunk::new(vec![100; 100], 16000, 1))
                .await
                .unwrap();
            let paused = provider
                .continuation_control(
                    &session,
                    ContinuationOperation::Pause { logical_run_id: 10 },
                    tokio::time::Instant::now() + Duration::from_secs(2),
                )
                .await
                .unwrap();
            assert_eq!(paused.pause_epoch, Some(1));
            assert_eq!(paused.continue_window_ms, (!recover).then_some(5000));
            let sent_a = provider.audio_delivery_progress().unwrap().sent_bytes;
            assert_eq!(sent_a, 200);
            let accepted = provider
                .continuation_control(
                    &session,
                    ContinuationOperation::Continue { pause_epoch: 1 },
                    tokio::time::Instant::now() + Duration::from_secs(2),
                )
                .await
                .unwrap();
            assert!(accepted.eligible_now);
            assert_eq!(
                provider.audio_delivery_progress().unwrap().sent_bytes,
                sent_a
            );
            provider
                .send_first_continuation_audio(
                    &session,
                    1,
                    &AudioChunk::new(vec![200; 100], 16000, 1),
                    &crate::domain::ContinuationWriteFence::new(
                        Default::default(),
                        tokio::time::Instant::now() + Duration::from_secs(2),
                    ),
                )
                .await
                .unwrap();
            provider.flush_pending_audio_batch("test B").await.unwrap();
            tokio::time::timeout(Duration::from_secs(1), async {
                while provider.audio_delivery_progress().unwrap().acked_bytes != 400 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(rx.recv().await.unwrap().text, "A");
            assert_eq!(rx.recv().await.unwrap().text, "late A");
            assert_eq!(provider.continuation_session(), Some(session));
            provider.abort().await.unwrap();
            let (audio, controls, queries) = server.await.unwrap();
            assert_eq!(queries.len(), if recover { 2 } else { 0 });
            if recover {
                assert_ne!(
                    queries[0]["operation_request_id"],
                    queries[1]["operation_request_id"]
                );
            }
            let expected: Vec<u8> = [vec![100i16; 100], vec![200i16; 100]]
                .concat()
                .into_iter()
                .flat_map(i16::to_le_bytes)
                .collect();
            assert_eq!(audio, expected);
            assert_eq!(controls.len(), 2);
            assert_eq!(controls[0]["control_seq"], 1);
            assert_eq!(controls[1]["control_seq"], 2);
        }
    }

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
    fn catchup_rollback_preserves_negotiation_and_limits_pacing_to_realtime() {
        let mut provider = BackendProvider::new();
        provider.outcome_negotiated.store(true, Ordering::SeqCst);
        assert_eq!(provider.preferred_audio_batch_samples(), Some(4800));
        assert_eq!(
            provider.negotiated_audio_interval(9600),
            Duration::from_millis(75)
        );
        provider.el_catchup_disabled = true;
        assert_eq!(provider.preferred_audio_batch_samples(), None);
        assert_eq!(
            provider.negotiated_audio_interval(9600),
            Duration::from_millis(300)
        );
        assert_eq!(
            provider.negotiated_audio_interval(960),
            Duration::from_millis(30)
        );
        assert!(provider.outcome_negotiated.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn initialize_trims_backend_url_and_auth_token() {
        let mut provider = BackendProvider::new();
        let mut config = SttConfig::new(SttProviderType::Backend);
        config.backend_url = Some("  wss://example.test  \n".to_string());
        config.backend_auth_token = Some("  test-token  \n".to_string());

        provider
            .initialize(&config)
            .await
            .expect("initialize provider");

        assert_eq!(provider.backend_url, "wss://example.test");
        assert_eq!(provider.auth_token.as_deref(), Some("test-token"));
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

    #[test]
    fn test_backend_provider_uses_configured_backend_streaming_provider() {
        let mut config = SttConfig::new(SttProviderType::Backend);
        config.backend_streaming_provider = crate::domain::BackendStreamingProvider::ElevenLabs;

        assert_eq!(backend_streaming_provider_name(&config), "elevenlabs");

        config.backend_streaming_provider = crate::domain::BackendStreamingProvider::Deepgram;
        assert_eq!(backend_streaming_provider_name(&config), "deepgram");
    }

    #[test]
    fn test_legacy_direct_provider_mapping_stays_backward_compatible() {
        let config = SttConfig::new(SttProviderType::Deepgram);
        assert_eq!(backend_streaming_provider_name(&config), "deepgram");

        let config = SttConfig::new(SttProviderType::AssemblyAI);
        assert_eq!(backend_streaming_provider_name(&config), "assemblyai");
    }

    async fn spawn_config_capture_server() -> (String, JoinHandle<serde_json::Value>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local websocket listener");
        let addr = listener.local_addr().expect("listener addr");

        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept websocket tcp");
            let mut ws = accept_async(stream).await.expect("accept websocket");

            while let Some(next) = ws.next().await {
                let msg = match next {
                    Ok(message) => message,
                    Err(tokio_tungstenite::tungstenite::Error::Protocol(
                        tokio_tungstenite::tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
                    )) => break,
                    Err(error) => panic!("websocket message: {error}"),
                };
                if let Message::Text(text) = msg {
                    let value: serde_json::Value =
                        serde_json::from_str(&text).expect("config json");
                    if value.get("type").and_then(|v| v.as_str()) == Some("config") {
                        return value;
                    }
                }
            }

            panic!("BackendProvider did not send Config message");
        });

        (format!("ws://{addr}"), task)
    }

    #[derive(Debug)]
    struct LifecycleCapture {
        config: serde_json::Value,
        binary_lengths: Vec<usize>,
        saw_finalize: bool,
    }

    async fn spawn_lifecycle_mock_backend() -> (String, JoinHandle<LifecycleCapture>) {
        spawn_lifecycle_mock_backend_with_finalize_delay(Duration::ZERO).await
    }

    async fn spawn_lifecycle_mock_backend_with_finalize_delay(
        finalize_delay: Duration,
    ) -> (String, JoinHandle<LifecycleCapture>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local websocket listener");
        let addr = listener.local_addr().expect("listener addr");

        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept websocket tcp");
            let mut ws = accept_async(stream).await.expect("accept websocket");
            let mut config: Option<serde_json::Value> = None;
            let mut binary_lengths = Vec::new();
            let mut saw_finalize = false;

            while let Some(next) = ws.next().await {
                let msg = match next {
                    Ok(message) => message,
                    Err(tokio_tungstenite::tungstenite::Error::Protocol(
                        tokio_tungstenite::tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
                    )) => break,
                    Err(error) => panic!("websocket message: {error}"),
                };
                match msg {
                    Message::Text(text) => {
                        let value: serde_json::Value =
                            serde_json::from_str(&text).expect("client json message");
                        match value.get("type").and_then(|v| v.as_str()) {
                            Some("config") => {
                                config = Some(value);
                                ws.send(Message::Text(
                                    r#"{"type":"ready","session_id":"mock-session"}"#.to_string(),
                                ))
                                .await
                                .expect("send ready");
                            }
                            Some("finalize") => {
                                saw_finalize = true;
                                tokio::time::sleep(finalize_delay).await;
                                ws.send(Message::Text(
                                    r#"{"type":"finalize_complete","status":"drained","saw_result":true}"#
                                        .to_string(),
                                ))
                                .await
                                .expect("send finalize_complete");
                            }
                            Some("close") => break,
                            _ => {}
                        }
                    }
                    Message::Binary(bytes) => {
                        binary_lengths.push(bytes.len());
                        let seq = binary_lengths.len();
                        ws.send(Message::Text(format!(r#"{{"type":"ack","seq":{seq}}}"#)))
                            .await
                            .expect("send ack");
                        ws.send(Message::Text(
                            r#"{"type":"partial","text":"hello","confidence":0.52,"is_segment_final":false,"start_ms":0,"duration_ms":300}"#
                                .to_string(),
                        ))
                        .await
                        .expect("send partial");
                        ws.send(Message::Text(
                            r#"{"type":"usage_update","seconds_used":0.3,"seconds_remaining_plan":59.7,"seconds_remaining_total":59.7}"#
                                .to_string(),
                        ))
                        .await
                        .expect("send usage_update");
                        ws.send(Message::Text(
                            r#"{"type":"final","text":"hello world","confidence":0.91,"start_ms":0,"duration_ms":420}"#
                                .to_string(),
                        ))
                        .await
                        .expect("send final");
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }

            LifecycleCapture {
                config: config.expect("client config"),
                binary_lengths,
                saw_finalize,
            }
        });

        (format!("ws://{addr}"), task)
    }

    async fn spawn_finalize_on_stop_mock_backend() -> (String, JoinHandle<LifecycleCapture>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local websocket listener");
        let addr = listener.local_addr().expect("listener addr");

        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept websocket tcp");
            let mut ws = accept_async(stream).await.expect("accept websocket");
            let mut config: Option<serde_json::Value> = None;
            let mut binary_lengths = Vec::new();
            let mut saw_finalize = false;

            while let Some(next) = ws.next().await {
                let msg = match next {
                    Ok(message) => message,
                    Err(tokio_tungstenite::tungstenite::Error::Protocol(
                        tokio_tungstenite::tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
                    )) => break,
                    Err(error) => panic!("websocket message: {error}"),
                };
                match msg {
                    Message::Text(text) => {
                        let value: serde_json::Value =
                            serde_json::from_str(&text).expect("client json message");
                        match value.get("type").and_then(|v| v.as_str()) {
                            Some("config") => {
                                config = Some(value);
                                ws.send(Message::Text(
                                    r#"{"type":"ready","session_id":"mock-session"}"#.to_string(),
                                ))
                                .await
                                .expect("send ready");
                            }
                            Some("finalize") => {
                                saw_finalize = true;
                                ws.send(Message::Text(
                                    r#"{"type":"final","text":"late final","confidence":0.93,"start_ms":0,"duration_ms":360}"#
                                        .to_string(),
                                ))
                                .await
                                .expect("send late final");
                                ws.send(Message::Text(
                                    r#"{"type":"finalize_complete","status":"drained","saw_result":true}"#
                                        .to_string(),
                                ))
                                .await
                                .expect("send finalize_complete");
                            }
                            Some("close") => break,
                            _ => {}
                        }
                    }
                    Message::Binary(bytes) => {
                        binary_lengths.push(bytes.len());
                        let seq = binary_lengths.len();
                        ws.send(Message::Text(format!(r#"{{"type":"ack","seq":{seq}}}"#)))
                            .await
                            .expect("send ack");
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }

            LifecycleCapture {
                config: config.expect("client config"),
                binary_lengths,
                saw_finalize,
            }
        });

        (format!("ws://{addr}"), task)
    }

    async fn spawn_finalize_ack_before_late_final_mock_backend(
    ) -> (String, JoinHandle<LifecycleCapture>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local websocket listener");
        let addr = listener.local_addr().expect("listener addr");

        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept websocket tcp");
            let mut ws = accept_async(stream).await.expect("accept websocket");
            let mut config: Option<serde_json::Value> = None;
            let mut binary_lengths = Vec::new();
            let mut saw_finalize = false;

            while let Some(next) = ws.next().await {
                let msg = match next {
                    Ok(message) => message,
                    Err(tokio_tungstenite::tungstenite::Error::Protocol(
                        tokio_tungstenite::tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
                    )) => break,
                    Err(error) => panic!("websocket message: {error}"),
                };
                match msg {
                    Message::Text(text) => {
                        let value: serde_json::Value =
                            serde_json::from_str(&text).expect("client json message");
                        match value.get("type").and_then(|v| v.as_str()) {
                            Some("config") => {
                                config = Some(value);
                                ws.send(Message::Text(
                                    r#"{"type":"ready","session_id":"mock-session"}"#.to_string(),
                                ))
                                .await
                                .expect("send ready");
                            }
                            Some("finalize") => {
                                saw_finalize = true;
                                ws.send(Message::Text(
                                    r#"{"type":"finalize_complete","status":"flushed","saw_result":true}"#
                                        .to_string(),
                                ))
                                .await
                                .expect("send finalize_complete");
                                tokio::time::sleep(Duration::from_millis(50)).await;
                                let _ = ws
                                    .send(Message::Text(
                                        r#"{"type":"final","text":"post ack final","confidence":0.94,"start_ms":0,"duration_ms":360}"#
                                            .to_string(),
                                    ))
                                    .await;
                            }
                            Some("close") => break,
                            _ => {}
                        }
                    }
                    Message::Binary(bytes) => {
                        binary_lengths.push(bytes.len());
                        let seq = binary_lengths.len();
                        ws.send(Message::Text(format!(r#"{{"type":"ack","seq":{seq}}}"#)))
                            .await
                            .expect("send ack");
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }

            LifecycleCapture {
                config: config.expect("client config"),
                binary_lengths,
                saw_finalize,
            }
        });

        (format!("ws://{addr}"), task)
    }

    async fn spawn_finalize_timeout_mock_backend() -> (String, JoinHandle<bool>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local websocket listener");
        let addr = listener.local_addr().expect("listener addr");

        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept websocket tcp");
            let mut ws = accept_async(stream).await.expect("accept websocket");
            let mut saw_finalize = false;
            let mut sequence = 0usize;

            while let Some(next) = ws.next().await {
                let msg = match next {
                    Ok(message) => message,
                    Err(tokio_tungstenite::tungstenite::Error::Protocol(
                        tokio_tungstenite::tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
                    )) => break,
                    Err(error) => panic!("websocket message: {error}"),
                };
                match msg {
                    Message::Text(text) => {
                        let value: serde_json::Value =
                            serde_json::from_str(&text).expect("client json message");
                        match value.get("type").and_then(|value| value.as_str()) {
                            Some("config") => {
                                ws.send(Message::Text(
                                    r#"{"type":"ready","session_id":"mock-session"}"#.to_string(),
                                ))
                                .await
                                .expect("send ready");
                            }
                            Some("finalize") => saw_finalize = true,
                            Some("close") => break,
                            _ => {}
                        }
                    }
                    Message::Binary(_) => {
                        sequence += 1;
                        ws.send(Message::Text(format!(
                            r#"{{"type":"ack","seq":{sequence}}}"#
                        )))
                        .await
                        .expect("send ack");
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }

            saw_finalize
        });

        (format!("ws://{addr}"), task)
    }

    async fn spawn_finalize_disconnect_mock_backend() -> (String, JoinHandle<bool>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local websocket listener");
        let addr = listener.local_addr().expect("listener addr");

        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept websocket tcp");
            let mut ws = accept_async(stream).await.expect("accept websocket");
            let mut saw_finalize = false;
            let mut sequence = 0usize;

            while let Some(next) = ws.next().await {
                let msg = match next {
                    Ok(message) => message,
                    Err(tokio_tungstenite::tungstenite::Error::Protocol(
                        tokio_tungstenite::tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
                    )) => break,
                    Err(error) => panic!("websocket message: {error}"),
                };
                match msg {
                    Message::Text(text) => {
                        let value: serde_json::Value =
                            serde_json::from_str(&text).expect("client json message");
                        match value.get("type").and_then(|value| value.as_str()) {
                            Some("config") => {
                                ws.send(Message::Text(
                                    r#"{"type":"ready","session_id":"mock-session"}"#.to_string(),
                                ))
                                .await
                                .expect("send ready");
                            }
                            Some("finalize") => {
                                saw_finalize = true;
                                ws.close(None)
                                    .await
                                    .expect("close websocket during finalize");
                                break;
                            }
                            Some("close") => break,
                            _ => {}
                        }
                    }
                    Message::Binary(_) => {
                        sequence += 1;
                        ws.send(Message::Text(format!(
                            r#"{{"type":"ack","seq":{sequence}}}"#
                        )))
                        .await
                        .expect("send ack");
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }

            saw_finalize
        });

        (format!("ws://{addr}"), task)
    }

    #[tokio::test]
    async fn test_backend_provider_sends_selected_streaming_provider_in_config_message() {
        for (selected, expected) in [
            (
                crate::domain::BackendStreamingProvider::Deepgram,
                "deepgram",
            ),
            (
                crate::domain::BackendStreamingProvider::ElevenLabs,
                "elevenlabs",
            ),
        ] {
            let (backend_url, config_task) = spawn_config_capture_server().await;
            let mut config = SttConfig::new(SttProviderType::Backend);
            config.backend_url = Some(backend_url);
            config.backend_auth_token = Some("test-token".to_string());
            config.backend_streaming_provider = selected;
            config.language = "en".to_string();
            config.streaming_keyterms = Some("VoicetextAI, API".to_string());

            let mut provider = BackendProvider::new();
            provider.initialize(&config).await.unwrap();
            provider
                .start_stream(
                    Arc::new(|_| {}),
                    Arc::new(|_| {}),
                    Arc::new(|_| {}),
                    Arc::new(|_, _| {}),
                )
                .await
                .unwrap();

            let config_msg = tokio::time::timeout(Duration::from_secs(3), config_task)
                .await
                .expect("config capture timeout")
                .expect("config capture task");

            let _ = provider.abort().await;

            assert_eq!(config_msg["type"], "config");
            assert_eq!(config_msg["provider"], expected);
            assert_eq!(config_msg["language"], "en");
            assert_eq!(config_msg["sample_rate"], 16000);
            assert_eq!(config_msg["encoding"], "pcm_s16le");
            assert_eq!(config_msg["keyterms"][0], "VoicetextAI");
            assert_eq!(config_msg["keyterms"][1], "API");
        }
    }

    #[tokio::test]
    async fn test_backend_provider_elevenlabs_full_mock_stream_lifecycle() {
        let (backend_url, server_task) = spawn_lifecycle_mock_backend().await;
        let mut config = SttConfig::new(SttProviderType::Backend);
        config.backend_url = Some(backend_url);
        config.backend_auth_token = Some("test-token".to_string());
        config.backend_streaming_provider = crate::domain::BackendStreamingProvider::ElevenLabs;
        config.language = "en".to_string();
        config.streaming_keyterms = Some("VoicetextAI, ElevenLabs".to_string());

        let (quality_tx, mut quality_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let (partial_tx, mut partial_rx) = tokio::sync::mpsc::unbounded_channel::<Transcription>();
        let (final_tx, mut final_rx) = tokio::sync::mpsc::unbounded_channel::<Transcription>();
        let (usage_tx, mut usage_rx) = tokio::sync::mpsc::unbounded_channel::<(f32, f32)>();

        let mut provider = BackendProvider::new();
        provider.set_usage_callback(Arc::new(move |used, remaining| {
            let _ = usage_tx.send((used, remaining));
        }));
        provider.initialize(&config).await.unwrap();
        provider
            .start_stream(
                Arc::new(move |t| {
                    let _ = partial_tx.send(t);
                }),
                Arc::new(move |t| {
                    let _ = final_tx.send(t);
                }),
                Arc::new(|err| panic!("unexpected backend provider error: {err}")),
                Arc::new(move |quality, _reason| {
                    let _ = quality_tx.send(quality);
                }),
            )
            .await
            .unwrap();

        let quality = tokio::time::timeout(Duration::from_secs(3), quality_rx.recv())
            .await
            .expect("quality callback timeout")
            .expect("quality callback");
        assert_eq!(quality, "Good");

        provider
            .send_audio(&AudioChunk::new(vec![1000; 1024], 16_000, 1))
            .await
            .unwrap();

        let partial = tokio::time::timeout(Duration::from_secs(3), partial_rx.recv())
            .await
            .expect("partial callback timeout")
            .expect("partial callback");
        assert_eq!(partial.text, "hello");
        assert!(!partial.is_final);
        assert_eq!(partial.confidence, Some(0.52));
        assert_eq!(partial.duration, 0.3);

        let usage = tokio::time::timeout(Duration::from_secs(3), usage_rx.recv())
            .await
            .expect("usage callback timeout")
            .expect("usage callback");
        assert!((usage.0 - 0.3).abs() < 0.001);
        assert!((usage.1 - 59.7).abs() < 0.001);

        let final_result = tokio::time::timeout(Duration::from_secs(3), final_rx.recv())
            .await
            .expect("final callback timeout")
            .expect("final callback");
        assert_eq!(final_result.text, "hello world");
        assert!(final_result.is_final);
        assert_eq!(final_result.confidence, Some(0.91));
        assert_eq!(final_result.duration, 0.42);

        provider.pause_stream().await.unwrap();
        assert!(provider.is_connection_alive());
        provider.abort().await.unwrap();

        let capture = tokio::time::timeout(Duration::from_secs(3), server_task)
            .await
            .expect("mock backend timeout")
            .expect("mock backend task");

        assert_eq!(capture.config["type"], "config");
        assert_eq!(capture.config["provider"], "elevenlabs");
        assert_eq!(capture.config["language"], "en");
        assert_eq!(capture.config["sample_rate"], 16000);
        assert_eq!(capture.config["encoding"], "pcm_s16le");
        assert_eq!(capture.config["keyterms"][0], "VoicetextAI");
        assert_eq!(capture.config["keyterms"][1], "ElevenLabs");
        assert_eq!(capture.config["capabilities"][0], CAPABILITY_FINALIZE_ACK);
        assert_eq!(capture.binary_lengths, vec![1920, 128]);
        assert_eq!(capture.binary_lengths.iter().sum::<usize>(), 1024 * 2);
        assert!(capture.saw_finalize);
    }

    #[tokio::test]
    async fn backend_provider_resumes_same_socket_for_second_transcription_session() {
        let (backend_url, server_task) = spawn_lifecycle_mock_backend().await;
        let mut config = SttConfig::new(SttProviderType::Backend);
        config.backend_url = Some(backend_url);
        config.backend_auth_token = Some("test-token".to_string());
        config.backend_streaming_provider = crate::domain::BackendStreamingProvider::Deepgram;

        let (first_final_tx, mut first_final_rx) =
            tokio::sync::mpsc::unbounded_channel::<Transcription>();
        let mut provider = BackendProvider::new();
        provider.initialize(&config).await.unwrap();
        provider
            .start_stream(
                Arc::new(|_| {}),
                Arc::new(move |transcription| {
                    let _ = first_final_tx.send(transcription);
                }),
                Arc::new(|error| panic!("unexpected first-session error: {error}")),
                Arc::new(|_, _| {}),
            )
            .await
            .unwrap();

        provider
            .send_audio(&AudioChunk::new(vec![1000; 960], 16_000, 1))
            .await
            .unwrap();
        let first_final = tokio::time::timeout(Duration::from_secs(3), first_final_rx.recv())
            .await
            .expect("first final timeout")
            .expect("first final callback");
        assert_eq!(first_final.text, "hello world");

        provider.pause_stream().await.unwrap();
        assert!(provider.is_connection_alive());

        let (second_final_tx, mut second_final_rx) =
            tokio::sync::mpsc::unbounded_channel::<Transcription>();
        provider
            .resume_stream(
                Arc::new(|_| {}),
                Arc::new(move |transcription| {
                    let _ = second_final_tx.send(transcription);
                }),
                Arc::new(|error| panic!("unexpected second-session error: {error}")),
                Arc::new(|_, _| {}),
            )
            .await
            .unwrap();
        provider
            .send_audio(&AudioChunk::new(vec![2000; 960], 16_000, 1))
            .await
            .unwrap();

        let second_final = tokio::time::timeout(Duration::from_secs(3), second_final_rx.recv())
            .await
            .expect("second final timeout")
            .expect("second final callback");
        assert_eq!(second_final.text, "hello world");
        assert!(first_final_rx.try_recv().is_err());

        provider.pause_stream().await.unwrap();
        assert!(provider.is_connection_alive());
        provider.abort().await.unwrap();

        let capture = tokio::time::timeout(Duration::from_secs(3), server_task)
            .await
            .expect("mock backend timeout")
            .expect("mock backend task");
        assert_eq!(capture.binary_lengths, vec![1920, 1920]);
        assert!(capture.saw_finalize);
    }

    #[tokio::test]
    async fn backend_provider_accepts_provider_bounded_delayed_finalize_ack() {
        let (backend_url, server_task) =
            spawn_lifecycle_mock_backend_with_finalize_delay(Duration::from_millis(2_100)).await;
        let mut config = SttConfig::new(SttProviderType::Backend);
        config.backend_url = Some(backend_url);
        config.backend_auth_token = Some("test-token".to_string());
        config.backend_streaming_provider = crate::domain::BackendStreamingProvider::ElevenLabs;

        let mut provider = BackendProvider::new();
        provider.initialize(&config).await.unwrap();
        provider
            .start_stream(
                Arc::new(|_| {}),
                Arc::new(|_| {}),
                Arc::new(|error| panic!("unexpected provider error: {error}")),
                Arc::new(|_, _| {}),
            )
            .await
            .unwrap();
        provider
            .send_audio(&AudioChunk::new(vec![1000; 960], 16_000, 1))
            .await
            .unwrap();

        provider.pause_stream().await.unwrap();
        assert!(provider.is_connection_alive());
        provider.abort().await.unwrap();

        let capture = server_task.await.unwrap();
        assert!(capture.saw_finalize);
    }

    #[tokio::test]
    async fn backend_provider_does_not_reuse_socket_without_finalize_ack() {
        let (backend_url, server_task) = spawn_finalize_timeout_mock_backend().await;
        let mut config = SttConfig::new(SttProviderType::Backend);
        config.backend_url = Some(backend_url);
        config.backend_auth_token = Some("test-token".to_string());

        let mut provider = BackendProvider::new();
        provider.finalize_drain_ack_timeout = Duration::from_millis(50);
        provider.initialize(&config).await.unwrap();
        provider
            .start_stream(
                Arc::new(|_| {}),
                Arc::new(|_| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
            )
            .await
            .unwrap();
        provider
            .send_audio(&AudioChunk::new(vec![1000; 960], 16_000, 1))
            .await
            .unwrap();

        let error = provider
            .pause_stream()
            .await
            .expect_err("pause must fail closed without finalize acknowledgement");
        assert!(error.to_string().contains("timed out"), "error={error}");
        assert!(!provider.is_connection_alive());
        provider.abort().await.unwrap();

        assert!(tokio::time::timeout(Duration::from_secs(3), server_task)
            .await
            .expect("mock backend timeout")
            .expect("mock backend task"));
    }

    #[tokio::test]
    async fn backend_provider_fails_immediately_when_transport_closes_during_finalize() {
        let (backend_url, server_task) = spawn_finalize_disconnect_mock_backend().await;
        let mut config = SttConfig::new(SttProviderType::Backend);
        config.backend_url = Some(backend_url);
        config.backend_auth_token = Some("test-token".to_string());

        let mut provider = BackendProvider::new();
        provider.initialize(&config).await.unwrap();
        provider
            .start_stream(
                Arc::new(|_| {}),
                Arc::new(|_| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
            )
            .await
            .unwrap();
        provider
            .send_audio(&AudioChunk::new(vec![1000; 960], 16_000, 1))
            .await
            .unwrap();

        let error = tokio::time::timeout(Duration::from_millis(500), provider.pause_stream())
            .await
            .expect("transport close must wake finalize waiter")
            .expect_err("pause must fail closed after transport close");
        assert!(
            error.to_string().contains("waiter dropped"),
            "error={error}"
        );
        assert!(!provider.is_connection_alive());
        provider.abort().await.unwrap();

        assert!(server_task.await.unwrap());
    }

    #[tokio::test]
    async fn panicking_partial_callback_does_not_kill_backend_receiver() {
        let (backend_url, server_task) = spawn_lifecycle_mock_backend().await;
        let mut config = SttConfig::new(SttProviderType::Backend);
        config.backend_url = Some(backend_url);
        config.backend_auth_token = Some("test-token".to_string());
        config.language = "en".to_string();

        let (final_tx, mut final_rx) = tokio::sync::mpsc::unbounded_channel::<Transcription>();
        let mut provider = BackendProvider::new();
        provider.initialize(&config).await.unwrap();
        provider
            .start_stream(
                Arc::new(|_| panic!("simulated partial callback panic")),
                Arc::new(move |transcription| {
                    let _ = final_tx.send(transcription);
                }),
                Arc::new(|error| panic!("unexpected backend provider error: {error}")),
                Arc::new(|_, _| {}),
            )
            .await
            .unwrap();

        provider
            .send_audio(&AudioChunk::new(vec![1000; 480], 16_000, 1))
            .await
            .unwrap();
        let final_result = tokio::time::timeout(Duration::from_secs(3), final_rx.recv())
            .await
            .expect("final callback timeout")
            .expect("final callback");
        assert_eq!(final_result.text, "hello world");

        provider.abort().await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), server_task)
            .await
            .expect("mock backend timeout")
            .expect("mock backend task");
    }

    #[tokio::test]
    async fn stop_stream_waits_for_finalize_drain_before_closing_receiver() {
        let (backend_url, server_task) = spawn_finalize_on_stop_mock_backend().await;
        let mut config = SttConfig::new(SttProviderType::Backend);
        config.backend_url = Some(backend_url);
        config.backend_auth_token = Some("test-token".to_string());
        config.backend_streaming_provider = crate::domain::BackendStreamingProvider::Deepgram;
        config.language = "en".to_string();

        let (final_tx, mut final_rx) = tokio::sync::mpsc::unbounded_channel::<Transcription>();
        let mut provider = BackendProvider::new();
        provider.initialize(&config).await.unwrap();
        provider
            .start_stream(
                Arc::new(|_| {}),
                Arc::new(move |t| {
                    let _ = final_tx.send(t);
                }),
                Arc::new(|err| panic!("unexpected backend provider error: {err}")),
                Arc::new(|_, _| {}),
            )
            .await
            .unwrap();

        provider
            .send_audio(&AudioChunk::new(vec![1000; 1024], 16_000, 1))
            .await
            .unwrap();
        provider.stop_stream().await.unwrap();

        let final_result = tokio::time::timeout(Duration::from_secs(3), final_rx.recv())
            .await
            .expect("final callback timeout")
            .expect("final callback");
        assert_eq!(final_result.text, "late final");
        assert!(final_result.is_final);

        let capture = tokio::time::timeout(Duration::from_secs(3), server_task)
            .await
            .expect("mock backend timeout")
            .expect("mock backend task");
        assert_eq!(capture.binary_lengths, vec![1920, 128]);
        assert_eq!(capture.binary_lengths.iter().sum::<usize>(), 1024 * 2);
        assert!(capture.saw_finalize);
    }

    #[tokio::test]
    async fn stop_stream_waits_for_post_ack_finalize_text_before_closing_receiver() {
        let (backend_url, server_task) = spawn_finalize_ack_before_late_final_mock_backend().await;
        let mut config = SttConfig::new(SttProviderType::Backend);
        config.backend_url = Some(backend_url);
        config.backend_auth_token = Some("test-token".to_string());
        config.backend_streaming_provider = crate::domain::BackendStreamingProvider::Deepgram;
        config.language = "en".to_string();

        let (final_tx, mut final_rx) = tokio::sync::mpsc::unbounded_channel::<Transcription>();
        let mut provider = BackendProvider::new();
        provider.initialize(&config).await.unwrap();
        provider
            .start_stream(
                Arc::new(|_| {}),
                Arc::new(move |t| {
                    let _ = final_tx.send(t);
                }),
                Arc::new(|err| panic!("unexpected backend provider error: {err}")),
                Arc::new(|_, _| {}),
            )
            .await
            .unwrap();

        provider
            .send_audio(&AudioChunk::new(vec![1000; 480], 16_000, 1))
            .await
            .unwrap();
        provider.stop_stream().await.unwrap();

        let final_result = tokio::time::timeout(Duration::from_secs(3), final_rx.recv())
            .await
            .expect("post-ack final callback timeout")
            .expect("post-ack final callback");
        assert_eq!(final_result.text, "post ack final");
        assert!(final_result.is_final);

        let capture = tokio::time::timeout(Duration::from_secs(3), server_task)
            .await
            .expect("mock backend timeout")
            .expect("mock backend task");
        assert_eq!(capture.binary_lengths, vec![960]);
        assert!(capture.saw_finalize);
    }

    #[test]
    fn test_server_error_code_categories() {
        assert_eq!(
            category_for_server_error("RATE_LIMIT_EXCEEDED"),
            SttConnectionCategory::RateLimited
        );
        assert_eq!(
            category_for_server_error("TOO_MANY_SESSIONS"),
            SttConnectionCategory::RateLimited
        );
        assert_eq!(
            category_for_server_error("PROVIDER_UNAVAILABLE"),
            SttConnectionCategory::ServerUnavailable
        );
        assert_eq!(
            category_for_server_error("INTERNAL_ERROR"),
            SttConnectionCategory::ServerError
        );
        assert_eq!(
            category_for_server_error("PROVIDER_QUOTA_EXCEEDED"),
            SttConnectionCategory::ProviderQuotaExceeded
        );
    }

    #[test]
    fn test_only_fatal_server_errors_suppress_following_close() {
        assert_eq!(
            category_for_server_error("LIMIT_EXCEEDED"),
            SttConnectionCategory::LimitExceeded
        );
        assert!(server_error_closes_stream("LIMIT_EXCEEDED"));
        assert!(server_error_closes_stream("RATE_LIMIT_EXCEEDED"));
        assert!(server_error_closes_stream("PROVIDER_QUOTA_EXCEEDED"));
        assert!(server_error_closes_stream("PROVIDER_UNAVAILABLE"));
        assert!(!server_error_closes_stream("BAD_REQUEST"));
    }

    #[tokio::test]
    async fn unexpected_eof_reports_closed_connection_once() {
        let reported = Arc::new(AtomicUsize::new(0));
        let reported_for_callback = reported.clone();
        let callbacks = Arc::new(Mutex::new(CallbackState {
            active: Some(CallbackSet {
                on_partial: Arc::new(|_| {}),
                on_final: Arc::new(|_| {}),
                on_error: Arc::new(move |error| {
                    let SttError::Connection(error) = error else {
                        panic!("expected connection error");
                    };
                    assert_eq!(error.details.category, Some(SttConnectionCategory::Closed));
                    reported_for_callback.fetch_add(1, Ordering::SeqCst);
                }),
                on_connection_quality: Arc::new(|_, _| {}),
            }),
            ..Default::default()
        }));
        let is_closed = Arc::new(AtomicBool::new(false));

        report_backend_unexpected_eof(&callbacks, &is_closed, false).await;
        report_backend_unexpected_eof(&callbacks, &is_closed, false).await;

        assert!(is_closed.load(Ordering::SeqCst));
        assert_eq!(reported.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_error_callback_prefers_pending_during_resume() {
        fn callback_set(marker: Arc<AtomicUsize>, value: usize) -> CallbackSet {
            CallbackSet {
                on_partial: Arc::new(|_| {}),
                on_final: Arc::new(|_| {}),
                on_error: Arc::new(move |_| {
                    marker.store(value, Ordering::Relaxed);
                }),
                on_connection_quality: Arc::new(|_, _| {}),
            }
        }

        let marker = Arc::new(AtomicUsize::new(0));
        let state = CallbackState {
            active: Some(callback_set(marker.clone(), 1)),
            pending: Some(callback_set(marker.clone(), 2)),
            swap_on_next_ack: true,
            swap_after_seq: 10,
            generation_fence_after_seq: None,
        };

        let cb = state.error_callback().expect("error callback");
        cb(SttError::Processing("boom".to_string()));

        assert_eq!(marker.load(Ordering::Relaxed), 2);
    }
    #[test]
    fn terminal_delivery_mode_retains_ready_selection_after_transport_closes() {
        for accepted in [false, true] {
            let provider = BackendProvider::new();
            assert_eq!(provider.continuation_delivery_mode(), Some(false));
            provider.continuation.lock().unwrap().offered = true;
            assert_eq!(provider.continuation_delivery_mode(), None);
            {
                let mut ready = provider.continuation.lock().unwrap();
                ready.ready_seen = true;
                ready.session = accepted.then(|| ContinuationSession {
                    connection_generation: 1,
                    provider_session_id: "terminal-first".into(),
                });
            }
            provider.is_closed.store(true, Ordering::SeqCst);
            assert!(provider.continuation_session().is_none());
            assert_eq!(provider.continuation_delivery_mode(), Some(accepted));
        }
    }

    #[test]
    fn delivery_ledger_handles_ack_before_writer_completion_and_duplicate_ack() {
        let mut ledger = DeliveryLedger::default();
        assert_eq!(ledger.issue(), 1);
        assert!(ledger.ack(1));
        ledger.sent(1, 960);
        assert_eq!(ledger.issue(), 2);
        ledger.sent(2, 1920);
        assert_eq!(
            ledger.progress,
            AudioDeliveryProgress {
                sent_bytes: 2880,
                acked_bytes: 960
            }
        );
        ledger.ack(2);
        ledger.ack(2);
        ledger.ack(1);
        assert_eq!(ledger.progress.acked_bytes, 2880);
        assert!(ledger.pending.is_empty());
    }

    async fn modern_finalize_case(status: &str, reason: &str, expected_ok: bool) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let status = status.to_string();
        let reason = reason.to_string();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(tcp).await.unwrap();
            let _ = ws.next().await.unwrap().unwrap();
            ws.send(Message::Text(r#"{"type":"ready","session_id":"test","accepted_capabilities":["finalize_outcome_v1"]}"#.into())).await.unwrap();
            let mut seq = 0;
            while let Some(Ok(msg)) = ws.next().await {
                match msg {
                    Message::Binary(_) => {
                        seq += 1;
                        ws.send(Message::Text(format!(r#"{{"type":"ack","seq":{seq}}}"#)))
                            .await
                            .unwrap();
                        for delivery in [1, 1, 2] {
                            ws.send(Message::Text(format!(
                                r#"{{"type":"stable","text":"да","delivery_seq":{delivery}}}"#
                            )))
                            .await
                            .unwrap();
                        }
                    }
                    Message::Text(text) if text.contains("finalize") => {
                        let value = serde_json::json!({"type":"finalize_complete", "status":status, "saw_result":true,
                            "outcome":{"reason":reason,"tail_evidence":"unconfirmed","provider_release":"released","last_delivery_seq":2,"stable_snapshot":"да да"}});
                        ws.send(Message::Text(value.to_string())).await.unwrap();
                    }
                    Message::Text(text) if text.contains("close") => break,
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        });
        let mut config = SttConfig::new(SttProviderType::Backend);
        config.backend_url = Some(url);
        config.backend_auth_token = Some("test-token".into());
        config.backend_streaming_provider = crate::domain::BackendStreamingProvider::ElevenLabs;
        let mut provider = BackendProvider::new();
        provider.initialize(&config).await.unwrap();
        let results = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = results.clone();
        provider
            .start_stream(
                Arc::new(|_| {}),
                Arc::new(move |t| captured.lock().unwrap().push(t)),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
            )
            .await
            .unwrap();
        provider
            .send_audio(&AudioChunk::new(vec![1; 480], 16_000, 1))
            .await
            .unwrap();
        assert_eq!(provider.stop_stream().await.is_ok(), expected_ok);
        let evidence = provider.finalize_evidence().unwrap();
        assert_eq!(evidence.stable_snapshot, "да да");
        provider.abort().await.unwrap();
        assert_eq!(provider.finalize_evidence(), Some(evidence));
        let captured = results.lock().unwrap();
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[0].delivery_seq, Some(1));
        assert_eq!(captured[1].delivery_seq, Some(2));
        assert!(!captured[0].timing_known);
        drop(captured);
        assert_eq!(
            provider.audio_delivery_progress().unwrap(),
            AudioDeliveryProgress {
                sent_bytes: 960,
                acked_bytes: 960
            }
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn negotiated_stable_identity_and_unconfirmed_evidence_survive_teardown() {
        modern_finalize_case("unconfirmed", "drained", true).await;
    }

    #[tokio::test]
    async fn negotiated_failed_and_timeout_outcomes_are_not_success() {
        modern_finalize_case("failed", "provider_error", false).await;
        modern_finalize_case("timeout", "deadline", false).await;
        modern_finalize_case("no_provider", "drained", false).await;
        modern_finalize_case("future_status", "drained", false).await;
    }
    #[tokio::test]
    async fn audio_ack_window_only_blocks_after_capability_acceptance() {
        let provider = BackendProvider::new();
        provider.is_closed.store(false, Ordering::SeqCst);
        {
            let mut ledger = provider.delivery.lock().unwrap();
            ledger.issue();
            ledger.sent(1, 32_000);
        }
        tokio::time::timeout(
            Duration::from_millis(50),
            provider.wait_for_audio_window(960),
        )
        .await
        .expect("legacy peers must not get new ACK waits")
        .unwrap();
        provider.outcome_negotiated.store(true, Ordering::SeqCst);
        assert!(tokio::time::timeout(
            Duration::from_millis(20),
            provider.wait_for_audio_window(960)
        )
        .await
        .is_err());
        provider.delivery.lock().unwrap().ack(1);
        provider.ack_changed.notify_one();
        tokio::time::timeout(
            Duration::from_millis(50),
            provider.wait_for_audio_window(960),
        )
        .await
        .expect("ACK must unblock sender")
        .unwrap();
    }
    #[test]
    fn delivery_ledger_rejects_future_ack_and_preserves_keepalive_sequence_floor() {
        let mut ledger = DeliveryLedger::default();
        assert!(!ledger.ack(1));
        assert_eq!(ledger.issue(), 1);
        ledger.sent(1, 960);
        assert!(!ledger.ack(2));
        assert_eq!(ledger.progress.acked_bytes, 0);
        assert!(ledger.ack(1));
        ledger.begin_run();
        assert_eq!(ledger.progress, AudioDeliveryProgress::default());
        assert!(!ledger.ack(1));
        assert_eq!(ledger.issue(), 2);
        ledger.sent(2, 1920);
        assert!(!ledger.ack(1));
        assert_eq!(ledger.progress.acked_bytes, 0);
        assert!(ledger.ack(2));
        assert_eq!(
            ledger.progress,
            AudioDeliveryProgress {
                sent_bytes: 1920,
                acked_bytes: 1920
            }
        );
        // A new WS has an entirely new sequence namespace.
        let mut new_socket = DeliveryLedger::default();
        assert!(!new_socket.ack(2));
        assert_eq!(new_socket.issue(), 1);
        assert!(!new_socket.ack(2));
    }

    #[test]
    fn audio_capacity_counts_unacked_and_buffered_audio_together() {
        let mut ledger = DeliveryLedger::default();
        ledger.issue();
        ledger.sent(1, 800_000);
        assert!(ledger.admits(150_000, 10_000));
        assert!(!ledger.admits(150_000, 10_002));
        assert_eq!(ledger.debt_bytes(150_000), 950_000);
        ledger.ack(1);
        assert!(ledger.admits(150_000, 800_000));
        assert!(!ledger.admits(960_000, 2));
    }

    #[tokio::test]
    async fn unissued_ack_poisons_stream_without_crediting_audio() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(tcp).await.unwrap();
            let _ = ws.next().await;
            ws.send(Message::Text(
                r#"{"type":"ready","session_id":"test"}"#.into(),
            ))
            .await
            .unwrap();
            // An ACK from an old namespace, before this socket issued any PCM.
            ws.send(Message::Text(r#"{"type":"ack","seq":7}"#.into()))
                .await
                .unwrap();
            while let Some(Ok(message)) = ws.next().await {
                assert!(
                    !matches!(message, Message::Close(_)),
                    "hard abort must not send a WS Close"
                );
                assert!(
                    !matches!(message, Message::Binary(_)),
                    "poisoned stream must not send PCM"
                );
            }
        });
        let mut config = SttConfig::new(SttProviderType::Backend);
        config.backend_url = Some(url);
        config.backend_auth_token = Some("test-token".into());
        let mut provider = BackendProvider::new();
        provider.initialize(&config).await.unwrap();
        let (errors, mut received) = tokio::sync::mpsc::unbounded_channel();
        provider
            .start_stream(
                Arc::new(|_| {}),
                Arc::new(|_| {}),
                Arc::new(move |e| {
                    let _ = errors.send(e);
                }),
                Arc::new(|_, _| {}),
            )
            .await
            .unwrap();
        let error = tokio::time::timeout(Duration::from_secs(2), received.recv())
            .await
            .unwrap()
            .unwrap();
        let SttError::Connection(error) = error else {
            panic!("expected typed protocol failure");
        };
        assert_eq!(error.details.server_code.as_deref(), Some("INVALID_ACK"));
        assert_eq!(
            error.details.category,
            Some(SttConnectionCategory::ServerError)
        );
        assert!(provider
            .send_audio(&AudioChunk::new(vec![1; 480], 16000, 1))
            .await
            .is_err());
        assert_eq!(
            provider.audio_delivery_progress(),
            Some(AudioDeliveryProgress::default())
        );
        provider.abort().await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn eight_second_pre_ready_delay_has_no_new_ack_deadline_or_graceful_abort() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(tcp).await.unwrap();
            let _ = ws.next().await;
            let ready_delay = tokio::time::sleep(Duration::from_secs(8));
            tokio::pin!(ready_delay);
            let mut ready = false;
            let mut seq = 0;
            let mut bytes = 0;
            loop {
                tokio::select! {
                    _ = &mut ready_delay, if !ready => {
                        ready = true;
                        ws.send(Message::Text(r#"{"type":"ready","session_id":"test","accepted_capabilities":["finalize_outcome_v1"]}"#.into())).await.unwrap();
                        if seq > 0 { ws.send(Message::Text(serde_json::json!({"type":"ack","seq":seq}).to_string())).await.unwrap(); }
                    }
                    message = ws.next() => {
                        match message {
                            Some(Ok(Message::Binary(audio))) => {
                                assert!(audio.len() <= 9600);
                                bytes += audio.len();
                                seq += 1;
                                if ready { ws.send(Message::Text(serde_json::json!({"type":"ack","seq":seq}).to_string())).await.unwrap(); }
                            }
                            Some(Ok(Message::Close(_))) => panic!("hard abort emitted WebSocket Close"),
                            Some(Ok(Message::Text(_))) => panic!("hard abort emitted a JSON control frame"),
                            Some(Err(_)) | None => break,
                            _ => {}
                        }
                    }
                }
            }
            (seq, bytes)
        });
        let mut config = SttConfig::new(SttProviderType::Backend);
        config.backend_url = Some(url);
        config.backend_auth_token = Some("test-token".into());
        config.backend_streaming_provider = crate::domain::BackendStreamingProvider::ElevenLabs;
        let mut provider = BackendProvider::new();
        provider.initialize(&config).await.unwrap();
        provider
            .start_stream(
                Arc::new(|_| {}),
                Arc::new(|_| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            for _ in 0..40 {
                provider
                    .send_audio(&AudioChunk::new(vec![1; 480], 16000, 1))
                    .await
                    .unwrap();
            }
        })
        .await
        .expect("pre-Ready debt must not activate a negotiated ACK timeout");
        assert_eq!(
            provider.audio_delivery_progress().unwrap().sent_bytes,
            38_400
        );
        assert_eq!(provider.audio_delivery_progress().unwrap().acked_bytes, 0);
        assert_eq!(provider.preferred_audio_batch_samples(), None);
        tokio::time::timeout(Duration::from_secs(9), async {
            while !provider.outcome_negotiated.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(provider.preferred_audio_batch_samples(), Some(4800));
        // A partial frame remains in the batch. Abort must discard it locally,
        // without flushing pending PCM or either JSON/WS Close to the peer.
        provider
            .send_audio(&AudioChunk::new(vec![1; 20], 16000, 1))
            .await
            .unwrap();
        assert_eq!(provider.audio_batch.len(), 40);
        provider.abort().await.unwrap();
        let (seq, bytes) = tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
        assert_eq!((seq, bytes), (40, 38_400));
    }
}
