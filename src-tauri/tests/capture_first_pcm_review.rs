//! Explicit device timing probe. PCM is discarded in the callback, never stored or sent.
use app_lib::domain::{AudioCapture, AudioConfig};
use app_lib::infrastructure::audio::SystemAudioCapture;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[tokio::test]
#[ignore = "requires explicit TEST identity and access to default input device"]
async fn prepared_default_device_first_pcm_latency() {
    assert_eq!(
        std::env::var("VOICETEXT_TEST_IDENTITY").unwrap(),
        "test-elevenlabs-stability-20260906"
    );
    let mut capture = SystemAudioCapture::new().expect("default input device");
    capture.initialize(AudioConfig::default()).await.unwrap();
    let device_name = capture.device_name();
    let mut measured = Vec::new();
    let mut warmup = Vec::new();
    for cycle in 0..53 {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let first = Arc::new(Mutex::new(Some(tx)));
        let started = Instant::now();
        let result = capture
            .start_capture(Arc::new(move |chunk| {
                if chunk.data.is_empty() {
                    return;
                }
                if let Some(tx) = first.lock().unwrap().take() {
                    let _ = tx.send(started.elapsed().as_secs_f64() * 1000.0);
                }
            }))
            .await;
        if let Err(error) = result {
            let _ = capture.stop_capture().await;
            panic!("device start failed in cycle {cycle}: {error}");
        }
        let observed = tokio::time::timeout(Duration::from_secs(3), rx).await;
        capture
            .stop_capture()
            .await
            .expect("device stopped after probe");
        assert!(!capture.is_capturing());
        let elapsed = observed
            .expect("first PCM within 3 seconds")
            .expect("callback alive");
        if cycle < 3 {
            warmup.push(elapsed);
        } else {
            measured.push(elapsed);
        }
    }
    let mut sorted = measured.clone();
    sorted.sort_by(f64::total_cmp);
    let p95 = sorted[47];
    println!(
        "CAPTURE_FIRST_PCM_EVIDENCE {}",
        serde_json::json!({
            "source": "real SystemAudioCapture default input, 3 warmups then 50 start/first-PCM/stop cycles; not full hotkey latency",
            "pcm_stored": false, "provider_created": false, "device_name": device_name,
            "warmup_ms": warmup, "samples_ms": measured, "p95_ms": p95, "max_ms": sorted[49]
        })
    );
    assert!(
        p95 <= 250.0,
        "prepared device first PCM p95 exceeded 250ms: {p95}"
    );
}
