//! Local-only regression: the legacy peer does not accept accelerated EL delivery.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use app_lib::application::services::{AudioDrainReason, TranscriptionService};
use app_lib::domain::{
    AudioCapture, AudioChunk, AudioChunkCallback, AudioConfig, AudioResult,
    BackendStreamingProvider, SttConfig, SttProvider, SttProviderFactory, SttProviderType,
    SttResult,
};
use app_lib::infrastructure::stt::BackendProvider;
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio_tungstenite::{accept_async, tungstenite::Message};

#[derive(Default)]
struct CaptureState {
    callback: Mutex<Option<AudioChunkCallback>>,
    active: AtomicBool,
}

struct FixtureCapture(Arc<CaptureState>);

#[async_trait]
impl AudioCapture for FixtureCapture {
    async fn initialize(&mut self, _config: AudioConfig) -> AudioResult<()> {
        Ok(())
    }
    async fn start_capture(&mut self, callback: AudioChunkCallback) -> AudioResult<()> {
        *self.0.callback.lock().unwrap() = Some(callback);
        self.0.active.store(true, Ordering::SeqCst);
        Ok(())
    }
    async fn stop_capture(&mut self) -> AudioResult<()> {
        self.0.active.store(false, Ordering::SeqCst);
        Ok(())
    }
    fn is_capturing(&self) -> bool {
        self.0.active.load(Ordering::SeqCst)
    }
    fn config(&self) -> AudioConfig {
        AudioConfig::default()
    }
}

struct BackendFactory;
impl SttProviderFactory for BackendFactory {
    fn create(&self, _config: &SttConfig) -> SttResult<Box<dyn SttProvider>> {
        Ok(Box::new(BackendProvider::new()))
    }
}

#[tokio::test]
async fn legacy_elevenlabs_stop_drains_eight_second_prebuffer_before_finalize() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        // Distinct source samples make omissions, reordering, and replay observable.
        // Exactly 8 seconds; the final capture packet contains the remaining 320 samples.
        let samples: Vec<i16> = (0..128_000)
            .map(|index| ((index * 37 + index / 480) % 20_001) as i16 - 10_000)
            .collect();
        let expected: Vec<u8> = samples.iter().flat_map(|sample| sample.to_le_bytes()).collect();
        let peer_expected = expected.clone();
        let peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(stream).await.unwrap();
            let mut received = Vec::new();
            let mut lengths = Vec::new();
            let mut finalizes = 0;
            let mut saw_close = false;
            while let Some(message) = ws.next().await {
                match message.unwrap() {
                    Message::Text(text) => {
                        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
                        match value["type"].as_str() {
                            Some("config") => {
                                // Deliberately omit capability acceptance to exercise old Render.
                                ws.send(Message::Text(
                                    r#"{"type":"ready","session_id":"legacy-local-test"}"#.into(),
                                )).await.unwrap();
                            }
                            Some("finalize") => {
                                finalizes += 1;
                                assert!(received == peer_expected, "Finalize preceded complete ordered PCM delivery: got {} bytes, expected {}", received.len(), peer_expected.len());
                                ws.send(Message::Text(
                                    r#"{"type":"final","text":"complete local fixture","confidence":1.0,"start_ms":0,"duration_ms":8000}"#.into(),
                                )).await.unwrap();
                                ws.send(Message::Text(
                                    r#"{"type":"finalize_complete","status":"flushed","saw_result":true}"#.into(),
                                )).await.unwrap();
                            }
                            Some("close") => { saw_close = true; break; }
                            _ => {}
                        }
                    }
                    Message::Binary(bytes) => {
                        assert_eq!(finalizes, 0, "PCM replayed after Finalize");
                        lengths.push(bytes.len());
                        received.extend_from_slice(&bytes);
                        ws.send(Message::Text(format!(
                            r#"{{"type":"ack","seq":{}}}"#, lengths.len()
                        ))).await.unwrap();
                    }
                    Message::Ping(bytes) => ws.send(Message::Pong(bytes)).await.unwrap(),
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            assert!(saw_close, "Hard abort replaced normal protocol close");
            assert_eq!(finalizes, 1);
            assert!(received == peer_expected, "Final PCM differs from ordered source fixture");
            assert!(lengths.iter().all(|length| *length <= 960), "Unnegotiated batching changed legacy pacing");
            received.len()
        });

        let capture = Arc::new(CaptureState::default());
        let service = TranscriptionService::new(
            Box::new(FixtureCapture(capture.clone())), Arc::new(BackendFactory),
        );
        let mut config = SttConfig::new(SttProviderType::Backend);
        config.backend_url = Some(format!("ws://{address}"));
        config.backend_auth_token = Some("local-test-token".into());
        config.backend_streaming_provider = BackendStreamingProvider::ElevenLabs;
        config.keep_connection_alive = false;
        let errors = Arc::new(Mutex::new(Vec::new()));
        let capture_errors = errors.clone();
        let token = service.prepare_recording_capture(
            801, config, Arc::new(|_, _| {}), Arc::new(|_, _| {}),
            Arc::new(move |error| capture_errors.lock().unwrap().push(format!("{error}"))),
        ).await.unwrap();
        let emit = capture.callback.lock().unwrap().clone().unwrap();
        for chunk in samples.chunks(480) {
            emit(AudioChunk::new(chunk.to_vec(), 16_000, 1));
        }
        let provider_errors = errors.clone();
        service.connect_prepared_recording(
            token, Arc::new(|_| {}), Arc::new(|_| {}), Arc::new(|_, _| {}),
            Arc::new(|_, _| {}),
            Arc::new(move |error| provider_errors.lock().unwrap().push(format!("{error}"))),
            Arc::new(|_, _| {}), Arc::new(AtomicBool::new(false)),
        ).await.unwrap();
        service.stop_capture_for_run(token.run_id).await.unwrap();
        let started = std::time::Instant::now();
        service.finalize_provider_for_run(token.run_id).await.unwrap();
        assert!(started.elapsed() > Duration::from_millis(2500), "Fixture failed to exercise the former deadline");
        let report = service.completed_report_for_run(token.run_id).await.unwrap();
        assert_eq!(report.audio.reason, AudioDrainReason::Drained);
        assert_eq!(report.audio.accepted_bytes, expected.len() as u64);
        assert_eq!(report.audio.submitted_bytes, expected.len() as u64);
        assert_eq!(report.audio.unknown_bytes, 0);
        assert_eq!(report.audio.remaining_bytes, 0);
        assert!(!report.audio.is_incomplete());
        assert!(report.error.is_none());
        assert!(report.provider.is_none(), "Legacy peer fabricated negotiated outcome");
        assert!(!capture.active.load(Ordering::SeqCst));
        let observed_errors = errors.lock().unwrap().clone();
        assert!(observed_errors.is_empty(), "Unexpected errors: {observed_errors:?}");
        assert_eq!(peer.await.unwrap(), expected.len());
    }).await.expect("Legacy backlog stop exceeded bounded 15 second test deadline");
}
