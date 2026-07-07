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

#[cfg(target_os = "macos")]
fn post_paste_commit_delay() -> Duration {
    Duration::from_millis(AUTO_PASTE_POST_PASTE_COMMIT_DELAY_MS)
}

fn restore_clipboard_after_successful_paste_enabled() -> bool {
    !cfg!(target_os = "macos")
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
    paste_text_for_target_with(
        target,
        || paste_text_via_focused_accessibility_element(text, target),
        || paste_text_hybrid_for_target(text, target),
    )
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
        if let Some(previous_text) = previous_text.as_deref() {
            if let Err(restore_error) = clipboard
                .set_text(previous_text)
                .context("Failed to restore previous clipboard text after paste shortcut failure")
            {
                log::warn!("{}", restore_error);
            }
        }
        return Err(error);
    }

    if !restore_clipboard_after_successful_paste_enabled() {
        log::info!(
            "Keeping auto-paste text in clipboard after successful paste command on macOS to avoid delayed paste races"
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
        AUTO_PASTE_RESTORE_CLIPBOARD_DELAY_MS, VOICETEXT_BUNDLE_ID, VOICETEXT_DEV_BUNDLE_ID,
        VOICETEXT_PROD_BUNDLE_ID,
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
        let mut events = vec![
            "get:1".to_string(),
            format!("set:{}", text),
            "sleep:80".to_string(),
            "paste".to_string(),
        ];
        if !super::restore_clipboard_after_successful_paste_enabled() {
            events.push(format!(
                "sleep:{}",
                super::AUTO_PASTE_POST_PASTE_COMMIT_DELAY_MS
            ));
        } else {
            events.push(format!("sleep:{}", AUTO_PASTE_RESTORE_CLIPBOARD_DELAY_MS));
            events.push("get:2".to_string());
            events.push("set:previous".to_string());
        }
        events
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
        assert_eq!(clipboard.text, text);
        assert!(injector.typed_texts.is_empty());
        assert_eq!(injector.paste_shortcut_calls, 1);
        assert_eq!(
            super::clipboard_backend_threshold_chars(),
            super::AUTO_PASTE_MACOS_CLIPBOARD_THRESHOLD_CHARS
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_target_clipboard_paste_uses_saved_target_for_paste_command() {
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
                let seen_targets = seen_targets.clone();
                move |target| {
                    seen_targets.borrow_mut().push(target.clone());
                    Ok(())
                }
            })
            .unwrap();

        assert_eq!(method, AutoPasteMethod::Clipboard);
        assert_eq!(seen_targets.borrow().as_slice(), [target]);
        assert_eq!(clipboard.text, text);
    }

    #[test]
    fn clipboard_restore_delay_leaves_time_for_async_paste_handlers() {
        assert!(AUTO_PASTE_RESTORE_CLIPBOARD_DELAY_MS >= 2_500);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn clipboard_success_waits_for_macos_paste_commit_before_returning_focus() {
        assert!(super::AUTO_PASTE_POST_PASTE_COMMIT_DELAY_MS >= 250);
        assert!(
            super::AUTO_PASTE_POST_PASTE_COMMIT_DELAY_MS < AUTO_PASTE_RESTORE_CLIPBOARD_DELAY_MS
        );
    }

    #[test]
    fn successful_clipboard_restore_policy_keeps_text_on_macos() {
        assert_eq!(
            super::restore_clipboard_after_successful_paste_enabled(),
            !cfg!(target_os = "macos")
        );
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
        let method = super::paste_text_for_target(&marker, &target)?;
        let pasted_value =
            wait_for_focused_ax_value_containing(&target, &marker).with_context(|| {
                format!("TextEdit focused editor did not contain marker after {method:?} paste")
            })?;

        assert!(
            pasted_value.contains(&marker),
            "focused TextEdit AXValue must contain marker after {method:?} paste"
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
        let method = super::paste_text_for_target(&marker, &target)?;
        let title =
            wait_for_front_window_title_containing(target.pid, &marker).with_context(|| {
                format!("controlled textarea did not commit DOM input event after {method:?} paste")
            })?;

        assert!(
            title.contains(&marker),
            "controlled browser textarea must commit marker through DOM input event after {method:?} paste"
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
        if super::restore_clipboard_after_successful_paste_enabled() {
            assert_eq!(clipboard.text, "previous");
        } else {
            assert_eq!(clipboard.text, text);
        }
        assert!(injector.typed_texts.is_empty());
        assert_eq!(injector.paste_shortcut_calls, 1);
        assert_eq!(*events.borrow(), expected_events);
    }

    #[test]
    fn hybrid_skips_restore_when_clipboard_changed_after_paste() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let text = long_text();
        let mut clipboard = FakeClipboard::new("previous", events.clone());
        if super::restore_clipboard_after_successful_paste_enabled() {
            clipboard.change_on_get = Some((2, "user copy".to_string()));
        }
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
        if super::restore_clipboard_after_successful_paste_enabled() {
            assert_eq!(clipboard.text, "user copy");
        } else {
            assert_eq!(clipboard.text, text);
        }
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
            if super::restore_clipboard_after_successful_paste_enabled() {
                expected.push(format!("sleep:{}", AUTO_PASTE_RESTORE_CLIPBOARD_DELAY_MS));
            } else {
                expected.push(format!(
                    "sleep:{}",
                    super::AUTO_PASTE_POST_PASTE_COMMIT_DELAY_MS
                ));
            }
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
