// Clean Architecture layers
pub mod domain;
pub mod application;
pub mod infrastructure;
mod presentation;

use presentation::commands;
use presentation::state::AppState;
use tauri::{Emitter, Manager};
use infrastructure::ConfigStore;

// Определяем базовый NSPanel класс для macOS (появление поверх fullscreen приложений)
#[cfg(target_os = "macos")]
use tauri_nspanel::tauri_panel;

#[cfg(target_os = "macos")]
tauri_panel! {
    panel!(FloatingPanel {
        config: {
            can_become_key_window: false,  // Критично для fullscreen! Активация через программный метод в auth режиме
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
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_store::Builder::default().build());

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
                // Глушим слишком многословные модули (огромные JSON в DEBUG)
                .level_for("tauri_plugin_updater", log::LevelFilter::Info)
                .level_for("reqwest", log::LevelFilter::Warn)
                .level_for("hyper", log::LevelFilter::Warn)
                .format(|out, message, record| {
                    use tauri_plugin_log::fern::colors::{Color, ColoredLevelConfig};

                    // Цвета для уровней логирования
                    let colors = ColoredLevelConfig::new()
                        .error(Color::Red)
                        .warn(Color::Yellow)
                        .info(Color::Green)
                        .debug(Color::Cyan)
                        .trace(Color::Magenta);

                    // Укорачиваем путь модуля - берём только последнюю часть
                    let target = record.target();
                    let short_target = target.rsplit("::").next().unwrap_or(target);

                    // Время в локальном формате
                    let now = chrono::Local::now();
                    let time_str = now.format("%H:%M:%S");

                    // Форматируем лог: время серым, уровень цветной, модуль серым, сообщение белым
                    out.finish(format_args!(
                        "\x1b[90m{}\x1b[0m {} \x1b[90m{}\x1b[0m  {}",
                        time_str,
                        colors.color(record.level()),
                        short_target,
                        message
                    ))
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
            commands::update_stt_config,
            commands::get_app_config_snapshot,
            commands::get_stt_config_snapshot,
            commands::get_auth_state_snapshot,
            commands::get_ui_preferences_snapshot,
            commands::update_ui_preferences,
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
            commands::show_auth_window,
            commands::show_recording_window,
            commands::show_settings_window,
            commands::set_authenticated,
        ])
        .setup(|app| {
            #[cfg(debug_assertions)]
            {
                log::info!("Voice to Text application started in debug mode");
            }

            // E2E режим: нужен для WebDriver тестов (Linux/Windows), чтобы:
            // - main окно было видно сразу
            // - не блокироваться на auth UI
            //
            // Важно: включаем только в debug, чтобы это не могло случайно попасть в релиз.
            #[cfg(debug_assertions)]
            let is_e2e = std::env::var("VOICETEXT_E2E").ok().as_deref() == Some("1");
            #[cfg(not(debug_assertions))]
            let is_e2e = false;

            if is_e2e {
                let state = app.state::<AppState>();
                tauri::async_runtime::block_on(async {
                    *state.is_authenticated.write().await = true;
                });
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
                    use tauri_nspanel::{WebviewWindowExt as _, CollectionBehavior, PanelLevel};

                    let app_handle = app.handle().clone();
                    let window_clone = window.clone();

                    // Конвертация в NSPanel должна происходить на главном потоке
                    if let Err(e) = app_handle.run_on_main_thread(move || {
                        match window_clone.to_panel::<FloatingPanel>() {
                            Ok(panel) => {
                                log::info!("Окно успешно конвертировано в NSPanel (macOS)");

                                // Устанавливаем nonactivatingPanel style mask - окно не забирает фокус
                                // Это критично для появления поверх fullscreen приложений
                                use tauri_nspanel::StyleMask;
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
                                log::info!("✅ NSPanel настроен для появления поверх fullscreen");
                            },
                            Err(e) => {
                                log::warn!("⚠️  Не удалось конвертировать окно в NSPanel: {} (используем обычное окно)", e);
                            }
                        }
                    }) {
                        log::error!("Failed to run NSPanel conversion on main thread: {}", e);
                    }
                }

                if is_e2e {
                    let _ = window.show();
                } else {
                    let _ = window.hide();
                }

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

            // Настраиваем auth окно (обычное NSWindow - клавиатура работает нормально)
            if let Some(auth_window) = app.get_webview_window("auth") {
                // Auth окно НЕ конвертируем в NSPanel - остаётся обычным NSWindow
                // Клавиатура работает как положено, но окно не появляется поверх fullscreen
                let _ = auth_window.hide();

                // Обработчик закрытия - скрываем вместо закрытия
                let auth_clone = auth_window.clone();
                auth_window.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = auth_clone.hide();
                        log::debug!("Auth window hidden instead of closed");
                    }
                });

                log::info!("Auth window configured (regular NSWindow for keyboard input)");
            }

            // Загружаем сохраненные конфигурации
            // API ключи теперь берутся из embedded_keys.rs (встроены в build) или из пользовательской конфигурации
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                // Загружаем STT конфигурацию
                if let Ok(mut saved_config) = ConfigStore::load_config().await {
                    // API ключи теперь обрабатываются напрямую в провайдерах
                    // Приоритет: пользовательские ключи (deepgram_api_key/assemblyai_api_key) → встроенные ключи

                    if let Some(state) = app_handle.try_state::<AppState>() {
                        // Backend-only режим: по умолчанию держим соединение живым между сессиями записи.
                        // TTL короткий (см. stt_config.keep_alive_ttl_secs), чтобы не держать "висящие" коннекты в фоне
                        // и не упереться в лимиты параллельных соединений провайдера (например Deepgram).
                        let mut config_migrated = false;
                        if saved_config.provider == crate::domain::SttProviderType::Backend
                            && !saved_config.keep_connection_alive
                        {
                            saved_config.keep_connection_alive = true;
                            config_migrated = true;
                            log::info!(
                                "Enabled keep_connection_alive for backend provider by default (ttl={}s)",
                                saved_config.keep_alive_ttl_secs
                            );
                        }

                        // Best-effort: сохраняем миграцию обратно на диск, чтобы настройка была стабильной.
                        if config_migrated {
                            if let Err(e) = ConfigStore::save_config(&saved_config).await {
                                log::warn!("Failed to persist migrated STT config: {}", e);
                            }
                        }

                        // Сохраняем токен если он уже был установлен (race condition с Vue set_authenticated)
                        let current_config = state.transcription_service.get_config().await;
                        if current_config.backend_auth_token.is_some() && saved_config.backend_auth_token.is_none() {
                            log::info!("Preserving existing backend_auth_token from current config");
                            saved_config.backend_auth_token = current_config.backend_auth_token;
                        }

                        if let Err(e) = state.transcription_service.update_config(saved_config.clone()).await {
                            log::error!("Failed to load saved STT config: {}", e);
                        } else {
                            // Синхронизируем с AppConfig
                            state.config.write().await.stt = saved_config;
                            log::info!("Loaded saved STT configuration");

                            // Важно: загрузка идёт асинхронно, и окна могут успеть стартануть sync раньше.
                            // Поэтому после успешной загрузки мы обязаны пнуть invalidation, иначе UI может остаться на дефолтах.
                            let revision = AppState::bump_revision(&state.stt_config_revision).await;
                            let _ = app_handle.emit(
                                crate::presentation::EVENT_STATE_SYNC_INVALIDATION,
                                crate::presentation::StateSyncInvalidationPayload {
                                    topic: "stt-config".to_string(),
                                    revision,
                                    source_id: None,
                                    timestamp_ms: chrono::Utc::now().timestamp_millis(),
                                },
                            );
                        }
                    }
                }

                // Загружаем конфигурацию приложения
                if let Ok(saved_app_config) = ConfigStore::load_app_config().await {
                    if let Some(state) = app_handle.try_state::<AppState>() {
                        *state.config.write().await = saved_app_config.clone();

                        state.transcription_service
                            .set_microphone_sensitivity(saved_app_config.microphone_sensitivity)
                            .await;

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

                        // Аналогично STT: после асинхронной загрузки пинаем invalidation.
                        let revision = AppState::bump_revision(&state.app_config_revision).await;
                        let _ = app_handle.emit(
                            crate::presentation::EVENT_STATE_SYNC_INVALIDATION,
                            crate::presentation::StateSyncInvalidationPayload {
                                topic: "app-config".to_string(),
                                revision,
                                source_id: None,
                                timestamp_ms: chrono::Utc::now().timestamp_millis(),
                            },
                        );
                    }
                }

                // Загружаем UI-настройки
                if let Some(state) = app_handle.try_state::<AppState>() {
                    match ConfigStore::load_ui_preferences().await {
                        Ok(prefs) => {
                            log::info!("Loaded UI preferences: theme={}, locale={}", prefs.theme, prefs.locale);
                            *state.ui_preferences.write().await = prefs;

                            // Пинаем invalidation после загрузки prefs, чтобы окна, которые уже стартанули, догнали SoT.
                            let revision = AppState::bump_revision(&state.ui_preferences_revision).await;
                            let _ = app_handle.emit(
                                crate::presentation::EVENT_STATE_SYNC_INVALIDATION,
                                crate::presentation::StateSyncInvalidationPayload {
                                    topic: "ui-preferences".to_string(),
                                    revision,
                                    source_id: None,
                                    timestamp_ms: chrono::Utc::now().timestamp_millis(),
                                },
                            );
                        }
                        Err(e) => {
                            log::warn!("Failed to load UI preferences: {}", e);
                        }
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

            // Настраиваем deep link handler для OAuth callback
            #[cfg(desktop)]
            {
                use tauri_plugin_deep_link::DeepLinkExt;

                // Регистрируем URL scheme
                if let Err(e) = app.deep_link().register("voicetotext") {
                    log::warn!("Failed to register deep link: {}", e);
                }

                // Обработчик deep link событий
                let handle = app.handle().clone();
                app.deep_link().on_open_url(move |event| {
                    let urls = event.urls();
                    for url in urls {
                        log::info!("Received deep link: {}", url);
                        if let Some(window) = handle.get_webview_window("main") {
                            let _ = window.emit("deep-link", url.to_string());
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                });
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, _event| {
            // Клик по иконке в Dock (только macOS)
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { has_visible_windows, .. } = _event {
                if !has_visible_windows {
                    if let Some(window) = _app.get_webview_window("main") {
                        if let Err(e) = crate::presentation::commands::show_webview_window_on_active_monitor(&window) {
                            log::error!("Failed to show window on Dock click: {}", e);
                            let _ = window.show();
                        }
                        let _ = window.set_focus();
                    }
                }
            }
        });
}
