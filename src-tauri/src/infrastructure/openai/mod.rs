/// OpenAI integrations (realtime translation, etc.).
pub mod realtime_translation;

pub use realtime_translation::{
    OpenAIErrorKind, OpenAIRealtimeEvent, OpenAIRealtimeTranslationClient, OpenAITranslationError,
};
