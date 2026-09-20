pub(crate) mod effective_capture;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::RwLock;

#[cfg(not(all(debug_assertions, feature = "webdriver-e2e")))]
use crate::application::services::IncomingSpokenTranslationPorts;
use crate::application::services::{
    IncomingTranslationFacade, IncomingTranslationFacadeFactory, LiveTranslationPorts,
    LiveTranslationService,
};
use crate::application::TranscriptionService;
use crate::domain::{
    AppConfig, AudioCapture, AudioCaptureIdentity, AudioError, RecordingMode, UiPreferences,
};
#[cfg(not(all(debug_assertions, feature = "webdriver-e2e")))]
use crate::infrastructure::audio::{
    DefaultLocalPlaybackOutputFactory, DefaultSpokenTranslationCapability,
};
use crate::infrastructure::{
    audio::{DefaultPlatformAudioFactory, SystemAudioCapture, VadCaptureWrapper, VadProcessor},
    auto_paste::AutoPasteTarget,
    openai::OpenAIRealtimeTranslationFactory,
    AuthSession, AuthStore, AuthStoreData, AuthUser, ConfigStore, DefaultSttProviderFactory,
};

const RECORDING_WINDOW_POSITION_SAVE_SUPPRESSION_MS: i64 = 800;
const TRANSLATION_APP_EXIT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(4_500);

#[derive(Clone, Copy)]
pub(crate) enum WarmInputSuspension {
    Sleep = 1,
    Takeover = 2,
    Auth = 4,
    Shutdown = 8,
    Policy = 16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordingIntentCoordinatorMode {
    Legacy,
    Desired,
}

impl RecordingIntentCoordinatorMode {
    fn from_startup_env() -> Self {
        let value = std::env::var("VOICETEXT_HOTKEY_INTENT_COORDINATOR").ok();
        match value.as_deref() {
            Some(value) => {
                let parsed = Self::parse(value);
                if parsed.is_none() {
                    log::warn!(
                        "Unknown VOICETEXT_HOTKEY_INTENT_COORDINATOR={value}; using legacy rollback path"
                    );
                }
                parsed.unwrap_or(Self::Legacy)
            }
            None => Self::Desired,
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "legacy" => Some(Self::Legacy),
            "desired" => Some(Self::Desired),
            _ => None,
        }
    }
}

fn default_incoming_translation_factory() -> IncomingTranslationFacadeFactory {
    let audio_factory = Arc::new(DefaultPlatformAudioFactory::new());
    #[cfg(all(debug_assertions, feature = "webdriver-e2e"))]
    {
        IncomingTranslationFacadeFactory::new(
            Arc::new(DefaultSttProviderFactory::new()),
            audio_factory,
            crate::presentation::e2e_translation::spoken_translation_ports(),
        )
    }
    #[cfg(not(all(debug_assertions, feature = "webdriver-e2e")))]
    {
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
}

fn default_live_translation_ports() -> LiveTranslationPorts {
    LiveTranslationPorts::new(
        Arc::new(DefaultPlatformAudioFactory::new()),
        Arc::new(OpenAIRealtimeTranslationFactory),
    )
}

pub(super) fn recording_intent_policy(
    config: &AppConfig,
    version: u64,
) -> super::recording_intent_coordinator::RuntimePolicySnapshot {
    super::recording_intent_coordinator::RuntimePolicySnapshot {
        version,
        capture_mode: match config.recording_mode {
            RecordingMode::Dictation => super::recording_intent_coordinator::CaptureMode::Dictation,
            RecordingMode::LiveTranslation => {
                super::recording_intent_coordinator::CaptureMode::LiveTranslation
            }
        },
        recording_mode: if config.hold_to_record {
            super::recording_intent_coordinator::RecordingMode::Hold
        } else {
            super::recording_intent_coordinator::RecordingMode::Toggle
        },
        show_panel_on_start: true,
        show_mini_panel: config.show_mini_recording_window,
        hide_panel_on_hotkey_stop: config.hide_recording_window_on_hotkey,
        hide_panel_on_force_off: true,
        max_stop_attempts: 3,
        max_finalize_attempts: 2,
    }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VadTimeoutEvent {
    pub capture_identity: Option<AudioCaptureIdentity>,
    pub legacy_session_id: u64,
}

fn is_current_vad_capture_identity(
    identity: AudioCaptureIdentity,
    token_is_current: bool,
    active_run_id: Option<u64>,
    buffering_generation: Option<u64>,
) -> bool {
    token_is_current
        && active_run_id == Some(identity.run_id)
        && buffering_generation.map_or(true, |generation| generation == identity.generation)
}

#[derive(Debug)]
struct DeferredVadTimeoutFence {
    identity: AudioCaptureIdentity,
    terminal: bool,
}

impl DeferredVadTimeoutFence {
    fn new(identity: AudioCaptureIdentity) -> Self {
        Self {
            identity,
            terminal: false,
        }
    }

    fn observe(
        &mut self,
        token_is_current: bool,
        active_run_id: Option<u64>,
        buffering_generation: Option<u64>,
        stop_already_started: bool,
        coordinator_recording: bool,
        service_status: crate::domain::RecordingStatus,
    ) -> Option<AudioCaptureIdentity> {
        if self.terminal {
            return None;
        }
        if stop_already_started
            || !is_current_vad_capture_identity(
                self.identity,
                token_is_current,
                active_run_id,
                buffering_generation,
            )
        {
            self.terminal = true;
            return None;
        }
        if coordinator_recording && service_status == crate::domain::RecordingStatus::Recording {
            self.terminal = true;
            return Some(self.identity);
        }
        None
    }

    fn is_terminal(&self) -> bool {
        self.terminal
    }
}

/// Global application state managed by Tauri
///
/// This state is shared across all Tauri commands and can be accessed
/// using State<AppState> parameter in command functions
pub struct AppState {
    /// Main transcription service
    pub transcription_service: Arc<TranscriptionService>,

    /// Shared bounded native executor for negotiated continuation validation and delivery.
    pub continuation_context:
        Arc<crate::infrastructure::continuation_context::ContinuationContextManager>,

    /// Native-only accepted logical delivery membership, retained boundedly for late finals.
    pub continuation_delivery_runs:
        Arc<std::sync::Mutex<crate::domain::ContinuationDeliveryOwnership>>,

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
    pub history: Arc<RwLock<crate::domain::TranscriptionHistory>>,

    /// Latest partial transcription
    pub partial_transcription: Arc<RwLock<Option<String>>>,

    /// Latest final transcription
    pub final_transcription: Arc<RwLock<Option<String>>>,

    /// Microphone test state
    pub microphone_test: Arc<RwLock<MicrophoneTestState>>,

    /// Receiver для VAD silence timeout событий
    /// Используется в setup для установки обработчика
    pub vad_timeout_tx: tokio::sync::mpsc::UnboundedSender<VadTimeoutEvent>,
    pub vad_timeout_rx:
        Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<VadTimeoutEvent>>>,

    /// VAD timeout handler task (для перезапуска при смене устройства)
    vad_handler_task: Arc<RwLock<Option<tauri::async_runtime::JoinHandle<()>>>>,

    /// Последнее активное приложение (перед показом VoicetextAI окна)
    /// Используется только как fallback для legacy/non-recording window flows.
    pub last_focused_app_target: Arc<std::sync::Mutex<Option<AutoPasteTarget>>>,

    /// Immutable per-session paste destinations. A newer recording must not
    /// redirect delayed text that still belongs to an older session.
    pub auto_paste_targets_by_session: Arc<std::sync::Mutex<BTreeMap<u64, AutoPasteTarget>>>,

    /// A restart can show the panel while the previous session is still
    /// finalizing and before the coordinator allocates the next run id.
    pub pending_auto_paste_targets_by_revision:
        Arc<std::sync::Mutex<BTreeMap<u64, AutoPasteTarget>>>,

    /// Флаг авторизации пользователя (синхронизируется из frontend)
    /// Используется для определения какое окно показывать при нажатии hotkey
    pub is_authenticated: Arc<RwLock<bool>>,

    /// Callback-safe mirror for synchronous global-hotkey handlers.
    pub is_authenticated_runtime: Arc<AtomicBool>,

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
    pub recording_start_pending_after_stop:
        super::recording_window_lifecycle::PendingRecordingStart,

    pub recording_hotkey_intents: super::recording_window_lifecycle::RecordingHotkeyIntents,

    /// Startup-only rollout selector. It is intentionally immutable so one process
    /// cannot split input sources between two lifecycle owners.
    pub recording_intent_coordinator_mode: RecordingIntentCoordinatorMode,

    /// Pure reducer state. This synchronous mutex is held only for `reduce`; async
    /// audio, provider, filesystem, and AppKit effects run after it is released.
    pub recording_intent_coordinator:
        Arc<std::sync::Mutex<super::recording_intent_coordinator::CoordinatorState>>,

    /// Physical input identity is independent from recording lifecycle timing.
    pub recording_hotkey_gestures:
        Arc<std::sync::Mutex<super::recording_hotkey_gestures::RecordingHotkeyGestureNormalizer>>,

    /// Exact AppConfig snapshots referenced by `RunContext.policy.version`.
    /// Effects read these synchronously before doing async work, so an in-flight
    /// start cannot silently adopt settings changed by a newer intent.
    pub recording_intent_policy_snapshots: Arc<std::sync::Mutex<BTreeMap<u64, AppConfig>>>,

    /// Transcript delivery is independently queued from provider stop. A run is
    /// finalized only after its barrier reaches the queue consumer.
    pub transcript_delivery_barriers:
        Arc<std::sync::Mutex<BTreeMap<u64, super::commands::TranscriptDeliveryBarrierPort>>>,

    pub prepared_capture_tokens:
        Arc<std::sync::Mutex<BTreeMap<u64, crate::application::PreparedCaptureToken>>>,

    pub recording_capture_readiness_generation: AtomicU64,
    pub recording_capture_readiness:
        Arc<std::sync::Mutex<crate::presentation::events::RecordingCaptureReadinessPayload>>,

    /// `notify_one` retains a permit if shutdown reaches Idle before the waiter
    /// is registered.
    pub recording_shutdown_ready: Arc<tokio::sync::Notify>,

    /// Accepted On intent timestamp keyed by revision. Used only for one
    /// aggregate accepted-to-panel-applied latency record.
    pub recording_panel_intent_started_at: Arc<std::sync::Mutex<BTreeMap<u64, std::time::Instant>>>,

    /// Cooperative cancellation token per start effect. Providers without a
    /// cancellation API still report late success for run-scoped cleanup.
    pub recording_start_cancellations: Arc<std::sync::Mutex<BTreeMap<u64, Arc<AtomicBool>>>>,

    /// Callback-safe mirror; the async config lock is never acquired in an OS callback.
    pub hold_to_record_runtime: AtomicBool,

    pub recording_window_lifecycle:
        Arc<super::recording_window_lifecycle::RecordingWindowLifecycle>,

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
    warm_input_suspended: AtomicU8,
    warm_lifecycle_revision: AtomicU64,
    warm_lifecycle_guard: std::sync::Mutex<()>,
    warm_owner_creation_guard: tokio::sync::Mutex<()>,
    #[cfg(target_os = "macos")]
    pub(crate) warm_dictation_input: std::sync::Mutex<
        Option<(
            Option<String>,
            Arc<crate::infrastructure::audio::WarmDictationInput>,
        )>,
    >,

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
        #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
        return Self::native_e2e();

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
                let app_config = AppConfig::default();
                let coordinator_policy = recording_intent_policy(&app_config, 0);

                let continuation_context = Arc::new(
                    crate::infrastructure::continuation_context::ContinuationContextManager::new(),
                );
                service.set_continuation_context_guard(continuation_context.clone());
                return Self {
                    continuation_context,
                    continuation_delivery_runs: Arc::new(std::sync::Mutex::new(crate::domain::ContinuationDeliveryOwnership::default())),
                    transcription_service: service,
                    config: Arc::new(RwLock::new(app_config.clone())),
                    app_config_revision: Arc::new(RwLock::new(0)),
                    stt_config_revision: Arc::new(RwLock::new(0)),
                    auth_state_revision: Arc::new(RwLock::new(0)),
                    ui_preferences_revision: Arc::new(RwLock::new(0)),
                    ui_preferences: Arc::new(RwLock::new(UiPreferences::default())),
                    history: Arc::new(RwLock::new(crate::domain::TranscriptionHistory::default())),
                    partial_transcription: Arc::new(RwLock::new(None)),
                    final_transcription: Arc::new(RwLock::new(None)),
                    microphone_test: Arc::new(RwLock::new(MicrophoneTestState::default())),
                    vad_timeout_tx: vad_tx,
                    vad_timeout_rx: Arc::new(tokio::sync::Mutex::new(vad_rx)),
                    vad_handler_task: Arc::new(RwLock::new(None)),
                    last_focused_app_target: Arc::new(std::sync::Mutex::new(None)),
                    auto_paste_targets_by_session: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
                    pending_auto_paste_targets_by_revision: Arc::new(std::sync::Mutex::new(
                        BTreeMap::new(),
                    )),
                    is_authenticated: Arc::new(RwLock::new(false)),
                    is_authenticated_runtime: Arc::new(AtomicBool::new(false)),
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
                    recording_start_pending_after_stop: Default::default(),
                    recording_window_lifecycle: Arc::new(Default::default()),
                    recording_hotkey_intents: Default::default(),
                    recording_intent_coordinator_mode:
                        RecordingIntentCoordinatorMode::from_startup_env(),
                    recording_intent_coordinator: Arc::new(std::sync::Mutex::new(
                        super::recording_intent_coordinator::CoordinatorState::new(
                            coordinator_policy,
                        ),
                    )),
                    recording_hotkey_gestures: Arc::new(std::sync::Mutex::new(Default::default())),
                    recording_intent_policy_snapshots: Arc::new(std::sync::Mutex::new(
                        BTreeMap::from([(0, app_config)]),
                    )),
                    transcript_delivery_barriers: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
                    prepared_capture_tokens: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
                    recording_capture_readiness_generation: AtomicU64::new(0),
                    recording_capture_readiness: Arc::new(std::sync::Mutex::new(
                        crate::presentation::events::RecordingCaptureReadinessPayload {
                            capture_generation: None, logical_run_id: None, capture_episode_id: None, capture_ready: None, transport_ready: None,
                            generation: 0,
                            run_id: None,
                            revision: None,
                            state: crate::presentation::events::RecordingCaptureReadinessState::Unavailable,
                            reason: crate::presentation::events::RecordingCaptureReadinessReason::Idle,
                        },
                    )),
                    recording_shutdown_ready: Arc::new(tokio::sync::Notify::new()),
                    recording_panel_intent_started_at: Arc::new(std::sync::Mutex::new(
                        BTreeMap::new(),
                    )),
                    recording_start_cancellations: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
                    hold_to_record_runtime: AtomicBool::new(false),
                    recording_hotkey_toggle_guard: Arc::new(tokio::sync::Mutex::new(())),
                    recording_lifecycle_guard: Arc::new(tokio::sync::Mutex::new(())),
                    audio_start_guard: Arc::new(tokio::sync::Mutex::new(())),
                    incoming_translation_lifecycle_guard: Arc::new(tokio::sync::Mutex::new(())),
                    recording_hotkey_registration_guard: Arc::new(tokio::sync::Mutex::new(())),
                    double_space_hotkey_enabled_runtime: AtomicBool::new(false),
                    double_space_hotkey_listener_started: AtomicBool::new(false),
                    active_audio_capture_device: Arc::new(RwLock::new(None)),
                    warm_input_suspended: AtomicU8::new(0),
                    warm_lifecycle_revision: AtomicU64::new(0),
                    warm_lifecycle_guard: std::sync::Mutex::new(()),
                    warm_owner_creation_guard: tokio::sync::Mutex::new(()),
                    #[cfg(target_os = "macos")]
                    warm_dictation_input: std::sync::Mutex::new(None),
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
                let hold_to_record_runtime = app_config.hold_to_record;
                let coordinator_policy = recording_intent_policy(&app_config, 0);

                let continuation_context = Arc::new(
                    crate::infrastructure::continuation_context::ContinuationContextManager::new(),
                );
                service.set_continuation_context_guard(continuation_context.clone());
                return Self {
                    continuation_context,
                    continuation_delivery_runs: Arc::new(std::sync::Mutex::new(crate::domain::ContinuationDeliveryOwnership::default())),
                    transcription_service: service,
                    config: Arc::new(RwLock::new(app_config.clone())),
                    app_config_revision: Arc::new(RwLock::new(0)),
                    stt_config_revision: Arc::new(RwLock::new(0)),
                    auth_state_revision: Arc::new(RwLock::new(0)),
                    ui_preferences_revision: Arc::new(RwLock::new(0)),
                    ui_preferences: Arc::new(RwLock::new(UiPreferences::default())),
                    history: Arc::new(RwLock::new(crate::domain::TranscriptionHistory::default())),
                    partial_transcription: Arc::new(RwLock::new(None)),
                    final_transcription: Arc::new(RwLock::new(None)),
                    microphone_test: Arc::new(RwLock::new(MicrophoneTestState::default())),
                    vad_timeout_tx: vad_tx,
                    vad_timeout_rx: Arc::new(tokio::sync::Mutex::new(vad_rx)),
                    vad_handler_task: Arc::new(RwLock::new(None)),
                    last_focused_app_target: Arc::new(std::sync::Mutex::new(None)),
                    auto_paste_targets_by_session: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
                    pending_auto_paste_targets_by_revision: Arc::new(std::sync::Mutex::new(
                        BTreeMap::new(),
                    )),
                    is_authenticated: Arc::new(RwLock::new(false)),
                    is_authenticated_runtime: Arc::new(AtomicBool::new(false)),
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
                    recording_start_pending_after_stop: Default::default(),
                    recording_window_lifecycle: Arc::new(Default::default()),
                    recording_hotkey_intents: Default::default(),
                    recording_intent_coordinator_mode:
                        RecordingIntentCoordinatorMode::from_startup_env(),
                    recording_intent_coordinator: Arc::new(std::sync::Mutex::new(
                        super::recording_intent_coordinator::CoordinatorState::new(
                            coordinator_policy,
                        ),
                    )),
                    recording_hotkey_gestures: Arc::new(std::sync::Mutex::new(Default::default())),
                    recording_intent_policy_snapshots: Arc::new(std::sync::Mutex::new(
                        BTreeMap::from([(0, app_config)]),
                    )),
                    transcript_delivery_barriers: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
                    prepared_capture_tokens: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
                    recording_capture_readiness_generation: AtomicU64::new(0),
                    recording_capture_readiness: Arc::new(std::sync::Mutex::new(
                        crate::presentation::events::RecordingCaptureReadinessPayload {
                            capture_generation: None, logical_run_id: None, capture_episode_id: None, capture_ready: None, transport_ready: None,
                            generation: 0,
                            run_id: None,
                            revision: None,
                            state: crate::presentation::events::RecordingCaptureReadinessState::Unavailable,
                            reason: crate::presentation::events::RecordingCaptureReadinessReason::Idle,
                        },
                    )),
                    recording_shutdown_ready: Arc::new(tokio::sync::Notify::new()),
                    recording_panel_intent_started_at: Arc::new(std::sync::Mutex::new(
                        BTreeMap::new(),
                    )),
                    recording_start_cancellations: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
                    hold_to_record_runtime: AtomicBool::new(hold_to_record_runtime),
                    recording_hotkey_toggle_guard: Arc::new(tokio::sync::Mutex::new(())),
                    recording_lifecycle_guard: Arc::new(tokio::sync::Mutex::new(())),
                    audio_start_guard: Arc::new(tokio::sync::Mutex::new(())),
                    incoming_translation_lifecycle_guard: Arc::new(tokio::sync::Mutex::new(())),
                    recording_hotkey_registration_guard: Arc::new(tokio::sync::Mutex::new(())),
                    double_space_hotkey_enabled_runtime: AtomicBool::new(false),
                    double_space_hotkey_listener_started: AtomicBool::new(false),
                    active_audio_capture_device: Arc::new(RwLock::new(Some(None))),
                    warm_input_suspended: AtomicU8::new(0),
                    warm_lifecycle_revision: AtomicU64::new(0),
                    warm_lifecycle_guard: std::sync::Mutex::new(()),
                    warm_owner_creation_guard: tokio::sync::Mutex::new(()),
                    #[cfg(target_os = "macos")]
                    warm_dictation_input: std::sync::Mutex::new(None),
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

        let effective_capture_identity =
            Arc::new(std::sync::Mutex::new(system_audio.device_name()));
        let system_audio = effective_capture::EffectiveCapture::new(
            system_audio,
            effective_capture_identity.clone(),
            SystemAudioCapture::device_name,
        );

        // Wrap system audio with VAD
        let mut vad_wrapper = VadCaptureWrapper::new_with_microphone_sensitivity(
            Box::new(system_audio),
            vad,
            microphone_sensitivity.clone(),
        );

        // Устанавливаем callback который отправляет событие в channel
        let vad_tx_for_cb = vad_tx.clone();
        let active_session_id_for_vad = active_transcription_session_id.clone();
        vad_wrapper.set_identified_silence_timeout_callback(Arc::new(move |capture_identity| {
            let legacy_session_id = capture_identity
                .is_none()
                .then(|| active_session_id_for_vad.load(Ordering::Relaxed))
                .unwrap_or(0);
            log::info!(
                "VAD silence timeout triggered - sending notification (capture_identity={:?}, legacy_session_id={})",
                capture_identity,
                legacy_session_id
            );
            let _ = vad_tx_for_cb.send(VadTimeoutEvent {
                capture_identity,
                legacy_session_id,
            });
        }));

        let audio_capture = Box::new(vad_wrapper);
        let stt_factory = Arc::new(DefaultSttProviderFactory::new());

        let transcription_service = Arc::new(
            TranscriptionService::new_with_microphone_sensitivity_and_device(
                audio_capture,
                stt_factory,
                microphone_sensitivity,
                effective_capture_identity,
            ),
        );

        log::info!(
            "AppState initialized with SystemAudioCapture + VAD (timeout: {}ms)",
            app_config.vad_silence_timeout_ms
        );

        Self::from_recording_ports(
            transcription_service,
            app_config,
            vad_tx,
            vad_rx,
            active_transcription_session_id,
        )
    }

    fn from_recording_ports(
        transcription_service: Arc<TranscriptionService>,
        app_config: AppConfig,
        vad_tx: tokio::sync::mpsc::UnboundedSender<VadTimeoutEvent>,
        vad_rx: tokio::sync::mpsc::UnboundedReceiver<VadTimeoutEvent>,
        active_transcription_session_id: Arc<AtomicU64>,
    ) -> Self {
        let hold_to_record_runtime = app_config.hold_to_record;
        let coordinator_policy = recording_intent_policy(&app_config, 0);
        let continuation_context = Arc::new(
            crate::infrastructure::continuation_context::ContinuationContextManager::new(),
        );
        transcription_service.set_continuation_context_guard(continuation_context.clone());
        Self {
            continuation_context,
            continuation_delivery_runs: Arc::new(std::sync::Mutex::new(
                crate::domain::ContinuationDeliveryOwnership::default(),
            )),
            transcription_service,
            config: Arc::new(RwLock::new(app_config.clone())),
            app_config_revision: Arc::new(RwLock::new(0)),
            stt_config_revision: Arc::new(RwLock::new(0)),
            auth_state_revision: Arc::new(RwLock::new(0)),
            ui_preferences_revision: Arc::new(RwLock::new(0)),
            ui_preferences: Arc::new(RwLock::new(UiPreferences::default())),
            history: Arc::new(RwLock::new(crate::domain::TranscriptionHistory::default())),
            partial_transcription: Arc::new(RwLock::new(None)),
            final_transcription: Arc::new(RwLock::new(None)),
            microphone_test: Arc::new(RwLock::new(MicrophoneTestState::default())),
            vad_timeout_tx: vad_tx,
            vad_timeout_rx: Arc::new(tokio::sync::Mutex::new(vad_rx)),
            vad_handler_task: Arc::new(RwLock::new(None)),
            last_focused_app_target: Arc::new(std::sync::Mutex::new(None)),
            auto_paste_targets_by_session: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
            pending_auto_paste_targets_by_revision: Arc::new(
                std::sync::Mutex::new(BTreeMap::new()),
            ),
            is_authenticated: Arc::new(RwLock::new(false)),
            is_authenticated_runtime: Arc::new(AtomicBool::new(false)),
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
            recording_start_pending_after_stop: Default::default(),
            recording_window_lifecycle: Arc::new(Default::default()),
            recording_hotkey_intents: Default::default(),
            recording_intent_coordinator_mode: RecordingIntentCoordinatorMode::from_startup_env(),
            recording_intent_coordinator: Arc::new(std::sync::Mutex::new(
                super::recording_intent_coordinator::CoordinatorState::new(coordinator_policy),
            )),
            recording_hotkey_gestures: Arc::new(std::sync::Mutex::new(Default::default())),
            recording_intent_policy_snapshots: Arc::new(std::sync::Mutex::new(BTreeMap::from([(
                0, app_config,
            )]))),
            transcript_delivery_barriers: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
            prepared_capture_tokens: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
            recording_capture_readiness_generation: AtomicU64::new(0),
            recording_capture_readiness: Arc::new(std::sync::Mutex::new(
                crate::presentation::events::RecordingCaptureReadinessPayload {
                    capture_generation: None,
                    logical_run_id: None,
                    capture_episode_id: None,
                    capture_ready: None,
                    transport_ready: None,
                    generation: 0,
                    run_id: None,
                    revision: None,
                    state: crate::presentation::events::RecordingCaptureReadinessState::Unavailable,
                    reason: crate::presentation::events::RecordingCaptureReadinessReason::Idle,
                },
            )),
            recording_shutdown_ready: Arc::new(tokio::sync::Notify::new()),
            recording_panel_intent_started_at: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
            recording_start_cancellations: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
            hold_to_record_runtime: AtomicBool::new(hold_to_record_runtime),
            recording_hotkey_toggle_guard: Arc::new(tokio::sync::Mutex::new(())),
            recording_lifecycle_guard: Arc::new(tokio::sync::Mutex::new(())),
            audio_start_guard: Arc::new(tokio::sync::Mutex::new(())),
            incoming_translation_lifecycle_guard: Arc::new(tokio::sync::Mutex::new(())),
            recording_hotkey_registration_guard: Arc::new(tokio::sync::Mutex::new(())),
            double_space_hotkey_enabled_runtime: AtomicBool::new(false),
            double_space_hotkey_listener_started: AtomicBool::new(false),
            active_audio_capture_device: Arc::new(RwLock::new(Some(None))),
            warm_input_suspended: AtomicU8::new(0),
            warm_lifecycle_revision: AtomicU64::new(0),
            warm_lifecycle_guard: std::sync::Mutex::new(()),
            warm_owner_creation_guard: tokio::sync::Mutex::new(()),
            #[cfg(target_os = "macos")]
            warm_dictation_input: std::sync::Mutex::new(None),
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

    #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
    fn native_e2e() -> Self {
        let fixture = super::native_e2e::fixture();
        let service = Arc::new(TranscriptionService::new(
            Box::new(super::native_e2e::FixtureCapture::new(fixture.clone())),
            if super::native_e2e::live_mode() {
                Arc::new(crate::infrastructure::factory::DefaultSttProviderFactory::new())
            } else {
                Arc::new(super::native_e2e::FixtureFactory(fixture))
            },
        ));
        let mut config = AppConfig {
            auto_copy_to_clipboard: false,
            auto_paste_text: false,
            show_mini_recording_window: false,
            keep_recording_until_manual_stop: true,
            ..AppConfig::default()
        };
        if super::native_e2e::live_mode() {
            config.stt = crate::domain::SttConfig::new(crate::domain::SttProviderType::Backend);
            config.stt.backend_streaming_provider =
                crate::domain::BackendStreamingProvider::ElevenLabs;
            config.stt.backend_url = Some("ws://127.0.0.1:51866".into());
            config.stt.backend_auth_token = Some("dev-local-token".into());
            config.stt.language = "ru".into();
            config.stt.keep_connection_alive = false;
        }
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Self::from_recording_ports(service, config, tx, rx, Arc::new(AtomicU64::new(0)))
    }

    pub(crate) async fn set_authenticated(&self, authenticated: bool) {
        let mut guarded = self.is_authenticated.write().await;
        *guarded = authenticated;
        self.is_authenticated_runtime
            .store(authenticated, Ordering::Release);
        drop(guarded);
        if !authenticated {
            self.suspend_warm_input(WarmInputSuspension::Auth);
            if let Err(error) = self.close_warm_input().await {
                log::error!("Auth loss warm input close failed: {error}");
            }
        } else if let Err(error) = self
            .resume_warm_input_if_allowed(WarmInputSuspension::Auth)
            .await
        {
            log::warn!("Authenticated warm input prewarm failed: {error}");
        }
    }

    pub async fn shutdown_translation_runtimes(&self) {
        if !claim_translation_shutdown(&self.translation_shutdown_started) {
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

    pub(crate) async fn apply_backend_auth_token_to_stt(
        &self,
        app_handle: &AppHandle,
        token: Option<String>,
    ) {
        let _guard = self.stt_config_guard.lock().await;

        // Best-effort: ошибки не должны блокировать UX, но они важны для диагностики.
        // Важно: берём текущий in-memory config, чтобы не "сбрасывать" keep-alive и другие поля
        // при конкурирующих disk-write сценариях.
        let mut config = self.transcription_service.get_config().await;
        let policy_changed =
            crate::application::apply_backend_dictation_keep_alive_policy(&mut config);
        if config.backend_auth_token == token && !policy_changed {
            return;
        }
        config.backend_auth_token = token;
        self.config.write().await.stt = config.clone();
        if let Err(e) = ConfigStore::save_config(&config).await {
            log::warn!("Failed to persist STT config token: {}", e);
        }
        if let Err(e) = self.transcription_service.update_config(config).await {
            log::warn!("Failed to update transcription service config token: {}", e);
        }
        let snapshot = self.config.read().await.clone();
        let version = Self::bump_revision(&self.app_config_revision)
            .await
            .parse::<u64>()
            .unwrap_or(0);
        super::commands::sync_recording_intent_runtime(app_handle.clone(), &snapshot, version);
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
        #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
        return;
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
        let is_authenticated_runtime = self.is_authenticated_runtime.clone();
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

                    let mut authenticated = is_authenticated_arc.write().await;
                    *authenticated = false;
                    is_authenticated_runtime.store(false, Ordering::Release);
                    drop(authenticated);

                    if let Some(state) = app_handle_for_task.try_state::<AppState>() {
                        state.set_authenticated(false).await;
                    }

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
                        state
                            .apply_backend_auth_token_to_stt(&app_handle_for_task, None)
                            .await;
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

                let mut authenticated = is_authenticated_arc.write().await;
                *authenticated = true;
                is_authenticated_runtime.store(true, Ordering::Release);
                drop(authenticated);

                // Update STT token best-effort
                if let Some(state) = app_handle_for_task.try_state::<AppState>() {
                    state
                        .apply_backend_auth_token_to_stt(
                            &app_handle_for_task,
                            Some(json.data.access_token),
                        )
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

            while let Some(timeout_event) = rx_guard.recv().await {
                log::info!(
                    "VAD silence timeout detected (capture_identity={:?}, legacy_session_id={})",
                    timeout_event.capture_identity,
                    timeout_event.legacy_session_id
                );

                let Some(state) = app_handle.try_state::<AppState>() else {
                    log::warn!("VAD timeout ignored - app state is unavailable");
                    continue;
                };
                if state.recording_intent_coordinator_mode
                    == RecordingIntentCoordinatorMode::Desired
                {
                    let Some(identity) = timeout_event.capture_identity else {
                        log::warn!("VAD timeout ignored - desired capture has no run identity");
                        continue;
                    };
                    let token = crate::application::PreparedCaptureToken {
                        run_id: identity.run_id,
                        generation: identity.generation,
                    };
                    let policy_version = {
                        let coordinator = state
                            .recording_intent_coordinator
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        let Some(run) = coordinator.capture.run() else {
                            log::info!("VAD timeout ignored - capture is already idle");
                            continue;
                        };
                        let buffering_generation = match coordinator.capture {
                            super::recording_intent_coordinator::CaptureState::Buffering {
                                generation,
                                ..
                            } => Some(generation),
                            _ => None,
                        };
                        if !is_current_vad_capture_identity(
                            identity,
                            service.capture_token_is_current(token),
                            Some(run.run_id.get()),
                            buffering_generation,
                        ) {
                            log::info!(
                                "VAD timeout ignored - coordinator owns a different capture (identity={:?}, active_run={})",
                                identity,
                                run.run_id.get()
                            );
                            continue;
                        }
                        run.policy.version
                    };
                    let frozen_manual_stop_only = state
                        .recording_intent_policy_snapshots
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .get(&policy_version)
                        .map(|config| config.keep_recording_until_manual_stop);
                    let Some(frozen_manual_stop_only) = frozen_manual_stop_only else {
                        log::warn!(
                            "VAD timeout ignored - frozen policy is unavailable for run {}",
                            identity.run_id
                        );
                        continue;
                    };
                    if frozen_manual_stop_only {
                        log::info!(
                            "VAD timeout ignored - frozen run policy requires manual stop (run_id={})",
                            identity.run_id
                        );
                        continue;
                    }

                    // Negotiated pending PCM has a seal-and-drain route. Stop this
                    // exact episode now; waiting for attachment keeps the mic live.
                    let immediate_stop = {
                        let coordinator = state
                            .recording_intent_coordinator
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        if coordinator.continuation.is_some()
                            && coordinator.capture_identity()
                                == Some((
                                    super::recording_intent_coordinator::RunId::new(
                                        identity.run_id,
                                    ),
                                    identity.generation,
                                ))
                            && service.capture_token_is_current(token)
                        {
                            coordinator.current_capture_stop(
                                super::recording_intent_coordinator::IntentSource::Vad,
                            )
                        } else {
                            None
                        }
                    };
                    if let Some(event) = immediate_stop {
                        super::commands::dispatch_recording_coordinator_event(
                            app_handle.clone(),
                            event,
                        );
                        continue;
                    }

                    // During Buffering the timeout belongs to a captured phrase that has not
                    // reached the provider yet. Defer stop until Recording so normal shutdown
                    // drains the run-scoped FIFO instead of cancelling and discarding it.
                    let mut fence = DeferredVadTimeoutFence::new(identity);
                    loop {
                        let capture = state
                            .recording_intent_coordinator
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .capture;
                        let active_run_id = capture.run().map(|run| run.run_id.get());
                        let buffering_generation = match capture {
                            super::recording_intent_coordinator::CaptureState::Buffering {
                                generation,
                                ..
                            } => Some(generation),
                            _ => None,
                        };
                        let stop_already_started = matches!(
                            capture,
                            super::recording_intent_coordinator::CaptureState::Stopping { .. }
                                | super::recording_intent_coordinator::CaptureState::StopUncertain {
                                    ..
                                }
                        );
                        let coordinator_recording = matches!(
                            capture,
                            super::recording_intent_coordinator::CaptureState::Recording { .. }
                        );
                        let ready_identity = fence.observe(
                            service.capture_token_is_current(token),
                            active_run_id,
                            buffering_generation,
                            stop_already_started,
                            coordinator_recording,
                            service.get_status().await,
                        );
                        if let Some(ready_identity) = ready_identity {
                            super::commands::dispatch_recording_coordinator_event(
                                app_handle.clone(),
                                super::recording_intent_coordinator::CoordinatorEvent::CaptureIntent {
                                    intent: super::recording_intent_coordinator::RecordingIntent::stop_expected(
                                        super::recording_intent_coordinator::IntentSource::Vad,
                                        None,
                                        Some(super::recording_intent_coordinator::RunId::new(
                                            ready_identity.run_id,
                                        )),
                                    ),
                                    generation: ready_identity.generation,
                                },
                            );
                            break;
                        }
                        if fence.is_terminal() {
                            log::info!(
                                "Deferred VAD timeout dropped - coordinator capture changed (identity={:?})",
                                identity
                            );
                            break;
                        }
                        tokio::time::sleep(tokio::time::Duration::from_millis(25)).await;
                    }
                    continue;
                }

                let timeout_session_id = timeout_event.legacy_session_id;
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

                // Processing is the provider-neutral irreversible-stop boundary. Emit it for
                // the claimed session before stop_recording turns capture off, so UI cannot
                // remain in Listening while the microphone is already stopped.
                use tauri::Emitter;
                let mode = *active_recording_mode.read().await;
                if let Err(error) = app_handle.emit(
                    crate::presentation::events::EVENT_RECORDING_STATUS,
                    crate::presentation::RecordingStatusPayload {
                        window_owner_session_id: None,
                        session_id: timeout_session_id,
                        status: crate::domain::RecordingStatus::Processing,
                        stopped_via_hotkey: false,
                        mode,
                    },
                ) {
                    log::warn!(
                        "Failed to emit Processing before VAD capture stop (session_id={}): {}",
                        timeout_session_id,
                        error
                    );
                }

                match service.stop_recording().await {
                    Ok(_) => {
                        log::info!("Recording stopped successfully by VAD timeout");

                        // Эмитим событие в UI
                        *active_recording_mode.write().await = None;
                        let _ = app_handle.emit(
                            crate::presentation::events::EVENT_RECORDING_STATUS,
                            crate::presentation::RecordingStatusPayload {
                                window_owner_session_id: None,
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
                                    window_owner_session_id: None,
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
        let vad_timeout_ms = self.config.read().await.vad_silence_timeout_ms;
        self.ensure_audio_capture_device_with_vad_timeout(
            device_name,
            app_handle,
            force,
            vad_timeout_ms,
        )
        .await
    }

    /// Ensures the capture wrapper uses the VAD timeout frozen for the owning run.
    pub async fn ensure_audio_capture_device_with_vad_timeout(
        &self,
        device_name: Option<String>,
        app_handle: tauri::AppHandle,
        force: bool,
        vad_timeout_ms: u64,
    ) -> Result<(), String> {
        let config = self.config.read().await;
        let keep_ready =
            config.keep_microphone_ready && config.recording_mode == RecordingMode::Dictation;
        drop(config);
        self.ensure_audio_capture_with_policy(
            device_name,
            app_handle,
            force,
            vad_timeout_ms,
            keep_ready,
        )
        .await
    }

    pub async fn ensure_audio_capture_with_policy(
        &self,
        device_name: Option<String>,
        app_handle: tauri::AppHandle,
        force: bool,
        vad_timeout_ms: u64,
        keep_ready: bool,
    ) -> Result<(), String> {
        if self.transcription_service.has_capture_owner() {
            return Err("Cannot replace capture while an episode still owns the microphone".into());
        }
        #[cfg(target_os = "macos")]
        if keep_ready && Self::warm_route_is_eligible(device_name.as_deref()) {
            self.warm_input_suspended
                .fetch_and(!(WarmInputSuspension::Policy as u8), Ordering::AcqRel);
            return self.install_warm_capture(device_name, vad_timeout_ms).await;
        }
        let _ = keep_ready;
        self.suspend_warm_input(WarmInputSuspension::Policy);
        self.close_warm_input().await?;
        #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
        self.transcription_service
            .set_effective_capture_device(Some("native-window-e2e".into()));
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

        self.recreate_audio_capture_with_device_and_vad_timeout(
            normalized_device_name,
            app_handle,
            vad_timeout_ms,
        )
        .await
    }

    pub fn warm_capture_is_ready(&self) -> bool {
        if self.warm_input_suspended.load(Ordering::Acquire) != 0 {
            return false;
        }
        #[cfg(target_os = "macos")]
        return self
            .warm_dictation_input
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|(_, owner)| owner.is_warm_ready());
        #[cfg(not(target_os = "macos"))]
        false
    }

    #[cfg(target_os = "macos")]
    fn warm_route_is_eligible(requested: Option<&str>) -> bool {
        #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
        {
            let _ = requested;
            return true;
        }
        #[cfg(not(all(debug_assertions, feature = "native-window-e2e")))]
        crate::infrastructure::audio::WarmDictationInput::cpal_is_eligible(requested)
    }

    pub fn invalidate_warm_input(&self) {
        #[cfg(target_os = "macos")]
        if let Some((_, owner)) = self.warm_dictation_input.lock().unwrap().as_ref() {
            if let Err(error) = owner.invalidate_now() {
                log::error!("Warm input invalidation failed: {error}");
            }
        }
    }

    pub fn suspend_warm_input(&self, reason: WarmInputSuspension) {
        let _lifecycle = self.warm_lifecycle_guard.lock().unwrap();
        if self
            .warm_lifecycle_revision
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
            .is_err()
        {
            self.warm_input_suspended
                .fetch_or(WarmInputSuspension::Shutdown as u8, Ordering::AcqRel);
        }
        self.warm_input_suspended
            .fetch_or(reason as u8, Ordering::AcqRel);
        #[cfg(target_os = "macos")]
        if let Some((_, owner)) = self.warm_dictation_input.lock().unwrap().as_ref() {
            if let Err(error) = owner.suspend_now() {
                log::error!("Warm input suspend failed: {error}");
            }
        }
    }

    pub fn warm_input_is_suspended(&self, reason: WarmInputSuspension) -> bool {
        self.warm_input_suspended.load(Ordering::Acquire) & reason as u8 != 0
    }

    pub async fn sync_warm_input_policy(&self) -> Result<(), String> {
        let _guard = self.audio_start_guard.lock().await;
        if self.transcription_service.has_capture_owner() {
            return Ok(());
        }
        let config = self.config.read().await;
        let enabled =
            config.keep_microphone_ready && config.recording_mode == RecordingMode::Dictation;
        drop(config);
        if enabled {
            self.resume_warm_input_if_allowed(WarmInputSuspension::Policy)
                .await
        } else {
            self.suspend_warm_input(WarmInputSuspension::Policy);
            self.close_warm_input().await
        }
    }

    pub async fn resume_warm_input_if_allowed(
        &self,
        reason: WarmInputSuspension,
    ) -> Result<(), String> {
        self.resume_warm_input_at_revision(reason, self.warm_input_revision())
            .await
    }

    pub fn warm_input_revision(&self) -> u64 {
        self.warm_lifecycle_revision.load(Ordering::Acquire)
    }

    pub async fn resume_warm_input_at_revision(
        &self,
        reason: WarmInputSuspension,
        revision: u64,
    ) -> Result<(), String> {
        {
            let _lifecycle = self.warm_lifecycle_guard.lock().unwrap();
            if self.warm_input_revision() != revision {
                return Ok(());
            }
            if !matches!(reason, WarmInputSuspension::Shutdown) {
                self.warm_input_suspended
                    .fetch_and(!(reason as u8), Ordering::AcqRel);
            }
        }
        self.prewarm_warm_input_if_allowed().await
    }

    pub async fn prewarm_warm_input_if_allowed(&self) -> Result<(), String> {
        if self.transcription_service.has_capture_owner() {
            return Ok(());
        }
        if self.warm_input_suspended.load(Ordering::Acquire) != 0 {
            return Ok(());
        }
        let config = self.config.read().await;
        if !config.keep_microphone_ready
            || config.recording_mode != RecordingMode::Dictation
            || !self.is_authenticated_runtime.load(Ordering::Acquire)
        {
            return Ok(());
        }
        #[cfg(all(
            target_os = "macos",
            not(all(debug_assertions, feature = "native-window-e2e"))
        ))]
        if crate::infrastructure::microphone_permission::microphone_permission_status()
            != crate::infrastructure::microphone_permission::MicrophonePermissionStatus::Authorized
        {
            return Ok(());
        }
        #[cfg(target_os = "macos")]
        let requested = normalize_audio_capture_device_name(config.selected_audio_device.clone());
        drop(config);
        #[cfg(target_os = "macos")]
        {
            if !Self::warm_route_is_eligible(requested.as_deref()) {
                return Ok(());
            }
            let owner = self.warm_owner_for(requested).await?;
            {
                let _slot = self.warm_dictation_input.lock().unwrap();
                if self.warm_input_suspended.load(Ordering::Acquire) != 0 {
                    return Ok(());
                }
                owner.resume().map_err(|e| e.to_string())?;
            }
            owner.prewarm().await.map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub async fn close_warm_input(&self) -> Result<(), String> {
        let _creation = self.warm_owner_creation_guard.lock().await;
        #[cfg(target_os = "macos")]
        {
            let owner = self
                .warm_dictation_input
                .lock()
                .unwrap()
                .as_ref()
                .map(|(_, owner)| owner.clone());
            if let Some(owner) = owner {
                owner.close().await.map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    pub async fn shutdown_warm_input(&self) -> Result<(), String> {
        self.suspend_warm_input(WarmInputSuspension::Shutdown);
        let _creation = self.warm_owner_creation_guard.lock().await;
        #[cfg(target_os = "macos")]
        {
            let owner = self
                .warm_dictation_input
                .lock()
                .unwrap()
                .as_ref()
                .map(|(_, owner)| owner.clone());
            if let Some(owner) = owner {
                owner.shutdown().await.map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    async fn install_warm_capture(
        &self,
        device_name: Option<String>,
        vad_timeout_ms: u64,
    ) -> Result<(), String> {
        use crate::application::services::CaptureRecoveryPolicy;
        let requested = normalize_audio_capture_device_name(device_name);
        if self.warm_input_suspended.load(Ordering::Acquire) != 0 {
            return Err("Warm microphone is suspended".into());
        }
        let owner = self.warm_owner_for(requested.clone()).await?;
        {
            // A cold fallback suspends the cached warm owner as well as setting
            // the AppState policy bit. Clearing that bit alone is not enough:
            // a later built-in route must re-allow the same owner before its
            // lease can attach. Fence this against sleep/auth/shutdown so a
            // concurrent lifecycle transition cannot be undone here.
            let _lifecycle = self.warm_lifecycle_guard.lock().unwrap();
            if self.warm_input_suspended.load(Ordering::Acquire) != 0 {
                return Err("Warm microphone is suspended".into());
            }
            owner.resume().map_err(|e| e.to_string())?;
        }
        let vad = VadProcessor::new(Some(vad_timeout_ms), None).map_err(|e| e.to_string())?;
        let input: Box<dyn AudioCapture> = Box::new(effective_capture::EffectiveCapture::new(
            owner.lease(),
            self.transcription_service.effective_capture_device_source(),
            crate::infrastructure::audio::WarmDictationLease::device_name,
        ));
        #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
        let input = super::native_e2e::observe_warm_capture(input);
        let mut capture = VadCaptureWrapper::new_with_microphone_sensitivity(
            input,
            vad,
            self.transcription_service.microphone_sensitivity_source(),
        );
        let sender = self.vad_timeout_tx.clone();
        let session = self.active_transcription_session_id.clone();
        capture.set_identified_silence_timeout_callback(Arc::new(move |capture_identity| {
            let legacy_session_id = if capture_identity.is_none() {
                session.load(Ordering::Relaxed)
            } else {
                0
            };
            let _ = sender.send(VadTimeoutEvent {
                capture_identity,
                legacy_session_id,
            });
        }));
        self.transcription_service
            .replace_audio_capture_with_policy(
                Box::new(capture),
                CaptureRecoveryPolicy::OwnerManaged,
            )
            .await
            .map_err(|e| e.to_string())?;
        *self.active_audio_capture_device.write().await = Some(requested);
        Ok(())
    }

    #[cfg(target_os = "macos")]
    async fn warm_owner_for(
        &self,
        requested: Option<String>,
    ) -> Result<Arc<crate::infrastructure::audio::WarmDictationInput>, String> {
        let _creation = self.warm_owner_creation_guard.lock().await;
        let previous = self.warm_dictation_input.lock().unwrap().clone();
        if let Some((key, owner)) = previous.as_ref() {
            if *key == requested {
                return Ok(owner.clone());
            }
        }
        if let Some((_, owner)) = previous {
            owner.shutdown().await.map_err(|e| e.to_string())?;
        }
        #[cfg(not(all(debug_assertions, feature = "native-window-e2e")))]
        let owner = crate::infrastructure::audio::WarmDictationInput::new_cpal(requested.clone())
            .map_err(|e| e.to_string())?;
        #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
        let owner = super::native_e2e::warm_input_owner().map_err(|e| e.to_string())?;
        *self.warm_dictation_input.lock().unwrap() = Some((requested, owner.clone()));
        if self.warm_input_suspended.load(Ordering::Acquire) != 0 {
            owner.suspend_now().map_err(|e| e.to_string())?;
        }
        Ok(owner)
    }

    /// Пересоздает audio capture с новым устройством (применяет selected_audio_device)
    /// Можно вызывать при старте приложения и при смене устройства в настройках
    pub async fn recreate_audio_capture_with_device(
        &self,
        device_name: Option<String>,
        app_handle: tauri::AppHandle,
    ) -> Result<(), String> {
        let vad_timeout_ms = self.config.read().await.vad_silence_timeout_ms;
        self.recreate_audio_capture_with_device_and_vad_timeout(
            device_name,
            app_handle,
            vad_timeout_ms,
        )
        .await
    }

    async fn recreate_audio_capture_with_device_and_vad_timeout(
        &self,
        device_name: Option<String>,
        app_handle: tauri::AppHandle,
        vad_timeout_ms: u64,
    ) -> Result<(), String> {
        #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
        {
            // Retain deterministic capture; never enumerate/open a real audio device.
            let _ = (device_name, app_handle, vad_timeout_ms);
            self.transcription_service
                .set_effective_capture_device(Some("native-window-e2e".into()));
            return Ok(());
        }
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

        // Создаем VAD processor
        let vad = VadProcessor::new(Some(vad_timeout_ms), None)
            .map_err(|e| format!("Failed to create VAD processor: {}", e))?;

        let effective_capture_identity =
            self.transcription_service.effective_capture_device_source();
        let system_audio = effective_capture::EffectiveCapture::new(
            system_audio,
            effective_capture_identity.clone(),
            SystemAudioCapture::device_name,
        );

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
        vad_wrapper.set_identified_silence_timeout_callback(Arc::new(move |capture_identity| {
            let legacy_session_id = capture_identity
                .is_none()
                .then(|| active_session_id_for_vad.load(Ordering::Relaxed))
                .unwrap_or(0);
            log::info!(
                "VAD silence timeout triggered - sending notification (capture_identity={:?}, legacy_session_id={})",
                capture_identity,
                legacy_session_id
            );
            let _ = vad_tx.send(VadTimeoutEvent {
                capture_identity,
                legacy_session_id,
            });
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

fn claim_translation_shutdown(started: &AtomicBool) -> bool {
    !started.swap(true, Ordering::SeqCst)
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn warm_lifecycle_auth_cannot_clear_sleep_and_stale_wake_cannot_clear_new_sleep() {
        use super::{AppState, WarmInputSuspension};
        let service = std::sync::Arc::new(crate::application::TranscriptionService::new(
            Box::new(crate::infrastructure::audio::MockAudioCapture::new()),
            std::sync::Arc::new(crate::infrastructure::DefaultSttProviderFactory::new()),
        ));
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let state = AppState::from_recording_ports(
            service,
            crate::domain::AppConfig::default(),
            tx,
            rx,
            std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        );
        state.suspend_warm_input(WarmInputSuspension::Sleep);
        let first_sleep = state.warm_input_revision();
        state.set_authenticated(false).await;
        state.set_authenticated(true).await;
        assert!(state.warm_input_is_suspended(WarmInputSuspension::Sleep));
        assert!(!state.warm_input_is_suspended(WarmInputSuspension::Auth));
        state.suspend_warm_input(WarmInputSuspension::Sleep);
        state
            .resume_warm_input_at_revision(WarmInputSuspension::Sleep, first_sleep)
            .await
            .unwrap();
        assert!(state.warm_input_is_suspended(WarmInputSuspension::Sleep));
        state
            .resume_warm_input_if_allowed(WarmInputSuspension::Sleep)
            .await
            .unwrap();
        assert!(!state.warm_input_is_suspended(WarmInputSuspension::Sleep));
        state.shutdown_warm_input().await.unwrap();
        state
            .resume_warm_input_if_allowed(WarmInputSuspension::Shutdown)
            .await
            .unwrap();
        assert!(state.warm_input_is_suspended(WarmInputSuspension::Shutdown));
    }
    use super::{
        audio_capture_device_cache_matches, claim_translation_shutdown, claim_vad_timeout_session,
        is_current_vad_capture_identity, is_current_vad_timeout_session,
        normalize_audio_capture_device_name, restore_vad_timeout_session_claim_if_unclaimed,
        DeferredVadTimeoutFence, RecordingIntentCoordinatorMode,
    };
    use crate::domain::{AudioCaptureIdentity, RecordingStatus};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    #[test]
    fn translation_shutdown_claim_is_exactly_once_for_duplicate_exit_events() {
        let started = AtomicBool::new(false);

        assert!(claim_translation_shutdown(&started));
        assert!(!claim_translation_shutdown(&started));
        assert!(started.load(Ordering::SeqCst));
    }

    #[test]
    fn recording_intent_rollout_values_are_explicit_and_reversible() {
        assert_eq!(
            RecordingIntentCoordinatorMode::parse("desired"),
            Some(RecordingIntentCoordinatorMode::Desired)
        );
        assert_eq!(
            RecordingIntentCoordinatorMode::parse("legacy"),
            Some(RecordingIntentCoordinatorMode::Legacy)
        );
        assert_eq!(RecordingIntentCoordinatorMode::parse("DESIRED"), None);
        assert_eq!(RecordingIntentCoordinatorMode::parse(""), None);
    }

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
    fn vad_capture_identity_rejects_old_generation_and_accepts_current_pending_capture() {
        let old = AudioCaptureIdentity {
            run_id: 17,
            generation: 4,
        };

        assert!(!is_current_vad_capture_identity(
            old,
            true,
            Some(17),
            Some(5)
        ));
        assert!(is_current_vad_capture_identity(
            AudioCaptureIdentity {
                run_id: 17,
                generation: 5,
            },
            true,
            Some(17),
            Some(5)
        ));
    }

    #[test]
    fn vad_capture_identity_rejects_recreated_wrapper_callback_for_new_run() {
        let old_wrapper = AudioCaptureIdentity {
            run_id: 17,
            generation: 5,
        };

        assert!(!is_current_vad_capture_identity(
            old_wrapper,
            false,
            Some(18),
            Some(6)
        ));
    }

    #[test]
    fn vad_capture_identity_rejects_cancelled_pending_capture() {
        let cancelled = AudioCaptureIdentity {
            run_id: 19,
            generation: 7,
        };

        assert!(!is_current_vad_capture_identity(
            cancelled, false, None, None
        ));
    }

    #[test]
    fn pending_phrase_vad_waits_for_both_recording_states_and_dispatches_once() {
        let identity = AudioCaptureIdentity {
            run_id: 23,
            generation: 8,
        };
        let mut fence = DeferredVadTimeoutFence::new(identity);

        // The phrase is captured in the pending FIFO, but its provider is not ready.
        assert_eq!(
            fence.observe(true, Some(23), Some(8), false, false, RecordingStatus::Idle),
            None
        );
        assert!(!fence.is_terminal());

        // The service commits Recording before StartFinished reaches the coordinator. Stopping
        // here would turn the coordinator's pending start into cancellation and lose the FIFO.
        assert_eq!(
            fence.observe(
                true,
                Some(23),
                None,
                false,
                false,
                RecordingStatus::Recording,
            ),
            None
        );
        assert!(!fence.is_terminal());

        // Once both sides commit Recording, the ordinary run-scoped stop may drain the phrase.
        assert_eq!(
            fence.observe(
                true,
                Some(23),
                None,
                false,
                true,
                RecordingStatus::Recording,
            ),
            Some(identity)
        );
        assert!(fence.is_terminal());
        assert_eq!(
            fence.observe(
                true,
                Some(23),
                None,
                false,
                true,
                RecordingStatus::Recording,
            ),
            None,
            "one VAD event must dispatch at most one stop"
        );
    }

    #[test]
    fn pending_phrase_vad_cancellation_becomes_terminal_without_stop() {
        let identity = AudioCaptureIdentity {
            run_id: 24,
            generation: 9,
        };
        let mut fence = DeferredVadTimeoutFence::new(identity);

        assert_eq!(
            fence.observe(true, Some(24), Some(9), false, false, RecordingStatus::Idle),
            None
        );
        assert_eq!(
            fence.observe(false, None, None, false, false, RecordingStatus::Idle),
            None
        );
        assert!(fence.is_terminal());
        assert_eq!(
            fence.observe(
                true,
                Some(25),
                None,
                false,
                true,
                RecordingStatus::Recording,
            ),
            None,
            "a cancelled timeout cannot stop the replacement run"
        );
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
