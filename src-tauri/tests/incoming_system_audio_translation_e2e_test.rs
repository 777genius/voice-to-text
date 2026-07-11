#![cfg(target_os = "macos")]

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
    AudioCaptureTarget, AudioChunk, AudioConfig, AudioEnqueueOutcome, ConnectionQualityCallback,
    ErrorCallback, LocalPlaybackOutputFactory, LocalPlaybackRoute, RecordingStatus, SttConfig,
    SttError, SttProvider, SttProviderFactory, SttResult, SystemAudioCaptureFactory,
    SystemAudioCaptureRequest, Transcription, TranscriptionCallback, TranslationAudioOutputConfig,
};
use app_lib::infrastructure::audio::{
    DefaultLocalPlaybackOutputFactory, DefaultPlatformAudioFactory,
};
use async_trait::async_trait;
use reqwest::header::CONTENT_TYPE;

const OPENAI_TRANSCRIPTIONS_URL: &str = "https://api.openai.com/v1/audio/transcriptions";
const OPENAI_TRANSCRIPTION_MODEL: &str = "gpt-4o-transcribe";
const TRANSCRIBE_AFTER_SAMPLES: usize = 16_000 * 2;
const MAX_TRANSLATED_FINALS_PER_SOAK: usize = 3;
static NEXT_TEMP_AUDIO_ID: AtomicUsize = AtomicUsize::new(0);

fn load_openai_api_key() -> String {
    let _ = dotenv::dotenv();
    std::env::var("OPENAI_API_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
        .expect("OPENAI_API_KEY must be set in src-tauri/.env or environment")
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
    let aiff_path = unique_temp_audio_path("voicetext_incoming_system_audio_source", "aiff");

    let status = Command::new("say")
        .args([
            "-v",
            "Alex",
            "-o",
            aiff_path.to_str().expect("valid aiff path"),
            "Hello from the call. This checks incoming subtitles.",
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

fn wav_pcm16(sample_rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
    let data_len = samples.len() as u32 * 2;
    let byte_rate = sample_rate * channels as u32 * 2;
    let block_align = channels * 2;
    let mut wav = Vec::with_capacity(44 + data_len as usize);

    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        wav.extend_from_slice(&sample.to_le_bytes());
    }

    wav
}

fn multipart_transcription_body(boundary: &str, wav: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();

    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"model\"\r\n\r\n");
    body.extend_from_slice(OPENAI_TRANSCRIPTION_MODEL.as_bytes());
    body.extend_from_slice(b"\r\n");

    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"response_format\"\r\n\r\n");
    body.extend_from_slice(b"text\r\n");

    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"incoming-loopback.wav\"\r\n",
    );
    body.extend_from_slice(b"Content-Type: audio/wav\r\n\r\n");
    body.extend_from_slice(wav);
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());

    body
}

async fn transcribe_with_openai(
    client: &reqwest::Client,
    api_key: &str,
    sample_rate: u32,
    channels: u16,
    samples: &[i16],
) -> SttResult<String> {
    let wav = wav_pcm16(sample_rate, channels, samples);
    let boundary = format!("voicetext-e2e-{}", std::process::id());
    let response = client
        .post(OPENAI_TRANSCRIPTIONS_URL)
        .bearer_auth(api_key)
        .header(
            CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(multipart_transcription_body(&boundary, &wav))
        .send()
        .await
        .map_err(|err| {
            SttError::Connection(app_lib::domain::SttConnectionError::simple(err.to_string()))
        })?;

    let status = response.status();
    let text = response.text().await.map_err(|err| {
        SttError::Connection(app_lib::domain::SttConnectionError::simple(err.to_string()))
    })?;

    if !status.is_success() {
        return Err(match status.as_u16() {
            401 | 403 => SttError::Authentication(text),
            _ => SttError::Connection(app_lib::domain::SttConnectionError::simple(format!(
                "OpenAI transcription HTTP {}: {}",
                status.as_u16(),
                text
            ))),
        });
    }

    let transcript = text.trim().to_string();
    if transcript.is_empty() {
        return Err(SttError::Processing(
            "OpenAI transcription returned empty text".to_string(),
        ));
    }

    Ok(transcript)
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
            let transcript = transcribe_with_openai(
                &self.client,
                &self.api_key,
                self.sample_rate,
                self.channels,
                &samples,
            )
            .await?;
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
#[ignore = "requires macOS Screen & System Audio permission, system audio output, and OpenAI transcription/translation APIs"]
async fn incoming_translation_service_captures_system_audio_and_emits_translated_text() {
    let api_key = load_openai_api_key();
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
#[ignore = "long soak: requires macOS Screen & System Audio permission, system audio output, and OpenAI APIs"]
async fn incoming_translation_service_long_running_system_audio_soak() {
    let api_key = load_openai_api_key();
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
