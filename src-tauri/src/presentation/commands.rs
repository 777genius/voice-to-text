use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow, Window};

use crate::domain::{
    AppConfig, AudioCapture, AudioError, BackendStreamingProvider, PlatformAudioSetupStatus,
    RecordingMode, RecordingStatus, RecordingWindowPosition, SttConnectionCategory, SttError,
    SttProviderType,
};
use crate::infrastructure::{
    auto_paste::AutoPasteTarget, AuthSession, AuthStore, AuthUser, ConfigStore,
};
use crate::presentation::{
    events::*, AppState, AudioLevelPayload, ConnectionQualityPayload, FinalTranscriptionPayload,
    MicrophoneTestLevelPayload, PartialTranscriptionPayload, RecordingStatusPayload,
    TranscriptionErrorPayload,
};

fn classify_transcription_error_type_from_stt(err: &SttError) -> String {
    // ВАЖНО: во фронте error_type используется для connect-retry, поэтому
    // тут нельзя делать "умный" парсинг строки — только типы и детали.
    match err {
        SttError::Authentication(_) => "authentication".to_string(),
        SttError::Configuration(_) => "configuration".to_string(),
        SttError::Connection(conn) => {
            if conn.details.category == Some(SttConnectionCategory::Timeout) {
                "timeout".to_string()
            } else if conn.details.category == Some(SttConnectionCategory::LimitExceeded) {
                "limit_exceeded".to_string()
            } else if conn.details.category == Some(SttConnectionCategory::ProviderQuotaExceeded) {
                "provider_quota_exceeded".to_string()
            } else {
                "connection".to_string()
            }
        }
        SttError::Processing(_) | SttError::Unsupported(_) | SttError::Internal(_) => {
            "processing".to_string()
        }
    }
}

fn error_details_from_stt(err: &SttError) -> Option<TranscriptionErrorDetailsPayload> {
    match err {
        SttError::Connection(conn) => Some(conn.details.clone().into()),
        _ => None,
    }
}

fn is_audio_capture_start_failure(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| cause.is::<AudioError>())
}

fn take_active_transcription_session_id(state: &AppState) -> u64 {
    state
        .active_transcription_session_id
        .swap(0, Ordering::Relaxed)
}

fn clear_active_transcription_session_id_if_current(state: &AppState, session_id: u64) {
    let _ = state.active_transcription_session_id.compare_exchange(
        session_id,
        0,
        Ordering::Relaxed,
        Ordering::Relaxed,
    );
}

fn emit_idle_recording_status(
    app_handle: &AppHandle,
    session_id: u64,
    stopped_via_hotkey: bool,
    mode: Option<RecordingMode>,
) {
    log::debug!(
        "Emitting status: Idle (stopped_via_hotkey: {}, mode: {:?})",
        stopped_via_hotkey,
        mode
    );
    let _ = app_handle.emit(
        EVENT_RECORDING_STATUS,
        RecordingStatusPayload {
            session_id,
            status: RecordingStatus::Idle,
            stopped_via_hotkey,
            mode,
        },
    );
}

async fn active_recording_status(state: &AppState) -> RecordingStatus {
    let active_mode = *state.active_recording_mode.read().await;
    if matches!(active_mode, Some(RecordingMode::LiveTranslation)) {
        let svc = state.live_translation_service.read().await;
        if let Some(service) = svc.as_ref() {
            return service.get_status().await;
        }
    }

    state.transcription_service.get_status().await
}

#[tauri::command]
pub fn log_client_event(
    level: Option<String>,
    event: String,
    data: Option<serde_json::Value>,
) -> Result<(), String> {
    let data_string = data
        .map(|value| value.to_string())
        .unwrap_or_else(|| "{}".to_string());
    let data_preview = if data_string.chars().count() > 2_000 {
        format!(
            "{}...<truncated>",
            data_string.chars().take(2_000).collect::<String>()
        )
    } else {
        data_string
    };
    let message = format!("Client event: {} {}", event, data_preview);

    match level.as_deref() {
        Some("debug") => log::debug!(target: "client", "{}", message),
        Some("warn") => log::warn!(target: "client", "{}", message),
        Some("error") => log::error!(target: "client", "{}", message),
        _ => log::info!(target: "client", "{}", message),
    }

    Ok(())
}

fn should_hide_recording_window_immediately_on_hotkey_stop(
    config: &AppConfig,
    window_visible: bool,
) -> bool {
    window_visible || config.show_mini_recording_window || config.hide_recording_window_on_hotkey
}

async fn hide_recording_window_for_hotkey_stop_if_needed(
    window: &tauri::WebviewWindow,
    config: &AppConfig,
    state: &AppState,
    accepted_press_seq: u64,
    context: &'static str,
) -> Result<bool, String> {
    let window_visible = window.is_visible().map_err(|e| e.to_string())?;
    let should_hide_immediately =
        should_hide_recording_window_immediately_on_hotkey_stop(config, window_visible);
    log::info!(
        "Hotkey stop hide check ({}): window_visible={}, hide_immediately={}, show_mini={}, hide_on_hotkey={}",
        context,
        window_visible,
        should_hide_immediately,
        config.show_mini_recording_window,
        config.hide_recording_window_on_hotkey
    );

    if should_hide_immediately && window_visible {
        let _ = window.emit(EVENT_RECORDING_WINDOW_WILL_HIDE_FOR_HOTKEY_STOP, ());
        tokio::time::sleep(Duration::from_millis(hotkey_stop_hide_ui_flush_ms(config))).await;
        if hotkey_action_is_stale(
            accepted_press_seq,
            state
                .recording_hotkey_accepted_press_seq
                .load(Ordering::SeqCst),
        ) {
            log::info!(
                "[HotkeyDiag] hotkey stop hide skipped because a newer press was accepted (context={}, stop_press_seq={})",
                context,
                accepted_press_seq
            );
            return Ok(false);
        }
        window.hide().map_err(|e| e.to_string())?;
        return Ok(true);
    }

    Ok(false)
}

fn emit_recording_start_requested(
    app_handle: &AppHandle,
    source: &'static str,
    can_resume_keep_alive: bool,
    warm_start_expected: bool,
) {
    log::info!(
        "[HotkeyDiag] emitting recording:start-requested (source={}, can_resume_keep_alive={}, warm_start_expected={})",
        source,
        can_resume_keep_alive,
        warm_start_expected
    );
    let _ = app_handle.emit(
        "recording:start-requested",
        serde_json::json!({
            "source": source,
            "canResumeKeepAlive": can_resume_keep_alive,
            "warmStartExpected": warm_start_expected
        }),
    );
}

const HOTKEY_START_STOP_SUPPRESSION_MS: u64 = 1_500;
const HOTKEY_STOP_HIDE_UI_FLUSH_MS: u64 = 35;
const MINI_HOTKEY_STOP_HIDE_UI_FLUSH_MS: u64 = 220;
const HOTKEY_STOP_WAIT_FOR_RECORDING_MS: u64 = 12_000;
const HOTKEY_STOP_WAIT_POLL_MS: u64 = 25;
const HOLD_TO_RECORD_RELEASE_START_WAIT_MS: u64 = 2_000;

fn hotkey_stop_hide_ui_flush_ms(config: &AppConfig) -> u64 {
    if config.show_mini_recording_window {
        MINI_HOTKEY_STOP_HIDE_UI_FLUSH_MS
    } else {
        HOTKEY_STOP_HIDE_UI_FLUSH_MS
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecordingHotkeyDispatchIntent {
    Toggle,
    Start,
    Stop,
    Ignore,
}

fn recording_hotkey_press_intent(
    hold_to_record: bool,
    status: RecordingStatus,
) -> RecordingHotkeyDispatchIntent {
    if !hold_to_record {
        return RecordingHotkeyDispatchIntent::Toggle;
    }

    match status {
        RecordingStatus::Idle => RecordingHotkeyDispatchIntent::Start,
        RecordingStatus::Processing => RecordingHotkeyDispatchIntent::Start,
        _ => RecordingHotkeyDispatchIntent::Ignore,
    }
}

fn recording_hotkey_release_intent(
    hold_to_record: bool,
    status: RecordingStatus,
) -> RecordingHotkeyDispatchIntent {
    if !hold_to_record {
        return RecordingHotkeyDispatchIntent::Ignore;
    }

    match status {
        RecordingStatus::Starting | RecordingStatus::Recording => {
            RecordingHotkeyDispatchIntent::Stop
        }
        _ => RecordingHotkeyDispatchIntent::Ignore,
    }
}

#[cfg(target_os = "macos")]
const HOTKEY_PHYSICAL_RELEASE_POLL_MS: u64 = 16;
#[cfg(target_os = "macos")]
const HOTKEY_PHYSICAL_RELEASE_TIMEOUT_MS: u64 = 10_000;

#[cfg(target_os = "macos")]
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn CGEventSourceKeyState(state_id: i32, key: u16) -> bool;
}

fn now_ms_u64() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0) as u64
}

fn suppress_immediate_hotkey_stop_after_start(state: &AppState, accepted_press_seq: u64) {
    let until_ms = now_ms_u64().saturating_add(HOTKEY_START_STOP_SUPPRESSION_MS);
    state
        .recording_hotkey_stop_suppressed_until_ms
        .store(until_ms, Ordering::SeqCst);
    state
        .recording_hotkey_stop_suppression_press_seq
        .store(accepted_press_seq, Ordering::SeqCst);
    log::debug!(
        "[HotkeyDiag] armed start protection: accepted_press_seq={}, until_ms={}",
        accepted_press_seq,
        until_ms
    );
}

fn should_ignore_hotkey_stop_after_start(
    now_ms: u64,
    suppressed_until_ms: u64,
    suppression_press_seq: u64,
    current_press_seq: u64,
) -> bool {
    now_ms <= suppressed_until_ms && current_press_seq <= suppression_press_seq
}

fn hotkey_action_is_stale(action_press_seq: u64, current_press_seq: u64) -> bool {
    action_press_seq != current_press_seq
}

fn should_cancel_hold_to_record_pending_start(
    hold_to_record: bool,
    action_press_seq: Option<u64>,
    current_press_seq: u64,
    released_since_press: bool,
) -> bool {
    hold_to_record
        && action_press_seq.is_some_and(|seq| {
            hotkey_action_is_stale(seq, current_press_seq) || released_since_press
        })
}

fn should_ignore_immediate_hotkey_stop_after_start(state: &AppState) -> bool {
    let now_ms = now_ms_u64();
    should_ignore_hotkey_stop_after_start(
        now_ms,
        state
            .recording_hotkey_stop_suppressed_until_ms
            .load(Ordering::SeqCst),
        state
            .recording_hotkey_stop_suppression_press_seq
            .load(Ordering::SeqCst),
        state
            .recording_hotkey_accepted_press_seq
            .load(Ordering::SeqCst),
    )
}

async fn get_hotkey_start_connection_hint(state: &AppState, config: &AppConfig) -> (bool, bool) {
    let can_resume_keep_alive = state
        .transcription_service
        .can_resume_keep_alive_connection()
        .await;
    let keep_alive_enabled =
        config.stt.keep_connection_alive || config.stt.provider == SttProviderType::Backend;
    let status = active_recording_status(state).await;
    let warm_start_expected = can_resume_keep_alive
        || (keep_alive_enabled
            && matches!(
                status,
                RecordingStatus::Recording | RecordingStatus::Processing
            ));

    (can_resume_keep_alive, warm_start_expected)
}

#[cfg(test)]
fn should_show_recording_window_on_processing_hotkey(
    config: &AppConfig,
    window_visible: bool,
) -> bool {
    config.show_mini_recording_window
        || (!window_visible && !config.hide_recording_window_on_hotkey)
}

async fn prepare_recording_hotkey_start(
    state: &AppState,
    app_handle: &AppHandle,
    source: &'static str,
    stop_suppression_press_seq: Option<u64>,
) -> AppConfig {
    let config = state.config.read().await.clone();
    let (can_resume_keep_alive, warm_start_expected) =
        get_hotkey_start_connection_hint(state, &config).await;
    emit_recording_start_requested(
        app_handle,
        source,
        can_resume_keep_alive,
        warm_start_expected,
    );
    if let Some(accepted_press_seq) = stop_suppression_press_seq {
        if config.hold_to_record {
            return config;
        }
        suppress_immediate_hotkey_stop_after_start(state, accepted_press_seq);
    }

    config
}

async fn apply_recording_window_for_hotkey_start(
    state: &AppState,
    app_handle: &AppHandle,
    config: &AppConfig,
) -> Result<bool, String> {
    let Some(window) = app_handle.get_webview_window("main") else {
        return Err("main window is unavailable".to_string());
    };

    let hide_window_on_hotkey =
        config.hide_recording_window_on_hotkey && !config.show_mini_recording_window;
    let window_visible = window.is_visible().map_err(|e| e.to_string())?;
    if hide_window_on_hotkey {
        if window_visible {
            window.hide().map_err(|e| e.to_string())?;
        } else {
            save_active_app_target_for_auto_paste(state).await;
        }
        return Ok(false);
    }

    if config.show_mini_recording_window || !window_visible {
        save_active_app_target_for_auto_paste(state).await;
        show_webview_window_with_recording_config(&window, &config, state)?;
        return Ok(true);
    }

    Ok(false)
}

async fn show_recording_window_for_hotkey_start(
    state: &AppState,
    app_handle: &AppHandle,
    source: &'static str,
    stop_suppression_press_seq: Option<u64>,
) -> Result<bool, String> {
    let config =
        prepare_recording_hotkey_start(state, app_handle, source, stop_suppression_press_seq).await;
    apply_recording_window_for_hotkey_start(state, app_handle, &config).await
}

fn emit_recording_window_shown(app_handle: &AppHandle) {
    if let Some(window) = app_handle.get_webview_window("main") {
        let _ = window.emit(EVENT_RECORDING_WINDOW_SHOWN, ());
    }
}
async fn stop_recording_and_emit_idle(
    state: &AppState,
    app_handle: &AppHandle,
    stopped_via_hotkey: bool,
) -> Result<String, String> {
    // Dispatcher: если активный режим — live_translation, останавливаем translation сервис.
    let active_mode = *state.active_recording_mode.read().await;
    if matches!(
        active_mode,
        Some(crate::domain::RecordingMode::LiveTranslation)
    ) {
        return stop_live_translation_recording(state, app_handle, stopped_via_hotkey).await;
    }

    let status_before_stop = state.transcription_service.get_status().await;
    log::info!(
        "Stopping recording: stopped_via_hotkey={}, status_before={:?}",
        stopped_via_hotkey,
        status_before_stop
    );

    match state.transcription_service.stop_recording().await {
        Ok(result) => {
            let session_id = take_active_transcription_session_id(state);
            log::info!(
                "Recording stop completed: stopped_via_hotkey={}, session_id={}, result={}",
                stopped_via_hotkey,
                session_id,
                result
            );
            *state.active_recording_mode.write().await = None;
            emit_idle_recording_status(app_handle, session_id, stopped_via_hotkey, None);
            Ok(result)
        }
        Err(err) => {
            let current_status = state.transcription_service.get_status().await;
            if current_status == RecordingStatus::Idle {
                let session_id = take_active_transcription_session_id(state);
                log::warn!(
                    "Recording stop returned error after service recovered to Idle; emitting Idle status: {}",
                    err
                );
                *state.active_recording_mode.write().await = None;
                emit_idle_recording_status(app_handle, session_id, stopped_via_hotkey, None);
                Ok("Recording stopped".to_string())
            } else {
                log::error!(
                    "Recording stop failed: stopped_via_hotkey={}, status_before={:?}, current_status={:?}, error={}",
                    stopped_via_hotkey,
                    status_before_stop,
                    current_status,
                    err
                );
                Err(err.to_string())
            }
        }
    }
}

async fn get_or_create_live_translation_service(
    state: &AppState,
) -> std::sync::Arc<crate::application::services::LiveTranslationService> {
    {
        let guard = state.live_translation_service.read().await;
        if let Some(existing) = guard.as_ref() {
            return existing.clone();
        }
    }
    let mut guard = state.live_translation_service.write().await;
    if let Some(existing) = guard.as_ref() {
        return existing.clone();
    }
    let service = std::sync::Arc::new(crate::application::services::LiveTranslationService::new());
    *guard = Some(service.clone());
    service
}

fn translation_error_type_to_str(
    err: &crate::application::services::LiveTranslationError,
) -> &'static str {
    err.error_type()
}

fn resolve_openai_api_key(config: &AppConfig) -> String {
    config
        .openai_api_key
        .as_ref()
        .map(|key| key.trim())
        .filter(|key| !key.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| std::env::var("OPENAI_API_KEY").unwrap_or_default())
}

async fn start_live_translation_recording(
    state: &AppState,
    app_handle: AppHandle,
    session_id: u64,
) -> Result<String, String> {
    use crate::application::services::{
        LiveTranslationCallbacks, LiveTranslationConfig, LiveTranslationError,
    };
    use crate::domain::RecordingMode;
    use crate::presentation::events::{
        EVENT_AUDIO_SPECTRUM, EVENT_TRANSLATION_DELTA, EVENT_TRANSLATION_ERROR,
    };

    let config = state.config.read().await.clone();
    let service = get_or_create_live_translation_service(state).await;
    *state.active_recording_mode.write().await = Some(RecordingMode::LiveTranslation);

    // Translation status emit — Starting с mode
    let _ = app_handle.emit(
        EVENT_RECORDING_STATUS,
        RecordingStatusPayload {
            session_id,
            status: RecordingStatus::Starting,
            stopped_via_hotkey: false,
            mode: Some(RecordingMode::LiveTranslation),
        },
    );

    let translation_cfg = LiveTranslationConfig {
        openai_api_key: resolve_openai_api_key(&config),
        target_language: "en".to_string(),
        microphone_device: config.selected_audio_device.clone(),
        microphone_sensitivity: config.microphone_sensitivity,
        session_id,
    };

    // Callbacks: emit события во фронт
    let app_handle_transcript = app_handle.clone();
    let on_transcript_delta: std::sync::Arc<dyn Fn(String) + Send + Sync> =
        std::sync::Arc::new(move |text: String| {
            let payload = crate::presentation::events::TranslationDeltaPayload {
                session_id,
                text,
                timestamp: now_ms_u64(),
            };
            if let Err(e) = app_handle_transcript.emit(EVENT_TRANSLATION_DELTA, payload) {
                log::error!("Failed to emit translation delta: {}", e);
            }
        });

    let app_handle_spectrum = app_handle.clone();
    let on_audio_spectrum: std::sync::Arc<dyn Fn([f32; 48]) + Send + Sync> =
        std::sync::Arc::new(move |bars: [f32; 48]| {
            let payload = AudioSpectrumPayload {
                bars: bars.to_vec(),
            };
            let _ = app_handle_spectrum.emit(EVENT_AUDIO_SPECTRUM, payload);
        });

    let app_handle_error = app_handle.clone();
    let app_handle_error_cleanup = app_handle.clone();
    let on_error: std::sync::Arc<dyn Fn(LiveTranslationError) + Send + Sync> =
        std::sync::Arc::new(move |err: LiveTranslationError| {
            let error_type = translation_error_type_to_str(&err).to_string();
            let payload = crate::presentation::events::TranslationErrorPayload {
                session_id,
                error: err.to_string(),
                error_type,
            };
            if let Err(e) = app_handle_error.emit(EVENT_TRANSLATION_ERROR, payload) {
                log::error!("Failed to emit translation error: {}", e);
            }
            let _ = app_handle_error.emit(
                EVENT_RECORDING_STATUS,
                RecordingStatusPayload {
                    session_id,
                    status: RecordingStatus::Error,
                    stopped_via_hotkey: false,
                    mode: Some(RecordingMode::LiveTranslation),
                },
            );
            let cleanup_handle = app_handle_error_cleanup.clone();
            tauri::async_runtime::spawn(async move {
                let Some(state) = cleanup_handle.try_state::<AppState>() else {
                    return;
                };
                clear_active_transcription_session_id_if_current(state.inner(), session_id);
                let mut active_mode = state.active_recording_mode.write().await;
                if matches!(*active_mode, Some(RecordingMode::LiveTranslation)) {
                    *active_mode = None;
                }
            });
        });

    let app_handle_status = app_handle.clone();
    let on_status: std::sync::Arc<dyn Fn(RecordingStatus) + Send + Sync> =
        std::sync::Arc::new(move |status: RecordingStatus| {
            // Состояния Starting / Recording / Error эмитятся выше явно; здесь логируем для диагностики.
            log::debug!(
                "LiveTranslation status callback: session={}, status={:?}",
                session_id,
                status
            );
            // Можем дополнительно эмитить, но во избежание двойных эмитов оставим только лог.
            let _ = app_handle_status;
        });

    let callbacks = LiveTranslationCallbacks {
        on_transcript_delta,
        on_audio_spectrum,
        on_error,
        on_status,
    };

    match service.start_translation(translation_cfg, callbacks).await {
        Ok(()) => {
            *state.active_recording_mode.write().await = Some(RecordingMode::LiveTranslation);
            let _ = app_handle.emit(
                EVENT_RECORDING_STATUS,
                RecordingStatusPayload {
                    session_id,
                    status: RecordingStatus::Recording,
                    stopped_via_hotkey: false,
                    mode: Some(RecordingMode::LiveTranslation),
                },
            );
            Ok("LiveTranslation started".to_string())
        }
        Err(err) => {
            let error_type = translation_error_type_to_str(&err).to_string();
            let payload = crate::presentation::events::TranslationErrorPayload {
                session_id,
                error: err.to_string(),
                error_type,
            };
            let _ = app_handle.emit(EVENT_TRANSLATION_ERROR, payload);
            let _ = app_handle.emit(
                EVENT_RECORDING_STATUS,
                RecordingStatusPayload {
                    session_id,
                    status: RecordingStatus::Error,
                    stopped_via_hotkey: false,
                    mode: Some(RecordingMode::LiveTranslation),
                },
            );
            clear_active_transcription_session_id_if_current(state, session_id);
            let mut active_mode = state.active_recording_mode.write().await;
            if matches!(*active_mode, Some(RecordingMode::LiveTranslation)) {
                *active_mode = None;
            }
            Err(err.to_string())
        }
    }
}

async fn stop_live_translation_recording(
    state: &AppState,
    app_handle: &AppHandle,
    stopped_via_hotkey: bool,
) -> Result<String, String> {
    use crate::domain::RecordingMode;

    let service = {
        let guard = state.live_translation_service.read().await;
        guard.as_ref().cloned()
    };

    let session_id = take_active_transcription_session_id(state);

    // Эмитим Processing (drain) — UI знает что мы заканчиваем
    let _ = app_handle.emit(
        EVENT_RECORDING_STATUS,
        RecordingStatusPayload {
            session_id,
            status: RecordingStatus::Processing,
            stopped_via_hotkey,
            mode: Some(RecordingMode::LiveTranslation),
        },
    );

    if let Some(svc) = service {
        if let Err(e) = svc.stop_translation().await {
            log::warn!("LiveTranslationService stop returned error: {}", e);
        }
    }

    *state.active_recording_mode.write().await = None;
    emit_idle_recording_status(
        app_handle,
        session_id,
        stopped_via_hotkey,
        Some(RecordingMode::LiveTranslation),
    );
    Ok("LiveTranslation stopped".to_string())
}

async fn get_or_create_incoming_translation_service(
    state: &AppState,
) -> std::sync::Arc<crate::application::services::IncomingTranslationService> {
    {
        let guard = state.incoming_translation_service.read().await;
        if let Some(existing) = guard.as_ref() {
            return existing.clone();
        }
    }
    let mut guard = state.incoming_translation_service.write().await;
    if let Some(existing) = guard.as_ref() {
        return existing.clone();
    }
    let service =
        std::sync::Arc::new(crate::application::services::IncomingTranslationService::new());
    *guard = Some(service.clone());
    service
}

#[tauri::command]
pub async fn start_incoming_translation(
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<String, String> {
    use crate::application::services::{
        IncomingTranslationCallbacks, IncomingTranslationConfig, IncomingTranslationError,
    };
    use crate::presentation::events::{
        EVENT_INCOMING_TRANSLATION_DELTA, EVENT_INCOMING_TRANSLATION_ERROR,
        EVENT_INCOMING_TRANSLATION_SOURCE_FINAL, EVENT_INCOMING_TRANSLATION_STATUS,
    };

    let service = get_or_create_incoming_translation_service(state.inner()).await;
    if service.get_status().await != RecordingStatus::Idle {
        return Ok("Incoming translation already running".to_string());
    }

    let session_id = state
        .incoming_translation_session_seq
        .fetch_add(1, Ordering::Relaxed)
        + 1;
    let mut stt_config = state.transcription_service.get_config().await;
    stt_config.keep_connection_alive = false;
    let app_config = state.config.read().await.clone();
    let mut cfg = IncomingTranslationConfig::new_with_defaults(stt_config, session_id);
    cfg.openai_api_key = resolve_openai_api_key(&app_config);

    let _ = app_handle.emit(
        EVENT_INCOMING_TRANSLATION_STATUS,
        IncomingTranslationStatusPayload {
            session_id,
            status: RecordingStatus::Starting,
        },
    );

    let source_handle = app_handle.clone();
    let on_source_final: std::sync::Arc<dyn Fn(String) + Send + Sync> =
        std::sync::Arc::new(move |text: String| {
            let _ = source_handle.emit(
                EVENT_INCOMING_TRANSLATION_SOURCE_FINAL,
                IncomingTranslationTextPayload {
                    session_id,
                    text,
                    timestamp: now_ms_u64(),
                },
            );
        });

    let delta_handle = app_handle.clone();
    let on_translation_delta: std::sync::Arc<dyn Fn(String) + Send + Sync> =
        std::sync::Arc::new(move |text: String| {
            let _ = delta_handle.emit(
                EVENT_INCOMING_TRANSLATION_DELTA,
                IncomingTranslationTextPayload {
                    session_id,
                    text,
                    timestamp: now_ms_u64(),
                },
            );
        });

    let error_handle = app_handle.clone();
    let on_error: std::sync::Arc<dyn Fn(IncomingTranslationError) + Send + Sync> =
        std::sync::Arc::new(move |err: IncomingTranslationError| {
            let should_emit_error_status = matches!(
                &err,
                IncomingTranslationError::Configuration(_)
                    | IncomingTranslationError::Authentication(_)
                    | IncomingTranslationError::RateLimited(_)
            );
            let _ = error_handle.emit(
                EVENT_INCOMING_TRANSLATION_ERROR,
                IncomingTranslationErrorPayload {
                    session_id,
                    error: err.to_string(),
                    error_type: err.error_type().to_string(),
                },
            );
            if should_emit_error_status {
                let _ = error_handle.emit(
                    EVENT_INCOMING_TRANSLATION_STATUS,
                    IncomingTranslationStatusPayload {
                        session_id,
                        status: RecordingStatus::Error,
                    },
                );
            }
        });

    let status_handle = app_handle.clone();
    let on_status: std::sync::Arc<dyn Fn(RecordingStatus) + Send + Sync> =
        std::sync::Arc::new(move |status: RecordingStatus| {
            let _ = status_handle.emit(
                EVENT_INCOMING_TRANSLATION_STATUS,
                IncomingTranslationStatusPayload { session_id, status },
            );
        });

    let callbacks = IncomingTranslationCallbacks {
        on_source_final,
        on_translation_delta,
        on_error,
        on_status,
    };

    service.start(cfg, callbacks).await.map_err(|err| {
        let _ = app_handle.emit(
            EVENT_INCOMING_TRANSLATION_ERROR,
            IncomingTranslationErrorPayload {
                session_id,
                error: err.to_string(),
                error_type: err.error_type().to_string(),
            },
        );
        let _ = app_handle.emit(
            EVENT_INCOMING_TRANSLATION_STATUS,
            IncomingTranslationStatusPayload {
                session_id,
                status: RecordingStatus::Error,
            },
        );
        err.to_string()
    })?;

    Ok("Incoming translation started".to_string())
}

#[tauri::command]
pub async fn stop_incoming_translation(
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<String, String> {
    use crate::presentation::events::EVENT_INCOMING_TRANSLATION_STATUS;

    let service = {
        let guard = state.incoming_translation_service.read().await;
        guard.as_ref().cloned()
    };
    let session_id = state
        .incoming_translation_session_seq
        .load(Ordering::Relaxed)
        .max(1);

    let _ = app_handle.emit(
        EVENT_INCOMING_TRANSLATION_STATUS,
        IncomingTranslationStatusPayload {
            session_id,
            status: RecordingStatus::Processing,
        },
    );

    if let Some(service) = service {
        service.stop().await.map_err(|err| err.to_string())?;
    }

    let _ = app_handle.emit(
        EVENT_INCOMING_TRANSLATION_STATUS,
        IncomingTranslationStatusPayload {
            session_id,
            status: RecordingStatus::Idle,
        },
    );
    Ok("Incoming translation stopped".to_string())
}

#[tauri::command]
pub async fn toggle_incoming_translation(
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<String, String> {
    let service = get_or_create_incoming_translation_service(state.inner()).await;
    if service.get_status().await == RecordingStatus::Idle {
        start_incoming_translation(state, app_handle).await
    } else {
        stop_incoming_translation(state, app_handle).await
    }
}

#[tauri::command]
pub async fn get_incoming_translation_status(
    state: State<'_, AppState>,
) -> Result<RecordingStatus, String> {
    let service = {
        let guard = state.incoming_translation_service.read().await;
        guard.as_ref().cloned()
    };
    if let Some(service) = service {
        Ok(service.get_status().await)
    } else {
        Ok(RecordingStatus::Idle)
    }
}

#[tauri::command]
pub async fn get_live_translation_platform_status() -> Result<PlatformAudioSetupStatus, String> {
    use crate::domain::PlatformAudioFactory;

    let factory = crate::infrastructure::audio::DefaultPlatformAudioFactory::new();
    Ok(factory.setup_status().await)
}

/// Start recording voice
#[tauri::command]
pub async fn start_recording(
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<String, String> {
    log::info!("Command: start_recording");

    // Новый идентификатор сессии записи. Маркируем им все события transcription:* и recording:status,
    // чтобы frontend мог игнорировать "поздние" сообщения от предыдущей сессии.
    let session_id = state
        .transcription_session_seq
        .fetch_add(1, Ordering::Relaxed)
        + 1;
    state
        .active_transcription_session_id
        .store(session_id, Ordering::Relaxed);
    log::info!("Recording session started: session_id={}", session_id);

    // Dispatcher: если в Settings выбран live_translation, направляем в отдельный сервис
    // и НЕ запускаем STT pipeline. Dictation идёт по прежнему пути ниже.
    let selected_mode = state.config.read().await.recording_mode;
    if selected_mode == crate::domain::RecordingMode::LiveTranslation {
        return start_live_translation_recording(state.inner(), app_handle, session_id).await;
    }
    // Dictation mode: помечаем active_recording_mode, чтобы stop корректно роутил.
    *state.active_recording_mode.write().await = Some(crate::domain::RecordingMode::Dictation);

    // На macOS при отсутствии разрешения на микрофон CoreAudio может отдавать "тишину" (все нули),
    // и UI будет выглядеть как "не записывает".
    // Поэтому проверяем статус и даём явную ошибку.
    #[cfg(target_os = "macos")]
    {
        use crate::infrastructure::microphone_permission::{
            microphone_permission_status, MicrophonePermissionStatus,
        };

        match microphone_permission_status() {
            MicrophonePermissionStatus::Authorized | MicrophonePermissionStatus::NotDetermined => {}
            _ => {
                let error_msg =
                    "Нет доступа к микрофону. Откройте macOS System Settings → Privacy & Security → Microphone и включите доступ для приложения."
                        .to_string();
                let stt_err = SttError::Configuration(error_msg.clone());
                let error_type = classify_transcription_error_type_from_stt(&stt_err);
                let payload = TranscriptionErrorPayload {
                    session_id,
                    error: error_msg.clone(),
                    error_type,
                    error_details: error_details_from_stt(&stt_err),
                };
                if let Err(emit_err) = app_handle.emit(EVENT_TRANSCRIPTION_ERROR, payload) {
                    log::error!("Failed to emit transcription error event: {}", emit_err);
                }
                let _ = app_handle.emit(
                    EVENT_RECORDING_STATUS,
                    RecordingStatusPayload {
                        session_id,
                        status: RecordingStatus::Error,
                        stopped_via_hotkey: false,
                        mode: None,
                    },
                );
                clear_active_transcription_session_id_if_current(state.inner(), session_id);
                return Err(error_msg);
            }
        }
    }

    let app_handle_clone = app_handle.clone();
    let state_partial = state.partial_transcription.clone();

    // Callback for partial transcriptions
    let on_partial = Arc::new(move |transcription: crate::domain::Transcription| {
        let text = transcription.text.clone();
        let app_handle = app_handle_clone.clone();
        let state_partial = state_partial.clone();

        tokio::spawn(async move {
            // Update state
            *state_partial.write().await = Some(text.clone());

            // Emit event to frontend
            let payload =
                PartialTranscriptionPayload::from_transcription(transcription, session_id);
            if let Err(e) = app_handle.emit(EVENT_TRANSCRIPTION_PARTIAL, payload) {
                log::error!("Failed to emit partial transcription event: {}", e);
            }
        });
    });

    let app_handle_final = app_handle.clone();
    let state_final = state.final_transcription.clone();
    let state_history = state.history.clone();
    let state_config = state.config.clone();

    // Callback for final transcription
    let on_final = Arc::new(move |transcription: crate::domain::Transcription| {
        let text = transcription.text.clone();
        let app_handle = app_handle_final.clone();
        let state_final = state_final.clone();
        let state_history = state_history.clone();
        let state_config = state_config.clone();

        tokio::spawn(async move {
            // Update state
            *state_final.write().await = Some(text.clone());

            // Add to history
            state_history.write().await.push(transcription.clone());

            // Keep only last N items
            let max_items = state_config.read().await.max_history_items;
            let mut history = state_history.write().await;
            let len = history.len();
            if len > max_items {
                history.drain(0..len - max_items);
            }
            drop(history);

            // Emit event to frontend
            let payload =
                FinalTranscriptionPayload::from_transcription(transcription.clone(), session_id);
            if let Err(e) = app_handle.emit(EVENT_TRANSCRIPTION_FINAL, payload) {
                log::error!("Failed to emit final transcription event: {}", e);
            }
        });
    });

    let app_handle_level = app_handle.clone();

    // Callback for audio level visualization
    let on_audio_level = Arc::new(move |level: f32| {
        let app_handle = app_handle_level.clone();

        // Don't spawn task for every level update - just emit directly
        let payload = AudioLevelPayload { level };
        let _ = app_handle.emit(EVENT_AUDIO_LEVEL, payload);
    });

    let app_handle_spectrum = app_handle.clone();

    // Callback for audio spectrum visualization (48 bars)
    let on_audio_spectrum = Arc::new(move |bars: [f32; 48]| {
        let app_handle = app_handle_spectrum.clone();
        let payload = AudioSpectrumPayload {
            bars: bars.to_vec(),
        };
        let _ = app_handle.emit(EVENT_AUDIO_SPECTRUM, payload);
    });

    let app_handle_error = app_handle.clone();

    // Callback for error handling
    let on_error = Arc::new(move |err: SttError| {
        let app_handle = app_handle_error.clone();

        tokio::spawn(async move {
            let error_type = classify_transcription_error_type_from_stt(&err);
            let error_details = error_details_from_stt(&err);
            let error = err.to_string();

            log::error!("STT error occurred: {} (type: {})", error, error_type);

            // Emit error event to frontend
            let payload = TranscriptionErrorPayload {
                session_id,
                error,
                error_type,
                error_details,
            };
            if let Err(e) = app_handle.emit(EVENT_TRANSCRIPTION_ERROR, payload) {
                log::error!("Failed to emit transcription error event: {}", e);
            }

            // Emit Error status
            let _ = app_handle.emit(
                EVENT_RECORDING_STATUS,
                RecordingStatusPayload {
                    session_id,
                    status: RecordingStatus::Error,
                    stopped_via_hotkey: false,
                    mode: None,
                },
            );

            if let Some(state) = app_handle.try_state::<AppState>() {
                clear_active_transcription_session_id_if_current(state.inner(), session_id);
            }
        });
    });

    let app_handle_quality = app_handle.clone();

    // Callback for connection quality updates
    let on_connection_quality = Arc::new(move |quality: String, reason: Option<String>| {
        let app_handle = app_handle_quality.clone();

        tokio::spawn(async move {
            log::info!(
                "Connection quality changed: {} (reason: {:?})",
                quality,
                reason
            );

            // Emit connection quality event to frontend
            let payload = ConnectionQualityPayload {
                session_id,
                quality: match quality.as_str() {
                    "Good" => crate::presentation::events::ConnectionQuality::Good,
                    "Poor" => crate::presentation::events::ConnectionQuality::Poor,
                    "Recovering" => crate::presentation::events::ConnectionQuality::Recovering,
                    _ => crate::presentation::events::ConnectionQuality::Good,
                },
                reason,
            };

            if let Err(e) = app_handle.emit(EVENT_CONNECTION_QUALITY, payload) {
                log::error!("Failed to emit connection quality event: {}", e);
            }
        });
    });

    // Emit Starting only when a real connection/startup path is expected.
    // In keep-alive resume mode the WebSocket is already open, so emitting Starting
    // creates a false "reconnecting" blink in the UI.
    let can_resume_keep_alive = state
        .transcription_service
        .can_resume_keep_alive_connection()
        .await;
    if can_resume_keep_alive {
        log::info!(
            "[ReconnectDiag] skipping Starting status because keep-alive connection is resumable"
        );
    } else {
        log::debug!("Emitting status: Starting (stopped_via_hotkey: false)");
        let _ = app_handle.emit(
            EVENT_RECORDING_STATUS,
            RecordingStatusPayload {
                session_id,
                status: RecordingStatus::Starting,
                stopped_via_hotkey: false,
                mode: None,
            },
        );
    }

    // Пересоздаём audio capture только когда выбранное устройство реально изменилось.
    // Если cached capture сломался/устройство исчезло, ниже будет forced recreate + один retry.
    let selected_device = state.config.read().await.selected_audio_device.clone();
    if let Err(e) = state
        .ensure_audio_capture_device(selected_device.clone(), app_handle.clone(), false)
        .await
    {
        let error_msg = format!("Не удалось инициализировать устройство записи: {}", e);
        let stt_err = SttError::Configuration(e.to_string());
        let error_type = classify_transcription_error_type_from_stt(&stt_err);

        let payload = TranscriptionErrorPayload {
            session_id,
            error: error_msg.clone(),
            error_type,
            error_details: error_details_from_stt(&stt_err),
        };
        if let Err(emit_err) = app_handle.emit(EVENT_TRANSCRIPTION_ERROR, payload) {
            log::error!("Failed to emit transcription error event: {}", emit_err);
        }
        let _ = app_handle.emit(
            EVENT_RECORDING_STATUS,
            RecordingStatusPayload {
                session_id,
                status: RecordingStatus::Error,
                stopped_via_hotkey: false,
                mode: None,
            },
        );
        clear_active_transcription_session_id_if_current(state.inner(), session_id);
        return Err(error_msg);
    }

    // Start recording (async - WebSocket connect, audio capture start)
    let mut start_result = state
        .transcription_service
        .start_recording(
            on_partial.clone(),
            on_final.clone(),
            on_audio_level.clone(),
            on_audio_spectrum.clone(),
            on_error.clone(),
            on_connection_quality.clone(),
        )
        .await;

    if let Err(err) = &start_result {
        if is_audio_capture_start_failure(err) {
            log::warn!(
                "[StartLatencyDiag] audio capture start failed; forcing capture recreate and retrying once: {}",
                err
            );
            state.invalidate_audio_capture_device_cache().await;

            match state
                .ensure_audio_capture_device(selected_device.clone(), app_handle.clone(), true)
                .await
            {
                Ok(_) => {
                    start_result = state
                        .transcription_service
                        .start_recording(
                            on_partial,
                            on_final,
                            on_audio_level,
                            on_audio_spectrum,
                            on_error.clone(),
                            on_connection_quality.clone(),
                        )
                        .await;
                }
                Err(recreate_err) => {
                    start_result = Err(anyhow::anyhow!(
                        "Failed to recreate audio capture after start failure: {}",
                        recreate_err
                    ));
                }
            }
        }
    }

    if start_result
        .as_ref()
        .err()
        .map(is_audio_capture_start_failure)
        .unwrap_or(false)
    {
        state.invalidate_audio_capture_device_cache().await;
    }

    // Важно: если старт провалился ДО того, как провайдер успел вызвать on_error (например, упали на handshake/connection refused),
    // UI останется в Starting и будет ощущение "подключение идёт, но ничего не происходит".
    // Поэтому здесь явно отправляем error + status=Error тем же контрактом, что и в runtime-ошибках.
    if let Err(e) = start_result {
        // Стараемся извлечь исходную причину максимально надёжно (без парсинга строки).
        let stt = e
            .downcast_ref::<SttError>()
            .cloned()
            .unwrap_or_else(|| SttError::Internal(e.to_string()));
        let error = stt.to_string();
        let error_type = classify_transcription_error_type_from_stt(&stt);

        log::error!(
            "Failed to start recording: {} (type: {})",
            error,
            error_type
        );

        // Сначала transcription:error, потом recording:status=Error (во фронте есть логика suppression/retry).
        on_error(stt);
        clear_active_transcription_session_id_if_current(state.inner(), session_id);

        return Err(error);
    }

    // Emit Recording status after successful start
    log::debug!("Emitting status: Recording (stopped_via_hotkey: false)");
    let _ = app_handle.emit(
        EVENT_RECORDING_STATUS,
        RecordingStatusPayload {
            session_id,
            status: RecordingStatus::Recording,
            stopped_via_hotkey: false,
            mode: None,
        },
    );

    Ok("Recording started".to_string())
}

/// Stop recording voice
#[tauri::command]
pub async fn stop_recording(
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<String, String> {
    log::info!("Command: stop_recording");

    stop_recording_and_emit_idle(state.inner(), &app_handle, false).await
}

/// Get current recording status
#[tauri::command]
pub async fn get_recording_status(state: State<'_, AppState>) -> Result<RecordingStatus, String> {
    log::debug!("Command: get_recording_status");
    Ok(active_recording_status(state.inner()).await)
}

use tauri::{LogicalSize, PhysicalPosition, Position};

const RECORDING_WINDOW_EDGE_MARGIN_PX: i32 = 32;
const FULL_RECORDING_WINDOW_WIDTH: f64 = 460.0;
const FULL_RECORDING_WINDOW_HEIGHT: f64 = 330.0;
const MINI_RECORDING_WINDOW_WIDTH: f64 = 236.0;
const MINI_RECORDING_WINDOW_HEIGHT: f64 = 38.0;

enum RecordingWindowPlacement {
    Center,
    Mini {
        saved_position: Option<RecordingWindowPosition>,
    },
}

fn recording_window_placement_from_config(config: &AppConfig) -> RecordingWindowPlacement {
    if config.show_mini_recording_window {
        RecordingWindowPlacement::Mini {
            saved_position: config.recording_window_position.clone(),
        }
    } else {
        RecordingWindowPlacement::Center
    }
}

fn recording_window_size_from_config(config: &AppConfig) -> LogicalSize<f64> {
    if config.show_mini_recording_window {
        LogicalSize::new(MINI_RECORDING_WINDOW_WIDTH, MINI_RECORDING_WINDOW_HEIGHT)
    } else {
        LogicalSize::new(FULL_RECORDING_WINDOW_WIDTH, FULL_RECORDING_WINDOW_HEIGHT)
    }
}

fn clamp_axis(value: i32, min: i32, max: i32) -> i32 {
    if max < min {
        min
    } else {
        value.clamp(min, max)
    }
}

fn fit_position_to_monitor(
    position: PhysicalPosition<i32>,
    monitor_size: tauri::PhysicalSize<u32>,
    monitor_position: PhysicalPosition<i32>,
    window_size: tauri::PhysicalSize<u32>,
) -> PhysicalPosition<i32> {
    let min_x = monitor_position.x + RECORDING_WINDOW_EDGE_MARGIN_PX;
    let min_y = monitor_position.y + RECORDING_WINDOW_EDGE_MARGIN_PX;
    let max_x = monitor_position.x + monitor_size.width as i32
        - window_size.width as i32
        - RECORDING_WINDOW_EDGE_MARGIN_PX;
    let max_y = monitor_position.y + monitor_size.height as i32
        - window_size.height as i32
        - RECORDING_WINDOW_EDGE_MARGIN_PX;

    PhysicalPosition {
        x: clamp_axis(position.x, min_x, max_x),
        y: clamp_axis(position.y, min_y, max_y),
    }
}

fn calculate_recording_window_position(
    placement: &RecordingWindowPlacement,
    monitor_size: tauri::PhysicalSize<u32>,
    monitor_position: PhysicalPosition<i32>,
    window_size: tauri::PhysicalSize<u32>,
) -> PhysicalPosition<i32> {
    match placement {
        RecordingWindowPlacement::Center => PhysicalPosition {
            x: monitor_position.x + (monitor_size.width as i32 - window_size.width as i32) / 2,
            y: monitor_position.y + (monitor_size.height as i32 - window_size.height as i32) / 2,
        },
        RecordingWindowPlacement::Mini { saved_position } => {
            let default_position = PhysicalPosition {
                x: monitor_position.x + monitor_size.width as i32
                    - window_size.width as i32
                    - RECORDING_WINDOW_EDGE_MARGIN_PX,
                y: monitor_position.y + monitor_size.height as i32
                    - window_size.height as i32
                    - RECORDING_WINDOW_EDGE_MARGIN_PX,
            };
            let requested = saved_position
                .as_ref()
                .map(|p| PhysicalPosition { x: p.x, y: p.y })
                .unwrap_or(default_position);

            fit_position_to_monitor(requested, monitor_size, monitor_position, window_size)
        }
    }
}

async fn save_active_app_target_for_auto_paste(_state: &AppState) {
    #[cfg(target_os = "macos")]
    {
        match crate::infrastructure::auto_paste::get_active_app_target() {
            Some(target) => {
                *_state.last_focused_app_target.write().await = Some(target.clone());
                log::info!(
                    "Saved last focused auto-paste target: bundle_id={}, pid={}",
                    target.bundle_id,
                    target.pid
                );
            }
            None => {
                *_state.last_focused_app_target.write().await = None;
                log::warn!("No valid frontmost app target saved for auto-paste");
            }
        }
    }
}

/// Показывает recording окно с учетом пользовательского режима размещения.
pub fn show_window_with_recording_config(
    window: &Window,
    config: &AppConfig,
    state: &AppState,
) -> Result<(), String> {
    state.suppress_recording_window_position_save();
    window
        .set_size(recording_window_size_from_config(config))
        .map_err(|e| format!("Failed to set recording window size: {}", e))?;

    show_window_on_active_monitor_impl(
        || window.current_monitor(),
        || window.primary_monitor(),
        || window.outer_size(),
        |pos| window.set_position(pos),
        || window.show(),
        recording_window_placement_from_config(config),
    )
}

/// Показывает окно на активном мониторе (где находится курсор мыши) - для WebviewWindow
pub fn show_webview_window_on_active_monitor<R: tauri::Runtime>(
    window: &WebviewWindow<R>,
) -> Result<(), String> {
    show_window_on_active_monitor_impl(
        || window.current_monitor(),
        || window.primary_monitor(),
        || window.outer_size(),
        |pos| window.set_position(pos),
        || window.show(),
        RecordingWindowPlacement::Center,
    )
}

/// Показывает recording WebviewWindow с учетом пользовательского режима размещения.
pub fn show_webview_window_with_recording_config<R: tauri::Runtime>(
    window: &WebviewWindow<R>,
    config: &AppConfig,
    state: &AppState,
) -> Result<(), String> {
    state.suppress_recording_window_position_save();
    window
        .set_size(recording_window_size_from_config(config))
        .map_err(|e| format!("Failed to set recording window size: {}", e))?;

    show_window_on_active_monitor_impl(
        || window.current_monitor(),
        || window.primary_monitor(),
        || window.outer_size(),
        |pos| window.set_position(pos),
        || window.show(),
        recording_window_placement_from_config(config),
    )
}

/// Удерживает recording окно внутри видимой области текущего монитора после resize.
#[tauri::command]
pub fn fit_recording_window_to_visible_area(
    state: State<'_, AppState>,
    window: Window,
) -> Result<(), String> {
    let current_monitor = window
        .current_monitor()
        .map_err(|e| format!("Failed to get current monitor: {}", e))?
        .or_else(|| {
            log::warn!("current_monitor() вернул None, использую primary монитор");
            window.primary_monitor().ok().flatten()
        })
        .ok_or("No monitor found")?;

    let monitor_size = *current_monitor.size();
    let monitor_position = *current_monitor.position();
    let window_size = window
        .outer_size()
        .map_err(|e| format!("Failed to get window size: {}", e))?;
    let current_position = window
        .outer_position()
        .map_err(|e| format!("Failed to get window position: {}", e))?;
    let next_position = fit_position_to_monitor(
        current_position,
        monitor_size,
        monitor_position,
        window_size,
    );

    if next_position != current_position {
        state.suppress_recording_window_position_save();
        window
            .set_position(Position::Physical(next_position))
            .map_err(|e| format!("Failed to set window position: {}", e))?;
    }

    Ok(())
}

#[tauri::command]
pub fn set_recording_window_size(
    state: State<'_, AppState>,
    window: Window,
    width: f64,
    height: f64,
) -> Result<(), String> {
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return Err("Invalid recording window size".to_string());
    }

    state.suppress_recording_window_position_save();
    window
        .set_size(LogicalSize::new(width, height))
        .map_err(|e| format!("Failed to set recording window size: {}", e))?;

    fit_recording_window_to_visible_area(state, window)
}

/// Общая реализация для позиционирования окна на текущем мониторе
fn show_window_on_active_monitor_impl<F1, F2, F3, F4, F5>(
    get_current_monitor: F1,
    get_primary_monitor: F2,
    get_outer_size: F3,
    set_position: F4,
    show: F5,
    placement: RecordingWindowPlacement,
) -> Result<(), String>
where
    F1: FnOnce() -> tauri::Result<Option<tauri::Monitor>>,
    F2: FnOnce() -> tauri::Result<Option<tauri::Monitor>>,
    F3: FnOnce() -> tauri::Result<tauri::PhysicalSize<u32>>,
    F4: FnOnce(Position) -> tauri::Result<()>,
    F5: FnOnce() -> tauri::Result<()>,
{
    log::debug!("Определяем активный монитор для позиционирования окна...");

    // Определяем текущий монитор (где находится окно)
    let current_monitor = get_current_monitor()
        .map_err(|e| format!("Failed to get current monitor: {}", e))?
        .or_else(|| {
            log::warn!("current_monitor() вернул None, использую primary монитор");
            get_primary_monitor().ok().flatten()
        })
        .ok_or("No monitor found")?;

    // Получаем размеры и позицию монитора
    let monitor_size = current_monitor.size();
    let monitor_position = current_monitor.position();

    log::debug!(
        "Монитор: позиция ({}, {}), размер {}x{}",
        monitor_position.x,
        monitor_position.y,
        monitor_size.width,
        monitor_size.height
    );

    // Получаем размеры окна
    let window_size = get_outer_size().map_err(|e| format!("Failed to get window size: {}", e))?;

    let target_position = calculate_recording_window_position(
        &placement,
        *monitor_size,
        *monitor_position,
        window_size,
    );

    log::debug!(
        "Устанавливаю позицию окна: ({}, {})",
        target_position.x,
        target_position.y
    );

    // Устанавливаем позицию окна
    set_position(Position::Physical(target_position))
        .map_err(|e| format!("Failed to set window position: {}", e))?;

    // Показываем окно
    show().map_err(|e| e.to_string())?;

    log::info!("✅ Окно показано");

    Ok(())
}

#[cfg(test)]
mod snapshot_contract_tests {
    use super::{
        auto_paste_text_can_trigger_recording_hotkey, calculate_recording_window_position,
        hotkey_action_is_stale, is_audio_capture_start_failure, recording_hotkey_press_intent,
        recording_hotkey_release_intent, resolve_streaming_keyterms_update,
        should_cancel_hold_to_record_pending_start,
        should_hide_recording_window_immediately_on_hotkey_stop,
        should_ignore_hotkey_stop_after_start, should_show_recording_window_on_processing_hotkey,
        validate_auto_paste_target_for_focus, AppConfigSnapshotData, RecordingHotkeyDispatchIntent,
        RecordingWindowPlacement, SnapshotEnvelope, SttConfigSnapshotData,
    };
    use crate::domain::{
        AppConfig, AudioError, BackendStreamingProvider, RecordingStatus, RecordingWindowPosition,
        SttError, SttProviderType,
    };
    use crate::infrastructure::auto_paste::{AutoPasteTarget, VOICETEXT_BUNDLE_ID};
    use tauri::{PhysicalPosition, PhysicalSize};

    fn assert_absent(json: &str, needles: &[&str]) {
        for needle in needles {
            assert!(
                !json.contains(needle),
                "snapshot JSON must not contain `{}`; got: {}",
                needle,
                json
            );
        }
    }

    #[test]
    fn hotkey_stop_hides_mini_window_before_finalize_drain() {
        let mut config = AppConfig::default();
        config.show_mini_recording_window = true;
        config.hide_recording_window_on_hotkey = false;

        assert!(should_hide_recording_window_immediately_on_hotkey_stop(
            &config, false
        ));
    }

    #[test]
    fn hotkey_stop_hides_visible_regular_window_before_finalize_drain() {
        let mut config = AppConfig::default();
        config.show_mini_recording_window = false;
        config.hide_recording_window_on_hotkey = false;

        assert!(!should_hide_recording_window_immediately_on_hotkey_stop(
            &config, false
        ));
        assert!(should_hide_recording_window_immediately_on_hotkey_stop(
            &config, true
        ));

        config.hide_recording_window_on_hotkey = true;
        assert!(should_hide_recording_window_immediately_on_hotkey_stop(
            &config, false
        ));
    }

    #[test]
    fn processing_hotkey_reopens_hidden_or_mini_window() {
        let mut config = AppConfig::default();
        config.show_mini_recording_window = false;

        assert!(should_show_recording_window_on_processing_hotkey(
            &config, false
        ));
        assert!(!should_show_recording_window_on_processing_hotkey(
            &config, true
        ));

        config.hide_recording_window_on_hotkey = true;
        assert!(!should_show_recording_window_on_processing_hotkey(
            &config, false
        ));

        config.show_mini_recording_window = true;
        assert!(should_show_recording_window_on_processing_hotkey(
            &config, true
        ));
    }

    #[test]
    fn hotkey_start_stop_suppression_blocks_same_key_hold_only() {
        assert!(should_ignore_hotkey_stop_after_start(1_100, 2_500, 7, 7));
        assert!(!should_ignore_hotkey_stop_after_start(1_100, 2_500, 7, 8));
        assert!(!should_ignore_hotkey_stop_after_start(2_501, 2_500, 7, 7));
    }

    #[test]
    fn auto_paste_suppresses_bare_hotkey_only_when_text_can_type_it() {
        assert!(!auto_paste_text_can_trigger_recording_hotkey(
            "обычный текст",
            "Backquote"
        ));
        assert!(auto_paste_text_can_trigger_recording_hotkey(
            "text with ` code",
            "Backquote"
        ));
        assert!(!auto_paste_text_can_trigger_recording_hotkey(
            "x",
            "CmdOrCtrl+Shift+X"
        ));
        assert!(auto_paste_text_can_trigger_recording_hotkey("hello", "H"));
    }

    #[test]
    fn auto_paste_focus_target_validation_rejects_missing_or_invalid_target() {
        assert!(validate_auto_paste_target_for_focus(None).is_err());
        assert!(validate_auto_paste_target_for_focus(Some(AutoPasteTarget {
            bundle_id: VOICETEXT_BUNDLE_ID.to_string(),
            pid: 123,
        }))
        .is_err());
        assert!(validate_auto_paste_target_for_focus(Some(AutoPasteTarget {
            bundle_id: "com.example.App".to_string(),
            pid: 0,
        }))
        .is_err());

        let target = validate_auto_paste_target_for_focus(Some(AutoPasteTarget {
            bundle_id: "com.example.App".to_string(),
            pid: 123,
        }))
        .expect("target must be valid");

        assert_eq!(target.bundle_id, "com.example.App");
        assert_eq!(target.pid, 123);
    }

    #[test]
    fn start_retry_detects_audio_capture_errors_without_matching_strings() {
        let audio_err =
            anyhow::Error::new(AudioError::Capture("simulated start failure".to_string()))
                .context("Failed to start audio capture");
        assert!(is_audio_capture_start_failure(&audio_err));

        let stt_err = anyhow::Error::new(SttError::Internal("simulated stt failure".to_string()));
        assert!(!is_audio_capture_start_failure(&stt_err));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_physical_release_watch_maps_common_hotkeys() {
        assert_eq!(
            super::hotkey_key_code_for_physical_release_watch("Backquote"),
            Some(50)
        );
        assert_eq!(
            super::hotkey_key_code_for_physical_release_watch("CmdOrCtrl+Backquote"),
            Some(50)
        );
        assert_eq!(
            super::hotkey_key_code_for_physical_release_watch("CmdOrCtrl+Shift+X"),
            Some(7)
        );
        assert_eq!(
            super::hotkey_key_code_for_physical_release_watch("Shift+Digit1"),
            Some(18)
        );
    }

    #[test]
    fn mini_recording_window_uses_saved_position() {
        let placement = RecordingWindowPlacement::Mini {
            saved_position: Some(RecordingWindowPosition { x: 240, y: 160 }),
        };

        let position = calculate_recording_window_position(
            &placement,
            PhysicalSize {
                width: 1920,
                height: 1080,
            },
            PhysicalPosition { x: 0, y: 0 },
            PhysicalSize {
                width: 236,
                height: 38,
            },
        );

        assert_eq!(position, PhysicalPosition { x: 240, y: 160 });
    }

    #[test]
    fn mini_recording_window_keeps_saved_position_visible() {
        let placement = RecordingWindowPlacement::Mini {
            saved_position: Some(RecordingWindowPosition { x: 4, y: 8 }),
        };

        let position = calculate_recording_window_position(
            &placement,
            PhysicalSize {
                width: 1920,
                height: 1080,
            },
            PhysicalPosition { x: 0, y: 0 },
            PhysicalSize {
                width: 236,
                height: 38,
            },
        );

        assert_eq!(position, PhysicalPosition { x: 32, y: 32 });
    }

    #[test]
    fn app_config_snapshot_keeps_internal_secrets_out() {
        let env = SnapshotEnvelope {
            revision: "1".to_string(),
            data: AppConfigSnapshotData {
                microphone_sensitivity: 100,
                recording_hotkey: "CmdOrCtrl+Shift+X".to_string(),
                auto_copy_to_clipboard: true,
                auto_paste_text: false,
                play_completion_sound: false,
                hide_recording_window_on_hotkey: false,
                show_mini_recording_window: false,
                keep_recording_until_manual_stop: false,
                hold_to_record: false,
                selected_audio_device: None,
                recording_mode: crate::domain::RecordingMode::Dictation,
                openai_api_key: None,
            },
        };

        let json = serde_json::to_string(&env).expect("must serialize");

        // Жёсткий запрет на потенциально чувствительные поля + запрет на вложенный stt.
        assert_absent(
            &json,
            &[
                "backend_auth_token",
                "backend_url",
                "refresh_token",
                "access_token",
                "\"stt\"",
            ],
        );

        // И базовая проверка наличия ожидаемых ключей.
        let v: serde_json::Value = serde_json::from_str(&json).expect("must parse json");
        let data = v
            .get("data")
            .and_then(|x| x.as_object())
            .expect("data object");
        assert!(data.contains_key("microphone_sensitivity"));
        assert!(data.contains_key("recording_hotkey"));
        assert!(data.contains_key("auto_copy_to_clipboard"));
        assert!(data.contains_key("auto_paste_text"));
        assert!(data.contains_key("play_completion_sound"));
        assert!(data.contains_key("hide_recording_window_on_hotkey"));
        assert!(data.contains_key("show_mini_recording_window"));
        assert!(data.contains_key("keep_recording_until_manual_stop"));
        assert!(data.contains_key("hold_to_record"));
        assert!(data.contains_key("selected_audio_device"));
        assert!(data.contains_key("openai_api_key"));
    }

    #[test]
    fn hold_to_record_hotkey_intents_are_press_to_start_release_to_stop() {
        assert_eq!(
            recording_hotkey_press_intent(false, RecordingStatus::Recording),
            RecordingHotkeyDispatchIntent::Toggle
        );
        assert_eq!(
            recording_hotkey_release_intent(false, RecordingStatus::Recording),
            RecordingHotkeyDispatchIntent::Ignore
        );

        assert_eq!(
            recording_hotkey_press_intent(true, RecordingStatus::Idle),
            RecordingHotkeyDispatchIntent::Start
        );
        assert_eq!(
            recording_hotkey_press_intent(true, RecordingStatus::Recording),
            RecordingHotkeyDispatchIntent::Ignore
        );
        assert_eq!(
            recording_hotkey_press_intent(true, RecordingStatus::Processing),
            RecordingHotkeyDispatchIntent::Start
        );
        assert_eq!(
            recording_hotkey_release_intent(true, RecordingStatus::Starting),
            RecordingHotkeyDispatchIntent::Stop
        );
        assert_eq!(
            recording_hotkey_release_intent(true, RecordingStatus::Recording),
            RecordingHotkeyDispatchIntent::Stop
        );
        assert_eq!(
            recording_hotkey_release_intent(true, RecordingStatus::Idle),
            RecordingHotkeyDispatchIntent::Ignore
        );
    }

    #[test]
    fn hold_to_record_cancels_stale_release_and_pending_start() {
        assert!(hotkey_action_is_stale(1, 2));
        assert!(!hotkey_action_is_stale(2, 2));

        assert!(should_cancel_hold_to_record_pending_start(
            true,
            Some(1),
            2,
            false
        ));
        assert!(should_cancel_hold_to_record_pending_start(
            true,
            Some(2),
            2,
            true
        ));
        assert!(!should_cancel_hold_to_record_pending_start(
            true,
            Some(2),
            2,
            false
        ));
        assert!(!should_cancel_hold_to_record_pending_start(
            false,
            Some(1),
            2,
            true
        ));
        assert!(!should_cancel_hold_to_record_pending_start(
            true, None, 2, true
        ));
    }

    #[test]
    fn streaming_keyterms_update_prefers_new_field_over_legacy_alias() {
        assert_eq!(
            resolve_streaming_keyterms_update(
                Some(Some("new terms".to_string())),
                Some(Some("old terms".to_string()))
            ),
            Some(Some("new terms".to_string()))
        );
        assert_eq!(
            resolve_streaming_keyterms_update(Some(None), Some(Some("old terms".to_string()))),
            Some(None)
        );
        assert_eq!(
            resolve_streaming_keyterms_update(None, Some(Some("old terms".to_string()))),
            Some(Some("old terms".to_string()))
        );
        assert_eq!(resolve_streaming_keyterms_update(None, None), None);
    }

    #[test]
    fn stt_config_snapshot_is_public_and_does_not_leak_backend_token_or_url() {
        let env = SnapshotEnvelope {
            revision: "7".to_string(),
            data: SttConfigSnapshotData {
                provider: SttProviderType::Backend,
                backend_streaming_provider: BackendStreamingProvider::Deepgram,
                language: "ru".to_string(),
                auto_detect_language: false,
                enable_punctuation: true,
                filter_profanity: false,
                deepgram_api_key: None,
                assemblyai_api_key: None,
                model: None,
                keep_connection_alive: true,
                streaming_keyterms: None,
                deepgram_keyterms: None,
            },
        };

        let json = serde_json::to_string(&env).expect("must serialize");
        assert_absent(
            &json,
            &[
                "backend_auth_token",
                "backend_url",
                "refresh_token",
                "access_token",
            ],
        );

        // Проверяем, что JSON-форма стабильная (ожидаемые ключи присутствуют).
        let v: serde_json::Value = serde_json::from_str(&json).expect("must parse json");
        let data = v
            .get("data")
            .and_then(|x| x.as_object())
            .expect("data object");
        assert!(data.contains_key("provider"));
        assert!(data.contains_key("backend_streaming_provider"));
        assert!(data.contains_key("language"));
        assert!(data.contains_key("keep_connection_alive"));
        assert!(data.contains_key("streaming_keyterms"));
        assert!(data.contains_key("deepgram_keyterms"));
    }
}
/// Toggle window visibility
#[tauri::command]
pub async fn toggle_window(state: State<'_, AppState>, window: Window) -> Result<(), String> {
    log::info!("Command: toggle_window");

    if window.is_visible().map_err(|e| e.to_string())? {
        window.hide().map_err(|e| e.to_string())?;
    } else {
        // Перед показом окна сохраняем текущее активное приложение
        // (чтобы потом вставлять текст в правильное окно)
        save_active_app_target_for_auto_paste(state.inner()).await;

        let config = state.config.read().await.clone();
        show_window_with_recording_config(&window, &config, state.inner())?;

        // Сообщаем фронту, что окно показано (для надёжного reset UI).
        // Не используем focus, т.к. main на macOS может быть nonactivating NSPanel.
        let _ = window.emit(EVENT_RECORDING_WINDOW_SHOWN, ());
    }

    Ok(())
}

/// Toggle recording and show window if hidden
#[tauri::command]
pub async fn toggle_recording_with_window(
    state: State<'_, AppState>,
    window: Window,
    app_handle: AppHandle,
) -> Result<(), String> {
    log::info!("Command: toggle_recording_with_window");

    // Если пользователь не авторизован — не показываем recording окно.
    // Иначе получается странное поведение: окно может получить фокус, но UI в нём "none" (скрыт правилами windowMode).
    let is_authenticated = *state.is_authenticated.read().await;
    if !is_authenticated {
        log::info!(
            "toggle_recording_with_window: user not authenticated -> redirect to auth window"
        );
        show_auth_window(app_handle).await?;
        return Ok(());
    }

    // Переключаем состояние записи
    let current_status = active_recording_status(state.inner()).await;
    log::info!(
        "[HotkeyDiag] toggle_recording_with_window: current_status={:?}",
        current_status
    );

    match current_status {
        RecordingStatus::Idle => {
            state
                .recording_start_pending_after_stop
                .store(false, Ordering::SeqCst);
            let config = state.config.read().await.clone();
            let hide_window_on_hotkey =
                config.hide_recording_window_on_hotkey && !config.show_mini_recording_window;
            show_recording_window_for_hotkey_start(state.inner(), &app_handle, "command", None)
                .await?;

            // Запускаем запись
            if let Err(err) = start_recording(state.clone(), app_handle.clone()).await {
                if hide_window_on_hotkey {
                    if let Err(show_err) =
                        show_window_with_recording_config(&window, &config, state.inner())
                    {
                        log::warn!(
                            "Failed to show recording window after hotkey start error: {}",
                            show_err
                        );
                    } else {
                        let _ = window.emit(EVENT_RECORDING_WINDOW_SHOWN, ());
                    }
                }
                return Err(err);
            }
            if !hide_window_on_hotkey {
                emit_recording_window_shown(&app_handle);
            }
            log::info!("Recording started via hotkey");
        }
        RecordingStatus::Starting => {
            // Запись еще запускается - игнорируем повторное нажатие
            log::debug!("Ignoring toggle - recording is starting (WebSocket connecting, audio capture initializing)");
        }
        RecordingStatus::Recording => {
            if should_ignore_immediate_hotkey_stop_after_start(state.inner()) {
                log::info!(
                    "[HotkeyDiag] recording hotkey stop ignored: start protection window is active"
                );
                return Ok(());
            }
            let config = state.config.read().await.clone();
            let window_visible = window.is_visible().map_err(|e| e.to_string())?;
            let should_hide_immediately =
                should_hide_recording_window_immediately_on_hotkey_stop(&config, window_visible);
            log::info!(
                "Hotkey stop requested: window_visible={}, hide_immediately={}, show_mini={}, hide_on_hotkey={}",
                window_visible,
                should_hide_immediately,
                config.show_mini_recording_window,
                config.hide_recording_window_on_hotkey
            );
            let hidden_for_hotkey_stop = if should_hide_immediately && window_visible {
                let _ = window.emit(EVENT_RECORDING_WINDOW_WILL_HIDE_FOR_HOTKEY_STOP, ());
                tokio::time::sleep(Duration::from_millis(hotkey_stop_hide_ui_flush_ms(&config)))
                    .await;
                window.hide().map_err(|e| e.to_string())?;
                true
            } else {
                false
            };

            // Останавливаем запись
            if let Err(err) = stop_recording_and_emit_idle(state.inner(), &app_handle, true).await {
                if hidden_for_hotkey_stop {
                    if let Err(show_err) =
                        show_window_with_recording_config(&window, &config, state.inner())
                    {
                        log::warn!(
                            "Failed to restore recording window after hotkey stop error: {}",
                            show_err
                        );
                    } else {
                        let _ = window.emit(EVENT_RECORDING_WINDOW_SHOWN, ());
                    }
                }
                return Err(err);
            }

            log::info!("Recording stopped via hotkey");
        }
        RecordingStatus::Processing => {
            if matches!(
                *state.active_recording_mode.read().await,
                Some(RecordingMode::LiveTranslation)
            ) {
                log::info!("[HotkeyDiag] ignoring queued start while live translation is draining");
            } else {
                queue_recording_start_after_stop(
                    state.inner(),
                    app_handle.clone(),
                    "command",
                    None,
                );
            }
        }
        RecordingStatus::Error => {
            log::warn!("Cannot toggle recording - system is in error state");
        }
    }

    Ok(())
}

/// Internal version for calling from hotkey handler (without State wrapper)
pub async fn toggle_recording_with_window_internal(
    state: &AppState,
    window: tauri::WebviewWindow,
    app_handle: AppHandle,
    accepted_press_seq: u64,
    pre_hidden_for_hotkey_stop: bool,
) -> Result<(), String> {
    log::info!("toggle_recording_with_window_internal (from hotkey)");

    // Проверяем авторизацию - если не авторизован, показываем auth окно
    let is_authenticated = *state.is_authenticated.read().await;
    if !is_authenticated {
        log::info!("User not authenticated - showing auth window");
        if let Some(auth) = app_handle.get_webview_window("auth") {
            auth.show().map_err(|e| e.to_string())?;
            auth.set_focus().map_err(|e| e.to_string())?;
        }
        return Ok(());
    }

    let current_status = active_recording_status(state).await;
    log::info!(
        "[HotkeyDiag] toggle_recording_with_window_internal: current_status={:?}",
        current_status
    );

    match current_status {
        RecordingStatus::Idle => {
            state
                .recording_start_pending_after_stop
                .store(false, Ordering::SeqCst);
            let config = state.config.read().await.clone();
            let hide_window_on_hotkey =
                config.hide_recording_window_on_hotkey && !config.show_mini_recording_window;
            let window_visible = window.is_visible().map_err(|e| e.to_string())?;
            log::info!(
                "[HotkeyDiag] hotkey start path: provider={:?}, config_keep_alive={}, ttl_secs={}, show_mini={}, hide_on_hotkey={}, window_visible={}",
                config.stt.provider,
                config.stt.keep_connection_alive,
                config.stt.keep_alive_ttl_secs,
                config.show_mini_recording_window,
                config.hide_recording_window_on_hotkey,
                window_visible
            );

            let prepared_config = prepare_recording_hotkey_start(
                state,
                &app_handle,
                "global-hotkey",
                Some(accepted_press_seq),
            )
            .await;

            // ВАЖНО: стартуем запись на Rust-стороне.
            // Иначе, когда окно было скрыто, WebView/JS могут быть "усыплены" и не обработать event,
            // из-за чего хоткей откроет окно, но запись не стартует и UI останется в старом состоянии.
            let state_handle = app_handle
                .try_state::<AppState>()
                .ok_or_else(|| "AppState не доступен".to_string())?;
            if let Err(err) = start_recording(state_handle, app_handle.clone()).await {
                if hide_window_on_hotkey {
                    if let Err(show_err) =
                        show_webview_window_with_recording_config(&window, &config, state)
                    {
                        log::warn!(
                            "Failed to show recording window after hotkey start error: {}",
                            show_err
                        );
                    } else {
                        let _ = window.emit(EVENT_RECORDING_WINDOW_SHOWN, ());
                    }
                }
                return Err(err);
            }

            match apply_recording_window_for_hotkey_start(state, &app_handle, &prepared_config)
                .await
            {
                Ok(_) => {}
                Err(show_err) => {
                    log::warn!(
                        "Recording started, but recording window could not be updated after hotkey start: {}",
                        show_err
                    );
                }
            }

            if !hide_window_on_hotkey {
                emit_recording_window_shown(&app_handle);
            }
            log::info!("Recording started via hotkey (internal)");
        }
        RecordingStatus::Starting => {
            let config = state.config.read().await.clone();
            let hidden_for_hotkey_stop = if pre_hidden_for_hotkey_stop {
                log::info!(
                    "[HotkeyDiag] hotkey stop during Starting: window was already hidden before guard"
                );
                true
            } else {
                hide_recording_window_for_hotkey_stop_if_needed(
                    &window,
                    &config,
                    state,
                    accepted_press_seq,
                    "internal-starting",
                )
                .await?
            };

            let deadline = tokio::time::Instant::now()
                + Duration::from_millis(HOTKEY_STOP_WAIT_FOR_RECORDING_MS);
            loop {
                let status = active_recording_status(state).await;
                match status {
                    RecordingStatus::Recording => {
                        if let Err(err) =
                            stop_recording_and_emit_idle(state, &app_handle, true).await
                        {
                            if hidden_for_hotkey_stop {
                                if let Err(show_err) = show_webview_window_with_recording_config(
                                    &window, &config, state,
                                ) {
                                    log::warn!(
                                        "Failed to restore recording window after delayed hotkey stop error: {}",
                                        show_err
                                    );
                                } else {
                                    let _ = window.emit(EVENT_RECORDING_WINDOW_SHOWN, ());
                                }
                            }
                            return Err(err);
                        }
                        log::info!(
                            "[HotkeyDiag] recording stopped via hotkey after Starting completed"
                        );
                        return Ok(());
                    }
                    RecordingStatus::Starting => {
                        if tokio::time::Instant::now() >= deadline {
                            log::warn!(
                                "[HotkeyDiag] hotkey stop during Starting timed out waiting for Recording"
                            );
                            return Ok(());
                        }
                        tokio::time::sleep(Duration::from_millis(HOTKEY_STOP_WAIT_POLL_MS)).await;
                    }
                    RecordingStatus::Processing => {
                        log::info!(
                            "[HotkeyDiag] hotkey stop during Starting: service is already Processing"
                        );
                        return Ok(());
                    }
                    RecordingStatus::Idle => {
                        log::info!("[HotkeyDiag] hotkey stop during Starting: service became Idle");
                        return Ok(());
                    }
                    RecordingStatus::Error => {
                        log::info!(
                            "[HotkeyDiag] hotkey stop during Starting: service entered Error"
                        );
                        return Ok(());
                    }
                }
            }
        }
        RecordingStatus::Recording => {
            if should_ignore_immediate_hotkey_stop_after_start(state) {
                log::info!(
                    "[HotkeyDiag] recording hotkey stop ignored: start protection window is active"
                );
                return Ok(());
            }
            let config = state.config.read().await.clone();
            let hidden_for_hotkey_stop = if pre_hidden_for_hotkey_stop {
                log::info!("[HotkeyDiag] hotkey stop window was already hidden before guard");
                true
            } else {
                hide_recording_window_for_hotkey_stop_if_needed(
                    &window,
                    &config,
                    state,
                    accepted_press_seq,
                    "internal",
                )
                .await?
            };

            if let Err(err) = stop_recording_and_emit_idle(state, &app_handle, true).await {
                if hidden_for_hotkey_stop {
                    if let Err(show_err) =
                        show_webview_window_with_recording_config(&window, &config, state)
                    {
                        log::warn!(
                            "Failed to restore recording window after hotkey stop error: {}",
                            show_err
                        );
                    } else {
                        let _ = window.emit(EVENT_RECORDING_WINDOW_SHOWN, ());
                    }
                }
                return Err(err);
            }

            log::info!("Recording stopped via hotkey");
        }
        RecordingStatus::Processing => {
            if matches!(
                *state.active_recording_mode.read().await,
                Some(RecordingMode::LiveTranslation)
            ) {
                log::info!("[HotkeyDiag] ignoring queued start while live translation is draining");
            } else {
                queue_recording_start_after_stop(
                    state,
                    app_handle.clone(),
                    "global-hotkey",
                    Some(accepted_press_seq),
                );
            }
        }
        RecordingStatus::Error => {
            log::warn!("Cannot toggle recording - error state");
        }
    }

    Ok(())
}

/// Minimize window
#[tauri::command]
pub async fn minimize_window(window: Window) -> Result<(), String> {
    log::info!("Command: minimize_window");
    window.minimize().map_err(|e| e.to_string())?;
    Ok(())
}

//
// STT Configuration Commands
//

/// Update STT configuration
fn resolve_streaming_keyterms_update(
    streaming_keyterms: Option<Option<String>>,
    legacy_deepgram_keyterms: Option<Option<String>>,
) -> Option<Option<String>> {
    streaming_keyterms.or(legacy_deepgram_keyterms)
}

#[tauri::command]
pub async fn update_stt_config(
    state: State<'_, AppState>,
    app_handle: AppHandle,
    window: Window,
    provider: String,
    language: String,
    backend_streaming_provider: Option<String>,
    deepgram_api_key: Option<String>,
    assemblyai_api_key: Option<String>,
    model: Option<String>,
    // Важно: двойной Option позволяет отличить "поле не прислали" (None)
    // от "поле прислали как null" (Some(None)). Это нужно, чтобы
    // частичные обновления (например, только language) не затирали keyterms.
    streaming_keyterms: Option<Option<String>>,
    // Deprecated IPC alias. Оставляем на один миграционный период для старых окон/сборок.
    deepgram_keyterms: Option<Option<String>>,
) -> Result<(), String> {
    log::info!(
        "Command: update_stt_config - provider: {}, language: {}, model: {:?}",
        provider,
        language,
        model
    );

    let _guard = state.stt_config_guard.lock().await;

    // Выбор провайдера отключён — всегда используем Backend.
    // Параметр provider оставлен, чтобы не ломать совместимость API.
    let _ = provider;
    let provider_type = SttProviderType::Backend;

    // Снимаем текущее состояние для сравнения после сохранения
    let old_stt = {
        let config = state.config.read().await;
        config.stt.clone()
    };

    // Загружаем существующую конфигурацию из файла (если есть)
    let mut config = ConfigStore::load_config().await.unwrap_or_default();

    // Обновляем только переданные параметры
    config.provider = provider_type;
    config.language = language;
    if let Some(next_provider) = backend_streaming_provider {
        config.backend_streaming_provider = next_provider.parse::<BackendStreamingProvider>()?;
    }

    // Whisper/model больше не используем в backend-only архитектуре.
    let _ = model;
    config.model = None;

    // В backend-only режиме keep-alive полезен: это снижает latency при повторном старте записи,
    // потому что мы переиспользуем WebSocket соединение с нашим сервером.
    //
    // Важно: TTL держим чуть ниже backend audio idle timeout.
    // Иначе локальный keep-alive может попытаться переиспользовать WS на границе серверного закрытия.
    config.keep_connection_alive = true;
    if config.provider == crate::domain::SttProviderType::Backend {
        config.keep_alive_ttl_secs = crate::domain::BACKEND_KEEPALIVE_TTL_SECS;
    }

    log::debug!(
        "Setting keep_connection_alive={} for provider {:?}",
        config.keep_connection_alive,
        provider_type
    );

    // API ключи больше не используем в настройках (backend-only).
    let _ = deepgram_api_key;
    let _ = assemblyai_api_key;
    config.deepgram_api_key = None;
    config.assemblyai_api_key = None;

    // Keyterms для улучшения streaming-распознавания
    // - None: не меняем существующее значение
    // - Some(None): очищаем
    // - Some(Some(v)): устанавливаем v
    if let Some(next) = resolve_streaming_keyterms_update(streaming_keyterms, deepgram_keyterms) {
        config.streaming_keyterms = next;
    }

    // Обновляем конфигурацию в сервисе
    state
        .transcription_service
        .update_config(config.clone())
        .await
        .map_err(|e| e.to_string())?;

    // ВАЖНО: синхронизируем STT конфигурацию в AppConfig чтобы при сохранении
    // app_config.json не перезаписывались старые значения
    {
        let mut app_config = state.config.write().await;
        app_config.stt = config.clone();
    }

    // Сохраняем конфигурацию на диск (без API ключа)
    ConfigStore::save_config(&config)
        .await
        .map_err(|e| format!("Failed to save config: {}", e))?;

    // Синхронизация между окнами — бампим ревизию при любых изменениях STT конфига,
    // чтобы state-sync корректно подтягивал актуальный snapshot (включая keyterms и т.д.)
    let stt_changed = config.language != old_stt.language
        || config.streaming_keyterms != old_stt.streaming_keyterms
        || config.backend_streaming_provider != old_stt.backend_streaming_provider
        || config.provider != old_stt.provider;
    if stt_changed {
        let revision = AppState::bump_revision(&state.stt_config_revision).await;
        let _ = app_handle.emit(
            EVENT_STATE_SYNC_INVALIDATION,
            crate::presentation::StateSyncInvalidationPayload {
                topic: "stt-config".to_string(),
                revision,
                source_id: Some(window.label().to_string()),
                timestamp_ms: chrono::Utc::now().timestamp_millis(),
            },
        );
    }

    log::info!("STT configuration updated and saved successfully");
    Ok(())
}

//
// App Configuration Commands
//

/// Обёртка snapshot для state-sync протокола
#[derive(Debug, Clone, serde::Serialize)]
pub struct SnapshotEnvelope<T: serde::Serialize> {
    pub revision: String,
    pub data: T,
}
/// Snapshot app-config для frontend windows.
/// Может содержать user-entered API keys для Settings UI, поэтому не логировать целиком.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AppConfigSnapshotData {
    pub microphone_sensitivity: u8,
    pub recording_hotkey: String,
    pub auto_copy_to_clipboard: bool,
    pub auto_paste_text: bool,
    pub play_completion_sound: bool,
    pub hide_recording_window_on_hotkey: bool,
    pub show_mini_recording_window: bool,
    pub keep_recording_until_manual_stop: bool,
    pub hold_to_record: bool,
    pub selected_audio_device: Option<String>,
    pub recording_mode: crate::domain::RecordingMode,
    pub openai_api_key: Option<String>,
}
/// Get current application configuration + revision (for cross-window sync)
#[tauri::command]
pub async fn get_app_config_snapshot(
    state: State<'_, AppState>,
) -> Result<SnapshotEnvelope<AppConfigSnapshotData>, String> {
    log::debug!("Command: get_app_config_snapshot");
    let config = state.config.read().await.clone();
    let data = AppConfigSnapshotData {
        microphone_sensitivity: config.microphone_sensitivity,
        recording_hotkey: config.recording_hotkey,
        auto_copy_to_clipboard: config.auto_copy_to_clipboard,
        auto_paste_text: config.auto_paste_text,
        play_completion_sound: config.play_completion_sound,
        hide_recording_window_on_hotkey: config.hide_recording_window_on_hotkey,
        show_mini_recording_window: config.show_mini_recording_window,
        keep_recording_until_manual_stop: config.keep_recording_until_manual_stop,
        hold_to_record: config.hold_to_record,
        selected_audio_device: config.selected_audio_device,
        recording_mode: config.recording_mode,
        openai_api_key: config.openai_api_key,
    };
    let revision = state.app_config_revision.read().await.to_string();
    Ok(SnapshotEnvelope { revision, data })
}

/// Минимальный "public" снапшот stt-config для фронтенда.
///
/// Важно: не включаем backend_auth_token / backend_url (секреты), потому что снапшоты идут во все окна через IPC.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SttConfigSnapshotData {
    pub provider: crate::domain::SttProviderType,
    pub backend_streaming_provider: crate::domain::BackendStreamingProvider,
    pub language: String,
    pub auto_detect_language: bool,
    pub enable_punctuation: bool,
    pub filter_profanity: bool,
    pub deepgram_api_key: Option<String>,
    pub assemblyai_api_key: Option<String>,
    pub model: Option<String>,
    pub keep_connection_alive: bool,
    pub streaming_keyterms: Option<String>,
    pub deepgram_keyterms: Option<String>,
}

/// Get current STT configuration snapshot
#[tauri::command]
pub async fn get_stt_config_snapshot(
    state: State<'_, AppState>,
) -> Result<SnapshotEnvelope<SttConfigSnapshotData>, String> {
    log::debug!("Command: get_stt_config_snapshot");
    let config = state.transcription_service.get_config().await;
    let data = SttConfigSnapshotData {
        provider: config.provider,
        backend_streaming_provider: config.backend_streaming_provider,
        language: config.language,
        auto_detect_language: config.auto_detect_language,
        enable_punctuation: config.enable_punctuation,
        filter_profanity: config.filter_profanity,
        deepgram_api_key: config.deepgram_api_key,
        assemblyai_api_key: config.assemblyai_api_key,
        model: config.model,
        keep_connection_alive: config.keep_connection_alive,
        streaming_keyterms: config.streaming_keyterms.clone(),
        deepgram_keyterms: config.streaming_keyterms,
    };
    let revision = state.stt_config_revision.read().await.to_string();
    Ok(SnapshotEnvelope { revision, data })
}

/// Данные для snapshot авторизации
#[derive(Debug, Clone, serde::Serialize)]
pub struct AuthStateData {
    pub is_authenticated: bool,
}

/// Get current auth state snapshot
#[tauri::command]
pub async fn get_auth_state_snapshot(
    state: State<'_, AppState>,
) -> Result<SnapshotEnvelope<AuthStateData>, String> {
    log::trace!("Command: get_auth_state_snapshot");
    let is_authenticated = *state.is_authenticated.read().await;
    let revision = state.auth_state_revision.read().await.to_string();
    Ok(SnapshotEnvelope {
        revision,
        data: AuthStateData { is_authenticated },
    })
}

/// Полный снапшот auth-session (device_id + tokens).
///
/// В отличие от auth-state, этот снапшот содержит секреты (access/refresh),
/// поэтому его нельзя логировать/сериализовать в публичные конфиги.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AuthSessionSnapshotData {
    pub device_id: String,
    pub session: Option<AuthSessionSnapshotSessionData>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AuthSessionSnapshotSessionData {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub access_expires_at: String,
    pub refresh_expires_at: Option<String>,
    pub user: Option<AuthSessionSnapshotUserData>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AuthSessionSnapshotUserData {
    pub id: String,
    pub email: String,
    pub email_verified: bool,
}

fn ms_to_rfc3339(ms: i64) -> String {
    // Важно: если ms некорректный — fallback на epoch, чтобы не падать.
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
        .unwrap_or_else(|| chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap())
        .to_rfc3339()
}

/// Get current auth session snapshot (for cross-window sync).
#[tauri::command]
pub async fn get_auth_session_snapshot(
    state: State<'_, AppState>,
) -> Result<SnapshotEnvelope<AuthSessionSnapshotData>, String> {
    log::trace!("Command: get_auth_session_snapshot");

    let store = state.auth_store.read().await.clone();
    let data = AuthSessionSnapshotData {
        device_id: store.device_id,
        session: store.session.map(|s| AuthSessionSnapshotSessionData {
            access_token: s.access_token,
            refresh_token: s.refresh_token,
            access_expires_at: ms_to_rfc3339(s.access_expires_at_ms),
            refresh_expires_at: s.refresh_expires_at_ms.map(ms_to_rfc3339),
            user: s.user.map(|u| AuthSessionSnapshotUserData {
                id: u.id,
                email: u.email,
                email_verified: u.email_verified,
            }),
        }),
    };

    let revision = state.auth_session_revision.read().await.to_string();
    Ok(SnapshotEnvelope { revision, data })
}

/// Get current UI preferences snapshot
#[tauri::command]
pub async fn get_ui_preferences_snapshot(
    state: State<'_, AppState>,
) -> Result<SnapshotEnvelope<crate::domain::UiPreferences>, String> {
    log::debug!("Command: get_ui_preferences_snapshot");
    let data = state.ui_preferences.read().await.clone();
    let revision = state.ui_preferences_revision.read().await.to_string();
    Ok(SnapshotEnvelope { revision, data })
}

/// Обновить UI-настройки (тема, локаль) и уведомить все окна
#[tauri::command]
pub async fn update_ui_preferences(
    state: State<'_, AppState>,
    app_handle: AppHandle,
    window: Window,
    theme: String,
    locale: String,
    use_system_theme: Option<bool>,
) -> Result<(), String> {
    let use_system_theme = use_system_theme.unwrap_or(false);
    log::info!(
        "Command: update_ui_preferences - theme: {}, locale: {}, use_system_theme: {}",
        theme,
        locale,
        use_system_theme
    );

    {
        let current = state.ui_preferences.read().await;
        if current.theme == theme
            && current.locale == locale
            && current.use_system_theme == use_system_theme
        {
            return Ok(());
        }
    }

    let prefs = crate::domain::UiPreferences {
        theme: theme.clone(),
        locale: locale.clone(),
        use_system_theme,
    };

    // Сохраняем в state
    *state.ui_preferences.write().await = prefs.clone();

    // Сохраняем на диск
    ConfigStore::save_ui_preferences(&prefs)
        .await
        .map_err(|e| format!("Failed to save UI preferences: {}", e))?;

    // Bump revision и отправляем invalidation
    let revision = AppState::bump_revision(&state.ui_preferences_revision).await;
    let _ = app_handle.emit(
        EVENT_STATE_SYNC_INVALIDATION,
        crate::presentation::StateSyncInvalidationPayload {
            topic: "ui-preferences".to_string(),
            revision,
            source_id: Some(window.label().to_string()),
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        },
    );

    Ok(())
}

/// Update application configuration (e.g., microphone sensitivity, recording hotkey, auto-copy/paste)
#[tauri::command]
pub async fn update_app_config(
    state: State<'_, AppState>,
    app_handle: AppHandle,
    window: Window,
    microphone_sensitivity: Option<u8>,
    recording_hotkey: Option<String>,
    auto_copy_to_clipboard: Option<bool>,
    auto_paste_text: Option<bool>,
    play_completion_sound: Option<bool>,
    hide_recording_window_on_hotkey: Option<bool>,
    show_mini_recording_window: Option<bool>,
    keep_recording_until_manual_stop: Option<bool>,
    hold_to_record: Option<bool>,
    selected_audio_device: Option<String>,
    recording_mode: Option<crate::domain::RecordingMode>,
    openai_api_key: Option<String>,
) -> Result<(), String> {
    log::info!("Command: update_app_config - sensitivity: {:?}, hotkey: {:?}, auto_copy: {:?}, auto_paste: {:?}, completion_sound: {:?}, hide_window_on_hotkey: {:?}, mini_window: {:?}, manual_stop_only: {:?}, hold_to_record: {:?}, device: {:?}, mode: {:?}, openai_key: {}",
        microphone_sensitivity, recording_hotkey, auto_copy_to_clipboard, auto_paste_text, play_completion_sound, hide_recording_window_on_hotkey, show_mini_recording_window, keep_recording_until_manual_stop, hold_to_record, selected_audio_device, recording_mode, openai_api_key.as_ref().is_some_and(|key| !key.trim().is_empty()));

    // Защита от "тихих" провалов: если фронт случайно отправил snake_case ключи,
    // Tauri не сматчит аргументы, и сюда придут одни None.
    // Тогда лучше вернуть явную ошибку, чем сделать вид что всё ок.
    if microphone_sensitivity.is_none()
        && recording_hotkey.is_none()
        && auto_copy_to_clipboard.is_none()
        && auto_paste_text.is_none()
        && play_completion_sound.is_none()
        && hide_recording_window_on_hotkey.is_none()
        && show_mini_recording_window.is_none()
        && keep_recording_until_manual_stop.is_none()
        && hold_to_record.is_none()
        && selected_audio_device.is_none()
        && recording_mode.is_none()
        && openai_api_key.is_none()
    {
        return Err("update_app_config: не получены поля для обновления. Проверьте, что фронтенд отправляет args в camelCase (например microphoneSensitivity, recordingHotkey, autoCopyToClipboard, autoPasteText, playCompletionSound, hideRecordingWindowOnHotkey, showMiniRecordingWindow, keepRecordingUntilManualStop, holdToRecord, selectedAudioDevice, recordingMode, openaiApiKey).".to_string());
    }

    let mut config = state.config.write().await;
    let mut hotkey_changed = false;
    let mut any_changed = false;

    if let Some(sensitivity) = microphone_sensitivity {
        let clamped = sensitivity.min(200); // Ensure 0-200 range
        if config.microphone_sensitivity != clamped {
            log::info!(
                "Updating microphone sensitivity: {} -> {}",
                config.microphone_sensitivity,
                clamped
            );
            config.microphone_sensitivity = clamped;
            any_changed = true;
        }

        // Обновляем также в TranscriptionService для применения в реальном времени
        state
            .transcription_service
            .set_microphone_sensitivity(clamped)
            .await;
    }

    if let Some(new_hotkey) = recording_hotkey {
        if new_hotkey != config.recording_hotkey {
            // Валидируем что это корректная комбинация клавиш
            use tauri_plugin_global_shortcut::Shortcut;
            if new_hotkey.parse::<Shortcut>().is_err() {
                return Err(format!("Неверный формат горячей клавиши: {}", new_hotkey));
            }

            log::info!(
                "Updating recording hotkey: {} -> {}",
                config.recording_hotkey,
                new_hotkey
            );
            config.recording_hotkey = new_hotkey;
            hotkey_changed = true;
            any_changed = true;
        }
    }

    if let Some(auto_copy) = auto_copy_to_clipboard {
        if config.auto_copy_to_clipboard != auto_copy {
            config.auto_copy_to_clipboard = auto_copy;
            any_changed = true;
        }
    }

    if let Some(auto_paste) = auto_paste_text {
        if config.auto_paste_text != auto_paste {
            config.auto_paste_text = auto_paste;
            any_changed = true;
        }
    }

    if let Some(completion_sound) = play_completion_sound {
        if config.play_completion_sound != completion_sound {
            log::info!(
                "Updating play_completion_sound: {} -> {}",
                config.play_completion_sound,
                completion_sound
            );
            config.play_completion_sound = completion_sound;
            any_changed = true;
        }
    }

    if let Some(hide_window_on_hotkey) = hide_recording_window_on_hotkey {
        if config.hide_recording_window_on_hotkey != hide_window_on_hotkey {
            log::info!(
                "Updating hide_recording_window_on_hotkey: {} -> {}",
                config.hide_recording_window_on_hotkey,
                hide_window_on_hotkey
            );
            config.hide_recording_window_on_hotkey = hide_window_on_hotkey;
            any_changed = true;
        }
    }

    if let Some(show_mini_window) = show_mini_recording_window {
        if config.show_mini_recording_window != show_mini_window {
            log::info!(
                "Updating show_mini_recording_window: {} -> {}",
                config.show_mini_recording_window,
                show_mini_window
            );
            config.show_mini_recording_window = show_mini_window;
            any_changed = true;
        }
    }

    if let Some(manual_stop_only) = keep_recording_until_manual_stop {
        if config.keep_recording_until_manual_stop != manual_stop_only {
            log::info!(
                "Updating keep_recording_until_manual_stop: {} -> {}",
                config.keep_recording_until_manual_stop,
                manual_stop_only
            );
            config.keep_recording_until_manual_stop = manual_stop_only;
            any_changed = true;
        }
    }

    if let Some(hold_mode) = hold_to_record {
        if config.hold_to_record != hold_mode {
            log::info!(
                "Updating hold_to_record: {} -> {}",
                config.hold_to_record,
                hold_mode
            );
            config.hold_to_record = hold_mode;
            any_changed = true;
        }
    }

    if let Some(new_mode) = recording_mode {
        if config.recording_mode != new_mode {
            log::info!(
                "Updating recording_mode: {:?} -> {:?}",
                config.recording_mode,
                new_mode
            );
            config.recording_mode = new_mode;
            any_changed = true;
        }
    }

    if let Some(key) = openai_api_key {
        let normalized = key.trim().to_string();
        let next_key = if normalized.is_empty() {
            None
        } else {
            Some(normalized)
        };
        if config.openai_api_key != next_key {
            log::info!(
                "Updating openai_api_key: present {} -> {}",
                config
                    .openai_api_key
                    .as_ref()
                    .is_some_and(|value| !value.trim().is_empty()),
                next_key
                    .as_ref()
                    .is_some_and(|value| !value.trim().is_empty())
            );
            config.openai_api_key = next_key;
            any_changed = true;
        }
    }

    let mut device_changed = false;
    if let Some(device) = selected_audio_device {
        let normalized = device.trim().to_string();
        let device_opt = if normalized.is_empty() {
            None
        } else {
            Some(normalized)
        };

        // Проверяем изменилось ли устройство
        if config.selected_audio_device != device_opt {
            log::info!(
                "Updating selected_audio_device: {:?} -> {:?}",
                config.selected_audio_device,
                device_opt
            );
            config.selected_audio_device = device_opt;
            device_changed = true;
            any_changed = true;
        }
    }

    // Если ничего не менялось — выходим без лишнего I/O и invalidation
    if !any_changed {
        drop(config);
        log::info!("App config unchanged, skipping save");
        return Ok(());
    }

    log::info!("Saving app config to disk: sensitivity={}, hotkey={}, provider={:?}, language={}, device={:?}",
        config.microphone_sensitivity, config.recording_hotkey, config.stt.provider, config.stt.language, config.selected_audio_device);

    // Запоминаем selected_audio_device для применения после сохранения
    let device_to_apply = if device_changed {
        Some(config.selected_audio_device.clone())
    } else {
        None
    };

    // Сохраняем конфигурацию на диск
    ConfigStore::save_app_config(&config)
        .await
        .map_err(|e| format!("Failed to save app config: {}", e))?;

    // Если горячая клавиша изменилась - перерегистрируем её
    if hotkey_changed {
        drop(config); // освобождаем lock перед async операцией

        log::info!("Re-registering recording hotkey");

        // Перерегистрируем горячую клавишу
        register_recording_hotkey(state.clone(), app_handle.clone()).await?;
    } else {
        drop(config); // освобождаем lock если не было hotkey_changed
    }

    // Если устройство изменилось - пересоздаем audio capture
    if let Some(device_opt) = device_to_apply {
        log::info!("Applying changed audio device: {:?}", device_opt);

        state
            .recreate_audio_capture_with_device(device_opt.clone(), app_handle.clone())
            .await
            .map_err(|e| {
                log::error!("Failed to apply new audio device: {}", e);
                format!(
                    "Настройки сохранены, но не удалось применить новое устройство записи: {}",
                    e
                )
            })?;

        log::info!("Audio device changed and applied successfully");
    }

    // Синхронизация между окнами через state-sync
    let revision = AppState::bump_revision(&state.app_config_revision).await;
    let _ = app_handle.emit(
        EVENT_STATE_SYNC_INVALIDATION,
        crate::presentation::StateSyncInvalidationPayload {
            topic: "app-config".to_string(),
            revision,
            source_id: Some(window.label().to_string()),
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        },
    );

    log::info!("App configuration updated and saved successfully");
    Ok(())
}

//
// Microphone Test Commands
//

use crate::domain::AudioConfig;
use crate::infrastructure::audio::SystemAudioCapture;

/// Start microphone test
#[tauri::command]
pub async fn start_microphone_test(
    state: State<'_, AppState>,
    app_handle: AppHandle,
    sensitivity: Option<u8>,
    device_name: Option<String>,
) -> Result<(), String> {
    log::info!("Command: start_microphone_test - device: {:?}", device_name);

    #[cfg(target_os = "macos")]
    {
        use crate::infrastructure::microphone_permission::{
            microphone_permission_status, MicrophonePermissionStatus,
        };

        match microphone_permission_status() {
            MicrophonePermissionStatus::Authorized | MicrophonePermissionStatus::NotDetermined => {}
            _ => {
                return Err(
                    "Нет доступа к микрофону. Откройте macOS System Settings → Privacy & Security → Microphone и включите доступ для приложения."
                        .to_string(),
                );
            }
        }
    }

    let mut test_state = state.microphone_test.write().await;

    if test_state.is_testing {
        return Err("Microphone test already running".to_string());
    }

    // Создаем новый audio capture для теста с выбранным устройством
    let device_to_use = device_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);
    let mut capture = Box::new(
        match SystemAudioCapture::with_device(device_to_use.clone()) {
            Ok(capture) => capture,
            Err(crate::domain::AudioError::DeviceNotFound(e)) if device_to_use.is_some() => {
                log::warn!(
                    "Microphone test requested unavailable device ({}). Falling back to default input device.",
                    e
                );
                SystemAudioCapture::new().map_err(|fallback_err| {
                    format!("Failed to create audio capture: {}", fallback_err)
                })?
            }
            Err(e) => return Err(format!("Failed to create audio capture: {}", e)),
        },
    );

    // Инициализируем захват
    capture
        .initialize(AudioConfig::default())
        .await
        .map_err(|e| format!("Failed to initialize audio capture: {}", e))?;

    // Сбрасываем буфер
    test_state.buffer.lock().await.clear();

    // Получаем ссылку на shared buffer
    let buffer_for_task = test_state.buffer.clone();

    // Используем переданную чувствительность или загружаем из сохраненной конфигурации
    let sensitivity = match sensitivity {
        Some(s) => s.min(200),
        None => state.config.read().await.microphone_sensitivity,
    };

    log::info!(
        "Starting microphone test with sensitivity: {}%",
        sensitivity
    );

    // Создаем канал для передачи данных из callback
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

    let on_chunk = Arc::new(move |chunk: crate::domain::AudioChunk| {
        let _ = tx.send(chunk);
    });

    // Запускаем обработчик чанков в async контексте
    let app_handle_clone = app_handle.clone();

    tokio::spawn(async move {
        // Вычисляем коэффициент усиления (та же логика что в TranscriptionService)
        let requested_gain = if sensitivity <= 100 {
            // 0-100% → 0.0x-1.0x (приглушение/нормальный уровень)
            sensitivity as f32 / 100.0
        } else {
            // 100-200% → 1.0x-5.0x (усиление для тихих микрофонов)
            1.0 + (sensitivity - 100) as f32 / 100.0 * 4.0
        };

        log::info!(
            "Microphone test: sensitivity={}%, requested_gain={:.2}x",
            sensitivity,
            requested_gain
        );

        while let Some(chunk) = rx.recv().await {
            // Вычисляем уровень громкости ДО усиления
            let max_amplitude: i32 = chunk
                .data
                .iter()
                .map(|&s| (s as i32).abs())
                .max()
                .unwrap_or(0);
            let normalized_level = (max_amplitude as f32 / 32767.0).sqrt().min(1.0);

            // Отправляем событие в UI (показываем уровень ДО усиления для честной индикации)
            let _ = app_handle_clone.emit(
                EVENT_MICROPHONE_TEST_LEVEL,
                MicrophoneTestLevelPayload {
                    level: normalized_level,
                },
            );

            // Простой limiter: если requested_gain приводит к клиппингу — уменьшаем gain для этого чанка.
            let effective_gain = if max_amplitude <= 0 {
                requested_gain
            } else {
                let headroom = 0.98_f32;
                let limiter_gain = (32767.0 * headroom) / (max_amplitude as f32);
                requested_gain.min(limiter_gain)
            };

            // Применяем gain к каждому сэмплу с защитой от clipping
            let amplified_data: Vec<i16> = chunk
                .data
                .iter()
                .map(|&sample| {
                    let amplified = (sample as f32 * effective_gain).clamp(-32767.0, 32767.0);
                    amplified as i16
                })
                .collect();

            // Сохраняем усиленный звук в буфер (для честного воспроизведения)
            let mut buffer = buffer_for_task.lock().await;
            buffer.extend_from_slice(&amplified_data);
            // Ограничиваем размер буфера (максимум 5 секунд = 80000 samples @ 16kHz)
            let buffer_len = buffer.len();
            if buffer_len > 80000 {
                buffer.drain(0..buffer_len - 80000);
            }
        }
    });

    // Запускаем захват
    capture
        .start_capture(on_chunk)
        .await
        .map_err(|e| format!("Failed to start audio capture: {}", e))?;

    test_state.capture = Some(capture);
    test_state.is_testing = true;

    log::info!("Microphone test started");
    Ok(())
}

/// Stop microphone test and return recorded audio
#[tauri::command]
pub async fn stop_microphone_test(state: State<'_, AppState>) -> Result<Vec<i16>, String> {
    log::info!("Command: stop_microphone_test");

    let mut test_state = state.microphone_test.write().await;

    if !test_state.is_testing {
        return Err("Microphone test not running".to_string());
    }

    // Останавливаем захват
    if let Some(mut capture) = test_state.capture.take() {
        capture
            .stop_capture()
            .await
            .map_err(|e| format!("Failed to stop audio capture: {}", e))?;
    }

    test_state.is_testing = false;

    // Возвращаем копию буфера и очищаем его
    let mut buffer_guard = test_state.buffer.lock().await;
    let buffer = buffer_guard.clone();
    buffer_guard.clear();
    drop(buffer_guard);

    log::info!(
        "Microphone test stopped, buffer size: {} samples",
        buffer.len()
    );
    Ok(buffer)
}

//
// Hotkey Management Commands
//

const RECORDING_HOTKEY_DEBOUNCE_MS: u64 = 450;
const RECORDING_HOTKEY_MIN_REPRESS_MS: u64 = 120;
const RECORDING_HOTKEY_MIN_RELEASE_TO_REPRESS_MS: u64 = 50;
const RECORDING_HOTKEY_MISSED_RELEASE_ACCEPT_MS: u64 = 300;
const RECORDING_HOTKEY_MISSED_RELEASE_CONFIRM_MS: u64 = 220;
const RECORDING_HOTKEY_RAW_REPEAT_GAP_MS: u64 = 250;
const RECORDING_HOTKEY_STALE_PRESS_MS: u64 = 10_000;
const RECORDING_HOTKEY_RELEASE_GRACE_MS: u64 = 700;
const HOTKEY_PENDING_START_TIMEOUT_MS: u64 = 3_000;
const HOTKEY_PENDING_START_POLL_MS: u64 = 25;
const AUTO_PASTE_HOTKEY_SUPPRESSION_MS: u64 = 450;
const AUTO_PASTE_HOTKEY_SUPPRESSION_TAIL_MS: u64 = 150;
const AUTO_PASTE_FOCUS_VERIFY_TIMEOUT_MS: u64 = 300;
const AUTO_PASTE_FOCUS_VERIFY_POLL_MS: u64 = 50;

async fn start_recording_after_queued_hotkey_idle(
    state: State<'_, AppState>,
    app_handle: AppHandle,
    source: &'static str,
    stop_suppression_press_seq: Option<u64>,
) -> Result<(), String> {
    let toggle_guard = state.recording_hotkey_toggle_guard.clone();
    let _toggle_guard = toggle_guard.lock().await;

    let status = active_recording_status(state.inner()).await;
    if status != RecordingStatus::Idle {
        log::info!(
            "[HotkeyDiag] queued start cancelled: status changed before start (source={}, status={:?})",
            source,
            status
        );
        return Ok(());
    }

    let config = state.config.read().await.clone();
    let hide_window_on_hotkey =
        config.hide_recording_window_on_hotkey && !config.show_mini_recording_window;

    log::info!(
        "[HotkeyDiag] executing queued start after stop: source={}, show_mini={}, hide_on_hotkey={}",
        source,
        config.show_mini_recording_window,
        config.hide_recording_window_on_hotkey
    );

    let prepared_config = prepare_recording_hotkey_start(
        state.inner(),
        &app_handle,
        source,
        stop_suppression_press_seq,
    )
    .await;

    if let Err(err) = start_recording(state.clone(), app_handle.clone()).await {
        if hide_window_on_hotkey {
            if let Some(window) = app_handle.get_webview_window("main") {
                if let Err(show_err) =
                    show_webview_window_with_recording_config(&window, &config, state.inner())
                {
                    log::warn!(
                        "Failed to show recording window after queued hotkey start error: {}",
                        show_err
                    );
                } else {
                    let _ = window.emit(EVENT_RECORDING_WINDOW_SHOWN, ());
                }
            }
        }
        return Err(err);
    }

    match apply_recording_window_for_hotkey_start(state.inner(), &app_handle, &prepared_config)
        .await
    {
        Ok(_) => {}
        Err(show_err) => {
            log::warn!(
                "Recording started, but recording window could not be updated after queued hotkey start: {}",
                show_err
            );
        }
    }

    if !hide_window_on_hotkey {
        emit_recording_window_shown(&app_handle);
    }
    log::info!("[HotkeyDiag] queued start after stop completed");
    Ok(())
}

fn queue_recording_start_after_stop(
    state: &AppState,
    app_handle: AppHandle,
    source: &'static str,
    stop_suppression_press_seq: Option<u64>,
) {
    if state
        .recording_start_pending_after_stop
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        log::info!(
            "[HotkeyDiag] queued start after stop already pending (source={})",
            source
        );
        return;
    }

    log::info!(
        "[HotkeyDiag] queued start after stop while current stop/finalize is processing (source={})",
        source
    );

    let _ = tauri::async_runtime::spawn(async move {
        let deadline =
            tokio::time::Instant::now() + Duration::from_millis(HOTKEY_PENDING_START_TIMEOUT_MS);

        loop {
            let Some(state) = app_handle.try_state::<AppState>() else {
                log::warn!("[HotkeyDiag] queued start cancelled: AppState is unavailable");
                return;
            };

            let hold_to_record = state.config.read().await.hold_to_record;
            if should_cancel_hold_to_record_pending_start(
                hold_to_record,
                stop_suppression_press_seq,
                state
                    .recording_hotkey_accepted_press_seq
                    .load(Ordering::SeqCst),
                state
                    .recording_hotkey_released_since_press
                    .load(Ordering::SeqCst),
            ) {
                state
                    .recording_start_pending_after_stop
                    .store(false, Ordering::SeqCst);
                log::info!(
                    "[HotkeyDiag] queued hold-to-record start cancelled before Idle (source={}, press_seq={:?})",
                    source,
                    stop_suppression_press_seq
                );
                return;
            }

            let status = active_recording_status(state.inner()).await;
            match status {
                RecordingStatus::Idle => break,
                RecordingStatus::Processing | RecordingStatus::Starting => {
                    if tokio::time::Instant::now() >= deadline {
                        state
                            .recording_start_pending_after_stop
                            .store(false, Ordering::SeqCst);
                        log::warn!(
                            "[HotkeyDiag] queued start timed out waiting for Idle (source={}, last_status={:?})",
                            source,
                            status
                        );
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(HOTKEY_PENDING_START_POLL_MS)).await;
                }
                RecordingStatus::Recording => {
                    state
                        .recording_start_pending_after_stop
                        .store(false, Ordering::SeqCst);
                    log::info!(
                        "[HotkeyDiag] queued start cancelled: already recording (source={})",
                        source
                    );
                    return;
                }
                RecordingStatus::Error => {
                    state
                        .recording_start_pending_after_stop
                        .store(false, Ordering::SeqCst);
                    log::warn!(
                        "[HotkeyDiag] queued start cancelled: service is in Error state (source={})",
                        source
                    );
                    return;
                }
            }
        }

        let Some(state) = app_handle.try_state::<AppState>() else {
            log::warn!("[HotkeyDiag] queued start cancelled: AppState is unavailable at start");
            return;
        };
        state
            .recording_start_pending_after_stop
            .store(false, Ordering::SeqCst);

        let hold_to_record = state.config.read().await.hold_to_record;
        if should_cancel_hold_to_record_pending_start(
            hold_to_record,
            stop_suppression_press_seq,
            state
                .recording_hotkey_accepted_press_seq
                .load(Ordering::SeqCst),
            state
                .recording_hotkey_released_since_press
                .load(Ordering::SeqCst),
        ) {
            log::info!(
                "[HotkeyDiag] queued hold-to-record start cancelled at Idle (source={}, press_seq={:?})",
                source,
                stop_suppression_press_seq
            );
            return;
        }

        if let Err(err) = start_recording_after_queued_hotkey_idle(
            state,
            app_handle.clone(),
            source,
            stop_suppression_press_seq,
        )
        .await
        {
            log::error!(
                "[HotkeyDiag] queued start after stop failed (source={}): {}",
                source,
                err
            );
        }
    });
}

fn hotkey_modifier_blocks_text_input_trigger(part: &str) -> bool {
    hotkey_part_is_modifier(part) && !part.eq_ignore_ascii_case("shift")
}

fn hotkey_part_to_text_char(part: &str, shifted: bool) -> Option<char> {
    let normalized = part.trim();
    match normalized {
        "`" | "Backquote" => Some(if shifted { '~' } else { '`' }),
        "-" | "Minus" => Some(if shifted { '_' } else { '-' }),
        "=" | "Equal" => Some(if shifted { '+' } else { '=' }),
        "[" | "BracketLeft" => Some(if shifted { '{' } else { '[' }),
        "]" | "BracketRight" => Some(if shifted { '}' } else { ']' }),
        "\\" | "Backslash" | "IntlBackslash" => Some(if shifted { '|' } else { '\\' }),
        ";" | "Semicolon" => Some(if shifted { ':' } else { ';' }),
        "'" | "Quote" => Some(if shifted { '"' } else { '\'' }),
        "," | "Comma" => Some(if shifted { '<' } else { ',' }),
        "." | "Period" => Some(if shifted { '>' } else { '.' }),
        "/" | "Slash" => Some(if shifted { '?' } else { '/' }),
        "Space" => Some(' '),
        _ if normalized.len() == 1 => normalized.chars().next(),
        _ if normalized.starts_with("Digit") && normalized.len() == 6 => normalized.chars().last(),
        _ => None,
    }
}

fn text_contains_hotkey_char(text: &str, hotkey_char: char) -> bool {
    if hotkey_char.is_ascii_alphabetic() {
        text.chars().any(|ch| ch.eq_ignore_ascii_case(&hotkey_char))
    } else {
        text.contains(hotkey_char)
    }
}

fn auto_paste_text_can_trigger_recording_hotkey(text: &str, hotkey: &str) -> bool {
    if text.is_empty() {
        return false;
    }

    let parts: Vec<&str> = hotkey
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    if parts.is_empty() {
        return true;
    }

    if parts
        .iter()
        .any(|part| hotkey_modifier_blocks_text_input_trigger(part))
    {
        return false;
    }

    let shifted = parts.iter().any(|part| part.eq_ignore_ascii_case("shift"));
    let key_parts: Vec<&str> = parts
        .iter()
        .copied()
        .filter(|part| !part.eq_ignore_ascii_case("shift"))
        .collect();
    if key_parts.len() != 1 {
        return true;
    }

    hotkey_part_to_text_char(key_parts[0], shifted)
        .map(|hotkey_char| text_contains_hotkey_char(text, hotkey_char))
        .unwrap_or(true)
}

fn auto_paste_hotkey_suppression_duration(text: &str, hotkey: &str) -> Duration {
    if auto_paste_text_can_trigger_recording_hotkey(text, hotkey) {
        Duration::from_millis(AUTO_PASTE_HOTKEY_SUPPRESSION_MS)
    } else {
        Duration::from_millis(0)
    }
}

fn validate_auto_paste_target_for_focus(
    target: Option<AutoPasteTarget>,
) -> Result<AutoPasteTarget, String> {
    let Some(target) = target else {
        return Err(
            "Auto-paste target is unavailable; refusing to paste into current focus".to_string(),
        );
    };

    let Some(normalized_target) = crate::infrastructure::auto_paste::normalize_auto_paste_target(
        target.bundle_id.clone(),
        target.pid,
    ) else {
        return Err(format!(
            "Invalid auto-paste target; refusing to paste: bundle_id={}, pid={}",
            target.bundle_id, target.pid
        ));
    };

    Ok(normalized_target)
}

#[cfg(target_os = "macos")]
async fn wait_for_auto_paste_target_focus(target: &AutoPasteTarget) -> bool {
    let max_attempts =
        (AUTO_PASTE_FOCUS_VERIFY_TIMEOUT_MS / AUTO_PASTE_FOCUS_VERIFY_POLL_MS).max(1);

    for attempt in 0..=max_attempts {
        if crate::infrastructure::auto_paste::frontmost_app_matches_target(target) {
            log::info!(
                "Auto-paste target focused: bundle_id={}, pid={}, attempt={}",
                target.bundle_id,
                target.pid,
                attempt
            );
            return true;
        }

        if attempt < max_attempts {
            tokio::time::sleep(Duration::from_millis(AUTO_PASTE_FOCUS_VERIFY_POLL_MS)).await;
        }
    }

    false
}

#[cfg(target_os = "macos")]
fn macos_letter_key_code(ch: char) -> Option<u16> {
    match ch.to_ascii_uppercase() {
        'A' => Some(0),
        'S' => Some(1),
        'D' => Some(2),
        'F' => Some(3),
        'H' => Some(4),
        'G' => Some(5),
        'Z' => Some(6),
        'X' => Some(7),
        'C' => Some(8),
        'V' => Some(9),
        'B' => Some(11),
        'Q' => Some(12),
        'W' => Some(13),
        'E' => Some(14),
        'R' => Some(15),
        'Y' => Some(16),
        'T' => Some(17),
        'O' => Some(31),
        'U' => Some(32),
        'I' => Some(34),
        'P' => Some(35),
        'L' => Some(37),
        'J' => Some(38),
        'K' => Some(40),
        'N' => Some(45),
        'M' => Some(46),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
fn macos_digit_key_code(ch: char) -> Option<u16> {
    match ch {
        '1' => Some(18),
        '2' => Some(19),
        '3' => Some(20),
        '4' => Some(21),
        '6' => Some(22),
        '5' => Some(23),
        '9' => Some(25),
        '7' => Some(26),
        '8' => Some(28),
        '0' => Some(29),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
fn macos_hotkey_part_key_code(part: &str) -> Option<u16> {
    let normalized = part.trim();
    let upper = normalized.to_ascii_uppercase();

    if upper.len() == 1 {
        let ch = upper.chars().next()?;
        return macos_letter_key_code(ch).or_else(|| macos_digit_key_code(ch));
    }

    if upper.starts_with("KEY") && upper.len() == 4 {
        return upper.chars().last().and_then(macos_letter_key_code);
    }

    if upper.starts_with("DIGIT") && upper.len() == 6 {
        return upper.chars().last().and_then(macos_digit_key_code);
    }

    match upper.as_str() {
        "`" | "BACKQUOTE" | "GRAVE" | "GRAVEACCENT" => Some(50),
        "-" | "MINUS" => Some(27),
        "=" | "EQUAL" => Some(24),
        "[" | "BRACKETLEFT" | "LEFTBRACKET" => Some(33),
        "]" | "BRACKETRIGHT" | "RIGHTBRACKET" => Some(30),
        "\\" | "BACKSLASH" | "INTLBACKSLASH" => Some(42),
        ";" | "SEMICOLON" => Some(41),
        "'" | "QUOTE" => Some(39),
        "," | "COMMA" => Some(43),
        "." | "PERIOD" => Some(47),
        "/" | "SLASH" => Some(44),
        "SPACE" => Some(49),
        "TAB" => Some(48),
        "ENTER" | "RETURN" => Some(36),
        "ESC" | "ESCAPE" => Some(53),
        "BACKSPACE" | "DELETE" => Some(51),
        "ARROWLEFT" | "LEFT" => Some(123),
        "ARROWRIGHT" | "RIGHT" => Some(124),
        "ARROWDOWN" | "DOWN" => Some(125),
        "ARROWUP" | "UP" => Some(126),
        "F1" => Some(122),
        "F2" => Some(120),
        "F3" => Some(99),
        "F4" => Some(118),
        "F5" => Some(96),
        "F6" => Some(97),
        "F7" => Some(98),
        "F8" => Some(100),
        "F9" => Some(101),
        "F10" => Some(109),
        "F11" => Some(103),
        "F12" => Some(111),
        _ => None,
    }
}

fn hotkey_part_is_modifier(part: &str) -> bool {
    matches!(
        part.to_ascii_lowercase().as_str(),
        "shift"
            | "cmd"
            | "command"
            | "cmdorctrl"
            | "ctrl"
            | "control"
            | "alt"
            | "option"
            | "super"
            | "meta"
    )
}

#[cfg(target_os = "macos")]
fn hotkey_key_code_for_physical_release_watch(hotkey: &str) -> Option<u16> {
    let key_parts: Vec<&str> = hotkey
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty() && !hotkey_part_is_modifier(part))
        .collect();

    if key_parts.len() != 1 {
        return None;
    }

    macos_hotkey_part_key_code(key_parts[0])
}

#[cfg(not(target_os = "macos"))]
fn hotkey_key_code_for_physical_release_watch(_hotkey: &str) -> Option<u16> {
    None
}

#[cfg(target_os = "macos")]
fn macos_physical_key_is_pressed(key_code: u16) -> bool {
    unsafe {
        // 0 is kCGEventSourceStateCombinedSessionState.
        CGEventSourceKeyState(0, key_code)
    }
}

#[cfg(target_os = "macos")]
fn schedule_recording_hotkey_physical_release_watch(
    app_clone: AppHandle,
    accepted_press_seq: u64,
    key_code: Option<u16>,
) {
    let Some(key_code) = key_code else {
        return;
    };

    let _ = tauri::async_runtime::spawn(async move {
        let deadline =
            tokio::time::Instant::now() + Duration::from_millis(HOTKEY_PHYSICAL_RELEASE_TIMEOUT_MS);

        loop {
            tokio::time::sleep(Duration::from_millis(HOTKEY_PHYSICAL_RELEASE_POLL_MS)).await;

            if macos_physical_key_is_pressed(key_code) {
                if tokio::time::Instant::now() < deadline {
                    continue;
                }
                log::debug!(
                    "[HotkeyDiag] physical release watch timed out (accepted_press_seq={}, key_code={})",
                    accepted_press_seq,
                    key_code
                );
                return;
            }

            let Some(state) = app_clone.try_state::<crate::presentation::state::AppState>() else {
                return;
            };
            let state_inner = state.inner();
            let current_press_seq = state_inner
                .recording_hotkey_accepted_press_seq
                .load(Ordering::SeqCst);
            if current_press_seq != accepted_press_seq {
                log::debug!(
                    "[HotkeyDiag] physical release watch ignored stale press seq (watch={}, current={})",
                    accepted_press_seq,
                    current_press_seq
                );
                return;
            }

            let release_ms = now_ms_u64();
            state_inner
                .recording_hotkey_released_since_press
                .store(true, Ordering::SeqCst);
            state_inner
                .recording_hotkey_last_release_ms
                .store(release_ms, Ordering::SeqCst);
            state_inner
                .recording_hotkey_release_generation
                .fetch_add(1, Ordering::SeqCst);
            state_inner
                .recording_hotkey_is_pressed
                .store(false, Ordering::SeqCst);
            log::debug!(
                "[HotkeyDiag] hotkey latch cleared by physical key release watch (accepted_press_seq={}, key_code={})",
                accepted_press_seq,
                key_code
            );
            return;
        }
    });
}

#[cfg(not(target_os = "macos"))]
fn schedule_recording_hotkey_physical_release_watch(
    _app_clone: AppHandle,
    _accepted_press_seq: u64,
    _key_code: Option<u16>,
) {
}

fn accept_recording_hotkey_press(state: &AppState, accepted_at_ms: u64) -> u64 {
    state
        .recording_hotkey_released_since_press
        .store(false, Ordering::SeqCst);
    state
        .last_recording_hotkey_ms
        .store(accepted_at_ms, Ordering::Relaxed);
    state
        .recording_hotkey_accepted_press_seq
        .fetch_add(1, Ordering::SeqCst)
        + 1
}

fn dispatch_recording_hotkey_toggle(app_clone: AppHandle, accepted_press_seq: u64) {
    let _ = tauri::async_runtime::spawn(async move {
        let Some(state) = app_clone.try_state::<crate::presentation::state::AppState>() else {
            log::warn!("Recording hotkey ignored: AppState is unavailable");
            return;
        };
        let toggle_guard = state.recording_hotkey_toggle_guard.clone();
        let mut pre_hidden_for_hotkey_stop = false;
        let _toggle_guard = match toggle_guard.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                let status_before_lock = active_recording_status(state.inner()).await;
                if status_before_lock == RecordingStatus::Processing {
                    if let Err(err) = show_recording_window_for_hotkey_start(
                        state.inner(),
                        &app_clone,
                        "global-hotkey-waiting-for-stop",
                        Some(accepted_press_seq),
                    )
                    .await
                    {
                        log::warn!(
                            "[HotkeyDiag] failed to show window while waiting for previous hotkey action: {}",
                            err
                        );
                    }
                } else if matches!(
                    status_before_lock,
                    RecordingStatus::Starting | RecordingStatus::Recording
                ) {
                    if should_ignore_immediate_hotkey_stop_after_start(state.inner()) {
                        log::info!(
                            "[HotkeyDiag] waiting for hotkey guard; stop hide skipped because start protection is active (status={:?})",
                            status_before_lock
                        );
                    } else if let Some(window) = app_clone.get_webview_window("main") {
                        let config = state.config.read().await.clone();
                        match hide_recording_window_for_hotkey_stop_if_needed(
                            &window,
                            &config,
                            state.inner(),
                            accepted_press_seq,
                            "pre-guard-stop",
                        )
                        .await
                        {
                            Ok(hidden) => {
                                pre_hidden_for_hotkey_stop = hidden;
                                log::info!(
                                    "[HotkeyDiag] waiting for hotkey guard before stop; status={:?}, pre_hidden={}",
                                    status_before_lock,
                                    hidden
                                );
                            }
                            Err(err) => {
                                log::warn!(
                                    "[HotkeyDiag] failed to pre-hide window while waiting for hotkey guard: {}",
                                    err
                                );
                            }
                        }
                    } else {
                        log::warn!(
                            "[HotkeyDiag] cannot pre-hide window while waiting for hotkey guard: main window is unavailable"
                        );
                    }
                }
                toggle_guard.lock().await
            }
        };

        let Some(window) = app_clone.get_webview_window("main") else {
            log::warn!("Recording hotkey ignored: main window is unavailable");
            return;
        };
        let status_before = active_recording_status(state.inner()).await;
        let window_visible = window.is_visible().ok();
        log::info!(
            "[HotkeyDiag] dispatch toggle: status_before={:?}, window_visible={:?}, accepted_press_seq={}",
            status_before,
            window_visible,
            accepted_press_seq
        );

        if let Err(e) = crate::presentation::commands::toggle_recording_with_window_internal(
            state.inner(),
            window,
            app_clone.clone(),
            accepted_press_seq,
            pre_hidden_for_hotkey_stop,
        )
        .await
        {
            log::error!("Failed to toggle recording: {}", e);
        }
    });
}

fn dispatch_recording_hotkey_press(app_clone: AppHandle, accepted_press_seq: u64) {
    std::mem::drop(tauri::async_runtime::spawn(async move {
        let Some(state) = app_clone.try_state::<crate::presentation::state::AppState>() else {
            log::warn!("Recording hotkey press ignored: AppState is unavailable");
            return;
        };

        let hold_to_record = state.config.read().await.hold_to_record;
        let status = active_recording_status(state.inner()).await;
        let intent = recording_hotkey_press_intent(hold_to_record, status);
        log::info!(
            "[HotkeyDiag] hotkey press intent={:?}, hold_to_record={}, status={:?}",
            intent,
            hold_to_record,
            status
        );

        match intent {
            RecordingHotkeyDispatchIntent::Toggle | RecordingHotkeyDispatchIntent::Start => {
                dispatch_recording_hotkey_toggle(app_clone.clone(), accepted_press_seq);
            }
            RecordingHotkeyDispatchIntent::Stop | RecordingHotkeyDispatchIntent::Ignore => {}
        }
    }));
}

fn dispatch_recording_hotkey_release(app_clone: AppHandle, accepted_press_seq: u64) {
    std::mem::drop(tauri::async_runtime::spawn(async move {
        let Some(state) = app_clone.try_state::<crate::presentation::state::AppState>() else {
            log::warn!("Recording hotkey release ignored: AppState is unavailable");
            return;
        };

        let hold_to_record = state.config.read().await.hold_to_record;
        let mut status = active_recording_status(state.inner()).await;
        if hold_to_record && status == RecordingStatus::Idle {
            let deadline = tokio::time::Instant::now()
                + Duration::from_millis(HOLD_TO_RECORD_RELEASE_START_WAIT_MS);
            while tokio::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(HOTKEY_STOP_WAIT_POLL_MS)).await;
                status = active_recording_status(state.inner()).await;
                if status != RecordingStatus::Idle {
                    break;
                }
            }
        }

        let current_press_seq = state
            .recording_hotkey_accepted_press_seq
            .load(Ordering::SeqCst);
        if hold_to_record && hotkey_action_is_stale(accepted_press_seq, current_press_seq) {
            log::info!(
                "[HotkeyDiag] hotkey release ignored because a newer press is active (release_press_seq={}, current_press_seq={})",
                accepted_press_seq,
                current_press_seq
            );
            return;
        }

        let intent = recording_hotkey_release_intent(hold_to_record, status);
        log::info!(
            "[HotkeyDiag] hotkey release intent={:?}, hold_to_record={}, status={:?}",
            intent,
            hold_to_record,
            status
        );

        match intent {
            RecordingHotkeyDispatchIntent::Toggle | RecordingHotkeyDispatchIntent::Stop => {
                dispatch_recording_hotkey_toggle(app_clone.clone(), accepted_press_seq);
            }
            RecordingHotkeyDispatchIntent::Start | RecordingHotkeyDispatchIntent::Ignore => {}
        }
    }));
}

fn schedule_missed_release_hotkey_confirmation(
    app_clone: AppHandle,
    press_generation: u64,
    candidate_press_ms: u64,
    previous_accepted_press_ms: u64,
    raw_press_delta_ms: u64,
    physical_release_key_code: Option<u16>,
) {
    let _ = tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(
            RECORDING_HOTKEY_MISSED_RELEASE_CONFIRM_MS,
        ))
        .await;

        let Some(state) = app_clone.try_state::<crate::presentation::state::AppState>() else {
            log::warn!(
                "Recording hotkey missed-release confirmation cancelled: AppState is unavailable"
            );
            return;
        };
        let state_inner = state.inner();

        let current_press_generation = state_inner
            .recording_hotkey_press_generation
            .load(Ordering::SeqCst);
        let last_raw_press_ms = state_inner
            .recording_hotkey_last_raw_press_ms
            .load(Ordering::SeqCst);
        let last_accepted_press_ms = state_inner.last_recording_hotkey_ms.load(Ordering::Relaxed);

        if current_press_generation != press_generation
            || last_raw_press_ms != candidate_press_ms
            || last_accepted_press_ms != previous_accepted_press_ms
        {
            log::debug!(
                "[HotkeyDiag] missed-release confirmation cancelled: press_generation={}->{}, raw_ms={}->{}, accepted_ms={}->{}",
                press_generation,
                current_press_generation,
                candidate_press_ms,
                last_raw_press_ms,
                previous_accepted_press_ms,
                last_accepted_press_ms
            );
            return;
        }

        let delta_ms = candidate_press_ms.saturating_sub(previous_accepted_press_ms);
        if delta_ms < RECORDING_HOTKEY_MISSED_RELEASE_ACCEPT_MS {
            log::debug!(
                "[HotkeyDiag] missed-release confirmation cancelled by debounce: delta_ms={}",
                delta_ms
            );
            return;
        }

        let accepted_press_seq = accept_recording_hotkey_press(state_inner, candidate_press_ms);
        schedule_recording_hotkey_physical_release_watch(
            app_clone.clone(),
            accepted_press_seq,
            physical_release_key_code,
        );
        log::warn!(
            "[HotkeyDiag] accepting hotkey press after missed release confirmation (delta_ms={}, raw_press_delta_ms={}, confirm_ms={}, accepted_press_seq={})",
            delta_ms,
            raw_press_delta_ms,
            RECORDING_HOTKEY_MISSED_RELEASE_CONFIRM_MS,
            accepted_press_seq
        );
        dispatch_recording_hotkey_press(app_clone, accepted_press_seq);
    });
}

/// Register or update recording hotkey
#[tauri::command]
pub async fn register_recording_hotkey(
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};

    let _registration_guard = state.recording_hotkey_registration_guard.lock().await;
    let hotkey = state.config.read().await.recording_hotkey.clone();
    log::info!("Command: register_recording_hotkey - hotkey: {}", hotkey);

    // ВАЖНО: сначала убеждаемся, что хоткей парсится, и только потом снимаем текущие регистрации.
    // Иначе при ошибке парсинга мы останемся вообще без хоткея.
    let (effective_hotkey, shortcut) = match hotkey.parse::<Shortcut>() {
        Ok(sc) => (hotkey.clone(), sc),
        Err(parse_err) => {
            // Пытаемся нормализовать строку (например Backquote -> `).
            if let Some(normalized) =
                crate::infrastructure::hotkey::normalize_recording_hotkey(&hotkey)
            {
                match normalized.parse::<Shortcut>() {
                    Ok(sc) => {
                        log::warn!(
                            "Hotkey '{}' failed to parse ({}), using normalized '{}'",
                            hotkey,
                            parse_err,
                            normalized
                        );
                        // Best-effort: фиксируем нормализованное значение в SoT + на диск,
                        // чтобы UI и фактический хоткей не расходились.
                        if normalized != hotkey {
                            let (should_save, config_snapshot) = {
                                let mut cfg = state.config.write().await;
                                let changed = cfg.recording_hotkey != normalized;
                                if changed {
                                    cfg.recording_hotkey = normalized.clone();
                                }
                                (changed, cfg.clone())
                            };
                            if should_save {
                                if let Err(e) = crate::infrastructure::ConfigStore::save_app_config(
                                    &config_snapshot,
                                )
                                .await
                                {
                                    log::warn!("Failed to persist normalized hotkey to app_config.json: {}", e);
                                }
                            }
                        }
                        (normalized, sc)
                    }
                    Err(_) => {
                        // Фоллбек на дефолт: всегда должен работать.
                        let fallback =
                            crate::infrastructure::hotkey::DEFAULT_RECORDING_HOTKEY.to_string();
                        let sc = fallback.parse::<Shortcut>().map_err(|e| {
                            format!("Failed to parse fallback hotkey '{}': {}", fallback, e)
                        })?;
                        log::error!(
                            "Failed to parse hotkey '{}' ({}). Falling back to '{}'",
                            hotkey,
                            parse_err,
                            fallback
                        );

                        // Синхронизируем SoT на дефолт, чтобы UI не показывал неработающее значение.
                        let config_snapshot = {
                            let mut cfg = state.config.write().await;
                            cfg.recording_hotkey = fallback.clone();
                            cfg.clone()
                        };
                        if let Err(e) =
                            crate::infrastructure::ConfigStore::save_app_config(&config_snapshot)
                                .await
                        {
                            log::warn!(
                                "Failed to persist fallback hotkey to app_config.json: {}",
                                e
                            );
                        }

                        // Пинаем invalidation, чтобы UI получил реальный (рабочий) хоткей.
                        let revision = AppState::bump_revision(&state.app_config_revision).await;
                        let _ = app_handle.emit(
                            EVENT_STATE_SYNC_INVALIDATION,
                            crate::presentation::StateSyncInvalidationPayload {
                                topic: "app-config".to_string(),
                                revision,
                                source_id: None,
                                timestamp_ms: chrono::Utc::now().timestamp_millis(),
                            },
                        );

                        (fallback, sc)
                    }
                }
            } else {
                let fallback = crate::infrastructure::hotkey::DEFAULT_RECORDING_HOTKEY.to_string();
                let sc = fallback.parse::<Shortcut>().map_err(|e| {
                    format!("Failed to parse fallback hotkey '{}': {}", fallback, e)
                })?;
                log::error!(
                    "Failed to parse hotkey '{}' ({}). Falling back to '{}'",
                    hotkey,
                    parse_err,
                    fallback
                );

                let config_snapshot = {
                    let mut cfg = state.config.write().await;
                    cfg.recording_hotkey = fallback.clone();
                    cfg.clone()
                };
                if let Err(e) =
                    crate::infrastructure::ConfigStore::save_app_config(&config_snapshot).await
                {
                    log::warn!(
                        "Failed to persist fallback hotkey to app_config.json: {}",
                        e
                    );
                }

                let revision = AppState::bump_revision(&state.app_config_revision).await;
                let _ = app_handle.emit(
                    EVENT_STATE_SYNC_INVALIDATION,
                    crate::presentation::StateSyncInvalidationPayload {
                        topic: "app-config".to_string(),
                        revision,
                        source_id: None,
                        timestamp_ms: chrono::Utc::now().timestamp_millis(),
                    },
                );

                (fallback, sc)
            }
        }
    };

    // Отменяем все старые регистрации
    if let Err(e) = app_handle.global_shortcut().unregister_all() {
        log::warn!("Failed to unregister all shortcuts: {}", e);
    }

    state
        .recording_hotkey_is_pressed
        .store(false, Ordering::SeqCst);
    state
        .recording_hotkey_last_raw_press_ms
        .store(0, Ordering::SeqCst);
    state
        .recording_hotkey_released_since_press
        .store(false, Ordering::SeqCst);
    state
        .recording_hotkey_last_release_ms
        .store(0, Ordering::SeqCst);
    state
        .recording_hotkey_stop_suppressed_until_ms
        .store(0, Ordering::SeqCst);
    state
        .recording_hotkey_accepted_press_seq
        .store(0, Ordering::SeqCst);
    state
        .recording_hotkey_stop_suppression_press_seq
        .store(0, Ordering::SeqCst);
    state
        .recording_hotkey_press_generation
        .store(0, Ordering::SeqCst);
    state
        .recording_hotkey_release_generation
        .fetch_add(1, Ordering::SeqCst);
    let physical_release_key_code = hotkey_key_code_for_physical_release_watch(&effective_hotkey);
    if let Some(key_code) = physical_release_key_code {
        log::info!(
            "[HotkeyDiag] physical release watch enabled for recording hotkey (key_code={})",
            key_code
        );
    }

    // Создаем обработчик - вызываем toggle напрямую вместо события.
    // Важно: key repeat может присылать несколько Pressed при удержании клавиши,
    // а на macOS bare-key hotkeys иногда дают Released между repeat Pressed.
    // Поэтому Released сбрасывает latch только после небольшой паузы без новых Pressed.
    app_handle
        .global_shortcut()
        .on_shortcut(shortcut, move |app, _shortcut, event| {
            use tauri_plugin_global_shortcut::ShortcutState;

            let Some(state) = app.try_state::<crate::presentation::state::AppState>() else {
                return;
            };

            match event.state {
                ShortcutState::Released => {
                    let app_clone = app.clone();
                    let app_for_hold_release = app.clone();
                    let state_inner = state.inner();
                    let now_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
                    state_inner
                        .recording_hotkey_released_since_press
                        .store(true, Ordering::SeqCst);
                    state_inner
                        .recording_hotkey_last_release_ms
                        .store(now_ms, Ordering::SeqCst);
                    let release_generation = state_inner
                        .recording_hotkey_release_generation
                        .fetch_add(1, Ordering::SeqCst)
                        + 1;
                    let accepted_press_seq = state_inner
                        .recording_hotkey_accepted_press_seq
                        .load(Ordering::SeqCst);

                    dispatch_recording_hotkey_release(app_for_hold_release, accepted_press_seq);

                    let _ = tauri::async_runtime::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(
                            RECORDING_HOTKEY_RELEASE_GRACE_MS,
                        ))
                        .await;

                        let Some(state) =
                            app_clone.try_state::<crate::presentation::state::AppState>()
                        else {
                            return;
                        };
                        let state_inner = state.inner();
                        if state_inner
                            .recording_hotkey_release_generation
                            .load(Ordering::SeqCst)
                            == release_generation
                        {
                            state_inner
                                .recording_hotkey_is_pressed
                                .store(false, Ordering::SeqCst);
                            log::debug!("Recording hotkey latch cleared after release grace");
                        }
                    });
                    return;
                }
                ShortcutState::Pressed => {}
            }

            let state_inner = state.inner();
            let now_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
            let suppressed_until_ms = state_inner
                .recording_hotkey_suppressed_until_ms
                .load(Ordering::SeqCst);
            if state_inner.should_suppress_recording_hotkey(now_ms) {
                log::info!(
                    "Recording hotkey ignored: suppressed during auto-paste (remaining_ms={})",
                    suppressed_until_ms.saturating_sub(now_ms)
                );
                return;
            }

            state_inner
                .recording_hotkey_release_generation
                .fetch_add(1, Ordering::SeqCst);

            let last_ms = state_inner.last_recording_hotkey_ms.load(Ordering::Relaxed);
            let delta = now_ms.saturating_sub(last_ms);
            let previous_raw_press_ms = state_inner
                .recording_hotkey_last_raw_press_ms
                .swap(now_ms, Ordering::SeqCst);
            let raw_press_delta = now_ms.saturating_sub(previous_raw_press_ms);
            let press_generation = state_inner
                .recording_hotkey_press_generation
                .fetch_add(1, Ordering::SeqCst)
                + 1;

            let mut accepted_despite_active_latch = false;
            if state_inner
                .recording_hotkey_is_pressed
                .swap(true, Ordering::SeqCst)
            {
                let saw_release = state_inner
                    .recording_hotkey_released_since_press
                    .swap(false, Ordering::SeqCst);
                let release_to_press_ms = now_ms.saturating_sub(
                    state_inner
                        .recording_hotkey_last_release_ms
                        .load(Ordering::SeqCst),
                );

                if saw_release
                    && delta >= RECORDING_HOTKEY_MIN_REPRESS_MS
                    && release_to_press_ms >= RECORDING_HOTKEY_MIN_RELEASE_TO_REPRESS_MS
                {
                    accepted_despite_active_latch = true;
                    log::info!(
                        "[HotkeyDiag] accepting quick repress after observed release (delta_ms={}, release_to_press_ms={})",
                        delta,
                        release_to_press_ms
                    );
                } else if !saw_release
                    && delta >= RECORDING_HOTKEY_MISSED_RELEASE_ACCEPT_MS
                    && raw_press_delta >= RECORDING_HOTKEY_RAW_REPEAT_GAP_MS
                {
                    log::info!(
                        "[HotkeyDiag] scheduling missed-release confirmation (delta_ms={}, raw_press_delta_ms={}, press_generation={})",
                        delta,
                        raw_press_delta,
                        press_generation
                    );
                    schedule_missed_release_hotkey_confirmation(
                        app.clone(),
                        press_generation,
                        now_ms,
                        last_ms,
                        raw_press_delta,
                        physical_release_key_code,
                    );
                    return;
                } else if delta < RECORDING_HOTKEY_STALE_PRESS_MS {
                    log::info!(
                        "Recording hotkey ignored: key repeat while latch is active (delta_ms={}, raw_press_delta_ms={}, saw_release={}, release_to_press_ms={})",
                        delta,
                        raw_press_delta,
                        saw_release,
                        release_to_press_ms
                    );
                    return;
                } else {
                    log::warn!("Hotkey press latch looked stale; accepting new press");
                }
            }

            // Дополнительный debounce оставляем для настоящих двойных событий press/release/press.
            if !accepted_despite_active_latch && delta < RECORDING_HOTKEY_DEBOUNCE_MS {
                log::debug!("Hotkey ignored (debounced): {}ms since last trigger", delta);
                return;
            }
            let accepted_press_seq = accept_recording_hotkey_press(state_inner, now_ms);
            schedule_recording_hotkey_physical_release_watch(
                app.clone(),
                accepted_press_seq,
                physical_release_key_code,
            );

            log::debug!("Recording hotkey pressed");
            dispatch_recording_hotkey_press(app.clone(), accepted_press_seq);
        })
        .map_err(|e| format!("Failed to register hotkey '{}': {}", effective_hotkey, e))?;

    log::info!("Successfully registered hotkey: {}", effective_hotkey);
    Ok(())
}

/// Временно снять регистрацию горячей клавиши (пока пользователь настраивает новую)
#[tauri::command]
pub async fn unregister_recording_hotkey(
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    use tauri_plugin_global_shortcut::GlobalShortcutExt;

    let _registration_guard = state.recording_hotkey_registration_guard.lock().await;
    log::info!("Command: unregister_recording_hotkey - временно снимаем хоткей");

    state
        .recording_hotkey_is_pressed
        .store(false, Ordering::SeqCst);
    state
        .recording_hotkey_last_raw_press_ms
        .store(0, Ordering::SeqCst);
    state
        .recording_hotkey_released_since_press
        .store(false, Ordering::SeqCst);
    state
        .recording_hotkey_last_release_ms
        .store(0, Ordering::SeqCst);
    state
        .recording_hotkey_stop_suppressed_until_ms
        .store(0, Ordering::SeqCst);
    state
        .recording_hotkey_accepted_press_seq
        .store(0, Ordering::SeqCst);
    state
        .recording_hotkey_stop_suppression_press_seq
        .store(0, Ordering::SeqCst);
    state
        .recording_hotkey_press_generation
        .store(0, Ordering::SeqCst);
    state
        .recording_hotkey_release_generation
        .fetch_add(1, Ordering::SeqCst);

    if let Err(e) = app_handle.global_shortcut().unregister_all() {
        log::warn!("Failed to unregister all shortcuts: {}", e);
    }

    Ok(())
}

//
// Update Commands
//

/// Check for application updates
#[tauri::command]
pub async fn check_for_updates(
    app_handle: AppHandle,
) -> Result<Option<crate::infrastructure::updater::UpdateInfo>, String> {
    log::info!("Command: check_for_updates");
    let update = crate::infrastructure::updater::check_for_update(app_handle).await?;
    crate::infrastructure::updater::remember_update_check_result(update.clone());
    Ok(update)
}

/// Returns the last cached available update, if one was found by background or manual checks
#[tauri::command]
pub fn get_cached_available_update() -> Option<crate::infrastructure::updater::UpdateInfo> {
    crate::infrastructure::updater::cached_available_update()
}

/// Check and install application update with user confirmation
#[tauri::command]
pub async fn install_update(app_handle: AppHandle) -> Result<String, String> {
    log::info!("Command: install_update");
    crate::infrastructure::updater::check_and_install_update(app_handle).await
}

/// Shows the standalone update window without resizing the recording popover
#[tauri::command]
pub async fn show_update_window(app_handle: AppHandle) -> Result<(), String> {
    log::info!("Command: show_update_window");

    if let Some(update) = app_handle.get_webview_window("update") {
        show_webview_window_on_active_monitor(&update)?;
        update.set_focus().map_err(|e| e.to_string())?;
        let _ = update.emit("update-window-opened", ());
        return Ok(());
    }

    Err("Update window not found".to_string())
}

//
// Whisper Model Management Commands
//

use crate::infrastructure::models::{
    delete_model, download_model, get_available_models, get_model_size, is_model_downloaded,
    WhisperModelInfo,
};

/// Get list of available Whisper models
#[tauri::command]
pub async fn get_available_whisper_models() -> Result<Vec<WhisperModelInfo>, String> {
    log::debug!("Command: get_available_whisper_models");

    let mut models = get_available_models();

    // Обогащаем данными о локальном наличии
    for model in &mut models {
        let is_downloaded = is_model_downloaded(&model.name);
        let local_size = if is_downloaded {
            get_model_size(&model.name)
        } else {
            None
        };

        // Добавляем информацию в description если модель скачана
        if is_downloaded {
            if let Some(size) = local_size {
                model.description = format!(
                    "{} (Скачана, {} на диске)",
                    model.description,
                    format_size_human(size)
                );
            } else {
                model.description = format!("{} (Скачана)", model.description);
            }
        }
    }

    Ok(models)
}

/// Check if specific Whisper model is downloaded
#[tauri::command]
pub async fn check_whisper_model(model_name: String) -> Result<bool, String> {
    log::debug!("Command: check_whisper_model - model: {}", model_name);
    Ok(is_model_downloaded(&model_name))
}

/// Download Whisper model with progress tracking
#[tauri::command]
pub async fn download_whisper_model(
    app_handle: AppHandle,
    model_name: String,
) -> Result<String, String> {
    log::info!("Command: download_whisper_model - model: {}", model_name);

    // Проверяем что модель еще не скачана
    if is_model_downloaded(&model_name) {
        return Err(format!("Model '{}' is already downloaded", model_name));
    }

    // Эмитируем событие начала загрузки
    let _ = app_handle.emit("whisper-model:download-started", model_name.clone());

    // Создаем callback для отслеживания прогресса
    let app_handle_progress = app_handle.clone();
    let model_name_progress = model_name.clone();

    let progress_callback = move |downloaded: u64, total: u64| {
        let progress = if total > 0 {
            (downloaded as f64 / total as f64 * 100.0) as u8
        } else {
            0
        };

        #[derive(Clone, serde::Serialize)]
        struct DownloadProgressPayload {
            model_name: String,
            downloaded: u64,
            total: u64,
            progress: u8,
        }

        let _ = app_handle_progress.emit(
            "whisper-model:download-progress",
            DownloadProgressPayload {
                model_name: model_name_progress.clone(),
                downloaded,
                total,
                progress,
            },
        );
    };

    // Загружаем модель
    let model_path = download_model(&model_name, progress_callback)
        .await
        .map_err(|e| format!("Failed to download model: {}", e))?;

    // Эмитируем событие завершения загрузки
    let _ = app_handle.emit("whisper-model:download-completed", model_name.clone());

    log::info!(
        "Model '{}' downloaded successfully to {:?}",
        model_name,
        model_path
    );
    Ok(format!("Model '{}' downloaded successfully", model_name))
}

/// Delete Whisper model
#[tauri::command]
pub async fn delete_whisper_model(model_name: String) -> Result<String, String> {
    log::info!("Command: delete_whisper_model - model: {}", model_name);

    delete_model(&model_name).map_err(|e| format!("Failed to delete model: {}", e))?;

    Ok(format!("Model '{}' deleted successfully", model_name))
}

/// Get available audio input devices
#[tauri::command]
pub async fn get_audio_devices() -> Result<Vec<String>, String> {
    log::info!("Command: get_audio_devices");

    use cpal::traits::{DeviceTrait, HostTrait};

    let host = cpal::default_host();

    let devices: Vec<String> = host
        .input_devices()
        .map_err(|e| format!("Failed to enumerate input devices: {}", e))?
        .filter_map(|device| device.name().ok())
        .collect();

    log::info!("Found {} audio input devices", devices.len());

    Ok(devices)
}

fn format_size_human(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.0} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.0} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

//
// Auto-Paste Commands
//

/// Проверяет есть ли разрешение Accessibility на macOS
/// На других платформах всегда возвращает true
#[tauri::command]
pub async fn check_accessibility_permission() -> Result<bool, String> {
    log::debug!("Command: check_accessibility_permission");
    Ok(crate::infrastructure::auto_paste::check_accessibility_permission())
}

/// Открывает системные настройки macOS в разделе Privacy & Security > Accessibility
/// На других платформах ничего не делает
#[tauri::command]
pub async fn request_accessibility_permission() -> Result<(), String> {
    log::info!("Command: request_accessibility_permission");
    crate::infrastructure::auto_paste::open_accessibility_settings().map_err(|e| e.to_string())
}

/// Автоматически вставляет текст в последнее активное окно
/// Требует разрешения Accessibility на macOS
#[tauri::command]
pub async fn auto_paste_text(
    state: State<'_, AppState>,
    app_handle: AppHandle,
    text: String,
) -> Result<(), String> {
    log::info!("Command: auto_paste_text - text length: {}", text.len());

    let recording_hotkey = state.config.read().await.recording_hotkey.clone();
    let suppression_duration = auto_paste_hotkey_suppression_duration(&text, &recording_hotkey);
    if suppression_duration.as_millis() > 0 {
        state.suppress_recording_hotkey_for(suppression_duration);
        log::info!(
            "Recording hotkey suppressed for {}ms during auto-paste (hotkey={})",
            suppression_duration.as_millis(),
            recording_hotkey
        );
    } else {
        log::debug!(
            "Recording hotkey suppression skipped during auto-paste (hotkey={} cannot be produced by text)",
            recording_hotkey
        );
    }

    // Проверяем разрешение Accessibility на macOS
    #[cfg(target_os = "macos")]
    {
        if !crate::infrastructure::auto_paste::check_accessibility_permission() {
            return Err("Accessibility permission not granted. Please enable it in System Settings > Privacy & Security > Accessibility".to_string());
        }
    }

    #[cfg(target_os = "macos")]
    {
        let target = validate_auto_paste_target_for_focus(
            state.last_focused_app_target.read().await.clone(),
        )
        .map_err(|message| {
            log::warn!("{}", message);
            message
        })?;

        log::info!(
            "Attempting to focus auto-paste target: bundle_id={}, pid={}",
            target.bundle_id,
            target.pid
        );

        crate::infrastructure::auto_paste::activate_running_app_by_target(&target).map_err(
            |e| {
                let message = format!(
                    "Failed to activate auto-paste target without launching app: {}",
                    e
                );
                log::warn!("{}", message);
                message
            },
        )?;

        if !wait_for_auto_paste_target_focus(&target).await {
            let message = format!(
                "Auto-paste target did not become frontmost; refusing to paste: bundle_id={}, pid={}",
                target.bundle_id, target.pid
            );
            log::warn!("{}", message);
            return Err(message);
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        log::info!("Auto-paste target activation is not required on this platform");
    }

    // Вставляем текст в blocking thread (enigo работает с синхронными нативными API)
    let text_clone = text.clone();
    let paste_result = tokio::task::spawn_blocking(move || {
        crate::infrastructure::auto_paste::paste_text(&text_clone)
    })
    .await
    .map_err(|e| format!("Failed to join blocking task: {}", e))?
    .map_err(|e| format!("Failed to paste text: {}", e));
    if suppression_duration.as_millis() > 0 {
        state.suppress_recording_hotkey_for(Duration::from_millis(
            AUTO_PASTE_HOTKEY_SUPPRESSION_TAIL_MS,
        ));
    }
    paste_result?;

    // Возвращаем окно VoicetextAI поверх всех окон (но без фокуса)
    if let Some(window) = app_handle.get_webview_window("main") {
        let _ = window.set_always_on_top(true);
        log::debug!("VoicetextAI window kept on top");
    }

    log::info!("Text auto-pasted successfully");
    Ok(())
}

/// Копирует текст в системный clipboard используя arboard (кроссплатформенно)
/// Работает БЕЗ активации приложения - решает проблему с nonactivating_panel на macOS
#[tauri::command]
pub async fn copy_to_clipboard_native(text: String) -> Result<(), String> {
    log::debug!(
        "Command: copy_to_clipboard_native - text length: {}",
        text.len()
    );

    // Используем blocking task (arboard работает с синхронными системными API, как enigo)
    tokio::task::spawn_blocking(move || crate::infrastructure::copy_to_clipboard(&text))
        .await
        .map_err(|e| format!("Failed to join blocking task: {}", e))?
        .map_err(|e| format!("Failed to copy to clipboard: {}", e))?;

    log::info!("Text copied to clipboard successfully");
    Ok(())
}

/// Показывает auth окно и скрывает recording (main)
#[tauri::command]
pub async fn show_auth_window(app_handle: AppHandle) -> Result<(), String> {
    log::info!("Command: show_auth_window");

    // Скрываем recording окно (main)
    if let Some(main) = app_handle.get_webview_window("main") {
        // На macOS main может быть NSPanel с высоким уровнем; перед hide сбрасываем always-on-top
        if let Err(e) = main.set_always_on_top(false) {
            log::warn!("Failed to disable always-on-top for main window: {}", e);
        }
        if let Err(e) = main.hide() {
            log::warn!("Failed to hide main window: {}", e);
        }
    }

    // Скрываем settings окно (если было открыто)
    if let Some(settings) = app_handle.get_webview_window("settings") {
        if let Err(e) = settings.hide() {
            log::warn!("Failed to hide settings window: {}", e);
        }
    }

    // Скрываем profile окно (если было открыто)
    if let Some(profile) = app_handle.get_webview_window("profile") {
        if let Err(e) = profile.hide() {
            log::warn!("Failed to hide profile window: {}", e);
        }
    }

    // Показываем auth окно
    if let Some(auth) = app_handle.get_webview_window("auth") {
        // Центрируем и показываем на активном мониторе, чтобы окно точно было видно
        show_webview_window_on_active_monitor(&auth)?;
        auth.set_focus().map_err(|e| e.to_string())?;
    }

    Ok(())
}

/// Показывает recording окно (main) и скрывает auth
#[tauri::command]
pub async fn show_recording_window(
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<(), String> {
    log::info!("Command: show_recording_window");

    // Скрываем auth окно
    if let Some(auth) = app_handle.get_webview_window("auth") {
        if let Err(e) = auth.hide() {
            log::warn!("Failed to hide auth window: {}", e);
        }
    }

    // Скрываем settings окно
    if let Some(settings) = app_handle.get_webview_window("settings") {
        if let Err(e) = settings.hide() {
            log::warn!("Failed to hide settings window: {}", e);
        }
    }

    // Скрываем profile окно
    if let Some(profile) = app_handle.get_webview_window("profile") {
        if let Err(e) = profile.hide() {
            log::warn!("Failed to hide profile window: {}", e);
        }
    }

    // Показываем recording окно (NSPanel - появляется поверх fullscreen, без фокуса)
    if let Some(window) = app_handle.get_webview_window("main") {
        let config = state.config.read().await.clone();
        show_webview_window_with_recording_config(&window, &config, state.inner())?;
        let _ = window.emit(EVENT_RECORDING_WINDOW_SHOWN, ());
        if let Err(e) = window.set_always_on_top(true) {
            log::warn!("Failed to enable always-on-top for main window: {}", e);
        }
    }

    Ok(())
}

#[derive(Debug, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct ShowSettingsWindowArgs {
    scroll_to_section: Option<String>,
}

/// Показывает settings окно и скрывает recording (main)
#[tauri::command]
pub async fn show_settings_window(
    state: State<'_, AppState>,
    app_handle: AppHandle,
    args: Option<ShowSettingsWindowArgs>,
) -> Result<(), String> {
    let scroll_to_section = args.and_then(|a| a.scroll_to_section);
    log::info!(
        "Command: show_settings_window (scroll_to_section: {:?})",
        scroll_to_section
    );

    // Настройки доступны только авторизованному пользователю.
    // Если не авторизован — открываем auth окно, а settings держим скрытым.
    if !*state.is_authenticated.read().await {
        log::info!("show_settings_window: user is not authenticated -> redirect to auth window");
        show_auth_window(app_handle).await?;
        return Err("Not authenticated".to_string());
    }

    // Перед показом окна подтягиваем конфиги с диска (best-effort).
    // Это снижает шанс увидеть дефолты, если окно открыли очень рано после старта приложения,
    // когда фоновые load_* задачи ещё не завершились.
    //
    // Важно: не делаем это фатальным — если чтение упало, показываем окно с текущим in-memory состоянием.
    {
        if let Ok(saved_app) = ConfigStore::load_app_config().await {
            *state.config.write().await = saved_app.clone();
            state
                .transcription_service
                .set_microphone_sensitivity(saved_app.microphone_sensitivity)
                .await;
        }

        if let Ok(mut saved_stt) = ConfigStore::load_config().await {
            let _guard = state.stt_config_guard.lock().await;
            // Держим auth token консистентным с AuthStore (Rust SoT).
            let token = state
                .auth_store
                .read()
                .await
                .session
                .as_ref()
                .map(|s| s.access_token.clone());
            saved_stt.backend_auth_token = token;
            let _ = state
                .transcription_service
                .update_config(saved_stt.clone())
                .await;
            state.config.write().await.stt = saved_stt;
        }

        if let Ok(prefs) = ConfigStore::load_ui_preferences().await {
            *state.ui_preferences.write().await = prefs;
        }
    }

    // Скрываем recording окно (main)
    if let Some(main) = app_handle.get_webview_window("main") {
        // На macOS main может быть NSPanel с высоким уровнем; перед hide сбрасываем always-on-top
        if let Err(e) = main.set_always_on_top(false) {
            log::warn!("Failed to disable always-on-top for main window: {}", e);
        }
        if let Err(e) = main.hide() {
            log::warn!("Failed to hide main window: {}", e);
        }
    }

    // Скрываем auth окно (на всякий случай)
    if let Some(auth) = app_handle.get_webview_window("auth") {
        if let Err(e) = auth.hide() {
            log::warn!("Failed to hide auth window: {}", e);
        }
    }

    // Скрываем profile окно
    if let Some(profile) = app_handle.get_webview_window("profile") {
        if let Err(e) = profile.hide() {
            log::warn!("Failed to hide profile window: {}", e);
        }
    }

    // Показываем settings окно
    if let Some(settings) = app_handle.get_webview_window("settings") {
        show_webview_window_on_active_monitor(&settings)?;
        settings.set_focus().map_err(|e| e.to_string())?;
        let payload = serde_json::json!({
            "scrollToSection": scroll_to_section
        });
        let _ = settings.emit("settings-window-opened", payload);
    }

    Ok(())
}

/// Показывает profile окно и скрывает остальные
#[tauri::command]
pub async fn show_profile_window(
    state: State<'_, AppState>,
    app_handle: AppHandle,
    initial_section: Option<String>,
) -> Result<(), String> {
    log::info!("Command: show_profile_window");

    if !*state.is_authenticated.read().await {
        log::info!("show_profile_window: not authenticated -> redirect to auth");
        show_auth_window(app_handle).await?;
        return Err("Not authenticated".to_string());
    }

    // Скрываем все окна
    if let Some(main) = app_handle.get_webview_window("main") {
        let _ = main.set_always_on_top(false);
        let _ = main.hide();
    }
    if let Some(auth) = app_handle.get_webview_window("auth") {
        let _ = auth.hide();
    }
    if let Some(settings) = app_handle.get_webview_window("settings") {
        let _ = settings.hide();
    }

    // Показываем profile
    if let Some(profile) = app_handle.get_webview_window("profile") {
        show_webview_window_on_active_monitor(&profile)?;
        profile.set_focus().map_err(|e| e.to_string())?;
        let _ = profile.emit(
            "profile-window-opened",
            serde_json::json!({
                "initialSection": initial_section.unwrap_or_else(|| "none".to_string())
            }),
        );
    }

    Ok(())
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthSessionInputUser {
    pub id: String,
    pub email: String,
    pub email_verified: bool,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthSessionInput {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub access_expires_at: String,
    pub refresh_expires_at: Option<String>,
    pub device_id: Option<String>, // ignore: Rust SoT
    pub user: Option<AuthSessionInputUser>,
}

fn parse_rfc3339_to_ms(s: &str) -> Result<i64, String> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.timestamp_millis())
        .map_err(|e| format!("Invalid RFC3339 datetime: {} ({})", s, e))
}

async fn emit_invalidation(
    app_handle: &AppHandle,
    topic: &str,
    revision: String,
    source_id: Option<String>,
) {
    let _ = app_handle.emit(
        EVENT_STATE_SYNC_INVALIDATION,
        crate::presentation::StateSyncInvalidationPayload {
            topic: topic.to_string(),
            revision,
            source_id,
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        },
    );
}

/// Устанавливает/очищает auth session в Rust (SoT) и запускает фоновые refresh-таймеры.
#[tauri::command]
pub async fn set_auth_session(
    state: State<'_, AppState>,
    app_handle: AppHandle,
    window: Window,
    session: Option<AuthSessionInput>,
) -> Result<(), String> {
    // 1) Обновляем store в памяти + сохраняем на диск
    let mut next = state.auth_store.read().await.clone();

    let prev_is_auth = next.is_authenticated();

    next.session = match session {
        Some(s) => {
            // device_id критичен: refresh token привязан к client_id на сервере.
            // Поэтому если frontend прислал device_id (например, после login/refresh),
            // обновляем Rust SoT, чтобы background refresh работал корректно.
            if let Some(did) = s.device_id.as_deref() {
                let did = did.trim();
                if !did.is_empty() && did != next.device_id {
                    next.device_id = did.to_string();
                }
            }

            let access_expires_at_ms = parse_rfc3339_to_ms(&s.access_expires_at)?;
            let refresh_expires_at_ms = match s.refresh_expires_at.as_deref() {
                Some(v) => Some(parse_rfc3339_to_ms(v)?),
                None => None,
            };

            Some(AuthSession {
                access_token: s.access_token,
                refresh_token: s.refresh_token,
                access_expires_at_ms,
                refresh_expires_at_ms,
                user: s.user.map(|u| AuthUser {
                    id: u.id,
                    email: u.email,
                    email_verified: u.email_verified,
                }),
            })
        }
        None => None,
    };

    if let Err(e) = AuthStore::save(&next).await {
        return Err(format!("Failed to save auth store: {}", e));
    }

    *state.auth_store.write().await = next.clone();

    // 2) Обновляем derived auth flag
    let next_is_auth = next.is_authenticated();
    *state.is_authenticated.write().await = next_is_auth;

    // 3) Обновляем токен для STT (чтобы hotkey start_recording всегда имел актуальный access)
    let stt_token = next.session.as_ref().map(|s| s.access_token.clone());
    state.apply_backend_auth_token_to_stt(stt_token).await;

    // 4) Bump revisions + invalidations
    // auth-state только если поменялся флаг
    if prev_is_auth != next_is_auth {
        let rev_state = AppState::bump_revision(&state.auth_state_revision).await;
        emit_invalidation(
            &app_handle,
            "auth-state",
            rev_state,
            Some(window.label().to_string()),
        )
        .await;
    }

    // auth-session всегда: и login/logout, и refresh.
    let rev_session = AppState::bump_revision(&state.auth_session_revision).await;
    emit_invalidation(
        &app_handle,
        "auth-session",
        rev_session,
        Some(window.label().to_string()),
    )
    .await;

    // 5) Перезапускаем фоновый refresh
    state.restart_auth_refresh_task(app_handle.clone()).await;

    Ok(())
}

/// Обновляет флаг авторизации в backend (синхронизация из frontend)
#[tauri::command]
pub async fn set_authenticated(
    state: State<'_, AppState>,
    app_handle: AppHandle,
    window: Window,
    authenticated: bool,
    token: Option<String>,
) -> Result<(), String> {
    log::info!(
        "Command: set_authenticated - authenticated: {}",
        authenticated
    );

    let current_auth = *state.is_authenticated.read().await;
    if current_auth == authenticated {
        // Токен мог обновиться — проверяем и обновляем тихо (без bump revision)
        if authenticated {
            if let Some(ref t) = token {
                let current_token = state
                    .transcription_service
                    .get_config()
                    .await
                    .backend_auth_token;
                if current_token.as_deref() != Some(t.as_str()) {
                    state.apply_backend_auth_token_to_stt(Some(t.clone())).await;
                }
            }
        }
        return Ok(());
    }

    *state.is_authenticated.write().await = authenticated;

    // Обновляем только токен в текущем in-memory STT конфиге, чтобы не перетирать keyterms
    // и другие поля конкурентным чтением старой disk-версии.
    if authenticated {
        if let Some(ref t) = token {
            log::info!("set_authenticated: received token with len: {}", t.len());
            state.apply_backend_auth_token_to_stt(Some(t.clone())).await;
            log::info!("Backend auth token saved to config");
        } else {
            log::warn!("set_authenticated: authenticated=true but token is None!");
            state.apply_backend_auth_token_to_stt(None).await;
        }
    } else {
        state.apply_backend_auth_token_to_stt(None).await;
        log::info!("Backend auth token cleared from config");
    }

    // Синхронизация между окнами через state-sync
    let revision = AppState::bump_revision(&state.auth_state_revision).await;
    let _ = app_handle.emit(
        EVENT_STATE_SYNC_INVALIDATION,
        crate::presentation::StateSyncInvalidationPayload {
            topic: "auth-state".to_string(),
            revision,
            source_id: Some(window.label().to_string()),
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        },
    );

    Ok(())
}
