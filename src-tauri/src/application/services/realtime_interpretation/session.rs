use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::FutureExt;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::domain::{
    amplify_i16_samples, AudioCapture, AudioChunk, AudioChunkCallback, RealtimeTranslationError,
    RealtimeTranslationErrorKind, RealtimeTranslationEvent, RealtimeTranslationSession,
    TranslationAudioOutput,
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
}

impl From<RealtimeTranslationError> for RealtimeInterpretationError {
    fn from(error: RealtimeTranslationError) -> Self {
        let message = error.to_string();
        match error.kind() {
            RealtimeTranslationErrorKind::Authentication => Self::Authentication(message),
            RealtimeTranslationErrorKind::RateLimited => Self::RateLimited(message),
            RealtimeTranslationErrorKind::Connection => Self::Connection(message),
            RealtimeTranslationErrorKind::Timeout => Self::Timeout(message),
            RealtimeTranslationErrorKind::Protocol | RealtimeTranslationErrorKind::Internal => {
                Self::Processing(message)
            }
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
    Capture(String),
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
}

impl RealtimeInterpretationPolicy {
    pub fn outgoing() -> Self {
        Self {
            input_frame_samples: 4_800,
            input_queue_capacity_chunks: 160,
            input_overload_drop_threshold: 32,
            output_queue_capacity_chunks: 32,
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
    stop_requested: Arc<AtomicBool>,
    session_id: u64,
    policy: RealtimeInterpretationPolicy,
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

        let (output_tx, output_rx) = mpsc::channel(config.policy.output_queue_capacity_chunks);
        let output_worker_task = spawn_output_worker(
            ports.output,
            output_rx,
            reporter.clone(),
            config.policy.clone(),
        );
        let event_forwarder_task = spawn_event_forwarder(
            ports.translation_events,
            output_tx.clone(),
            callbacks.clone(),
            reporter.clone(),
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
        let capture_callback = build_capture_callback(
            input_tx,
            reporter,
            stop_requested.clone(),
            config.policy.clone(),
        );

        let mut session = Self {
            capture: Some(ports.capture),
            input_abort_tx,
            input_worker_task: Some(input_worker_task),
            event_forwarder_task: Some(event_forwarder_task),
            output_tx,
            output_worker_task: Some(output_worker_task),
            stop_requested,
            session_id: config.session_id,
            policy: config.policy,
        };

        if let Err(error) = session
            .capture
            .as_mut()
            .expect("capture must exist during startup")
            .start_capture(capture_callback)
            .await
        {
            let message = error.to_string();
            session
                .shutdown(RealtimeInterpretationShutdown::Abort)
                .await;
            return Err(RealtimeInterpretationStartError::Capture(message));
        }

        Ok((session, stop_rx))
    }

    pub fn session_id(&self) -> u64 {
        self.session_id
    }

    pub async fn shutdown(mut self, mode: RealtimeInterpretationShutdown) {
        self.stop_requested.store(true, Ordering::SeqCst);
        if let Some(capture) = self.capture.as_mut() {
            if let Err(error) = capture.stop_capture().await {
                log::warn!(
                    "RealtimeInterpretationSession: capture stop failed for session {}: {}",
                    self.session_id,
                    error
                );
            }
        }

        match mode {
            RealtimeInterpretationShutdown::Graceful => self.shutdown_gracefully().await,
            RealtimeInterpretationShutdown::Abort => self.shutdown_immediately().await,
        }

        self.capture = None;
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
) -> JoinHandle<()> {
    let panic_reporter = reporter.clone();
    tokio::spawn(async move {
        if AssertUnwindSafe(run_event_forwarder(
            translation_events,
            output_tx,
            callbacks,
            reporter,
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
            }
            RealtimeTranslationEvent::TranslatedTextDelta(text) => {
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
) -> OutputWorkerTask {
    let panic_reporter = reporter.clone();
    tokio::spawn(async move {
        match AssertUnwindSafe(run_output_worker(output, output_rx, reporter, policy))
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
) -> Box<dyn TranslationAudioOutput> {
    let mut health_poll = tokio::time::interval(policy.output_health_poll_interval);
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
                        if let Err(error) = output.enqueue_pcm16(&samples).await {
                            reporter.error(RealtimeInterpretationError::Processing(error.to_string()));
                            return output;
                        }
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
                if !output.is_open() {
                    reporter.error(RealtimeInterpretationError::Processing(format!(
                        "{} output stream stopped unexpectedly",
                        policy.output_route_name
                    )));
                    return output;
                }
            }
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
) -> AudioChunkCallback {
    let consecutive_drops = AtomicU64::new(0);
    Arc::new(
        move |chunk| match try_enqueue_audio_chunk(&input_tx, chunk, &consecutive_drops) {
            Ok(()) => {}
            Err(AudioQueueEnqueueError::Full(drops))
                if drops == policy.input_overload_drop_threshold =>
            {
                reporter.error(RealtimeInterpretationError::Processing(format!(
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
            Err(_) => {}
        },
    )
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
        AudioConfig, RealtimeTranslationConfig, TranslationAudioOutputConfig,
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

        async fn enqueue_pcm16(&self, _samples: &[i16]) -> TranslationAudioOutputResult<()> {
            self.state
                .output_enqueue_entered
                .store(true, Ordering::SeqCst);
            std::future::pending::<()>().await;
            Ok(())
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

        async fn enqueue_pcm16(&self, samples: &[i16]) -> TranslationAudioOutputResult<()> {
            self.state
                .output_samples
                .lock()
                .unwrap()
                .extend_from_slice(samples);
            Ok(())
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
    async fn output_health_failure_is_a_terminal_processing_error() {
        let state = Arc::new(ContractState::default());
        state.output_closed.store(true, Ordering::SeqCst);
        let (_command_tx, command_rx) = mpsc::channel(1);
        let (stop_tx, mut stop_rx) = mpsc::unbounded_channel();
        let mut policy = RealtimeInterpretationPolicy::outgoing();
        policy.output_health_poll_interval = Duration::from_millis(5);

        let output_task = tokio::spawn(run_output_worker(
            Box::new(ContractOutput { state }),
            command_rx,
            RuntimeStopReporter::new(stop_tx),
            policy,
        ));
        let stop = tokio::time::timeout(Duration::from_secs(1), stop_rx.recv())
            .await
            .unwrap()
            .unwrap();

        assert!(matches!(
            stop,
            RealtimeInterpretationStop::Error(RealtimeInterpretationError::Processing(message))
                if message.contains("virtual microphone output stream stopped unexpectedly")
        ));
        output_task.await.unwrap();
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
