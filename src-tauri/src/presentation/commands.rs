use std::sync::Arc;
use tauri::{AppHandle, Emitter, State, Window};

use crate::domain::{RecordingStatus, AudioCapture};
use crate::infrastructure::ConfigStore;
use crate::presentation::{
    events::*, AppState, AudioLevelPayload, FinalTranscriptionPayload, PartialTranscriptionPayload,
    RecordingStatusPayload, MicrophoneTestLevelPayload,
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
        .start_recording(on_partial, on_final, on_audio_level)
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
pub async fn toggle_window(window: Window) -> Result<(), String> {
    log::info!("Command: toggle_window");

    if window.is_visible().map_err(|e| e.to_string())? {
        window.hide().map_err(|e| e.to_string())?;
    } else {
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
                window.show().map_err(|e| e.to_string())?;
            }

            // Запускаем запись
            start_recording(state, app_handle).await?;
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
    api_key: Option<String>,
) -> Result<(), String> {
    log::info!("Command: update_stt_config - provider: {}, language: {}", provider, language);

    // Парсим provider type
    let provider_type = match provider.to_lowercase().as_str() {
        "assemblyai" | "assembly-ai" => SttProviderType::AssemblyAI,
        "deepgram" => SttProviderType::Deepgram,
        "whisper" | "whisper-local" => SttProviderType::WhisperLocal,
        "google" | "google-cloud" => SttProviderType::GoogleCloud,
        "azure" => SttProviderType::Azure,
        _ => return Err(format!("Unknown STT provider: {}", provider)),
    };

    // ВАЖНО: Загружаем существующую конфигурацию из файла (если есть)
    // чтобы не потерять сохраненный API ключ при изменении других настроек
    let mut config = ConfigStore::load_config().await.unwrap_or_default();

    // Обновляем только переданные параметры
    config.provider = provider_type;
    config.language = language;

    // Автоматически устанавливаем keep_connection_alive в зависимости от провайдера
    // Deepgram: безопасно (биллит по длительности аудио, не по времени соединения)
    // AssemblyAI: опасно (биллит по времени соединения)
    config.keep_connection_alive = matches!(provider_type, SttProviderType::Deepgram);

    log::debug!("Setting keep_connection_alive={} for provider {:?}",
        config.keep_connection_alive, provider_type);

    // Логика обработки API ключа:
    // 1. Если передан новый ключ (непустой) - обновляем его
    // 2. Если не передан - пытаемся загрузить из переменных окружения
    // 3. Иначе оставляем существующий из конфига
    let has_new_key = api_key.as_ref().map_or(false, |k| !k.trim().is_empty());

    if has_new_key {
        // Передан новый непустой ключ - используем его
        config.api_key = api_key.clone();
        log::debug!("Using new API key provided in request");
    } else if config.api_key.is_none() {
        // Ключ отсутствует - пытаемся загрузить из .env
        config.api_key = match provider_type {
            SttProviderType::AssemblyAI => {
                std::env::var("ASSEMBLYAI_API_KEY").ok()
            }
            SttProviderType::Deepgram => {
                std::env::var("DEEPGRAM_API_KEY").ok()
            }
            SttProviderType::GoogleCloud => {
                std::env::var("GOOGLE_CLOUD_API_KEY").ok()
            }
            _ => None,
        };
        if config.api_key.is_some() {
            log::debug!("Loaded API key from environment variable");
        }
    } else {
        // Ключ не передан - оставляем существующий без изменений
        log::debug!("Keeping existing API key from config file");
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

/// Update application configuration (e.g., microphone sensitivity, recording hotkey)
#[tauri::command]
pub async fn update_app_config(
    state: State<'_, AppState>,
    app_handle: AppHandle,
    microphone_sensitivity: Option<u8>,
    recording_hotkey: Option<String>,
) -> Result<(), String> {
    log::info!("Command: update_app_config - sensitivity: {:?}, hotkey: {:?}",
        microphone_sensitivity, recording_hotkey);

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

    log::info!("Saving app config to disk: sensitivity={}, hotkey={}, provider={:?}, language={}",
        config.microphone_sensitivity, config.recording_hotkey, config.stt.provider, config.stt.language);

    // Сохраняем конфигурацию на диск
    ConfigStore::save_app_config(&config)
        .await
        .map_err(|e| format!("Failed to save app config: {}", e))?;

    // Если горячая клавиша изменилась - перерегистрируем её
    if hotkey_changed {
        drop(config); // освобождаем lock перед async операцией

        log::info!("Re-registering recording hotkey");

        // Перерегистрируем горячую клавишу
        register_recording_hotkey(state.clone(), app_handle).await?;
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
) -> Result<(), String> {
    log::info!("Command: start_microphone_test");

    let mut test_state = state.microphone_test.write().await;

    if test_state.is_testing {
        return Err("Microphone test already running".to_string());
    }

    // Создаем новый audio capture для теста
    let mut capture = Box::new(
        SystemAudioCapture::new()
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
        // Вычисляем порог на основе чувствительности (та же логика что в TranscriptionService)
        let threshold = if sensitivity >= 100 {
            0
        } else {
            ((100 - sensitivity) as f32 / 100.0 * 32767.0) as i16
        };

        log::info!("Microphone test threshold: {} (sensitivity: {}%)", threshold, sensitivity);

        while let Some(chunk) = rx.recv().await {
            // Вычисляем уровень громкости
            // Используем перцептивную нормализацию (корень квадратный) как в VU-метрах
            let max_amplitude = chunk.data.iter().map(|&s| s.abs()).max().unwrap_or(0);
            let normalized_level = (max_amplitude as f32 / 32767.0).sqrt().min(1.0);

            // Отправляем событие в UI (показываем реальный уровень независимо от порога)
            let _ = app_handle_clone.emit(
                EVENT_MICROPHONE_TEST_LEVEL,
                MicrophoneTestLevelPayload {
                    level: normalized_level,
                },
            );

            // Применяем фильтрацию по чувствительности
            // Сохраняем в буфер только звук выше порога (как при реальной записи)
            if max_amplitude >= threshold {
                let mut buffer = buffer_for_task.lock().await;
                buffer.extend_from_slice(&chunk.data);
                // Ограничиваем размер буфера (максимум 5 секунд = 80000 samples @ 16kHz)
                let buffer_len = buffer.len();
                if buffer_len > 80000 {
                    buffer.drain(0..buffer_len - 80000);
                }
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
