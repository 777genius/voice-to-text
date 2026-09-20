//! Explicit built-in-only diagnostic. It retains timing/count metadata, never
//! raw audio, and never constructs a provider, transcription service or target.
use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

fn summary_us(values: &[u64]) -> serde_json::Value {
    if values.is_empty() {
        return serde_json::Value::Null;
    }
    let mut values = values.to_vec();
    values.sort_unstable();
    serde_json::json!({ "count": values.len(), "p95": values[((values.len() * 95).div_ceil(100)).saturating_sub(1)], "max": values.last() })
}

#[tokio::test]
#[ignore = "explicit built-in microphone acceptance; requires VOICETEXT_RUN_WARM_BUILTIN_PROBE=1"]
async fn native_builtin_warm_acceptance_probe() {
    assert_eq!(
        std::env::var("VOICETEXT_RUN_WARM_BUILTIN_PROBE").as_deref(),
        Ok("1"),
        "explicit native microphone opt-in is required"
    );
    assert!(
        WarmDictationInput::cpal_is_eligible(None),
        "default input must have CoreAudio built-in transport"
    );
    let owner = WarmDictationInput::new_cpal(None).unwrap();
    let trace = Arc::new(Mutex::new(Vec::<RawObservation>::new()));
    let trace_callback = trace.clone();
    *owner.shared.raw_observer.lock().unwrap() = Some(Arc::new(move |observation| {
        let mut trace = trace_callback.lock().unwrap();
        if trace.len() < 8192 {
            trace.push(observation);
        }
    }));
    let cold_started = Instant::now();
    owner.prewarm().await.unwrap();
    let cold_prewarm_us = cold_started.elapsed().as_micros() as u64;
    let format = owner.effective_format().unwrap();
    // Intentional idle observation interval, not a UI/capture latency workaround.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let idle_before = trace
        .lock()
        .unwrap()
        .iter()
        .filter(|row| row.lease_epoch.is_none())
        .count();
    assert!(idle_before >= 2, "must observe actual idle callbacks");
    let delivered = Arc::new(AtomicU64::new(0));
    let mut episodes = Vec::new();
    let mut attach_times = Vec::new();
    let mut first_pcm_times = Vec::new();
    for cycle in 0..10 {
        for label in ["A", "B"] {
            let run = episodes.len() as u64 + 1;
            let mut lease = owner.lease();
            lease.set_capture_identity(Some(AudioCaptureIdentity {
                run_id: run,
                generation: run,
            }));
            let (first_tx, first_rx) = tokio::sync::oneshot::channel();
            let first_tx = Mutex::new(Some(first_tx));
            let samples = delivered.clone();
            let started = Instant::now();
            lease
                .start_capture(Arc::new(move |chunk| {
                    samples.fetch_add(chunk.data.len() as u64, Ordering::Relaxed);
                    if let Some(sender) = first_tx.lock().unwrap().take() {
                        let _ = sender.send(started.elapsed().as_micros() as u64);
                    }
                }))
                .await
                .unwrap();
            let attach_us = started.elapsed().as_micros() as u64;
            let epoch = lease.lease.as_ref().unwrap().identity.epoch;
            let first_pcm_us = tokio::time::timeout(Duration::from_secs(2), first_rx)
                .await
                .unwrap()
                .unwrap();
            lease.stop_capture().await.unwrap();
            let accepted_before_idle = delivered.load(Ordering::Relaxed);
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert_eq!(
                delivered.load(Ordering::Relaxed),
                accepted_before_idle,
                "idle callback delivered PCM after successful stop"
            );
            let trace = trace.lock().unwrap();
            let omitted: Vec<_> = trace
                .iter()
                .filter(|row| row.lease_epoch == Some(epoch) && !row.admitted)
                .collect();
            let observed_omitted_frames: usize = omitted
                .iter()
                .map(|row| row.interleaved_samples / format.channels as usize)
                .sum();
            episodes.push(serde_json::json!({ "cycle": cycle + 1, "episode": label, "run": run, "attach_us": attach_us,
                "first_pcm_us": first_pcm_us, "boundary_discarded_blocks": omitted.len(),
                "observed_boundary_omission_frames": observed_omitted_frames,
                "observed_boundary_omission_us": observed_omitted_frames as u64 * 1_000_000 / format.sample_rate as u64 }));
            attach_times.push(attach_us);
            first_pcm_times.push(first_pcm_us);
        }
    }
    let opens = owner.shared.native_opens.load(Ordering::Relaxed);
    assert_eq!(opens, 1, "warm episodes reopened the native device");
    owner.shutdown().await.unwrap();
    let trace = trace.lock().unwrap();
    let periods: Vec<_> = trace
        .windows(2)
        .filter(|rows| rows[0].physical_generation == rows[1].physical_generation)
        .map(|rows| rows[1].at.saturating_sub(rows[0].at).as_micros() as u64)
        .collect();
    let idle_blocks = trace
        .iter()
        .filter(|row| row.lease_epoch.is_none() && !row.admitted)
        .count();
    let evidence = serde_json::json!({
        "schema": "warm-built-in-probe-v1", "source_sha": std::env::var("VOICETEXT_WARM_PROBE_SHA").ok(),
        "sample_rate": format.sample_rate, "channels": format.channels, "effective_device": format.effective_name,
        "cold_prewarm_us": cold_prewarm_us, "physical_opens": opens, "lease_count": episodes.len(),
        "callback_period_us": summary_us(&periods), "attach_us": summary_us(&attach_times), "first_pcm_us": summary_us(&first_pcm_times),
        "idle_discarded_blocks": idle_blocks, "idle_delivered_pcm_samples": 0,
        "normalized_delivered_samples": delivered.load(Ordering::Relaxed),
        "slot_contention_blocks": owner.shared.gate.contention_blocks.load(Ordering::Relaxed),
        "trace_truncated": trace.len() == 8192, "episodes": episodes,
        "limits": ["callback-boundary software observations, not ADC/sample-exact privacy", "one boundary callback omitted per lease; no universal hardware loss bound", "no VAD/FIFO/provider/UI acceptance in this probe"]
    });
    println!("WARM_MIC_ACCEPTANCE_JSON={evidence}");
    assert_eq!(
        evidence["slot_contention_blocks"], 0,
        "callback contention loss requires investigation before acceptance"
    );
}
