// Headless composed tests: real service and BackendProvider, manually driven
// AudioCapture, and a loopback WebSocket peer that controls ElevenLabs Ready.
use crate::infrastructure::stt::BackendProvider;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::{error::ProtocolError, Error as WsError, Message};

struct ReadyBackendFactory;
impl SttProviderFactory for ReadyBackendFactory {
    fn create(&self, _: &SttConfig) -> SttResult<Box<dyn SttProvider>> {
        Ok(Box::new(BackendProvider::new()))
    }
}

fn ready_test_config(url: String) -> SttConfig {
    let mut config = SttConfig::new(SttProviderType::Backend);
    config.backend_url = Some(url);
    config.backend_auth_token = Some("loopback-test".into());
    config.backend_streaming_provider = crate::domain::BackendStreamingProvider::ElevenLabs;
    config
}

async fn ready_test_capture(
    service: &TranscriptionService,
    callback: &Arc<std::sync::Mutex<Option<crate::domain::AudioChunkCallback>>>,
    run_id: u64,
    config: SttConfig,
) -> (PreparedCaptureToken, crate::domain::AudioChunkCallback) {
    let token = service
        .prepare_recording_capture(
            run_id,
            config,
            Arc::new(|_, _| {}),
            Arc::new(|_, _| {}),
            Arc::new(|_| {}),
        )
        .await
        .unwrap();
    let capture = callback.lock().unwrap().clone().expect("capture callback");
    (token, capture)
}

async fn ready_test_connect(
    service: Arc<TranscriptionService>,
    token: PreparedCaptureToken,
    cancelled: Arc<AtomicBool>,
    final_text: Arc<std::sync::Mutex<Vec<String>>>,
    on_ready: impl Fn(String, Option<String>) + Send + Sync + 'static,
) -> Result<()> {
    service
        .connect_prepared_recording(
            token,
            Arc::new(|_| {}),
            Arc::new(move |result| final_text.lock().unwrap().push(result.text)),
            Arc::new(|_, _| {}),
            Arc::new(|_, _| {}),
            Arc::new(|_| {}),
            Arc::new(on_ready),
            cancelled,
        )
        .await
}

async fn ready_test_peer(
    listener: tokio::net::TcpListener,
    config_seen: tokio::sync::oneshot::Sender<()>,
    release_ready: tokio::sync::oneshot::Receiver<()>,
) -> (Vec<Vec<u8>>, usize, usize) {
    let (socket, _) = listener.accept().await.unwrap();
    let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
    let Message::Text(config) = ws.next().await.unwrap().unwrap() else {
        panic!("Config must precede audio")
    };
    let config: serde_json::Value = serde_json::from_str(&config).unwrap();
    assert_eq!(config["type"], "config");
    assert_eq!(config["provider"], "elevenlabs");
    config_seen.send(()).unwrap();
    release_ready.await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(60), ws.next())
            .await
            .is_err(),
        "no audio or control frame may precede Ready"
    );
    ws.send(Message::Text(
        serde_json::json!({"type":"ready","session_id":"gated",
            "accepted_capabilities":["finalize_outcome_v1"]})
        .to_string()
        .into(),
    ))
    .await
    .unwrap();
    let mut frames = Vec::new();
    let mut finalizes = 0;
    let mut closes = 0;
    loop {
        match ws.next().await.unwrap().unwrap() {
            Message::Binary(bytes) => {
                assert!(finalizes == 0, "audio cannot follow Finalize");
                assert!(bytes.len() <= 9600, "ElevenLabs wire frame cap");
                frames.push(bytes.to_vec());
                ws.send(Message::Text(
                    serde_json::json!({"type":"ack","seq":frames.len()})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            }
            Message::Text(text) => {
                let message: serde_json::Value = serde_json::from_str(&text).unwrap();
                match message["type"].as_str().unwrap() {
                    "finalize" => {
                        finalizes += 1;
                        assert_eq!(finalizes, 1);
                        for _ in 0..2 {
                            ws.send(Message::Text(
                                serde_json::json!({"type":"stable","delivery_seq":1,
                                    "text":"captured speech"})
                                .to_string()
                                .into(),
                            ))
                            .await
                            .unwrap();
                        }
                        ws.send(Message::Text(
                            serde_json::json!({"type":"finalize_complete","status":"drained",
                                "saw_result":true,"outcome":{"reason":"drained",
                                "tail_evidence":"segment_observed","provider_release":"released",
                                "last_delivery_seq":1,"stable_snapshot":"captured speech"}})
                            .to_string()
                            .into(),
                        ))
                        .await
                        .unwrap();
                    }
                    "close" => closes += 1,
                    "keepalive" => {}
                    other => panic!("unexpected control: {other}"),
                }
            }
            Message::Close(_) => break,
            Message::Ping(payload) => ws.send(Message::Pong(payload)).await.unwrap(),
            other => panic!("unexpected frame: {other:?}"),
        }
    }
    (frames, finalizes, closes)
}

#[tokio::test]
async fn composed_el_ready_fifo_live_and_sealed_stop_preserve_exact_pcm() {
    // Logical 0/4/8 seconds of source PCM are injected while the peer withholds
    // Ready. This avoids an 8-second wall-clock delay in the headless suite.
    for (case, seconds, stop_before_ready) in [(0, 0, false), (1, 4, false), (2, 8, true)] {
        tokio::time::timeout(Duration::from_secs(18), async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let config = ready_test_config(format!("ws://{}", listener.local_addr().unwrap()));
            let (config_tx, config_rx) = tokio::sync::oneshot::channel();
            let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
            let peer = tokio::spawn(ready_test_peer(listener, config_tx, ready_rx));
            let callback = Arc::new(std::sync::Mutex::new(None));
            let service = Arc::new(TranscriptionService::new(
                Box::new(ManualAudioCapture::new(callback.clone())),
                Arc::new(ReadyBackendFactory),
            ));
            service.set_microphone_sensitivity(100).await;
            service.update_config(config.clone()).await.unwrap();
            let run_id = 801 + case;
            let (token, capture) = ready_test_capture(&service, &callback, run_id, config).await;
            let final_text = Arc::new(std::sync::Mutex::new(Vec::new()));
            let connect = tokio::spawn(ready_test_connect(
                service.clone(),
                token,
                Arc::new(AtomicBool::new(false)),
                final_text.clone(),
                |_, _| {},
            ));
            config_rx.await.unwrap();
            let mut expected = Vec::new();
            for index in 0..seconds * 20 {
                let sample = 1000 + index as i16;
                capture(AudioChunk::new(vec![sample; 800], 16_000, 1));
                expected.extend(sample.to_le_bytes().repeat(800));
            }
            if stop_before_ready {
                service.stop_capture_for_run(run_id).await.unwrap();
            }
            ready_tx.send(()).unwrap();
            connect.await.unwrap().unwrap();
            if !stop_before_ready {
                let live = vec![1700; 800];
                capture(AudioChunk::new(live.clone(), 16_000, 1));
                expected.extend(1700_i16.to_le_bytes().repeat(800));
                service.stop_capture_for_run(run_id).await.unwrap();
            }
            service.finalize_provider_for_run(run_id).await.unwrap();
            let (frames, finalizes, closes) = peer.await.unwrap();
            assert_eq!((finalizes, closes), (1, 1));
            assert_eq!(frames.concat(), expected, "case {case}: FIFO/ACK bytes");
            if seconds > 0 {
                assert!(
                    frames.len() < seconds * 20 / 4,
                    "case {case}: queued tiny chunks must coalesce"
                );
            }
            assert_eq!(final_text.lock().unwrap().as_slice(), ["captured speech"]);
            let report = service.completed_report_for_run(run_id).await.unwrap();
            assert_eq!(report.audio.accepted_bytes, expected.len() as u64);
            assert_eq!(report.audio.submitted_bytes, expected.len() as u64);
            assert!(!report.audio.is_incomplete());
            assert!(service.stt_provider.read().await.is_none());
            assert!(!service.audio_capture.read().await.is_capturing());
        })
        .await
        .expect("composed Ready/Finalize deadline");
    }
}

#[tokio::test]
async fn composed_ready_callback_cancellation_fences_audio_before_start_commits() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = ready_test_config(format!("ws://{}", listener.local_addr().unwrap()));
        let peer = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
            assert!(matches!(
                ws.next().await.unwrap().unwrap(),
                Message::Text(_)
            ));
            ws.send(Message::Text(
                serde_json::json!({"type":"ready","session_id":"cancel-at-ready",
                    "accepted_capabilities":["finalize_outcome_v1"]})
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
            while let Some(frame) = ws.next().await {
                match frame {
                    Ok(Message::Binary(_)) => panic!("cancelled run sent stale PCM"),
                    Ok(Message::Close(_)) => break,
                    Ok(Message::Ping(payload)) => ws.send(Message::Pong(payload)).await.unwrap(),
                    Err(WsError::Protocol(ProtocolError::ResetWithoutClosingHandshake)) => break,
                    Err(WsError::Io(error))
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::UnexpectedEof
                        ) =>
                    {
                        break
                    }
                    Err(error) => panic!("unexpected abort peer error: {error}"),
                    _ => {}
                }
            }
        });
        let callback = Arc::new(std::sync::Mutex::new(None));
        let service = Arc::new(TranscriptionService::new(
            Box::new(ManualAudioCapture::new(callback.clone())),
            Arc::new(ReadyBackendFactory),
        ));
        service.update_config(config.clone()).await.unwrap();
        let (token, capture) = ready_test_capture(&service, &callback, 901, config).await;
        capture(AudioChunk::new(vec![1234; 800], 16_000, 1));
        let cancelled = Arc::new(AtomicBool::new(false));
        let mark_cancelled = cancelled.clone();
        let result = ready_test_connect(
            service.clone(),
            token,
            cancelled,
            Arc::new(std::sync::Mutex::new(Vec::new())),
            move |quality, detail| {
                if quality == "Good" && detail.is_none() {
                    mark_cancelled.store(true, Ordering::Release);
                }
            },
        )
        .await;
        assert!(result
            .unwrap_err()
            .downcast_ref::<PreparedCaptureCancelled>()
            .is_some());
        peer.await.unwrap();
        assert!(!service.audio_capture.read().await.is_capturing());
        assert!(service.stt_provider.read().await.is_none());
        assert_eq!(service.logical_provider_run_id(), 0);
    })
    .await
    .expect("cancel-at-Ready cleanup deadline");
}

#[tokio::test]
async fn composed_cancel_before_ready_fences_stale_capture_from_fresh_run() {
    tokio::time::timeout(Duration::from_secs(6), async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = ready_test_config(format!("ws://{}", listener.local_addr().unwrap()));
        let (first_config_tx, first_config_rx) = tokio::sync::oneshot::channel();
        let peer = tokio::spawn(async move {
            let (old_socket, _) = listener.accept().await.unwrap();
            let mut old_ws = tokio_tungstenite::accept_async(old_socket).await.unwrap();
            assert!(matches!(old_ws.next().await.unwrap().unwrap(), Message::Text(_)));
            first_config_tx.send(()).unwrap();
            while let Some(frame) = old_ws.next().await {
                match frame {
                    Ok(Message::Binary(_)) => panic!("cancelled pre-Ready run sent PCM"),
                    Ok(Message::Close(_)) => break,
                    Ok(Message::Ping(payload)) => old_ws.send(Message::Pong(payload)).await.unwrap(),
                    Err(WsError::Protocol(ProtocolError::ResetWithoutClosingHandshake)) => break,
                    Err(WsError::Io(error)) if matches!(error.kind(),
                        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::UnexpectedEof) => break,
                    Err(error) => panic!("unexpected abort peer error: {error}"),
                    _ => {}
                }
            }
            let (new_socket, _) = listener.accept().await.unwrap();
            let mut new_ws = tokio_tungstenite::accept_async(new_socket).await.unwrap();
            assert!(matches!(new_ws.next().await.unwrap().unwrap(), Message::Text(_)));
            new_ws.send(Message::Text(
                serde_json::json!({"type":"ready","session_id":"fresh",
                    "accepted_capabilities":["finalize_outcome_v1"]})
                    .to_string().into(),
            )).await.unwrap();
            let mut pcm = Vec::new();
            let mut frames = 0;
            let mut finalizes = 0;
            let mut closes = 0;
            while let Some(frame) = new_ws.next().await {
                match frame.unwrap() {
                    Message::Binary(bytes) => {
                        frames += 1;
                        pcm.extend_from_slice(&bytes);
                        new_ws.send(Message::Text(
                            serde_json::json!({"type":"ack","seq":frames}).to_string().into(),
                        )).await.unwrap();
                    }
                    Message::Text(text) => {
                        let message: serde_json::Value = serde_json::from_str(&text).unwrap();
                        match message["type"].as_str().unwrap() {
                            "finalize" => {
                                finalizes += 1;
                                new_ws.send(Message::Text(
                                    serde_json::json!({"type":"finalize_complete","status":"drained",
                                        "saw_result":false,"outcome":{"reason":"drained",
                                        "tail_evidence":"unconfirmed","provider_release":"released",
                                        "last_delivery_seq":0,"stable_snapshot":""}})
                                        .to_string().into(),
                                )).await.unwrap();
                            }
                            "close" => closes += 1,
                            "keepalive" => {},
                            other => panic!("unexpected fresh-run control: {other}"),
                        }
                    }
                    Message::Close(_) => break,
                    Message::Ping(payload) => new_ws.send(Message::Pong(payload)).await.unwrap(),
                    other => panic!("unexpected frame: {other:?}"),
                }
            }
            assert_eq!((finalizes, closes), (1, 1));
            pcm
        });
        let callback = Arc::new(std::sync::Mutex::new(None));
        let service = Arc::new(TranscriptionService::new(
            Box::new(ManualAudioCapture::new(callback.clone())),
            Arc::new(ReadyBackendFactory),
        ));
        service.update_config(config.clone()).await.unwrap();
        let (old_token, old_capture) = ready_test_capture(&service, &callback, 911, config.clone()).await;
        old_capture(AudioChunk::new(vec![1111; 800], 16_000, 1));
        let cancelled = Arc::new(AtomicBool::new(false));
        let old_connect = tokio::spawn(ready_test_connect(service.clone(), old_token,
            cancelled.clone(), Arc::new(std::sync::Mutex::new(Vec::new())), |_, _| {}));
        first_config_rx.await.unwrap();
        cancelled.store(true, Ordering::Release);
        assert!(old_connect.await.unwrap().unwrap_err().downcast_ref::<PreparedCaptureCancelled>().is_some());
        assert!(service.stt_provider.read().await.is_none());
        let (new_token, new_capture) = ready_test_capture(&service, &callback, 912, config).await;
        // A caller can still own an old Arc callback, but its generation must be sealed.
        old_capture(AudioChunk::new(vec![9999; 800], 16_000, 1));
        new_capture(AudioChunk::new(vec![2222; 800], 16_000, 1));
        ready_test_connect(service.clone(), new_token, Arc::new(AtomicBool::new(false)),
            Arc::new(std::sync::Mutex::new(Vec::new())), |_, _| {}).await.unwrap();
        service.stop_capture_for_run(912).await.unwrap();
        service.finalize_provider_for_run(912).await.unwrap();
        assert_eq!(peer.await.unwrap(), 2222_i16.to_le_bytes().repeat(800));
        assert!(!service.audio_capture.read().await.is_capturing());
        assert!(service.stt_provider.read().await.is_none());
    }).await.expect("cancel/fresh-run deadline");
}
