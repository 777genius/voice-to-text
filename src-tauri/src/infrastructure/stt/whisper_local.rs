use async_trait::async_trait;

use crate::domain::{
    AudioChunk, SttConfig, SttError, SttProvider, SttResult, TranscriptionCallback,
};

// Полная реализация с whisper-rs (требуется feature "whisper" и cmake)
#[cfg(feature = "whisper")]
mod whisper_impl {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;
    use whisper_rs::{WhisperContext, WhisperContextParameters, FullParams, SamplingStrategy};

    pub struct WhisperLocalProvider {
        config: Option<SttConfig>,
        is_streaming: bool,
        audio_buffer: Vec<i16>,
        whisper_ctx: Option<Arc<WhisperContext>>,
        on_final_callback: Option<TranscriptionCallback>,
    }

    impl WhisperLocalProvider {
        pub fn new() -> Self {
            Self {
                config: None,
                is_streaming: false,
                audio_buffer: Vec::new(),
                whisper_ctx: None,
                on_final_callback: None,
            }
        }

        fn get_model_path(model_name: &str) -> SttResult<PathBuf> {
            let app_data_dir = dirs::data_dir()
                .ok_or_else(|| SttError::Configuration("Cannot determine app data directory".to_string()))?;

            let models_dir = app_data_dir.join("voice-to-text").join("models");
            let model_file = models_dir.join(format!("ggml-{}.bin", model_name));

            if !model_file.exists() {
                return Err(SttError::Configuration(format!(
                    "Model file not found: {}. Please download the model first.",
                    model_file.display()
                )));
            }

            Ok(model_file)
        }

        fn convert_audio_to_f32(samples: &[i16]) -> Vec<f32> {
            samples.iter().map(|&s| s as f32 / 32768.0).collect()
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

            let model_name = config
                .model
                .clone()
                .unwrap_or_else(|| "base".to_string());

            log::info!("WhisperLocalProvider: Using model: {}", model_name);

            let model_path = Self::get_model_path(&model_name)?;
            log::info!("WhisperLocalProvider: Loading model from: {}", model_path.display());

            let model_path_clone = model_path.clone();
            let whisper_ctx = tokio::task::spawn_blocking(move || {
                let params = WhisperContextParameters::default();
                WhisperContext::new_with_params(&model_path_clone.to_string_lossy(), params)
                    .map_err(|e| SttError::Internal(format!("Failed to load Whisper model: {}", e)))
            })
            .await
            .map_err(|e| SttError::Internal(format!("Failed to spawn model loading task: {}", e)))??;

            self.whisper_ctx = Some(Arc::new(whisper_ctx));
            self.config = Some(config.clone());

            log::info!("WhisperLocalProvider: Model loaded successfully");
            Ok(())
        }

        async fn start_stream(
            &mut self,
            _on_partial: TranscriptionCallback,
            on_final: TranscriptionCallback,
            _on_error: crate::domain::ErrorCallback,
        ) -> SttResult<()> {
            log::info!("WhisperLocalProvider: Starting stream (buffering mode)");

            if self.whisper_ctx.is_none() {
                return Err(SttError::Configuration(
                    "Whisper context not initialized. Call initialize() first.".to_string(),
                ));
            }

            self.is_streaming = true;
            self.audio_buffer.clear();
            self.on_final_callback = Some(on_final);

            log::info!("WhisperLocalProvider: Ready to buffer audio");
            Ok(())
        }

        async fn send_audio(&mut self, chunk: &AudioChunk) -> SttResult<()> {
            if !self.is_streaming {
                return Err(SttError::Processing("Not streaming".to_string()));
            }

            self.audio_buffer.extend_from_slice(&chunk.data);

            if self.audio_buffer.len() % (16000 * 2) == 0 {
                let duration_sec = self.audio_buffer.len() / 16000;
                log::debug!("WhisperLocalProvider: Buffered {}s of audio", duration_sec);
            }

            Ok(())
        }

        async fn stop_stream(&mut self) -> SttResult<()> {
            log::info!("WhisperLocalProvider: Stopping stream and processing audio");
            self.is_streaming = false;

            if self.audio_buffer.is_empty() {
                log::warn!("WhisperLocalProvider: No audio to process");
                self.audio_buffer.clear();
                return Ok(());
            }

            let duration_sec = self.audio_buffer.len() as f32 / 16000.0;
            log::info!("WhisperLocalProvider: Processing {:.2}s of audio ({} samples)",
                duration_sec, self.audio_buffer.len());

            let ctx = self.whisper_ctx.as_ref()
                .ok_or_else(|| SttError::Internal("Whisper context not available".to_string()))?
                .clone();

            let callback = self.on_final_callback.as_ref()
                .ok_or_else(|| SttError::Internal("Final callback not set".to_string()))?
                .clone();

            let audio_f32 = Self::convert_audio_to_f32(&self.audio_buffer);
            self.audio_buffer.clear();

            let language = self.config.as_ref()
                .and_then(|c| Some(c.language.clone()))
                .unwrap_or_else(|| "ru".to_string());

            let start_time = std::time::Instant::now();

            let transcription_result = tokio::task::spawn_blocking(move || {
                let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
                params.set_language(Some(&language));
                params.set_translate(false);
                params.set_print_progress(false);
                params.set_print_special(false);
                params.set_print_realtime(false);
                params.set_n_threads(num_cpus::get() as i32);

                let mut state = ctx.create_state()
                    .map_err(|e| SttError::Internal(format!("Failed to create Whisper state: {}", e)))?;

                state.full(params, &audio_f32)
                    .map_err(|e| SttError::Processing(format!("Transcription failed: {}", e)))?;

                let num_segments = state.full_n_segments()
                    .map_err(|e| SttError::Processing(format!("Failed to get segments: {}", e)))?;

                let mut full_text = String::new();
                for i in 0..num_segments {
                    match state.full_get_segment_text(i) {
                        Ok(segment_text) => {
                            full_text.push_str(&segment_text);
                            full_text.push(' ');
                        }
                        Err(e) => {
                            log::warn!("Failed to get segment {} text: {}", i, e);
                        }
                    }
                }

                Ok::<String, SttError>(full_text.trim().to_string())
            })
            .await
            .map_err(|e| SttError::Internal(format!("Transcription task failed: {}", e)))??;

            let elapsed = start_time.elapsed();
            log::info!("WhisperLocalProvider: Transcription completed in {:.2}s: '{}'",
                elapsed.as_secs_f32(), transcription_result);

            let transcription = Transcription {
                text: transcription_result,
                is_final: true,
                confidence: None,
                language: Some(language),
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_else(|_| std::time::Duration::from_secs(0))
                    .as_secs() as i64,
                start: 0.0, // Whisper Local не предоставляет start время
                duration: 0.0, // Whisper Local не предоставляет duration
            };

            callback(transcription);

            log::info!("WhisperLocalProvider: Stream stopped");
            Ok(())
        }

        async fn abort(&mut self) -> SttResult<()> {
            log::info!("WhisperLocalProvider: Aborting stream");
            self.is_streaming = false;
            self.audio_buffer.clear();
            self.on_final_callback = None;

            log::info!("WhisperLocalProvider: Stream aborted");
            Ok(())
        }

        fn name(&self) -> &str {
            "Whisper Local (Offline)"
        }

        fn is_online(&self) -> bool {
            false
        }
    }
}

// Заглушка когда whisper feature не включен
#[cfg(not(feature = "whisper"))]
mod whisper_impl {
    use super::*;

    pub struct WhisperLocalProvider {
        config: Option<SttConfig>,
    }

    impl WhisperLocalProvider {
        pub fn new() -> Self {
            Self { config: None }
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
            self.config = Some(config.clone());
            log::warn!("WhisperLocalProvider is not available in this build");
            Err(SttError::Configuration(
                "Whisper Local provider is not available in this build. \
                 Install cmake and rebuild with: cargo build --features whisper"
                    .to_string(),
            ))
        }

        async fn start_stream(
            &mut self,
            _on_partial: TranscriptionCallback,
            _on_final: TranscriptionCallback,
            _on_error: crate::domain::ErrorCallback,
            _on_connection_quality: crate::domain::ConnectionQualityCallback,
        ) -> SttResult<()> {
            Err(SttError::Configuration(
                "Whisper Local provider is not available".to_string(),
            ))
        }

        async fn send_audio(&mut self, _chunk: &AudioChunk) -> SttResult<()> {
            Err(SttError::Configuration(
                "Whisper Local provider is not available".to_string(),
            ))
        }

        async fn stop_stream(&mut self) -> SttResult<()> {
            Err(SttError::Configuration(
                "Whisper Local provider is not available".to_string(),
            ))
        }

        async fn abort(&mut self) -> SttResult<()> {
            Ok(())
        }

        fn name(&self) -> &str {
            "Whisper Local (Not Available - rebuild with --features whisper)"
        }

        fn is_online(&self) -> bool {
            false
        }
    }
}

// Экспортируем реализацию (либо полную либо заглушку)
pub use whisper_impl::WhisperLocalProvider;
