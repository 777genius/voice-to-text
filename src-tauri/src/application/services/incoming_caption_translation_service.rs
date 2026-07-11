//! IncomingCaptionTranslationService - text subtitles for system audio.
//!
//! Pipeline:
//! - platform system audio capture, 16 kHz mono PCM16
//! - STT provider from current app config
//! - finalized transcript chunks -> OpenAI text translation
//! - translated text -> UI events
//!
//! This is separate from dictation and outgoing live translation:
//! - no auto-paste/copy/history
//! - no recording hotkey ownership
//! - no virtual microphone output

use std::collections::{HashSet, VecDeque};
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::FutureExt;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::task::JoinHandle;

use crate::domain::{
    AudioCapture, AudioCaptureTarget, AudioChunk, AudioChunkCallback, AudioConfig,
    ConnectionQualityCallback, ErrorCallback, PlatformAudioFactory, RecordingStatus, SttConfig,
    SttConnectionCategory, SttConnectionError, SttError, SttProvider, SttProviderFactory,
    SttResult, Transcription, TranscriptionCallback,
};
use crate::infrastructure::audio::DefaultPlatformAudioFactory;
use crate::infrastructure::openai::{OpenAITextTranslationClient, OpenAITextTranslationError};
use crate::infrastructure::DefaultSttProviderFactory;

const TARGET_LANGUAGE_DEFAULT: &str = "ru";
const AUDIO_QUEUE_CAPACITY: usize = 256;
const AUDIO_QUEUE_OVERLOAD_DROP_THRESHOLD: u64 = 32;
const TRANSLATION_QUEUE_CAPACITY: usize = 64;
const MAX_TRANSLATION_SEGMENT_BYTES: usize = 64 * 1024;
const STOP_DRAIN_TIMEOUT_MS: u64 = 1_800;
const STOP_TRANSLATION_DRAIN_TIMEOUT_MS: u64 = 3_000;
const STOP_TRANSLATION_DRAIN_POLL_MS: u64 = 20;
const SILENCE_PEAK_THRESHOLD: i32 = 220;
const SILENCE_KEEPALIVE_CHUNKS: u32 = 25;
const TRANSLATION_FAILURES_BEFORE_UI_ERROR: u32 = 3;
const TRANSLATION_MAX_ATTEMPTS: u32 = 2;
const TRANSLATION_RETRY_DELAY_MS: u64 = 200;
const TRANSLATED_SEGMENT_DEDUPE_CAPACITY: usize = 2048;
const STT_START_TIMEOUT: Duration = Duration::from_secs(20);
const STT_SEND_TIMEOUT: Duration = Duration::from_secs(6);
const STT_STOP_TIMEOUT: Duration = Duration::from_secs(10);
const STT_ABORT_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone)]
pub struct IncomingTranslationConfig {
    pub stt_config: SttConfig,
    pub openai_api_key: String,
    pub target_language: String,
    pub session_id: u64,
}

impl IncomingTranslationConfig {
    pub fn new_with_defaults(stt_config: SttConfig, session_id: u64) -> Self {
        Self {
            stt_config,
            openai_api_key: std::env::var("OPENAI_API_KEY").unwrap_or_default(),
            target_language: TARGET_LANGUAGE_DEFAULT.to_string(),
            session_id,
        }
    }
}

fn normalize_incoming_translation_target_language(value: &str) -> String {
    let language = value.trim();
    if language.is_empty()
        || language.eq_ignore_ascii_case("auto")
        || language.eq_ignore_ascii_case("multi")
    {
        TARGET_LANGUAGE_DEFAULT.to_string()
    } else {
        language.to_string()
    }
}

#[derive(Clone)]
pub struct IncomingTranslationCallbacks {
    pub on_source_final: Arc<dyn Fn(String) + Send + Sync>,
    pub on_translation_delta: Arc<dyn Fn(String) + Send + Sync>,
    pub on_error: Arc<dyn Fn(IncomingTranslationError) + Send + Sync>,
    pub on_status: Arc<dyn Fn(RecordingStatus) + Send + Sync>,
}

#[derive(Clone)]
struct IncomingRuntimeFailureReporter {
    callbacks: IncomingTranslationCallbacks,
    running: Arc<AtomicBool>,
    status: Arc<RwLock<RecordingStatus>>,
    runtime_cleanup_tx: mpsc::UnboundedSender<()>,
    startup_error: Arc<StdMutex<Option<IncomingTranslationError>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AudioQueueEnqueueError {
    Full(u64),
    Closed,
}

impl IncomingRuntimeFailureReporter {
    fn report(&self, error: IncomingTranslationError) -> bool {
        let mut startup_error = match self.startup_error.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if self
            .running
            .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return false;
        }
        *startup_error = Some(error.clone());
        drop(startup_error);

        let callbacks = self.callbacks.clone();
        let status = self.status.clone();
        let runtime_cleanup_tx = self.runtime_cleanup_tx.clone();
        tokio::spawn(async move {
            *status.write().await = RecordingStatus::Error;
            notify_incoming_runtime_error(&callbacks, error);
            let _ = runtime_cleanup_tx.send(());
        });
        true
    }
}

fn spawn_incoming_runtime_task<F>(
    label: &'static str,
    future: F,
    runtime_failure_reporter: IncomingRuntimeFailureReporter,
) -> JoinHandle<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        if AssertUnwindSafe(future).catch_unwind().await.is_err() {
            let _ = runtime_failure_reporter.report(IncomingTranslationError::Processing(format!(
                "{} task panicked",
                label
            )));
        }
    })
}

fn notify_incoming_runtime_error(
    callbacks: &IncomingTranslationCallbacks,
    error: IncomingTranslationError,
) {
    call_incoming_callback("on_error", || (callbacks.on_error)(error));
    call_incoming_callback("Error status", || {
        (callbacks.on_status)(RecordingStatus::Error)
    });
}

fn call_incoming_callback(label: &str, callback: impl FnOnce()) {
    if std::panic::catch_unwind(AssertUnwindSafe(callback)).is_err() {
        log::error!(
            "IncomingCaptionTranslationService: {} callback panicked",
            label
        );
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum IncomingTranslationError {
    #[error("incoming translation session already active")]
    AlreadyActive,
    #[error("configuration: {0}")]
    Configuration(String),
    #[error("authentication: {0}")]
    Authentication(String),
    #[error("rate_limited: {0}")]
    RateLimited(String),
    #[error("connection: {0}")]
    Connection(String),
    #[error("processing: {0}")]
    Processing(String),
    #[error("unsupported_target_language: {0}")]
    UnsupportedTargetLanguage(String),
    #[error("permission_denied: {0}")]
    PermissionDenied(String),
    #[error("unsafe_audio_route: {0}")]
    UnsafeAudioRoute(String),
    #[error("input_device_lost: {0}")]
    InputDeviceLost(String),
    #[error("output_device_lost: {0}")]
    OutputDeviceLost(String),
    #[error("input_overload: {0}")]
    InputOverload(String),
    #[error("output_overload: {0}")]
    OutputOverload(String),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("timeout: {0}")]
    Timeout(String),
    #[error("{0}")]
    ReportedDuringStartup(Box<IncomingTranslationError>),
}

impl IncomingTranslationError {
    pub fn error_type(&self) -> &'static str {
        match self {
            Self::AlreadyActive => "already_active",
            Self::Configuration(_) => "configuration",
            Self::Authentication(_) => "authentication",
            Self::RateLimited(_) => "rate_limited",
            Self::Connection(_) => "connection",
            Self::Processing(_) => "processing",
            Self::UnsupportedTargetLanguage(_) => "unsupported_target_language",
            Self::PermissionDenied(_) => "permission_denied",
            Self::UnsafeAudioRoute(_) => "unsafe_audio_route",
            Self::InputDeviceLost(_) => "input_device_lost",
            Self::OutputDeviceLost(_) => "output_device_lost",
            Self::InputOverload(_) => "input_overload",
            Self::OutputOverload(_) => "output_overload",
            Self::Protocol(_) => "protocol",
            Self::Timeout(_) => "timeout",
            Self::ReportedDuringStartup(error) => error.error_type(),
        }
    }

    pub fn was_reported(&self) -> bool {
        matches!(self, Self::ReportedDuringStartup(_))
    }
}

impl From<SttError> for IncomingTranslationError {
    fn from(err: SttError) -> Self {
        match err {
            SttError::Configuration(msg) => Self::Configuration(msg),
            SttError::Authentication(msg) => Self::Authentication(msg),
            SttError::Connection(conn) => Self::Connection(conn.to_string()),
            SttError::Processing(msg) | SttError::Unsupported(msg) | SttError::Internal(msg) => {
                Self::Processing(msg)
            }
        }
    }
}

impl From<OpenAITextTranslationError> for IncomingTranslationError {
    fn from(err: OpenAITextTranslationError) -> Self {
        let message = err.to_string();
        match err {
            OpenAITextTranslationError::Authentication(_) => Self::Authentication(message),
            OpenAITextTranslationError::RateLimited(_) => Self::RateLimited(message),
            OpenAITextTranslationError::Connection(_) => Self::Connection(message),
            OpenAITextTranslationError::Protocol(_) => Self::Processing(message),
        }
    }
}

async fn await_stt_operation<F>(operation: F, timeout: Duration, label: &str) -> SttResult<()>
where
    F: Future<Output = SttResult<()>>,
{
    match tokio::time::timeout(timeout, operation).await {
        Ok(result) => result,
        Err(_) => Err(SttError::Connection(SttConnectionError::with_category(
            format!("{} timed out after {} ms", label, timeout.as_millis()),
            SttConnectionCategory::Timeout,
        ))),
    }
}

pub(super) struct IncomingCaptionTranslationService {
    status: Arc<RwLock<RecordingStatus>>,
    stt_factory: Arc<dyn SttProviderFactory>,
    audio_factory: Arc<dyn PlatformAudioFactory>,
    translator_factory: Arc<dyn TextTranslatorFactory>,
    inner: Arc<Mutex<Option<RunningIncomingSession>>>,
}

struct RunningIncomingSession {
    capture: Box<dyn AudioCapture>,
    stt_provider: Arc<Mutex<Box<dyn SttProvider>>>,
    audio_pump_task: JoinHandle<()>,
    translation_task: JoinHandle<()>,
    pending_translations: Arc<AtomicUsize>,
    running: Arc<AtomicBool>,
    stop_requested: Arc<AtomicBool>,
    session_id: u64,
}

#[derive(Debug, Clone)]
struct TranslationJob {
    text: String,
    source: &'static str,
    start: f64,
    duration: f64,
}

#[derive(Debug)]
struct BoundedSegmentDedupe {
    capacity: usize,
    keys: HashSet<String>,
    order: VecDeque<String>,
}

impl BoundedSegmentDedupe {
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            keys: HashSet::new(),
            order: VecDeque::new(),
        }
    }

    fn remember(&mut self, key: String) -> bool {
        if self.keys.contains(&key) {
            return false;
        }

        if self.keys.len() >= self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.keys.remove(&oldest);
            }
        }

        self.keys.insert(key.clone());
        self.order.push_back(key);
        true
    }

    fn forget(&mut self, key: &str) {
        if self.keys.remove(key) {
            self.order.retain(|stored| stored != key);
        }
    }

    #[cfg(test)]
    fn contains(&self, key: &str) -> bool {
        self.keys.contains(key)
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

#[async_trait]
trait TextTranslator: Send + Sync {
    async fn translate_text(
        &self,
        text: &str,
        target_language: &str,
    ) -> Result<String, OpenAITextTranslationError>;
}

trait TextTranslatorFactory: Send + Sync {
    fn create(
        &self,
        api_key: String,
    ) -> Result<Arc<dyn TextTranslator>, OpenAITextTranslationError>;
}

struct OpenAITextTranslatorFactory;

impl TextTranslatorFactory for OpenAITextTranslatorFactory {
    fn create(
        &self,
        api_key: String,
    ) -> Result<Arc<dyn TextTranslator>, OpenAITextTranslationError> {
        Ok(Arc::new(OpenAITextTranslationClient::new(api_key)?))
    }
}

#[async_trait]
impl TextTranslator for OpenAITextTranslationClient {
    async fn translate_text(
        &self,
        text: &str,
        target_language: &str,
    ) -> Result<String, OpenAITextTranslationError> {
        OpenAITextTranslationClient::translate_text(self, text, target_language).await
    }
}

impl Default for IncomingCaptionTranslationService {
    fn default() -> Self {
        Self::new()
    }
}

impl IncomingCaptionTranslationService {
    pub(super) fn new() -> Self {
        Self::new_with_factories(
            Arc::new(DefaultSttProviderFactory::new()),
            Arc::new(DefaultPlatformAudioFactory::new()),
        )
    }

    pub(super) fn new_with_factories(
        stt_factory: Arc<dyn SttProviderFactory>,
        audio_factory: Arc<dyn PlatformAudioFactory>,
    ) -> Self {
        Self {
            status: Arc::new(RwLock::new(RecordingStatus::Idle)),
            stt_factory,
            audio_factory,
            translator_factory: Arc::new(OpenAITextTranslatorFactory),
            inner: Arc::new(Mutex::new(None)),
        }
    }

    #[cfg(test)]
    fn new_with_all_factories(
        stt_factory: Arc<dyn SttProviderFactory>,
        audio_factory: Arc<dyn PlatformAudioFactory>,
        translator_factory: Arc<dyn TextTranslatorFactory>,
    ) -> Self {
        Self {
            status: Arc::new(RwLock::new(RecordingStatus::Idle)),
            stt_factory,
            audio_factory,
            translator_factory,
            inner: Arc::new(Mutex::new(None)),
        }
    }

    pub(super) async fn get_status(&self) -> RecordingStatus {
        *self.status.read().await
    }

    pub(super) async fn active_session_id(&self) -> Option<u64> {
        self.inner
            .lock()
            .await
            .as_ref()
            .map(|session| session.session_id)
    }

    pub(super) async fn state_snapshot(&self) -> (Option<u64>, RecordingStatus) {
        let guard = self.inner.lock().await;
        let session_id = guard.as_ref().map(|session| session.session_id);
        let status = *self.status.read().await;
        (session_id, status)
    }

    pub(super) async fn start(
        &self,
        config: IncomingTranslationConfig,
        callbacks: IncomingTranslationCallbacks,
    ) -> Result<(), IncomingTranslationError> {
        let mut guard = self.inner.lock().await;
        if guard.is_some() {
            return Err(IncomingTranslationError::AlreadyActive);
        }

        if config.openai_api_key.trim().is_empty() {
            return Err(IncomingTranslationError::Configuration(
                "OpenAI API key не задан. Укажите ключ в Settings или задайте OPENAI_API_KEY"
                    .to_string(),
            ));
        }
        let target_language =
            normalize_incoming_translation_target_language(&config.target_language);

        *self.status.write().await = RecordingStatus::Starting;
        call_incoming_callback("Starting status", || {
            (callbacks.on_status)(RecordingStatus::Starting)
        });

        let audio_target = AudioCaptureTarget::incoming_subtitles();
        let mut capture = match self
            .audio_factory
            .create_system_loopback_capture(audio_target)
        {
            Ok(capture) => capture,
            Err(e) => {
                self.reset_failed_start().await;
                return Err(IncomingTranslationError::Configuration(e.to_string()));
            }
        };
        if let Err(e) = capture
            .initialize(AudioConfig {
                sample_rate: audio_target.sample_rate,
                channels: audio_target.channels,
                buffer_size: AudioConfig::default().buffer_size,
            })
            .await
        {
            self.reset_failed_start().await;
            return Err(IncomingTranslationError::Configuration(e.to_string()));
        }

        let mut provider = match self.stt_factory.create(&config.stt_config) {
            Ok(provider) => provider,
            Err(e) => {
                self.reset_failed_start().await;
                return Err(e.into());
            }
        };
        if let Err(e) = provider.initialize(&config.stt_config).await {
            abort_initialized_stt_after_start_failure(
                &mut provider,
                config.session_id,
                "stt initialize",
            )
            .await;
            self.reset_failed_start().await;
            return Err(e.into());
        }

        let translator = match self
            .translator_factory
            .create(config.openai_api_key.clone())
        {
            Ok(translator) => translator,
            Err(e) => {
                abort_initialized_stt_after_start_failure(
                    &mut provider,
                    config.session_id,
                    "translator create",
                )
                .await;
                self.reset_failed_start().await;
                return Err(e.into());
            }
        };
        let running = Arc::new(AtomicBool::new(true));
        let stop_requested = Arc::new(AtomicBool::new(false));
        let pending_translations = Arc::new(AtomicUsize::new(0));
        let translated_segment_keys = Arc::new(StdMutex::new(BoundedSegmentDedupe::new(
            TRANSLATED_SEGMENT_DEDUPE_CAPACITY,
        )));
        let (translation_tx, translation_rx) =
            mpsc::channel::<TranslationJob>(TRANSLATION_QUEUE_CAPACITY);
        let (runtime_cleanup_tx, runtime_cleanup_rx) = mpsc::unbounded_channel::<()>();
        let startup_error = Arc::new(StdMutex::new(None));
        let runtime_failure_reporter = IncomingRuntimeFailureReporter {
            callbacks: callbacks.clone(),
            running: running.clone(),
            status: self.status.clone(),
            runtime_cleanup_tx: runtime_cleanup_tx.clone(),
            startup_error: startup_error.clone(),
        };
        let translation_task = spawn_incoming_runtime_task(
            "translation worker",
            run_translation_worker(
                translation_rx,
                translator,
                runtime_failure_reporter.clone(),
                target_language.clone(),
                pending_translations.clone(),
            ),
            runtime_failure_reporter.clone(),
        );

        let translated_segment_keys_for_final = translated_segment_keys.clone();
        let translation_tx_for_final = translation_tx.clone();
        let pending_translations_for_final = pending_translations.clone();
        let runtime_failure_reporter_for_final = runtime_failure_reporter.clone();
        let on_final: TranscriptionCallback = Arc::new(move |transcription: Transcription| {
            handle_finalized_transcription(
                transcription,
                &runtime_failure_reporter_for_final,
                translated_segment_keys_for_final.clone(),
                translation_tx_for_final.clone(),
                pending_translations_for_final.clone(),
                "final",
            );
        });

        let translated_segment_keys_for_partial = translated_segment_keys.clone();
        let translation_tx_for_partial = translation_tx.clone();
        let pending_translations_for_partial = pending_translations.clone();
        let runtime_failure_reporter_for_partial = runtime_failure_reporter.clone();
        let on_partial: TranscriptionCallback = Arc::new(move |transcription: Transcription| {
            if !transcription.is_final {
                return;
            }
            handle_finalized_transcription(
                transcription,
                &runtime_failure_reporter_for_partial,
                translated_segment_keys_for_partial.clone(),
                translation_tx_for_partial.clone(),
                pending_translations_for_partial.clone(),
                "partial_final",
            );
        });

        let runtime_failure_reporter_for_error = runtime_failure_reporter.clone();
        let on_error: ErrorCallback = Arc::new(move |err: SttError| {
            let _ = runtime_failure_reporter_for_error.report(err.into());
        });
        let on_connection_quality: ConnectionQualityCallback =
            Arc::new(move |_quality: String, _reason: Option<String>| {});

        let start_stream_result = await_stt_operation(
            provider.start_stream(on_partial, on_final, on_error, on_connection_quality),
            STT_START_TIMEOUT,
            "incoming STT start_stream",
        )
        .await;
        if let Err(e) = start_stream_result {
            running.store(false, Ordering::SeqCst);
            translation_task.abort();
            let _ = translation_task.await;
            abort_initialized_stt_after_start_failure(
                &mut provider,
                config.session_id,
                "stt start_stream",
            )
            .await;
            self.reset_failed_start().await;
            return Err(e.into());
        }

        let provider = Arc::new(Mutex::new(provider));
        let (audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(AUDIO_QUEUE_CAPACITY);
        let callback_running = running.clone();
        let callback_stop_requested = stop_requested.clone();
        let consecutive_dropped_audio_chunks = Arc::new(AtomicU64::new(0));
        let runtime_failure_reporter_for_audio = runtime_failure_reporter.clone();
        let on_chunk: AudioChunkCallback = Arc::new(move |chunk: AudioChunk| {
            if !callback_running.load(Ordering::Relaxed) {
                return;
            }
            match try_enqueue_audio_chunk(&audio_tx, chunk, &consecutive_dropped_audio_chunks) {
                Ok(()) => {}
                Err(AudioQueueEnqueueError::Full(consecutive_drops))
                    if consecutive_drops >= AUDIO_QUEUE_OVERLOAD_DROP_THRESHOLD =>
                {
                    let _ = runtime_failure_reporter_for_audio.report(
                        IncomingTranslationError::Processing(
                            "System audio processing cannot keep up; incoming subtitles were stopped to avoid silently losing speech"
                                .to_string(),
                        ),
                    );
                }
                Err(AudioQueueEnqueueError::Closed)
                    if !callback_stop_requested.load(Ordering::SeqCst) =>
                {
                    let _ = runtime_failure_reporter_for_audio.report(
                        IncomingTranslationError::Processing(
                            "System audio processor stopped unexpectedly".to_string(),
                        ),
                    );
                }
                Err(_) => {}
            }
        });

        if let Err(e) = capture.start_capture(on_chunk).await {
            cleanup_started_stt_after_capture_failure(
                provider.clone(),
                translation_task,
                running.clone(),
                config.session_id,
            )
            .await;
            self.reset_failed_start().await;
            return Err(IncomingTranslationError::Configuration(e.to_string()));
        }

        let pump_provider = provider.clone();
        let pump_callbacks = callbacks.clone();
        let pump_running = running.clone();
        let pump_stop_requested = stop_requested.clone();
        let pump_status = self.status.clone();
        let pump_runtime_cleanup_tx = runtime_cleanup_tx.clone();
        let audio_pump_task = spawn_incoming_runtime_task(
            "audio pump",
            async move {
                run_audio_pump(
                    audio_rx,
                    pump_provider,
                    pump_callbacks,
                    pump_running,
                    pump_stop_requested,
                    pump_status,
                    pump_runtime_cleanup_tx,
                )
                .await;
            },
            runtime_failure_reporter.clone(),
        );

        *guard = Some(RunningIncomingSession {
            capture,
            stt_provider: provider,
            audio_pump_task,
            translation_task,
            pending_translations,
            running: running.clone(),
            stop_requested,
            session_id: config.session_id,
        });
        spawn_runtime_cleanup_monitor(
            self.inner.clone(),
            self.status.clone(),
            runtime_cleanup_rx,
            config.session_id,
        );
        if mark_incoming_recording_started(&self.status, &running, &callbacks).await {
            log::info!(
                "IncomingCaptionTranslationService: session {} started, target={}",
                config.session_id,
                target_language
            );
        } else {
            log::warn!(
                "IncomingCaptionTranslationService: session {} failed before start completed",
                config.session_id
            );
            *self.status.write().await = RecordingStatus::Error;
            let reported_error = match startup_error.lock() {
                Ok(guard) => guard.clone(),
                Err(poisoned) => poisoned.into_inner().clone(),
            };
            return Err(match reported_error {
                Some(error) => IncomingTranslationError::ReportedDuringStartup(Box::new(error)),
                None => IncomingTranslationError::Processing(
                    "incoming translation failed before startup completed".to_string(),
                ),
            });
        }
        Ok(())
    }

    async fn reset_failed_start(&self) {
        *self.status.write().await = RecordingStatus::Idle;
    }

    pub(super) async fn stop(&self) -> Result<(), IncomingTranslationError> {
        let mut guard = self.inner.lock().await;
        let Some(mut session) = guard.take() else {
            *self.status.write().await = RecordingStatus::Idle;
            return Ok(());
        };

        session.stop_requested.store(true, Ordering::SeqCst);
        *self.status.write().await = RecordingStatus::Processing;

        if let Err(e) = session.capture.stop_capture().await {
            log::warn!(
                "IncomingCaptionTranslationService: stop capture failed for session {}: {}",
                session.session_id,
                e
            );
        }

        let _ = wait_task_done(
            &mut session.audio_pump_task,
            Duration::from_millis(STOP_DRAIN_TIMEOUT_MS),
            session.session_id,
        )
        .await;

        stop_stt_provider_with_abort(&session.stt_provider, session.session_id, "manual stop")
            .await;

        wait_pending_translations(
            session.pending_translations.clone(),
            Duration::from_millis(STOP_TRANSLATION_DRAIN_TIMEOUT_MS),
            session.session_id,
        )
        .await;

        session.running.store(false, Ordering::SeqCst);
        session.translation_task.abort();
        let _ = session.translation_task.await;

        *self.status.write().await = RecordingStatus::Idle;
        Ok(())
    }
}

fn handle_finalized_transcription(
    transcription: Transcription,
    runtime_failure_reporter: &IncomingRuntimeFailureReporter,
    translated_segment_keys: Arc<StdMutex<BoundedSegmentDedupe>>,
    translation_tx: mpsc::Sender<TranslationJob>,
    pending_translations: Arc<AtomicUsize>,
    source: &'static str,
) {
    let text = transcription.text.trim().to_string();
    if text.is_empty() || !runtime_failure_reporter.running.load(Ordering::Relaxed) {
        return;
    }
    if text.len() > MAX_TRANSLATION_SEGMENT_BYTES {
        let _ = runtime_failure_reporter.report(IncomingTranslationError::Processing(format!(
            "Speech segment is too large to translate safely ({} bytes, limit {} bytes)",
            text.len(),
            MAX_TRANSLATION_SEGMENT_BYTES
        )));
        return;
    }

    let key = finalized_segment_key(&transcription, &text);
    let should_translate = match key.as_ref() {
        Some(key) => remember_translated_segment_key(&translated_segment_keys, key),
        None => true,
    };

    if !should_translate {
        log::debug!(
            "IncomingCaptionTranslationService: skip duplicate {} segment '{}'",
            source,
            text
        );
        return;
    }

    log::info!(
        "IncomingCaptionTranslationService: translate {} segment len={}, start={:.2}s, duration={:.2}s",
        source,
        text.len(),
        transcription.start,
        transcription.duration
    );
    call_incoming_callback("source final", || {
        (runtime_failure_reporter.callbacks.on_source_final)(text.clone())
    });

    pending_translations.fetch_add(1, Ordering::SeqCst);
    if let Err(err) = translation_tx.try_send(TranslationJob {
        text,
        source,
        start: transcription.start,
        duration: transcription.duration,
    }) {
        decrement_pending_translations(&pending_translations);
        if let Some(key) = key.as_deref() {
            forget_translated_segment_key(&translated_segment_keys, key);
        }
        log::warn!(
            "IncomingCaptionTranslationService: translation queue unavailable for {} segment: {}",
            source,
            err
        );
        let _ = runtime_failure_reporter.report(IncomingTranslationError::Processing(
            "Translation queue is overloaded; incoming subtitles were stopped to avoid losing translated speech"
                .to_string(),
        ));
    }
}

fn remember_translated_segment_key(
    translated_segment_keys: &StdMutex<BoundedSegmentDedupe>,
    key: &str,
) -> bool {
    match translated_segment_keys.lock() {
        Ok(mut seen) => seen.remember(key.to_string()),
        Err(err) => {
            log::warn!(
                "IncomingCaptionTranslationService: segment dedupe lock poisoned: {}",
                err
            );
            true
        }
    }
}

fn forget_translated_segment_key(
    translated_segment_keys: &StdMutex<BoundedSegmentDedupe>,
    key: &str,
) {
    if let Ok(mut seen) = translated_segment_keys.lock() {
        seen.forget(key);
    }
}

fn should_mark_incoming_recording_started(
    current_status: RecordingStatus,
    is_running: bool,
) -> bool {
    is_running && current_status == RecordingStatus::Starting
}

async fn mark_incoming_recording_started(
    status: &Arc<RwLock<RecordingStatus>>,
    running: &AtomicBool,
    callbacks: &IncomingTranslationCallbacks,
) -> bool {
    let mut status_guard = status.write().await;
    if !should_mark_incoming_recording_started(*status_guard, running.load(Ordering::SeqCst)) {
        return false;
    }

    *status_guard = RecordingStatus::Recording;
    drop(status_guard);
    call_incoming_callback("Recording status", || {
        (callbacks.on_status)(RecordingStatus::Recording)
    });
    true
}

async fn run_translation_worker(
    mut translation_rx: mpsc::Receiver<TranslationJob>,
    translator: Arc<dyn TextTranslator>,
    runtime_failure_reporter: IncomingRuntimeFailureReporter,
    target_language: String,
    pending_translations: Arc<AtomicUsize>,
) {
    let mut consecutive_failures = 0u32;

    while let Some(job) = translation_rx.recv().await {
        let mut stop_after_error = false;
        if !runtime_failure_reporter.running.load(Ordering::Relaxed) {
            decrement_pending_translations(&pending_translations);
            drain_pending_translation_jobs(&mut translation_rx, &pending_translations);
            break;
        }

        log::info!(
            "IncomingCaptionTranslationService: request {} translation len={}, start={:.2}s, duration={:.2}s",
            job.source,
            job.text.len(),
            job.start,
            job.duration
        );

        match translate_text_with_retry(
            translator.as_ref(),
            &job.text,
            &target_language,
            runtime_failure_reporter.running.as_ref(),
        )
        .await
        {
            Ok(translated) => {
                consecutive_failures = 0;
                if runtime_failure_reporter.running.load(Ordering::Relaxed)
                    && !translated.trim().is_empty()
                {
                    call_incoming_callback("translation delta", || {
                        (runtime_failure_reporter.callbacks.on_translation_delta)(translated)
                    });
                }
            }
            Err(err) => {
                consecutive_failures = consecutive_failures.saturating_add(1);
                let should_emit = matches!(
                    err,
                    OpenAITextTranslationError::Authentication(_)
                        | OpenAITextTranslationError::RateLimited(_)
                ) || consecutive_failures >= TRANSLATION_FAILURES_BEFORE_UI_ERROR;

                log::warn!(
                    "IncomingCaptionTranslationService: translation failed ({}/{} before UI error): {}",
                    consecutive_failures,
                    TRANSLATION_FAILURES_BEFORE_UI_ERROR,
                    err
                );
                if should_emit && runtime_failure_reporter.running.load(Ordering::Relaxed) {
                    stop_after_error = runtime_failure_reporter.report(err.into());
                }
            }
        }

        decrement_pending_translations(&pending_translations);
        if stop_after_error {
            drain_pending_translation_jobs(&mut translation_rx, &pending_translations);
            break;
        }
    }
}

async fn translate_text_with_retry(
    translator: &dyn TextTranslator,
    text: &str,
    target_language: &str,
    running: &AtomicBool,
) -> Result<String, OpenAITextTranslationError> {
    let mut attempt = 1u32;
    loop {
        match translator.translate_text(text, target_language).await {
            Ok(translated) => return Ok(translated),
            Err(err)
                if attempt < TRANSLATION_MAX_ATTEMPTS
                    && running.load(Ordering::Relaxed)
                    && matches!(
                        &err,
                        OpenAITextTranslationError::Connection(_)
                            | OpenAITextTranslationError::Protocol(_)
                    ) =>
            {
                log::warn!(
                    "IncomingCaptionTranslationService: transient translation attempt {}/{} failed, retrying: {}",
                    attempt,
                    TRANSLATION_MAX_ATTEMPTS,
                    err
                );
                attempt += 1;
                tokio::time::sleep(Duration::from_millis(TRANSLATION_RETRY_DELAY_MS)).await;
            }
            Err(err) => return Err(err),
        }
    }
}

fn drain_pending_translation_jobs(
    translation_rx: &mut mpsc::Receiver<TranslationJob>,
    pending_translations: &AtomicUsize,
) {
    while translation_rx.try_recv().is_ok() {
        decrement_pending_translations(pending_translations);
    }
}

fn decrement_pending_translations(pending_translations: &AtomicUsize) {
    let _ = pending_translations.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
        Some(value.saturating_sub(1))
    });
}

fn finalized_segment_key(transcription: &Transcription, text: &str) -> Option<String> {
    if transcription.start <= 0.0 && transcription.duration <= 0.0 {
        return None;
    }

    Some(format!(
        "{:.3}:{:.3}:{}",
        transcription.start, transcription.duration, text
    ))
}

fn try_enqueue_audio_chunk(
    audio_tx: &mpsc::Sender<AudioChunk>,
    chunk: AudioChunk,
    consecutive_dropped_audio_chunks: &AtomicU64,
) -> Result<(), AudioQueueEnqueueError> {
    match audio_tx.try_send(chunk) {
        Ok(()) => {
            consecutive_dropped_audio_chunks.store(0, Ordering::Relaxed);
            Ok(())
        }
        Err(mpsc::error::TrySendError::Full(_chunk)) => {
            let dropped = consecutive_dropped_audio_chunks.fetch_add(1, Ordering::Relaxed) + 1;
            if dropped == 1 || dropped % 100 == 0 {
                log::warn!(
                    "IncomingCaptionTranslationService: dropped {} consecutive system audio chunks because STT input queue is full",
                    dropped
                );
            }
            Err(AudioQueueEnqueueError::Full(dropped))
        }
        Err(mpsc::error::TrySendError::Closed(_chunk)) => Err(AudioQueueEnqueueError::Closed),
    }
}

async fn run_audio_pump(
    mut audio_rx: mpsc::Receiver<AudioChunk>,
    provider: Arc<Mutex<Box<dyn SttProvider>>>,
    callbacks: IncomingTranslationCallbacks,
    running: Arc<AtomicBool>,
    stop_requested: Arc<AtomicBool>,
    status: Arc<RwLock<RecordingStatus>>,
    runtime_cleanup_tx: mpsc::UnboundedSender<()>,
) {
    let mut silence_chunks = 0u32;

    while let Some(chunk) = audio_rx.recv().await {
        if !running.load(Ordering::Relaxed) {
            break;
        }
        if should_skip_silent_chunk(&chunk, &mut silence_chunks) {
            continue;
        }

        let result = {
            let mut provider = provider.lock().await;
            await_stt_operation(
                provider.send_audio(&chunk),
                STT_SEND_TIMEOUT,
                "incoming STT send_audio",
            )
            .await
        };
        if let Err(err) = result {
            if running.load(Ordering::Relaxed) {
                running.store(false, Ordering::SeqCst);
                *status.write().await = RecordingStatus::Error;
                notify_incoming_runtime_error(&callbacks, err.into());
                let _ = runtime_cleanup_tx.send(());
            }
            break;
        }
    }

    if !stop_requested.load(Ordering::SeqCst)
        && running
            .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    {
        let err = IncomingTranslationError::Connection(
            "system audio capture stopped unexpectedly".to_string(),
        );
        *status.write().await = RecordingStatus::Error;
        notify_incoming_runtime_error(&callbacks, err);
        let _ = runtime_cleanup_tx.send(());
    }
}

fn spawn_runtime_cleanup_monitor(
    inner: Arc<Mutex<Option<RunningIncomingSession>>>,
    status: Arc<RwLock<RecordingStatus>>,
    mut runtime_cleanup_rx: mpsc::UnboundedReceiver<()>,
    session_id: u64,
) {
    tokio::spawn(async move {
        if runtime_cleanup_rx.recv().await.is_none() {
            return;
        }

        let mut guard = inner.lock().await;
        let is_current = guard
            .as_ref()
            .map(|session| session.session_id == session_id)
            .unwrap_or(false);
        let session = if is_current { guard.take() } else { None };

        let Some(session) = session else {
            return;
        };

        *status.write().await = RecordingStatus::Error;
        cleanup_session_after_runtime_error(session).await;
        drop(guard);
    });
}

async fn cleanup_session_after_runtime_error(mut session: RunningIncomingSession) {
    let session_id = session.session_id;
    session.stop_requested.store(true, Ordering::SeqCst);

    if let Err(e) = session.capture.stop_capture().await {
        log::warn!(
            "IncomingCaptionTranslationService runtime cleanup: stop capture failed for session {}: {}",
            session_id,
            e
        );
    }

    let _ = wait_task_done(
        &mut session.audio_pump_task,
        Duration::from_millis(STOP_DRAIN_TIMEOUT_MS),
        session_id,
    )
    .await;

    stop_stt_provider_with_abort(&session.stt_provider, session_id, "runtime cleanup").await;

    session.running.store(false, Ordering::SeqCst);
    session.translation_task.abort();
    let _ = session.translation_task.await;

    log::info!(
        "IncomingCaptionTranslationService: session {} cleaned up after runtime error",
        session_id
    );
}

async fn cleanup_started_stt_after_capture_failure(
    provider: Arc<Mutex<Box<dyn SttProvider>>>,
    translation_task: JoinHandle<()>,
    running: Arc<AtomicBool>,
    session_id: u64,
) {
    running.store(false, Ordering::SeqCst);

    stop_stt_provider_with_abort(&provider, session_id, "capture start failure").await;

    translation_task.abort();
    let _ = translation_task.await;
}

async fn abort_initialized_stt_after_start_failure(
    provider: &mut Box<dyn SttProvider>,
    session_id: u64,
    reason: &str,
) {
    if let Err(abort_err) =
        await_stt_operation(provider.abort(), STT_ABORT_TIMEOUT, "incoming STT abort").await
    {
        log::warn!(
            "IncomingCaptionTranslationService: stt abort after {} failure failed for session {}: {}",
            reason,
            session_id,
            abort_err
        );
    }
}

async fn stop_stt_provider_with_abort(
    provider: &Arc<Mutex<Box<dyn SttProvider>>>,
    session_id: u64,
    reason: &str,
) {
    let stop_result = {
        let mut provider = provider.lock().await;
        await_stt_operation(
            provider.stop_stream(),
            STT_STOP_TIMEOUT,
            "incoming STT stop_stream",
        )
        .await
    };
    if let Err(err) = stop_result {
        log::warn!(
            "IncomingCaptionTranslationService: stt stop failed during {} for session {}: {}",
            reason,
            session_id,
            err
        );
        let abort_result = {
            let mut provider = provider.lock().await;
            await_stt_operation(provider.abort(), STT_ABORT_TIMEOUT, "incoming STT abort").await
        };
        if let Err(abort_err) = abort_result {
            log::warn!(
                "IncomingCaptionTranslationService: stt abort failed during {} for session {}: {}",
                reason,
                session_id,
                abort_err
            );
        }
    }
}

fn should_skip_silent_chunk(chunk: &AudioChunk, silence_chunks: &mut u32) -> bool {
    let peak = chunk
        .data
        .iter()
        .map(|&sample| (sample as i32).abs())
        .max()
        .unwrap_or(0);

    if peak > SILENCE_PEAK_THRESHOLD {
        *silence_chunks = 0;
        return false;
    }

    *silence_chunks = silence_chunks.saturating_add(1);
    *silence_chunks > SILENCE_KEEPALIVE_CHUNKS
}

async fn wait_task_done(task: &mut JoinHandle<()>, timeout: Duration, session_id: u64) -> bool {
    tokio::select! {
        result = &mut *task => {
            if let Err(e) = result {
                if !e.is_cancelled() {
                    log::warn!("IncomingCaptionTranslationService: audio pump join failed for session {}: {}", session_id, e);
                }
            }
            true
        }
        _ = tokio::time::sleep(timeout) => {
            task.abort();
            let _ = task.await;
            false
        }
    }
}

async fn wait_pending_translations(
    pending_translations: Arc<AtomicUsize>,
    timeout: Duration,
    session_id: u64,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    while pending_translations.load(Ordering::SeqCst) > 0 {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            log::warn!(
                "IncomingCaptionTranslationService: translation drain timeout for session {} (pending={})",
                session_id,
                pending_translations.load(Ordering::SeqCst)
            );
            return;
        }

        tokio::time::sleep(
            Duration::from_millis(STOP_TRANSLATION_DRAIN_POLL_MS)
                .min(deadline.saturating_duration_since(now)),
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stt_operation_timeout_is_typed_as_connection_timeout() {
        let err = await_stt_operation(
            std::future::pending::<SttResult<()>>(),
            Duration::from_millis(20),
            "test operation",
        )
        .await
        .unwrap_err();

        assert!(matches!(
            err,
            SttError::Connection(connection)
                if connection.details.category == Some(SttConnectionCategory::Timeout)
                    && connection.message.contains("test operation timed out")
        ));
    }

    #[derive(Default)]
    struct TrackingProviderState {
        initialized: std::sync::atomic::AtomicBool,
        started: std::sync::atomic::AtomicBool,
        stopped: std::sync::atomic::AtomicBool,
        aborted: std::sync::atomic::AtomicBool,
    }

    struct TrackingSttProvider {
        state: std::sync::Arc<TrackingProviderState>,
        fail_initialize: bool,
        fail_stop: bool,
    }

    #[async_trait::async_trait]
    impl SttProvider for TrackingSttProvider {
        async fn initialize(&mut self, _config: &SttConfig) -> crate::domain::SttResult<()> {
            self.state.initialized.store(true, Ordering::SeqCst);
            if self.fail_initialize {
                Err(SttError::Connection(
                    crate::domain::SttConnectionError::simple("simulated initialize failure"),
                ))
            } else {
                Ok(())
            }
        }

        async fn start_stream(
            &mut self,
            _on_partial: TranscriptionCallback,
            _on_final: TranscriptionCallback,
            _on_error: ErrorCallback,
            _on_connection_quality: ConnectionQualityCallback,
        ) -> crate::domain::SttResult<()> {
            self.state.started.store(true, Ordering::SeqCst);
            Ok(())
        }

        async fn send_audio(&mut self, _chunk: &AudioChunk) -> crate::domain::SttResult<()> {
            Ok(())
        }

        async fn stop_stream(&mut self) -> crate::domain::SttResult<()> {
            self.state.stopped.store(true, Ordering::SeqCst);
            if self.fail_stop {
                Err(SttError::Connection(
                    crate::domain::SttConnectionError::simple("simulated stop failure"),
                ))
            } else {
                Ok(())
            }
        }

        async fn abort(&mut self) -> crate::domain::SttResult<()> {
            self.state.aborted.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn name(&self) -> &str {
            "tracking"
        }

        fn is_online(&self) -> bool {
            true
        }
    }

    struct TrackingSttFactory {
        state: std::sync::Arc<TrackingProviderState>,
        fail_initialize: bool,
        fail_stop: bool,
    }

    impl SttProviderFactory for TrackingSttFactory {
        fn create(&self, _config: &SttConfig) -> crate::domain::SttResult<Box<dyn SttProvider>> {
            Ok(Box::new(TrackingSttProvider {
                state: self.state.clone(),
                fail_initialize: self.fail_initialize,
                fail_stop: self.fail_stop,
            }))
        }
    }

    #[derive(Default)]
    struct FailingLoopbackCapture {
        config: crate::domain::AudioConfig,
    }

    #[async_trait::async_trait]
    impl AudioCapture for FailingLoopbackCapture {
        async fn initialize(
            &mut self,
            config: crate::domain::AudioConfig,
        ) -> crate::domain::AudioResult<()> {
            self.config = config;
            Ok(())
        }

        async fn start_capture(
            &mut self,
            _on_chunk: AudioChunkCallback,
        ) -> crate::domain::AudioResult<()> {
            Err(crate::domain::AudioError::Capture(
                "simulated loopback start failure".to_string(),
            ))
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

    struct FailingLoopbackAudioFactory;

    #[async_trait::async_trait]
    impl PlatformAudioFactory for FailingLoopbackAudioFactory {
        fn create_microphone_capture(
            &self,
            _device_name: Option<String>,
            _target: AudioCaptureTarget,
        ) -> crate::domain::AudioResult<Box<dyn AudioCapture>> {
            Err(crate::domain::AudioError::Configuration(
                "not used in incoming translation tests".to_string(),
            ))
        }

        fn create_translation_output(
            &self,
        ) -> crate::domain::TranslationAudioOutputResult<
            Box<dyn crate::domain::TranslationAudioOutput>,
        > {
            Err(crate::domain::TranslationAudioOutputError::Configuration(
                "not used in incoming translation tests".to_string(),
            ))
        }

        fn create_system_loopback_capture(
            &self,
            _target: AudioCaptureTarget,
        ) -> crate::domain::AudioResult<Box<dyn AudioCapture>> {
            Ok(Box::new(FailingLoopbackCapture::default()))
        }

        async fn setup_status(&self) -> crate::domain::PlatformAudioSetupStatus {
            crate::domain::PlatformAudioSetupStatus {
                platform: "test".to_string(),
                status: crate::domain::PlatformAudioSetupState::Ready,
                outgoing_supported: true,
                incoming_supported: true,
                virtual_microphone_name: "test".to_string(),
                message: "ready".to_string(),
            }
        }

        fn is_virtual_microphone_input(&self, _name: &str) -> bool {
            false
        }
    }

    #[derive(Default)]
    struct SyntheticIncomingCaptureState {
        initialized_config: StdMutex<Option<AudioConfig>>,
        started: std::sync::atomic::AtomicBool,
        stopped: std::sync::atomic::AtomicBool,
    }

    struct SyntheticIncomingCapture {
        state: std::sync::Arc<SyntheticIncomingCaptureState>,
        chunks: Vec<AudioChunk>,
        config: AudioConfig,
        callback: Option<AudioChunkCallback>,
    }

    #[async_trait::async_trait]
    impl AudioCapture for SyntheticIncomingCapture {
        async fn initialize(&mut self, config: AudioConfig) -> crate::domain::AudioResult<()> {
            self.config = config;
            *self.state.initialized_config.lock().unwrap() = Some(config);
            Ok(())
        }

        async fn start_capture(
            &mut self,
            on_chunk: AudioChunkCallback,
        ) -> crate::domain::AudioResult<()> {
            self.state.started.store(true, Ordering::SeqCst);
            for chunk in self.chunks.clone() {
                on_chunk(chunk);
            }
            self.callback = Some(on_chunk);
            Ok(())
        }

        async fn stop_capture(&mut self) -> crate::domain::AudioResult<()> {
            self.state.stopped.store(true, Ordering::SeqCst);
            self.callback = None;
            Ok(())
        }

        fn is_capturing(&self) -> bool {
            self.state.started.load(Ordering::SeqCst) && !self.state.stopped.load(Ordering::SeqCst)
        }

        fn config(&self) -> AudioConfig {
            self.config
        }
    }

    struct SyntheticIncomingAudioFactory {
        capture_state: std::sync::Arc<SyntheticIncomingCaptureState>,
        requested_target: std::sync::Arc<StdMutex<Option<AudioCaptureTarget>>>,
    }

    #[async_trait::async_trait]
    impl PlatformAudioFactory for SyntheticIncomingAudioFactory {
        fn create_microphone_capture(
            &self,
            _device_name: Option<String>,
            _target: AudioCaptureTarget,
        ) -> crate::domain::AudioResult<Box<dyn AudioCapture>> {
            Err(crate::domain::AudioError::Configuration(
                "not used in incoming translation e2e".to_string(),
            ))
        }

        fn create_translation_output(
            &self,
        ) -> crate::domain::TranslationAudioOutputResult<
            Box<dyn crate::domain::TranslationAudioOutput>,
        > {
            Err(crate::domain::TranslationAudioOutputError::Configuration(
                "not used in incoming translation e2e".to_string(),
            ))
        }

        fn create_system_loopback_capture(
            &self,
            target: AudioCaptureTarget,
        ) -> crate::domain::AudioResult<Box<dyn AudioCapture>> {
            *self.requested_target.lock().unwrap() = Some(target);
            Ok(Box::new(SyntheticIncomingCapture {
                state: self.capture_state.clone(),
                chunks: vec![AudioChunk::new(vec![1_000; 1_600], 16_000, 1)],
                config: AudioConfig::default(),
                callback: None,
            }))
        }

        async fn setup_status(&self) -> crate::domain::PlatformAudioSetupStatus {
            crate::domain::PlatformAudioSetupStatus {
                platform: "test".to_string(),
                status: crate::domain::PlatformAudioSetupState::Ready,
                outgoing_supported: true,
                incoming_supported: true,
                virtual_microphone_name: "synthetic".to_string(),
                message: "ready".to_string(),
            }
        }

        fn is_virtual_microphone_input(&self, _name: &str) -> bool {
            false
        }
    }

    #[derive(Default)]
    struct SyntheticIncomingProviderState {
        initialized: std::sync::atomic::AtomicBool,
        started: std::sync::atomic::AtomicBool,
        stopped: std::sync::atomic::AtomicBool,
        fail_during_start: std::sync::atomic::AtomicBool,
        fail_send: std::sync::atomic::AtomicBool,
        sent_chunks: std::sync::atomic::AtomicUsize,
        final_callback: StdMutex<Option<TranscriptionCallback>>,
        error_callback: StdMutex<Option<ErrorCallback>>,
    }

    struct SyntheticIncomingSttProvider {
        state: std::sync::Arc<SyntheticIncomingProviderState>,
    }

    #[async_trait::async_trait]
    impl SttProvider for SyntheticIncomingSttProvider {
        async fn initialize(&mut self, _config: &SttConfig) -> crate::domain::SttResult<()> {
            self.state.initialized.store(true, Ordering::SeqCst);
            Ok(())
        }

        async fn start_stream(
            &mut self,
            _on_partial: TranscriptionCallback,
            on_final: TranscriptionCallback,
            on_error: ErrorCallback,
            _on_connection_quality: ConnectionQualityCallback,
        ) -> crate::domain::SttResult<()> {
            self.state.started.store(true, Ordering::SeqCst);
            *self.state.final_callback.lock().unwrap() = Some(on_final);
            if self.state.fail_during_start.load(Ordering::SeqCst) {
                on_error(SttError::Connection(
                    crate::domain::SttConnectionError::simple(
                        "simulated receiver failure during startup",
                    ),
                ));
            }
            *self.state.error_callback.lock().unwrap() = Some(on_error);
            Ok(())
        }

        async fn send_audio(&mut self, chunk: &AudioChunk) -> crate::domain::SttResult<()> {
            if self.state.fail_send.load(Ordering::SeqCst) {
                return Err(SttError::Processing(
                    "simulated send_audio failure".to_string(),
                ));
            }

            let call = self.state.sent_chunks.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                if let Some(on_final) = self.state.final_callback.lock().unwrap().as_ref() {
                    on_final(
                        Transcription::final_result("hello from zoom".to_string())
                            .with_timing(0.0, chunk.duration_ms() as f64 / 1000.0),
                    );
                }
            }
            Ok(())
        }

        async fn stop_stream(&mut self) -> crate::domain::SttResult<()> {
            self.state.stopped.store(true, Ordering::SeqCst);
            Ok(())
        }

        async fn abort(&mut self) -> crate::domain::SttResult<()> {
            Ok(())
        }

        fn name(&self) -> &str {
            "synthetic-incoming-stt"
        }

        fn is_online(&self) -> bool {
            true
        }
    }

    struct SyntheticIncomingSttFactory {
        state: std::sync::Arc<SyntheticIncomingProviderState>,
    }

    impl SttProviderFactory for SyntheticIncomingSttFactory {
        fn create(&self, _config: &SttConfig) -> crate::domain::SttResult<Box<dyn SttProvider>> {
            Ok(Box::new(SyntheticIncomingSttProvider {
                state: self.state.clone(),
            }))
        }
    }

    #[derive(Default)]
    struct StopFinalProviderState {
        stopped: std::sync::atomic::AtomicBool,
        final_callback: StdMutex<Option<TranscriptionCallback>>,
    }

    struct StopFinalSttProvider {
        state: std::sync::Arc<StopFinalProviderState>,
    }

    #[async_trait::async_trait]
    impl SttProvider for StopFinalSttProvider {
        async fn initialize(&mut self, _config: &SttConfig) -> crate::domain::SttResult<()> {
            Ok(())
        }

        async fn start_stream(
            &mut self,
            _on_partial: TranscriptionCallback,
            on_final: TranscriptionCallback,
            _on_error: ErrorCallback,
            _on_connection_quality: ConnectionQualityCallback,
        ) -> crate::domain::SttResult<()> {
            *self.state.final_callback.lock().unwrap() = Some(on_final);
            Ok(())
        }

        async fn send_audio(&mut self, _chunk: &AudioChunk) -> crate::domain::SttResult<()> {
            Ok(())
        }

        async fn stop_stream(&mut self) -> crate::domain::SttResult<()> {
            self.state.stopped.store(true, Ordering::SeqCst);
            if let Some(on_final) = self.state.final_callback.lock().unwrap().as_ref() {
                on_final(
                    Transcription::final_result("late final from stop".to_string())
                        .with_timing(1.0, 0.5),
                );
            }
            Ok(())
        }

        async fn abort(&mut self) -> crate::domain::SttResult<()> {
            Ok(())
        }

        fn name(&self) -> &str {
            "stop-final-stt"
        }

        fn is_online(&self) -> bool {
            true
        }
    }

    struct StopFinalSttFactory {
        state: std::sync::Arc<StopFinalProviderState>,
    }

    impl SttProviderFactory for StopFinalSttFactory {
        fn create(&self, _config: &SttConfig) -> crate::domain::SttResult<Box<dyn SttProvider>> {
            Ok(Box::new(StopFinalSttProvider {
                state: self.state.clone(),
            }))
        }
    }

    #[derive(Default)]
    struct SyntheticTextTranslatorState {
        requests: StdMutex<Vec<(String, String)>>,
    }

    struct SyntheticTextTranslatorFactory {
        state: std::sync::Arc<SyntheticTextTranslatorState>,
    }

    impl TextTranslatorFactory for SyntheticTextTranslatorFactory {
        fn create(
            &self,
            _api_key: String,
        ) -> Result<Arc<dyn TextTranslator>, OpenAITextTranslationError> {
            Ok(Arc::new(SyntheticTextTranslator {
                state: self.state.clone(),
            }))
        }
    }

    struct SyntheticTextTranslator {
        state: std::sync::Arc<SyntheticTextTranslatorState>,
    }

    #[async_trait::async_trait]
    impl TextTranslator for SyntheticTextTranslator {
        async fn translate_text(
            &self,
            text: &str,
            target_language: &str,
        ) -> Result<String, OpenAITextTranslationError> {
            self.state
                .requests
                .lock()
                .unwrap()
                .push((text.to_string(), target_language.to_string()));
            Ok("привет из zoom".to_string())
        }
    }

    struct FlakyTextTranslator {
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl TextTranslator for FlakyTextTranslator {
        async fn translate_text(
            &self,
            text: &str,
            _target_language: &str,
        ) -> Result<String, OpenAITextTranslationError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                Err(OpenAITextTranslationError::Connection(
                    "temporary network blip".to_string(),
                ))
            } else {
                Ok(format!("{text} translated"))
            }
        }
    }

    struct RateLimitedTextTranslator;

    #[async_trait::async_trait]
    impl TextTranslator for RateLimitedTextTranslator {
        async fn translate_text(
            &self,
            _text: &str,
            _target_language: &str,
        ) -> Result<String, OpenAITextTranslationError> {
            Err(OpenAITextTranslationError::RateLimited(
                "simulated rate limit".to_string(),
            ))
        }
    }

    struct RateLimitedTextTranslatorFactory;

    impl TextTranslatorFactory for RateLimitedTextTranslatorFactory {
        fn create(
            &self,
            _api_key: String,
        ) -> Result<Arc<dyn TextTranslator>, OpenAITextTranslationError> {
            Ok(Arc::new(RateLimitedTextTranslator))
        }
    }

    struct FailingTextTranslatorFactory;

    impl TextTranslatorFactory for FailingTextTranslatorFactory {
        fn create(
            &self,
            _api_key: String,
        ) -> Result<Arc<dyn TextTranslator>, OpenAITextTranslationError> {
            Err(OpenAITextTranslationError::Authentication(
                "simulated translator create failure".to_string(),
            ))
        }
    }

    fn test_callbacks(
        statuses: std::sync::Arc<StdMutex<Vec<RecordingStatus>>>,
    ) -> IncomingTranslationCallbacks {
        IncomingTranslationCallbacks {
            on_source_final: Arc::new(|_| {}),
            on_translation_delta: Arc::new(|_| {}),
            on_error: Arc::new(|_| {}),
            on_status: Arc::new(move |status| {
                statuses.lock().unwrap().push(status);
            }),
        }
    }

    fn test_runtime_failure_reporter(
        callbacks: IncomingTranslationCallbacks,
        running: Arc<AtomicBool>,
    ) -> IncomingRuntimeFailureReporter {
        let (runtime_cleanup_tx, _runtime_cleanup_rx) = mpsc::unbounded_channel();
        IncomingRuntimeFailureReporter {
            callbacks,
            running,
            status: Arc::new(RwLock::new(RecordingStatus::Recording)),
            runtime_cleanup_tx,
            startup_error: Arc::new(StdMutex::new(None)),
        }
    }

    async fn wait_until_runtime_cleanup(
        service: &IncomingCaptionTranslationService,
        capture_state: &SyntheticIncomingCaptureState,
        provider_state: &SyntheticIncomingProviderState,
    ) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            if service.active_session_id().await.is_none()
                && capture_state.stopped.load(Ordering::SeqCst)
                && provider_state.stopped.load(Ordering::SeqCst)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        panic!(
            "runtime cleanup did not finish: active_session={:?}, capture_stopped={}, provider_stopped={}",
            service.active_session_id().await,
            capture_state.stopped.load(Ordering::SeqCst),
            provider_state.stopped.load(Ordering::SeqCst)
        );
    }

    #[test]
    fn silent_gate_keeps_initial_silence_then_skips() {
        let mut silence = 0;
        let chunk = AudioChunk::new(vec![0; 480], 16_000, 1);

        for _ in 0..SILENCE_KEEPALIVE_CHUNKS {
            assert!(!should_skip_silent_chunk(&chunk, &mut silence));
        }
        assert!(should_skip_silent_chunk(&chunk, &mut silence));
    }

    #[test]
    fn silent_gate_resets_on_audio() {
        let mut silence = SILENCE_KEEPALIVE_CHUNKS + 10;
        let chunk = AudioChunk::new(vec![SILENCE_PEAK_THRESHOLD as i16 + 1; 480], 16_000, 1);

        assert!(!should_skip_silent_chunk(&chunk, &mut silence));
        assert_eq!(silence, 0);
    }

    #[test]
    fn finalized_segment_key_uses_timing_when_available() {
        let transcription = Transcription::final_result("hello".to_string()).with_timing(1.25, 0.5);

        assert_eq!(
            finalized_segment_key(&transcription, "hello"),
            Some("1.250:0.500:hello".to_string())
        );
    }

    #[test]
    fn finalized_segment_key_skips_segments_without_timing() {
        let transcription = Transcription::final_result("hello".to_string());

        assert_eq!(finalized_segment_key(&transcription, "hello"), None);
    }

    #[test]
    fn finalized_segments_without_timing_can_repeat_same_text() {
        let source_finals = Arc::new(StdMutex::new(Vec::<String>::new()));
        let callbacks = IncomingTranslationCallbacks {
            on_source_final: {
                let source_finals = source_finals.clone();
                Arc::new(move |text| source_finals.lock().unwrap().push(text))
            },
            on_translation_delta: Arc::new(|_| {}),
            on_error: Arc::new(|_| {}),
            on_status: Arc::new(|_| {}),
        };
        let running = Arc::new(AtomicBool::new(true));
        let seen = Arc::new(StdMutex::new(BoundedSegmentDedupe::new(2)));
        let pending_translations = Arc::new(AtomicUsize::new(0));
        let (tx, mut rx) = mpsc::channel::<TranslationJob>(2);
        let transcription = Transcription::final_result("repeat".to_string());
        let runtime_failure_reporter =
            test_runtime_failure_reporter(callbacks.clone(), running.clone());

        handle_finalized_transcription(
            transcription.clone(),
            &runtime_failure_reporter,
            seen.clone(),
            tx.clone(),
            pending_translations.clone(),
            "final",
        );
        handle_finalized_transcription(
            transcription,
            &runtime_failure_reporter,
            seen.clone(),
            tx,
            pending_translations.clone(),
            "final",
        );

        assert!(seen.lock().unwrap().is_empty());
        assert_eq!(
            source_finals.lock().unwrap().as_slice(),
            &["repeat", "repeat"]
        );
        assert_eq!(rx.try_recv().unwrap().text, "repeat");
        assert_eq!(rx.try_recv().unwrap().text, "repeat");
        assert_eq!(pending_translations.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn try_enqueue_audio_chunk_tracks_only_consecutive_queue_drops() {
        let (tx, mut rx) = mpsc::channel::<AudioChunk>(1);
        let dropped = AtomicU64::new(0);
        let first = AudioChunk::new(vec![1; 8], 16_000, 1);
        let second = AudioChunk::new(vec![2; 8], 16_000, 1);
        let third = AudioChunk::new(vec![3; 8], 16_000, 1);

        assert_eq!(
            try_enqueue_audio_chunk(&tx, first.clone(), &dropped),
            Ok(())
        );
        assert_eq!(
            try_enqueue_audio_chunk(&tx, second, &dropped),
            Err(AudioQueueEnqueueError::Full(1))
        );

        assert_eq!(dropped.load(Ordering::Relaxed), 1);
        assert_eq!(rx.try_recv().unwrap().data, first.data);
        assert_eq!(try_enqueue_audio_chunk(&tx, third, &dropped), Ok(()));
        assert_eq!(dropped.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn try_enqueue_audio_chunk_reports_closed_processor_queue() {
        let (tx, rx) = mpsc::channel::<AudioChunk>(1);
        drop(rx);

        assert_eq!(
            try_enqueue_audio_chunk(
                &tx,
                AudioChunk::new(vec![1; 8], 16_000, 1),
                &AtomicU64::new(0),
            ),
            Err(AudioQueueEnqueueError::Closed)
        );
    }

    #[tokio::test]
    async fn runtime_task_panic_reports_error_and_requests_cleanup() {
        let running = Arc::new(AtomicBool::new(true));
        let status = Arc::new(RwLock::new(RecordingStatus::Recording));
        let errors = Arc::new(StdMutex::new(Vec::<String>::new()));
        let callbacks = IncomingTranslationCallbacks {
            on_source_final: Arc::new(|_| {}),
            on_translation_delta: Arc::new(|_| {}),
            on_error: {
                let errors = errors.clone();
                Arc::new(move |error| errors.lock().unwrap().push(error.to_string()))
            },
            on_status: Arc::new(|_| {}),
        };
        let (runtime_cleanup_tx, mut runtime_cleanup_rx) = mpsc::unbounded_channel();
        let reporter = IncomingRuntimeFailureReporter {
            callbacks,
            running: running.clone(),
            status: status.clone(),
            runtime_cleanup_tx,
            startup_error: Arc::new(StdMutex::new(None)),
        };
        let task = spawn_incoming_runtime_task(
            "test worker",
            async move { panic!("simulated incoming task panic") },
            reporter,
        );

        task.await
            .expect("panic must be contained by runtime guard");
        tokio::time::timeout(Duration::from_secs(1), runtime_cleanup_rx.recv())
            .await
            .expect("runtime cleanup timeout")
            .expect("runtime cleanup signal");

        assert!(!running.load(Ordering::SeqCst));
        assert_eq!(*status.read().await, RecordingStatus::Error);
        assert!(errors
            .lock()
            .unwrap()
            .iter()
            .any(|error| error.contains("test worker task panicked")));
    }

    #[tokio::test]
    async fn runtime_error_callback_panic_does_not_block_cleanup() {
        let running = Arc::new(AtomicBool::new(true));
        let status = Arc::new(RwLock::new(RecordingStatus::Recording));
        let status_called = Arc::new(AtomicBool::new(false));
        let callbacks = IncomingTranslationCallbacks {
            on_source_final: Arc::new(|_| {}),
            on_translation_delta: Arc::new(|_| {}),
            on_error: Arc::new(|_| panic!("simulated incoming error callback panic")),
            on_status: {
                let status_called = status_called.clone();
                Arc::new(move |next| {
                    if next == RecordingStatus::Error {
                        status_called.store(true, Ordering::SeqCst);
                    }
                })
            },
        };
        let (runtime_cleanup_tx, mut runtime_cleanup_rx) = mpsc::unbounded_channel();
        let reporter = IncomingRuntimeFailureReporter {
            callbacks,
            running,
            status: status.clone(),
            runtime_cleanup_tx,
            startup_error: Arc::new(StdMutex::new(None)),
        };

        assert!(reporter.report(IncomingTranslationError::Processing(
            "test failure".to_string()
        )));
        tokio::time::timeout(Duration::from_secs(1), runtime_cleanup_rx.recv())
            .await
            .expect("runtime cleanup timeout")
            .expect("runtime cleanup signal");

        assert_eq!(*status.read().await, RecordingStatus::Error);
        assert!(status_called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn audio_pump_reports_unexpected_capture_channel_close() {
        let provider: Arc<Mutex<Box<dyn SttProvider>>> =
            Arc::new(Mutex::new(Box::new(TrackingSttProvider {
                state: Arc::new(TrackingProviderState::default()),
                fail_initialize: false,
                fail_stop: false,
            })));
        let (audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(1);
        drop(audio_tx);
        let running = Arc::new(AtomicBool::new(true));
        let status = Arc::new(RwLock::new(RecordingStatus::Recording));
        let errors = Arc::new(StdMutex::new(Vec::<String>::new()));
        let statuses = Arc::new(StdMutex::new(Vec::<RecordingStatus>::new()));
        let callbacks = IncomingTranslationCallbacks {
            on_source_final: Arc::new(|_| {}),
            on_translation_delta: Arc::new(|_| {}),
            on_error: {
                let errors = errors.clone();
                Arc::new(move |err| errors.lock().unwrap().push(err.to_string()))
            },
            on_status: {
                let statuses = statuses.clone();
                Arc::new(move |next| statuses.lock().unwrap().push(next))
            },
        };
        let (cleanup_tx, mut cleanup_rx) = mpsc::unbounded_channel();

        run_audio_pump(
            audio_rx,
            provider,
            callbacks,
            running.clone(),
            Arc::new(AtomicBool::new(false)),
            status.clone(),
            cleanup_tx,
        )
        .await;

        assert!(!running.load(Ordering::SeqCst));
        assert_eq!(*status.read().await, RecordingStatus::Error);
        assert!(errors
            .lock()
            .unwrap()
            .iter()
            .any(|err| err.contains("system audio capture stopped unexpectedly")));
        assert_eq!(
            statuses.lock().unwrap().as_slice(),
            &[RecordingStatus::Error]
        );
        assert!(cleanup_rx.try_recv().is_ok());
    }

    #[tokio::test]
    async fn audio_pump_callback_panics_do_not_block_cleanup_signal() {
        let provider: Arc<Mutex<Box<dyn SttProvider>>> =
            Arc::new(Mutex::new(Box::new(TrackingSttProvider {
                state: Arc::new(TrackingProviderState::default()),
                fail_initialize: false,
                fail_stop: false,
            })));
        let (audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(1);
        drop(audio_tx);
        let running = Arc::new(AtomicBool::new(true));
        let status = Arc::new(RwLock::new(RecordingStatus::Recording));
        let callbacks = IncomingTranslationCallbacks {
            on_source_final: Arc::new(|_| {}),
            on_translation_delta: Arc::new(|_| {}),
            on_error: Arc::new(|_| panic!("simulated incoming on_error panic")),
            on_status: Arc::new(|_| panic!("simulated incoming on_status panic")),
        };
        let (cleanup_tx, mut cleanup_rx) = mpsc::unbounded_channel();

        run_audio_pump(
            audio_rx,
            provider,
            callbacks,
            running.clone(),
            Arc::new(AtomicBool::new(false)),
            status.clone(),
            cleanup_tx,
        )
        .await;

        assert!(!running.load(Ordering::SeqCst));
        assert_eq!(*status.read().await, RecordingStatus::Error);
        assert!(cleanup_rx.try_recv().is_ok());
    }

    #[tokio::test]
    async fn audio_pump_does_not_report_planned_capture_channel_close() {
        let provider: Arc<Mutex<Box<dyn SttProvider>>> =
            Arc::new(Mutex::new(Box::new(TrackingSttProvider {
                state: Arc::new(TrackingProviderState::default()),
                fail_initialize: false,
                fail_stop: false,
            })));
        let (audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(1);
        drop(audio_tx);
        let running = Arc::new(AtomicBool::new(true));
        let status = Arc::new(RwLock::new(RecordingStatus::Processing));
        let (cleanup_tx, mut cleanup_rx) = mpsc::unbounded_channel();

        run_audio_pump(
            audio_rx,
            provider,
            test_callbacks(Arc::new(StdMutex::new(Vec::new()))),
            running.clone(),
            Arc::new(AtomicBool::new(true)),
            status.clone(),
            cleanup_tx,
        )
        .await;

        assert!(running.load(Ordering::SeqCst));
        assert_eq!(*status.read().await, RecordingStatus::Processing);
        assert!(cleanup_rx.try_recv().is_err());
    }

    #[test]
    fn pending_translation_decrement_does_not_underflow() {
        let pending = AtomicUsize::new(0);

        decrement_pending_translations(&pending);
        assert_eq!(pending.load(Ordering::SeqCst), 0);

        pending.store(2, Ordering::SeqCst);
        decrement_pending_translations(&pending);
        decrement_pending_translations(&pending);
        decrement_pending_translations(&pending);

        assert_eq!(pending.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn translation_queue_overload_stops_session_and_reports_error() {
        let errors = Arc::new(StdMutex::new(Vec::<String>::new()));
        let statuses = Arc::new(StdMutex::new(Vec::<RecordingStatus>::new()));
        let callbacks = IncomingTranslationCallbacks {
            on_source_final: Arc::new(|_| {}),
            on_translation_delta: Arc::new(|_| {}),
            on_error: {
                let errors = errors.clone();
                Arc::new(move |error| errors.lock().unwrap().push(error.to_string()))
            },
            on_status: {
                let statuses = statuses.clone();
                Arc::new(move |status| statuses.lock().unwrap().push(status))
            },
        };
        let running = Arc::new(AtomicBool::new(true));
        let status = Arc::new(RwLock::new(RecordingStatus::Recording));
        let (runtime_cleanup_tx, mut runtime_cleanup_rx) = mpsc::unbounded_channel();
        let runtime_failure_reporter = IncomingRuntimeFailureReporter {
            callbacks: callbacks.clone(),
            running: running.clone(),
            status: status.clone(),
            runtime_cleanup_tx,
            startup_error: Arc::new(StdMutex::new(None)),
        };
        let seen = Arc::new(StdMutex::new(BoundedSegmentDedupe::new(2)));
        let pending_translations = Arc::new(AtomicUsize::new(0));
        let (tx, mut rx) = mpsc::channel::<TranslationJob>(1);
        tx.try_send(TranslationJob {
            text: "occupied".to_string(),
            source: "test",
            start: 0.0,
            duration: 0.0,
        })
        .unwrap();

        let transcription =
            Transcription::final_result("hello from call".to_string()).with_timing(2.0, 0.4);
        handle_finalized_transcription(
            transcription,
            &runtime_failure_reporter,
            seen.clone(),
            tx,
            pending_translations.clone(),
            "final",
        );

        assert!(!seen.lock().unwrap().contains("2.000:0.400:hello from call"));
        assert_eq!(pending_translations.load(Ordering::SeqCst), 0);
        assert_eq!(rx.try_recv().unwrap().text, "occupied");
        assert!(!running.load(Ordering::SeqCst));
        tokio::time::timeout(Duration::from_secs(1), runtime_cleanup_rx.recv())
            .await
            .expect("runtime cleanup timeout")
            .expect("runtime cleanup signal");
        assert_eq!(*status.read().await, RecordingStatus::Error);
        assert!(errors
            .lock()
            .unwrap()
            .iter()
            .any(|error| error.contains("Translation queue is overloaded")));
        assert_eq!(
            statuses.lock().unwrap().as_slice(),
            &[RecordingStatus::Error]
        );
    }

    #[tokio::test]
    async fn oversized_translation_segment_stops_before_event_or_queue_allocation() {
        let source_finals = Arc::new(StdMutex::new(Vec::<String>::new()));
        let errors = Arc::new(StdMutex::new(Vec::<String>::new()));
        let callbacks = IncomingTranslationCallbacks {
            on_source_final: {
                let source_finals = source_finals.clone();
                Arc::new(move |text| source_finals.lock().unwrap().push(text))
            },
            on_translation_delta: Arc::new(|_| {}),
            on_error: {
                let errors = errors.clone();
                Arc::new(move |error| errors.lock().unwrap().push(error.to_string()))
            },
            on_status: Arc::new(|_| {}),
        };
        let running = Arc::new(AtomicBool::new(true));
        let (tx, mut rx) = mpsc::channel::<TranslationJob>(1);
        let (runtime_cleanup_tx, mut runtime_cleanup_rx) = mpsc::unbounded_channel();
        let reporter = IncomingRuntimeFailureReporter {
            callbacks,
            running: running.clone(),
            status: Arc::new(RwLock::new(RecordingStatus::Recording)),
            runtime_cleanup_tx,
            startup_error: Arc::new(StdMutex::new(None)),
        };

        handle_finalized_transcription(
            Transcription::final_result("x".repeat(MAX_TRANSLATION_SEGMENT_BYTES + 1)),
            &reporter,
            Arc::new(StdMutex::new(BoundedSegmentDedupe::new(2))),
            tx,
            Arc::new(AtomicUsize::new(0)),
            "final",
        );

        assert!(!running.load(Ordering::SeqCst));
        assert!(source_finals.lock().unwrap().is_empty());
        assert!(rx.try_recv().is_err());
        tokio::time::timeout(Duration::from_secs(1), runtime_cleanup_rx.recv())
            .await
            .expect("runtime cleanup timeout")
            .expect("runtime cleanup signal");
        assert!(errors
            .lock()
            .unwrap()
            .iter()
            .any(|error| error.contains("too large to translate safely")));
    }

    #[test]
    fn bounded_segment_dedupe_evicts_oldest_key() {
        let mut dedupe = BoundedSegmentDedupe::new(2);

        assert!(dedupe.remember("one".to_string()));
        assert!(dedupe.remember("two".to_string()));
        assert!(!dedupe.remember("one".to_string()));
        assert!(dedupe.remember("three".to_string()));

        assert!(!dedupe.contains("one"));
        assert!(dedupe.contains("two"));
        assert!(dedupe.contains("three"));
        assert!(dedupe.remember("one".to_string()));
    }

    #[test]
    fn incoming_start_status_guard_does_not_overwrite_error() {
        assert!(should_mark_incoming_recording_started(
            RecordingStatus::Starting,
            true,
        ));
        assert!(!should_mark_incoming_recording_started(
            RecordingStatus::Error,
            true,
        ));
        assert!(!should_mark_incoming_recording_started(
            RecordingStatus::Starting,
            false,
        ));
    }

    #[test]
    fn incoming_translation_target_language_is_trimmed_and_defaulted() {
        assert_eq!(
            normalize_incoming_translation_target_language("  de\n"),
            "de"
        );
        assert_eq!(normalize_incoming_translation_target_language(""), "ru");
        assert_eq!(normalize_incoming_translation_target_language("auto"), "ru");
        assert_eq!(
            normalize_incoming_translation_target_language("MULTI"),
            "ru"
        );
    }

    #[tokio::test]
    async fn start_aborts_stt_provider_after_initialize_failure() {
        let provider_state = std::sync::Arc::new(TrackingProviderState::default());
        let capture_state = std::sync::Arc::new(SyntheticIncomingCaptureState::default());
        let service = IncomingCaptionTranslationService::new_with_factories(
            std::sync::Arc::new(TrackingSttFactory {
                state: provider_state.clone(),
                fail_initialize: true,
                fail_stop: false,
            }),
            std::sync::Arc::new(SyntheticIncomingAudioFactory {
                capture_state: capture_state.clone(),
                requested_target: std::sync::Arc::new(StdMutex::new(None)),
            }),
        );
        let statuses = std::sync::Arc::new(StdMutex::new(Vec::new()));
        let mut config = IncomingTranslationConfig::new_with_defaults(SttConfig::default(), 76);
        config.openai_api_key = "sk-test".to_string();

        let err = service
            .start(config, test_callbacks(statuses.clone()))
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            IncomingTranslationError::Connection(msg)
                if msg.contains("simulated initialize failure")
        ));
        assert!(provider_state.initialized.load(Ordering::SeqCst));
        assert!(provider_state.aborted.load(Ordering::SeqCst));
        assert!(!provider_state.started.load(Ordering::SeqCst));
        assert!(!capture_state.started.load(Ordering::SeqCst));
        assert_eq!(service.get_status().await, RecordingStatus::Idle);
        assert!(service.inner.lock().await.is_none());
        assert_eq!(
            statuses.lock().unwrap().as_slice(),
            &[RecordingStatus::Starting]
        );
    }

    #[tokio::test]
    async fn start_cleans_stt_stream_when_loopback_capture_start_fails() {
        let provider_state = std::sync::Arc::new(TrackingProviderState::default());
        let service = IncomingCaptionTranslationService::new_with_factories(
            std::sync::Arc::new(TrackingSttFactory {
                state: provider_state.clone(),
                fail_initialize: false,
                fail_stop: false,
            }),
            std::sync::Arc::new(FailingLoopbackAudioFactory),
        );
        let statuses = std::sync::Arc::new(StdMutex::new(Vec::new()));
        let mut config = IncomingTranslationConfig::new_with_defaults(SttConfig::default(), 77);
        config.openai_api_key = "sk-test".to_string();

        let err = service
            .start(config, test_callbacks(statuses.clone()))
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            IncomingTranslationError::Configuration(msg)
                if msg.contains("simulated loopback start failure")
        ));
        assert!(provider_state.initialized.load(Ordering::SeqCst));
        assert!(provider_state.started.load(Ordering::SeqCst));
        assert!(provider_state.stopped.load(Ordering::SeqCst));
        assert!(!provider_state.aborted.load(Ordering::SeqCst));
        assert_eq!(service.get_status().await, RecordingStatus::Idle);
        assert!(service.inner.lock().await.is_none());
        assert_eq!(
            statuses.lock().unwrap().as_slice(),
            &[RecordingStatus::Starting]
        );
    }

    #[tokio::test]
    async fn start_aborts_stt_stream_if_cleanup_stop_fails() {
        let provider_state = std::sync::Arc::new(TrackingProviderState::default());
        let service = IncomingCaptionTranslationService::new_with_factories(
            std::sync::Arc::new(TrackingSttFactory {
                state: provider_state.clone(),
                fail_initialize: false,
                fail_stop: true,
            }),
            std::sync::Arc::new(FailingLoopbackAudioFactory),
        );
        let statuses = std::sync::Arc::new(StdMutex::new(Vec::new()));
        let mut config = IncomingTranslationConfig::new_with_defaults(SttConfig::default(), 78);
        config.openai_api_key = "sk-test".to_string();

        let err = service
            .start(config, test_callbacks(statuses))
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            IncomingTranslationError::Configuration(msg)
                if msg.contains("simulated loopback start failure")
        ));
        assert!(provider_state.started.load(Ordering::SeqCst));
        assert!(provider_state.stopped.load(Ordering::SeqCst));
        assert!(provider_state.aborted.load(Ordering::SeqCst));
        assert_eq!(service.get_status().await, RecordingStatus::Idle);
        assert!(service.inner.lock().await.is_none());
    }

    #[tokio::test]
    async fn start_aborts_initialized_stt_when_translator_create_fails() {
        let provider_state = std::sync::Arc::new(TrackingProviderState::default());
        let capture_state = std::sync::Arc::new(SyntheticIncomingCaptureState::default());
        let requested_target = std::sync::Arc::new(StdMutex::new(None));
        let service = IncomingCaptionTranslationService::new_with_all_factories(
            std::sync::Arc::new(TrackingSttFactory {
                state: provider_state.clone(),
                fail_initialize: false,
                fail_stop: false,
            }),
            std::sync::Arc::new(SyntheticIncomingAudioFactory {
                capture_state: capture_state.clone(),
                requested_target,
            }),
            std::sync::Arc::new(FailingTextTranslatorFactory),
        );
        let statuses = std::sync::Arc::new(StdMutex::new(Vec::new()));
        let mut config = IncomingTranslationConfig::new_with_defaults(SttConfig::default(), 79);
        config.openai_api_key = "sk-test".to_string();

        let err = service
            .start(config, test_callbacks(statuses.clone()))
            .await
            .unwrap_err();

        assert!(
            matches!(err, IncomingTranslationError::Authentication(msg) if msg.contains("simulated translator create failure"))
        );
        assert!(provider_state.initialized.load(Ordering::SeqCst));
        assert!(!provider_state.started.load(Ordering::SeqCst));
        assert!(provider_state.aborted.load(Ordering::SeqCst));
        assert!(!capture_state.started.load(Ordering::SeqCst));
        assert_eq!(service.get_status().await, RecordingStatus::Idle);
        assert!(service.inner.lock().await.is_none());
        assert_eq!(
            statuses.lock().unwrap().as_slice(),
            &[RecordingStatus::Starting]
        );
    }

    #[tokio::test]
    async fn duplicate_start_does_not_replace_the_active_session() {
        let capture_state = std::sync::Arc::new(SyntheticIncomingCaptureState::default());
        let provider_state = std::sync::Arc::new(SyntheticIncomingProviderState::default());
        let translator_state = std::sync::Arc::new(SyntheticTextTranslatorState::default());
        let service = IncomingCaptionTranslationService::new_with_all_factories(
            std::sync::Arc::new(SyntheticIncomingSttFactory {
                state: provider_state,
            }),
            std::sync::Arc::new(SyntheticIncomingAudioFactory {
                capture_state,
                requested_target: std::sync::Arc::new(StdMutex::new(None)),
            }),
            std::sync::Arc::new(SyntheticTextTranslatorFactory {
                state: translator_state,
            }),
        );

        let mut first_config =
            IncomingTranslationConfig::new_with_defaults(SttConfig::default(), 106);
        first_config.openai_api_key = "sk-test".to_string();
        service
            .start(
                first_config,
                test_callbacks(std::sync::Arc::new(StdMutex::new(Vec::new()))),
            )
            .await
            .unwrap();

        let second_statuses = std::sync::Arc::new(StdMutex::new(Vec::new()));
        let mut second_config =
            IncomingTranslationConfig::new_with_defaults(SttConfig::default(), 107);
        second_config.openai_api_key = "sk-test".to_string();
        let err = service
            .start(second_config, test_callbacks(second_statuses.clone()))
            .await
            .unwrap_err();

        assert!(matches!(err, IncomingTranslationError::AlreadyActive));
        assert_eq!(service.active_session_id().await, Some(106));
        assert!(second_statuses.lock().unwrap().is_empty());

        service.stop().await.unwrap();
    }

    #[tokio::test]
    async fn synthetic_incoming_translation_e2e_captures_transcribes_and_translates() {
        let capture_state = std::sync::Arc::new(SyntheticIncomingCaptureState::default());
        let requested_target = std::sync::Arc::new(StdMutex::new(None));
        let provider_state = std::sync::Arc::new(SyntheticIncomingProviderState::default());
        let translator_state = std::sync::Arc::new(SyntheticTextTranslatorState::default());
        let service = IncomingCaptionTranslationService::new_with_all_factories(
            std::sync::Arc::new(SyntheticIncomingSttFactory {
                state: provider_state.clone(),
            }),
            std::sync::Arc::new(SyntheticIncomingAudioFactory {
                capture_state: capture_state.clone(),
                requested_target: requested_target.clone(),
            }),
            std::sync::Arc::new(SyntheticTextTranslatorFactory {
                state: translator_state.clone(),
            }),
        );

        let statuses = std::sync::Arc::new(StdMutex::new(Vec::new()));
        let source_text = std::sync::Arc::new(StdMutex::new(String::new()));
        let translated_text = std::sync::Arc::new(StdMutex::new(String::new()));
        let errors = std::sync::Arc::new(StdMutex::new(Vec::new()));

        let callbacks = IncomingTranslationCallbacks {
            on_source_final: {
                let source_text = source_text.clone();
                Arc::new(move |text| source_text.lock().unwrap().push_str(&text))
            },
            on_translation_delta: {
                let translated_text = translated_text.clone();
                Arc::new(move |text| translated_text.lock().unwrap().push_str(&text))
            },
            on_error: {
                let errors = errors.clone();
                Arc::new(move |err| errors.lock().unwrap().push(err.to_string()))
            },
            on_status: {
                let statuses = statuses.clone();
                Arc::new(move |status| statuses.lock().unwrap().push(status))
            },
        };

        let mut config = IncomingTranslationConfig::new_with_defaults(SttConfig::default(), 102);
        config.openai_api_key = "sk-test".to_string();
        config.target_language = "  ru\n".to_string();

        service.start(config, callbacks).await.unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            if !translated_text.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        assert_eq!(service.get_status().await, RecordingStatus::Recording);
        assert!(capture_state.started.load(Ordering::SeqCst));
        let requested_target = requested_target.lock().unwrap().unwrap();
        assert_eq!(
            requested_target.sample_rate,
            AudioCaptureTarget::incoming_subtitles().sample_rate
        );
        assert_eq!(
            requested_target.channels,
            AudioCaptureTarget::incoming_subtitles().channels
        );
        let capture_config = capture_state.initialized_config.lock().unwrap().unwrap();
        assert_eq!(capture_config.sample_rate, 16_000);
        assert_eq!(capture_config.channels, 1);
        assert!(provider_state.initialized.load(Ordering::SeqCst));
        assert!(provider_state.started.load(Ordering::SeqCst));
        assert_eq!(provider_state.sent_chunks.load(Ordering::SeqCst), 1);
        assert_eq!(source_text.lock().unwrap().as_str(), "hello from zoom");
        assert_eq!(translated_text.lock().unwrap().as_str(), "привет из zoom");
        assert!(errors.lock().unwrap().is_empty());
        assert_eq!(
            translator_state.requests.lock().unwrap().as_slice(),
            &[("hello from zoom".to_string(), "ru".to_string())]
        );
        assert_eq!(
            statuses.lock().unwrap().as_slice(),
            &[RecordingStatus::Starting, RecordingStatus::Recording]
        );

        service.stop().await.unwrap();

        assert_eq!(service.get_status().await, RecordingStatus::Idle);
        assert!(capture_state.stopped.load(Ordering::SeqCst));
        assert!(provider_state.stopped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn status_callback_panic_does_not_break_incoming_session_lifecycle() {
        let capture_state = std::sync::Arc::new(SyntheticIncomingCaptureState::default());
        let service = IncomingCaptionTranslationService::new_with_all_factories(
            std::sync::Arc::new(SyntheticIncomingSttFactory {
                state: std::sync::Arc::new(SyntheticIncomingProviderState::default()),
            }),
            std::sync::Arc::new(SyntheticIncomingAudioFactory {
                capture_state: capture_state.clone(),
                requested_target: std::sync::Arc::new(StdMutex::new(None)),
            }),
            std::sync::Arc::new(SyntheticTextTranslatorFactory {
                state: std::sync::Arc::new(SyntheticTextTranslatorState::default()),
            }),
        );
        let callbacks = IncomingTranslationCallbacks {
            on_source_final: Arc::new(|_| {}),
            on_translation_delta: Arc::new(|_| {}),
            on_error: Arc::new(|_| {}),
            on_status: Arc::new(|_| panic!("simulated incoming status callback panic")),
        };
        let mut config = IncomingTranslationConfig::new_with_defaults(SttConfig::default(), 108);
        config.openai_api_key = "sk-test".to_string();

        service
            .start(config, callbacks)
            .await
            .expect("status callback panic must not abort startup");
        assert_eq!(service.get_status().await, RecordingStatus::Recording);
        assert_eq!(service.active_session_id().await, Some(108));

        service.stop().await.unwrap();
        assert_eq!(service.get_status().await, RecordingStatus::Idle);
        assert!(capture_state.stopped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn terminal_translation_error_cleans_resources_without_manual_stop() {
        let capture_state = std::sync::Arc::new(SyntheticIncomingCaptureState::default());
        let requested_target = std::sync::Arc::new(StdMutex::new(None));
        let provider_state = std::sync::Arc::new(SyntheticIncomingProviderState::default());
        let service = IncomingCaptionTranslationService::new_with_all_factories(
            std::sync::Arc::new(SyntheticIncomingSttFactory {
                state: provider_state.clone(),
            }),
            std::sync::Arc::new(SyntheticIncomingAudioFactory {
                capture_state: capture_state.clone(),
                requested_target,
            }),
            std::sync::Arc::new(RateLimitedTextTranslatorFactory),
        );

        let errors = std::sync::Arc::new(StdMutex::new(Vec::new()));
        let statuses = std::sync::Arc::new(StdMutex::new(Vec::new()));
        let callbacks = IncomingTranslationCallbacks {
            on_source_final: Arc::new(|_| {}),
            on_translation_delta: Arc::new(|_| {}),
            on_error: {
                let errors = errors.clone();
                Arc::new(move |err| errors.lock().unwrap().push(err.to_string()))
            },
            on_status: {
                let statuses = statuses.clone();
                Arc::new(move |status| statuses.lock().unwrap().push(status))
            },
        };

        let mut config = IncomingTranslationConfig::new_with_defaults(SttConfig::default(), 104);
        config.openai_api_key = "sk-test".to_string();
        config.target_language = "ru".to_string();

        service.start(config, callbacks).await.unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            if service.get_status().await == RecordingStatus::Error {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        assert_eq!(service.get_status().await, RecordingStatus::Error);
        assert!(
            errors
                .lock()
                .unwrap()
                .iter()
                .any(|err| err.contains("simulated rate limit")),
            "expected rate limit error, got {:?}",
            errors.lock().unwrap()
        );
        assert!(
            statuses.lock().unwrap().contains(&RecordingStatus::Error),
            "expected Error status, got {:?}",
            statuses.lock().unwrap()
        );

        wait_until_runtime_cleanup(&service, &capture_state, &provider_state).await;
        assert_eq!(service.get_status().await, RecordingStatus::Error);

        service.stop().await.unwrap();

        assert_eq!(service.get_status().await, RecordingStatus::Idle);
        assert!(capture_state.stopped.load(Ordering::SeqCst));
        assert!(provider_state.stopped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn stt_send_audio_failure_cleans_resources_without_manual_stop() {
        let capture_state = std::sync::Arc::new(SyntheticIncomingCaptureState::default());
        let requested_target = std::sync::Arc::new(StdMutex::new(None));
        let provider_state = std::sync::Arc::new(SyntheticIncomingProviderState::default());
        provider_state.fail_send.store(true, Ordering::SeqCst);
        let translator_state = std::sync::Arc::new(SyntheticTextTranslatorState::default());
        let service = IncomingCaptionTranslationService::new_with_all_factories(
            std::sync::Arc::new(SyntheticIncomingSttFactory {
                state: provider_state.clone(),
            }),
            std::sync::Arc::new(SyntheticIncomingAudioFactory {
                capture_state: capture_state.clone(),
                requested_target,
            }),
            std::sync::Arc::new(SyntheticTextTranslatorFactory {
                state: translator_state,
            }),
        );

        let errors = std::sync::Arc::new(StdMutex::new(Vec::new()));
        let statuses = std::sync::Arc::new(StdMutex::new(Vec::new()));
        let callbacks = IncomingTranslationCallbacks {
            on_source_final: Arc::new(|_| {}),
            on_translation_delta: Arc::new(|_| {}),
            on_error: {
                let errors = errors.clone();
                Arc::new(move |err| errors.lock().unwrap().push(err.to_string()))
            },
            on_status: {
                let statuses = statuses.clone();
                Arc::new(move |status| statuses.lock().unwrap().push(status))
            },
        };

        let mut config = IncomingTranslationConfig::new_with_defaults(SttConfig::default(), 105);
        config.openai_api_key = "sk-test".to_string();
        config.target_language = "ru".to_string();

        service.start(config, callbacks).await.unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            if service.get_status().await == RecordingStatus::Error {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        assert_eq!(service.get_status().await, RecordingStatus::Error);
        assert!(
            errors
                .lock()
                .unwrap()
                .iter()
                .any(|err| err.contains("simulated send_audio failure")),
            "expected send_audio error, got {:?}",
            errors.lock().unwrap()
        );
        assert!(
            statuses.lock().unwrap().contains(&RecordingStatus::Error),
            "expected Error status, got {:?}",
            statuses.lock().unwrap()
        );

        wait_until_runtime_cleanup(&service, &capture_state, &provider_state).await;
        assert_eq!(service.get_status().await, RecordingStatus::Error);

        service.stop().await.unwrap();

        assert_eq!(service.get_status().await, RecordingStatus::Idle);
        assert!(capture_state.stopped.load(Ordering::SeqCst));
        assert!(provider_state.stopped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn stt_error_callback_cleans_resources_without_manual_stop() {
        let capture_state = std::sync::Arc::new(SyntheticIncomingCaptureState::default());
        let requested_target = std::sync::Arc::new(StdMutex::new(None));
        let provider_state = std::sync::Arc::new(SyntheticIncomingProviderState::default());
        let translator_state = std::sync::Arc::new(SyntheticTextTranslatorState::default());
        let service = IncomingCaptionTranslationService::new_with_all_factories(
            std::sync::Arc::new(SyntheticIncomingSttFactory {
                state: provider_state.clone(),
            }),
            std::sync::Arc::new(SyntheticIncomingAudioFactory {
                capture_state: capture_state.clone(),
                requested_target,
            }),
            std::sync::Arc::new(SyntheticTextTranslatorFactory {
                state: translator_state,
            }),
        );

        let errors = std::sync::Arc::new(StdMutex::new(Vec::new()));
        let statuses = std::sync::Arc::new(StdMutex::new(Vec::new()));
        let callbacks = IncomingTranslationCallbacks {
            on_source_final: Arc::new(|_| {}),
            on_translation_delta: Arc::new(|_| {}),
            on_error: {
                let errors = errors.clone();
                Arc::new(move |err| errors.lock().unwrap().push(err.to_string()))
            },
            on_status: {
                let statuses = statuses.clone();
                Arc::new(move |status| statuses.lock().unwrap().push(status))
            },
        };

        let mut config = IncomingTranslationConfig::new_with_defaults(SttConfig::default(), 106);
        config.openai_api_key = "sk-test".to_string();
        config.target_language = "ru".to_string();

        service.start(config, callbacks).await.unwrap();

        let callback = provider_state
            .error_callback
            .lock()
            .unwrap()
            .clone()
            .expect("stt error callback");
        callback(SttError::Connection(
            crate::domain::SttConnectionError::simple("simulated async receiver failure"),
        ));

        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            if service.get_status().await == RecordingStatus::Error {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        assert_eq!(service.get_status().await, RecordingStatus::Error);
        assert!(
            errors
                .lock()
                .unwrap()
                .iter()
                .any(|err| err.contains("simulated async receiver failure")),
            "expected async receiver error, got {:?}",
            errors.lock().unwrap()
        );
        assert!(
            statuses.lock().unwrap().contains(&RecordingStatus::Error),
            "expected Error status, got {:?}",
            statuses.lock().unwrap()
        );

        wait_until_runtime_cleanup(&service, &capture_state, &provider_state).await;
        assert_eq!(service.get_status().await, RecordingStatus::Error);

        service.stop().await.unwrap();

        assert_eq!(service.get_status().await, RecordingStatus::Idle);
        assert!(capture_state.stopped.load(Ordering::SeqCst));
        assert!(provider_state.stopped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn stt_error_during_start_is_returned_instead_of_false_success() {
        let capture_state = std::sync::Arc::new(SyntheticIncomingCaptureState::default());
        let requested_target = std::sync::Arc::new(StdMutex::new(None));
        let provider_state = std::sync::Arc::new(SyntheticIncomingProviderState::default());
        provider_state
            .fail_during_start
            .store(true, Ordering::SeqCst);
        let translator_state = std::sync::Arc::new(SyntheticTextTranslatorState::default());
        let service = IncomingCaptionTranslationService::new_with_all_factories(
            std::sync::Arc::new(SyntheticIncomingSttFactory {
                state: provider_state.clone(),
            }),
            std::sync::Arc::new(SyntheticIncomingAudioFactory {
                capture_state: capture_state.clone(),
                requested_target,
            }),
            std::sync::Arc::new(SyntheticTextTranslatorFactory {
                state: translator_state,
            }),
        );

        let errors = std::sync::Arc::new(StdMutex::new(Vec::new()));
        let statuses = std::sync::Arc::new(StdMutex::new(Vec::new()));
        let callbacks = IncomingTranslationCallbacks {
            on_source_final: Arc::new(|_| {}),
            on_translation_delta: Arc::new(|_| {}),
            on_error: {
                let errors = errors.clone();
                Arc::new(move |err| errors.lock().unwrap().push(err.to_string()))
            },
            on_status: {
                let statuses = statuses.clone();
                Arc::new(move |status| statuses.lock().unwrap().push(status))
            },
        };

        let mut config = IncomingTranslationConfig::new_with_defaults(SttConfig::default(), 107);
        config.openai_api_key = "sk-test".to_string();
        config.target_language = "ru".to_string();

        let err = service.start(config, callbacks).await.unwrap_err();

        assert!(err.was_reported());
        assert_eq!(err.error_type(), "connection");
        assert!(err
            .to_string()
            .contains("simulated receiver failure during startup"));
        assert_eq!(service.get_status().await, RecordingStatus::Error);
        assert!(!statuses
            .lock()
            .unwrap()
            .contains(&RecordingStatus::Recording));

        wait_until_runtime_cleanup(&service, &capture_state, &provider_state).await;
        assert_eq!(errors.lock().unwrap().len(), 1);
        assert_eq!(
            statuses
                .lock()
                .unwrap()
                .iter()
                .filter(|status| **status == RecordingStatus::Error)
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn stop_drains_final_transcription_emitted_by_stt_stop_stream() {
        let capture_state = std::sync::Arc::new(SyntheticIncomingCaptureState::default());
        let requested_target = std::sync::Arc::new(StdMutex::new(None));
        let provider_state = std::sync::Arc::new(StopFinalProviderState::default());
        let translator_state = std::sync::Arc::new(SyntheticTextTranslatorState::default());
        let service = IncomingCaptionTranslationService::new_with_all_factories(
            std::sync::Arc::new(StopFinalSttFactory {
                state: provider_state.clone(),
            }),
            std::sync::Arc::new(SyntheticIncomingAudioFactory {
                capture_state,
                requested_target,
            }),
            std::sync::Arc::new(SyntheticTextTranslatorFactory {
                state: translator_state.clone(),
            }),
        );

        let source_text = std::sync::Arc::new(StdMutex::new(String::new()));
        let translated_text = std::sync::Arc::new(StdMutex::new(String::new()));
        let callbacks = IncomingTranslationCallbacks {
            on_source_final: {
                let source_text = source_text.clone();
                Arc::new(move |text| source_text.lock().unwrap().push_str(&text))
            },
            on_translation_delta: {
                let translated_text = translated_text.clone();
                Arc::new(move |text| translated_text.lock().unwrap().push_str(&text))
            },
            on_error: Arc::new(|err| panic!("unexpected incoming translation error: {err}")),
            on_status: Arc::new(|_| {}),
        };

        let mut config = IncomingTranslationConfig::new_with_defaults(SttConfig::default(), 103);
        config.openai_api_key = "sk-test".to_string();
        config.target_language = "ru".to_string();

        service.start(config, callbacks).await.unwrap();
        service.stop().await.unwrap();

        assert!(provider_state.stopped.load(Ordering::SeqCst));
        assert_eq!(source_text.lock().unwrap().as_str(), "late final from stop");
        assert_eq!(translated_text.lock().unwrap().as_str(), "привет из zoom");
        assert_eq!(
            translator_state.requests.lock().unwrap().as_slice(),
            &[("late final from stop".to_string(), "ru".to_string())]
        );
        assert_eq!(service.get_status().await, RecordingStatus::Idle);
    }

    #[tokio::test]
    async fn translation_worker_retries_transient_error_without_losing_segment() {
        let (tx, rx) = mpsc::channel::<TranslationJob>(4);
        let running = Arc::new(AtomicBool::new(true));
        let pending = Arc::new(AtomicUsize::new(0));
        let status = Arc::new(RwLock::new(RecordingStatus::Recording));
        let (cleanup_tx, mut cleanup_rx) = mpsc::unbounded_channel();
        let translated = Arc::new(StdMutex::new(Vec::<String>::new()));
        let errors = Arc::new(StdMutex::new(Vec::<String>::new()));
        let callbacks = IncomingTranslationCallbacks {
            on_source_final: Arc::new(|_| {}),
            on_translation_delta: {
                let translated = translated.clone();
                Arc::new(move |text| translated.lock().unwrap().push(text))
            },
            on_error: {
                let errors = errors.clone();
                Arc::new(move |err| errors.lock().unwrap().push(err.to_string()))
            },
            on_status: Arc::new(|_| {}),
        };
        let runtime_failure_reporter = IncomingRuntimeFailureReporter {
            callbacks,
            running,
            status: status.clone(),
            runtime_cleanup_tx: cleanup_tx,
            startup_error: Arc::new(StdMutex::new(None)),
        };

        let worker = tokio::spawn(run_translation_worker(
            rx,
            Arc::new(FlakyTextTranslator {
                calls: std::sync::atomic::AtomicUsize::new(0),
            }),
            runtime_failure_reporter,
            "ru".to_string(),
            pending.clone(),
        ));

        pending.fetch_add(2, Ordering::SeqCst);
        tx.send(TranslationJob {
            text: "first".to_string(),
            source: "test",
            start: 0.0,
            duration: 1.0,
        })
        .await
        .unwrap();
        tx.send(TranslationJob {
            text: "second".to_string(),
            source: "test",
            start: 1.0,
            duration: 1.0,
        })
        .await
        .unwrap();
        drop(tx);
        worker.await.unwrap();

        assert_eq!(
            translated.lock().unwrap().as_slice(),
            &[
                String::from("first translated"),
                String::from("second translated")
            ]
        );
        assert!(errors.lock().unwrap().is_empty());
        assert_eq!(pending.load(Ordering::SeqCst), 0);
        assert_eq!(*status.read().await, RecordingStatus::Recording);
        assert!(cleanup_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn translation_worker_surfaces_rate_limit_error() {
        let (tx, rx) = mpsc::channel::<TranslationJob>(2);
        let running = Arc::new(AtomicBool::new(true));
        let pending = Arc::new(AtomicUsize::new(0));
        let status = Arc::new(RwLock::new(RecordingStatus::Recording));
        let (cleanup_tx, mut cleanup_rx) = mpsc::unbounded_channel();
        let errors = Arc::new(StdMutex::new(Vec::<String>::new()));
        let statuses = Arc::new(StdMutex::new(Vec::<RecordingStatus>::new()));
        let callbacks = IncomingTranslationCallbacks {
            on_source_final: Arc::new(|_| {}),
            on_translation_delta: Arc::new(|_| {}),
            on_error: {
                let errors = errors.clone();
                Arc::new(move |err| errors.lock().unwrap().push(err.to_string()))
            },
            on_status: {
                let statuses = statuses.clone();
                Arc::new(move |status| statuses.lock().unwrap().push(status))
            },
        };
        let runtime_failure_reporter = IncomingRuntimeFailureReporter {
            callbacks,
            running: running.clone(),
            status: status.clone(),
            runtime_cleanup_tx: cleanup_tx,
            startup_error: Arc::new(StdMutex::new(None)),
        };

        let worker = tokio::spawn(run_translation_worker(
            rx,
            Arc::new(RateLimitedTextTranslator),
            runtime_failure_reporter,
            "ru".to_string(),
            pending.clone(),
        ));

        pending.fetch_add(2, Ordering::SeqCst);
        tx.send(TranslationJob {
            text: "hello".to_string(),
            source: "test",
            start: 0.0,
            duration: 1.0,
        })
        .await
        .unwrap();
        tx.send(TranslationJob {
            text: "queued after terminal error".to_string(),
            source: "test",
            start: 1.0,
            duration: 1.0,
        })
        .await
        .unwrap();
        drop(tx);
        worker.await.unwrap();

        assert!(
            errors
                .lock()
                .unwrap()
                .iter()
                .any(|err| err.contains("Rate limited")),
            "expected rate limit error, got {:?}",
            errors.lock().unwrap()
        );
        assert_eq!(pending.load(Ordering::SeqCst), 0);
        assert!(!running.load(Ordering::SeqCst));
        assert_eq!(*status.read().await, RecordingStatus::Error);
        assert_eq!(
            statuses.lock().unwrap().as_slice(),
            &[RecordingStatus::Error]
        );
        assert!(cleanup_rx.try_recv().is_ok());
    }
}
