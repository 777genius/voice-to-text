use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::RwLock;

use crate::application::services::{
    IncomingSpokenTranslationPorts, IncomingTranslationFacade, IncomingTranslationFacadeFactory,
    LiveTranslationPorts, LiveTranslationService,
};
use crate::application::TranscriptionService;
use crate::domain::{
    AppConfig, AudioCapture, AudioError, RecordingMode, Transcription, UiPreferences,
};
use crate::infrastructure::{
    audio::{
        DefaultLocalPlaybackOutputFactory, DefaultPlatformAudioFactory,
        DefaultSpokenTranslationCapability, SystemAudioCapture, VadCaptureWrapper, VadProcessor,
    },
    auto_paste::AutoPasteTarget,
    openai::OpenAIRealtimeTranslationFactory,
    AuthSession, AuthStore, AuthStoreData, AuthUser, ConfigStore, DefaultSttProviderFactory,
};

const RECORDING_WINDOW_POSITION_SAVE_SUPPRESSION_MS: i64 = 800;
const TRANSLATION_APP_EXIT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(4_500);

fn default_incoming_translation_factory() -> IncomingTranslationFacadeFactory {
    let audio_factory = Arc::new(DefaultPlatformAudioFactory::new());
    IncomingTranslationFacadeFactory::new(
        Arc::new(DefaultSttProviderFactory::new()),
        audio_factory.clone(),
        IncomingSpokenTranslationPorts::new(
            audio_factory,
            Arc::new(DefaultLocalPlaybackOutputFactory::new()),
            Arc::new(OpenAIRealtimeTranslationFactory),
            Arc::new(DefaultSpokenTranslationCapability::new()),
        ),
    )
}

fn default_live_translation_ports() -> LiveTranslationPorts {
    LiveTranslationPorts::new(
        Arc::new(DefaultPlatformAudioFactory::new()),
        Arc::new(OpenAIRealtimeTranslationFactory),
    )
}

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

fn normalize_audio_capture_device_name(device_name: Option<String>) -> Option<String> {
    device_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
}

fn audio_capture_device_cache_matches(
    _cached_device: &Option<Option<String>>,
    _requested_device: &Option<String>,
) -> bool {
    // Do not reuse cpal input handles between recording starts. After hotplug,
    // the saved device name can be the same while the underlying handle is stale
    // or has internally fallen back to the system default input.
    false
}

fn is_current_vad_timeout_session(timeout_session_id: u64, active_session_id: u64) -> bool {
    timeout_session_id != 0 && active_session_id == timeout_session_id
}

fn claim_vad_timeout_session(
    active_session_id: &AtomicU64,
    timeout_session_id: u64,
) -> Result<(), u64> {
    if timeout_session_id == 0 {
        return Err(active_session_id.load(Ordering::Relaxed));
    }

    active_session_id
        .compare_exchange(timeout_session_id, 0, Ordering::Relaxed, Ordering::Relaxed)
        .map(|_| ())
}

fn restore_vad_timeout_session_claim_if_unclaimed(
    active_session_id: &AtomicU64,
    timeout_session_id: u64,
) {
    if timeout_session_id == 0 {
        return;
    }

    let _ = active_session_id.compare_exchange(
        0,
        timeout_session_id,
        Ordering::Relaxed,
        Ordering::Relaxed,
    );
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

    /// Per-topic ревизии для state-sync протокола (монотонно растут)
    pub app_config_revision: Arc<RwLock<u64>>,
    pub stt_config_revision: Arc<RwLock<u64>>,
    pub auth_state_revision: Arc<RwLock<u64>>,
    pub ui_preferences_revision: Arc<RwLock<u64>>,

    /// UI-настройки (тема, локаль)
    pub ui_preferences: Arc<RwLock<UiPreferences>>,

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
    pub vad_timeout_tx: tokio::sync::mpsc::UnboundedSender<u64>,
    pub vad_timeout_rx: Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<u64>>>,

    /// VAD timeout handler task (для перезапуска при смене устройства)
    vad_handler_task: Arc<RwLock<Option<tauri::async_runtime::JoinHandle<()>>>>,

    /// Последнее активное приложение (перед показом VoicetextAI окна)
    /// Используется для автоматической вставки текста в правильное окно
    pub last_focused_app_target: Arc<RwLock<Option<AutoPasteTarget>>>,

    /// Флаг авторизации пользователя (синхронизируется из frontend)
    /// Используется для определения какое окно показывать при нажатии hotkey
    pub is_authenticated: Arc<RwLock<bool>>,

    /// Auth store (device_id + session) — Rust source of truth.
    ///
    /// Важно: нужен даже когда WebView "спит" (hotkey сценарий).
    pub auth_store: Arc<RwLock<AuthStoreData>>,

    /// Ревизия auth-session topic (меняется и при refresh, и при login/logout).
    pub auth_session_revision: Arc<RwLock<u64>>,

    /// Фоновая задача refresh токенов (если есть refresh_token).
    pub auth_refresh_task: Arc<RwLock<Option<tauri::async_runtime::JoinHandle<()>>>>,

    /// Гарантия, что одновременно существует только одна refresh-задача.
    /// Нужна, потому что `restart_auth_refresh_task` может вызываться конкурентно (несколько окон/событий),
    /// и без сериализации легко получить 2+ задач, которые спамят refresh/лог/диск.
    pub auth_refresh_task_guard: Arc<tokio::sync::Mutex<()>>,

    /// Сериализует read-modify-write операции над STT конфигом.
    /// Иначе concurrent save путями (settings/auth/startup) можно перетереть `streaming_keyterms`
    /// stale-снапшотом даже если каждое место по отдельности "правильное".
    pub stt_config_guard: Arc<tokio::sync::Mutex<()>>,

    /// Сериализует auto-paste: параллельные вставки перемешали бы между собой
    /// последовательность clipboard set → Cmd+V → restore, и в целевое окно ушёл бы чужой текст.
    pub auto_paste_guard: Arc<tokio::sync::Mutex<()>>,

    /// Дебаунс для глобального hotkey записи.
    /// Нужен из‑за key repeat / случайных двойных срабатываний, которые выглядят как "мигание" окна.
    pub last_recording_hotkey_ms: AtomicU64,

    /// Последний сырой Pressed event до фильтрации.
    /// Нужен, чтобы отличать удержание клавиши от нового press, если Released потерялся.
    pub recording_hotkey_last_raw_press_ms: AtomicU64,

    /// Latch для глобального hotkey записи.
    /// Не даёт key repeat повторно переключать запись, пока пользователь физически не отпустил клавишу.
    pub recording_hotkey_is_pressed: AtomicBool,

    /// Видели Released после последнего принятого Pressed.
    /// Позволяет принять быстрое повторное нажатие, не открывая дверь обычному key repeat.
    pub recording_hotkey_released_since_press: AtomicBool,

    /// Время последнего Released для фильтрации синтетических release/press пар от key repeat.
    pub recording_hotkey_last_release_ms: AtomicU64,

    /// Поколение отложенного сброса hotkey latch.
    /// Нужен, потому что на macOS bare-key shortcuts могут присылать Released между repeat Pressed.
    pub recording_hotkey_release_generation: AtomicU64,

    /// Поколение сырых Pressed events.
    /// Нужно, чтобы отличать удержанный key repeat от нового нажатия, если Released потерялся.
    pub recording_hotkey_press_generation: AtomicU64,

    /// Количество принятых Pressed events после latch/debounce фильтров.
    /// Нужен, чтобы stop-after-start suppression не зависел от Released, который macOS может потерять.
    pub recording_hotkey_accepted_press_seq: AtomicU64,

    /// До какого момента игнорировать Pressed от recording hotkey.
    /// Нужно на время auto-paste, потому что synthetic text input может совпасть с выбранной клавишей.
    pub recording_hotkey_suppressed_until_ms: AtomicU64,

    /// До какого момента не даём hotkey сразу остановить только что запрошенный start.
    /// Защищает от повторного Pressed/key repeat после быстрого stop -> start.
    pub recording_hotkey_stop_suppressed_until_ms: AtomicU64,

    /// accepted_press_seq на момент включения stop-after-start suppression.
    pub recording_hotkey_stop_suppression_press_seq: AtomicU64,

    /// Пользователь нажал hotkey ещё раз, пока предыдущая запись завершалась.
    /// После перехода Recording -> Processing -> Idle стартуем новую запись автоматически.
    pub recording_start_pending_after_stop: AtomicBool,

    /// Сериализует hotkey toggle, чтобы stop и следующий start не выполнялись параллельно.
    pub recording_hotkey_toggle_guard: Arc<tokio::sync::Mutex<()>>,

    /// Сериализует lifecycle записи для UI-команд, hotkey и live translation.
    pub recording_lifecycle_guard: Arc<tokio::sync::Mutex<()>>,

    /// Не даёт start/preflight операциям одновременно открывать одни audio resources.
    pub audio_start_guard: Arc<tokio::sync::Mutex<()>>,

    /// Сериализует start/stop входящих субтитров между окнами.
    pub incoming_translation_lifecycle_guard: Arc<tokio::sync::Mutex<()>>,

    /// Сериализует register/unregister глобального hotkey.
    /// На Windows startup/settings пути могут иначе оставить зарегистрированным устаревшее значение.
    pub recording_hotkey_registration_guard: Arc<tokio::sync::Mutex<()>>,

    /// Runtime mirror for the optional double-Space hotkey.
    /// Read from the global keyboard listener thread without async locks.
    pub double_space_hotkey_enabled_runtime: AtomicBool,

    /// The rdev global listener is blocking and cannot be stopped cleanly, so it is started once.
    pub double_space_hotkey_listener_started: AtomicBool,

    /// Какое устройство сейчас применено к audio capture.
    /// None снаружи = неизвестно/нужно пересоздать; Some(None) = системный default input.
    pub active_audio_capture_device: Arc<RwLock<Option<Option<String>>>>,

    /// Счётчик сессий записи. Нужен, чтобы маркировать события transcription:* и не смешивать сессии.
    pub transcription_session_seq: AtomicU64,

    /// Активная (последняя запущенная) сессия записи.
    /// Используется для маркировки статусов Idle/Error, которые эмитятся "в обход" start_recording callbacks.
    pub active_transcription_session_id: Arc<AtomicU64>,

    /// Какой режим (dictation / live_translation) сейчас владеет активной сессией.
    /// None = ничего не запущено. Hotkey stop читает active_recording_mode (не AppConfig),
    /// чтобы остановить именно то, что играет — даже если пользователь переключил Settings.
    pub active_recording_mode: Arc<RwLock<Option<RecordingMode>>>,

    /// Live translation service. Создаётся лениво при первом start_translation,
    /// потому что connect к OpenAI стоит денег и не должен происходить до явного намерения.
    pub live_translation_service: Arc<RwLock<Option<Arc<LiveTranslationService>>>>,

    /// Outgoing translation dependencies composed outside the application service.
    pub live_translation_ports: LiveTranslationPorts,

    /// Incoming translation facade: system audio -> selected delivery pipeline.
    /// Separate from active_recording_mode so it can run alongside outgoing translation later.
    pub incoming_translation_facade: Arc<RwLock<Option<Arc<IncomingTranslationFacade>>>>,

    /// Infrastructure dependencies for the macOS spoken incoming pipeline.
    pub incoming_translation_factory: IncomingTranslationFacadeFactory,

    /// Счётчик сессий входящих субтитров. Отдельный от recording session id.
    pub incoming_translation_session_seq: AtomicU64,

    /// Prevents duplicate async cleanup when Tauri emits both ExitRequested and Exit.
    pub translation_shutdown_started: AtomicBool,

    /// До какого момента игнорировать WindowEvent::Moved для main окна.
    /// Нужно, чтобы программные resize/show/fit не перезаписывали пользовательскую mini-позицию.
    pub recording_window_position_save_suppressed_until_ms: AtomicI64,
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
                let microphone_sensitivity = Arc::new(AtomicU8::new(100));
                let service = Arc::new(TranscriptionService::new_with_microphone_sensitivity(
                    Box::new(mock),
                    stt_factory,
                    microphone_sensitivity,
                ));

                // Создаем dummy channel для VAD (не будет использоваться с mock)
                let (vad_tx, vad_rx) = tokio::sync::mpsc::unbounded_channel();

                return Self {
                    transcription_service: service,
                    config: Arc::new(RwLock::new(AppConfig::default())),
                    app_config_revision: Arc::new(RwLock::new(0)),
                    stt_config_revision: Arc::new(RwLock::new(0)),
                    auth_state_revision: Arc::new(RwLock::new(0)),
                    ui_preferences_revision: Arc::new(RwLock::new(0)),
                    ui_preferences: Arc::new(RwLock::new(UiPreferences::default())),
                    history: Arc::new(RwLock::new(Vec::new())),
                    partial_transcription: Arc::new(RwLock::new(None)),
                    final_transcription: Arc::new(RwLock::new(None)),
                    microphone_test: Arc::new(RwLock::new(MicrophoneTestState::default())),
                    vad_timeout_tx: vad_tx,
                    vad_timeout_rx: Arc::new(tokio::sync::Mutex::new(vad_rx)),
                    vad_handler_task: Arc::new(RwLock::new(None)),
                    last_focused_app_target: Arc::new(RwLock::new(None)),
                    is_authenticated: Arc::new(RwLock::new(false)),
                    auth_store: Arc::new(RwLock::new(AuthStoreData {
                        device_id: format!("desktop-{}", uuid::Uuid::new_v4()),
                        session: None,
                    })),
                    auth_session_revision: Arc::new(RwLock::new(0)),
                    auth_refresh_task: Arc::new(RwLock::new(None)),
                    auth_refresh_task_guard: Arc::new(tokio::sync::Mutex::new(())),
                    stt_config_guard: Arc::new(tokio::sync::Mutex::new(())),
                    auto_paste_guard: Arc::new(tokio::sync::Mutex::new(())),
                    last_recording_hotkey_ms: AtomicU64::new(0),
                    recording_hotkey_last_raw_press_ms: AtomicU64::new(0),
                    recording_hotkey_is_pressed: AtomicBool::new(false),
                    recording_hotkey_released_since_press: AtomicBool::new(false),
                    recording_hotkey_last_release_ms: AtomicU64::new(0),
                    recording_hotkey_release_generation: AtomicU64::new(0),
                    recording_hotkey_press_generation: AtomicU64::new(0),
                    recording_hotkey_accepted_press_seq: AtomicU64::new(0),
                    recording_hotkey_suppressed_until_ms: AtomicU64::new(0),
                    recording_hotkey_stop_suppressed_until_ms: AtomicU64::new(0),
                    recording_hotkey_stop_suppression_press_seq: AtomicU64::new(0),
                    recording_start_pending_after_stop: AtomicBool::new(false),
                    recording_hotkey_toggle_guard: Arc::new(tokio::sync::Mutex::new(())),
                    recording_lifecycle_guard: Arc::new(tokio::sync::Mutex::new(())),
                    audio_start_guard: Arc::new(tokio::sync::Mutex::new(())),
                    incoming_translation_lifecycle_guard: Arc::new(tokio::sync::Mutex::new(())),
                    recording_hotkey_registration_guard: Arc::new(tokio::sync::Mutex::new(())),
                    double_space_hotkey_enabled_runtime: AtomicBool::new(false),
                    double_space_hotkey_listener_started: AtomicBool::new(false),
                    active_audio_capture_device: Arc::new(RwLock::new(None)),
                    transcription_session_seq: AtomicU64::new(0),
                    active_transcription_session_id: Arc::new(AtomicU64::new(0)),
                    active_recording_mode: Arc::new(RwLock::new(None)),
                    live_translation_service: Arc::new(RwLock::new(None)),
                    live_translation_ports: default_live_translation_ports(),
                    incoming_translation_facade: Arc::new(RwLock::new(None)),
                    incoming_translation_factory: default_incoming_translation_factory(),
                    incoming_translation_session_seq: AtomicU64::new(0),
                    translation_shutdown_started: AtomicBool::new(false),
                    recording_window_position_save_suppressed_until_ms: AtomicI64::new(0),
                };
            }
        };

        // Initialize VAD processor с timeout из конфигурации
        let app_config = AppConfig::default();
        let microphone_sensitivity = Arc::new(AtomicU8::new(app_config.microphone_sensitivity));
        let vad = match VadProcessor::new(Some(app_config.vad_silence_timeout_ms), None) {
            Ok(processor) => processor,
            Err(e) => {
                log::error!("Failed to initialize VAD: {}. Proceeding without VAD.", e);
                // Fallback: use system audio without VAD
                let stt_factory = Arc::new(DefaultSttProviderFactory::new());
                let service = Arc::new(TranscriptionService::new_with_microphone_sensitivity(
                    Box::new(system_audio),
                    stt_factory,
                    microphone_sensitivity,
                ));

                // Создаем dummy channel для VAD (не будет использоваться без VAD)
                let (vad_tx, vad_rx) = tokio::sync::mpsc::unbounded_channel();

                return Self {
                    transcription_service: service,
                    config: Arc::new(RwLock::new(app_config)),
                    app_config_revision: Arc::new(RwLock::new(0)),
                    stt_config_revision: Arc::new(RwLock::new(0)),
                    auth_state_revision: Arc::new(RwLock::new(0)),
                    ui_preferences_revision: Arc::new(RwLock::new(0)),
                    ui_preferences: Arc::new(RwLock::new(UiPreferences::default())),
                    history: Arc::new(RwLock::new(Vec::new())),
                    partial_transcription: Arc::new(RwLock::new(None)),
                    final_transcription: Arc::new(RwLock::new(None)),
                    microphone_test: Arc::new(RwLock::new(MicrophoneTestState::default())),
                    vad_timeout_tx: vad_tx,
                    vad_timeout_rx: Arc::new(tokio::sync::Mutex::new(vad_rx)),
                    vad_handler_task: Arc::new(RwLock::new(None)),
                    last_focused_app_target: Arc::new(RwLock::new(None)),
                    is_authenticated: Arc::new(RwLock::new(false)),
                    auth_store: Arc::new(RwLock::new(AuthStoreData {
                        device_id: format!("desktop-{}", uuid::Uuid::new_v4()),
                        session: None,
                    })),
                    auth_session_revision: Arc::new(RwLock::new(0)),
                    auth_refresh_task: Arc::new(RwLock::new(None)),
                    auth_refresh_task_guard: Arc::new(tokio::sync::Mutex::new(())),
                    stt_config_guard: Arc::new(tokio::sync::Mutex::new(())),
                    auto_paste_guard: Arc::new(tokio::sync::Mutex::new(())),
                    last_recording_hotkey_ms: AtomicU64::new(0),
                    recording_hotkey_last_raw_press_ms: AtomicU64::new(0),
                    recording_hotkey_is_pressed: AtomicBool::new(false),
                    recording_hotkey_released_since_press: AtomicBool::new(false),
                    recording_hotkey_last_release_ms: AtomicU64::new(0),
                    recording_hotkey_release_generation: AtomicU64::new(0),
                    recording_hotkey_press_generation: AtomicU64::new(0),
                    recording_hotkey_accepted_press_seq: AtomicU64::new(0),
                    recording_hotkey_suppressed_until_ms: AtomicU64::new(0),
                    recording_hotkey_stop_suppressed_until_ms: AtomicU64::new(0),
                    recording_hotkey_stop_suppression_press_seq: AtomicU64::new(0),
                    recording_start_pending_after_stop: AtomicBool::new(false),
                    recording_hotkey_toggle_guard: Arc::new(tokio::sync::Mutex::new(())),
                    recording_lifecycle_guard: Arc::new(tokio::sync::Mutex::new(())),
                    audio_start_guard: Arc::new(tokio::sync::Mutex::new(())),
                    incoming_translation_lifecycle_guard: Arc::new(tokio::sync::Mutex::new(())),
                    recording_hotkey_registration_guard: Arc::new(tokio::sync::Mutex::new(())),
                    double_space_hotkey_enabled_runtime: AtomicBool::new(false),
                    double_space_hotkey_listener_started: AtomicBool::new(false),
                    active_audio_capture_device: Arc::new(RwLock::new(Some(None))),
                    transcription_session_seq: AtomicU64::new(0),
                    active_transcription_session_id: Arc::new(AtomicU64::new(0)),
                    active_recording_mode: Arc::new(RwLock::new(None)),
                    live_translation_service: Arc::new(RwLock::new(None)),
                    live_translation_ports: default_live_translation_ports(),
                    incoming_translation_facade: Arc::new(RwLock::new(None)),
                    incoming_translation_factory: default_incoming_translation_factory(),
                    incoming_translation_session_seq: AtomicU64::new(0),
                    translation_shutdown_started: AtomicBool::new(false),
                    recording_window_position_save_suppressed_until_ms: AtomicI64::new(0),
                };
            }
        };

        // Создаем channel для VAD timeout событий
        let (vad_tx, vad_rx) = tokio::sync::mpsc::unbounded_channel();
        let active_transcription_session_id = Arc::new(AtomicU64::new(0));

        // Wrap system audio with VAD
        let mut vad_wrapper = VadCaptureWrapper::new_with_microphone_sensitivity(
            Box::new(system_audio),
            vad,
            microphone_sensitivity.clone(),
        );

        // Устанавливаем callback который отправляет событие в channel
        let vad_tx_for_cb = vad_tx.clone();
        let active_session_id_for_vad = active_transcription_session_id.clone();
        vad_wrapper.set_silence_timeout_callback(Arc::new(move || {
            let session_id = active_session_id_for_vad.load(Ordering::Relaxed);
            log::info!(
                "VAD silence timeout triggered - sending notification (session_id={})",
                session_id
            );
            let _ = vad_tx_for_cb.send(session_id);
        }));

        let audio_capture = Box::new(vad_wrapper);
        let stt_factory = Arc::new(DefaultSttProviderFactory::new());

        let transcription_service =
            Arc::new(TranscriptionService::new_with_microphone_sensitivity(
                audio_capture,
                stt_factory,
                microphone_sensitivity,
            ));

        log::info!(
            "AppState initialized with SystemAudioCapture + VAD (timeout: {}ms)",
            app_config.vad_silence_timeout_ms
        );

        Self {
            transcription_service,
            config: Arc::new(RwLock::new(app_config)),
            app_config_revision: Arc::new(RwLock::new(0)),
            stt_config_revision: Arc::new(RwLock::new(0)),
            auth_state_revision: Arc::new(RwLock::new(0)),
            ui_preferences_revision: Arc::new(RwLock::new(0)),
            ui_preferences: Arc::new(RwLock::new(UiPreferences::default())),
            history: Arc::new(RwLock::new(Vec::new())),
            partial_transcription: Arc::new(RwLock::new(None)),
            final_transcription: Arc::new(RwLock::new(None)),
            microphone_test: Arc::new(RwLock::new(MicrophoneTestState::default())),
            vad_timeout_tx: vad_tx,
            vad_timeout_rx: Arc::new(tokio::sync::Mutex::new(vad_rx)),
            vad_handler_task: Arc::new(RwLock::new(None)),
            last_focused_app_target: Arc::new(RwLock::new(None)),
            is_authenticated: Arc::new(RwLock::new(false)),
            auth_store: Arc::new(RwLock::new(AuthStoreData {
                device_id: format!("desktop-{}", uuid::Uuid::new_v4()),
                session: None,
            })),
            auth_session_revision: Arc::new(RwLock::new(0)),
            auth_refresh_task: Arc::new(RwLock::new(None)),
            auth_refresh_task_guard: Arc::new(tokio::sync::Mutex::new(())),
            stt_config_guard: Arc::new(tokio::sync::Mutex::new(())),
            auto_paste_guard: Arc::new(tokio::sync::Mutex::new(())),
            last_recording_hotkey_ms: AtomicU64::new(0),
            recording_hotkey_last_raw_press_ms: AtomicU64::new(0),
            recording_hotkey_is_pressed: AtomicBool::new(false),
            recording_hotkey_released_since_press: AtomicBool::new(false),
            recording_hotkey_last_release_ms: AtomicU64::new(0),
            recording_hotkey_release_generation: AtomicU64::new(0),
            recording_hotkey_press_generation: AtomicU64::new(0),
            recording_hotkey_accepted_press_seq: AtomicU64::new(0),
            recording_hotkey_suppressed_until_ms: AtomicU64::new(0),
            recording_hotkey_stop_suppressed_until_ms: AtomicU64::new(0),
            recording_hotkey_stop_suppression_press_seq: AtomicU64::new(0),
            recording_start_pending_after_stop: AtomicBool::new(false),
            recording_hotkey_toggle_guard: Arc::new(tokio::sync::Mutex::new(())),
            recording_lifecycle_guard: Arc::new(tokio::sync::Mutex::new(())),
            audio_start_guard: Arc::new(tokio::sync::Mutex::new(())),
            incoming_translation_lifecycle_guard: Arc::new(tokio::sync::Mutex::new(())),
            recording_hotkey_registration_guard: Arc::new(tokio::sync::Mutex::new(())),
            double_space_hotkey_enabled_runtime: AtomicBool::new(false),
            double_space_hotkey_listener_started: AtomicBool::new(false),
            active_audio_capture_device: Arc::new(RwLock::new(Some(None))),
            transcription_session_seq: AtomicU64::new(0),
            active_transcription_session_id,
            active_recording_mode: Arc::new(RwLock::new(None)),
            live_translation_service: Arc::new(RwLock::new(None)),
            live_translation_ports: default_live_translation_ports(),
            incoming_translation_facade: Arc::new(RwLock::new(None)),
            incoming_translation_factory: default_incoming_translation_factory(),
            incoming_translation_session_seq: AtomicU64::new(0),
            translation_shutdown_started: AtomicBool::new(false),
            recording_window_position_save_suppressed_until_ms: AtomicI64::new(0),
        }
    }

    pub async fn shutdown_translation_runtimes(&self) {
        if self
            .translation_shutdown_started
            .swap(true, Ordering::SeqCst)
        {
            return;
        }

        let cleanup = async {
            let incoming = self.incoming_translation_facade.read().await.clone();
            let outgoing = self.live_translation_service.read().await.clone();
            crate::application::services::abort_translation_runtimes(incoming, outgoing).await
        };
        match tokio::time::timeout(TRANSLATION_APP_EXIT_TIMEOUT, cleanup).await {
            Ok(result) => {
                if let Some(error) = result.incoming_error {
                    log::warn!("Incoming translation abort failed during app exit: {error}");
                }
                if let Some(error) = result.outgoing_error {
                    log::warn!("Outgoing translation abort failed during app exit: {error}");
                }
            }
            Err(_) => log::warn!(
                "Translation cleanup exceeded the {} ms application-exit deadline; remaining resources will be released by process exit",
                TRANSLATION_APP_EXIT_TIMEOUT.as_millis()
            ),
        }
    }

    pub(crate) fn suppress_recording_window_position_save(&self) {
        let until =
            chrono::Utc::now().timestamp_millis() + RECORDING_WINDOW_POSITION_SAVE_SUPPRESSION_MS;
        self.recording_window_position_save_suppressed_until_ms
            .store(until, Ordering::SeqCst);
    }

    pub(crate) fn should_skip_recording_window_position_save(&self) -> bool {
        chrono::Utc::now().timestamp_millis()
            <= self
                .recording_window_position_save_suppressed_until_ms
                .load(Ordering::SeqCst)
    }

    pub(crate) fn suppress_recording_hotkey_for(&self, duration: std::time::Duration) {
        let now_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
        let duration_ms = duration.as_millis().min(u64::MAX as u128) as u64;
        let until_ms = now_ms.saturating_add(duration_ms);

        let mut current = self
            .recording_hotkey_suppressed_until_ms
            .load(Ordering::SeqCst);
        while until_ms > current {
            match self.recording_hotkey_suppressed_until_ms.compare_exchange(
                current,
                until_ms,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(next) => current = next,
            }
        }
    }

    pub(crate) fn should_suppress_recording_hotkey(&self, now_ms: u64) -> bool {
        now_ms
            <= self
                .recording_hotkey_suppressed_until_ms
                .load(Ordering::SeqCst)
    }

    /// Инкрементирует ревизию и возвращает её строковое представление
    pub async fn bump_revision(counter: &Arc<RwLock<u64>>) -> String {
        let mut rev = counter.write().await;
        *rev = rev.saturating_add(1);
        rev.to_string()
    }

    fn get_api_base_url() -> String {
        std::env::var("VOICE_TO_TEXT_API_URL")
            .unwrap_or_else(|_| "https://api.voicetext.site".to_string())
    }

    fn parse_rfc3339_to_ms(s: &str) -> Option<i64> {
        chrono::DateTime::parse_from_rfc3339(s)
            .map(|dt| dt.timestamp_millis())
            .ok()
    }

    pub(crate) async fn apply_backend_auth_token_to_stt(&self, token: Option<String>) {
        let _guard = self.stt_config_guard.lock().await;

        // Best-effort: ошибки не должны блокировать UX, но они важны для диагностики.
        // Важно: берём текущий in-memory config, чтобы не "сбрасывать" keep-alive и другие поля
        // при конкурирующих disk-write сценариях.
        let mut config = self.transcription_service.get_config().await;
        if config.backend_auth_token == token {
            return;
        }
        config.backend_auth_token = token;
        self.config.write().await.stt.backend_auth_token = config.backend_auth_token.clone();
        if let Err(e) = ConfigStore::save_config(&config).await {
            log::warn!("Failed to persist STT config token: {}", e);
        }
        if let Err(e) = self.transcription_service.update_config(config).await {
            log::warn!("Failed to update transcription service config token: {}", e);
        }
    }

    async fn emit_invalidation(
        app_handle: &AppHandle,
        topic: &str,
        revision: String,
        source_id: Option<String>,
    ) {
        let _ = app_handle.emit(
            crate::presentation::events::EVENT_STATE_SYNC_INVALIDATION,
            crate::presentation::StateSyncInvalidationPayload {
                topic: topic.to_string(),
                revision,
                source_id,
                timestamp_ms: chrono::Utc::now().timestamp_millis(),
            },
        );
    }

    /// Перезапускает фоновую задачу refresh токенов на основании текущего auth_store.
    ///
    /// Запускается:
    /// - после загрузки auth_store на старте приложения
    /// - после любых изменений сессии (login/logout/refresh) через `set_auth_session`
    pub async fn restart_auth_refresh_task(&self, app_handle: AppHandle) {
        // Сериализуем рестарт, чтобы не плодить конкурентные refresh-loop задачи.
        let _guard = self.auth_refresh_task_guard.lock().await;

        // Abort previous task
        if let Some(handle) = self.auth_refresh_task.write().await.take() {
            handle.abort();
            let _ = handle.await;
        }

        let store = self.auth_store.read().await.clone();
        let Some(session) = store.session.clone() else {
            return;
        };
        let Some(_refresh_token) = session.refresh_token.clone() else {
            return;
        };

        // If refresh token is expired (when known) — don't start.
        if let Some(exp) = session.refresh_expires_at_ms {
            if exp <= chrono::Utc::now().timestamp_millis() {
                return;
            }
        }

        let auth_store_arc = self.auth_store.clone();
        let is_authenticated_arc = self.is_authenticated.clone();
        let auth_state_revision = self.auth_state_revision.clone();
        let auth_session_revision = self.auth_session_revision.clone();
        let app_handle_for_task = app_handle.clone();
        let service_for_task = self.transcription_service.clone();

        let task = tauri::async_runtime::spawn(async move {
            const REFRESH_BUFFER_MS: i64 = 2 * 60 * 1000; // 2 minutes before access expiry
            const ERROR_RETRY_DELAY_SECS: u64 = 30;
            const RATE_LIMIT_RETRY_DELAY_SECS: u64 = 2 * 60;
            const MIN_SUCCESS_REFRESH_INTERVAL_SECS: u64 = 30;

            #[derive(serde::Serialize)]
            struct RefreshReq {
                refresh_token: String,
                device_id: String,
            }

            #[derive(serde::Deserialize)]
            struct RefreshResp {
                data: RefreshRespData,
            }

            #[derive(serde::Deserialize)]
            struct RefreshRespUser {
                id: String,
                email: String,
                email_verified: bool,
            }

            #[derive(serde::Deserialize)]
            struct RefreshRespData {
                access_token: String,
                refresh_token: Option<String>,
                access_expires_at: String,
                refresh_expires_at: Option<String>,
                user: Option<RefreshRespUser>,
            }

            loop {
                let (device_id, current_session) = {
                    let store = auth_store_arc.read().await;
                    (store.device_id.clone(), store.session.clone())
                };

                let Some(sess) = current_session else {
                    break;
                };
                let Some(_refresh_token) = sess.refresh_token.clone() else {
                    break;
                };

                if let Some(exp) = sess.refresh_expires_at_ms {
                    if exp <= chrono::Utc::now().timestamp_millis() {
                        break;
                    }
                }

                // Wait until refresh time
                let now_ms = chrono::Utc::now().timestamp_millis();
                let refresh_at_ms = (sess.access_expires_at_ms - REFRESH_BUFFER_MS).max(now_ms);
                let sleep_ms = (refresh_at_ms - now_ms).max(0) as u64;
                if sleep_ms > 0 {
                    tokio::time::sleep(tokio::time::Duration::from_millis(sleep_ms)).await;
                }

                // Re-check after sleep (session could have been refreshed elsewhere)
                let (device_id2, session2) = {
                    let store = auth_store_arc.read().await;
                    (store.device_id.clone(), store.session.clone())
                };
                let Some(sess2) = session2 else {
                    break;
                };
                let Some(refresh_token2) = sess2.refresh_token.clone() else {
                    break;
                };

                let now_ms2 = chrono::Utc::now().timestamp_millis();
                if sess2.access_expires_at_ms - REFRESH_BUFFER_MS > now_ms2 {
                    continue;
                }

                let url = format!("{}/api/v1/auth/refresh", AppState::get_api_base_url());
                // Важно: refresh не должен "висеть" бесконечно — иначе мы можем пропустить окно обновления
                // и получить 401 в hotkey/STT сценарии.
                let client = match reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(20))
                    .connect_timeout(std::time::Duration::from_secs(10))
                    .build()
                {
                    Ok(c) => c,
                    Err(e) => {
                        log::warn!("[auth-refresh] failed to build HTTP client: {}", e);
                        tokio::time::sleep(tokio::time::Duration::from_secs(
                            ERROR_RETRY_DELAY_SECS,
                        ))
                        .await;
                        continue;
                    }
                };
                let resp = client
                    .post(url)
                    .header("Content-Type", "application/json")
                    .header("X-Client-Type", "native")
                    .json(&RefreshReq {
                        refresh_token: refresh_token2.clone(),
                        device_id: device_id2.clone(),
                    })
                    .send()
                    .await;

                let resp = match resp {
                    Ok(r) => r,
                    Err(e) => {
                        log::warn!("[auth-refresh] network error: {}", e);
                        tokio::time::sleep(tokio::time::Duration::from_secs(
                            ERROR_RETRY_DELAY_SECS,
                        ))
                        .await;
                        continue;
                    }
                };

                if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
                    let now_ms = chrono::Utc::now().timestamp_millis();
                    let access_ttl_ms = sess2.access_expires_at_ms - now_ms;
                    let refresh_ttl_ms = sess2.refresh_expires_at_ms.map(|ms| ms - now_ms);

                    // Считываем ответ, чтобы логировать серверный код/сообщение (важно для диагностики).
                    let body_text = resp.text().await.unwrap_or_default();
                    let (server_code, server_msg) = (|| {
                        let v: serde_json::Value = serde_json::from_str(&body_text).ok()?;
                        // envelope: { error: { code, message } }
                        let err = v.get("error")?;
                        let code = err
                            .get("code")
                            .and_then(|x| x.as_str())
                            .map(|s| s.to_string());
                        let msg = err
                            .get("message")
                            .and_then(|x| x.as_str())
                            .map(|s| s.to_string());
                        Some((code, msg))
                    })()
                    .unwrap_or((None, None));

                    // Важно: на 401 возможна гонка с refresh-token rotation:
                    // другое окно/поток успел обновить refresh_token, но мы ещё не увидели запись.
                    // Делаем короткую паузу и сверяем "источник правды" ещё раз.
                    tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
                    let became_stale = {
                        let store = auth_store_arc.read().await;
                        let current_device_id = store.device_id.clone();
                        let current_refresh =
                            store.session.as_ref().and_then(|s| s.refresh_token.clone());
                        current_device_id != device_id2
                            || current_refresh != Some(refresh_token2.clone())
                    };

                    if became_stale {
                        log::debug!(
                            "[auth-refresh] 401 on stale session — store already changed (device_id={}, code={:?})",
                            device_id2,
                            server_code
                        );
                        // Без паузы можно уйти в tight-loop, если refresh_at_ms == now и store постоянно "дёргается".
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                        continue;
                    }

                    log::warn!(
                        "[auth-refresh] refresh rejected (401) — clearing session (device_id={}, access_ttl_ms={}, refresh_ttl_ms={:?}, code={:?}, msg={:?})",
                        device_id2,
                        access_ttl_ms,
                        refresh_ttl_ms,
                        server_code,
                        server_msg
                    );

                    // Clear session, keep device_id
                    let mut store = auth_store_arc.write().await;
                    store.session = None;
                    let _ = AuthStore::save(&store).await;
                    drop(store);

                    *is_authenticated_arc.write().await = false;

                    let rev_state = AppState::bump_revision(&auth_state_revision).await;
                    AppState::emit_invalidation(
                        &app_handle_for_task,
                        "auth-state",
                        rev_state,
                        None,
                    )
                    .await;

                    let rev_session = AppState::bump_revision(&auth_session_revision).await;
                    AppState::emit_invalidation(
                        &app_handle_for_task,
                        "auth-session",
                        rev_session,
                        None,
                    )
                    .await;

                    // Clear STT token
                    if let Some(state) = app_handle_for_task.try_state::<AppState>() {
                        state.apply_backend_auth_token_to_stt(None).await;
                    }

                    break;
                }

                if !resp.status().is_success() {
                    let status = resp.status();
                    let retry_delay_secs = if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                        resp.headers()
                            .get(reqwest::header::RETRY_AFTER)
                            .and_then(|v| v.to_str().ok())
                            .and_then(|s| s.parse::<u64>().ok())
                            .unwrap_or(RATE_LIMIT_RETRY_DELAY_SECS)
                            .clamp(ERROR_RETRY_DELAY_SECS, RATE_LIMIT_RETRY_DELAY_SECS)
                    } else {
                        ERROR_RETRY_DELAY_SECS
                    };
                    log::warn!(
                        "[auth-refresh] refresh failed: status={}, retry_in={}s",
                        status.as_u16(),
                        retry_delay_secs
                    );
                    tokio::time::sleep(tokio::time::Duration::from_secs(retry_delay_secs)).await;
                    continue;
                }

                let json: RefreshResp = match resp.json().await {
                    Ok(j) => j,
                    Err(e) => {
                        log::warn!("[auth-refresh] invalid JSON: {}", e);
                        tokio::time::sleep(tokio::time::Duration::from_secs(
                            ERROR_RETRY_DELAY_SECS,
                        ))
                        .await;
                        continue;
                    }
                };

                let access_expires_at_ms =
                    match AppState::parse_rfc3339_to_ms(&json.data.access_expires_at) {
                        Some(ms) => ms,
                        None => {
                            log::warn!(
                                "[auth-refresh] bad access_expires_at: {}",
                                json.data.access_expires_at
                            );
                            tokio::time::sleep(tokio::time::Duration::from_secs(
                                ERROR_RETRY_DELAY_SECS,
                            ))
                            .await;
                            continue;
                        }
                    };

                let refresh_expires_at_ms = json
                    .data
                    .refresh_expires_at
                    .as_deref()
                    .and_then(AppState::parse_rfc3339_to_ms);

                // Update store + persist
                {
                    let mut store = auth_store_arc.write().await;
                    store.session = Some(AuthSession {
                        access_token: json.data.access_token.clone(),
                        // Если сервер не вернул refresh_token, сохраняем актуальный токен
                        // из текущей сессии (refresh_token2).
                        refresh_token: json.data.refresh_token.clone().or(Some(refresh_token2)),
                        access_expires_at_ms,
                        refresh_expires_at_ms,
                        user: json.data.user.map(|u| AuthUser {
                            id: u.id,
                            email: u.email,
                            email_verified: u.email_verified,
                        }),
                    });
                    let _ = AuthStore::save(&store).await;
                }

                *is_authenticated_arc.write().await = true;

                // Update STT token best-effort
                if let Some(state) = app_handle_for_task.try_state::<AppState>() {
                    state
                        .apply_backend_auth_token_to_stt(Some(json.data.access_token))
                        .await;
                } else {
                    let _ = &service_for_task;
                }

                // Emit auth-session invalidation (auth-state stays the same)
                let rev_session = AppState::bump_revision(&auth_session_revision).await;
                AppState::emit_invalidation(
                    &app_handle_for_task,
                    "auth-session",
                    rev_session,
                    None,
                )
                .await;

                // Если локальные часы сильно ушли вперёд, access_expires_at может выглядеть
                // уже истёкшим сразу после успешного refresh. Минимальная пауза защищает backend
                // от tight-loop, а нормальный TTL всё равно будет досчитан на следующем круге.
                tokio::time::sleep(tokio::time::Duration::from_secs(
                    MIN_SUCCESS_REFRESH_INTERVAL_SECS,
                ))
                .await;

                // Continue loop (will schedule next refresh)
                let _ = device_id; // silence unused warning in some builds
            }
        });

        *self.auth_refresh_task.write().await = Some(task);
    }

    /// Запускает обработчик VAD timeout событий (вызывается из setup)
    /// Слушает channel и автоматически останавливает запись
    pub fn start_vad_timeout_handler(&self, app_handle: tauri::AppHandle) {
        let service = self.transcription_service.clone();
        let rx = self.vad_timeout_rx.clone();

        let handle = tauri::async_runtime::spawn(async move {
            let mut rx_guard = rx.lock().await;

            while let Some(timeout_session_id) = rx_guard.recv().await {
                log::info!(
                    "VAD silence timeout detected - auto-stopping recording (session_id={})",
                    timeout_session_id
                );

                let Some(state) = app_handle.try_state::<AppState>() else {
                    log::warn!("VAD timeout ignored - app state is unavailable");
                    continue;
                };
                let _lifecycle_guard = state.recording_lifecycle_guard.lock().await;
                let active_session_id = state.active_transcription_session_id.clone();
                let config = state.config.clone();
                let active_recording_mode = state.active_recording_mode.clone();

                let current_session_id = active_session_id.load(Ordering::Relaxed);
                if !is_current_vad_timeout_session(timeout_session_id, current_session_id) {
                    log::info!(
                        "VAD timeout ignored - stale session event: timeout_session_id={}, active_session_id={}",
                        timeout_session_id,
                        current_session_id
                    );
                    continue;
                }

                let manual_stop_only = config.read().await.keep_recording_until_manual_stop;
                if manual_stop_only {
                    log::info!("VAD timeout ignored - keep_recording_until_manual_stop is enabled");
                    continue;
                }

                // Проверяем что действительно идет запись
                let status = service.get_status().await;
                if status != crate::domain::RecordingStatus::Recording {
                    log::debug!("VAD timeout ignored - not recording (status: {:?})", status);
                    continue;
                }

                if let Err(active) =
                    claim_vad_timeout_session(active_session_id.as_ref(), timeout_session_id)
                {
                    log::info!(
                        "VAD timeout ignored - session changed before stop: timeout_session_id={}, active_session_id={}",
                        timeout_session_id,
                        active
                    );
                    continue;
                }

                // Останавливаем запись
                match service.stop_recording().await {
                    Ok(_) => {
                        log::info!("Recording stopped successfully by VAD timeout");

                        // Эмитим событие в UI
                        use tauri::Emitter;
                        *active_recording_mode.write().await = None;
                        let _ = app_handle.emit(
                            crate::presentation::events::EVENT_RECORDING_STATUS,
                            crate::presentation::RecordingStatusPayload {
                                session_id: timeout_session_id,
                                status: crate::domain::RecordingStatus::Idle,
                                stopped_via_hotkey: false,
                                mode: None,
                            },
                        );

                        // Также эмитим специальное событие VAD timeout (для информирования)
                        let _ = app_handle.emit("vad-silence-timeout", ());
                    }
                    Err(e) => {
                        log::error!("Failed to stop recording on VAD timeout: {}", e);
                        if service.get_status().await == crate::domain::RecordingStatus::Idle {
                            log::warn!(
                                "VAD stop failed after service recovered to Idle; emitting Idle status"
                            );
                            use tauri::Emitter;
                            *active_recording_mode.write().await = None;
                            let _ = app_handle.emit(
                                crate::presentation::events::EVENT_RECORDING_STATUS,
                                crate::presentation::RecordingStatusPayload {
                                    session_id: timeout_session_id,
                                    status: crate::domain::RecordingStatus::Idle,
                                    stopped_via_hotkey: false,
                                    mode: None,
                                },
                            );
                        } else {
                            restore_vad_timeout_session_claim_if_unclaimed(
                                active_session_id.as_ref(),
                                timeout_session_id,
                            );
                        }
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
    #[allow(dead_code)]
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

    pub async fn invalidate_audio_capture_device_cache(&self) {
        *self.active_audio_capture_device.write().await = None;
    }

    pub async fn ensure_audio_capture_device(
        &self,
        device_name: Option<String>,
        app_handle: tauri::AppHandle,
        force: bool,
    ) -> Result<(), String> {
        let normalized_device_name = normalize_audio_capture_device_name(device_name);
        let cached_device = self.active_audio_capture_device.read().await.clone();

        if !force && audio_capture_device_cache_matches(&cached_device, &normalized_device_name) {
            log::debug!(
                "Audio capture reuse: device unchanged ({:?})",
                normalized_device_name
            );
            return Ok(());
        }

        if force {
            log::info!(
                "Audio capture recreate forced for device: {:?}",
                normalized_device_name
            );
        } else {
            log::info!(
                "Audio capture recreate required: cached={:?}, requested={:?}",
                cached_device,
                normalized_device_name
            );
        }

        self.recreate_audio_capture_with_device(normalized_device_name, app_handle)
            .await
    }

    /// Пересоздает audio capture с новым устройством (применяет selected_audio_device)
    /// Можно вызывать при старте приложения и при смене устройства в настройках
    pub async fn recreate_audio_capture_with_device(
        &self,
        device_name: Option<String>,
        app_handle: tauri::AppHandle,
    ) -> Result<(), String> {
        let normalized_device_name = normalize_audio_capture_device_name(device_name);

        log::info!(
            "Recreating audio capture with device: {:?}",
            normalized_device_name
        );

        // Создаем новый SystemAudioCapture с выбранным устройством.
        // Если сохранённое имя устройства временно недоступно, автоматически
        // откатываемся на системный input по умолчанию, но не стираем выбор пользователя:
        // после переподключения следующий старт снова попробует выбранный микрофон.
        let mut effective_device_name = normalized_device_name.clone();
        let system_audio = match SystemAudioCapture::with_device(normalized_device_name.clone()) {
            Ok(capture) => capture,
            Err(AudioError::DeviceNotFound(e)) if normalized_device_name.is_some() => {
                log::warn!(
                    "Requested audio device is unavailable ({}). Falling back to default input device.",
                    e
                );
                effective_device_name = None;
                SystemAudioCapture::new().map_err(|fallback_err| {
                    format!(
                        "Failed to create audio capture with fallback to default input device: {}",
                        fallback_err
                    )
                })?
            }
            Err(e) => {
                return Err(format!(
                    "Failed to create audio capture with device {:?}: {}",
                    normalized_device_name, e
                ));
            }
        };

        // Получаем текущий VAD timeout из конфига
        let vad_timeout_ms = self.config.read().await.vad_silence_timeout_ms;

        // Создаем VAD processor
        let vad = VadProcessor::new(Some(vad_timeout_ms), None)
            .map_err(|e| format!("Failed to create VAD processor: {}", e))?;

        // Wrap system audio with VAD
        let mut vad_wrapper = VadCaptureWrapper::new_with_microphone_sensitivity(
            Box::new(system_audio),
            vad,
            self.transcription_service.microphone_sensitivity_source(),
        );

        // Используем общий VAD timeout sender, чтобы избежать гонок/дедлоков при смене устройства.
        // Receiver слушается единственным обработчиком, а при смене устройства меняется только callback.
        let vad_tx = self.vad_timeout_tx.clone();
        let active_session_id_for_vad = self.active_transcription_session_id.clone();
        vad_wrapper.set_silence_timeout_callback(Arc::new(move || {
            let session_id = active_session_id_for_vad.load(Ordering::Relaxed);
            log::info!(
                "VAD silence timeout triggered - sending notification (session_id={})",
                session_id
            );
            let _ = vad_tx.send(session_id);
        }));

        // Заменяем audio capture в TranscriptionService
        self.transcription_service
            .replace_audio_capture(Box::new(vad_wrapper))
            .await
            .map_err(|e| format!("Failed to replace audio capture: {}", e))?;

        *self.active_audio_capture_device.write().await = Some(effective_device_name.clone());

        // Handler перезапускать не нужно: receiver остаётся тем же.
        let _ = app_handle;

        log::info!(
            "Audio capture recreated successfully with device: {:?}",
            effective_device_name
        );
        Ok(())
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        audio_capture_device_cache_matches, claim_vad_timeout_session,
        is_current_vad_timeout_session, normalize_audio_capture_device_name,
        restore_vad_timeout_session_claim_if_unclaimed,
    };
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn audio_capture_device_cache_recreates_same_explicit_device_after_hotplug() {
        let cached = Some(Some("Studio Mic".to_string()));
        let requested = Some("Studio Mic".to_string());
        assert!(!audio_capture_device_cache_matches(&cached, &requested));
    }

    #[test]
    fn audio_capture_device_cache_recreates_default_device() {
        let cached = Some(None);
        let requested = None;
        assert!(!audio_capture_device_cache_matches(&cached, &requested));
    }

    #[test]
    fn audio_capture_device_cache_recreates_unknown_or_changed_device() {
        assert!(!audio_capture_device_cache_matches(
            &None,
            &Some("Studio Mic".to_string())
        ));
        assert!(!audio_capture_device_cache_matches(
            &Some(Some("Old Mic".to_string())),
            &Some("New Mic".to_string())
        ));
        assert!(!audio_capture_device_cache_matches(
            &Some(Some("Old Mic".to_string())),
            &None
        ));
    }

    #[test]
    fn normalize_audio_capture_device_name_treats_blank_as_default() {
        assert_eq!(normalize_audio_capture_device_name(None), None);
        assert_eq!(
            normalize_audio_capture_device_name(Some("  Studio Mic  ".to_string())),
            Some("Studio Mic".to_string())
        );
        assert_eq!(
            normalize_audio_capture_device_name(Some("   ".to_string())),
            None
        );
    }

    #[test]
    fn vad_timeout_session_match_rejects_zero_and_stale_events() {
        assert!(!is_current_vad_timeout_session(0, 1));
        assert!(!is_current_vad_timeout_session(1, 0));
        assert!(!is_current_vad_timeout_session(1, 2));
        assert!(is_current_vad_timeout_session(2, 2));
    }

    #[test]
    fn vad_timeout_session_claim_clears_only_current_session() {
        let active_session_id = AtomicU64::new(7);

        assert_eq!(claim_vad_timeout_session(&active_session_id, 6), Err(7));
        assert_eq!(active_session_id.load(Ordering::Relaxed), 7);

        assert_eq!(claim_vad_timeout_session(&active_session_id, 0), Err(7));
        assert_eq!(active_session_id.load(Ordering::Relaxed), 7);

        assert_eq!(claim_vad_timeout_session(&active_session_id, 7), Ok(()));
        assert_eq!(active_session_id.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn vad_timeout_session_restore_does_not_overwrite_new_session() {
        let active_session_id = AtomicU64::new(0);
        restore_vad_timeout_session_claim_if_unclaimed(&active_session_id, 7);
        assert_eq!(active_session_id.load(Ordering::Relaxed), 7);

        active_session_id.store(9, Ordering::Relaxed);
        restore_vad_timeout_session_claim_if_unclaimed(&active_session_id, 7);
        assert_eq!(active_session_id.load(Ordering::Relaxed), 9);
    }
}
