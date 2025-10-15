use serde::{Deserialize, Serialize};

/// Represents the result of a speech-to-text transcription
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transcription {
    /// The transcribed text
    pub text: String,

    /// Indicates if this is a final transcription or partial
    pub is_final: bool,

    /// Confidence score (0.0 to 1.0), if available
    pub confidence: Option<f32>,

    /// Language detected or used
    pub language: Option<String>,

    /// Timestamp when transcription was created
    pub timestamp: i64,
}

impl Transcription {
    pub fn new(text: String, is_final: bool) -> Self {
        Self {
            text,
            is_final,
            confidence: None,
            language: None,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64,
        }
    }

    pub fn with_confidence(mut self, confidence: f32) -> Self {
        self.confidence = Some(confidence);
        self
    }

    pub fn with_language(mut self, language: String) -> Self {
        self.language = Some(language);
        self
    }

    /// Creates a partial transcription result
    pub fn partial(text: String) -> Self {
        Self::new(text, false)
    }

    /// Creates a final transcription result
    pub fn final_result(text: String) -> Self {
        Self::new(text, true)
    }
}

/// Recording status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecordingStatus {
    Idle,
    Starting, // Запись инициализируется (WebSocket подключается, audio capture запускается)
    Recording, // Запись активна и работает
    Processing,
    Error,
}

impl Default for RecordingStatus {
    fn default() -> Self {
        Self::Idle
    }
}
