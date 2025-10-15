use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{interval, Duration};

use crate::domain::{AudioCapture, AudioChunk, AudioChunkCallback, AudioConfig, AudioResult};

/// Mock audio capture for testing and development
///
/// This implementation generates synthetic audio data at regular intervals
pub struct MockAudioCapture {
    config: AudioConfig,
    is_capturing: Arc<RwLock<bool>>,
}

impl MockAudioCapture {
    pub fn new() -> Self {
        Self {
            config: AudioConfig::default(),
            is_capturing: Arc::new(RwLock::new(false)),
        }
    }
}

impl Default for MockAudioCapture {
    fn default() -> Self {
        Self::new()
    }
}

// Simple random number generator for testing
mod rand {
    use std::cell::Cell;

    thread_local! {
        static STATE: Cell<u64> = Cell::new(1);
    }

    pub fn random<T>() -> T
    where
        T: From<u16>,
    {
        STATE.with(|state| {
            let mut x = state.get();
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            state.set(x);
            T::from((x & 0xFFFF) as u16)
        })
    }
}

#[async_trait]
impl AudioCapture for MockAudioCapture {
    async fn initialize(&mut self, config: AudioConfig) -> AudioResult<()> {
        log::info!("MockAudioCapture: Initializing with config: {:?}", config);
        self.config = config;
        Ok(())
    }

    async fn start_capture(&mut self, on_chunk: AudioChunkCallback) -> AudioResult<()> {
        let mut is_capturing = self.is_capturing.write().await;

        if *is_capturing {
            return Err(crate::domain::AudioError::Capture(
                "Already capturing".to_string(),
            ));
        }

        *is_capturing = true;
        drop(is_capturing);

        log::info!("MockAudioCapture: Starting capture");

        let is_capturing_clone = self.is_capturing.clone();
        let config = self.config;

        // Spawn background task to generate audio chunks
        tokio::spawn(async move {
            // Calculate chunk size for ~100ms of audio
            let chunk_duration_ms = 100;
            let samples_per_chunk =
                (config.sample_rate as usize * chunk_duration_ms) / 1000 * config.channels as usize;

            let mut timer = interval(Duration::from_millis(chunk_duration_ms as u64));

            loop {
                timer.tick().await;

                let is_capturing = is_capturing_clone.read().await;
                if !*is_capturing {
                    break;
                }
                drop(is_capturing);

                // Generate synthetic chunk
                let mut data = vec![0i16; samples_per_chunk];
                for sample in data.iter_mut() {
                    let val = rand::random::<u16>() as i16;
                    *sample = (val % 100) - 50;
                }

                let chunk = AudioChunk::new(data, config.sample_rate, config.channels);

                log::debug!(
                    "MockAudioCapture: Generated chunk with {} samples",
                    chunk.data.len()
                );

                // Call callback
                on_chunk(chunk);
            }

            log::info!("MockAudioCapture: Capture loop ended");
        });

        Ok(())
    }

    async fn stop_capture(&mut self) -> AudioResult<()> {
        log::info!("MockAudioCapture: Stopping capture");
        *self.is_capturing.write().await = false;
        Ok(())
    }

    fn is_capturing(&self) -> bool {
        // This is not async, so we can't await here
        // For mock purposes, we'll return false
        // In production, you might use atomic bool
        false
    }

    fn config(&self) -> AudioConfig {
        self.config
    }
}
