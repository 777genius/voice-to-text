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
    ServerUnavailable,
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

/// Callback type for receiving audio level updates (0.0 - 1.0)
pub type AudioLevelCallback = Arc<dyn Fn(f32) + Send + Sync>;

/// Callback type for receiving audio spectrum updates (48 bars, each 0.0 - 1.0)
pub type AudioSpectrumCallback = Arc<dyn Fn([f32; 48]) + Send + Sync>;

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

    /// Check if provider is online (cloud-based)
    fn is_online(&self) -> bool;
}

/// Factory trait for creating STT providers
///
/// This allows dependency injection and makes testing easier
pub trait SttProviderFactory: Send + Sync {
    fn create(&self, config: &SttConfig) -> SttResult<Box<dyn SttProvider>>;
}
