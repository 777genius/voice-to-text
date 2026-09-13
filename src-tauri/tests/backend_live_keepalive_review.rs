//! Explicit opt-in live regression against the disposable local backend only.
use app_lib::domain::{
    AudioChunk, BackendStreamingProvider, SttConfig, SttProvider, SttProviderType, Transcription,
};
use app_lib::infrastructure::stt::BackendProvider;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[tokio::test]
#[ignore = "requires explicit disposable backend and synthetic PCM"]
async fn live_deepgram_two_runs_preserve_keepalive_and_callbacks() {
    assert_eq!(
        std::env::var("VOICETEXT_TEST_IDENTITY").unwrap(),
        "test-elevenlabs-stability-20260906"
    );
    let url = std::env::var("VOICETEXT_BACKEND_URL").unwrap();
    assert_eq!(url, "ws://127.0.0.1:51869/api/v1/transcribe/stream");
    let raw = std::fs::read(std::env::var("VOICETEXT_SYNTHETIC_PCM").unwrap()).unwrap();
    assert_eq!(raw.len(), 788288);
    let samples: Vec<i16> = raw
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let events = Arc::new(Mutex::new(Vec::<(usize, bool, String)>::new()));
    let mut provider = BackendProvider::new();
    let mut config = SttConfig::new(SttProviderType::Backend);
    config.backend_url = Some(
        url.trim_end_matches("/api/v1/transcribe/stream")
            .to_string(),
    );
    config.backend_auth_token = Some("dev-local-token".into());
    config.backend_streaming_provider = BackendStreamingProvider::Deepgram;
    config.language = "ru".into();
    provider.initialize(&config).await.unwrap();
    for run in 0..2 {
        let ep = events.clone();
        let ef = events.clone();
        let er = errors.clone();
        let partial =
            Arc::new(move |t: Transcription| ep.lock().unwrap().push((run, t.is_final, t.text)));
        let final_cb =
            Arc::new(move |t: Transcription| ef.lock().unwrap().push((run, true, t.text)));
        let error = Arc::new(move |e| er.lock().unwrap().push(format!("{e}")));
        let quality = Arc::new(|_, _| {});
        if run == 0 {
            provider
                .start_stream(partial, final_cb, error, quality)
                .await
                .unwrap();
        } else {
            provider
                .resume_stream(partial, final_cb, error, quality)
                .await
                .unwrap();
        }
        assert_eq!(
            provider.preferred_audio_batch_samples(),
            None,
            "EL sender policy leaked into DG"
        );
        let epoch = tokio::time::Instant::now();
        for (i, chunk) in samples.chunks(480).enumerate() {
            tokio::time::sleep_until(epoch + Duration::from_millis(i as u64 * 30)).await;
            provider
                .send_audio(&AudioChunk::new(chunk.to_vec(), 16000, 1))
                .await
                .unwrap();
        }
        provider.pause_stream().await.unwrap();
        assert!(provider.is_connection_alive());
        assert!(
            provider.finalize_evidence().is_none(),
            "EL outcome leaked into DG"
        );
        let progress = provider.audio_delivery_progress().unwrap();
        assert_eq!(progress.sent_bytes, raw.len() as u64);
        assert_eq!(progress.acked_bytes, progress.sent_bytes);
        let snapshot = events.lock().unwrap().clone();
        let text = snapshot
            .iter()
            .filter(|(r, stable, _)| *r == run && *stable)
            .map(|(_, _, s)| s.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            text.contains("книга") && text.contains("самол"),
            "Missing start/tail in run {run}: {text}"
        );
        println!("run={run} bytes={} stable_text={text}", progress.sent_bytes);
        if run == 0 {
            let before = events.lock().unwrap().len();
            tokio::time::sleep(Duration::from_secs(12)).await;
            assert!(
                provider.is_connection_alive(),
                "DG idle keepalive lost connection"
            );
            assert_eq!(
                events.lock().unwrap().len(),
                before,
                "late events after pause terminal"
            );
        }
    }
    provider.abort().await.unwrap();
    assert!(!provider.is_connection_alive());
    assert!(
        errors.lock().unwrap().is_empty(),
        "provider errors: {:?}",
        errors.lock().unwrap()
    );
}
