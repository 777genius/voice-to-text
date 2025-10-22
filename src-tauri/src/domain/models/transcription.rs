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

    /// Start time of the audio segment in seconds (from Deepgram)
    pub start: f64,

    /// Duration of the audio segment in seconds (from Deepgram)
    pub duration: f64,
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
            start: 0.0,
            duration: 0.0,
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

    pub fn with_timing(mut self, start: f64, duration: f64) -> Self {
        self.start = start;
        self.duration = duration;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transcription_new() {
        let t = Transcription::new("hello".to_string(), true);
        assert_eq!(t.text, "hello");
        assert!(t.is_final);
        assert!(t.confidence.is_none());
        assert!(t.language.is_none());
        assert!(t.timestamp > 0);
    }

    #[test]
    fn test_transcription_partial() {
        let t = Transcription::partial("test".to_string());
        assert_eq!(t.text, "test");
        assert!(!t.is_final);
    }

    #[test]
    fn test_transcription_final_result() {
        let t = Transcription::final_result("done".to_string());
        assert_eq!(t.text, "done");
        assert!(t.is_final);
    }

    #[test]
    fn test_transcription_with_confidence() {
        let t = Transcription::new("test".to_string(), true)
            .with_confidence(0.95);
        assert_eq!(t.confidence, Some(0.95));
    }

    #[test]
    fn test_transcription_with_language() {
        let t = Transcription::new("test".to_string(), true)
            .with_language("en".to_string());
        assert_eq!(t.language, Some("en".to_string()));
    }

    #[test]
    fn test_transcription_builder_chain() {
        let t = Transcription::new("hello".to_string(), true)
            .with_confidence(0.8)
            .with_language("ru".to_string());
        assert_eq!(t.text, "hello");
        assert_eq!(t.confidence, Some(0.8));
        assert_eq!(t.language, Some("ru".to_string()));
    }

    #[test]
    fn test_transcription_clone() {
        let t1 = Transcription::new("test".to_string(), true);
        let t2 = t1.clone();
        assert_eq!(t1.text, t2.text);
        assert_eq!(t1.is_final, t2.is_final);
    }

    #[test]
    fn test_recording_status_default() {
        assert_eq!(RecordingStatus::default(), RecordingStatus::Idle);
    }

    #[test]
    fn test_recording_status_equality() {
        assert_eq!(RecordingStatus::Idle, RecordingStatus::Idle);
        assert_ne!(RecordingStatus::Idle, RecordingStatus::Recording);
    }
}
