use async_trait::async_trait;
use std::sync::Arc;

use crate::domain::models::{AudioChunk, SttConfig, Transcription};

/// Result type for STT operations
pub type SttResult<T> = Result<T, SttError>;

/// Errors that can occur during speech-to-text operations
#[derive(Debug, thiserror::Error, Clone)]
pub enum SttError {
    #[error("Configuration error: {0}")]
    Configuration(String),

    #[error("Connection error: {0}")]
    Connection(SttConnectionError),

    #[error("Authentication error: {0}")]
    Authentication(String),

    #[error("Processing error: {0}")]
    Processing(String),

    #[error("Unsupported operation: {0}")]
    Unsupported(String),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Continuation audio was positively not started; original drain remains authoritative")]
    ContinuationAudioNotStarted,
}

/// Более структурированная информация о сетевой/WS ошибке.
/// Нужна, чтобы UI мог показывать точную причину не на основе парсинга строки.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SttConnectionDetails {
    /// Категория ошибки (для понятного текста в UI).
    pub category: Option<SttConnectionCategory>,
    /// HTTP статус, если ошибка произошла на этапе handshake.
    pub http_status: Option<u16>,
    /// Close code WebSocket (если сервер закрыл соединение).
    pub ws_close_code: Option<u16>,
    /// std::io::ErrorKind, если есть (например ConnectionRefused).
    pub io_error_kind: Option<String>,
    /// raw OS error code (если доступно).
    pub os_error: Option<i32>,
    /// Код ошибки от сервера (если пришёл `ServerMessage::Error`).
    pub server_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SttConnectionCategory {
    Offline,
    Dns,
    Tls,
    Refused,
    Reset,
    Timeout,
    Http,
    RateLimited,
    LimitExceeded,
    ProviderQuotaExceeded,
    ServerUnavailable,
    ServerError,
    Closed,
    Unknown,
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct SttConnectionError {
    pub message: String,
    pub details: SttConnectionDetails,
}

impl SttConnectionError {
    pub fn simple(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            details: SttConnectionDetails::default(),
        }
    }

    pub fn with_category(message: impl Into<String>, category: SttConnectionCategory) -> Self {
        Self {
            message: message.into(),
            details: SttConnectionDetails {
                category: Some(category),
                ..Default::default()
            },
        }
    }
}

/// Callback type for receiving transcription updates
pub type TranscriptionCallback = Arc<dyn Fn(Transcription) + Send + Sync>;

/// Provenance of the current meter sample, distinct from capture ownership.
#[derive(Debug, Clone, Copy)]
pub struct AudioMeterSample {
    pub owner: super::AudioCaptureIdentity,
    pub captured_at_ms: i64,
}

/// Callback type for receiving audio level updates (0.0 - 1.0)
pub type AudioLevelCallback = Arc<dyn Fn(AudioMeterSample, f32) + Send + Sync>;

/// Callback type for receiving audio spectrum updates (48 bars, each 0.0 - 1.0)
pub type AudioSpectrumCallback = Arc<dyn Fn(AudioMeterSample, [f32; 48]) + Send + Sync>;

/// Callback type for receiving errors (error message, error type)
pub type ErrorCallback = Arc<dyn Fn(SttError) + Send + Sync>;

/// Callback type for receiving connection quality updates
/// Параметры: (quality: String, reason: Option<String>)
/// quality может быть: "Good", "Poor", "Recovering"
pub type ConnectionQualityCallback = Arc<dyn Fn(String, Option<String>) + Send + Sync>;

/// Trait defining the contract for speech-to-text providers
///
/// This abstraction allows switching between different STT implementations
/// (local whisper, cloud providers, etc.) without changing business logic.
///
/// Following the Dependency Inversion Principle (SOLID), the domain layer
/// defines this interface, and infrastructure layer provides implementations.
#[async_trait]
pub trait SttProvider: Send + Sync {
    /// Initialize the provider with configuration
    async fn initialize(&mut self, config: &SttConfig) -> SttResult<()>;

    /// Start streaming transcription session
    ///
    /// # Arguments
    /// * `on_partial` - Callback for partial transcription results
    /// * `on_final` - Callback for final transcription results
    /// * `on_error` - Callback for connection/processing errors
    /// * `on_connection_quality` - Callback for connection quality updates
    async fn start_stream(
        &mut self,
        on_partial: TranscriptionCallback,
        on_final: TranscriptionCallback,
        on_error: ErrorCallback,
        on_connection_quality: ConnectionQualityCallback,
    ) -> SttResult<()>;

    /// Send audio chunk for transcription
    ///
    /// This method should be called repeatedly with audio chunks
    /// during an active streaming session
    async fn send_audio(&mut self, chunk: &AudioChunk) -> SttResult<()>;

    /// Stop streaming and finalize transcription
    async fn stop_stream(&mut self) -> SttResult<()>;

    /// Abort current session without waiting for finalization
    async fn abort(&mut self) -> SttResult<()>;

    /// Pause streaming (keep connection alive but stop processing audio)
    /// Only supported by providers with keep_connection_alive capability
    async fn pause_stream(&mut self) -> SttResult<()> {
        Err(SttError::Unsupported(
            "pause_stream not supported by this provider".to_string(),
        ))
    }

    /// Resume streaming after pause (reactivate callbacks and audio processing)
    /// Only supported by providers with keep_connection_alive capability
    async fn resume_stream(
        &mut self,
        _on_partial: TranscriptionCallback,
        _on_final: TranscriptionCallback,
        _on_error: ErrorCallback,
        _on_connection_quality: ConnectionQualityCallback,
    ) -> SttResult<()> {
        Err(SttError::Unsupported(
            "resume_stream not supported by this provider".to_string(),
        ))
    }

    /// Immutable delivery selection, retained through teardown. None means unknown.
    fn continuation_delivery_mode(&self) -> Option<bool> {
        Some(self.continuation_lifecycle_session().is_some())
    }

    /// Immutable actual-Ready negotiation, absent on legacy/DG or before Ready.
    fn continuation_session(&self) -> Option<crate::domain::ContinuationSession> {
        None
    }

    /// Retained negotiation identity for lifecycle observation after a connection
    /// closes. This is not an audio/control admission permit.
    fn continuation_lifecycle_session(&self) -> Option<crate::domain::ContinuationSession> {
        self.continuation_session()
    }

    /// Receiver-owned lifecycle wakeups; callers inspect the immutable Ready or
    /// terminal evidence after waking. Notifications never imply readiness alone.
    fn continuation_lifecycle_notify(&self) -> Option<Arc<tokio::sync::Notify>> {
        None
    }

    /// Distinct from legacy pause/resume: retains callbacks and cumulative ACK ledger.
    /// One mutation, then bounded status recovery; never retries a mutation.
    async fn continuation_control(
        &mut self,
        _session: &crate::domain::ContinuationSession,
        _operation: crate::domain::ContinuationOperation,
        _deadline: tokio::time::Instant,
    ) -> SttResult<crate::domain::ContinuationControlResult> {
        Err(SttError::Unsupported("EL continuation unavailable".into()))
    }

    fn continuation_not_started(&self) -> Option<crate::domain::ContinuationNotStartedEvidence> {
        None
    }

    fn continuation_operation_identity(
        &self,
    ) -> Option<(String, crate::domain::ContinuationOperation)> {
        None
    }

    /// First B must cross the actual writer, not merely enter its batching queue.
    /// The transport marks attempted at its sink boundary after all pre-write waits.
    async fn send_first_continuation_audio(
        &mut self,
        _session: &crate::domain::ContinuationSession,
        _pause_epoch: u64,
        _chunk: &AudioChunk,
        _fence: &crate::domain::ContinuationWriteFence,
    ) -> SttResult<crate::domain::ContinuationFirstWrite> {
        Err(SttError::Unsupported(
            "First continuation audio unavailable".into(),
        ))
    }

    /// Evidence remains available after transport teardown.
    fn finalize_evidence(&self) -> Option<crate::domain::models::ProviderFinalizeReport> {
        None
    }

    fn audio_delivery_progress(&self) -> Option<crate::domain::models::AudioDeliveryProgress> {
        None
    }

    /// Optional upper bound for coalescing already queued normalized PCM samples.
    fn preferred_audio_batch_samples(&self) -> Option<usize> {
        None
    }

    /// Get provider name for identification
    fn name(&self) -> &str;

    /// Check if provider supports streaming
    fn supports_streaming(&self) -> bool {
        true
    }

    /// Check if provider supports keep-alive connections (persistent WebSocket between recordings)
    fn supports_keep_alive(&self) -> bool {
        false
    }

    /// Check if connection is currently alive (paused but not closed)
    fn is_connection_alive(&self) -> bool {
        false
    }

    /// TEST observation only: (actual Ready on current connection, local socket retained).
    /// None is unknown and must never qualify a live trial.
    #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
    fn native_e2e_transport_observation(&self) -> Option<(bool, bool)> {
        None
    }

    /// Check if provider is online (cloud-based)
    fn is_online(&self) -> bool;
}

/// Factory trait for creating STT providers
///
/// This allows dependency injection and makes testing easier
pub trait SttProviderFactory: Send + Sync {
    fn create(&self, config: &SttConfig) -> SttResult<Box<dyn SttProvider>>;
}
