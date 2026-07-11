use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use futures_util::FutureExt;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::domain::{
    amplify_i16_samples, AudioCapture, AudioCaptureErrorCallback, AudioChunk, AudioChunkCallback,
    AudioEnqueueOutcome, AudioError, RealtimeTranslationError, RealtimeTranslationErrorKind,
    RealtimeTranslationEvent, RealtimeTranslationSession, TranslationAudioOutput,
    TranslationAudioOutputError,
};

use super::frame_assembler::Pcm16FrameAssembler;

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
    input_drain_timeout: Duration,
    event_drain_timeout: Duration,
    translation_finish_timeout: Duration,
    output_health_poll_interval: Duration,
    output_drain_safety: Duration,
    output_drain_max: Duration,
    output_drain_poll: Duration,
    output_drain_empty_threshold: Duration,
    worker_stop_timeout: Duration,
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

impl RealtimeInterpretationPolicy {
    pub fn outgoing() -> Self {
        Self {
            input_frame_samples: 4_800,
            input_queue_capacity_chunks: 160,
            input_overload_drop_threshold: 32,
            output_queue_capacity_chunks: 32,
            output_overload_drop_threshold: 32,
            input_drain_timeout: Duration::from_millis(1_500),
            event_drain_timeout: Duration::from_millis(1_500),
            translation_finish_timeout: Duration::from_millis(8_000),
            output_health_poll_interval: Duration::from_millis(250),
            output_drain_safety: Duration::from_millis(250),
            output_drain_max: Duration::from_millis(12_000),
            output_drain_poll: Duration::from_millis(50),
            output_drain_empty_threshold: Duration::from_millis(30),
            worker_stop_timeout: Duration::from_millis(1_500),
            input_source_name: "Microphone",
            output_route_name: "virtual microphone",
            silence_cadence: None,
        }
    }

    pub fn incoming_spoken() -> Self {
        Self {
            input_frame_samples: 4_800,
            input_queue_capacity_chunks: 160,
            input_overload_drop_threshold: 32,
            output_queue_capacity_chunks: 32,
            output_overload_drop_threshold: 8,
            input_drain_timeout: Duration::from_millis(1_500),
            event_drain_timeout: Duration::from_millis(1_500),
            translation_finish_timeout: Duration::from_millis(5_000),
            output_health_poll_interval: Duration::from_millis(250),
            output_drain_safety: Duration::from_millis(250),
            output_drain_max: Duration::from_millis(5_000),
            output_drain_poll: Duration::from_millis(50),
            output_drain_empty_threshold: Duration::from_millis(30),
            worker_stop_timeout: Duration::from_millis(1_000),
            input_source_name: "System audio",
            output_route_name: "local playback",
            silence_cadence: Some(SilenceCadencePolicy {
                gap_threshold: Duration::from_millis(400),
                interval: Duration::from_millis(200),
                sample_rate: 24_000,
                channels: 1,
            }),
        }
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
    sent: Arc<AtomicBool>,
}

impl RuntimeStopReporter {
    fn new(tx: mpsc::UnboundedSender<RealtimeInterpretationStop>) -> Self {
        Self {
            tx,
            sent: Arc::new(AtomicBool::new(false)),
        }
    }

    fn error(&self, error: RealtimeInterpretationError) {
        self.send(RealtimeInterpretationStop::Error(error));
    }

    fn closed(&self) {
        self.send(RealtimeInterpretationStop::Closed);
    }

    fn send(&self, stop: RealtimeInterpretationStop) {
        if self
            .sent
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let _ = self.tx.send(stop);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AudioQueueEnqueueError {
    Full(u64),
    Closed,
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
    output_worker_task: Option<OutputWorkerTask>,
    silence_cadence_task: Option<JoinHandle<()>>,
    stop_requested: Arc<AtomicBool>,
    session_id: u64,
    policy: RealtimeInterpretationPolicy,
    diagnostics: Arc<RuntimeDiagnostics>,
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
        let stop_requested = Arc::new(AtomicBool::new(false));
        let diagnostics = Arc::new(RuntimeDiagnostics::new(config.session_id, &config.policy));

        let (output_tx, output_rx) = mpsc::channel(config.policy.output_queue_capacity_chunks);
        let output_worker_task = spawn_output_worker(
            ports.output,
            output_rx,
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
            output_worker_task: Some(output_worker_task),
            silence_cadence_task,
            stop_requested,
            session_id: config.session_id,
            policy: config.policy,
            diagnostics,
        };

        if let Err(error) = session
            .capture
            .as_mut()
            .expect("capture must exist during startup")
            .start_capture(capture_callback)
            .await
        {
            session
                .shutdown(RealtimeInterpretationShutdown::Abort)
                .await;
            return Err(RealtimeInterpretationStartError::Capture(error));
        }

        Ok((session, stop_rx))
    }

    pub fn session_id(&self) -> u64 {
        self.session_id
    }

    pub async fn set_output_gain(&self, gain: f32) -> Result<(), RealtimeInterpretationError> {
        let (done_tx, done_rx) = oneshot::channel();
        tokio::time::timeout(
            self.policy.worker_stop_timeout,
            self.output_tx.send(OutputCommand::SetGain {
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

        tokio::time::timeout(self.policy.worker_stop_timeout, done_rx)
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
            .map_err(|error| map_output_error(error, self.policy.output_route_name))
    }

    pub async fn shutdown(mut self, mode: RealtimeInterpretationShutdown) {
        let cleanup_started = Instant::now();
        self.stop_requested.store(true, Ordering::SeqCst);
        if let Some(task) = self.silence_cadence_task.take() {
            task.abort();
            let _ = task.await;
        }
        if let Some(capture) = self.capture.as_mut() {
            if let Err(error) = capture.stop_capture().await {
                log::warn!(
                    "RealtimeInterpretationSession: capture stop failed for session {}: {}",
                    self.session_id,
                    error
                );
            }
            capture.set_terminal_error_callback(None);
        }

        match mode {
            RealtimeInterpretationShutdown::Graceful => self.shutdown_gracefully().await,
            RealtimeInterpretationShutdown::Abort => self.shutdown_immediately().await,
        }

        self.capture = None;
        self.diagnostics.report(
            match mode {
                RealtimeInterpretationShutdown::Graceful => "graceful",
                RealtimeInterpretationShutdown::Abort => "abort",
            },
            cleanup_started.elapsed().as_millis().min(u64::MAX as u128) as u64,
        );
        log::info!(
            "RealtimeInterpretationSession: session {} cleaned up ({:?})",
            self.session_id,
            mode
        );
    }

    async fn shutdown_gracefully(&mut self) {
        let _ = tokio::time::timeout(
            self.policy.worker_stop_timeout,
            self.output_tx.send(OutputCommand::BeginDrain),
        )
        .await;

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
        self.close_output().await;
    }

    async fn recover_input_client(&mut self, abort_immediately: bool) -> Option<InputWorkerResult> {
        let mut task = self.input_worker_task.take()?;
        if abort_immediately {
            let _ = self.input_abort_tx.send(());
        }

        match tokio::time::timeout(self.policy.input_drain_timeout, &mut task).await {
            Ok(Ok(client)) => client,
            Ok(Err(error)) => {
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
                self.wait_for_aborted_input_worker(task).await
            }
            Err(_) => {
                task.abort();
                let _ = task.await;
                None
            }
        }
    }

    async fn wait_for_aborted_input_worker(
        &self,
        mut task: InputWorkerTask,
    ) -> Option<InputWorkerResult> {
        match tokio::time::timeout(self.policy.worker_stop_timeout, &mut task).await {
            Ok(Ok(client)) => client,
            Ok(Err(error)) => {
                log::warn!(
                    "RealtimeInterpretationSession: aborted input worker join failed for session {}: {}",
                    self.session_id,
                    error
                );
                None
            }
            Err(_) => {
                task.abort();
                let _ = task.await;
                None
            }
        }
    }

    async fn wait_for_event_forwarder(&mut self, abort_immediately: bool) {
        let Some(mut task) = self.event_forwarder_task.take() else {
            return;
        };
        if abort_immediately {
            task.abort();
        }

        if tokio::time::timeout(self.policy.event_drain_timeout, &mut task)
            .await
            .is_err()
        {
            task.abort();
            let _ = task.await;
        }
    }

    async fn drain_output(&self) {
        let (done_tx, done_rx) = oneshot::channel();
        let send_result = tokio::time::timeout(
            self.policy.worker_stop_timeout,
            self.output_tx.send(OutputCommand::Drain { done: done_tx }),
        )
        .await;
        if !matches!(send_result, Ok(Ok(()))) {
            return;
        }
        let wait = self
            .policy
            .output_drain_max
            .saturating_add(self.policy.output_drain_safety);
        let _ = tokio::time::timeout(wait, done_rx).await;
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

        let Some(mut task) = self.output_worker_task.take() else {
            return;
        };
        match tokio::time::timeout(self.policy.worker_stop_timeout, &mut task).await {
            Ok(Ok(Some(mut output))) if !close_sent => {
                if let Err(error) = output.close().await {
                    log::warn!(
                        "RealtimeInterpretationSession: recovered output close failed for session {}: {}",
                        self.session_id,
                        error
                    );
                }
            }
            Ok(Ok(_)) => {}
            Ok(Err(error)) => log::warn!(
                "RealtimeInterpretationSession: output worker join failed for session {}: {}",
                self.session_id,
                error
            ),
            Err(_) => {
                task.abort();
                let _ = task.await;
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
        self.diagnostics.report("drop", 0);
    }
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

fn spawn_output_worker(
    output: Box<dyn TranslationAudioOutput>,
    output_rx: mpsc::Receiver<OutputCommand>,
    reporter: RuntimeStopReporter,
    policy: RealtimeInterpretationPolicy,
    diagnostics: Arc<RuntimeDiagnostics>,
) -> OutputWorkerTask {
    let panic_reporter = reporter.clone();
    tokio::spawn(async move {
        match AssertUnwindSafe(run_output_worker(
            output,
            output_rx,
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
    reporter: RuntimeStopReporter,
    policy: RealtimeInterpretationPolicy,
    diagnostics: Arc<RuntimeDiagnostics>,
) -> Box<dyn TranslationAudioOutput> {
    let mut health_poll = tokio::time::interval(policy.output_health_poll_interval);
    let mut consecutive_output_drops = 0u64;
    health_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    health_poll.tick().await;

    loop {
        tokio::select! {
            command = output_rx.recv() => {
                let Some(command) = command else {
                    return output;
                };
                match command {
                    OutputCommand::Audio(samples) => {
                        match output.enqueue_pcm16(&samples).await {
                            Ok(AudioEnqueueOutcome::Queued { pending }) => {
                                consecutive_output_drops = 0;
                                diagnostics.observe_output_pending(pending);
                            }
                            Ok(AudioEnqueueOutcome::DroppedOldest { duration, pending }) => {
                                consecutive_output_drops = consecutive_output_drops.saturating_add(1);
                                diagnostics.observe_output_drop(duration, pending);
                                log::warn!(
                                    "RealtimeInterpretationSession: {} output dropped {} ms (pending={} ms, consecutive={})",
                                    policy.output_route_name,
                                    duration.as_millis(),
                                    pending.as_millis(),
                                    consecutive_output_drops
                                );
                                if consecutive_output_drops >= policy.output_overload_drop_threshold {
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
                    OutputCommand::BeginDrain => output.begin_drain_mode(),
                    OutputCommand::Drain { done } => {
                        drain_output_tail(output.as_ref(), &policy).await;
                        let _ = done.send(());
                    }
                    OutputCommand::Close { done } => {
                        let result = output.close().await.map_err(|error| error.to_string());
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
            }
        }
    }
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
) {
    let initial_pending = match output.prepare_for_drain() {
        Ok(pending) => pending,
        Err(error) => {
            log::warn!(
                "RealtimeInterpretationSession: output drain prepare failed: {}",
                error
            );
            return;
        }
    };
    if initial_pending <= policy.output_drain_empty_threshold {
        return;
    }

    let wait_budget = initial_pending
        .saturating_add(policy.output_drain_safety)
        .min(policy.output_drain_max);
    let deadline = tokio::time::Instant::now() + wait_budget;
    loop {
        let pending = output.pending_playback_duration();
        if pending <= policy.output_drain_empty_threshold {
            return;
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            log::warn!(
                "RealtimeInterpretationSession: output tail drain timed out (pending={} ms)",
                pending.as_millis()
            );
            return;
        }
        tokio::time::sleep(
            policy
                .output_drain_poll
                .min(pending)
                .min(deadline.saturating_duration_since(now)),
        )
        .await;
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
    let consecutive_drops = AtomicU64::new(0);
    Arc::new(move |chunk| {
        capture_activity.touch();
        let queue_capacity = input_tx.max_capacity();
        diagnostics.observe_input(
            &chunk,
            queue_capacity
                .saturating_sub(input_tx.capacity())
                .saturating_add(1)
                .min(queue_capacity),
        );
        match try_enqueue_audio_chunk(&input_tx, chunk, &consecutive_drops) {
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

pub(crate) fn try_enqueue_audio_chunk(
    input_tx: &mpsc::Sender<AudioChunk>,
    chunk: AudioChunk,
    consecutive_drops: &AtomicU64,
) -> Result<(), AudioQueueEnqueueError> {
    match input_tx.try_send(chunk) {
        Ok(()) => {
            consecutive_drops.store(0, Ordering::Relaxed);
            Ok(())
        }
        Err(mpsc::error::TrySendError::Full(_)) => {
            let drops = consecutive_drops.fetch_add(1, Ordering::Relaxed) + 1;
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
    fn input_queue_drop_counter_resets_after_success() {
        let (tx, mut rx) = mpsc::channel(1);
        let drops = AtomicU64::new(0);
        let first = AudioChunk::new(vec![1], 24_000, 1);
        let second = AudioChunk::new(vec![2], 24_000, 1);

        assert_eq!(try_enqueue_audio_chunk(&tx, first, &drops), Ok(()));
        assert_eq!(
            try_enqueue_audio_chunk(&tx, second.clone(), &drops),
            Err(AudioQueueEnqueueError::Full(1))
        );
        assert!(rx.try_recv().is_ok());
        assert_eq!(try_enqueue_audio_chunk(&tx, second, &drops), Ok(()));
        assert_eq!(drops.load(Ordering::Relaxed), 0);
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

    #[derive(Default)]
    struct ContractState {
        capture_stopped: AtomicBool,
        append_calls: AtomicUsize,
        finish_calls: AtomicUsize,
        abort_calls: AtomicUsize,
        output_closed: AtomicBool,
        output_enqueue_entered: AtomicBool,
        output_dropped: AtomicBool,
        output_should_drop_oldest: AtomicBool,
        capture_error_callback: StdMutex<Option<AudioCaptureErrorCallback>>,
        input_samples: StdMutex<Vec<i16>>,
        output_samples: StdMutex<Vec<i16>>,
    }

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
            callback(AudioChunk::new(vec![1_200; 4_800], 24_000, 1));
            self.callback = Some(callback);
            Ok(())
        }

        async fn stop_capture(&mut self) -> crate::domain::AudioResult<()> {
            self.state.capture_stopped.store(true, Ordering::SeqCst);
            self.callback = None;
            Ok(())
        }

        fn set_terminal_error_callback(&mut self, callback: Option<AudioCaptureErrorCallback>) {
            *self.state.capture_error_callback.lock().unwrap() = callback;
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
                    pcm16: vec![10, 20, 30],
                    sample_rate: 24_000,
                    channels: 1,
                })
                .await
                .unwrap();
            Ok(())
        }

        async fn finish(&mut self, _timeout: Duration) -> Result<(), RealtimeTranslationError> {
            self.state.finish_calls.fetch_add(1, Ordering::SeqCst);
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
                .output_samples
                .lock()
                .unwrap()
                .extend_from_slice(samples);
            if self.state.output_should_drop_oldest.load(Ordering::SeqCst) {
                Ok(AudioEnqueueOutcome::DroppedOldest {
                    duration: Duration::from_millis(100),
                    pending: Duration::from_secs(6),
                })
            } else {
                Ok(AudioEnqueueOutcome::Queued {
                    pending: Duration::ZERO,
                })
            }
        }

        async fn close(&mut self) -> TranslationAudioOutputResult<()> {
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
            Ok(Duration::ZERO)
        }

        fn pending_playback_duration(&self) -> Duration {
            Duration::ZERO
        }
    }

    #[tokio::test]
    async fn output_health_failure_is_a_terminal_device_error() {
        let state = Arc::new(ContractState::default());
        state.output_closed.store(true, Ordering::SeqCst);
        let (_command_tx, command_rx) = mpsc::channel(1);
        let (stop_tx, mut stop_rx) = mpsc::unbounded_channel();
        let mut policy = RealtimeInterpretationPolicy::outgoing();
        policy.output_health_poll_interval = Duration::from_millis(5);
        let diagnostics = Arc::new(RuntimeDiagnostics::new(1, &policy));

        let output_task = tokio::spawn(run_output_worker(
            Box::new(ContractOutput { state }),
            command_rx,
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
        let (stop_tx, mut stop_rx) = mpsc::unbounded_channel();
        let mut policy = RealtimeInterpretationPolicy::outgoing();
        policy.output_overload_drop_threshold = 2;
        let diagnostics = Arc::new(RuntimeDiagnostics::new(2, &policy));

        let output_task = tokio::spawn(run_output_worker(
            Box::new(ContractOutput { state }),
            command_rx,
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
        assert_eq!(*state.output_samples.lock().unwrap(), vec![10, 20, 30]);
        assert_eq!(&*translated_text.lock().unwrap(), "hello world");
        assert_eq!(observed_input_samples.load(Ordering::SeqCst), 4_800);
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
}
