// Подавляем warnings от старой версии objc crate
#![allow(unexpected_cfgs)]

use anyhow::{Context, Result};
use arboard::Clipboard;
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
#[cfg(target_os = "macos")]
use std::ffi::{c_char, c_void, CString};
use std::thread;
use std::time::Duration;

pub const VOICETEXT_PROD_BUNDLE_ID: &str = "com.voicetotext.app";
pub const VOICETEXT_DEV_BUNDLE_ID: &str = "com.voicetotext.app.dev";

#[cfg(debug_assertions)]
pub const VOICETEXT_BUNDLE_ID: &str = VOICETEXT_DEV_BUNDLE_ID;
#[cfg(not(debug_assertions))]
pub const VOICETEXT_BUNDLE_ID: &str = VOICETEXT_PROD_BUNDLE_ID;

// On macOS, Electron/code editors are more reliable with a real clipboard paste
// than with CGEvent Unicode typing. The physical Cmd+V sequence below avoids
// layout-dependent Unicode "v" events, which could print "м" on Russian layouts.
pub const AUTO_PASTE_CLIPBOARD_THRESHOLD_CHARS: usize = 100;
#[cfg(target_os = "macos")]
pub const AUTO_PASTE_MACOS_CLIPBOARD_THRESHOLD_CHARS: usize = 1;

const VOICETEXT_BUNDLE_IDS: &[&str] = &[VOICETEXT_PROD_BUNDLE_ID, VOICETEXT_DEV_BUNDLE_ID];
const AUTO_PASTE_PRE_PASTE_DELAY_MS: u64 = 80;
#[cfg(any(target_os = "macos", test))]
const AUTO_PASTE_POST_PASTE_COMMIT_DELAY_MS: u64 = 250;
#[cfg(target_os = "macos")]
const AUTO_PASTE_MACOS_RESTORE_CLIPBOARD_DELAY_MS: u64 = 500;
#[cfg(not(target_os = "macos"))]
const AUTO_PASTE_RESTORE_CLIPBOARD_DELAY_MS: u64 = 2_500;
#[cfg(target_os = "macos")]
const MACOS_ANSI_V_KEY_CODE: u16 = 9;
#[cfg(target_os = "macos")]
const MACOS_LEFT_COMMAND_KEY_CODE: u16 = 55;
#[cfg(target_os = "macos")]
const MACOS_PASTE_KEY_EVENT_DELAY_MS: u64 = 20;
#[cfg(target_os = "macos")]
const MAC_CG_SESSION_EVENT_TAP: u32 = 1;
#[cfg(target_os = "macos")]
const MAC_CG_EVENT_FLAG_MASK_COMMAND: u64 = 0x0010_0000;
#[cfg(target_os = "macos")]
const MAC_CG_EVENT_SOURCE_STATE_HID_SYSTEM_STATE: u32 = 1;
#[cfg(target_os = "macos")]
const MAC_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
#[cfg(target_os = "macos")]
const MAC_AX_SUCCESS: i32 = 0;
#[cfg(target_os = "macos")]
const MACOS_AX_MENU_SEARCH_MAX_DEPTH: usize = 8;
#[cfg(target_os = "macos")]
const MACOS_AX_MENU_SEARCH_MAX_NODES: usize = 512;
#[cfg(target_os = "macos")]
const MACOS_CLIPBOARD_FIRST_BUNDLE_ID_PARTS: &[&str] = &[
    "brave",
    "chrome",
    "chromium",
    "edgemac",
    "firefox",
    "arc",
    "electron",
    "vscode",
    "cursor",
    "claude",
    "codex",
    "openai",
    "xcode",
    "terminal",
    "iterm",
    "warp",
    "ghostty",
    "kitty",
    "alacritty",
    "wezterm",
    "tabby",
];
#[cfg(target_os = "macos")]
const MACOS_SAFE_CLIPBOARD_RESTORE_BUNDLE_IDS: &[&str] = &["com.apple.TextEdit"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutoPasteMethod {
    Accessibility,
    Typed,
    Clipboard,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutoPasteTarget {
    pub bundle_id: String,
    pub pid: i32,
}

#[cfg(target_os = "macos")]
type MacCGEventRef = *mut c_void;
#[cfg(target_os = "macos")]
type MacCGEventSourceRef = *mut c_void;
#[cfg(target_os = "macos")]
type MacCFStringRef = *const c_void;
#[cfg(target_os = "macos")]
type MacCFTypeRef = *const c_void;
#[cfg(target_os = "macos")]
type MacCFArrayRef = *const c_void;
#[cfg(target_os = "macos")]
type MacCFBooleanRef = *const c_void;
#[cfg(target_os = "macos")]
type MacAXUIElementRef = *mut c_void;

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MacKeyboardEventSpec {
    key_code: u16,
    key_down: bool,
    flags: u64,
    unicode: Option<u16>,
    delay_after_ms: u64,
}

#[cfg(target_os = "macos")]
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> MacAXUIElementRef;
    fn AXUIElementCreateSystemWide() -> MacAXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        element: MacAXUIElementRef,
        attribute: MacCFStringRef,
        value: *mut MacCFTypeRef,
    ) -> i32;
    fn AXUIElementIsAttributeSettable(
        element: MacAXUIElementRef,
        attribute: MacCFStringRef,
        settable: *mut bool,
    ) -> i32;
    fn AXUIElementSetAttributeValue(
        element: MacAXUIElementRef,
        attribute: MacCFStringRef,
        value: MacCFTypeRef,
    ) -> i32;
    fn AXUIElementPerformAction(element: MacAXUIElementRef, action: MacCFStringRef) -> i32;
    fn AXUIElementGetPid(element: MacAXUIElementRef, pid: *mut i32) -> i32;
    fn CGEventCreateKeyboardEvent(
        source: MacCGEventSourceRef,
        virtual_key: u16,
        key_down: bool,
    ) -> MacCGEventRef;
    fn CGEventSourceCreate(state_id: u32) -> MacCGEventSourceRef;
    fn CGEventSetFlags(event: MacCGEventRef, flags: u64);
    fn CGEventKeyboardSetUnicodeString(
        event: MacCGEventRef,
        string_length: usize,
        unicode_string: *const u16,
    );
    fn CGEventPost(tap: u32, event: MacCGEventRef);
}

#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(cf: *const c_void);
    fn CFArrayGetCount(array: MacCFArrayRef) -> isize;
    fn CFArrayGetValueAtIndex(array: MacCFArrayRef, index: isize) -> *const c_void;
    fn CFStringCreateWithCString(
        allocator: *const c_void,
        c_str: *const c_char,
        encoding: u32,
    ) -> MacCFStringRef;
    fn CFStringGetCString(
        string: MacCFStringRef,
        buffer: *mut c_char,
        buffer_size: isize,
        encoding: u32,
    ) -> bool;
    fn CFBooleanGetValue(boolean: MacCFBooleanRef) -> u8;
}

pub fn normalize_auto_paste_target(bundle_id: String, pid: i32) -> Option<AutoPasteTarget> {
    let bundle_id = bundle_id.trim().to_string();
    if bundle_id.is_empty() {
        return None;
    }
    if VOICETEXT_BUNDLE_IDS.contains(&bundle_id.as_str()) {
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

#[cfg(target_os = "macos")]
struct ScopedCFString(MacCFStringRef);

#[cfg(target_os = "macos")]
impl ScopedCFString {
    fn new(value: &str) -> Result<Self> {
        let c_value = CString::new(value).context("AX attribute contains NUL byte")?;
        let cf_string = unsafe {
            CFStringCreateWithCString(
                std::ptr::null(),
                c_value.as_ptr(),
                MAC_CF_STRING_ENCODING_UTF8,
            )
        };
        if cf_string.is_null() {
            anyhow::bail!("Failed to create CFString for AX attribute");
        }
        Ok(Self(cf_string))
    }

    fn as_ptr(&self) -> MacCFStringRef {
        self.0
    }
}

#[cfg(target_os = "macos")]
impl Drop for ScopedCFString {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0 as *const c_void) };
        }
    }
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
pub struct FocusedElementDiagnostics {
    pub role: Option<String>,
    pub subrole: Option<String>,
    pub value_settable: Option<bool>,
    pub selected_text_settable: Option<bool>,
    pub selected_text_range_settable: Option<bool>,
    pub likely_text_input: bool,
}

fn focused_element_likely_accepts_text(
    role: Option<&str>,
    subrole: Option<&str>,
    value_settable: Option<bool>,
    selected_text_settable: Option<bool>,
    selected_text_range_settable: Option<bool>,
) -> bool {
    if value_settable == Some(true)
        || selected_text_settable == Some(true)
        || selected_text_range_settable == Some(true)
    {
        return true;
    }

    matches!(
        role,
        Some("AXTextArea" | "AXTextField" | "AXComboBox" | "AXSearchField")
    ) || subrole.is_some_and(|value| value.contains("Text") || value.contains("Field"))
}

#[cfg(target_os = "macos")]
fn cf_string_to_string(value: MacCFStringRef) -> Option<String> {
    if value.is_null() {
        return None;
    }

    let mut buffer = vec![0_i8; 1024];
    let ok = unsafe {
        CFStringGetCString(
            value,
            buffer.as_mut_ptr(),
            buffer.len() as isize,
            MAC_CF_STRING_ENCODING_UTF8,
        )
    };
    if !ok {
        return None;
    }

    let value = unsafe { std::ffi::CStr::from_ptr(buffer.as_ptr()) };
    Some(value.to_string_lossy().to_string())
}

#[cfg(target_os = "macos")]
fn copy_ax_string_attribute(element: MacAXUIElementRef, attribute_name: &str) -> Option<String> {
    let attribute = ScopedCFString::new(attribute_name).ok()?;
    let mut value: MacCFTypeRef = std::ptr::null();
    let error = unsafe { AXUIElementCopyAttributeValue(element, attribute.as_ptr(), &mut value) };
    if error != MAC_AX_SUCCESS || value.is_null() {
        return None;
    }

    let result = cf_string_to_string(value as MacCFStringRef);
    unsafe { CFRelease(value as *const c_void) };
    result
}

#[cfg(target_os = "macos")]
fn copy_ax_attribute(element: MacAXUIElementRef, attribute_name: &str) -> Option<MacCFTypeRef> {
    let attribute = ScopedCFString::new(attribute_name).ok()?;
    let mut value: MacCFTypeRef = std::ptr::null();
    let error = unsafe { AXUIElementCopyAttributeValue(element, attribute.as_ptr(), &mut value) };
    if error == MAC_AX_SUCCESS && !value.is_null() {
        Some(value)
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn copy_ax_bool_attribute(element: MacAXUIElementRef, attribute_name: &str) -> Option<bool> {
    let value = copy_ax_attribute(element, attribute_name)?;
    let result = unsafe { CFBooleanGetValue(value as MacCFBooleanRef) != 0 };
    unsafe { CFRelease(value as *const c_void) };
    Some(result)
}

#[cfg(target_os = "macos")]
fn ax_attribute_settable(element: MacAXUIElementRef, attribute_name: &str) -> Option<bool> {
    let attribute = ScopedCFString::new(attribute_name).ok()?;
    let mut settable = false;
    let error =
        unsafe { AXUIElementIsAttributeSettable(element, attribute.as_ptr(), &mut settable) };
    if error == MAC_AX_SUCCESS {
        Some(settable)
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn focused_element_pid(element: MacAXUIElementRef) -> Option<i32> {
    let mut pid = 0;
    let error = unsafe { AXUIElementGetPid(element, &mut pid) };
    if error == MAC_AX_SUCCESS && pid > 0 {
        Some(pid)
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn focused_element_pid_matches_target(target: &AutoPasteTarget, focused_pid: Option<i32>) -> bool {
    focused_pid == Some(target.pid)
}

#[cfg(target_os = "macos")]
fn copy_application_focused_element(target: &AutoPasteTarget) -> Result<MacAXUIElementRef> {
    let app = unsafe { AXUIElementCreateApplication(target.pid) };
    if app.is_null() {
        anyhow::bail!(
            "Failed to create AX application element for pid={}",
            target.pid
        );
    }

    let focused_attribute = ScopedCFString::new("AXFocusedUIElement")?;
    let mut focused: MacCFTypeRef = std::ptr::null();
    let error =
        unsafe { AXUIElementCopyAttributeValue(app, focused_attribute.as_ptr(), &mut focused) };
    unsafe { CFRelease(app as *const c_void) };
    if error != MAC_AX_SUCCESS || focused.is_null() {
        anyhow::bail!(
            "Failed to read AXFocusedUIElement for bundle_id={}, pid={}, ax_error={}",
            target.bundle_id,
            target.pid,
            error
        );
    }

    Ok(focused as MacAXUIElementRef)
}

#[cfg(target_os = "macos")]
fn copy_system_focused_element() -> Result<MacAXUIElementRef> {
    let system = unsafe { AXUIElementCreateSystemWide() };
    if system.is_null() {
        anyhow::bail!("Failed to create system-wide AX element");
    }

    let focused_attribute = ScopedCFString::new("AXFocusedUIElement")?;
    let mut focused: MacCFTypeRef = std::ptr::null();
    let error =
        unsafe { AXUIElementCopyAttributeValue(system, focused_attribute.as_ptr(), &mut focused) };
    unsafe { CFRelease(system as *const c_void) };
    if error != MAC_AX_SUCCESS || focused.is_null() {
        anyhow::bail!(
            "Failed to read system-wide AXFocusedUIElement, ax_error={}",
            error
        );
    }

    Ok(focused as MacAXUIElementRef)
}

#[cfg(target_os = "macos")]
fn focused_element_diagnostics_from_element(
    focused_element: MacAXUIElementRef,
) -> FocusedElementDiagnostics {
    let role = copy_ax_string_attribute(focused_element, "AXRole");
    let subrole = copy_ax_string_attribute(focused_element, "AXSubrole");
    let value_settable = ax_attribute_settable(focused_element, "AXValue");
    let selected_text_settable = ax_attribute_settable(focused_element, "AXSelectedText");
    let selected_text_range_settable =
        ax_attribute_settable(focused_element, "AXSelectedTextRange");

    let likely_text_input = focused_element_likely_accepts_text(
        role.as_deref(),
        subrole.as_deref(),
        value_settable,
        selected_text_settable,
        selected_text_range_settable,
    );

    FocusedElementDiagnostics {
        role,
        subrole,
        value_settable,
        selected_text_settable,
        selected_text_range_settable,
        likely_text_input,
    }
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PasteMenuMatchMode {
    ExactTitle,
}

#[cfg(target_os = "macos")]
#[derive(Clone, Debug, PartialEq, Eq)]
struct PasteMenuItemMatch {
    title: Option<String>,
    cmd_char: Option<String>,
    enabled: Option<bool>,
    mode: PasteMenuMatchMode,
}

#[cfg(target_os = "macos")]
fn paste_menu_item_matches_values(
    role: Option<&str>,
    title: Option<&str>,
    enabled: Option<bool>,
    mode: PasteMenuMatchMode,
) -> bool {
    if role != Some("AXMenuItem") {
        return false;
    }
    if enabled == Some(false) {
        return false;
    }

    match mode {
        PasteMenuMatchMode::ExactTitle => {
            title.is_some_and(|value| matches!(value.trim(), "Paste" | "Вставить" | "Вставити"))
        }
    }
}

#[cfg(target_os = "macos")]
fn macos_paste_menu_item_match(
    element: MacAXUIElementRef,
    mode: PasteMenuMatchMode,
) -> Option<PasteMenuItemMatch> {
    let role = copy_ax_string_attribute(element, "AXRole");
    if role.as_deref() != Some("AXMenuItem") {
        return None;
    }

    let title = copy_ax_string_attribute(element, "AXTitle");
    let cmd_char = copy_ax_string_attribute(element, "AXMenuItemCmdChar");
    let enabled = copy_ax_bool_attribute(element, "AXEnabled");
    if paste_menu_item_matches_values(role.as_deref(), title.as_deref(), enabled, mode) {
        Some(PasteMenuItemMatch {
            title,
            cmd_char,
            enabled,
            mode,
        })
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn press_macos_ax_element(element: MacAXUIElementRef) -> Result<()> {
    let action = ScopedCFString::new("AXPress")?;
    let error = unsafe { AXUIElementPerformAction(element, action.as_ptr()) };
    if error == MAC_AX_SUCCESS {
        Ok(())
    } else {
        anyhow::bail!("AXPress failed with error {}", error);
    }
}

#[cfg(target_os = "macos")]
fn press_paste_menu_item_in_array(
    array: MacCFArrayRef,
    depth: usize,
    visited: &mut usize,
    mode: PasteMenuMatchMode,
) -> Result<bool> {
    let count = unsafe { CFArrayGetCount(array) };
    if count <= 0 {
        return Ok(false);
    }

    for index in 0..count {
        let child = unsafe { CFArrayGetValueAtIndex(array, index) as MacAXUIElementRef };
        if child.is_null() {
            continue;
        }

        if press_paste_menu_item_in_tree(child, depth, visited, mode)? {
            return Ok(true);
        }
    }

    Ok(false)
}

#[cfg(target_os = "macos")]
fn press_paste_menu_item_in_tree(
    element: MacAXUIElementRef,
    depth: usize,
    visited: &mut usize,
    mode: PasteMenuMatchMode,
) -> Result<bool> {
    if element.is_null()
        || depth > MACOS_AX_MENU_SEARCH_MAX_DEPTH
        || *visited >= MACOS_AX_MENU_SEARCH_MAX_NODES
    {
        return Ok(false);
    }
    *visited += 1;

    if let Some(menu_match) = macos_paste_menu_item_match(element, mode) {
        log::info!(
            "Pressing paste menu item through Accessibility: title={:?}, cmd_char={:?}, enabled={:?}, mode={:?}",
            menu_match.title,
            menu_match.cmd_char,
            menu_match.enabled,
            menu_match.mode
        );
        match press_macos_ax_element(element) {
            Ok(()) => return Ok(true),
            Err(error) => log::warn!("Matched paste menu item but could not press it: {}", error),
        }
    }

    if let Some(menu) = copy_ax_attribute(element, "AXMenu") {
        let result =
            press_paste_menu_item_in_tree(menu as MacAXUIElementRef, depth + 1, visited, mode);
        unsafe { CFRelease(menu as *const c_void) };
        if result? {
            return Ok(true);
        }
    }

    if let Some(children) = copy_ax_attribute(element, "AXChildren") {
        let result =
            press_paste_menu_item_in_array(children as MacCFArrayRef, depth + 1, visited, mode);
        unsafe { CFRelease(children as *const c_void) };
        if result? {
            return Ok(true);
        }
    }

    Ok(false)
}

#[cfg(target_os = "macos")]
fn press_macos_paste_menu_item_for_target(target: &AutoPasteTarget) -> Result<bool> {
    let app = unsafe { AXUIElementCreateApplication(target.pid) };
    if app.is_null() {
        anyhow::bail!(
            "Failed to create AX application element for pid={}",
            target.pid
        );
    }

    let Some(menu_bar) = copy_ax_attribute(app, "AXMenuBar") else {
        unsafe { CFRelease(app as *const c_void) };
        anyhow::bail!(
            "Failed to read AXMenuBar for bundle_id={}, pid={}",
            target.bundle_id,
            target.pid
        );
    };

    for mode in macos_paste_menu_search_modes() {
        let mut visited = 0;
        let pressed =
            press_paste_menu_item_in_tree(menu_bar as MacAXUIElementRef, 0, &mut visited, *mode)?;
        if pressed {
            unsafe {
                CFRelease(menu_bar as *const c_void);
                CFRelease(app as *const c_void);
            }
            log::info!(
                "Pressed paste menu item through Accessibility for bundle_id={}, pid={}",
                target.bundle_id,
                target.pid
            );
            return Ok(true);
        }
    }

    unsafe {
        CFRelease(menu_bar as *const c_void);
        CFRelease(app as *const c_void);
    }
    Ok(false)
}

#[cfg(target_os = "macos")]
fn macos_paste_menu_search_modes() -> &'static [PasteMenuMatchMode] {
    &[PasteMenuMatchMode::ExactTitle]
}

#[cfg(target_os = "macos")]
pub fn focused_element_diagnostics(target: &AutoPasteTarget) -> Result<FocusedElementDiagnostics> {
    let focused = match copy_application_focused_element(target) {
        Ok(focused) => focused,
        Err(error) => {
            log::debug!(
                "Application AXFocusedUIElement lookup failed; trying system-wide focused element: {}",
                error
            );
            let focused = copy_system_focused_element()?;
            let focused_pid = focused_element_pid(focused);
            if !focused_element_pid_matches_target(target, focused_pid) {
                unsafe { CFRelease(focused as *const c_void) };
                anyhow::bail!(
                    "System-wide focused element belongs to a different pid: expected={}, actual={:?}",
                    target.pid,
                    focused_pid
                );
            }
            focused
        }
    };

    let diagnostics = focused_element_diagnostics_from_element(focused);
    unsafe { CFRelease(focused as *const c_void) };
    Ok(diagnostics)
}

#[cfg(target_os = "macos")]
pub fn log_focused_element_diagnostics(target: &AutoPasteTarget) {
    match focused_element_diagnostics(target) {
        Ok(diagnostics) => {
            log::info!(
                "Auto-paste focused element: role={:?}, subrole={:?}, value_settable={:?}, selected_text_settable={:?}, selected_text_range_settable={:?}, likely_text_input={}",
                diagnostics.role,
                diagnostics.subrole,
                diagnostics.value_settable,
                diagnostics.selected_text_settable,
                diagnostics.selected_text_range_settable,
                diagnostics.likely_text_input
            );
            if !diagnostics.likely_text_input {
                log::warn!(
                    "Auto-paste target is frontmost, but focused AX element does not look editable; paste may be ignored"
                );
            }
        }
        Err(error) => {
            log::warn!("Failed to inspect auto-paste focused AX element: {}", error);
        }
    }
}

#[cfg(target_os = "macos")]
fn paste_text_via_focused_accessibility_element(
    text: &str,
    target: &AutoPasteTarget,
) -> Result<bool> {
    if text.is_empty() {
        return Ok(true);
    }

    let focused = copy_system_focused_element()?;
    let focused_pid = focused_element_pid(focused);
    if !focused_element_pid_matches_target(target, focused_pid) {
        unsafe { CFRelease(focused as *const c_void) };
        log::warn!(
            "Accessibility auto-paste skipped: focused pid mismatch, expected={}, actual={:?}",
            target.pid,
            focused_pid
        );
        return Ok(false);
    }

    let diagnostics = focused_element_diagnostics_from_element(focused);
    if !diagnostics.likely_text_input {
        unsafe { CFRelease(focused as *const c_void) };
        log::warn!(
            "Accessibility auto-paste skipped: focused element does not look editable, role={:?}, subrole={:?}, selected_text_settable={:?}",
            diagnostics.role,
            diagnostics.subrole,
            diagnostics.selected_text_settable
        );
        return Ok(false);
    }

    let attribute = ScopedCFString::new("AXSelectedText")?;
    let value = ScopedCFString::new(text)?;
    let error =
        unsafe { AXUIElementSetAttributeValue(focused, attribute.as_ptr(), value.as_ptr()) };
    unsafe { CFRelease(focused as *const c_void) };

    if error == MAC_AX_SUCCESS {
        log::info!(
            "Accessibility auto-paste inserted text through AXSelectedText: chars={}",
            text.chars().count()
        );
        Ok(true)
    } else {
        log::warn!(
            "Accessibility auto-paste failed; falling back to clipboard paste: ax_error={}",
            error
        );
        Ok(false)
    }
}

#[cfg(target_os = "macos")]
fn paste_text_with_macos_accessibility_attempt<A, F>(
    accessibility_insert: A,
    fallback_paste: F,
) -> Result<AutoPasteMethod>
where
    A: FnOnce() -> Result<bool>,
    F: FnOnce() -> Result<AutoPasteMethod>,
{
    match accessibility_insert() {
        Ok(true) => Ok(AutoPasteMethod::Accessibility),
        Ok(false) => fallback_paste(),
        Err(error) => {
            log::warn!(
                "Accessibility auto-paste failed before insertion; falling back to clipboard paste: {}",
                error
            );
            fallback_paste()
        }
    }
}

#[cfg(target_os = "macos")]
fn target_prefers_clipboard_paste(target: &AutoPasteTarget) -> bool {
    let bundle_id = target.bundle_id.to_ascii_lowercase();
    MACOS_CLIPBOARD_FIRST_BUNDLE_ID_PARTS
        .iter()
        .any(|part| bundle_id.contains(part))
}

#[cfg(target_os = "macos")]
fn target_can_safely_restore_clipboard(target: &AutoPasteTarget) -> bool {
    MACOS_SAFE_CLIPBOARD_RESTORE_BUNDLE_IDS
        .iter()
        .any(|bundle_id| target.bundle_id.eq_ignore_ascii_case(bundle_id))
}

#[cfg(target_os = "macos")]
fn target_has_unreliable_ax_paste_menu(target: &AutoPasteTarget) -> bool {
    target_prefers_clipboard_paste(target)
}

#[cfg(target_os = "macos")]
fn paste_text_for_target_with<A, F>(
    target: &AutoPasteTarget,
    accessibility_insert: A,
    fallback_paste: F,
) -> Result<AutoPasteMethod>
where
    A: FnOnce() -> Result<bool>,
    F: FnOnce() -> Result<AutoPasteMethod>,
{
    if target_prefers_clipboard_paste(target) {
        log::info!(
            "Auto-paste using clipboard-first path for web/editor target: bundle_id={}, pid={}",
            target.bundle_id,
            target.pid
        );
        return match fallback_paste() {
            Ok(method) => Ok(method),
            Err(clipboard_error) => {
                log::warn!(
                    "Clipboard-first auto-paste failed; trying AXSelectedText fallback before giving up: {}",
                    clipboard_error
                );
                match accessibility_insert() {
                    Ok(true) => Ok(AutoPasteMethod::Accessibility),
                    Ok(false) => anyhow::bail!(
                        "Clipboard-first auto-paste failed and AXSelectedText fallback was unavailable: {}",
                        clipboard_error
                    ),
                    Err(accessibility_error) => anyhow::bail!(
                        "Clipboard-first auto-paste failed and AXSelectedText fallback errored: {}; ax_error={}",
                        clipboard_error,
                        accessibility_error
                    ),
                }
            }
        };
    }

    paste_text_with_macos_accessibility_attempt(accessibility_insert, fallback_paste)
}

trait ClipboardAccess {
    fn get_text(&mut self) -> Result<String>;
    fn set_text(&mut self, text: &str) -> Result<()>;
}

trait TextInjector {
    fn type_text(&mut self, text: &str) -> Result<()>;
    fn paste_shortcut(&mut self) -> Result<()>;
    fn restore_clipboard_after_successful_paste(&self) -> bool {
        true
    }
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

#[cfg(target_os = "macos")]
struct MacTargetTextInjector<P>
where
    P: FnMut(&AutoPasteTarget) -> Result<()>,
{
    target: AutoPasteTarget,
    paste_command: P,
}

#[cfg(target_os = "macos")]
impl<P> TextInjector for MacTargetTextInjector<P>
where
    P: FnMut(&AutoPasteTarget) -> Result<()>,
{
    fn type_text(&mut self, text: &str) -> Result<()> {
        paste_text(text)
    }

    fn paste_shortcut(&mut self) -> Result<()> {
        (self.paste_command)(&self.target)
    }

    fn restore_clipboard_after_successful_paste(&self) -> bool {
        target_can_safely_restore_clipboard(&self.target)
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
    #[cfg(target_os = "macos")]
    {
        Duration::from_millis(AUTO_PASTE_MACOS_RESTORE_CLIPBOARD_DELAY_MS)
    }

    #[cfg(not(target_os = "macos"))]
    {
        Duration::from_millis(AUTO_PASTE_RESTORE_CLIPBOARD_DELAY_MS)
    }
}

#[cfg(target_os = "macos")]
fn post_paste_commit_delay() -> Duration {
    Duration::from_millis(AUTO_PASTE_POST_PASTE_COMMIT_DELAY_MS)
}

#[cfg(not(target_os = "macos"))]
fn paste_modifier_key() -> Key {
    Key::Control
}

fn send_paste_shortcut() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        return send_macos_paste_command();
    }

    #[cfg(not(target_os = "macos"))]
    {
        send_enigo_paste_shortcut()
    }
}

#[cfg(target_os = "macos")]
fn send_macos_paste_command() -> Result<()> {
    let Some(target) = get_active_app_target() else {
        anyhow::bail!("No valid frontmost app target for paste menu");
    };

    send_macos_paste_command_to_target(&target)
}

#[cfg(target_os = "macos")]
fn send_macos_paste_command_to_target(target: &AutoPasteTarget) -> Result<()> {
    if !frontmost_app_matches_target(target) {
        activate_running_app_by_target(target)
            .context("Failed to reactivate saved auto-paste target before paste command")?;
        thread::sleep(pre_paste_delay());
    }

    send_macos_paste_command_for_target(
        target,
        press_macos_paste_menu_item_for_target,
        send_macos_paste_shortcut,
        send_macos_system_events_paste_menu_item,
    )
}

#[cfg(target_os = "macos")]
fn send_macos_paste_command_for_target<M, K, S>(
    target: &AutoPasteTarget,
    mut press_paste_menu_item: M,
    mut send_keyboard_shortcut: K,
    mut send_system_events_menu_item: S,
) -> Result<()>
where
    M: FnMut(&AutoPasteTarget) -> Result<bool>,
    K: FnMut() -> Result<()>,
    S: FnMut(&AutoPasteTarget) -> Result<()>,
{
    if target_has_unreliable_ax_paste_menu(target) {
        log::info!(
            "Using System Events paste menu before AXPress for AX-unreliable target: bundle_id={}, pid={}",
            target.bundle_id,
            target.pid
        );

        if let Err(error) = send_system_events_menu_item(target) {
            anyhow::bail!(
                "System Events paste menu click failed for AX-unreliable target; refusing AXPress paste menu fallback to avoid false success: {}",
                error
            );
        }

        return Ok(());
    }

    match press_paste_menu_item(target) {
        Ok(true) => return Ok(()),
        Ok(false) => {
            log::warn!("Paste menu item was not found; falling back to System Events menu click");
        }
        Err(error) => {
            log::warn!(
                "Paste menu item path failed; falling back to System Events menu click: {}",
                error
            );
        }
    }

    if let Err(error) = send_system_events_menu_item(target) {
        log::warn!("System Events paste menu click failed: {}", error);
    } else {
        return Ok(());
    }

    if target_prefers_clipboard_paste(target) {
        anyhow::bail!(
            "safe paste menu paths failed for clipboard-first target; refusing physical Cmd+V fallback for bundle_id={}, pid={}",
            target.bundle_id,
            target.pid
        );
    }

    send_keyboard_shortcut()
}

#[cfg(not(target_os = "macos"))]
fn send_enigo_paste_shortcut() -> Result<()> {
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

#[cfg(target_os = "macos")]
fn macos_system_events_paste_menu_script(pid: i32) -> String {
    format!(
        r#"tell application "System Events"
  set targetProcess to first process whose unix id is {pid}
  tell targetProcess
    repeat with editMenuName in {{"Edit", "Правка", "Редагувати"}}
      try
        set editMenu to menu editMenuName of menu bar item editMenuName of menu bar 1
        repeat with pasteItemName in {{"Paste", "Вставить", "Вставити"}}
          try
            set pasteItem to menu item pasteItemName of editMenu
            if enabled of pasteItem then
              click pasteItem
              return "clicked"
            end if
          end try
        end repeat
      end try
    end repeat
  end tell
end tell
error "Paste menu item not found or disabled""#
    )
}

#[cfg(target_os = "macos")]
fn send_macos_system_events_paste_menu_item(target: &AutoPasteTarget) -> Result<()> {
    log::info!(
        "Clicking paste menu item via System Events for bundle_id={}, pid={}",
        target.bundle_id,
        target.pid
    );

    let output = std::process::Command::new("osascript")
        .arg("-e")
        .arg(macos_system_events_paste_menu_script(target.pid))
        .output()
        .context("Failed to run osascript for paste menu item")?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        anyhow::bail!(
            "osascript paste menu item failed with status {}: {}",
            output.status,
            stderr
        );
    }
}

#[cfg(target_os = "macos")]
fn send_macos_paste_shortcut() -> Result<()> {
    log::info!("Sending paste shortcut via CoreGraphics physical command sequence");

    unsafe {
        let source = CGEventSourceCreate(MAC_CG_EVENT_SOURCE_STATE_HID_SYSTEM_STATE);
        if source.is_null() {
            anyhow::bail!("Failed to create paste event source");
        }

        let mut command_down_sent = false;
        for spec in macos_paste_event_sequence() {
            if let Err(error) = post_macos_keyboard_event(source, spec) {
                if command_down_sent {
                    let release_result = post_macos_keyboard_event(
                        source,
                        MacKeyboardEventSpec {
                            key_code: MACOS_LEFT_COMMAND_KEY_CODE,
                            key_down: false,
                            flags: 0,
                            unicode: None,
                            delay_after_ms: 0,
                        },
                    );
                    if let Err(release_error) = release_result {
                        log::warn!(
                            "Failed to release synthetic command key after paste shortcut error: {}",
                            release_error
                        );
                    }
                }
                CFRelease(source as *const c_void);
                return Err(error);
            }

            if spec.key_code == MACOS_LEFT_COMMAND_KEY_CODE {
                command_down_sent = spec.key_down;
            }
        }

        CFRelease(source as *const c_void);
    }

    Ok(())
}

#[cfg(target_os = "macos")]
fn macos_paste_event_sequence() -> [MacKeyboardEventSpec; 4] {
    [
        MacKeyboardEventSpec {
            key_code: MACOS_LEFT_COMMAND_KEY_CODE,
            key_down: true,
            flags: MAC_CG_EVENT_FLAG_MASK_COMMAND,
            unicode: None,
            delay_after_ms: MACOS_PASTE_KEY_EVENT_DELAY_MS,
        },
        MacKeyboardEventSpec {
            key_code: MACOS_ANSI_V_KEY_CODE,
            key_down: true,
            flags: MAC_CG_EVENT_FLAG_MASK_COMMAND,
            unicode: None,
            delay_after_ms: MACOS_PASTE_KEY_EVENT_DELAY_MS,
        },
        MacKeyboardEventSpec {
            key_code: MACOS_ANSI_V_KEY_CODE,
            key_down: false,
            flags: MAC_CG_EVENT_FLAG_MASK_COMMAND,
            unicode: None,
            delay_after_ms: MACOS_PASTE_KEY_EVENT_DELAY_MS,
        },
        MacKeyboardEventSpec {
            key_code: MACOS_LEFT_COMMAND_KEY_CODE,
            key_down: false,
            flags: 0,
            unicode: None,
            delay_after_ms: 0,
        },
    ]
}

#[cfg(target_os = "macos")]
unsafe fn post_macos_keyboard_event(
    source: MacCGEventSourceRef,
    spec: MacKeyboardEventSpec,
) -> Result<()> {
    let event = CGEventCreateKeyboardEvent(source, spec.key_code, spec.key_down);
    if event.is_null() {
        anyhow::bail!(
            "Failed to create paste keyboard event: key_code={}, key_down={}",
            spec.key_code,
            spec.key_down
        );
    }

    CGEventSetFlags(event, spec.flags);
    if let Some(unicode) = spec.unicode {
        CGEventKeyboardSetUnicodeString(event, 1, &unicode);
    }
    CGEventPost(MAC_CG_SESSION_EVENT_TAP, event);
    CFRelease(event as *const c_void);

    if spec.delay_after_ms > 0 {
        thread::sleep(Duration::from_millis(spec.delay_after_ms));
    }

    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn send_paste_key(enigo: &mut Enigo) -> Result<()> {
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
    text.chars().count() >= clipboard_backend_threshold_chars()
}

pub fn clipboard_backend_threshold_chars() -> usize {
    #[cfg(target_os = "macos")]
    {
        AUTO_PASTE_MACOS_CLIPBOARD_THRESHOLD_CHARS
    }

    #[cfg(not(target_os = "macos"))]
    {
        AUTO_PASTE_CLIPBOARD_THRESHOLD_CHARS
    }
}

pub fn send_backspaces(count: usize) -> Result<()> {
    if count == 0 {
        return Ok(());
    }

    log::info!("Sending {} Backspace key events", count);
    let mut enigo = Enigo::new(&Settings::default())
        .context("Failed to initialize Enigo keyboard controller")?;
    for _ in 0..count {
        enigo
            .key(Key::Backspace, Direction::Click)
            .context("Failed to send Backspace key")?;
    }
    Ok(())
}

pub fn paste_text_hybrid(text: &str) -> Result<AutoPasteMethod> {
    let mut injector = SystemTextInjector;
    let mut delay = ThreadDelay;

    if !should_use_clipboard_backend(text) {
        log::info!(
            "Auto-paste using direct typing backend: chars={}, clipboard_threshold={}",
            text.chars().count(),
            clipboard_backend_threshold_chars()
        );
        injector.type_text(text)?;
        return Ok(AutoPasteMethod::Typed);
    }

    let mut clipboard = match SystemClipboard::new() {
        Ok(clipboard) => clipboard,
        Err(error) => {
            if !keyboard_typing_fallback_enabled() {
                anyhow::bail!(
                    "Clipboard initialization failed and keyboard typing fallback is disabled on macOS: {}",
                    error
                );
            }
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

#[cfg(target_os = "macos")]
pub fn paste_text_for_target(text: &str, target: &AutoPasteTarget) -> Result<AutoPasteMethod> {
    #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
    let attempt = super::continuation_context::observation::legacy_start();
    let result = paste_text_for_target_with(
        target,
        || paste_text_via_focused_accessibility_element(text, target),
        || paste_text_hybrid_for_target(text, target),
    );
    #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
    super::continuation_context::observation::legacy_finish(
        attempt,
        match &result {
            Ok(AutoPasteMethod::Accessibility) => "accessibility-api-ok",
            Ok(AutoPasteMethod::Typed) => "typing-api-ok",
            Ok(AutoPasteMethod::Clipboard) => "clipboard-api-ok",
            Err(_) => "error-insertion-unknown",
        },
    );
    result
}

#[cfg(target_os = "macos")]
fn paste_text_hybrid_for_target(text: &str, target: &AutoPasteTarget) -> Result<AutoPasteMethod> {
    let mut clipboard = match SystemClipboard::new() {
        Ok(clipboard) => clipboard,
        Err(error) => {
            if !keyboard_typing_fallback_enabled() {
                anyhow::bail!(
                    "Clipboard initialization failed and keyboard typing fallback is disabled on macOS: {}",
                    error
                );
            }
            log::warn!(
                "Clipboard initialization failed; falling back to keyboard typing: {}",
                error
            );
            paste_text(text)?;
            return Ok(AutoPasteMethod::Typed);
        }
    };
    let mut delay = ThreadDelay;
    paste_text_hybrid_for_target_with(
        text,
        target,
        &mut clipboard,
        &mut delay,
        send_macos_paste_command_to_target,
    )
}

#[cfg(target_os = "macos")]
fn paste_text_hybrid_for_target_with<C, D, P>(
    text: &str,
    target: &AutoPasteTarget,
    clipboard: &mut C,
    delay: &mut D,
    paste_command: P,
) -> Result<AutoPasteMethod>
where
    C: ClipboardAccess,
    D: DelayProvider,
    P: FnMut(&AutoPasteTarget) -> Result<()>,
{
    let mut injector = MacTargetTextInjector {
        target: target.clone(),
        paste_command,
    };
    paste_text_hybrid_with(text, clipboard, &mut injector, delay)
}

#[cfg(not(target_os = "macos"))]
pub fn paste_text_for_target(text: &str, _target: &AutoPasteTarget) -> Result<AutoPasteMethod> {
    paste_text_hybrid(text)
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
            if !keyboard_typing_fallback_enabled() {
                anyhow::bail!(
                    "Clipboard auto-paste failed and keyboard typing fallback is disabled on macOS: {}",
                    error
                );
            }
            log::warn!(
                "Clipboard auto-paste failed; falling back to keyboard typing: {}",
                error
            );
            injector.type_text(text)?;
            Ok(AutoPasteMethod::Typed)
        }
    }
}

fn keyboard_typing_fallback_enabled() -> bool {
    !cfg!(target_os = "macos")
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
    let previous_text = match clipboard.get_text() {
        Ok(text) => Some(text),
        Err(error) => {
            log::warn!(
                "Clipboard previous text is unavailable; auto-paste will continue without restore: {}",
                error
            );
            None
        }
    };

    clipboard
        .set_text(text)
        .context("Failed to put auto-paste text into clipboard")?;
    delay.sleep(pre_paste_delay());

    if let Err(error) = injector
        .paste_shortcut()
        .context("Failed to send paste shortcut")
    {
        if injector.restore_clipboard_after_successful_paste() {
            if let Some(previous_text) = previous_text.as_deref() {
                if let Err(restore_error) = clipboard.set_text(previous_text).context(
                    "Failed to restore previous clipboard text after paste shortcut failure",
                ) {
                    log::warn!("{}", restore_error);
                }
            }
        } else {
            log::warn!(
                "Keeping auto-paste text in clipboard after paste command failure because target may read clipboard asynchronously"
            );
        }
        return Err(error);
    }

    if !injector.restore_clipboard_after_successful_paste() {
        log::info!(
            "Keeping auto-paste text in clipboard after successful paste command to avoid delayed paste races"
        );
        #[cfg(target_os = "macos")]
        delay.sleep(post_paste_commit_delay());
    } else if let Some(previous_text) = previous_text.as_deref() {
        delay.sleep(restore_clipboard_delay());
        restore_clipboard_if_unchanged(text, previous_text, clipboard);
    } else {
        delay.sleep(restore_clipboard_delay());
        log::warn!(
            "Keeping auto-paste text in clipboard because previous clipboard text was unreadable"
        );
    }
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
        focused_element_likely_accepts_text, normalize_auto_paste_target, paste_text_hybrid_with,
        should_use_clipboard_backend, target_matches_bundle_and_pid, AutoPasteMethod,
        ClipboardAccess, DelayProvider, TextInjector, AUTO_PASTE_CLIPBOARD_THRESHOLD_CHARS,
        VOICETEXT_BUNDLE_ID, VOICETEXT_DEV_BUNDLE_ID, VOICETEXT_PROD_BUNDLE_ID,
    };
    use anyhow::{bail, Context, Result};
    use std::cell::RefCell;
    #[cfg(target_os = "macos")]
    use std::ffi::c_void;
    #[cfg(target_os = "macos")]
    use std::path::PathBuf;
    #[cfg(target_os = "macos")]
    use std::process::{Child, Command};
    use std::rc::Rc;
    #[cfg(target_os = "macos")]
    use std::thread;
    use std::time::Duration;
    #[cfg(target_os = "macos")]
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

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
        "a".repeat(super::clipboard_backend_threshold_chars())
    }

    fn expected_successful_clipboard_events(text: &str) -> Vec<String> {
        vec![
            "get:1".to_string(),
            format!("set:{}", text),
            "sleep:80".to_string(),
            "paste".to_string(),
            format!("sleep:{}", super::restore_clipboard_delay().as_millis()),
            "get:2".to_string(),
            "set:previous".to_string(),
        ]
    }

    #[cfg(target_os = "macos")]
    fn expected_successful_clipboard_first_target_events(text: &str) -> Vec<String> {
        vec![
            "get:1".to_string(),
            format!("set:{}", text),
            "sleep:80".to_string(),
            "paste".to_string(),
            format!("sleep:{}", super::AUTO_PASTE_POST_PASTE_COMMIT_DELAY_MS),
        ]
    }

    #[cfg(target_os = "macos")]
    fn applescript_string_literal(value: &str) -> String {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    }

    #[cfg(target_os = "macos")]
    fn run_osascript(script: &str) -> Result<String> {
        let output = Command::new("osascript")
            .arg("-e")
            .arg(script)
            .output()
            .context("failed to run osascript")?;

        if !output.status.success() {
            bail!(
                "osascript failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    #[cfg(target_os = "macos")]
    struct TextEditSandboxFile {
        path: PathBuf,
        file_name: String,
    }

    #[cfg(target_os = "macos")]
    impl TextEditSandboxFile {
        fn new() -> Result<Self> {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .context("system clock is before unix epoch")?
                .as_nanos();
            let file_name = format!("voicetext-auto-paste-e2e-{stamp}.txt");
            let path = std::env::temp_dir().join(&file_name);
            std::fs::write(&path, "").context("failed to create TextEdit sandbox file")?;
            Ok(Self { path, file_name })
        }

        fn open_in_textedit(&self) -> Result<()> {
            let status = Command::new("open")
                .arg("-a")
                .arg("TextEdit")
                .arg(&self.path)
                .status()
                .context("failed to launch TextEdit with sandbox file")?;
            if !status.success() {
                bail!("open -a TextEdit failed with status {status}");
            }
            run_osascript(r#"tell application "TextEdit" to activate"#)?;
            Ok(())
        }
    }

    #[cfg(target_os = "macos")]
    impl Drop for TextEditSandboxFile {
        fn drop(&mut self) {
            let file_name = applescript_string_literal(&self.file_name);
            let script = format!(
                r#"tell application "TextEdit"
  repeat with d in documents
    try
      if name of d is {file_name} then
        close d saving no
        return "closed"
      end if
    end try
  end repeat
end tell
return "not_found""#
            );
            let _ = Command::new("osascript").arg("-e").arg(script).output();
            let _ = std::fs::remove_file(&self.path);
        }
    }

    #[cfg(target_os = "macos")]
    fn wait_until<T, F>(timeout: Duration, mut probe: F) -> Result<T>
    where
        F: FnMut() -> Result<Option<T>>,
    {
        let deadline = Instant::now() + timeout;
        let mut last_error = None;
        loop {
            match probe() {
                Ok(Some(value)) => return Ok(value),
                Ok(None) => {}
                Err(error) => last_error = Some(error),
            }
            if Instant::now() >= deadline {
                if let Some(error) = last_error {
                    bail!("timed out waiting for condition after last error: {error}");
                }
                bail!("timed out waiting for condition");
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    #[cfg(target_os = "macos")]
    fn wait_for_textedit_target() -> Result<super::AutoPasteTarget> {
        wait_until(Duration::from_secs(10), || {
            let _ = run_osascript(r#"tell application "TextEdit" to activate"#);
            let Some(target) = textedit_running_target()? else {
                return Ok(None);
            };
            let _ = super::activate_running_app_by_target(&target);
            Ok(super::frontmost_app_matches_target(&target).then_some(target))
        })
    }

    #[cfg(target_os = "macos")]
    fn textedit_running_target() -> Result<Option<super::AutoPasteTarget>> {
        let output = run_osascript(
            r#"tell application "System Events"
  set matches to every application process whose bundle identifier is "com.apple.TextEdit"
  if (count of matches) is 0 then return ""
  set targetProcess to item 1 of matches
  return unix id of targetProcess
end tell"#,
        )?;
        let output = output.trim();
        if output.is_empty() {
            return Ok(None);
        }

        let pid = output
            .parse::<i32>()
            .with_context(|| format!("failed to parse TextEdit pid from {output:?}"))?;
        Ok(Some(super::AutoPasteTarget {
            bundle_id: "com.apple.TextEdit".to_string(),
            pid,
        }))
    }

    #[cfg(target_os = "macos")]
    fn copy_focused_element_for_target(
        target: &super::AutoPasteTarget,
    ) -> Result<super::MacAXUIElementRef> {
        match super::copy_application_focused_element(target) {
            Ok(focused) => Ok(focused),
            Err(application_error) => {
                let focused = super::copy_system_focused_element().with_context(|| {
                    format!("application AXFocusedUIElement also failed: {application_error}")
                })?;
                let focused_pid = super::focused_element_pid(focused);
                if !super::focused_element_pid_matches_target(target, focused_pid) {
                    unsafe { super::CFRelease(focused as *const c_void) };
                    bail!(
                        "focused pid mismatch while reading AXValue: expected={}, actual={:?}",
                        target.pid,
                        focused_pid
                    );
                }
                Ok(focused)
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn focused_ax_value_for_target(target: &super::AutoPasteTarget) -> Result<Option<String>> {
        let focused = copy_focused_element_for_target(target)?;
        let focused_pid = super::focused_element_pid(focused);
        if !super::focused_element_pid_matches_target(target, focused_pid) {
            unsafe { super::CFRelease(focused as *const c_void) };
            bail!(
                "focused pid mismatch while reading AXValue: expected={}, actual={:?}",
                target.pid,
                focused_pid
            );
        }

        let value = super::copy_ax_string_attribute(focused, "AXValue");
        unsafe { super::CFRelease(focused as *const c_void) };
        Ok(value)
    }

    #[cfg(target_os = "macos")]
    fn wait_for_focused_ax_value_containing(
        target: &super::AutoPasteTarget,
        expected: &str,
    ) -> Result<String> {
        wait_until(Duration::from_secs(5), || {
            let value = focused_ax_value_for_target(target)?;
            Ok(value.filter(|text| text.contains(expected)))
        })
    }

    #[cfg(target_os = "macos")]
    fn wait_for_focused_textedit_ax_value(target: &super::AutoPasteTarget) -> Result<String> {
        wait_until(Duration::from_secs(5), || {
            let diagnostics = super::focused_element_diagnostics(target)?;
            if !diagnostics.likely_text_input {
                return Ok(None);
            }
            Ok(focused_ax_value_for_target(target)?.or(Some(String::new())))
        })
    }

    #[cfg(target_os = "macos")]
    #[derive(Clone, Copy)]
    struct BrowserCandidate {
        executable: &'static str,
        bundle_id: &'static str,
    }

    #[cfg(target_os = "macos")]
    struct BrowserSandbox {
        child: Child,
        root: PathBuf,
        bundle_id: String,
    }

    #[cfg(target_os = "macos")]
    impl BrowserSandbox {
        fn launch_controlled_textarea(candidate: BrowserCandidate) -> Result<Self> {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .context("system clock is before unix epoch")?
                .as_nanos();
            let root = std::env::temp_dir().join(format!("voicetext-browser-e2e-{stamp}"));
            let profile_dir = root.join("profile");
            let page_path = root.join("controlled-textarea.html");
            std::fs::create_dir_all(&profile_dir)
                .context("failed to create browser profile dir")?;
            std::fs::write(&page_path, controlled_textarea_html())
                .context("failed to create browser e2e page")?;
            let file_url = format!("file://{}", page_path.display());

            let child = Command::new(candidate.executable)
                .arg(format!("--user-data-dir={}", profile_dir.display()))
                .arg("--no-first-run")
                .arg("--no-default-browser-check")
                .arg("--disable-session-crashed-bubble")
                .arg("--disable-features=Translate")
                .arg("--new-window")
                .arg(file_url)
                .spawn()
                .context("failed to launch isolated browser for auto-paste e2e")?;

            Ok(Self {
                child,
                root,
                bundle_id: candidate.bundle_id.to_string(),
            })
        }
    }

    #[cfg(target_os = "macos")]
    impl Drop for BrowserSandbox {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[cfg(target_os = "macos")]
    fn browser_candidates() -> Vec<BrowserCandidate> {
        [
            BrowserCandidate {
                executable: "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
                bundle_id: "com.brave.Browser",
            },
            BrowserCandidate {
                executable: "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
                bundle_id: "com.google.Chrome",
            },
            BrowserCandidate {
                executable: "/Applications/Chromium.app/Contents/MacOS/Chromium",
                bundle_id: "org.chromium.Chromium",
            },
        ]
        .into_iter()
        .filter(|candidate| std::path::Path::new(candidate.executable).exists())
        .collect()
    }

    #[cfg(target_os = "macos")]
    fn controlled_textarea_html() -> &'static str {
        r#"<!doctype html>
<meta charset="utf-8">
<title>VT_READY</title>
<textarea id="input" autofocus style="width: 800px; height: 240px;"></textarea>
<script>
const input = document.getElementById('input');
let state = '';
function render() {
  if (input.value !== state) input.value = state;
}
function updateTitle(prefix) {
  document.title = prefix + ':' + state;
}
input.addEventListener('input', () => {
  state = input.value;
  updateTitle('VT_VALUE');
});
input.addEventListener('change', () => {
  state = input.value;
  updateTitle('VT_VALUE');
});
window.addEventListener('load', () => {
  input.focus();
  updateTitle('VT_READY');
});
setInterval(render, 50);
</script>"#
    }

    #[cfg(target_os = "macos")]
    fn browser_running_targets(bundle_id: &str) -> Result<Vec<super::AutoPasteTarget>> {
        let bundle_id_literal = applescript_string_literal(bundle_id);
        let script = format!(
            r#"tell application "System Events"
  set matches to every application process whose bundle identifier is {bundle_id_literal}
  set output to ""
  repeat with targetProcess in matches
    try
      set output to output & (unix id of targetProcess as text) & linefeed
    end try
  end repeat
  return output
end tell"#
        );
        let output = run_osascript(&script)?;
        let targets = output
            .lines()
            .filter_map(|line| line.trim().parse::<i32>().ok())
            .map(|pid| super::AutoPasteTarget {
                bundle_id: bundle_id.to_string(),
                pid,
            })
            .collect();
        Ok(targets)
    }

    #[cfg(target_os = "macos")]
    fn wait_for_browser_target(
        bundle_id: &str,
        expected_title: &str,
    ) -> Result<super::AutoPasteTarget> {
        wait_until(Duration::from_secs(10), || {
            for target in browser_running_targets(bundle_id)? {
                let title = front_window_title_for_pid(target.pid).unwrap_or_default();
                if !title.contains(expected_title) {
                    continue;
                }

                let _ = super::activate_running_app_by_target(&target);
                if super::frontmost_app_matches_target(&target) {
                    return Ok(Some(target));
                }
            }
            Ok(None)
        })
    }

    #[cfg(target_os = "macos")]
    fn front_window_title_for_pid(pid: i32) -> Result<String> {
        let script = format!(
            r#"tell application "System Events"
  set targetProcess to first process whose unix id is {pid}
  return name of front window of targetProcess
end tell"#
        );
        run_osascript(&script)
    }

    #[cfg(target_os = "macos")]
    fn wait_for_front_window_title_containing(pid: i32, expected: &str) -> Result<String> {
        wait_until(Duration::from_secs(10), || {
            let title = front_window_title_for_pid(pid)?;
            Ok(title.contains(expected).then_some(title))
        })
    }

    #[cfg(target_os = "macos")]
    struct TextClipboardGuard {
        original: String,
    }

    #[cfg(target_os = "macos")]
    impl TextClipboardGuard {
        fn replace_with(marker: &str) -> Result<Self> {
            let mut clipboard = super::SystemClipboard::new()
                .context("clipboard must be available for macOS auto-paste e2e")?;
            let original = clipboard
                .get_text()
                .context("macOS auto-paste e2e requires a readable text clipboard")?;
            clipboard
                .set_text(marker)
                .context("failed to set e2e clipboard marker")?;
            Ok(Self { original })
        }

        fn current_text(&self) -> Result<String> {
            let mut clipboard = super::SystemClipboard::new()
                .context("clipboard must be available for macOS auto-paste e2e")?;
            clipboard
                .get_text()
                .context("failed to read e2e clipboard text")
        }
    }

    #[cfg(target_os = "macos")]
    impl Drop for TextClipboardGuard {
        fn drop(&mut self) {
            if let Ok(mut clipboard) = super::SystemClipboard::new() {
                let _ = clipboard.set_text(&self.original);
            }
        }
    }

    #[test]
    fn normalize_auto_paste_target_rejects_voicetext_bundles() {
        for bundle_id in [
            VOICETEXT_BUNDLE_ID,
            VOICETEXT_PROD_BUNDLE_ID,
            VOICETEXT_DEV_BUNDLE_ID,
        ] {
            assert_eq!(
                normalize_auto_paste_target(bundle_id.to_string(), 123),
                None
            );
        }
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
    fn clipboard_backend_starts_at_platform_threshold() {
        let threshold = super::clipboard_backend_threshold_chars();
        let below_threshold = "a".repeat(threshold - 1);
        let at_threshold = "a".repeat(threshold);

        assert!(!should_use_clipboard_backend(&below_threshold));
        assert!(should_use_clipboard_backend(&at_threshold));
        assert_eq!(AUTO_PASTE_CLIPBOARD_THRESHOLD_CHARS, 100);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_uses_clipboard_for_short_text_in_electron_targets() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let text = "привет".to_string();
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
            paste_text_hybrid_with(&text, &mut clipboard, &mut injector, &mut delay).unwrap();

        assert_eq!(method, AutoPasteMethod::Clipboard);
        assert_eq!(clipboard.text, "previous");
        assert!(injector.typed_texts.is_empty());
        assert_eq!(injector.paste_shortcut_calls, 1);
        assert_eq!(
            super::clipboard_backend_threshold_chars(),
            super::AUTO_PASTE_MACOS_CLIPBOARD_THRESHOLD_CHARS
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_clipboard_first_target_keeps_auto_paste_text_after_saved_target_paste() {
        let target = normalize_auto_paste_target("com.openai.codex".to_string(), 123)
            .expect("target must be valid");
        let events = Rc::new(RefCell::new(Vec::new()));
        let seen_targets = Rc::new(RefCell::new(Vec::<super::AutoPasteTarget>::new()));
        let text = "привет".to_string();
        let mut clipboard = FakeClipboard::new("previous", events.clone());
        let mut delay = FakeDelay {
            events: events.clone(),
            ..Default::default()
        };

        let method =
            super::paste_text_hybrid_for_target_with(&text, &target, &mut clipboard, &mut delay, {
                let events = events.clone();
                let seen_targets = seen_targets.clone();
                move |target| {
                    events.borrow_mut().push("paste".to_string());
                    seen_targets.borrow_mut().push(target.clone());
                    Ok(())
                }
            })
            .unwrap();

        assert_eq!(method, AutoPasteMethod::Clipboard);
        assert_eq!(seen_targets.borrow().as_slice(), [target]);
        assert_eq!(clipboard.text, text);
        assert_eq!(
            *events.borrow(),
            expected_successful_clipboard_first_target_events(&text)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_clipboard_first_target_does_not_restore_previous_clipboard_into_late_paste_reader() {
        let target = normalize_auto_paste_target("com.openai.codex".to_string(), 123)
            .expect("target must be valid");
        let events = Rc::new(RefCell::new(Vec::new()));
        let previous_clipboard = "4874100030141942";
        let text = "привет из диктовки".to_string();
        let mut clipboard = FakeClipboard::new(previous_clipboard, events.clone());
        let mut delay = FakeDelay {
            events: events.clone(),
            ..Default::default()
        };

        let method =
            super::paste_text_hybrid_for_target_with(&text, &target, &mut clipboard, &mut delay, {
                let events = events.clone();
                move |_| {
                    events.borrow_mut().push("paste".to_string());
                    Ok(())
                }
            })
            .unwrap();

        assert_eq!(method, AutoPasteMethod::Clipboard);
        assert_eq!(clipboard.text, text);
        assert!(!events
            .borrow()
            .iter()
            .any(|event| event == &format!("set:{previous_clipboard}")));
        assert_eq!(
            *events.borrow(),
            expected_successful_clipboard_first_target_events(&text)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_clipboard_first_target_keeps_auto_paste_text_when_paste_command_reports_failure() {
        let target = normalize_auto_paste_target("com.openai.codex".to_string(), 123)
            .expect("target must be valid");
        let events = Rc::new(RefCell::new(Vec::new()));
        let previous_clipboard = "secret clipboard";
        let text = "привет из диктовки".to_string();
        let mut clipboard = FakeClipboard::new(previous_clipboard, events.clone());
        let mut delay = FakeDelay {
            events: events.clone(),
            ..Default::default()
        };

        let error =
            super::paste_text_hybrid_for_target_with(&text, &target, &mut clipboard, &mut delay, {
                let events = events.clone();
                move |_| {
                    events.borrow_mut().push("paste".to_string());
                    bail!("paste command reported failure after dispatch")
                }
            })
            .unwrap_err();

        assert!(error
            .to_string()
            .contains("keyboard typing fallback is disabled"));
        assert_eq!(clipboard.text, text);
        assert!(!events
            .borrow()
            .iter()
            .any(|event| event == &format!("set:{previous_clipboard}")));
        assert_eq!(
            *events.borrow(),
            vec![
                "get:1".to_string(),
                format!("set:{}", text),
                "sleep:80".to_string(),
                "paste".to_string(),
            ]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_known_native_target_restores_previous_clipboard_after_paste() {
        let target = normalize_auto_paste_target("com.apple.TextEdit".to_string(), 123)
            .expect("target must be valid");
        let events = Rc::new(RefCell::new(Vec::new()));
        let text = "привет из диктовки".to_string();
        let mut clipboard = FakeClipboard::new("previous", events.clone());
        let mut delay = FakeDelay {
            events: events.clone(),
            ..Default::default()
        };

        let method =
            super::paste_text_hybrid_for_target_with(&text, &target, &mut clipboard, &mut delay, {
                let events = events.clone();
                move |_| {
                    events.borrow_mut().push("paste".to_string());
                    Ok(())
                }
            })
            .unwrap();

        assert_eq!(method, AutoPasteMethod::Clipboard);
        assert_eq!(clipboard.text, "previous");
        assert_eq!(
            *events.borrow(),
            expected_successful_clipboard_events(&text)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_known_native_target_restores_previous_clipboard_when_paste_command_fails() {
        let target = normalize_auto_paste_target("com.apple.TextEdit".to_string(), 123)
            .expect("target must be valid");
        let events = Rc::new(RefCell::new(Vec::new()));
        let text = "привет из диктовки".to_string();
        let mut clipboard = FakeClipboard::new("previous", events.clone());
        let mut delay = FakeDelay {
            events: events.clone(),
            ..Default::default()
        };

        let error =
            super::paste_text_hybrid_for_target_with(&text, &target, &mut clipboard, &mut delay, {
                let events = events.clone();
                move |_| {
                    events.borrow_mut().push("paste".to_string());
                    bail!("paste command failed before target consumed clipboard")
                }
            })
            .unwrap_err();

        assert!(error
            .to_string()
            .contains("keyboard typing fallback is disabled"));
        assert_eq!(clipboard.text, "previous");
        assert_eq!(
            *events.borrow(),
            vec![
                "get:1".to_string(),
                format!("set:{}", text),
                "sleep:80".to_string(),
                "paste".to_string(),
                "set:previous".to_string(),
            ]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_unknown_target_keeps_auto_paste_text_after_paste() {
        let target = normalize_auto_paste_target("com.example.UnknownEditor".to_string(), 123)
            .expect("target must be valid");
        let events = Rc::new(RefCell::new(Vec::new()));
        let text = "привет из диктовки".to_string();
        let mut clipboard = FakeClipboard::new("previous", events.clone());
        let mut delay = FakeDelay {
            events: events.clone(),
            ..Default::default()
        };

        let method =
            super::paste_text_hybrid_for_target_with(&text, &target, &mut clipboard, &mut delay, {
                let events = events.clone();
                move |_| {
                    events.borrow_mut().push("paste".to_string());
                    Ok(())
                }
            })
            .unwrap();

        assert_eq!(method, AutoPasteMethod::Clipboard);
        assert_eq!(clipboard.text, text);
        assert_eq!(
            *events.borrow(),
            expected_successful_clipboard_first_target_events(&text)
        );
    }

    #[test]
    fn clipboard_restore_delay_leaves_time_for_async_paste_handlers() {
        #[cfg(target_os = "macos")]
        assert!(
            super::restore_clipboard_delay() >= super::post_paste_commit_delay(),
            "macOS restore must wait at least through the paste commit window"
        );

        #[cfg(not(target_os = "macos"))]
        assert!(super::AUTO_PASTE_RESTORE_CLIPBOARD_DELAY_MS >= 2_500);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn clipboard_success_waits_for_macos_paste_commit_before_restore() {
        assert!(super::AUTO_PASTE_POST_PASTE_COMMIT_DELAY_MS >= 250);
        assert!(super::restore_clipboard_delay() >= super::post_paste_commit_delay());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_paste_shortcut_holds_physical_command_around_v_key() {
        let sequence = super::macos_paste_event_sequence();

        assert_eq!(sequence.len(), 4);
        assert_eq!(sequence[0].key_code, super::MACOS_LEFT_COMMAND_KEY_CODE);
        assert!(sequence[0].key_down);
        assert_eq!(sequence[0].flags, super::MAC_CG_EVENT_FLAG_MASK_COMMAND);
        assert_eq!(sequence[1].key_code, super::MACOS_ANSI_V_KEY_CODE);
        assert!(sequence[1].key_down);
        assert_eq!(sequence[1].flags, super::MAC_CG_EVENT_FLAG_MASK_COMMAND);
        assert_eq!(sequence[1].unicode, None);
        assert_eq!(sequence[2].key_code, super::MACOS_ANSI_V_KEY_CODE);
        assert!(!sequence[2].key_down);
        assert_eq!(sequence[2].flags, super::MAC_CG_EVENT_FLAG_MASK_COMMAND);
        assert_eq!(sequence[2].unicode, None);
        assert_eq!(sequence[3].key_code, super::MACOS_LEFT_COMMAND_KEY_CODE);
        assert!(!sequence[3].key_down);
        assert_eq!(sequence[3].flags, 0);
        assert!(!sequence.iter().any(|event| {
            event.key_code == super::MACOS_ANSI_V_KEY_CODE
                && event.flags != super::MAC_CG_EVENT_FLAG_MASK_COMMAND
        }));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_paste_command_uses_menu_before_keyboard_shortcut() {
        let target =
            normalize_auto_paste_target("com.apple.TextEdit".to_string(), 123).expect("valid");
        let events = Rc::new(RefCell::new(Vec::<&'static str>::new()));

        super::send_macos_paste_command_for_target(
            &target,
            {
                let events = events.clone();
                move |_| {
                    events.borrow_mut().push("menu");
                    Ok(true)
                }
            },
            {
                let events = events.clone();
                move || {
                    events.borrow_mut().push("keyboard");
                    Ok(())
                }
            },
            {
                let events = events.clone();
                move |_| {
                    events.borrow_mut().push("system-events");
                    Ok(())
                }
            },
        )
        .unwrap();

        assert_eq!(events.borrow().as_slice(), ["menu"]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_ax_unreliable_targets_use_system_events_before_ax_menu() {
        for bundle_id in [
            "com.openai.codex",
            "com.anthropic.claude",
            "com.google.Chrome",
            "com.apple.Terminal",
            "com.googlecode.iterm2",
            "dev.warp.Warp-Stable",
        ] {
            let target = normalize_auto_paste_target(bundle_id.to_string(), 123)
                .expect("target must be valid");
            let events = Rc::new(RefCell::new(Vec::<&'static str>::new()));

            super::send_macos_paste_command_for_target(
                &target,
                {
                    let events = events.clone();
                    move |_| {
                        events.borrow_mut().push("menu");
                        bail!("AXPress paste menu should not run for clipboard-first target")
                    }
                },
                {
                    let events = events.clone();
                    move || {
                        events.borrow_mut().push("keyboard");
                        bail!("keyboard fallback should not run for clipboard-first target")
                    }
                },
                {
                    let events = events.clone();
                    move |_| {
                        events.borrow_mut().push("system-events");
                        Ok(())
                    }
                },
            )
            .unwrap();

            assert_eq!(events.borrow().as_slice(), ["system-events"]);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_ax_unreliable_targets_refuse_ax_menu_false_success_after_system_events_failure() {
        let target = normalize_auto_paste_target("com.openai.codex".to_string(), 123)
            .expect("target must be valid");
        let events = Rc::new(RefCell::new(Vec::<&'static str>::new()));

        let result = super::send_macos_paste_command_for_target(
            &target,
            {
                let events = events.clone();
                move |_| {
                    events.borrow_mut().push("menu");
                    Ok(true)
                }
            },
            {
                let events = events.clone();
                move || {
                    events.borrow_mut().push("keyboard");
                    Ok(())
                }
            },
            {
                let events = events.clone();
                move |_| {
                    events.borrow_mut().push("system-events");
                    bail!("system events unavailable")
                }
            },
        );

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("refusing AXPress paste menu fallback"));
        assert_eq!(events.borrow().as_slice(), ["system-events"]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_clipboard_first_targets_refuse_ax_and_keyboard_after_system_events_failure() {
        let target =
            normalize_auto_paste_target("com.google.Chrome".to_string(), 123).expect("valid");
        let events = Rc::new(RefCell::new(Vec::<&'static str>::new()));

        let result = super::send_macos_paste_command_for_target(
            &target,
            {
                let events = events.clone();
                move |_| {
                    events.borrow_mut().push("menu");
                    Ok(false)
                }
            },
            {
                let events = events.clone();
                move || {
                    events.borrow_mut().push("keyboard");
                    Ok(())
                }
            },
            {
                let events = events.clone();
                move |_| {
                    events.borrow_mut().push("system-events");
                    bail!("system events unavailable")
                }
            },
        );

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("refusing AXPress paste menu fallback"));
        assert_eq!(events.borrow().as_slice(), ["system-events"]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_paste_command_keeps_keyboard_fallback_for_non_clipboard_first_targets() {
        let target =
            normalize_auto_paste_target("com.apple.TextEdit".to_string(), 123).expect("valid");
        let events = Rc::new(RefCell::new(Vec::<&'static str>::new()));

        super::send_macos_paste_command_for_target(
            &target,
            {
                let events = events.clone();
                move |_| {
                    events.borrow_mut().push("menu");
                    Ok(false)
                }
            },
            {
                let events = events.clone();
                move || {
                    events.borrow_mut().push("keyboard");
                    Ok(())
                }
            },
            {
                let events = events.clone();
                move |_| {
                    events.borrow_mut().push("system-events");
                    bail!("system events unavailable")
                }
            },
        )
        .unwrap();

        assert_eq!(
            events.borrow().as_slice(),
            ["menu", "system-events", "keyboard"]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_system_events_paste_menu_script_clicks_menu_without_keystrokes() {
        let script = super::macos_system_events_paste_menu_script(123);

        assert!(script.contains("System Events"));
        assert!(script.contains("first process whose unix id is 123"));
        assert!(script.contains("click pasteItem"));
        assert!(script.contains("\"Paste\""));
        assert!(script.contains("\"Вставить\""));
        assert!(!script.contains("keystroke"));
        assert!(!script.contains("key code"));
        assert!(!script.contains("command down"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_paste_menu_match_prefers_paste_menu_items() {
        assert_eq!(
            super::macos_paste_menu_search_modes(),
            &[super::PasteMenuMatchMode::ExactTitle]
        );
        assert!(super::paste_menu_item_matches_values(
            Some("AXMenuItem"),
            Some("Paste"),
            Some(true),
            super::PasteMenuMatchMode::ExactTitle
        ));
        assert!(super::paste_menu_item_matches_values(
            Some("AXMenuItem"),
            Some("Вставить"),
            None,
            super::PasteMenuMatchMode::ExactTitle
        ));
        assert!(super::paste_menu_item_matches_values(
            Some("AXMenuItem"),
            Some("Вставити"),
            Some(true),
            super::PasteMenuMatchMode::ExactTitle
        ));
        assert!(!super::paste_menu_item_matches_values(
            Some("AXButton"),
            Some("Paste"),
            Some(true),
            super::PasteMenuMatchMode::ExactTitle
        ));
        assert!(!super::paste_menu_item_matches_values(
            Some("AXMenuItem"),
            Some("Pasteboard"),
            Some(true),
            super::PasteMenuMatchMode::ExactTitle
        ));
        assert!(!super::paste_menu_item_matches_values(
            Some("AXMenuItem"),
            Some("Paste"),
            Some(false),
            super::PasteMenuMatchMode::ExactTitle
        ));
        assert!(!super::paste_menu_item_matches_values(
            Some("AXMenuItem"),
            Some("Paste and Match Style"),
            Some(true),
            super::PasteMenuMatchMode::ExactTitle
        ));
    }

    #[test]
    fn keyboard_typing_fallback_is_disabled_on_macos() {
        assert_eq!(
            super::keyboard_typing_fallback_enabled(),
            !cfg!(target_os = "macos")
        );
    }

    #[test]
    fn focused_element_likely_accepts_text_for_editable_roles_or_settable_attrs() {
        assert!(focused_element_likely_accepts_text(
            Some("AXTextArea"),
            None,
            Some(false),
            Some(false),
            Some(false)
        ));
        assert!(focused_element_likely_accepts_text(
            Some("AXGroup"),
            None,
            Some(true),
            Some(false),
            Some(false)
        ));
        assert!(focused_element_likely_accepts_text(
            Some("AXWebArea"),
            None,
            Some(false),
            Some(true),
            Some(true)
        ));
        assert!(!focused_element_likely_accepts_text(
            Some("AXButton"),
            None,
            Some(false),
            Some(false),
            Some(false)
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_accessibility_insert_is_preferred_before_clipboard_fallback() {
        let method = super::paste_text_with_macos_accessibility_attempt(
            || Ok(true),
            || bail!("clipboard fallback should not run"),
        )
        .unwrap();

        assert_eq!(method, AutoPasteMethod::Accessibility);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_accessibility_insert_falls_back_to_clipboard_when_unavailable() {
        let method = super::paste_text_with_macos_accessibility_attempt(
            || Ok(false),
            || Ok(AutoPasteMethod::Clipboard),
        )
        .unwrap();

        assert_eq!(method, AutoPasteMethod::Clipboard);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_accessibility_insert_falls_back_to_clipboard_on_error() {
        let method = super::paste_text_with_macos_accessibility_attempt(
            || bail!("AX unavailable"),
            || Ok(AutoPasteMethod::Clipboard),
        )
        .unwrap();

        assert_eq!(method, AutoPasteMethod::Clipboard);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_web_and_editor_targets_use_clipboard_before_accessibility() {
        for bundle_id in [
            "com.brave.Browser",
            "com.google.Chrome",
            "com.anthropic.claude",
            "com.openai.codex",
            "com.todesktop.cursor",
            "com.microsoft.VSCode",
        ] {
            let target = normalize_auto_paste_target(bundle_id.to_string(), 123)
                .expect("target must be valid");
            let method = super::paste_text_for_target_with(
                &target,
                || Ok(true),
                || Ok(AutoPasteMethod::Clipboard),
            )
            .unwrap();

            assert_eq!(method, AutoPasteMethod::Clipboard);
            assert!(super::target_prefers_clipboard_paste(&target));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_clipboard_first_targets_try_accessibility_after_clipboard_failure() {
        let target = normalize_auto_paste_target("com.anthropic.claude".to_string(), 123)
            .expect("target must be valid");
        let events = Rc::new(RefCell::new(Vec::<&'static str>::new()));

        let method = super::paste_text_for_target_with(
            &target,
            {
                let events = events.clone();
                move || {
                    events.borrow_mut().push("accessibility");
                    Ok(true)
                }
            },
            {
                let events = events.clone();
                move || {
                    events.borrow_mut().push("clipboard");
                    bail!("menu paste unavailable")
                }
            },
        )
        .unwrap();

        assert_eq!(method, AutoPasteMethod::Accessibility);
        assert_eq!(events.borrow().as_slice(), ["clipboard", "accessibility"]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_clipboard_first_targets_return_error_when_accessibility_fallback_unavailable() {
        let target = normalize_auto_paste_target("com.openai.codex".to_string(), 123)
            .expect("target must be valid");

        let error = super::paste_text_for_target_with(
            &target,
            || Ok(false),
            || bail!("menu paste unavailable"),
        )
        .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("Clipboard-first auto-paste failed"));
        assert!(message.contains("AXSelectedText fallback was unavailable"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_terminal_targets_use_clipboard_before_accessibility() {
        for bundle_id in [
            "com.apple.Terminal",
            "com.googlecode.iterm2",
            "dev.warp.Warp-Stable",
            "com.mitchellh.ghostty",
            "com.github.wez.wezterm",
            "org.alacritty",
        ] {
            let target = normalize_auto_paste_target(bundle_id.to_string(), 123)
                .expect("target must be valid");
            let method = super::paste_text_for_target_with(
                &target,
                || bail!("AXSelectedText should be skipped for {}", bundle_id),
                || Ok(AutoPasteMethod::Clipboard),
            )
            .unwrap();

            assert_eq!(method, AutoPasteMethod::Clipboard);
            assert!(super::target_prefers_clipboard_paste(&target));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_native_targets_can_still_try_accessibility_before_clipboard() {
        let target = normalize_auto_paste_target("com.apple.TextEdit".to_string(), 123)
            .expect("target must be valid");
        let method = super::paste_text_for_target_with(
            &target,
            || Ok(true),
            || bail!("clipboard fallback should not run for successful native AX insert"),
        )
        .unwrap();

        assert_eq!(method, AutoPasteMethod::Accessibility);
        assert!(!super::target_prefers_clipboard_paste(&target));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_accessibility_insert_requires_focused_pid_to_match_target() {
        let target = normalize_auto_paste_target("com.example.App".to_string(), 123)
            .expect("target must be valid");

        assert!(super::focused_element_pid_matches_target(
            &target,
            Some(123)
        ));
        assert!(!super::focused_element_pid_matches_target(
            &target,
            Some(456)
        ));
        assert!(!super::focused_element_pid_matches_target(&target, None));
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "macOS GUI e2e: opens a temporary TextEdit document and uses the real paste path"]
    fn macos_textedit_runtime_e2e_pastes_into_sandbox_file() -> Result<()> {
        if !super::check_accessibility_permission() {
            bail!("Accessibility permission is required for the macOS auto-paste e2e");
        }

        let sandbox = TextEditSandboxFile::new()?;
        sandbox.open_in_textedit()?;

        let target = wait_for_textedit_target().context("TextEdit did not become frontmost")?;
        wait_for_front_window_title_containing(target.pid, &sandbox.file_name).with_context(
            || {
                format!(
                    "TextEdit front window did not become sandbox file {:?}",
                    sandbox.file_name
                )
            },
        )?;
        let initial_value = wait_for_focused_textedit_ax_value(&target)
            .context("TextEdit focused editor did not become readable")?;
        assert!(
            initial_value.is_empty(),
            "sandbox TextEdit file must start empty, got: {initial_value:?}"
        );

        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before unix epoch")?
            .as_nanos();
        let marker = format!("voicetext e2e {stamp} привет");
        let clipboard_marker = format!("voicetext clipboard restore e2e {stamp}");
        let clipboard_guard = TextClipboardGuard::replace_with(&clipboard_marker)?;
        let method = super::paste_text_for_target(&marker, &target)?;
        let pasted_value =
            wait_for_focused_ax_value_containing(&target, &marker).with_context(|| {
                format!("TextEdit focused editor did not contain marker after {method:?} paste")
            })?;

        assert!(
            pasted_value.contains(&marker),
            "focused TextEdit AXValue must contain marker after {method:?} paste"
        );
        assert_eq!(
            clipboard_guard.current_text()?,
            clipboard_marker,
            "clipboard text must be restored after {method:?} paste"
        );
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "macOS GUI e2e: launches an isolated Brave/Chrome profile and uses the real paste path"]
    fn macos_browser_controlled_textarea_e2e_commits_dom_input_event() -> Result<()> {
        if !super::check_accessibility_permission() {
            bail!("Accessibility permission is required for the macOS browser auto-paste e2e");
        }

        let candidates = browser_candidates();
        if candidates.is_empty() {
            bail!("Brave/Chrome executable not found for browser auto-paste e2e");
        }

        let mut errors = Vec::new();
        for candidate in candidates {
            match run_browser_controlled_textarea_e2e(candidate) {
                Ok(()) => return Ok(()),
                Err(error) => errors.push(format!("{}: {error:#}", candidate.bundle_id)),
            }
        }

        bail!(
            "no browser candidate completed controlled textarea e2e:\n{}",
            errors.join("\n")
        )
    }

    #[cfg(target_os = "macos")]
    fn run_browser_controlled_textarea_e2e(candidate: BrowserCandidate) -> Result<()> {
        let sandbox = BrowserSandbox::launch_controlled_textarea(candidate)?;
        let target =
            wait_for_browser_target(&sandbox.bundle_id, "VT_READY").with_context(|| {
                format!(
                    "isolated browser target did not become frontmost: bundle_id={}",
                    sandbox.bundle_id
                )
            })?;
        wait_for_front_window_title_containing(target.pid, "VT_READY")
            .context("controlled textarea page did not report VT_READY in window title")?;

        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before unix epoch")?
            .as_nanos();
        let marker = format!("voicetext browser e2e {stamp}");
        let clipboard_marker = format!("voicetext browser clipboard restore e2e {stamp}");
        let clipboard_guard = TextClipboardGuard::replace_with(&clipboard_marker)?;
        let method = super::paste_text_for_target(&marker, &target)?;
        let title =
            wait_for_front_window_title_containing(target.pid, &marker).with_context(|| {
                format!("controlled textarea did not commit DOM input event after {method:?} paste")
            })?;

        assert!(
            title.contains(&marker),
            "controlled browser textarea must commit marker through DOM input event after {method:?} paste"
        );
        assert_eq!(
            clipboard_guard.current_text()?,
            marker,
            "clipboard-first browser target must keep auto-paste text after {method:?} paste to avoid delayed reader races"
        );
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn hybrid_uses_typing_for_short_text() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let text = "short text";
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
            paste_text_hybrid_with(text, &mut clipboard, &mut injector, &mut delay).unwrap();

        assert_eq!(method, AutoPasteMethod::Typed);
        assert_eq!(clipboard.text, "previous");
        assert_eq!(injector.typed_texts, vec![text]);
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
        let expected_events = expected_successful_clipboard_events(&text);

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
    fn hybrid_uses_clipboard_when_previous_text_is_unavailable() {
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

        assert_eq!(method, AutoPasteMethod::Clipboard);
        assert_eq!(clipboard.text, text);
        assert!(injector.typed_texts.is_empty());
        assert_eq!(injector.paste_shortcut_calls, 1);
        assert_eq!(*events.borrow(), {
            let mut expected = vec![
                "get:1".to_string(),
                format!("set:{}", text),
                "sleep:80".to_string(),
                "paste".to_string(),
            ];
            expected.push(format!(
                "sleep:{}",
                super::restore_clipboard_delay().as_millis()
            ));
            expected
        });
    }

    #[test]
    fn hybrid_restores_clipboard_and_obeys_typing_fallback_policy_when_paste_shortcut_fails() {
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
        let mut expected_events = vec![
            "get:1".to_string(),
            format!("set:{}", text),
            "sleep:80".to_string(),
            "paste".to_string(),
            "set:previous".to_string(),
        ];
        if super::keyboard_typing_fallback_enabled() {
            expected_events.push(format!("type:{}", text.len()));
        }

        let result = paste_text_hybrid_with(&text, &mut clipboard, &mut injector, &mut delay);

        assert_eq!(clipboard.text, "previous");
        assert_eq!(injector.paste_shortcut_calls, 1);
        assert_eq!(*events.borrow(), expected_events);
        if super::keyboard_typing_fallback_enabled() {
            assert_eq!(result.unwrap(), AutoPasteMethod::Typed);
            assert_eq!(injector.typed_texts, vec![text]);
        } else {
            assert!(injector.typed_texts.is_empty());
            assert!(result
                .unwrap_err()
                .to_string()
                .contains("keyboard typing fallback is disabled"));
        }
    }
}

#[cfg(target_os = "macos")]
pub(crate) struct ContinuationAutoreleasePool(cocoa::base::id);
#[cfg(target_os = "macos")]
impl ContinuationAutoreleasePool {
    pub(crate) fn new() -> Self {
        Self(unsafe { cocoa::foundation::NSAutoreleasePool::new(cocoa::base::nil) })
    }
}
#[cfg(target_os = "macos")]
impl Drop for ContinuationAutoreleasePool {
    fn drop(&mut self) {
        use cocoa::foundation::NSAutoreleasePool;
        unsafe {
            self.0.drain();
        }
    }
}

// Continuation effects must recheck the request after every intervening native
// lookup/effect. Keep the legacy activation helper above unchanged.
#[cfg(target_os = "macos")]
fn activate_continuation_target(target: &AutoPasteTarget) -> Result<()> {
    use super::continuation_context::begin_effect;
    use cocoa::base::{id, nil};
    use objc::{class, msg_send, sel, sel_impl};

    if normalize_auto_paste_target(target.bundle_id.clone(), target.pid).is_none() {
        anyhow::bail!("Invalid continuation target");
    }
    unsafe {
        let app: id = msg_send![class!(NSRunningApplication),
            runningApplicationWithProcessIdentifier: target.pid];
        if app == nil || running_app_bundle_id(app).as_deref() != Some(target.bundle_id.as_str()) {
            anyhow::bail!("Continuation target unavailable");
        }
        if !begin_effect() {
            anyhow::bail!("Continuation execution refused");
        }
        let _: bool = msg_send![app, unhide];
        if !begin_effect() {
            anyhow::bail!("Continuation execution refused");
        }
        let activated: bool = msg_send![app, activateWithOptions: 1usize << 1];
        if !activated {
            anyhow::bail!("Continuation activation refused");
        }
    }
    Ok(())
}

// Qualification is deliberately explicit: AX support alone is insufficient.
pub(crate) fn continuation_app_qualified(target: &AutoPasteTarget) -> bool {
    target.bundle_id == "com.apple.TextEdit" && target.pid > 0
}

fn publish_continuation_copy(
    clipboard: &mut impl ClipboardAccess,
    text: &str,
    revision: u64,
) -> super::continuation_context::GuardedPasteOutcome {
    use super::continuation_context::{begin_effect, GuardedPasteOutcome};
    if !begin_effect() {
        return GuardedPasteOutcome::Unavailable;
    }
    match clipboard.set_text(text) {
        Ok(()) => GuardedPasteOutcome::Confirmed { revision },
        Err(_) => GuardedPasteOutcome::Uncertain,
    }
}

// Terminal automatic copy shares the guarded executor, never the legacy IPC.
pub(crate) fn continuation_copy(
    text: &str,
    revision: u64,
) -> super::continuation_context::GuardedPasteOutcome {
    use super::continuation_context::GuardedPasteOutcome;
    #[cfg(target_os = "macos")]
    {
        let Ok(mut clipboard) = Clipboard::new() else {
            return GuardedPasteOutcome::Unavailable;
        };
        use super::continuation_context::TextClipboard;
        let Some(previous) = clipboard.revision() else {
            return GuardedPasteOutcome::Unavailable;
        };
        return if clipboard.write(text, previous).is_some() {
            GuardedPasteOutcome::Confirmed { revision }
        } else {
            GuardedPasteOutcome::Uncertain
        };
    }
    #[cfg(not(target_os = "macos"))]
    {
        let Ok(mut clipboard) = SystemClipboard::new() else {
            return GuardedPasteOutcome::Unavailable;
        };
        publish_continuation_copy(&mut clipboard, text, revision)
    }
}

// Separate from legacy retries and transcript diagnostics.
#[cfg(not(target_os = "macos"))]
pub(crate) struct ContinuationNativeContext;
#[cfg(not(target_os = "macos"))]
impl ContinuationNativeContext {
    pub(crate) fn capture(_: AutoPasteTarget) -> Result<Self> {
        anyhow::bail!("native context unavailable")
    }
    pub(crate) fn validate(&mut self) -> crate::domain::ports::ContextValidation {
        crate::domain::ports::ContextValidation::Unavailable
    }
    pub(crate) fn paste(&mut self, _: &str) -> super::continuation_context::GuardedPasteOutcome {
        super::continuation_context::GuardedPasteOutcome::Unavailable
    }
}
#[cfg(target_os = "macos")]
pub(crate) use continuation_native::ContinuationNativeContext;
#[cfg(all(target_os = "macos", debug_assertions, feature = "native-window-e2e"))]
pub use continuation_native::SyntheticTextEditReader;
#[cfg(all(
    debug_assertions,
    feature = "native-window-e2e",
    any(target_os = "macos", test)
))]
pub(crate) mod synthetic_readiness {
    use serde_json::{json, Value};
    use std::time::{Duration, Instant};
    #[derive(Debug)]
    struct Stop(&'static str);
    impl std::fmt::Display for Stop {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.0)
        }
    }
    impl std::error::Error for Stop {}
    fn stop(reason: &'static str) -> anyhow::Error {
        Stop(reason).into()
    }
    #[derive(Debug, Clone)]
    pub struct OperationError {
        pub initial_focus: bool,
        pub bind: bool,
        pub sequence: u64,
        pub op: &'static str,
        pub called: bool,
        pub code: Option<i32>,
        pub expired: bool,
        pub null: bool,
        pub wrong_type: bool,
        pub outcome: &'static str,
        pub evidence: Value,
    }
    impl std::fmt::Display for OperationError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}: {} code={:?}", self.op, self.outcome, self.code)
        }
    }
    impl std::error::Error for OperationError {}
    impl OperationError {
        fn rejected_late_success(&self) -> bool {
            self.called
                && matches!(self.code, None | Some(0))
                && self.expired
                && !self.null
                && !self.wrong_type
                && self.outcome == "local-deadline"
        }
        fn rejected_late_focus(&self) -> bool {
            self.op == "AXFocusedUIElement" && self.code == Some(0) && self.rejected_late_success()
        }
        fn expired_before_call(&self) -> bool {
            !self.called
                && self.code.is_none()
                && self.expired
                && !self.null
                && !self.wrong_type
                && self.outcome == "local-deadline"
        }
        fn transient_process_lookup(&self) -> bool {
            self.op == "NSRunningApplicationBundleIdentifier"
                && self.called
                && self.code.is_none()
                && self.null
                && !self.wrong_type
                && self.outcome == "success-null"
        }
        // This result is rejected and dropped, never used as an acquired element.
        pub fn rejected_late_initial_focus(&self) -> bool {
            self.rejected_late_focus() && self.initial_focus && self.bind && self.sequence == 0
        }
        pub fn retryable(&self) -> bool {
            self.transient_process_lookup()
                || (self.initial_focus
                    && self.bind
                    && self.sequence == 0
                    && self.op == "AXFocusedUIElement"
                    && self.called
                    && self.code == Some(-25204)
                    && self.outcome == "native-error"
                    && self.null
                    && !self.wrong_type)
        }
        pub fn retryable_sample(&self) -> bool {
            self.transient_process_lookup()
                || (self.rejected_late_success() && !self.bind && self.sequence > 0)
                || (self.expired_before_call() && !self.bind && self.sequence > 0)
                || (!self.initial_focus
                    && !self.bind
                    && self.sequence > 0
                    && matches!(
                        self.op,
                        "AXFocusedUIElement" | "AXWindow" | "AXDocument" | "AXValue"
                    )
                    && self.called
                    && self.code == Some(-25204)
                    && self.outcome == "native-error"
                    && self.null
                    && !self.expired
                    && !self.wrong_type)
        }
    }
    pub fn admit(
        now: Instant,
        deadline: Instant,
        stopped: bool,
        reserve_ms: u64,
    ) -> Result<(), &'static str> {
        if stopped {
            Err("cancelled")
        } else if now >= deadline {
            Err("arm-deadline")
        } else if now + Duration::from_millis(reserve_ms) >= deadline {
            Err("insufficient-reserve")
        } else {
            Ok(())
        }
    }
    pub fn reset_attempt(d: &mut Value, attempt: usize, start: f64) {
        d["attempt"] = json!(attempt);
        d["stage"] = json!("target");
        d["phase"] = json!("bind");
        d["phaseStartMs"] = json!(start);
        d["bindMetadataStartMs"] = Value::Null;
        d["bindMetadata"] = Value::Null;
        d["bindTargetLookup"] = Value::Null;
        d["pendingOperation"] = Value::Null;
        d["rawCallMs"] = Value::Null;
    }
    pub fn attempt_snapshot(d: &Value) -> Value {
        json!({"target":d["bindTargetLookup"],"foreground":d["bindMetadata"]})
    }
    #[test]
    fn attempt_reset_keeps_only_immutable_prior_observations() {
        let mut d = json!({});
        reset_attempt(&mut d, 1, 0.0);
        d["bindMetadata"] = json!({"pid":42});
        d["bindTargetLookup"] = json!({"pid":42});
        let history = attempt_snapshot(&d);
        for reason in ["cancelled", "arm-deadline", "metadata-trust"] {
            reset_attempt(&mut d, 2, 50.0);
            // Failure before metadata/target replacement uses the real snapshot.
            let failed = attempt_snapshot(&d);
            assert!(failed["foreground"].is_null(), "{reason}");
            assert!(failed["target"].is_null(), "{reason}");
            assert!(d["bindMetadataStartMs"].is_null());
            assert_eq!(history["target"]["pid"], 42);
            assert_eq!(history["foreground"]["pid"], 42);
        }
    }
    // A missing process lookup may be corroborated once by fresh foreground state.
    // A present but wrong bundle is positive mismatch evidence and cannot fall back.
    pub fn validate_process_identity(
        bundle: Option<&str>,
        pid: i32,
        expected: &str,
        deadline: Instant,
        now: impl Fn() -> Instant,
        cancelled: impl Fn() -> bool,
        fallback: impl FnOnce(Instant) -> anyhow::Result<Option<(i32, String)>>,
        mut observe: impl FnMut(bool, bool, &'static str),
    ) -> anyhow::Result<()> {
        if let Some(bundle) = bundle {
            let matches = bundle == expected;
            observe(false, false, if matches { "matched" } else { "mismatch" });
            anyhow::ensure!(matches, "owned PID/bundle changed");
            return Ok(());
        }
        observe(true, false, "missing");
        admit(now(), deadline, cancelled(), 0).map_err(anyhow::Error::msg)?;
        observe(true, true, "pending");
        let result = fallback(deadline).and_then(|observed| {
            admit(now(), deadline, cancelled(), 0).map_err(anyhow::Error::msg)?;
            validate_foreground(
                pid,
                expected,
                observed
                    .as_ref()
                    .map(|(pid, bundle)| (*pid, bundle.as_str())),
            )
        });
        observe(
            true,
            true,
            if result.is_ok() {
                "matched"
            } else {
                "rejected"
            },
        );
        result
    }
    pub fn validate_foreground(
        pid: i32,
        bundle: &str,
        observed: Option<(i32, &str)>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(observed.is_some(), "foreground unavailable");
        anyhow::ensure!(observed == Some((pid, bundle)), "owned foreground changed");
        Ok(())
    }
    pub fn pin(
        owner: &mut Option<(i32, String, std::path::PathBuf)>,
        resolved: (i32, String, std::path::PathBuf),
    ) -> anyhow::Result<()> {
        if let Some(owner) = owner.as_ref() {
            anyhow::ensure!(*owner == resolved, "owned target changed");
        } else {
            *owner = Some(resolved);
        }
        Ok(())
    }
    // The worker and scripted tests execute this same serial controller. Evidence
    // is separate from rolling AX diagnostics, so enrichment cannot replace it.
    const BIND_ATTEMPT_LIMIT: usize = 8;
    pub fn bind<T>(
        deadline: Instant,
        now: impl Fn() -> Instant,
        stopped: impl Fn() -> bool,
        mut wait: impl FnMut(Duration),
        mut attempt: impl FnMut(usize) -> anyhow::Result<T>,
        mut snapshot: impl FnMut() -> Value,
        mut publish: impl FnMut(Value),
    ) -> anyhow::Result<T> {
        let origin = now();
        let ms = |t: Instant| t.saturating_duration_since(origin).as_secs_f64() * 1000.0;
        let mut evidence = json!({"attempts":[],"firstTransient":null,"lastTransient":null,
            "firstNonTransient":null,"terminalCause":null,"stopReason":null,"recoveredDuringBind":false,
            "deadlineFromWorkerMs":ms(deadline)});
        let result = (|| {
            for number in 1..=BIND_ATTEMPT_LIMIT {
                admit(now(), deadline, stopped(), 450).map_err(stop)?;
                let rows = evidence["attempts"].as_array_mut().unwrap();
                if rows.len() >= BIND_ATTEMPT_LIMIT {
                    return Err(stop("evidence-overflow"));
                }
                rows.push(json!({"attempt":number,"status":"pending","startMs":ms(now())}));
                publish(evidence.clone());
                admit(now(), deadline, stopped(), 450).map_err(stop)?;
                let start = now();
                let result = attempt(number);
                let error = result.as_ref().err();
                let typed = error.and_then(|e| e.downcast_ref::<OperationError>());
                let late_initial_focus =
                    typed.is_some_and(OperationError::rejected_late_initial_focus);
                let transient = typed.is_some_and(OperationError::retryable) || late_initial_focus;
                let cause = typed
                    .map(|e| e.evidence.clone())
                    .unwrap_or_else(|| json!({"classification":"unknown"}));
                let row = &mut evidence["attempts"][number - 1];
                row["status"] = json!("completed");
                row["endMs"] = json!(ms(now()));
                row["remainingMs"] =
                    json!(deadline.saturating_duration_since(now()).as_secs_f64() * 1000.0);
                row["metadata"] = snapshot();
                row["failure"] = if error.is_some() {
                    cause.clone()
                } else {
                    Value::Null
                };
                row["retryDecision"] = json!("stop");
                if error.is_some() {
                    let key = if transient {
                        "firstTransient"
                    } else {
                        "firstNonTransient"
                    };
                    if evidence[key].is_null() {
                        evidence[key] = cause.clone();
                    }
                    if transient {
                        evidence["lastTransient"] = cause.clone();
                    }
                    evidence["terminalCause"] = cause;
                }
                publish(evidence.clone()); // Returned errors survive cancellation/overrun.
                admit(now(), deadline, stopped(), 0).map_err(stop)?;
                if now().duration_since(start) >= Duration::from_millis(200) && !transient {
                    return Err(stop("bind-deadline"));
                }
                match result {
                    Ok(reader) => {
                        admit(now(), deadline, stopped(), 250).map_err(stop)?;
                        evidence["attempts"][number - 1]["retryDecision"] = json!("bound");
                        evidence["terminalCause"] = Value::Null;
                        evidence["recoveredDuringBind"] = json!(number > 1);
                        return Ok(reader);
                    }
                    Err(error) if !transient => return Err(error),
                    Err(_) => {}
                }
                if number == BIND_ATTEMPT_LIMIT {
                    return Err(stop("attempt-limit"));
                }
                admit(now(), deadline, stopped(), 500).map_err(stop)?;
                let began = now();
                evidence["attempts"][number - 1]["retryDecision"] = json!("retry");
                evidence["attempts"][number - 1]["backoffMs"] = json!([ms(began), null]);
                publish(evidence.clone());
                let wake = began + Duration::from_millis(50);
                while now() < wake {
                    admit(now(), deadline, stopped(), 0).map_err(stop)?;
                    wait(
                        wake.saturating_duration_since(now())
                            .min(Duration::from_millis(5)),
                    );
                }
                evidence["attempts"][number - 1]["backoffMs"] = json!([ms(began), ms(now())]);
            }
            unreachable!()
        })();
        if let Err(error) = &result {
            // Only fixed policy labels enter retry evidence; arbitrary error text stays out.
            let reason = error
                .downcast_ref::<Stop>()
                .map_or("terminal-failure", |s| s.0);
            evidence["stopReason"] = json!(reason);
        }
        publish(evidence);
        result
    }
    // A transient AX server refusal invalidates the whole sample. Retry the full
    // identity/value/identity transaction without rebinding or publishing text.
    pub fn sample<T>(
        deadline: Instant,
        now: impl Fn() -> Instant,
        consecutive_skips: &std::cell::Cell<u8>,
        mut wait: impl FnMut(Duration),
        mut attempt: impl FnMut() -> anyhow::Result<T>,
    ) -> anyhow::Result<Option<T>> {
        for number in 1..=3 {
            admit(now(), deadline, false, 0).map_err(stop)?;
            match attempt() {
                Ok(value) => {
                    consecutive_skips.set(0);
                    return Ok(Some(value));
                }
                Err(error) => {
                    let typed = error.downcast_ref::<OperationError>();
                    let retryable = typed.is_some_and(OperationError::retryable_sample);
                    if !retryable {
                        return Err(error);
                    }
                    let rejected_late_success = typed.is_some_and(|error| {
                        error.rejected_late_success() || error.expired_before_call()
                    });
                    if number == 3 || (rejected_late_success && now() >= deadline) {
                        let skips = consecutive_skips.get().saturating_add(1);
                        if skips > 2 {
                            return Err(error);
                        }
                        consecutive_skips.set(skips);
                        return Ok(None);
                    }
                }
            }
            admit(now(), deadline, false, 5).map_err(stop)?;
            wait(Duration::from_millis(5));
        }
        unreachable!()
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        use std::cell::{Cell, RefCell};
        fn transient(sequence: u64) -> OperationError {
            OperationError {
                initial_focus: true,
                bind: true,
                sequence,
                op: "AXFocusedUIElement",
                called: true,
                code: Some(-25204),
                expired: false,
                null: true,
                wrong_type: false,
                outcome: "native-error",
                evidence: json!({"code":-25204,"readSequence":sequence}),
            }
        }
        // No sleeps or native calls: scripts drive the production controller.
        fn run(
            script: Vec<anyhow::Result<()>>,
            budget: u64,
            duration: u64,
            oversleep: u64,
            cancel_at: Option<u64>,
        ) -> (bool, usize, usize, Value) {
            let origin = Instant::now();
            let elapsed = Cell::new(0u64);
            let attempts = Cell::new(0);
            let waits = Cell::new(0);
            let evidence = RefCell::new(Value::Null);
            let mut script = script.into_iter();
            let result = bind(
                origin + Duration::from_millis(budget),
                || origin + Duration::from_millis(elapsed.get()),
                || cancel_at.is_some_and(|t| elapsed.get() >= t),
                |d| {
                    assert!(d <= Duration::from_millis(5));
                    waits.set(waits.get() + 1);
                    elapsed.set(elapsed.get() + d.as_millis() as u64 + oversleep);
                },
                |_| {
                    attempts.set(attempts.get() + 1);
                    elapsed.set(elapsed.get() + duration);
                    script.next().unwrap()
                },
                || json!({"pid":42,"bundle":"com.apple.TextEdit"}),
                |d| *evidence.borrow_mut() = d,
            );
            (
                result.is_ok(),
                attempts.get(),
                waits.get(),
                evidence.into_inner(),
            )
        }
        #[test]
        fn recovery_and_exhaustion_keep_history_without_fatal_state() {
            for failures in 0..=BIND_ATTEMPT_LIMIT {
                let mut script: Vec<_> = (0..failures)
                    .map(|i| {
                        let mut e = transient(0);
                        e.evidence["attempt"] = json!(i + 1);
                        Err(e.into())
                    })
                    .collect();
                if failures < BIND_ATTEMPT_LIMIT {
                    script.push(Ok(()));
                }
                let (ok, attempts, waits, d) = run(script, 2000, 0, 0, None);
                assert_eq!(ok, failures < BIND_ATTEMPT_LIMIT);
                assert_eq!(attempts, (failures + 1).min(BIND_ATTEMPT_LIMIT));
                assert_eq!(waits, failures.min(BIND_ATTEMPT_LIMIT - 1) * 10);
                assert_eq!(
                    d["recoveredDuringBind"],
                    failures > 0 && failures < BIND_ATTEMPT_LIMIT
                );
                assert!(d["firstNonTransient"].is_null());
                assert!(d.get("firstFatal").is_none());
                assert!(d.get("valid").is_none());
                assert!(d.get("error").is_none());
                if failures > 0 {
                    assert_eq!(d["firstTransient"]["attempt"], 1);
                    assert_eq!(d["lastTransient"]["attempt"], failures);
                }
                if failures == BIND_ATTEMPT_LIMIT {
                    assert_eq!(d["stopReason"], "attempt-limit");
                    assert_eq!(d["terminalCause"]["code"], -25204);
                }
            }
        }
        #[test]
        fn only_current_typed_initial_acquisition_can_retry() {
            let mut expired_transient = transient(0);
            expired_transient.expired = true;
            assert!(expired_transient.retryable());
            let mut errors = Vec::new();
            for category in 0..10 {
                let mut e = transient(0);
                match category {
                    0 => e.initial_focus = false,
                    1 => e.bind = false,
                    2 => e.sequence = 1,
                    3 => e.op = "AXRole",
                    4 => e.called = false,
                    5 => e.code = Some(0),
                    6 => e.code = Some(-25205),
                    7 => e.null = false,
                    8 => e.wrong_type = true,
                    _ => e.outcome = "local-deadline",
                }
                errors.push(anyhow::Error::from(e));
            }
            errors.push(anyhow::anyhow!(
                "AXFocusedUIElement: native-error code=Some(-25204) TEXT_SENTINEL"
            ));
            for error in errors {
                // The preceding transient supplies a misleading stale row.
                let (ok, count, _, d) =
                    run(vec![Err(transient(0).into()), Err(error)], 2000, 0, 0, None);
                assert!(!ok);
                assert_eq!(count, 2);
                assert!(!d["firstNonTransient"].is_null());
                assert_eq!(d["firstTransient"]["code"], -25204);
                assert!(!d.to_string().contains("TEXT_SENTINEL"));
            }
        }
        #[test]
        fn ongoing_sample_retries_only_transient_attribute_refusal() {
            let origin = Instant::now();
            let elapsed = Cell::new(0u64);
            let calls = Cell::new(0usize);
            let skips = Cell::new(0u8);
            let result = sample(
                origin + Duration::from_millis(200),
                || origin + Duration::from_millis(elapsed.get()),
                &skips,
                |duration| elapsed.set(elapsed.get() + duration.as_millis() as u64),
                || {
                    calls.set(calls.get() + 1);
                    if calls.get() == 1 {
                        let mut error = transient(7);
                        error.initial_focus = false;
                        error.bind = false;
                        Err(error.into())
                    } else {
                        Ok("fresh")
                    }
                },
            );
            assert_eq!(result.unwrap(), Some("fresh"));
            assert_eq!(calls.get(), 2);

            for mutate in 0..8 {
                let calls = Cell::new(0usize);
                let mut error = transient(7);
                error.initial_focus = false;
                error.bind = false;
                match mutate {
                    0 => error.initial_focus = true,
                    1 => error.bind = true,
                    2 => error.sequence = 0,
                    3 => error.op = "AXRole",
                    4 => error.called = false,
                    5 => error.code = Some(-25205),
                    6 => error.expired = true,
                    _ => error.wrong_type = true,
                }
                let result = sample(
                    origin + Duration::from_millis(200),
                    || origin,
                    &skips,
                    |_| panic!("non-transient error must not wait"),
                    || {
                        calls.set(calls.get() + 1);
                        Err::<(), _>(error.clone().into())
                    },
                );
                assert!(result.is_err());
                assert_eq!(calls.get(), 1);
            }
            for op in ["AXFocusedUIElement", "AXWindow", "AXDocument", "AXValue"] {
                let mut error = transient(7);
                error.initial_focus = false;
                error.bind = false;
                error.op = op;
                assert!(error.retryable_sample(), "{op}");
            }
        }
        #[test]
        fn late_successful_focus_is_discarded_as_a_bounded_sample_skip() {
            let origin = Instant::now();
            let elapsed = Cell::new(0u64);
            let skips = Cell::new(0u8);
            let calls = Cell::new(0usize);
            for expected in 1..=2 {
                let result = sample(
                    origin + Duration::from_millis(200),
                    || origin + Duration::from_millis(elapsed.get()),
                    &skips,
                    |_| panic!("expired late success must not wait"),
                    || {
                        calls.set(calls.get() + 1);
                        elapsed.set(250);
                        let mut error = late_focus();
                        error.initial_focus = false;
                        error.bind = false;
                        error.sequence = expected;
                        Err::<(), _>(error.into())
                    },
                );
                assert_eq!(result.unwrap(), None);
                assert_eq!(skips.get(), expected as u8);
                elapsed.set(0);
            }
            let result = sample(
                origin + Duration::from_millis(200),
                || origin + Duration::from_millis(elapsed.get()),
                &skips,
                |_| panic!("expired late success must not wait"),
                || {
                    elapsed.set(250);
                    let mut error = late_focus();
                    error.initial_focus = false;
                    error.bind = false;
                    error.sequence = 3;
                    Err::<(), _>(error.into())
                },
            );
            assert!(result
                .unwrap_err()
                .downcast_ref::<OperationError>()
                .is_some());
            assert_eq!(calls.get(), 2);
        }
        #[test]
        fn process_identity_lookup_nil_is_the_only_retryable_lookup_outcome() {
            let mut lookup = transient(7);
            lookup.initial_focus = false;
            lookup.bind = false;
            lookup.op = "NSRunningApplicationBundleIdentifier";
            lookup.code = None;
            lookup.null = true;
            lookup.outcome = "success-null";
            assert!(lookup.retryable());
            assert!(lookup.retryable_sample());
            for mutate in 0..5 {
                let mut error = lookup.clone();
                match mutate {
                    0 => error.op = "AXValue",
                    1 => error.called = false,
                    2 => error.code = Some(-25204),
                    3 => error.null = false,
                    _ => error.outcome = "native-error",
                }
                assert!(!error.retryable());
                assert!(!error.retryable_sample());
            }
        }
        #[test]
        fn late_successful_process_lookup_discards_the_whole_sample() {
            let origin = Instant::now();
            let elapsed = Cell::new(0u64);
            let skips = Cell::new(0u8);
            let result = sample(
                origin + Duration::from_millis(200),
                || origin + Duration::from_millis(elapsed.get()),
                &skips,
                |_| panic!("expired late process lookup must not wait"),
                || {
                    elapsed.set(250);
                    let mut error = late_focus();
                    error.initial_focus = false;
                    error.bind = false;
                    error.sequence = 7;
                    error.op = "NSRunningApplicationBundleIdentifier";
                    error.code = None;
                    Err::<(), _>(error.into())
                },
            );
            assert_eq!(result.unwrap(), None);
            assert_eq!(skips.get(), 1);
        }
        #[test]
        fn scheduler_overrun_before_native_call_discards_the_whole_sample() {
            let origin = Instant::now();
            let elapsed = Cell::new(0u64);
            let skips = Cell::new(0u8);
            let result = sample(
                origin + Duration::from_millis(200),
                || origin + Duration::from_millis(elapsed.get()),
                &skips,
                |_| panic!("expired sample must not wait"),
                || {
                    elapsed.set(250);
                    let mut error = late_focus();
                    error.initial_focus = false;
                    error.bind = false;
                    error.sequence = 8;
                    error.op = "AXUIElementGetPid";
                    error.called = false;
                    error.code = None;
                    Err::<(), _>(error.into())
                },
            );
            assert_eq!(result.unwrap(), None);
            assert_eq!(skips.get(), 1);
        }
        #[test]
        fn ongoing_sample_keeps_one_deadline_and_stops_after_three_attempts() {
            let origin = Instant::now();
            let elapsed = Cell::new(0u64);
            let calls = Cell::new(0usize);
            let skips = Cell::new(0u8);
            for expected in 1..=2 {
                let result = sample(
                    origin + Duration::from_millis(200),
                    || origin + Duration::from_millis(elapsed.get()),
                    &skips,
                    |duration| elapsed.set(elapsed.get() + duration.as_millis() as u64),
                    || {
                        calls.set(calls.get() + 1);
                        let mut error = transient(9);
                        error.initial_focus = false;
                        error.bind = false;
                        Err::<(), _>(error.into())
                    },
                );
                assert_eq!(result.unwrap(), None);
                assert_eq!(skips.get(), expected);
            }
            let result = sample(
                origin + Duration::from_millis(200),
                || origin + Duration::from_millis(elapsed.get()),
                &skips,
                |duration| elapsed.set(elapsed.get() + duration.as_millis() as u64),
                || {
                    calls.set(calls.get() + 1);
                    let mut error = transient(9);
                    error.initial_focus = false;
                    error.bind = false;
                    Err::<(), _>(error.into())
                },
            );
            assert!(result
                .unwrap_err()
                .downcast_ref::<OperationError>()
                .is_some());
            assert_eq!(calls.get(), 9);
            assert_eq!(elapsed.get(), 30);

            elapsed.set(196);
            calls.set(0);
            skips.set(0);
            let result = sample(
                origin + Duration::from_millis(200),
                || origin + Duration::from_millis(elapsed.get()),
                &skips,
                |_| panic!("insufficient reserve must not wait"),
                || {
                    calls.set(calls.get() + 1);
                    let mut error = transient(10);
                    error.initial_focus = false;
                    error.bind = false;
                    Err::<(), _>(error.into())
                },
            );
            assert_eq!(result.unwrap_err().to_string(), "insufficient-reserve");
            assert_eq!(calls.get(), 1);
        }
        #[test]
        fn missing_bundle_fallback_is_fresh_once_and_does_not_hide_mismatch() {
            let origin = Instant::now();
            let deadline = origin + Duration::from_millis(200);
            for bundle in [Some("com.apple.TextEdit"), Some("other"), Some(""), None] {
                for foreground in [
                    None,
                    Some((42, "com.apple.TextEdit".to_owned())),
                    Some((43, "com.apple.TextEdit".to_owned())),
                    Some((42, "other".to_owned())),
                ] {
                    let calls = Cell::new(0);
                    let events = RefCell::new(Vec::new());
                    let result = validate_process_identity(
                        bundle,
                        42,
                        "com.apple.TextEdit",
                        deadline,
                        || origin,
                        || false,
                        |received_deadline| {
                            assert_eq!(received_deadline, deadline);
                            calls.set(calls.get() + 1);
                            events.borrow_mut().push("foreground");
                            Ok(foreground.clone())
                        },
                        |missing, attempted, status| {
                            if status == "pending" {
                                assert!(missing && attempted);
                            }
                            events.borrow_mut().push(status);
                        },
                    );
                    let expected = bundle == Some("com.apple.TextEdit")
                        || (bundle.is_none()
                            && foreground == Some((42, "com.apple.TextEdit".to_owned())));
                    assert_eq!(result.is_ok(), expected);
                    assert_eq!(calls.get(), usize::from(bundle.is_none()));
                    if bundle.is_none() {
                        assert_eq!(&events.borrow()[..3], &["missing", "pending", "foreground"]);
                    }
                }
            }
        }
        #[test]
        fn missing_bundle_fallback_keeps_original_cutoff_and_cancellation() {
            let origin = Instant::now();
            let deadline = origin + Duration::from_millis(200);
            for offset in [199, 200, 201] {
                for delay_inside_fallback in [false, true] {
                    for cancelled in [false, true] {
                        let now = Cell::new(if delay_inside_fallback {
                            origin
                        } else {
                            origin + Duration::from_millis(offset)
                        });
                        let calls = Cell::new(0);
                        let result = validate_process_identity(
                            None,
                            42,
                            "com.apple.TextEdit",
                            deadline,
                            || now.get(),
                            || cancelled,
                            |received_deadline| {
                                assert_eq!(received_deadline, deadline);
                                calls.set(calls.get() + 1);
                                now.set(origin + Duration::from_millis(offset));
                                Ok(Some((42, "com.apple.TextEdit".into())))
                            },
                            |_, _, _| {},
                        );
                        assert_eq!(result.is_ok(), offset == 199 && !cancelled);
                        assert_eq!(
                            calls.get(),
                            usize::from(!cancelled && (delay_inside_fallback || offset < 200))
                        );
                    }
                }
            }
            let cancelled = Cell::new(false);
            let result = validate_process_identity(
                None,
                42,
                "com.apple.TextEdit",
                deadline,
                || origin,
                || cancelled.get(),
                |_| {
                    cancelled.set(true);
                    Ok(Some((42, "com.apple.TextEdit".into())))
                },
                |_, _, _| {},
            );
            assert_eq!(result.unwrap_err().to_string(), "cancelled");
        }
        #[test]
        fn successful_missing_bundle_fallback_preserves_later_ax_failure() {
            let origin = Instant::now();
            let calls = Cell::new(0);
            let result = validate_process_identity(
                None,
                42,
                "com.apple.TextEdit",
                origin + Duration::from_millis(200),
                || origin,
                || false,
                |_| {
                    calls.set(calls.get() + 1);
                    Ok(Some((42, "com.apple.TextEdit".into())))
                },
                |_, _, _| {},
            )
            .and_then(|_| Err::<(), _>(anyhow::anyhow!("owned editor/window changed")));
            assert_eq!(calls.get(), 1);
            assert_eq!(
                result.unwrap_err().to_string(),
                "owned editor/window changed"
            );
        }
        fn late_focus() -> OperationError {
            let mut error = transient(0);
            error.code = Some(0);
            error.expired = true;
            error.null = false;
            error.outcome = "local-deadline";
            error.evidence = json!({"code":0,"outcome":"local-deadline"});
            error
        }
        #[test]
        fn late_focus_retry_excludes_every_neighboring_category() {
            assert!(late_focus().rejected_late_initial_focus());
            for category in 0..11 {
                let mut error = late_focus();
                match category {
                    0 => error.initial_focus = false,
                    1 => error.bind = false,
                    2 => error.sequence = 1,
                    3 => error.op = "AXValue",
                    4 => error.called = false,
                    5 => error.code = Some(-25204),
                    6 => error.expired = false,
                    7 => error.null = true,
                    8 => error.wrong_type = true,
                    9 => error.outcome = "success-null",
                    _ => error.code = None,
                }
                assert!(!error.rejected_late_initial_focus(), "category {category}");
            }
        }
        #[test]
        fn late_focus_is_discarded_then_fresh_attempt_can_bind() {
            for first_ms in [199, 200, 201] {
                let origin = Instant::now();
                let now = Cell::new(origin);
                let calls = Cell::new(0);
                let metadata = Cell::new(0);
                let mut evidence = Value::Null;
                let result = bind(
                    origin + Duration::from_secs(2),
                    || now.get(),
                    || false,
                    |delay| now.set(now.get() + delay),
                    |attempt| {
                        calls.set(calls.get() + 1);
                        metadata.set(attempt);
                        if attempt == 1 {
                            now.set(now.get() + Duration::from_millis(first_ms));
                            let mut error = late_focus();
                            error.expired = first_ms >= 200;
                            return Err(error.into());
                        }
                        Ok("fresh identity")
                    },
                    || json!({"freshAttempt":metadata.get()}),
                    |next| evidence = next,
                );
                assert_eq!(result.is_ok(), first_ms >= 200);
                assert_eq!(calls.get(), if first_ms >= 200 { 2 } else { 1 });
                // Rejected acquisition never escapes as a reader or armed state.
                assert!(evidence.get("armed").is_none());
                assert!(evidence.get("valid").is_none());
                if first_ms >= 200 {
                    assert_eq!(result.unwrap(), "fresh identity");
                    assert_eq!(evidence["attempts"][1]["metadata"]["freshAttempt"], 2);
                }
            }
        }
        #[test]
        fn late_focus_retains_cancel_shared_deadline_attempt_limit_and_history() {
            for (budget, duration, cancel) in
                [(2000, 200, Some(200)), (2000, 2000, None), (500, 200, None)]
            {
                let (ok, count, _, evidence) =
                    run(vec![Err(late_focus().into())], budget, duration, 0, cancel);
                assert!(!ok);
                assert_eq!(count, 1);
                assert_eq!(evidence["lastTransient"]["code"], 0);
            }
            let (ok, count, _, evidence) = run(
                (0..BIND_ATTEMPT_LIMIT)
                    .map(|_| Err(late_focus().into()))
                    .collect(),
                2000,
                0,
                0,
                None,
            );
            assert!(!ok);
            assert_eq!(count, BIND_ATTEMPT_LIMIT);
            assert_eq!(evidence["stopReason"], "attempt-limit");
            let (ok, count, _, evidence) = run(
                vec![Err(transient(0).into()), Err(late_focus().into()), Ok(())],
                2000,
                0,
                0,
                None,
            );
            assert!(ok);
            assert_eq!(count, 3);
            assert_eq!(evidence["firstTransient"]["code"], -25204);
            assert_eq!(evidence["lastTransient"]["code"], 0);
        }
        #[test]
        fn final_foreground_must_still_match_pinned_owner() {
            assert!(validate_foreground(
                42,
                "com.apple.TextEdit",
                Some((42, "com.apple.TextEdit"))
            )
            .is_ok());
            for observed in [None, Some((43, "com.apple.TextEdit")), Some((42, "other"))] {
                assert!(validate_foreground(42, "com.apple.TextEdit", observed).is_err());
            }
        }
        #[test]
        fn strict_reserves_overrun_oversleep_and_cancellation() {
            let origin = Instant::now();
            for reserve in [0, 250, 450, 500] {
                let cutoff = origin + Duration::from_millis(reserve);
                assert!(admit(origin, cutoff, false, reserve).is_err());
                assert!(admit(origin, cutoff + Duration::from_nanos(1), false, reserve).is_ok());
                assert!(admit(origin, cutoff + Duration::from_secs(2), true, reserve).is_err());
            }
            for budget in [0, 450] {
                assert_eq!(run(vec![], budget, 0, 0, None).1, 0);
            }
            assert!(run(vec![Ok(())], 451, 0, 0, None).0);
            for duration in [200, 2001] {
                assert!(!run(vec![Ok(())], 2000, duration, 0, None).0);
            }
            for (budget, oversleep, cancel, count) in [
                (500, 0, None, 1),
                (550, 100, None, 1),
                (2000, 0, Some(0), 0),
                (2000, 0, Some(5), 1),
                (2000, 0, Some(1), 1),
            ] {
                let (ok, actual, _, d) =
                    run(vec![Err(transient(0).into())], budget, 1, oversleep, cancel);
                assert!(!ok);
                assert_eq!(actual, count);
                if count > 0 {
                    assert_eq!(d["lastTransient"]["code"], -25204);
                }
            }
        }
        #[test]
        fn owner_survives_failed_attempt_and_rejects_pid_bundle_or_path_change() {
            let original: (i32, String, std::path::PathBuf) =
                (42, "com.apple.TextEdit".to_owned(), "/owned".into());
            for changed in [
                (43, original.1.clone(), original.2.clone()),
                (42, "other".into(), original.2.clone()),
                (42, original.1.clone(), "/other".into()),
            ] {
                let mut owner = None;
                let mut calls = 0;
                let origin = Instant::now();
                let now = Cell::new(origin);
                let mut evidence = Value::Null;
                let result = bind(
                    origin + Duration::from_secs(2),
                    || now.get(),
                    || false,
                    |d| now.set(now.get() + d),
                    |_| {
                        calls += 1;
                        pin(
                            &mut owner,
                            if calls == 1 {
                                original.clone()
                            } else {
                                changed.clone()
                            },
                        )?;
                        Err::<(), _>(transient(0).into())
                    },
                    || Value::Null,
                    |d| evidence = d,
                );
                assert!(result.is_err());
                assert_eq!(calls, 2);
                assert_eq!(owner, Some(original.clone()));
                assert!(!evidence["firstNonTransient"].is_null());
            }
        }
    }
}
#[cfg(target_os = "macos")]
mod continuation_native {
    use super::*;
    use crate::domain::ports::ContextValidation;
    use crate::infrastructure::continuation_context::GuardedPasteOutcome;
    use objc::{class, msg_send, sel, sel_impl};
    // Actual item types, not board.types (which synthesizes legacy aliases).
    pub(crate) type ClipboardSnapshot = Vec<Vec<(Vec<u16>, Vec<u8>)>>;
    struct CocoaOwned(cocoa::base::id);
    impl Drop for CocoaOwned {
        fn drop(&mut self) {
            unsafe {
                let _: () = msg_send![self.0, release];
            }
        }
    }
    struct PrivateBoard(cocoa::base::id);
    impl PrivateBoard {
        fn revision(&self) -> Option<i64> {
            if self.0.is_null() {
                return None;
            }
            Some(unsafe {
                let n: isize = msg_send![self.0, changeCount];
                n as i64
            })
        }
        fn read(&self) -> Option<ClipboardSnapshot> {
            unsafe {
                let _pool = CocoaOwned(msg_send![class!(NSAutoreleasePool), new]);
                let before = self.revision()?;
                let items: cocoa::base::id = msg_send![self.0, pasteboardItems];
                let n: usize = msg_send![items, count];
                if n > 32 {
                    return None;
                }
                if n == 0 {
                    // A declared but unresolved board is not an empty clipboard.
                    let types: cocoa::base::id = msg_send![self.0, types];
                    let count: usize = msg_send![types, count];
                    if count != 0 {
                        return None;
                    }
                }
                let mut snapshot = Vec::new();
                let mut total = 0usize;
                for i in 0..n {
                    let item: cocoa::base::id = msg_send![items, objectAtIndex: i];
                    let types: cocoa::base::id = msg_send![item, types];
                    let m: usize = msg_send![types, count];
                    if !(1..=64).contains(&m) {
                        return None;
                    }
                    let mut representations = Vec::new();
                    for j in 0..m {
                        let kind: cocoa::base::id = msg_send![types, objectAtIndex: j];
                        let len: usize = msg_send![kind, length];
                        if len == 0 || len > 1024 {
                            return None;
                        }
                        let mut name = Vec::with_capacity(len);
                        for k in 0..len {
                            let unit: u16 = msg_send![kind, characterAtIndex: k];
                            name.push(unit);
                        }
                        let lower = String::from_utf16(&name).ok()?.to_ascii_lowercase();
                        if lower.contains("promise") {
                            return None;
                        }
                        // Eager resolution can block in Cocoa; executor abandonment still
                        // forbids any subsequent mutation through begin_effect.
                        let data: cocoa::base::id = msg_send![item, dataForType: kind];
                        if data.is_null() {
                            return None;
                        }
                        let len: usize = msg_send![data, length];
                        total = total.checked_add(len)?;
                        if len > 4 * 1024 * 1024 || total > 16 * 1024 * 1024 {
                            return None;
                        }
                        let bytes: *const u8 = msg_send![data, bytes];
                        if len != 0 && bytes.is_null() {
                            return None;
                        }
                        representations.push((
                            name,
                            if len == 0 {
                                Vec::new()
                            } else {
                                std::slice::from_raw_parts(bytes, len).to_vec()
                            },
                        ));
                    }
                    snapshot.push(representations);
                }
                (self.revision() == Some(before)).then_some(snapshot)
            }
        }
        fn publish(&self, snapshot: &ClipboardSnapshot, expected: i64) -> Option<i64> {
            unsafe {
                let _pool = CocoaOwned(msg_send![class!(NSAutoreleasePool), new]);
                let array = CocoaOwned(msg_send![class!(NSMutableArray), new]);
                if array.0.is_null() {
                    return None;
                }
                // Prepare all eager objects before clearContents; RAII covers refusal.
                for representations in snapshot {
                    let item = CocoaOwned(msg_send![class!(NSPasteboardItem), new]);
                    if item.0.is_null() {
                        return None;
                    }
                    for (name, bytes) in representations {
                        let kind: cocoa::base::id = msg_send![class!(NSString), alloc];
                        let kind = CocoaOwned(
                            msg_send![kind, initWithCharacters: name.as_ptr() length: name.len()],
                        );
                        let data: cocoa::base::id = msg_send![class!(NSData), dataWithBytes: bytes.as_ptr() length: bytes.len()];
                        if kind.0.is_null() || data.is_null() {
                            return None;
                        }
                        let ok: bool = msg_send![item.0, setData: data forType: kind.0];
                        if !ok {
                            return None;
                        }
                    }
                    let _: () = msg_send![array.0, addObject: item.0];
                }
                if self.revision() != Some(expected)
                    || !crate::infrastructure::continuation_context::begin_effect()
                {
                    return None;
                }
                // No NSPasteboard CAS exists. Never adopt a later observed count.
                let publication: isize = msg_send![self.0, clearContents];
                if self.revision() != Some(publication as i64)
                    || !crate::infrastructure::continuation_context::begin_effect()
                {
                    return None;
                }
                let ok: bool = snapshot.is_empty() || msg_send![self.0, writeObjects: array.0];
                (ok && self.revision() == Some(publication as i64)).then_some(publication as i64)
            }
        }
        fn write(&self, text: &str, expected: i64) -> Option<i64> {
            self.publish(
                &vec![vec![(
                    "public.utf8-plain-text".encode_utf16().collect(),
                    text.as_bytes().to_vec(),
                )]],
                expected,
            )
        }
    }
    fn general_board() -> PrivateBoard {
        PrivateBoard(unsafe { msg_send![class!(NSPasteboard), generalPasteboard] })
    }
    impl crate::infrastructure::continuation_context::TextClipboard for Clipboard {
        type Snapshot = ClipboardSnapshot;
        fn revision(&mut self) -> Option<i64> {
            general_board().revision()
        }
        fn read(&mut self) -> Option<Self::Snapshot> {
            general_board().read()
        }
        fn write(&mut self, text: &str, expected: i64) -> Option<i64> {
            general_board().write(text, expected)
        }
        fn restore(&mut self, snapshot: &Self::Snapshot, expected: i64) -> Option<i64> {
            general_board().publish(snapshot, expected)
        }
    }
    #[cfg(test)]
    mod named_board_tests {
        use super::*;
        use cocoa::foundation::NSString;
        fn with_board(test: impl FnOnce(&PrivateBoard)) {
            unsafe {
                let _pool = CocoaOwned(msg_send![class!(NSAutoreleasePool), new]);
                let raw: cocoa::base::id =
                    msg_send![class!(NSPasteboard), pasteboardWithUniqueName];
                assert!(!raw.is_null());
                struct Remove(cocoa::base::id);
                impl Drop for Remove {
                    fn drop(&mut self) {
                        unsafe {
                            let _: () = msg_send![self.0, releaseGlobally];
                        }
                    }
                }
                let _remove = Remove(raw);
                test(&PrivateBoard(raw));
            }
        }
        fn representation(name: &str, data: &[u8]) -> (Vec<u16>, Vec<u8>) {
            (name.encode_utf16().collect(), data.to_vec())
        }
        #[test]
        fn native_same_text_republication_inside_delivery_refuses_keys() {
            use crate::infrastructure::continuation_context::{
                clipboard_delivery, insert_with_current_clipboard, TextClipboard,
            };

            struct CountingClipboard<'a> {
                board: &'a PrivateBoard,
                writes: usize,
                restores: usize,
                publication: Option<i64>,
            }
            impl TextClipboard for CountingClipboard<'_> {
                type Snapshot = ClipboardSnapshot;
                fn revision(&mut self) -> Option<i64> {
                    self.board.revision()
                }
                fn read(&mut self) -> Option<Self::Snapshot> {
                    self.board.read()
                }
                fn write(&mut self, text: &str, expected: i64) -> Option<i64> {
                    self.writes += 1;
                    self.publication = self.board.write(text, expected);
                    self.publication
                }
                fn restore(&mut self, snapshot: &Self::Snapshot, expected: i64) -> Option<i64> {
                    self.restores += 1;
                    self.board.publish(snapshot, expected)
                }
            }

            let mut revisions = None;
            with_board(|board| {
                let original = vec![vec![
                    representation("public.utf8-plain-text", b"SYNTHETIC_ORIGINAL"),
                    representation("public.html", b"<b>SYNTHETIC_ORIGINAL</b>"),
                ]];
                let r0 = board.publish(&original, board.revision().unwrap()).unwrap();
                let saved = board.read().unwrap();
                assert_eq!(saved, original);
                assert_eq!(board.revision(), Some(r0));
                let transcript = String::from("SYNTHETIC_DICTATION");
                let transcript_before = transcript.clone();
                let expected = vec![vec![representation(
                    "public.utf8-plain-text",
                    transcript.as_bytes(),
                )]];
                let mut clipboard = CountingClipboard {
                    board,
                    writes: 0,
                    restores: 0,
                    publication: None,
                };
                let mut outer_callbacks = 0;
                let mut independent_publications = 0;
                let mut insertion_callbacks = 0;
                let mut newer = None;
                let outcome = clipboard_delivery(&mut clipboard, &transcript, true, |cb, r1| {
                    outer_callbacks += 1;
                    assert_eq!(cb.writes, 1);
                    assert_eq!(cb.publication, Some(r1));
                    assert!(r0 < r1);
                    assert_eq!(board.revision(), Some(r1));
                    let dictation = board.read().unwrap();
                    assert_eq!(dictation, expected);

                    // A separate native publication keeps every byte but changes ownership.
                    independent_publications += 1;
                    let r2 = board.publish(&dictation, r1).unwrap();
                    assert!(r1 < r2);
                    assert_eq!(board.revision(), Some(r2));
                    assert_eq!(board.read(), Some(dictation.clone()));
                    newer = Some((r2, dictation));
                    revisions = Some((r0, r1, r2));
                    insert_with_current_clipboard(cb, r1, || {
                        insertion_callbacks += 1;
                        GuardedPasteOutcome::Unavailable
                    })
                });
                assert_eq!(outcome, GuardedPasteOutcome::ContextMismatch);
                assert_eq!(outer_callbacks, 1);
                assert_eq!(clipboard.writes, 1);
                assert_eq!(independent_publications, 1);
                assert_eq!(insertion_callbacks, 0);
                assert_eq!(clipboard.restores, 0);
                let (r2, snapshot) = newer.unwrap();
                assert_eq!(board.revision(), Some(r2));
                let final_snapshot = board.read().unwrap();
                assert_eq!(final_snapshot, snapshot);
                assert_eq!(final_snapshot, expected);
                assert_ne!(final_snapshot, saved);
                assert_eq!(transcript, transcript_before);
                // The adapter's borrow ends before with_board requests native cleanup.
            });
            let (r0, r1, r2) = revisions.unwrap();
            // Emit only after every assertion and normal releaseGlobally RAII return.
            // Binary identity and process termination belong to the executing parent.
            println!(
                concat!(
                    "E37_NAMED_BOARD {{\"schemaVersion\":1,",
                    "\"scenario\":\"native_same_text_republication_inside_delivery_refuses_keys\",",
                    "\"testOnly\":true,\"realNativeSurface\":\"named-NSPasteboard\",",
                    "\"r0\":{},\"r1\":{},\"r2\":{},\"finalRevision\":{},",
                    "\"stageOrder\":[\"seed\",\"transactionPublication\",",
                    "\"independentRepublication\",\"ownershipCheck\",",
                    "\"transactionReturn\",\"helperReturn\"],",
                    "\"transactionWrites\":1,\"independentPublications\":1,",
                    "\"outerCallbacks\":1,\"insertionCallbacks\":0,\"restoreCalls\":0,",
                    "\"scenarioInvocations\":1,\"outcome\":\"ContextMismatch\",",
                    "\"exactContentRetained\":true,\"originalNotRestored\":true,",
                    "\"syntheticInputUnchanged\":true,\"assertionsPassed\":true,",
                    "\"releaseGloballyRequested\":true,\"cleanupVerified\":null,",
                    "\"targetValidation\":\"not-exercised\",\"generalPasteboardAccess\":false,",
                    "\"providerPath\":\"absent\",\"capturePath\":\"absent\",",
                    "\"qualificationPassed\":false}}"
                ),
                r0, r1, r2, r2
            );
        }
        #[test]
        fn synthesized_alias_uses_actual_item_and_empty_roundtrip() {
            with_board(|board| {
                let empty = board.read().unwrap();
                assert!(empty.is_empty());
                let revision = board
                    .write("😀e\u{301}", board.revision().unwrap())
                    .unwrap();
                unsafe {
                    let types: cocoa::base::id = msg_send![board.0, types];
                    let alias = CocoaOwned(
                        cocoa::foundation::NSString::alloc(cocoa::base::nil)
                            .init_str("NSStringPboardType"),
                    );
                    let has_alias: bool = msg_send![types, containsObject: alias.0];
                    assert!(has_alias, "native synthesized alias must be exercised");
                }
                assert_eq!(
                    board.read().unwrap(),
                    vec![vec![representation(
                        "public.utf8-plain-text",
                        "😀e\u{301}".as_bytes()
                    )]]
                );
                board.publish(&empty, revision).unwrap();
                assert_eq!(board.read(), Some(empty));
            });
        }
        #[test]
        fn rich_and_multiple_items_roundtrip_without_flattening() {
            with_board(|board| {
                for image in [
                    vec![vec![
                        representation("public.html", b"<b>Hello</b>"),
                        representation("public.utf8-plain-text", b"Hello"),
                    ]],
                    vec![
                        vec![representation("public.utf8-plain-text", b"first")],
                        vec![
                            representation("public.html", b"<i>second</i>"),
                            representation("public.utf8-plain-text", b"second"),
                        ],
                    ],
                ] {
                    let revision = board.publish(&image, board.revision().unwrap()).unwrap();
                    let saved = board.read().unwrap();
                    assert_eq!(saved, image);
                    let publication = board.write("dictation", revision).unwrap();
                    let restored = board.publish(&saved, publication).unwrap();
                    assert_eq!(publication, revision + 1);
                    assert_eq!(restored, revision + 2);
                    assert_eq!(board.read(), Some(image));
                }
            });
        }
        #[test]
        fn unavailable_declared_representation_refuses_without_mutation() {
            with_board(|board| unsafe {
                let kind =
                    CocoaOwned(NSString::alloc(cocoa::base::nil).init_str("test.unavailable"));
                let types: cocoa::base::id = msg_send![class!(NSArray), arrayWithObject: kind.0];
                let _: isize = msg_send![board.0, declareTypes: types owner: cocoa::base::nil];
                let before = board.revision();
                assert!(board.read().is_none());
                assert_eq!(board.revision(), before);
            });
        }

        #[test]
        fn bounded_snapshot_refuses_before_mutation_and_stale_restore_refuses() {
            with_board(|board| {
                let plain = representation("public.utf8-plain-text", b"original");
                for image in [
                    vec![vec![plain.clone()]; 33],
                    vec![(0..65)
                        .map(|i| representation(&format!("test.type{i}"), b"x"))
                        .collect()],
                    vec![vec![representation(
                        "com.apple.pasteboard.promised-file-url",
                        b"x",
                    )]],
                    vec![vec![representation(
                        "public.data",
                        &vec![0; 4 * 1024 * 1024 + 1],
                    )]],
                    vec![vec![representation("public.data", &vec![0; 4 * 1024 * 1024])]; 5],
                ] {
                    let revision = board.publish(&image, board.revision().unwrap()).unwrap();
                    assert!(board.read().is_none());
                    assert_eq!(board.revision(), Some(revision));
                }
                // Cocoa itself rejects this malformed UTI before publication;
                // it cannot be installed to exercise the reader's defensive bound.
                let before = board.revision().unwrap();
                let invalid_type = vec![vec![representation(&"x".repeat(1025), b"x")]];
                assert!(board.publish(&invalid_type, before).is_none());
                assert_eq!(board.revision(), Some(before));
                let original = vec![vec![plain]];
                let revision = board.publish(&original, board.revision().unwrap()).unwrap();
                let external = board.write("external", revision).unwrap();
                assert!(board.publish(&original, revision).is_none());
                assert_eq!(board.revision(), Some(external));
                assert_eq!(
                    board.read().unwrap(),
                    vec![vec![representation("public.utf8-plain-text", b"external")]]
                );
            });
        }
    }
    fn guarded_keys(publication: i64) -> Result<()> {
        unsafe {
            let source = CGEventSourceCreate(MAC_CG_EVENT_SOURCE_STATE_HID_SYSTEM_STATE);
            if source.is_null() {
                anyhow::bail!("event source unavailable");
            }
            let mut failed = false;
            let mut command_down = false;
            let mut paste_down = false;
            // Once attempted, key-up cleanup balances our keys; it cannot initiate
            // another paste. No new key-down is authorized after abandonment.
            for spec in macos_paste_event_sequence() {
                if !spec.key_down
                    && !(if spec.key_code == MACOS_LEFT_COMMAND_KEY_CODE {
                        command_down
                    } else {
                        paste_down
                    })
                {
                    continue;
                }
                let board: cocoa::base::id = msg_send![class!(NSPasteboard), generalPasteboard];
                let revision: isize = msg_send![board, changeCount];
                if revision as i64 != publication {
                    failed = true;
                }
                if spec.key_down
                    && (failed || !crate::infrastructure::continuation_context::begin_effect())
                {
                    failed = true;
                    continue;
                }
                let event = CGEventCreateKeyboardEvent(source, spec.key_code, spec.key_down);
                if event.is_null() {
                    failed = true;
                    continue;
                }
                CGEventSetFlags(event, spec.flags);
                let revision: isize = msg_send![board, changeCount];
                if !spec.key_down
                    || (revision as i64 == publication
                        && crate::infrastructure::continuation_context::begin_effect())
                {
                    CGEventPost(MAC_CG_SESSION_EVENT_TAP, event);
                    if spec.key_code == MACOS_LEFT_COMMAND_KEY_CODE {
                        command_down = spec.key_down;
                    } else {
                        paste_down = spec.key_down;
                    }
                } else {
                    failed = true;
                }
                CFRelease(event as *const c_void);
                if spec.delay_after_ms > 0 {
                    thread::sleep(Duration::from_millis(spec.delay_after_ms));
                }
            }
            CFRelease(source as *const c_void);
            if failed {
                anyhow::bail!("guarded key sequence incomplete");
            }
            Ok(())
        }
    }
    const ANCHOR: isize = 64;
    use crate::infrastructure::continuation_context::{
        LocalSnapshot as Snapshot, TextRange as Range,
    };
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXUIElementGetTypeID() -> usize;
        fn AXValueGetTypeID() -> usize;
        fn AXUIElementSetMessagingTimeout(element: MacAXUIElementRef, timeout: f32) -> i32;
        fn AXValueCreate(kind: u32, value: *const c_void) -> MacCFTypeRef;
        fn AXValueGetValue(value: MacCFTypeRef, kind: u32, output: *mut c_void) -> bool;
        fn AXUIElementCopyParameterizedAttributeValue(
            element: MacAXUIElementRef,
            attribute: MacCFStringRef,
            parameter: MacCFTypeRef,
            value: *mut MacCFTypeRef,
        ) -> i32;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFGetTypeID(value: MacCFTypeRef) -> usize;
        fn CFNumberGetTypeID() -> usize;
        fn CFStringGetTypeID() -> usize;
        fn CFEqual(a: MacCFTypeRef, b: MacCFTypeRef) -> bool;
        fn CFNumberGetValue(number: MacCFTypeRef, kind: i32, value: *mut c_void) -> bool;
        fn CFStringGetLength(value: MacCFStringRef) -> isize;
        fn CFStringGetCharacters(value: MacCFStringRef, range: Range, buffer: *mut u16);
    }
    struct Owned(MacCFTypeRef);
    impl Drop for Owned {
        fn drop(&mut self) {
            unsafe { CFRelease(self.0) };
        }
    }
    impl Owned {
        fn attribute(&self, name: &str) -> Option<Self> {
            if !self.bounded() {
                return None;
            }
            copy_ax_attribute(self.0 as MacAXUIElementRef, name).map(Self)
        }
        fn bounded(&self) -> bool {
            let Some(timeout) =
                crate::infrastructure::continuation_context::native_timeout_seconds()
            else {
                return false;
            };
            unsafe {
                CFGetTypeID(self.0) == AXUIElementGetTypeID()
                    && AXUIElementSetMessagingTimeout(self.0 as MacAXUIElementRef, timeout) == 0
            }
        }
    }
    fn selection(element: &Owned) -> Option<Range> {
        let value = element.attribute("AXSelectedTextRange")?;
        if unsafe { CFGetTypeID(value.0) != AXValueGetTypeID() } {
            return None;
        }
        let mut range = Range {
            location: 0,
            length: 0,
        };
        if !unsafe { AXValueGetValue(value.0, 4, &mut range as *mut _ as *mut c_void) }
            || range.location < 0
            || range.length < 0
            || range.length > 4096
        {
            return None;
        }
        Some(range)
    }
    fn total(element: &Owned) -> Option<isize> {
        let value = element.attribute("AXNumberOfCharacters")?;
        if unsafe { CFGetTypeID(value.0) != CFNumberGetTypeID() } {
            return None;
        }
        let mut total: isize = 0;
        if unsafe { CFNumberGetValue(value.0, 14, &mut total as *mut _ as *mut c_void) }
            && total >= 0
        {
            Some(total)
        } else {
            None
        }
    }
    fn text_range(element: &Owned, range: Range) -> Option<Vec<u16>> {
        if !element.bounded() {
            return None;
        }
        if range.length < 0 || range.length > 65536 + 4096 + 2 * ANCHOR {
            return None;
        }
        let parameter = unsafe { AXValueCreate(4, &range as *const _ as *const c_void) };
        if parameter.is_null() {
            return None;
        }
        let parameter = Owned(parameter);
        let name = ScopedCFString::new("AXStringForRange").ok()?;
        let mut value = std::ptr::null();
        if unsafe {
            AXUIElementCopyParameterizedAttributeValue(
                element.0 as MacAXUIElementRef,
                name.as_ptr(),
                parameter.0,
                &mut value,
            )
        } != 0
            || value.is_null()
        {
            return None;
        }
        let value = Owned(value);
        if unsafe {
            CFGetTypeID(value.0) != CFStringGetTypeID()
                || CFStringGetLength(value.0) != range.length
        } {
            return None;
        }
        let mut units = vec![0; range.length as usize];
        unsafe {
            CFStringGetCharacters(
                value.0,
                Range {
                    location: 0,
                    length: range.length,
                },
                units.as_mut_ptr(),
            )
        };
        Some(units)
    }
    fn snapshot(element: &Owned) -> Option<Snapshot> {
        let selection = selection(element)?;
        let total = total(element)?;
        let end = selection.location.checked_add(selection.length)?;
        if end > total {
            return None;
        }
        let start = (selection.location - ANCHOR).max(0);
        let anchor = text_range(
            element,
            Range {
                location: start,
                length: end.saturating_add(ANCHOR).min(total) - start,
            },
        )?;
        Some(Snapshot {
            selection,
            total,
            start,
            anchor,
        })
    }
    #[derive(PartialEq, Eq)]
    struct DocumentIdentity {
        document: Option<Vec<u16>>,
        title: Vec<u16>,
    }
    fn bounded_string_attribute(element: &Owned, name: &str) -> Option<Option<Vec<u16>>> {
        if !element.bounded() {
            return None;
        }
        let name = ScopedCFString::new(name).ok()?;
        let mut value = std::ptr::null();
        let error = unsafe {
            AXUIElementCopyAttributeValue(element.0 as MacAXUIElementRef, name.as_ptr(), &mut value)
        };
        // Unsupported/no-value is distinct from a messaging timeout or failure.
        if error == -25205 || error == -25212 {
            return Some(None);
        }
        if error != 0 || value.is_null() {
            return None;
        }
        let value = Owned(value);
        if unsafe { CFGetTypeID(value.0) != CFStringGetTypeID() } {
            return None;
        }
        let length = unsafe { CFStringGetLength(value.0) };
        if !(0..=1024).contains(&length) {
            return None;
        }
        let mut units = vec![0; length as usize];
        unsafe {
            CFStringGetCharacters(
                value.0,
                Range {
                    location: 0,
                    length,
                },
                units.as_mut_ptr(),
            )
        };
        Some(Some(units))
    }
    fn document_identity(window: &Owned) -> Option<DocumentIdentity> {
        Some(DocumentIdentity {
            document: bounded_string_attribute(window, "AXDocument")?,
            title: bounded_string_attribute(window, "AXTitle")??,
        })
    }
    // Unlike the legacy target selector, this observation must include our own
    // frontmost process. The legacy selector intentionally filters Voicetext out.
    fn frontmost_identity_for_validation() -> Option<AutoPasteTarget> {
        use cocoa::base::{id, nil};
        use objc::{class, msg_send, sel, sel_impl};
        unsafe {
            let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
            let active_app: id = msg_send![workspace, frontmostApplication];
            if active_app == nil {
                return None;
            }
            let bundle_id = running_app_bundle_id(active_app)?;
            let pid = running_app_pid(active_app);
            if pid <= 0 || bundle_id.is_empty() {
                return None;
            }
            Some(AutoPasteTarget { bundle_id, pid })
        }
    }
    // TEST-only, worker-local evidence; no AX values or newly focused text are logged.
    #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
    thread_local! {
        static READER_CANCEL: std::cell::RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>> = const { std::cell::RefCell::new(None) };
        static READER_BIND_OBSERVER: std::cell::Cell<Option<fn(serde_json::Value)>> = const { std::cell::Cell::new(None) };
        static READER_DIAGNOSTICS: std::cell::RefCell<serde_json::Value> = std::cell::RefCell::new(
            serde_json::json!({"operations":[],"bindOperations":[],"firstFatal":null,"phase":"bind","phaseStartMs":0,"readSequence":0}));
    }
    // Independent full-document reader: no continuation guard or insertion state.
    #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
    pub struct SyntheticTextEditReader {
        target: AutoPasteTarget,
        app: Owned,
        element: Owned,
        window: Owned,
        path: String,
        consecutive_transient_samples: std::cell::Cell<u8>,
    }
    #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
    impl SyntheticTextEditReader {
        pub fn finish_arming() {
            READER_CANCEL.with(|s| *s.borrow_mut() = None);
        }
        pub fn observe_bind(observer: fn(serde_json::Value)) {
            READER_BIND_OBSERVER.with(|o| o.set(Some(observer)));
        }
        fn publish_bind() {
            if READER_DIAGNOSTICS.with(|d| d.borrow()["phase"] == "bind") {
                READER_BIND_OBSERVER.with(|o| {
                    if let Some(observer) = o.get() {
                        observer(Self::diagnostics(None));
                    }
                });
            }
        }
        fn raw_ax<T>(call: impl FnOnce() -> T) -> T {
            let entry = Self::clock_ms();
            let value = call();
            let returned = Self::clock_ms();
            READER_DIAGNOSTICS
                .with(|d| d.borrow_mut()["rawCallMs"] = serde_json::json!([entry, returned]));
            value
        }
        fn require_ax_element(value: MacCFTypeRef) -> Result<()> {
            anyhow::ensure!(
                !value.is_null() && unsafe { CFGetTypeID(value) == AXUIElementGetTypeID() },
                "AX element type"
            );
            Ok(())
        }
        fn phase(phase: &str) {
            READER_DIAGNOSTICS.with(|d| {
                let mut d = d.borrow_mut();
                d["phase"] = serde_json::json!(phase);
                d["phaseStartMs"] = serde_json::json!(Self::clock_ms());
            });
        }
        fn metadata(deadline: Option<std::time::Instant>) -> Result<serde_json::Value> {
            extern "C" {
                fn AXIsProcessTrusted() -> bool;
            }
            let _pool = unsafe { CocoaOwned(msg_send![class!(NSAutoreleasePool), new]) };
            let start = Self::clock_ms();
            let foreground = frontmost_identity_for_validation();
            let end = Self::clock_ms();
            if let Some(deadline) = deadline {
                Self::operation("metadata-trust", deadline, || (None, false, false, ()))?;
            }
            Ok(
                serde_json::json!({"lookupIntervalMs":[start,end],"atMs":Self::clock_ms(),"trusted":unsafe { AXIsProcessTrusted() },
                "foregroundPid":foreground.as_ref().map(|t| t.pid),
                "foregroundBundle":foreground.as_ref().map(|t| &t.bundle_id)}),
            )
        }
        pub fn diagnostics(error: Option<&str>) -> serde_json::Value {
            READER_DIAGNOSTICS.with(|d| {
                let mut d = d.borrow_mut();
                if let Some(error) = error {
                    if d["firstFatal"].is_null() {
                        d["firstFatal"] = serde_json::json!({"error":error,"atMs":Self::clock_ms(),
                            "phase":d["phase"],"phaseStartMs":d["phaseStartMs"],"readSequence":d["readSequence"],
                            "operation":d["operations"].as_array().and_then(|r| r.last()),"metadata":null});
                    }
                }
                d.clone()
            })
        }
        pub fn enrich_failure_metadata() -> serde_json::Value {
            let pending = READER_DIAGNOSTICS.with(|d| {
                let d = d.borrow();
                !d["firstFatal"].is_null() && d["firstFatal"]["metadata"].is_null()
            });
            if pending {
                let metadata = Self::metadata(None).expect("unbudgeted terminal metadata");
                READER_DIAGNOSTICS.with(|d| d.borrow_mut()["firstFatal"]["metadata"] = metadata);
            }
            Self::diagnostics(None)
        }
        fn outcome(code: Option<i32>, null: bool, wrong_type: bool, expired: bool) -> &'static str {
            if let Some(code) = code {
                if code != 0 {
                    return "native-error";
                }
            }
            if null {
                "success-null"
            } else if wrong_type {
                "type-mismatch"
            } else if expired {
                "local-deadline"
            } else {
                "ok"
            }
        }
        fn operation<T>(
            op: &'static str,
            deadline: std::time::Instant,
            call: impl FnOnce() -> (Option<i32>, bool, bool, T),
        ) -> Result<T> {
            let start = Self::clock_ms();
            let now = std::time::Instant::now();
            let signed_remaining = if deadline >= now {
                deadline.duration_since(now).as_secs_f64()
            } else {
                -now.duration_since(deadline).as_secs_f64()
            } * 1000.0;
            let remaining = signed_remaining.max(0.0);
            READER_DIAGNOSTICS.with(|d| { let mut d = d.borrow_mut();
                d["rawCallMs"] = serde_json::Value::Null; d["typeCheckMs"] = serde_json::Value::Null;
                d["pendingOperation"] = serde_json::json!({"attempt":d["attempt"],"stage":d["stage"],"op":op,"startMs":start,"deadlineMs":start+signed_remaining}); });
            Self::publish_bind();
            let cancelled = || {
                READER_CANCEL.with(|s| {
                    s.borrow()
                        .as_ref()
                        .is_some_and(|s| s.load(std::sync::atomic::Ordering::SeqCst))
                })
            };
            let result = if remaining > 0.0 && std::time::Instant::now() < deadline && !cancelled()
            {
                Some(call())
            } else {
                None
            };
            let end = Self::clock_ms();
            let expired = std::time::Instant::now() >= deadline;
            let (code, null, wrong_type) = result
                .as_ref()
                .map(|r| (r.0, r.1, r.2))
                .unwrap_or((None, false, false));
            let outcome = Self::outcome(code, null, wrong_type, expired);
            let failure = READER_DIAGNOSTICS.with(|d| { let mut d = d.borrow_mut();
                let row = serde_json::json!({"attempt":d["attempt"],"stage":d["stage"],"phase":d["phase"],"phaseStartMs":d["phaseStartMs"],"readSequence":d["readSequence"],"op":op,
                    "startMs":start,"endMs":end,"deadlineMs":start+signed_remaining,"remainingMs":remaining,
                    "remainingAfterMs":deadline.saturating_duration_since(std::time::Instant::now()).as_secs_f64()*1000.0,
                    "rawCallMs":d["rawCallMs"],"typeCheckMs":d["typeCheckMs"],"called":result.is_some(),"code":code,"null":(null),"typeMismatch":wrong_type,
                    "deadlineExceeded":expired,"outcome":outcome});
                let failure = super::synthetic_readiness::OperationError { initial_focus:d["stage"] == "initial-focus",
                    bind:d["phase"] == "bind", sequence:d["readSequence"].as_u64().unwrap_or(u64::MAX),
                    op, called:result.is_some(), code, expired, null, wrong_type, outcome, evidence:row.clone() };
                if d["phase"] == "bind" && d["attempt"].as_u64().unwrap_or(1) == 1 {
                    let bind = d["bindOperations"].as_array_mut().unwrap();
                    if bind.len() < 32 { bind.push(row.clone()); } else { d["evidenceOverflow"] = serde_json::json!(true); }
                }
                d["pendingOperation"] = serde_json::Value::Null;
                let rows = d["operations"].as_array_mut().unwrap();
                if rows.len() == 32 { rows.remove(0); } rows.push(row);
                failure
            });
            Self::publish_bind();
            anyhow::ensure!(
                !READER_DIAGNOSTICS.with(|d| d.borrow()["evidenceOverflow"] == true),
                "evidence-overflow"
            );
            if outcome != "ok" || expired || result.is_none() || cancelled() {
                drop(result); // Release even a late successful Owned AX value before retry classification.
                return Err(failure.into());
            }
            Ok(result.expect("operation before deadline").3)
        }
        fn checked_pid(value: MacCFTypeRef, deadline: std::time::Instant) -> Result<Option<i32>> {
            Self::require_ax_element(value)?;
            let mut pid = 0;
            Self::operation("AXUIElementGetPid", deadline, || {
                let code = Self::raw_ax(|| unsafe {
                    AXUIElementGetPid(value as MacAXUIElementRef, &mut pid)
                });
                (Some(code), false, false, ())
            })?;
            Ok((pid > 0).then_some(pid))
        }
        pub fn clock_ms() -> f64 {
            super::super::continuation_context::observation::now_ms()
        }
        fn attr(
            element: &Owned,
            name: &'static str,
            deadline: std::time::Instant,
        ) -> Result<Owned> {
            Self::require_ax_element(element.0)?;
            Self::operation("AXUIElementSetMessagingTimeout", deadline, || {
                (
                    Some(Self::raw_ax(|| unsafe {
                        AXUIElementSetMessagingTimeout(element.0 as MacAXUIElementRef, 0.025)
                    })),
                    false,
                    false,
                    (),
                )
            })?;
            let attribute = ScopedCFString::new(name)?;
            Self::operation(name, deadline, || {
                let mut ptr = std::ptr::null();
                let code = Self::raw_ax(|| unsafe {
                    AXUIElementCopyAttributeValue(
                        element.0 as MacAXUIElementRef,
                        attribute.as_ptr(),
                        &mut ptr,
                    )
                });
                let value = (!ptr.is_null()).then(|| Owned(ptr));
                let type_entry = Self::clock_ms();
                let expected = unsafe {
                    if matches!(name, "AXFocusedUIElement" | "AXWindow") {
                        AXUIElementGetTypeID()
                    } else {
                        CFStringGetTypeID()
                    }
                };
                let wrong = value
                    .as_ref()
                    .is_some_and(|v| unsafe { CFGetTypeID(v.0) != expected });
                let type_return = Self::clock_ms();
                READER_DIAGNOSTICS.with(|d| {
                    d.borrow_mut()["typeCheckMs"] = serde_json::json!([type_entry, type_return])
                });
                (Some(code), ptr.is_null(), wrong, value)
            })?
            .ok_or_else(|| anyhow::anyhow!("success-null"))
        }
        fn string(value: Owned) -> Result<String> {
            anyhow::ensure!(
                unsafe { CFGetTypeID(value.0) == CFStringGetTypeID() },
                "AX value type"
            );
            let length = unsafe { CFStringGetLength(value.0) };
            anyhow::ensure!((0..=4096).contains(&length), "AX UTF16 overflow");
            let mut units = vec![0; length as usize];
            unsafe {
                CFStringGetCharacters(
                    value.0,
                    Range {
                        location: 0,
                        length,
                    },
                    units.as_mut_ptr(),
                )
            };
            String::from_utf16(&units).map_err(Into::into)
        }
        pub fn bind(
            path: &std::path::Path,
            owner: &mut Option<(i32, String, std::path::PathBuf)>,
            attempt: usize,
            cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
            arm_deadline: std::time::Instant,
        ) -> Result<Self> {
            let entry = std::time::Instant::now();
            READER_DIAGNOSTICS.with(|d| {
                super::synthetic_readiness::reset_attempt(
                    &mut d.borrow_mut(),
                    attempt,
                    Self::clock_ms(),
                )
            });
            super::synthetic_readiness::admit(
                entry,
                arm_deadline,
                cancel.load(std::sync::atomic::Ordering::SeqCst),
                450,
            )
            .map_err(anyhow::Error::msg)?;
            let deadline = entry + Duration::from_millis(200);
            READER_CANCEL.with(|s| *s.borrow_mut() = Some(cancel));
            READER_DIAGNOSTICS.with(|d| {
                d.borrow_mut()["bindMetadataStartMs"] = serde_json::json!(Self::clock_ms())
            });
            Self::publish_bind();
            Self::operation("bind-preamble", deadline, || (None, false, false, ()))?;
            let metadata = Self::metadata(Some(deadline))?;
            READER_DIAGNOSTICS.with(|d| d.borrow_mut()["bindMetadata"] = metadata);
            Self::publish_bind();
            Self::operation("bind-lookup", deadline, || (None, false, false, ()))?;
            let _pool = unsafe { CocoaOwned(msg_send![class!(NSAutoreleasePool), new]) };
            let lookup_start = Self::clock_ms();
            READER_DIAGNOSTICS.with(|d| {
                d.borrow_mut()["bindTargetLookup"] =
                    serde_json::json!({"intervalMs":[lookup_start,null]})
            });
            Self::publish_bind();
            Self::operation("bind-target", deadline, || (None, false, false, ()))?;
            let target = get_active_app_target();
            let lookup_end = Self::clock_ms();
            READER_DIAGNOSTICS.with(|d| {
                d.borrow_mut()["bindTargetLookup"] = serde_json::json!({
                "intervalMs":[lookup_start,lookup_end],"pid":target.as_ref().map(|t| t.pid),
                "bundle":target.as_ref().map(|t| &t.bundle_id)})
            });
            Self::publish_bind();
            let target = target.ok_or_else(|| anyhow::anyhow!("owned target unavailable"))?;
            anyhow::ensure!(
                target.bundle_id == "com.apple.TextEdit",
                "owned TextEdit required"
            );
            let resolved = (target.pid, target.bundle_id.clone(), path.to_path_buf());
            super::synthetic_readiness::pin(owner, resolved)?;
            Self::operation("bind-foreground", deadline, || (None, false, false, ()))?;
            let foreground = frontmost_identity_for_validation()
                .ok_or_else(|| anyhow::anyhow!("foreground unavailable"))?;
            anyhow::ensure!(
                foreground.pid == target.pid && foreground.bundle_id == target.bundle_id,
                "owned foreground changed"
            );
            let app = Self::operation("AXUIElementCreateApplication", deadline, || {
                let ptr = Self::raw_ax(|| unsafe { AXUIElementCreateApplication(target.pid) });
                (
                    None,
                    ptr.is_null(),
                    false,
                    (!ptr.is_null()).then(|| Owned(ptr as MacCFTypeRef)),
                )
            })?
            .ok_or_else(|| anyhow::anyhow!("AX app success-null"))?;
            READER_DIAGNOSTICS
                .with(|d| d.borrow_mut()["stage"] = serde_json::json!("initial-focus"));
            let element = Self::attr(&app, "AXFocusedUIElement", deadline)?;
            READER_DIAGNOSTICS.with(|d| d.borrow_mut()["stage"] = serde_json::json!("identity"));
            Self::require_ax_element(element.0)?;
            anyhow::ensure!(
                Self::string(Self::attr(&element, "AXRole", deadline)?)? == "AXTextArea",
                "owned TextEdit editor role"
            );
            let window = Self::attr(&element, "AXWindow", deadline)?;
            Self::require_ax_element(window.0)?;
            let reader = Self {
                target,
                app,
                element,
                window,
                path: path
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("owned path UTF8"))?
                    .into(),
                consecutive_transient_samples: std::cell::Cell::new(0),
            };
            reader.identity(deadline)?;
            // Acquisition/identity work may span a foreground change. Observe again
            // inside the same strict attempt deadline before allowing bind success.
            let foreground = Self::operation("bind-final-foreground", deadline, || {
                (None, false, false, frontmost_identity_for_validation())
            })?;
            super::synthetic_readiness::validate_foreground(
                reader.target.pid,
                &reader.target.bundle_id,
                foreground
                    .as_ref()
                    .map(|target| (target.pid, target.bundle_id.as_str())),
            )?;
            READER_DIAGNOSTICS
                .with(|d| d.borrow_mut()["bindEndMs"] = serde_json::json!(Self::clock_ms()));
            Ok(reader)
        }
        fn identity(&self, deadline: std::time::Instant) -> Result<()> {
            Self::operation("identity-entry", deadline, || (None, false, false, ()))?;
            // Query this PID, never rebind to whatever app/editor is currently focused.
            let bundle = Self::operation(
                "NSRunningApplicationBundleIdentifier",
                deadline,
                || unsafe {
                    let pool = CocoaOwned(msg_send![class!(NSAutoreleasePool), new]);
                    let app: cocoa::base::id = msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier:self.target.pid];
                    let bundle = if app == cocoa::base::nil {
                        None
                    } else {
                        running_app_bundle_id(app)
                    };
                    drop(pool);
                    (None, bundle.is_none(), false, bundle)
                },
            )?;
            anyhow::ensure!(
                bundle.as_deref() == Some(self.target.bundle_id.as_str()),
                "owned PID/bundle changed"
            );
            anyhow::ensure!(
                Self::checked_pid(self.element.0, deadline)? == Some(self.target.pid)
                    && Self::checked_pid(self.window.0, deadline)? == Some(self.target.pid),
                "owned AX PID changed"
            );
            let focused = Self::attr(&self.app, "AXFocusedUIElement", deadline)?;
            Self::require_ax_element(focused.0)?;
            let window = Self::attr(&focused, "AXWindow", deadline)?;
            Self::require_ax_element(window.0)?;
            anyhow::ensure!(
                unsafe { CFEqual(focused.0, self.element.0) && CFEqual(window.0, self.window.0) },
                "owned editor/window changed"
            );
            let document = Self::string(Self::attr(&self.window, "AXDocument", deadline)?)?;
            // AXDocument may be an escaped file URL. Parse it, never compare a suffix/title.
            let path = if document.starts_with("file:") {
                tauri::Url::parse(&document)?
                    .to_file_path()
                    .map_err(|_| anyhow::anyhow!("AX document URL"))?
            } else {
                std::path::PathBuf::from(document)
            };
            anyhow::ensure!(
                path == std::path::Path::new(&self.path),
                "owned document changed"
            );
            Self::operation("identity-complete", deadline, || (None, false, false, ()))?;
            Ok(())
        }
        pub fn identity_evidence(&self) -> serde_json::Value {
            serde_json::json!({"pid":self.target.pid,"bundle":self.target.bundle_id,"path":self.path,
                "window":format!("{:p}",self.window.0),"editor":format!("{:p}",self.element.0)})
        }
        pub fn read(
            &self,
            arm_deadline: Option<std::time::Instant>,
            deadline: std::time::Instant,
        ) -> Result<Option<String>> {
            let entry = std::time::Instant::now();
            if let Some(deadline) = arm_deadline {
                super::synthetic_readiness::admit(entry, deadline, false, 250)
                    .map_err(anyhow::Error::msg)?;
            }
            READER_DIAGNOSTICS.with(|d| {
                let mut d = d.borrow_mut();
                d["readSequence"] = serde_json::json!(d["readSequence"].as_u64().unwrap_or(0) + 1);
            });
            super::synthetic_readiness::sample(
                deadline,
                std::time::Instant::now,
                &self.consecutive_transient_samples,
                std::thread::sleep,
                || {
                    Self::phase("pre-value");
                    self.identity(deadline)?;
                    Self::phase("value");
                    let value = Self::string(Self::attr(&self.element, "AXValue", deadline)?)?;
                    Self::phase("post-value");
                    self.identity(deadline)?;
                    Ok(value)
                },
            )
        }
    }
    #[cfg(all(test, debug_assertions, feature = "native-window-e2e"))]
    #[test]
    fn synthetic_reader_refuses_cfstring_before_get_pid() {
        let wrong_type = ScopedCFString::new("not an AX element").unwrap();
        let error = SyntheticTextEditReader::checked_pid(
            wrong_type.as_ptr() as MacCFTypeRef,
            std::time::Instant::now() + Duration::from_millis(200),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "AX element type");
    }
    #[cfg(all(test, debug_assertions, feature = "native-window-e2e"))]
    #[test]
    fn synthetic_reader_diagnostic_categories_and_bound() {
        type R = SyntheticTextEditReader;
        for code in [-25204, -25205, -25212] {
            assert_eq!(R::outcome(Some(code), true, false, true), "native-error");
        }
        assert_eq!(R::outcome(Some(0), true, false, false), "success-null");
        assert_eq!(R::outcome(Some(0), false, true, false), "type-mismatch");
        assert_eq!(R::outcome(None, false, false, true), "local-deadline");
        assert_eq!(R::outcome(Some(0), false, false, false), "ok");
        R::phase("bind");
        let expired = std::time::Instant::now() - Duration::from_millis(40);
        assert!(
            R::operation("expired", expired, || -> (Option<i32>, bool, bool, ()) {
                panic!("expired budget must not call native API")
            })
            .is_err()
        );
        let d = R::diagnostics(None);
        let row = d["operations"].as_array().unwrap().last().unwrap();
        assert_eq!(row["called"], false);
        assert_eq!(row["null"].as_bool(), Some(false));
        assert_eq!(row["remainingMs"], 0.0);
        assert!(row["startMs"].as_f64().unwrap() - row["deadlineMs"].as_f64().unwrap() >= 40.0);
        for i in 0..40 {
            R::phase(if i == 0 { "pre-value" } else { "post-value" });
            assert!(R::operation(
                "raw-code",
                std::time::Instant::now() + Duration::from_millis(200),
                || (Some(-25212), true, false, ())
            )
            .is_err());
        }
        let d = R::diagnostics(None);
        assert_eq!(d["operations"].as_array().unwrap().len(), 32);
        assert_eq!(d["operations"][31]["code"], -25212);
        assert_eq!(d["operations"][31]["null"].as_bool(), Some(true));
        assert_eq!(d["operations"][31]["phase"], "post-value");
        assert_eq!(d["bindOperations"][0]["op"], "expired");
        assert!(d["bindOperations"].as_array().unwrap().len() <= 32);
    }
    #[cfg(all(test, debug_assertions, feature = "native-window-e2e"))]
    #[test]
    fn synthetic_reader_first_fatal_survives_later_diagnostics() {
        type R = SyntheticTextEditReader;
        for (phase, sequence) in [("bind", 0), ("pre-value", 1), ("post-value", 17)] {
            READER_DIAGNOSTICS.with(|d| {
                *d.borrow_mut() = serde_json::json!({
                "operations":[],"bindOperations":[],"firstFatal":null,"readSequence":sequence})
            });
            R::phase(phase);
            let error = R::operation(
                "AXFocusedUIElement",
                std::time::Instant::now() + Duration::from_millis(200),
                || (Some(-25204), true, false, ()),
            )
            .unwrap_err()
            .to_string();
            let first = R::diagnostics(Some(&error))["firstFatal"].clone();
            assert_eq!(first["phase"], phase);
            assert_eq!(first["readSequence"], sequence);
            assert_eq!(first["operation"]["code"], -25204);
            R::phase("shutdown");
            assert_eq!(R::diagnostics(Some("join timeout"))["firstFatal"], first);
        }
    }
    pub(crate) struct ContinuationNativeContext {
        target: AutoPasteTarget,
        app: Owned,
        element: Owned,
        window: Owned,
        document: DocumentIdentity,
        expected: Snapshot,
    }
    impl ContinuationNativeContext {
        pub(crate) fn capture(target: AutoPasteTarget) -> Result<Self> {
            if !continuation_app_qualified(&target) {
                anyhow::bail!("application is not qualified for continuation");
            }
            let _budget =
                crate::infrastructure::continuation_context::NativeEligibilityBudget::start();
            if !check_accessibility_permission() || !frontmost_app_matches_target(&target) {
                anyhow::bail!("target unavailable before focus transfer");
            }
            let app = unsafe { AXUIElementCreateApplication(target.pid) };
            if app.is_null() {
                anyhow::bail!("AX unavailable");
            }
            let app = Owned(app as MacCFTypeRef);
            let captured = (|| {
                if !app.bounded() {
                    return None;
                }
                let element = app.attribute("AXFocusedUIElement")?;
                if !element.bounded()
                    || focused_element_pid(element.0 as MacAXUIElementRef) != Some(target.pid)
                {
                    return None;
                }
                let subrole = bounded_string_attribute(&element, "AXSubrole")?;
                if subrole.as_ref().is_some_and(|value| {
                    "AXSecureTextField".encode_utf16().eq(value.iter().copied())
                }) || !element.bounded()
                    || ax_attribute_settable(element.0 as MacAXUIElementRef, "AXSelectedTextRange")
                        != Some(true)
                {
                    return None;
                }
                let window = element.attribute("AXWindow")?;
                if !window.bounded() {
                    return None;
                }
                let document = document_identity(&window)?;
                let expected = snapshot(&element)?;
                Some((element, window, document, expected))
            })();
            let (element, window, document, expected) =
                captured.ok_or_else(|| anyhow::anyhow!("bounded AX context unavailable"))?;
            if crate::infrastructure::continuation_context::native_timeout_seconds().is_none() {
                anyhow::bail!("native eligibility budget exhausted");
            }
            Ok(Self {
                target,
                app,
                element,
                window,
                document,
                expected,
            })
        }
        fn identity(&self) -> Option<bool> {
            let front = frontmost_identity_for_validation()?;
            if !crate::infrastructure::continuation_context::permits_frontmost_validation(
                &self.target,
                &front,
                std::process::id() as i32,
                VOICETEXT_BUNDLE_IDS,
            ) {
                return Some(false);
            }
            let focused = self.app.attribute("AXFocusedUIElement")?;
            if !focused.bounded() {
                return None;
            }
            let window = focused.attribute("AXWindow")?;
            if !window.bounded() {
                return None;
            }
            Some(
                unsafe { CFEqual(focused.0, self.element.0) && CFEqual(window.0, self.window.0) }
                    && document_identity(&window)? == self.document,
            )
        }
        pub(crate) fn validate(&mut self) -> ContextValidation {
            let _budget =
                crate::infrastructure::continuation_context::NativeEligibilityBudget::start();
            if !check_accessibility_permission() {
                return ContextValidation::Unavailable;
            }
            let checked = (|| {
                if !self.identity()? {
                    return Some(false);
                }
                let current = snapshot(&self.element)?;
                if !self.expected.same_insertion_point(&current) {
                    return Some(false);
                }
                // PLAN §8 permits distant edits. Rebase size metadata only after
                // confirming the retained identity and local insertion point.
                self.expected.total = current.total;
                Some(true)
            })();
            if crate::infrastructure::continuation_context::native_timeout_seconds().is_none() {
                return ContextValidation::Unavailable;
            }
            match checked {
                Some(true) => ContextValidation::Valid { revision: 0 },
                Some(false) => ContextValidation::Mismatch,
                None => ContextValidation::Unavailable,
            }
        }
        pub(crate) fn paste(&mut self, text: &str) -> GuardedPasteOutcome {
            if !matches!(self.validate(), ContextValidation::Valid { .. }) {
                return GuardedPasteOutcome::ContextMismatch;
            }
            if !crate::infrastructure::continuation_context::begin_effect() {
                return GuardedPasteOutcome::Unavailable;
            }
            if activate_continuation_target(&self.target).is_err() {
                return GuardedPasteOutcome::Unavailable;
            }
            if !frontmost_app_matches_target(&self.target)
                || !matches!(self.validate(), ContextValidation::Valid { .. })
            {
                return GuardedPasteOutcome::ContextMismatch;
            }
            let Ok(mut clipboard) = Clipboard::new() else {
                return GuardedPasteOutcome::Unavailable;
            };
            let restore = target_can_safely_restore_clipboard(&self.target);
            crate::infrastructure::continuation_context::clipboard_delivery(
                &mut clipboard,
                text,
                restore,
                |clipboard, publication| self.insert_once(text, clipboard, publication),
            )
        }
        fn insert_once(
            &mut self,
            text: &str,
            clipboard: &mut Clipboard,
            publication: i64,
        ) -> GuardedPasteOutcome {
            if !frontmost_app_matches_target(&self.target)
                || !matches!(self.validate(), ContextValidation::Valid { .. })
            {
                return GuardedPasteOutcome::ContextMismatch;
            }
            crate::infrastructure::continuation_context::insert_with_current_clipboard(
                clipboard,
                publication,
                || self.post_and_confirm(text, publication),
            )
        }
        fn post_and_confirm(&mut self, text: &str, publication: i64) -> GuardedPasteOutcome {
            // Key posting may partially succeed. There is no fallback or retry.
            if guarded_keys(publication).is_err() {
                return GuardedPasteOutcome::Uncertain;
            }
            thread::sleep(Duration::from_millis(250));
            let Some(expected) = self.expected.after_insertion(text) else {
                return GuardedPasteOutcome::Uncertain;
            };
            let confirmed = (|| {
                Some(
                    self.identity()?
                        && selection(&self.element)? == expected.selection
                        && total(&self.element)? == expected.total
                        && text_range(
                            &self.element,
                            Range {
                                location: expected.start,
                                length: expected.anchor.len() as isize,
                            },
                        )? == expected.anchor,
                )
            })() == Some(true);
            if !confirmed {
                return GuardedPasteOutcome::Uncertain;
            }
            let Some(next) = snapshot(&self.element) else {
                return GuardedPasteOutcome::Uncertain;
            };
            if !expected.contains(&next) {
                return GuardedPasteOutcome::Uncertain;
            }
            self.expected = next;
            GuardedPasteOutcome::Confirmed { revision: 0 }
        }
    }
}

#[cfg(test)]
mod continuation_remediation_tests {
    use super::*;

    #[test]
    fn qualification_requires_textedit_not_merely_an_ax_capable_app() {
        for (bundle, expected) in [
            ("com.apple.TextEdit", true),
            ("com.apple.Notes", false),
            ("com.google.Chrome", false),
            ("com.apple.TextEdit.other", false),
            ("", false),
        ] {
            assert_eq!(
                continuation_app_qualified(&AutoPasteTarget {
                    bundle_id: bundle.into(),
                    pid: 123
                }),
                expected
            );
        }
        assert!(!continuation_app_qualified(&AutoPasteTarget {
            bundle_id: "com.apple.TextEdit".into(),
            pid: 0
        }));
    }

    #[test]
    fn platform_copy_publishes_once_and_classifies_write_failure_as_uncertain() {
        struct Clipboard {
            writes: Vec<String>,
            fail: bool,
        }
        impl ClipboardAccess for Clipboard {
            fn get_text(&mut self) -> Result<String> {
                panic!("copy does not read or restore")
            }
            fn set_text(&mut self, text: &str) -> Result<()> {
                self.writes.push(text.into());
                if self.fail {
                    anyhow::bail!("publication may have partially succeeded");
                }
                Ok(())
            }
        }
        for fail in [false, true] {
            let mut clipboard = Clipboard {
                writes: vec![],
                fail,
            };
            assert_eq!(
                publish_continuation_copy(&mut clipboard, "final text", 7),
                if fail {
                    super::super::continuation_context::GuardedPasteOutcome::Uncertain
                } else {
                    super::super::continuation_context::GuardedPasteOutcome::Confirmed {
                        revision: 7,
                    }
                }
            );
            assert_eq!(clipboard.writes, ["final text"]);
        }
    }
}
