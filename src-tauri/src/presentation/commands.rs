use std::sync::Arc;
use std::sync::atomic::Ordering;
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow, Window};

use crate::domain::{AudioCapture, RecordingStatus, SttConnectionCategory, SttError};
use crate::infrastructure::{AuthSession, AuthStore, AuthUser, ConfigStore};
use crate::presentation::{
    events::*, AppState, AudioLevelPayload, FinalTranscriptionPayload, PartialTranscriptionPayload,
    RecordingStatusPayload, MicrophoneTestLevelPayload, TranscriptionErrorPayload, ConnectionQualityPayload,
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
            } else {
                "connection".to_string()
            }
        }
        SttError::Processing(_) | SttError::Unsupported(_) | SttError::Internal(_) => "processing".to_string(),
    }
}

fn error_details_from_stt(err: &SttError) -> Option<TranscriptionErrorDetailsPayload> {
    match err {
        SttError::Connection(conn) => Some(conn.details.clone().into()),
        _ => None,
    }
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
    let session_id = state.transcription_session_seq.fetch_add(1, Ordering::Relaxed) + 1;
    state
        .active_transcription_session_id
        .store(session_id, Ordering::Relaxed);
    log::info!("Recording session started: session_id={}", session_id);

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
            let payload = PartialTranscriptionPayload::from_transcription(transcription, session_id);
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
            let payload = FinalTranscriptionPayload::from_transcription(transcription.clone(), session_id);
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
                },
            );
        });
    });

    let app_handle_quality = app_handle.clone();

    // Callback for connection quality updates
    let on_connection_quality = Arc::new(move |quality: String, reason: Option<String>| {
        let app_handle = app_handle_quality.clone();

        tokio::spawn(async move {
            log::info!("Connection quality changed: {} (reason: {:?})", quality, reason);

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

    // Emit Starting status immediately
    log::debug!("Emitting status: Starting (stopped_via_hotkey: false)");
    let _ = app_handle.emit(
        EVENT_RECORDING_STATUS,
        RecordingStatusPayload {
            session_id,
            status: RecordingStatus::Starting,
            stopped_via_hotkey: false,
        },
    );

    // Если в настройках выбран "Default" (selected_audio_device=None), то при подключении/смене микрофона
    // системное устройство по умолчанию может измениться, а захват останется привязанным к старому девайсу.
    // Поэтому перед стартом записи пересоздаём audio capture по текущему конфигу.
    let selected_device = state.config.read().await.selected_audio_device.clone();
    state
        .recreate_audio_capture_with_device(selected_device, app_handle.clone())
        .await
        .map_err(|e| format!("Не удалось инициализировать устройство записи: {}", e))?;

    // Start recording (async - WebSocket connect, audio capture start)
    let start_result = state
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

    // Важно: если старт провалился ДО того, как провайдер успел вызвать on_error (например, упали на handshake/connection refused),
    // UI останется в Starting и будет ощущение "подключение идёт, но ничего не происходит".
    // Поэтому здесь явно отправляем error + status=Error тем же контрактом, что и в runtime-ошибках.
    if let Err(e) = start_result {
        // Стараемся извлечь исходную причину максимально надёжно (без парсинга строки).
        let stt = e.downcast_ref::<SttError>().cloned().unwrap_or_else(|| {
            SttError::Internal(e.to_string())
        });
        let error = stt.to_string();
        let error_type = classify_transcription_error_type_from_stt(&stt);

        log::error!("Failed to start recording: {} (type: {})", error, error_type);

        // Сначала transcription:error, потом recording:status=Error (во фронте есть логика suppression/retry).
        on_error(stt);

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

    let session_id = state.active_transcription_session_id.load(Ordering::Relaxed);

    let result = state
        .transcription_service
        .stop_recording()
        .await
        .map_err(|e| e.to_string())?;

    // Emit status change
    log::debug!("Emitting status: Idle (stopped_via_hotkey: false)");
    let _ = app_handle.emit(
        EVENT_RECORDING_STATUS,
        RecordingStatusPayload {
            session_id,
            status: RecordingStatus::Idle,
            stopped_via_hotkey: false,
        },
    );

    Ok(result)
}

/// Get current recording status
#[tauri::command]
pub async fn get_recording_status(state: State<'_, AppState>) -> Result<RecordingStatus, String> {
    log::debug!("Command: get_recording_status");
    Ok(state.transcription_service.get_status().await)
}

use tauri::{PhysicalPosition, Position};

/// Показывает окно на активном мониторе (где находится курсор мыши) - для Window
pub fn show_window_on_active_monitor(window: &Window) -> Result<(), String> {
    show_window_on_active_monitor_impl(
        || window.current_monitor(),
        || window.primary_monitor(),
        || window.outer_size(),
        |pos| window.set_position(pos),
        || window.show(),
    )
}

/// Показывает окно на активном мониторе (где находится курсор мыши) - для WebviewWindow
pub fn show_webview_window_on_active_monitor<R: tauri::Runtime>(window: &WebviewWindow<R>) -> Result<(), String> {
    show_window_on_active_monitor_impl(
        || window.current_monitor(),
        || window.primary_monitor(),
        || window.outer_size(),
        |pos| window.set_position(pos),
        || window.show(),
    )
}

/// Общая реализация для позиционирования окна по центру текущего монитора
fn show_window_on_active_monitor_impl<F1, F2, F3, F4, F5>(
    get_current_monitor: F1,
    get_primary_monitor: F2,
    get_outer_size: F3,
    set_position: F4,
    show: F5,
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

    log::debug!("Монитор: позиция ({}, {}), размер {}x{}",
        monitor_position.x, monitor_position.y,
        monitor_size.width, monitor_size.height
    );

    // Получаем размеры окна
    let window_size = get_outer_size()
        .map_err(|e| format!("Failed to get window size: {}", e))?;

    // Вычисляем центральную позицию на мониторе
    let x = monitor_position.x + (monitor_size.width as i32 - window_size.width as i32) / 2;
    let y = monitor_position.y + (monitor_size.height as i32 - window_size.height as i32) / 2;

    log::debug!("Устанавливаю позицию окна: ({}, {})", x, y);

    // Устанавливаем позицию окна
    set_position(Position::Physical(PhysicalPosition { x, y }))
        .map_err(|e| format!("Failed to set window position: {}", e))?;

    // Показываем окно
    show().map_err(|e| e.to_string())?;

    log::info!("✅ Окно показано по центру монитора");

    Ok(())
}

#[cfg(test)]
mod snapshot_contract_tests {
    use super::{AppConfigSnapshotData, SnapshotEnvelope, SttConfigSnapshotData};
    use crate::domain::SttProviderType;

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
    fn app_config_snapshot_is_public_and_does_not_leak_secrets() {
        let env = SnapshotEnvelope {
            revision: "1".to_string(),
            data: AppConfigSnapshotData {
                microphone_sensitivity: 100,
                recording_hotkey: "CmdOrCtrl+Shift+X".to_string(),
                auto_copy_to_clipboard: true,
                auto_paste_text: false,
                selected_audio_device: None,
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
        let data = v.get("data").and_then(|x| x.as_object()).expect("data object");
        assert!(data.contains_key("microphone_sensitivity"));
        assert!(data.contains_key("recording_hotkey"));
        assert!(data.contains_key("auto_copy_to_clipboard"));
        assert!(data.contains_key("auto_paste_text"));
        assert!(data.contains_key("selected_audio_device"));
    }

    #[test]
    fn stt_config_snapshot_is_public_and_does_not_leak_backend_token_or_url() {
        let env = SnapshotEnvelope {
            revision: "7".to_string(),
            data: SttConfigSnapshotData {
                provider: SttProviderType::Backend,
                language: "ru".to_string(),
                auto_detect_language: false,
                enable_punctuation: true,
                filter_profanity: false,
                deepgram_api_key: None,
                assemblyai_api_key: None,
                model: None,
                keep_connection_alive: true,
                deepgram_keyterms: None,
            },
        };

        let json = serde_json::to_string(&env).expect("must serialize");
        assert_absent(
            &json,
            &["backend_auth_token", "backend_url", "refresh_token", "access_token"],
        );

        // Проверяем, что JSON-форма стабильная (ожидаемые ключи присутствуют).
        let v: serde_json::Value = serde_json::from_str(&json).expect("must parse json");
        let data = v.get("data").and_then(|x| x.as_object()).expect("data object");
        assert!(data.contains_key("provider"));
        assert!(data.contains_key("language"));
        assert!(data.contains_key("keep_connection_alive"));
    }
}
/// Toggle window visibility
#[tauri::command]
pub async fn toggle_window(
    state: State<'_, AppState>,
    window: Window,
) -> Result<(), String> {
    log::info!("Command: toggle_window");

    if window.is_visible().map_err(|e| e.to_string())? {
        window.hide().map_err(|e| e.to_string())?;
    } else {
        // Перед показом окна сохраняем bundle ID текущего активного приложения
        // (чтобы потом вставлять текст в правильное окно)
        #[cfg(target_os = "macos")]
        {
            if let Some(bundle_id) = crate::infrastructure::auto_paste::get_active_app_bundle_id() {
                *state.last_focused_app_bundle_id.write().await = Some(bundle_id.clone());
                log::info!("Saved last focused app bundle ID: {}", bundle_id);
            }
        }

        show_window_on_active_monitor(&window)?;

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
        log::info!("toggle_recording_with_window: user not authenticated -> redirect to auth window");
        show_auth_window(app_handle).await?;
        return Ok(());
    }

    // Переключаем состояние записи
    let current_status = state.transcription_service.get_status().await;

    match current_status {
        RecordingStatus::Idle => {
            // Показываем окно если оно скрыто (не забираем фокус)
            if !window.is_visible().map_err(|e| e.to_string())? {
                // Перед показом окна сохраняем bundle ID текущего активного приложения
                #[cfg(target_os = "macos")]
                {
                    if let Some(bundle_id) = crate::infrastructure::auto_paste::get_active_app_bundle_id() {
                        *state.last_focused_app_bundle_id.write().await = Some(bundle_id.clone());
                        log::info!("Saved last focused app bundle ID: {}", bundle_id);
                    }
                }

                show_window_on_active_monitor(&window)?;

                // Сообщаем фронту, что окно показано (для надёжного reset UI).
                let _ = window.emit(EVENT_RECORDING_WINDOW_SHOWN, ());
            }

            // Запускаем запись
            start_recording(state.clone(), app_handle).await?;
            log::info!("Recording started via hotkey");
        }
        RecordingStatus::Starting => {
            // Запись еще запускается - игнорируем повторное нажатие
            log::debug!("Ignoring toggle - recording is starting (WebSocket connecting, audio capture initializing)");
        }
        RecordingStatus::Recording => {
            // Останавливаем запись
            let _result = state
                .transcription_service
                .stop_recording()
                .await
                .map_err(|e| e.to_string())?;

            log::info!("Recording stopped via hotkey");

            // Эмитируем статус Idle с флагом stopped_via_hotkey
            // Frontend скроет окно когда получит этот статус
            let session_id = state.active_transcription_session_id.load(Ordering::Relaxed);
            log::info!("Emitting status: Idle (stopped_via_hotkey: TRUE) - window will auto-hide");
            let _ = app_handle.emit(
                EVENT_RECORDING_STATUS,
                RecordingStatusPayload {
                    session_id,
                    status: RecordingStatus::Idle,
                    stopped_via_hotkey: true,
                },
            );
        }
        RecordingStatus::Processing => {
            // Игнорируем - запись уже останавливается
            log::debug!("Ignoring toggle - recording is already being processed");
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

    let current_status = state.transcription_service.get_status().await;

    match current_status {
        RecordingStatus::Idle => {
            // Показываем окно если оно скрыто
            if !window.is_visible().map_err(|e| e.to_string())? {
                #[cfg(target_os = "macos")]
                {
                    if let Some(bundle_id) = crate::infrastructure::auto_paste::get_active_app_bundle_id() {
                        *state.last_focused_app_bundle_id.write().await = Some(bundle_id.clone());
                        log::info!("Saved last focused app bundle ID: {}", bundle_id);
                    }
                }
                show_webview_window_on_active_monitor(&window)?;

                // Сообщаем фронту, что окно показано (для надёжного reset UI).
                let _ = window.emit(EVENT_RECORDING_WINDOW_SHOWN, ());
            }

            // ВАЖНО: стартуем запись на Rust-стороне.
            // Иначе, когда окно было скрыто, WebView/JS могут быть "усыплены" и не обработать event,
            // из-за чего хоткей откроет окно, но запись не стартует и UI останется в старом состоянии.
            let state_handle = app_handle
                .try_state::<AppState>()
                .ok_or_else(|| "AppState не доступен".to_string())?;
            start_recording(state_handle, app_handle.clone()).await?;
            log::info!("Recording started via hotkey (internal)");
        }
        RecordingStatus::Starting => {
            log::debug!("Ignoring toggle - recording is starting");
        }
        RecordingStatus::Recording => {
            let _result = state
                .transcription_service
                .stop_recording()
                .await
                .map_err(|e| e.to_string())?;

            log::info!("Recording stopped via hotkey");
            let session_id = state.active_transcription_session_id.load(Ordering::Relaxed);
            let _ = app_handle.emit(
                EVENT_RECORDING_STATUS,
                RecordingStatusPayload {
                    session_id,
                    status: RecordingStatus::Idle,
                    stopped_via_hotkey: true,
                },
            );
        }
        RecordingStatus::Processing => {
            log::debug!("Ignoring toggle - recording is processing");
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

use crate::domain::SttProviderType;

/// Update STT configuration
#[tauri::command]
pub async fn update_stt_config(
    state: State<'_, AppState>,
    app_handle: AppHandle,
    window: Window,
    provider: String,
    language: String,
    deepgram_api_key: Option<String>,
    assemblyai_api_key: Option<String>,
    model: Option<String>,
    // Важно: двойной Option позволяет отличить "поле не прислали" (None)
    // от "поле прислали как null" (Some(None)). Это нужно, чтобы
    // частичные обновления (например, только language) не затирали keyterms.
    deepgram_keyterms: Option<Option<String>>,
) -> Result<(), String> {
    log::info!("Command: update_stt_config - provider: {}, language: {}, model: {:?}", provider, language, model);

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

    // Whisper/model больше не используем в backend-only архитектуре.
    let _ = model;
    config.model = None;

    // В backend-only режиме keep-alive полезен: это снижает latency при повторном старте записи,
    // потому что мы переиспользуем WebSocket соединение с нашим сервером.
    //
    // Важно: keep-alive удерживает живые соединения в фоне. Если TTL сделать слишком большим,
    // можно упереться в лимиты параллельных соединений (и на нашей стороне, и на стороне провайдера, например Deepgram).
    // Поэтому по умолчанию держим включенным, но TTL оставляем коротким (см. stt_config.keep_alive_ttl_secs).
    config.keep_connection_alive = true;

    log::debug!("Setting keep_connection_alive={} for provider {:?}",
        config.keep_connection_alive, provider_type);

    // API ключи больше не используем в настройках (backend-only).
    let _ = deepgram_api_key;
    let _ = assemblyai_api_key;
    config.deepgram_api_key = None;
    config.assemblyai_api_key = None;

    // Keyterms для улучшения распознавания Deepgram
    // - None: не меняем существующее значение
    // - Some(None): очищаем
    // - Some(Some(v)): устанавливаем v
    if let Some(next) = deepgram_keyterms {
        config.deepgram_keyterms = next;
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
        || config.deepgram_keyterms != old_stt.deepgram_keyterms
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

/// Минимальный "public" снапшот app-config для фронтенда.
///
/// Важно: не включаем STT конфиг и тем более токены — снапшоты идут во все окна через IPC.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AppConfigSnapshotData {
    pub microphone_sensitivity: u8,
    pub recording_hotkey: String,
    pub auto_copy_to_clipboard: bool,
    pub auto_paste_text: bool,
    pub selected_audio_device: Option<String>,
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
        selected_audio_device: config.selected_audio_device,
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
    pub language: String,
    pub auto_detect_language: bool,
    pub enable_punctuation: bool,
    pub filter_profanity: bool,
    pub deepgram_api_key: Option<String>,
    pub assemblyai_api_key: Option<String>,
    pub model: Option<String>,
    pub keep_connection_alive: bool,
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
        language: config.language,
        auto_detect_language: config.auto_detect_language,
        enable_punctuation: config.enable_punctuation,
        filter_profanity: config.filter_profanity,
        deepgram_api_key: config.deepgram_api_key,
        assemblyai_api_key: config.assemblyai_api_key,
        model: config.model,
        keep_connection_alive: config.keep_connection_alive,
        deepgram_keyterms: config.deepgram_keyterms,
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
pub async fn get_auth_state_snapshot(state: State<'_, AppState>) -> Result<SnapshotEnvelope<AuthStateData>, String> {
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
pub async fn get_ui_preferences_snapshot(state: State<'_, AppState>) -> Result<SnapshotEnvelope<crate::domain::UiPreferences>, String> {
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
        if current.theme == theme && current.locale == locale && current.use_system_theme == use_system_theme {
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
    selected_audio_device: Option<String>,
) -> Result<(), String> {
    log::info!("Command: update_app_config - sensitivity: {:?}, hotkey: {:?}, auto_copy: {:?}, auto_paste: {:?}, device: {:?}",
        microphone_sensitivity, recording_hotkey, auto_copy_to_clipboard, auto_paste_text, selected_audio_device);

    // Защита от "тихих" провалов: если фронт случайно отправил snake_case ключи,
    // Tauri не сматчит аргументы, и сюда придут одни None.
    // Тогда лучше вернуть явную ошибку, чем сделать вид что всё ок.
    if microphone_sensitivity.is_none()
        && recording_hotkey.is_none()
        && auto_copy_to_clipboard.is_none()
        && auto_paste_text.is_none()
        && selected_audio_device.is_none()
    {
        return Err("update_app_config: не получены поля для обновления. Проверьте, что фронтенд отправляет args в camelCase (например microphoneSensitivity, recordingHotkey, autoCopyToClipboard, autoPasteText, selectedAudioDevice).".to_string());
    }

    let mut config = state.config.write().await;
    let mut hotkey_changed = false;
    let mut any_changed = false;

    if let Some(sensitivity) = microphone_sensitivity {
        let clamped = sensitivity.min(200); // Ensure 0-200 range
        if config.microphone_sensitivity != clamped {
            log::info!("Updating microphone sensitivity: {} -> {}", config.microphone_sensitivity, clamped);
            config.microphone_sensitivity = clamped;
            any_changed = true;
        }

        // Обновляем также в TranscriptionService для применения в реальном времени
        state.transcription_service.set_microphone_sensitivity(clamped).await;
    }

    if let Some(new_hotkey) = recording_hotkey {
        if new_hotkey != config.recording_hotkey {
            // Валидируем что это корректная комбинация клавиш
            use tauri_plugin_global_shortcut::Shortcut;
            if new_hotkey.parse::<Shortcut>().is_err() {
                return Err(format!("Неверный формат горячей клавиши: {}", new_hotkey));
            }

            log::info!("Updating recording hotkey: {} -> {}", config.recording_hotkey, new_hotkey);
            config.recording_hotkey = new_hotkey;
            hotkey_changed = true;
            any_changed = true;
        }
    }

    if let Some(auto_copy) = auto_copy_to_clipboard {
        if config.auto_copy_to_clipboard != auto_copy {
            log::info!("Updating auto_copy_to_clipboard: {} -> {}", config.auto_copy_to_clipboard, auto_copy);
            config.auto_copy_to_clipboard = auto_copy;
            any_changed = true;
        }
    }

    if let Some(auto_paste) = auto_paste_text {
        if config.auto_paste_text != auto_paste {
            log::info!("Updating auto_paste_text: {} -> {}", config.auto_paste_text, auto_paste);
            config.auto_paste_text = auto_paste;
            any_changed = true;
        }
    }

    let mut device_changed = false;
    if let Some(device) = selected_audio_device {
        let device_opt = if device.is_empty() { None } else { Some(device.clone()) };

        // Проверяем изменилось ли устройство
        if config.selected_audio_device != device_opt {
            log::info!("Updating selected_audio_device: {:?} -> {:?}", config.selected_audio_device, device_opt);
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

        state.recreate_audio_capture_with_device(device_opt.clone(), app_handle.clone())
            .await
            .map_err(|e| {
                log::error!("Failed to apply new audio device: {}", e);
                format!("Настройки сохранены, но не удалось применить новое устройство записи: {}", e)
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

use crate::infrastructure::audio::SystemAudioCapture;
use crate::domain::AudioConfig;

/// Start microphone test
#[tauri::command]
pub async fn start_microphone_test(
    state: State<'_, AppState>,
    app_handle: AppHandle,
    sensitivity: Option<u8>,
    device_name: Option<String>,
) -> Result<(), String> {
    log::info!("Command: start_microphone_test - device: {:?}", device_name);

    let mut test_state = state.microphone_test.write().await;

    if test_state.is_testing {
        return Err("Microphone test already running".to_string());
    }

    // Создаем новый audio capture для теста с выбранным устройством
    let device_to_use = device_name.filter(|s| !s.is_empty()); // None если пустая строка
    let mut capture = Box::new(
        SystemAudioCapture::with_device(device_to_use.clone())
            .map_err(|e| format!("Failed to create audio capture: {}", e))?,
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

    log::info!("Starting microphone test with sensitivity: {}%", sensitivity);

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
pub async fn stop_microphone_test(
    state: State<'_, AppState>,
) -> Result<Vec<i16>, String> {
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

    log::info!("Microphone test stopped, buffer size: {} samples", buffer.len());
    Ok(buffer)
}

//
// Hotkey Management Commands
//

/// Register or update recording hotkey
#[tauri::command]
pub async fn register_recording_hotkey(
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<(), String> {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};
    use std::sync::atomic::Ordering;

    let hotkey = state.config.read().await.recording_hotkey.clone();
    log::info!("Command: register_recording_hotkey - hotkey: {}", hotkey);

    // ВАЖНО: сначала убеждаемся, что хоткей парсится, и только потом снимаем текущие регистрации.
    // Иначе при ошибке парсинга мы останемся вообще без хоткея.
    let (effective_hotkey, shortcut) = match hotkey.parse::<Shortcut>() {
        Ok(sc) => (hotkey.clone(), sc),
        Err(parse_err) => {
            // Пытаемся нормализовать строку (например Backquote -> `).
            if let Some(normalized) = crate::infrastructure::hotkey::normalize_recording_hotkey(&hotkey) {
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
                                if let Err(e) = crate::infrastructure::ConfigStore::save_app_config(&config_snapshot).await {
                                    log::warn!("Failed to persist normalized hotkey to app_config.json: {}", e);
                                }
                            }
                        }
                        (normalized, sc)
                    }
                    Err(_) => {
                        // Фоллбек на дефолт: всегда должен работать.
                        let fallback = crate::infrastructure::hotkey::DEFAULT_RECORDING_HOTKEY.to_string();
                        let sc = fallback
                            .parse::<Shortcut>()
                            .map_err(|e| format!("Failed to parse fallback hotkey '{}': {}", fallback, e))?;
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
                        if let Err(e) = crate::infrastructure::ConfigStore::save_app_config(&config_snapshot).await {
                            log::warn!("Failed to persist fallback hotkey to app_config.json: {}", e);
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
                let sc = fallback
                    .parse::<Shortcut>()
                    .map_err(|e| format!("Failed to parse fallback hotkey '{}': {}", fallback, e))?;
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
                if let Err(e) = crate::infrastructure::ConfigStore::save_app_config(&config_snapshot).await {
                    log::warn!("Failed to persist fallback hotkey to app_config.json: {}", e);
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

    // Создаем обработчик - вызываем toggle напрямую вместо события
    // Важно: фильтруем только Pressed события, иначе срабатывает и на key down, и на key up
    app_handle.global_shortcut().on_shortcut(shortcut, move |app, _shortcut, event| {
        use tauri_plugin_global_shortcut::ShortcutState;
        if event.state != ShortcutState::Pressed {
            return;
        }
        log::debug!("Recording hotkey pressed");
        let app_clone = app.clone();
        let _ = tauri::async_runtime::spawn(async move {
            let state_opt = app_clone.try_state::<crate::presentation::state::AppState>();
            let window_opt = app_clone.get_webview_window("main");

            if let (Some(state), Some(window)) = (state_opt, window_opt) {
                let app_for_call = app_clone.clone();

                // Дебаунс: защищаемся от key repeat / двойных срабатываний.
                // Иначе окно может "мигать" (показ/скрытие несколько раз подряд).
                let now_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
                let last_ms = state.inner().last_recording_hotkey_ms.load(Ordering::Relaxed);
                let delta = now_ms.saturating_sub(last_ms);
                if delta < 450 {
                    log::debug!("Hotkey ignored (debounced): {}ms since last trigger", delta);
                    return;
                }
                state.inner().last_recording_hotkey_ms.store(now_ms, Ordering::Relaxed);

                if let Err(e) = crate::presentation::commands::toggle_recording_with_window_internal(
                    state.inner(),
                    window,
                    app_for_call,
                ).await {
                    log::error!("Failed to toggle recording: {}", e);
                }
            }
        });
    }).map_err(|e| format!("Failed to register hotkey '{}': {}", effective_hotkey, e))?;

    log::info!("Successfully registered hotkey: {}", effective_hotkey);
    Ok(())
}

/// Временно снять регистрацию горячей клавиши (пока пользователь настраивает новую)
#[tauri::command]
pub async fn unregister_recording_hotkey(
    app_handle: AppHandle,
) -> Result<(), String> {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;

    log::info!("Command: unregister_recording_hotkey - временно снимаем хоткей");

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
    crate::infrastructure::updater::check_for_update(app_handle).await
}

/// Check and install application update with user confirmation
#[tauri::command]
pub async fn install_update(app_handle: AppHandle) -> Result<String, String> {
    log::info!("Command: install_update");
    crate::infrastructure::updater::check_and_install_update(app_handle).await
}

//
// Whisper Model Management Commands
//

use crate::infrastructure::models::{
    WhisperModelInfo, download_model, get_available_models,
    is_model_downloaded, get_model_size, delete_model,
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
                model.description = format!("{} (Скачана, {} на диске)",
                    model.description, format_size_human(size));
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

        let _ = app_handle_progress.emit("whisper-model:download-progress", DownloadProgressPayload {
            model_name: model_name_progress.clone(),
            downloaded,
            total,
            progress,
        });
    };

    // Загружаем модель
    let model_path = download_model(&model_name, progress_callback)
        .await
        .map_err(|e| format!("Failed to download model: {}", e))?;

    // Эмитируем событие завершения загрузки
    let _ = app_handle.emit("whisper-model:download-completed", model_name.clone());

    log::info!("Model '{}' downloaded successfully to {:?}", model_name, model_path);
    Ok(format!("Model '{}' downloaded successfully", model_name))
}

/// Delete Whisper model
#[tauri::command]
pub async fn delete_whisper_model(model_name: String) -> Result<String, String> {
    log::info!("Command: delete_whisper_model - model: {}", model_name);

    delete_model(&model_name)
        .map_err(|e| format!("Failed to delete model: {}", e))?;

    Ok(format!("Model '{}' deleted successfully", model_name))
}

/// Get available audio input devices
#[tauri::command]
pub async fn get_audio_devices() -> Result<Vec<String>, String> {
    log::info!("Command: get_audio_devices");

    use cpal::traits::{HostTrait, DeviceTrait};

    let host = cpal::default_host();

    let devices: Vec<String> = host
        .input_devices()
        .map_err(|e| format!("Failed to enumerate input devices: {}", e))?
        .filter_map(|device| {
            device.name().ok()
        })
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
    crate::infrastructure::auto_paste::open_accessibility_settings()
        .map_err(|e| e.to_string())
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

    // Проверяем разрешение Accessibility на macOS
    #[cfg(target_os = "macos")]
    {
        if !crate::infrastructure::auto_paste::check_accessibility_permission() {
            return Err("Accessibility permission not granted. Please enable it in System Settings > Privacy & Security > Accessibility".to_string());
        }
    }

    // Получаем bundle ID последнего активного окна
    let last_bundle_id = state.last_focused_app_bundle_id.read().await.clone();

    // Не скрываем окно VoicetextAI - оставляем его видимым поверх всех
    // (оно уже настроено с alwaysOnTop: true в tauri.conf.json)

    // Если есть сохраненное окно - пытаемся активировать его
    if let Some(bundle_id) = last_bundle_id {
        log::info!("Attempting to activate last focused app: {}", bundle_id);

        match crate::infrastructure::auto_paste::activate_app_by_bundle_id(&bundle_id) {
            Ok(_) => {
                log::info!("✅ Successfully activated app: {}", bundle_id);
                // Даем время окну активироваться
                tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;
            }
            Err(e) => {
                log::warn!("⚠️ Failed to activate app '{}': {}", bundle_id, e);
                log::info!("💡 Will paste to currently active window instead");
                // Не критично - просто вставим в текущее активное окно
                // Даем небольшую паузу для переключения фокуса вручную если нужно
                tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            }
        }
    } else {
        log::info!("ℹ️ No saved window - pasting to currently active window");
    }

    // Вставляем текст в blocking thread (enigo работает с синхронными нативными API)
    let text_clone = text.clone();
    tokio::task::spawn_blocking(move || {
        crate::infrastructure::auto_paste::paste_text(&text_clone)
    })
    .await
    .map_err(|e| format!("Failed to join blocking task: {}", e))?
    .map_err(|e| format!("Failed to paste text: {}", e))?;

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
    log::debug!("Command: copy_to_clipboard_native - text length: {}", text.len());

    // Используем blocking task (arboard работает с синхронными системными API, как enigo)
    tokio::task::spawn_blocking(move || {
        crate::infrastructure::copy_to_clipboard(&text)
    })
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
pub async fn show_recording_window(app_handle: AppHandle) -> Result<(), String> {
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
        show_webview_window_on_active_monitor(&window)?;
        let _ = window.emit(EVENT_RECORDING_WINDOW_SHOWN, ());
        if let Err(e) = window.set_always_on_top(true) {
            log::warn!("Failed to enable always-on-top for main window: {}", e);
        }
    }

    Ok(())
}

/// Показывает settings окно и скрывает recording (main)
#[tauri::command]
pub async fn show_settings_window(
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<(), String> {
    log::info!("Command: show_settings_window");

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
            state.transcription_service
                .set_microphone_sensitivity(saved_app.microphone_sensitivity)
                .await;
        }

        if let Ok(mut saved_stt) = ConfigStore::load_config().await {
            // Держим auth token консистентным с AuthStore (Rust SoT).
            let token = state
                .auth_store
                .read()
                .await
                .session
                .as_ref()
                .map(|s| s.access_token.clone());
            saved_stt.backend_auth_token = token;
            let _ = state.transcription_service.update_config(saved_stt.clone()).await;
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
        let _ = settings.emit("settings-window-opened", true);
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
        let _ = profile.emit("profile-window-opened", serde_json::json!({
            "initialSection": initial_section.unwrap_or_else(|| "none".to_string())
        }));
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
    let (mut stt, loaded_from_disk) = match ConfigStore::load_config().await {
        Ok(c) => (c, true),
        Err(e) => {
            log::warn!(
                "Failed to load STT config for token update: {}. Using current in-memory config.",
                e
            );
            (state.transcription_service.get_config().await, false)
        }
    };
    stt.backend_auth_token = stt_token;
    // Важно: если чтение с диска упало, не перезаписываем файл дефолтами из-за побочного эффекта.
    // Токен всё равно надёжно хранится в AuthStore, а STT сервис обновим в памяти.
    if loaded_from_disk {
        if let Err(e) = ConfigStore::save_config(&stt).await {
            log::warn!("Failed to persist STT config token: {}", e);
        }
    }
    if let Err(e) = state.transcription_service.update_config(stt).await {
        log::warn!("Failed to update transcription service config token: {}", e);
    }

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
    log::info!("Command: set_authenticated - authenticated: {}", authenticated);

    let current_auth = *state.is_authenticated.read().await;
    if current_auth == authenticated {
        // Токен мог обновиться — проверяем и обновляем тихо (без bump revision)
        if authenticated {
            if let Some(ref t) = token {
                let (mut config, loaded_from_disk) = match ConfigStore::load_config().await {
                    Ok(c) => (c, true),
                    Err(e) => {
                        log::warn!(
                            "Failed to load STT config for token refresh: {}. Using current in-memory config.",
                            e
                        );
                        (state.transcription_service.get_config().await, false)
                    }
                };
                if config.backend_auth_token.as_deref() != Some(t.as_str()) {
                    config.backend_auth_token = Some(t.clone());
                    // Не перезаписываем файл, если чтение не удалось.
                    if loaded_from_disk {
                        let _ = ConfigStore::save_config(&config).await;
                    }
                    let _ = state.transcription_service.update_config(config).await;
                }
            }
        }
        return Ok(());
    }

    *state.is_authenticated.write().await = authenticated;

    // Сохраняем или очищаем backend auth token в конфиге
    let mut config = match ConfigStore::load_config().await {
        Ok(c) => c,
        Err(e) => {
            log::warn!("Failed to load STT config for auth change: {}. Using current in-memory config.", e);
            state.transcription_service.get_config().await
        }
    };
    if authenticated {
        if let Some(ref t) = token {
            log::info!("set_authenticated: received token with len: {}", t.len());
            config.backend_auth_token = Some(t.clone());
            log::info!("Backend auth token saved to config");
        } else {
            log::warn!("set_authenticated: authenticated=true but token is None!");
        }
    } else {
        // При логауте очищаем токен
        config.backend_auth_token = None;
        log::info!("Backend auth token cleared from config");
    }

    // Сохраняем конфиг и обновляем сервис
    // Если конфиг не удаётся сохранить — не считаем это фаталом для UX:
    // токен уже сохранён в AuthStore, а STT сервис обновим в памяти.
    if let Err(e) = ConfigStore::save_config(&config).await {
        log::warn!("Failed to save STT config during auth change: {}", e);
    }
    let _ = state.transcription_service.update_config(config).await;

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
