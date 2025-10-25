use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State, Window};

use crate::domain::{RecordingStatus, AudioCapture};
use crate::infrastructure::ConfigStore;
use crate::presentation::{
    events::*, AppState, AudioLevelPayload, FinalTranscriptionPayload, PartialTranscriptionPayload,
    RecordingStatusPayload, MicrophoneTestLevelPayload, TranscriptionErrorPayload,
};

/// Start recording voice
#[tauri::command]
pub async fn start_recording(
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<String, String> {
    log::info!("Command: start_recording");

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
            let payload = PartialTranscriptionPayload::from(transcription);
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
            let payload = FinalTranscriptionPayload::from(transcription.clone());
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

    let app_handle_error = app_handle.clone();

    // Callback for error handling
    let on_error = Arc::new(move |error: String, error_type: String| {
        let app_handle = app_handle_error.clone();

        tokio::spawn(async move {
            log::error!("STT error occurred: {} (type: {})", error, error_type);

            // Emit error event to frontend
            let payload = TranscriptionErrorPayload { error, error_type };
            if let Err(e) = app_handle.emit(EVENT_TRANSCRIPTION_ERROR, payload) {
                log::error!("Failed to emit transcription error event: {}", e);
            }

            // Emit Error status
            let _ = app_handle.emit(
                EVENT_RECORDING_STATUS,
                RecordingStatusPayload {
                    status: RecordingStatus::Error,
                    stopped_via_hotkey: false,
                },
            );
        });
    });

    // Emit Starting status immediately
    log::debug!("Emitting status: Starting (stopped_via_hotkey: false)");
    let _ = app_handle.emit(
        EVENT_RECORDING_STATUS,
        RecordingStatusPayload {
            status: RecordingStatus::Starting,
            stopped_via_hotkey: false,
        },
    );

    // Start recording (async - WebSocket connect, audio capture start)
    state
        .transcription_service
        .start_recording(on_partial, on_final, on_audio_level, on_error)
        .await
        .map_err(|e| e.to_string())?;

    // Emit Recording status after successful start
    log::debug!("Emitting status: Recording (stopped_via_hotkey: false)");
    let _ = app_handle.emit(
        EVENT_RECORDING_STATUS,
        RecordingStatusPayload {
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

        window.show().map_err(|e| e.to_string())?;
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

                window.show().map_err(|e| e.to_string())?;
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

            log::info!("Recording stopped via hotkey, waiting for final transcription");

            // Эмитируем статус Idle с флагом stopped_via_hotkey
            // Frontend скроет окно когда получит этот статус
            log::info!("Emitting status: Idle (stopped_via_hotkey: TRUE) - window will auto-hide");
            let _ = app_handle.emit(
                EVENT_RECORDING_STATUS,
                RecordingStatusPayload {
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

use crate::domain::{AppConfig, SttConfig, SttProviderType};

/// Get current STT configuration
#[tauri::command]
pub async fn get_stt_config(state: State<'_, AppState>) -> Result<SttConfig, String> {
    log::debug!("Command: get_stt_config");
    let config = state.transcription_service.get_config().await;
    Ok(config)
}

/// Update STT configuration
#[tauri::command]
pub async fn update_stt_config(
    state: State<'_, AppState>,
    provider: String,
    language: String,
    deepgram_api_key: Option<String>,
    assemblyai_api_key: Option<String>,
    model: Option<String>,
) -> Result<(), String> {
    log::info!("Command: update_stt_config - provider: {}, language: {}, model: {:?}", provider, language, model);

    // Парсим provider type
    let provider_type = match provider.to_lowercase().as_str() {
        "assemblyai" | "assembly-ai" => SttProviderType::AssemblyAI,
        "deepgram" => SttProviderType::Deepgram,
        "whisper" | "whisper-local" => SttProviderType::WhisperLocal,
        "google" | "google-cloud" => SttProviderType::GoogleCloud,
        "azure" => SttProviderType::Azure,
        _ => return Err(format!("Unknown STT provider: {}", provider)),
    };

    // Загружаем существующую конфигурацию из файла (если есть)
    let mut config = ConfigStore::load_config().await.unwrap_or_default();

    // Обновляем только переданные параметры
    config.provider = provider_type;
    config.language = language;

    // Обновляем модель если передана
    if let Some(model_name) = model {
        config.model = Some(model_name);
    }

    // Автоматически устанавливаем keep_connection_alive в зависимости от провайдера
    // Deepgram: безопасно (биллит по длительности аудио, не по времени соединения)
    // AssemblyAI: опасно (биллит по времени соединения)
    config.keep_connection_alive = matches!(provider_type, SttProviderType::Deepgram);

    log::debug!("Setting keep_connection_alive={} for provider {:?}",
        config.keep_connection_alive, provider_type);

    // Обновляем пользовательские API ключи если они переданы
    // Пустая строка означает "очистить" (использовать встроенный ключ)
    if let Some(key) = deepgram_api_key {
        if key.trim().is_empty() {
            config.deepgram_api_key = None; // Очистить (будет использован встроенный)
            log::debug!("Deepgram API key cleared (will use embedded key)");
        } else {
            config.deepgram_api_key = Some(key);
            log::debug!("Deepgram API key updated");
        }
    }

    if let Some(key) = assemblyai_api_key {
        if key.trim().is_empty() {
            config.assemblyai_api_key = None; // Очистить (будет использован встроенный)
            log::debug!("AssemblyAI API key cleared (will use embedded key)");
        } else {
            config.assemblyai_api_key = Some(key);
            log::debug!("AssemblyAI API key updated");
        }
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

    log::info!("STT configuration updated and saved successfully");
    Ok(())
}

//
// App Configuration Commands
//

/// Get current application configuration
#[tauri::command]
pub async fn get_app_config(state: State<'_, AppState>) -> Result<AppConfig, String> {
    log::debug!("Command: get_app_config");
    let config = state.config.read().await.clone();
    Ok(config)
}

/// Update application configuration (e.g., microphone sensitivity, recording hotkey, auto-copy/paste)
#[tauri::command]
pub async fn update_app_config(
    state: State<'_, AppState>,
    app_handle: AppHandle,
    microphone_sensitivity: Option<u8>,
    recording_hotkey: Option<String>,
    auto_copy_to_clipboard: Option<bool>,
    auto_paste_text: Option<bool>,
    selected_audio_device: Option<String>,
) -> Result<(), String> {
    log::info!("Command: update_app_config - sensitivity: {:?}, hotkey: {:?}, auto_copy: {:?}, auto_paste: {:?}, device: {:?}",
        microphone_sensitivity, recording_hotkey, auto_copy_to_clipboard, auto_paste_text, selected_audio_device);

    let mut config = state.config.write().await;
    let mut hotkey_changed = false;

    if let Some(sensitivity) = microphone_sensitivity {
        let clamped = sensitivity.min(200); // Ensure 0-200 range
        log::info!("Updating microphone sensitivity: {} -> {}", config.microphone_sensitivity, clamped);
        config.microphone_sensitivity = clamped;

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
        }
    }

    if let Some(auto_copy) = auto_copy_to_clipboard {
        log::info!("Updating auto_copy_to_clipboard: {} -> {}", config.auto_copy_to_clipboard, auto_copy);
        config.auto_copy_to_clipboard = auto_copy;
    }

    if let Some(auto_paste) = auto_paste_text {
        log::info!("Updating auto_paste_text: {} -> {}", config.auto_paste_text, auto_paste);
        config.auto_paste_text = auto_paste;
    }

    let mut device_changed = false;
    if let Some(device) = selected_audio_device {
        let device_opt = if device.is_empty() { None } else { Some(device.clone()) };

        // Проверяем изменилось ли устройство
        if config.selected_audio_device != device_opt {
            log::info!("Updating selected_audio_device: {:?} -> {:?}", config.selected_audio_device, device_opt);
            config.selected_audio_device = device_opt;
            device_changed = true;
        }
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
        let gain = if sensitivity <= 100 {
            // 0-100% → 0.0x-1.0x (приглушение/нормальный уровень)
            sensitivity as f32 / 100.0
        } else {
            // 100-200% → 1.0x-5.0x (усиление для тихих микрофонов)
            1.0 + (sensitivity - 100) as f32 / 100.0 * 4.0
        };

        log::info!("Microphone test: sensitivity={}%, gain={:.2}x", sensitivity, gain);

        while let Some(chunk) = rx.recv().await {
            // Вычисляем уровень громкости ДО усиления
            let max_amplitude = chunk.data.iter().map(|&s| s.abs()).max().unwrap_or(0);
            let normalized_level = (max_amplitude as f32 / 32767.0).sqrt().min(1.0);

            // Отправляем событие в UI (показываем уровень ДО усиления для честной индикации)
            let _ = app_handle_clone.emit(
                EVENT_MICROPHONE_TEST_LEVEL,
                MicrophoneTestLevelPayload {
                    level: normalized_level,
                },
            );

            // Применяем gain к каждому сэмплу с защитой от clipping
            let amplified_data: Vec<i16> = chunk.data.iter()
                .map(|&sample| {
                    let amplified = (sample as f32 * gain).clamp(-32767.0, 32767.0);
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

    let hotkey = state.config.read().await.recording_hotkey.clone();
    log::info!("Command: register_recording_hotkey - hotkey: {}", hotkey);

    // Отменяем все старые регистрации
    if let Err(e) = app_handle.global_shortcut().unregister_all() {
        log::warn!("Failed to unregister all shortcuts: {}", e);
    }

    // Парсим новую горячую клавишу
    let shortcut = hotkey.parse::<Shortcut>()
        .map_err(|e| format!("Failed to parse hotkey '{}': {}", hotkey, e))?;

    // Создаем обработчик
    app_handle.global_shortcut().on_shortcut(shortcut, move |app, _event, _shortcut| {
        log::debug!("Recording hotkey pressed");
        let _ = tauri::async_runtime::block_on(async {
            use tauri::Emitter;
            let _ = app.emit("hotkey:toggle-recording", ());
        });
    }).map_err(|e| format!("Failed to set hotkey handler: {}", e))?;

    // Регистрируем
    app_handle.global_shortcut().register(shortcut)
        .map_err(|e| format!("Failed to register hotkey '{}': {}", hotkey, e))?;

    log::info!("Successfully registered hotkey: {}", hotkey);
    Ok(())
}

//
// Update Commands
//

/// Check for application updates
#[tauri::command]
pub async fn check_for_updates(app_handle: AppHandle) -> Result<Option<String>, String> {
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

    // Не скрываем окно Voice to Text - оставляем его видимым поверх всех
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

    // Возвращаем окно Voice to Text поверх всех окон (но без фокуса)
    if let Some(window) = app_handle.get_webview_window("main") {
        let _ = window.set_always_on_top(true);
        log::debug!("Voice to Text window kept on top");
    }

    log::info!("Text auto-pasted successfully");
    Ok(())
}
