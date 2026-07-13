mod paid_e2e_support;

use std::future::Future;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use app_lib::application::{
    IncomingTranslationCallbacks, IncomingTranslationConfig, IncomingTranslationFacade,
};
use app_lib::domain::{
    AudioCapture, AudioCaptureTarget, AudioChunk, AudioChunkCallback, AudioConfig,
    AudioEnqueueOutcome, AudioResult, LocalPlaybackOutputFactory, LocalPlaybackRoute,
    RealtimeInputNoiseReduction, RealtimeTranslationConfig, RealtimeTranslationErrorKind,
    RealtimeTranslationEvent, RealtimeTranslationFactory, RealtimeTranslationSession,
    RecordingStatus, SpokenIncomingCapability, SpokenTranslationCapability, SttConfig,
    SystemAudioCaptureFactory, SystemAudioCaptureRequest, TranslationAudioOutput,
    TranslationAudioOutputConfig, TranslationAudioOutputResult,
};
use app_lib::infrastructure::openai::OpenAIRealtimeTranslationClient;
use async_trait::async_trait;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use http::header::AUTHORIZATION;
use http::HeaderValue;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{accept_hdr_async, connect_async, WebSocketStream};

use paid_e2e_support::load_paid_e2e_api_key;

const TEST_CREDENTIAL: &str = "integration-placeholder";
const TEST_TIMEOUT: Duration = Duration::from_secs(2);
const TRANSLATED_OUTPUT_SAMPLE_RATE: usize = 24_000;
const OPENAI_REALTIME_TRANSLATION_URL: &str =
    "wss://api.openai.com/v1/realtime/translations?model=gpt-realtime-translate";

async fn spawn_tcp_server<F, Fut>(script: F) -> (String, JoinHandle<()>)
where
    F: FnOnce(TcpStream) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        script(stream).await;
    });
    (
        format!("ws://{address}/v1/realtime/translations?model=test"),
        task,
    )
}

async fn spawn_paid_cutoff_proxy(
    api_key: String,
) -> (String, JoinHandle<()>, tokio::sync::oneshot::Receiver<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind paid cutoff proxy");
    let endpoint = format!(
        "ws://{}/v1/realtime/translations?model=gpt-realtime-translate",
        listener.local_addr().expect("paid cutoff proxy address")
    );
    let (cut_tx, cut_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let (local_stream, _) = listener.accept().await.expect("accept translation client");
        let mut local = accept_async(local_stream)
            .await
            .expect("accept local translation websocket");

        let mut upstream_request = OPENAI_REALTIME_TRANSLATION_URL
            .into_client_request()
            .expect("build OpenAI translation request");
        upstream_request.headers_mut().insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", api_key.trim()))
                .expect("valid paid E2E authorization header"),
        );
        let (mut upstream, _) =
            tokio::time::timeout(Duration::from_secs(15), connect_async(upstream_request))
                .await
                .expect("OpenAI translation proxy connect timed out")
                .expect("OpenAI translation proxy connect failed");

        loop {
            tokio::select! {
                local_message = local.next() => {
                    let message = local_message
                        .expect("local translation websocket ended before cutoff")
                        .expect("local translation websocket read failed");
                    let cuts_connection = matches!(
                        &message,
                        Message::Text(text)
                            if serde_json::from_str::<Value>(text)
                                .ok()
                                .and_then(|value| value["type"].as_str().map(str::to_owned))
                                .as_deref()
                                == Some("session.input_audio_buffer.append")
                    );
                    upstream
                        .send(message)
                        .await
                        .expect("forward client message to OpenAI");
                    if cuts_connection {
                        let _ = cut_tx.send(());
                        return;
                    }
                }
                upstream_message = upstream.next() => {
                    let message = upstream_message
                        .expect("OpenAI websocket ended before controlled cutoff")
                        .expect("OpenAI websocket read failed before controlled cutoff");
                    local
                        .send(message)
                        .await
                        .expect("forward OpenAI event to translation client");
                }
            }
        }
    });

    (endpoint, task, cut_rx)
}

async fn receive_json(ws: &mut WebSocketStream<TcpStream>) -> Value {
    let message = tokio::time::timeout(TEST_TIMEOUT, ws.next())
        .await
        .expect("WebSocket message timed out")
        .expect("WebSocket closed unexpectedly")
        .expect("WebSocket read failed");
    let Message::Text(text) = message else {
        panic!("expected text WebSocket message");
    };
    serde_json::from_str(&text).expect("client message must be JSON")
}

async fn send_json(ws: &mut WebSocketStream<TcpStream>, value: Value) {
    ws.send(Message::Text(value.to_string()))
        .await
        .expect("server event send must succeed");
}

fn pcm16_base64(samples: &[i16]) -> String {
    let bytes: Vec<u8> = samples
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[derive(Default)]
struct SyntheticFlowState {
    capture_request: Mutex<Option<SystemAudioCaptureRequest>>,
    capture_started: AtomicBool,
    capture_stopped: AtomicBool,
    output_opened: AtomicBool,
    output_closed: AtomicBool,
    output_samples: Mutex<Vec<i16>>,
    output_sample_count: AtomicUsize,
    output_pending_high_water_samples: AtomicUsize,
    simulated_playback: Mutex<SimulatedPlayback>,
    output_gain: Mutex<Option<f32>>,
}

struct SimulatedPlayback {
    pending_samples: usize,
    updated_at: Instant,
}

impl Default for SimulatedPlayback {
    fn default() -> Self {
        Self {
            pending_samples: 0,
            updated_at: Instant::now(),
        }
    }
}

impl SyntheticFlowState {
    fn refresh_pending_samples(&self) -> usize {
        let mut playback = self.simulated_playback.lock().unwrap();
        let now = Instant::now();
        let elapsed_samples = (now.duration_since(playback.updated_at).as_secs_f64()
            * TRANSLATED_OUTPUT_SAMPLE_RATE as f64) as usize;
        playback.pending_samples = playback.pending_samples.saturating_sub(elapsed_samples);
        playback.updated_at = now;
        playback.pending_samples
    }

    fn enqueue_simulated_playback(&self, samples: usize) -> Duration {
        let mut playback = self.simulated_playback.lock().unwrap();
        let now = Instant::now();
        let elapsed_samples = (now.duration_since(playback.updated_at).as_secs_f64()
            * TRANSLATED_OUTPUT_SAMPLE_RATE as f64) as usize;
        playback.pending_samples = playback
            .pending_samples
            .saturating_sub(elapsed_samples)
            .saturating_add(samples);
        playback.updated_at = now;
        self.output_pending_high_water_samples
            .fetch_max(playback.pending_samples, Ordering::Relaxed);
        Duration::from_secs_f64(
            playback.pending_samples as f64 / TRANSLATED_OUTPUT_SAMPLE_RATE as f64,
        )
    }

    fn pending_playback_duration(&self) -> Duration {
        Duration::from_secs_f64(
            self.refresh_pending_samples() as f64 / TRANSLATED_OUTPUT_SAMPLE_RATE as f64,
        )
    }
}

struct SyntheticCaptureFactory {
    state: Arc<SyntheticFlowState>,
    samples: Arc<Vec<i16>>,
}

impl SystemAudioCaptureFactory for SyntheticCaptureFactory {
    fn preflight_system_audio_capture(
        &self,
        request: SystemAudioCaptureRequest,
    ) -> AudioResult<()> {
        *self.state.capture_request.lock().unwrap() = Some(request);
        Ok(())
    }

    fn create_system_audio_capture(
        &self,
        request: SystemAudioCaptureRequest,
    ) -> AudioResult<Box<dyn AudioCapture>> {
        assert_eq!(request, SystemAudioCaptureRequest::isolated(request.target));
        Ok(Box::new(SyntheticCapture {
            state: self.state.clone(),
            samples: self.samples.clone(),
            config: AudioConfig::default(),
        }))
    }
}

struct SyntheticCapture {
    state: Arc<SyntheticFlowState>,
    samples: Arc<Vec<i16>>,
    config: AudioConfig,
}

#[async_trait]
impl AudioCapture for SyntheticCapture {
    async fn initialize(&mut self, config: AudioConfig) -> AudioResult<()> {
        self.config = config;
        Ok(())
    }

    async fn start_capture(&mut self, on_chunk: AudioChunkCallback) -> AudioResult<()> {
        self.state.capture_started.store(true, Ordering::SeqCst);
        for samples in self.samples.chunks(4_800) {
            on_chunk(AudioChunk::new(
                samples.to_vec(),
                self.config.sample_rate,
                self.config.channels,
            ));
        }
        Ok(())
    }

    async fn stop_capture(&mut self) -> AudioResult<()> {
        self.state.capture_stopped.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn is_capturing(&self) -> bool {
        self.state.capture_started.load(Ordering::SeqCst)
            && !self.state.capture_stopped.load(Ordering::SeqCst)
    }

    fn config(&self) -> AudioConfig {
        self.config
    }
}

struct CollectingOutputFactory {
    state: Arc<SyntheticFlowState>,
    retain_samples: bool,
}

impl LocalPlaybackOutputFactory for CollectingOutputFactory {
    fn create_local_playback_output(
        &self,
        route: LocalPlaybackRoute,
    ) -> TranslationAudioOutputResult<Box<dyn TranslationAudioOutput>> {
        assert_eq!(route, LocalPlaybackRoute::SystemDefault);
        Ok(Box::new(CollectingOutput {
            state: self.state.clone(),
            retain_samples: self.retain_samples,
        }))
    }
}

struct CollectingOutput {
    state: Arc<SyntheticFlowState>,
    retain_samples: bool,
}

#[async_trait]
impl TranslationAudioOutput for CollectingOutput {
    async fn open(
        &mut self,
        config: TranslationAudioOutputConfig,
    ) -> TranslationAudioOutputResult<()> {
        self.state.output_opened.store(true, Ordering::SeqCst);
        *self.state.output_gain.lock().unwrap() = Some(config.gain);
        Ok(())
    }

    async fn enqueue_pcm16(
        &self,
        samples: &[i16],
    ) -> TranslationAudioOutputResult<AudioEnqueueOutcome> {
        self.state
            .output_sample_count
            .fetch_add(samples.len(), Ordering::Relaxed);
        if self.retain_samples {
            self.state
                .output_samples
                .lock()
                .unwrap()
                .extend_from_slice(samples);
        }
        let pending = self.state.enqueue_simulated_playback(samples.len());
        Ok(AudioEnqueueOutcome::Queued { pending })
    }

    async fn close(&mut self) -> TranslationAudioOutputResult<()> {
        self.state.output_closed.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn set_gain(&mut self, gain: f32) -> TranslationAudioOutputResult<()> {
        *self.state.output_gain.lock().unwrap() = Some(gain);
        Ok(())
    }

    fn is_open(&self) -> bool {
        self.state.output_opened.load(Ordering::SeqCst)
            && !self.state.output_closed.load(Ordering::SeqCst)
    }

    fn device_name(&self) -> Option<String> {
        Some("synthetic-system-default".into())
    }

    fn begin_drain_mode(&self) {}

    fn prepare_for_drain(&self) -> TranslationAudioOutputResult<Duration> {
        Ok(self.state.pending_playback_duration())
    }

    fn pending_playback_duration(&self) -> Duration {
        self.state.pending_playback_duration()
    }
}

struct ReadyCapability;

impl SpokenTranslationCapability for ReadyCapability {
    fn check(&self, _target_language: &str) -> SpokenIncomingCapability {
        SpokenIncomingCapability::Ready
    }
}

struct LocalWebSocketTranslationFactory {
    endpoint: String,
}

impl RealtimeTranslationFactory for LocalWebSocketTranslationFactory {
    fn create(&self) -> Box<dyn RealtimeTranslationSession> {
        Box::new(
            OpenAIRealtimeTranslationClient::with_endpoint(self.endpoint.clone()).with_timeouts(
                TEST_TIMEOUT,
                TEST_TIMEOUT,
                TEST_TIMEOUT,
                Duration::from_millis(100),
            ),
        )
    }
}

struct PaidProxyTranslationFactory {
    endpoint: String,
}

impl RealtimeTranslationFactory for PaidProxyTranslationFactory {
    fn create(&self) -> Box<dyn RealtimeTranslationSession> {
        Box::new(
            OpenAIRealtimeTranslationClient::with_endpoint(self.endpoint.clone()).with_timeouts(
                Duration::from_secs(15),
                Duration::from_secs(15),
                Duration::from_secs(5),
                Duration::from_secs(1),
            ),
        )
    }
}

#[derive(Default)]
struct ServerObservation {
    authorization: Mutex<Option<String>>,
    path: Mutex<Option<String>>,
    target_language: Mutex<Option<String>>,
    appended_samples: Mutex<Vec<i16>>,
    close_received: AtomicBool,
}

#[tokio::test]
#[allow(clippy::result_large_err)]
async fn spoken_facade_runs_synthetic_audio_through_local_websocket_and_playback() {
    let server_observation = Arc::new(ServerObservation::default());
    let server_state = server_observation.clone();
    let (endpoint, server_task) = spawn_tcp_server(move |stream| async move {
        let header_state = server_state.clone();
        let mut ws = accept_hdr_async(stream, move |request: &Request, response: Response| {
            *header_state.authorization.lock().unwrap() = request
                .headers()
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            *header_state.path.lock().unwrap() = Some(request.uri().to_string());
            Ok(response)
        })
        .await
        .expect("local WebSocket handshake must succeed");

        let update = receive_json(&mut ws).await;
        assert_eq!(update["type"], "session.update");
        assert!(
            update
                .pointer("/session/audio/input/noise_reduction")
                .is_some_and(Value::is_null),
            "clean system audio must disable provider noise reduction: {update}"
        );
        *server_state.target_language.lock().unwrap() = update
            .pointer("/session/audio/output/language")
            .and_then(Value::as_str)
            .map(str::to_string);
        send_json(&mut ws, json!({ "type": "session.created" })).await;
        send_json(&mut ws, json!({ "type": "session.updated" })).await;

        let append = receive_json(&mut ws).await;
        assert_eq!(append["type"], "session.input_audio_buffer.append");
        let audio = append["audio"].as_str().expect("append must contain audio");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(audio)
            .expect("append audio must be base64");
        let samples: Vec<i16> = bytes
            .chunks_exact(2)
            .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
            .collect();
        server_state
            .appended_samples
            .lock()
            .unwrap()
            .extend_from_slice(&samples);

        send_json(
            &mut ws,
            json!({ "type": "future.server.event", "ignored": true }),
        )
        .await;
        send_json(
            &mut ws,
            json!({ "type": "session.input_transcript.delta", "delta": "hello caller" }),
        )
        .await;
        send_json(
            &mut ws,
            json!({ "type": "session.output_transcript.delta", "delta": "привет" }),
        )
        .await;
        send_json(
            &mut ws,
            json!({
                "type": "session.output_audio.delta",
                "delta": pcm16_base64(&[120, -240, 360])
            }),
        )
        .await;

        loop {
            let message = receive_json(&mut ws).await;
            if message["type"] == "session.close" {
                server_state.close_received.store(true, Ordering::SeqCst);
                break;
            }
        }
        send_json(
            &mut ws,
            json!({ "type": "session.output_transcript.delta", "delta": " мир" }),
        )
        .await;
        send_json(
            &mut ws,
            json!({
                "type": "session.output_audio.delta",
                "audio": pcm16_base64(&[-480, 600])
            }),
        )
        .await;
        send_json(&mut ws, json!({ "type": "session.closed" })).await;
    })
    .await;

    let flow_state = Arc::new(SyntheticFlowState::default());
    let input_samples: Arc<Vec<i16>> = Arc::new((0..4_800).map(|value| value as i16).collect());
    let service = IncomingTranslationFacade::new_spoken_with_factories(
        Arc::new(SyntheticCaptureFactory {
            state: flow_state.clone(),
            samples: input_samples.clone(),
        }),
        Arc::new(CollectingOutputFactory {
            state: flow_state.clone(),
            retain_samples: true,
        }),
        Arc::new(LocalWebSocketTranslationFactory { endpoint }),
        Arc::new(ReadyCapability),
    );
    let source_text = Arc::new(Mutex::new(String::new()));
    let translated_text = Arc::new(Mutex::new(String::new()));
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let statuses = Arc::new(Mutex::new(Vec::<RecordingStatus>::new()));
    let callbacks = IncomingTranslationCallbacks {
        on_source_final: {
            let source_text = source_text.clone();
            Arc::new(move |delta| source_text.lock().unwrap().push_str(&delta))
        },
        on_translation_delta: {
            let translated_text = translated_text.clone();
            Arc::new(move |delta| translated_text.lock().unwrap().push_str(&delta))
        },
        on_error: {
            let errors = errors.clone();
            Arc::new(move |error| errors.lock().unwrap().push(error.to_string()))
        },
        on_status: {
            let statuses = statuses.clone();
            Arc::new(move |status| statuses.lock().unwrap().push(status))
        },
    };
    let mut config = IncomingTranslationConfig::new_with_defaults(SttConfig::default(), 8_001);
    config.openai_api_key = TEST_CREDENTIAL.into();
    config.target_language = "ru".into();
    config.playback_gain = 0.65;

    service.start(config, callbacks).await.unwrap();
    tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            if translated_text.lock().unwrap().contains("привет")
                && !flow_state.output_samples.lock().unwrap().is_empty()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("translated text and audio must reach application callbacks");
    service.stop().await.unwrap();
    tokio::time::timeout(TEST_TIMEOUT, server_task)
        .await
        .expect("fake WebSocket server must stop")
        .expect("fake WebSocket server task must not panic");

    assert_eq!(service.get_status().await, RecordingStatus::Idle);
    assert!(flow_state.capture_started.load(Ordering::SeqCst));
    assert!(flow_state.capture_stopped.load(Ordering::SeqCst));
    assert!(flow_state.output_opened.load(Ordering::SeqCst));
    assert!(flow_state.output_closed.load(Ordering::SeqCst));
    assert_eq!(*flow_state.output_gain.lock().unwrap(), Some(0.65));
    assert_eq!(
        *flow_state.output_samples.lock().unwrap(),
        vec![120, -240, 360, -480, 600]
    );
    assert_eq!(&*source_text.lock().unwrap(), "hello caller");
    assert_eq!(&*translated_text.lock().unwrap(), "привет мир");
    assert!(errors.lock().unwrap().is_empty());
    assert!(statuses
        .lock()
        .unwrap()
        .contains(&RecordingStatus::Starting));
    assert!(statuses
        .lock()
        .unwrap()
        .contains(&RecordingStatus::Recording));
    assert!(statuses
        .lock()
        .unwrap()
        .contains(&RecordingStatus::Processing));
    assert!(statuses.lock().unwrap().contains(&RecordingStatus::Idle));

    let request = flow_state.capture_request.lock().unwrap().unwrap();
    assert_eq!(
        request.target,
        AudioCaptureTarget::incoming_realtime_translation()
    );
    assert_eq!(
        &*server_observation.authorization.lock().unwrap(),
        &Some(format!("Bearer {TEST_CREDENTIAL}"))
    );
    assert_eq!(
        server_observation.path.lock().unwrap().as_deref(),
        Some("/v1/realtime/translations?model=test")
    );
    assert_eq!(
        server_observation
            .target_language
            .lock()
            .unwrap()
            .as_deref(),
        Some("ru")
    );
    assert_eq!(
        &*server_observation.appended_samples.lock().unwrap(),
        &*input_samples
    );
    assert!(server_observation.close_received.load(Ordering::SeqCst));
}

#[tokio::test]
#[ignore = "paid/manual: requires VOICETEXT_RUN_PAID_E2E=1 and a dedicated OPENAI_E2E_API_KEY"]
async fn paid_openai_network_interruption_cleans_incoming_capture_and_output() {
    let api_key = load_paid_e2e_api_key();
    let (endpoint, proxy_task, cutoff) = spawn_paid_cutoff_proxy(api_key.clone()).await;
    let flow_state = Arc::new(SyntheticFlowState::default());
    let service = IncomingTranslationFacade::new_spoken_with_factories(
        Arc::new(SyntheticCaptureFactory {
            state: flow_state.clone(),
            samples: Arc::new(vec![1_200; 4_800]),
        }),
        Arc::new(CollectingOutputFactory {
            state: flow_state.clone(),
            retain_samples: false,
        }),
        Arc::new(PaidProxyTranslationFactory { endpoint }),
        Arc::new(ReadyCapability),
    );
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let statuses = Arc::new(Mutex::new(Vec::<RecordingStatus>::new()));
    let callbacks = IncomingTranslationCallbacks {
        on_source_final: Arc::new(|_| {}),
        on_translation_delta: Arc::new(|_| {}),
        on_error: {
            let errors = errors.clone();
            Arc::new(move |error| errors.lock().unwrap().push(error.to_string()))
        },
        on_status: {
            let statuses = statuses.clone();
            Arc::new(move |status| statuses.lock().unwrap().push(status))
        },
    };
    let mut config = IncomingTranslationConfig::new_with_defaults(SttConfig::default(), 8_003);
    config.openai_api_key = api_key;
    config.target_language = "ru".into();

    service
        .start(config, callbacks)
        .await
        .expect("paid translation session must become ready before interruption");
    tokio::time::timeout(Duration::from_secs(20), cutoff)
        .await
        .expect("proxy must interrupt the paid session after the first PCM append")
        .expect("paid cutoff signal must be delivered");
    tokio::time::timeout(Duration::from_secs(10), async {
        while service.active_session_id().await.is_some()
            || service.get_status().await != RecordingStatus::Error
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("network interruption must clean the incoming runtime");
    proxy_task
        .await
        .expect("paid cutoff proxy task must stop cleanly");

    assert!(flow_state.capture_started.load(Ordering::SeqCst));
    assert!(flow_state.capture_stopped.load(Ordering::SeqCst));
    assert!(flow_state.output_opened.load(Ordering::SeqCst));
    assert!(flow_state.output_closed.load(Ordering::SeqCst));
    assert!(flow_state.output_samples.lock().unwrap().is_empty());
    assert!(
        matches!(errors.lock().unwrap().as_slice(), [message] if message.contains("connection"))
    );
    assert!(statuses.lock().unwrap().contains(&RecordingStatus::Error));
}

fn local_client(endpoint: String, ready_timeout: Duration) -> OpenAIRealtimeTranslationClient {
    OpenAIRealtimeTranslationClient::with_endpoint(endpoint).with_timeouts(
        TEST_TIMEOUT,
        ready_timeout,
        TEST_TIMEOUT,
        Duration::from_millis(50),
    )
}

fn test_translation_config() -> RealtimeTranslationConfig {
    RealtimeTranslationConfig::new(
        TEST_CREDENTIAL.into(),
        "ru".into(),
        RealtimeInputNoiseReduction::NearField,
    )
}

#[tokio::test]
async fn delayed_ready_succeeds_but_missing_ready_times_out() {
    let (endpoint, server) = spawn_tcp_server(|stream| async move {
        let mut ws = accept_async(stream).await.unwrap();
        assert_eq!(receive_json(&mut ws).await["type"], "session.update");
        tokio::time::sleep(Duration::from_millis(40)).await;
        send_json(&mut ws, json!({ "type": "session.updated" })).await;
        assert_eq!(receive_json(&mut ws).await["type"], "session.close");
        send_json(&mut ws, json!({ "type": "session.closed" })).await;
    })
    .await;
    let mut client = local_client(endpoint, Duration::from_millis(300));
    client
        .connect(test_translation_config())
        .await
        .expect("delayed readiness inside timeout must succeed");
    client.finish(Duration::from_millis(100)).await.unwrap();
    server.await.unwrap();

    let (endpoint, server) = spawn_tcp_server(|stream| async move {
        let mut ws = accept_async(stream).await.unwrap();
        assert_eq!(receive_json(&mut ws).await["type"], "session.update");
        tokio::time::sleep(Duration::from_millis(200)).await;
    })
    .await;
    let mut client = local_client(endpoint, Duration::from_millis(30));
    let started = Instant::now();
    let error = client.connect(test_translation_config()).await.unwrap_err();
    assert_eq!(error.kind(), RealtimeTranslationErrorKind::Timeout);
    assert!(started.elapsed() < Duration::from_millis(300));
    server.await.unwrap();
}

async fn assert_runtime_protocol_failure(server_event: Message) {
    let (endpoint, server) = spawn_tcp_server(move |stream| async move {
        let mut ws = accept_async(stream).await.unwrap();
        assert_eq!(receive_json(&mut ws).await["type"], "session.update");
        send_json(&mut ws, json!({ "type": "session.updated" })).await;
        ws.send(server_event).await.unwrap();
    })
    .await;
    let mut client = local_client(endpoint, TEST_TIMEOUT);
    let mut events = client.connect(test_translation_config()).await.unwrap();
    let event = tokio::time::timeout(TEST_TIMEOUT, events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        event,
        RealtimeTranslationEvent::Failed(error)
            if error.kind() == RealtimeTranslationErrorKind::Protocol
    ));
    client.abort().await;
    server.await.unwrap();
}

#[tokio::test]
async fn malformed_json_and_base64_are_terminal_protocol_failures() {
    assert_runtime_protocol_failure(Message::Text("{not-json".into())).await;
    assert_runtime_protocol_failure(Message::Text(
        json!({ "type": "session.output_audio.delta", "delta": "%%%" }).to_string(),
    ))
    .await;
}

async fn assert_handshake_status_kind(
    status: u16,
    reason: &str,
    expected: RealtimeTranslationErrorKind,
) {
    let reason = reason.to_string();
    let (endpoint, server) = spawn_tcp_server(move |mut stream| async move {
        let mut request = [0u8; 2048];
        let _ = stream.read(&mut request).await.unwrap();
        let response =
            format!("HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        stream.write_all(response.as_bytes()).await.unwrap();
    })
    .await;
    let mut client = local_client(endpoint, TEST_TIMEOUT);
    let error = client.connect(test_translation_config()).await.unwrap_err();
    assert_eq!(error.kind(), expected);
    server.await.unwrap();
}

#[tokio::test]
async fn handshake_401_and_429_keep_typed_error_categories() {
    assert_handshake_status_kind(
        401,
        "Unauthorized",
        RealtimeTranslationErrorKind::Authentication,
    )
    .await;
    assert_handshake_status_kind(
        429,
        "Too Many Requests",
        RealtimeTranslationErrorKind::RateLimited,
    )
    .await;
}

#[tokio::test]
async fn abrupt_close_emits_closed_and_stalled_close_is_bounded() {
    let (endpoint, server) = spawn_tcp_server(|stream| async move {
        let mut ws = accept_async(stream).await.unwrap();
        assert_eq!(receive_json(&mut ws).await["type"], "session.update");
        send_json(&mut ws, json!({ "type": "session.updated" })).await;
        ws.send(Message::Close(None)).await.unwrap();
    })
    .await;
    let mut client = local_client(endpoint, TEST_TIMEOUT);
    let mut events = client.connect(test_translation_config()).await.unwrap();
    assert_eq!(
        tokio::time::timeout(TEST_TIMEOUT, events.recv())
            .await
            .unwrap(),
        Some(RealtimeTranslationEvent::Closed)
    );
    client.abort().await;
    server.await.unwrap();

    let (endpoint, server) = spawn_tcp_server(|stream| async move {
        let mut ws = accept_async(stream).await.unwrap();
        assert_eq!(receive_json(&mut ws).await["type"], "session.update");
        send_json(&mut ws, json!({ "type": "session.updated" })).await;
        assert_eq!(receive_json(&mut ws).await["type"], "session.close");
        tokio::time::sleep(Duration::from_secs(1)).await;
    })
    .await;
    let mut client = local_client(endpoint, TEST_TIMEOUT);
    let _events = client.connect(test_translation_config()).await.unwrap();
    let started = Instant::now();
    client.finish(Duration::from_millis(50)).await.unwrap();
    assert!(started.elapsed() < Duration::from_millis(400));
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn oversized_server_message_is_rejected_by_websocket_limit() {
    let (endpoint, server) = spawn_tcp_server(|stream| async move {
        let mut ws = accept_async(stream).await.unwrap();
        assert_eq!(receive_json(&mut ws).await["type"], "session.update");
        send_json(&mut ws, json!({ "type": "session.updated" })).await;
        let oversized = "x".repeat(8 * 1024 * 1024 + 1);
        let _ = ws.send(Message::Text(oversized)).await;
    })
    .await;
    let mut client = local_client(endpoint, TEST_TIMEOUT);
    let mut events = client.connect(test_translation_config()).await.unwrap();
    let event = tokio::time::timeout(TEST_TIMEOUT, events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        event,
        RealtimeTranslationEvent::Failed(error)
            if error.kind() == RealtimeTranslationErrorKind::Connection
    ));
    client.abort().await;
    server.await.unwrap();
}

fn spoken_soak_duration() -> Duration {
    std::env::var("SPOKEN_TRANSLATION_SOAK_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(30 * 60))
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

#[tokio::test]
#[ignore = "30-minute synthetic spoken soak; set SPOKEN_TRANSLATION_SOAK_SECONDS for a shorter manual run"]
async fn spoken_runtime_long_soak_keeps_audio_flow_bounded_and_stops_cleanly() {
    const SAMPLES_PER_EVENT: usize = TRANSLATED_OUTPUT_SAMPLE_RATE / 10;
    let emitted_audio_events = Arc::new(AtomicUsize::new(0));
    let server_event_count = emitted_audio_events.clone();
    let (endpoint, server) = spawn_tcp_server(move |stream| async move {
        let mut ws = accept_async(stream).await.unwrap();
        assert_eq!(receive_json(&mut ws).await["type"], "session.update");
        send_json(&mut ws, json!({ "type": "session.updated" })).await;
        let realtime_audio_delta = pcm16_base64(&vec![1_200; SAMPLES_PER_EVENT]);
        let mut ticker = tokio::time::interval(Duration::from_millis(100));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    let index = server_event_count.fetch_add(1, Ordering::SeqCst);
                    send_json(
                        &mut ws,
                        json!({
                            "type": "session.output_audio.delta",
                            "delta": realtime_audio_delta
                        }),
                    )
                    .await;
                    if index % 10 == 0 {
                        send_json(
                            &mut ws,
                            json!({ "type": "session.output_transcript.delta", "delta": "." }),
                        )
                        .await;
                    }
                }
                message = ws.next() => {
                    let Some(Ok(Message::Text(text))) = message else {
                        return;
                    };
                    let message: Value = serde_json::from_str(&text).unwrap();
                    if message["type"] == "session.close" {
                        send_json(&mut ws, json!({ "type": "session.closed" })).await;
                        return;
                    }
                }
            }
        }
    })
    .await;

    let flow_state = Arc::new(SyntheticFlowState::default());
    let service = IncomingTranslationFacade::new_spoken_with_factories(
        Arc::new(SyntheticCaptureFactory {
            state: flow_state.clone(),
            samples: Arc::new(vec![7; 4_800]),
        }),
        Arc::new(CollectingOutputFactory {
            state: flow_state.clone(),
            retain_samples: false,
        }),
        Arc::new(LocalWebSocketTranslationFactory { endpoint }),
        Arc::new(ReadyCapability),
    );
    let translated_text_chars = Arc::new(AtomicUsize::new(0));
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let callbacks = IncomingTranslationCallbacks {
        on_source_final: Arc::new(|_| {}),
        on_translation_delta: {
            let translated_text_chars = translated_text_chars.clone();
            Arc::new(move |delta| {
                translated_text_chars.fetch_add(delta.len(), Ordering::Relaxed);
            })
        },
        on_error: {
            let errors = errors.clone();
            Arc::new(move |error| errors.lock().unwrap().push(error.to_string()))
        },
        on_status: Arc::new(|_| {}),
    };
    let mut config = IncomingTranslationConfig::new_with_defaults(SttConfig::default(), 8_002);
    config.openai_api_key = TEST_CREDENTIAL.into();
    config.target_language = "ru".into();
    let duration = spoken_soak_duration();

    service.start(config, callbacks).await.unwrap();
    let warmup = Duration::from_secs(10).min(duration / 4);
    let sample_interval = Duration::from_secs(5).min(duration.max(Duration::from_secs(1)));
    let started_at = tokio::time::Instant::now();
    let deadline = tokio::time::Instant::now() + duration;
    let mut next_rss_sample = started_at + warmup;
    let mut rss_samples_kib = Vec::new();
    let mut max_backlog_events = 0usize;
    let mut final_backlog_events = 0usize;
    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(1).min(duration)).await;
        assert_eq!(service.get_status().await, RecordingStatus::Recording);
        assert!(errors.lock().unwrap().is_empty());

        let emitted = emitted_audio_events.load(Ordering::SeqCst);
        let enqueued_samples = flow_state.output_sample_count.load(Ordering::Relaxed);
        let pending_samples = flow_state.refresh_pending_samples();
        let consumed = enqueued_samples.saturating_sub(pending_samples) / SAMPLES_PER_EVENT;
        final_backlog_events = emitted.saturating_sub(consumed);
        max_backlog_events = max_backlog_events.max(final_backlog_events);
        assert!(
            final_backlog_events <= 20,
            "spoken runtime output backlog exceeded two seconds: emitted={emitted}, consumed={consumed}, backlog={final_backlog_events}"
        );

        let now = tokio::time::Instant::now();
        if now >= next_rss_sample {
            rss_samples_kib.push(
                current_process_rss_kib()
                    .expect("release soak requires a working ps RSS measurement"),
            );
            next_rss_sample = now + sample_interval;
        }
    }
    service.stop().await.unwrap();
    tokio::time::timeout(TEST_TIMEOUT, server)
        .await
        .expect("soak server must observe graceful close")
        .expect("soak server must not panic");

    let events = emitted_audio_events.load(Ordering::SeqCst);
    let samples = flow_state.output_sample_count.load(Ordering::Relaxed);
    let text_chars = translated_text_chars.load(Ordering::Relaxed);
    let pending_high_water_samples = flow_state
        .output_pending_high_water_samples
        .load(Ordering::Relaxed);
    assert!(
        rss_samples_kib.len() >= 2,
        "spoken runtime soak requires at least two RSS samples, got {:?}",
        rss_samples_kib
    );
    let baseline_rss_kib = rss_samples_kib[0];
    let max_rss_kib = *rss_samples_kib.iter().max().unwrap();
    let rss_growth_kib = max_rss_kib.saturating_sub(baseline_rss_kib);
    println!(
        "spoken_soak_seconds={}, audio_events={events}, output_samples={samples}, text_chars={}, max_backlog_events={}, final_backlog_events={}, pending_high_water_samples={}, rss_samples_kib={:?}, rss_growth_kib={}",
        duration.as_secs(),
        text_chars,
        max_backlog_events,
        final_backlog_events,
        pending_high_water_samples,
        rss_samples_kib,
        rss_growth_kib
    );
    assert!(events > 0);
    assert_eq!(samples, events * SAMPLES_PER_EVENT);
    assert!(samples <= (duration.as_secs() as usize + 2) * TRANSLATED_OUTPUT_SAMPLE_RATE);
    assert!(pending_high_water_samples > 0);
    assert!(
        pending_high_water_samples <= TRANSLATED_OUTPUT_SAMPLE_RATE * 2,
        "spoken runtime pending playback exceeded two seconds: {pending_high_water_samples} samples"
    );
    assert!(
        final_backlog_events <= 5,
        "spoken runtime backlog did not drain near real time: {final_backlog_events} events"
    );
    assert!(
        rss_growth_kib <= 16 * 1024,
        "spoken runtime peak RSS grew by {rss_growth_kib} KiB across the measured soak window"
    );
    assert!(flow_state.output_samples.lock().unwrap().is_empty());
    assert!(flow_state.capture_stopped.load(Ordering::SeqCst));
    assert!(flow_state.output_closed.load(Ordering::SeqCst));
    assert_eq!(service.get_status().await, RecordingStatus::Idle);
}
