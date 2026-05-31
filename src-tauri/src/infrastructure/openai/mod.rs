/// OpenAI integrations (realtime translation, etc.).
pub mod realtime_translation;
pub mod text_translation;

pub use realtime_translation::{
    OpenAIErrorKind, OpenAIRealtimeEvent, OpenAIRealtimeTranslationClient, OpenAITranslationError,
};
pub use text_translation::{OpenAITextTranslationClient, OpenAITextTranslationError};
