use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, Runtime,
};

use crate::infrastructure::config_store::ConfigStore;
use crate::presentation::commands::{
    show_webview_window_on_active_monitor, show_webview_window_with_recording_config,
};
use crate::presentation::events::{
    EVENT_RECORDING_WINDOW_SHOWN, EVENT_SETTINGS_FOCUS_UPDATES, EVENT_SETTINGS_WINDOW_OPENED,
};

async fn show_main_window_from_tray<R: Runtime>(app: &AppHandle<R>) {
    if let Some(state) = app.try_state::<crate::presentation::state::AppState>() {
        if !*state.is_authenticated.read().await {
            log::info!("Tray open: user is not authenticated, opening auth window");
            show_auth_window_from_tray(app).await;
            return;
        }
    }

    if let Some(auth) = app.get_webview_window("auth") {
        let _ = auth.hide();
    }
    if let Some(profile) = app.get_webview_window("profile") {
        let _ = profile.hide();
    }
    if let Some(settings) = app.get_webview_window("settings") {
        let _ = settings.hide();
    }
    if let Some(window) = app.get_webview_window("main") {
        let show_result =
            if let Some(state) = app.try_state::<crate::presentation::state::AppState>() {
                let config = state.config.read().await.clone();
                show_webview_window_with_recording_config(&window, &config, state.inner())
            } else {
                show_webview_window_on_active_monitor(&window)
            };
        if let Err(e) = show_result {
            log::error!("Failed to show window: {}", e);
        }
        let _ = window.emit(EVENT_RECORDING_WINDOW_SHOWN, ());
        if let Err(e) = window.set_focus() {
            log::error!("Failed to focus window: {}", e);
        }
        crate::infrastructure::updater::request_interactive_update_check(
            app.clone(),
            "tray_show_main",
        );
    }
}

async fn show_auth_window_from_tray<R: Runtime>(app: &AppHandle<R>) {
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.set_always_on_top(false);
        let _ = main.hide();
    }
    if let Some(settings) = app.get_webview_window("settings") {
        let _ = settings.hide();
    }
    if let Some(profile) = app.get_webview_window("profile") {
        let _ = profile.hide();
    }
    if let Some(auth) = app.get_webview_window("auth") {
        if let Err(e) = show_webview_window_on_active_monitor(&auth) {
            log::error!("Failed to show auth window from tray: {}", e);
        }
        let _ = auth.set_focus();
    }
}

async fn show_settings_window_from_tray<R: Runtime>(
    app: &AppHandle<R>,
    scroll_to_section: Option<&str>,
) {
    if let Some(state) = app.try_state::<crate::presentation::state::AppState>() {
        if !*state.is_authenticated.read().await {
            log::info!("Tray settings: user is not authenticated, opening auth window");
            show_auth_window_from_tray(app).await;
            return;
        }

        if let Ok(saved_app) = ConfigStore::load_app_config().await {
            *state.config.write().await = saved_app.clone();
            state
                .transcription_service
                .set_microphone_sensitivity(saved_app.microphone_sensitivity)
                .await;
        }

        if let Ok(mut saved_stt) = ConfigStore::load_config().await {
            let _guard = state.stt_config_guard.lock().await;
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

    if let Some(main) = app.get_webview_window("main") {
        let _ = main.set_always_on_top(false);
        let _ = main.hide();
    }
    if let Some(auth) = app.get_webview_window("auth") {
        let _ = auth.hide();
    }
    if let Some(profile) = app.get_webview_window("profile") {
        let _ = profile.hide();
    }

    if let Some(settings) = app.get_webview_window("settings") {
        if let Err(e) = show_webview_window_on_active_monitor(&settings) {
            log::error!("Failed to show settings window from tray: {}", e);
            return;
        }
        let _ = settings.set_focus();
        let _ = settings.emit(
            EVENT_SETTINGS_WINDOW_OPENED,
            serde_json::json!({
                "scrollToSection": scroll_to_section
            }),
        );
        if scroll_to_section == Some("updates") {
            let _ = app.emit(EVENT_SETTINGS_FOCUS_UPDATES, ());
        } else {
            crate::infrastructure::updater::request_interactive_update_check(
                app.clone(),
                "tray_show_settings",
            );
        }
    }
}

async fn open_updates_from_tray<R: Runtime>(app: AppHandle<R>) {
    show_settings_window_from_tray(&app, Some("updates")).await;
    crate::infrastructure::updater::run_manual_update_check_and_emit(app.clone(), "tray_manual")
        .await;
    let _ = app.emit(EVENT_SETTINGS_FOCUS_UPDATES, ());
}

/// Создает и настраивает system tray иконку с меню
pub fn create_tray<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    // Создаем элементы меню
    let show_item = MenuItem::with_id(app, "show", "Открыть", true, None::<&str>)?;
    let settings_item = MenuItem::with_id(app, "settings", "Настройки", true, None::<&str>)?;
    let profile_item = MenuItem::with_id(app, "profile", "Профиль", true, None::<&str>)?;
    let check_updates_item = MenuItem::with_id(
        app,
        "check_updates",
        "Проверить обновления",
        true,
        None::<&str>,
    )?;
    let separator = tauri::menu::PredefinedMenuItem::separator(app)?;
    let quit_item = MenuItem::with_id(app, "quit", "Выход", true, None::<&str>)?;

    // Собираем меню
    let menu = Menu::with_items(
        app,
        &[
            &show_item,
            &settings_item,
            &profile_item,
            &check_updates_item,
            &separator,
            &quit_item,
        ],
    )?;

    // Создаем tray иконку
    let mut tray_builder = TrayIconBuilder::new().menu(&menu);
    if let Some(icon) = app.default_window_icon().cloned() {
        tray_builder = tray_builder.icon(icon);
    } else {
        log::warn!("Default window icon is missing; creating tray without an explicit icon");
    }

    let _tray = tray_builder
        .tooltip("VoicetextAI")
        .on_menu_event(move |app, event| {
            // Обрабатываем клики по меню
            match event.id.as_ref() {
                "show" => {
                    let app_clone = app.clone();
                    tauri::async_runtime::spawn(async move {
                        show_main_window_from_tray(&app_clone).await;
                    });
                }
                "settings" => {
                    log::info!("Opening settings window from tray");
                    let app_clone = app.clone();
                    tauri::async_runtime::spawn(async move {
                        show_settings_window_from_tray(&app_clone, None).await;
                    });
                }
                "profile" => {
                    log::info!("Opening profile window from tray");
                    let app_clone = app.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Some(state) = app_clone.try_state::<crate::presentation::state::AppState>() {
                            let is_authenticated = *state.is_authenticated.read().await;
                            if !is_authenticated {
                                if let Some(auth) = app_clone.get_webview_window("auth") {
                                    let _ = crate::presentation::commands::show_webview_window_on_active_monitor(&auth);
                                    let _ = auth.set_focus();
                                }
                                return;
                            }
                        }
                        if let Some(profile) = app_clone.get_webview_window("profile") {
                            if let Some(main) = app_clone.get_webview_window("main") {
                                let _ = main.set_always_on_top(false);
                                let _ = main.hide();
                            }
                            if let Some(settings) = app_clone.get_webview_window("settings") {
                                let _ = settings.hide();
                            }
                            let _ = crate::presentation::commands::show_webview_window_on_active_monitor(&profile);
                            let _ = profile.set_focus();
                            let _ = profile.emit("profile-window-opened", serde_json::json!({
                                "initialSection": "none"
                            }));
                        }
                    });
                }
                "check_updates" => {
                    log::info!("Manual update check requested from tray menu");
                    let app_clone = app.clone();
                    tauri::async_runtime::spawn(async move {
                        open_updates_from_tray(app_clone).await;
                    });
                }
                "quit" => {
                    log::info!("Quitting application from tray menu");
                    app.exit(0);
                }
                _ => {}
            }
        })
        .on_tray_icon_event(|tray, event| {
            // Обрабатываем клик по самой иконке (не меню)
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle();
                if let Some(window) = app.get_webview_window("main") {
                    // При клике левой кнопкой - показываем/скрываем окно
                    match window.is_visible() {
                        Ok(true) => {
                            let _ = window.hide();
                        }
                        Ok(false) => {
                            let app_clone = app.clone();
                            tauri::async_runtime::spawn(async move {
                                show_main_window_from_tray(&app_clone).await;
                            });
                        }
                        Err(e) => {
                            log::error!("Failed to check window visibility: {}", e);
                        }
                    }
                }
            }
        })
        .build(app)?;

    log::info!("System tray created successfully");
    Ok(())
}
