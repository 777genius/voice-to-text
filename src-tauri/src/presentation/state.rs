use std::sync::Arc;
use tokio::sync::RwLock;

use crate::application::TranscriptionService;
use crate::domain::{AppConfig, Transcription, AudioCapture};
use crate::infrastructure::{
    audio::{SystemAudioCapture, VadCaptureWrapper, VadProcessor},
    DefaultSttProviderFactory,
};

/// State for microphone testing
pub struct MicrophoneTestState {
    /// Audio capture instance for testing
    pub capture: Option<Box<dyn AudioCapture>>,
    /// Shared buffer of recorded samples during test
    pub buffer: Arc<tokio::sync::Mutex<Vec<i16>>>,
    /// Is test currently running
    pub is_testing: bool,
}

impl Default for MicrophoneTestState {
    fn default() -> Self {
        Self {
            capture: None,
            buffer: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            is_testing: false,
        }
    }
}

/// Global application state managed by Tauri
///
/// This state is shared across all Tauri commands and can be accessed
/// using State<AppState> parameter in command functions
pub struct AppState {
    /// Main transcription service
    pub transcription_service: Arc<TranscriptionService>,

    /// Application configuration
    pub config: Arc<RwLock<AppConfig>>,

    /// Transcription history
    pub history: Arc<RwLock<Vec<Transcription>>>,

    /// Latest partial transcription
    pub partial_transcription: Arc<RwLock<Option<String>>>,

    /// Latest final transcription
    pub final_transcription: Arc<RwLock<Option<String>>>,

    /// Microphone test state
    pub microphone_test: Arc<RwLock<MicrophoneTestState>>,
}

impl AppState {
    pub fn new() -> Self {
        // Initialize real audio capture with VAD
        let system_audio = match SystemAudioCapture::new() {
            Ok(capture) => capture,
            Err(e) => {
                log::error!("Failed to initialize system audio: {}. Using mock.", e);
                // Fallback to mock if no audio device
                let mock = crate::infrastructure::audio::MockAudioCapture::new();
                let stt_factory = Arc::new(DefaultSttProviderFactory::new());
                let service = Arc::new(TranscriptionService::new(Box::new(mock), stt_factory));

                return Self {
                    transcription_service: service,
                    config: Arc::new(RwLock::new(AppConfig::default())),
                    history: Arc::new(RwLock::new(Vec::new())),
                    partial_transcription: Arc::new(RwLock::new(None)),
                    final_transcription: Arc::new(RwLock::new(None)),
                    microphone_test: Arc::new(RwLock::new(MicrophoneTestState::default())),
                };
            }
        };

        // Initialize VAD processor с timeout из конфигурации
        let app_config = AppConfig::default();
        let vad = match VadProcessor::new(Some(app_config.vad_silence_timeout_ms), None) {
            Ok(processor) => processor,
            Err(e) => {
                log::error!("Failed to initialize VAD: {}. Proceeding without VAD.", e);
                // Fallback: use system audio without VAD
                let stt_factory = Arc::new(DefaultSttProviderFactory::new());
                let service = Arc::new(TranscriptionService::new(Box::new(system_audio), stt_factory));

                return Self {
                    transcription_service: service,
                    config: Arc::new(RwLock::new(app_config)),
                    history: Arc::new(RwLock::new(Vec::new())),
                    partial_transcription: Arc::new(RwLock::new(None)),
                    final_transcription: Arc::new(RwLock::new(None)),
                    microphone_test: Arc::new(RwLock::new(MicrophoneTestState::default())),
                };
            }
        };

        // Wrap system audio with VAD
        let mut vad_wrapper = VadCaptureWrapper::new(Box::new(system_audio), vad);

        // Set silence timeout callback (optional: emit event to UI)
        vad_wrapper.set_silence_timeout_callback(Arc::new(|| {
            log::info!("VAD silence timeout triggered - auto-stopping recording");
            // TODO: Emit Tauri event to UI if needed
            // app_handle.emit("vad-silence-timeout", {})
        }));

        let audio_capture = Box::new(vad_wrapper);
        let stt_factory = Arc::new(DefaultSttProviderFactory::new());

        let transcription_service = Arc::new(TranscriptionService::new(audio_capture, stt_factory));

        log::info!("AppState initialized with SystemAudioCapture + VAD (timeout: {}ms)",
            app_config.vad_silence_timeout_ms);

        Self {
            transcription_service,
            config: Arc::new(RwLock::new(app_config)),
            history: Arc::new(RwLock::new(Vec::new())),
            partial_transcription: Arc::new(RwLock::new(None)),
            final_transcription: Arc::new(RwLock::new(None)),
            microphone_test: Arc::new(RwLock::new(MicrophoneTestState::default())),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
