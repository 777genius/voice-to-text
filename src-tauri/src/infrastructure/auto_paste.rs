// Подавляем warnings от старой версии objc crate
#![allow(unexpected_cfgs)]

use anyhow::{Context, Result};
use arboard::Clipboard;
#[cfg(not(target_os = "macos"))]
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use std::{thread, time::Duration};

const PASTE_SETTLE_MS: u64 = 300;
#[cfg(target_os = "macos")]
const MACOS_PASTE_KEY_SETTLE_MS: u64 = 20;

/// Проверяет, есть ли у приложения разрешение Accessibility на macOS
/// На других платформах всегда возвращает true (разрешение не требуется)
#[cfg(target_os = "macos")]
pub fn check_accessibility_permission() -> bool {
    // Используем правильный C API из ApplicationServices framework
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
    }

    unsafe {
        let trusted = AXIsProcessTrusted();

        if !trusted {
            log::warn!("❌ Accessibility permission NOT granted - auto-paste will not work");
        } else {
            log::info!("✅ Accessibility permission granted - auto-paste is available");
        }

        trusted
    }
}

#[cfg(not(target_os = "macos"))]
pub fn check_accessibility_permission() -> bool {
    // На Windows/Linux разрешение Accessibility не требуется
    true
}

/// Открывает системные настройки macOS в разделе Privacy & Security > Accessibility
/// На других платформах ничего не делает
#[cfg(target_os = "macos")]
pub fn open_accessibility_settings() -> Result<()> {
    use std::process::Command;

    log::info!("Opening macOS Accessibility settings");

    // Открываем System Settings > Privacy & Security > Accessibility
    // URL схема для прямого перехода к настройкам Accessibility
    let status = Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        .status()
        .context("Failed to open System Settings")?;

    if !status.success() {
        anyhow::bail!("Failed to open Accessibility settings");
    }

    log::info!("Accessibility settings opened successfully");
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn open_accessibility_settings() -> Result<()> {
    // На Windows/Linux настройки Accessibility не существуют
    log::warn!("open_accessibility_settings called on non-macOS platform");
    Ok(())
}

/// Получает bundle ID активного приложения (для macOS)
/// Возвращает bundle ID текущего активного приложения или None если не удалось получить
#[cfg(target_os = "macos")]
pub fn get_active_app_bundle_id() -> Option<String> {
    use cocoa::base::{id, nil};
    use objc::{class, msg_send, sel, sel_impl};

    unsafe {
        let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
        let active_app: id = msg_send![workspace, frontmostApplication];

        if active_app == nil {
            log::warn!("Failed to get frontmost application");
            return None;
        }

        let bundle_id: id = msg_send![active_app, bundleIdentifier];

        if bundle_id == nil {
            log::warn!("Failed to get bundle identifier");
            return None;
        }

        let bundle_id_str: *const i8 = msg_send![bundle_id, UTF8String];
        let bundle_id_string = std::ffi::CStr::from_ptr(bundle_id_str)
            .to_string_lossy()
            .to_string();

        log::debug!("Active app bundle ID: {}", bundle_id_string);
        Some(bundle_id_string)
    }
}

#[cfg(not(target_os = "macos"))]
pub fn get_active_app_bundle_id() -> Option<String> {
    // На других платформах не поддерживается
    None
}

/// Активирует приложение по bundle ID (для macOS)
/// Переключает фокус на указанное приложение
#[cfg(target_os = "macos")]
pub fn activate_app_by_bundle_id(bundle_id: &str) -> Result<()> {
    use std::process::Command;

    log::info!("Activating app with bundle ID: {}", bundle_id);

    if bundle_id.trim().is_empty() {
        anyhow::bail!("Bundle ID is empty");
    }

    let output = Command::new("/usr/bin/open")
        .arg("-b")
        .arg(bundle_id)
        .output()
        .context("Failed to run /usr/bin/open")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let detail = if stderr.is_empty() {
            format!("exit status {}", output.status)
        } else {
            stderr
        };
        anyhow::bail!(
            "Failed to activate application with bundle ID '{}': {}",
            bundle_id,
            detail
        );
    }

    log::info!("App activated successfully: {}", bundle_id);
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn activate_app_by_bundle_id(_bundle_id: &str) -> Result<()> {
    // На других платформах не поддерживается
    log::warn!("activate_app_by_bundle_id called on non-macOS platform");
    Ok(())
}

/// Вставляет текст в активное окно через системный clipboard и paste shortcut
///
/// Логика:
/// Кладёт текст в clipboard, нажимает Cmd/Ctrl+V, затем best-effort восстанавливает clipboard
///
/// Требует разрешения Accessibility на macOS
pub fn paste_text(text: &str) -> Result<()> {
    log::info!(
        "🔧 paste_text called with {} chars: '{}'",
        text.len(),
        if text.len() > 50 {
            format!("{}...", text.chars().take(50).collect::<String>())
        } else {
            text.to_string()
        }
    );

    // Проверяем разрешение Accessibility на macOS
    #[cfg(target_os = "macos")]
    {
        let has_permission = check_accessibility_permission();
        log::info!(
            "🔐 Accessibility permission check result: {}",
            has_permission
        );

        if !has_permission {
            let error_msg = "Accessibility permission not granted. Please enable it in System Settings > Privacy & Security > Accessibility";
            log::error!("❌ {}", error_msg);
            anyhow::bail!(error_msg);
        }
    }

    log::info!("📋 Preparing clipboard paste...");
    let mut clipboard = Clipboard::new().context("Failed to initialize clipboard")?;
    let previous_clipboard_text = clipboard.get_text().ok();
    clipboard
        .set_text(text.to_string())
        .context("Failed to write text to clipboard")?;

    log::info!(
        "⌨️ Pasting text at cursor position ({} chars): '{}'...",
        text.len(),
        if text.len() > 30 {
            format!("{}...", text.chars().take(30).collect::<String>())
        } else {
            text.to_string()
        }
    );

    log::debug!("   Starting paste shortcut...");
    send_paste_shortcut()?;
    thread::sleep(Duration::from_millis(PASTE_SETTLE_MS));
    log::debug!("   ✓ Paste shortcut completed");

    if let Some(previous_text) = previous_clipboard_text {
        if let Err(err) = clipboard.set_text(previous_text) {
            log::warn!("Failed to restore previous clipboard text: {}", err);
        }
    }

    log::info!("✅ Text pasted successfully at cursor position!");
    Ok(())
}

#[cfg(target_os = "macos")]
fn send_paste_shortcut() -> Result<()> {
    use std::{ffi::c_void, ptr};

    type CGEventRef = *mut c_void;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn CGEventCreateKeyboardEvent(
            source: *const c_void,
            virtual_key: u16,
            key_down: bool,
        ) -> CGEventRef;
        fn CGEventSetFlags(event: CGEventRef, flags: u64);
        fn CGEventPost(tap: u32, event: CGEventRef);
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(cf: *const c_void);
    }

    const KCG_HID_EVENT_TAP: u32 = 0;
    const KCG_EVENT_FLAG_MASK_COMMAND: u64 = 0x0010_0000;
    const KEY_CODE_V: u16 = 0x09;

    unsafe {
        let key_down = CGEventCreateKeyboardEvent(ptr::null(), KEY_CODE_V, true);
        let key_up = CGEventCreateKeyboardEvent(ptr::null(), KEY_CODE_V, false);

        if key_down.is_null() || key_up.is_null() {
            if !key_down.is_null() {
                CFRelease(key_down.cast_const());
            }
            if !key_up.is_null() {
                CFRelease(key_up.cast_const());
            }
            anyhow::bail!("Failed to create macOS paste keyboard event");
        }

        CGEventSetFlags(key_down, KCG_EVENT_FLAG_MASK_COMMAND);
        CGEventSetFlags(key_up, KCG_EVENT_FLAG_MASK_COMMAND);
        CGEventPost(KCG_HID_EVENT_TAP, key_down);
        thread::sleep(Duration::from_millis(MACOS_PASTE_KEY_SETTLE_MS));
        CGEventPost(KCG_HID_EVENT_TAP, key_up);

        CFRelease(key_down.cast_const());
        CFRelease(key_up.cast_const());
    }

    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn send_paste_shortcut() -> Result<()> {
    log::info!("⌨️ Initializing Enigo keyboard controller...");
    let mut enigo = Enigo::new(&Settings::default())
        .context("Failed to initialize Enigo keyboard controller")?;
    log::info!("✅ Enigo initialized successfully");

    paste_shortcut(&mut enigo)
}

#[cfg(not(target_os = "macos"))]
fn paste_shortcut(enigo: &mut Enigo) -> Result<()> {
    let modifier = Key::Control;
    enigo
        .key(modifier, Direction::Press)
        .context("Failed to press paste shortcut modifier")?;
    let paste_result = enigo.key(Key::Unicode('v'), Direction::Click);
    let release_result = enigo.key(modifier, Direction::Release);

    paste_result.context("Failed to press paste shortcut key")?;
    release_result.context("Failed to release paste shortcut modifier")?;
    Ok(())
}
