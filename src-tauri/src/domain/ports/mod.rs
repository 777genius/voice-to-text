mod audio_capture;
mod local_playback_output_factory;
mod realtime_translation;
mod spoken_translation_capability;
/// Domain ports - interfaces (traits) that define contracts for external dependencies
/// These abstractions allow the domain layer to remain independent of infrastructure
mod stt_provider;
mod system_audio_capture_factory;
mod translation_audio_output;

pub use audio_capture::*;
pub use local_playback_output_factory::*;
pub use realtime_translation::*;
pub use spoken_translation_capability::*;
pub use stt_provider::*;
pub use system_audio_capture_factory::*;
pub use translation_audio_output::*;
