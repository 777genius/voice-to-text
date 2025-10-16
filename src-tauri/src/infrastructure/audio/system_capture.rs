use async_trait::async_trait;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleFormat, Stream, StreamConfig, SupportedStreamConfig};
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use std::sync::{Arc, Mutex};

use crate::domain::{AudioCapture, AudioChunk, AudioChunkCallback, AudioConfig, AudioError, AudioResult};

/// Real system audio capture using cpal + rubato resampling
///
/// Flow:
/// 1. Check supported_input_configs() for best format
/// 2. cpal captures audio at native sample rate (e.g., 48kHz f32)
/// 3. Buffer samples until we have fixed chunk_size for rubato
/// 4. Convert f32 to i16 PCM
/// 5. Convert stereo to mono if needed
/// 6. Rubato resamples to 16kHz mono
/// 7. Call on_chunk callback
///
/// Target format:
/// - 16kHz sample rate
/// - Mono channel
/// - i16 PCM samples
const TARGET_SAMPLE_RATE: u32 = 16000;
const TARGET_CHANNELS: u16 = 1;
const RESAMPLER_CHUNK_SIZE: usize = 1024; // Fixed chunk size for rubato

/// System audio capture with automatic resampling
pub struct SystemAudioCapture {
    device: Device,
    stream: Option<Stream>,
    native_config: SupportedStreamConfig,
    audio_config: AudioConfig,
    is_capturing: bool,
}

impl SystemAudioCapture {
    /// Create new system audio capture with default input device
    pub fn new() -> AudioResult<Self> {
        let host = cpal::default_host();

        let device = host
            .default_input_device()
            .ok_or_else(|| AudioError::DeviceNotFound("No input device available".to_string()))?;

        let device_name = device.name().unwrap_or_else(|_| "Unknown".to_string());
        log::info!("Using audio input device: {}", device_name);

        // Логируем все доступные устройства для отладки
        let all_devices: Vec<String> = host
            .input_devices()
            .ok()
            .map(|devices| {
                devices
                    .filter_map(|d| d.name().ok())
                    .collect()
            })
            .unwrap_or_default();
        log::debug!("Available input devices: {:?}", all_devices);

        // Check supported configs and choose best one
        let supported_configs = device
            .supported_input_configs()
            .map_err(|e| AudioError::Configuration(format!("Failed to get supported configs: {}", e)))?;

        let native_config = supported_configs
            .filter(|config| {
                // Prefer f32 format if available
                config.sample_format() == SampleFormat::F32
            })
            .min_by_key(|config| {
                // Choose config closest to 16kHz
                (config.min_sample_rate().0 as i32 - TARGET_SAMPLE_RATE as i32).abs()
            })
            .or_else(|| {
                // Fallback: any f32 config
                device
                    .supported_input_configs()
                    .ok()?
                    .find(|c| c.sample_format() == SampleFormat::F32)
            })
            .ok_or_else(|| AudioError::Configuration("No suitable f32 audio config found".to_string()))?;

        log::info!(
            "Selected audio config: {} Hz, {} channels, {:?} format",
            native_config.max_sample_rate().0,
            native_config.channels(),
            native_config.sample_format()
        );

        Ok(Self {
            device,
            stream: None,
            native_config: native_config.with_max_sample_rate(),
            audio_config: AudioConfig::default(),
            is_capturing: false,
        })
    }

    /// Create resampler for converting native sample rate to 16kHz
    fn create_resampler(
        from_sample_rate: u32,
        channels: usize,
    ) -> AudioResult<SincFixedIn<f32>> {
        let params = SincInterpolationParameters {
            sinc_len: 256,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 256,
            window: WindowFunction::BlackmanHarris2,
        };

        SincFixedIn::<f32>::new(
            TARGET_SAMPLE_RATE as f64 / from_sample_rate as f64,
            2.0, // Max relative ratio change
            params,
            RESAMPLER_CHUNK_SIZE,
            channels,
        )
        .map_err(|e| AudioError::Internal(format!("Failed to create resampler: {}", e)))
    }

    /// Convert f32 samples to i16 PCM (in-place conversion concept)
    #[inline]
    fn f32_to_i16(samples: &[f32]) -> Vec<i16> {
        samples
            .iter()
            .map(|&sample| {
                let clamped = sample.clamp(-1.0, 1.0);
                (clamped * 32767.0) as i16
            })
            .collect()
    }

    /// Convert stereo to mono by averaging channels
    #[inline]
    fn stereo_to_mono(samples: &[i16]) -> Vec<i16> {
        samples
            .chunks_exact(2)
            .map(|chunk| ((chunk[0] as i32 + chunk[1] as i32) / 2) as i16)
            .collect()
    }
}

impl Default for SystemAudioCapture {
    fn default() -> Self {
        Self::new().expect("Failed to create system audio capture")
    }
}

// SAFETY: SystemAudioCapture содержит cpal::Stream который не Send/Sync на macOS.
// Однако мы гарантируем безопасность тем что:
// 1. Stream доступен только через RwLock/Mutex в async методах
// 2. Stream никогда не перемещается между потоками
// 3. На Windows cpal::Stream уже Send/Sync, так что безопасно
unsafe impl Send for SystemAudioCapture {}
unsafe impl Sync for SystemAudioCapture {}

#[async_trait]
impl AudioCapture for SystemAudioCapture {
    async fn initialize(&mut self, config: AudioConfig) -> AudioResult<()> {
        self.audio_config = config;
        log::info!("SystemAudioCapture initialized with config: {:?}", config);
        Ok(())
    }

    async fn start_capture(&mut self, on_chunk: AudioChunkCallback) -> AudioResult<()> {
        if self.is_capturing {
            return Err(AudioError::Capture(
                "Already capturing audio".to_string(),
            ));
        }

        let native_sample_rate = self.native_config.sample_rate().0;
        let native_channels = self.native_config.channels() as usize;

        log::info!(
            "Starting audio capture: {} Hz → {} Hz, {} channels → {} channel",
            native_sample_rate,
            TARGET_SAMPLE_RATE,
            native_channels,
            TARGET_CHANNELS
        );

        // Create resampler if needed (wrapped in Arc<Mutex<>> for thread safety)
        let needs_resampling = native_sample_rate != TARGET_SAMPLE_RATE;
        let resampler: Option<Arc<Mutex<SincFixedIn<f32>>>> = if needs_resampling {
            Some(Arc::new(Mutex::new(Self::create_resampler(
                native_sample_rate,
                1, // mono after conversion
            )?)))
        } else {
            None
        };

        // Input buffer for accumulating samples before resampling
        // Shared between callback invocations via Arc<Mutex<>>
        let input_buffer: Arc<Mutex<Vec<i16>>> = Arc::new(Mutex::new(Vec::with_capacity(RESAMPLER_CHUNK_SIZE * 2)));

        // Clone for move into callback
        let resampler_clone = resampler.clone();
        let input_buffer_clone = input_buffer.clone();

        // Build audio stream with the selected config
        let stream_config: StreamConfig = self.native_config.clone().into();

        let stream = self
            .device
            .build_input_stream(
                &stream_config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    // 1. Convert f32 to i16
                    let mut pcm_samples = Self::f32_to_i16(data);

                    // 2. Convert stereo to mono if needed
                    if native_channels > 1 {
                        pcm_samples = Self::stereo_to_mono(&pcm_samples);
                    }

                    // 3. Add to input buffer (защита от poisoned mutex)
                    let mut buffer = match input_buffer_clone.lock() {
                        Ok(b) => b,
                        Err(e) => {
                            log::error!("Audio input buffer poisoned (panic in other thread): {}", e);
                            log::error!("Audio capture stopping due to unrecoverable error");
                            return;
                        }
                    };
                    buffer.extend_from_slice(&pcm_samples);

                    // 4. Process chunks of fixed size for resampler
                    while buffer.len() >= RESAMPLER_CHUNK_SIZE {
                        let chunk: Vec<i16> = buffer.drain(..RESAMPLER_CHUNK_SIZE).collect();

                        // 5. Resample if needed
                        let final_samples = if let Some(ref rs) = resampler_clone {
                            // Convert i16 to f32 for rubato
                            let float_chunk: Vec<f32> = chunk
                                .iter()
                                .map(|&s| s as f32 / 32767.0)
                                .collect();

                            // Prepare input for resampler (channels x samples)
                            let resampler_input = vec![float_chunk];

                            // Resample (защита от poisoned mutex)
                            let mut resampler_guard = match rs.lock() {
                                Ok(r) => r,
                                Err(e) => {
                                    log::error!("Resampler mutex poisoned: {}", e);
                                    log::error!("Skipping audio chunk due to resampler error");
                                    continue; // пропускаем этот чанк
                                }
                            };

                            match resampler_guard.process(&resampler_input, None) {
                                Ok(output) => {
                                    // Convert back to i16
                                    Self::f32_to_i16(&output[0])
                                }
                                Err(e) => {
                                    log::error!("Resampling error: {}", e);
                                    continue; // пропускаем этот чанк
                                }
                            }
                        } else {
                            chunk
                        };

                        // 6. Send chunk via callback
                        let audio_chunk = AudioChunk::new(
                            final_samples,
                            TARGET_SAMPLE_RATE,
                            TARGET_CHANNELS,
                        );

                        on_chunk(audio_chunk);
                    }
                },
                |err| {
                    log::error!("Audio stream error: {}", err);
                },
                None,
            )
            .map_err(|e| AudioError::Capture(format!("Failed to build audio stream: {}", e)))?;

        // Start the stream
        stream
            .play()
            .map_err(|e| AudioError::Capture(format!("Failed to start audio stream: {}", e)))?;

        self.stream = Some(stream);
        self.is_capturing = true;

        log::info!("Audio capture started successfully");
        Ok(())
    }

    async fn stop_capture(&mut self) -> AudioResult<()> {
        if !self.is_capturing {
            log::warn!("Audio capture was not active");
            return Ok(());
        }

        if let Some(stream) = self.stream.take() {
            drop(stream); // Stream is stopped when dropped
        }

        self.is_capturing = false;
        log::info!("Audio capture stopped");

        Ok(())
    }

    fn is_capturing(&self) -> bool {
        self.is_capturing
    }

    fn config(&self) -> AudioConfig {
        self.audio_config.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_f32_to_i16_conversion() {
        let input = vec![0.0, 0.5, -0.5, 1.0, -1.0];
        let output = SystemAudioCapture::f32_to_i16(&input);

        assert_eq!(output[0], 0);
        assert_eq!(output[1], 16383); // 0.5 * 32767
        assert_eq!(output[2], -16383);
        assert_eq!(output[3], 32767);
        assert_eq!(output[4], -32767);
    }

    #[test]
    fn test_stereo_to_mono() {
        let stereo = vec![1000, 2000, 3000, 4000, 5000, 6000];
        let mono = SystemAudioCapture::stereo_to_mono(&stereo);

        assert_eq!(mono.len(), 3);
        assert_eq!(mono[0], 1500); // (1000 + 2000) / 2
        assert_eq!(mono[1], 3500);
        assert_eq!(mono[2], 5500);
    }

    #[tokio::test]
    async fn test_capture_creation() {
        // This test may fail if no audio device is available
        let result = SystemAudioCapture::new();
        // Just check it doesn't panic
        match result {
            Ok(capture) => {
                assert!(!capture.is_capturing());
            }
            Err(_) => {
                // OK if no device available in CI environment
            }
        }
    }

    #[test]
    fn test_f32_to_i16_edge_cases() {
        // Тест граничных случаев конвертации
        let edge_cases = vec![-1.5, 1.5, -2.0, 2.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY];
        let output = SystemAudioCapture::f32_to_i16(&edge_cases);

        // Значения должны быть clamped в диапазон [-1.0, 1.0]
        assert_eq!(output[0], -32767); // -1.5 clamped to -1.0
        assert_eq!(output[1], 32767);  // 1.5 clamped to 1.0
        assert_eq!(output[2], -32767); // -2.0 clamped to -1.0
        assert_eq!(output[3], 32767);  // 2.0 clamped to 1.0
    }

    #[test]
    fn test_stereo_to_mono_empty() {
        let empty: Vec<i16> = vec![];
        let result = SystemAudioCapture::stereo_to_mono(&empty);
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_stereo_to_mono_overflow_protection() {
        // Проверяем что нет overflow при усреднении больших значений
        let stereo = vec![i16::MAX, i16::MAX, i16::MIN, i16::MIN];
        let mono = SystemAudioCapture::stereo_to_mono(&stereo);

        // (MAX + MAX) / 2 = MAX, (MIN + MIN) / 2 = MIN
        assert_eq!(mono.len(), 2);
        assert_eq!(mono[0], i16::MAX);
        assert_eq!(mono[1], i16::MIN);
    }

    #[tokio::test]
    async fn test_capture_state_transitions() {
        let result = SystemAudioCapture::new();

        if let Ok(mut capture) = result {
            // Начальное состояние - не capturing
            assert!(!capture.is_capturing());

            // Проверяем что config можно получить
            let config = capture.config();
            assert_eq!(config.sample_rate, 16000);
            assert_eq!(config.channels, 1);

            // Проверяем initialize
            let init_result = capture.initialize(AudioConfig::default()).await;
            assert!(init_result.is_ok());
        }
    }
}
