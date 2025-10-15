use async_trait::async_trait;
use std::sync::Arc;

use crate::domain::models::{AudioChunk, AudioConfig};

/// Result type for audio capture operations
pub type AudioResult<T> = Result<T, AudioError>;

/// Errors that can occur during audio capture
#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("Device not found: {0}")]
    DeviceNotFound(String),

    #[error("Device access denied: {0}")]
    AccessDenied(String),

    #[error("Configuration error: {0}")]
    Configuration(String),

    #[error("Capture error: {0}")]
    Capture(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

/// Callback type for receiving audio chunks
pub type AudioChunkCallback = Arc<dyn Fn(AudioChunk) + Send + Sync>;

/// Trait defining the contract for audio capture
///
/// This abstraction allows switching between different audio capture implementations
/// (system microphone, file input, etc.) without changing business logic.
#[async_trait]
pub trait AudioCapture: Send + Sync {
    /// Initialize audio capture with configuration
    async fn initialize(&mut self, config: AudioConfig) -> AudioResult<()>;

    /// Start capturing audio
    ///
    /// # Arguments
    /// * `on_chunk` - Callback invoked for each audio chunk captured
    async fn start_capture(&mut self, on_chunk: AudioChunkCallback) -> AudioResult<()>;

    /// Stop capturing audio
    async fn stop_capture(&mut self) -> AudioResult<()>;

    /// Check if currently capturing
    fn is_capturing(&self) -> bool;

    /// Get current audio configuration
    fn config(&self) -> AudioConfig;
}
