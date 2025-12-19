use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, Runtime,
};

use crate::presentation::commands::show_webview_window_on_active_monitor;

/// Создает и настраивает system tray иконку с меню
pub fn create_tray<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    // Создаем элементы меню
    let show_item = MenuItem::with_id(app, "show", "Открыть", true, None::<&str>)?;
    let settings_item = MenuItem::with_id(app, "settings", "Настройки", true, None::<&str>)?;
    let check_updates_item =
        MenuItem::with_id(app, "check_updates", "Проверить обновления", true, None::<&str>)?;
    let separator = tauri::menu::PredefinedMenuItem::separator(app)?;
    let quit_item = MenuItem::with_id(app, "quit", "Выход", true, None::<&str>)?;

    // Собираем меню
    let menu = Menu::with_items(
        app,
        &[
            &show_item,
            &settings_item,
            &check_updates_item,
            &separator,
            &quit_item,
        ],
    )?;

    // Создаем tray иконку
    let _tray = TrayIconBuilder::new()
        .menu(&menu)
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("Voice to Text")
        .on_menu_event(move |app, event| {
            // Обрабатываем клики по меню
            match event.id.as_ref() {
                "show" => {
                    if let Some(window) = app.get_webview_window("main") {
                        if let Err(e) = show_webview_window_on_active_monitor(&window) {
                            log::error!("Failed to show window: {}", e);
                        }
                        if let Err(e) = window.set_focus() {
                            log::error!("Failed to focus window: {}", e);
                        }
                    }
                }
                "settings" => {
                    // Открываем окно и переключаемся на вкладку настроек
                    if let Some(window) = app.get_webview_window("main") {
                        if let Err(e) = show_webview_window_on_active_monitor(&window) {
                            log::error!("Failed to show window: {}", e);
                        }
                        // Эмитируем событие для переключения на настройки
                        if let Err(e) = app.emit("tray:open-settings", ()) {
                            log::error!("Failed to emit settings event: {}", e);
                        }
                    }
                }
                "check_updates" => {
                    log::info!("Manual update check requested from tray menu");
                    // Эмитируем событие для проверки обновлений
                    if let Err(e) = app.emit("tray:check-updates", ()) {
                        log::error!("Failed to emit check updates event: {}", e);
                    }
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
                            let _ = show_webview_window_on_active_monitor(&window);
                            let _ = window.set_focus();
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
