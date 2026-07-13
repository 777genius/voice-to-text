use async_trait::async_trait;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Host, SampleFormat, Stream, StreamConfig, SupportedStreamConfig};
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::domain::{AudioDeviceId, AudioEnqueueOutcome, TranslationAudioOutput};

pub use crate::domain::{
    TranslationAudioOutput as AudioOutput, TranslationAudioOutputConfig as AudioOutputConfig,
    TranslationAudioOutputError as AudioOutputError,
    TranslationAudioOutputResult as AudioOutputResult,
};

/// Размер чанка для resampler внутри output pipeline (в source-сэмплах, mono).
const RESAMPLER_CHUNK_SIZE: usize = 256;
const NANOS_PER_SECOND: u128 = 1_000_000_000;

fn apply_output_gain(sample: f32, gain: f32) -> f32 {
    let amplified = sample * gain;
    if gain <= 1.0 {
        amplified.clamp(-1.0, 1.0)
    } else {
        amplified.tanh()
    }
}

fn frames_for_duration(duration: Duration, sample_rate: u32) -> usize {
    let numerator = duration.as_nanos().saturating_mul(u128::from(sample_rate));
    numerator
        .saturating_add(NANOS_PER_SECOND - 1)
        .checked_div(NANOS_PER_SECOND)
        .unwrap_or(0)
        .min(usize::MAX as u128) as usize
}

/// Имя env-переменной для оверрайда output устройства (для devs которым нужно другое имя).
pub const ENV_TRANSLATION_OUTPUT_DEVICE: &str = "VOICETEXT_TRANSLATION_OUTPUT_DEVICE";

/// Кандидаты названий BlackHole, по которым ищем устройство (в порядке убывания специфичности).
pub const MACOS_BLACKHOLE_DEVICE_NAMES: &[&str] = &["BlackHole 2ch", "BlackHole"];

/// Кандидаты VB-CABLE output endpoint. В приложения для звонков выбирается CABLE Output,
/// а VoicetextAI пишет именно в CABLE Input.
pub const WINDOWS_VB_CABLE_OUTPUT_DEVICE_NAMES: &[&str] =
    &["CABLE Input", "VB-Audio Virtual Cable", "VB-CABLE"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputDeviceSelector {
    SystemDefault,
    Explicit(AudioDeviceId),
    CandidateNames {
        env_var: &'static str,
        candidates: &'static [&'static str],
        not_found_message: &'static str,
    },
}

impl OutputDeviceSelector {
    pub fn macos_blackhole() -> Self {
        Self::CandidateNames {
            env_var: ENV_TRANSLATION_OUTPUT_DEVICE,
            candidates: MACOS_BLACKHOLE_DEVICE_NAMES,
            not_found_message:
                "BlackHole 2ch не найден. Установите blackhole-2ch (brew install --cask blackhole-2ch), \
                 перезагрузите macOS и выберите BlackHole 2ch как микрофон в Meet/Zoom. \
                 Также можно задать VOICETEXT_TRANSLATION_OUTPUT_DEVICE с подстрокой имени нужного устройства.",
        }
    }

    pub fn windows_vb_cable() -> Self {
        Self::CandidateNames {
            env_var: ENV_TRANSLATION_OUTPUT_DEVICE,
            candidates: WINDOWS_VB_CABLE_OUTPUT_DEVICE_NAMES,
            not_found_message:
                "VB-CABLE не найден. Установите VB-Audio Virtual Cable, перезагрузите Windows, \
                 выберите CABLE Output как микрофон в Meet/Zoom, а VoicetextAI будет писать в CABLE Input. \
                 Также можно задать VOICETEXT_TRANSLATION_OUTPUT_DEVICE с подстрокой имени output устройства.",
        }
    }
}

#[derive(Debug)]
struct OutputPlaybackState {
    prebuffering: bool,
    draining: bool,
    underrun_count: u64,
    overflow_count: u64,
    dropped_audio_ms: u64,
}

impl Default for OutputPlaybackState {
    fn default() -> Self {
        Self {
            prebuffering: true,
            draining: false,
            underrun_count: 0,
            overflow_count: 0,
            dropped_audio_ms: 0,
        }
    }
}

/// CPAL-output, направленный на platform virtual microphone endpoint
/// (BlackHole/VB-CABLE или иное устройство из env override).
/// Не использует async внутри audio callback — только sync Mutex с короткими lock-окнами.
pub struct CpalAudioOutput {
    selector: OutputDeviceSelector,
    device: Option<Device>,
    device_name: Option<String>,
    stream: Option<Stream>,
    native_config: Option<SupportedStreamConfig>,
    config: Option<AudioOutputConfig>,

    /// Входная очередь PCM16 mono из источника (OpenAI 24 kHz).
    source_queue: Arc<Mutex<VecDeque<i16>>>,
    /// Готовый к воспроизведению буфер f32, уже в native sample rate и channel layout.
    output_ready: Arc<Mutex<VecDeque<f32>>>,
    playback_state: Arc<Mutex<OutputPlaybackState>>,
    /// CPAL reports device loss asynchronously through the stream error callback.
    stream_error: Arc<Mutex<Option<String>>>,
    /// Resampler 24 kHz mono → native sample rate mono (если нужно).
    resampler: Option<Arc<Mutex<SincFixedIn<f32>>>>,
    /// Признак что output открыт (атомарно бы лучше, но используем под общим контролем).
    is_open: bool,
}

impl CpalAudioOutput {
    pub fn new() -> Self {
        Self::platform_default()
    }

    pub fn platform_default() -> Self {
        #[cfg(target_os = "windows")]
        {
            Self::windows_vb_cable()
        }

        #[cfg(target_os = "macos")]
        {
            Self::macos_blackhole()
        }

        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            Self::with_selector(OutputDeviceSelector::CandidateNames {
                env_var: ENV_TRANSLATION_OUTPUT_DEVICE,
                candidates: &[],
                not_found_message:
                    "CPAL virtual microphone output is not configured for this platform.",
            })
        }
    }

    pub fn macos_blackhole() -> Self {
        Self::with_selector(OutputDeviceSelector::macos_blackhole())
    }

    pub fn windows_vb_cable() -> Self {
        Self::with_selector(OutputDeviceSelector::windows_vb_cable())
    }

    pub fn system_default() -> Self {
        Self::with_selector(OutputDeviceSelector::SystemDefault)
    }

    pub fn explicit_device(device_id: AudioDeviceId) -> Self {
        Self::with_selector(OutputDeviceSelector::Explicit(device_id))
    }

    pub fn with_selector(selector: OutputDeviceSelector) -> Self {
        Self {
            selector,
            device: None,
            device_name: None,
            stream: None,
            native_config: None,
            config: None,
            source_queue: Arc::new(Mutex::new(VecDeque::with_capacity(48_000))),
            output_ready: Arc::new(Mutex::new(VecDeque::with_capacity(192_000))),
            playback_state: Arc::new(Mutex::new(OutputPlaybackState::default())),
            stream_error: Arc::new(Mutex::new(None)),
            resampler: None,
            is_open: false,
        }
    }

    fn select_output_device(
        host: &Host,
        selector: &OutputDeviceSelector,
    ) -> AudioOutputResult<(Device, String)> {
        if matches!(selector, OutputDeviceSelector::SystemDefault) {
            let device = host.default_output_device().ok_or_else(|| {
                AudioOutputError::Device("No system default output device is available".into())
            })?;
            let name = device
                .name()
                .map_err(|error| AudioOutputError::Device(error.to_string()))?;
            return Ok((device, name));
        }

        if let OutputDeviceSelector::Explicit(device_id) = selector {
            let device = host
                .output_devices()
                .map_err(|error| AudioOutputError::Device(error.to_string()))?
                .find(|device| {
                    device
                        .name()
                        .map(|name| name == device_id.as_str())
                        .unwrap_or(false)
                })
                .ok_or_else(|| {
                    AudioOutputError::Device(format!(
                        "Requested output device '{}' is unavailable",
                        device_id.as_str()
                    ))
                })?;
            let name = device
                .name()
                .unwrap_or_else(|_| device_id.as_str().to_string());
            return Ok((device, name));
        }

        let OutputDeviceSelector::CandidateNames {
            env_var,
            candidates,
            not_found_message,
        } = selector
        else {
            unreachable!("all output selectors handled")
        };

        if let Ok(override_name) = std::env::var(env_var) {
            let trimmed = override_name.trim();
            if !trimmed.is_empty() {
                let device = host
                    .output_devices()
                    .map_err(|e| AudioOutputError::Device(e.to_string()))?
                    .find(|d| {
                        d.name()
                            .map(|name| output_device_name_matches(&name, trimmed))
                            .unwrap_or(false)
                    });
                return match device {
                    Some(d) => {
                        let name = d.name().unwrap_or_else(|_| trimmed.to_string());
                        log::info!("Output device picked via {}: {}", env_var, name);
                        Ok((d, name))
                    }
                    None => Err(AudioOutputError::Configuration(format!(
                        "Output устройство '{}' (из {}) не найдено в системе.",
                        trimmed, env_var
                    ))),
                };
            }
        }

        let outputs: Vec<Device> = host
            .output_devices()
            .map_err(|e| AudioOutputError::Device(e.to_string()))?
            .collect();

        for candidate in *candidates {
            for dev in &outputs {
                if let Ok(name) = dev.name() {
                    if output_device_name_matches(&name, candidate) {
                        log::info!("Auto-picked output device: {}", name);
                        return Ok((dev.clone(), name));
                    }
                }
            }
        }

        Err(AudioOutputError::Configuration(
            not_found_message.to_string(),
        ))
    }

    fn build_resampler(
        from_sample_rate: u32,
        to_sample_rate: u32,
    ) -> AudioOutputResult<SincFixedIn<f32>> {
        let params = SincInterpolationParameters {
            sinc_len: 256,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 256,
            window: WindowFunction::BlackmanHarris2,
        };

        SincFixedIn::<f32>::new(
            to_sample_rate as f64 / from_sample_rate as f64,
            2.0,
            params,
            RESAMPLER_CHUNK_SIZE,
            1, // input mono
        )
        .map_err(|e| AudioOutputError::Resample(e.to_string()))
    }

    fn enqueue_bounded_source_samples(
        queue: &Arc<Mutex<VecDeque<i16>>>,
        samples: &[i16],
        max_source_samples: usize,
    ) -> AudioOutputResult<usize> {
        let max_source_samples = max_source_samples.max(RESAMPLER_CHUNK_SIZE);
        let mut queue = queue
            .lock()
            .map_err(|_| AudioOutputError::Stream("source_queue poisoned".into()))?;
        let total = queue.len().saturating_add(samples.len());
        let dropped = total.saturating_sub(max_source_samples);
        let drop_from_queue = dropped.min(queue.len());
        queue.drain(..drop_from_queue);
        let drop_from_input = dropped.saturating_sub(drop_from_queue).min(samples.len());
        queue.extend(samples[drop_from_input..].iter().copied());
        Ok(dropped)
    }

    /// Берёт source_queue, прогоняет через resampler фиксированными чанками и
    /// раскладывает mono-результат на native_ch (дублируем каждый сэмпл).
    fn drain_source_and_push(
        source_queue: &Arc<Mutex<VecDeque<i16>>>,
        output_ready: &Arc<Mutex<VecDeque<f32>>>,
        resampler: &Option<Arc<Mutex<SincFixedIn<f32>>>>,
        native_channels: usize,
        max_buffered_frames: usize,
        gain: f32,
        flush_partial: bool,
    ) -> AudioOutputResult<usize> {
        // Берём всё что есть, но обрабатываем фиксированными чанками RESAMPLER_CHUNK_SIZE.
        let mut local_chunks: Vec<Vec<i16>> = Vec::new();
        {
            let mut q = source_queue
                .lock()
                .map_err(|_| AudioOutputError::Stream("source_queue poisoned".into()))?;
            while q.len() >= RESAMPLER_CHUNK_SIZE {
                let chunk: Vec<i16> = q.drain(..RESAMPLER_CHUNK_SIZE).collect();
                local_chunks.push(chunk);
            }

            if flush_partial && !q.is_empty() {
                let mut chunk: Vec<i16> = q.drain(..).collect();
                if resampler.is_some() && chunk.len() < RESAMPLER_CHUNK_SIZE {
                    chunk.resize(RESAMPLER_CHUNK_SIZE, 0);
                }
                local_chunks.push(chunk);
            }
        }

        if local_chunks.is_empty() {
            return Ok(0);
        }

        let mut expanded_samples = Vec::new();
        for chunk in local_chunks {
            let mono_f32: Vec<f32> = chunk
                .iter()
                .map(|&sample| apply_output_gain(sample as f32 / 32_768.0, gain))
                .collect();
            let native_mono: Vec<f32> = if let Some(rs) = resampler {
                let mut r = rs
                    .lock()
                    .map_err(|_| AudioOutputError::Resample("resampler state poisoned".into()))?;
                let mut output_per_channel = r
                    .process(&[mono_f32], None)
                    .map_err(|e| AudioOutputError::Resample(e.to_string()))?;
                output_per_channel.pop().unwrap_or_default()
            } else {
                mono_f32
            };

            // mono → native_ch: дублируем сэмпл во все каналы.
            for sample in native_mono {
                let sample = sample.clamp(-1.0, 1.0);
                for _ in 0..native_channels.max(1) {
                    expanded_samples.push(sample);
                }
            }
        }

        if expanded_samples.is_empty() {
            return Ok(0);
        }

        let mut out = output_ready
            .lock()
            .map_err(|_| AudioOutputError::Stream("output_ready poisoned".into()))?;
        out.extend(expanded_samples);

        // Bounded queue: если выросло выше потолка (frames * channels) — дропаем старые сэмплы.
        let cap_samples = max_buffered_frames.saturating_mul(native_channels.max(1));
        let dropped_samples = out.len().saturating_sub(cap_samples);
        if dropped_samples > 0 {
            for _ in 0..dropped_samples {
                out.pop_front();
            }
            log::warn!(
                "CpalAudioOutput overflow: dropped {} samples (queue cap {} frames * {} ch)",
                dropped_samples,
                max_buffered_frames,
                native_channels
            );
        }
        Ok(dropped_samples / native_channels.max(1))
    }

    fn build_stream(
        device: &Device,
        native: &SupportedStreamConfig,
        output_ready: Arc<Mutex<VecDeque<f32>>>,
        playback_state: Arc<Mutex<OutputPlaybackState>>,
        stream_error: Arc<Mutex<Option<String>>>,
        prebuffer_samples: usize,
    ) -> AudioOutputResult<Stream> {
        let stream_config: StreamConfig = native.clone().into();
        let sample_format = native.sample_format();
        let err_fn = move |err: cpal::StreamError| {
            let message = err.to_string();
            log::error!("CpalAudioOutput stream error: {}", message);
            if let Ok(mut stored_error) = stream_error.lock() {
                if stored_error.is_none() {
                    *stored_error = Some(message);
                }
            }
        };

        let make_callback = || {
            let q = output_ready.clone();
            let state = playback_state.clone();
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                fill_output(data, &q, &state, prebuffer_samples);
            }
        };

        match sample_format {
            SampleFormat::F32 => device
                .build_output_stream(&stream_config, make_callback(), err_fn, None)
                .map_err(|e| AudioOutputError::Stream(e.to_string())),
            SampleFormat::I16 => {
                let q = output_ready.clone();
                let state = playback_state.clone();
                device
                    .build_output_stream(
                        &stream_config,
                        move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                            fill_output_i16(data, &q, &state, prebuffer_samples);
                        },
                        err_fn,
                        None,
                    )
                    .map_err(|e| AudioOutputError::Stream(e.to_string()))
            }
            SampleFormat::U16 => {
                let q = output_ready.clone();
                let state = playback_state.clone();
                device
                    .build_output_stream(
                        &stream_config,
                        move |data: &mut [u16], _: &cpal::OutputCallbackInfo| {
                            fill_output_u16(data, &q, &state, prebuffer_samples);
                        },
                        err_fn,
                        None,
                    )
                    .map_err(|e| AudioOutputError::Stream(e.to_string()))
            }
            other => Err(AudioOutputError::Configuration(format!(
                "Неподдерживаемый формат семплов output устройства: {:?}",
                other
            ))),
        }
    }

    /// Raises queue headroom for graceful stop before OpenAI starts sending the final tail.
    pub fn begin_drain_mode(&self) {
        if let Ok(mut state) = self.playback_state.lock() {
            state.draining = true;
            state.prebuffering = false;
        }
    }

    fn record_output_drop(&self, duration: Duration) {
        if duration.is_zero() {
            return;
        }
        if let Ok(mut state) = self.playback_state.lock() {
            state.overflow_count = state.overflow_count.saturating_add(1);
            state.dropped_audio_ms = state
                .dropped_audio_ms
                .saturating_add(duration.as_millis().min(u64::MAX as u128) as u64);
        }
    }

    /// Flushes the final partial source chunk and lets the output callback play
    /// whatever remains without waiting for another full prebuffer.
    pub fn prepare_for_drain(&self) -> AudioOutputResult<Duration> {
        if !self.is_open {
            return Ok(Duration::ZERO);
        }
        self.ensure_stream_healthy()?;
        let Some(native) = self.native_config.as_ref() else {
            return Err(AudioOutputError::Closed);
        };
        let Some(cfg) = self.config.as_ref() else {
            return Err(AudioOutputError::Closed);
        };
        let dropped_frames = Self::drain_source_and_push(
            &self.source_queue,
            &self.output_ready,
            &self.resampler,
            native.channels() as usize,
            frames_for_duration(cfg.drain_max_buffered_duration, native.sample_rate().0),
            cfg.gain,
            true,
        )?;
        if dropped_frames > 0 {
            let dropped_duration = Duration::from_secs_f64(
                dropped_frames as f64 / native.sample_rate().0.max(1) as f64,
            );
            self.record_output_drop(dropped_duration);
            return Err(AudioOutputError::Stream(format!(
                "translation output overflowed during drain and dropped {} ms",
                dropped_duration.as_millis()
            )));
        }

        let pending = self.pending_playback_duration();
        if pending > Duration::ZERO {
            if let Ok(mut state) = self.playback_state.lock() {
                state.prebuffering = false;
            }
        }
        Ok(pending)
    }

    fn ensure_stream_healthy(&self) -> AudioOutputResult<()> {
        let stored_error = self
            .stream_error
            .lock()
            .map_err(|_| AudioOutputError::Stream("stream_error state poisoned".into()))?;
        match stored_error.as_ref() {
            Some(message) => Err(AudioOutputError::Stream(format!(
                "audio output stream failed: {}",
                message
            ))),
            None => self.ensure_default_route_unchanged(),
        }
    }

    fn ensure_default_route_unchanged(&self) -> AudioOutputResult<()> {
        if !self.is_open || !matches!(self.selector, OutputDeviceSelector::SystemDefault) {
            return Ok(());
        }
        let opened_name = self
            .device_name
            .as_deref()
            .ok_or(AudioOutputError::Closed)?;
        let current_device = cpal::default_host()
            .default_output_device()
            .ok_or_else(|| {
                AudioOutputError::Device(
                    "System default output device is no longer available".into(),
                )
            })?;
        let current_name = current_device
            .name()
            .map_err(|error| AudioOutputError::Device(error.to_string()))?;
        if current_name != opened_name {
            return Err(AudioOutputError::Device(format!(
                "System default output changed from '{}' to '{}'; restart translated playback",
                opened_name, current_name
            )));
        }
        Ok(())
    }

    /// Estimated amount of audio still queued for playback.
    pub fn pending_playback_duration(&self) -> Duration {
        let Some(native) = self.native_config.as_ref() else {
            return Duration::ZERO;
        };
        let native_sr = native.sample_rate().0 as u128;
        let native_channels = (native.channels() as usize).max(1);
        if native_sr == 0 {
            return Duration::ZERO;
        }

        let output_samples = self.output_ready.lock().map(|q| q.len()).unwrap_or(0);
        let output_frames = output_samples / native_channels;
        let output_ms = (output_frames as u128).saturating_mul(1000) / native_sr;

        let source_ms = if let Some(cfg) = self.config.as_ref() {
            let source_samples = self.source_queue.lock().map(|q| q.len()).unwrap_or(0);
            if cfg.source_sample_rate > 0 {
                (source_samples as u128).saturating_mul(1000) / cfg.source_sample_rate as u128
            } else {
                0
            }
        } else {
            0
        };

        Duration::from_millis((output_ms + source_ms).min(u64::MAX as u128) as u64)
    }
}

impl Default for CpalAudioOutput {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TranslationAudioOutput for CpalAudioOutput {
    async fn open(&mut self, config: AudioOutputConfig) -> AudioOutputResult<()> {
        if self.is_open {
            return Err(AudioOutputError::Configuration(
                "CpalAudioOutput уже открыт".to_string(),
            ));
        }
        let config = config.normalized();
        if config.source_sample_rate == 0
            || config.source_channels != 1
            || config.max_buffered_duration.is_zero()
            || config.drain_max_buffered_duration < config.max_buffered_duration
        {
            return Err(AudioOutputError::Configuration(format!(
                "Invalid translation output config: {} Hz {} ch, buffer={}ms drain_buffer={}ms",
                config.source_sample_rate,
                config.source_channels,
                config.max_buffered_duration.as_millis(),
                config.drain_max_buffered_duration.as_millis()
            )));
        }
        let host = cpal::default_host();
        let (device, name) = Self::select_output_device(&host, &self.selector)?;
        let native = device
            .default_output_config()
            .map_err(|e| AudioOutputError::Configuration(e.to_string()))?;

        let native_sr = native.sample_rate().0;
        let native_ch = native.channels() as usize;
        if native_sr == 0 || native_ch == 0 {
            return Err(AudioOutputError::Configuration(format!(
                "Output устройство '{}' вернуло некорректный конфиг: sr={}, ch={}",
                name, native_sr, native_ch
            )));
        }
        log::info!(
            "Opening CpalAudioOutput '{}': source {} Hz {} ch -> native {} Hz {} ch (format: {:?})",
            name,
            config.source_sample_rate,
            config.source_channels,
            native_sr,
            native_ch,
            native.sample_format()
        );

        let needs_resample = native_sr != config.source_sample_rate;
        let resampler = if needs_resample {
            Some(Arc::new(Mutex::new(Self::build_resampler(
                config.source_sample_rate,
                native_sr,
            )?)))
        } else {
            None
        };

        if let Ok(mut q) = self.source_queue.lock() {
            q.clear();
        }
        if let Ok(mut q) = self.output_ready.lock() {
            q.clear();
        }
        if let Ok(mut state) = self.playback_state.lock() {
            state.prebuffering = true;
            state.draining = false;
        }
        if let Ok(mut stream_error) = self.stream_error.lock() {
            *stream_error = None;
        }

        let prebuffer_frames =
            ((native_sr as u64).saturating_mul(config.prebuffer_ms) / 1000).max(1) as usize;
        let prebuffer_samples = prebuffer_frames.saturating_mul(native_ch.max(1));

        let stream = Self::build_stream(
            &device,
            &native,
            self.output_ready.clone(),
            self.playback_state.clone(),
            self.stream_error.clone(),
            prebuffer_samples,
        )?;
        stream
            .play()
            .map_err(|e| AudioOutputError::Stream(e.to_string()))?;

        self.device = Some(device);
        self.device_name = Some(name);
        self.stream = Some(stream);
        self.native_config = Some(native);
        self.config = Some(config);
        self.resampler = resampler;
        self.is_open = true;

        log::info!("CpalAudioOutput opened successfully");
        Ok(())
    }

    async fn enqueue_pcm16(&self, samples: &[i16]) -> AudioOutputResult<AudioEnqueueOutcome> {
        if !self.is_open {
            return Err(AudioOutputError::Closed);
        }
        self.ensure_stream_healthy()?;
        if samples.is_empty() {
            return Ok(AudioEnqueueOutcome::Queued {
                pending: self.pending_playback_duration(),
            });
        }
        let Some(native) = self.native_config.as_ref() else {
            return Err(AudioOutputError::Closed);
        };
        let Some(cfg) = self.config.as_ref() else {
            return Err(AudioOutputError::Closed);
        };
        if cfg.gain == 0.0 {
            return Ok(AudioEnqueueOutcome::Queued {
                pending: Duration::ZERO,
            });
        }
        let native_channels = native.channels() as usize;
        let max_buffered_frames = if self
            .playback_state
            .lock()
            .map(|state| state.draining)
            .unwrap_or(false)
        {
            frames_for_duration(cfg.drain_max_buffered_duration, native.sample_rate().0)
        } else {
            frames_for_duration(cfg.max_buffered_duration, native.sample_rate().0)
        };

        let max_source_samples = ((max_buffered_frames as u128)
            .saturating_mul(cfg.source_sample_rate as u128)
            / native.sample_rate().0.max(1) as u128)
            .max(RESAMPLER_CHUNK_SIZE as u128)
            .min(usize::MAX as u128) as usize;
        let dropped_source_samples =
            Self::enqueue_bounded_source_samples(&self.source_queue, samples, max_source_samples)?;

        let dropped_frames = Self::drain_source_and_push(
            &self.source_queue,
            &self.output_ready,
            &self.resampler,
            native_channels,
            max_buffered_frames,
            cfg.gain,
            false,
        )?;
        let pending = self.pending_playback_duration();
        if dropped_frames == 0 && dropped_source_samples == 0 {
            Ok(AudioEnqueueOutcome::Queued { pending })
        } else {
            let native_sample_rate = native.sample_rate().0.max(1) as u64;
            let dropped_source_duration = Duration::from_secs_f64(
                dropped_source_samples as f64 / cfg.source_sample_rate.max(1) as f64,
            );
            let dropped_output_duration =
                Duration::from_secs_f64(dropped_frames as f64 / native_sample_rate as f64);
            let dropped_duration = dropped_source_duration.saturating_add(dropped_output_duration);
            self.record_output_drop(dropped_duration);
            Ok(AudioEnqueueOutcome::DroppedOldest {
                duration: dropped_duration,
                pending,
            })
        }
    }

    async fn close(&mut self) -> AudioOutputResult<()> {
        self.is_open = false;
        let closed_device_name = self.device_name.as_deref().unwrap_or("unknown").to_string();
        if let Some(s) = self.stream.take() {
            drop(s);
        }
        self.device = None;
        self.device_name = None;
        self.native_config = None;
        self.config = None;
        self.resampler = None;
        if let Ok(mut q) = self.source_queue.lock() {
            q.clear();
        }
        if let Ok(mut q) = self.output_ready.lock() {
            q.clear();
        }
        if let Ok(mut state) = self.playback_state.lock() {
            log::info!(
                "cpal_output_diagnostics device={} underruns={} overflows={} dropped_audio_ms={}",
                closed_device_name,
                state.underrun_count,
                state.overflow_count,
                state.dropped_audio_ms
            );
            state.prebuffering = true;
            state.draining = false;
            state.underrun_count = 0;
            state.overflow_count = 0;
            state.dropped_audio_ms = 0;
        }
        if let Ok(mut stream_error) = self.stream_error.lock() {
            *stream_error = None;
        }
        log::info!("CpalAudioOutput closed");
        Ok(())
    }

    fn set_gain(&mut self, gain: f32) -> AudioOutputResult<()> {
        let Some(config) = self.config.as_mut() else {
            return Err(AudioOutputError::Closed);
        };
        let gain = crate::domain::normalize_output_gain(gain);
        config.gain = gain;
        if gain == 0.0 {
            self.source_queue
                .lock()
                .map_err(|_| AudioOutputError::Stream("source_queue poisoned".into()))?
                .clear();
            self.output_ready
                .lock()
                .map_err(|_| AudioOutputError::Stream("output_ready poisoned".into()))?
                .clear();
        }
        Ok(())
    }

    fn is_open(&self) -> bool {
        self.is_open && self.ensure_stream_healthy().is_ok()
    }

    fn health_check(&self) -> AudioOutputResult<()> {
        if !self.is_open {
            return Err(AudioOutputError::Closed);
        }
        self.ensure_stream_healthy()
    }

    fn device_name(&self) -> Option<String> {
        self.device_name.clone()
    }

    fn begin_drain_mode(&self) {
        CpalAudioOutput::begin_drain_mode(self);
    }

    fn prepare_for_drain(&self) -> AudioOutputResult<Duration> {
        CpalAudioOutput::prepare_for_drain(self)
    }

    fn pending_playback_duration(&self) -> Duration {
        CpalAudioOutput::pending_playback_duration(self)
    }
}

// SAFETY: аналогично SystemAudioCapture — cpal::Stream не Send/Sync на macOS,
// но мы держим его только под локами и не двигаем между потоками.
unsafe impl Send for CpalAudioOutput {}
unsafe impl Sync for CpalAudioOutput {}

/// Заполняет f32 output buffer. Underrun → тишина (zero) и возврат в prebuffer.
fn fill_output(
    out: &mut [f32],
    queue: &Arc<Mutex<VecDeque<f32>>>,
    playback_state: &Arc<Mutex<OutputPlaybackState>>,
    prebuffer_samples: usize,
) {
    let mut pulled = 0usize;
    if let (Ok(mut q), Ok(mut state)) = (queue.lock(), playback_state.lock()) {
        if state.prebuffering && !state.draining {
            if q.len() < prebuffer_samples {
                out.fill(0.0);
                return;
            }
            state.prebuffering = false;
        }

        while pulled < out.len() {
            match q.pop_front() {
                Some(v) => {
                    out[pulled] = v;
                    pulled += 1;
                }
                None => break,
            }
        }

        if pulled < out.len() && !state.draining {
            state.underrun_count = state.underrun_count.saturating_add(1);
            state.prebuffering = true;
        }
    }
    for slot in out.iter_mut().skip(pulled) {
        *slot = 0.0;
    }
}

fn fill_output_i16(
    out: &mut [i16],
    queue: &Arc<Mutex<VecDeque<f32>>>,
    playback_state: &Arc<Mutex<OutputPlaybackState>>,
    prebuffer_samples: usize,
) {
    let mut tmp = vec![0f32; out.len()];
    fill_output(&mut tmp, queue, playback_state, prebuffer_samples);
    for (dst, src) in out.iter_mut().zip(tmp.iter()) {
        let clamped = src.clamp(-1.0, 1.0);
        *dst = (clamped * 32_767.0) as i16;
    }
}

fn fill_output_u16(
    out: &mut [u16],
    queue: &Arc<Mutex<VecDeque<f32>>>,
    playback_state: &Arc<Mutex<OutputPlaybackState>>,
    prebuffer_samples: usize,
) {
    let mut tmp = vec![0f32; out.len()];
    fill_output(&mut tmp, queue, playback_state, prebuffer_samples);
    for (dst, src) in out.iter_mut().zip(tmp.iter()) {
        let clamped = src.clamp(-1.0, 1.0);
        let scaled = (clamped * 32_767.0) + 32_768.0;
        *dst = scaled as u16;
    }
}

fn output_device_name_matches(name: &str, candidate: &str) -> bool {
    let lower_name = name.to_ascii_lowercase();
    let lower_candidate = candidate.to_ascii_lowercase();
    lower_name.contains(&lower_candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_translation_config_is_24khz_mono() {
        let cfg = AudioOutputConfig::openai_translation();
        assert_eq!(cfg.source_sample_rate, 24_000);
        assert_eq!(cfg.source_channels, 1);
        assert_eq!(cfg.prebuffer_ms, 200);
        assert_eq!(cfg.max_buffered_duration, Duration::from_millis(6_250));
        assert!(cfg.drain_max_buffered_duration > cfg.max_buffered_duration);
        assert_eq!(cfg.gain, 1.0);
    }

    #[test]
    fn incoming_spoken_profile_has_bounded_live_and_final_flush_headroom() {
        let cfg = AudioOutputConfig::incoming_spoken_translation();
        let outgoing = AudioOutputConfig::openai_translation();

        assert_eq!(cfg.max_buffered_duration, Duration::from_secs(10));
        assert_eq!(cfg.drain_max_buffered_duration, Duration::from_secs(25));
        assert_eq!(
            frames_for_duration(cfg.max_buffered_duration, 44_100),
            441_000
        );
        assert_eq!(
            frames_for_duration(cfg.max_buffered_duration, 96_000),
            960_000
        );
        assert!(cfg.drain_max_buffered_duration > cfg.max_buffered_duration);
        assert_eq!(
            frames_for_duration(outgoing.max_buffered_duration, 44_100),
            275_625
        );
        assert_eq!(
            frames_for_duration(outgoing.max_buffered_duration, 48_000),
            300_000
        );
        assert_eq!(
            frames_for_duration(outgoing.max_buffered_duration, 96_000),
            600_000
        );
        assert_eq!(
            frames_for_duration(Duration::from_secs(10) + Duration::from_nanos(1), 48_000),
            480_001
        );
        assert_eq!(frames_for_duration(Duration::ZERO, 48_000), 0);
        assert_eq!(frames_for_duration(Duration::MAX, u32::MAX), usize::MAX);
    }

    #[test]
    fn cpal_output_starts_closed() {
        let out = CpalAudioOutput::new();
        assert!(!out.is_open());
        assert!(out.device_name().is_none());
    }

    #[test]
    fn local_and_virtual_routes_share_one_output_implementation() {
        assert_eq!(
            CpalAudioOutput::system_default().selector,
            OutputDeviceSelector::SystemDefault
        );
        assert!(matches!(
            CpalAudioOutput::macos_blackhole().selector,
            OutputDeviceSelector::CandidateNames { .. }
        ));
        let id = AudioDeviceId::new("Named Output");
        assert_eq!(
            CpalAudioOutput::explicit_device(id.clone()).selector,
            OutputDeviceSelector::Explicit(id)
        );
    }

    #[test]
    fn output_device_name_matching_is_case_insensitive() {
        assert!(output_device_name_matches(
            "Speakers (CABLE INPUT VB-Audio Virtual Cable)",
            "Cable Input"
        ));
        assert!(output_device_name_matches("BlackHole 2ch", "blackhole"));
        assert!(!output_device_name_matches(
            "MacBook Speakers",
            "CABLE Input"
        ));
    }

    #[tokio::test]
    async fn enqueue_on_closed_returns_error() {
        let out = CpalAudioOutput::new();
        let err = out.enqueue_pcm16(&[1, 2, 3]).await;
        assert!(matches!(err, Err(AudioOutputError::Closed)));
    }

    #[tokio::test]
    async fn invalid_source_format_fails_before_opening_a_device() {
        let mut out = CpalAudioOutput::system_default();
        let invalid = AudioOutputConfig {
            source_channels: 2,
            ..AudioOutputConfig::openai_translation()
        };

        let error = out.open(invalid).await.unwrap_err();

        assert!(matches!(
            error,
            AudioOutputError::Configuration(message) if message.contains("2 ch")
        ));
        assert!(!out.is_open());
    }

    #[test]
    fn source_queue_discards_oldest_audio_without_exceeding_bound() {
        let queue = Arc::new(Mutex::new(VecDeque::from_iter(0i16..200)));
        let samples: Vec<i16> = (200..300).collect();

        let dropped =
            CpalAudioOutput::enqueue_bounded_source_samples(&queue, &samples, 256).unwrap();

        assert_eq!(dropped, 44);
        assert_eq!(queue.lock().unwrap().len(), 256);
        assert_eq!(queue.lock().unwrap().front().copied(), Some(44));
        assert_eq!(queue.lock().unwrap().back().copied(), Some(299));
    }

    #[test]
    fn oversized_single_delta_keeps_only_bounded_newest_audio() {
        let queue = Arc::new(Mutex::new(VecDeque::new()));
        let samples: Vec<i16> = (0..600).map(|sample| sample as i16).collect();

        let dropped =
            CpalAudioOutput::enqueue_bounded_source_samples(&queue, &samples, 256).unwrap();

        assert_eq!(dropped, 344);
        assert_eq!(queue.lock().unwrap().len(), 256);
        assert_eq!(queue.lock().unwrap().front().copied(), Some(344));
        assert_eq!(queue.lock().unwrap().back().copied(), Some(599));
    }

    #[test]
    fn asynchronous_stream_failure_becomes_output_error() {
        let out = CpalAudioOutput::new();
        *out.stream_error.lock().unwrap() = Some("device disappeared".to_string());

        let err = out
            .ensure_stream_healthy()
            .expect_err("stored CPAL failure must be observable by the service");

        assert!(matches!(
            err,
            AudioOutputError::Stream(message)
                if message.contains("device disappeared")
        ));
    }

    #[test]
    fn fill_output_waits_for_prebuffer_and_rebuffers_after_underrun() {
        let queue = Arc::new(Mutex::new(VecDeque::from(vec![0.1, 0.2, 0.3, 0.4])));
        let state = Arc::new(Mutex::new(OutputPlaybackState::default()));
        let mut out = [1.0f32; 2];

        fill_output(&mut out, &queue, &state, 6);

        assert_eq!(out, [0.0, 0.0]);
        assert_eq!(queue.lock().unwrap().len(), 4);
        assert!(state.lock().unwrap().prebuffering);

        queue.lock().unwrap().extend([0.5, 0.6]);
        let mut started = [0.0f32; 4];
        fill_output(&mut started, &queue, &state, 6);

        assert_eq!(started, [0.1, 0.2, 0.3, 0.4]);
        assert_eq!(queue.lock().unwrap().len(), 2);
        assert!(!state.lock().unwrap().prebuffering);

        let mut underrun = [0.0f32; 4];
        fill_output(&mut underrun, &queue, &state, 6);

        assert_eq!(underrun, [0.5, 0.6, 0.0, 0.0]);
        assert!(state.lock().unwrap().prebuffering);
    }

    #[test]
    fn begin_drain_mode_disables_prebuffer_for_late_tail() {
        let out = CpalAudioOutput::new();
        out.output_ready.lock().unwrap().extend([0.1, 0.2]);

        {
            let mut state = out.playback_state.lock().unwrap();
            state.prebuffering = true;
            state.draining = false;
        }

        out.begin_drain_mode();

        let state = out.playback_state.lock().unwrap();
        assert!(state.draining);
        assert!(!state.prebuffering);
    }

    #[test]
    fn fill_output_drain_mode_does_not_rebuffer_after_underrun() {
        let queue = Arc::new(Mutex::new(VecDeque::new()));
        let state = Arc::new(Mutex::new(OutputPlaybackState {
            prebuffering: false,
            draining: true,
            ..OutputPlaybackState::default()
        }));
        let mut underrun = [1.0f32; 4];

        fill_output(&mut underrun, &queue, &state, 64);

        assert_eq!(underrun, [0.0, 0.0, 0.0, 0.0]);
        assert!(!state.lock().unwrap().prebuffering);

        queue.lock().unwrap().push_back(0.7);
        let mut tail = [0.0f32; 4];
        fill_output(&mut tail, &queue, &state, 64);

        assert_eq!(tail, [0.7, 0.0, 0.0, 0.0]);
        assert!(!state.lock().unwrap().prebuffering);
    }

    #[test]
    fn drain_source_and_push_caps_ready_queue_to_latest_audio() {
        let source = Arc::new(Mutex::new(VecDeque::from(vec![
            1000, 2000, 3000, 4000, 5000,
        ])));
        let ready = Arc::new(Mutex::new(VecDeque::new()));

        let dropped_frames =
            CpalAudioOutput::drain_source_and_push(&source, &ready, &None, 2, 3, 1.0, true)
                .unwrap();

        assert!(source.lock().unwrap().is_empty());
        assert_eq!(dropped_frames, 2);
        let ready: Vec<f32> = ready.lock().unwrap().iter().copied().collect();
        assert_eq!(ready.len(), 6);
        assert_eq!(ready[0], 3000.0 / 32_768.0);
        assert_eq!(ready[1], 3000.0 / 32_768.0);
        assert_eq!(ready[4], 5000.0 / 32_768.0);
        assert_eq!(ready[5], 5000.0 / 32_768.0);
    }

    #[test]
    fn prepare_for_drain_reports_partial_chunk_overflow() {
        let mut output = CpalAudioOutput::new();
        output.is_open = true;
        output.native_config = Some(SupportedStreamConfig::new(
            1,
            cpal::SampleRate(100),
            cpal::SupportedBufferSize::Range { min: 1, max: 128 },
            SampleFormat::F32,
        ));
        output.config = Some(AudioOutputConfig {
            max_buffered_duration: Duration::from_millis(30),
            drain_max_buffered_duration: Duration::from_millis(30),
            ..AudioOutputConfig::openai_translation()
        });
        output.output_ready.lock().unwrap().extend([0.1, 0.2, 0.3]);
        output.source_queue.lock().unwrap().push_back(1_000);

        let error = output
            .prepare_for_drain()
            .expect_err("partial drain overflow must not stay silent");

        assert!(matches!(
            error,
            AudioOutputError::Stream(message) if message.contains("overflowed during drain")
        ));
        let state = output.playback_state.lock().unwrap();
        assert_eq!(state.overflow_count, 1);
        assert_eq!(state.dropped_audio_ms, 10);
    }

    #[test]
    fn output_gain_scales_and_clamps_pcm_without_clipping() {
        let source = Arc::new(Mutex::new(VecDeque::from(vec![i16::MAX, i16::MIN])));
        let ready = Arc::new(Mutex::new(VecDeque::new()));

        CpalAudioOutput::drain_source_and_push(&source, &ready, &None, 1, 4, 0.5, true).unwrap();

        let ready: Vec<f32> = ready.lock().unwrap().iter().copied().collect();
        assert_eq!(ready.len(), 2);
        assert!((ready[0] - 0.5).abs() < 0.001);
        assert!((ready[1] + 0.5).abs() < 0.001);
        assert!(ready.iter().all(|sample| (-1.0..=1.0).contains(sample)));
    }

    #[test]
    fn boosted_output_uses_symmetric_soft_limiting_without_changing_unity_gain() {
        let unity = apply_output_gain(0.75, 1.0);
        let boosted = apply_output_gain(0.75, 2.0);
        let boosted_negative = apply_output_gain(-0.75, 2.0);

        assert_eq!(unity, 0.75);
        assert!(boosted > unity);
        assert!(boosted < 1.0);
        assert!((boosted + boosted_negative).abs() < f32::EPSILON);
        assert_eq!(apply_output_gain(0.0, 2.0), 0.0);
    }

    #[test]
    fn zero_gain_queues_silence_without_closing_pipeline() {
        let source = Arc::new(Mutex::new(VecDeque::from(vec![12_000, -12_000])));
        let ready = Arc::new(Mutex::new(VecDeque::new()));

        CpalAudioOutput::drain_source_and_push(&source, &ready, &None, 1, 4, 0.0, true).unwrap();

        assert_eq!(
            ready.lock().unwrap().iter().copied().collect::<Vec<_>>(),
            vec![0.0, 0.0]
        );
    }

    #[test]
    fn runtime_mute_clears_already_buffered_playback() {
        let mut output = CpalAudioOutput::system_default();
        output.config = Some(AudioOutputConfig::openai_translation());
        output.source_queue.lock().unwrap().extend([1, 2, 3]);
        output.output_ready.lock().unwrap().extend([0.1, 0.2]);

        output.set_gain(0.0).unwrap();

        assert_eq!(output.config.unwrap().gain, 0.0);
        assert!(output.source_queue.lock().unwrap().is_empty());
        assert!(output.output_ready.lock().unwrap().is_empty());
    }

    #[test]
    fn output_pipeline_propagates_poisoned_resampler_state() {
        let source = Arc::new(Mutex::new(VecDeque::from(vec![1i16; RESAMPLER_CHUNK_SIZE])));
        let ready = Arc::new(Mutex::new(VecDeque::new()));
        let resampler = Arc::new(Mutex::new(
            CpalAudioOutput::build_resampler(24_000, 48_000).unwrap(),
        ));
        let poisoned = resampler.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoned.lock().unwrap();
            panic!("poison test resampler");
        })
        .join();

        let err = CpalAudioOutput::drain_source_and_push(
            &source,
            &ready,
            &Some(resampler),
            2,
            48_000,
            1.0,
            false,
        )
        .unwrap_err();

        assert!(matches!(
            err,
            AudioOutputError::Resample(message) if message.contains("poisoned")
        ));
    }
}
