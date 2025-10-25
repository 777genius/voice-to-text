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

    /// Receiver для VAD silence timeout событий
    /// Используется в setup для установки обработчика
    pub vad_timeout_rx: Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<()>>>,

    /// VAD timeout handler task (для перезапуска при смене устройства)
    vad_handler_task: Arc<RwLock<Option<tauri::async_runtime::JoinHandle<()>>>>,

    /// Bundle ID последнего активного приложения (перед показом Voice to Text окна)
    /// Используется для автоматической вставки текста в правильное окно
    pub last_focused_app_bundle_id: Arc<RwLock<Option<String>>>,
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

                // Создаем dummy channel для VAD (не будет использоваться с mock)
                let (_vad_tx, vad_rx) = tokio::sync::mpsc::unbounded_channel();

                return Self {
                    transcription_service: service,
                    config: Arc::new(RwLock::new(AppConfig::default())),
                    history: Arc::new(RwLock::new(Vec::new())),
                    partial_transcription: Arc::new(RwLock::new(None)),
                    final_transcription: Arc::new(RwLock::new(None)),
                    microphone_test: Arc::new(RwLock::new(MicrophoneTestState::default())),
                    vad_timeout_rx: Arc::new(tokio::sync::Mutex::new(vad_rx)),
                    vad_handler_task: Arc::new(RwLock::new(None)),
                    last_focused_app_bundle_id: Arc::new(RwLock::new(None)),
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

                // Создаем dummy channel для VAD (не будет использоваться без VAD)
                let (_vad_tx, vad_rx) = tokio::sync::mpsc::unbounded_channel();

                return Self {
                    transcription_service: service,
                    config: Arc::new(RwLock::new(app_config)),
                    history: Arc::new(RwLock::new(Vec::new())),
                    partial_transcription: Arc::new(RwLock::new(None)),
                    final_transcription: Arc::new(RwLock::new(None)),
                    microphone_test: Arc::new(RwLock::new(MicrophoneTestState::default())),
                    vad_timeout_rx: Arc::new(tokio::sync::Mutex::new(vad_rx)),
                    vad_handler_task: Arc::new(RwLock::new(None)),
                    last_focused_app_bundle_id: Arc::new(RwLock::new(None)),
                };
            }
        };

        // Создаем channel для VAD timeout событий
        let (vad_tx, vad_rx) = tokio::sync::mpsc::unbounded_channel();

        // Wrap system audio with VAD
        let mut vad_wrapper = VadCaptureWrapper::new(Box::new(system_audio), vad);

        // Устанавливаем callback который отправляет событие в channel
        vad_wrapper.set_silence_timeout_callback(Arc::new(move || {
            log::info!("VAD silence timeout triggered - sending notification");
            let _ = vad_tx.send(());
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
            vad_timeout_rx: Arc::new(tokio::sync::Mutex::new(vad_rx)),
            vad_handler_task: Arc::new(RwLock::new(None)),
            last_focused_app_bundle_id: Arc::new(RwLock::new(None)),
        }
    }

    /// Запускает обработчик VAD timeout событий (вызывается из setup)
    /// Слушает channel и автоматически останавливает запись
    pub fn start_vad_timeout_handler(&self, app_handle: tauri::AppHandle) {
        let service = self.transcription_service.clone();
        let rx = self.vad_timeout_rx.clone();

        let handle = tauri::async_runtime::spawn(async move {
            let mut rx_guard = rx.lock().await;

            while let Some(_) = rx_guard.recv().await {
                log::info!("VAD silence timeout detected - auto-stopping recording");

                // Проверяем что действительно идет запись
                let status = service.get_status().await;
                if status != crate::domain::RecordingStatus::Recording {
                    log::debug!("VAD timeout ignored - not recording (status: {:?})", status);
                    continue;
                }

                // Останавливаем запись
                match service.stop_recording().await {
                    Ok(_) => {
                        log::info!("Recording stopped successfully by VAD timeout");

                        // Эмитим событие в UI
                        use tauri::Emitter;
                        let _ = app_handle.emit(
                            crate::presentation::events::EVENT_RECORDING_STATUS,
                            crate::presentation::RecordingStatusPayload {
                                status: crate::domain::RecordingStatus::Idle,
                                stopped_via_hotkey: false,
                            },
                        );

                        // Также эмитим специальное событие VAD timeout (для информирования)
                        let _ = app_handle.emit("vad-silence-timeout", ());
                    }
                    Err(e) => {
                        log::error!("Failed to stop recording on VAD timeout: {}", e);
                    }
                }
            }

            log::warn!("VAD timeout handler exited");
        });

        // Сохраняем handle для возможности перезапуска
        let task_arc = self.vad_handler_task.clone();
        tauri::async_runtime::spawn(async move {
            *task_arc.write().await = Some(handle);
        });

        log::info!("VAD auto-stop handler started");
    }

    /// Перезапускает VAD timeout handler (используется при смене устройства)
    pub async fn restart_vad_timeout_handler(&self, app_handle: tauri::AppHandle) {
        log::info!("Restarting VAD timeout handler");

        // Отменяем старый handler если он запущен
        if let Some(old_handle) = self.vad_handler_task.write().await.take() {
            log::debug!("Aborting old VAD handler");
            old_handle.abort();
            let _ = old_handle.await; // Ждем завершения
        }

        // Запускаем новый handler
        self.start_vad_timeout_handler(app_handle);

        log::info!("VAD timeout handler restarted successfully");
    }

    /// Пересоздает audio capture с новым устройством (применяет selected_audio_device)
    /// Можно вызывать при старте приложения и при смене устройства в настройках
    pub async fn recreate_audio_capture_with_device(
        &self,
        device_name: Option<String>,
        app_handle: tauri::AppHandle,
    ) -> Result<(), String> {
        log::info!("Recreating audio capture with device: {:?}", device_name);

        // Создаем новый SystemAudioCapture с выбранным устройством
        let system_audio = SystemAudioCapture::with_device(device_name.clone())
            .map_err(|e| format!("Failed to create audio capture with device {:?}: {}", device_name, e))?;

        // Получаем текущий VAD timeout из конфига
        let vad_timeout_ms = self.config.read().await.vad_silence_timeout_ms;

        // Создаем VAD processor
        let vad = VadProcessor::new(Some(vad_timeout_ms), None)
            .map_err(|e| format!("Failed to create VAD processor: {}", e))?;

        // Wrap system audio with VAD
        let mut vad_wrapper = VadCaptureWrapper::new(Box::new(system_audio), vad);

        // Копируем callback из текущего vad_timeout_rx (создаем новый channel)
        let (vad_tx, vad_rx) = tokio::sync::mpsc::unbounded_channel();
        vad_wrapper.set_silence_timeout_callback(Arc::new(move || {
            log::info!("VAD silence timeout triggered - sending notification");
            let _ = vad_tx.send(());
        }));

        // Заменяем vad_timeout_rx на новый
        *self.vad_timeout_rx.lock().await = vad_rx;

        // Заменяем audio capture в TranscriptionService
        self.transcription_service
            .replace_audio_capture(Box::new(vad_wrapper))
            .await
            .map_err(|e| format!("Failed to replace audio capture: {}", e))?;

        // Перезапускаем VAD timeout handler чтобы он слушал новый channel
        self.restart_vad_timeout_handler(app_handle).await;

        log::info!("Audio capture recreated successfully with device: {:?}", device_name);
        Ok(())
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
