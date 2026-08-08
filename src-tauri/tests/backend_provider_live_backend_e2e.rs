use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use app_lib::domain::{
    AudioChunk, BackendStreamingProvider, SttConfig, SttError, SttProvider, SttProviderType,
    Transcription,
};
use app_lib::infrastructure::stt::BackendProvider;
use serial_test::serial;
use tokio::sync::mpsc;

const SAMPLE_RATE: u32 = 16_000;
const CHANNELS: u16 = 1;
const FRAME_BYTES: usize = 960;

#[derive(Debug)]
enum TranscriptEvent {
    SettledSegment(String),
    Committed(String),
}

#[tokio::test]
#[serial]
async fn desktop_backend_provider_reaches_live_backend_for_all_streaming_providers() {
    let Ok(backend_url) = std::env::var("VOICETEXT_LIVE_BACKEND_URL") else {
        eprintln!("VOICETEXT_LIVE_BACKEND_URL не задан, пропускаем live desktop-backend e2e");
        return;
    };
    let backend_auth_token = std::env::var("VOICETEXT_LIVE_BACKEND_TOKEN")
        .unwrap_or_else(|_| "dev-local-token".to_string());

    let audio = load_hello_fixture();

    for (provider, language) in [
        (BackendStreamingProvider::Deepgram, "en"),
        (BackendStreamingProvider::ElevenLabs, "en"),
        (BackendStreamingProvider::ElevenLabs, "multi"),
    ] {
        tokio::time::timeout(
            Duration::from_secs(75),
            run_provider_flow(
                &backend_url,
                &backend_auth_token,
                provider,
                language,
                &audio,
            ),
        )
        .await
        .unwrap_or_else(|_| {
            panic!("live desktop-backend e2e timed out for {provider:?}/{language}")
        });
    }
}

#[tokio::test]
#[serial]
async fn desktop_backend_provider_streams_elevenlabs_settled_ru_segment_before_commit() {
    let Ok(backend_url) = std::env::var("VOICETEXT_LIVE_BACKEND_URL") else {
        eprintln!("VOICETEXT_LIVE_BACKEND_URL не задан, пропускаем live segment-final e2e");
        return;
    };
    let backend_auth_token = std::env::var("VOICETEXT_LIVE_BACKEND_TOKEN")
        .unwrap_or_else(|_| "dev-local-token".to_string());
    let audio = load_long_ru_fixture();

    let mut config = SttConfig::new(SttProviderType::Backend);
    config.backend_url = Some(backend_url);
    config.backend_auth_token = Some(backend_auth_token);
    config.backend_streaming_provider = BackendStreamingProvider::ElevenLabs;
    config.language = "ru".to_string();

    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<TranscriptEvent>();
    let (error_tx, mut error_rx) = mpsc::unbounded_channel::<SttError>();
    let mut provider = BackendProvider::new();
    provider
        .initialize(&config)
        .await
        .expect("ElevenLabs RU backend initialization should succeed");

    let partial_event_tx = event_tx.clone();
    let final_event_tx = event_tx;
    provider
        .start_stream(
            Arc::new(move |transcription| {
                if transcription.is_final && !transcription.text.trim().is_empty() {
                    let _ =
                        partial_event_tx.send(TranscriptEvent::SettledSegment(transcription.text));
                }
            }),
            Arc::new(move |transcription| {
                if !transcription.text.trim().is_empty() {
                    let _ = final_event_tx.send(TranscriptEvent::Committed(transcription.text));
                }
            }),
            Arc::new(move |error| {
                let _ = error_tx.send(error);
            }),
            Arc::new(|_, _| {}),
        )
        .await
        .expect("ElevenLabs RU backend stream should start");

    send_audio_fixture_paced(&mut provider, &audio).await;

    let settled_segment = tokio::select! {
        event = event_rx.recv() => match event {
            Some(TranscriptEvent::SettledSegment(text)) => text,
            Some(event) => panic!("expected settled segment before commit, got {event:?}"),
            None => panic!("ElevenLabs RU transcript event channel closed before segment"),
        },
        Some(error) = error_rx.recv() => panic!("ElevenLabs RU provider error: {error}"),
        _ = tokio::time::sleep(Duration::from_secs(15)) => {
            panic!("ElevenLabs RU segment-final timeout before explicit commit");
        },
    };

    provider
        .pause_stream()
        .await
        .expect("ElevenLabs RU explicit commit should succeed");

    let committed = tokio::select! {
        event = event_rx.recv() => match event {
            Some(TranscriptEvent::Committed(text)) => text,
            Some(event) => panic!("expected committed transcript after explicit commit, got {event:?}"),
            None => panic!("ElevenLabs RU transcript event channel closed before commit"),
        },
        Some(error) = error_rx.recv() => panic!("ElevenLabs RU provider error: {error}"),
        _ = tokio::time::sleep(Duration::from_secs(15)) => {
            panic!("ElevenLabs RU committed transcript timeout after explicit commit");
        },
    };

    provider
        .abort()
        .await
        .expect("ElevenLabs RU backend stream should abort cleanly");

    assert!(!settled_segment.trim().is_empty());
    assert!(!committed.trim().is_empty());
    assert!(
        normalize_transcript(&committed) != normalize_transcript(&settled_segment),
        "explicit commit must not repeat the already emitted stable RU segment: segment={settled_segment:?}, committed={committed:?}"
    );
    println!(
        "live ElevenLabs RU segment-final e2e passed: segment={settled_segment:?}, committed={committed:?}"
    );
}

async fn run_provider_flow(
    backend_url: &str,
    backend_auth_token: &str,
    streaming_provider: BackendStreamingProvider,
    language: &str,
    audio: &[u8],
) {
    let mut config = SttConfig::new(SttProviderType::Backend);
    config.backend_url = Some(backend_url.to_string());
    config.backend_auth_token = Some(backend_auth_token.to_string());
    config.backend_streaming_provider = streaming_provider;
    config.language = language.to_string();
    config.keep_connection_alive = true;

    let (quality_tx, mut quality_rx) = mpsc::unbounded_channel::<String>();
    let (partial_tx, mut partial_rx) = mpsc::unbounded_channel::<Transcription>();
    let (final_tx, mut final_rx) = mpsc::unbounded_channel::<Transcription>();
    let (error_tx, mut error_rx) = mpsc::unbounded_channel::<SttError>();
    let (usage_tx, mut usage_rx) = mpsc::unbounded_channel::<(f32, f32)>();

    let mut provider = BackendProvider::new();
    provider.set_usage_callback(Arc::new(move |used, remaining| {
        let _ = usage_tx.send((used, remaining));
    }));
    provider
        .initialize(&config)
        .await
        .unwrap_or_else(|err| panic!("initialize failed for {streaming_provider:?}: {err}"));

    let first_error_tx = error_tx.clone();
    provider
        .start_stream(
            Arc::new(move |t| {
                let _ = partial_tx.send(t);
            }),
            Arc::new(move |t| {
                let _ = final_tx.send(t);
            }),
            Arc::new(move |err| {
                let _ = first_error_tx.send(err);
            }),
            Arc::new(move |quality, _reason| {
                let _ = quality_tx.send(quality);
            }),
        )
        .await
        .unwrap_or_else(|err| panic!("start_stream failed for {streaming_provider:?}: {err}"));

    let quality = recv_or_error(
        &mut quality_rx,
        &mut error_rx,
        Duration::from_secs(8),
        "quality callback",
        streaming_provider,
    )
    .await;
    assert_eq!(quality, "Good");

    send_audio_fixture(&mut provider, streaming_provider, audio).await;

    let first_usage = recv_or_error(
        &mut usage_rx,
        &mut error_rx,
        Duration::from_secs(8),
        "usage callback",
        streaming_provider,
    )
    .await;
    assert!(
        first_usage.0 >= 0.0,
        "usage seconds_used must be non-negative for {streaming_provider:?}: {first_usage:?}"
    );

    provider
        .pause_stream()
        .await
        .unwrap_or_else(|err| panic!("pause_stream failed for {streaming_provider:?}: {err}"));
    assert!(
        provider.is_connection_alive(),
        "backend keep-alive connection should remain alive after pause for {streaming_provider:?}"
    );

    let mut partial_texts = Vec::new();
    let first_final_text = wait_for_final_or_collect_partials(
        &mut final_rx,
        &mut partial_rx,
        &mut error_rx,
        &mut partial_texts,
        streaming_provider,
    )
    .await;

    let (second_partial_tx, mut second_partial_rx) = mpsc::unbounded_channel::<Transcription>();
    let (second_final_tx, mut second_final_rx) = mpsc::unbounded_channel::<Transcription>();
    let second_error_tx = error_tx.clone();
    provider
        .resume_stream(
            Arc::new(move |t| {
                let _ = second_partial_tx.send(t);
            }),
            Arc::new(move |t| {
                let _ = second_final_tx.send(t);
            }),
            Arc::new(move |err| {
                let _ = second_error_tx.send(err);
            }),
            Arc::new(|_, _| {}),
        )
        .await
        .unwrap_or_else(|err| panic!("resume_stream failed for {streaming_provider:?}: {err}"));

    send_audio_fixture(&mut provider, streaming_provider, audio).await;
    provider.pause_stream().await.unwrap_or_else(|err| {
        panic!("second pause_stream failed for {streaming_provider:?}: {err}")
    });
    assert!(
        provider.is_connection_alive(),
        "backend connection should remain alive after second pause for {streaming_provider:?}"
    );

    let mut second_partial_texts = Vec::new();
    let second_final_text = wait_for_final_or_collect_partials(
        &mut second_final_rx,
        &mut second_partial_rx,
        &mut error_rx,
        &mut second_partial_texts,
        streaming_provider,
    )
    .await;

    provider
        .abort()
        .await
        .unwrap_or_else(|err| panic!("abort failed for {streaming_provider:?}: {err}"));

    let first_normalized = first_final_text.to_ascii_lowercase();
    assert!(
        first_normalized.contains("hello"),
        "unexpected first transcript for {streaming_provider:?}: final={first_final_text:?}, partials={partial_texts:?}"
    );
    let second_normalized = second_final_text.to_ascii_lowercase();
    assert!(
        second_normalized.contains("hello"),
        "unexpected resumed transcript for {streaming_provider:?}: final={second_final_text:?}, partials={second_partial_texts:?}"
    );

    println!(
        "live desktop-backend resume e2e passed for {:?}/{}: first={:?}, second={:?}",
        streaming_provider, language, first_final_text, second_final_text
    );
}

async fn send_audio_fixture(
    provider: &mut BackendProvider,
    streaming_provider: BackendStreamingProvider,
    audio: &[u8],
) {
    for frame in audio.chunks(FRAME_BYTES) {
        let chunk = AudioChunk::from_bytes(frame, SAMPLE_RATE, CHANNELS);
        provider
            .send_audio(&chunk)
            .await
            .unwrap_or_else(|err| panic!("send_audio failed for {streaming_provider:?}: {err}"));
    }
}

async fn send_audio_fixture_paced(provider: &mut BackendProvider, audio: &[u8]) {
    for frame in audio.chunks(FRAME_BYTES) {
        let chunk = AudioChunk::from_bytes(frame, SAMPLE_RATE, CHANNELS);
        provider
            .send_audio(&chunk)
            .await
            .expect("paced RU fixture frame should be sent");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

async fn wait_for_final_or_collect_partials(
    final_rx: &mut mpsc::UnboundedReceiver<Transcription>,
    partial_rx: &mut mpsc::UnboundedReceiver<Transcription>,
    error_rx: &mut mpsc::UnboundedReceiver<SttError>,
    partial_texts: &mut Vec<String>,
    streaming_provider: BackendStreamingProvider,
) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);

    loop {
        let now = tokio::time::Instant::now();
        assert!(
            now < deadline,
            "final transcript timeout for {streaming_provider:?}; partials={partial_texts:?}"
        );

        tokio::select! {
            Some(final_result) = final_rx.recv() => {
                if !final_result.text.trim().is_empty() {
                    return final_result.text;
                }
            }
            Some(partial) = partial_rx.recv() => {
                if !partial.text.trim().is_empty() {
                    partial_texts.push(partial.text);
                }
            }
            Some(err) = error_rx.recv() => {
                panic!("provider error for {streaming_provider:?}: {err}");
            }
            _ = tokio::time::sleep_until(deadline) => {
                panic!("final transcript timeout for {streaming_provider:?}; partials={partial_texts:?}");
            }
        }
    }
}

async fn recv_or_error<T>(
    rx: &mut mpsc::UnboundedReceiver<T>,
    error_rx: &mut mpsc::UnboundedReceiver<SttError>,
    timeout: Duration,
    label: &str,
    streaming_provider: BackendStreamingProvider,
) -> T {
    tokio::select! {
        value = rx.recv() => value.unwrap_or_else(|| {
            panic!("{label} channel closed for {streaming_provider:?}")
        }),
        Some(err) = error_rx.recv() => {
            panic!("provider error before {label} for {streaming_provider:?}: {err}");
        }
        _ = tokio::time::sleep(timeout) => {
            panic!("{label} timeout for {streaming_provider:?}");
        }
    }
}

fn load_fixture(name: &str) -> Vec<u8> {
    let fixtures_dir = std::env::var_os("VOICETEXT_BACKEND_FIXTURES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../backend/tests/fixtures")
        });
    let path = fixtures_dir.join(format!("{name}.pcm"));
    std::fs::read(&path).unwrap_or_else(|err| {
        panic!(
            "failed to read backend PCM fixture at {}: {err}",
            path.display()
        )
    })
}

fn load_hello_fixture() -> Vec<u8> {
    load_fixture("hello_en")
}

fn load_long_ru_fixture() -> Vec<u8> {
    let mut audio = load_fixture("hello_ru");
    // A deliberate natural pause verifies that provider VAD emits a stable
    // segment before explicit stop. Without this gap the fixture is continuous
    // speech and no provider can infer a safe utterance boundary.
    audio.extend(vec![0; (SAMPLE_RATE as usize * 650 / 1_000) * 2]);
    audio.extend(load_fixture("numbers"));
    audio
}

fn normalize_transcript(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric())
        .collect()
}
