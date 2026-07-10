use async_trait::async_trait;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleFormat, Stream, StreamConfig, SupportedStreamConfig};
use rubato::{Resampler, SincFixedIn};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};

use crate::domain::{
    AudioCapture, AudioCaptureTarget, AudioChunk, AudioChunkCallback, AudioConfig, AudioError,
    AudioResult,
};

use super::SystemAudioCapture;

const RESAMPLER_CHUNK_SIZE: usize = 1024;
type LoopbackCallbackSlot = Arc<Mutex<Option<AudioChunkCallback>>>;

fn emit_loopback_chunk(callback_slot: &LoopbackCallbackSlot, chunk: AudioChunk) {
    let callback = match callback_slot.lock() {
        Ok(callback) => callback.clone(),
        Err(err) => {
            log::error!("WASAPI loopback callback slot poisoned: {}", err);
            return;
        }
    };
    if let Some(callback) = callback {
        if catch_unwind(AssertUnwindSafe(|| callback(chunk))).is_err() {
            log::error!("WASAPI loopback callback panicked; revoking it for this stream");
            revoke_loopback_callback(callback_slot);
        }
    }
}

fn revoke_loopback_callback(callback_slot: &LoopbackCallbackSlot) {
    match callback_slot.lock() {
        Ok(mut callback) => {
            *callback = None;
        }
        Err(err) => {
            log::error!(
                "WASAPI loopback callback slot poisoned during stream failure: {}",
                err
            );
        }
    }
}

pub struct WindowsWasapiLoopbackCapture {
    device: Device,
    stream: Option<Stream>,
    native_config: SupportedStreamConfig,
    audio_config: AudioConfig,
    target: AudioCaptureTarget,
    is_capturing: bool,
}

impl WindowsWasapiLoopbackCapture {
    pub fn new(target: AudioCaptureTarget) -> AudioResult<Self> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or_else(|| {
            AudioError::DeviceNotFound(
                "No default output device available for WASAPI loopback".into(),
            )
        })?;
        let native_config = device.default_output_config().map_err(|e| {
            AudioError::Configuration(format!("Failed to read output config for loopback: {}", e))
        })?;

        log::info!(
            "Using WASAPI loopback output device: {}",
            device.name().unwrap_or_else(|_| "Unknown".to_string())
        );

        Ok(Self {
            device,
            stream: None,
            native_config,
            audio_config: AudioConfig::default(),
            target,
            is_capturing: false,
        })
    }
}

unsafe impl Send for WindowsWasapiLoopbackCapture {}
unsafe impl Sync for WindowsWasapiLoopbackCapture {}

#[async_trait]
impl AudioCapture for WindowsWasapiLoopbackCapture {
    async fn initialize(&mut self, config: AudioConfig) -> AudioResult<()> {
        self.audio_config = config;
        Ok(())
    }

    async fn start_capture(&mut self, on_chunk: AudioChunkCallback) -> AudioResult<()> {
        if self.is_capturing {
            return Err(AudioError::Capture(
                "WASAPI loopback capture is already running".to_string(),
            ));
        }

        let native_sample_rate = self.native_config.sample_rate().0;
        let native_channels = self.native_config.channels() as usize;
        let target_sample_rate = self.target.sample_rate;
        let target_channels = self.target.channels;

        let resampler: Option<Arc<Mutex<SincFixedIn<f32>>>> =
            if native_sample_rate != target_sample_rate {
                Some(Arc::new(Mutex::new(SystemAudioCapture::create_resampler(
                    native_sample_rate,
                    target_sample_rate,
                    1,
                )?)))
            } else {
                None
            };
        let input_buffer: Arc<Mutex<Vec<i16>>> =
            Arc::new(Mutex::new(Vec::with_capacity(RESAMPLER_CHUNK_SIZE * 4)));

        let resampler_clone = resampler.clone();
        let input_buffer_clone = input_buffer.clone();
        let callback_slot: LoopbackCallbackSlot = Arc::new(Mutex::new(Some(on_chunk)));
        let callback_slot_for_audio = callback_slot.clone();
        let process_pcm = move |mut pcm_samples: Vec<i16>| {
            if native_channels > 1 {
                pcm_samples = SystemAudioCapture::downmix_to_mono(&pcm_samples, native_channels);
            }

            let mut buffer = match input_buffer_clone.lock() {
                Ok(buffer) => buffer,
                Err(e) => {
                    log::error!("WASAPI loopback buffer poisoned: {}", e);
                    return;
                }
            };
            buffer.extend_from_slice(&pcm_samples);

            while buffer.len() >= RESAMPLER_CHUNK_SIZE {
                let chunk: Vec<i16> = buffer.drain(..RESAMPLER_CHUNK_SIZE).collect();
                let final_samples = if let Some(ref rs) = resampler_clone {
                    let float_chunk: Vec<f32> = chunk.iter().map(|&s| s as f32 / 32767.0).collect();
                    let mut resampler_guard = match rs.lock() {
                        Ok(guard) => guard,
                        Err(e) => {
                            log::error!("WASAPI loopback resampler poisoned: {}", e);
                            continue;
                        }
                    };
                    match resampler_guard.process(&[float_chunk], None) {
                        Ok(output) => SystemAudioCapture::f32_to_i16(&output[0]),
                        Err(e) => {
                            log::error!("WASAPI loopback resample error: {}", e);
                            continue;
                        }
                    }
                } else {
                    chunk
                };

                emit_loopback_chunk(
                    &callback_slot_for_audio,
                    AudioChunk::new(final_samples, target_sample_rate, target_channels),
                );
            }
        };

        let stream_config: StreamConfig = self.native_config.clone().into();
        let callback_slot_for_error = callback_slot.clone();
        let err_fn = move |err: cpal::StreamError| {
            log::error!("WASAPI loopback stream error: {}", err);
            revoke_loopback_callback(&callback_slot_for_error);
        };
        let stream_result = match self.native_config.sample_format() {
            SampleFormat::F32 => self.device.build_input_stream(
                &stream_config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    process_pcm(SystemAudioCapture::f32_to_i16(data));
                },
                err_fn,
                None,
            ),
            SampleFormat::I16 => self.device.build_input_stream(
                &stream_config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| process_pcm(data.to_vec()),
                err_fn,
                None,
            ),
            SampleFormat::U16 => self.device.build_input_stream(
                &stream_config,
                move |data: &[u16], _: &cpal::InputCallbackInfo| {
                    process_pcm(SystemAudioCapture::u16_to_i16(data));
                },
                err_fn,
                None,
            ),
            other => {
                return Err(AudioError::Configuration(format!(
                    "Unsupported WASAPI loopback sample format: {:?}",
                    other
                )));
            }
        }
        .map_err(|e| {
            AudioError::Capture(format!("Failed to build WASAPI loopback stream: {}", e))
        })?;

        stream_result
            .play()
            .map_err(|e| AudioError::Capture(format!("Failed to start WASAPI loopback: {}", e)))?;

        self.stream = Some(stream_result);
        self.is_capturing = true;
        Ok(())
    }

    async fn stop_capture(&mut self) -> AudioResult<()> {
        if let Some(stream) = self.stream.take() {
            drop(stream);
        }
        self.is_capturing = false;
        Ok(())
    }

    fn is_capturing(&self) -> bool {
        self.is_capturing
    }

    fn config(&self) -> AudioConfig {
        self.audio_config
    }
}
