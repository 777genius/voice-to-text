/// Audio capture implementations
mod cpal_output;
#[cfg_attr(all(test, not(target_os = "linux")), allow(dead_code))]
#[cfg(any(target_os = "linux", test))]
mod linux_pulse;
mod local_playback_factory;
mod macos_spoken_translation_capability;
#[cfg(target_os = "macos")]
mod macos_system_audio_capture;
mod mock_capture;
mod platform_factory;
mod system_capture;
mod vad_capture_wrapper;
mod vad_processor;
#[cfg(any(target_os = "macos", test))]
mod warm_capture_gate;
#[cfg(target_os = "macos")]
mod warm_cpal;
#[cfg(any(target_os = "macos", test))]
mod warm_dictation_input;
#[cfg(any(target_os = "macos", test))]
mod warm_pcm;
#[cfg(target_os = "windows")]
mod windows_wasapi_loopback_capture;

pub use cpal_output::{
    AudioOutput, AudioOutputConfig, AudioOutputError, AudioOutputResult, CpalAudioOutput,
    ENV_TRANSLATION_OUTPUT_DEVICE, MACOS_BLACKHOLE_DEVICE_NAMES,
    WINDOWS_VB_CABLE_OUTPUT_DEVICE_NAMES,
};
pub use local_playback_factory::DefaultLocalPlaybackOutputFactory;
pub use macos_spoken_translation_capability::DefaultSpokenTranslationCapability;
#[cfg(target_os = "macos")]
pub use macos_system_audio_capture::MacosSystemAudioCapture;
pub use mock_capture::MockAudioCapture;
pub use platform_factory::{is_macos_blackhole_device_name, DefaultPlatformAudioFactory};
pub use system_capture::{SystemAudioCapture, SystemAudioCaptureOptions};
pub use vad_capture_wrapper::VadCaptureWrapper;
pub use vad_processor::{VadProcessor, VadResult};
#[cfg(any(target_os = "macos", test))]
pub(crate) use warm_dictation_input::{WarmDictationInput, WarmDictationLease};
#[cfg(any(
    test,
    all(debug_assertions, feature = "native-window-e2e", target_os = "macos")
))]
pub(crate) use warm_dictation_input::{
    WarmInputFormat, WarmNativeFactory, WarmNativeInput, WarmRawBlock, WarmRawCallback,
};
#[cfg(target_os = "windows")]
pub use windows_wasapi_loopback_capture::WindowsWasapiLoopbackCapture;
