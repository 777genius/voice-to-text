mod paid_e2e_support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use app_lib::application::{
    IncomingTranslationCallbacks, IncomingTranslationConfig, IncomingTranslationFacade,
    LiveTranslationCallbacks, LiveTranslationConfig, LiveTranslationService,
};
use app_lib::domain::{
    AudioCapture, AudioCaptureTarget, AudioChunk, AudioChunkCallback, AudioConfig, AudioError,
    AudioResult, PlatformAudioFactory, PlatformAudioSetupState, PlatformAudioSetupStatus,
    RealtimeInputNoiseReduction, RealtimeTranslationConfig, RealtimeTranslationEvent,
    RecordingStatus, SttConfig, TranslationAudioOutput, TranslationAudioOutputResult,
};
use app_lib::infrastructure::audio::{
    AudioOutputConfig, CpalAudioOutput, DefaultLocalPlaybackOutputFactory,
    DefaultPlatformAudioFactory, DefaultSpokenTranslationCapability,
};
use app_lib::infrastructure::openai::{
    OpenAIRealtimeTranslationClient, OpenAIRealtimeTranslationFactory,
};
use async_trait::async_trait;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tokio::sync::{mpsc, Notify};

use paid_e2e_support::{
    audible_pcm16_segments, audible_pcm16_window, load_paid_e2e_api_key, transcribe_audible_pcm16,
    transcribe_pcm16_with_whisper, transcribe_segmented_pcm16, transcription_segments, wav_pcm16,
    ObservedLocalPlaybackFactory, PlaybackProbe,
};

static NEXT_TEMP_AUDIO_ID: AtomicUsize = AtomicUsize::new(0);

struct TempGeneratedFile {
    path: PathBuf,
}

impl TempGeneratedFile {
    fn new(prefix: &str, extension: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let sequence = NEXT_TEMP_AUDIO_ID.fetch_add(1, Ordering::Relaxed);
        Self {
            path: std::env::temp_dir().join(format!(
                "{prefix}_{}_{}_{}.{}",
                std::process::id(),
                nanos,
                sequence,
                extension
            )),
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn path_str(&self) -> &str {
        self.path.to_str().expect("valid temp audio path")
    }
}

impl Drop for TempGeneratedFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn generate_russian_pcm24_phrase(prefix: &str, text: &str) -> Vec<i16> {
    let aiff_path = TempGeneratedFile::new(prefix, "aiff");
    let raw_path = TempGeneratedFile::new(prefix, "s16le");

    let say_status = Command::new("say")
        .args(["-v", "Milena", "-o", aiff_path.path_str(), text])
        .status()
        .expect("must run macOS say");
    assert!(say_status.success(), "macOS say failed");

    let ffmpeg_status = Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            aiff_path.path_str(),
            "-ac",
            "1",
            "-ar",
            "24000",
            "-f",
            "s16le",
            raw_path.path_str(),
        ])
        .status()
        .expect("must run ffmpeg");
    assert!(ffmpeg_status.success(), "ffmpeg conversion failed");

    let bytes = fs::read(raw_path.path()).expect("must read generated pcm");
    let mut samples: Vec<i16> = bytes
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();
    samples.extend(std::iter::repeat(0).take(36_000));
    samples
}

fn generate_russian_pcm24() -> Vec<i16> {
    generate_russian_pcm24_phrase(
        "voicetext_openai_ru_source",
        "Привет, меня зовут Алексей. Я проверяю перевод голоса на английский язык.",
    )
}

fn generate_english_system_fixture(prefix: &str, text: &str) -> TempGeneratedFile {
    let fixture = TempGeneratedFile::new(prefix, "aiff");
    let say_status = Command::new("say")
        .args(["-v", "Samantha", "-o", fixture.path_str(), text])
        .status()
        .expect("must generate concurrent incoming speech");
    assert!(say_status.success(), "macOS say failed for duplex fixture");
    fixture
}

async fn play_system_fixture(path: &Path, gain: f32) {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let gain = gain.to_string();
        let status = Command::new("afplay")
            .args(["--volume", &gain])
            .arg(path)
            .status()
            .expect("must play concurrent incoming fixture");
        assert!(status.success(), "afplay failed for duplex fixture");
    })
    .await
    .expect("duplex fixture playback worker must not panic");
}

fn find_blackhole_input() -> cpal::Device {
    let host = cpal::default_host();
    host.input_devices()
        .expect("must list input devices")
        .find(|device| {
            device
                .name()
                .map(|name| name.contains("BlackHole 2ch") || name.contains("BlackHole"))
                .unwrap_or(false)
        })
        .expect("BlackHole input device must exist")
}

fn start_blackhole_capture(captured: Arc<Mutex<Vec<f32>>>) -> (cpal::Stream, u32, u16) {
    let input = find_blackhole_input();
    let input_config = input
        .default_input_config()
        .expect("BlackHole input must have default config");
    let stream_config: cpal::StreamConfig = input_config.clone().into();
    let sample_rate = stream_config.sample_rate.0;
    let channels = stream_config.channels;
    let err_fn = |err| eprintln!("BlackHole input stream error: {err}");

    let stream = match input_config.sample_format() {
        cpal::SampleFormat::F32 => input
            .build_input_stream(
                &stream_config,
                move |data: &[f32], _| {
                    captured.lock().unwrap().extend_from_slice(data);
                },
                err_fn,
                None,
            )
            .expect("must build f32 input stream"),
        cpal::SampleFormat::I16 => input
            .build_input_stream(
                &stream_config,
                move |data: &[i16], _| {
                    let mut guard = captured.lock().unwrap();
                    guard.extend(data.iter().map(|sample| *sample as f32 / i16::MAX as f32));
                },
                err_fn,
                None,
            )
            .expect("must build i16 input stream"),
        cpal::SampleFormat::U16 => input
            .build_input_stream(
                &stream_config,
                move |data: &[u16], _| {
                    let mut guard = captured.lock().unwrap();
                    guard.extend(
                        data.iter()
                            .map(|sample| (*sample as f32 / u16::MAX as f32) * 2.0 - 1.0),
                    );
                },
                err_fn,
                None,
            )
            .expect("must build u16 input stream"),
        other => panic!("unsupported input sample format: {other:?}"),
    };
    (stream, sample_rate, channels)
}

#[derive(Default)]
struct AudioStats {
    samples: usize,
    sum_sq: f64,
    peak: f32,
}

impl AudioStats {
    fn push_f32(&mut self, sample: f32) {
        self.samples += 1;
        self.sum_sq += (sample as f64) * (sample as f64);
        self.peak = self.peak.max(sample.abs());
    }

    fn rms(&self) -> f32 {
        if self.samples == 0 {
            return 0.0;
        }
        (self.sum_sq / self.samples as f64).sqrt() as f32
    }
}

fn start_blackhole_stats_capture(stats: Arc<Mutex<AudioStats>>) -> cpal::Stream {
    let input = find_blackhole_input();
    let input_config = input
        .default_input_config()
        .expect("BlackHole input must have default config");
    let stream_config: cpal::StreamConfig = input_config.clone().into();
    let err_fn = |err| eprintln!("BlackHole input stream error: {err}");

    match input_config.sample_format() {
        cpal::SampleFormat::F32 => input
            .build_input_stream(
                &stream_config,
                move |data: &[f32], _| {
                    let mut guard = stats.lock().unwrap();
                    for sample in data {
                        guard.push_f32(*sample);
                    }
                },
                err_fn,
                None,
            )
            .expect("must build f32 stats input stream"),
        cpal::SampleFormat::I16 => input
            .build_input_stream(
                &stream_config,
                move |data: &[i16], _| {
                    let mut guard = stats.lock().unwrap();
                    for sample in data {
                        guard.push_f32(*sample as f32 / i16::MAX as f32);
                    }
                },
                err_fn,
                None,
            )
            .expect("must build i16 stats input stream"),
        cpal::SampleFormat::U16 => input
            .build_input_stream(
                &stream_config,
                move |data: &[u16], _| {
                    let mut guard = stats.lock().unwrap();
                    for sample in data {
                        guard.push_f32((*sample as f32 / u16::MAX as f32) * 2.0 - 1.0);
                    }
                },
                err_fn,
                None,
            )
            .expect("must build u16 stats input stream"),
        other => panic!("unsupported input sample format: {other:?}"),
    }
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|v| v * v).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

struct OutgoingAudioTranscription {
    transcript: String,
    primary_transcript: String,
    primary_error: Option<String>,
    segmented_fallback_attempted: bool,
    segmented_fallback_transcript: String,
    segmented_fallback_error: Option<String>,
    whisper_fallback_attempted: bool,
    whisper_fallback_transcript: String,
    whisper_fallback_error: Option<String>,
}

fn outgoing_audio_has_expected_meaning(transcript: &str, require_duplex_tail: bool) -> bool {
    let normalized = transcript.to_lowercase();
    normalized.contains("alex")
        && normalized.contains("english")
        && (normalized.contains("voice") || normalized.contains("translation"))
        && (!require_duplex_tail
            || (normalized.contains("phoenix")
                && (normalized.contains('7') || normalized.contains("seven"))))
}

async fn transcribe_outgoing_audio(
    client: &reqwest::Client,
    api_key: &str,
    sample_rate: u32,
    channels: u16,
    samples: &[i16],
    require_duplex_tail: bool,
) -> Result<OutgoingAudioTranscription, String> {
    let primary = transcribe_audible_pcm16(client, api_key, sample_rate, channels, samples).await;
    let primary_error = primary.as_ref().err().cloned();
    let primary_transcript = primary.unwrap_or_default();
    let segmented_fallback_attempted = primary_error.is_some()
        || !outgoing_audio_has_expected_meaning(&primary_transcript, require_duplex_tail);
    let segmented_fallback = if segmented_fallback_attempted {
        Some(transcribe_segmented_pcm16(client, api_key, sample_rate, channels, samples).await)
    } else {
        None
    };
    let segmented_fallback_error = segmented_fallback
        .as_ref()
        .and_then(|result| result.as_ref().err())
        .cloned();
    let segmented_fallback_transcript = segmented_fallback.and_then(Result::ok).unwrap_or_default();
    let gpt_transcript = match (
        primary_transcript.is_empty(),
        segmented_fallback_transcript.is_empty(),
    ) {
        (false, false) => format!("{primary_transcript}\n{segmented_fallback_transcript}"),
        (false, true) => primary_transcript.clone(),
        (true, false) => segmented_fallback_transcript.clone(),
        (true, true) => String::new(),
    };
    let whisper_fallback_attempted =
        !outgoing_audio_has_expected_meaning(&gpt_transcript, require_duplex_tail);
    let whisper_fallback = if whisper_fallback_attempted {
        Some(transcribe_pcm16_with_whisper(client, api_key, sample_rate, channels, samples).await)
    } else {
        None
    };
    let whisper_fallback_error = whisper_fallback
        .as_ref()
        .and_then(|result| result.as_ref().err())
        .cloned();
    let whisper_fallback_transcript = whisper_fallback.and_then(Result::ok).unwrap_or_default();
    let transcript = match (
        gpt_transcript.is_empty(),
        whisper_fallback_transcript.is_empty(),
    ) {
        (false, false) => format!("{gpt_transcript}\n{whisper_fallback_transcript}"),
        (false, true) => gpt_transcript,
        (true, false) => whisper_fallback_transcript.clone(),
        (true, true) => {
            return Err(format!(
                "primary transcription failed: {}; segmented fallback failed: {}; whisper fallback failed: {}",
                primary_error.as_deref().unwrap_or("empty transcript"),
                segmented_fallback_error
                    .as_deref()
                    .unwrap_or("empty transcript"),
                whisper_fallback_error
                    .as_deref()
                    .unwrap_or("empty transcript")
            ));
        }
    };
    Ok(OutgoingAudioTranscription {
        transcript,
        primary_transcript,
        primary_error,
        segmented_fallback_attempted,
        segmented_fallback_transcript,
        segmented_fallback_error,
        whisper_fallback_attempted,
        whisper_fallback_transcript,
        whisper_fallback_error,
    })
}

fn outgoing_artifact_root() -> PathBuf {
    if let Some(path) = std::env::var_os("OUTGOING_TRANSLATION_E2E_ARTIFACTS") {
        return PathBuf::from(path);
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/e2e-artifacts")
        .join(format!("outgoing-live-{timestamp}"))
}

#[test]
fn audible_window_trims_outer_silence_with_channel_aligned_padding() {
    let mut samples = vec![0i16; 20];
    samples.extend([400, -400]);
    samples.extend(vec![0i16; 20]);

    let window = audible_pcm16_window(&samples, 8, 2);

    assert_eq!(window.len() % 2, 0);
    assert_eq!(window, &samples[12..30]);
    assert_eq!(audible_pcm16_window(&[0; 8], 8, 1), &[0; 8]);
}

#[test]
fn audible_segments_split_long_internal_silence_and_ignore_short_clicks() {
    let mut samples = vec![0i16; 10];
    samples.extend(vec![500i16; 10]);
    samples.extend(vec![0i16; 10]);
    samples.extend(vec![-500i16; 10]);
    samples.extend(vec![0i16; 10]);
    samples.push(700);
    samples.extend(vec![0i16; 10]);

    let segments = audible_pcm16_segments(&samples, 10, 1);

    assert_eq!(segments.len(), 2);
    assert!(segments[0].iter().any(|sample| *sample == 500));
    assert!(segments[1].iter().any(|sample| *sample == -500));
}

#[test]
fn short_transcription_keeps_full_context_while_long_capture_is_segmented() {
    let mut short = vec![500i16; 10];
    short.extend(vec![0i16; 20]);
    short.extend(vec![-500i16; 10]);

    assert_eq!(transcription_segments(&short, 10, 1).len(), 1);

    let mut long = vec![500i16; 100];
    long.extend(vec![0i16; 100]);
    long.extend(vec![-500i16; 110]);

    let segments = transcription_segments(&long, 10, 1);
    assert_eq!(segments.len(), 2);
    assert!(segments[0].iter().any(|sample| *sample == 500));
    assert!(segments[1].iter().any(|sample| *sample == -500));
}

fn live_audio_soak_duration() -> Duration {
    std::env::var("LIVE_AUDIO_SOAK_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(30 * 60))
}

struct SyntheticPcmMicrophone {
    pcm: Arc<Vec<i16>>,
    config: AudioConfig,
    started: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    callback: Option<AudioChunkCallback>,
    task: Option<tokio::task::JoinHandle<()>>,
}

#[async_trait]
impl AudioCapture for SyntheticPcmMicrophone {
    async fn initialize(&mut self, config: AudioConfig) -> AudioResult<()> {
        self.config = config;
        Ok(())
    }

    async fn start_capture(&mut self, on_chunk: AudioChunkCallback) -> AudioResult<()> {
        self.started.store(true, Ordering::SeqCst);
        self.stopped.store(false, Ordering::SeqCst);
        self.finished.store(false, Ordering::SeqCst);
        let chunks: Vec<Vec<i16>> = self.pcm.chunks(4_800).map(|chunk| chunk.to_vec()).collect();
        let config = self.config;
        let stopped = self.stopped.clone();
        let finished = self.finished.clone();
        self.callback = Some(on_chunk.clone());
        self.task = Some(tokio::spawn(async move {
            for chunk in chunks {
                if stopped.load(Ordering::SeqCst) {
                    break;
                }
                on_chunk(AudioChunk::new(chunk, config.sample_rate, config.channels));
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            finished.store(true, Ordering::SeqCst);
        }));
        Ok(())
    }

    async fn stop_capture(&mut self) -> AudioResult<()> {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(task) = self.task.take() {
            let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
        }
        self.callback = None;
        Ok(())
    }

    fn is_capturing(&self) -> bool {
        self.started.load(Ordering::SeqCst) && !self.stopped.load(Ordering::SeqCst)
    }

    fn config(&self) -> AudioConfig {
        self.config
    }
}

struct SyntheticMicToBlackholeFactory {
    pcm: Arc<Vec<i16>>,
    requested_target: Arc<Mutex<Option<AudioCaptureTarget>>>,
    mic_started: Arc<AtomicBool>,
    mic_stopped: Arc<AtomicBool>,
    mic_finished: Arc<AtomicBool>,
}

struct ControlledPcmMicrophone {
    receiver: Option<mpsc::UnboundedReceiver<Vec<i16>>>,
    config: AudioConfig,
    started: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    completed_phrases: Arc<AtomicUsize>,
    stop_signal: Arc<Notify>,
    task: Option<tokio::task::JoinHandle<()>>,
}

#[async_trait]
impl AudioCapture for ControlledPcmMicrophone {
    async fn initialize(&mut self, config: AudioConfig) -> AudioResult<()> {
        self.config = config;
        Ok(())
    }

    async fn start_capture(&mut self, on_chunk: AudioChunkCallback) -> AudioResult<()> {
        let mut receiver = self.receiver.take().ok_or_else(|| {
            AudioError::Capture("controlled microphone cannot be started twice".into())
        })?;
        self.started.store(true, Ordering::SeqCst);
        self.stopped.store(false, Ordering::SeqCst);

        let config = self.config;
        let stopped = self.stopped.clone();
        let completed_phrases = self.completed_phrases.clone();
        let stop_signal = self.stop_signal.clone();
        self.task = Some(tokio::spawn(async move {
            'capture: loop {
                let pcm = tokio::select! {
                    _ = stop_signal.notified() => break,
                    pcm = receiver.recv() => match pcm {
                        Some(pcm) => pcm,
                        None => break,
                    },
                };
                for chunk in pcm.chunks(4_800) {
                    if stopped.load(Ordering::SeqCst) {
                        break 'capture;
                    }
                    on_chunk(AudioChunk::new(
                        chunk.to_vec(),
                        config.sample_rate,
                        config.channels,
                    ));
                    tokio::select! {
                        _ = stop_signal.notified() => break 'capture,
                        _ = tokio::time::sleep(Duration::from_millis(200)) => {}
                    }
                }
                completed_phrases.fetch_add(1, Ordering::SeqCst);
            }
        }));
        Ok(())
    }

    async fn stop_capture(&mut self) -> AudioResult<()> {
        self.stopped.store(true, Ordering::SeqCst);
        self.stop_signal.notify_one();
        if let Some(task) = self.task.take() {
            tokio::time::timeout(Duration::from_secs(1), task)
                .await
                .map_err(|_| AudioError::Capture("controlled microphone stop timed out".into()))?
                .map_err(|error| {
                    AudioError::Capture(format!("controlled microphone task failed: {error}"))
                })?;
        }
        Ok(())
    }

    fn is_capturing(&self) -> bool {
        self.started.load(Ordering::SeqCst) && !self.stopped.load(Ordering::SeqCst)
    }

    fn config(&self) -> AudioConfig {
        self.config
    }
}

struct ControlledMicToBlackholeFactory {
    receiver: Mutex<Option<mpsc::UnboundedReceiver<Vec<i16>>>>,
    requested_target: Arc<Mutex<Option<AudioCaptureTarget>>>,
    mic_started: Arc<AtomicBool>,
    mic_stopped: Arc<AtomicBool>,
    completed_phrases: Arc<AtomicUsize>,
}

#[async_trait]
impl PlatformAudioFactory for ControlledMicToBlackholeFactory {
    fn create_microphone_capture(
        &self,
        _device_name: Option<String>,
        target: AudioCaptureTarget,
    ) -> AudioResult<Box<dyn AudioCapture>> {
        *self.requested_target.lock().unwrap() = Some(target);
        let receiver =
            self.receiver.lock().unwrap().take().ok_or_else(|| {
                AudioError::Capture("controlled microphone already created".into())
            })?;
        Ok(Box::new(ControlledPcmMicrophone {
            receiver: Some(receiver),
            config: AudioConfig::default(),
            started: self.mic_started.clone(),
            stopped: self.mic_stopped.clone(),
            completed_phrases: self.completed_phrases.clone(),
            stop_signal: Arc::new(Notify::new()),
            task: None,
        }))
    }

    fn create_translation_output(
        &self,
    ) -> TranslationAudioOutputResult<Box<dyn TranslationAudioOutput>> {
        Ok(Box::new(CpalAudioOutput::new()))
    }

    fn create_system_loopback_capture(
        &self,
        _target: AudioCaptureTarget,
    ) -> AudioResult<Box<dyn AudioCapture>> {
        Err(AudioError::Configuration(
            "not used by controlled duplex e2e".to_string(),
        ))
    }

    async fn setup_status(&self) -> PlatformAudioSetupStatus {
        PlatformAudioSetupStatus {
            platform: std::env::consts::OS.to_string(),
            status: PlatformAudioSetupState::Ready,
            outgoing_supported: true,
            incoming_supported: true,
            virtual_microphone_name: "BlackHole 2ch".to_string(),
            message: "controlled mic to platform virtual output".to_string(),
        }
    }

    fn is_virtual_microphone_input(&self, _name: &str) -> bool {
        false
    }

    fn microphone_preflight(&self) -> Result<(), AudioError> {
        Ok(())
    }
}

struct LoopingPcmMicrophone {
    pcm: Arc<Vec<i16>>,
    config: AudioConfig,
    started: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    emitted_chunks: Arc<AtomicUsize>,
}

#[async_trait]
impl AudioCapture for LoopingPcmMicrophone {
    async fn initialize(&mut self, config: AudioConfig) -> AudioResult<()> {
        self.config = config;
        Ok(())
    }

    async fn start_capture(&mut self, on_chunk: AudioChunkCallback) -> AudioResult<()> {
        self.started.store(true, Ordering::SeqCst);
        self.stopped.store(false, Ordering::SeqCst);

        let pcm = self.pcm.clone();
        let config = self.config;
        let stopped = self.stopped.clone();
        let emitted_chunks = self.emitted_chunks.clone();
        tokio::spawn(async move {
            let chunks: Vec<Vec<i16>> = pcm.chunks(4_800).map(|chunk| chunk.to_vec()).collect();
            if chunks.is_empty() {
                return;
            }
            let mut index = 0usize;
            while !stopped.load(Ordering::SeqCst) {
                on_chunk(AudioChunk::new(
                    chunks[index].clone(),
                    config.sample_rate,
                    config.channels,
                ));
                emitted_chunks.fetch_add(1, Ordering::SeqCst);
                index = (index + 1) % chunks.len();
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        });
        Ok(())
    }

    async fn stop_capture(&mut self) -> AudioResult<()> {
        self.stopped.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn is_capturing(&self) -> bool {
        self.started.load(Ordering::SeqCst) && !self.stopped.load(Ordering::SeqCst)
    }

    fn config(&self) -> AudioConfig {
        self.config
    }
}

struct LoopingMicToBlackholeFactory {
    pcm: Arc<Vec<i16>>,
    requested_target: Arc<Mutex<Option<AudioCaptureTarget>>>,
    mic_started: Arc<AtomicBool>,
    mic_stopped: Arc<AtomicBool>,
    emitted_chunks: Arc<AtomicUsize>,
}

#[async_trait]
impl PlatformAudioFactory for LoopingMicToBlackholeFactory {
    fn create_microphone_capture(
        &self,
        _device_name: Option<String>,
        target: AudioCaptureTarget,
    ) -> AudioResult<Box<dyn AudioCapture>> {
        *self.requested_target.lock().unwrap() = Some(target);
        Ok(Box::new(LoopingPcmMicrophone {
            pcm: self.pcm.clone(),
            config: AudioConfig::default(),
            started: self.mic_started.clone(),
            stopped: self.mic_stopped.clone(),
            emitted_chunks: self.emitted_chunks.clone(),
        }))
    }

    fn create_translation_output(
        &self,
    ) -> TranslationAudioOutputResult<Box<dyn TranslationAudioOutput>> {
        Ok(Box::new(CpalAudioOutput::new()))
    }

    fn create_system_loopback_capture(
        &self,
        _target: AudioCaptureTarget,
    ) -> AudioResult<Box<dyn AudioCapture>> {
        Err(AudioError::Configuration(
            "not used by live translation service soak".to_string(),
        ))
    }

    async fn setup_status(&self) -> PlatformAudioSetupStatus {
        PlatformAudioSetupStatus {
            platform: std::env::consts::OS.to_string(),
            status: PlatformAudioSetupState::Ready,
            outgoing_supported: true,
            incoming_supported: true,
            virtual_microphone_name: "BlackHole 2ch".to_string(),
            message: "looping synthetic mic to platform virtual output".to_string(),
        }
    }

    fn is_virtual_microphone_input(&self, _name: &str) -> bool {
        false
    }

    fn microphone_preflight(&self) -> Result<(), AudioError> {
        Ok(())
    }
}

#[async_trait]
impl PlatformAudioFactory for SyntheticMicToBlackholeFactory {
    fn create_microphone_capture(
        &self,
        _device_name: Option<String>,
        target: AudioCaptureTarget,
    ) -> AudioResult<Box<dyn AudioCapture>> {
        *self.requested_target.lock().unwrap() = Some(target);
        Ok(Box::new(SyntheticPcmMicrophone {
            pcm: self.pcm.clone(),
            config: AudioConfig::default(),
            started: self.mic_started.clone(),
            stopped: self.mic_stopped.clone(),
            finished: self.mic_finished.clone(),
            callback: None,
            task: None,
        }))
    }

    fn create_translation_output(
        &self,
    ) -> TranslationAudioOutputResult<Box<dyn TranslationAudioOutput>> {
        Ok(Box::new(CpalAudioOutput::new()))
    }

    fn create_system_loopback_capture(
        &self,
        _target: AudioCaptureTarget,
    ) -> AudioResult<Box<dyn AudioCapture>> {
        Err(AudioError::Configuration(
            "not used by live translation service blackhole e2e".to_string(),
        ))
    }

    async fn setup_status(&self) -> PlatformAudioSetupStatus {
        PlatformAudioSetupStatus {
            platform: std::env::consts::OS.to_string(),
            status: PlatformAudioSetupState::Ready,
            outgoing_supported: true,
            incoming_supported: true,
            virtual_microphone_name: "BlackHole 2ch".to_string(),
            message: "synthetic mic to platform virtual output".to_string(),
        }
    }

    fn is_virtual_microphone_input(&self, _name: &str) -> bool {
        false
    }

    fn microphone_preflight(&self) -> Result<(), AudioError> {
        Ok(())
    }
}

fn drain_openai_events(
    rx: &mut mpsc::Receiver<RealtimeTranslationEvent>,
    translated_text: &mut String,
    pending_audio: &mut Vec<Vec<i16>>,
) -> Option<String> {
    let mut failure = None;
    while let Ok(event) = rx.try_recv() {
        match event {
            RealtimeTranslationEvent::TranslatedTextDelta(text) => {
                print!("{text}");
                translated_text.push_str(&text);
            }
            RealtimeTranslationEvent::TranslatedAudio { pcm16, .. } => pending_audio.push(pcm16),
            RealtimeTranslationEvent::SourceTextDelta(text) => {
                println!("openai_input_delta={text}");
            }
            RealtimeTranslationEvent::Failed(error) => {
                failure = Some(format!("OpenAI realtime error: {error}"));
            }
            RealtimeTranslationEvent::Closed => {
                failure = Some("OpenAI realtime session closed while streaming".to_string());
            }
        }
    }
    failure
}

#[tokio::test]
#[ignore = "paid/manual: requires BlackHole/VB-CABLE, VOICETEXT_RUN_PAID_E2E=1, and a dedicated OPENAI_E2E_API_KEY"]
async fn live_translation_service_synthetic_voice_reaches_blackhole() {
    let api_key = load_paid_e2e_api_key();
    let source_pcm = Arc::new(generate_russian_pcm24());

    let captured = Arc::new(Mutex::new(Vec::<f32>::new()));
    let (input_stream, capture_sample_rate, capture_channels) =
        start_blackhole_capture(captured.clone());
    input_stream.play().expect("must start BlackHole capture");

    let requested_target = Arc::new(Mutex::new(None));
    let mic_started = Arc::new(AtomicBool::new(false));
    let mic_stopped = Arc::new(AtomicBool::new(false));
    let mic_finished = Arc::new(AtomicBool::new(false));
    let service = LiveTranslationService::new_with_factories(
        Arc::new(SyntheticMicToBlackholeFactory {
            pcm: source_pcm,
            requested_target: requested_target.clone(),
            mic_started: mic_started.clone(),
            mic_stopped: mic_stopped.clone(),
            mic_finished: mic_finished.clone(),
        }),
        Arc::new(OpenAIRealtimeTranslationFactory),
    );

    let translated_text = Arc::new(Mutex::new(String::new()));
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let statuses = Arc::new(Mutex::new(Vec::<RecordingStatus>::new()));
    let callbacks = LiveTranslationCallbacks {
        on_transcript_delta: {
            let translated_text = translated_text.clone();
            Arc::new(move |text| translated_text.lock().unwrap().push_str(&text))
        },
        on_audio_spectrum: Arc::new(|_| {}),
        on_error: {
            let errors = errors.clone();
            Arc::new(move |err| errors.lock().unwrap().push(err.to_string()))
        },
        on_status: {
            let statuses = statuses.clone();
            Arc::new(move |status| statuses.lock().unwrap().push(status))
        },
    };

    let mut config = LiveTranslationConfig::new_with_defaults(9_001);
    config.openai_api_key = api_key.clone();
    config.target_language = "en".to_string();
    config.microphone_sensitivity = 100;

    service
        .start_translation(config, callbacks)
        .await
        .expect("service must start live translation");
    assert_eq!(service.get_status().await, RecordingStatus::Recording);
    assert!(mic_started.load(Ordering::SeqCst));
    assert_eq!(
        requested_target.lock().unwrap().unwrap().sample_rate,
        AudioCaptureTarget::outgoing_translation().sample_rate
    );

    let completion = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if mic_finished.load(Ordering::SeqCst) {
                break;
            }
            let current_errors = errors.lock().unwrap().clone();
            assert!(
                current_errors.is_empty(),
                "outgoing translation failed before source completion: {current_errors:?}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    assert!(
        completion.is_ok(),
        "synthetic microphone timeout: mic_finished={}, text={:?}, errors={:?}",
        mic_finished.load(Ordering::SeqCst),
        translated_text.lock().unwrap(),
        errors.lock().unwrap()
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    service
        .stop_translation()
        .await
        .expect("service must stop live translation");
    tokio::time::sleep(Duration::from_millis(500)).await;
    drop(input_stream);

    let final_status = service.get_status().await;
    let final_errors = errors.lock().unwrap().clone();
    assert!(
        final_errors.is_empty(),
        "unexpected service errors with final status {final_status:?}: {final_errors:?}"
    );
    assert_eq!(final_status, RecordingStatus::Idle);
    assert!(mic_stopped.load(Ordering::SeqCst));
    assert!(
        statuses
            .lock()
            .unwrap()
            .contains(&RecordingStatus::Starting)
            && statuses
                .lock()
                .unwrap()
                .contains(&RecordingStatus::Recording),
        "service did not emit expected statuses: {:?}",
        statuses.lock().unwrap()
    );

    let translated_text = translated_text.lock().unwrap().clone();
    let captured_samples = captured.lock().unwrap().clone();
    let captured_pcm16: Vec<i16> = captured_samples
        .iter()
        .map(|sample| (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16)
        .collect();
    let audible_pcm16 =
        audible_pcm16_window(&captured_pcm16, capture_sample_rate, capture_channels);
    let virtual_mic_transcription = transcribe_outgoing_audio(
        &reqwest::Client::new(),
        &api_key,
        capture_sample_rate,
        capture_channels,
        audible_pcm16,
        false,
    )
    .await
    .expect("captured virtual microphone audio must be independently transcribable");
    let virtual_mic_transcript = &virtual_mic_transcription.transcript;
    let measured_rms = rms(&captured_samples);
    let peak = captured_samples
        .iter()
        .fold(0.0f32, |acc, sample| acc.max(sample.abs()));

    let artifact_root = outgoing_artifact_root();
    fs::create_dir_all(&artifact_root).expect("must create outgoing E2E artifact directory");
    fs::write(
        artifact_root.join("virtual-mic-full.wav"),
        wav_pcm16(capture_sample_rate, capture_channels, &captured_pcm16),
    )
    .expect("must write full virtual microphone artifact");
    fs::write(
        artifact_root.join("virtual-mic-audible.wav"),
        wav_pcm16(capture_sample_rate, capture_channels, audible_pcm16),
    )
    .expect("must write audible virtual microphone artifact");
    fs::write(
        artifact_root.join("service-transcript.txt"),
        &translated_text,
    )
    .expect("must write outgoing service transcript");
    fs::write(
        artifact_root.join("virtual-mic-transcript.txt"),
        &virtual_mic_transcript,
    )
    .expect("must write virtual microphone transcript");
    fs::write(
        artifact_root.join("metrics.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "capture_sample_rate": capture_sample_rate,
            "capture_channels": capture_channels,
            "full_samples": captured_pcm16.len(),
            "audible_samples": audible_pcm16.len(),
            "rms": measured_rms,
            "peak": peak,
            "service_transcript": &translated_text,
            "virtual_mic_transcript": virtual_mic_transcript,
            "virtual_mic_primary_transcript": &virtual_mic_transcription.primary_transcript,
            "virtual_mic_primary_error": &virtual_mic_transcription.primary_error,
            "virtual_mic_segmented_fallback_attempted": virtual_mic_transcription.segmented_fallback_attempted,
            "virtual_mic_segmented_fallback_transcript": &virtual_mic_transcription.segmented_fallback_transcript,
            "virtual_mic_segmented_fallback_error": &virtual_mic_transcription.segmented_fallback_error,
            "virtual_mic_whisper_fallback_attempted": virtual_mic_transcription.whisper_fallback_attempted,
            "virtual_mic_whisper_fallback_transcript": &virtual_mic_transcription.whisper_fallback_transcript,
            "virtual_mic_whisper_fallback_error": &virtual_mic_transcription.whisper_fallback_error,
        }))
        .expect("must serialize outgoing E2E metrics"),
    )
    .expect("must write outgoing E2E metrics");

    println!("service_translated_text={translated_text}");
    println!("service_virtual_mic_transcript={virtual_mic_transcript}");
    println!(
        "service_blackhole_samples={}, service_blackhole_rms={measured_rms:.6}, service_blackhole_peak={peak:.6}",
        captured_samples.len()
    );
    println!("service_outgoing_artifacts={}", artifact_root.display());

    let service_transcript = translated_text.to_lowercase();
    assert!(
        service_transcript.contains("alex")
            && service_transcript.contains("english")
            && (service_transcript.contains("voice") || service_transcript.contains("translation")),
        "service translated text lost expected meaning: {translated_text}"
    );
    assert!(
        outgoing_audio_has_expected_meaning(virtual_mic_transcript, false),
        "virtual microphone audio lost translated meaning: {virtual_mic_transcript}"
    );
    assert!(
        measured_rms > 0.005 && peak > 0.03,
        "service translated audio did not reach BlackHole input: rms={measured_rms:.6}, peak={peak:.6}"
    );
}

#[tokio::test]
#[ignore = "paid/manual: requires macOS system audio permission, BlackHole 2ch, VOICETEXT_RUN_PAID_E2E=1, and a dedicated OPENAI_E2E_API_KEY"]
async fn simultaneous_incoming_and_outgoing_routes_translate_and_stop_independently() {
    let api_key = load_paid_e2e_api_key();
    let first_incoming_fixture = generate_english_system_fixture(
        "voicetext_duplex_en_initial",
        "There are forty two release checks. The final review is Friday morning.",
    );
    let incoming_after_outgoing_stop_fixture = generate_english_system_fixture(
        "voicetext_duplex_en_after_outgoing_stop",
        "The deployment checklist contains seventeen verification tasks. The backup review happens on Monday afternoon.",
    );
    let first_outgoing_pcm = generate_russian_pcm24();
    let outgoing_after_incoming_stop_pcm = generate_russian_pcm24_phrase(
        "voicetext_duplex_ru_after_incoming_stop",
        "После остановки входящего перевода канал продолжает работать. Код Феникс семь.",
    );

    let blackhole_capture = Arc::new(Mutex::new(Vec::<f32>::new()));
    let (input_stream, capture_sample_rate, capture_channels) =
        start_blackhole_capture(blackhole_capture.clone());
    input_stream
        .play()
        .expect("must start duplex BlackHole capture");

    let playback_probe = Arc::new(PlaybackProbe::default());
    let incoming = IncomingTranslationFacade::new_spoken_with_factories(
        Arc::new(DefaultPlatformAudioFactory::new()),
        Arc::new(ObservedLocalPlaybackFactory::new(
            Arc::new(DefaultLocalPlaybackOutputFactory::new()),
            playback_probe.clone(),
        )),
        Arc::new(OpenAIRealtimeTranslationFactory),
        Arc::new(DefaultSpokenTranslationCapability::new()),
    );
    let incoming_text = Arc::new(Mutex::new(String::new()));
    let incoming_errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let incoming_callbacks = IncomingTranslationCallbacks {
        on_source_final: Arc::new(|_| {}),
        on_translation_delta: {
            let incoming_text = incoming_text.clone();
            Arc::new(move |text| incoming_text.lock().unwrap().push_str(&text))
        },
        on_error: {
            let incoming_errors = incoming_errors.clone();
            Arc::new(move |error| incoming_errors.lock().unwrap().push(error.to_string()))
        },
        on_status: Arc::new(|_| {}),
    };
    let incoming_config = |session_id| {
        let mut config =
            IncomingTranslationConfig::new_with_defaults(SttConfig::default(), session_id);
        config.openai_api_key = api_key.clone();
        config.target_language = "ru".into();
        config.playback_gain = 0.8;
        config
    };

    let requested_target = Arc::new(Mutex::new(None));
    let mic_started = Arc::new(AtomicBool::new(false));
    let mic_stopped = Arc::new(AtomicBool::new(false));
    let completed_phrases = Arc::new(AtomicUsize::new(0));
    let (mic_tx, mic_rx) = mpsc::unbounded_channel();
    let outgoing = LiveTranslationService::new_with_factories(
        Arc::new(ControlledMicToBlackholeFactory {
            receiver: Mutex::new(Some(mic_rx)),
            requested_target: requested_target.clone(),
            mic_started: mic_started.clone(),
            mic_stopped: mic_stopped.clone(),
            completed_phrases: completed_phrases.clone(),
        }),
        Arc::new(OpenAIRealtimeTranslationFactory),
    );
    let outgoing_text = Arc::new(Mutex::new(String::new()));
    let outgoing_errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let outgoing_callbacks = LiveTranslationCallbacks {
        on_transcript_delta: {
            let outgoing_text = outgoing_text.clone();
            Arc::new(move |text| outgoing_text.lock().unwrap().push_str(&text))
        },
        on_audio_spectrum: Arc::new(|_| {}),
        on_error: {
            let outgoing_errors = outgoing_errors.clone();
            Arc::new(move |error| outgoing_errors.lock().unwrap().push(error.to_string()))
        },
        on_status: Arc::new(|_| {}),
    };
    let mut outgoing_config = LiveTranslationConfig::new_with_defaults(32_002);
    outgoing_config.openai_api_key = api_key.clone();
    outgoing_config.target_language = "en".into();
    outgoing_config.microphone_sensitivity = 100;

    incoming
        .start(incoming_config(32_001), incoming_callbacks.clone())
        .await
        .expect("duplex incoming translation must start");
    outgoing
        .start_translation(outgoing_config, outgoing_callbacks)
        .await
        .expect("duplex outgoing translation must start");
    assert_eq!(incoming.get_status().await, RecordingStatus::Recording);
    assert_eq!(outgoing.get_status().await, RecordingStatus::Recording);
    assert!(mic_started.load(Ordering::SeqCst));

    mic_tx
        .send(first_outgoing_pcm)
        .expect("must queue first controlled microphone phrase");
    tokio::time::sleep(Duration::from_millis(750)).await;
    play_system_fixture(first_incoming_fixture.path(), 0.8).await;
    let initial_evidence = tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            let translated = incoming_text.lock().unwrap().to_lowercase();
            let has_count = translated.contains("42") || translated.contains("сорок дв");
            let has_checks = translated.contains("провер") || translated.contains("чек");
            let has_friday = translated.contains("пятниц");
            if has_count
                && has_checks
                && has_friday
                && playback_probe.audible_accepted_samples() >= 12_000
                && blackhole_capture
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|sample| sample.abs() >= 0.008)
                    .count()
                    >= 4_000
                && completed_phrases.load(Ordering::SeqCst) >= 1
            {
                return Ok::<(), String>(());
            }
            let current_incoming_errors = incoming_errors.lock().unwrap().clone();
            let current_outgoing_errors = outgoing_errors.lock().unwrap().clone();
            if !current_incoming_errors.is_empty() || !current_outgoing_errors.is_empty() {
                return Err(format!(
                    "duplex translation failed: incoming={current_incoming_errors:?}, outgoing={current_outgoing_errors:?}"
                ));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    let initial_failure = match initial_evidence {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error),
        Err(_) => Some(format!(
            "initial duplex evidence timed out: incoming={:?}, outgoing={:?}, accepted_audible={}, blackhole_audible={}, completed_phrases={}, incoming_errors={:?}, outgoing_errors={:?}",
            incoming_text.lock().unwrap().clone(),
            outgoing_text.lock().unwrap().clone(),
            playback_probe.audible_accepted_samples(),
            blackhole_capture
                .lock()
                .unwrap()
                .iter()
                .filter(|sample| sample.abs() >= 0.008)
                .count(),
            completed_phrases.load(Ordering::SeqCst),
            incoming_errors.lock().unwrap().clone(),
            outgoing_errors.lock().unwrap().clone()
        )),
    };
    if let Some(error) = initial_failure {
        let _ = incoming.stop().await;
        let _ = outgoing.stop_translation().await;
        drop(input_stream);
        panic!("{error}");
    }

    incoming
        .stop()
        .await
        .expect("first duplex incoming session must stop");
    assert_eq!(incoming.get_status().await, RecordingStatus::Idle);
    assert_eq!(outgoing.get_status().await, RecordingStatus::Recording);
    let first_incoming_dropped_batches = playback_probe.dropped_batches();
    let first_incoming_dropped_audio = playback_probe.dropped_audio_duration();

    tokio::time::sleep(Duration::from_millis(500)).await;
    let outgoing_text_boundary = outgoing_text.lock().unwrap().len();
    let blackhole_audible_boundary = blackhole_capture
        .lock()
        .unwrap()
        .iter()
        .filter(|sample| sample.abs() >= 0.008)
        .count();
    mic_tx
        .send(outgoing_after_incoming_stop_pcm)
        .expect("outgoing mic must accept fresh speech after incoming stops");
    let outgoing_continued = tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            let blackhole_audible = blackhole_capture
                .lock()
                .unwrap()
                .iter()
                .filter(|sample| sample.abs() >= 0.008)
                .count();
            if blackhole_audible >= blackhole_audible_boundary.saturating_add(4_000)
                && completed_phrases.load(Ordering::SeqCst) >= 2
            {
                return Ok::<(), String>(());
            }
            let errors = outgoing_errors.lock().unwrap().clone();
            if !errors.is_empty() {
                return Err(format!("outgoing failed after incoming stop: {errors:?}"));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    let outgoing_failure = match outgoing_continued {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error),
        Err(_) => Some(format!(
            "outgoing produced no fresh evidence after incoming stop: fresh_text={:?}, blackhole_audible_delta={}, completed_phrases={}, errors={:?}",
            outgoing_text
                .lock()
                .unwrap()
                .get(outgoing_text_boundary..)
                .unwrap_or_default()
                .to_string(),
            blackhole_capture
                .lock()
                .unwrap()
                .iter()
                .filter(|sample| sample.abs() >= 0.008)
                .count()
                .saturating_sub(blackhole_audible_boundary),
            completed_phrases.load(Ordering::SeqCst),
            outgoing_errors.lock().unwrap().clone()
        )),
    };
    if let Some(error) = outgoing_failure {
        let _ = outgoing.stop_translation().await;
        drop(input_stream);
        panic!("{error}");
    }

    incoming
        .start(incoming_config(32_003), incoming_callbacks)
        .await
        .expect("incoming must restart while outgoing remains active");
    assert_eq!(incoming.get_status().await, RecordingStatus::Recording);
    assert_eq!(outgoing.get_status().await, RecordingStatus::Recording);

    tokio::time::sleep(Duration::from_millis(500)).await;
    outgoing
        .stop_translation()
        .await
        .expect("duplex outgoing translation must stop");
    assert_eq!(outgoing.get_status().await, RecordingStatus::Idle);
    assert_eq!(incoming.get_status().await, RecordingStatus::Recording);
    assert!(mic_stopped.load(Ordering::SeqCst));

    let incoming_text_boundary = incoming_text.lock().unwrap().len();
    let incoming_audio_boundary = playback_probe.audible_accepted_samples();
    tokio::time::sleep(Duration::from_millis(750)).await;
    play_system_fixture(incoming_after_outgoing_stop_fixture.path(), 0.8).await;
    let incoming_continued = tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            let incoming_guard = incoming_text.lock().unwrap();
            let fresh_incoming = incoming_guard
                .get(incoming_text_boundary..)
                .unwrap_or_default()
                .to_lowercase();
            drop(incoming_guard);
            let has_count = fresh_incoming.contains("17") || fresh_incoming.contains("семнадцать");
            let has_checks = fresh_incoming.contains("провер") || fresh_incoming.contains("задач");
            let has_monday = fresh_incoming.contains("понедель");
            if has_count
                && has_checks
                && has_monday
                && playback_probe.audible_accepted_samples()
                    >= incoming_audio_boundary.saturating_add(12_000)
            {
                return Ok::<(), String>(());
            }
            let errors = incoming_errors.lock().unwrap().clone();
            if !errors.is_empty() {
                return Err(format!("incoming failed after outgoing stop: {errors:?}"));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    let incoming_failure = match incoming_continued {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error),
        Err(_) => Some(format!(
            "incoming produced no fresh evidence after outgoing stop: fresh_text={:?}, accepted_audible_delta={}, errors={:?}",
            incoming_text
                .lock()
                .unwrap()
                .get(incoming_text_boundary..)
                .unwrap_or_default()
                .to_string(),
            playback_probe
                .audible_accepted_samples()
                .saturating_sub(incoming_audio_boundary),
            incoming_errors.lock().unwrap().clone()
        )),
    };
    if let Some(error) = incoming_failure {
        let _ = incoming.stop().await;
        drop(input_stream);
        panic!("{error}");
    }

    incoming
        .stop()
        .await
        .expect("restarted duplex incoming session must stop");
    assert_eq!(incoming.get_status().await, RecordingStatus::Idle);
    assert_eq!(playback_probe.opened(), 2);
    assert_eq!(playback_probe.closed(), 2);
    assert!(playback_probe.accepted_samples() >= playback_probe.audible_accepted_samples());
    assert!(playback_probe.audible_accepted_samples() >= 24_000);
    let second_incoming_dropped_batches = playback_probe
        .dropped_batches()
        .saturating_sub(first_incoming_dropped_batches);
    let second_incoming_dropped_audio = playback_probe
        .dropped_audio_duration()
        .saturating_sub(first_incoming_dropped_audio);
    println!(
        "duplex_playback_drops=first:{} batches/{}ms second:{} batches/{}ms",
        first_incoming_dropped_batches,
        first_incoming_dropped_audio.as_millis(),
        second_incoming_dropped_batches,
        second_incoming_dropped_audio.as_millis()
    );
    assert!(
        first_incoming_dropped_batches < 8 && second_incoming_dropped_batches < 8,
        "incoming playback reached overload drop threshold: first={first_incoming_dropped_batches}, second={second_incoming_dropped_batches}"
    );
    assert!(
        first_incoming_dropped_audio <= Duration::from_secs(1)
            && second_incoming_dropped_audio <= Duration::from_secs(1),
        "incoming playback dropped too much audio: first={}ms, second={}ms",
        first_incoming_dropped_audio.as_millis(),
        second_incoming_dropped_audio.as_millis()
    );

    tokio::time::sleep(Duration::from_millis(500)).await;
    drop(input_stream);
    let captured_samples = blackhole_capture.lock().unwrap().clone();
    let captured_pcm16: Vec<i16> = captured_samples
        .iter()
        .map(|sample| (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16)
        .collect();
    let audible_pcm16 =
        audible_pcm16_window(&captured_pcm16, capture_sample_rate, capture_channels);
    let virtual_mic_transcription = transcribe_outgoing_audio(
        &reqwest::Client::new(),
        &api_key,
        capture_sample_rate,
        capture_channels,
        audible_pcm16,
        true,
    )
    .await
    .expect("duplex virtual microphone audio must be independently transcribable");
    let virtual_mic_transcript = &virtual_mic_transcription.transcript;

    let incoming_transcript = incoming_text.lock().unwrap().clone();
    let outgoing_transcript = outgoing_text.lock().unwrap().clone();
    let artifact_root = outgoing_artifact_root();
    fs::create_dir_all(&artifact_root).expect("must create duplex artifact directory");
    fs::write(
        artifact_root.join("duplex-virtual-mic.wav"),
        wav_pcm16(capture_sample_rate, capture_channels, audible_pcm16),
    )
    .expect("must write duplex virtual microphone artifact");
    fs::write(
        artifact_root.join("duplex-incoming-transcript.txt"),
        &incoming_transcript,
    )
    .expect("must write duplex incoming transcript");
    fs::write(
        artifact_root.join("duplex-outgoing-transcript.txt"),
        &outgoing_transcript,
    )
    .expect("must write duplex outgoing transcript");
    fs::write(
        artifact_root.join("duplex-virtual-mic-transcript.txt"),
        &virtual_mic_transcript,
    )
    .expect("must write duplex virtual microphone transcript");
    println!("duplex_incoming_text={incoming_transcript}");
    println!("duplex_outgoing_text={outgoing_transcript}");
    println!("duplex_virtual_mic_text={virtual_mic_transcript}");
    println!("duplex_artifacts={}", artifact_root.display());

    let outgoing_normalized = outgoing_transcript.to_lowercase();
    let incoming_normalized = incoming_transcript.to_lowercase();
    assert!(
        (incoming_normalized.contains("42") || incoming_normalized.contains("сорок дв"))
            && (incoming_normalized.contains("провер") || incoming_normalized.contains("чек"))
            && incoming_normalized.contains("пятниц")
            && (incoming_normalized.contains("17") || incoming_normalized.contains("семнадцать"))
            && incoming_normalized.contains("понедель"),
        "duplex incoming text lost meaning: {incoming_transcript}"
    );
    assert!(
        outgoing_normalized.contains("alex")
            && outgoing_normalized.contains("english")
            && (outgoing_normalized.contains("voice")
                || outgoing_normalized.contains("translation"))
            && outgoing_normalized.contains("phoenix")
            && (outgoing_normalized.contains('7') || outgoing_normalized.contains("seven")),
        "duplex outgoing text lost meaning: {outgoing_transcript}"
    );
    assert!(
        outgoing_audio_has_expected_meaning(virtual_mic_transcript, true),
        "duplex virtual microphone lost meaning: {virtual_mic_transcript}"
    );
    assert!(
        incoming_errors.lock().unwrap().is_empty() && outgoing_errors.lock().unwrap().is_empty(),
        "duplex errors: incoming={:?}, outgoing={:?}",
        incoming_errors.lock().unwrap(),
        outgoing_errors.lock().unwrap()
    );
}

#[tokio::test]
#[ignore = "paid/manual soak: requires BlackHole 2ch, VOICETEXT_RUN_PAID_E2E=1, and a dedicated OPENAI_E2E_API_KEY"]
async fn live_translation_service_long_running_synthetic_voice_soak() {
    let api_key = load_paid_e2e_api_key();
    let soak_duration = live_audio_soak_duration();
    let source_pcm = Arc::new(generate_russian_pcm24());

    let blackhole_stats = Arc::new(Mutex::new(AudioStats::default()));
    let input_stream = start_blackhole_stats_capture(blackhole_stats.clone());
    input_stream.play().expect("must start BlackHole capture");

    let requested_target = Arc::new(Mutex::new(None));
    let mic_started = Arc::new(AtomicBool::new(false));
    let mic_stopped = Arc::new(AtomicBool::new(false));
    let emitted_chunks = Arc::new(AtomicUsize::new(0));
    let service = LiveTranslationService::new_with_factories(
        Arc::new(LoopingMicToBlackholeFactory {
            pcm: source_pcm,
            requested_target: requested_target.clone(),
            mic_started: mic_started.clone(),
            mic_stopped: mic_stopped.clone(),
            emitted_chunks: emitted_chunks.clone(),
        }),
        Arc::new(OpenAIRealtimeTranslationFactory),
    );

    let translated_text = Arc::new(Mutex::new(String::new()));
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let statuses = Arc::new(Mutex::new(Vec::<RecordingStatus>::new()));
    let callbacks = LiveTranslationCallbacks {
        on_transcript_delta: {
            let translated_text = translated_text.clone();
            Arc::new(move |text| translated_text.lock().unwrap().push_str(&text))
        },
        on_audio_spectrum: Arc::new(|_| {}),
        on_error: {
            let errors = errors.clone();
            Arc::new(move |err| errors.lock().unwrap().push(err.to_string()))
        },
        on_status: {
            let statuses = statuses.clone();
            Arc::new(move |status| statuses.lock().unwrap().push(status))
        },
    };

    let mut config = LiveTranslationConfig::new_with_defaults(19_001);
    config.openai_api_key = api_key;
    config.target_language = "en".to_string();
    config.microphone_sensitivity = 100;

    service
        .start_translation(config, callbacks)
        .await
        .expect("service must start long live translation soak");
    assert_eq!(service.get_status().await, RecordingStatus::Recording);
    assert!(mic_started.load(Ordering::SeqCst));
    assert_eq!(
        requested_target.lock().unwrap().unwrap().sample_rate,
        AudioCaptureTarget::outgoing_translation().sample_rate
    );

    println!(
        "live_translation_soak_seconds={}",
        soak_duration.as_secs_f32()
    );
    tokio::time::sleep(soak_duration).await;

    service
        .stop_translation()
        .await
        .expect("service must stop long live translation soak");
    tokio::time::sleep(Duration::from_millis(750)).await;
    drop(input_stream);

    assert_eq!(service.get_status().await, RecordingStatus::Idle);
    assert!(mic_stopped.load(Ordering::SeqCst));
    assert!(
        errors.lock().unwrap().is_empty(),
        "unexpected service errors during soak: {:?}",
        errors.lock().unwrap()
    );
    assert!(
        statuses
            .lock()
            .unwrap()
            .contains(&RecordingStatus::Recording),
        "service did not reach Recording during soak: {:?}",
        statuses.lock().unwrap()
    );

    let emitted = emitted_chunks.load(Ordering::SeqCst);
    let translated_len = translated_text.lock().unwrap().trim().len();
    let stats = blackhole_stats.lock().unwrap();
    let measured_rms = stats.rms();
    let peak = stats.peak;
    println!(
        "live_translation_soak_chunks={emitted}, translated_chars={translated_len}, blackhole_samples={}, blackhole_rms={measured_rms:.6}, blackhole_peak={peak:.6}",
        stats.samples
    );

    assert!(
        emitted >= (soak_duration.as_millis() / 300) as usize,
        "synthetic mic emitted too few chunks for soak duration: {emitted}"
    );
    assert!(
        translated_len > 0,
        "OpenAI did not emit translated transcript during soak"
    );
    assert!(
        measured_rms > 0.003 && peak > 0.02,
        "translated soak audio did not reach BlackHole input: rms={measured_rms:.6}, peak={peak:.6}"
    );
}

#[tokio::test]
#[ignore = "paid/manual: requires BlackHole 2ch, VOICETEXT_RUN_PAID_E2E=1, and a dedicated OPENAI_E2E_API_KEY"]
async fn openai_translation_audio_is_written_to_blackhole() {
    let api_key = load_paid_e2e_api_key();
    let source_pcm = generate_russian_pcm24();

    let mut client = OpenAIRealtimeTranslationClient::new();
    let mut rx = client
        .connect(RealtimeTranslationConfig::new(
            api_key,
            "en".to_string(),
            RealtimeInputNoiseReduction::NearField,
        ))
        .await
        .expect("must connect OpenAI realtime");
    let mut translated_text = String::new();
    let mut pending_audio = Vec::<Vec<i16>>::new();

    tokio::time::sleep(Duration::from_millis(500)).await;
    if let Some(failure) = drain_openai_events(&mut rx, &mut translated_text, &mut pending_audio) {
        panic!("{failure}");
    }

    for chunk in source_pcm.chunks(4_800) {
        if let Some(failure) =
            drain_openai_events(&mut rx, &mut translated_text, &mut pending_audio)
        {
            panic!("{failure}");
        }
        client
            .append_pcm16(chunk)
            .await
            .expect("must append input audio");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    client
        .finish(Duration::from_secs(6))
        .await
        .expect("must close OpenAI realtime session");
    let _ = drain_openai_events(&mut rx, &mut translated_text, &mut pending_audio);

    let captured = Arc::new(Mutex::new(Vec::<f32>::new()));
    let (input_stream, _, _) = start_blackhole_capture(captured.clone());
    input_stream.play().expect("must start BlackHole capture");

    let mut output = CpalAudioOutput::new();
    let output_config = AudioOutputConfig {
        max_buffered_duration: Duration::from_secs(20),
        drain_max_buffered_duration: Duration::from_secs(20),
        ..AudioOutputConfig::openai_translation()
    };
    output
        .open(output_config)
        .await
        .expect("must open BlackHole output");

    let mut audio_samples = 0usize;
    let mut translated_audio_for_stats = Vec::new();
    for samples in pending_audio {
        audio_samples += samples.len();
        translated_audio_for_stats.extend(
            samples
                .iter()
                .map(|sample| *sample as f32 / i16::MAX as f32),
        );
        output
            .enqueue_pcm16(&samples)
            .await
            .expect("must enqueue translated audio");
    }
    while let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
        match event {
            RealtimeTranslationEvent::TranslatedTextDelta(text) => translated_text.push_str(&text),
            RealtimeTranslationEvent::TranslatedAudio { pcm16: samples, .. } => {
                audio_samples += samples.len();
                translated_audio_for_stats.extend(
                    samples
                        .iter()
                        .map(|sample| *sample as f32 / i16::MAX as f32),
                );
                output
                    .enqueue_pcm16(&samples)
                    .await
                    .expect("must enqueue translated audio");
            }
            RealtimeTranslationEvent::Failed(error) => {
                panic!("OpenAI realtime error: {error}");
            }
            _ => {}
        }
    }

    tokio::time::sleep(Duration::from_secs(6)).await;
    output.close().await.expect("must close output");
    drop(input_stream);

    let captured_samples = captured.lock().unwrap().clone();
    let measured_rms = rms(&captured_samples);
    let peak = captured_samples
        .iter()
        .fold(0.0f32, |acc, sample| acc.max(sample.abs()));

    println!("translated_text={translated_text}");
    println!(
        "openai_audio_samples={audio_samples}, openai_audio_rms={:.6}, blackhole_samples={}, blackhole_rms={measured_rms:.6}, blackhole_peak={peak:.6}",
        rms(&translated_audio_for_stats),
        captured_samples.len()
    );

    assert!(audio_samples > 0, "OpenAI did not return translated audio");
    assert!(
        translated_text.to_lowercase().contains("english")
            || translated_text.to_lowercase().contains("voice")
            || translated_text.to_lowercase().contains("translation")
            || translated_text.to_lowercase().contains("alex"),
        "translated text looks unexpected/empty: {translated_text}"
    );
    assert!(
        measured_rms > 0.005 && peak > 0.03,
        "translated audio did not reach BlackHole input: rms={measured_rms:.6}, peak={peak:.6}"
    );
}
