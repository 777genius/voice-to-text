use async_trait::async_trait;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use cpal::traits::{DeviceTrait, HostTrait};

use crate::domain::{
    AudioCapture, AudioCaptureTarget, AudioError, AudioResult, PlatformAudioFactory,
    PlatformAudioSetupState, PlatformAudioSetupStatus, SystemAudioCaptureFactory,
    SystemAudioCaptureRequest, TranslationAudioOutput, TranslationAudioOutputResult,
};

#[cfg(any(target_os = "macos", target_os = "windows"))]
use super::CpalAudioOutput;
use super::SystemAudioCapture;
#[cfg(target_os = "macos")]
use super::MACOS_BLACKHOLE_DEVICE_NAMES;
#[cfg(target_os = "windows")]
use super::WINDOWS_VB_CABLE_OUTPUT_DEVICE_NAMES;

#[cfg(target_os = "linux")]
use super::linux_pulse::{
    linux_pulse_setup_status, LinuxPulseAudioOutput, LinuxPulseMonitorCapture,
    LINUX_VIRTUAL_MICROPHONE_DESCRIPTION,
};
#[cfg(target_os = "macos")]
use super::MacosSystemAudioCapture;
#[cfg(target_os = "windows")]
use super::WindowsWasapiLoopbackCapture;

#[derive(Debug, Default)]
pub struct DefaultPlatformAudioFactory;

impl DefaultPlatformAudioFactory {
    pub fn new() -> Self {
        Self
    }
}

impl SystemAudioCaptureFactory for DefaultPlatformAudioFactory {
    fn preflight_system_audio_capture(
        &self,
        request: SystemAudioCaptureRequest,
    ) -> AudioResult<()> {
        #[cfg(target_os = "macos")]
        {
            MacosSystemAudioCapture::preflight(request)
        }

        #[cfg(not(target_os = "macos"))]
        {
            let _ = request;
            Err(AudioError::Configuration(format!(
                "Isolated realtime system audio capture is unsupported on {}",
                std::env::consts::OS
            )))
        }
    }

    fn create_system_audio_capture(
        &self,
        request: SystemAudioCaptureRequest,
    ) -> AudioResult<Box<dyn AudioCapture>> {
        #[cfg(target_os = "macos")]
        {
            Ok(Box::new(MacosSystemAudioCapture::new(request)?))
        }

        #[cfg(not(target_os = "macos"))]
        {
            let _ = request;
            Err(AudioError::Configuration(format!(
                "Isolated realtime system audio capture is unsupported on {}",
                std::env::consts::OS
            )))
        }
    }
}

#[async_trait]
impl PlatformAudioFactory for DefaultPlatformAudioFactory {
    fn create_microphone_capture(
        &self,
        device_name: Option<String>,
        target: AudioCaptureTarget,
    ) -> AudioResult<Box<dyn AudioCapture>> {
        let capture = SystemAudioCapture::with_device_and_target(device_name, target)?;
        if let Some(selected_name) = capture.device_name() {
            if self.is_virtual_microphone_input(&selected_name) {
                return Err(AudioError::Configuration(format!(
                    "'{}' is a virtual translation microphone. Select a real microphone in VoicetextAI and select the virtual microphone only in Meet/Zoom.",
                    selected_name
                )));
            }
        }
        Ok(Box::new(capture))
    }

    fn create_translation_output(
        &self,
    ) -> TranslationAudioOutputResult<Box<dyn TranslationAudioOutput>> {
        #[cfg(target_os = "macos")]
        {
            Ok(Box::new(CpalAudioOutput::macos_blackhole()))
        }

        #[cfg(target_os = "windows")]
        {
            Ok(Box::new(CpalAudioOutput::windows_vb_cable()))
        }

        #[cfg(target_os = "linux")]
        {
            Ok(Box::new(LinuxPulseAudioOutput::new_default()))
        }

        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        {
            Err(crate::domain::TranslationAudioOutputError::Configuration(
                "Live translation virtual microphone output is not supported on this platform"
                    .to_string(),
            ))
        }
    }

    fn create_system_loopback_capture(
        &self,
        target: AudioCaptureTarget,
    ) -> AudioResult<Box<dyn AudioCapture>> {
        #[cfg(target_os = "macos")]
        {
            self.create_system_audio_capture(SystemAudioCaptureRequest::isolated(target))
        }

        #[cfg(target_os = "windows")]
        {
            Ok(Box::new(WindowsWasapiLoopbackCapture::new(target)?))
        }

        #[cfg(target_os = "linux")]
        {
            Ok(Box::new(LinuxPulseMonitorCapture::new_default(target)))
        }

        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        {
            let _ = target;
            Err(AudioError::Configuration(
                "Incoming system audio translation is not supported on this platform".to_string(),
            ))
        }
    }

    async fn setup_status(&self) -> PlatformAudioSetupStatus {
        #[cfg(target_os = "macos")]
        {
            macos_setup_status()
        }

        #[cfg(target_os = "windows")]
        {
            windows_setup_status()
        }

        #[cfg(target_os = "linux")]
        {
            linux_pulse_setup_status().await
        }

        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        {
            PlatformAudioSetupStatus {
                platform: std::env::consts::OS.to_string(),
                status: PlatformAudioSetupState::Unsupported,
                outgoing_supported: false,
                incoming_supported: false,
                virtual_microphone_name: String::new(),
                message: "Live translation audio routing is not supported on this platform"
                    .to_string(),
            }
        }
    }

    fn is_virtual_microphone_input(&self, name: &str) -> bool {
        #[cfg(target_os = "macos")]
        {
            is_macos_blackhole_device_name(name)
        }

        #[cfg(target_os = "windows")]
        {
            is_windows_virtual_cable_microphone_name(name)
        }

        #[cfg(target_os = "linux")]
        {
            is_linux_virtual_microphone_name(name)
        }

        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        {
            let _ = name;
            false
        }
    }

    fn microphone_preflight(&self) -> Result<(), AudioError> {
        #[cfg(target_os = "macos")]
        {
            use crate::infrastructure::microphone_permission::{
                microphone_permission_status, MicrophonePermissionStatus,
            };

            match microphone_permission_status() {
                MicrophonePermissionStatus::Authorized
                | MicrophonePermissionStatus::NotDetermined => Ok(()),
                _ => Err(AudioError::AccessDenied(
                    "Нет доступа к микрофону. Откройте macOS System Settings -> Privacy & Security -> Microphone и включите доступ для приложения."
                        .to_string(),
                )),
            }
        }

        #[cfg(not(target_os = "macos"))]
        {
            Ok(())
        }
    }
}

#[cfg(target_os = "macos")]
fn macos_setup_status() -> PlatformAudioSetupStatus {
    let ready = output_device_exists(MACOS_BLACKHOLE_DEVICE_NAMES);
    PlatformAudioSetupStatus {
        platform: "macos".to_string(),
        status: if ready {
            PlatformAudioSetupState::Ready
        } else {
            PlatformAudioSetupState::MissingVirtualDevice
        },
        outgoing_supported: ready,
        incoming_supported: true,
        virtual_microphone_name: "BlackHole 2ch".to_string(),
        message: if ready {
            "BlackHole 2ch is ready. Select BlackHole 2ch as microphone in Meet/Zoom.".to_string()
        } else {
            "Install BlackHole 2ch, reboot macOS, then select BlackHole 2ch as microphone in Meet/Zoom."
                .to_string()
        },
    }
}

#[cfg(target_os = "windows")]
fn windows_setup_status() -> PlatformAudioSetupStatus {
    let ready = output_device_exists(WINDOWS_VB_CABLE_OUTPUT_DEVICE_NAMES);
    PlatformAudioSetupStatus {
        platform: "windows".to_string(),
        status: if ready {
            PlatformAudioSetupState::Ready
        } else {
            PlatformAudioSetupState::MissingVirtualDevice
        },
        outgoing_supported: ready,
        incoming_supported: true,
        virtual_microphone_name: "CABLE Output".to_string(),
        message: if ready {
            "VB-CABLE is ready. Select CABLE Output as microphone in Meet/Zoom.".to_string()
        } else {
            "Install VB-Audio Virtual Cable, reboot Windows, then select CABLE Output as microphone in Meet/Zoom."
                .to_string()
        },
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn output_device_exists(candidates: &[&str]) -> bool {
    let host = cpal::default_host();
    let Ok(devices) = host.output_devices() else {
        return false;
    };

    let override_name = std::env::var(super::ENV_TRANSLATION_OUTPUT_DEVICE)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    devices.filter_map(|device| device.name().ok()).any(|name| {
        if let Some(override_name) = override_name.as_deref() {
            return normalized_contains(&name, override_name);
        }
        candidates
            .iter()
            .any(|candidate| normalized_contains(&name, candidate))
    })
}

pub fn is_macos_blackhole_device_name(name: &str) -> bool {
    normalized_contains_any(name, &["blackhole"])
}

#[cfg(any(target_os = "windows", test))]
pub fn is_windows_virtual_cable_microphone_name(name: &str) -> bool {
    normalized_contains_any(name, &["cable output"])
}

#[cfg(target_os = "linux")]
pub fn is_linux_virtual_microphone_name(name: &str) -> bool {
    normalized_contains_any(
        name,
        &[
            "voicetextai virtual microphone",
            "voicetext_translation_mic",
            LINUX_VIRTUAL_MICROPHONE_DESCRIPTION,
        ],
    )
}

fn normalized_contains_any(name: &str, needles: &[&str]) -> bool {
    needles
        .iter()
        .any(|needle| normalized_contains(name, needle))
}

fn normalized_contains(name: &str, needle: &str) -> bool {
    name.to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_cable_output_is_rejected_as_app_microphone() {
        assert!(is_windows_virtual_cable_microphone_name(
            "Microphone (CABLE Output VB-Audio Virtual Cable)"
        ));
        assert!(!is_windows_virtual_cable_microphone_name(
            "Speakers (CABLE Input VB-Audio Virtual Cable)"
        ));
    }

    #[test]
    fn macos_blackhole_is_rejected_as_app_microphone() {
        assert!(is_macos_blackhole_device_name("BlackHole 2ch"));
        assert!(!is_macos_blackhole_device_name("MacBook Pro Microphone"));
    }
}
