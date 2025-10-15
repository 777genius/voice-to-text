use async_trait::async_trait;

use crate::domain::{
    AudioChunk, SttConfig, SttError, SttProvider, SttResult, TranscriptionCallback,
};

/// Local Whisper.cpp STT provider for offline transcription
///
/// TODO: Implement whisper.cpp integration
/// - Load model (base/small/medium/large)
/// - Buffer audio chunks
/// - Process accumulated audio
/// - Return transcription results
pub struct WhisperLocalProvider {
    config: Option<SttConfig>,
    is_streaming: bool,
    audio_buffer: Vec<i16>,
}

impl WhisperLocalProvider {
    pub fn new() -> Self {
        Self {
            config: None,
            is_streaming: false,
            audio_buffer: Vec::new(),
        }
    }
}

impl Default for WhisperLocalProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SttProvider for WhisperLocalProvider {
    async fn initialize(&mut self, config: &SttConfig) -> SttResult<()> {
        log::info!("WhisperLocalProvider: Initializing");

        let model = config
            .model
            .clone()
            .unwrap_or_else(|| "base".to_string());

        log::info!("WhisperLocalProvider: Using model: {}", model);

        // TODO: Load whisper.cpp model
        // 1. Check if model file exists
        // 2. Initialize whisper context
        // 3. Configure parameters (language, etc.)

        self.config = Some(config.clone());
        Ok(())
    }

    async fn start_stream(
        &mut self,
        _on_partial: TranscriptionCallback,
        _on_final: TranscriptionCallback,
    ) -> SttResult<()> {
        log::info!("WhisperLocalProvider: Starting stream (not implemented)");
        self.is_streaming = true;
        self.audio_buffer.clear();

        // TODO: Prepare for audio streaming
        // Note: whisper.cpp doesn't support true streaming yet,
        // so we'll buffer audio and process on stop

        Err(SttError::Unsupported(
            "Whisper Local provider not yet implemented".to_string(),
        ))
    }

    async fn send_audio(&mut self, chunk: &AudioChunk) -> SttResult<()> {
        if !self.is_streaming {
            return Err(SttError::Processing("Not streaming".to_string()));
        }

        // Buffer audio for processing
        self.audio_buffer.extend_from_slice(&chunk.data);

        // TODO: Optionally send partial results using streaming approach
        // (e.g., WhisperLive or similar streaming wrapper)

        Ok(())
    }

    async fn stop_stream(&mut self) -> SttResult<()> {
        log::info!("WhisperLocalProvider: Stopping stream");
        self.is_streaming = false;

        // TODO: Process accumulated audio buffer
        // 1. Convert buffer to format expected by whisper.cpp
        // 2. Run transcription
        // 3. Call on_final callback with result

        self.audio_buffer.clear();
        Ok(())
    }

    async fn abort(&mut self) -> SttResult<()> {
        log::info!("WhisperLocalProvider: Aborting stream");
        self.is_streaming = false;
        self.audio_buffer.clear();
        Ok(())
    }

    fn name(&self) -> &str {
        "Whisper Local"
    }

    fn is_online(&self) -> bool {
        false // Local provider is offline
    }
}
