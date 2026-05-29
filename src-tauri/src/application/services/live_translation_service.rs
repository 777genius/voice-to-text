//! LiveTranslationService — orchestrator для live-перевода голоса.
//!
//! Pipeline:
//! - mic 24 kHz mono (SystemAudioCapture с translation options)
//!   - применяем microphone_sensitivity gain
//!   - feed-им в audio_spectrum analyzer
//!   - отправляем в OpenAI realtime translation client (PCM16 base64)
//! - OpenAI отдаёт events:
//!   - `AudioDelta(Vec<i16>)` → `CpalAudioOutput.enqueue_pcm16(...)` (BlackHole 2ch)
//!   - `TranscriptDelta(String)` → callback в UI (popover)
//!   - `Error(...)` → callback в UI + статус Error
//!   - `Closed` → если незапланированно → callback в UI
//!
//! Этот сервис намеренно НЕ повторяет логику TranscriptionService:
//! - нет auto-paste/copy/history;
//! - нет STT auth retry/logout;
//! - нет VAD (translation идёт сплошным потоком, включая тишину).

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::task::JoinHandle;

use crate::domain::{
    amplify_i16_samples, microphone_sensitivity_gain, AudioCapture, AudioChunk, AudioChunkCallback,
    RecordingStatus,
};
use crate::infrastructure::audio::{
    AudioOutput, AudioOutputConfig, CpalAudioOutput, SystemAudioCapture, SystemAudioCaptureOptions,
};
use crate::infrastructure::openai::{
    OpenAIErrorKind, OpenAIRealtimeEvent, OpenAIRealtimeTranslationClient, OpenAITranslationError,
};

use super::audio_spectrum::AudioSpectrumAnalyzer;

const TRANSLATION_TARGET_LANGUAGE_DEFAULT: &str = "en";
const GRACEFUL_CLOSE_TIMEOUT_MS: u64 = 2_500;
const MIC_PUMP_DRAIN_TIMEOUT_MS: u64 = 500;
const FORWARDER_DRAIN_TIMEOUT_MS: u64 = 750;
const OUTPUT_DRAIN_SAFETY_MS: u64 = 250;
const OUTPUT_DRAIN_MAX_MS: u64 = 6_000;
const OUTPUT_DRAIN_POLL_MS: u64 = 50;
const OUTPUT_DRAIN_EMPTY_THRESHOLD_MS: u64 = 30;

#[derive(Debug, Clone)]
pub struct LiveTranslationConfig {
    pub openai_api_key: String,
    /// Target language (ISO code: "en", "es", "fr", ...). По умолчанию "en".
    pub target_language: String,
    pub microphone_device: Option<String>,
    pub microphone_sensitivity: u8,
    pub session_id: u64,
}

impl LiveTranslationConfig {
    pub fn new_with_defaults(session_id: u64) -> Self {
        Self {
            openai_api_key: std::env::var("OPENAI_API_KEY").unwrap_or_default(),
            target_language: TRANSLATION_TARGET_LANGUAGE_DEFAULT.to_string(),
            microphone_device: None,
            microphone_sensitivity: 100,
            session_id,
        }
    }
}

#[derive(Clone)]
pub struct LiveTranslationCallbacks {
    pub on_transcript_delta: Arc<dyn Fn(String) + Send + Sync>,
    pub on_audio_spectrum: Arc<dyn Fn([f32; 48]) + Send + Sync>,
    pub on_error: Arc<dyn Fn(LiveTranslationError) + Send + Sync>,
    pub on_status: Arc<dyn Fn(RecordingStatus) + Send + Sync>,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum LiveTranslationError {
    #[error("configuration: {0}")]
    Configuration(String),
    #[error("authentication: {0}")]
    Authentication(String),
    #[error("rate_limited: {0}")]
    RateLimited(String),
    #[error("connection: {0}")]
    Connection(String),
    #[error("timeout: {0}")]
    Timeout(String),
    #[error("processing: {0}")]
    Processing(String),
}

impl LiveTranslationError {
    /// Канонический строковый ID для UI (тот же что в TranslationErrorPayload.error_type).
    pub fn error_type(&self) -> &'static str {
        match self {
            Self::Configuration(_) => "configuration",
            Self::Authentication(_) => "authentication",
            Self::RateLimited(_) => "rate_limited",
            Self::Connection(_) => "connection",
            Self::Timeout(_) => "timeout",
            Self::Processing(_) => "processing",
        }
    }
}

impl From<OpenAITranslationError> for LiveTranslationError {
    fn from(err: OpenAITranslationError) -> Self {
        let msg = err.to_string();
        match err.kind() {
            OpenAIErrorKind::Authentication => LiveTranslationError::Authentication(msg),
            OpenAIErrorKind::RateLimited => LiveTranslationError::RateLimited(msg),
            OpenAIErrorKind::Connection => LiveTranslationError::Connection(msg),
            OpenAIErrorKind::Protocol => LiveTranslationError::Processing(msg),
            OpenAIErrorKind::Internal => LiveTranslationError::Processing(msg),
        }
    }
}

pub struct LiveTranslationService {
    status: Arc<RwLock<RecordingStatus>>,
    inner: Arc<Mutex<Option<RunningSession>>>,
}

struct RunningSession {
    capture: Arc<RwLock<SystemAudioCapture>>,
    output: Arc<RwLock<CpalAudioOutput>>,
    client: Arc<Mutex<OpenAIRealtimeTranslationClient>>,
    forwarder_task: JoinHandle<()>,
    audio_pump_task: JoinHandle<()>,
    session_id: u64,
}

#[derive(Debug, Clone)]
enum RuntimeStop {
    Error(LiveTranslationError),
    Closed,
}

#[derive(Debug, Clone, Copy)]
enum CleanupMode {
    GracefulStop,
    RuntimeFailure,
}

impl Default for LiveTranslationService {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveTranslationService {
    pub fn new() -> Self {
        Self {
            status: Arc::new(RwLock::new(RecordingStatus::Idle)),
            inner: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn get_status(&self) -> RecordingStatus {
        *self.status.read().await
    }

    pub async fn active_session_id(&self) -> Option<u64> {
        self.inner.lock().await.as_ref().map(|s| s.session_id)
    }

    /// Полный preflight + старт всех потоков.
    /// Order: api_key → output device → mic preflight → openai connect → mic capture → audio pump.
    /// Если падает что-то в середине — корректно откатываем уже открытые ресурсы.
    pub async fn start_translation(
        &self,
        config: LiveTranslationConfig,
        callbacks: LiveTranslationCallbacks,
    ) -> Result<(), LiveTranslationError> {
        let mut guard = self.inner.lock().await;
        if guard.is_some() {
            return Err(LiveTranslationError::Configuration(
                "Translation session уже активна".into(),
            ));
        }

        *self.status.write().await = RecordingStatus::Starting;
        (callbacks.on_status)(RecordingStatus::Starting);

        // 1. API key
        if config.openai_api_key.trim().is_empty() {
            let err = LiveTranslationError::Configuration(
                "OPENAI_API_KEY не задан. Положите ключ в frontend/src-tauri/.env или frontend/.env"
                    .into(),
            );
            self.transition_to_error().await;
            return Err(err);
        }

        // 2. Output device (BlackHole) — fail cheap, до OpenAI
        let mut output_concrete = CpalAudioOutput::new();
        if let Err(e) = output_concrete
            .open(AudioOutputConfig::openai_translation())
            .await
        {
            let err = LiveTranslationError::Configuration(e.to_string());
            self.transition_to_error().await;
            return Err(err);
        }
        let output = Arc::new(RwLock::new(output_concrete));

        // 3. Mic preflight. Do this before paid OpenAI session creation.
        if let Err(e) = microphone_permission_preflight() {
            let _ = output.write().await.close().await;
            let err = LiveTranslationError::Configuration(e);
            self.transition_to_error().await;
            return Err(err);
        }

        let capture_result = SystemAudioCapture::with_device_and_options(
            config.microphone_device.clone(),
            SystemAudioCaptureOptions::translation(),
        );
        let capture = match capture_result {
            Ok(c) => Arc::new(RwLock::new(c)),
            Err(e) => {
                let _ = output.write().await.close().await;
                let err = LiveTranslationError::Configuration(format!("mic init: {}", e));
                self.transition_to_error().await;
                return Err(err);
            }
        };

        // 4. OpenAI client connect
        let mut client = OpenAIRealtimeTranslationClient::new(
            config.openai_api_key.clone(),
            config.target_language.clone(),
        );
        let openai_rx = match client.connect().await {
            Ok(rx) => rx,
            Err(e) => {
                // откатываем output
                let _ = output.write().await.close().await;
                let mapped: LiveTranslationError = e.into();
                self.transition_to_error().await;
                return Err(mapped);
            }
        };
        let client = Arc::new(Mutex::new(client));

        // 5. Bridge sync mic callback -> async pump
        // SystemAudioCapture зовёт on_chunk из cpal-thread синхронно. Сразу пушим в mpsc.
        let (mic_tx, mic_rx) = mpsc::unbounded_channel::<AudioChunk>();
        let mic_callback: AudioChunkCallback = Arc::new(move |chunk: AudioChunk| {
            let _ = mic_tx.send(chunk);
        });

        // 6. Start mic capture
        if let Err(e) = capture.write().await.start_capture(mic_callback).await {
            // hard cleanup
            client.lock().await.abort().await;
            let _ = output.write().await.close().await;
            let err = LiveTranslationError::Configuration(format!("mic start: {}", e));
            self.transition_to_error().await;
            return Err(err);
        }

        // 7. Spawn runtime tasks. Fatal task exits go through runtime_stop_tx so the
        // service can clean capture/output/client and not leave inner stuck Some(...).
        let (runtime_stop_tx, runtime_stop_rx) = mpsc::unbounded_channel::<RuntimeStop>();

        let audio_pump_task = {
            let client = client.clone();
            let sensitivity = config.microphone_sensitivity;
            let on_spectrum = callbacks.on_audio_spectrum.clone();
            let runtime_stop_tx = runtime_stop_tx.clone();
            tokio::spawn(async move {
                run_audio_pump(mic_rx, client, sensitivity, on_spectrum, runtime_stop_tx).await;
            })
        };

        let forwarder_task = {
            let output = output.clone();
            let on_transcript = callbacks.on_transcript_delta.clone();
            let runtime_stop_tx = runtime_stop_tx.clone();
            tokio::spawn(async move {
                run_event_forwarder(openai_rx, output, on_transcript, runtime_stop_tx).await;
            })
        };

        *guard = Some(RunningSession {
            capture,
            output,
            client,
            forwarder_task,
            audio_pump_task,
            session_id: config.session_id,
        });
        spawn_runtime_cleanup_monitor(
            self.inner.clone(),
            self.status.clone(),
            callbacks.clone(),
            runtime_stop_rx,
            config.session_id,
        );
        *self.status.write().await = RecordingStatus::Recording;
        (callbacks.on_status)(RecordingStatus::Recording);
        log::info!(
            "LiveTranslationService: session {} started, target={}, sensitivity={}",
            config.session_id,
            config.target_language,
            config.microphone_sensitivity
        );
        Ok(())
    }

    /// Graceful stop:
    /// 1) mic capture stop сразу
    /// 2) даём audio pump дослать уже захваченные mic chunks
    /// 3) openai client.close(drain timeout) - даём дотечь хвосту
    /// 4) ждём фактический хвост output queue, а не фиксированное время
    /// 5) output close
    /// 6) abort pumps
    pub async fn stop_translation(&self) -> Result<(), LiveTranslationError> {
        let mut guard = self.inner.lock().await;
        let Some(session) = guard.take() else {
            return Ok(());
        };
        *self.status.write().await = RecordingStatus::Processing;

        cleanup_session(session, CleanupMode::GracefulStop).await;

        *self.status.write().await = RecordingStatus::Idle;
        Ok(())
    }

    async fn transition_to_error(&self) {
        *self.status.write().await = RecordingStatus::Error;
    }
}

#[cfg(target_os = "macos")]
fn microphone_permission_preflight() -> Result<(), String> {
    use crate::infrastructure::microphone_permission::{
        microphone_permission_status, MicrophonePermissionStatus,
    };

    match microphone_permission_status() {
        MicrophonePermissionStatus::Authorized | MicrophonePermissionStatus::NotDetermined => Ok(()),
        _ => Err(
            "Нет доступа к микрофону. Откройте macOS System Settings -> Privacy & Security -> Microphone и включите доступ для приложения."
                .to_string(),
        ),
    }
}

#[cfg(not(target_os = "macos"))]
fn microphone_permission_preflight() -> Result<(), String> {
    Ok(())
}

async fn run_audio_pump(
    mut mic_rx: mpsc::UnboundedReceiver<AudioChunk>,
    client: Arc<Mutex<OpenAIRealtimeTranslationClient>>,
    sensitivity: u8,
    on_spectrum: Arc<dyn Fn([f32; 48]) + Send + Sync>,
    runtime_stop_tx: mpsc::UnboundedSender<RuntimeStop>,
) {
    let gain = microphone_sensitivity_gain(sensitivity);
    let mut spectrum = AudioSpectrumAnalyzer::new();

    while let Some(chunk) = mic_rx.recv().await {
        // gain
        let amplified = if (gain - 1.0).abs() < f32::EPSILON {
            chunk.data
        } else {
            amplify_i16_samples(&chunk.data, gain)
        };

        // spectrum (cheap, FFT 256-1024 samples)
        if let Some(bars) = spectrum.push_samples(&amplified) {
            on_spectrum(bars);
        }

        // отправка в OpenAI
        let send_res = {
            let client = client.lock().await;
            client.append_input_audio(&amplified).await
        };
        if let Err(e) = send_res {
            let kind_err: LiveTranslationError = e.into();
            log::warn!("LiveTranslationService audio pump send error: {}", kind_err);
            let _ = runtime_stop_tx.send(RuntimeStop::Error(kind_err));
            // single error → выходим, чтобы не спамить
            break;
        }
    }
    log::info!("LiveTranslationService: audio pump exited");
}

async fn run_event_forwarder(
    mut openai_rx: mpsc::UnboundedReceiver<OpenAIRealtimeEvent>,
    output: Arc<RwLock<CpalAudioOutput>>,
    on_transcript: Arc<dyn Fn(String) + Send + Sync>,
    runtime_stop_tx: mpsc::UnboundedSender<RuntimeStop>,
) {
    while let Some(ev) = openai_rx.recv().await {
        match ev {
            OpenAIRealtimeEvent::SessionCreated | OpenAIRealtimeEvent::SessionUpdated => {
                // лог уже сделан внутри клиента
            }
            OpenAIRealtimeEvent::AudioDelta(pcm16) => {
                let out = output.read().await;
                if let Err(e) = out.enqueue_pcm16(&pcm16).await {
                    log::warn!("LiveTranslationService: output enqueue failed: {}", e);
                    let _ = runtime_stop_tx.send(RuntimeStop::Error(
                        LiveTranslationError::Processing(e.to_string()),
                    ));
                    break;
                }
            }
            OpenAIRealtimeEvent::TranscriptDelta(text) => {
                on_transcript(text);
            }
            OpenAIRealtimeEvent::InputTranscriptDelta(text) => {
                log::debug!("translation source delta: {}", text);
            }
            OpenAIRealtimeEvent::Error {
                code,
                message,
                kind,
            } => {
                let kind_err = match kind {
                    OpenAIErrorKind::Authentication => {
                        LiveTranslationError::Authentication(message)
                    }
                    OpenAIErrorKind::RateLimited => LiveTranslationError::RateLimited(message),
                    OpenAIErrorKind::Connection => LiveTranslationError::Connection(message),
                    OpenAIErrorKind::Protocol => LiveTranslationError::Processing(message),
                    OpenAIErrorKind::Internal => LiveTranslationError::Processing(message),
                };
                log::error!(
                    "LiveTranslationService: server error (code={:?}): {}",
                    code,
                    kind_err
                );
                let _ = runtime_stop_tx.send(RuntimeStop::Error(kind_err));
                break;
            }
            OpenAIRealtimeEvent::Closed => {
                log::info!("LiveTranslationService: openai session closed");
                let _ = runtime_stop_tx.send(RuntimeStop::Closed);
                break;
            }
        }
    }
    log::info!("LiveTranslationService: event forwarder exited");
}

fn spawn_runtime_cleanup_monitor(
    inner: Arc<Mutex<Option<RunningSession>>>,
    status: Arc<RwLock<RecordingStatus>>,
    callbacks: LiveTranslationCallbacks,
    mut runtime_stop_rx: mpsc::UnboundedReceiver<RuntimeStop>,
    session_id: u64,
) {
    tokio::spawn(async move {
        let Some(stop) = runtime_stop_rx.recv().await else {
            return;
        };

        let session = {
            let mut guard = inner.lock().await;
            let is_current = guard
                .as_ref()
                .map(|session| session.session_id == session_id)
                .unwrap_or(false);
            if is_current {
                guard.take()
            } else {
                None
            }
        };

        let Some(session) = session else {
            // Manual stop already took ownership. Ignore close/error from the shutdown path.
            return;
        };

        let err = match stop {
            RuntimeStop::Error(err) => err,
            RuntimeStop::Closed => LiveTranslationError::Connection(
                "OpenAI realtime translation session closed unexpectedly".to_string(),
            ),
        };

        *status.write().await = RecordingStatus::Error;
        (callbacks.on_error)(err);
        (callbacks.on_status)(RecordingStatus::Error);

        cleanup_session(session, CleanupMode::RuntimeFailure).await;
    });
}

async fn cleanup_session(mut session: RunningSession, mode: CleanupMode) {
    let session_id = session.session_id;

    if let Err(e) = session.capture.write().await.stop_capture().await {
        log::warn!(
            "LiveTranslationService cleanup: mic stop_capture failed for session {}: {}",
            session_id,
            e
        );
    }

    match mode {
        CleanupMode::GracefulStop => {
            let audio_pump_finished = wait_task_done(
                &mut session.audio_pump_task,
                Duration::from_millis(MIC_PUMP_DRAIN_TIMEOUT_MS),
                "audio pump",
                session_id,
            )
            .await;
            if !audio_pump_finished {
                session.audio_pump_task.abort();
            }

            let close_res = {
                let mut client = session.client.lock().await;
                client
                    .close(Duration::from_millis(GRACEFUL_CLOSE_TIMEOUT_MS))
                    .await
            };
            if let Err(e) = close_res {
                log::warn!(
                    "LiveTranslationService cleanup: openai close failed for session {}: {}",
                    session_id,
                    e
                );
            }

            let _ = wait_task_done(
                &mut session.forwarder_task,
                Duration::from_millis(FORWARDER_DRAIN_TIMEOUT_MS),
                "event forwarder",
                session_id,
            )
            .await;

            drain_output_tail(session.output.clone(), session_id).await;
        }
        CleanupMode::RuntimeFailure => {
            session.client.lock().await.abort().await;
        }
    }

    if let Err(e) = session.output.write().await.close().await {
        log::warn!(
            "LiveTranslationService cleanup: output close failed for session {}: {}",
            session_id,
            e
        );
    }

    session.audio_pump_task.abort();
    session.forwarder_task.abort();

    log::info!(
        "LiveTranslationService: session {} cleaned up ({:?})",
        session_id,
        mode
    );
}

async fn wait_task_done(
    task: &mut JoinHandle<()>,
    timeout: Duration,
    label: &str,
    session_id: u64,
) -> bool {
    match tokio::time::timeout(timeout, task).await {
        Ok(Ok(())) => {
            log::debug!(
                "LiveTranslationService cleanup: {} drained for session {}",
                label,
                session_id
            );
            true
        }
        Ok(Err(e)) => {
            log::warn!(
                "LiveTranslationService cleanup: {} join failed for session {}: {}",
                label,
                session_id,
                e
            );
            true
        }
        Err(_) => {
            log::debug!(
                "LiveTranslationService cleanup: {} did not drain within {} ms for session {}",
                label,
                timeout.as_millis(),
                session_id
            );
            false
        }
    }
}

async fn drain_output_tail(output: Arc<RwLock<CpalAudioOutput>>, session_id: u64) {
    let initial_pending = {
        let out = output.read().await;
        match out.prepare_for_drain() {
            Ok(pending) => pending,
            Err(e) => {
                log::warn!(
                    "LiveTranslationService cleanup: output drain prepare failed for session {}: {}",
                    session_id,
                    e
                );
                return;
            }
        }
    };

    if initial_pending <= Duration::from_millis(OUTPUT_DRAIN_EMPTY_THRESHOLD_MS) {
        return;
    }

    let requested_wait =
        initial_pending.saturating_add(Duration::from_millis(OUTPUT_DRAIN_SAFETY_MS));
    let max_wait = Duration::from_millis(OUTPUT_DRAIN_MAX_MS);
    let wait_budget = requested_wait.min(max_wait);
    let deadline = tokio::time::Instant::now() + wait_budget;

    log::info!(
        "LiveTranslationService cleanup: draining output tail for session {} (pending={} ms, budget={} ms)",
        session_id,
        initial_pending.as_millis(),
        wait_budget.as_millis()
    );

    loop {
        let pending = {
            let out = output.read().await;
            out.pending_playback_duration()
        };

        if pending <= Duration::from_millis(OUTPUT_DRAIN_EMPTY_THRESHOLD_MS) {
            log::debug!(
                "LiveTranslationService cleanup: output tail drained for session {}",
                session_id
            );
            return;
        }

        let now = tokio::time::Instant::now();
        if now >= deadline {
            log::warn!(
                "LiveTranslationService cleanup: output tail drain timeout for session {} (pending={} ms)",
                session_id,
                pending.as_millis()
            );
            return;
        }

        let remaining = deadline.saturating_duration_since(now);
        tokio::time::sleep(
            Duration::from_millis(OUTPUT_DRAIN_POLL_MS)
                .min(pending)
                .min(remaining),
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_type_strings_are_stable() {
        assert_eq!(
            LiveTranslationError::Configuration("x".into()).error_type(),
            "configuration"
        );
        assert_eq!(
            LiveTranslationError::Authentication("x".into()).error_type(),
            "authentication"
        );
        assert_eq!(
            LiveTranslationError::RateLimited("x".into()).error_type(),
            "rate_limited"
        );
        assert_eq!(
            LiveTranslationError::Connection("x".into()).error_type(),
            "connection"
        );
        assert_eq!(
            LiveTranslationError::Timeout("x".into()).error_type(),
            "timeout"
        );
        assert_eq!(
            LiveTranslationError::Processing("x".into()).error_type(),
            "processing"
        );
    }

    #[test]
    fn config_defaults_pick_target_en() {
        let cfg = LiveTranslationConfig::new_with_defaults(42);
        assert_eq!(cfg.target_language, "en");
        assert_eq!(cfg.session_id, 42);
        assert_eq!(cfg.microphone_sensitivity, 100);
        assert!(cfg.microphone_device.is_none());
    }

    #[tokio::test]
    async fn service_starts_in_idle() {
        let svc = LiveTranslationService::new();
        assert_eq!(svc.get_status().await, RecordingStatus::Idle);
        assert!(svc.active_session_id().await.is_none());
    }

    #[tokio::test]
    async fn empty_api_key_fails_preflight_with_configuration_error() {
        let svc = LiveTranslationService::new();
        let cfg = LiveTranslationConfig {
            openai_api_key: String::new(),
            target_language: "en".into(),
            microphone_device: None,
            microphone_sensitivity: 100,
            session_id: 1,
        };
        let cbs = LiveTranslationCallbacks {
            on_transcript_delta: Arc::new(|_| {}),
            on_audio_spectrum: Arc::new(|_| {}),
            on_error: Arc::new(|_| {}),
            on_status: Arc::new(|_| {}),
        };
        let err = svc.start_translation(cfg, cbs).await.unwrap_err();
        assert!(matches!(err, LiveTranslationError::Configuration(_)));
        assert_eq!(svc.get_status().await, RecordingStatus::Error);
    }
}
