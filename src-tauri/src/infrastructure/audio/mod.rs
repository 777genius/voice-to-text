/// Audio capture implementations

mod mock_capture;
mod vad_processor;
mod system_capture;
mod vad_capture_wrapper;

pub use mock_capture::MockAudioCapture;
pub use vad_processor::{VadProcessor, VadResult};
pub use system_capture::SystemAudioCapture;
pub use vad_capture_wrapper::VadCaptureWrapper;
