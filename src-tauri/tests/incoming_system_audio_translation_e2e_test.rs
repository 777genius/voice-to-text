#![cfg(target_os = "macos")]

mod paid_e2e_support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use app_lib::application::{
    IncomingTranslationCallbacks, IncomingTranslationConfig, IncomingTranslationFacade,
};
use app_lib::domain::{
    AudioCapture, AudioCaptureErrorCallback, AudioCaptureTarget, AudioChunk, AudioChunkCallback,
    AudioConfig, AudioEnqueueOutcome, ConnectionQualityCallback, ErrorCallback,
    LocalPlaybackOutputFactory, LocalPlaybackRoute, RecordingStatus, SpokenIncomingCapability,
    SpokenTranslationCapability, SttConfig, SttError, SttProvider, SttProviderFactory, SttResult,
    SystemAudioCaptureFactory, SystemAudioCaptureRequest, Transcription, TranscriptionCallback,
    TranslationAudioOutput, TranslationAudioOutputConfig, TranslationAudioOutputResult,
};
use app_lib::infrastructure::audio::{
    DefaultLocalPlaybackOutputFactory, DefaultPlatformAudioFactory,
};
use app_lib::infrastructure::openai::OpenAIRealtimeTranslationFactory;
use async_trait::async_trait;
use paid_e2e_support::{load_paid_e2e_api_key, transcribe_pcm16, wav_pcm16};
const TRANSCRIBE_AFTER_SAMPLES: usize = 16_000 * 2;
const MAX_TRANSLATED_FINALS_PER_SOAK: usize = 3;
const AUDIBLE_SAMPLE_ABS_THRESHOLD: u16 = 128;
const MIN_AUDIBLE_TRANSLATED_SAMPLES: usize = 1_200;
const PAID_SOURCE_PREROLL: Duration = Duration::from_secs(1);
static NEXT_TEMP_AUDIO_ID: AtomicUsize = AtomicUsize::new(0);

fn audible_sample_count(samples: &[i16]) -> usize {
    samples
        .iter()
        .filter(|sample| sample.unsigned_abs() >= AUDIBLE_SAMPLE_ABS_THRESHOLD)
        .count()
}

fn live_audio_soak_duration() -> Duration {
    std::env::var("LIVE_AUDIO_SOAK_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(600))
}

struct TempAudioFixture {
    path: PathBuf,
}

impl TempAudioFixture {
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempAudioFixture {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn unique_temp_audio_path(prefix: &str, extension: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = NEXT_TEMP_AUDIO_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "{prefix}_{}_{}_{}.{}",
        std::process::id(),
        nanos,
        sequence,
        extension
    ))
}

fn wait_for_child_with_timeout(mut child: Child, timeout: Duration, process_name: &str) {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child
            .try_wait()
            .unwrap_or_else(|err| panic!("must wait for {process_name}: {err}"))
        {
            assert!(status.success(), "{process_name} failed");
            return;
        }

        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{process_name} timed out after {timeout:?}");
        }

        std::thread::sleep(Duration::from_millis(50));
    }
}

fn generate_system_audio_fixture() -> TempAudioFixture {
    generate_spoken_audio_fixture(
        "voicetext_incoming_system_audio_source",
        "Samantha",
        "Hello from the call. This checks incoming subtitles.",
    )
}

fn generate_spoken_audio_fixture(prefix: &str, voice: &str, text: &str) -> TempAudioFixture {
    let aiff_path = unique_temp_audio_path(prefix, "aiff");

    let voices = Command::new("say")
        .args(["-v", "?"])
        .output()
        .expect("must list installed macOS voices");
    assert!(voices.status.success(), "macOS voice listing failed");
    let voice_is_installed = String::from_utf8_lossy(&voices.stdout)
        .lines()
        .any(|line| line.split_whitespace().next() == Some(voice));
    assert!(
        voice_is_installed,
        "required macOS voice {voice} is not installed"
    );

    let status = Command::new("say")
        .args([
            "-v",
            voice,
            "-r",
            "145",
            "-o",
            aiff_path.to_str().expect("valid aiff path"),
            text,
        ])
        .status()
        .expect("must run macOS say");
    assert!(status.success(), "macOS say failed");
    TempAudioFixture { path: aiff_path }
}

fn play_system_audio(path: &Path) {
    let child = Command::new("afplay")
        .arg(path)
        .spawn()
        .expect("must run afplay");
    wait_for_child_with_timeout(child, Duration::from_secs(15), "afplay");
}

fn generate_tone(sample_rate: u32, frequency_hz: f64, duration: Duration) -> Vec<i16> {
    let sample_count = (sample_rate as f64 * duration.as_secs_f64()).round() as usize;
    (0..sample_count)
        .map(|index| {
            let phase = std::f64::consts::TAU * frequency_hz * index as f64 / sample_rate as f64;
            (phase.sin() * i16::MAX as f64 * 0.35) as i16
        })
        .collect()
}

fn generate_tone_fixture(frequency_hz: f64, duration: Duration) -> TempAudioFixture {
    let sample_rate = 24_000;
    let samples = generate_tone(sample_rate, frequency_hz, duration);
    let path = unique_temp_audio_path("voicetext_system_audio_tone", "wav");
    fs::write(&path, wav_pcm16(sample_rate, 1, &samples)).expect("must write tone fixture");
    TempAudioFixture { path }
}

fn goertzel_power(samples: &[i16], sample_rate: u32, frequency_hz: f64) -> f64 {
    if samples.is_empty() || sample_rate == 0 {
        return 0.0;
    }
    let omega = std::f64::consts::TAU * frequency_hz / sample_rate as f64;
    let coefficient = 2.0 * omega.cos();
    let mut previous = 0.0f64;
    let mut previous_two = 0.0f64;
    for sample in samples {
        let current = *sample as f64 + coefficient * previous - previous_two;
        previous_two = previous;
        previous = current;
    }
    previous_two * previous_two + previous * previous - coefficient * previous * previous_two
}

fn join_named_thread(handle: std::thread::JoinHandle<()>, name: &str) -> Result<(), String> {
    handle.join().map_err(|panic_payload| {
        let reason = panic_payload
            .downcast_ref::<&str>()
            .map(|value| (*value).to_string())
            .or_else(|| panic_payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic payload".to_string());
        format!("{name} thread panicked: {reason}")
    })
}

#[derive(Default)]
struct PaidSpokenOutputState {
    opened: AtomicBool,
    closed: AtomicBool,
    samples: Mutex<Vec<i16>>,
    gain: Mutex<Option<f32>>,
    started_at: Mutex<Option<Instant>>,
    first_audio_ms: Mutex<Option<u128>>,
    audible_samples: AtomicUsize,
}

struct PaidSpokenOutputFactory {
    state: Arc<PaidSpokenOutputState>,
}

impl LocalPlaybackOutputFactory for PaidSpokenOutputFactory {
    fn create_local_playback_output(
        &self,
        route: LocalPlaybackRoute,
    ) -> TranslationAudioOutputResult<Box<dyn TranslationAudioOutput>> {
        assert_eq!(route, LocalPlaybackRoute::SystemDefault);
        Ok(Box::new(PaidSpokenOutput {
            state: self.state.clone(),
            delegate: DefaultLocalPlaybackOutputFactory::new()
                .create_local_playback_output(route)?,
        }))
    }
}

struct PaidSpokenOutput {
    state: Arc<PaidSpokenOutputState>,
    delegate: Box<dyn TranslationAudioOutput>,
}

#[async_trait]
impl TranslationAudioOutput for PaidSpokenOutput {
    async fn open(
        &mut self,
        config: TranslationAudioOutputConfig,
    ) -> TranslationAudioOutputResult<()> {
        self.delegate.open(config).await?;
        self.state.opened.store(true, Ordering::SeqCst);
        *self.state.gain.lock().unwrap() = Some(config.gain);
        Ok(())
    }

    async fn enqueue_pcm16(
        &self,
        samples: &[i16],
    ) -> TranslationAudioOutputResult<AudioEnqueueOutcome> {
        let outcome = self.delegate.enqueue_pcm16(samples).await?;
        let mut first_audio_ms = self.state.first_audio_ms.lock().unwrap();
        if first_audio_ms.is_none() {
            *first_audio_ms = self
                .state
                .started_at
                .lock()
                .unwrap()
                .map(|started_at| started_at.elapsed().as_millis());
        }
        drop(first_audio_ms);
        self.state
            .samples
            .lock()
            .unwrap()
            .extend_from_slice(samples);
        self.state
            .audible_samples
            .fetch_add(audible_sample_count(samples), Ordering::Relaxed);
        Ok(outcome)
    }

    async fn close(&mut self) -> TranslationAudioOutputResult<()> {
        self.delegate.close().await?;
        self.state.closed.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn set_gain(&mut self, gain: f32) -> TranslationAudioOutputResult<()> {
        self.delegate.set_gain(gain)?;
        *self.state.gain.lock().unwrap() = Some(gain);
        Ok(())
    }

    fn is_open(&self) -> bool {
        self.delegate.is_open()
    }

    fn device_name(&self) -> Option<String> {
        self.delegate.device_name()
    }

    fn begin_drain_mode(&self) {
        self.delegate.begin_drain_mode();
    }

    fn prepare_for_drain(&self) -> TranslationAudioOutputResult<Duration> {
        self.delegate.prepare_for_drain()
    }

    fn pending_playback_duration(&self) -> Duration {
        self.delegate.pending_playback_duration()
    }
}

struct PaidSpokenReadyCapability;

impl SpokenTranslationCapability for PaidSpokenReadyCapability {
    fn check(&self, _target_language: &str) -> SpokenIncomingCapability {
        SpokenIncomingCapability::Ready
    }
}

struct ObservedSystemAudioCaptureFactory {
    inner: DefaultPlatformAudioFactory,
    started_at: Instant,
    first_input_ms: Arc<Mutex<Option<u128>>>,
    captured_samples: Arc<Mutex<Vec<i16>>>,
}

impl SystemAudioCaptureFactory for ObservedSystemAudioCaptureFactory {
    fn preflight_system_audio_capture(
        &self,
        request: SystemAudioCaptureRequest,
    ) -> app_lib::domain::AudioResult<()> {
        self.inner.preflight_system_audio_capture(request)
    }

    fn create_system_audio_capture(
        &self,
        request: SystemAudioCaptureRequest,
    ) -> app_lib::domain::AudioResult<Box<dyn AudioCapture>> {
        Ok(Box::new(ObservedSystemAudioCapture {
            inner: self.inner.create_system_audio_capture(request)?,
            started_at: self.started_at,
            first_input_ms: self.first_input_ms.clone(),
            captured_samples: self.captured_samples.clone(),
        }))
    }
}

struct ObservedSystemAudioCapture {
    inner: Box<dyn AudioCapture>,
    started_at: Instant,
    first_input_ms: Arc<Mutex<Option<u128>>>,
    captured_samples: Arc<Mutex<Vec<i16>>>,
}

#[async_trait]
impl AudioCapture for ObservedSystemAudioCapture {
    async fn initialize(&mut self, config: AudioConfig) -> app_lib::domain::AudioResult<()> {
        self.inner.initialize(config).await
    }

    async fn start_capture(
        &mut self,
        callback: AudioChunkCallback,
    ) -> app_lib::domain::AudioResult<()> {
        let started_at = self.started_at;
        let first_input_ms = self.first_input_ms.clone();
        let captured_samples = self.captured_samples.clone();
        self.inner
            .start_capture(Arc::new(move |chunk| {
                first_input_ms
                    .lock()
                    .unwrap()
                    .get_or_insert_with(|| started_at.elapsed().as_millis());
                captured_samples
                    .lock()
                    .unwrap()
                    .extend_from_slice(&chunk.data);
                callback(chunk);
            }))
            .await
    }

    async fn stop_capture(&mut self) -> app_lib::domain::AudioResult<()> {
        self.inner.stop_capture().await
    }

    fn set_terminal_error_callback(&mut self, callback: Option<AudioCaptureErrorCallback>) {
        self.inner.set_terminal_error_callback(callback);
    }

    fn is_capturing(&self) -> bool {
        self.inner.is_capturing()
    }

    fn config(&self) -> AudioConfig {
        self.inner.config()
    }
}

#[derive(Clone, Copy)]
struct PaidSpokenScenario {
    id: &'static str,
    primary_voice: &'static str,
    source: &'static str,
    secondary_source: Option<&'static str>,
    human_reference: &'static str,
    required_translation_markers: &'static [&'static [&'static str]],
    translation_output_required: bool,
}

fn missing_translation_marker_groups(translated: &str, required_groups: &[&[&str]]) -> Vec<String> {
    let normalized = translated.to_lowercase();
    required_groups
        .iter()
        .filter(|group| !group.iter().any(|marker| normalized.contains(marker)))
        .map(|group| group.join(" | "))
        .collect()
}

fn paid_spoken_scenarios() -> Vec<PaidSpokenScenario> {
    vec![
        PaidSpokenScenario {
            id: "english_to_russian",
            primary_voice: "Samantha",
            source: "Hello from the call. Please translate this sentence into Russian.",
            secondary_source: None,
            human_reference: "Natural Russian translation preserving the request.",
            required_translation_markers: &[&["перевед", "перевод"]],
            translation_output_required: true,
        },
        PaidSpokenScenario {
            id: "names_and_numbers",
            primary_voice: "Samantha",
            source: "My name is Robert Brown. This is a business meeting. The meeting date is October 21st. The meeting time is 3:45 PM. The room number is 207.",
            secondary_source: None,
            human_reference: "Preserve the name, meeting context, date, time, and room 207.",
            required_translation_markers: &[
                &["роберт"],
                &["встреч", "совещ"],
                &["октябр"],
                &["21", "двадцать перв"],
                &["3:45", "15:45", "три сорок пять", "пятнадцать сорок пять"],
                &["207", "два ноль семь", "двести семь"],
            ],
            translation_output_required: true,
        },
        PaidSpokenScenario {
            id: "technical_terms",
            primary_voice: "Samantha",
            source: "The WebSocket reconnect uses exponential backoff. The system uses bounded queues. The audio format is 24 kilohertz PCM.",
            secondary_source: None,
            human_reference: "Preserve WebSocket, exponential backoff, bounded queues, and 24 kHz PCM.",
            required_translation_markers: &[
                &["websocket", "веб-сокет", "вебсокет"],
                &["экспоненц", "exponential"],
                &["очеред"],
                &["24", "двадцать четыре"],
                &["pcm"],
            ],
            translation_output_required: true,
        },
        PaidSpokenScenario {
            id: "mixed_english_russian",
            primary_voice: "Milena",
            source: "Please open настройки and choose режим text and audio for this call.",
            secondary_source: None,
            human_reference: "Produce coherent Russian while preserving the UI mode name.",
            required_translation_markers: &[
                &["настрой"],
                &["режим"],
                &["текст"],
                &["аудио", "звук"],
            ],
            translation_output_required: true,
        },
        PaidSpokenScenario {
            id: "already_russian",
            primary_voice: "Milena",
            source: "Добрый день. Проверяем, что русская речь остается понятной и не искажается.",
            secondary_source: None,
            human_reference: "Keep the Russian meaning without inventing content.",
            required_translation_markers: &[
                &["добрый день"],
                &["русск"],
                &["понят"],
                &["искаж"],
            ],
            translation_output_required: false,
        },
        PaidSpokenScenario {
            id: "long_context",
            primary_voice: "Samantha",
            source: "During yesterday's incident the first deployment failed because the certificate expired. After the certificate was renewed, the second deployment succeeded, so do not roll back the database migration.",
            secondary_source: None,
            human_reference: "Preserve chronology, causality, and the instruction not to roll back.",
            required_translation_markers: &[
                &["сертификат"],
                &["втор"],
                &["успеш"],
                &["не откат", "не делать откат", "не отмен"],
                &["миграц"],
            ],
            translation_output_required: true,
        },
        PaidSpokenScenario {
            id: "pause_and_silence",
            primary_voice: "Samantha",
            source: "The first value is twelve. [[slnc 700]] The second value is forty seven. [[slnc 900]] Keep both values.",
            secondary_source: None,
            human_reference: "Preserve values 12 and 47 across pauses.",
            required_translation_markers: &[
                &["12", "двенадцать"],
                &["47", "сорок семь"],
            ],
            translation_output_required: true,
        },
        PaidSpokenScenario {
            id: "overlapping_speakers",
            primary_voice: "Samantha",
            source: "Alice says the release is scheduled for Friday morning.",
            secondary_source: Some("Bob says the security review must finish before Thursday evening."),
            human_reference: "Best effort mixed-track translation; note any lost speaker or timing detail.",
            required_translation_markers: &[&["пятниц", "четверг"]],
            translation_output_required: true,
        },
    ]
}

fn paid_artifact_root() -> PathBuf {
    if let Some(path) = std::env::var_os("INCOMING_SPOKEN_E2E_ARTIFACTS") {
        return PathBuf::from(path);
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/e2e-artifacts")
        .join(format!("incoming-spoken-{timestamp}"))
}

async fn play_paid_scenario(primary: &Path, secondary: Option<&Path>) {
    let primary = primary.to_path_buf();
    let secondary = secondary.map(Path::to_path_buf);
    tokio::task::spawn_blocking(move || {
        let primary_child = Command::new("afplay")
            .arg(primary)
            .spawn()
            .expect("must play primary paid fixture");
        let secondary_child = secondary.map(|path| {
            std::thread::sleep(Duration::from_millis(180));
            Command::new("afplay")
                .arg(path)
                .spawn()
                .expect("must play overlapping paid fixture")
        });
        wait_for_child_with_timeout(primary_child, Duration::from_secs(25), "primary afplay");
        if let Some(child) = secondary_child {
            wait_for_child_with_timeout(child, Duration::from_secs(25), "secondary afplay");
        }
    })
    .await
    .expect("paid scenario playback worker must not panic");
}

#[tokio::test]
#[ignore = "requires macOS Screen & System Audio permission and audible system output"]
async fn isolated_realtime_capture_emits_24khz_mono_and_stops_callbacks() {
    let fixture = generate_system_audio_fixture();
    let factory = DefaultPlatformAudioFactory::new();
    let target = AudioCaptureTarget::incoming_realtime_translation();
    let request = SystemAudioCaptureRequest::isolated(target);
    factory
        .preflight_system_audio_capture(request)
        .expect("isolated ScreenCaptureKit preflight must pass before network connect");
    let mut capture = factory
        .create_system_audio_capture(request)
        .expect("isolated realtime capture must be created");
    capture
        .initialize(AudioConfig {
            sample_rate: target.sample_rate,
            channels: target.channels,
            buffer_size: 720,
        })
        .await
        .expect("realtime capture target must initialize");

    let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::unbounded_channel::<AudioChunk>();
    capture
        .start_capture(Arc::new(move |chunk| {
            let _ = chunk_tx.send(chunk);
        }))
        .await
        .expect("isolated ScreenCaptureKit capture must start");
    let mut player = Command::new("afplay")
        .arg(fixture.path())
        .spawn()
        .expect("must run afplay");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut chunks = Vec::new();
    while tokio::time::Instant::now() < deadline && chunks.len() < 4 {
        if let Ok(Some(chunk)) =
            tokio::time::timeout(Duration::from_millis(500), chunk_rx.recv()).await
        {
            chunks.push(chunk);
        }
    }
    let _ = player.wait();
    capture
        .stop_capture()
        .await
        .expect("isolated capture must stop");

    assert!(
        !chunks.is_empty(),
        "system audio produced no capture chunks"
    );
    assert!(chunks.iter().all(|chunk| {
        chunk.sample_rate == target.sample_rate
            && chunk.channels == target.channels
            && chunk.data.len() == 720
    }));
    while chunk_rx.try_recv().is_ok() {}
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(
        chunk_rx.try_recv().is_err(),
        "capture emitted a callback after stop"
    );
    assert!(!capture.is_capturing());
}

#[tokio::test]
#[ignore = "requires macOS Screen & System Audio permission and audible system output"]
async fn system_default_playback_is_drained_and_excluded_from_system_capture() {
    const EXTERNAL_TONE_HZ: f64 = 440.0;
    const SAME_PROCESS_TONE_HZ: f64 = 880.0;
    let tone_duration = Duration::from_secs(4);
    let external_fixture = generate_tone_fixture(EXTERNAL_TONE_HZ, tone_duration);
    let target = AudioCaptureTarget::incoming_realtime_translation();

    let capture_factory = DefaultPlatformAudioFactory::new();
    let request = SystemAudioCaptureRequest::isolated(target);
    capture_factory
        .preflight_system_audio_capture(request)
        .expect("isolated capture preflight must pass");
    let mut capture = capture_factory
        .create_system_audio_capture(request)
        .expect("isolated capture must be created");
    capture
        .initialize(AudioConfig {
            sample_rate: target.sample_rate,
            channels: target.channels,
            buffer_size: 720,
        })
        .await
        .expect("isolated capture must initialize");
    let captured = Arc::new(Mutex::new(Vec::<i16>::new()));
    capture
        .start_capture({
            let captured = captured.clone();
            Arc::new(move |chunk| {
                captured.lock().unwrap().extend_from_slice(&chunk.data);
            })
        })
        .await
        .expect("isolated capture must start");

    let playback_factory = DefaultLocalPlaybackOutputFactory::new();
    let mut output = playback_factory
        .create_local_playback_output(LocalPlaybackRoute::SystemDefault)
        .expect("system default local playback must be available on macOS");
    output
        .open(TranslationAudioOutputConfig::openai_translation().with_gain(1.0))
        .await
        .expect("system default local playback must open");
    let same_process_tone = generate_tone(target.sample_rate, SAME_PROCESS_TONE_HZ, tone_duration);
    let outcome = output
        .enqueue_pcm16(&same_process_tone)
        .await
        .expect("same-process tone must enqueue");
    assert!(matches!(outcome, AudioEnqueueOutcome::Queued { .. }));
    let mut external_player = Command::new("afplay")
        .arg(external_fixture.path())
        .spawn()
        .expect("must run external afplay tone");
    let status = external_player.wait().expect("must wait for afplay tone");
    assert!(status.success(), "external afplay tone failed");

    output.begin_drain_mode();
    output
        .prepare_for_drain()
        .expect("local playback must prepare for drain");
    let drain_deadline = tokio::time::Instant::now() + Duration::from_secs(6);
    while output.pending_playback_duration() > Duration::from_millis(30)
        && tokio::time::Instant::now() < drain_deadline
    {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        output.pending_playback_duration() <= Duration::from_millis(30),
        "system default playback did not drain"
    );
    assert!(output.is_open(), "local playback stream failed during tone");
    output.close().await.expect("local playback must close");
    capture.stop_capture().await.expect("capture must stop");

    let captured = captured.lock().unwrap().clone();
    let skip = (target.sample_rate / 4) as usize;
    let analyzed = captured.get(skip..).unwrap_or(&captured);
    let external_power = goertzel_power(analyzed, target.sample_rate, EXTERNAL_TONE_HZ);
    let same_process_power = goertzel_power(analyzed, target.sample_rate, SAME_PROCESS_TONE_HZ);
    assert!(
        external_power > 1.0e12,
        "external 440 Hz tone was not captured"
    );
    assert!(
        same_process_power < external_power * 0.05,
        "same-process 880 Hz leaked into capture: external={external_power:e}, self={same_process_power:e}"
    );
}

#[tokio::test]
#[ignore = "paid/manual: requires macOS permission, audible output, VOICETEXT_RUN_PAID_E2E=1, and a dedicated OPENAI_E2E_API_KEY"]
async fn incoming_spoken_translation_returns_realtime_text_and_audio_from_system_capture() {
    let api_key = load_paid_e2e_api_key();
    let artifact_root = paid_artifact_root();
    fs::create_dir_all(&artifact_root).expect("must create paid E2E artifact root");
    let scenario_filter =
        std::env::var("INCOMING_SPOKEN_E2E_SCENARIO").unwrap_or_else(|_| "all".into());
    let scenarios: Vec<_> = paid_spoken_scenarios()
        .into_iter()
        .filter(|scenario| scenario_filter == "all" || scenario_filter == scenario.id)
        .collect();
    assert!(
        !scenarios.is_empty(),
        "unknown INCOMING_SPOKEN_E2E_SCENARIO={scenario_filter}"
    );
    let transcription_client = reqwest::Client::new();

    for (index, scenario) in scenarios.into_iter().enumerate() {
        let fixture = generate_spoken_audio_fixture(
            &format!("voicetext_paid_{}", scenario.id),
            scenario.primary_voice,
            scenario.source,
        );
        let secondary_fixture = scenario.secondary_source.map(|source| {
            generate_spoken_audio_fixture(
                &format!("voicetext_paid_{}_secondary", scenario.id),
                "Daniel",
                source,
            )
        });
        let scenario_dir = artifact_root.join(scenario.id);
        fs::create_dir_all(&scenario_dir).expect("must create scenario artifact directory");
        fs::copy(fixture.path(), scenario_dir.join("source-primary.aiff"))
            .expect("must persist primary source audio");
        if let Some(secondary) = secondary_fixture.as_ref() {
            fs::copy(secondary.path(), scenario_dir.join("source-secondary.aiff"))
                .expect("must persist secondary source audio");
        }

        let output_state = Arc::new(PaidSpokenOutputState::default());
        let source_text = Arc::new(Mutex::new(String::new()));
        let translated_text = Arc::new(Mutex::new(String::new()));
        let errors = Arc::new(Mutex::new(Vec::<String>::new()));
        let first_input_ms = Arc::new(Mutex::new(None::<u128>));
        let captured_samples = Arc::new(Mutex::new(Vec::<i16>::new()));
        let first_source_text_ms = Arc::new(Mutex::new(None::<u128>));
        let first_translated_text_ms = Arc::new(Mutex::new(None::<u128>));
        let started_at = Instant::now();
        *output_state.started_at.lock().unwrap() = Some(started_at);
        let service = IncomingTranslationFacade::new_spoken_with_factories(
            Arc::new(ObservedSystemAudioCaptureFactory {
                inner: DefaultPlatformAudioFactory::new(),
                started_at,
                first_input_ms: first_input_ms.clone(),
                captured_samples: captured_samples.clone(),
            }),
            Arc::new(PaidSpokenOutputFactory {
                state: output_state.clone(),
            }),
            Arc::new(OpenAIRealtimeTranslationFactory),
            Arc::new(PaidSpokenReadyCapability),
        );
        let callbacks = IncomingTranslationCallbacks {
            on_source_final: {
                let source_text = source_text.clone();
                let first_source_text_ms = first_source_text_ms.clone();
                Arc::new(move |delta| {
                    first_source_text_ms
                        .lock()
                        .unwrap()
                        .get_or_insert_with(|| started_at.elapsed().as_millis());
                    source_text.lock().unwrap().push_str(&delta);
                })
            },
            on_translation_delta: {
                let translated_text = translated_text.clone();
                let first_translated_text_ms = first_translated_text_ms.clone();
                Arc::new(move |delta| {
                    first_translated_text_ms
                        .lock()
                        .unwrap()
                        .get_or_insert_with(|| started_at.elapsed().as_millis());
                    translated_text.lock().unwrap().push_str(&delta);
                })
            },
            on_error: {
                let errors = errors.clone();
                Arc::new(move |error| errors.lock().unwrap().push(error.to_string()))
            },
            on_status: Arc::new(|_| {}),
        };
        let mut config = IncomingTranslationConfig::new_with_defaults(
            SttConfig::default(),
            30_001 + index as u64,
        );
        config.openai_api_key = api_key.clone();
        config.target_language = "ru".into();
        config.playback_gain = 0.8;

        service
            .start(config, callbacks)
            .await
            .unwrap_or_else(|error| panic!("paid scenario {} must start: {error}", scenario.id));
        tokio::time::sleep(PAID_SOURCE_PREROLL).await;
        let source_playback_started_ms = started_at.elapsed().as_millis();
        play_paid_scenario(
            fixture.path(),
            secondary_fixture.as_ref().map(TempAudioFixture::path),
        )
        .await;
        let output_wait_timeout = if scenario.translation_output_required {
            Duration::from_secs(30)
        } else {
            Duration::from_secs(5)
        };
        let result = tokio::time::timeout(output_wait_timeout, async {
            loop {
                if !translated_text.lock().unwrap().trim().is_empty()
                    && output_state.audible_samples.load(Ordering::Relaxed)
                        >= MIN_AUDIBLE_TRANSLATED_SAMPLES
                {
                    break;
                }
                if !errors.lock().unwrap().is_empty() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await;
        let wait_completed_before_timeout = result.is_ok();
        let stop_result = service.stop().await;
        let stop_error = stop_result.as_ref().err().map(ToString::to_string);

        let source = source_text.lock().unwrap().clone();
        let translated = translated_text.lock().unwrap().clone();
        let translated_samples = output_state.samples.lock().unwrap().clone();
        let captured_input_samples = captured_samples.lock().unwrap().clone();
        let translated_audible_samples = output_state.audible_samples.load(Ordering::Relaxed);
        let captured_audible_samples = audible_sample_count(&captured_input_samples);
        let captured_input_transcription = transcribe_pcm16(
            &transcription_client,
            &api_key,
            24_000,
            1,
            &captured_input_samples,
        )
        .await;
        let captured_input_transcription_error = captured_input_transcription
            .as_ref()
            .err()
            .map(ToString::to_string);
        let captured_input_transcript = captured_input_transcription.unwrap_or_default();
        let terminal_errors = errors.lock().unwrap().clone();
        let source_transcript_available = !source.trim().is_empty();
        let missing_marker_groups =
            missing_translation_marker_groups(&translated, scenario.required_translation_markers);
        let output_received_within_timeout = wait_completed_before_timeout
            && terminal_errors.is_empty()
            && !translated.trim().is_empty()
            && translated_audible_samples >= MIN_AUDIBLE_TRANSLATED_SAMPLES;
        fs::write(scenario_dir.join("source-transcript.txt"), &source)
            .expect("must write source transcript");
        fs::write(scenario_dir.join("translated-transcript.txt"), &translated)
            .expect("must write translated transcript");
        fs::write(
            scenario_dir.join("translated-audio.wav"),
            wav_pcm16(24_000, 1, &translated_samples),
        )
        .expect("must write translated audio");
        fs::write(
            scenario_dir.join("captured-input.wav"),
            wav_pcm16(24_000, 1, &captured_input_samples),
        )
        .expect("must write captured input audio");
        fs::write(
            scenario_dir.join("captured-input-transcript.txt"),
            &captured_input_transcript,
        )
        .expect("must write captured input transcript");
        let metrics = serde_json::json!({
            "scenario": scenario.id,
            "expected_source": scenario.source,
            "expected_secondary_source": scenario.secondary_source,
            "human_reference": scenario.human_reference,
            "source_playback_started_ms": source_playback_started_ms,
            "first_input_ms": *first_input_ms.lock().unwrap(),
            "first_source_text_ms": *first_source_text_ms.lock().unwrap(),
            "first_translated_text_ms": *first_translated_text_ms.lock().unwrap(),
            "first_translated_audio_ms": *output_state.first_audio_ms.lock().unwrap(),
            "source_transcript_available": source_transcript_available,
            "translation_output_required": scenario.translation_output_required,
            "output_wait_timeout_ms": output_wait_timeout.as_millis(),
            "output_received_within_timeout": output_received_within_timeout,
            "captured_input_samples": captured_input_samples.len(),
            "captured_audible_samples": captured_audible_samples,
            "captured_input_transcription_error": captured_input_transcription_error.clone(),
            "translated_audio_samples": translated_samples.len(),
            "translated_audible_samples": translated_audible_samples,
            "missing_translation_marker_groups": missing_marker_groups.clone(),
            "errors": terminal_errors.clone(),
            "stop_error": stop_error,
        });
        fs::write(
            scenario_dir.join("metrics.json"),
            serde_json::to_vec_pretty(&metrics).expect("metrics must serialize"),
        )
        .expect("must write metrics");

        if !source_transcript_available {
            println!(
                "paid scenario {}: optional OpenAI source transcript was not emitted",
                scenario.id
            );
        }
        assert!(
            captured_input_transcription_error.is_none(),
            "scenario {} captured input transcription failed: {:?}",
            scenario.id,
            captured_input_transcription_error
        );
        assert!(
            !captured_input_transcript.trim().is_empty(),
            "scenario {} captured input transcript is empty",
            scenario.id
        );
        assert!(
            terminal_errors.is_empty(),
            "scenario {} errors: {:?}",
            scenario.id,
            terminal_errors
        );
        if scenario.translation_output_required {
            assert!(
                output_received_within_timeout,
                "scenario {} returned no text/audible audio within {} seconds; artifacts: {}",
                scenario.id,
                output_wait_timeout.as_secs(),
                scenario_dir.display()
            );
        }
        if let Err(error) = stop_result {
            panic!("paid scenario {} must stop: {error}", scenario.id);
        }
        if scenario.translation_output_required {
            assert!(
                translated
                    .chars()
                    .any(|character| ('\u{0400}'..='\u{04ff}').contains(&character)),
                "scenario {} expected Russian text, got: {translated}",
                scenario.id
            );
            assert!(
                missing_marker_groups.is_empty(),
                "scenario {} lost required meaning/entities {:?}; translated: {}",
                scenario.id,
                missing_marker_groups,
                translated
            );
            assert!(
                translated_audible_samples >= MIN_AUDIBLE_TRANSLATED_SAMPLES,
                "scenario {} translated PCM contains no meaningful audio",
                scenario.id
            );
        }
        assert!(output_state.opened.load(Ordering::SeqCst));
        assert!(output_state.closed.load(Ordering::SeqCst));
        assert_eq!(*output_state.gain.lock().unwrap(), Some(0.8));
        assert_eq!(service.get_status().await, RecordingStatus::Idle);
    }

    println!("paid_incoming_artifacts={}", artifact_root.display());
}

#[tokio::test]
#[ignore = "paid/manual: requires macOS permission, audible output, VOICETEXT_RUN_PAID_E2E=1, and a dedicated OPENAI_E2E_API_KEY"]
async fn incoming_spoken_translation_paid_stop_mid_phrase_is_bounded() {
    let api_key = load_paid_e2e_api_key();
    let fixture = generate_spoken_audio_fixture(
        "voicetext_paid_stop_mid_phrase",
        "Samantha",
        "This is a deliberately long sentence about a production incident, a certificate renewal, a database migration, and a scheduled release, and the translation session will be stopped before the speaker can finish the complete thought.",
    );
    let output_state = Arc::new(PaidSpokenOutputState::default());
    let started_at = Instant::now();
    *output_state.started_at.lock().unwrap() = Some(started_at);
    let service = IncomingTranslationFacade::new_spoken_with_factories(
        Arc::new(DefaultPlatformAudioFactory::new()),
        Arc::new(PaidSpokenOutputFactory {
            state: output_state.clone(),
        }),
        Arc::new(OpenAIRealtimeTranslationFactory),
        Arc::new(PaidSpokenReadyCapability),
    );
    let callbacks = IncomingTranslationCallbacks {
        on_source_final: Arc::new(|_| {}),
        on_translation_delta: Arc::new(|_| {}),
        on_error: Arc::new(|_| {}),
        on_status: Arc::new(|_| {}),
    };
    let mut config = IncomingTranslationConfig::new_with_defaults(SttConfig::default(), 40_001);
    config.openai_api_key = api_key;
    config.target_language = "ru".into();
    config.playback_gain = 0.8;
    service
        .start(config, callbacks)
        .await
        .expect("paid stop-mid-phrase session must start");

    let mut player = Command::new("afplay")
        .arg(fixture.path())
        .spawn()
        .expect("must play stop-mid-phrase fixture");
    tokio::time::sleep(Duration::from_millis(700)).await;
    let stop_started = Instant::now();
    tokio::time::timeout(Duration::from_secs(8), service.stop())
        .await
        .expect("stop mid phrase must remain bounded")
        .expect("stop mid phrase must cleanly close the realtime session");
    let _ = player.kill();
    let _ = player.wait();

    let samples_after_stop = output_state.samples.lock().unwrap().len();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        output_state.samples.lock().unwrap().len(),
        samples_after_stop,
        "translated audio arrived after stop completed"
    );
    assert!(output_state.closed.load(Ordering::SeqCst));
    assert_eq!(service.get_status().await, RecordingStatus::Idle);

    let artifact_dir = paid_artifact_root().join("stop_mid_phrase");
    fs::create_dir_all(&artifact_dir).expect("must create stop-mid-phrase artifact directory");
    fs::copy(fixture.path(), artifact_dir.join("source-primary.aiff"))
        .expect("must persist stop-mid-phrase source audio");
    fs::write(
        artifact_dir.join("translated-audio.wav"),
        wav_pcm16(24_000, 1, &output_state.samples.lock().unwrap()),
    )
    .expect("must persist stop-mid-phrase translated audio");
    fs::write(
        artifact_dir.join("metrics.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "stop_requested_ms": stop_started.duration_since(started_at).as_millis(),
            "stop_duration_ms": stop_started.elapsed().as_millis(),
            "translated_audio_samples": samples_after_stop,
        }))
        .expect("stop-mid-phrase metrics must serialize"),
    )
    .expect("must persist stop-mid-phrase metrics");
}

#[derive(Default)]
struct OpenAiLoopbackSttState {
    initialized: AtomicBool,
    started: AtomicBool,
    stopped: AtomicBool,
    transcribed: AtomicBool,
    received_audio_chunks: AtomicUsize,
    emitted_finals: AtomicUsize,
}

struct OpenAiLoopbackSttProvider {
    state: Arc<OpenAiLoopbackSttState>,
    api_key: String,
    client: reqwest::Client,
    samples: Vec<i16>,
    sample_rate: u32,
    channels: u16,
    on_final: Option<TranscriptionCallback>,
    cached_transcript: Option<String>,
}

#[async_trait]
impl SttProvider for OpenAiLoopbackSttProvider {
    async fn initialize(&mut self, _config: &SttConfig) -> SttResult<()> {
        self.state.initialized.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn start_stream(
        &mut self,
        _on_partial: TranscriptionCallback,
        on_final: TranscriptionCallback,
        _on_error: ErrorCallback,
        _on_connection_quality: ConnectionQualityCallback,
    ) -> SttResult<()> {
        self.state.started.store(true, Ordering::SeqCst);
        self.on_final = Some(on_final);
        self.samples.clear();
        Ok(())
    }

    async fn send_audio(&mut self, chunk: &AudioChunk) -> SttResult<()> {
        self.state
            .received_audio_chunks
            .fetch_add(1, Ordering::SeqCst);
        self.sample_rate = chunk.sample_rate;
        self.channels = chunk.channels.max(1);
        self.samples.extend_from_slice(&chunk.data);

        if self.samples.len() < TRANSCRIBE_AFTER_SAMPLES {
            return Ok(());
        }

        if self.state.emitted_finals.load(Ordering::SeqCst) >= MAX_TRANSLATED_FINALS_PER_SOAK {
            self.samples.clear();
            return Ok(());
        }

        let samples = std::mem::take(&mut self.samples);
        let transcript = if let Some(transcript) = self.cached_transcript.as_ref() {
            transcript.clone()
        } else {
            let transcript = transcribe_pcm16(
                &self.client,
                &self.api_key,
                self.sample_rate,
                self.channels,
                &samples,
            )
            .await
            .map_err(SttError::Processing)?;
            self.state.transcribed.store(true, Ordering::SeqCst);
            self.cached_transcript = Some(transcript.clone());
            transcript
        };

        if let Some(on_final) = self.on_final.as_ref() {
            let final_index = self.state.emitted_finals.fetch_add(1, Ordering::SeqCst);
            let duration = samples.len() as f64 / self.sample_rate as f64 / self.channels as f64;
            let start = final_index as f64 * (duration + 0.001);
            on_final(Transcription::final_result(transcript).with_timing(start, duration));
        }
        Ok(())
    }

    async fn stop_stream(&mut self) -> SttResult<()> {
        self.state.stopped.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn abort(&mut self) -> SttResult<()> {
        Ok(())
    }

    fn name(&self) -> &str {
        "openai-loopback-stt"
    }

    fn is_online(&self) -> bool {
        true
    }
}

struct OpenAiLoopbackSttFactory {
    state: Arc<OpenAiLoopbackSttState>,
    api_key: String,
}

impl SttProviderFactory for OpenAiLoopbackSttFactory {
    fn create(&self, _config: &SttConfig) -> SttResult<Box<dyn SttProvider>> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|err| {
                SttError::Connection(app_lib::domain::SttConnectionError::simple(err.to_string()))
            })?;

        Ok(Box::new(OpenAiLoopbackSttProvider {
            state: self.state.clone(),
            api_key: self.api_key.clone(),
            client,
            samples: Vec::new(),
            sample_rate: 16_000,
            channels: 1,
            on_final: None,
            cached_transcript: None,
        }))
    }
}

#[tokio::test]
#[ignore = "paid/manual: requires macOS system audio permission, VOICETEXT_RUN_PAID_E2E=1, and a dedicated OPENAI_E2E_API_KEY"]
async fn incoming_translation_service_captures_system_audio_and_emits_translated_text() {
    let api_key = load_paid_e2e_api_key();
    let fixture = generate_system_audio_fixture();
    let stt_state = Arc::new(OpenAiLoopbackSttState::default());
    let service = IncomingTranslationFacade::new_with_factories(
        Arc::new(OpenAiLoopbackSttFactory {
            state: stt_state.clone(),
            api_key: api_key.clone(),
        }),
        Arc::new(DefaultPlatformAudioFactory::new()),
    );

    let statuses = Arc::new(Mutex::new(Vec::<RecordingStatus>::new()));
    let source_text = Arc::new(Mutex::new(String::new()));
    let translated_text = Arc::new(Mutex::new(String::new()));
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let callbacks = IncomingTranslationCallbacks {
        on_source_final: {
            let source_text = source_text.clone();
            Arc::new(move |text| source_text.lock().unwrap().push_str(&text))
        },
        on_translation_delta: {
            let translated_text = translated_text.clone();
            Arc::new(move |text| translated_text.lock().unwrap().push_str(&text))
        },
        on_error: {
            let errors = errors.clone();
            Arc::new(move |err| errors.lock().unwrap().push(err.to_string()))
        },
        on_status: {
            let statuses = statuses.clone();
            Arc::new(move |status| statuses.lock().unwrap().push(status))
        },
    };

    let mut config = IncomingTranslationConfig::new_with_defaults(SttConfig::default(), 10_001);
    config.openai_api_key = api_key;
    config.target_language = "ru".to_string();

    service
        .start(config, callbacks)
        .await
        .expect("incoming translation service must start");
    assert_eq!(service.get_status().await, RecordingStatus::Recording);
    assert!(stt_state.initialized.load(Ordering::SeqCst));
    assert!(stt_state.started.load(Ordering::SeqCst));

    play_system_audio(fixture.path());

    let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
    while tokio::time::Instant::now() < deadline {
        if !translated_text.lock().unwrap().trim().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    service.stop().await.expect("incoming service must stop");

    assert_eq!(service.get_status().await, RecordingStatus::Idle);
    assert!(stt_state.stopped.load(Ordering::SeqCst));
    assert!(
        stt_state.transcribed.load(Ordering::SeqCst),
        "system loopback did not produce enough speech audio for OpenAI transcription"
    );
    assert!(
        errors.lock().unwrap().is_empty(),
        "unexpected incoming translation errors: {:?}",
        errors.lock().unwrap()
    );
    let source_lower = source_text.lock().unwrap().to_lowercase();
    assert!(
        source_lower.contains("call") || source_lower.contains("subtitle"),
        "source final text was not emitted: {:?}",
        source_text.lock().unwrap()
    );
    let translated = translated_text.lock().unwrap().clone();
    assert!(
        translated
            .chars()
            .any(|ch| ('\u{0400}'..='\u{04ff}').contains(&ch)),
        "translated incoming text was not emitted"
    );
    assert!(
        statuses
            .lock()
            .unwrap()
            .contains(&RecordingStatus::Starting)
            && statuses
                .lock()
                .unwrap()
                .contains(&RecordingStatus::Recording),
        "expected Starting and Recording statuses, got {:?}",
        statuses.lock().unwrap()
    );
}

#[tokio::test]
#[ignore = "paid/manual soak: requires macOS system audio permission, VOICETEXT_RUN_PAID_E2E=1, and a dedicated OPENAI_E2E_API_KEY"]
async fn incoming_translation_service_long_running_system_audio_soak() {
    let api_key = load_paid_e2e_api_key();
    let soak_duration = live_audio_soak_duration();
    let fixture = generate_system_audio_fixture();
    let stt_state = Arc::new(OpenAiLoopbackSttState::default());
    let service = IncomingTranslationFacade::new_with_factories(
        Arc::new(OpenAiLoopbackSttFactory {
            state: stt_state.clone(),
            api_key: api_key.clone(),
        }),
        Arc::new(DefaultPlatformAudioFactory::new()),
    );

    let statuses = Arc::new(Mutex::new(Vec::<RecordingStatus>::new()));
    let source_text = Arc::new(Mutex::new(String::new()));
    let translated_text = Arc::new(Mutex::new(String::new()));
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let callbacks = IncomingTranslationCallbacks {
        on_source_final: {
            let source_text = source_text.clone();
            Arc::new(move |text| source_text.lock().unwrap().push_str(&text))
        },
        on_translation_delta: {
            let translated_text = translated_text.clone();
            Arc::new(move |text| translated_text.lock().unwrap().push_str(&text))
        },
        on_error: {
            let errors = errors.clone();
            Arc::new(move |err| errors.lock().unwrap().push(err.to_string()))
        },
        on_status: {
            let statuses = statuses.clone();
            Arc::new(move |status| statuses.lock().unwrap().push(status))
        },
    };

    let mut config = IncomingTranslationConfig::new_with_defaults(SttConfig::default(), 20_001);
    config.openai_api_key = api_key;
    config.target_language = "ru".to_string();

    service
        .start(config, callbacks)
        .await
        .expect("incoming translation service must start long soak");
    assert_eq!(service.get_status().await, RecordingStatus::Recording);

    let keep_playing = Arc::new(AtomicBool::new(true));
    let keep_playing_thread = keep_playing.clone();
    let fixture_thread = fixture.path().to_path_buf();
    let player = std::thread::spawn(move || {
        while keep_playing_thread.load(Ordering::SeqCst) {
            play_system_audio(&fixture_thread);
            std::thread::sleep(Duration::from_millis(250));
        }
    });

    println!(
        "incoming_translation_soak_seconds={}",
        soak_duration.as_secs_f32()
    );
    let deadline = tokio::time::Instant::now() + soak_duration;
    let mut saw_translation = false;
    while tokio::time::Instant::now() < deadline {
        if !translated_text.lock().unwrap().trim().is_empty() {
            saw_translation = true;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    keep_playing.store(false, Ordering::SeqCst);
    join_named_thread(player, "incoming system audio player")
        .expect("incoming system audio player thread must finish cleanly");
    service.stop().await.expect("incoming service must stop");

    assert_eq!(service.get_status().await, RecordingStatus::Idle);
    assert!(stt_state.stopped.load(Ordering::SeqCst));
    assert!(
        stt_state.transcribed.load(Ordering::SeqCst),
        "system loopback did not produce enough speech audio during soak"
    );
    assert!(
        stt_state.received_audio_chunks.load(Ordering::SeqCst) > 1,
        "system loopback did not keep delivering audio during soak"
    );
    let min_expected_finals = if soak_duration >= Duration::from_secs(15) {
        2
    } else {
        1
    };
    assert!(
        stt_state.emitted_finals.load(Ordering::SeqCst) >= min_expected_finals,
        "incoming soak emitted too few translated finals: {}",
        stt_state.emitted_finals.load(Ordering::SeqCst)
    );
    assert!(
        saw_translation,
        "translated incoming text was not emitted during soak"
    );
    assert!(
        errors.lock().unwrap().is_empty(),
        "unexpected incoming translation errors during soak: {:?}",
        errors.lock().unwrap()
    );
    println!(
        "incoming_translation_soak_source_chars={}, translated_chars={}, audio_chunks={}, finals={}",
        source_text.lock().unwrap().len(),
        translated_text.lock().unwrap().len(),
        stt_state.received_audio_chunks.load(Ordering::SeqCst),
        stt_state.emitted_finals.load(Ordering::SeqCst)
    );
}

#[test]
fn join_named_thread_surfaces_background_panic() {
    let handle = std::thread::spawn(|| panic!("simulated afplay failure"));
    let err = join_named_thread(handle, "test-player").unwrap_err();

    assert!(err.contains("test-player thread panicked"));
    assert!(err.contains("simulated afplay failure"));
}
