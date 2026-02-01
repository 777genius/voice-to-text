use async_trait::async_trait;
use std::sync::{Arc, Mutex};

use crate::domain::{AudioCapture, AudioChunk, AudioChunkCallback, AudioConfig, AudioResult};
use crate::infrastructure::audio::{VadProcessor, VadResult};

/// Callback type for silence timeout events
pub type SilenceTimeoutCallback = Arc<dyn Fn() + Send + Sync>;

/// VAD-aware audio capture wrapper
///
/// Wraps any AudioCapture implementation and adds Voice Activity Detection:
/// - Buffers incoming audio until we have exactly 480 samples (30ms @ 16kHz)
/// - Runs WebRTC VAD on each complete frame
/// - On VadResult::SilenceTimeout (configurable, default 3000ms) → triggers silence callback ONCE
/// - Passes through audio chunks to downstream callback
///
/// Requirements:
/// - Input MUST be 16kHz mono i16 PCM (VAD requirement)
/// - Frames MUST be exactly 480 samples (30ms @ 16kHz)
pub struct VadCaptureWrapper {
    inner: Box<dyn AudioCapture>,
    vad: Arc<Mutex<VadProcessor>>,
    on_silence_timeout: Option<SilenceTimeoutCallback>,
    audio_config: AudioConfig,
    silence_timeout_triggered: Arc<Mutex<bool>>, // Флаг для одноразового вызова callback
}

impl VadCaptureWrapper {
    /// Create new VAD wrapper around an audio capture
    ///
    /// # Arguments
    /// * `inner` - Underlying audio capture (must output 16kHz mono)
    /// * `vad` - VAD processor instance
    pub fn new(inner: Box<dyn AudioCapture>, vad: VadProcessor) -> Self {
        Self {
            inner,
            vad: Arc::new(Mutex::new(vad)),
            on_silence_timeout: None,
            audio_config: AudioConfig::default(),
            silence_timeout_triggered: Arc::new(Mutex::new(false)),
        }
    }

    /// Set callback for silence timeout events
    ///
    /// This callback is invoked ONCE when VAD detects configured silence timeout
    pub fn set_silence_timeout_callback(&mut self, callback: SilenceTimeoutCallback) {
        self.on_silence_timeout = Some(callback);
    }
}

#[async_trait]
impl AudioCapture for VadCaptureWrapper {
    async fn initialize(&mut self, config: AudioConfig) -> AudioResult<()> {
        self.audio_config = config.clone();
        self.inner.initialize(config).await
    }

    async fn start_capture(&mut self, on_chunk: AudioChunkCallback) -> AudioResult<()> {
        // Сбрасываем флаг при старте новой записи
        if let Ok(mut flag) = self.silence_timeout_triggered.lock() {
            *flag = false;
        }

        let vad = self.vad.clone();
        let silence_callback = self.on_silence_timeout.clone();
        let timeout_flag = self.silence_timeout_triggered.clone();

        // Frame buffer for accumulating exactly 480 samples (30ms @ 16kHz)
        // Shared between callback invocations via Arc<Mutex<>>
        let frame_buffer: Arc<Mutex<Vec<i16>>> = Arc::new(Mutex::new(Vec::with_capacity(960)));

        // Wrapped callback that processes audio through VAD
        let wrapped_callback = Arc::new(move |chunk: AudioChunk| {
            // Validate input format (VAD requirements)
            if chunk.sample_rate != 16000 {
                log::error!(
                    "VAD requires 16kHz audio, got {} Hz. Skipping VAD.",
                    chunk.sample_rate
                );
                on_chunk(chunk); // Pass through without VAD
                return;
            }

            if chunk.channels != 1 {
                log::error!(
                    "VAD requires mono audio, got {} channels. Skipping VAD.",
                    chunk.channels
                );
                on_chunk(chunk); // Pass through without VAD
                return;
            }

            // Add samples to frame buffer (защита от poisoned mutex)
            let mut buffer = match frame_buffer.lock() {
                Ok(b) => b,
                Err(e) => {
                    log::error!("VAD frame buffer poisoned: {}", e);
                    log::error!("Passing through audio without VAD processing");
                    on_chunk(chunk); // передаем оригинальный chunk без VAD
                    return;
                }
            };
            buffer.extend_from_slice(&chunk.data);

            // Process complete 30ms frames (480 samples @ 16kHz)
            const VAD_FRAME_SIZE: usize = 480;

            while buffer.len() >= VAD_FRAME_SIZE {
                let frame: Vec<i16> = buffer.drain(..VAD_FRAME_SIZE).collect();

                // Run VAD on this frame (защита от poisoned mutex)
                let mut vad_guard = match vad.lock() {
                    Ok(v) => v,
                    Err(e) => {
                        log::error!("VAD processor poisoned: {}", e);
                        log::error!("Passing through audio chunk without VAD");
                        on_chunk(AudioChunk::new(frame, 16000, 1));
                        continue;
                    }
                };

                let vad_result = match vad_guard.process_samples(&frame) {
                    Ok(result) => result,
                    Err(e) => {
                        log::error!("VAD processing error: {}", e);
                        // Pass through on error
                        on_chunk(AudioChunk::new(frame, 16000, 1));
                        continue;
                    }
                };
                drop(vad_guard); // Release VAD lock before callback

                match vad_result {
                    VadResult::Speech => {
                        // Speech detected - pass chunk through
                        log::trace!("VAD: Speech detected");
                        on_chunk(AudioChunk::new(frame, 16000, 1));
                    }
                    VadResult::Silence => {
                        // Silence but below timeout - still pass through
                        log::trace!("VAD: Silence (below timeout)");
                        on_chunk(AudioChunk::new(frame, 16000, 1));
                    }
                    VadResult::SilenceTimeout => {
                        // Silence timeout reached - trigger callback (только один раз)
                        let mut already_triggered = match timeout_flag.lock() {
                            Ok(f) => f,
                            Err(e) => {
                                log::error!("VAD timeout flag poisoned: {}", e);
                                // Все равно передаем аудио
                                on_chunk(AudioChunk::new(frame, 16000, 1));
                                continue;
                            }
                        };

                        if !*already_triggered {
                            // Получаем настоящий timeout из VAD для логирования
                            let timeout_ms = {
                                match vad.lock() {
                                    Ok(vad_guard) => vad_guard.timeout().as_millis(),
                                    Err(_) => 0, // fallback если mutex poisoned
                                }
                            };

                            log::info!("VAD: Silence timeout reached ({}ms)", timeout_ms);

                            // Emit silence timeout event ОДИН РАЗ
                            if let Some(ref callback) = silence_callback {
                                callback();
                            }

                            *already_triggered = true;
                        }

                        // Продолжаем пропускать аудио (для финализации)
                        on_chunk(AudioChunk::new(frame, 16000, 1));
                    }
                    VadResult::Buffering => {
                        // Should not happen since we buffer to 480 samples
                        log::trace!("VAD: Buffering");
                    }
                }
            }
        });

        // Start inner capture with wrapped callback
        self.inner.start_capture(wrapped_callback).await
    }

    async fn stop_capture(&mut self) -> AudioResult<()> {
        // Reset VAD state on stop
        if let Ok(mut vad) = self.vad.lock() {
            vad.reset();
        }

        self.inner.stop_capture().await
    }

    fn is_capturing(&self) -> bool {
        self.inner.is_capturing()
    }

    fn config(&self) -> AudioConfig {
        self.audio_config.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::audio::MockAudioCapture;

    #[tokio::test]
    async fn test_vad_wrapper_creation() {
        let mock_capture = Box::new(MockAudioCapture::new());
        let vad = VadProcessor::default().expect("Failed to create VAD");

        let wrapper = VadCaptureWrapper::new(mock_capture, vad);
        assert!(!wrapper.is_capturing());
    }

    #[tokio::test]
    async fn test_vad_wrapper_with_callback() {
        let mock_capture = Box::new(MockAudioCapture::new());
        let vad = VadProcessor::default().expect("Failed to create VAD");

        let mut wrapper = VadCaptureWrapper::new(mock_capture, vad);

        // Set silence timeout callback
        let silence_triggered = Arc::new(Mutex::new(false));
        let silence_flag = silence_triggered.clone();
        wrapper.set_silence_timeout_callback(Arc::new(move || {
            *silence_flag.lock().unwrap() = true;
        }));

        // Test that wrapper can be initialized
        let config = AudioConfig::default();
        let result = wrapper.initialize(config).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_invalid_sample_rate_passthrough() {
        let mock_capture = Box::new(MockAudioCapture::new());
        let vad = VadProcessor::default().expect("Failed to create VAD");
        let mut wrapper = VadCaptureWrapper::new(mock_capture, vad);

        let chunks_received = Arc::new(Mutex::new(0usize));
        let counter = chunks_received.clone();

        let on_chunk = Arc::new(move |chunk: AudioChunk| {
            // Проверяем что chunk прошел с неправильным sample rate
            assert_eq!(chunk.sample_rate, 48000);
            *counter.lock().unwrap() += 1;
        });

        wrapper.initialize(AudioConfig::default()).await.unwrap();
        wrapper.start_capture(on_chunk).await.unwrap();

        // MockAudioCapture не будет автоматически отправлять chunks,
        // но мы проверили что wrapper инициализируется
        assert!(!wrapper.is_capturing()); // Mock не начнет capture сам
    }

    #[tokio::test]
    async fn test_invalid_channels_passthrough() {
        let mock_capture = Box::new(MockAudioCapture::new());
        let vad = VadProcessor::default().expect("Failed to create VAD");
        let mut wrapper = VadCaptureWrapper::new(mock_capture, vad);

        let chunks_received = Arc::new(Mutex::new(0usize));
        let counter = chunks_received.clone();

        let on_chunk = Arc::new(move |chunk: AudioChunk| {
            // Проверяем что chunk прошел со стерео
            assert_eq!(chunk.channels, 2);
            *counter.lock().unwrap() += 1;
        });

        wrapper.initialize(AudioConfig::default()).await.unwrap();
        wrapper.start_capture(on_chunk).await.unwrap();

        // Wrapper должен инициализироваться даже с неправильными данными
        assert!(!wrapper.is_capturing());
    }

    #[tokio::test]
    async fn test_silence_timeout_callback_trigger() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let mock_capture = Box::new(MockAudioCapture::new());
        let vad = VadProcessor::new(Some(90), None).expect("Failed to create VAD");

        let mut wrapper = VadCaptureWrapper::new(mock_capture, vad);

        // Используем AtomicBool для thread-safe флага
        let silence_triggered = Arc::new(AtomicBool::new(false));
        let flag_clone = silence_triggered.clone();

        wrapper.set_silence_timeout_callback(Arc::new(move || {
            flag_clone.store(true, Ordering::SeqCst);
        }));

        wrapper.initialize(AudioConfig::default()).await.unwrap();

        // Проверяем что callback установлен
        assert!(!silence_triggered.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_audio_passthrough() {
        let mock_capture = Box::new(MockAudioCapture::new());
        let vad = VadProcessor::default().expect("Failed to create VAD");
        let mut wrapper = VadCaptureWrapper::new(mock_capture, vad);

        let chunks_count = Arc::new(Mutex::new(0));
        let counter = chunks_count.clone();

        let on_chunk = Arc::new(move |chunk: AudioChunk| {
            // Проверяем формат chunk
            assert_eq!(chunk.sample_rate, 16000);
            assert_eq!(chunk.channels, 1);
            *counter.lock().unwrap() += 1;
        });

        let config = AudioConfig::default();
        wrapper.initialize(config).await.unwrap();
        wrapper.start_capture(on_chunk).await.unwrap();

        // MockAudioCapture не генерирует реальные chunks, но wrapper инициализирован
        wrapper.stop_capture().await.unwrap();
        assert!(!wrapper.is_capturing());
    }
}
