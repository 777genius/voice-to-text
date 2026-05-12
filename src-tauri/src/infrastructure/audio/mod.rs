/// Audio capture implementations
mod mock_capture;
mod system_capture;
mod vad_capture_wrapper;
mod vad_processor;

pub use mock_capture::MockAudioCapture;
pub use system_capture::SystemAudioCapture;
pub use vad_capture_wrapper::VadCaptureWrapper;
pub use vad_processor::{VadProcessor, VadResult};
