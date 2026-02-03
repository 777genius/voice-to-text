use serde::Serialize;

use crate::domain::{RecordingStatus, Transcription};
use crate::domain::{SttConnectionCategory, SttConnectionDetails};

/// Event names for Tauri event system
pub const EVENT_TRANSCRIPTION_PARTIAL: &str = "transcription:partial";
pub const EVENT_TRANSCRIPTION_FINAL: &str = "transcription:final";
pub const EVENT_RECORDING_STATUS: &str = "recording:status";
pub const EVENT_AUDIO_LEVEL: &str = "audio:level";
pub const EVENT_AUDIO_SPECTRUM: &str = "audio:spectrum";
pub const EVENT_MICROPHONE_TEST_LEVEL: &str = "microphone_test:level";

pub const EVENT_TRANSCRIPTION_ERROR: &str = "transcription:error";
pub const EVENT_CONNECTION_QUALITY: &str = "connection:quality";

// UI lifecycle events
// Важно: это не "focus", потому что main окно на macOS может быть nonactivating NSPanel и не получать фокус.
pub const EVENT_RECORDING_WINDOW_SHOWN: &str = "recording:window-shown";

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
    /// Уникальный идентификатор сессии записи (монотонно растёт).
    /// Нужен, чтобы frontend мог игнорировать "поздние" события от предыдущей сессии.
    pub session_id: u64,
    pub text: String,
    pub timestamp: i64,
    pub is_segment_final: bool, // true когда сегмент финализирован (is_final=true в Deepgram)
    pub start: f64, // start время utterance в секундах (от Deepgram)
    pub duration: f64, // длительность utterance в секундах (от Deepgram)
}

impl PartialTranscriptionPayload {
    pub fn from_transcription(t: Transcription, session_id: u64) -> Self {
        Self {
            session_id,
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
    /// Уникальный идентификатор сессии записи (монотонно растёт).
    pub session_id: u64,
    pub text: String,
    pub confidence: Option<f32>,
    pub language: Option<String>,
    pub timestamp: i64,
}

impl FinalTranscriptionPayload {
    pub fn from_transcription(t: Transcription, session_id: u64) -> Self {
        Self {
            session_id,
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
    /// Уникальный идентификатор сессии записи (монотонно растёт).
    pub session_id: u64,
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
    pub session_id: u64,
    pub error: String,
    pub error_type: String, // "connection", "configuration", "processing", "timeout", "authentication"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_details: Option<TranscriptionErrorDetailsPayload>,
}

/// Детали ошибки для UI (сериализуемый формат).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptionErrorDetailsPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ws_close_code: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub io_error_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_error: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_code: Option<String>,
}

impl From<SttConnectionDetails> for TranscriptionErrorDetailsPayload {
    fn from(value: SttConnectionDetails) -> Self {
        Self {
            category: value.category.map(stt_category_to_string),
            http_status: value.http_status,
            ws_close_code: value.ws_close_code,
            io_error_kind: value.io_error_kind,
            os_error: value.os_error,
            server_code: value.server_code,
        }
    }
}

fn stt_category_to_string(cat: SttConnectionCategory) -> String {
    match cat {
        SttConnectionCategory::Offline => "offline",
        SttConnectionCategory::Dns => "dns",
        SttConnectionCategory::Tls => "tls",
        SttConnectionCategory::Refused => "refused",
        SttConnectionCategory::Reset => "reset",
        SttConnectionCategory::Timeout => "timeout",
        SttConnectionCategory::Http => "http",
        SttConnectionCategory::ServerUnavailable => "server_unavailable",
        SttConnectionCategory::Closed => "closed",
        SttConnectionCategory::Unknown => "unknown",
    }
    .to_string()
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
    pub session_id: u64,
    pub quality: ConnectionQuality,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>, // дополнительная информация о причине
}
