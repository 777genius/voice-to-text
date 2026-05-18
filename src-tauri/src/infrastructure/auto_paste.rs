// Подавляем warnings от старой версии objc crate
#![allow(unexpected_cfgs)]

use anyhow::{Context, Result};
use enigo::{Enigo, Keyboard, Settings};

pub const VOICETEXT_BUNDLE_ID: &str = "com.voicetotext.app";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutoPasteTarget {
    pub bundle_id: String,
    pub pid: i32,
}

pub fn normalize_auto_paste_target(bundle_id: String, pid: i32) -> Option<AutoPasteTarget> {
    let bundle_id = bundle_id.trim().to_string();
    if bundle_id.is_empty() {
        return None;
    }
    if bundle_id == VOICETEXT_BUNDLE_ID {
        return None;
    }
    if pid <= 0 {
        return None;
    }

    Some(AutoPasteTarget { bundle_id, pid })
}

fn target_matches_bundle_and_pid(target: &AutoPasteTarget, bundle_id: &str, pid: i32) -> bool {
    target.pid == pid && target.bundle_id == bundle_id
}

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

#[cfg(target_os = "macos")]
fn nsstring_to_string(value: cocoa::base::id) -> Option<String> {
    use cocoa::base::nil;
    use objc::{msg_send, sel, sel_impl};

    if value == nil {
        return None;
    }

    unsafe {
        let str_ptr: *const i8 = msg_send![value, UTF8String];
        if str_ptr.is_null() {
            return None;
        }

        Some(
            std::ffi::CStr::from_ptr(str_ptr)
                .to_string_lossy()
                .to_string(),
        )
    }
}

#[cfg(target_os = "macos")]
fn running_app_bundle_id(app: cocoa::base::id) -> Option<String> {
    use objc::{msg_send, sel, sel_impl};

    unsafe {
        let bundle_id: cocoa::base::id = msg_send![app, bundleIdentifier];
        nsstring_to_string(bundle_id)
    }
}

#[cfg(target_os = "macos")]
fn running_app_pid(app: cocoa::base::id) -> i32 {
    use objc::{msg_send, sel, sel_impl};

    unsafe { msg_send![app, processIdentifier] }
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

/// Получает target активного приложения для auto-paste.
#[cfg(target_os = "macos")]
pub fn get_active_app_target() -> Option<AutoPasteTarget> {
    use cocoa::base::{id, nil};
    use objc::{class, msg_send, sel, sel_impl};

    unsafe {
        let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
        let active_app: id = msg_send![workspace, frontmostApplication];

        if active_app == nil {
            log::warn!("Failed to get frontmost application");
            return None;
        }

        let bundle_id = running_app_bundle_id(active_app)?;
        let pid = running_app_pid(active_app);
        let target = normalize_auto_paste_target(bundle_id.clone(), pid);

        if target.is_none() {
            log::warn!(
                "Frontmost app is not a valid auto-paste target: bundle_id={}, pid={}",
                bundle_id,
                pid
            );
        }

        target
    }
}

#[cfg(not(target_os = "macos"))]
pub fn get_active_app_target() -> Option<AutoPasteTarget> {
    None
}

/// Активирует уже запущенное приложение по PID, не запуская новый instance.
#[cfg(target_os = "macos")]
pub fn activate_running_app_by_target(target: &AutoPasteTarget) -> Result<()> {
    use cocoa::base::{id, nil};
    use objc::{class, msg_send, sel, sel_impl};

    const NS_APPLICATION_ACTIVATE_IGNORING_OTHER_APPS: usize = 1 << 1;

    log::info!(
        "Activating running app target: bundle_id={}, pid={}",
        target.bundle_id,
        target.pid
    );

    if normalize_auto_paste_target(target.bundle_id.clone(), target.pid).is_none() {
        anyhow::bail!(
            "Invalid auto-paste target: bundle_id='{}', pid={}",
            target.bundle_id,
            target.pid
        );
    }

    unsafe {
        let running_app: id = msg_send![
            class!(NSRunningApplication),
            runningApplicationWithProcessIdentifier: target.pid
        ];
        if running_app == nil {
            anyhow::bail!(
                "Auto-paste target process is not running: bundle_id={}, pid={}",
                target.bundle_id,
                target.pid
            );
        }

        let Some(current_bundle_id) = running_app_bundle_id(running_app) else {
            anyhow::bail!(
                "Auto-paste target has no bundle ID: expected={}, pid={}",
                target.bundle_id,
                target.pid
            );
        };
        if current_bundle_id != target.bundle_id {
            anyhow::bail!(
                "Auto-paste target bundle mismatch: expected={}, actual={}, pid={}",
                target.bundle_id,
                current_bundle_id,
                target.pid
            );
        }

        let _: bool = msg_send![running_app, unhide];
        let activated: bool = msg_send![
            running_app,
            activateWithOptions: NS_APPLICATION_ACTIVATE_IGNORING_OTHER_APPS
        ];

        if !activated {
            anyhow::bail!(
                "macOS refused to activate auto-paste target: bundle_id={}, pid={}",
                target.bundle_id,
                target.pid
            );
        }
    }

    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn activate_running_app_by_target(_target: &AutoPasteTarget) -> Result<()> {
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn frontmost_app_matches_target(target: &AutoPasteTarget) -> bool {
    let Some(frontmost) = get_active_app_target() else {
        return false;
    };

    target_matches_bundle_and_pid(target, &frontmost.bundle_id, frontmost.pid)
}

#[cfg(not(target_os = "macos"))]
pub fn frontmost_app_matches_target(_target: &AutoPasteTarget) -> bool {
    true
}

/// Вставляет текст в активное окно используя симуляцию клавиатуры
///
/// Логика:
/// Вводит текст в текущую позицию курсора (как печатает человек)
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

    log::info!("⌨️ Initializing Enigo keyboard controller...");
    let mut enigo = Enigo::new(&Settings::default())
        .context("Failed to initialize Enigo keyboard controller")?;
    log::info!("✅ Enigo initialized successfully");

    // Вводим текст в текущую позицию курсора (как человек)
    log::info!(
        "⌨️ Typing text at cursor position ({} chars): '{}'...",
        text.len(),
        if text.len() > 30 {
            format!("{}...", text.chars().take(30).collect::<String>())
        } else {
            text.to_string()
        }
    );

    log::debug!("   Starting text input...");
    enigo.text(text).context("Failed to type text")?;
    log::debug!("   ✓ Text input completed");

    log::info!("✅ Text typed successfully at cursor position!");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{normalize_auto_paste_target, target_matches_bundle_and_pid, VOICETEXT_BUNDLE_ID};

    #[test]
    fn normalize_auto_paste_target_rejects_self_bundle() {
        assert_eq!(
            normalize_auto_paste_target(VOICETEXT_BUNDLE_ID.to_string(), 123),
            None
        );
    }

    #[test]
    fn normalize_auto_paste_target_rejects_invalid_values() {
        assert_eq!(normalize_auto_paste_target("".to_string(), 123), None);
        assert_eq!(
            normalize_auto_paste_target("com.example.App".to_string(), 0),
            None
        );
        assert_eq!(
            normalize_auto_paste_target("com.example.App".to_string(), -1),
            None
        );
    }

    #[test]
    fn normalize_auto_paste_target_trims_bundle_id() {
        let target = normalize_auto_paste_target(" com.example.App ".to_string(), 123)
            .expect("target must be valid");

        assert_eq!(target.bundle_id, "com.example.App");
        assert_eq!(target.pid, 123);
    }

    #[test]
    fn target_matches_bundle_and_pid_requires_exact_match() {
        let target = normalize_auto_paste_target("com.example.App".to_string(), 123)
            .expect("target must be valid");

        assert!(target_matches_bundle_and_pid(
            &target,
            "com.example.App",
            123
        ));
        assert!(!target_matches_bundle_and_pid(
            &target,
            "com.example.Other",
            123
        ));
        assert!(!target_matches_bundle_and_pid(
            &target,
            "com.example.App",
            456
        ));
    }
}
