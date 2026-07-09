//! LiveTranslationService — orchestrator для live-перевода голоса.
//!
//! Pipeline:
//! - mic 24 kHz mono через platform audio factory
//!   - применяем microphone_sensitivity gain
//!   - feed-им в audio_spectrum analyzer
//!   - отправляем в OpenAI realtime translation client (PCM16 base64)
//! - OpenAI отдаёт events:
//!   - `AudioDelta(Vec<i16>)` → `TranslationAudioOutput.enqueue_pcm16(...)`
//!   - `TranscriptDelta(String)` → callback в UI (popover)
//!   - `Error(...)` → callback в UI + статус Error
//!   - `Closed` → если незапланированно → callback в UI
//!
//! Этот сервис намеренно НЕ повторяет логику TranscriptionService:
//! - нет auto-paste/copy/history;
//! - нет STT auth retry/logout;
//! - нет VAD (translation идёт сплошным потоком, включая тишину).

use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::task::JoinHandle;

use crate::domain::{
    amplify_i16_samples, microphone_sensitivity_gain, AudioCapture, AudioCaptureTarget, AudioChunk,
    AudioChunkCallback, AudioConfig, PlatformAudioFactory, RecordingStatus, TranslationAudioOutput,
    TranslationAudioOutputConfig,
};
use crate::infrastructure::audio::DefaultPlatformAudioFactory;
use crate::infrastructure::openai::{
    OpenAIErrorKind, OpenAIRealtimeEvent, OpenAIRealtimeTranslationClient, OpenAITranslationError,
};

use super::audio_spectrum::AudioSpectrumAnalyzer;

const TRANSLATION_TARGET_LANGUAGE_DEFAULT: &str = "en";
const OPENAI_INPUT_FRAME_SAMPLES: usize = 4_800; // 200 ms at 24 kHz mono.
const MIC_QUEUE_CAPACITY_CHUNKS: usize = 160; // Roughly a few seconds, depending on device chunking.
const GRACEFUL_CLOSE_TIMEOUT_MS: u64 = 8_000;
const MIC_PUMP_DRAIN_TIMEOUT_MS: u64 = 1_500;
const FORWARDER_DRAIN_TIMEOUT_MS: u64 = 1_500;
const OUTPUT_DRAIN_SAFETY_MS: u64 = 250;
const OUTPUT_DRAIN_MAX_MS: u64 = 12_000;
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

fn normalize_live_translation_target_language(value: &str) -> String {
    let language = value.trim();
    if language.is_empty()
        || language.eq_ignore_ascii_case("auto")
        || language.eq_ignore_ascii_case("multi")
    {
        TRANSLATION_TARGET_LANGUAGE_DEFAULT.to_string()
    } else {
        language.to_string()
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
    audio_factory: Arc<dyn PlatformAudioFactory>,
    client_factory: Arc<dyn RealtimeTranslationClientFactory>,
}

struct RunningSession {
    capture: Arc<RwLock<Box<dyn AudioCapture>>>,
    output: Arc<RwLock<Box<dyn TranslationAudioOutput>>>,
    client: Arc<Mutex<Box<dyn RealtimeTranslationClientPort>>>,
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

#[async_trait]
trait RealtimeTranslationClientPort: Send + Sync {
    async fn connect(
        &mut self,
    ) -> Result<mpsc::Receiver<OpenAIRealtimeEvent>, OpenAITranslationError>;
    async fn append_input_audio(&self, pcm16: &[i16]) -> Result<(), OpenAITranslationError>;
    async fn close(&mut self, drain_timeout: Duration) -> Result<(), OpenAITranslationError>;
    async fn abort(&mut self);
}

trait RealtimeTranslationClientFactory: Send + Sync {
    fn create(
        &self,
        api_key: String,
        target_language: String,
    ) -> Box<dyn RealtimeTranslationClientPort>;
}

struct OpenAIRealtimeTranslationClientFactory;

impl RealtimeTranslationClientFactory for OpenAIRealtimeTranslationClientFactory {
    fn create(
        &self,
        api_key: String,
        target_language: String,
    ) -> Box<dyn RealtimeTranslationClientPort> {
        Box::new(OpenAIRealtimeTranslationClient::new(
            api_key,
            target_language,
        ))
    }
}

#[async_trait]
impl RealtimeTranslationClientPort for OpenAIRealtimeTranslationClient {
    async fn connect(
        &mut self,
    ) -> Result<mpsc::Receiver<OpenAIRealtimeEvent>, OpenAITranslationError> {
        OpenAIRealtimeTranslationClient::connect(self).await
    }

    async fn append_input_audio(&self, pcm16: &[i16]) -> Result<(), OpenAITranslationError> {
        OpenAIRealtimeTranslationClient::append_input_audio(self, pcm16).await
    }

    async fn close(&mut self, drain_timeout: Duration) -> Result<(), OpenAITranslationError> {
        OpenAIRealtimeTranslationClient::close(self, drain_timeout).await
    }

    async fn abort(&mut self) {
        OpenAIRealtimeTranslationClient::abort(self).await
    }
}

impl Default for LiveTranslationService {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveTranslationService {
    pub fn new() -> Self {
        Self::new_with_audio_factory(Arc::new(DefaultPlatformAudioFactory::new()))
    }

    pub fn new_with_audio_factory(audio_factory: Arc<dyn PlatformAudioFactory>) -> Self {
        Self::new_with_factories(
            audio_factory,
            Arc::new(OpenAIRealtimeTranslationClientFactory),
        )
    }

    fn new_with_factories(
        audio_factory: Arc<dyn PlatformAudioFactory>,
        client_factory: Arc<dyn RealtimeTranslationClientFactory>,
    ) -> Self {
        Self {
            status: Arc::new(RwLock::new(RecordingStatus::Idle)),
            inner: Arc::new(Mutex::new(None)),
            audio_factory,
            client_factory,
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
                "OpenAI API key не задан. Укажите ключ в Settings или задайте OPENAI_API_KEY"
                    .into(),
            );
            self.transition_to_error().await;
            return Err(err);
        }
        let target_language = normalize_live_translation_target_language(&config.target_language);

        // 2. Output device - fail cheap, before OpenAI session creation.
        let mut output_concrete = match self.audio_factory.create_translation_output() {
            Ok(output) => output,
            Err(e) => {
                let err = LiveTranslationError::Configuration(e.to_string());
                self.transition_to_error().await;
                return Err(err);
            }
        };
        if let Err(e) = output_concrete
            .open(TranslationAudioOutputConfig::openai_translation())
            .await
        {
            let err = LiveTranslationError::Configuration(e.to_string());
            self.transition_to_error().await;
            return Err(err);
        }
        let output = Arc::new(RwLock::new(output_concrete));

        // 3. Mic preflight. Do this before paid OpenAI session creation.
        if let Err(e) = self.audio_factory.microphone_preflight() {
            let _ = output.write().await.close().await;
            let err = LiveTranslationError::Configuration(e.to_string());
            self.transition_to_error().await;
            return Err(err);
        }

        let capture_result = self.audio_factory.create_microphone_capture(
            config.microphone_device.clone(),
            AudioCaptureTarget::outgoing_translation(),
        );
        let capture = match capture_result {
            Ok(c) => c,
            Err(e) => {
                let _ = output.write().await.close().await;
                let err = LiveTranslationError::Configuration(format!("mic init: {}", e));
                self.transition_to_error().await;
                return Err(err);
            }
        };
        let capture = Arc::new(RwLock::new(capture));
        if let Err(e) = capture
            .write()
            .await
            .initialize(AudioConfig {
                sample_rate: AudioCaptureTarget::outgoing_translation().sample_rate,
                channels: AudioCaptureTarget::outgoing_translation().channels,
                buffer_size: AudioConfig::default().buffer_size,
            })
            .await
        {
            let _ = output.write().await.close().await;
            let err = LiveTranslationError::Configuration(format!("mic init: {}", e));
            self.transition_to_error().await;
            return Err(err);
        }

        // 4. OpenAI client connect
        let mut client = self
            .client_factory
            .create(config.openai_api_key.clone(), target_language.clone());
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
        let (mic_tx, mic_rx) = mpsc::channel::<AudioChunk>(MIC_QUEUE_CAPACITY_CHUNKS);
        let dropped_mic_chunks = Arc::new(AtomicU64::new(0));
        let mic_callback: AudioChunkCallback = Arc::new(move |chunk: AudioChunk| {
            try_enqueue_mic_chunk(&mic_tx, chunk, &dropped_mic_chunks);
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
        if mark_live_recording_started(&self.status, &callbacks).await {
            log::info!(
                "LiveTranslationService: session {} started, target={}, sensitivity={}",
                config.session_id,
                target_language,
                config.microphone_sensitivity
            );
        } else {
            log::warn!(
                "LiveTranslationService: session {} failed before start completed",
                config.session_id
            );
        }
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

fn should_mark_live_recording_started(current_status: RecordingStatus) -> bool {
    current_status == RecordingStatus::Starting
}

async fn mark_live_recording_started(
    status: &Arc<RwLock<RecordingStatus>>,
    callbacks: &LiveTranslationCallbacks,
) -> bool {
    let mut status_guard = status.write().await;
    if !should_mark_live_recording_started(*status_guard) {
        return false;
    }

    *status_guard = RecordingStatus::Recording;
    drop(status_guard);
    (callbacks.on_status)(RecordingStatus::Recording);
    true
}

fn take_ready_openai_input_frames(buffer: &mut Vec<i16>) -> Vec<Vec<i16>> {
    let mut frames = Vec::new();
    while buffer.len() >= OPENAI_INPUT_FRAME_SAMPLES {
        frames.push(buffer.drain(..OPENAI_INPUT_FRAME_SAMPLES).collect());
    }
    frames
}

fn take_padded_final_openai_input_frame(buffer: &mut Vec<i16>) -> Option<Vec<i16>> {
    if buffer.is_empty() {
        return None;
    }

    let mut frame = std::mem::take(buffer);
    frame.resize(OPENAI_INPUT_FRAME_SAMPLES, 0);
    Some(frame)
}

async fn send_openai_input_frame(
    client: &Arc<Mutex<Box<dyn RealtimeTranslationClientPort>>>,
    frame: &[i16],
) -> Result<(), LiveTranslationError> {
    let client = client.lock().await;
    client.append_input_audio(frame).await.map_err(Into::into)
}

async fn run_audio_pump(
    mut mic_rx: mpsc::Receiver<AudioChunk>,
    client: Arc<Mutex<Box<dyn RealtimeTranslationClientPort>>>,
    sensitivity: u8,
    on_spectrum: Arc<dyn Fn([f32; 48]) + Send + Sync>,
    runtime_stop_tx: mpsc::UnboundedSender<RuntimeStop>,
) {
    let gain = microphone_sensitivity_gain(sensitivity);
    let mut spectrum = AudioSpectrumAnalyzer::new();
    let mut openai_input_buffer = Vec::<i16>::with_capacity(OPENAI_INPUT_FRAME_SAMPLES * 2);
    let mut failed = false;

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

        openai_input_buffer.extend_from_slice(&amplified);
        for frame in take_ready_openai_input_frames(&mut openai_input_buffer) {
            if let Err(kind_err) = send_openai_input_frame(&client, &frame).await {
                log::warn!("LiveTranslationService audio pump send error: {}", kind_err);
                let _ = runtime_stop_tx.send(RuntimeStop::Error(kind_err));
                failed = true;
                break;
            }
        }

        if failed {
            break;
        }
    }

    if !failed {
        if let Some(frame) = take_padded_final_openai_input_frame(&mut openai_input_buffer) {
            if let Err(kind_err) = send_openai_input_frame(&client, &frame).await {
                log::warn!(
                    "LiveTranslationService final audio frame send error: {}",
                    kind_err
                );
                let _ = runtime_stop_tx.send(RuntimeStop::Error(kind_err));
            }
        }
    }

    log::info!("LiveTranslationService: audio pump exited");
}

fn try_enqueue_mic_chunk(
    mic_tx: &mpsc::Sender<AudioChunk>,
    chunk: AudioChunk,
    dropped_mic_chunks: &AtomicU64,
) {
    match mic_tx.try_send(chunk) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(_chunk)) => {
            let dropped = dropped_mic_chunks.fetch_add(1, AtomicOrdering::Relaxed) + 1;
            if dropped == 1 || dropped % 100 == 0 {
                log::warn!(
                    "LiveTranslationService: dropped {} mic chunks because OpenAI input queue is full",
                    dropped
                );
            }
        }
        Err(mpsc::error::TrySendError::Closed(_chunk)) => {}
    }
}

async fn run_event_forwarder(
    mut openai_rx: mpsc::Receiver<OpenAIRealtimeEvent>,
    output: Arc<RwLock<Box<dyn TranslationAudioOutput>>>,
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

        let mut guard = inner.lock().await;
        let is_current = guard
            .as_ref()
            .map(|session| session.session_id == session_id)
            .unwrap_or(false);
        let session = if is_current { guard.take() } else { None };

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
        drop(guard);
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
            {
                let out = session.output.read().await;
                out.begin_drain_mode();
            }

            let audio_pump_finished = wait_task_done(
                &mut session.audio_pump_task,
                Duration::from_millis(MIC_PUMP_DRAIN_TIMEOUT_MS),
                "audio pump",
                session_id,
            )
            .await;
            if !audio_pump_finished {
                abort_task_done(
                    &mut session.audio_pump_task,
                    Duration::from_millis(MIC_PUMP_DRAIN_TIMEOUT_MS),
                    "audio pump",
                    session_id,
                )
                .await;
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
                session.client.lock().await.abort().await;
            }

            let forwarder_finished = wait_task_done(
                &mut session.forwarder_task,
                Duration::from_millis(FORWARDER_DRAIN_TIMEOUT_MS),
                "event forwarder",
                session_id,
            )
            .await;
            if !forwarder_finished {
                abort_task_done(
                    &mut session.forwarder_task,
                    Duration::from_millis(FORWARDER_DRAIN_TIMEOUT_MS),
                    "event forwarder",
                    session_id,
                )
                .await;
            }

            drain_output_tail(session.output.clone(), session_id).await;
        }
        CleanupMode::RuntimeFailure => {
            session.client.lock().await.abort().await;
            abort_task_done(
                &mut session.audio_pump_task,
                Duration::from_millis(MIC_PUMP_DRAIN_TIMEOUT_MS),
                "audio pump",
                session_id,
            )
            .await;
            abort_task_done(
                &mut session.forwarder_task,
                Duration::from_millis(FORWARDER_DRAIN_TIMEOUT_MS),
                "event forwarder",
                session_id,
            )
            .await;
        }
    }

    if let Err(e) = session.output.write().await.close().await {
        log::warn!(
            "LiveTranslationService cleanup: output close failed for session {}: {}",
            session_id,
            e
        );
    }

    if matches!(mode, CleanupMode::GracefulStop) {
        session.audio_pump_task.abort();
        session.forwarder_task.abort();
    }

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

async fn abort_task_done(
    task: &mut JoinHandle<()>,
    timeout: Duration,
    label: &str,
    session_id: u64,
) -> bool {
    task.abort();
    match tokio::time::timeout(timeout, task).await {
        Ok(Ok(())) => {
            log::debug!(
                "LiveTranslationService cleanup: {} stopped for session {}",
                label,
                session_id
            );
            true
        }
        Ok(Err(e)) if e.is_cancelled() => {
            log::debug!(
                "LiveTranslationService cleanup: {} aborted for session {}",
                label,
                session_id
            );
            true
        }
        Ok(Err(e)) => {
            log::warn!(
                "LiveTranslationService cleanup: {} abort join failed for session {}: {}",
                label,
                session_id,
                e
            );
            true
        }
        Err(_) => {
            log::warn!(
                "LiveTranslationService cleanup: {} did not abort within {} ms for session {}",
                label,
                timeout.as_millis(),
                session_id
            );
            false
        }
    }
}

async fn drain_output_tail(output: Arc<RwLock<Box<dyn TranslationAudioOutput>>>, session_id: u64) {
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;

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

    #[test]
    fn blackhole_input_device_names_are_detected() {
        use crate::infrastructure::audio::is_macos_blackhole_device_name;

        assert!(is_macos_blackhole_device_name("BlackHole 2ch"));
        assert!(is_macos_blackhole_device_name("blackhole"));
        assert!(!is_macos_blackhole_device_name("Внешний микрофон"));
        assert!(!is_macos_blackhole_device_name("MacBook Pro Microphone"));
    }

    #[test]
    fn openai_input_buffer_emits_200ms_frames() {
        let mut buffer = vec![1; 2_000];
        assert!(take_ready_openai_input_frames(&mut buffer).is_empty());

        buffer.extend(std::iter::repeat_n(2, 2_800));
        let frames = take_ready_openai_input_frames(&mut buffer);

        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].len(), OPENAI_INPUT_FRAME_SAMPLES);
        assert!(buffer.is_empty());
    }

    #[test]
    fn final_openai_input_frame_is_padded_to_200ms() {
        let mut buffer = vec![7; 123];
        let frame = take_padded_final_openai_input_frame(&mut buffer).unwrap();

        assert_eq!(frame.len(), OPENAI_INPUT_FRAME_SAMPLES);
        assert!(frame[..123].iter().all(|sample| *sample == 7));
        assert!(frame[123..].iter().all(|sample| *sample == 0));
        assert!(buffer.is_empty());
    }

    #[test]
    fn try_enqueue_mic_chunk_drops_when_bounded_queue_is_full() {
        let (tx, mut rx) = mpsc::channel::<AudioChunk>(1);
        let dropped = AtomicU64::new(0);
        let first = AudioChunk::new(vec![1; 10], 24_000, 1);
        let second = AudioChunk::new(vec![2; 10], 24_000, 1);

        try_enqueue_mic_chunk(&tx, first.clone(), &dropped);
        try_enqueue_mic_chunk(&tx, second, &dropped);

        assert_eq!(dropped.load(AtomicOrdering::Relaxed), 1);
        assert_eq!(rx.try_recv().unwrap().data, first.data);
    }

    #[test]
    fn live_start_status_guard_does_not_overwrite_terminal_status() {
        assert!(should_mark_live_recording_started(
            RecordingStatus::Starting
        ));
        assert!(!should_mark_live_recording_started(RecordingStatus::Error));
        assert!(!should_mark_live_recording_started(
            RecordingStatus::Processing
        ));
        assert!(!should_mark_live_recording_started(RecordingStatus::Idle));
    }

    #[test]
    fn live_translation_target_language_is_trimmed_and_defaulted() {
        assert_eq!(normalize_live_translation_target_language("  es\n"), "es");
        assert_eq!(normalize_live_translation_target_language(""), "en");
        assert_eq!(normalize_live_translation_target_language("auto"), "en");
        assert_eq!(normalize_live_translation_target_language("MULTI"), "en");
    }

    #[tokio::test]
    async fn service_starts_in_idle() {
        let svc = LiveTranslationService::new();
        assert_eq!(svc.get_status().await, RecordingStatus::Idle);
        assert!(svc.active_session_id().await.is_none());
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum TestFactoryMode {
        OutputCreateFails,
        MicPreflightFails,
        MicInitializeFails,
        MicCreateFails,
    }

    #[derive(Default)]
    struct TestFactoryState {
        output_create_calls: AtomicUsize,
        output_opened: AtomicBool,
        output_closed: AtomicBool,
        mic_preflight_calls: AtomicUsize,
        mic_create_calls: AtomicUsize,
        mic_initialize_calls: AtomicUsize,
    }

    struct TestTranslationOutput {
        state: Arc<TestFactoryState>,
    }

    #[async_trait::async_trait]
    impl TranslationAudioOutput for TestTranslationOutput {
        async fn open(
            &mut self,
            _config: TranslationAudioOutputConfig,
        ) -> crate::domain::TranslationAudioOutputResult<()> {
            self.state.output_opened.store(true, Ordering::SeqCst);
            Ok(())
        }

        async fn enqueue_pcm16(
            &self,
            _samples: &[i16],
        ) -> crate::domain::TranslationAudioOutputResult<()> {
            Ok(())
        }

        async fn close(&mut self) -> crate::domain::TranslationAudioOutputResult<()> {
            self.state.output_closed.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn is_open(&self) -> bool {
            self.state.output_opened.load(Ordering::SeqCst)
                && !self.state.output_closed.load(Ordering::SeqCst)
        }

        fn device_name(&self) -> Option<String> {
            Some("test-output".to_string())
        }

        fn begin_drain_mode(&self) {}

        fn prepare_for_drain(&self) -> crate::domain::TranslationAudioOutputResult<Duration> {
            Ok(Duration::ZERO)
        }

        fn pending_playback_duration(&self) -> Duration {
            Duration::ZERO
        }
    }

    struct TestAudioCapture {
        config: crate::domain::AudioConfig,
        state: Arc<TestFactoryState>,
        fail_initialize: bool,
    }

    #[async_trait::async_trait]
    impl AudioCapture for TestAudioCapture {
        async fn initialize(
            &mut self,
            config: crate::domain::AudioConfig,
        ) -> crate::domain::AudioResult<()> {
            self.state
                .mic_initialize_calls
                .fetch_add(1, Ordering::SeqCst);
            if self.fail_initialize {
                return Err(crate::domain::AudioError::Configuration(
                    "simulated mic initialize failure".to_string(),
                ));
            }
            self.config = config;
            Ok(())
        }

        async fn start_capture(
            &mut self,
            _on_chunk: AudioChunkCallback,
        ) -> crate::domain::AudioResult<()> {
            Ok(())
        }

        async fn stop_capture(&mut self) -> crate::domain::AudioResult<()> {
            Ok(())
        }

        fn is_capturing(&self) -> bool {
            false
        }

        fn config(&self) -> crate::domain::AudioConfig {
            self.config
        }
    }

    struct TestPlatformAudioFactory {
        mode: TestFactoryMode,
        state: Arc<TestFactoryState>,
    }

    #[async_trait::async_trait]
    impl PlatformAudioFactory for TestPlatformAudioFactory {
        fn create_microphone_capture(
            &self,
            _device_name: Option<String>,
            _target: AudioCaptureTarget,
        ) -> crate::domain::AudioResult<Box<dyn AudioCapture>> {
            self.state.mic_create_calls.fetch_add(1, Ordering::SeqCst);
            if self.mode == TestFactoryMode::MicCreateFails {
                return Err(crate::domain::AudioError::Configuration(
                    "simulated mic create failure".to_string(),
                ));
            }
            Ok(Box::new(TestAudioCapture {
                config: crate::domain::AudioConfig::default(),
                state: self.state.clone(),
                fail_initialize: self.mode == TestFactoryMode::MicInitializeFails,
            }))
        }

        fn create_translation_output(
            &self,
        ) -> crate::domain::TranslationAudioOutputResult<Box<dyn TranslationAudioOutput>> {
            self.state
                .output_create_calls
                .fetch_add(1, Ordering::SeqCst);
            if self.mode == TestFactoryMode::OutputCreateFails {
                return Err(crate::domain::TranslationAudioOutputError::Configuration(
                    "simulated output create failure".to_string(),
                ));
            }
            Ok(Box::new(TestTranslationOutput {
                state: self.state.clone(),
            }))
        }

        fn create_system_loopback_capture(
            &self,
            _target: AudioCaptureTarget,
        ) -> crate::domain::AudioResult<Box<dyn AudioCapture>> {
            Ok(Box::new(TestAudioCapture {
                config: crate::domain::AudioConfig::default(),
                state: self.state.clone(),
                fail_initialize: false,
            }))
        }

        async fn setup_status(&self) -> crate::domain::PlatformAudioSetupStatus {
            crate::domain::PlatformAudioSetupStatus {
                platform: "test".to_string(),
                status: crate::domain::PlatformAudioSetupState::Ready,
                outgoing_supported: true,
                incoming_supported: true,
                virtual_microphone_name: "test-output".to_string(),
                message: "ready".to_string(),
            }
        }

        fn is_virtual_microphone_input(&self, _name: &str) -> bool {
            false
        }

        fn microphone_preflight(&self) -> Result<(), crate::domain::AudioError> {
            self.state
                .mic_preflight_calls
                .fetch_add(1, Ordering::SeqCst);
            if self.mode == TestFactoryMode::MicPreflightFails {
                return Err(crate::domain::AudioError::AccessDenied(
                    "simulated mic access denied".to_string(),
                ));
            }
            Ok(())
        }
    }

    fn test_callbacks() -> LiveTranslationCallbacks {
        LiveTranslationCallbacks {
            on_transcript_delta: Arc::new(|_| {}),
            on_audio_spectrum: Arc::new(|_| {}),
            on_error: Arc::new(|_| {}),
            on_status: Arc::new(|_| {}),
        }
    }

    fn valid_config(session_id: u64) -> LiveTranslationConfig {
        LiveTranslationConfig {
            openai_api_key: "sk-test".to_string(),
            target_language: "en".into(),
            microphone_device: None,
            microphone_sensitivity: 100,
            session_id,
        }
    }

    #[derive(Default)]
    struct SyntheticOutputState {
        opened: AtomicBool,
        closed: AtomicBool,
        open_config: StdMutex<Option<TranslationAudioOutputConfig>>,
        enqueued: StdMutex<Vec<i16>>,
    }

    struct SyntheticTranslationOutput {
        state: Arc<SyntheticOutputState>,
    }

    #[async_trait::async_trait]
    impl TranslationAudioOutput for SyntheticTranslationOutput {
        async fn open(
            &mut self,
            config: TranslationAudioOutputConfig,
        ) -> crate::domain::TranslationAudioOutputResult<()> {
            self.state.opened.store(true, Ordering::SeqCst);
            self.state.closed.store(false, Ordering::SeqCst);
            *self.state.open_config.lock().unwrap() = Some(config);
            Ok(())
        }

        async fn enqueue_pcm16(
            &self,
            samples: &[i16],
        ) -> crate::domain::TranslationAudioOutputResult<()> {
            self.state
                .enqueued
                .lock()
                .unwrap()
                .extend_from_slice(samples);
            Ok(())
        }

        async fn close(&mut self) -> crate::domain::TranslationAudioOutputResult<()> {
            self.state.closed.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn is_open(&self) -> bool {
            self.state.opened.load(Ordering::SeqCst) && !self.state.closed.load(Ordering::SeqCst)
        }

        fn device_name(&self) -> Option<String> {
            Some("synthetic-virtual-mic".to_string())
        }

        fn begin_drain_mode(&self) {}

        fn prepare_for_drain(&self) -> crate::domain::TranslationAudioOutputResult<Duration> {
            Ok(Duration::ZERO)
        }

        fn pending_playback_duration(&self) -> Duration {
            Duration::ZERO
        }
    }

    struct SyntheticMicCapture {
        chunks: Vec<AudioChunk>,
        config: crate::domain::AudioConfig,
        started: Arc<AtomicBool>,
        stopped: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl AudioCapture for SyntheticMicCapture {
        async fn initialize(
            &mut self,
            config: crate::domain::AudioConfig,
        ) -> crate::domain::AudioResult<()> {
            self.config = config;
            Ok(())
        }

        async fn start_capture(
            &mut self,
            on_chunk: AudioChunkCallback,
        ) -> crate::domain::AudioResult<()> {
            self.started.store(true, Ordering::SeqCst);
            for chunk in self.chunks.clone() {
                on_chunk(chunk);
            }
            Ok(())
        }

        async fn stop_capture(&mut self) -> crate::domain::AudioResult<()> {
            self.stopped.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn is_capturing(&self) -> bool {
            self.started.load(Ordering::SeqCst) && !self.stopped.load(Ordering::SeqCst)
        }

        fn config(&self) -> crate::domain::AudioConfig {
            self.config
        }
    }

    struct SyntheticPlatformAudioFactory {
        output_state: Arc<SyntheticOutputState>,
        capture_started: Arc<AtomicBool>,
        capture_stopped: Arc<AtomicBool>,
        mic_target: Arc<StdMutex<Option<AudioCaptureTarget>>>,
    }

    #[async_trait::async_trait]
    impl PlatformAudioFactory for SyntheticPlatformAudioFactory {
        fn create_microphone_capture(
            &self,
            _device_name: Option<String>,
            target: AudioCaptureTarget,
        ) -> crate::domain::AudioResult<Box<dyn AudioCapture>> {
            *self.mic_target.lock().unwrap() = Some(target);
            Ok(Box::new(SyntheticMicCapture {
                chunks: vec![AudioChunk::new(
                    vec![1_200; OPENAI_INPUT_FRAME_SAMPLES],
                    24_000,
                    1,
                )],
                config: crate::domain::AudioConfig::default(),
                started: self.capture_started.clone(),
                stopped: self.capture_stopped.clone(),
            }))
        }

        fn create_translation_output(
            &self,
        ) -> crate::domain::TranslationAudioOutputResult<Box<dyn TranslationAudioOutput>> {
            Ok(Box::new(SyntheticTranslationOutput {
                state: self.output_state.clone(),
            }))
        }

        fn create_system_loopback_capture(
            &self,
            _target: AudioCaptureTarget,
        ) -> crate::domain::AudioResult<Box<dyn AudioCapture>> {
            Err(crate::domain::AudioError::Configuration(
                "not used in live translation e2e".to_string(),
            ))
        }

        async fn setup_status(&self) -> crate::domain::PlatformAudioSetupStatus {
            crate::domain::PlatformAudioSetupStatus {
                platform: "test".to_string(),
                status: crate::domain::PlatformAudioSetupState::Ready,
                outgoing_supported: true,
                incoming_supported: true,
                virtual_microphone_name: "synthetic-virtual-mic".to_string(),
                message: "ready".to_string(),
            }
        }

        fn is_virtual_microphone_input(&self, _name: &str) -> bool {
            false
        }
    }

    #[derive(Default)]
    struct SyntheticRealtimeState {
        connect_calls: AtomicUsize,
        append_calls: AtomicUsize,
        close_calls: AtomicUsize,
        abort_calls: AtomicUsize,
        fail_close: AtomicBool,
        target_language: StdMutex<Option<String>>,
        received_samples: StdMutex<Vec<i16>>,
        event_tx: StdMutex<Option<mpsc::Sender<OpenAIRealtimeEvent>>>,
        runtime_event_after_first_append: StdMutex<Option<OpenAIRealtimeEvent>>,
    }

    struct SyntheticRealtimeClientFactory {
        state: Arc<SyntheticRealtimeState>,
    }

    impl RealtimeTranslationClientFactory for SyntheticRealtimeClientFactory {
        fn create(
            &self,
            _api_key: String,
            target_language: String,
        ) -> Box<dyn RealtimeTranslationClientPort> {
            *self.state.target_language.lock().unwrap() = Some(target_language);
            Box::new(SyntheticRealtimeClient {
                state: self.state.clone(),
            })
        }
    }

    struct SyntheticRealtimeClient {
        state: Arc<SyntheticRealtimeState>,
    }

    #[async_trait::async_trait]
    impl RealtimeTranslationClientPort for SyntheticRealtimeClient {
        async fn connect(
            &mut self,
        ) -> Result<mpsc::Receiver<OpenAIRealtimeEvent>, OpenAITranslationError> {
            self.state.connect_calls.fetch_add(1, Ordering::SeqCst);
            let (tx, rx) = mpsc::channel(16);
            *self.state.event_tx.lock().unwrap() = Some(tx.clone());
            let _ = tx.try_send(OpenAIRealtimeEvent::SessionCreated);
            let _ = tx.try_send(OpenAIRealtimeEvent::SessionUpdated);
            Ok(rx)
        }

        async fn append_input_audio(&self, pcm16: &[i16]) -> Result<(), OpenAITranslationError> {
            let call = self.state.append_calls.fetch_add(1, Ordering::SeqCst);
            self.state
                .received_samples
                .lock()
                .unwrap()
                .extend_from_slice(pcm16);

            if call == 0 {
                let tx = self.state.event_tx.lock().unwrap().clone();
                if let Some(tx) = tx {
                    let _ = tx
                        .send(OpenAIRealtimeEvent::TranscriptDelta("hello ".to_string()))
                        .await;
                    let _ = tx
                        .send(OpenAIRealtimeEvent::AudioDelta(vec![9_000; 2_400]))
                        .await;
                    let runtime_event = {
                        self.state
                            .runtime_event_after_first_append
                            .lock()
                            .unwrap()
                            .take()
                    };
                    if let Some(event) = runtime_event {
                        let _ = tx.send(event).await;
                    }
                }
            }
            Ok(())
        }

        async fn close(&mut self, _drain_timeout: Duration) -> Result<(), OpenAITranslationError> {
            self.state.close_calls.fetch_add(1, Ordering::SeqCst);
            if self.state.fail_close.load(Ordering::SeqCst) {
                return Err(OpenAITranslationError::Connection(
                    "simulated close failure".to_string(),
                ));
            }
            let tx = self.state.event_tx.lock().unwrap().clone();
            if let Some(tx) = tx {
                let _ = tx
                    .send(OpenAIRealtimeEvent::TranscriptDelta("world".to_string()))
                    .await;
                let _ = tx
                    .send(OpenAIRealtimeEvent::AudioDelta(vec![-9_000; 1_200]))
                    .await;
                let _ = tx.send(OpenAIRealtimeEvent::Closed).await;
            }
            Ok(())
        }

        async fn abort(&mut self) {
            self.state.abort_calls.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[derive(Default)]
    struct BlockingOutputState {
        enqueue_entered: AtomicBool,
        closed: AtomicBool,
    }

    struct BlockingTranslationOutput {
        state: Arc<BlockingOutputState>,
    }

    #[async_trait::async_trait]
    impl TranslationAudioOutput for BlockingTranslationOutput {
        async fn open(
            &mut self,
            _config: TranslationAudioOutputConfig,
        ) -> crate::domain::TranslationAudioOutputResult<()> {
            Ok(())
        }

        async fn enqueue_pcm16(
            &self,
            _samples: &[i16],
        ) -> crate::domain::TranslationAudioOutputResult<()> {
            self.state.enqueue_entered.store(true, Ordering::SeqCst);
            std::future::pending::<()>().await;
            Ok(())
        }

        async fn close(&mut self) -> crate::domain::TranslationAudioOutputResult<()> {
            self.state.closed.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn is_open(&self) -> bool {
            !self.state.closed.load(Ordering::SeqCst)
        }

        fn device_name(&self) -> Option<String> {
            Some("blocking-output".to_string())
        }

        fn begin_drain_mode(&self) {}

        fn prepare_for_drain(&self) -> crate::domain::TranslationAudioOutputResult<Duration> {
            Ok(Duration::ZERO)
        }

        fn pending_playback_duration(&self) -> Duration {
            Duration::ZERO
        }
    }

    #[tokio::test]
    async fn runtime_failure_aborts_forwarder_before_closing_output() {
        let output_state = Arc::new(BlockingOutputState::default());
        let output: Arc<RwLock<Box<dyn TranslationAudioOutput>>> =
            Arc::new(RwLock::new(Box::new(BlockingTranslationOutput {
                state: output_state.clone(),
            })));
        let capture_stopped = Arc::new(AtomicBool::new(false));
        let capture: Arc<RwLock<Box<dyn AudioCapture>>> =
            Arc::new(RwLock::new(Box::new(SyntheticMicCapture {
                chunks: Vec::new(),
                config: AudioConfig::default(),
                started: Arc::new(AtomicBool::new(true)),
                stopped: capture_stopped.clone(),
            })));
        let realtime_state = Arc::new(SyntheticRealtimeState::default());
        let client: Arc<Mutex<Box<dyn RealtimeTranslationClientPort>>> =
            Arc::new(Mutex::new(Box::new(SyntheticRealtimeClient {
                state: realtime_state.clone(),
            })));

        let (_runtime_stop_tx, runtime_stop_rx) = mpsc::unbounded_channel::<RuntimeStop>();
        drop(runtime_stop_rx);
        let (openai_tx, openai_rx) = mpsc::channel::<OpenAIRealtimeEvent>(1);
        let (forwarder_stop_tx, _forwarder_stop_rx) = mpsc::unbounded_channel::<RuntimeStop>();
        let forwarder_task = tokio::spawn(run_event_forwarder(
            openai_rx,
            output.clone(),
            Arc::new(|_| {}),
            forwarder_stop_tx,
        ));
        openai_tx
            .send(OpenAIRealtimeEvent::AudioDelta(vec![1_000; 480]))
            .await
            .unwrap();

        tokio::time::timeout(Duration::from_secs(1), async {
            while !output_state.enqueue_entered.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("forwarder must enter blocking enqueue");

        let audio_pump_task = tokio::spawn(async {
            std::future::pending::<()>().await;
        });
        let session = RunningSession {
            capture,
            output,
            client,
            forwarder_task,
            audio_pump_task,
            session_id: 95,
        };

        tokio::time::timeout(
            Duration::from_secs(2),
            cleanup_session(session, CleanupMode::RuntimeFailure),
        )
        .await
        .expect("runtime cleanup must not block on output write lock");

        assert!(capture_stopped.load(Ordering::SeqCst));
        assert!(output_state.closed.load(Ordering::SeqCst));
        assert_eq!(realtime_state.abort_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn graceful_stop_aborts_stuck_forwarder_before_closing_output() {
        let output_state = Arc::new(BlockingOutputState::default());
        let output: Arc<RwLock<Box<dyn TranslationAudioOutput>>> =
            Arc::new(RwLock::new(Box::new(BlockingTranslationOutput {
                state: output_state.clone(),
            })));
        let capture_stopped = Arc::new(AtomicBool::new(false));
        let capture: Arc<RwLock<Box<dyn AudioCapture>>> =
            Arc::new(RwLock::new(Box::new(SyntheticMicCapture {
                chunks: Vec::new(),
                config: AudioConfig::default(),
                started: Arc::new(AtomicBool::new(true)),
                stopped: capture_stopped.clone(),
            })));
        let realtime_state = Arc::new(SyntheticRealtimeState::default());
        let client: Arc<Mutex<Box<dyn RealtimeTranslationClientPort>>> =
            Arc::new(Mutex::new(Box::new(SyntheticRealtimeClient {
                state: realtime_state.clone(),
            })));

        let (openai_tx, openai_rx) = mpsc::channel::<OpenAIRealtimeEvent>(1);
        let (forwarder_stop_tx, _forwarder_stop_rx) = mpsc::unbounded_channel::<RuntimeStop>();
        let forwarder_task = tokio::spawn(run_event_forwarder(
            openai_rx,
            output.clone(),
            Arc::new(|_| {}),
            forwarder_stop_tx,
        ));
        openai_tx
            .send(OpenAIRealtimeEvent::AudioDelta(vec![1_000; 480]))
            .await
            .unwrap();

        tokio::time::timeout(Duration::from_secs(1), async {
            while !output_state.enqueue_entered.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("forwarder must enter blocking enqueue");

        let audio_pump_task = tokio::spawn(async {});
        let session = RunningSession {
            capture,
            output,
            client,
            forwarder_task,
            audio_pump_task,
            session_id: 96,
        };

        tokio::time::timeout(
            Duration::from_secs(4),
            cleanup_session(session, CleanupMode::GracefulStop),
        )
        .await
        .expect("graceful cleanup must not block on output write lock");

        assert!(capture_stopped.load(Ordering::SeqCst));
        assert!(output_state.closed.load(Ordering::SeqCst));
        assert_eq!(realtime_state.close_calls.load(Ordering::SeqCst), 1);
        assert_eq!(realtime_state.abort_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn synthetic_outgoing_translation_e2e_pumps_mic_to_virtual_output() {
        let output_state = Arc::new(SyntheticOutputState::default());
        let realtime_state = Arc::new(SyntheticRealtimeState::default());
        let capture_started = Arc::new(AtomicBool::new(false));
        let capture_stopped = Arc::new(AtomicBool::new(false));
        let mic_target = Arc::new(StdMutex::new(None));

        let svc = LiveTranslationService::new_with_factories(
            Arc::new(SyntheticPlatformAudioFactory {
                output_state: output_state.clone(),
                capture_started: capture_started.clone(),
                capture_stopped: capture_stopped.clone(),
                mic_target: mic_target.clone(),
            }),
            Arc::new(SyntheticRealtimeClientFactory {
                state: realtime_state.clone(),
            }),
        );

        let translated_text = Arc::new(StdMutex::new(String::new()));
        let statuses = Arc::new(StdMutex::new(Vec::new()));
        let callbacks = LiveTranslationCallbacks {
            on_transcript_delta: {
                let translated_text = translated_text.clone();
                Arc::new(move |text| translated_text.lock().unwrap().push_str(&text))
            },
            on_audio_spectrum: Arc::new(|_| {}),
            on_error: Arc::new(|err| panic!("unexpected live translation error: {err}")),
            on_status: {
                let statuses = statuses.clone();
                Arc::new(move |status| statuses.lock().unwrap().push(status))
            },
        };

        let mut config = valid_config(91);
        config.target_language = "  es\n".to_string();
        svc.start_translation(config, callbacks).await.unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            let got_audio = !output_state.enqueued.lock().unwrap().is_empty();
            let got_text = !translated_text.lock().unwrap().is_empty();
            let got_input =
                realtime_state.received_samples.lock().unwrap().len() >= OPENAI_INPUT_FRAME_SAMPLES;
            if got_audio && got_text && got_input {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        assert_eq!(svc.get_status().await, RecordingStatus::Recording);
        assert!(capture_started.load(Ordering::SeqCst));
        assert_eq!(
            mic_target.lock().unwrap().unwrap().sample_rate,
            AudioCaptureTarget::outgoing_translation().sample_rate
        );
        assert_eq!(
            output_state
                .open_config
                .lock()
                .unwrap()
                .unwrap()
                .source_sample_rate,
            24_000
        );
        assert_eq!(
            realtime_state.target_language.lock().unwrap().as_deref(),
            Some("es")
        );
        assert!(
            realtime_state.received_samples.lock().unwrap().len() >= OPENAI_INPUT_FRAME_SAMPLES
        );
        assert!(translated_text.lock().unwrap().contains("hello"));
        assert!(!output_state.enqueued.lock().unwrap().is_empty());

        svc.stop_translation().await.unwrap();

        assert_eq!(svc.get_status().await, RecordingStatus::Idle);
        assert!(capture_stopped.load(Ordering::SeqCst));
        assert!(output_state.closed.load(Ordering::SeqCst));
        assert_eq!(realtime_state.connect_calls.load(Ordering::SeqCst), 1);
        assert_eq!(realtime_state.close_calls.load(Ordering::SeqCst), 1);
        assert_eq!(realtime_state.abort_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            statuses.lock().unwrap().as_slice(),
            &[RecordingStatus::Starting, RecordingStatus::Recording]
        );
    }

    #[tokio::test]
    async fn graceful_stop_aborts_openai_client_when_close_fails() {
        let output_state = Arc::new(SyntheticOutputState::default());
        let realtime_state = Arc::new(SyntheticRealtimeState::default());
        realtime_state.fail_close.store(true, Ordering::SeqCst);
        let capture_started = Arc::new(AtomicBool::new(false));
        let capture_stopped = Arc::new(AtomicBool::new(false));
        let mic_target = Arc::new(StdMutex::new(None));

        let svc = LiveTranslationService::new_with_factories(
            Arc::new(SyntheticPlatformAudioFactory {
                output_state: output_state.clone(),
                capture_started: capture_started.clone(),
                capture_stopped: capture_stopped.clone(),
                mic_target,
            }),
            Arc::new(SyntheticRealtimeClientFactory {
                state: realtime_state.clone(),
            }),
        );

        svc.start_translation(valid_config(94), test_callbacks())
            .await
            .unwrap();
        svc.stop_translation().await.unwrap();

        assert_eq!(svc.get_status().await, RecordingStatus::Idle);
        assert!(capture_started.load(Ordering::SeqCst));
        assert!(capture_stopped.load(Ordering::SeqCst));
        assert!(output_state.closed.load(Ordering::SeqCst));
        assert_eq!(realtime_state.close_calls.load(Ordering::SeqCst), 1);
        assert_eq!(realtime_state.abort_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn unexpected_openai_close_cleans_session_and_allows_restart() {
        let output_state = Arc::new(SyntheticOutputState::default());
        let realtime_state = Arc::new(SyntheticRealtimeState::default());
        *realtime_state
            .runtime_event_after_first_append
            .lock()
            .unwrap() = Some(OpenAIRealtimeEvent::Closed);
        let capture_started = Arc::new(AtomicBool::new(false));
        let capture_stopped = Arc::new(AtomicBool::new(false));
        let mic_target = Arc::new(StdMutex::new(None));

        let svc = LiveTranslationService::new_with_factories(
            Arc::new(SyntheticPlatformAudioFactory {
                output_state: output_state.clone(),
                capture_started: capture_started.clone(),
                capture_stopped: capture_stopped.clone(),
                mic_target,
            }),
            Arc::new(SyntheticRealtimeClientFactory {
                state: realtime_state.clone(),
            }),
        );

        let errors = Arc::new(StdMutex::new(Vec::<String>::new()));
        let statuses = Arc::new(StdMutex::new(Vec::<RecordingStatus>::new()));
        let callbacks = LiveTranslationCallbacks {
            on_transcript_delta: Arc::new(|_| {}),
            on_audio_spectrum: Arc::new(|_| {}),
            on_error: {
                let errors = errors.clone();
                Arc::new(move |err| errors.lock().unwrap().push(err.to_string()))
            },
            on_status: {
                let statuses = statuses.clone();
                Arc::new(move |status| statuses.lock().unwrap().push(status))
            },
        };

        svc.start_translation(valid_config(92), callbacks)
            .await
            .unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            if svc.get_status().await == RecordingStatus::Error {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        assert_eq!(svc.get_status().await, RecordingStatus::Error);
        assert!(svc.active_session_id().await.is_none());
        assert!(capture_stopped.load(Ordering::SeqCst));
        assert!(output_state.closed.load(Ordering::SeqCst));
        assert_eq!(realtime_state.abort_calls.load(Ordering::SeqCst), 1);
        assert!(
            errors
                .lock()
                .unwrap()
                .iter()
                .any(|err| err.contains("closed unexpectedly")),
            "expected closed-session error, got {:?}",
            errors.lock().unwrap()
        );
        assert!(
            statuses.lock().unwrap().contains(&RecordingStatus::Error),
            "expected Error status, got {:?}",
            statuses.lock().unwrap()
        );

        svc.start_translation(valid_config(93), test_callbacks())
            .await
            .unwrap();
        assert_eq!(svc.get_status().await, RecordingStatus::Recording);
        svc.stop_translation().await.unwrap();
        assert_eq!(svc.get_status().await, RecordingStatus::Idle);
        assert_eq!(realtime_state.connect_calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn output_create_failure_stops_before_microphone_preflight() {
        let state = Arc::new(TestFactoryState::default());
        let svc =
            LiveTranslationService::new_with_audio_factory(Arc::new(TestPlatformAudioFactory {
                mode: TestFactoryMode::OutputCreateFails,
                state: state.clone(),
            }));

        let err = svc
            .start_translation(valid_config(11), test_callbacks())
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            LiveTranslationError::Configuration(msg)
                if msg.contains("simulated output create failure")
        ));
        assert_eq!(state.output_create_calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.mic_preflight_calls.load(Ordering::SeqCst), 0);
        assert_eq!(state.mic_create_calls.load(Ordering::SeqCst), 0);
        assert_eq!(state.mic_initialize_calls.load(Ordering::SeqCst), 0);
        assert!(!state.output_opened.load(Ordering::SeqCst));
        assert!(!state.output_closed.load(Ordering::SeqCst));
        assert_eq!(svc.get_status().await, RecordingStatus::Error);
        assert!(svc.active_session_id().await.is_none());
    }

    #[tokio::test]
    async fn microphone_preflight_failure_closes_opened_output() {
        let state = Arc::new(TestFactoryState::default());
        let svc =
            LiveTranslationService::new_with_audio_factory(Arc::new(TestPlatformAudioFactory {
                mode: TestFactoryMode::MicPreflightFails,
                state: state.clone(),
            }));

        let err = svc
            .start_translation(valid_config(12), test_callbacks())
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            LiveTranslationError::Configuration(msg)
                if msg.contains("simulated mic access denied")
        ));
        assert_eq!(state.output_create_calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.mic_preflight_calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.mic_create_calls.load(Ordering::SeqCst), 0);
        assert_eq!(state.mic_initialize_calls.load(Ordering::SeqCst), 0);
        assert!(state.output_opened.load(Ordering::SeqCst));
        assert!(state.output_closed.load(Ordering::SeqCst));
        assert_eq!(svc.get_status().await, RecordingStatus::Error);
        assert!(svc.active_session_id().await.is_none());
    }

    #[tokio::test]
    async fn microphone_initialize_failure_closes_opened_output() {
        let state = Arc::new(TestFactoryState::default());
        let svc =
            LiveTranslationService::new_with_audio_factory(Arc::new(TestPlatformAudioFactory {
                mode: TestFactoryMode::MicInitializeFails,
                state: state.clone(),
            }));

        let err = svc
            .start_translation(valid_config(13), test_callbacks())
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            LiveTranslationError::Configuration(msg)
                if msg.contains("simulated mic initialize failure")
        ));
        assert_eq!(state.output_create_calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.mic_preflight_calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.mic_create_calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.mic_initialize_calls.load(Ordering::SeqCst), 1);
        assert!(state.output_opened.load(Ordering::SeqCst));
        assert!(state.output_closed.load(Ordering::SeqCst));
        assert_eq!(svc.get_status().await, RecordingStatus::Error);
        assert!(svc.active_session_id().await.is_none());
    }

    #[tokio::test]
    async fn microphone_create_failure_closes_opened_output() {
        let state = Arc::new(TestFactoryState::default());
        let svc =
            LiveTranslationService::new_with_audio_factory(Arc::new(TestPlatformAudioFactory {
                mode: TestFactoryMode::MicCreateFails,
                state: state.clone(),
            }));

        let err = svc
            .start_translation(valid_config(14), test_callbacks())
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            LiveTranslationError::Configuration(msg)
                if msg.contains("simulated mic create failure")
        ));
        assert_eq!(state.output_create_calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.mic_preflight_calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.mic_create_calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.mic_initialize_calls.load(Ordering::SeqCst), 0);
        assert!(state.output_opened.load(Ordering::SeqCst));
        assert!(state.output_closed.load(Ordering::SeqCst));
        assert_eq!(svc.get_status().await, RecordingStatus::Error);
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
