use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use futures_util::FutureExt;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::domain::{
    amplify_i16_samples, AudioCapture, AudioCaptureErrorCallback, AudioCaptureHealthProbe,
    AudioChunk, AudioChunkCallback, AudioEnqueueOutcome, AudioError, RealtimeTranslationError,
    RealtimeTranslationErrorKind, RealtimeTranslationEvent, RealtimeTranslationSession,
    TranslationAudioOutput, TranslationAudioOutputConfig, TranslationAudioOutputError,
};

use super::frame_assembler::Pcm16FrameAssembler;
use super::{start_owned_capture, StartupCaptureError};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RealtimeInterpretationError {
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
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("input_device_lost: {0}")]
    InputDeviceLost(String),
    #[error("output_device_lost: {0}")]
    OutputDeviceLost(String),
    #[error("input_overload: {0}")]
    InputOverload(String),
    #[error("output_overload: {0}")]
    OutputOverload(String),
}

impl From<RealtimeTranslationError> for RealtimeInterpretationError {
    fn from(error: RealtimeTranslationError) -> Self {
        let message = error.to_string();
        match error.kind() {
            RealtimeTranslationErrorKind::Authentication => Self::Authentication(message),
            RealtimeTranslationErrorKind::RateLimited => Self::RateLimited(message),
            RealtimeTranslationErrorKind::Connection => Self::Connection(message),
            RealtimeTranslationErrorKind::Timeout => Self::Timeout(message),
            RealtimeTranslationErrorKind::Protocol => Self::Protocol(message),
            RealtimeTranslationErrorKind::Internal => Self::Processing(message),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RealtimeInterpretationStop {
    Error(RealtimeInterpretationError),
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RealtimeInterpretationShutdown {
    Graceful,
    Abort,
}

#[derive(Debug, thiserror::Error)]
pub enum RealtimeInterpretationStartError {
    #[error("capture start: {0}")]
    Capture(AudioError),
    #[error("startup timeout: {0}")]
    Timeout(String),
}

pub type RealtimeTextCallback = Arc<dyn Fn(String) + Send + Sync>;
pub type RealtimeInputAudioCallback = Arc<dyn Fn(&[i16]) + Send + Sync>;

#[derive(Clone)]
pub struct RealtimeInterpretationCallbacks {
    pub on_translated_text: RealtimeTextCallback,
    pub on_source_text: RealtimeTextCallback,
    pub on_input_audio: RealtimeInputAudioCallback,
}

impl RealtimeInterpretationCallbacks {
    #[cfg(test)]
    fn no_op() -> Self {
        Self {
            on_translated_text: Arc::new(|_| {}),
            on_source_text: Arc::new(|_| {}),
            on_input_audio: Arc::new(|_| {}),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RealtimeInterpretationPolicy {
    input_frame_samples: usize,
    input_queue_capacity_chunks: usize,
    input_overload_drop_threshold: u64,
    output_queue_capacity_chunks: usize,
    output_overload_drop_threshold: u64,
    output_pending_overload: Option<OutputPendingOverloadPolicy>,
    input_drain_timeout: Duration,
    event_drain_timeout: Duration,
    translation_finish_timeout: Duration,
    capture_health_poll_interval: Duration,
    output_health_poll_interval: Duration,
    suspension_watchdog_poll: Duration,
    suspension_watchdog_threshold: Duration,
    output_drain_safety: Duration,
    output_drain_max: Duration,
    output_drain_poll: Duration,
    output_drain_empty_threshold: Duration,
    capture_start_timeout: Duration,
    worker_stop_timeout: Duration,
    graceful_shutdown_timeout: Duration,
    forced_shutdown_timeout: Duration,
    input_source_name: &'static str,
    output_route_name: &'static str,
    silence_cadence: Option<SilenceCadencePolicy>,
}

#[derive(Debug, Clone)]
struct SilenceCadencePolicy {
    gap_threshold: Duration,
    interval: Duration,
    sample_rate: u32,
    channels: u16,
}

#[derive(Debug, Clone)]
struct OutputPendingOverloadPolicy {
    limit: Duration,
    grace: Duration,
}

impl RealtimeInterpretationPolicy {
    pub fn outgoing() -> Self {
        Self {
            input_frame_samples: 4_800,
            input_queue_capacity_chunks: 160,
            input_overload_drop_threshold: 32,
            output_queue_capacity_chunks: 32,
            output_overload_drop_threshold: 32,
            output_pending_overload: None,
            input_drain_timeout: Duration::from_millis(1_500),
            event_drain_timeout: Duration::from_millis(1_500),
            translation_finish_timeout: Duration::from_millis(8_000),
            capture_health_poll_interval: Duration::from_millis(250),
            output_health_poll_interval: Duration::from_millis(250),
            suspension_watchdog_poll: Duration::from_secs(1),
            suspension_watchdog_threshold: Duration::from_secs(8),
            output_drain_safety: Duration::from_millis(250),
            output_drain_max: Duration::from_millis(12_000),
            output_drain_poll: Duration::from_millis(50),
            output_drain_empty_threshold: Duration::from_millis(30),
            capture_start_timeout: Duration::from_secs(5),
            worker_stop_timeout: Duration::from_millis(1_500),
            graceful_shutdown_timeout: Duration::ZERO,
            forced_shutdown_timeout: Duration::from_secs(2),
            input_source_name: "Microphone",
            output_route_name: "virtual microphone",
            silence_cadence: None,
        }
        .with_derived_graceful_shutdown_timeout()
    }

    pub fn incoming_spoken() -> Self {
        let output_drain_safety = Duration::from_millis(250);
        let output_drain_max = TranslationAudioOutputConfig::incoming_spoken_translation()
            .drain_max_buffered_duration
            .saturating_add(output_drain_safety);
        Self {
            input_frame_samples: 4_800,
            input_queue_capacity_chunks: 64,
            input_overload_drop_threshold: 8,
            output_queue_capacity_chunks: 32,
            output_overload_drop_threshold: 8,
            output_pending_overload: Some(OutputPendingOverloadPolicy {
                limit: Duration::from_secs(8),
                grace: Duration::from_secs(2),
            }),
            input_drain_timeout: Duration::from_millis(1_500),
            event_drain_timeout: Duration::from_millis(1_500),
            translation_finish_timeout: Duration::from_millis(5_000),
            capture_health_poll_interval: Duration::from_millis(250),
            output_health_poll_interval: Duration::from_millis(250),
            suspension_watchdog_poll: Duration::from_secs(1),
            suspension_watchdog_threshold: Duration::from_secs(8),
            output_drain_safety,
            output_drain_max,
            output_drain_poll: Duration::from_millis(50),
            output_drain_empty_threshold: Duration::from_millis(30),
            capture_start_timeout: Duration::from_secs(5),
            worker_stop_timeout: Duration::from_millis(1_000),
            graceful_shutdown_timeout: Duration::ZERO,
            forced_shutdown_timeout: Duration::from_secs(1),
            input_source_name: "System audio",
            output_route_name: "local playback",
            silence_cadence: Some(SilenceCadencePolicy {
                gap_threshold: Duration::from_millis(400),
                interval: Duration::from_millis(200),
                sample_rate: 24_000,
                channels: 1,
            }),
        }
        .with_derived_graceful_shutdown_timeout()
    }

    fn with_derived_graceful_shutdown_timeout(mut self) -> Self {
        self.graceful_shutdown_timeout = self.required_graceful_shutdown_timeout();
        self
    }

    fn required_graceful_shutdown_timeout(&self) -> Duration {
        self.input_drain_timeout
            .saturating_add(self.translation_finish_timeout)
            .saturating_add(self.event_drain_timeout)
            .saturating_add(self.output_drain_max)
            .saturating_add(self.output_drain_safety)
            .saturating_add(self.worker_stop_timeout.saturating_mul(6))
    }

    #[cfg(test)]
    fn maximum_shutdown_timeout(&self) -> Duration {
        self.worker_stop_timeout
            .saturating_add(self.graceful_shutdown_timeout)
            .saturating_add(self.forced_shutdown_timeout)
    }
}

#[derive(Debug, Clone)]
pub struct RealtimeInterpretationConfig {
    pub session_id: u64,
    pub input_gain: f32,
    pub policy: RealtimeInterpretationPolicy,
}

impl RealtimeInterpretationConfig {
    pub fn outgoing(session_id: u64, input_gain: f32) -> Self {
        Self {
            session_id,
            input_gain,
            policy: RealtimeInterpretationPolicy::outgoing(),
        }
    }

    pub fn incoming_spoken(session_id: u64) -> Self {
        Self {
            session_id,
            input_gain: 1.0,
            policy: RealtimeInterpretationPolicy::incoming_spoken(),
        }
    }

    pub(crate) fn with_capture_start_timeout(mut self, timeout: Duration) -> Self {
        self.policy.capture_start_timeout = timeout;
        self
    }
}

pub struct RealtimeInterpretationPorts {
    pub capture: Box<dyn AudioCapture>,
    pub output: Box<dyn TranslationAudioOutput>,
    pub translation: Box<dyn RealtimeTranslationSession>,
    pub translation_events: mpsc::Receiver<RealtimeTranslationEvent>,
}

#[derive(Clone)]
struct RuntimeStopReporter {
    tx: mpsc::UnboundedSender<RealtimeInterpretationStop>,
    startup_state: Arc<AtomicUsize>,
}

const RUNTIME_STARTING: usize = 0;
const RUNTIME_PUBLISHED: usize = 1;
const RUNTIME_TERMINAL: usize = 2;

impl RuntimeStopReporter {
    fn new(tx: mpsc::UnboundedSender<RealtimeInterpretationStop>) -> Self {
        Self {
            tx,
            startup_state: Arc::new(AtomicUsize::new(RUNTIME_STARTING)),
        }
    }

    fn error(&self, error: RealtimeInterpretationError) {
        self.send(RealtimeInterpretationStop::Error(error));
    }

    fn closed(&self) {
        self.send(RealtimeInterpretationStop::Closed);
    }

    fn send(&self, stop: RealtimeInterpretationStop) {
        if self.startup_state.swap(RUNTIME_TERMINAL, Ordering::SeqCst) != RUNTIME_TERMINAL {
            let _ = self.tx.send(stop);
        }
    }

    fn startup_state(&self) -> Arc<AtomicUsize> {
        self.startup_state.clone()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AudioQueueEnqueueError {
    Full(u64),
    Closed,
}

const DROP_PRESSURE_RECOVERY_SUCCESSES: u64 = 8;
const TRANSLATED_AUDIO_AUDIBLE_THRESHOLD: u16 = 128;

struct DropPressure {
    pressure: AtomicU64,
    recovery_successes: AtomicU64,
}

impl DropPressure {
    fn new() -> Self {
        Self {
            pressure: AtomicU64::new(0),
            recovery_successes: AtomicU64::new(0),
        }
    }

    fn record_drop(&self) -> u64 {
        self.recovery_successes.store(0, Ordering::Relaxed);
        self.pressure.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn record_success(&self) {
        let successes = self.recovery_successes.fetch_add(1, Ordering::Relaxed) + 1;
        if successes < DROP_PRESSURE_RECOVERY_SUCCESSES {
            return;
        }
        self.recovery_successes.store(0, Ordering::Relaxed);
        let _ = self
            .pressure
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |pressure| {
                Some(pressure.saturating_sub(1))
            });
    }

    #[cfg(test)]
    fn value(&self) -> u64 {
        self.pressure.load(Ordering::Relaxed)
    }
}

enum OutputCommand {
    Audio(Vec<i16>),
    SetGain {
        gain: f32,
        done: oneshot::Sender<Result<(), TranslationAudioOutputError>>,
    },
    BeginDrain,
    Drain {
        done: oneshot::Sender<()>,
    },
    Close {
        done: oneshot::Sender<Result<(), String>>,
    },
}

struct InputWorkerResult {
    client: Box<dyn RealtimeTranslationSession>,
    aborted: bool,
}

const UNSET_DIAGNOSTIC_MS: u64 = u64::MAX;

struct RuntimeDiagnostics {
    session_id: u64,
    input_source_name: &'static str,
    output_route_name: &'static str,
    started_at: Instant,
    input_audio_micros: AtomicU64,
    first_input_ms: AtomicU64,
    first_translated_text_ms: AtomicU64,
    first_translated_audio_ms: AtomicU64,
    input_queue_high_water: AtomicUsize,
    output_queue_high_water: AtomicUsize,
    output_pending_high_water_ms: AtomicU64,
    input_drop_count: AtomicU64,
    output_drop_count: AtomicU64,
    output_dropped_ms: AtomicU64,
    reported: AtomicBool,
}

impl RuntimeDiagnostics {
    fn new(session_id: u64, policy: &RealtimeInterpretationPolicy) -> Self {
        Self {
            session_id,
            input_source_name: policy.input_source_name,
            output_route_name: policy.output_route_name,
            started_at: Instant::now(),
            input_audio_micros: AtomicU64::new(0),
            first_input_ms: AtomicU64::new(UNSET_DIAGNOSTIC_MS),
            first_translated_text_ms: AtomicU64::new(UNSET_DIAGNOSTIC_MS),
            first_translated_audio_ms: AtomicU64::new(UNSET_DIAGNOSTIC_MS),
            input_queue_high_water: AtomicUsize::new(0),
            output_queue_high_water: AtomicUsize::new(0),
            output_pending_high_water_ms: AtomicU64::new(0),
            input_drop_count: AtomicU64::new(0),
            output_drop_count: AtomicU64::new(0),
            output_dropped_ms: AtomicU64::new(0),
            reported: AtomicBool::new(false),
        }
    }

    fn elapsed_ms(&self) -> u64 {
        self.started_at.elapsed().as_millis().min(u64::MAX as u128) as u64
    }

    fn mark_once(&self, metric: &AtomicU64) {
        let _ = metric.compare_exchange(
            UNSET_DIAGNOSTIC_MS,
            self.elapsed_ms(),
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }

    fn observe_input(&self, chunk: &AudioChunk, queue_depth: usize) {
        self.mark_once(&self.first_input_ms);
        let channels = u64::from(chunk.channels.max(1));
        let sample_rate = u64::from(chunk.sample_rate.max(1));
        let frames = chunk.data.len() as u64 / channels;
        self.input_audio_micros.fetch_add(
            frames.saturating_mul(1_000_000) / sample_rate,
            Ordering::Relaxed,
        );
        self.input_queue_high_water
            .fetch_max(queue_depth, Ordering::Relaxed);
    }

    fn observe_translated_text(&self) {
        self.mark_once(&self.first_translated_text_ms);
    }

    fn observe_translated_audio(&self, queue_depth: usize) {
        self.mark_once(&self.first_translated_audio_ms);
        self.output_queue_high_water
            .fetch_max(queue_depth, Ordering::Relaxed);
    }

    fn observe_output_pending(&self, pending: Duration) {
        self.output_pending_high_water_ms.fetch_max(
            pending.as_millis().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
    }

    fn observe_input_drop(&self) {
        self.input_drop_count.fetch_add(1, Ordering::Relaxed);
    }

    fn observe_output_drop(&self, duration: Duration, pending: Duration) {
        self.output_drop_count.fetch_add(1, Ordering::Relaxed);
        self.output_dropped_ms.fetch_add(
            duration.as_millis().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
        self.observe_output_pending(pending);
    }

    fn report(&self, stop_reason: &'static str, cleanup_ms: u64) {
        if self.reported.swap(true, Ordering::SeqCst) {
            return;
        }
        let optional_ms = |value: &AtomicU64| match value.load(Ordering::Relaxed) {
            UNSET_DIAGNOSTIC_MS => -1i64,
            value => value.min(i64::MAX as u64) as i64,
        };
        log::info!(
            "realtime_diagnostics session_id={} input_source={} output_route={} duration_ms={} input_audio_ms={} first_input_ms={} first_text_ms={} first_audio_ms={} input_queue_high_water={} output_queue_high_water={} output_pending_high_water_ms={} input_drops={} output_drops={} output_dropped_ms={} stop_reason={} cleanup_ms={}",
            self.session_id,
            self.input_source_name,
            self.output_route_name,
            self.elapsed_ms(),
            self.input_audio_micros.load(Ordering::Relaxed) / 1_000,
            optional_ms(&self.first_input_ms),
            optional_ms(&self.first_translated_text_ms),
            optional_ms(&self.first_translated_audio_ms),
            self.input_queue_high_water.load(Ordering::Relaxed),
            self.output_queue_high_water.load(Ordering::Relaxed),
            self.output_pending_high_water_ms.load(Ordering::Relaxed),
            self.input_drop_count.load(Ordering::Relaxed),
            self.output_drop_count.load(Ordering::Relaxed),
            self.output_dropped_ms.load(Ordering::Relaxed),
            stop_reason,
            cleanup_ms,
        );
    }
}

type InputWorkerTask = JoinHandle<Option<InputWorkerResult>>;
type OutputWorkerTask = JoinHandle<Option<Box<dyn TranslationAudioOutput>>>;

struct InputWorkerContext {
    callbacks: RealtimeInterpretationCallbacks,
    reporter: RuntimeStopReporter,
    stop_requested: Arc<AtomicBool>,
    input_gain: f32,
    policy: RealtimeInterpretationPolicy,
}

pub struct RealtimeInterpretationSession {
    capture: Option<Box<dyn AudioCapture>>,
    input_abort_tx: mpsc::UnboundedSender<()>,
    input_worker_task: Option<InputWorkerTask>,
    event_forwarder_task: Option<JoinHandle<()>>,
    output_tx: mpsc::Sender<OutputCommand>,
    output_abort_tx: watch::Sender<bool>,
    output_worker_task: Option<OutputWorkerTask>,
    silence_cadence_task: Option<JoinHandle<()>>,
    capture_health_task: Option<JoinHandle<()>>,
    suspension_watchdog_task: Option<JoinHandle<()>>,
    stop_requested: Arc<AtomicBool>,
    startup_state: Arc<AtomicUsize>,
    session_id: u64,
    policy: RealtimeInterpretationPolicy,
    diagnostics: Arc<RuntimeDiagnostics>,
}

#[derive(Clone)]
pub struct RealtimeInterpretationOutputControl {
    output_tx: mpsc::Sender<OutputCommand>,
    timeout: Duration,
    output_route_name: &'static str,
}

impl RealtimeInterpretationOutputControl {
    pub async fn set_gain(&self, gain: f32) -> Result<(), RealtimeInterpretationError> {
        set_output_gain(&self.output_tx, self.timeout, self.output_route_name, gain).await
    }
}

impl RealtimeInterpretationSession {
    pub async fn start(
        config: RealtimeInterpretationConfig,
        ports: RealtimeInterpretationPorts,
        callbacks: RealtimeInterpretationCallbacks,
    ) -> Result<
        (Self, mpsc::UnboundedReceiver<RealtimeInterpretationStop>),
        RealtimeInterpretationStartError,
    > {
        let (stop_tx, stop_rx) = mpsc::unbounded_channel();
        let reporter = RuntimeStopReporter::new(stop_tx);
        let startup_state = reporter.startup_state();
        let stop_requested = Arc::new(AtomicBool::new(false));
        let diagnostics = Arc::new(RuntimeDiagnostics::new(config.session_id, &config.policy));

        let (output_tx, output_rx) = mpsc::channel(config.policy.output_queue_capacity_chunks);
        let (output_abort_tx, output_abort_rx) = watch::channel(false);
        let output_worker_task = spawn_output_worker(
            ports.output,
            output_rx,
            output_abort_rx,
            reporter.clone(),
            config.policy.clone(),
            diagnostics.clone(),
        );
        let event_forwarder_task = spawn_event_forwarder(
            ports.translation_events,
            output_tx.clone(),
            callbacks.clone(),
            reporter.clone(),
            diagnostics.clone(),
        );

        let (input_tx, input_rx) = mpsc::channel(config.policy.input_queue_capacity_chunks);
        let (input_abort_tx, input_abort_rx) = mpsc::unbounded_channel();
        let input_worker_task = spawn_input_worker(
            input_rx,
            input_abort_rx,
            ports.translation,
            InputWorkerContext {
                callbacks,
                reporter: reporter.clone(),
                stop_requested: stop_requested.clone(),
                input_gain: config.input_gain,
                policy: config.policy.clone(),
            },
        );
        let capture_activity = Arc::new(CaptureActivity::new());
        let silence_cadence_task = spawn_silence_cadence_task(
            input_tx.clone(),
            reporter.clone(),
            stop_requested.clone(),
            capture_activity.clone(),
            config.policy.clone(),
        );
        let capture_callback = build_capture_callback(
            input_tx,
            reporter.clone(),
            stop_requested.clone(),
            config.policy.clone(),
            capture_activity,
            diagnostics.clone(),
        );

        let mut capture = ports.capture;
        let capture_health_probe = capture.health_probe();
        let capture_reporter = reporter.clone();
        let capture_stop_requested = stop_requested.clone();
        let input_source_name = config.policy.input_source_name;
        let capture_error_callback: AudioCaptureErrorCallback = Arc::new(move |error| {
            if !capture_stop_requested.load(Ordering::SeqCst) {
                let message = format!("{} capture failed: {}", input_source_name, error);
                let error = match error {
                    AudioError::DeviceNotFound(_) | AudioError::Capture(_) => {
                        RealtimeInterpretationError::InputDeviceLost(message)
                    }
                    _ => RealtimeInterpretationError::Processing(message),
                };
                capture_reporter.error(error);
            }
        });
        capture.set_terminal_error_callback(Some(capture_error_callback));

        let mut session = Self {
            capture: Some(capture),
            input_abort_tx,
            input_worker_task: Some(input_worker_task),
            event_forwarder_task: Some(event_forwarder_task),
            output_tx,
            output_abort_tx,
            output_worker_task: Some(output_worker_task),
            silence_cadence_task,
            capture_health_task: None,
            suspension_watchdog_task: None,
            stop_requested,
            startup_state,
            session_id: config.session_id,
            policy: config.policy,
            diagnostics,
        };

        let capture = session
            .capture
            .take()
            .expect("capture must exist during startup");
        let capture_start = start_owned_capture(
            capture,
            capture_callback,
            session.policy.capture_start_timeout,
        )
        .await;
        match capture_start {
            Ok(capture) => session.capture = Some(capture),
            Err(StartupCaptureError::Operation(error)) => {
                session
                    .shutdown(RealtimeInterpretationShutdown::Abort)
                    .await;
                return Err(RealtimeInterpretationStartError::Capture(error));
            }
            Err(StartupCaptureError::Timeout) => {
                let message = format!(
                    "{} capture start timed out after {} ms",
                    session.policy.input_source_name,
                    session.policy.capture_start_timeout.as_millis()
                );
                session
                    .shutdown(RealtimeInterpretationShutdown::Abort)
                    .await;
                return Err(RealtimeInterpretationStartError::Timeout(message));
            }
            Err(StartupCaptureError::Worker(message)) => {
                session
                    .shutdown(RealtimeInterpretationShutdown::Abort)
                    .await;
                return Err(RealtimeInterpretationStartError::Capture(
                    AudioError::Internal(message),
                ));
            }
        }
        session.capture_health_task = capture_health_probe.map(|probe| {
            spawn_capture_health_watchdog(
                probe,
                reporter.clone(),
                session.stop_requested.clone(),
                session.policy.capture_health_poll_interval,
                session.policy.input_source_name,
            )
        });
        session.suspension_watchdog_task = Some(spawn_suspension_watchdog(
            reporter,
            session.stop_requested.clone(),
            session.policy.suspension_watchdog_poll,
            session.policy.suspension_watchdog_threshold,
        ));

        Ok((session, stop_rx))
    }

    pub fn session_id(&self) -> u64 {
        self.session_id
    }

    /// Atomically hands a live runtime from startup to its supervisor. A terminal reporter and
    /// startup can never both win this transition.
    pub fn try_publish_startup(&self) -> bool {
        self.startup_state
            .compare_exchange(
                RUNTIME_STARTING,
                RUNTIME_PUBLISHED,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
    }

    pub fn output_control(&self) -> RealtimeInterpretationOutputControl {
        RealtimeInterpretationOutputControl {
            output_tx: self.output_tx.clone(),
            timeout: self.policy.worker_stop_timeout,
            output_route_name: self.policy.output_route_name,
        }
    }

    pub async fn shutdown(mut self, mode: RealtimeInterpretationShutdown) {
        let cleanup_started = Instant::now();
        self.stop_requested.store(true, Ordering::SeqCst);
        if let Some(task) = self.silence_cadence_task.take() {
            task.abort();
            let _ = task.await;
        }
        if let Some(task) = self.capture_health_task.take() {
            task.abort();
            let _ = task.await;
        }
        if let Some(task) = self.suspension_watchdog_task.take() {
            task.abort();
            let _ = task.await;
        }
        if let Some(capture) = self.capture.as_mut() {
            match tokio::time::timeout(self.policy.worker_stop_timeout, capture.stop_capture()).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => log::warn!(
                    "RealtimeInterpretationSession: capture stop failed for session {}: {}",
                    self.session_id,
                    error
                ),
                Err(_) => log::warn!(
                    "RealtimeInterpretationSession: capture stop timed out after {} ms for session {}",
                    self.policy.worker_stop_timeout.as_millis(),
                    self.session_id
                ),
            }
            capture.set_terminal_error_callback(None);
        }

        let stop_reason = match mode {
            RealtimeInterpretationShutdown::Graceful => {
                if tokio::time::timeout(
                    self.policy.graceful_shutdown_timeout,
                    self.shutdown_gracefully(),
                )
                .await
                .is_err()
                {
                    log::warn!(
                        "RealtimeInterpretationSession: graceful shutdown timed out after {} ms for session {}; forcing abort",
                        self.policy.graceful_shutdown_timeout.as_millis(),
                        self.session_id
                    );
                    if tokio::time::timeout(
                        self.policy.forced_shutdown_timeout,
                        self.shutdown_immediately(),
                    )
                    .await
                    .is_err()
                    {
                        log::warn!(
                            "RealtimeInterpretationSession: forced cleanup timed out after {} ms for session {}; dropping remaining workers",
                            self.policy.forced_shutdown_timeout.as_millis(),
                            self.session_id
                        );
                        "forced_timeout"
                    } else {
                        "graceful_timeout"
                    }
                } else {
                    "graceful"
                }
            }
            RealtimeInterpretationShutdown::Abort => {
                if tokio::time::timeout(
                    self.policy.forced_shutdown_timeout,
                    self.shutdown_immediately(),
                )
                .await
                .is_err()
                {
                    log::warn!(
                        "RealtimeInterpretationSession: abort cleanup timed out after {} ms for session {}; dropping remaining workers",
                        self.policy.forced_shutdown_timeout.as_millis(),
                        self.session_id
                    );
                    "abort_timeout"
                } else {
                    "abort"
                }
            }
        };

        self.capture = None;
        self.diagnostics.report(
            stop_reason,
            cleanup_started.elapsed().as_millis().min(u64::MAX as u128) as u64,
        );
        log::info!(
            "RealtimeInterpretationSession: session {} cleaned up ({:?})",
            self.session_id,
            mode
        );
    }

    async fn shutdown_gracefully(&mut self) {
        if self
            .output_tx
            .send(OutputCommand::BeginDrain)
            .await
            .is_err()
        {
            log::warn!(
                "RealtimeInterpretationSession: output worker stopped before drain mode for session {}",
                self.session_id
            );
        }

        let mut input = self.recover_input_client(false).await;
        if let Some(input) = input.as_mut().filter(|input| !input.aborted) {
            if let Err(error) = input
                .client
                .finish(self.policy.translation_finish_timeout)
                .await
            {
                log::warn!(
                    "RealtimeInterpretationSession: translation finish failed for session {}: {}",
                    self.session_id,
                    error
                );
                input.client.abort().await;
                input.aborted = true;
            }
        }

        self.wait_for_event_forwarder(false).await;
        self.drain_output().await;
        self.close_output().await;
    }

    async fn shutdown_immediately(&mut self) {
        let mut input = self.recover_input_client(true).await;
        if let Some(input) = input.as_mut().filter(|input| !input.aborted) {
            input.client.abort().await;
            input.aborted = true;
        }
        self.wait_for_event_forwarder(true).await;
        self.abort_output().await;
    }

    async fn recover_input_client(&mut self, abort_immediately: bool) -> Option<InputWorkerResult> {
        if abort_immediately {
            let _ = self.input_abort_tx.send(());
        }

        let initial_wait = {
            let task = self.input_worker_task.as_mut()?;
            tokio::time::timeout(self.policy.input_drain_timeout, task).await
        };
        match initial_wait {
            Ok(Ok(client)) => {
                self.input_worker_task.take();
                client
            }
            Ok(Err(error)) => {
                self.input_worker_task.take();
                log::warn!(
                    "RealtimeInterpretationSession: input worker join failed for session {}: {}",
                    self.session_id,
                    error
                );
                None
            }
            Err(_) if !abort_immediately => {
                log::warn!(
                    "RealtimeInterpretationSession: input drain timed out for session {}",
                    self.session_id
                );
                let _ = self.input_abort_tx.send(());
                self.wait_for_aborted_input_worker().await
            }
            Err(_) => {
                if let Some(task) = self.input_worker_task.take() {
                    task.abort();
                    let _ = task.await;
                }
                None
            }
        }
    }

    async fn wait_for_aborted_input_worker(&mut self) -> Option<InputWorkerResult> {
        let wait = {
            let task = self.input_worker_task.as_mut()?;
            tokio::time::timeout(self.policy.worker_stop_timeout, task).await
        };
        match wait {
            Ok(Ok(client)) => {
                self.input_worker_task.take();
                client
            }
            Ok(Err(error)) => {
                self.input_worker_task.take();
                log::warn!(
                    "RealtimeInterpretationSession: aborted input worker join failed for session {}: {}",
                    self.session_id,
                    error
                );
                None
            }
            Err(_) => {
                if let Some(task) = self.input_worker_task.take() {
                    task.abort();
                    let _ = task.await;
                }
                None
            }
        }
    }

    async fn wait_for_event_forwarder(&mut self, abort_immediately: bool) {
        let Some(task) = self.event_forwarder_task.as_mut() else {
            return;
        };
        if abort_immediately {
            task.abort();
            let wait = tokio::time::timeout(self.policy.worker_stop_timeout, task).await;
            if wait.is_err() {
                if let Some(task) = self.event_forwarder_task.take() {
                    task.abort();
                    let _ = task.await;
                }
            } else {
                self.event_forwarder_task.take();
            }
        } else {
            if let Err(error) = task.await {
                log::warn!(
                    "RealtimeInterpretationSession: event forwarder join failed for session {}: {}",
                    self.session_id,
                    error
                );
            }
            self.event_forwarder_task.take();
        }
    }

    async fn drain_output(&self) {
        let (done_tx, done_rx) = oneshot::channel();
        if self
            .output_tx
            .send(OutputCommand::Drain { done: done_tx })
            .await
            .is_err()
        {
            log::warn!(
                "RealtimeInterpretationSession: output worker stopped before final drain for session {}",
                self.session_id
            );
            return;
        }
        let wait = self
            .policy
            .output_drain_max
            .saturating_add(self.policy.output_drain_safety);
        if tokio::time::timeout(wait, done_rx).await.is_err() {
            log::warn!(
                "RealtimeInterpretationSession: output drain acknowledgement timed out after {} ms for session {}",
                wait.as_millis(),
                self.session_id
            );
        }
    }

    async fn close_output(&mut self) {
        let (done_tx, done_rx) = oneshot::channel();
        let close_sent = matches!(
            tokio::time::timeout(
                self.policy.worker_stop_timeout,
                self.output_tx.send(OutputCommand::Close { done: done_tx }),
            )
            .await,
            Ok(Ok(()))
        );
        if close_sent {
            if let Ok(Ok(Err(error))) =
                tokio::time::timeout(self.policy.worker_stop_timeout, done_rx).await
            {
                log::warn!(
                    "RealtimeInterpretationSession: output close failed for session {}: {}",
                    self.session_id,
                    error
                );
            }
        }

        let Some(task) = self.output_worker_task.as_mut() else {
            return;
        };
        let wait = tokio::time::timeout(self.policy.worker_stop_timeout, task).await;
        match wait {
            Ok(Ok(Some(mut output))) if !close_sent => {
                self.output_worker_task.take();
                if let Err(error) = output.close().await {
                    log::warn!(
                        "RealtimeInterpretationSession: recovered output close failed for session {}: {}",
                        self.session_id,
                        error
                    );
                }
            }
            Ok(Ok(_)) => {
                self.output_worker_task.take();
            }
            Ok(Err(error)) => {
                self.output_worker_task.take();
                log::warn!(
                    "RealtimeInterpretationSession: output worker join failed for session {}: {}",
                    self.session_id,
                    error
                );
            }
            Err(_) => {
                if let Some(task) = self.output_worker_task.take() {
                    task.abort();
                    let _ = task.await;
                }
            }
        }
    }

    async fn abort_output(&mut self) {
        let _ = self.output_abort_tx.send(true);
        let wait = {
            let Some(task) = self.output_worker_task.as_mut() else {
                return;
            };
            tokio::time::timeout(self.policy.worker_stop_timeout, task).await
        };

        match wait {
            Ok(Ok(Some(mut output))) => {
                self.output_worker_task.take();
                if let Ok(Err(error)) =
                    tokio::time::timeout(self.policy.worker_stop_timeout, output.close()).await
                {
                    log::warn!(
                        "RealtimeInterpretationSession: aborted output close failed for session {}: {}",
                        self.session_id,
                        error
                    );
                }
            }
            Ok(Ok(None)) => {
                self.output_worker_task.take();
            }
            Ok(Err(error)) => {
                self.output_worker_task.take();
                log::warn!(
                    "RealtimeInterpretationSession: aborted output worker join failed for session {}: {}",
                    self.session_id,
                    error
                );
            }
            Err(_) => {
                if let Some(task) = self.output_worker_task.take() {
                    task.abort();
                    let _ = task.await;
                }
            }
        }
    }
}

impl Drop for RealtimeInterpretationSession {
    fn drop(&mut self) {
        self.stop_requested.store(true, Ordering::SeqCst);
        let _ = self.input_abort_tx.send(());
        if let Some(task) = self.input_worker_task.take() {
            task.abort();
        }
        if let Some(task) = self.event_forwarder_task.take() {
            task.abort();
        }
        if let Some(task) = self.output_worker_task.take() {
            task.abort();
        }
        if let Some(task) = self.silence_cadence_task.take() {
            task.abort();
        }
        if let Some(task) = self.capture_health_task.take() {
            task.abort();
        }
        if let Some(task) = self.suspension_watchdog_task.take() {
            task.abort();
        }
        self.diagnostics.report("drop", 0);
    }
}

async fn set_output_gain(
    output_tx: &mpsc::Sender<OutputCommand>,
    timeout: Duration,
    output_route_name: &'static str,
    gain: f32,
) -> Result<(), RealtimeInterpretationError> {
    let (done_tx, done_rx) = oneshot::channel();
    tokio::time::timeout(
        timeout,
        output_tx.send(OutputCommand::SetGain {
            gain,
            done: done_tx,
        }),
    )
    .await
    .map_err(|_| {
        RealtimeInterpretationError::Timeout(
            "timed out while updating translated playback volume".into(),
        )
    })?
    .map_err(|_| {
        RealtimeInterpretationError::OutputDeviceLost(
            "local playback worker is no longer available".into(),
        )
    })?;

    tokio::time::timeout(timeout, done_rx)
        .await
        .map_err(|_| {
            RealtimeInterpretationError::Timeout(
                "timed out while applying translated playback volume".into(),
            )
        })?
        .map_err(|_| {
            RealtimeInterpretationError::OutputDeviceLost(
                "local playback worker stopped while applying volume".into(),
            )
        })?
        .map_err(|error| map_output_error(error, output_route_name))
}

fn spawn_input_worker(
    input_rx: mpsc::Receiver<AudioChunk>,
    abort_rx: mpsc::UnboundedReceiver<()>,
    client: Box<dyn RealtimeTranslationSession>,
    context: InputWorkerContext,
) -> InputWorkerTask {
    let panic_reporter = context.reporter.clone();
    tokio::spawn(async move {
        match AssertUnwindSafe(run_input_worker(input_rx, abort_rx, client, context))
            .catch_unwind()
            .await
        {
            Ok(result) => Some(result),
            Err(_) => {
                panic_reporter.error(RealtimeInterpretationError::Processing(
                    "input worker task panicked".to_string(),
                ));
                None
            }
        }
    })
}

async fn run_input_worker(
    mut input_rx: mpsc::Receiver<AudioChunk>,
    mut abort_rx: mpsc::UnboundedReceiver<()>,
    mut client: Box<dyn RealtimeTranslationSession>,
    context: InputWorkerContext,
) -> InputWorkerResult {
    let mut assembler = Pcm16FrameAssembler::new(context.policy.input_frame_samples);

    loop {
        let chunk = tokio::select! {
            biased;
            _ = abort_rx.recv() => {
                client.abort().await;
                return InputWorkerResult { client, aborted: true };
            }
            chunk = input_rx.recv() => chunk,
        };
        let Some(chunk) = chunk else {
            break;
        };

        let samples = if (context.input_gain - 1.0).abs() < f32::EPSILON {
            chunk.data
        } else {
            amplify_i16_samples(&chunk.data, context.input_gain)
        };
        call_interpretation_callback("input audio", || {
            (context.callbacks.on_input_audio)(&samples)
        });

        for frame in assembler.push(&samples) {
            if !append_frame_or_abort(&mut client, &frame, &mut abort_rx, &context.reporter).await {
                return InputWorkerResult {
                    client,
                    aborted: true,
                };
            }
        }
    }

    if let Some(frame) = assembler.finish_padded() {
        if !append_frame_or_abort(&mut client, &frame, &mut abort_rx, &context.reporter).await {
            return InputWorkerResult {
                client,
                aborted: true,
            };
        }
    }

    if !context.stop_requested.load(Ordering::SeqCst) {
        context
            .reporter
            .error(RealtimeInterpretationError::Connection(format!(
                "{} capture stopped unexpectedly",
                context.policy.input_source_name.to_lowercase()
            )));
        client.abort().await;
        return InputWorkerResult {
            client,
            aborted: true,
        };
    }

    InputWorkerResult {
        client,
        aborted: false,
    }
}

async fn append_frame_or_abort(
    client: &mut Box<dyn RealtimeTranslationSession>,
    frame: &[i16],
    abort_rx: &mut mpsc::UnboundedReceiver<()>,
    reporter: &RuntimeStopReporter,
) -> bool {
    tokio::select! {
        biased;
        _ = abort_rx.recv() => {
            client.abort().await;
            false
        }
        result = client.append_pcm16(frame) => {
            if let Err(error) = result {
                reporter.error(error.into());
                client.abort().await;
                false
            } else {
                true
            }
        }
    }
}

fn spawn_event_forwarder(
    translation_events: mpsc::Receiver<RealtimeTranslationEvent>,
    output_tx: mpsc::Sender<OutputCommand>,
    callbacks: RealtimeInterpretationCallbacks,
    reporter: RuntimeStopReporter,
    diagnostics: Arc<RuntimeDiagnostics>,
) -> JoinHandle<()> {
    let panic_reporter = reporter.clone();
    tokio::spawn(async move {
        if AssertUnwindSafe(run_event_forwarder(
            translation_events,
            output_tx,
            callbacks,
            reporter,
            diagnostics,
        ))
        .catch_unwind()
        .await
        .is_err()
        {
            panic_reporter.error(RealtimeInterpretationError::Processing(
                "event forwarder task panicked".to_string(),
            ));
        }
    })
}

async fn run_event_forwarder(
    mut translation_events: mpsc::Receiver<RealtimeTranslationEvent>,
    output_tx: mpsc::Sender<OutputCommand>,
    callbacks: RealtimeInterpretationCallbacks,
    reporter: RuntimeStopReporter,
    diagnostics: Arc<RuntimeDiagnostics>,
) {
    while let Some(event) = translation_events.recv().await {
        match event {
            RealtimeTranslationEvent::TranslatedAudio {
                pcm16,
                sample_rate,
                channels,
            } => {
                if sample_rate != 24_000 || channels != 1 {
                    reporter.error(RealtimeInterpretationError::Processing(format!(
                        "unsupported translated audio format: {} Hz, {} channels",
                        sample_rate, channels
                    )));
                    return;
                }
                if !translated_audio_has_audible_signal(&pcm16) {
                    continue;
                }
                if output_tx.send(OutputCommand::Audio(pcm16)).await.is_err() {
                    reporter.error(RealtimeInterpretationError::Processing(
                        "audio output worker stopped unexpectedly".to_string(),
                    ));
                    return;
                }
                diagnostics.observe_translated_audio(
                    output_tx
                        .max_capacity()
                        .saturating_sub(output_tx.capacity()),
                );
            }
            RealtimeTranslationEvent::TranslatedTextDelta(text) => {
                diagnostics.observe_translated_text();
                call_interpretation_callback("translated text", || {
                    (callbacks.on_translated_text)(text)
                });
            }
            RealtimeTranslationEvent::SourceTextDelta(text) => {
                call_interpretation_callback("source text", || (callbacks.on_source_text)(text));
            }
            RealtimeTranslationEvent::Failed(error) => {
                reporter.error(error.into());
                return;
            }
            RealtimeTranslationEvent::Closed => {
                reporter.closed();
                return;
            }
        }
    }
    reporter.closed();
}

fn translated_audio_has_audible_signal(samples: &[i16]) -> bool {
    samples
        .iter()
        .any(|sample| sample.unsigned_abs() >= TRANSLATED_AUDIO_AUDIBLE_THRESHOLD)
}

fn spawn_output_worker(
    output: Box<dyn TranslationAudioOutput>,
    output_rx: mpsc::Receiver<OutputCommand>,
    output_abort_rx: watch::Receiver<bool>,
    reporter: RuntimeStopReporter,
    policy: RealtimeInterpretationPolicy,
    diagnostics: Arc<RuntimeDiagnostics>,
) -> OutputWorkerTask {
    let panic_reporter = reporter.clone();
    tokio::spawn(async move {
        match AssertUnwindSafe(run_output_worker(
            output,
            output_rx,
            output_abort_rx,
            reporter,
            policy,
            diagnostics,
        ))
        .catch_unwind()
        .await
        {
            Ok(output) => Some(output),
            Err(_) => {
                panic_reporter.error(RealtimeInterpretationError::Processing(
                    "output worker task panicked".to_string(),
                ));
                None
            }
        }
    })
}

async fn run_output_worker(
    mut output: Box<dyn TranslationAudioOutput>,
    mut output_rx: mpsc::Receiver<OutputCommand>,
    mut output_abort_rx: watch::Receiver<bool>,
    reporter: RuntimeStopReporter,
    policy: RealtimeInterpretationPolicy,
    diagnostics: Arc<RuntimeDiagnostics>,
) -> Box<dyn TranslationAudioOutput> {
    let mut health_poll = tokio::time::interval(policy.output_health_poll_interval);
    let output_drop_pressure = DropPressure::new();
    let mut pending_overload_since = None;
    let mut graceful_draining = false;
    health_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    health_poll.tick().await;

    loop {
        tokio::select! {
            biased;
            _ = wait_for_output_abort(&mut output_abort_rx) => {
                return output;
            }
            command = output_rx.recv() => {
                let Some(command) = command else {
                    return output;
                };
                match command {
                    OutputCommand::Audio(samples) => {
                        let enqueue_result = tokio::select! {
                            biased;
                            _ = wait_for_output_abort(&mut output_abort_rx) => return output,
                            result = output.enqueue_pcm16(&samples) => result,
                        };
                        match enqueue_result {
                            Ok(AudioEnqueueOutcome::Queued { pending }) => {
                                output_drop_pressure.record_success();
                                diagnostics.observe_output_pending(pending);
                                if !graceful_draining
                                    && output_pending_is_overloaded(
                                        pending,
                                        &policy,
                                        &mut pending_overload_since,
                                        Instant::now(),
                                    )
                                {
                                    reporter.error(RealtimeInterpretationError::OutputOverload(format!(
                                        "{} output remained above the safe pending-audio limit; translation was stopped to avoid excessive delay",
                                        policy.output_route_name
                                    )));
                                    return output;
                                }
                            }
                            Ok(AudioEnqueueOutcome::DroppedOldest { duration, pending }) => {
                                let drop_pressure = output_drop_pressure.record_drop();
                                diagnostics.observe_output_drop(duration, pending);
                                log::warn!(
                                    "RealtimeInterpretationSession: {} output dropped {} ms (pending={} ms, pressure={})",
                                    policy.output_route_name,
                                    duration.as_millis(),
                                    pending.as_millis(),
                                    drop_pressure
                                );
                                if drop_pressure >= policy.output_overload_drop_threshold {
                                    reporter.error(RealtimeInterpretationError::OutputOverload(format!(
                                        "{} output repeatedly overflowed; translation was stopped to avoid incomplete delayed speech",
                                        policy.output_route_name
                                    )));
                                    return output;
                                }
                            }
                            Err(error) => {
                                reporter.error(map_output_error(error, policy.output_route_name));
                                return output;
                            }
                        }
                    }
                    OutputCommand::SetGain { gain, done } => {
                        let _ = done.send(output.set_gain(gain));
                    }
                    OutputCommand::BeginDrain => {
                        graceful_draining = true;
                        pending_overload_since = None;
                        output.begin_drain_mode();
                    }
                    OutputCommand::Drain { done } => {
                        if drain_output_tail(output.as_ref(), &policy, &mut output_abort_rx).await {
                            return output;
                        }
                        let _ = done.send(());
                    }
                    OutputCommand::Close { done } => {
                        let result = tokio::select! {
                            biased;
                            _ = wait_for_output_abort(&mut output_abort_rx) => return output,
                            result = output.close() => result.map_err(|error| error.to_string()),
                        };
                        let _ = done.send(result);
                        return output;
                    }
                }
            }
            _ = health_poll.tick() => {
                if let Err(error) = output.health_check() {
                    reporter.error(map_output_error(error, policy.output_route_name));
                    return output;
                }
                let pending = output.pending_playback_duration();
                diagnostics.observe_output_pending(pending);
                if !graceful_draining
                    && output_pending_is_overloaded(
                        pending,
                        &policy,
                        &mut pending_overload_since,
                        Instant::now(),
                    )
                {
                    reporter.error(RealtimeInterpretationError::OutputOverload(format!(
                        "{} output remained above the safe pending-audio limit; translation was stopped to avoid excessive delay",
                        policy.output_route_name
                    )));
                    return output;
                }
            }
        }
    }
}

fn output_pending_is_overloaded(
    pending: Duration,
    policy: &RealtimeInterpretationPolicy,
    high_since: &mut Option<Instant>,
    now: Instant,
) -> bool {
    let Some(overload) = policy.output_pending_overload.as_ref() else {
        *high_since = None;
        return false;
    };
    if pending <= overload.limit {
        *high_since = None;
        return false;
    }
    let started = high_since.get_or_insert(now);
    now.saturating_duration_since(*started) >= overload.grace
}

fn map_output_error(
    error: TranslationAudioOutputError,
    route_name: &str,
) -> RealtimeInterpretationError {
    let message = format!("{} output failed: {}", route_name, error);
    match error {
        TranslationAudioOutputError::Device(_)
        | TranslationAudioOutputError::Stream(_)
        | TranslationAudioOutputError::Closed => {
            RealtimeInterpretationError::OutputDeviceLost(message)
        }
        TranslationAudioOutputError::Configuration(_)
        | TranslationAudioOutputError::Resample(_) => {
            RealtimeInterpretationError::Processing(message)
        }
    }
}

async fn drain_output_tail(
    output: &dyn TranslationAudioOutput,
    policy: &RealtimeInterpretationPolicy,
    output_abort_rx: &mut watch::Receiver<bool>,
) -> bool {
    if *output_abort_rx.borrow() {
        return true;
    }
    let initial_pending = match output.prepare_for_drain() {
        Ok(pending) => pending,
        Err(error) => {
            log::warn!(
                "RealtimeInterpretationSession: output drain prepare failed: {}",
                error
            );
            return false;
        }
    };
    if initial_pending <= policy.output_drain_empty_threshold {
        return false;
    }

    let wait_budget = initial_pending
        .saturating_add(policy.output_drain_safety)
        .min(policy.output_drain_max);
    let deadline = tokio::time::Instant::now() + wait_budget;
    loop {
        let pending = output.pending_playback_duration();
        if pending <= policy.output_drain_empty_threshold {
            return false;
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            log::warn!(
                "RealtimeInterpretationSession: output tail drain timed out (pending={} ms)",
                pending.as_millis()
            );
            return false;
        }
        tokio::select! {
            biased;
            _ = wait_for_output_abort(output_abort_rx) => return true,
            _ = tokio::time::sleep(
                policy
                    .output_drain_poll
                    .min(pending)
                    .min(deadline.saturating_duration_since(now)),
            ) => {}
        }
    }
}

async fn wait_for_output_abort(output_abort_rx: &mut watch::Receiver<bool>) {
    loop {
        if *output_abort_rx.borrow() {
            return;
        }
        if output_abort_rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

fn build_capture_callback(
    input_tx: mpsc::Sender<AudioChunk>,
    reporter: RuntimeStopReporter,
    stop_requested: Arc<AtomicBool>,
    policy: RealtimeInterpretationPolicy,
    capture_activity: Arc<CaptureActivity>,
    diagnostics: Arc<RuntimeDiagnostics>,
) -> AudioChunkCallback {
    let drop_pressure = DropPressure::new();
    Arc::new(move |chunk| {
        if stop_requested.load(Ordering::SeqCst) {
            return;
        }
        capture_activity.touch();
        let queue_capacity = input_tx.max_capacity();
        diagnostics.observe_input(
            &chunk,
            queue_capacity
                .saturating_sub(input_tx.capacity())
                .saturating_add(1)
                .min(queue_capacity),
        );
        match try_enqueue_audio_chunk(&input_tx, chunk, &drop_pressure) {
            Ok(()) => {}
            Err(AudioQueueEnqueueError::Full(drops))
                if drops == policy.input_overload_drop_threshold =>
            {
                diagnostics.observe_input_drop();
                reporter.error(RealtimeInterpretationError::InputOverload(format!(
                    "{} audio processing cannot keep up; translation was stopped to avoid silently losing speech",
                    policy.input_source_name
                )));
            }
            Err(AudioQueueEnqueueError::Closed) if !stop_requested.load(Ordering::SeqCst) => {
                reporter.error(RealtimeInterpretationError::Processing(format!(
                    "{} audio processor stopped unexpectedly",
                    policy.input_source_name
                )));
            }
            Err(AudioQueueEnqueueError::Full(_)) => diagnostics.observe_input_drop(),
            Err(_) => {}
        }
    })
}

struct CaptureActivity {
    epoch: Instant,
    last_audio_ms: AtomicU64,
}

impl CaptureActivity {
    fn new() -> Self {
        Self {
            epoch: Instant::now(),
            last_audio_ms: AtomicU64::new(0),
        }
    }

    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis().min(u64::MAX as u128) as u64
    }

    fn touch(&self) {
        self.last_audio_ms.store(self.now_ms(), Ordering::Relaxed);
    }

    fn gap(&self) -> Duration {
        Duration::from_millis(
            self.now_ms()
                .saturating_sub(self.last_audio_ms.load(Ordering::Relaxed)),
        )
    }
}

fn spawn_silence_cadence_task(
    input_tx: mpsc::Sender<AudioChunk>,
    reporter: RuntimeStopReporter,
    stop_requested: Arc<AtomicBool>,
    capture_activity: Arc<CaptureActivity>,
    policy: RealtimeInterpretationPolicy,
) -> Option<JoinHandle<()>> {
    let cadence = policy.silence_cadence.clone()?;
    Some(tokio::spawn(async move {
        let mut interval = tokio::time::interval(cadence.interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        interval.tick().await;
        loop {
            interval.tick().await;
            if stop_requested.load(Ordering::SeqCst) {
                return;
            }
            if capture_activity.gap() < cadence.gap_threshold {
                continue;
            }
            let silence = AudioChunk::new(
                vec![0; policy.input_frame_samples],
                cadence.sample_rate,
                cadence.channels,
            );
            match input_tx.try_send(silence) {
                Ok(()) => capture_activity.touch(),
                Err(mpsc::error::TrySendError::Full(_)) => {}
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    if !stop_requested.load(Ordering::SeqCst) {
                        reporter.error(RealtimeInterpretationError::Processing(format!(
                            "{} silence cadence stopped unexpectedly",
                            policy.input_source_name
                        )));
                    }
                    return;
                }
            }
        }
    }))
}

fn suspension_gap_exceeded(
    monotonic_elapsed: Duration,
    wall_elapsed: Option<Duration>,
    threshold: Duration,
) -> bool {
    monotonic_elapsed >= threshold || wall_elapsed.is_some_and(|elapsed| elapsed >= threshold)
}

fn spawn_capture_health_watchdog(
    probe: AudioCaptureHealthProbe,
    reporter: RuntimeStopReporter,
    stop_requested: Arc<AtomicBool>,
    poll: Duration,
    input_source_name: &'static str,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(poll);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        interval.tick().await;
        loop {
            interval.tick().await;
            if stop_requested.load(Ordering::SeqCst) {
                return;
            }
            if !probe() {
                reporter.error(RealtimeInterpretationError::InputDeviceLost(format!(
                    "{} capture stopped unexpectedly",
                    input_source_name
                )));
                return;
            }
        }
    })
}

fn spawn_suspension_watchdog(
    reporter: RuntimeStopReporter,
    stop_requested: Arc<AtomicBool>,
    poll: Duration,
    threshold: Duration,
) -> JoinHandle<()> {
    let mut previous_monotonic = Instant::now();
    let mut previous_wall = SystemTime::now();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(poll).await;
            if stop_requested.load(Ordering::SeqCst) {
                return;
            }

            let current_monotonic = Instant::now();
            let current_wall = SystemTime::now();
            let monotonic_elapsed = current_monotonic.saturating_duration_since(previous_monotonic);
            let wall_elapsed = current_wall.duration_since(previous_wall).ok();

            if suspension_gap_exceeded(monotonic_elapsed, wall_elapsed, threshold) {
                let detected_elapsed = wall_elapsed.unwrap_or_default().max(monotonic_elapsed);
                reporter.error(RealtimeInterpretationError::Processing(format!(
                    "translation runtime was suspended for {} ms; restart translation after the system wakes",
                    detected_elapsed.as_millis()
                )));
                return;
            }

            previous_monotonic = current_monotonic;
            previous_wall = current_wall;
        }
    })
}

fn try_enqueue_audio_chunk(
    input_tx: &mpsc::Sender<AudioChunk>,
    chunk: AudioChunk,
    drop_pressure: &DropPressure,
) -> Result<(), AudioQueueEnqueueError> {
    match input_tx.try_send(chunk) {
        Ok(()) => {
            drop_pressure.record_success();
            Ok(())
        }
        Err(mpsc::error::TrySendError::Full(_)) => {
            let drops = drop_pressure.record_drop();
            Err(AudioQueueEnqueueError::Full(drops))
        }
        Err(mpsc::error::TrySendError::Closed(_)) => Err(AudioQueueEnqueueError::Closed),
    }
}

fn call_interpretation_callback(label: &str, callback: impl FnOnce()) {
    if std::panic::catch_unwind(AssertUnwindSafe(callback)).is_err() {
        log::error!("RealtimeInterpretationSession: {} callback panicked", label);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        AudioConfig, AudioError, RealtimeTranslationConfig, TranslationAudioOutputConfig,
        TranslationAudioOutputResult,
    };
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex as StdMutex;

    #[test]
    fn runtime_reporter_delivers_exactly_one_terminal_signal() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let reporter = RuntimeStopReporter::new(tx);

        reporter.error(RealtimeInterpretationError::Processing("first".into()));
        reporter.closed();

        assert!(matches!(
            rx.try_recv(),
            Ok(RealtimeInterpretationStop::Error(
                RealtimeInterpretationError::Processing(message)
            )) if message == "first"
        ));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn terminal_signal_atomically_beats_startup_publication() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let reporter = RuntimeStopReporter::new(tx);
        let startup_state = reporter.startup_state();

        reporter.closed();

        assert!(startup_state
            .compare_exchange(
                RUNTIME_STARTING,
                RUNTIME_PUBLISHED,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_err());
        assert_eq!(rx.try_recv(), Ok(RealtimeInterpretationStop::Closed));
    }

    #[test]
    fn suspension_gap_checks_monotonic_and_wall_clocks() {
        let threshold = Duration::from_secs(8);

        assert!(!suspension_gap_exceeded(
            Duration::from_secs(1),
            Some(Duration::from_secs(1)),
            threshold
        ));
        assert!(suspension_gap_exceeded(
            threshold,
            Some(Duration::from_secs(1)),
            threshold
        ));
        assert!(suspension_gap_exceeded(
            Duration::from_secs(1),
            Some(threshold),
            threshold
        ));
        assert!(!suspension_gap_exceeded(
            Duration::from_secs(1),
            None,
            threshold
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn suspension_watchdog_reports_terminal_cleanup_after_runtime_pause() {
        let (stop_tx, mut stop_rx) = mpsc::unbounded_channel();
        let stop_requested = Arc::new(AtomicBool::new(false));
        let task = spawn_suspension_watchdog(
            RuntimeStopReporter::new(stop_tx),
            stop_requested,
            Duration::from_millis(5),
            Duration::from_millis(20),
        );

        std::thread::sleep(Duration::from_millis(40));

        let event = tokio::time::timeout(Duration::from_secs(1), stop_rx.recv())
            .await
            .expect("watchdog should report after a runtime pause")
            .expect("watchdog stop channel should remain open");
        assert!(matches!(
            event,
            RealtimeInterpretationStop::Error(RealtimeInterpretationError::Processing(message))
                if message.contains("suspended") && message.contains("restart translation")
        ));
        task.await.expect("watchdog task should finish cleanly");
    }

    #[tokio::test]
    async fn suspension_watchdog_stops_without_reporting_an_error() {
        let (stop_tx, mut stop_rx) = mpsc::unbounded_channel();
        let stop_requested = Arc::new(AtomicBool::new(true));
        let task = spawn_suspension_watchdog(
            RuntimeStopReporter::new(stop_tx),
            stop_requested,
            Duration::from_millis(1),
            Duration::from_millis(10),
        );

        task.await.expect("watchdog task should finish cleanly");
        assert!(stop_rx.try_recv().is_err());
    }

    #[test]
    fn input_queue_drop_pressure_requires_sustained_success_to_recover() {
        let (tx, mut rx) = mpsc::channel(1);
        let drops = DropPressure::new();
        let first = AudioChunk::new(vec![1], 24_000, 1);
        let second = AudioChunk::new(vec![2], 24_000, 1);

        assert_eq!(try_enqueue_audio_chunk(&tx, first, &drops), Ok(()));
        assert_eq!(
            try_enqueue_audio_chunk(&tx, second.clone(), &drops),
            Err(AudioQueueEnqueueError::Full(1))
        );
        assert!(rx.try_recv().is_ok());
        assert_eq!(try_enqueue_audio_chunk(&tx, second, &drops), Ok(()));
        assert_eq!(drops.value(), 1);

        for _ in 1..DROP_PRESSURE_RECOVERY_SUCCESSES {
            drops.record_success();
        }
        assert_eq!(drops.value(), 0);
    }

    #[test]
    fn alternating_drops_accumulate_overload_pressure() {
        let pressure = DropPressure::new();

        for expected in 1..=8 {
            assert_eq!(pressure.record_drop(), expected);
            pressure.record_success();
        }

        assert_eq!(pressure.value(), 8);
    }

    #[tokio::test]
    async fn repeated_input_queue_drops_are_a_terminal_overload_error() {
        let (input_tx, _input_rx) = mpsc::channel(1);
        let (stop_tx, mut stop_rx) = mpsc::unbounded_channel();
        let mut policy = RealtimeInterpretationPolicy::incoming_spoken();
        policy.input_overload_drop_threshold = 2;
        let diagnostics = Arc::new(RuntimeDiagnostics::new(10, &policy));
        let callback = build_capture_callback(
            input_tx,
            RuntimeStopReporter::new(stop_tx),
            Arc::new(AtomicBool::new(false)),
            policy,
            Arc::new(CaptureActivity::new()),
            diagnostics.clone(),
        );

        callback(AudioChunk::new(vec![1; 4_800], 24_000, 1));
        callback(AudioChunk::new(vec![2; 4_800], 24_000, 1));
        callback(AudioChunk::new(vec![3; 4_800], 24_000, 1));

        assert!(matches!(
            stop_rx.recv().await,
            Some(RealtimeInterpretationStop::Error(
                RealtimeInterpretationError::InputOverload(message)
            )) if message.contains("cannot keep up")
        ));
        assert_eq!(diagnostics.input_drop_count.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn capture_callback_rejects_audio_after_stop_is_requested() {
        let (input_tx, mut input_rx) = mpsc::channel(1);
        let (stop_tx, mut stop_rx) = mpsc::unbounded_channel();
        let policy = RealtimeInterpretationPolicy::incoming_spoken();
        let diagnostics = Arc::new(RuntimeDiagnostics::new(12, &policy));
        let callback = build_capture_callback(
            input_tx,
            RuntimeStopReporter::new(stop_tx),
            Arc::new(AtomicBool::new(true)),
            policy,
            Arc::new(CaptureActivity::new()),
            diagnostics.clone(),
        );

        callback(AudioChunk::new(vec![1; 4_800], 24_000, 1));

        assert!(input_rx.try_recv().is_err());
        assert!(stop_rx.try_recv().is_err());
        assert_eq!(
            diagnostics.first_input_ms.load(Ordering::Relaxed),
            UNSET_DIAGNOSTIC_MS
        );
    }

    #[test]
    fn runtime_diagnostics_track_latency_queue_and_drop_bounds_without_payloads() {
        let policy = RealtimeInterpretationPolicy::incoming_spoken();
        let diagnostics = RuntimeDiagnostics::new(11, &policy);
        diagnostics.observe_input(&AudioChunk::new(vec![7; 2_400], 24_000, 1), 3);
        diagnostics.observe_translated_text();
        diagnostics.observe_translated_audio(4);
        diagnostics.observe_output_pending(Duration::from_millis(125));
        diagnostics.observe_input_drop();
        diagnostics.observe_output_drop(Duration::from_millis(80), Duration::from_millis(240));

        assert_eq!(
            diagnostics.input_audio_micros.load(Ordering::Relaxed),
            100_000
        );
        assert_ne!(
            diagnostics.first_input_ms.load(Ordering::Relaxed),
            UNSET_DIAGNOSTIC_MS
        );
        assert_ne!(
            diagnostics.first_translated_text_ms.load(Ordering::Relaxed),
            UNSET_DIAGNOSTIC_MS
        );
        assert_ne!(
            diagnostics
                .first_translated_audio_ms
                .load(Ordering::Relaxed),
            UNSET_DIAGNOSTIC_MS
        );
        assert_eq!(
            diagnostics.input_queue_high_water.load(Ordering::Relaxed),
            3
        );
        assert_eq!(
            diagnostics.output_queue_high_water.load(Ordering::Relaxed),
            4
        );
        assert_eq!(
            diagnostics
                .output_pending_high_water_ms
                .load(Ordering::Relaxed),
            240
        );
        assert_eq!(diagnostics.input_drop_count.load(Ordering::Relaxed), 1);
        assert_eq!(diagnostics.output_drop_count.load(Ordering::Relaxed), 1);
        assert_eq!(diagnostics.output_dropped_ms.load(Ordering::Relaxed), 80);
    }

    #[tokio::test]
    async fn incoming_silence_cadence_injects_one_bounded_frame_after_gap() {
        let (input_tx, mut input_rx) = mpsc::channel(1);
        let (stop_tx, mut stop_rx) = mpsc::unbounded_channel();
        let stop_requested = Arc::new(AtomicBool::new(false));
        let activity = Arc::new(CaptureActivity::new());
        let mut policy = RealtimeInterpretationPolicy::incoming_spoken();
        let cadence = policy.silence_cadence.as_mut().unwrap();
        cadence.gap_threshold = Duration::from_millis(5);
        cadence.interval = Duration::from_millis(5);
        let task = spawn_silence_cadence_task(
            input_tx,
            RuntimeStopReporter::new(stop_tx),
            stop_requested.clone(),
            activity,
            policy,
        )
        .unwrap();

        let chunk = tokio::time::timeout(Duration::from_secs(1), input_rx.recv())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(chunk.sample_rate, 24_000);
        assert_eq!(chunk.channels, 1);
        assert_eq!(chunk.data.len(), 4_800);
        assert!(chunk.data.iter().all(|sample| *sample == 0));
        assert!(stop_rx.try_recv().is_err());
        stop_requested.store(true, Ordering::SeqCst);
        task.await.unwrap();
    }

    #[test]
    fn neutral_translation_errors_keep_typed_application_category() {
        let error = RealtimeInterpretationError::from(RealtimeTranslationError::Timeout(
            "provider timeout".into(),
        ));

        assert!(matches!(
            error,
            RealtimeInterpretationError::Timeout(message)
                if message.contains("provider timeout")
        ));
    }

    #[test]
    fn no_op_callbacks_accept_all_contract_events() {
        let callbacks = RealtimeInterpretationCallbacks::no_op();
        (callbacks.on_input_audio)(&[1]);
        (callbacks.on_source_text)("source".into());
        (callbacks.on_translated_text)("translation".into());
    }

    #[test]
    fn incoming_policy_bounds_queue_and_derives_complete_shutdown_budget() {
        let policy = RealtimeInterpretationPolicy::incoming_spoken();

        assert!(policy.input_queue_capacity_chunks * 30 <= 2_000);
        assert_eq!(policy.input_overload_drop_threshold, 8);
        assert_eq!(
            policy.graceful_shutdown_timeout,
            policy.required_graceful_shutdown_timeout()
        );
        assert_eq!(
            policy.output_drain_max,
            TranslationAudioOutputConfig::incoming_spoken_translation()
                .drain_max_buffered_duration
                .saturating_add(policy.output_drain_safety)
        );
        assert!(policy.graceful_shutdown_timeout >= Duration::from_secs(39));
        assert!(policy.maximum_shutdown_timeout() >= Duration::from_secs(41));
        assert!(policy.maximum_shutdown_timeout() < Duration::from_secs(42));
    }

    #[test]
    fn sustained_high_pending_audio_is_terminal_but_a_short_burst_recovers() {
        let policy = RealtimeInterpretationPolicy::incoming_spoken();
        let overload = policy.output_pending_overload.as_ref().unwrap();
        let started = Instant::now();
        let mut high_since = None;

        assert!(!output_pending_is_overloaded(
            overload.limit + Duration::from_millis(1),
            &policy,
            &mut high_since,
            started,
        ));
        assert!(!output_pending_is_overloaded(
            overload.limit + Duration::from_millis(1),
            &policy,
            &mut high_since,
            started + overload.grace - Duration::from_millis(1),
        ));
        assert!(!output_pending_is_overloaded(
            overload.limit,
            &policy,
            &mut high_since,
            started + overload.grace,
        ));
        assert!(!output_pending_is_overloaded(
            overload.limit + Duration::from_millis(1),
            &policy,
            &mut high_since,
            started + overload.grace,
        ));
        assert!(output_pending_is_overloaded(
            overload.limit + Duration::from_millis(1),
            &policy,
            &mut high_since,
            started + overload.grace.saturating_mul(2),
        ));
    }

    #[tokio::test]
    async fn translated_callback_panic_does_not_hide_terminal_close() {
        let (event_tx, event_rx) = mpsc::channel(2);
        let (output_tx, _output_rx) = mpsc::channel(1);
        let (stop_tx, mut stop_rx) = mpsc::unbounded_channel();
        let callbacks = RealtimeInterpretationCallbacks {
            on_translated_text: Arc::new(|_| panic!("simulated callback panic")),
            on_source_text: Arc::new(|_| {}),
            on_input_audio: Arc::new(|_| {}),
        };

        event_tx
            .send(RealtimeTranslationEvent::TranslatedTextDelta("text".into()))
            .await
            .unwrap();
        event_tx
            .send(RealtimeTranslationEvent::Closed)
            .await
            .unwrap();
        run_event_forwarder(
            event_rx,
            output_tx,
            callbacks,
            RuntimeStopReporter::new(stop_tx),
            Arc::new(RuntimeDiagnostics::new(
                1,
                &RealtimeInterpretationPolicy::outgoing(),
            )),
        )
        .await;

        assert_eq!(stop_rx.try_recv(), Ok(RealtimeInterpretationStop::Closed));
    }

    #[tokio::test]
    async fn silent_provider_audio_is_not_forwarded_to_playback() {
        let (event_tx, event_rx) = mpsc::channel(3);
        let (output_tx, mut output_rx) = mpsc::channel(2);
        let (stop_tx, mut stop_rx) = mpsc::unbounded_channel();

        event_tx
            .send(RealtimeTranslationEvent::TranslatedAudio {
                pcm16: vec![-11, 0, 27],
                sample_rate: 24_000,
                channels: 1,
            })
            .await
            .unwrap();
        event_tx
            .send(RealtimeTranslationEvent::TranslatedAudio {
                pcm16: vec![0, 127, 128],
                sample_rate: 24_000,
                channels: 1,
            })
            .await
            .unwrap();
        event_tx
            .send(RealtimeTranslationEvent::Closed)
            .await
            .unwrap();

        run_event_forwarder(
            event_rx,
            output_tx,
            RealtimeInterpretationCallbacks::no_op(),
            RuntimeStopReporter::new(stop_tx),
            Arc::new(RuntimeDiagnostics::new(
                1,
                &RealtimeInterpretationPolicy::incoming_spoken(),
            )),
        )
        .await;

        match output_rx.recv().await {
            Some(OutputCommand::Audio(samples)) => assert_eq!(samples, vec![0, 127, 128]),
            _ => panic!("audible provider audio must reach playback"),
        }
        assert!(output_rx.try_recv().is_err());
        assert_eq!(stop_rx.try_recv(), Ok(RealtimeInterpretationStop::Closed));
    }

    #[derive(Default)]
    struct ContractState {
        capture_start_entered: AtomicBool,
        capture_running: AtomicBool,
        capture_stopped: AtomicBool,
        capture_stop_entered: AtomicBool,
        append_calls: AtomicUsize,
        finish_calls: AtomicUsize,
        abort_calls: AtomicUsize,
        finish_audio_chunks: AtomicUsize,
        output_closed: AtomicBool,
        output_enqueue_entered: AtomicBool,
        output_enqueue_delay_ms: AtomicU64,
        output_pending_per_chunk_ms: AtomicU64,
        output_pending_ms: AtomicU64,
        output_pending_at_close_ms: AtomicU64,
        output_prepare_calls: AtomicUsize,
        output_dropped: AtomicBool,
        output_should_drop_oldest: AtomicBool,
        capture_error_callback: StdMutex<Option<AudioCaptureErrorCallback>>,
        input_samples: StdMutex<Vec<i16>>,
        output_samples: StdMutex<Vec<i16>>,
    }

    const CONTRACT_AUDIO_SAMPLES: [i16; 3] = [1_000, 2_000, 3_000];
    const CONTRACT_DRAIN_AUDIO_BASE: i16 = 4_000;

    struct ContractCapture {
        state: Arc<ContractState>,
        callback: Option<AudioChunkCallback>,
    }

    #[async_trait::async_trait]
    impl AudioCapture for ContractCapture {
        async fn initialize(&mut self, _config: AudioConfig) -> crate::domain::AudioResult<()> {
            Ok(())
        }

        async fn start_capture(
            &mut self,
            callback: AudioChunkCallback,
        ) -> crate::domain::AudioResult<()> {
            self.state.capture_running.store(true, Ordering::SeqCst);
            callback(AudioChunk::new(vec![1_200; 4_800], 24_000, 1));
            self.callback = Some(callback);
            Ok(())
        }

        async fn stop_capture(&mut self) -> crate::domain::AudioResult<()> {
            self.state
                .capture_stop_entered
                .store(true, Ordering::SeqCst);
            self.state.capture_running.store(false, Ordering::SeqCst);
            self.state.capture_stopped.store(true, Ordering::SeqCst);
            self.callback = None;
            Ok(())
        }

        fn set_terminal_error_callback(&mut self, callback: Option<AudioCaptureErrorCallback>) {
            *self.state.capture_error_callback.lock().unwrap() = callback;
        }

        fn health_probe(&self) -> Option<AudioCaptureHealthProbe> {
            let state = self.state.clone();
            Some(Arc::new(move || {
                state.capture_running.load(Ordering::SeqCst)
            }))
        }

        fn is_capturing(&self) -> bool {
            self.callback.is_some()
        }

        fn config(&self) -> AudioConfig {
            AudioConfig {
                sample_rate: 24_000,
                channels: 1,
                buffer_size: 4_800,
            }
        }
    }

    struct BlockingStopCapture {
        state: Arc<ContractState>,
        callback: Option<AudioChunkCallback>,
    }

    struct BlockingStartCapture {
        state: Arc<ContractState>,
    }

    #[async_trait::async_trait]
    impl AudioCapture for BlockingStartCapture {
        async fn initialize(&mut self, _config: AudioConfig) -> crate::domain::AudioResult<()> {
            Ok(())
        }

        async fn start_capture(
            &mut self,
            _callback: AudioChunkCallback,
        ) -> crate::domain::AudioResult<()> {
            self.state
                .capture_start_entered
                .store(true, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(80));
            Ok(())
        }

        async fn stop_capture(&mut self) -> crate::domain::AudioResult<()> {
            self.state.capture_stopped.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn is_capturing(&self) -> bool {
            false
        }

        fn config(&self) -> AudioConfig {
            AudioConfig {
                sample_rate: 24_000,
                channels: 1,
                buffer_size: 4_800,
            }
        }
    }

    #[async_trait::async_trait]
    impl AudioCapture for BlockingStopCapture {
        async fn initialize(&mut self, _config: AudioConfig) -> crate::domain::AudioResult<()> {
            Ok(())
        }

        async fn start_capture(
            &mut self,
            callback: AudioChunkCallback,
        ) -> crate::domain::AudioResult<()> {
            self.callback = Some(callback);
            Ok(())
        }

        async fn stop_capture(&mut self) -> crate::domain::AudioResult<()> {
            self.state
                .capture_stop_entered
                .store(true, Ordering::SeqCst);
            std::future::pending::<crate::domain::AudioResult<()>>().await
        }

        fn is_capturing(&self) -> bool {
            self.callback.is_some()
        }

        fn config(&self) -> AudioConfig {
            AudioConfig {
                sample_rate: 24_000,
                channels: 1,
                buffer_size: 4_800,
            }
        }
    }

    struct ContractTranslationSession {
        state: Arc<ContractState>,
        events: mpsc::Sender<RealtimeTranslationEvent>,
    }

    #[async_trait::async_trait]
    impl RealtimeTranslationSession for ContractTranslationSession {
        async fn connect(
            &mut self,
            _config: RealtimeTranslationConfig,
        ) -> Result<mpsc::Receiver<RealtimeTranslationEvent>, RealtimeTranslationError> {
            Err(RealtimeTranslationError::Internal(
                "contract session is already connected".into(),
            ))
        }

        async fn append_pcm16(&mut self, samples: &[i16]) -> Result<(), RealtimeTranslationError> {
            self.state.append_calls.fetch_add(1, Ordering::SeqCst);
            self.state
                .input_samples
                .lock()
                .unwrap()
                .extend_from_slice(samples);
            self.events
                .send(RealtimeTranslationEvent::TranslatedTextDelta(
                    "hello ".into(),
                ))
                .await
                .unwrap();
            self.events
                .send(RealtimeTranslationEvent::TranslatedAudio {
                    pcm16: CONTRACT_AUDIO_SAMPLES.to_vec(),
                    sample_rate: 24_000,
                    channels: 1,
                })
                .await
                .unwrap();
            Ok(())
        }

        async fn finish(&mut self, _timeout: Duration) -> Result<(), RealtimeTranslationError> {
            self.state.finish_calls.fetch_add(1, Ordering::SeqCst);
            for chunk in 0..self.state.finish_audio_chunks.load(Ordering::SeqCst) {
                self.events
                    .send(RealtimeTranslationEvent::TranslatedAudio {
                        pcm16: vec![chunk as i16 + CONTRACT_DRAIN_AUDIO_BASE],
                        sample_rate: 24_000,
                        channels: 1,
                    })
                    .await
                    .unwrap();
            }
            self.events
                .send(RealtimeTranslationEvent::TranslatedTextDelta(
                    "world".into(),
                ))
                .await
                .unwrap();
            self.events
                .send(RealtimeTranslationEvent::Closed)
                .await
                .unwrap();
            Ok(())
        }

        async fn abort(&mut self) {
            self.state.abort_calls.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct ContractOutput {
        state: Arc<ContractState>,
    }

    struct BlockingContractOutput {
        state: Arc<ContractState>,
    }

    impl Drop for BlockingContractOutput {
        fn drop(&mut self) {
            self.state.output_dropped.store(true, Ordering::SeqCst);
        }
    }

    #[async_trait::async_trait]
    impl TranslationAudioOutput for BlockingContractOutput {
        async fn open(
            &mut self,
            _config: TranslationAudioOutputConfig,
        ) -> TranslationAudioOutputResult<()> {
            Ok(())
        }

        async fn enqueue_pcm16(
            &self,
            _samples: &[i16],
        ) -> TranslationAudioOutputResult<AudioEnqueueOutcome> {
            self.state
                .output_enqueue_entered
                .store(true, Ordering::SeqCst);
            std::future::pending::<()>().await;
            Ok(AudioEnqueueOutcome::Queued {
                pending: Duration::ZERO,
            })
        }

        async fn close(&mut self) -> TranslationAudioOutputResult<()> {
            self.state.output_closed.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn is_open(&self) -> bool {
            true
        }

        fn device_name(&self) -> Option<String> {
            Some("blocking-contract-output".into())
        }

        fn begin_drain_mode(&self) {}

        fn prepare_for_drain(&self) -> TranslationAudioOutputResult<Duration> {
            Ok(Duration::ZERO)
        }

        fn pending_playback_duration(&self) -> Duration {
            Duration::ZERO
        }
    }

    #[async_trait::async_trait]
    impl TranslationAudioOutput for ContractOutput {
        async fn open(
            &mut self,
            _config: TranslationAudioOutputConfig,
        ) -> TranslationAudioOutputResult<()> {
            Ok(())
        }

        async fn enqueue_pcm16(
            &self,
            samples: &[i16],
        ) -> TranslationAudioOutputResult<AudioEnqueueOutcome> {
            self.state
                .output_enqueue_entered
                .store(true, Ordering::SeqCst);
            let delay_ms = self.state.output_enqueue_delay_ms.load(Ordering::SeqCst);
            if delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
            self.state
                .output_samples
                .lock()
                .unwrap()
                .extend_from_slice(samples);
            let pending_per_chunk = self
                .state
                .output_pending_per_chunk_ms
                .load(Ordering::SeqCst);
            let pending = self
                .state
                .output_pending_ms
                .fetch_add(pending_per_chunk, Ordering::SeqCst)
                .saturating_add(pending_per_chunk);
            if self.state.output_should_drop_oldest.load(Ordering::SeqCst) {
                Ok(AudioEnqueueOutcome::DroppedOldest {
                    duration: Duration::from_millis(100),
                    pending: Duration::from_secs(6),
                })
            } else {
                Ok(AudioEnqueueOutcome::Queued {
                    pending: Duration::from_millis(pending),
                })
            }
        }

        async fn close(&mut self) -> TranslationAudioOutputResult<()> {
            self.state.output_pending_at_close_ms.store(
                self.state.output_pending_ms.load(Ordering::SeqCst),
                Ordering::SeqCst,
            );
            self.state.output_closed.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn is_open(&self) -> bool {
            !self.state.output_closed.load(Ordering::SeqCst)
        }

        fn device_name(&self) -> Option<String> {
            Some("contract-output".into())
        }

        fn begin_drain_mode(&self) {}

        fn prepare_for_drain(&self) -> TranslationAudioOutputResult<Duration> {
            self.state
                .output_prepare_calls
                .fetch_add(1, Ordering::SeqCst);
            Ok(Duration::from_millis(
                self.state.output_pending_ms.swap(0, Ordering::SeqCst),
            ))
        }

        fn pending_playback_duration(&self) -> Duration {
            Duration::from_millis(self.state.output_pending_ms.load(Ordering::SeqCst))
        }
    }

    #[tokio::test]
    async fn output_health_failure_is_a_terminal_device_error() {
        let state = Arc::new(ContractState::default());
        state.output_closed.store(true, Ordering::SeqCst);
        let (_command_tx, command_rx) = mpsc::channel(1);
        let (_abort_tx, abort_rx) = watch::channel(false);
        let (stop_tx, mut stop_rx) = mpsc::unbounded_channel();
        let mut policy = RealtimeInterpretationPolicy::outgoing();
        policy.output_health_poll_interval = Duration::from_millis(5);
        let diagnostics = Arc::new(RuntimeDiagnostics::new(1, &policy));

        let output_task = tokio::spawn(run_output_worker(
            Box::new(ContractOutput { state }),
            command_rx,
            abort_rx,
            RuntimeStopReporter::new(stop_tx),
            policy,
            diagnostics,
        ));
        let stop = tokio::time::timeout(Duration::from_secs(1), stop_rx.recv())
            .await
            .unwrap()
            .unwrap();

        assert!(matches!(
            stop,
            RealtimeInterpretationStop::Error(RealtimeInterpretationError::OutputDeviceLost(message))
                if message.contains("virtual microphone output failed")
        ));
        output_task.await.unwrap();
    }

    #[tokio::test]
    async fn repeated_output_drops_are_a_terminal_overload_error() {
        let state = Arc::new(ContractState::default());
        state
            .output_should_drop_oldest
            .store(true, Ordering::SeqCst);
        let (command_tx, command_rx) = mpsc::channel(2);
        let (_abort_tx, abort_rx) = watch::channel(false);
        let (stop_tx, mut stop_rx) = mpsc::unbounded_channel();
        let mut policy = RealtimeInterpretationPolicy::outgoing();
        policy.output_overload_drop_threshold = 2;
        let diagnostics = Arc::new(RuntimeDiagnostics::new(2, &policy));

        let output_task = tokio::spawn(run_output_worker(
            Box::new(ContractOutput { state }),
            command_rx,
            abort_rx,
            RuntimeStopReporter::new(stop_tx),
            policy,
            diagnostics,
        ));
        command_tx
            .send(OutputCommand::Audio(vec![1]))
            .await
            .unwrap();
        command_tx
            .send(OutputCommand::Audio(vec![2]))
            .await
            .unwrap();

        let stop = tokio::time::timeout(Duration::from_secs(1), stop_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            stop,
            RealtimeInterpretationStop::Error(RealtimeInterpretationError::OutputOverload(message))
                if message.contains("repeatedly overflowed")
        ));
        output_task.await.unwrap();
    }

    #[tokio::test]
    async fn terminal_capture_error_reaches_supervisor_once() {
        let state = Arc::new(ContractState::default());
        let (event_tx, event_rx) = mpsc::channel(8);
        let (session, mut stop_rx) = RealtimeInterpretationSession::start(
            RealtimeInterpretationConfig::outgoing(76, 1.0),
            RealtimeInterpretationPorts {
                capture: Box::new(ContractCapture {
                    state: state.clone(),
                    callback: None,
                }),
                output: Box::new(ContractOutput {
                    state: state.clone(),
                }),
                translation: Box::new(ContractTranslationSession {
                    state: state.clone(),
                    events: event_tx,
                }),
                translation_events: event_rx,
            },
            RealtimeInterpretationCallbacks::no_op(),
        )
        .await
        .unwrap();
        let error_callback = state
            .capture_error_callback
            .lock()
            .unwrap()
            .clone()
            .expect("session must register capture error callback");

        error_callback(AudioError::Capture("device lost".into()));
        error_callback(AudioError::Capture("duplicate device error".into()));

        let stop = tokio::time::timeout(Duration::from_secs(1), stop_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            stop,
            RealtimeInterpretationStop::Error(RealtimeInterpretationError::InputDeviceLost(message))
                if message.contains("Microphone capture failed") && message.contains("device lost")
        ));
        assert!(stop_rx.try_recv().is_err());

        session
            .shutdown(RealtimeInterpretationShutdown::Abort)
            .await;
    }

    #[tokio::test]
    async fn capture_health_probe_stops_session_when_terminal_callback_is_lost() {
        let state = Arc::new(ContractState::default());
        let (event_tx, event_rx) = mpsc::channel(8);
        let mut config = RealtimeInterpretationConfig::outgoing(761, 1.0);
        config.policy.capture_health_poll_interval = Duration::from_millis(5);
        let (session, mut stop_rx) = RealtimeInterpretationSession::start(
            config,
            RealtimeInterpretationPorts {
                capture: Box::new(ContractCapture {
                    state: state.clone(),
                    callback: None,
                }),
                output: Box::new(ContractOutput {
                    state: state.clone(),
                }),
                translation: Box::new(ContractTranslationSession {
                    state: state.clone(),
                    events: event_tx,
                }),
                translation_events: event_rx,
            },
            RealtimeInterpretationCallbacks::no_op(),
        )
        .await
        .unwrap();
        assert!(session.try_publish_startup());

        state.capture_running.store(false, Ordering::SeqCst);

        let stop = tokio::time::timeout(Duration::from_secs(1), stop_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            stop,
            RealtimeInterpretationStop::Error(RealtimeInterpretationError::InputDeviceLost(message))
                if message.contains("Microphone capture stopped unexpectedly")
        ));

        session
            .shutdown(RealtimeInterpretationShutdown::Abort)
            .await;
    }

    #[tokio::test]
    async fn session_contract_pumps_audio_text_and_graceful_tail_end_to_end() {
        let state = Arc::new(ContractState::default());
        let translated_text = Arc::new(StdMutex::new(String::new()));
        let observed_input_samples = Arc::new(AtomicUsize::new(0));
        let (event_tx, event_rx) = mpsc::channel(8);
        let callbacks = RealtimeInterpretationCallbacks {
            on_translated_text: {
                let translated_text = translated_text.clone();
                Arc::new(move |delta| translated_text.lock().unwrap().push_str(&delta))
            },
            on_source_text: Arc::new(|_| {}),
            on_input_audio: {
                let observed = observed_input_samples.clone();
                Arc::new(move |samples| {
                    observed.fetch_add(samples.len(), Ordering::SeqCst);
                })
            },
        };

        let (session, _stop_rx) = RealtimeInterpretationSession::start(
            RealtimeInterpretationConfig::outgoing(77, 1.0),
            RealtimeInterpretationPorts {
                capture: Box::new(ContractCapture {
                    state: state.clone(),
                    callback: None,
                }),
                output: Box::new(ContractOutput {
                    state: state.clone(),
                }),
                translation: Box::new(ContractTranslationSession {
                    state: state.clone(),
                    events: event_tx,
                }),
                translation_events: event_rx,
            },
            callbacks,
        )
        .await
        .unwrap();
        assert_eq!(session.session_id(), 77);

        tokio::time::timeout(Duration::from_secs(1), async {
            while state.append_calls.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("input frame must reach translation worker");
        session
            .shutdown(RealtimeInterpretationShutdown::Graceful)
            .await;

        assert!(state.capture_stopped.load(Ordering::SeqCst));
        assert!(state.output_closed.load(Ordering::SeqCst));
        assert_eq!(state.append_calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.finish_calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.abort_calls.load(Ordering::SeqCst), 0);
        assert_eq!(state.input_samples.lock().unwrap().len(), 4_800);
        assert_eq!(
            *state.output_samples.lock().unwrap(),
            CONTRACT_AUDIO_SAMPLES.to_vec()
        );
        assert_eq!(&*translated_text.lock().unwrap(), "hello world");
        assert_eq!(observed_input_samples.load(Ordering::SeqCst), 4_800);
    }

    #[tokio::test]
    async fn graceful_shutdown_delivers_drain_behind_a_full_output_queue() {
        let state = Arc::new(ContractState::default());
        state.finish_audio_chunks.store(4, Ordering::SeqCst);
        state.output_enqueue_delay_ms.store(80, Ordering::SeqCst);
        state
            .output_pending_per_chunk_ms
            .store(20, Ordering::SeqCst);
        let (event_tx, event_rx) = mpsc::channel(8);
        let mut config = RealtimeInterpretationConfig::incoming_spoken(7_703);
        config.policy.output_queue_capacity_chunks = 1;
        config.policy.output_pending_overload = Some(OutputPendingOverloadPolicy {
            limit: Duration::from_millis(30),
            grace: Duration::ZERO,
        });
        config.policy.output_health_poll_interval = Duration::from_millis(5);
        config.policy.worker_stop_timeout = Duration::from_millis(20);
        config.policy.output_drain_max = Duration::from_millis(500);
        config.policy.output_drain_safety = Duration::from_millis(20);
        config.policy.graceful_shutdown_timeout = Duration::from_secs(3);
        config.policy.forced_shutdown_timeout = Duration::from_millis(100);

        let (session, _stop_rx) = RealtimeInterpretationSession::start(
            config,
            RealtimeInterpretationPorts {
                capture: Box::new(ContractCapture {
                    state: state.clone(),
                    callback: None,
                }),
                output: Box::new(ContractOutput {
                    state: state.clone(),
                }),
                translation: Box::new(ContractTranslationSession {
                    state: state.clone(),
                    events: event_tx,
                }),
                translation_events: event_rx,
            },
            RealtimeInterpretationCallbacks::no_op(),
        )
        .await
        .unwrap();

        tokio::time::timeout(Duration::from_secs(1), async {
            while state.append_calls.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("input must reach the translation session");
        session
            .shutdown(RealtimeInterpretationShutdown::Graceful)
            .await;

        assert_eq!(state.finish_calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.abort_calls.load(Ordering::SeqCst), 0);
        assert_eq!(state.output_prepare_calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.output_pending_at_close_ms.load(Ordering::SeqCst), 0);
        assert!(state.output_closed.load(Ordering::SeqCst));
        assert_eq!(
            *state.output_samples.lock().unwrap(),
            vec![1_000, 2_000, 3_000, 4_000, 4_001, 4_002, 4_003]
        );
    }

    #[tokio::test]
    async fn abort_is_bounded_when_output_enqueue_never_returns() {
        let state = Arc::new(ContractState::default());
        let (event_tx, event_rx) = mpsc::channel(8);
        let mut config = RealtimeInterpretationConfig::outgoing(78, 1.0);
        config.policy.input_drain_timeout = Duration::from_millis(50);
        config.policy.event_drain_timeout = Duration::from_millis(50);
        config.policy.worker_stop_timeout = Duration::from_millis(50);

        let (session, _stop_rx) = RealtimeInterpretationSession::start(
            config,
            RealtimeInterpretationPorts {
                capture: Box::new(ContractCapture {
                    state: state.clone(),
                    callback: None,
                }),
                output: Box::new(BlockingContractOutput {
                    state: state.clone(),
                }),
                translation: Box::new(ContractTranslationSession {
                    state: state.clone(),
                    events: event_tx,
                }),
                translation_events: event_rx,
            },
            RealtimeInterpretationCallbacks::no_op(),
        )
        .await
        .unwrap();

        tokio::time::timeout(Duration::from_secs(1), async {
            while !state.output_enqueue_entered.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("output worker must enter enqueue");
        tokio::time::timeout(
            Duration::from_secs(1),
            session.shutdown(RealtimeInterpretationShutdown::Abort),
        )
        .await
        .expect("abort must not wait forever for a blocked output");

        assert!(state.capture_stopped.load(Ordering::SeqCst));
        assert!(state.output_dropped.load(Ordering::SeqCst));
        assert_eq!(state.abort_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn abort_is_bounded_when_capture_stop_never_returns() {
        let state = Arc::new(ContractState::default());
        let (event_tx, event_rx) = mpsc::channel(8);
        let mut config = RealtimeInterpretationConfig::incoming_spoken(7_801);
        config.policy.worker_stop_timeout = Duration::from_millis(30);
        config.policy.forced_shutdown_timeout = Duration::from_millis(100);

        let (session, _stop_rx) = RealtimeInterpretationSession::start(
            config,
            RealtimeInterpretationPorts {
                capture: Box::new(BlockingStopCapture {
                    state: state.clone(),
                    callback: None,
                }),
                output: Box::new(ContractOutput {
                    state: state.clone(),
                }),
                translation: Box::new(ContractTranslationSession {
                    state: state.clone(),
                    events: event_tx,
                }),
                translation_events: event_rx,
            },
            RealtimeInterpretationCallbacks::no_op(),
        )
        .await
        .unwrap();

        tokio::time::timeout(
            Duration::from_millis(500),
            session.shutdown(RealtimeInterpretationShutdown::Abort),
        )
        .await
        .expect("abort must bound a stalled capture stop");

        assert!(state.capture_stop_entered.load(Ordering::SeqCst));
        assert_eq!(state.abort_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn capture_start_timeout_aborts_paid_runtime_and_open_output() {
        let state = Arc::new(ContractState::default());
        let (event_tx, event_rx) = mpsc::channel(8);
        let mut config = RealtimeInterpretationConfig::incoming_spoken(7_802);
        config.policy.capture_start_timeout = Duration::from_millis(20);
        config.policy.worker_stop_timeout = Duration::from_millis(20);
        config.policy.forced_shutdown_timeout = Duration::from_millis(100);

        let result = tokio::time::timeout(
            Duration::from_millis(500),
            RealtimeInterpretationSession::start(
                config,
                RealtimeInterpretationPorts {
                    capture: Box::new(BlockingStartCapture {
                        state: state.clone(),
                    }),
                    output: Box::new(ContractOutput {
                        state: state.clone(),
                    }),
                    translation: Box::new(ContractTranslationSession {
                        state: state.clone(),
                        events: event_tx,
                    }),
                    translation_events: event_rx,
                },
                RealtimeInterpretationCallbacks::no_op(),
            ),
        )
        .await
        .expect("capture startup must have a global deadline");
        let error = match result {
            Ok(_) => panic!("stalled capture startup must fail"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            RealtimeInterpretationStartError::Timeout(message)
                if message.contains("timed out")
        ));
        assert!(state.capture_start_entered.load(Ordering::SeqCst));
        assert!(state.capture_stopped.load(Ordering::SeqCst));
        assert!(state.output_closed.load(Ordering::SeqCst));
        assert_eq!(state.abort_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn graceful_shutdown_has_one_global_deadline_and_forces_cleanup() {
        let state = Arc::new(ContractState::default());
        let (event_tx, event_rx) = mpsc::channel(8);
        let mut config = RealtimeInterpretationConfig::incoming_spoken(79);
        config.policy.input_drain_timeout = Duration::from_millis(30);
        config.policy.event_drain_timeout = Duration::from_millis(30);
        config.policy.worker_stop_timeout = Duration::from_millis(30);
        config.policy.graceful_shutdown_timeout = Duration::from_millis(80);
        config.policy.forced_shutdown_timeout = Duration::from_millis(80);

        let (session, _stop_rx) = RealtimeInterpretationSession::start(
            config,
            RealtimeInterpretationPorts {
                capture: Box::new(ContractCapture {
                    state: state.clone(),
                    callback: None,
                }),
                output: Box::new(BlockingContractOutput {
                    state: state.clone(),
                }),
                translation: Box::new(ContractTranslationSession {
                    state: state.clone(),
                    events: event_tx,
                }),
                translation_events: event_rx,
            },
            RealtimeInterpretationCallbacks::no_op(),
        )
        .await
        .unwrap();

        tokio::time::timeout(Duration::from_secs(1), async {
            while !state.output_enqueue_entered.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("output worker must enter enqueue");
        let started = Instant::now();
        tokio::time::timeout(
            Duration::from_millis(500),
            session.shutdown(RealtimeInterpretationShutdown::Graceful),
        )
        .await
        .expect("global graceful deadline must force bounded cleanup");

        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(state.capture_stopped.load(Ordering::SeqCst));
        assert!(state.output_dropped.load(Ordering::SeqCst));
    }
}
