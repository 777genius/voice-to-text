// Clean Architecture layers
pub mod domain;
pub mod application;
pub mod infrastructure;
mod presentation;

use presentation::commands;
use presentation::state::AppState;
use tauri::Manager;
use infrastructure::ConfigStore;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Загружаем переменные окружения из .env файла (если есть) для dev режима
    // API ключи теперь встроены в build через embedded_keys.rs
    #[cfg(debug_assertions)]
    match dotenv::dotenv() {
        Ok(path) => println!("✅ Loaded .env file from: {:?}", path),
        Err(e) => println!("ℹ️  No .env file loaded: {}", e),
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_log::Builder::default()
                .level(if cfg!(debug_assertions) {
                    log::LevelFilter::Debug
                } else {
                    log::LevelFilter::Info
                })
                .build(),
        )
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            commands::start_recording,
            commands::stop_recording,
            commands::get_recording_status,
            commands::toggle_window,
            commands::toggle_recording_with_window,
            commands::minimize_window,
            commands::get_stt_config,
            commands::update_stt_config,
            commands::get_app_config,
            commands::update_app_config,
            commands::start_microphone_test,
            commands::stop_microphone_test,
            commands::register_recording_hotkey,
            commands::check_for_updates,
            commands::install_update,
            commands::get_available_whisper_models,
            commands::check_whisper_model,
            commands::download_whisper_model,
            commands::delete_whisper_model,
        ])
        .setup(|app| {
            #[cfg(debug_assertions)]
            {
                log::info!("Voice to Text application started in debug mode");
            }

            // Создаем system tray иконку
            if let Err(e) = presentation::tray::create_tray(app.handle()) {
                log::error!("Failed to create system tray: {}", e);
            }

            // Окно скрыто при старте независимо от режима
            // Открывается по горячей клавише (не забирает фокус)
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.hide();

                // Настраиваем обработчик закрытия окна
                // При попытке закрыть - скрываем вместо завершения приложения
                let window_clone = window.clone();
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        // Отменяем закрытие
                        api.prevent_close();
                        // Скрываем окно
                        let _ = window_clone.hide();
                        log::debug!("Window hidden instead of closed (app still running in tray)");
                    }
                });
            }

            // Загружаем сохраненные конфигурации
            // API ключи теперь берутся из embedded_keys.rs (встроены в build) или из пользовательской конфигурации
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                // Загружаем STT конфигурацию
                if let Ok(saved_config) = ConfigStore::load_config().await {
                    // API ключи теперь обрабатываются напрямую в провайдерах
                    // Приоритет: пользовательские ключи (deepgram_api_key/assemblyai_api_key) → встроенные ключи

                    if let Some(state) = app_handle.try_state::<AppState>() {
                        if let Err(e) = state.transcription_service.update_config(saved_config.clone()).await {
                            log::error!("Failed to load saved STT config: {}", e);
                        } else {
                            // Синхронизируем с AppConfig
                            state.config.write().await.stt = saved_config;
                            log::info!("Loaded saved STT configuration");
                        }
                    }
                }

                // Загружаем конфигурацию приложения
                if let Ok(saved_app_config) = ConfigStore::load_app_config().await {
                    if let Some(state) = app_handle.try_state::<AppState>() {
                        // Обновляем AppConfig в state
                        *state.config.write().await = saved_app_config.clone();

                        // Обновляем чувствительность микрофона в сервисе
                        state.transcription_service
                            .set_microphone_sensitivity(saved_app_config.microphone_sensitivity)
                            .await;

                        log::info!("Loaded saved app configuration (sensitivity: {}%)",
                            saved_app_config.microphone_sensitivity);
                    }
                }
            });

            // Регистрируем горячую клавишу для записи
            let app_handle_for_hotkey = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                // Ждем небольшую задержку чтобы конфигурация успела загрузиться
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

                // Вызываем команду регистрации горячей клавиши
                if let Some(state) = app_handle_for_hotkey.try_state::<AppState>() {
                    let handle = app_handle_for_hotkey.clone();
                    match commands::register_recording_hotkey(state, handle).await {
                        Ok(_) => log::info!("Recording hotkey registered successfully"),
                        Err(e) => {
                            log::error!("Failed to register recording hotkey: {}", e);
                            log::warn!("⚠️  Please change the hotkey in Settings to a different combination.");
                            #[cfg(target_os = "macos")]
                            log::warn!("    Recommended: Cmd+Shift+X, Alt+X, or Cmd+Shift+R");
                            #[cfg(not(target_os = "macos"))]
                            log::warn!("    Recommended: Ctrl+Shift+X, Alt+X, or Ctrl+Shift+R");
                        }
                    }
                }
            });

            // Запускаем обработчик VAD timeout событий
            if let Some(state) = app.try_state::<AppState>() {
                state.start_vad_timeout_handler(app.handle().clone());
            }

            // Запускаем фоновую проверку обновлений (каждые 6 часов)
            log::info!("Starting background update checker");
            infrastructure::updater::start_background_update_check(app.handle().clone());

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
