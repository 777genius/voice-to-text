use serde::Serialize;

use crate::domain::{RecordingStatus, Transcription};

/// Event names for Tauri event system
pub const EVENT_TRANSCRIPTION_PARTIAL: &str = "transcription:partial";
pub const EVENT_TRANSCRIPTION_FINAL: &str = "transcription:final";
pub const EVENT_RECORDING_STATUS: &str = "recording:status";
pub const EVENT_AUDIO_LEVEL: &str = "audio:level";
pub const EVENT_MICROPHONE_TEST_LEVEL: &str = "microphone_test:level";

pub const EVENT_TRANSCRIPTION_ERROR: &str = "transcription:error";

/// Payload for partial transcription event
#[derive(Debug, Clone, Serialize)]
pub struct PartialTranscriptionPayload {
    pub text: String,
    pub timestamp: i64,
    pub is_segment_final: bool, // true когда сегмент финализирован (is_final=true в Deepgram)
    pub start: f64, // start время utterance в секундах (от Deepgram)
    pub duration: f64, // длительность utterance в секундах (от Deepgram)
}

impl From<Transcription> for PartialTranscriptionPayload {
    fn from(t: Transcription) -> Self {
        Self {
            text: t.text,
            timestamp: t.timestamp,
            is_segment_final: t.is_final, // передаем флаг финализации сегмента
            start: t.start,
            duration: t.duration,
        }
    }
}

/// Payload for final transcription event
#[derive(Debug, Clone, Serialize)]
pub struct FinalTranscriptionPayload {
    pub text: String,
    pub confidence: Option<f32>,
    pub language: Option<String>,
    pub timestamp: i64,
}

impl From<Transcription> for FinalTranscriptionPayload {
    fn from(t: Transcription) -> Self {
        Self {
            text: t.text,
            confidence: t.confidence,
            language: t.language,
            timestamp: t.timestamp,
        }
    }
}

/// Payload for recording status event
#[derive(Debug, Clone, Serialize)]
pub struct RecordingStatusPayload {
    pub status: RecordingStatus,
    #[serde(default)]
    pub stopped_via_hotkey: bool,
}

/// Payload for audio level event
#[derive(Debug, Clone, Serialize)]
pub struct AudioLevelPayload {
    /// Normalized audio level (0.0 - 1.0)
    pub level: f32,
}

/// Payload for microphone test level event
#[derive(Debug, Clone, Serialize)]
pub struct MicrophoneTestLevelPayload {
    /// Normalized audio level (0.0 - 1.0)
    pub level: f32,
}

/// Payload for transcription error event
#[derive(Debug, Clone, Serialize)]
pub struct TranscriptionErrorPayload {
    pub error: String,
    pub error_type: String, // "connection", "configuration", "processing", "timeout", "authentication"
}
