use serde::Serialize;

use crate::domain::{RecordingStatus, Transcription};

/// Event names for Tauri event system
pub const EVENT_TRANSCRIPTION_PARTIAL: &str = "transcription:partial";
pub const EVENT_TRANSCRIPTION_FINAL: &str = "transcription:final";
pub const EVENT_RECORDING_STATUS: &str = "recording:status";
pub const EVENT_AUDIO_LEVEL: &str = "audio:level";
pub const EVENT_AUDIO_SPECTRUM: &str = "audio:spectrum";
pub const EVENT_MICROPHONE_TEST_LEVEL: &str = "microphone_test:level";

pub const EVENT_TRANSCRIPTION_ERROR: &str = "transcription:error";
pub const EVENT_CONNECTION_QUALITY: &str = "connection:quality";

// State-sync протокол: invalidation event для синхронизации между окнами
pub const EVENT_STATE_SYNC_INVALIDATION: &str = "state-sync:invalidation";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StateSyncInvalidationPayload {
    pub topic: String,
    pub revision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    pub timestamp_ms: i64,
}

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

/// Payload for audio spectrum event
#[derive(Debug, Clone, Serialize)]
pub struct AudioSpectrumPayload {
    /// Normalized bars (48 values, each 0.0 - 1.0)
    pub bars: Vec<f32>,
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

/// Connection quality states
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub enum ConnectionQuality {
    /// Connection is working normally
    Good,
    /// Connection has issues (slow, errors)
    Poor,
    /// Connection is recovering from issues
    Recovering,
}

/// Payload for connection quality event
#[derive(Debug, Clone, Serialize)]
pub struct ConnectionQualityPayload {
    pub quality: ConnectionQuality,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>, // дополнительная информация о причине
}
