mod audio_capture;
mod realtime_translation;
/// Domain ports - interfaces (traits) that define contracts for external dependencies
/// These abstractions allow the domain layer to remain independent of infrastructure
mod stt_provider;
mod system_audio_capture_factory;
mod translation_audio_output;

pub use audio_capture::*;
pub use realtime_translation::*;
pub use stt_provider::*;
pub use system_audio_capture_factory::*;
pub use translation_audio_output::*;
