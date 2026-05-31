/// Audio capture implementations
mod cpal_output;
#[cfg(target_os = "macos")]
mod macos_system_audio_capture;
mod mock_capture;
mod system_capture;
mod vad_capture_wrapper;
mod vad_processor;

pub use cpal_output::{
    AudioOutput, AudioOutputConfig, AudioOutputError, AudioOutputResult, CpalAudioOutput,
    ENV_TRANSLATION_OUTPUT_DEVICE,
};
#[cfg(target_os = "macos")]
pub use macos_system_audio_capture::MacosSystemAudioCapture;
pub use mock_capture::MockAudioCapture;
pub use system_capture::{SystemAudioCapture, SystemAudioCaptureOptions};
pub use vad_capture_wrapper::VadCaptureWrapper;
pub use vad_processor::{VadProcessor, VadResult};
