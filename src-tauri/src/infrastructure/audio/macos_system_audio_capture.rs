use async_trait::async_trait;
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use screencapturekit::prelude::*;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::domain::{
    AudioCapture, AudioChunk, AudioChunkCallback, AudioConfig, AudioError, AudioResult,
};

const TARGET_SAMPLE_RATE: u32 = 16_000;
const TARGET_CHANNELS: u16 = 1;
const TARGET_FRAME_MS: usize = 30;
const TARGET_FRAME_SAMPLES: usize = (TARGET_SAMPLE_RATE as usize * TARGET_FRAME_MS) / 1000;

/// macOS system output audio capture via ScreenCaptureKit.
///
/// Captures what the user hears and emits 16 kHz mono PCM16 frames for STT.
pub struct MacosSystemAudioCapture {
    stream: Option<SCStream>,
    audio_config: AudioConfig,
    is_capturing: bool,
    callback_gate: CaptureCallbackGate,
}

impl MacosSystemAudioCapture {
    pub fn new() -> AudioResult<Self> {
        Ok(Self {
            stream: None,
            audio_config: AudioConfig::default(),
            is_capturing: false,
            callback_gate: CaptureCallbackGate::default(),
        })
    }
}

#[derive(Clone, Default)]
struct CaptureCallbackGate {
    running: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
}

type CaptureCallbackSlot = Arc<Mutex<Option<AudioChunkCallback>>>;

impl CaptureCallbackGate {
    fn begin_capture(&self) -> u64 {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.running.store(true, Ordering::SeqCst);
        generation
    }

    fn stop_capture(&self) {
        self.running.store(false, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    fn should_emit(&self, generation: u64) -> bool {
        self.running.load(Ordering::Relaxed)
            && self.generation.load(Ordering::Relaxed) == generation
    }
}

fn emit_capture_chunk(callback_slot: &CaptureCallbackSlot, chunk: AudioChunk) {
    let callback = match callback_slot.lock() {
        Ok(callback) => callback.clone(),
        Err(err) => {
            log::error!("ScreenCaptureKit callback slot poisoned: {}", err);
            return;
        }
    };
    if let Some(callback) = callback {
        if catch_unwind(AssertUnwindSafe(|| callback(chunk))).is_err() {
            log::error!("ScreenCaptureKit audio callback panicked; revoking it for this stream");
            match callback_slot.lock() {
                Ok(mut callback) => *callback = None,
                Err(err) => log::error!(
                    "ScreenCaptureKit callback slot poisoned while revoking panicked callback: {}",
                    err
                ),
            }
        }
    }
}

fn deactivate_capture_after_stream_error(
    callback_gate: &CaptureCallbackGate,
    callback_slot: &CaptureCallbackSlot,
) {
    callback_gate.stop_capture();
    match callback_slot.lock() {
        Ok(mut callback) => {
            *callback = None;
        }
        Err(err) => {
            log::error!(
                "ScreenCaptureKit callback slot poisoned during stream failure: {}",
                err
            );
        }
    }
}

struct ResamplePipeline {
    is_float: bool,
    is_big_endian: bool,
    bits_per_channel: u32,
    resampler: SincFixedIn<f32>,
    native_mono: Vec<f32>,
    out_i16: Vec<i16>,
    input_frame_samples: usize,
}

enum PipelineState {
    Pending,
    Ready(ResamplePipeline),
    Failed,
}

impl ResamplePipeline {
    fn new(
        from_sample_rate: u32,
        is_float: bool,
        is_big_endian: bool,
        bits_per_channel: u32,
    ) -> AudioResult<Self> {
        let input_frame_samples = ((from_sample_rate as usize) * TARGET_FRAME_MS / 1000).max(1);
        let params = SincInterpolationParameters {
            sinc_len: 256,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 256,
            window: WindowFunction::BlackmanHarris2,
        };
        let resampler = SincFixedIn::<f32>::new(
            TARGET_SAMPLE_RATE as f64 / from_sample_rate as f64,
            2.0,
            params,
            input_frame_samples,
            1,
        )
        .map_err(|e| AudioError::Internal(format!("Failed to create resampler: {e}")))?;

        Ok(Self {
            is_float,
            is_big_endian,
            bits_per_channel,
            resampler,
            native_mono: Vec::with_capacity(input_frame_samples * 4),
            out_i16: Vec::with_capacity(TARGET_FRAME_SAMPLES * 6),
            input_frame_samples,
        })
    }

    fn decode_and_push_native_mono(&mut self, sample: &CMSampleBuffer) {
        let Some(audio_list) = sample.audio_buffer_list() else {
            return;
        };

        let num_buffers = audio_list.num_buffers();
        if num_buffers == 0 {
            return;
        }

        if num_buffers == 1 {
            let Some(buf) = audio_list.get(0) else {
                return;
            };
            let channels = buf.number_channels as usize;
            let data = buf.data();
            if channels == 0 || data.is_empty() {
                return;
            }

            if self.is_float && self.bits_per_channel == 32 {
                decode_interleaved_f32_to_mono(
                    data,
                    channels,
                    self.is_big_endian,
                    &mut self.native_mono,
                );
            } else if !self.is_float && self.bits_per_channel == 16 {
                decode_interleaved_i16_to_mono(
                    data,
                    channels,
                    self.is_big_endian,
                    &mut self.native_mono,
                );
            }
            return;
        }

        let bytes_per_sample = if self.is_float && self.bits_per_channel == 32 {
            4
        } else if !self.is_float && self.bits_per_channel == 16 {
            2
        } else {
            return;
        };

        let mut channel_buffers: Vec<&[u8]> = Vec::with_capacity(num_buffers);
        for i in 0..num_buffers {
            let Some(buf) = audio_list.get(i) else {
                continue;
            };
            if buf.number_channels != 1 {
                return;
            }
            channel_buffers.push(buf.data());
        }

        decode_non_interleaved_to_mono(
            &channel_buffers,
            bytes_per_sample,
            self.is_big_endian,
            &mut self.native_mono,
        );
    }

    fn drain_frames(&mut self) -> Vec<Vec<i16>> {
        let mut frames = Vec::new();

        while self.native_mono.len() >= self.input_frame_samples {
            let chunk: Vec<f32> = self.native_mono.drain(..self.input_frame_samples).collect();
            let input = vec![chunk];
            let out = match self.resampler.process(&input, None) {
                Ok(out) => out,
                Err(err) => {
                    log::error!("ScreenCaptureKit audio resampling error: {}", err);
                    continue;
                }
            };

            if let Some(out_ch) = out.first() {
                self.out_i16.extend(out_ch.iter().map(|&s| {
                    let clamped = s.clamp(-1.0, 1.0);
                    (clamped * 32767.0) as i16
                }));
            }
        }

        while self.out_i16.len() >= TARGET_FRAME_SAMPLES {
            frames.push(self.out_i16.drain(..TARGET_FRAME_SAMPLES).collect());
        }

        frames
    }
}

fn build_resample_pipeline(
    sample_rate: u32,
    is_float: bool,
    is_big_endian: bool,
    bits: u32,
) -> Option<ResamplePipeline> {
    match ResamplePipeline::new(sample_rate, is_float, is_big_endian, bits) {
        Ok(pipe) => Some(pipe),
        Err(primary_err) => {
            log::error!("Failed to init ScreenCaptureKit resampler: {}", primary_err);
            match ResamplePipeline::new(48_000, true, false, 32) {
                Ok(pipe) => Some(pipe),
                Err(fallback_err) => {
                    log::error!(
                        "Failed to init fallback ScreenCaptureKit resampler: {}",
                        fallback_err
                    );
                    None
                }
            }
        }
    }
}

#[async_trait]
impl AudioCapture for MacosSystemAudioCapture {
    async fn initialize(&mut self, config: AudioConfig) -> AudioResult<()> {
        self.audio_config = config;
        Ok(())
    }

    async fn start_capture(&mut self, on_chunk: AudioChunkCallback) -> AudioResult<()> {
        if self.is_capturing {
            return Err(AudioError::Capture("Already capturing audio".to_string()));
        }
        let capture_generation = self.callback_gate.begin_capture();

        let content = SCShareableContent::get().map_err(|e| {
            self.callback_gate.stop_capture();
            AudioError::AccessDenied(format!(
                "ScreenCaptureKit unavailable: {e}. Откройте macOS System Settings -> Privacy & Security -> Screen & System Audio Recording и разрешите доступ для приложения."
            ))
        })?;
        let display = content.displays().into_iter().next().ok_or_else(|| {
            self.callback_gate.stop_capture();
            AudioError::DeviceNotFound("No displays found for capture".into())
        })?;

        let filter = SCContentFilter::create()
            .with_display(&display)
            .with_excluding_windows(&[])
            .build();
        let config = SCStreamConfiguration::new()
            .with_width(2)
            .with_height(2)
            .with_captures_audio(true)
            .with_excludes_current_process_audio(true)
            .with_sample_rate(48_000)
            .with_channel_count(2);

        let pipeline: Arc<Mutex<PipelineState>> = Arc::new(Mutex::new(PipelineState::Pending));
        let pipeline_for_cb = pipeline.clone();
        let callback_slot: CaptureCallbackSlot = Arc::new(Mutex::new(Some(on_chunk)));
        let callback_slot_for_audio = callback_slot.clone();
        let callback_gate = self.callback_gate.clone();
        let callback_gate_for_error = self.callback_gate.clone();
        let callback_slot_for_error = callback_slot.clone();
        let error_handler = ErrorHandler::new(move |error| {
            log::error!("ScreenCaptureKit stream error: {}", error);
            deactivate_capture_after_stream_error(
                &callback_gate_for_error,
                &callback_slot_for_error,
            );
        });

        let mut stream = SCStream::new_with_delegate(&filter, &config, error_handler);
        let added = stream.add_output_handler(
            move |sample: CMSampleBuffer, _output: SCStreamOutputType| {
                if !callback_gate.should_emit(capture_generation) {
                    return;
                }

                let Some(fmt) = sample.format_description() else {
                    return;
                };
                if !fmt.is_audio() {
                    return;
                }

                let sample_rate = fmt
                    .audio_sample_rate()
                    .map(|v| v.round() as u32)
                    .unwrap_or(48_000);
                let bits = fmt.audio_bits_per_channel().unwrap_or(32);
                let is_float = fmt.audio_is_float();
                let is_big_endian = fmt.audio_is_big_endian();

                let mut guard = match pipeline_for_cb.lock() {
                    Ok(guard) => guard,
                    Err(e) => {
                        log::error!("ScreenCaptureKit audio pipeline poisoned: {}", e);
                        return;
                    }
                };
                if matches!(*guard, PipelineState::Failed) {
                    return;
                }

                if matches!(*guard, PipelineState::Pending) {
                    let Some(pipe) =
                        build_resample_pipeline(sample_rate, is_float, is_big_endian, bits)
                    else {
                        *guard = PipelineState::Failed;
                        return;
                    };
                    *guard = PipelineState::Ready(pipe);
                }

                let PipelineState::Ready(pipe) = &mut *guard else {
                    return;
                };

                pipe.decode_and_push_native_mono(&sample);
                let frames = pipe.drain_frames();
                drop(guard);

                for frame in frames {
                    if !callback_gate.should_emit(capture_generation) {
                        return;
                    }
                    if !frame.is_empty() {
                        emit_capture_chunk(
                            &callback_slot_for_audio,
                            AudioChunk::new(frame, TARGET_SAMPLE_RATE, TARGET_CHANNELS),
                        );
                    }
                }
            },
            SCStreamOutputType::Audio,
        );

        if added.is_none() {
            self.callback_gate.stop_capture();
            return Err(AudioError::Internal(
                "Failed to register ScreenCaptureKit audio handler".to_string(),
            ));
        }

        stream.start_capture().map_err(|e| {
            self.callback_gate.stop_capture();
            AudioError::Capture(format!(
                "Failed to start ScreenCaptureKit capture: {e}. Проверьте macOS Screen & System Audio Recording permission."
            ))
        })?;

        self.stream = Some(stream);
        self.is_capturing = true;
        Ok(())
    }

    async fn stop_capture(&mut self) -> AudioResult<()> {
        if !self.is_capturing {
            return Ok(());
        }
        self.callback_gate.stop_capture();
        if let Some(stream) = self.stream.take() {
            let _ = stream.stop_capture();
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

fn decode_interleaved_f32_to_mono(
    data: &[u8],
    channels: usize,
    is_big_endian: bool,
    out: &mut Vec<f32>,
) {
    let frame_count = data.len() / (4 * channels);
    for frame in 0..frame_count {
        let mut sum = 0.0f32;
        for ch in 0..channels {
            let idx = (frame * channels + ch) * 4;
            let bytes = [data[idx], data[idx + 1], data[idx + 2], data[idx + 3]];
            let v = if is_big_endian {
                f32::from_be_bytes(bytes)
            } else {
                f32::from_le_bytes(bytes)
            };
            sum += v;
        }
        out.push(sum / channels as f32);
    }
}

fn decode_interleaved_i16_to_mono(
    data: &[u8],
    channels: usize,
    is_big_endian: bool,
    out: &mut Vec<f32>,
) {
    let frame_count = data.len() / (2 * channels);
    for frame in 0..frame_count {
        let mut sum = 0i32;
        for ch in 0..channels {
            let idx = (frame * channels + ch) * 2;
            let bytes = [data[idx], data[idx + 1]];
            let v = if is_big_endian {
                i16::from_be_bytes(bytes)
            } else {
                i16::from_le_bytes(bytes)
            };
            sum += v as i32;
        }
        out.push(sum as f32 / channels as f32 / 32767.0);
    }
}

fn decode_non_interleaved_to_mono(
    channel_buffers: &[&[u8]],
    bytes_per_sample: usize,
    is_big_endian: bool,
    out: &mut Vec<f32>,
) {
    if channel_buffers.is_empty() {
        return;
    }

    let frame_count = channel_buffers
        .iter()
        .map(|data| data.len() / bytes_per_sample)
        .min()
        .unwrap_or(0);

    for frame in 0..frame_count {
        let mut sum = 0.0f32;
        for data in channel_buffers {
            let idx = frame * bytes_per_sample;
            let v = if bytes_per_sample == 4 {
                let bytes = [data[idx], data[idx + 1], data[idx + 2], data[idx + 3]];
                if is_big_endian {
                    f32::from_be_bytes(bytes)
                } else {
                    f32::from_le_bytes(bytes)
                }
            } else {
                let bytes = [data[idx], data[idx + 1]];
                let v = if is_big_endian {
                    i16::from_be_bytes(bytes)
                } else {
                    i16::from_le_bytes(bytes)
                };
                v as f32 / 32767.0
            };
            sum += v;
        }
        out.push(sum / channel_buffers.len() as f32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callback_gate_blocks_stale_callbacks_after_stop_and_restart() {
        let gate = CaptureCallbackGate::default();

        let first_generation = gate.begin_capture();
        assert!(gate.should_emit(first_generation));

        gate.stop_capture();
        assert!(!gate.should_emit(first_generation));

        let second_generation = gate.begin_capture();
        assert!(!gate.should_emit(first_generation));
        assert!(gate.should_emit(second_generation));

        gate.stop_capture();
        assert!(!gate.should_emit(second_generation));
    }

    #[test]
    fn stream_error_revokes_callback_and_stops_capture_generation() {
        let gate = CaptureCallbackGate::default();
        let generation = gate.begin_capture();
        let callback_slot: CaptureCallbackSlot = Arc::new(Mutex::new(Some(Arc::new(|_chunk| {}))));

        deactivate_capture_after_stream_error(&gate, &callback_slot);

        assert!(!gate.should_emit(generation));
        assert!(callback_slot.lock().unwrap().is_none());
    }

    #[test]
    fn callback_panic_is_contained_and_revokes_callback() {
        let callback: AudioChunkCallback =
            Arc::new(|_chunk| panic!("simulated ScreenCaptureKit callback panic"));
        let callback_slot: CaptureCallbackSlot = Arc::new(Mutex::new(Some(callback)));

        emit_capture_chunk(&callback_slot, AudioChunk::new(vec![1, 2, 3], 16_000, 1));

        assert!(callback_slot.lock().unwrap().is_none());
    }

    #[test]
    fn decodes_interleaved_i16_to_mono() {
        let samples = [1000i16, 3000, -1000, -3000];
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut out = Vec::new();

        decode_interleaved_i16_to_mono(&bytes, 2, false, &mut out);

        assert_eq!(out.len(), 2);
        assert!((out[0] - (2000.0 / 32767.0)).abs() < 0.0001);
        assert!((out[1] - (-2000.0 / 32767.0)).abs() < 0.0001);
    }

    #[test]
    fn decodes_non_interleaved_f32_to_mono() {
        let left: Vec<u8> = [0.25f32, 0.5]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let right: Vec<u8> = [0.75f32, -0.5]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let mut out = Vec::new();

        decode_non_interleaved_to_mono(&[&left, &right], 4, false, &mut out);

        assert_eq!(out, vec![0.5, 0.0]);
    }
}
