use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::RwLock;
use tauri::{AppHandle, Emitter, Manager};

use crate::application::TranscriptionService;
use crate::domain::{AppConfig, Transcription, AudioCapture, UiPreferences};
use crate::infrastructure::{
    audio::{SystemAudioCapture, VadCaptureWrapper, VadProcessor},
    AuthSession, AuthStore, AuthStoreData, AuthUser, ConfigStore,
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
    pub vad_timeout_rx: Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<()>>>,

    /// VAD timeout handler task (для перезапуска при смене устройства)
    vad_handler_task: Arc<RwLock<Option<tauri::async_runtime::JoinHandle<()>>>>,

    /// Bundle ID последнего активного приложения (перед показом VoicetextAI окна)
    /// Используется для автоматической вставки текста в правильное окно
    pub last_focused_app_bundle_id: Arc<RwLock<Option<String>>>,

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

    /// Дебаунс для глобального hotkey записи.
    /// Нужен из‑за key repeat / случайных двойных срабатываний, которые выглядят как "мигание" окна.
    pub last_recording_hotkey_ms: AtomicU64,

    /// Счётчик сессий записи. Нужен, чтобы маркировать события transcription:* и не смешивать сессии.
    pub transcription_session_seq: AtomicU64,

    /// Активная (последняя запущенная) сессия записи.
    /// Используется для маркировки статусов Idle/Error, которые эмитятся "в обход" start_recording callbacks.
    pub active_transcription_session_id: AtomicU64,
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
                    app_config_revision: Arc::new(RwLock::new(0)),
                    stt_config_revision: Arc::new(RwLock::new(0)),
                    auth_state_revision: Arc::new(RwLock::new(0)),
                    ui_preferences_revision: Arc::new(RwLock::new(0)),
                    ui_preferences: Arc::new(RwLock::new(UiPreferences::default())),
                    history: Arc::new(RwLock::new(Vec::new())),
                    partial_transcription: Arc::new(RwLock::new(None)),
                    final_transcription: Arc::new(RwLock::new(None)),
                    microphone_test: Arc::new(RwLock::new(MicrophoneTestState::default())),
                    vad_timeout_rx: Arc::new(tokio::sync::Mutex::new(vad_rx)),
                    vad_handler_task: Arc::new(RwLock::new(None)),
                    last_focused_app_bundle_id: Arc::new(RwLock::new(None)),
                    is_authenticated: Arc::new(RwLock::new(false)),
                    auth_store: Arc::new(RwLock::new(AuthStoreData {
                        device_id: format!("desktop-{}", uuid::Uuid::new_v4()),
                        session: None,
                    })),
                    auth_session_revision: Arc::new(RwLock::new(0)),
                    auth_refresh_task: Arc::new(RwLock::new(None)),
                    auth_refresh_task_guard: Arc::new(tokio::sync::Mutex::new(())),
                    last_recording_hotkey_ms: AtomicU64::new(0),
                    transcription_session_seq: AtomicU64::new(0),
                    active_transcription_session_id: AtomicU64::new(0),
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
                    app_config_revision: Arc::new(RwLock::new(0)),
                    stt_config_revision: Arc::new(RwLock::new(0)),
                    auth_state_revision: Arc::new(RwLock::new(0)),
                    ui_preferences_revision: Arc::new(RwLock::new(0)),
                    ui_preferences: Arc::new(RwLock::new(UiPreferences::default())),
                    history: Arc::new(RwLock::new(Vec::new())),
                    partial_transcription: Arc::new(RwLock::new(None)),
                    final_transcription: Arc::new(RwLock::new(None)),
                    microphone_test: Arc::new(RwLock::new(MicrophoneTestState::default())),
                    vad_timeout_rx: Arc::new(tokio::sync::Mutex::new(vad_rx)),
                    vad_handler_task: Arc::new(RwLock::new(None)),
                    last_focused_app_bundle_id: Arc::new(RwLock::new(None)),
                    is_authenticated: Arc::new(RwLock::new(false)),
                    auth_store: Arc::new(RwLock::new(AuthStoreData {
                        device_id: format!("desktop-{}", uuid::Uuid::new_v4()),
                        session: None,
                    })),
                    auth_session_revision: Arc::new(RwLock::new(0)),
                    auth_refresh_task: Arc::new(RwLock::new(None)),
                    auth_refresh_task_guard: Arc::new(tokio::sync::Mutex::new(())),
                    last_recording_hotkey_ms: AtomicU64::new(0),
                    transcription_session_seq: AtomicU64::new(0),
                    active_transcription_session_id: AtomicU64::new(0),
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
            app_config_revision: Arc::new(RwLock::new(0)),
            stt_config_revision: Arc::new(RwLock::new(0)),
            auth_state_revision: Arc::new(RwLock::new(0)),
            ui_preferences_revision: Arc::new(RwLock::new(0)),
            ui_preferences: Arc::new(RwLock::new(UiPreferences::default())),
            history: Arc::new(RwLock::new(Vec::new())),
            partial_transcription: Arc::new(RwLock::new(None)),
            final_transcription: Arc::new(RwLock::new(None)),
            microphone_test: Arc::new(RwLock::new(MicrophoneTestState::default())),
            vad_timeout_rx: Arc::new(tokio::sync::Mutex::new(vad_rx)),
            vad_handler_task: Arc::new(RwLock::new(None)),
            last_focused_app_bundle_id: Arc::new(RwLock::new(None)),
            is_authenticated: Arc::new(RwLock::new(false)),
            auth_store: Arc::new(RwLock::new(AuthStoreData {
                device_id: format!("desktop-{}", uuid::Uuid::new_v4()),
                session: None,
            })),
            auth_session_revision: Arc::new(RwLock::new(0)),
            auth_refresh_task: Arc::new(RwLock::new(None)),
            auth_refresh_task_guard: Arc::new(tokio::sync::Mutex::new(())),
            last_recording_hotkey_ms: AtomicU64::new(0),
            transcription_session_seq: AtomicU64::new(0),
            active_transcription_session_id: AtomicU64::new(0),
        }
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

    async fn apply_backend_auth_token_to_stt(&self, token: Option<String>) {
        // Best-effort: ошибки не должны блокировать UX, но они важны для диагностики.
        // Важно: берём текущий in-memory config, чтобы не "сбрасывать" keep-alive и другие поля
        // при конкурирующих disk-write сценариях.
        let mut config = self.transcription_service.get_config().await;
        if config.backend_auth_token == token {
            return;
        }
        config.backend_auth_token = token;
        if let Err(e) = ConfigStore::save_config(&config).await {
            log::warn!("Failed to persist STT config token: {}", e);
        }
        if let Err(e) = self.transcription_service.update_config(config).await {
            log::warn!("Failed to update transcription service config token: {}", e);
        }
    }

    async fn emit_invalidation(app_handle: &AppHandle, topic: &str, revision: String, source_id: Option<String>) {
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
                        tokio::time::sleep(tokio::time::Duration::from_secs(ERROR_RETRY_DELAY_SECS)).await;
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
                        tokio::time::sleep(tokio::time::Duration::from_secs(ERROR_RETRY_DELAY_SECS)).await;
                        continue;
                    }
                };

                if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
                    let now_ms = chrono::Utc::now().timestamp_millis();
                    let access_ttl_ms = sess2.access_expires_at_ms - now_ms;
                    let refresh_ttl_ms = sess2
                        .refresh_expires_at_ms
                        .map(|ms| ms - now_ms);

                    // Считываем ответ, чтобы логировать серверный код/сообщение (важно для диагностики).
                    let body_text = resp.text().await.unwrap_or_default();
                    let (server_code, server_msg) = (|| {
                        let v: serde_json::Value = serde_json::from_str(&body_text).ok()?;
                        // envelope: { error: { code, message } }
                        let err = v.get("error")?;
                        let code = err.get("code").and_then(|x| x.as_str()).map(|s| s.to_string());
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
                        let current_refresh = store.session.as_ref().and_then(|s| s.refresh_token.clone());
                        current_device_id != device_id2 || current_refresh != Some(refresh_token2.clone())
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
                    AppState::emit_invalidation(&app_handle_for_task, "auth-state", rev_state, None).await;

                    let rev_session = AppState::bump_revision(&auth_session_revision).await;
                    AppState::emit_invalidation(&app_handle_for_task, "auth-session", rev_session, None).await;

                    // Clear STT token
                    if let Some(state) = app_handle_for_task.try_state::<AppState>() {
                        state.apply_backend_auth_token_to_stt(None).await;
                    }

                    break;
                }

                if !resp.status().is_success() {
                    log::warn!(
                        "[auth-refresh] refresh failed: status={}",
                        resp.status().as_u16()
                    );
                    tokio::time::sleep(tokio::time::Duration::from_secs(ERROR_RETRY_DELAY_SECS)).await;
                    continue;
                }

                let json: RefreshResp = match resp.json().await {
                    Ok(j) => j,
                    Err(e) => {
                        log::warn!("[auth-refresh] invalid JSON: {}", e);
                        tokio::time::sleep(tokio::time::Duration::from_secs(ERROR_RETRY_DELAY_SECS)).await;
                        continue;
                    }
                };

                let access_expires_at_ms = match AppState::parse_rfc3339_to_ms(&json.data.access_expires_at) {
                    Some(ms) => ms,
                    None => {
                        log::warn!("[auth-refresh] bad access_expires_at: {}", json.data.access_expires_at);
                        tokio::time::sleep(tokio::time::Duration::from_secs(ERROR_RETRY_DELAY_SECS)).await;
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
                AppState::emit_invalidation(&app_handle_for_task, "auth-session", rev_session, None).await;

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
                        let session_id = app_handle
                            .try_state::<AppState>()
                            .map(|s| s.active_transcription_session_id.load(Ordering::Relaxed))
                            .unwrap_or(0);
                        let _ = app_handle.emit(
                            crate::presentation::events::EVENT_RECORDING_STATUS,
                            crate::presentation::RecordingStatusPayload {
                                session_id,
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
