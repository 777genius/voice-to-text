// Clean Architecture layers
pub mod domain;
pub mod application;
pub mod infrastructure;
mod presentation;

use presentation::commands;
use presentation::state::AppState;
use tauri::Manager;
use infrastructure::ConfigStore;

// Определяем базовый NSPanel класс для macOS (появление поверх fullscreen приложений)
#[cfg(target_os = "macos")]
use tauri_nspanel::tauri_panel;

#[cfg(target_os = "macos")]
tauri_panel! {
    panel!(FloatingPanel {
        config: {
            can_become_key_window: false,
            can_become_main_window: false
        }
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Загружаем переменные окружения из .env файла (если есть) для dev режима
    // API ключи теперь встроены в build через embedded_keys.rs
    #[cfg(debug_assertions)]
    match dotenv::dotenv() {
        Ok(path) => println!("✅ Loaded .env file from: {:?}", path),
        Err(e) => println!("ℹ️  No .env file loaded: {}", e),
    }

    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_dialog::init());

    // Добавляем NSPanel плагин на macOS для появления поверх fullscreen приложений
    #[cfg(target_os = "macos")]
    {
        builder = builder.plugin(tauri_nspanel::init());
    }

    builder
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
            commands::get_audio_devices,
            commands::check_accessibility_permission,
            commands::request_accessibility_permission,
            commands::auto_paste_text,
            commands::copy_to_clipboard_native,
        ])
        .setup(|app| {
            #[cfg(debug_assertions)]
            {
                log::info!("Voice to Text application started in debug mode");
            }

            // ЗАПАСНОЙ ВАРИАНТ: Если NSPanel с StyleMask не работает поверх fullscreen,
            // раскомментируйте строку ниже. Окно гарантированно появится поверх ВСЕГО,
            // но иконка исчезнет из Dock (app станет фоновым сервисом).
            // #[cfg(target_os = "macos")]
            // app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            // Создаем system tray иконку
            if let Err(e) = presentation::tray::create_tray(app.handle()) {
                log::error!("Failed to create system tray: {}", e);
            }

            // Окно скрыто при старте независимо от режима
            // Открывается по горячей клавише (не забирает фокус)
            if let Some(window) = app.get_webview_window("main") {
                // На macOS конвертируем окно в NSPanel для появления поверх fullscreen приложений
                #[cfg(target_os = "macos")]
                {
                    use tauri_nspanel::{WebviewWindowExt as _, CollectionBehavior, PanelLevel, StyleMask};

                    let app_handle = app.handle().clone();
                    let window_clone = window.clone();

                    // Конвертация в NSPanel должна происходить на главном потоке
                    if let Err(e) = app_handle.run_on_main_thread(move || {
                        match window_clone.to_panel::<FloatingPanel>() {
                            Ok(panel) => {
                                log::info!("✅ Окно успешно конвертировано в NSPanel (macOS)");

                                // КРИТИЧНО: Устанавливаем nonactivatingPanel style mask
                                // Без этого NSPanel НЕ МОЖЕТ появляться поверх fullscreen (требование macOS)
                                panel.set_style_mask(StyleMask::empty().nonactivating_panel().into());
                                log::info!("🎭 Установлен style mask: nonactivating_panel");

                                // Устанавливаем максимальный window level для появления поверх fullscreen
                                panel.set_level(PanelLevel::ScreenSaver.value());
                                log::info!("🔝 Установлен window level = ScreenSaver (1000)");

                                // Настраиваем collection behavior для работы с fullscreen приложениями
                                panel.set_collection_behavior(
                                    CollectionBehavior::new()
                                        .full_screen_auxiliary()  // Работает с fullscreen приложениями
                                        .can_join_all_spaces()    // Видно на всех Spaces
                                        .into(),
                                );
                                log::info!("🎯 Установлен collection behavior: fullscreen_auxiliary + can_join_all_spaces");
                                log::info!("✅ NSPanel ПОЛНОСТЬЮ настроен - будет появляться поверх fullscreen приложений!");
                            },
                            Err(e) => {
                                log::warn!("⚠️  Не удалось конвертировать окно в NSPanel: {} (используем обычное окно)", e);
                            }
                        }
                    }) {
                        log::error!("Failed to run NSPanel conversion on main thread: {}", e);
                    }
                }

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

                        // Применяем выбранное устройство записи (если указано)
                        if let Err(e) = state.recreate_audio_capture_with_device(
                            saved_app_config.selected_audio_device.clone(),
                            app_handle.clone()
                        ).await {
                            log::error!("Failed to apply selected audio device: {}", e);
                            log::warn!("Using default audio device instead");
                        } else if saved_app_config.selected_audio_device.is_some() {
                            log::info!("Applied selected audio device: {:?}", saved_app_config.selected_audio_device);
                        }

                        log::info!("Loaded saved app configuration (sensitivity: {}%, device: {:?})",
                            saved_app_config.microphone_sensitivity, saved_app_config.selected_audio_device);
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
