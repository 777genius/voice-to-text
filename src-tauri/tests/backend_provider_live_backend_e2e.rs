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

#[tokio::test]
#[serial]
async fn desktop_backend_provider_reaches_live_backend_for_all_streaming_providers() {
    let Ok(backend_url) = std::env::var("VOICETEXT_LIVE_BACKEND_URL") else {
        eprintln!("VOICETEXT_LIVE_BACKEND_URL не задан, пропускаем live desktop-backend e2e");
        return;
    };

    let audio = load_hello_fixture();

    for (provider, language) in [
        (BackendStreamingProvider::Deepgram, "en"),
        (BackendStreamingProvider::ElevenLabs, "en"),
        (BackendStreamingProvider::ElevenLabs, "multi"),
    ] {
        tokio::time::timeout(
            Duration::from_secs(75),
            run_provider_flow(&backend_url, provider, language, &audio),
        )
        .await
        .unwrap_or_else(|_| {
            panic!("live desktop-backend e2e timed out for {provider:?}/{language}")
        });
    }
}

async fn run_provider_flow(
    backend_url: &str,
    streaming_provider: BackendStreamingProvider,
    language: &str,
    audio: &[u8],
) {
    let mut config = SttConfig::new(SttProviderType::Backend);
    config.backend_url = Some(backend_url.to_string());
    config.backend_auth_token = Some("dev-local-token".to_string());
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

    provider
        .start_stream(
            Arc::new(move |t| {
                let _ = partial_tx.send(t);
            }),
            Arc::new(move |t| {
                let _ = final_tx.send(t);
            }),
            Arc::new(move |err| {
                let _ = error_tx.send(err);
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

    for frame in audio.chunks(FRAME_BYTES) {
        let chunk = AudioChunk::from_bytes(frame, SAMPLE_RATE, CHANNELS);
        provider
            .send_audio(&chunk)
            .await
            .unwrap_or_else(|err| panic!("send_audio failed for {streaming_provider:?}: {err}"));
    }

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
    let final_text = wait_for_final_or_collect_partials(
        &mut final_rx,
        &mut partial_rx,
        &mut error_rx,
        &mut partial_texts,
        streaming_provider,
    )
    .await;

    provider
        .abort()
        .await
        .unwrap_or_else(|err| panic!("abort failed for {streaming_provider:?}: {err}"));

    let normalized = final_text.to_ascii_lowercase();
    assert!(
        normalized.contains("hello"),
        "unexpected final transcript for {streaming_provider:?}: final={final_text:?}, partials={partial_texts:?}"
    );

    println!(
        "live desktop-backend e2e passed for {:?}/{}: final={:?}, partials={:?}",
        streaming_provider, language, final_text, partial_texts
    );
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

fn load_hello_fixture() -> Vec<u8> {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../backend/tests/fixtures/hello_en.pcm");
    std::fs::read(&path).unwrap_or_else(|err| {
        panic!(
            "failed to read backend hello_en.pcm fixture at {}: {err}",
            path.display()
        )
    })
}
