// Подавляем warnings от старой версии objc crate (см. auto_paste.rs).
#![allow(unexpected_cfgs)]

use anyhow::{Context, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicrophonePermissionStatus {
    NotDetermined,
    Restricted,
    Denied,
    Authorized,
    Unknown(i32),
}

#[cfg(target_os = "macos")]
fn status_from_raw(v: i32) -> MicrophonePermissionStatus {
    match v {
        0 => MicrophonePermissionStatus::NotDetermined,
        1 => MicrophonePermissionStatus::Restricted,
        2 => MicrophonePermissionStatus::Denied,
        3 => MicrophonePermissionStatus::Authorized,
        other => MicrophonePermissionStatus::Unknown(other),
    }
}

#[cfg(target_os = "macos")]
pub fn microphone_permission_status() -> MicrophonePermissionStatus {
    use cocoa::base::id;
    use objc::{class, msg_send, sel, sel_impl};

    // Подключаем фреймворк, чтобы гарантировать наличие классов AVFoundation.
    #[link(name = "AVFoundation", kind = "framework")]
    extern "C" {}

    unsafe {
        // AVMediaTypeAudio == "soun"
        let media: id = msg_send![class!(NSString), stringWithUTF8String: b"soun\0".as_ptr() as *const i8];
        let raw: i32 = msg_send![class!(AVCaptureDevice), authorizationStatusForMediaType: media];
        let status = status_from_raw(raw);

        if status != MicrophonePermissionStatus::Authorized {
            log::warn!("❌ Microphone permission not granted: {:?}", status);
        } else {
            log::info!("✅ Microphone permission granted");
        }

        status
    }
}

#[cfg(not(target_os = "macos"))]
pub fn microphone_permission_status() -> MicrophonePermissionStatus {
    // На Windows/Linux отдельный runtime-check не нужен.
    MicrophonePermissionStatus::Authorized
}

pub fn has_microphone_permission() -> bool {
    matches!(
        microphone_permission_status(),
        MicrophonePermissionStatus::Authorized
    )
}

#[cfg(target_os = "macos")]
pub fn open_microphone_settings() -> Result<()> {
    use std::process::Command;

    let status = Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")
        .status()
        .context("Failed to open System Settings")?;

    if !status.success() {
        anyhow::bail!("Failed to open Microphone settings");
    }

    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn open_microphone_settings() -> Result<()> {
    Ok(())
}

