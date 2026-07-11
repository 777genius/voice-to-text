use async_trait::async_trait;
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use screencapturekit::prelude::*;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::domain::{
    AudioCapture, AudioCaptureErrorCallback, AudioCaptureTarget, AudioChunk, AudioChunkCallback,
    AudioConfig, AudioError, AudioResult, SelfAudioExclusionRequirement, SystemAudioCaptureRequest,
};

const NATIVE_SAMPLE_RATE: u32 = 48_000;
const NATIVE_CHANNELS: u16 = 2;
const TARGET_FRAME_MS: usize = 30;

/// macOS system output audio capture via ScreenCaptureKit.
///
/// Captures what the user hears, excludes this process, and emits target PCM16 frames.
pub struct MacosSystemAudioCapture {
    stream: Option<SCStream>,
    target: AudioCaptureTarget,
    audio_config: AudioConfig,
    callback_gate: CaptureCallbackGate,
    terminal_error_callback: CaptureErrorCallbackSlot,
}

impl MacosSystemAudioCapture {
    pub fn new(request: SystemAudioCaptureRequest) -> AudioResult<Self> {
        validate_capture_request(request)?;
        let target = request.target;
        Ok(Self {
            stream: None,
            target,
            audio_config: target_audio_config(target),
            callback_gate: CaptureCallbackGate::default(),
            terminal_error_callback: Arc::new(Mutex::new(None)),
        })
    }

    pub fn preflight(request: SystemAudioCaptureRequest) -> AudioResult<()> {
        validate_capture_request(request)?;
        build_stream_configuration()?;
        shareable_display().map(|_| ())
    }
}

#[derive(Clone, Default)]
struct CaptureCallbackGate {
    running: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
}

type CaptureCallbackSlot = Arc<Mutex<Option<AudioChunkCallback>>>;
type CaptureErrorCallbackSlot = Arc<Mutex<Option<AudioCaptureErrorCallback>>>;

impl CaptureCallbackGate {
    fn begin_capture(&self) -> u64 {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.running.store(true, Ordering::SeqCst);
        generation
    }

    fn stop_capture(&self) -> bool {
        let was_running = self.running.swap(false, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst);
        was_running
    }

    fn should_emit(&self, generation: u64) -> bool {
        self.running.load(Ordering::Relaxed)
            && self.generation.load(Ordering::Relaxed) == generation
    }

    fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}

fn validate_capture_request(request: SystemAudioCaptureRequest) -> AudioResult<()> {
    if request.self_audio_exclusion != SelfAudioExclusionRequirement::Required {
        return Err(AudioError::Configuration(
            "macOS realtime system capture requires current-process audio exclusion".into(),
        ));
    }
    if request.target.sample_rate == 0 || request.target.channels != 1 {
        return Err(AudioError::Configuration(format!(
            "macOS system capture target must be mono with a non-zero sample rate, got {} Hz {} ch",
            request.target.sample_rate, request.target.channels
        )));
    }
    Ok(())
}

fn target_audio_config(target: AudioCaptureTarget) -> AudioConfig {
    AudioConfig {
        sample_rate: target.sample_rate,
        channels: target.channels,
        buffer_size: ((target.sample_rate as usize * TARGET_FRAME_MS) / 1000).max(1) as u32,
    }
}

fn shareable_display() -> AudioResult<SCDisplay> {
    let content = SCShareableContent::get().map_err(|error| {
        AudioError::AccessDenied(format!(
            "ScreenCaptureKit unavailable: {error}. Откройте macOS System Settings -> Privacy & Security -> Screen & System Audio Recording и разрешите доступ для приложения."
        ))
    })?;
    content.displays().into_iter().next().ok_or_else(|| {
        AudioError::DeviceNotFound("No displays found for system audio capture".into())
    })
}

fn build_stream_configuration() -> AudioResult<SCStreamConfiguration> {
    let config = SCStreamConfiguration::new()
        .with_width(2)
        .with_height(2)
        .with_captures_audio(true)
        .with_excludes_current_process_audio(true)
        .with_sample_rate(NATIVE_SAMPLE_RATE as i32)
        .with_channel_count(NATIVE_CHANNELS as i32);
    if !config.excludes_current_process_audio() {
        return Err(AudioError::Configuration(
            "ScreenCaptureKit did not accept required current-process audio exclusion".into(),
        ));
    }
    Ok(config)
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
    error_callback_slot: &CaptureErrorCallbackSlot,
    message: String,
) {
    let should_report = callback_gate.stop_capture();
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
    if should_report {
        emit_terminal_capture_error(error_callback_slot, message);
    }
}

fn emit_terminal_capture_error(callback_slot: &CaptureErrorCallbackSlot, message: String) {
    let callback = match callback_slot.lock() {
        Ok(callback) => callback.clone(),
        Err(error) => {
            log::error!("ScreenCaptureKit error callback slot poisoned: {}", error);
            return;
        }
    };
    if let Some(callback) = callback {
        if catch_unwind(AssertUnwindSafe(|| callback(AudioError::Capture(message)))).is_err() {
            log::error!("ScreenCaptureKit terminal error callback panicked; revoking it");
            if let Ok(mut callback) = callback_slot.lock() {
                *callback = None;
            }
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
    target_frame_samples: usize,
}

enum PipelineState {
    Pending,
    Ready(ResamplePipeline),
    Failed,
}

impl ResamplePipeline {
    fn new(
        from_sample_rate: u32,
        target: AudioCaptureTarget,
        is_float: bool,
        is_big_endian: bool,
        bits_per_channel: u32,
    ) -> AudioResult<Self> {
        let input_frame_samples = ((from_sample_rate as usize) * TARGET_FRAME_MS / 1000).max(1);
        let target_frame_samples = ((target.sample_rate as usize) * TARGET_FRAME_MS / 1000).max(1);
        let params = SincInterpolationParameters {
            sinc_len: 256,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 256,
            window: WindowFunction::BlackmanHarris2,
        };
        let resampler = SincFixedIn::<f32>::new(
            target.sample_rate as f64 / from_sample_rate as f64,
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
            out_i16: Vec::with_capacity(target_frame_samples * 6),
            input_frame_samples,
            target_frame_samples,
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

        while self.out_i16.len() >= self.target_frame_samples {
            frames.push(self.out_i16.drain(..self.target_frame_samples).collect());
        }

        frames
    }
}

fn build_resample_pipeline(
    sample_rate: u32,
    target: AudioCaptureTarget,
    is_float: bool,
    is_big_endian: bool,
    bits: u32,
) -> Option<ResamplePipeline> {
    match ResamplePipeline::new(sample_rate, target, is_float, is_big_endian, bits) {
        Ok(pipe) => Some(pipe),
        Err(primary_err) => {
            log::error!("Failed to init ScreenCaptureKit resampler: {}", primary_err);
            match ResamplePipeline::new(NATIVE_SAMPLE_RATE, target, true, false, 32) {
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
        if config.sample_rate != self.target.sample_rate || config.channels != self.target.channels
        {
            return Err(AudioError::Configuration(format!(
                "macOS system capture was created for {} Hz {} ch but initialized with {} Hz {} ch",
                self.target.sample_rate, self.target.channels, config.sample_rate, config.channels
            )));
        }
        self.audio_config = config;
        Ok(())
    }

    async fn start_capture(&mut self, on_chunk: AudioChunkCallback) -> AudioResult<()> {
        if self.callback_gate.is_running() {
            return Err(AudioError::Capture("Already capturing audio".to_string()));
        }
        if let Some(stale_stream) = self.stream.take() {
            let _ = stale_stream.stop_capture();
        }
        let capture_generation = self.callback_gate.begin_capture();

        let display = shareable_display().inspect_err(|_| {
            self.callback_gate.stop_capture();
        })?;

        let filter = SCContentFilter::create()
            .with_display(&display)
            .with_excluding_windows(&[])
            .build();
        let config = build_stream_configuration().inspect_err(|_| {
            self.callback_gate.stop_capture();
        })?;

        let pipeline: Arc<Mutex<PipelineState>> = Arc::new(Mutex::new(PipelineState::Pending));
        let pipeline_for_cb = pipeline.clone();
        let callback_slot: CaptureCallbackSlot = Arc::new(Mutex::new(Some(on_chunk)));
        let callback_slot_for_audio = callback_slot.clone();
        let callback_gate = self.callback_gate.clone();
        let callback_gate_for_error = self.callback_gate.clone();
        let callback_slot_for_error = callback_slot.clone();
        let error_callback_slot = self.terminal_error_callback.clone();
        let error_handler = ErrorHandler::new(move |error| {
            let message = error.to_string();
            log::error!("ScreenCaptureKit stream error: {}", message);
            deactivate_capture_after_stream_error(
                &callback_gate_for_error,
                &callback_slot_for_error,
                &error_callback_slot,
                message,
            );
        });
        let target = self.target;

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
                        build_resample_pipeline(sample_rate, target, is_float, is_big_endian, bits)
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
                            AudioChunk::new(frame, target.sample_rate, target.channels),
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
        if !self.callback_gate.is_running() {
            if let Some(failed_stream) = self.stream.take() {
                let _ = failed_stream.stop_capture();
            }
            return Err(AudioError::Capture(
                "ScreenCaptureKit stream failed during startup".into(),
            ));
        }
        Ok(())
    }

    async fn stop_capture(&mut self) -> AudioResult<()> {
        self.callback_gate.stop_capture();
        if let Some(stream) = self.stream.take() {
            let _ = stream.stop_capture();
        }
        Ok(())
    }

    fn set_terminal_error_callback(&mut self, callback: Option<AudioCaptureErrorCallback>) {
        match self.terminal_error_callback.lock() {
            Ok(mut slot) => *slot = callback,
            Err(error) => log::error!(
                "ScreenCaptureKit terminal error callback slot poisoned: {}",
                error
            ),
        }
    }

    fn is_capturing(&self) -> bool {
        self.callback_gate.is_running()
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
        let errors = Arc::new(AtomicU64::new(0));
        let error_callback_slot: CaptureErrorCallbackSlot = Arc::new(Mutex::new(Some({
            let errors = errors.clone();
            Arc::new(move |_error| {
                errors.fetch_add(1, Ordering::SeqCst);
            })
        })));

        deactivate_capture_after_stream_error(
            &gate,
            &callback_slot,
            &error_callback_slot,
            "device lost".into(),
        );
        deactivate_capture_after_stream_error(
            &gate,
            &callback_slot,
            &error_callback_slot,
            "duplicate native error".into(),
        );

        assert!(!gate.should_emit(generation));
        assert!(callback_slot.lock().unwrap().is_none());
        assert_eq!(errors.load(Ordering::SeqCst), 1);
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

    #[test]
    fn stream_configuration_enforces_native_audio_and_self_exclusion() {
        let config = build_stream_configuration().unwrap();

        assert!(config.captures_audio());
        assert!(config.excludes_current_process_audio());
        assert_eq!(config.sample_rate(), NATIVE_SAMPLE_RATE as i32);
        assert_eq!(config.channel_count(), NATIVE_CHANNELS as i32);
    }

    #[test]
    fn capture_keeps_requested_caption_and_realtime_targets() {
        for target in [
            AudioCaptureTarget::incoming_subtitles(),
            AudioCaptureTarget::incoming_realtime_translation(),
        ] {
            let capture =
                MacosSystemAudioCapture::new(SystemAudioCaptureRequest::isolated(target)).unwrap();

            assert_eq!(capture.target, target);
            assert_eq!(capture.config().sample_rate, target.sample_rate);
            assert_eq!(capture.config().channels, target.channels);
            assert_eq!(
                capture.config().buffer_size,
                target.sample_rate * TARGET_FRAME_MS as u32 / 1000
            );
        }
    }

    #[test]
    fn resampler_emits_frames_for_each_requested_target() {
        for (target, expected_frame_samples) in [
            (AudioCaptureTarget::incoming_subtitles(), 480usize),
            (
                AudioCaptureTarget::incoming_realtime_translation(),
                720usize,
            ),
        ] {
            let mut pipeline =
                ResamplePipeline::new(NATIVE_SAMPLE_RATE, target, true, false, 32).unwrap();
            pipeline
                .native_mono
                .resize(pipeline.input_frame_samples * 4, 0.25);

            let frames = pipeline.drain_frames();

            assert!(!frames.is_empty());
            assert!(
                frames
                    .iter()
                    .all(|frame| frame.len() == expected_frame_samples),
                "unexpected frame size for {target:?}"
            );
        }
    }

    #[tokio::test]
    async fn initialize_rejects_config_that_does_not_match_factory_target() {
        let mut capture = MacosSystemAudioCapture::new(SystemAudioCaptureRequest::isolated(
            AudioCaptureTarget::incoming_realtime_translation(),
        ))
        .unwrap();

        let error = capture
            .initialize(AudioConfig::default())
            .await
            .unwrap_err();

        assert!(
            matches!(error, AudioError::Configuration(message) if message.contains("24_000") || message.contains("24000"))
        );
    }
}
