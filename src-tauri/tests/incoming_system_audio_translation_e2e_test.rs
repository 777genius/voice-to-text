#![cfg(target_os = "macos")]

mod paid_e2e_support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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
use paid_e2e_support::{
    load_paid_e2e_api_key, transcribe_pcm16, transcribe_pcm16_with_fallbacks, wav_pcm16,
};
const TRANSCRIBE_AFTER_SAMPLES: usize = 16_000 * 2;
const MAX_TRANSLATED_FINALS_PER_SOAK: usize = 7;
const MIN_TRANSLATED_FINALS_PER_RELEASE_SOAK: usize = 6;
const AUDIBLE_SAMPLE_ABS_THRESHOLD: u16 = 128;
const MIN_AUDIBLE_TRANSLATED_SAMPLES: usize = 1_200;
const PAID_SOURCE_PREROLL: Duration = Duration::from_secs(1);
const PAID_TRANSLATION_PLAYBACK_GAIN: f32 = 1.0;
const PAID_REQUIRED_SCENARIO_IDS: &[&str] = &[
    "english_to_russian",
    "names_and_numbers",
    "technical_terms",
    "mixed_english_russian",
    "already_russian",
    "long_context",
    "pause_and_silence",
    "overlapping_speakers",
    "half_volume_source",
];
const PAID_REQUIRED_AUDIO_SCENARIO_IDS: &[&str] = &[
    "english_to_russian",
    "names_and_numbers",
    "technical_terms",
    "long_context",
    "pause_and_silence",
    "overlapping_speakers",
    "half_volume_source",
];
const PAID_DIAGNOSTIC_OUTPUT_SCENARIO_IDS: &[&str] = &["mixed_english_russian", "already_russian"];
const PAID_DIAGNOSTIC_SEMANTIC_SCENARIO_IDS: &[&str] = &["overlapping_speakers"];
const PAID_DIAGNOSTIC_AUDIO_FACT_POLICIES: &[(&str, &str)] =
    &[("names_and_numbers", "meeting time 3:45 PM")];
static NEXT_TEMP_AUDIO_ID: AtomicUsize = AtomicUsize::new(0);

fn audible_sample_count(samples: &[i16]) -> usize {
    samples
        .iter()
        .filter(|sample| sample.unsigned_abs() >= AUDIBLE_SAMPLE_ABS_THRESHOLD)
        .count()
}

fn longest_internal_activity_gap_ms(
    activity_ms: &[u128],
    playback_started_ms: u128,
    playback_finished_ms: u128,
) -> Option<u128> {
    let activity = activity_ms
        .iter()
        .copied()
        .filter(|timestamp| *timestamp >= playback_started_ms && *timestamp <= playback_finished_ms)
        .collect::<Vec<_>>();
    activity
        .windows(2)
        .map(|window| window[1].saturating_sub(window[0]))
        .max()
}

fn live_audio_soak_duration() -> Duration {
    std::env::var("LIVE_AUDIO_SOAK_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(30 * 60))
}

fn paid_source_playback_gain(configured_gain: f32) -> f32 {
    let local_cap = std::env::var("INCOMING_SPOKEN_E2E_SOURCE_GAIN_CAP")
        .ok()
        .and_then(|value| value.parse::<f32>().ok())
        .filter(|gain| gain.is_finite() && *gain > 0.0)
        .map(|gain| gain.clamp(0.05, 1.0));
    local_cap.map_or(configured_gain, |cap| configured_gain.min(cap))
}

fn soak_activity_near_end_grace(soak_duration: Duration) -> Duration {
    Duration::from_secs_f64((soak_duration.as_secs_f64() * 0.05).clamp(5.0, 90.0))
}

fn soak_final_interval(soak_duration: Duration) -> Duration {
    let active_span = soak_duration.saturating_sub(soak_activity_near_end_grace(soak_duration));
    active_span.div_f64((MAX_TRANSLATED_FINALS_PER_SOAK - 1) as f64)
}

fn current_process_rss_kib() -> Option<u64> {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    std::str::from_utf8(&output.stdout)
        .ok()?
        .trim()
        .parse()
        .ok()
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
    audio_activity: Mutex<Vec<Instant>>,
    dropped_batches: AtomicUsize,
    dropped_audio_micros: AtomicU64,
    pending_high_water_micros: AtomicU64,
    pending_at_close_micros: AtomicU64,
    begin_drain_calls: AtomicUsize,
    drain_prepare_pending_micros: Mutex<Vec<u64>>,
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
        let audible_samples = audible_sample_count(samples);
        if audible_samples > 0 {
            let mut first_audio_ms = self.state.first_audio_ms.lock().unwrap();
            if first_audio_ms.is_none() {
                *first_audio_ms = self
                    .state
                    .started_at
                    .lock()
                    .unwrap()
                    .map(|started_at| started_at.elapsed().as_millis());
            }
        }
        self.state
            .samples
            .lock()
            .unwrap()
            .extend_from_slice(samples);
        self.state
            .audible_samples
            .fetch_add(audible_samples, Ordering::Relaxed);
        if audible_samples > 0 {
            self.state
                .audio_activity
                .lock()
                .unwrap()
                .push(Instant::now());
        }
        let pending_micros = self
            .delegate
            .pending_playback_duration()
            .as_micros()
            .min(u64::MAX as u128) as u64;
        self.state
            .pending_high_water_micros
            .fetch_max(pending_micros, Ordering::Relaxed);
        if let AudioEnqueueOutcome::DroppedOldest { duration, .. } = &outcome {
            self.state.dropped_batches.fetch_add(1, Ordering::Relaxed);
            self.state.dropped_audio_micros.fetch_add(
                duration.as_micros().min(u64::MAX as u128) as u64,
                Ordering::Relaxed,
            );
        }
        Ok(outcome)
    }

    async fn close(&mut self) -> TranslationAudioOutputResult<()> {
        self.state.pending_at_close_micros.store(
            self.delegate
                .pending_playback_duration()
                .as_micros()
                .min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
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
        self.state.begin_drain_calls.fetch_add(1, Ordering::Relaxed);
        self.delegate.begin_drain_mode();
    }

    fn prepare_for_drain(&self) -> TranslationAudioOutputResult<Duration> {
        let pending = self.delegate.prepare_for_drain()?;
        self.state
            .drain_prepare_pending_micros
            .lock()
            .unwrap()
            .push(pending.as_micros().min(u64::MAX as u128) as u64);
        Ok(pending)
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
    audible_activity_ms: Arc<Mutex<Vec<u128>>>,
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
            audible_activity_ms: self.audible_activity_ms.clone(),
            captured_samples: self.captured_samples.clone(),
        }))
    }
}

struct ObservedSystemAudioCapture {
    inner: Box<dyn AudioCapture>,
    started_at: Instant,
    first_input_ms: Arc<Mutex<Option<u128>>>,
    audible_activity_ms: Arc<Mutex<Vec<u128>>>,
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
        let audible_activity_ms = self.audible_activity_ms.clone();
        let captured_samples = self.captured_samples.clone();
        self.inner
            .start_capture(Arc::new(move |chunk| {
                if audible_sample_count(&chunk.data) > 0 {
                    let elapsed_ms = started_at.elapsed().as_millis();
                    first_input_ms.lock().unwrap().get_or_insert(elapsed_ms);
                    audible_activity_ms.lock().unwrap().push(elapsed_ms);
                }
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
struct RequiredSemanticFact {
    label: &'static str,
    marker_groups: &'static [&'static [&'static str]],
}

#[derive(Clone, Copy)]
struct PaidSpokenScenario {
    id: &'static str,
    primary_voice: &'static str,
    source: &'static str,
    source_playback_gain: f32,
    secondary_source: Option<&'static str>,
    human_reference: &'static str,
    required_source_markers: &'static [&'static [&'static str]],
    required_translation_facts: &'static [RequiredSemanticFact],
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

fn missing_semantic_facts(
    translated: &str,
    required_facts: &[RequiredSemanticFact],
) -> Vec<String> {
    let normalized = translated.to_lowercase();
    let clauses = normalized
        .split(['.', '!', '?', ';', '\n'])
        .map(str::trim)
        .filter(|clause| !clause.is_empty())
        .collect::<Vec<_>>();

    required_facts
        .iter()
        .filter(|fact| {
            !clauses.iter().any(|clause| {
                fact.marker_groups
                    .iter()
                    .all(|group| group.iter().any(|marker| clause.contains(marker)))
            })
        })
        .map(|fact| fact.label.to_string())
        .collect()
}

fn canonicalize_spoken_semantic_terms(transcript: &str) -> String {
    fn flush_token(output: &mut String, token: &mut String) {
        if token.eq_ignore_ascii_case("pcn") {
            output.push_str("pcm");
        } else {
            output.push_str(token);
        }
        token.clear();
    }

    let mut canonical = String::with_capacity(transcript.len());
    let mut token = String::new();
    for character in transcript.chars() {
        if character.is_alphanumeric() {
            token.push(character);
        } else {
            flush_token(&mut canonical, &mut token);
            canonical.push(character);
        }
    }
    flush_token(&mut canonical, &mut token);
    canonical
}

fn missing_spoken_semantic_facts(
    transcript: &str,
    required_facts: &[RequiredSemanticFact],
) -> Vec<String> {
    missing_semantic_facts(
        &canonicalize_spoken_semantic_terms(transcript),
        required_facts,
    )
}

fn is_diagnostic_audio_fact(scenario_id: &str, fact_label: &str) -> bool {
    PAID_DIAGNOSTIC_AUDIO_FACT_POLICIES
        .iter()
        .any(|(policy_scenario_id, policy_fact_label)| {
            *policy_scenario_id == scenario_id && *policy_fact_label == fact_label
        })
}

fn paid_spoken_scenarios() -> Vec<PaidSpokenScenario> {
    vec![
        PaidSpokenScenario {
            id: "english_to_russian",
            primary_voice: "Samantha",
            source: "Hello everyone on this Zoom call. Please translate this sentence into Russian.",
            source_playback_gain: 1.0,
            secondary_source: None,
            human_reference:
                "Всем привет на этом звонке в Zoom. Пожалуйста, переведите это предложение на русский язык.",
            required_source_markers: &[
                &["hello"],
                &["everyone"],
                &["zoom"],
                &["call"],
                &["translate"],
            ],
            required_translation_facts: &[
                RequiredSemanticFact {
                    label: "greeting",
                    marker_groups: &[&["привет", "здравств"]],
                },
                RequiredSemanticFact {
                    label: "call context",
                    marker_groups: &[&[
                        "звон",
                        "созвон",
                        "разговор",
                        "связ",
                        "лини",
                        "вызов",
                        "конференц",
                        "бесед",
                        "эфир",
                        "zoom",
                    ]],
                },
                RequiredSemanticFact {
                    label: "translation request",
                    marker_groups: &[&["перевед", "перевод"]],
                },
            ],
            translation_output_required: true,
        },
        PaidSpokenScenario {
            id: "names_and_numbers",
            primary_voice: "Samantha",
            source: "My name is Robert Brown. This is a business meeting. The meeting date is October 21st. The meeting time is 3:45 PM. The room number is 207.",
            source_playback_gain: 1.0,
            secondary_source: None,
            human_reference: "Меня зовут Роберт Браун. Это деловая встреча 21 октября в 15:45, комната 207.",
            required_source_markers: &[
                &["robert"],
                &["october"],
                &["21", "twenty first"],
                &["3:45", "three forty five"],
                &["207", "two hundred seven"],
            ],
            required_translation_facts: &[
                RequiredSemanticFact {
                    label: "Robert Brown",
                    marker_groups: &[&["роберт"], &["браун"]],
                },
                RequiredSemanticFact {
                    label: "business meeting",
                    marker_groups: &[&["делов", "рабоч"], &["встреч", "совещ"]],
                },
                RequiredSemanticFact {
                    label: "October 21",
                    marker_groups: &[&["октябр"], &["21", "двадцать перв"]],
                },
                RequiredSemanticFact {
                    label: "meeting time 3:45 PM",
                    marker_groups: &[&[
                        "3:45",
                        "15:45",
                        "три сорок пять",
                        "пятнадцать сорок пять",
                    ]],
                },
                RequiredSemanticFact {
                    label: "room 207",
                    marker_groups: &[
                        &["комнат", "кабинет", "зал", "аудитори"],
                        &["207", "два ноль семь", "двести семь"],
                    ],
                },
            ],
            translation_output_required: true,
        },
        PaidSpokenScenario {
            id: "technical_terms",
            primary_voice: "Samantha",
            source: "If the WebSocket connection drops, the client reconnects automatically using exponential backoff. The system uses bounded queues. The audio format is 24 kilohertz PCM.",
            source_playback_gain: 1.0,
            secondary_source: None,
            human_reference: "Если соединение WebSocket прерывается, клиент автоматически переподключается с экспоненциальной задержкой. Система использует ограниченные очереди. Формат аудио - PCM 24 килогерца.",
            required_source_markers: &[
                &["websocket", "web socket"],
                &["drop"],
                &["reconnect"],
                &["exponential"],
                &["queue"],
                &["24", "twenty four"],
                &["pcm"],
            ],
            required_translation_facts: &[
                RequiredSemanticFact {
                    label: "WebSocket reconnect with exponential backoff",
                    marker_groups: &[
                        &["websocket", "веб-сокет", "вебсокет"],
                        &["переподключ", "повторн", "reconnect"],
                        &["экспоненц", "exponential"],
                    ],
                },
                RequiredSemanticFact {
                    label: "bounded queues",
                    marker_groups: &[&["огранич", "лимит"], &["очеред", "буфер"]],
                },
                RequiredSemanticFact {
                    label: "24 kHz PCM",
                    marker_groups: &[&["24", "двадцать четыре"], &["pcm"]],
                },
            ],
            translation_output_required: true,
        },
        PaidSpokenScenario {
            id: "mixed_english_russian",
            primary_voice: "Milena",
            source: "Please open настройки and choose режим text and audio for this call.",
            source_playback_gain: 1.0,
            secondary_source: None,
            human_reference: "Пожалуйста, откройте настройки и выберите для этого звонка режим текста и аудио.",
            required_source_markers: &[
                &["настрой"],
                &["режим"],
                &["text", "текст"],
                &["audio", "аудио"],
            ],
            required_translation_facts: &[
                RequiredSemanticFact {
                    label: "open settings",
                    marker_groups: &[
                        &["открой", "открыть", "перейд", "зайд", "пойдем", "пойти"],
                        &["настрой"],
                    ],
                },
                RequiredSemanticFact {
                    label: "choose text and audio mode",
                    marker_groups: &[
                        &["выбер", "выбрать"],
                        &["режим"],
                        &["текст"],
                        &["аудио", "звук"],
                    ],
                },
            ],
            translation_output_required: false,
        },
        PaidSpokenScenario {
            id: "already_russian",
            primary_voice: "Milena",
            source: "Добрый день. Проверяем, что русская речь остается понятной и не искажается.",
            source_playback_gain: 1.0,
            secondary_source: None,
            human_reference:
                "Добрый день. Проверяем, что русская речь остается понятной и не искажается.",
            required_source_markers: &[&["добрый"], &["русск"], &["понят"]],
            required_translation_facts: &[
                RequiredSemanticFact {
                    label: "greeting",
                    marker_groups: &[&["добрый день"]],
                },
                RequiredSemanticFact {
                    label: "Russian speech remains understandable",
                    marker_groups: &[&["русск"], &["понят"]],
                },
                RequiredSemanticFact {
                    label: "speech is not distorted",
                    marker_groups: &[&["не"], &["искаж"]],
                },
            ],
            translation_output_required: false,
        },
        PaidSpokenScenario {
            id: "long_context",
            primary_voice: "Samantha",
            source: "During yesterday's incident, the first software deployment to production failed because the certificate expired. After the certificate was renewed, the second software deployment to production succeeded, so do not stop the database migration.",
            source_playback_gain: 1.0,
            secondary_source: None,
            human_reference: "Во время вчерашнего инцидента первое развертывание не удалось из-за истекшего сертификата. После обновления сертификата второе развертывание прошло успешно, поэтому не останавливайте миграцию базы данных.",
            required_source_markers: &[
                &["first"],
                &["software"],
                &["deployment"],
                &["production"],
                &["certificate"],
                &["expired"],
                &["second"],
                &["succeeded", "successful"],
                &["not stop", "do not stop"],
                &["migration"],
            ],
            required_translation_facts: &[
                RequiredSemanticFact {
                    label: "first deployment failed",
                    marker_groups: &[
                        &["перв"],
                        &[
                            "развертыв",
                            "развёртыв",
                            "деплой",
                            "запуск",
                            "поставк",
                            "релиз",
                            "выклад",
                            "выкат",
                            "установ",
                            "размещ",
                            "внедрен",
                            "внедрён",
                            "развертк",
                            "развёртк",
                        ],
                        &[
                            "не удалось",
                            "не сработ",
                            "провал",
                            "ошиб",
                            "неуспеш",
                            "сорвал",
                            "завал",
                            "неудач",
                            "сбо",
                        ],
                    ],
                },
                RequiredSemanticFact {
                    label: "certificate expired",
                    marker_groups: &[&["сертификат"], &["истек", "истёк", "просроч"]],
                },
                RequiredSemanticFact {
                    label: "certificate renewed",
                    marker_groups: &[
                        &["сертификат", "после его", "после продл"],
                        &["обнов", "продл", "возобнов", "замен"],
                    ],
                },
                RequiredSemanticFact {
                    label: "second deployment succeeded",
                    marker_groups: &[
                        &["втор"],
                        &[
                            "развертыв",
                            "развёртыв",
                            "деплой",
                            "попытк",
                            "поставк",
                            "релиз",
                            "выклад",
                            "выкат",
                            "установ",
                            "размещ",
                            "внедрен",
                            "внедрён",
                            "развертк",
                            "развёртк",
                        ],
                        &["успеш", "успех", "уда"],
                    ],
                },
                RequiredSemanticFact {
                    label: "do not stop database migration",
                    marker_groups: &[
                        &["не"],
                        &["останавлив", "прерыва", "прекращ"],
                        &["миграц"],
                    ],
                },
            ],
            translation_output_required: true,
        },
        PaidSpokenScenario {
            id: "pause_and_silence",
            primary_voice: "Samantha",
            source: "The first value is twelve. [[slnc 700]] The second value is forty seven. [[slnc 900]] Keep both values.",
            source_playback_gain: 1.0,
            secondary_source: None,
            human_reference:
                "Первое значение - двенадцать. Второе значение - сорок семь. Сохраните оба значения.",
            required_source_markers: &[
                &["12", "twelve"],
                &["47", "forty seven"],
            ],
            required_translation_facts: &[
                RequiredSemanticFact {
                    label: "first value is 12",
                    marker_groups: &[&["перв"], &["12", "двенадцать"]],
                },
                RequiredSemanticFact {
                    label: "second value is 47",
                    marker_groups: &[&["втор"], &["47", "сорок семь"]],
                },
                RequiredSemanticFact {
                    label: "keep both values",
                    marker_groups: &[&["оба", "обе"], &["значен", "ценност"]],
                },
            ],
            translation_output_required: true,
        },
        PaidSpokenScenario {
            id: "overlapping_speakers",
            primary_voice: "Samantha",
            source: "Alice says the release is scheduled for Friday morning.",
            source_playback_gain: 1.0,
            secondary_source: Some("Bob says the security review must finish before Thursday evening."),
            human_reference: "Алиса говорит, что релиз запланирован на утро пятницы. Боб говорит, что проверка безопасности должна завершиться до вечера четверга.",
            required_source_markers: &[
                &["alice"],
                &["bob"],
                &["friday"],
                &["thursday"],
            ],
            required_translation_facts: &[
                RequiredSemanticFact {
                    label: "Alice associates the release with Friday",
                    marker_groups: &[
                        &["алис", "элис"],
                        &["релиз", "выпуск"],
                        &["пятниц"],
                    ],
                },
                RequiredSemanticFact {
                    label: "Bob associates the security review with Thursday",
                    marker_groups: &[
                        &["боб"],
                        &["безопасн"],
                        &["четверг"],
                    ],
                },
            ],
            translation_output_required: true,
        },
        PaidSpokenScenario {
            id: "half_volume_source",
            primary_voice: "Samantha",
            source: "There are twenty seven open tasks. The meeting starts at nine thirty AM.",
            source_playback_gain: 0.5,
            secondary_source: None,
            human_reference:
                "Открыто двадцать семь задач. Встреча начинается в девять тридцать утра.",
            required_source_markers: &[
                &["27", "twenty seven"],
                &["9:30", "nine thirty"],
            ],
            required_translation_facts: &[
                RequiredSemanticFact {
                    label: "27 open tasks",
                    marker_groups: &[
                        &["27", "двадцать семь"],
                        &["задач"],
                        &["открыт", "незаверш", "в работе"],
                    ],
                },
                RequiredSemanticFact {
                    label: "meeting starts at 9:30 AM",
                    marker_groups: &[
                        &["встреч", "совещ"],
                        &["9:30", "09:30", "9.30", "09.30", "девять тридцать утра"],
                    ],
                },
            ],
            translation_output_required: true,
        },
    ]
}

#[test]
fn paid_spoken_matrix_keeps_complete_release_scenarios_and_assertions() {
    let scenarios = paid_spoken_scenarios();
    let ids = scenarios
        .iter()
        .map(|scenario| scenario.id)
        .collect::<Vec<_>>();

    assert_eq!(ids, PAID_REQUIRED_SCENARIO_IDS);
    assert_eq!(
        PAID_DIAGNOSTIC_SEMANTIC_SCENARIO_IDS,
        &["overlapping_speakers"]
    );
    assert!(PAID_DIAGNOSTIC_SEMANTIC_SCENARIO_IDS
        .iter()
        .all(|id| PAID_REQUIRED_AUDIO_SCENARIO_IDS.contains(id)));
    assert_eq!(
        PAID_DIAGNOSTIC_AUDIO_FACT_POLICIES,
        &[("names_and_numbers", "meeting time 3:45 PM")]
    );
    assert_eq!(
        PAID_DIAGNOSTIC_OUTPUT_SCENARIO_IDS,
        &["mixed_english_russian", "already_russian"]
    );
    assert!(PAID_DIAGNOSTIC_AUDIO_FACT_POLICIES
        .iter()
        .all(
            |(scenario_id, fact_label)| scenarios.iter().any(|scenario| {
                scenario.id == *scenario_id
                    && scenario
                        .required_translation_facts
                        .iter()
                        .any(|fact| fact.label == *fact_label)
            })
        ));
    assert_eq!(
        scenarios
            .iter()
            .filter(|scenario| scenario.translation_output_required)
            .map(|scenario| scenario.id)
            .collect::<Vec<_>>(),
        PAID_REQUIRED_AUDIO_SCENARIO_IDS
    );
    assert!(scenarios
        .iter()
        .all(|scenario| !scenario.required_source_markers.is_empty()));
    assert!(scenarios
        .iter()
        .all(|scenario| !scenario.required_translation_facts.is_empty()));
    assert!(scenarios.iter().all(|scenario| {
        missing_semantic_facts(
            scenario.human_reference,
            scenario.required_translation_facts,
        )
        .is_empty()
    }));
    assert_eq!(
        scenarios
            .iter()
            .filter(|scenario| !scenario.translation_output_required)
            .map(|scenario| scenario.id)
            .collect::<Vec<_>>(),
        PAID_DIAGNOSTIC_OUTPUT_SCENARIO_IDS
    );
}

#[test]
fn semantic_facts_reject_crossed_speaker_associations_and_inverted_outcomes() {
    let facts = [
        RequiredSemanticFact {
            label: "Alice release Friday",
            marker_groups: &[&["алис"], &["релиз"], &["пятниц"]],
        },
        RequiredSemanticFact {
            label: "Bob review Thursday",
            marker_groups: &[&["боб"], &["провер"], &["четверг"]],
        },
        RequiredSemanticFact {
            label: "first deployment failed",
            marker_groups: &[&["перв"], &["развертыв"], &["не удалось"]],
        },
        RequiredSemanticFact {
            label: "second deployment succeeded",
            marker_groups: &[&["втор"], &["развертыв"], &["успеш"]],
        },
    ];
    let correct = "Алиса говорит, что релиз будет в пятницу. Боб завершит проверку в четверг. Первое развертывание не удалось. Второе развертывание прошло успешно.";
    let inverted = "Алиса говорит, что релиз будет в четверг. Боб завершит проверку в пятницу. Первое развертывание прошло успешно. Второе развертывание не удалось.";

    assert!(missing_semantic_facts(correct, &facts).is_empty());
    assert_eq!(
        missing_semantic_facts(inverted, &facts),
        vec![
            "Alice release Friday",
            "Bob review Thursday",
            "first deployment failed",
            "second deployment succeeded",
        ]
    );
}

#[test]
fn long_context_semantics_accept_contextual_synonyms_without_weakening_negation() {
    let scenario = paid_spoken_scenarios()
        .into_iter()
        .find(|scenario| scenario.id == "long_context")
        .expect("long_context scenario must exist");
    let valid = "Во время вчерашнего инцидента первое развертывание ПО в продакшн завалилось, потому что сертификат истек. После продления второе развертывание ПО в продакшн прошло успешно. Так что не останавливайте миграцию базы данных.";
    let installation_variant = "Во время вчерашнего происшествия первая система установки ПО в продакшн не сработала, потому что сертификат истек. После обновления сертификата вторая установка ПО в продакшн прошла успешно. Так что не останавливайте миграцию базы данных.";
    let wrong_action = valid.replace("не останавливайте", "не откладывайте");

    assert!(missing_semantic_facts(valid, scenario.required_translation_facts).is_empty());
    assert!(
        missing_semantic_facts(installation_variant, scenario.required_translation_facts)
            .is_empty()
    );
    assert_eq!(
        missing_semantic_facts(&wrong_action, scenario.required_translation_facts),
        vec!["do not stop database migration"]
    );
}

#[test]
fn spoken_semantics_canonicalize_only_the_known_pcm_asr_confusion() {
    let facts = [RequiredSemanticFact {
        label: "24 kHz PCM",
        marker_groups: &[&["24"], &["pcm"]],
    }];

    assert_eq!(
        missing_semantic_facts("Аудиоформат 24 кГц PCN.", &facts),
        vec!["24 kHz PCM"]
    );
    assert!(missing_spoken_semantic_facts("Аудиоформат 24 кГц PCN.", &facts).is_empty());
    assert_eq!(
        canonicalize_spoken_semantic_terms("PCNetwork PCN, pcn."),
        "PCNetwork pcm, pcm."
    );
}

#[test]
fn measured_pause_ignores_playback_boundaries_and_finds_internal_gap() {
    let activity = vec![50, 110, 140, 170, 850, 880, 1_100];

    assert_eq!(
        longest_internal_activity_gap_ms(&activity, 100, 900),
        Some(680)
    );
}

#[test]
fn release_soak_schedule_is_bounded_and_reaches_the_final_window() {
    let soak_duration = Duration::from_secs(30 * 60);
    let interval = soak_final_interval(soak_duration);
    let estimated_first_final = Duration::from_secs(2);
    let estimated_last_final =
        estimated_first_final + interval.mul_f64((MAX_TRANSLATED_FINALS_PER_SOAK - 1) as f64);

    assert_eq!(MAX_TRANSLATED_FINALS_PER_SOAK, 7);
    assert_eq!(MIN_TRANSLATED_FINALS_PER_RELEASE_SOAK, 6);
    assert!(estimated_last_final <= soak_duration);
    assert!(
        soak_duration.saturating_sub(estimated_last_final)
            <= soak_activity_near_end_grace(soak_duration)
    );
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

async fn play_paid_scenario(primary: &Path, secondary: Option<&Path>, playback_gain: f32) {
    let primary = primary.to_path_buf();
    let secondary = secondary.map(Path::to_path_buf);
    tokio::task::spawn_blocking(move || {
        let playback_gain = playback_gain.to_string();
        let primary_child = Command::new("afplay")
            .args(["--volume", &playback_gain])
            .arg(primary)
            .spawn()
            .expect("must play primary paid fixture");
        let secondary_child = secondary.map(|path| {
            std::thread::sleep(Duration::from_millis(180));
            Command::new("afplay")
                .args(["--volume", &playback_gain])
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
    let post_stop_fixture = generate_tone_fixture(997.0, Duration::from_secs(1));
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
    let post_stop_player = Command::new("afplay")
        .arg(post_stop_fixture.path())
        .spawn()
        .expect("must stimulate system audio after capture stop");
    let post_stop_callback =
        tokio::time::timeout(Duration::from_millis(750), chunk_rx.recv()).await;
    wait_for_child_with_timeout(post_stop_player, Duration::from_secs(5), "post-stop afplay");
    assert!(
        !matches!(post_stop_callback, Ok(Some(_))),
        "capture emitted a callback after stop while system audio remained active"
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
    let all_scenarios = paid_spoken_scenarios();
    if scenario_filter == "all" {
        let scenario_ids = all_scenarios
            .iter()
            .map(|scenario| scenario.id)
            .collect::<Vec<_>>();
        assert_eq!(
            scenario_ids, PAID_REQUIRED_SCENARIO_IDS,
            "paid matrix must execute the complete release scenario set"
        );
    }
    let scenarios: Vec<_> = all_scenarios
        .into_iter()
        .filter(|scenario| scenario_filter == "all" || scenario_filter == scenario.id)
        .collect();
    assert!(
        !scenarios.is_empty(),
        "unknown INCOMING_SPOKEN_E2E_SCENARIO={scenario_filter}"
    );
    let transcription_client = reqwest::Client::new();
    let mut passed_scenario_ids = Vec::new();
    let mut audio_verified_scenario_ids = Vec::new();
    let mut quality_degraded_scenario_ids = Vec::new();

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
        let audible_activity_ms = Arc::new(Mutex::new(Vec::<u128>::new()));
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
                audible_activity_ms: audible_activity_ms.clone(),
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
        config.playback_gain = PAID_TRANSLATION_PLAYBACK_GAIN;

        service
            .start(config, callbacks)
            .await
            .unwrap_or_else(|error| panic!("paid scenario {} must start: {error}", scenario.id));
        tokio::time::sleep(PAID_SOURCE_PREROLL).await;
        let source_playback_started_ms = started_at.elapsed().as_millis();
        let source_playback_gain = paid_source_playback_gain(scenario.source_playback_gain);
        play_paid_scenario(
            fixture.path(),
            secondary_fixture.as_ref().map(TempAudioFixture::path),
            source_playback_gain,
        )
        .await;
        let source_playback_finished_ms = started_at.elapsed().as_millis();
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
        let stop_started = Instant::now();
        let stop_result = service.stop().await;
        let stop_duration_ms = stop_started.elapsed().as_millis();
        let stop_error = stop_result.as_ref().err().map(ToString::to_string);

        let source = source_text.lock().unwrap().clone();
        let translated = translated_text.lock().unwrap().clone();
        let translated_samples = output_state.samples.lock().unwrap().clone();
        let captured_input_samples = captured_samples.lock().unwrap().clone();
        let translated_audible_samples = output_state.audible_samples.load(Ordering::Relaxed);
        let captured_audible_samples = audible_sample_count(&captured_input_samples);
        let captured_input_transcription = transcribe_pcm16_with_fallbacks(
            &transcription_client,
            &api_key,
            24_000,
            1,
            &captured_input_samples,
            |transcript| {
                !missing_translation_marker_groups(transcript, scenario.required_source_markers)
                    .is_empty()
            },
        )
        .await;
        let captured_input_primary_transcription_error =
            captured_input_transcription.primary_error.clone();
        let captured_input_primary_missing_markers = missing_translation_marker_groups(
            &captured_input_transcription.primary_transcript,
            scenario.required_source_markers,
        );
        let captured_input_segment_fallback_attempted =
            captured_input_transcription.segmented_attempted;
        let captured_input_segment_fallback_error =
            captured_input_transcription.segmented_error.clone();
        let captured_input_gpt_missing_markers = missing_translation_marker_groups(
            &captured_input_transcription.gpt_transcript,
            scenario.required_source_markers,
        );
        let captured_input_mini_fallback_attempted = captured_input_transcription.mini_attempted;
        let captured_input_mini_fallback_error = captured_input_transcription.mini_error.clone();
        let captured_input_whisper_fallback_attempted =
            captured_input_transcription.whisper_attempted;
        let captured_input_whisper_fallback_error =
            captured_input_transcription.whisper_error.clone();
        let captured_input_transcription_error =
            if captured_input_transcription.transcript.is_empty() {
                Some(captured_input_transcription.failure_summary())
            } else {
                None
            };
        let captured_input_transcript = captured_input_transcription.transcript;
        let translated_audio_transcription_attempted =
            translated_audible_samples >= MIN_AUDIBLE_TRANSLATED_SAMPLES;
        let translated_audio_transcription = if translated_audio_transcription_attempted {
            transcribe_pcm16_with_fallbacks(
                &transcription_client,
                &api_key,
                24_000,
                1,
                &translated_samples,
                |transcript| {
                    !missing_spoken_semantic_facts(transcript, scenario.required_translation_facts)
                        .is_empty()
                },
            )
            .await
        } else {
            Default::default()
        };
        let translated_audio_primary_transcription_error =
            translated_audio_transcription.primary_error.clone();
        let translated_audio_primary_missing_facts = if translated_audio_transcription_attempted {
            missing_spoken_semantic_facts(
                &translated_audio_transcription.primary_transcript,
                scenario.required_translation_facts,
            )
        } else {
            Vec::new()
        };
        let translated_audio_segment_fallback_attempted =
            translated_audio_transcription.segmented_attempted;
        let translated_audio_segment_fallback_error =
            translated_audio_transcription.segmented_error.clone();
        let translated_audio_gpt_missing_facts = if translated_audio_transcription_attempted {
            missing_spoken_semantic_facts(
                &translated_audio_transcription.gpt_transcript,
                scenario.required_translation_facts,
            )
        } else {
            Vec::new()
        };
        let translated_audio_mini_fallback_attempted =
            translated_audio_transcription.mini_attempted;
        let translated_audio_mini_fallback_error =
            translated_audio_transcription.mini_error.clone();
        let translated_audio_whisper_fallback_attempted =
            translated_audio_transcription.whisper_attempted;
        let translated_audio_whisper_fallback_error =
            translated_audio_transcription.whisper_error.clone();
        let translated_audio_transcript = translated_audio_transcription.transcript.clone();
        let translated_audio_transcription_error =
            if translated_audio_transcription_attempted && translated_audio_transcript.is_empty() {
                Some(translated_audio_transcription.failure_summary())
            } else {
                None
            };
        let terminal_errors = errors.lock().unwrap().clone();
        let source_transcript_available = !source.trim().is_empty();
        let longest_audible_activity_gap_ms = longest_internal_activity_gap_ms(
            &audible_activity_ms.lock().unwrap(),
            source_playback_started_ms,
            source_playback_finished_ms,
        );
        let audible_activity = audible_activity_ms.lock().unwrap().clone();
        let first_input_during_playback_ms = audible_activity.iter().copied().find(|timestamp| {
            *timestamp >= source_playback_started_ms.saturating_sub(250)
                && *timestamp <= source_playback_finished_ms.saturating_add(2_000)
        });
        let preplayback_audible_activity_count = audible_activity
            .iter()
            .filter(|timestamp| **timestamp < source_playback_started_ms.saturating_sub(250))
            .count();
        let missing_source_marker_groups = missing_translation_marker_groups(
            &captured_input_transcript,
            scenario.required_source_markers,
        );
        let missing_translation_facts =
            missing_semantic_facts(&translated, scenario.required_translation_facts);
        let missing_audio_facts = if translated_audio_transcription_attempted {
            missing_spoken_semantic_facts(
                &translated_audio_transcript,
                scenario.required_translation_facts,
            )
        } else {
            Vec::new()
        };
        let diagnostic_missing_audio_facts = missing_audio_facts
            .iter()
            .filter(|fact| is_diagnostic_audio_fact(scenario.id, fact))
            .cloned()
            .collect::<Vec<_>>();
        let blocking_missing_audio_facts = missing_audio_facts
            .iter()
            .filter(|fact| !is_diagnostic_audio_fact(scenario.id, fact))
            .cloned()
            .collect::<Vec<_>>();
        let semantic_quality_blocking = scenario.translation_output_required
            && !PAID_DIAGNOSTIC_SEMANTIC_SCENARIO_IDS.contains(&scenario.id);
        let semantic_quality_degraded = scenario.translation_output_required
            && ((!semantic_quality_blocking
                && (!missing_translation_facts.is_empty() || !missing_audio_facts.is_empty()))
                || !diagnostic_missing_audio_facts.is_empty());
        let translation_output_emitted = !translated.trim().is_empty()
            || translated_audible_samples >= MIN_AUDIBLE_TRANSLATED_SAMPLES;
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
            scenario_dir.join("translated-audio-transcript.txt"),
            &translated_audio_transcript,
        )
        .expect("must write translated audio transcript");
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
        let mut metrics = serde_json::json!({
            "scenario": scenario.id,
            "expected_source": scenario.source,
            "expected_secondary_source": scenario.secondary_source,
            "source_playback_gain": source_playback_gain,
            "human_reference": scenario.human_reference,
            "source_playback_started_ms": source_playback_started_ms,
            "source_playback_finished_ms": source_playback_finished_ms,
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
            "longest_audible_activity_gap_ms": longest_audible_activity_gap_ms,
            "captured_input_transcription_error": captured_input_transcription_error.clone(),
            "missing_source_marker_groups": missing_source_marker_groups.clone(),
            "translated_audio_samples": translated_samples.len(),
            "translated_audible_samples": translated_audible_samples,
            "playback_dropped_batches": output_state.dropped_batches.load(Ordering::Relaxed),
            "playback_dropped_audio_ms": output_state.dropped_audio_micros.load(Ordering::Relaxed) / 1_000,
            "playback_pending_high_water_ms": output_state.pending_high_water_micros.load(Ordering::Relaxed) / 1_000,
            "playback_pending_at_close_ms": output_state.pending_at_close_micros.load(Ordering::Relaxed) / 1_000,
            "translated_audio_verification_required": scenario.translation_output_required,
            "translated_audio_transcription_attempted": translated_audio_transcription_attempted,
            "translated_audio_transcription_error": translated_audio_transcription_error.clone(),
            "translated_audio_primary_transcription_error": translated_audio_primary_transcription_error.clone(),
            "translated_audio_primary_missing_facts": translated_audio_primary_missing_facts,
            "translated_audio_segment_fallback_attempted": translated_audio_segment_fallback_attempted,
            "translated_audio_segment_fallback_error": translated_audio_segment_fallback_error.clone(),
            "translated_audio_gpt_missing_facts": translated_audio_gpt_missing_facts,
            "translated_audio_whisper_fallback_attempted": translated_audio_whisper_fallback_attempted,
            "translated_audio_whisper_fallback_error": translated_audio_whisper_fallback_error.clone(),
            "missing_translated_audio_facts": missing_audio_facts.clone(),
            "missing_translation_facts": missing_translation_facts.clone(),
            "errors": terminal_errors.clone(),
            "stop_error": stop_error,
        });
        let metrics_object = metrics.as_object_mut().expect("metrics must be an object");
        metrics_object.insert(
            "playback_begin_drain_calls".into(),
            output_state
                .begin_drain_calls
                .load(Ordering::Relaxed)
                .into(),
        );
        metrics_object.insert(
            "first_input_during_playback_ms".into(),
            first_input_during_playback_ms
                .map(|timestamp| timestamp.min(u64::MAX as u128) as u64)
                .into(),
        );
        metrics_object.insert(
            "preplayback_audible_activity_count".into(),
            preplayback_audible_activity_count.into(),
        );
        metrics_object.insert(
            "playback_drain_prepare_pending_ms".into(),
            output_state
                .drain_prepare_pending_micros
                .lock()
                .unwrap()
                .iter()
                .map(|micros| micros / 1_000)
                .collect::<Vec<_>>()
                .into(),
        );
        metrics_object.insert(
            "stop_duration_ms".into(),
            (stop_duration_ms.min(u64::MAX as u128) as u64).into(),
        );
        metrics_object.insert(
            "semantic_quality_policy".into(),
            (if semantic_quality_blocking {
                "blocking"
            } else {
                "diagnostic"
            })
            .into(),
        );
        metrics_object.insert(
            "semantic_quality_degraded".into(),
            semantic_quality_degraded.into(),
        );
        metrics_object.insert(
            "translated_audio_mini_fallback_attempted".into(),
            translated_audio_mini_fallback_attempted.into(),
        );
        metrics_object.insert(
            "translated_audio_mini_fallback_error".into(),
            translated_audio_mini_fallback_error.clone().into(),
        );
        metrics_object.insert(
            "blocking_missing_translated_audio_facts".into(),
            blocking_missing_audio_facts.clone().into(),
        );
        metrics_object.insert(
            "diagnostic_missing_translated_audio_facts".into(),
            diagnostic_missing_audio_facts.clone().into(),
        );
        metrics_object.insert(
            "captured_input_primary_transcription_error".into(),
            captured_input_primary_transcription_error.clone().into(),
        );
        metrics_object.insert(
            "captured_input_primary_missing_markers".into(),
            captured_input_primary_missing_markers.clone().into(),
        );
        metrics_object.insert(
            "captured_input_segment_fallback_attempted".into(),
            captured_input_segment_fallback_attempted.into(),
        );
        metrics_object.insert(
            "captured_input_segment_fallback_error".into(),
            captured_input_segment_fallback_error.clone().into(),
        );
        metrics_object.insert(
            "captured_input_gpt_missing_markers".into(),
            captured_input_gpt_missing_markers.clone().into(),
        );
        metrics_object.insert(
            "captured_input_mini_fallback_attempted".into(),
            captured_input_mini_fallback_attempted.into(),
        );
        metrics_object.insert(
            "captured_input_mini_fallback_error".into(),
            captured_input_mini_fallback_error.clone().into(),
        );
        metrics_object.insert(
            "captured_input_whisper_fallback_attempted".into(),
            captured_input_whisper_fallback_attempted.into(),
        );
        metrics_object.insert(
            "captured_input_whisper_fallback_error".into(),
            captured_input_whisper_fallback_error.clone().into(),
        );
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
            missing_source_marker_groups.is_empty(),
            "scenario {} did not capture required source entities {:?}; independent transcript: {}",
            scenario.id,
            missing_source_marker_groups,
            captured_input_transcript
        );
        let first_input = first_input_during_playback_ms.unwrap_or_else(|| {
            panic!(
                "scenario {} has no audible input during playback window {}..{}ms; first input: {:?}ms",
                scenario.id,
                source_playback_started_ms,
                source_playback_finished_ms,
                *first_input_ms.lock().unwrap()
            )
        });
        if scenario.id == "pause_and_silence" {
            assert!(
                longest_audible_activity_gap_ms.is_some_and(|gap| gap >= 500),
                "pause scenario did not contain a measured internal silence gap >= 500 ms: {:?}",
                longest_audible_activity_gap_ms
            );
        }
        if source_transcript_available {
            let first_source_text = (*first_source_text_ms.lock().unwrap()).unwrap_or_else(|| {
                panic!(
                    "scenario {} source transcript has no timestamp",
                    scenario.id
                )
            });
            assert!(
                first_source_text >= first_input,
                "scenario {} source transcript arrived before audible input",
                scenario.id
            );
        }
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
        if scenario.translation_output_required || translation_output_emitted {
            let first_translated_text =
                (*first_translated_text_ms.lock().unwrap()).unwrap_or_else(|| {
                    panic!("scenario {} has no translated text timestamp", scenario.id)
                });
            let first_translated_audio = (*output_state.first_audio_ms.lock().unwrap())
                .unwrap_or_else(|| {
                    panic!("scenario {} has no translated audio timestamp", scenario.id)
                });
            let output_deadline_ms =
                source_playback_finished_ms.saturating_add(output_wait_timeout.as_millis());
            assert!(
                first_translated_text >= first_input && first_translated_text <= output_deadline_ms,
                "scenario {} translated text latency is outside the measured deadline: input={}ms, text={}ms, deadline={}ms",
                scenario.id,
                first_input,
                first_translated_text,
                output_deadline_ms
            );
            assert!(
                first_translated_audio >= first_input && first_translated_audio <= output_deadline_ms,
                "scenario {} translated audio latency is outside the measured deadline: input={}ms, audio={}ms, deadline={}ms",
                scenario.id,
                first_input,
                first_translated_audio,
                output_deadline_ms
            );
            assert!(
                translated
                    .chars()
                    .any(|character| ('\u{0400}'..='\u{04ff}').contains(&character)),
                "scenario {} expected Russian text, got: {translated}",
                scenario.id
            );
            if semantic_quality_blocking {
                assert!(
                    missing_translation_facts.is_empty(),
                    "scenario {} lost required semantic facts {:?}; translated: {}",
                    scenario.id,
                    missing_translation_facts,
                    translated
                );
            }
            assert!(
                translated_audible_samples >= MIN_AUDIBLE_TRANSLATED_SAMPLES,
                "scenario {} translated PCM contains no meaningful audio",
                scenario.id
            );
            assert!(
                translated_audio_transcription_error.is_none(),
                "scenario {} translated audio transcription failed: {:?}",
                scenario.id,
                translated_audio_transcription_error
            );
            assert!(
                !translated_audio_transcript.trim().is_empty(),
                "scenario {} translated audio transcript is empty",
                scenario.id
            );
            if semantic_quality_blocking {
                assert!(
                    blocking_missing_audio_facts.is_empty(),
                    "scenario {} translated audio lost required semantic facts {:?}; independent audio transcript: {}",
                    scenario.id,
                    blocking_missing_audio_facts,
                    translated_audio_transcript
                );
            }
            if semantic_quality_degraded && !quality_degraded_scenario_ids.contains(&scenario.id) {
                quality_degraded_scenario_ids.push(scenario.id);
            }
            if scenario.translation_output_required {
                audio_verified_scenario_ids.push(scenario.id);
            }
        }
        assert!(output_state.opened.load(Ordering::SeqCst));
        assert!(output_state.closed.load(Ordering::SeqCst));
        assert_eq!(
            output_state.dropped_batches.load(Ordering::Relaxed),
            0,
            "scenario {} dropped translated playback audio",
            scenario.id
        );
        assert!(
            output_state.pending_at_close_micros.load(Ordering::Relaxed) <= 30_000,
            "scenario {} closed with undrained translated playback: {} us",
            scenario.id,
            output_state.pending_at_close_micros.load(Ordering::Relaxed)
        );
        assert_eq!(
            *output_state.gain.lock().unwrap(),
            Some(PAID_TRANSLATION_PLAYBACK_GAIN)
        );
        assert_eq!(service.get_status().await, RecordingStatus::Idle);
        passed_scenario_ids.push(scenario.id);
    }

    if scenario_filter == "all" {
        assert_eq!(passed_scenario_ids, PAID_REQUIRED_SCENARIO_IDS);
        assert_eq!(
            audio_verified_scenario_ids,
            PAID_REQUIRED_AUDIO_SCENARIO_IDS
        );
    }
    let complete_release_matrix = scenario_filter == "all"
        && passed_scenario_ids == PAID_REQUIRED_SCENARIO_IDS
        && audio_verified_scenario_ids == PAID_REQUIRED_AUDIO_SCENARIO_IDS;
    fs::write(
        artifact_root.join("matrix-summary.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "scenario_filter": scenario_filter,
            "required_scenario_ids": PAID_REQUIRED_SCENARIO_IDS,
            "passed_scenario_ids": passed_scenario_ids,
            "required_audio_scenario_ids": PAID_REQUIRED_AUDIO_SCENARIO_IDS,
            "audio_verified_scenario_ids": audio_verified_scenario_ids,
            "diagnostic_output_scenario_ids": PAID_DIAGNOSTIC_OUTPUT_SCENARIO_IDS,
            "diagnostic_semantic_scenario_ids": PAID_DIAGNOSTIC_SEMANTIC_SCENARIO_IDS,
            "diagnostic_audio_fact_policies": PAID_DIAGNOSTIC_AUDIO_FACT_POLICIES,
            "quality_degraded_scenario_ids": quality_degraded_scenario_ids,
            "complete_release_matrix": complete_release_matrix,
        }))
        .expect("matrix summary must serialize"),
    )
    .expect("must write paid matrix summary");

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
    let captured_samples = Arc::new(Mutex::new(Vec::<i16>::new()));
    let first_input_ms = Arc::new(Mutex::new(None::<u128>));
    let audible_activity_ms = Arc::new(Mutex::new(Vec::<u128>::new()));
    let source_text = Arc::new(Mutex::new(String::new()));
    let translated_text = Arc::new(Mutex::new(String::new()));
    let terminal_errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let stop_completed = Arc::new(AtomicBool::new(false));
    let callbacks_after_stop = Arc::new(AtomicUsize::new(0));
    let started_at = Instant::now();
    *output_state.started_at.lock().unwrap() = Some(started_at);
    let service = IncomingTranslationFacade::new_spoken_with_factories(
        Arc::new(ObservedSystemAudioCaptureFactory {
            inner: DefaultPlatformAudioFactory::new(),
            started_at,
            first_input_ms: first_input_ms.clone(),
            audible_activity_ms,
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
            let stop_completed = stop_completed.clone();
            let callbacks_after_stop = callbacks_after_stop.clone();
            Arc::new(move |delta| {
                if stop_completed.load(Ordering::SeqCst) {
                    callbacks_after_stop.fetch_add(1, Ordering::SeqCst);
                }
                source_text.lock().unwrap().push_str(&delta);
            })
        },
        on_translation_delta: {
            let translated_text = translated_text.clone();
            let stop_completed = stop_completed.clone();
            let callbacks_after_stop = callbacks_after_stop.clone();
            Arc::new(move |delta| {
                if stop_completed.load(Ordering::SeqCst) {
                    callbacks_after_stop.fetch_add(1, Ordering::SeqCst);
                }
                translated_text.lock().unwrap().push_str(&delta);
            })
        },
        on_error: {
            let terminal_errors = terminal_errors.clone();
            let stop_completed = stop_completed.clone();
            let callbacks_after_stop = callbacks_after_stop.clone();
            Arc::new(move |error| {
                if stop_completed.load(Ordering::SeqCst) {
                    callbacks_after_stop.fetch_add(1, Ordering::SeqCst);
                }
                terminal_errors.lock().unwrap().push(error.to_string());
            })
        },
        on_status: {
            let stop_completed = stop_completed.clone();
            let callbacks_after_stop = callbacks_after_stop.clone();
            Arc::new(move |_| {
                if stop_completed.load(Ordering::SeqCst) {
                    callbacks_after_stop.fetch_add(1, Ordering::SeqCst);
                }
            })
        },
    };
    let mut config = IncomingTranslationConfig::new_with_defaults(SttConfig::default(), 40_001);
    config.openai_api_key = api_key;
    config.target_language = "ru".into();
    config.playback_gain = PAID_TRANSLATION_PLAYBACK_GAIN;
    service
        .start(config, callbacks)
        .await
        .expect("paid stop-mid-phrase session must start");

    tokio::time::sleep(PAID_SOURCE_PREROLL).await;
    let mut player = Command::new("afplay")
        .arg(fixture.path())
        .spawn()
        .expect("must play stop-mid-phrase fixture");
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    assert!(
        player
            .try_wait()
            .expect("must inspect stop-mid-phrase player")
            .is_none(),
        "source fixture finished before the mid-phrase stop was requested"
    );
    let translated_text_before_stop = translated_text.lock().unwrap().clone();
    let translated_samples_before_stop = output_state.samples.lock().unwrap().len();
    let translated_audible_before_stop = output_state.audible_samples.load(Ordering::Relaxed);
    let stop_started = Instant::now();
    tokio::time::timeout(Duration::from_secs(42), service.stop())
        .await
        .expect("stop mid phrase must remain bounded")
        .expect("stop mid phrase must cleanly close the realtime session");
    let stop_duration = stop_started.elapsed();
    stop_completed.store(true, Ordering::SeqCst);
    let _ = player.kill();
    let _ = player.wait();

    let samples_after_stop = output_state.samples.lock().unwrap().len();
    let translated_after_stop = translated_text.lock().unwrap().clone();
    let captured_after_stop = captured_samples.lock().unwrap().clone();
    let errors_after_stop = terminal_errors.lock().unwrap().clone();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        output_state.samples.lock().unwrap().len(),
        samples_after_stop,
        "translated audio arrived after stop completed"
    );
    assert_eq!(
        translated_text.lock().unwrap().as_str(),
        translated_after_stop,
        "translated text arrived after stop completed"
    );
    assert_eq!(
        callbacks_after_stop.load(Ordering::SeqCst),
        0,
        "callbacks arrived after terminal stop"
    );
    assert!(
        errors_after_stop.is_empty(),
        "stop-mid-phrase terminal errors: {:?}",
        errors_after_stop
    );
    assert!(
        audible_sample_count(&captured_after_stop) >= 16_000,
        "stop-mid-phrase capture did not receive enough source speech"
    );
    assert!(
        translated_after_stop
            .chars()
            .any(|character| ('\u{0400}'..='\u{04ff}').contains(&character)),
        "graceful stop did not preserve a Russian translated text tail: {translated_after_stop}"
    );
    assert!(
        translated_after_stop.len() > translated_text_before_stop.len(),
        "graceful stop emitted no additional translated text tail; before={translated_text_before_stop:?}, after={translated_after_stop:?}"
    );
    assert!(
        samples_after_stop > translated_samples_before_stop,
        "graceful stop emitted no additional translated audio tail"
    );
    assert!(
        output_state.audible_samples.load(Ordering::Relaxed) >= MIN_AUDIBLE_TRANSLATED_SAMPLES,
        "graceful stop did not preserve meaningful translated audio"
    );
    assert!(
        output_state.audible_samples.load(Ordering::Relaxed) > translated_audible_before_stop,
        "graceful stop emitted no additional audible translated tail"
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
        artifact_dir.join("captured-input.wav"),
        wav_pcm16(24_000, 1, &captured_after_stop),
    )
    .expect("must persist stop-mid-phrase captured input");
    fs::write(
        artifact_dir.join("translated-text.txt"),
        &translated_after_stop,
    )
    .expect("must persist stop-mid-phrase translated text");
    fs::write(
        artifact_dir.join("metrics.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "stop_requested_ms": stop_started.duration_since(started_at).as_millis(),
            "stop_duration_ms": stop_duration.as_millis(),
            "translated_audio_samples": samples_after_stop,
            "translated_audio_samples_before_stop": translated_samples_before_stop,
            "translated_audible_samples": output_state.audible_samples.load(Ordering::Relaxed),
            "translated_audible_samples_before_stop": translated_audible_before_stop,
            "captured_input_samples": captured_after_stop.len(),
            "captured_audible_samples": audible_sample_count(&captured_after_stop),
            "first_input_ms": *first_input_ms.lock().unwrap(),
            "first_translated_audio_ms": *output_state.first_audio_ms.lock().unwrap(),
            "source_text": source_text.lock().unwrap().clone(),
            "translated_text_before_stop": translated_text_before_stop,
            "translated_text": translated_after_stop,
            "callbacks_after_stop": callbacks_after_stop.load(Ordering::SeqCst),
            "errors": errors_after_stop,
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
    transcription_requests: AtomicUsize,
    received_audio_chunks: AtomicUsize,
    emitted_finals: AtomicUsize,
    final_activity: Mutex<Vec<Instant>>,
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
    max_finals: usize,
    min_final_interval: Duration,
    last_final_at: Option<Instant>,
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
        self.last_final_at = None;
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

        if self.state.emitted_finals.load(Ordering::SeqCst) >= self.max_finals {
            self.samples.clear();
            return Ok(());
        }

        let now = Instant::now();
        if self
            .last_final_at
            .is_some_and(|last_final| now.duration_since(last_final) < self.min_final_interval)
        {
            self.samples.clear();
            return Ok(());
        }

        let samples = std::mem::take(&mut self.samples);
        let transcript = if let Some(transcript) = self.cached_transcript.as_ref() {
            transcript.clone()
        } else {
            self.state
                .transcription_requests
                .fetch_add(1, Ordering::SeqCst);
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
            self.state.final_activity.lock().unwrap().push(now);
            self.last_final_at = Some(now);
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
    max_finals: usize,
    min_final_interval: Duration,
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
            max_finals: self.max_finals,
            min_final_interval: self.min_final_interval,
            last_final_at: None,
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
            max_finals: 1,
            min_final_interval: Duration::ZERO,
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
    let final_interval = soak_final_interval(soak_duration);
    let service = IncomingTranslationFacade::new_with_factories(
        Arc::new(OpenAiLoopbackSttFactory {
            state: stt_state.clone(),
            api_key: api_key.clone(),
            max_finals: MAX_TRANSLATED_FINALS_PER_SOAK,
            min_final_interval: final_interval,
        }),
        Arc::new(DefaultPlatformAudioFactory::new()),
    );

    let statuses = Arc::new(Mutex::new(Vec::<RecordingStatus>::new()));
    let source_text = Arc::new(Mutex::new(String::new()));
    let translated_text = Arc::new(Mutex::new(String::new()));
    let translated_activity = Arc::new(Mutex::new(Vec::<Instant>::new()));
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let callbacks = IncomingTranslationCallbacks {
        on_source_final: {
            let source_text = source_text.clone();
            Arc::new(move |text| source_text.lock().unwrap().push_str(&text))
        },
        on_translation_delta: {
            let translated_text = translated_text.clone();
            let translated_activity = translated_activity.clone();
            Arc::new(move |text| {
                if !text.trim().is_empty() {
                    translated_activity.lock().unwrap().push(Instant::now());
                }
                translated_text.lock().unwrap().push_str(&text);
            })
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
    let deadline = Instant::now() + soak_duration;
    let mut saw_translation = false;
    while Instant::now() < deadline {
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
    let min_expected_finals = if soak_duration >= Duration::from_secs(30 * 60) {
        MIN_TRANSLATED_FINALS_PER_RELEASE_SOAK
    } else {
        2
    };
    let emitted_finals = stt_state.emitted_finals.load(Ordering::SeqCst);
    assert!(
        emitted_finals >= min_expected_finals,
        "incoming soak emitted too few translated finals: {}",
        emitted_finals
    );
    assert!(
        emitted_finals <= MAX_TRANSLATED_FINALS_PER_SOAK,
        "incoming soak exceeded its bounded translation trigger budget: {emitted_finals}"
    );
    assert_eq!(
        stt_state.transcription_requests.load(Ordering::SeqCst),
        1,
        "incoming soak must reuse one paid source transcription"
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
    let final_activity = stt_state.final_activity.lock().unwrap().clone();
    assert_eq!(final_activity.len(), emitted_finals);
    let max_periodic_gap = final_interval + Duration::from_secs(30);
    assert!(
        final_activity
            .windows(2)
            .all(|window| window[1].duration_since(window[0]) <= max_periodic_gap),
        "incoming soak stopped producing periodic source finals: interval={final_interval:?}, activity={final_activity:?}"
    );
    let near_end_grace = soak_activity_near_end_grace(soak_duration);
    let last_final = *final_activity
        .last()
        .expect("incoming soak must record final activity");
    assert!(
        deadline.saturating_duration_since(last_final) <= near_end_grace,
        "last source final was not near the end of the soak: last_age={:?}, grace={near_end_grace:?}",
        deadline.saturating_duration_since(last_final)
    );
    let last_translation = *translated_activity
        .lock()
        .unwrap()
        .last()
        .expect("incoming soak must translate the final periodic source text");
    assert!(
        deadline.saturating_duration_since(last_translation) <= near_end_grace,
        "last translated text was not near the end of the soak: last_age={:?}, grace={near_end_grace:?}",
        deadline.saturating_duration_since(last_translation)
    );
    println!(
        "incoming_translation_soak_source_chars={}, translated_chars={}, audio_chunks={}, finals={}, transcription_requests={}, last_final_age_ms={}, last_translation_age_ms={}",
        source_text.lock().unwrap().len(),
        translated_text.lock().unwrap().len(),
        stt_state.received_audio_chunks.load(Ordering::SeqCst),
        emitted_finals,
        stt_state.transcription_requests.load(Ordering::SeqCst),
        deadline.saturating_duration_since(last_final).as_millis(),
        deadline.saturating_duration_since(last_translation).as_millis(),
    );
}

#[tokio::test]
#[ignore = "paid/manual native spoken soak: requires unlocked macOS GUI, system audio permission, audible output, VOICETEXT_RUN_PAID_E2E=1, and OPENAI_E2E_API_KEY"]
async fn incoming_spoken_translation_long_running_native_soak() {
    const RELEASE_RSS_GROWTH_LIMIT_KIB: u64 = 32 * 1_024;
    const MAX_PENDING_PLAYBACK_MICROS: u64 = 2_000_000;

    let api_key = load_paid_e2e_api_key();
    let soak_duration = live_audio_soak_duration();
    let fixture = generate_spoken_audio_fixture(
        "voicetext_native_spoken_soak",
        "Samantha",
        "Hello from the long running call. Deployment forty two remains stable, and the translation pipeline is still active.",
    );
    let output_state = Arc::new(PaidSpokenOutputState::default());
    let service = IncomingTranslationFacade::new_spoken_with_factories(
        Arc::new(DefaultPlatformAudioFactory::new()),
        Arc::new(PaidSpokenOutputFactory {
            state: output_state.clone(),
        }),
        Arc::new(OpenAIRealtimeTranslationFactory),
        Arc::new(PaidSpokenReadyCapability),
    );
    let source_text = Arc::new(Mutex::new(String::new()));
    let translated_text = Arc::new(Mutex::new(String::new()));
    let translated_activity = Arc::new(Mutex::new(Vec::<Instant>::new()));
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let callbacks = IncomingTranslationCallbacks {
        on_source_final: {
            let source_text = source_text.clone();
            Arc::new(move |text| source_text.lock().unwrap().push_str(&text))
        },
        on_translation_delta: {
            let translated_text = translated_text.clone();
            let translated_activity = translated_activity.clone();
            Arc::new(move |text| {
                if !text.trim().is_empty() {
                    translated_activity.lock().unwrap().push(Instant::now());
                }
                translated_text.lock().unwrap().push_str(&text);
            })
        },
        on_error: {
            let errors = errors.clone();
            Arc::new(move |error| errors.lock().unwrap().push(error.to_string()))
        },
        on_status: Arc::new(|_| {}),
    };
    let mut config = IncomingTranslationConfig::new_with_defaults(SttConfig::default(), 20_002);
    config.openai_api_key = api_key;
    config.target_language = "ru".into();
    config.playback_gain = PAID_TRANSLATION_PLAYBACK_GAIN;

    service
        .start(config, callbacks)
        .await
        .expect("native spoken soak must start");
    assert_eq!(service.get_status().await, RecordingStatus::Recording);

    let release_grade = soak_duration >= Duration::from_secs(30 * 60);
    let target_plays = if release_grade {
        MAX_TRANSLATED_FINALS_PER_SOAK
    } else {
        2
    };
    let started_at = Instant::now();
    let deadline = started_at + soak_duration;
    let first_play_offset = Duration::from_secs(1).min(soak_duration / 10);
    let near_end_grace = soak_activity_near_end_grace(soak_duration);
    let active_play_span = soak_duration
        .saturating_sub(first_play_offset)
        .saturating_sub(near_end_grace);
    let play_interval = active_play_span.div_f64((target_plays.saturating_sub(1)).max(1) as f64);
    let rss_warmup = Duration::from_secs(10).min(soak_duration / 4);
    let rss_interval = Duration::from_secs(30).min((soak_duration / 4).max(Duration::from_secs(1)));
    let mut next_play_at = started_at + first_play_offset;
    let mut next_rss_at = started_at + rss_warmup;
    let mut play_count = 0usize;
    let mut rss_samples_kib = Vec::new();

    while Instant::now() < deadline {
        let now = Instant::now();
        if play_count < target_plays && now >= next_play_at {
            play_paid_scenario(fixture.path(), None, 1.0).await;
            play_count += 1;
            next_play_at =
                started_at + first_play_offset + play_interval.mul_f64(play_count as f64);
        }
        assert_eq!(service.get_status().await, RecordingStatus::Recording);
        assert!(
            errors.lock().unwrap().is_empty(),
            "native spoken soak failed: {:?}",
            errors.lock().unwrap()
        );
        let now = Instant::now();
        if now >= next_rss_at {
            rss_samples_kib.push(
                current_process_rss_kib()
                    .expect("native spoken soak requires a working ps RSS measurement"),
            );
            next_rss_at = now + rss_interval;
        }
        let remaining = deadline.saturating_duration_since(now);
        tokio::time::sleep(Duration::from_millis(250).min(remaining)).await;
    }

    service.stop().await.expect("native spoken soak must stop");
    let translated_text = translated_text.lock().unwrap().clone();
    let source_text = source_text.lock().unwrap().clone();
    let translated_activity = translated_activity.lock().unwrap().clone();
    let audio_activity = output_state.audio_activity.lock().unwrap().clone();
    let terminal_errors = errors.lock().unwrap().clone();
    let baseline_rss_kib = *rss_samples_kib
        .first()
        .expect("native spoken soak requires at least one RSS sample");
    let max_rss_kib = *rss_samples_kib.iter().max().unwrap();
    let rss_growth_kib = max_rss_kib.saturating_sub(baseline_rss_kib);
    let last_text_age = translated_activity
        .last()
        .map(|last| deadline.saturating_duration_since(*last));
    let last_audio_age = audio_activity
        .last()
        .map(|last| deadline.saturating_duration_since(*last));
    let artifact_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/e2e-artifacts/incoming-spoken-native-soak");
    fs::create_dir_all(&artifact_dir).expect("must create native spoken soak artifact directory");
    fs::write(
        artifact_dir.join("metrics.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "soak_seconds": soak_duration.as_secs(),
            "release_grade": release_grade,
            "source_play_count": play_count,
            "source_text_chars": source_text.len(),
            "translated_text_chars": translated_text.len(),
            "translated_audio_samples": output_state.samples.lock().unwrap().len(),
            "translated_audible_samples": output_state.audible_samples.load(Ordering::Relaxed),
            "playback_dropped_batches": output_state.dropped_batches.load(Ordering::Relaxed),
            "playback_dropped_audio_ms": output_state.dropped_audio_micros.load(Ordering::Relaxed) / 1_000,
            "playback_pending_high_water_ms": output_state.pending_high_water_micros.load(Ordering::Relaxed) / 1_000,
            "playback_pending_at_close_ms": output_state.pending_at_close_micros.load(Ordering::Relaxed) / 1_000,
            "rss_samples_kib": rss_samples_kib,
            "rss_growth_kib": rss_growth_kib,
            "last_translation_text_age_ms": last_text_age.map(|age| age.as_millis()),
            "last_translation_audio_age_ms": last_audio_age.map(|age| age.as_millis()),
            "errors": terminal_errors,
        }))
        .expect("native spoken soak metrics must serialize"),
    )
    .expect("must write native spoken soak metrics");

    assert_eq!(service.get_status().await, RecordingStatus::Idle);
    assert_eq!(play_count, target_plays);
    assert!(output_state.opened.load(Ordering::SeqCst));
    assert!(output_state.closed.load(Ordering::SeqCst));
    assert!(!translated_text.trim().is_empty());
    assert!(output_state.audible_samples.load(Ordering::Relaxed) > 0);
    assert!(terminal_errors.is_empty());
    assert_eq!(output_state.dropped_batches.load(Ordering::Relaxed), 0);
    assert!(
        output_state
            .pending_high_water_micros
            .load(Ordering::Relaxed)
            <= MAX_PENDING_PLAYBACK_MICROS
    );
    assert!(output_state.pending_at_close_micros.load(Ordering::Relaxed) <= 30_000);
    assert!(
        rss_samples_kib.len() >= 2,
        "native spoken soak requires at least two RSS samples: {rss_samples_kib:?}"
    );
    assert!(
        rss_growth_kib <= RELEASE_RSS_GROWTH_LIMIT_KIB,
        "native spoken soak RSS grew by {rss_growth_kib} KiB: {rss_samples_kib:?}"
    );
    let activity_grace = near_end_grace + Duration::from_secs(30);
    assert!(
        last_text_age.is_some_and(|age| age <= activity_grace),
        "native spoken translation text was not active near the end: {last_text_age:?}"
    );
    assert!(
        last_audio_age.is_some_and(|age| age <= activity_grace),
        "native spoken translation audio was not active near the end: {last_audio_age:?}"
    );
}

#[test]
fn join_named_thread_surfaces_background_panic() {
    let handle = std::thread::spawn(|| panic!("simulated afplay failure"));
    let err = join_named_thread(handle, "test-player").unwrap_err();

    assert!(err.contains("test-player thread panicked"));
    assert!(err.contains("simulated afplay failure"));
}
