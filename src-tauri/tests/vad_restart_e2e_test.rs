use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use app_lib::application::services::TranscriptionService;
use app_lib::domain::{
    AudioCapture, AudioChunk, AudioChunkCallback, AudioConfig, AudioLevelCallback,
    AudioSpectrumCallback, ConnectionQualityCallback, ErrorCallback, SttConfig, SttError,
    SttProvider, SttProviderFactory, SttResult, TranscriptionCallback,
};
use app_lib::infrastructure::audio::{VadCaptureWrapper, VadProcessor};
use async_trait::async_trait;

type CallbackSlot = Arc<Mutex<Option<AudioChunkCallback>>>;

struct ManualCallbackAudioCapture {
    callback: CallbackSlot,
    is_capturing: Arc<AtomicBool>,
    config: AudioConfig,
}

impl ManualCallbackAudioCapture {
    fn new(callback: CallbackSlot) -> Self {
        Self {
            callback,
            is_capturing: Arc::new(AtomicBool::new(false)),
            config: AudioConfig::default(),
        }
    }
}

#[async_trait]
impl AudioCapture for ManualCallbackAudioCapture {
    async fn initialize(&mut self, config: AudioConfig) -> app_lib::domain::AudioResult<()> {
        self.config = config;
        Ok(())
    }

    async fn start_capture(
        &mut self,
        on_chunk: AudioChunkCallback,
    ) -> app_lib::domain::AudioResult<()> {
        self.is_capturing.store(true, Ordering::SeqCst);
        *self.callback.lock().expect("callback mutex poisoned") = Some(on_chunk);
        Ok(())
    }

    async fn stop_capture(&mut self) -> app_lib::domain::AudioResult<()> {
        self.is_capturing.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn is_capturing(&self) -> bool {
        self.is_capturing.load(Ordering::SeqCst)
    }

    fn config(&self) -> AudioConfig {
        self.config
    }
}

struct CountingSttProvider {
    started_streams: Arc<AtomicUsize>,
    stopped_streams: Arc<AtomicUsize>,
    aborted_streams: Arc<AtomicUsize>,
    sent_chunks: Arc<AtomicUsize>,
}

#[async_trait]
impl SttProvider for CountingSttProvider {
    async fn initialize(&mut self, _config: &SttConfig) -> SttResult<()> {
        Ok(())
    }

    async fn start_stream(
        &mut self,
        _on_partial: TranscriptionCallback,
        _on_final: TranscriptionCallback,
        _on_error: ErrorCallback,
        _on_connection_quality: ConnectionQualityCallback,
    ) -> SttResult<()> {
        self.started_streams.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn send_audio(&mut self, _chunk: &AudioChunk) -> SttResult<()> {
        self.sent_chunks.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn stop_stream(&mut self) -> SttResult<()> {
        self.stopped_streams.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn abort(&mut self) -> SttResult<()> {
        self.aborted_streams.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn name(&self) -> &str {
        "counting_stt_provider"
    }

    fn is_online(&self) -> bool {
        true
    }
}

struct CountingSttProviderFactory {
    started_streams: Arc<AtomicUsize>,
    stopped_streams: Arc<AtomicUsize>,
    aborted_streams: Arc<AtomicUsize>,
    sent_chunks: Arc<AtomicUsize>,
}

impl SttProviderFactory for CountingSttProviderFactory {
    fn create(&self, _config: &SttConfig) -> SttResult<Box<dyn SttProvider>> {
        Ok(Box::new(CountingSttProvider {
            started_streams: self.started_streams.clone(),
            stopped_streams: self.stopped_streams.clone(),
            aborted_streams: self.aborted_streams.clone(),
            sent_chunks: self.sent_chunks.clone(),
        }))
    }
}

fn active_then_silence_chunk() -> AudioChunk {
    let mut samples = vec![0i16; 480 * 5];
    for sample in samples.iter_mut().take(480) {
        *sample = 1200;
    }
    AudioChunk::new(samples, 16000, 1)
}

fn noop_partial() -> TranscriptionCallback {
    Arc::new(|_| {})
}

fn noop_final() -> TranscriptionCallback {
    Arc::new(|_| {})
}

fn noop_level() -> AudioLevelCallback {
    Arc::new(|_| {})
}

fn noop_spectrum() -> AudioSpectrumCallback {
    Arc::new(|_| {})
}

fn noop_error() -> ErrorCallback {
    Arc::new(|_err: SttError| {})
}

fn noop_connection_quality() -> ConnectionQualityCallback {
    Arc::new(|_, _| {})
}

async fn start_service(service: &TranscriptionService) {
    service
        .start_recording(
            noop_partial(),
            noop_final(),
            noop_level(),
            noop_spectrum(),
            noop_error(),
            noop_connection_quality(),
        )
        .await
        .expect("recording should start");
}

async fn wait_for_counter_at_least(counter: &AtomicUsize, expected: usize) {
    for _ in 0..50 {
        if counter.load(Ordering::SeqCst) >= expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    panic!(
        "counter did not reach expected value: actual={}, expected={}",
        counter.load(Ordering::SeqCst),
        expected
    );
}

struct VadPipelineHarness {
    callback_slot: CallbackSlot,
    vad_timeouts: Arc<AtomicUsize>,
    started_streams: Arc<AtomicUsize>,
    stopped_streams: Arc<AtomicUsize>,
    aborted_streams: Arc<AtomicUsize>,
    sent_chunks: Arc<AtomicUsize>,
    service: TranscriptionService,
}

impl VadPipelineHarness {
    fn new() -> Self {
        let vad_timeouts = Arc::new(AtomicUsize::new(0));
        let vad_timeouts_for_callback = vad_timeouts.clone();

        Self::build(
            vad_timeouts,
            Arc::new(move || {
                vad_timeouts_for_callback.fetch_add(1, Ordering::SeqCst);
            }),
        )
    }

    fn new_with_silence_callback(silence_callback: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self::build(Arc::new(AtomicUsize::new(0)), silence_callback)
    }

    fn build(
        vad_timeouts: Arc<AtomicUsize>,
        silence_callback: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let callback_slot = Arc::new(Mutex::new(None));
        let manual_capture = Box::new(ManualCallbackAudioCapture::new(callback_slot.clone()));

        let mut vad_capture = VadCaptureWrapper::new(
            manual_capture,
            VadProcessor::new(Some(90), None).expect("VAD should initialize"),
        );
        vad_capture.set_silence_timeout_callback(silence_callback);

        let started_streams = Arc::new(AtomicUsize::new(0));
        let stopped_streams = Arc::new(AtomicUsize::new(0));
        let aborted_streams = Arc::new(AtomicUsize::new(0));
        let sent_chunks = Arc::new(AtomicUsize::new(0));
        let factory = Arc::new(CountingSttProviderFactory {
            started_streams: started_streams.clone(),
            stopped_streams: stopped_streams.clone(),
            aborted_streams: aborted_streams.clone(),
            sent_chunks: sent_chunks.clone(),
        });

        let service = TranscriptionService::new(Box::new(vad_capture), factory);

        Self {
            callback_slot,
            vad_timeouts,
            started_streams,
            stopped_streams,
            aborted_streams,
            sent_chunks,
            service,
        }
    }

    fn capture_callback(&self) -> AudioChunkCallback {
        self.callback_slot
            .lock()
            .expect("callback mutex poisoned")
            .clone()
            .expect("capture callback should be installed")
    }
}

#[tokio::test]
async fn vad_transcription_pipeline_ignores_stale_callback_after_restart() {
    let harness = VadPipelineHarness::new();

    start_service(&harness.service).await;
    let stale_callback = harness.capture_callback();
    harness
        .service
        .stop_recording()
        .await
        .expect("first stop should work");

    start_service(&harness.service).await;
    let current_callback = harness.capture_callback();

    stale_callback(active_then_silence_chunk());
    tokio::time::sleep(Duration::from_millis(120)).await;

    assert_eq!(
        harness.vad_timeouts.load(Ordering::SeqCst),
        0,
        "stale callback from the previous capture generation must not emit VAD timeout"
    );
    assert_eq!(
        harness.sent_chunks.load(Ordering::SeqCst),
        0,
        "stale callback should not enqueue audio into the restarted transcription pipeline"
    );

    current_callback(active_then_silence_chunk());
    wait_for_counter_at_least(&harness.vad_timeouts, 1).await;
    wait_for_counter_at_least(&harness.sent_chunks, 1).await;

    harness
        .service
        .stop_recording()
        .await
        .expect("second stop should work");

    assert_eq!(harness.started_streams.load(Ordering::SeqCst), 2);
    assert_eq!(harness.stopped_streams.load(Ordering::SeqCst), 2);
    assert_eq!(harness.aborted_streams.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn vad_transcription_pipeline_ignores_callback_after_manual_stop() {
    let harness = VadPipelineHarness::new();

    start_service(&harness.service).await;
    let stopped_callback = harness.capture_callback();
    harness
        .service
        .stop_recording()
        .await
        .expect("manual stop should work");

    stopped_callback(active_then_silence_chunk());
    tokio::time::sleep(Duration::from_millis(120)).await;

    assert_eq!(
        harness.vad_timeouts.load(Ordering::SeqCst),
        0,
        "callback retained by the audio layer after manual stop must not emit VAD timeout"
    );
    assert_eq!(
        harness.sent_chunks.load(Ordering::SeqCst),
        0,
        "callback retained by the audio layer after manual stop must not enqueue audio"
    );
    assert_eq!(harness.started_streams.load(Ordering::SeqCst), 1);
    assert_eq!(harness.stopped_streams.load(Ordering::SeqCst), 1);
    assert_eq!(harness.aborted_streams.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn vad_timeout_callback_reports_current_session_id_after_restart() {
    let active_session_id = Arc::new(AtomicU64::new(0));
    let (timeout_tx, mut timeout_rx) = tokio::sync::mpsc::unbounded_channel();
    let active_session_id_for_callback = active_session_id.clone();
    let harness = VadPipelineHarness::new_with_silence_callback(Arc::new(move || {
        let _ = timeout_tx.send(active_session_id_for_callback.load(Ordering::SeqCst));
    }));

    active_session_id.store(1, Ordering::SeqCst);
    start_service(&harness.service).await;
    let stale_callback = harness.capture_callback();
    harness
        .service
        .stop_recording()
        .await
        .expect("first stop should work");
    active_session_id.store(0, Ordering::SeqCst);

    active_session_id.store(2, Ordering::SeqCst);
    start_service(&harness.service).await;
    let current_callback = harness.capture_callback();

    stale_callback(active_then_silence_chunk());
    let stale_timeout = tokio::time::timeout(Duration::from_millis(120), timeout_rx.recv()).await;
    assert!(
        stale_timeout.is_err(),
        "stale callback from session 1 must not publish a VAD timeout for session 2"
    );

    current_callback(active_then_silence_chunk());
    let timeout_session_id = tokio::time::timeout(Duration::from_secs(1), timeout_rx.recv())
        .await
        .expect("current session should emit a VAD timeout")
        .expect("timeout channel should stay open");

    assert_eq!(
        timeout_session_id, 2,
        "VAD timeout callback must read the current active session id"
    );
    wait_for_counter_at_least(&harness.sent_chunks, 1).await;

    harness
        .service
        .stop_recording()
        .await
        .expect("second stop should work");

    assert_eq!(harness.started_streams.load(Ordering::SeqCst), 2);
    assert_eq!(harness.stopped_streams.load(Ordering::SeqCst), 2);
    assert_eq!(harness.aborted_streams.load(Ordering::SeqCst), 0);
}
