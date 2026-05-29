use async_trait::async_trait;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Host, SampleFormat, Stream, StreamConfig, SupportedStreamConfig};
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Размер чанка для resampler внутри output pipeline (в source-сэмплах, mono).
const RESAMPLER_CHUNK_SIZE: usize = 256;

/// Имя env-переменной для оверрайда output устройства (для devs которым нужно другое имя).
pub const ENV_TRANSLATION_OUTPUT_DEVICE: &str = "VOICETEXT_TRANSLATION_OUTPUT_DEVICE";

/// Кандидаты названий BlackHole, по которым ищем устройство (в порядке убывания специфичности).
const BLACKHOLE_DEVICE_NAMES: &[&str] = &["BlackHole 2ch", "BlackHole"];

#[derive(Debug, thiserror::Error)]
pub enum AudioOutputError {
    #[error("Configuration: {0}")]
    Configuration(String),

    #[error("Device: {0}")]
    Device(String),

    #[error("Stream: {0}")]
    Stream(String),

    #[error("Resample: {0}")]
    Resample(String),

    #[error("Output is closed")]
    Closed,
}

pub type AudioOutputResult<T> = Result<T, AudioOutputError>;

#[derive(Debug, Clone, Copy)]
pub struct AudioOutputConfig {
    /// Источник: sample rate входного PCM16 (для OpenAI translate = 24000).
    pub source_sample_rate: u32,
    /// Источник: каналы входного PCM16 (для OpenAI translate = 1, mono).
    pub source_channels: u16,
    /// Jitter/prebuffer перед стартом playback. OpenAI audio deltas приходят пачками,
    /// поэтому небольшой буфер сглаживает сетевые/серверные интервалы между чанками.
    pub prebuffer_ms: u64,
    /// Максимум буфера в output_ready (в frames per channel). Превышение → дропаем старое.
    /// ~2 секунды по native sample rate — безопасный потолок без раздувания latency.
    pub max_buffered_frames: usize,
}

impl AudioOutputConfig {
    /// Конфиг для OpenAI realtime translation output (24 kHz mono).
    pub fn openai_translation() -> Self {
        Self {
            source_sample_rate: 24_000,
            source_channels: 1,
            prebuffer_ms: 400,
            // OpenAI может отдавать synthesized audio пачками быстрее realtime.
            // Держим запас ~6 сек при native 48 kHz, чтобы не резать речь на burst'ах.
            max_buffered_frames: 300_000,
        }
    }
}

#[derive(Debug)]
struct OutputPlaybackState {
    prebuffering: bool,
}

impl Default for OutputPlaybackState {
    fn default() -> Self {
        Self { prebuffering: true }
    }
}

#[async_trait]
pub trait AudioOutput: Send + Sync {
    async fn open(&mut self, config: AudioOutputConfig) -> AudioOutputResult<()>;
    /// Отправляет mono PCM16 chunk. Внутри: resample → channel-expand → push в output queue.
    async fn enqueue_pcm16(&self, samples: &[i16]) -> AudioOutputResult<()>;
    async fn close(&mut self) -> AudioOutputResult<()>;
    fn is_open(&self) -> bool;
    /// Полное имя устройства, на которое сейчас открыт output (для диагностики).
    fn device_name(&self) -> Option<String>;
}

/// CPAL-output, направленный на BlackHole 2ch (или иное устройство из env override).
/// Не использует async внутри audio callback — только sync Mutex с короткими lock-окнами.
pub struct CpalAudioOutput {
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
    /// Resampler 24 kHz mono → native sample rate mono (если нужно).
    resampler: Option<Arc<Mutex<SincFixedIn<f32>>>>,
    /// Признак что output открыт (атомарно бы лучше, но используем под общим контролем).
    is_open: bool,
}

impl CpalAudioOutput {
    pub fn new() -> Self {
        Self {
            device: None,
            device_name: None,
            stream: None,
            native_config: None,
            config: None,
            source_queue: Arc::new(Mutex::new(VecDeque::with_capacity(48_000))),
            output_ready: Arc::new(Mutex::new(VecDeque::with_capacity(192_000))),
            playback_state: Arc::new(Mutex::new(OutputPlaybackState::default())),
            resampler: None,
            is_open: false,
        }
    }

    fn select_output_device(host: &Host) -> AudioOutputResult<(Device, String)> {
        if let Ok(override_name) = std::env::var(ENV_TRANSLATION_OUTPUT_DEVICE) {
            let trimmed = override_name.trim();
            if !trimmed.is_empty() {
                let device = host
                    .output_devices()
                    .map_err(|e| AudioOutputError::Device(e.to_string()))?
                    .find(|d| d.name().map(|n| n.contains(trimmed)).unwrap_or(false));
                return match device {
                    Some(d) => {
                        let name = d.name().unwrap_or_else(|_| trimmed.to_string());
                        log::info!(
                            "Output device picked via {}: {}",
                            ENV_TRANSLATION_OUTPUT_DEVICE,
                            name
                        );
                        Ok((d, name))
                    }
                    None => Err(AudioOutputError::Configuration(format!(
                        "Output устройство '{}' (из {}) не найдено в системе.",
                        trimmed, ENV_TRANSLATION_OUTPUT_DEVICE
                    ))),
                };
            }
        }

        let outputs: Vec<Device> = host
            .output_devices()
            .map_err(|e| AudioOutputError::Device(e.to_string()))?
            .collect();

        for candidate in BLACKHOLE_DEVICE_NAMES {
            for dev in &outputs {
                if let Ok(name) = dev.name() {
                    if name.contains(candidate) {
                        log::info!("Auto-picked output device: {}", name);
                        return Ok((dev.clone(), name));
                    }
                }
            }
        }

        Err(AudioOutputError::Configuration(
            "BlackHole 2ch не найден. Установите blackhole-2ch (brew install --cask blackhole-2ch), \
             перезагрузите macOS и выберите BlackHole 2ch как микрофон в Meet/Zoom. \
             Также можно задать VOICETEXT_TRANSLATION_OUTPUT_DEVICE с подстрокой имени нужного устройства."
                .to_string(),
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

    /// Берёт source_queue, прогоняет через resampler фиксированными чанками и
    /// раскладывает mono-результат на native_ch (дублируем каждый сэмпл).
    fn drain_source_and_push(
        source_queue: &Arc<Mutex<VecDeque<i16>>>,
        output_ready: &Arc<Mutex<VecDeque<f32>>>,
        resampler: &Option<Arc<Mutex<SincFixedIn<f32>>>>,
        native_channels: usize,
        max_buffered_frames: usize,
        flush_partial: bool,
    ) {
        // Берём всё что есть, но обрабатываем фиксированными чанками RESAMPLER_CHUNK_SIZE.
        let mut local_chunks: Vec<Vec<i16>> = Vec::new();
        {
            let Ok(mut q) = source_queue.lock() else {
                return;
            };
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
            return;
        }

        let mut expanded_samples = Vec::new();
        for chunk in local_chunks {
            let mono_f32: Vec<f32> = chunk.iter().map(|&s| s as f32 / 32_768.0).collect();
            let native_mono: Vec<f32> = if let Some(rs) = resampler {
                let Ok(mut r) = rs.lock() else {
                    continue;
                };
                match r.process(&[mono_f32], None) {
                    Ok(mut output_per_channel) => output_per_channel
                        .pop()
                        .or_else(|| Some(Vec::new()))
                        .unwrap(),
                    Err(e) => {
                        log::error!("Output resample error: {}", e);
                        continue;
                    }
                }
            } else {
                mono_f32
            };

            // mono → native_ch: дублируем сэмпл во все каналы.
            for sample in native_mono {
                for _ in 0..native_channels.max(1) {
                    expanded_samples.push(sample);
                }
            }
        }

        if expanded_samples.is_empty() {
            return;
        }

        let Ok(mut out) = output_ready.lock() else {
            return;
        };
        out.extend(expanded_samples);

        // Bounded queue: если выросло выше потолка (frames * channels) — дропаем старые сэмплы.
        let cap_samples = max_buffered_frames.saturating_mul(native_channels.max(1));
        if out.len() > cap_samples {
            let drop = out.len() - cap_samples;
            for _ in 0..drop {
                out.pop_front();
            }
            log::warn!(
                "CpalAudioOutput overflow: dropped {} samples (queue cap {} frames * {} ch)",
                drop,
                max_buffered_frames,
                native_channels
            );
        }
    }

    fn build_stream(
        device: &Device,
        native: &SupportedStreamConfig,
        output_ready: Arc<Mutex<VecDeque<f32>>>,
        playback_state: Arc<Mutex<OutputPlaybackState>>,
        prebuffer_samples: usize,
    ) -> AudioOutputResult<Stream> {
        let stream_config: StreamConfig = native.clone().into();
        let sample_format = native.sample_format();
        let err_fn = |err| log::error!("CpalAudioOutput stream error: {}", err);

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

    /// Flushes the final partial source chunk and lets the output callback play
    /// whatever remains without waiting for another full prebuffer.
    pub fn prepare_for_drain(&self) -> AudioOutputResult<Duration> {
        if !self.is_open {
            return Ok(Duration::ZERO);
        }
        let Some(native) = self.native_config.as_ref() else {
            return Err(AudioOutputError::Closed);
        };
        let Some(cfg) = self.config.as_ref() else {
            return Err(AudioOutputError::Closed);
        };

        Self::drain_source_and_push(
            &self.source_queue,
            &self.output_ready,
            &self.resampler,
            native.channels() as usize,
            cfg.max_buffered_frames,
            true,
        );

        let pending = self.pending_playback_duration();
        if pending > Duration::ZERO {
            if let Ok(mut state) = self.playback_state.lock() {
                state.prebuffering = false;
            }
        }
        Ok(pending)
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
impl AudioOutput for CpalAudioOutput {
    async fn open(&mut self, config: AudioOutputConfig) -> AudioOutputResult<()> {
        if self.is_open {
            return Err(AudioOutputError::Configuration(
                "CpalAudioOutput уже открыт".to_string(),
            ));
        }
        let host = cpal::default_host();
        let (device, name) = Self::select_output_device(&host)?;
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
        }

        let prebuffer_frames =
            ((native_sr as u64).saturating_mul(config.prebuffer_ms) / 1000).max(1) as usize;
        let prebuffer_samples = prebuffer_frames.saturating_mul(native_ch.max(1));

        let stream = Self::build_stream(
            &device,
            &native,
            self.output_ready.clone(),
            self.playback_state.clone(),
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

    async fn enqueue_pcm16(&self, samples: &[i16]) -> AudioOutputResult<()> {
        if !self.is_open {
            return Err(AudioOutputError::Closed);
        }
        if samples.is_empty() {
            return Ok(());
        }
        let Some(native) = self.native_config.as_ref() else {
            return Err(AudioOutputError::Closed);
        };
        let Some(cfg) = self.config.as_ref() else {
            return Err(AudioOutputError::Closed);
        };
        let native_channels = native.channels() as usize;

        {
            let mut q = self
                .source_queue
                .lock()
                .map_err(|_| AudioOutputError::Stream("source_queue poisoned".into()))?;
            q.extend(samples.iter().copied());
        }

        Self::drain_source_and_push(
            &self.source_queue,
            &self.output_ready,
            &self.resampler,
            native_channels,
            cfg.max_buffered_frames,
            false,
        );

        Ok(())
    }

    async fn close(&mut self) -> AudioOutputResult<()> {
        self.is_open = false;
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
            state.prebuffering = true;
        }
        log::info!("CpalAudioOutput closed");
        Ok(())
    }

    fn is_open(&self) -> bool {
        self.is_open
    }

    fn device_name(&self) -> Option<String> {
        self.device_name.clone()
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
        if state.prebuffering {
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

        if pulled < out.len() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_translation_config_is_24khz_mono() {
        let cfg = AudioOutputConfig::openai_translation();
        assert_eq!(cfg.source_sample_rate, 24_000);
        assert_eq!(cfg.source_channels, 1);
        assert!((250..=500).contains(&cfg.prebuffer_ms));
        assert!(cfg.max_buffered_frames >= 288_000); // headroom хотя бы 6 сек на 48 kHz
    }

    #[test]
    fn cpal_output_starts_closed() {
        let out = CpalAudioOutput::new();
        assert!(!out.is_open());
        assert!(out.device_name().is_none());
    }

    #[tokio::test]
    async fn enqueue_on_closed_returns_error() {
        let out = CpalAudioOutput::new();
        let err = out.enqueue_pcm16(&[1, 2, 3]).await;
        assert!(matches!(err, Err(AudioOutputError::Closed)));
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
}
