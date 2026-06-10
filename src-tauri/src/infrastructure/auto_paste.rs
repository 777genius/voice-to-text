// Подавляем warnings от старой версии objc crate
#![allow(unexpected_cfgs)]

use anyhow::{Context, Result};
use arboard::Clipboard;
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use std::thread;
use std::time::Duration;

pub const VOICETEXT_BUNDLE_ID: &str = "com.voicetotext.app";
pub const AUTO_PASTE_CLIPBOARD_THRESHOLD_CHARS: usize = 100;

const AUTO_PASTE_PRE_PASTE_DELAY_MS: u64 = 80;
const AUTO_PASTE_RESTORE_CLIPBOARD_DELAY_MS: u64 = 300;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutoPasteMethod {
    Typed,
    Clipboard,
}

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

trait ClipboardAccess {
    fn get_text(&mut self) -> Result<String>;
    fn set_text(&mut self, text: &str) -> Result<()>;
}

trait TextInjector {
    fn type_text(&mut self, text: &str) -> Result<()>;
    fn paste_shortcut(&mut self) -> Result<()>;
}

trait DelayProvider {
    fn sleep(&mut self, duration: Duration);
}

struct SystemClipboard {
    inner: Clipboard,
}

impl SystemClipboard {
    fn new() -> Result<Self> {
        Ok(Self {
            inner: Clipboard::new().context("Failed to initialize clipboard")?,
        })
    }
}

impl ClipboardAccess for SystemClipboard {
    fn get_text(&mut self) -> Result<String> {
        self.inner
            .get_text()
            .context("Failed to read current clipboard text")
    }

    fn set_text(&mut self, text: &str) -> Result<()> {
        self.inner
            .set_text(text.to_string())
            .context("Failed to write clipboard text")
    }
}

struct SystemTextInjector;

impl TextInjector for SystemTextInjector {
    fn type_text(&mut self, text: &str) -> Result<()> {
        paste_text(text)
    }

    fn paste_shortcut(&mut self) -> Result<()> {
        send_paste_shortcut()
    }
}

struct ThreadDelay;

impl DelayProvider for ThreadDelay {
    fn sleep(&mut self, duration: Duration) {
        thread::sleep(duration);
    }
}

fn pre_paste_delay() -> Duration {
    Duration::from_millis(AUTO_PASTE_PRE_PASTE_DELAY_MS)
}

fn restore_clipboard_delay() -> Duration {
    Duration::from_millis(AUTO_PASTE_RESTORE_CLIPBOARD_DELAY_MS)
}

fn paste_modifier_key() -> Key {
    #[cfg(target_os = "macos")]
    {
        Key::Meta
    }

    #[cfg(not(target_os = "macos"))]
    {
        Key::Control
    }
}

fn send_paste_shortcut() -> Result<()> {
    log::info!("Initializing Enigo keyboard controller for paste shortcut");
    let mut enigo = Enigo::new(&Settings::default())
        .context("Failed to initialize Enigo keyboard controller")?;
    let modifier = paste_modifier_key();

    enigo
        .key(modifier, Direction::Press)
        .context("Failed to press paste modifier key")?;
    let paste_result = send_paste_key(&mut enigo);
    let release_result = enigo
        .key(modifier, Direction::Release)
        .context("Failed to release paste modifier key");

    paste_result?;
    release_result?;
    Ok(())
}

fn send_paste_key(enigo: &mut Enigo) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        const MACOS_ANSI_V_KEY_CODE: u16 = 9;
        enigo
            .raw(MACOS_ANSI_V_KEY_CODE, Direction::Click)
            .context("Failed to send paste key")
    }

    #[cfg(target_os = "windows")]
    {
        enigo
            .key(Key::V, Direction::Click)
            .context("Failed to send paste key")
    }

    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        enigo
            .key(Key::Unicode('v'), Direction::Click)
            .context("Failed to send paste key")
    }
}

pub fn should_use_clipboard_backend(text: &str) -> bool {
    text.chars().count() >= AUTO_PASTE_CLIPBOARD_THRESHOLD_CHARS
}

pub fn paste_text_hybrid(text: &str) -> Result<AutoPasteMethod> {
    let mut injector = SystemTextInjector;
    let mut delay = ThreadDelay;

    if !should_use_clipboard_backend(text) {
        injector.type_text(text)?;
        return Ok(AutoPasteMethod::Typed);
    }

    let mut clipboard = match SystemClipboard::new() {
        Ok(clipboard) => clipboard,
        Err(error) => {
            log::warn!(
                "Clipboard initialization failed; falling back to keyboard typing: {}",
                error
            );
            injector.type_text(text)?;
            return Ok(AutoPasteMethod::Typed);
        }
    };

    paste_text_hybrid_with(text, &mut clipboard, &mut injector, &mut delay)
}

fn paste_text_hybrid_with<C, I, D>(
    text: &str,
    clipboard: &mut C,
    injector: &mut I,
    delay: &mut D,
) -> Result<AutoPasteMethod>
where
    C: ClipboardAccess,
    I: TextInjector,
    D: DelayProvider,
{
    if !should_use_clipboard_backend(text) {
        injector.type_text(text)?;
        return Ok(AutoPasteMethod::Typed);
    }

    match paste_text_via_clipboard(text, clipboard, injector, delay) {
        Ok(()) => Ok(AutoPasteMethod::Clipboard),
        Err(error) => {
            log::warn!(
                "Clipboard auto-paste failed; falling back to keyboard typing: {}",
                error
            );
            injector.type_text(text)?;
            Ok(AutoPasteMethod::Typed)
        }
    }
}

fn paste_text_via_clipboard<C, I, D>(
    text: &str,
    clipboard: &mut C,
    injector: &mut I,
    delay: &mut D,
) -> Result<()>
where
    C: ClipboardAccess,
    I: TextInjector,
    D: DelayProvider,
{
    let previous_text = clipboard
        .get_text()
        .context("Clipboard does not currently contain readable text")?;

    clipboard
        .set_text(text)
        .context("Failed to put auto-paste text into clipboard")?;
    delay.sleep(pre_paste_delay());

    if let Err(error) = injector
        .paste_shortcut()
        .context("Failed to send paste shortcut")
    {
        if let Err(restore_error) = clipboard
            .set_text(&previous_text)
            .context("Failed to restore previous clipboard text after paste shortcut failure")
        {
            log::warn!("{}", restore_error);
        }
        return Err(error);
    }

    delay.sleep(restore_clipboard_delay());
    restore_clipboard_if_unchanged(text, &previous_text, clipboard);
    Ok(())
}

fn restore_clipboard_if_unchanged<C>(text: &str, previous_text: &str, clipboard: &mut C)
where
    C: ClipboardAccess,
{
    match clipboard.get_text() {
        Ok(current_text) if current_text == text => {
            if previous_text != text {
                if let Err(error) = clipboard
                    .set_text(previous_text)
                    .context("Failed to restore previous clipboard text after auto-paste")
                {
                    log::warn!("{}", error);
                }
            }
        }
        Ok(_) => {
            log::warn!("Clipboard changed before restore; keeping current clipboard contents");
        }
        Err(error) => {
            log::warn!("Failed to verify clipboard before restore: {}", error);
        }
    }
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
    use super::{
        normalize_auto_paste_target, paste_text_hybrid_with, should_use_clipboard_backend,
        target_matches_bundle_and_pid, AutoPasteMethod, ClipboardAccess, DelayProvider,
        TextInjector, AUTO_PASTE_CLIPBOARD_THRESHOLD_CHARS, VOICETEXT_BUNDLE_ID,
    };
    use anyhow::{bail, Result};
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    #[derive(Default)]
    struct FakeClipboard {
        text: String,
        get_calls: usize,
        fail_get_on: Vec<usize>,
        change_on_get: Option<(usize, String)>,
        events: Rc<RefCell<Vec<String>>>,
    }

    impl FakeClipboard {
        fn new(text: &str, events: Rc<RefCell<Vec<String>>>) -> Self {
            Self {
                text: text.to_string(),
                events,
                ..Default::default()
            }
        }
    }

    impl ClipboardAccess for FakeClipboard {
        fn get_text(&mut self) -> Result<String> {
            self.get_calls += 1;
            self.events
                .borrow_mut()
                .push(format!("get:{}", self.get_calls));

            if self.fail_get_on.contains(&self.get_calls) {
                bail!("clipboard get failed");
            }

            if let Some((call, text)) = &self.change_on_get {
                if *call == self.get_calls {
                    self.text = text.clone();
                }
            }

            Ok(self.text.clone())
        }

        fn set_text(&mut self, text: &str) -> Result<()> {
            self.events.borrow_mut().push(format!("set:{}", text));
            self.text = text.to_string();
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeTextInjector {
        typed_texts: Vec<String>,
        paste_shortcut_calls: usize,
        fail_paste_shortcut: bool,
        events: Rc<RefCell<Vec<String>>>,
    }

    impl TextInjector for FakeTextInjector {
        fn type_text(&mut self, text: &str) -> Result<()> {
            self.events
                .borrow_mut()
                .push(format!("type:{}", text.len()));
            self.typed_texts.push(text.to_string());
            Ok(())
        }

        fn paste_shortcut(&mut self) -> Result<()> {
            self.events.borrow_mut().push("paste".to_string());
            self.paste_shortcut_calls += 1;
            if self.fail_paste_shortcut {
                bail!("paste shortcut failed");
            }
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeDelay {
        sleeps: Vec<Duration>,
        events: Rc<RefCell<Vec<String>>>,
    }

    impl DelayProvider for FakeDelay {
        fn sleep(&mut self, duration: Duration) {
            self.events
                .borrow_mut()
                .push(format!("sleep:{}", duration.as_millis()));
            self.sleeps.push(duration);
        }
    }

    fn long_text() -> String {
        "a".repeat(AUTO_PASTE_CLIPBOARD_THRESHOLD_CHARS)
    }

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

    #[test]
    fn clipboard_backend_starts_at_threshold() {
        let below_threshold = "a".repeat(AUTO_PASTE_CLIPBOARD_THRESHOLD_CHARS - 1);
        let at_threshold = long_text();

        assert!(!should_use_clipboard_backend(&below_threshold));
        assert!(should_use_clipboard_backend(&at_threshold));
    }

    #[test]
    fn hybrid_uses_typing_for_short_text() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut clipboard = FakeClipboard::new("previous", events.clone());
        let mut injector = FakeTextInjector {
            events: events.clone(),
            ..Default::default()
        };
        let mut delay = FakeDelay {
            events: events.clone(),
            ..Default::default()
        };

        let method =
            paste_text_hybrid_with("short text", &mut clipboard, &mut injector, &mut delay)
                .unwrap();

        assert_eq!(method, AutoPasteMethod::Typed);
        assert_eq!(injector.typed_texts, vec!["short text"]);
        assert_eq!(injector.paste_shortcut_calls, 0);
        assert!(delay.sleeps.is_empty());
        assert_eq!(events.borrow().as_slice(), ["type:10"]);
    }

    #[test]
    fn hybrid_uses_clipboard_for_long_text_and_restores_previous_text() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let text = long_text();
        let mut clipboard = FakeClipboard::new("previous", events.clone());
        let mut injector = FakeTextInjector {
            events: events.clone(),
            ..Default::default()
        };
        let mut delay = FakeDelay {
            events: events.clone(),
            ..Default::default()
        };
        let expected_events = vec![
            "get:1".to_string(),
            format!("set:{}", text),
            "sleep:80".to_string(),
            "paste".to_string(),
            "sleep:300".to_string(),
            "get:2".to_string(),
            "set:previous".to_string(),
        ];

        let method =
            paste_text_hybrid_with(&text, &mut clipboard, &mut injector, &mut delay).unwrap();

        assert_eq!(method, AutoPasteMethod::Clipboard);
        assert_eq!(clipboard.text, "previous");
        assert!(injector.typed_texts.is_empty());
        assert_eq!(injector.paste_shortcut_calls, 1);
        assert_eq!(*events.borrow(), expected_events);
    }

    #[test]
    fn hybrid_skips_restore_when_clipboard_changed_after_paste() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let text = long_text();
        let mut clipboard = FakeClipboard::new("previous", events.clone());
        clipboard.change_on_get = Some((2, "user copy".to_string()));
        let mut injector = FakeTextInjector {
            events: events.clone(),
            ..Default::default()
        };
        let mut delay = FakeDelay {
            events: events.clone(),
            ..Default::default()
        };

        let method =
            paste_text_hybrid_with(&text, &mut clipboard, &mut injector, &mut delay).unwrap();

        assert_eq!(method, AutoPasteMethod::Clipboard);
        assert_eq!(clipboard.text, "user copy");
        assert_eq!(injector.paste_shortcut_calls, 1);
        assert!(!events.borrow().iter().any(|event| event == "set:previous"));
    }

    #[test]
    fn hybrid_falls_back_to_typing_when_clipboard_text_is_unavailable() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let text = long_text();
        let mut clipboard = FakeClipboard::new("previous", events.clone());
        clipboard.fail_get_on = vec![1];
        let mut injector = FakeTextInjector {
            events: events.clone(),
            ..Default::default()
        };
        let mut delay = FakeDelay {
            events: events.clone(),
            ..Default::default()
        };

        let method =
            paste_text_hybrid_with(&text, &mut clipboard, &mut injector, &mut delay).unwrap();

        assert_eq!(method, AutoPasteMethod::Typed);
        assert_eq!(clipboard.text, "previous");
        assert_eq!(injector.typed_texts, vec![text]);
        assert_eq!(injector.paste_shortcut_calls, 0);
    }

    #[test]
    fn hybrid_restores_clipboard_before_typing_when_paste_shortcut_fails() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let text = long_text();
        let mut clipboard = FakeClipboard::new("previous", events.clone());
        let mut injector = FakeTextInjector {
            fail_paste_shortcut: true,
            events: events.clone(),
            ..Default::default()
        };
        let mut delay = FakeDelay {
            events: events.clone(),
            ..Default::default()
        };
        let expected_events = vec![
            "get:1".to_string(),
            format!("set:{}", text),
            "sleep:80".to_string(),
            "paste".to_string(),
            "set:previous".to_string(),
            "type:100".to_string(),
        ];

        let method =
            paste_text_hybrid_with(&text, &mut clipboard, &mut injector, &mut delay).unwrap();

        assert_eq!(method, AutoPasteMethod::Typed);
        assert_eq!(clipboard.text, "previous");
        assert_eq!(injector.typed_texts, vec![text]);
        assert_eq!(injector.paste_shortcut_calls, 1);
        assert_eq!(*events.borrow(), expected_events);
    }
}
