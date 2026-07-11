//! LiveTranslationService - outgoing facade для live-перевода голоса.
//!
//! Facade отвечает за outgoing-specific preflight, factories, spectrum callback и UI status.
//! Bounded queues, PCM framing, translation/output owner tasks, supervision и cleanup находятся
//! в `RealtimeInterpretationSession`, общем для outgoing и будущего incoming spoken mode.
//!
//! Этот сервис намеренно НЕ повторяет логику TranscriptionService:
//! - нет auto-paste/copy/history;
//! - нет STT auth retry/logout;
//! - нет VAD (translation идёт сплошным потоком, включая тишину).

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use tokio::sync::{Mutex, RwLock};

use crate::domain::{
    microphone_sensitivity_gain, AudioCaptureTarget, AudioConfig, PlatformAudioFactory,
    RealtimeTranslationConfig, RealtimeTranslationError, RealtimeTranslationErrorKind,
    RealtimeTranslationFactory, RecordingStatus, TranslationAudioOutputConfig,
};
use crate::infrastructure::audio::DefaultPlatformAudioFactory;
use crate::infrastructure::openai::OpenAIRealtimeTranslationFactory;

use super::{
    spawn_realtime_interpretation_supervisor, AudioSpectrumAnalyzer,
    RealtimeInterpretationCallbacks, RealtimeInterpretationConfig, RealtimeInterpretationError,
    RealtimeInterpretationPorts, RealtimeInterpretationSession, RealtimeInterpretationShutdown,
    RealtimeInterpretationStop,
};

const TRANSLATION_TARGET_LANGUAGE_DEFAULT: &str = "en";

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

impl From<RealtimeTranslationError> for LiveTranslationError {
    fn from(err: RealtimeTranslationError) -> Self {
        let msg = err.to_string();
        match err.kind() {
            RealtimeTranslationErrorKind::Authentication => {
                LiveTranslationError::Authentication(msg)
            }
            RealtimeTranslationErrorKind::RateLimited => LiveTranslationError::RateLimited(msg),
            RealtimeTranslationErrorKind::Connection => LiveTranslationError::Connection(msg),
            RealtimeTranslationErrorKind::Timeout => LiveTranslationError::Timeout(msg),
            RealtimeTranslationErrorKind::Protocol => LiveTranslationError::Processing(msg),
            RealtimeTranslationErrorKind::Internal => LiveTranslationError::Processing(msg),
        }
    }
}

impl From<RealtimeInterpretationError> for LiveTranslationError {
    fn from(error: RealtimeInterpretationError) -> Self {
        match error {
            RealtimeInterpretationError::Authentication(message) => Self::Authentication(message),
            RealtimeInterpretationError::RateLimited(message) => Self::RateLimited(message),
            RealtimeInterpretationError::Connection(message) => Self::Connection(message),
            RealtimeInterpretationError::Timeout(message) => Self::Timeout(message),
            RealtimeInterpretationError::Processing(message) => Self::Processing(message),
        }
    }
}

pub struct LiveTranslationService {
    status: Arc<RwLock<RecordingStatus>>,
    inner: Arc<Mutex<Option<RealtimeInterpretationSession>>>,
    audio_factory: Arc<dyn PlatformAudioFactory>,
    client_factory: Arc<dyn RealtimeTranslationFactory>,
}

fn notify_live_runtime_error(callbacks: &LiveTranslationCallbacks, error: LiveTranslationError) {
    call_live_callback("on_error", || (callbacks.on_error)(error));
    call_live_callback("Error status", || {
        (callbacks.on_status)(RecordingStatus::Error)
    });
}

fn call_live_callback(label: &str, callback: impl FnOnce()) {
    if std::panic::catch_unwind(AssertUnwindSafe(callback)).is_err() {
        log::error!("LiveTranslationService: {} callback panicked", label);
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
        Self::new_with_factories(audio_factory, Arc::new(OpenAIRealtimeTranslationFactory))
    }

    fn new_with_factories(
        audio_factory: Arc<dyn PlatformAudioFactory>,
        client_factory: Arc<dyn RealtimeTranslationFactory>,
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
        self.inner
            .lock()
            .await
            .as_ref()
            .map(RealtimeInterpretationSession::session_id)
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
        if guard.is_some() && *self.status.read().await == RecordingStatus::Error {
            if let Some(stale_session) = guard.take() {
                stale_session
                    .shutdown(RealtimeInterpretationShutdown::Abort)
                    .await;
            }
        }
        if guard.is_some() {
            return Err(LiveTranslationError::Configuration(
                "Translation session уже активна".into(),
            ));
        }

        *self.status.write().await = RecordingStatus::Starting;
        call_live_callback("Starting status", || {
            (callbacks.on_status)(RecordingStatus::Starting)
        });

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
        // 3. Mic preflight. Do this before paid OpenAI session creation.
        if let Err(e) = self.audio_factory.microphone_preflight() {
            let _ = output_concrete.close().await;
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
                let _ = output_concrete.close().await;
                let err = LiveTranslationError::Configuration(format!("mic init: {}", e));
                self.transition_to_error().await;
                return Err(err);
            }
        };
        let mut capture = capture;
        if let Err(e) = capture
            .initialize(AudioConfig {
                sample_rate: AudioCaptureTarget::outgoing_translation().sample_rate,
                channels: AudioCaptureTarget::outgoing_translation().channels,
                buffer_size: AudioConfig::default().buffer_size,
            })
            .await
        {
            let _ = output_concrete.close().await;
            let err = LiveTranslationError::Configuration(format!("mic init: {}", e));
            self.transition_to_error().await;
            return Err(err);
        }

        // 4. OpenAI client connect
        let mut client = self.client_factory.create();
        let translation_config =
            RealtimeTranslationConfig::new(config.openai_api_key.clone(), target_language.clone());
        let openai_rx = match client.connect(translation_config).await {
            Ok(rx) => rx,
            Err(e) => {
                // откатываем output
                let _ = output_concrete.close().await;
                let mapped: LiveTranslationError = e.into();
                self.transition_to_error().await;
                return Err(mapped);
            }
        };
        let spectrum = Arc::new(StdMutex::new(AudioSpectrumAnalyzer::new()));
        let on_spectrum = callbacks.on_audio_spectrum.clone();
        let core_callbacks = RealtimeInterpretationCallbacks {
            on_translated_text: callbacks.on_transcript_delta.clone(),
            on_source_text: Arc::new(|text| {
                log::debug!("translation source delta received ({} bytes)", text.len());
            }),
            on_input_audio: Arc::new(move |samples| {
                let mut analyzer = spectrum.lock().unwrap_or_else(|error| error.into_inner());
                if let Some(bars) = analyzer.push_samples(samples) {
                    call_live_callback("audio spectrum", || on_spectrum(bars));
                }
            }),
        };
        let core_config = RealtimeInterpretationConfig::outgoing(
            config.session_id,
            microphone_sensitivity_gain(config.microphone_sensitivity),
        );
        let (session, runtime_stop_rx) = match RealtimeInterpretationSession::start(
            core_config,
            RealtimeInterpretationPorts {
                capture,
                output: output_concrete,
                translation: client,
                translation_events: openai_rx,
            },
            core_callbacks,
        )
        .await
        {
            Ok(runtime) => runtime,
            Err(error) => {
                let error = LiveTranslationError::Configuration(error.to_string());
                self.transition_to_error().await;
                return Err(error);
            }
        };

        *guard = Some(session);
        spawn_live_runtime_cleanup_monitor(
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
    /// 2) общий input worker досылает уже принятые mic chunks
    /// 3) translation session finish принимает финальный text/audio tail
    /// 4) output worker дожидается фактического хвоста и закрывается
    pub async fn stop_translation(&self) -> Result<(), LiveTranslationError> {
        let mut guard = self.inner.lock().await;
        let Some(session) = guard.take() else {
            return Ok(());
        };
        *self.status.write().await = RecordingStatus::Processing;

        session
            .shutdown(RealtimeInterpretationShutdown::Graceful)
            .await;

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
    call_live_callback("Recording status", || {
        (callbacks.on_status)(RecordingStatus::Recording)
    });
    true
}

fn spawn_live_runtime_cleanup_monitor(
    inner: Arc<Mutex<Option<RealtimeInterpretationSession>>>,
    status: Arc<RwLock<RecordingStatus>>,
    callbacks: LiveTranslationCallbacks,
    runtime_stop_rx: tokio::sync::mpsc::UnboundedReceiver<RealtimeInterpretationStop>,
    session_id: u64,
) {
    let _supervisor = spawn_realtime_interpretation_supervisor(
        session_id,
        runtime_stop_rx,
        move |session_id, stop| async move {
            let mut guard = inner.lock().await;
            let is_current = guard
                .as_ref()
                .map(|session| session.session_id() == session_id)
                .unwrap_or(false);
            let session = if is_current { guard.take() } else { None };
            let Some(session) = session else {
                return;
            };

            let error = match stop {
                RealtimeInterpretationStop::Error(error) => error.into(),
                RealtimeInterpretationStop::Closed => LiveTranslationError::Connection(
                    "OpenAI realtime translation session closed unexpectedly".to_string(),
                ),
            };

            session
                .shutdown(RealtimeInterpretationShutdown::Abort)
                .await;
            *status.write().await = RecordingStatus::Error;
            notify_live_runtime_error(&callbacks, error);
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        AudioCapture, AudioChunk, AudioChunkCallback, RealtimeTranslationEvent,
        RealtimeTranslationSession, TranslationAudioOutput,
    };
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;
    use tokio::sync::mpsc;

    const OPENAI_INPUT_FRAME_SAMPLES: usize = 4_800;

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
    fn runtime_error_callback_panic_does_not_skip_status_callback() {
        let status_called = Arc::new(AtomicBool::new(false));
        let callbacks = LiveTranslationCallbacks {
            on_transcript_delta: Arc::new(|_| {}),
            on_audio_spectrum: Arc::new(|_| {}),
            on_error: Arc::new(|_| panic!("simulated live error callback panic")),
            on_status: {
                let status_called = status_called.clone();
                Arc::new(move |status| {
                    if status == RecordingStatus::Error {
                        status_called.store(true, Ordering::SeqCst);
                    }
                })
            },
        };

        notify_live_runtime_error(
            &callbacks,
            LiveTranslationError::Processing("test failure".to_string()),
        );

        assert!(status_called.load(Ordering::SeqCst));
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
        ) -> crate::domain::TranslationAudioOutputResult<crate::domain::AudioEnqueueOutcome>
        {
            Ok(crate::domain::AudioEnqueueOutcome::Queued {
                pending: Duration::ZERO,
            })
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
        ) -> crate::domain::TranslationAudioOutputResult<crate::domain::AudioEnqueueOutcome>
        {
            self.state
                .enqueued
                .lock()
                .unwrap()
                .extend_from_slice(samples);
            Ok(crate::domain::AudioEnqueueOutcome::Queued {
                pending: Duration::ZERO,
            })
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
        callback: Option<AudioChunkCallback>,
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
            self.callback = Some(on_chunk);
            Ok(())
        }

        async fn stop_capture(&mut self) -> crate::domain::AudioResult<()> {
            self.stopped.store(true, Ordering::SeqCst);
            self.callback = None;
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
                callback: None,
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
        fail_append_call: AtomicUsize,
        target_language: StdMutex<Option<String>>,
        received_samples: StdMutex<Vec<i16>>,
        event_tx: StdMutex<Option<mpsc::Sender<RealtimeTranslationEvent>>>,
        runtime_event_after_first_append: StdMutex<Option<RealtimeTranslationEvent>>,
    }

    struct SyntheticRealtimeClientFactory {
        state: Arc<SyntheticRealtimeState>,
    }

    impl RealtimeTranslationFactory for SyntheticRealtimeClientFactory {
        fn create(&self) -> Box<dyn RealtimeTranslationSession> {
            Box::new(SyntheticRealtimeClient {
                state: self.state.clone(),
            })
        }
    }

    struct SyntheticRealtimeClient {
        state: Arc<SyntheticRealtimeState>,
    }

    #[async_trait::async_trait]
    impl RealtimeTranslationSession for SyntheticRealtimeClient {
        async fn connect(
            &mut self,
            config: RealtimeTranslationConfig,
        ) -> Result<mpsc::Receiver<RealtimeTranslationEvent>, RealtimeTranslationError> {
            self.state.connect_calls.fetch_add(1, Ordering::SeqCst);
            *self.state.target_language.lock().unwrap() = Some(config.target_language);
            let (tx, rx) = mpsc::channel(16);
            *self.state.event_tx.lock().unwrap() = Some(tx.clone());
            Ok(rx)
        }

        async fn append_pcm16(&mut self, pcm16: &[i16]) -> Result<(), RealtimeTranslationError> {
            let call = self.state.append_calls.fetch_add(1, Ordering::SeqCst) + 1;
            self.state
                .received_samples
                .lock()
                .unwrap()
                .extend_from_slice(pcm16);
            if self.state.fail_append_call.load(Ordering::SeqCst) == call {
                return Err(RealtimeTranslationError::Connection(
                    "simulated append failure".to_string(),
                ));
            }

            if call == 1 {
                let tx = self.state.event_tx.lock().unwrap().clone();
                if let Some(tx) = tx {
                    let _ = tx
                        .send(RealtimeTranslationEvent::TranslatedTextDelta(
                            "hello ".to_string(),
                        ))
                        .await;
                    let _ = tx
                        .send(RealtimeTranslationEvent::TranslatedAudio {
                            pcm16: vec![9_000; 2_400],
                            sample_rate: 24_000,
                            channels: 1,
                        })
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

        async fn finish(
            &mut self,
            _drain_timeout: Duration,
        ) -> Result<(), RealtimeTranslationError> {
            self.state.close_calls.fetch_add(1, Ordering::SeqCst);
            if self.state.fail_close.load(Ordering::SeqCst) {
                return Err(RealtimeTranslationError::Connection(
                    "simulated close failure".to_string(),
                ));
            }
            let tx = self.state.event_tx.lock().unwrap().clone();
            if let Some(tx) = tx {
                let _ = tx
                    .send(RealtimeTranslationEvent::TranslatedTextDelta(
                        "world".to_string(),
                    ))
                    .await;
                let _ = tx
                    .send(RealtimeTranslationEvent::TranslatedAudio {
                        pcm16: vec![-9_000; 1_200],
                        sample_rate: 24_000,
                        channels: 1,
                    })
                    .await;
                let _ = tx.send(RealtimeTranslationEvent::Closed).await;
            }
            Ok(())
        }

        async fn abort(&mut self) {
            self.state.abort_calls.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn start_after_error_cleans_stale_session_before_retry() {
        let realtime_state = Arc::new(SyntheticRealtimeState::default());
        let svc = LiveTranslationService::new_with_factories(
            Arc::new(SyntheticPlatformAudioFactory {
                output_state: Arc::new(SyntheticOutputState::default()),
                capture_started: Arc::new(AtomicBool::new(false)),
                capture_stopped: Arc::new(AtomicBool::new(false)),
                mic_target: Arc::new(StdMutex::new(None)),
            }),
            Arc::new(SyntheticRealtimeClientFactory {
                state: realtime_state.clone(),
            }),
        );

        svc.start_translation(valid_config(97), test_callbacks())
            .await
            .unwrap();
        *svc.status.write().await = RecordingStatus::Error;

        svc.start_translation(valid_config(98), test_callbacks())
            .await
            .unwrap();

        assert_eq!(svc.active_session_id().await, Some(98));
        assert_eq!(svc.get_status().await, RecordingStatus::Recording);
        assert_eq!(realtime_state.connect_calls.load(Ordering::SeqCst), 2);
        assert_eq!(realtime_state.abort_calls.load(Ordering::SeqCst), 1);

        svc.stop_translation().await.unwrap();
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
    async fn status_callback_panic_does_not_break_live_session_lifecycle() {
        let output_state = Arc::new(SyntheticOutputState::default());
        let capture_stopped = Arc::new(AtomicBool::new(false));
        let svc = LiveTranslationService::new_with_factories(
            Arc::new(SyntheticPlatformAudioFactory {
                output_state: output_state.clone(),
                capture_started: Arc::new(AtomicBool::new(false)),
                capture_stopped: capture_stopped.clone(),
                mic_target: Arc::new(StdMutex::new(None)),
            }),
            Arc::new(SyntheticRealtimeClientFactory {
                state: Arc::new(SyntheticRealtimeState::default()),
            }),
        );
        let callbacks = LiveTranslationCallbacks {
            on_transcript_delta: Arc::new(|_| {}),
            on_audio_spectrum: Arc::new(|_| {}),
            on_error: Arc::new(|_| {}),
            on_status: Arc::new(|_| panic!("simulated live status callback panic")),
        };

        svc.start_translation(valid_config(99), callbacks)
            .await
            .expect("status callback panic must not abort startup");
        assert_eq!(svc.get_status().await, RecordingStatus::Recording);
        assert_eq!(svc.active_session_id().await, Some(99));

        svc.stop_translation().await.unwrap();
        assert_eq!(svc.get_status().await, RecordingStatus::Idle);
        assert!(capture_stopped.load(Ordering::SeqCst));
        assert!(output_state.closed.load(Ordering::SeqCst));
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
            .unwrap() = Some(RealtimeTranslationEvent::Closed);
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
    async fn openai_append_failure_cleans_session_and_allows_restart() {
        let output_state = Arc::new(SyntheticOutputState::default());
        let realtime_state = Arc::new(SyntheticRealtimeState::default());
        realtime_state.fail_append_call.store(1, Ordering::SeqCst);
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

        svc.start_translation(valid_config(95), callbacks)
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
        assert!(capture_started.load(Ordering::SeqCst));
        assert!(capture_stopped.load(Ordering::SeqCst));
        assert!(output_state.closed.load(Ordering::SeqCst));
        assert_eq!(realtime_state.abort_calls.load(Ordering::SeqCst), 1);
        assert!(
            errors
                .lock()
                .unwrap()
                .iter()
                .any(|err| err.contains("simulated append failure")),
            "expected append failure error, got {:?}",
            errors.lock().unwrap()
        );
        assert!(
            statuses.lock().unwrap().contains(&RecordingStatus::Error),
            "expected Error status, got {:?}",
            statuses.lock().unwrap()
        );

        realtime_state.fail_append_call.store(0, Ordering::SeqCst);
        svc.start_translation(valid_config(96), test_callbacks())
            .await
            .unwrap();
        assert_eq!(svc.get_status().await, RecordingStatus::Recording);
        svc.stop_translation().await.unwrap();
        assert_eq!(svc.get_status().await, RecordingStatus::Idle);
        assert_eq!(realtime_state.connect_calls.load(Ordering::SeqCst), 2);
        assert!(realtime_state.append_calls.load(Ordering::SeqCst) >= 2);
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
