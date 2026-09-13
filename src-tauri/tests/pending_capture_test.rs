use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use app_lib::application::services::{PreparedCaptureToken, TranscriptionService};
use app_lib::domain::{
    AudioCapture, AudioChunk, AudioChunkCallback, AudioConfig, AudioError, AudioLevelCallback,
    AudioSpectrumCallback, ConnectionQualityCallback, ErrorCallback, RecordingStatus, SttConfig,
    SttError, SttProvider, SttProviderFactory, SttResult, Transcription, TranscriptionCallback,
};
use async_trait::async_trait;
use tokio::sync::Semaphore;

#[derive(Default)]
struct CaptureState {
    callbacks: Mutex<Vec<AudioChunkCallback>>,
    active: AtomicBool,
    fail_stop: AtomicBool,
    starts: AtomicUsize,
    stops: AtomicUsize,
}

impl CaptureState {
    fn emit_current(&self, marker: i16) {
        let callback = self
            .callbacks
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("capture callback");
        callback(AudioChunk::new(vec![marker], 16_000, 1));
    }
}

struct ControlledCapture {
    state: Arc<CaptureState>,
    config: AudioConfig,
}

#[async_trait]
impl AudioCapture for ControlledCapture {
    async fn initialize(&mut self, config: AudioConfig) -> app_lib::domain::AudioResult<()> {
        self.config = config;
        Ok(())
    }

    async fn start_capture(
        &mut self,
        callback: AudioChunkCallback,
    ) -> app_lib::domain::AudioResult<()> {
        if self.state.active.swap(true, Ordering::SeqCst) {
            return Err(AudioError::Capture("capture already active".into()));
        }
        self.state.callbacks.lock().unwrap().push(callback);
        self.state.starts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn stop_capture(&mut self) -> app_lib::domain::AudioResult<()> {
        self.state.stops.fetch_add(1, Ordering::SeqCst);
        if self.state.fail_stop.load(Ordering::SeqCst) {
            return Err(AudioError::Capture("injected stop failure".into()));
        }
        self.state.active.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn is_capturing(&self) -> bool {
        self.state.active.load(Ordering::SeqCst)
    }

    fn config(&self) -> AudioConfig {
        self.config
    }
}

struct ProviderState {
    created: AtomicUsize,
    active: AtomicUsize,
    max_active: AtomicUsize,
    stopped: AtomicUsize,
    aborted: AtomicUsize,
    dropped: AtomicUsize,
    sent: Mutex<Vec<(usize, i16)>>,
    events: Mutex<Vec<String>>,
    stop_tails: Mutex<VecDeque<Option<String>>>,
    fail_next_start_auth: AtomicBool,
    block_next_start: AtomicBool,
    start_release: Semaphore,
    block_next_stop: AtomicBool,
    stop_release: Semaphore,
}

impl Default for ProviderState {
    fn default() -> Self {
        Self {
            created: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            stopped: AtomicUsize::new(0),
            aborted: AtomicUsize::new(0),
            dropped: AtomicUsize::new(0),
            sent: Mutex::new(Vec::new()),
            events: Mutex::new(Vec::new()),
            stop_tails: Mutex::new(VecDeque::new()),
            fail_next_start_auth: AtomicBool::new(false),
            block_next_start: AtomicBool::new(false),
            start_release: Semaphore::new(0),
            block_next_stop: AtomicBool::new(false),
            stop_release: Semaphore::new(0),
        }
    }
}

impl ProviderState {
    fn mark_active(&self) {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
    }
}

struct ControlledProvider {
    id: usize,
    state: Arc<ProviderState>,
    active: bool,
    on_final: Option<TranscriptionCallback>,
    tail: Option<String>,
}

impl Drop for ControlledProvider {
    fn drop(&mut self) {
        self.state.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl SttProvider for ControlledProvider {
    async fn initialize(&mut self, _config: &SttConfig) -> SttResult<()> {
        Ok(())
    }

    async fn start_stream(
        &mut self,
        _on_partial: TranscriptionCallback,
        on_final: TranscriptionCallback,
        _on_error: ErrorCallback,
        _on_quality: ConnectionQualityCallback,
    ) -> SttResult<()> {
        self.state
            .events
            .lock()
            .unwrap()
            .push(format!("start:{}", self.id));
        if self.state.block_next_start.swap(false, Ordering::SeqCst) {
            self.state.start_release.acquire().await.unwrap().forget();
        }
        if self
            .state
            .fail_next_start_auth
            .swap(false, Ordering::SeqCst)
        {
            return Err(SttError::Authentication("injected auth rejection".into()));
        }
        self.on_final = Some(on_final);
        self.active = true;
        self.state.mark_active();
        Ok(())
    }

    async fn send_audio(&mut self, chunk: &AudioChunk) -> SttResult<()> {
        let marker = *chunk.data.first().unwrap_or(&0);
        self.state.sent.lock().unwrap().push((self.id, marker));
        self.state
            .events
            .lock()
            .unwrap()
            .push(format!("audio:{}:{marker}", self.id));
        Ok(())
    }

    async fn stop_stream(&mut self) -> SttResult<()> {
        self.state
            .events
            .lock()
            .unwrap()
            .push(format!("stop-begin:{}", self.id));
        if self.state.block_next_stop.swap(false, Ordering::SeqCst) {
            self.state.stop_release.acquire().await.unwrap().forget();
        }
        if let (Some(callback), Some(tail)) = (&self.on_final, self.tail.take()) {
            callback(Transcription::final_result(tail));
        }
        if std::mem::take(&mut self.active) {
            self.state.active.fetch_sub(1, Ordering::SeqCst);
        }
        self.state.stopped.fetch_add(1, Ordering::SeqCst);
        self.state
            .events
            .lock()
            .unwrap()
            .push(format!("stop-end:{}", self.id));
        Ok(())
    }

    async fn abort(&mut self) -> SttResult<()> {
        if std::mem::take(&mut self.active) {
            self.state.active.fetch_sub(1, Ordering::SeqCst);
        }
        self.state.aborted.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn name(&self) -> &str {
        "controlled"
    }
    fn is_online(&self) -> bool {
        true
    }
}

struct ControlledFactory(Arc<ProviderState>);

impl SttProviderFactory for ControlledFactory {
    fn create(&self, _config: &SttConfig) -> SttResult<Box<dyn SttProvider>> {
        let id = self.0.created.fetch_add(1, Ordering::SeqCst) + 1;
        let tail = self.0.stop_tails.lock().unwrap().pop_front().flatten();
        Ok(Box::new(ControlledProvider {
            id,
            state: self.0.clone(),
            active: false,
            on_final: None,
            tail,
        }))
    }
}

struct Harness {
    service: Arc<TranscriptionService>,
    capture: Arc<CaptureState>,
    provider: Arc<ProviderState>,
}

impl Harness {
    fn new() -> Self {
        let capture = Arc::new(CaptureState::default());
        let provider = Arc::new(ProviderState::default());
        let service = Arc::new(TranscriptionService::new(
            Box::new(ControlledCapture {
                state: capture.clone(),
                config: AudioConfig::default(),
            }),
            Arc::new(ControlledFactory(provider.clone())),
        ));
        Self {
            service,
            capture,
            provider,
        }
    }

    async fn prepare(&self, run_id: u64, on_error: ErrorCallback) -> PreparedCaptureToken {
        self.service
            .prepare_recording_capture(
                run_id,
                SttConfig::default(),
                noop_level(),
                noop_spectrum(),
                on_error,
            )
            .await
            .expect("prepare")
    }

    async fn connect(
        &self,
        token: PreparedCaptureToken,
        on_final: TranscriptionCallback,
    ) -> anyhow::Result<()> {
        self.service
            .connect_prepared_recording(
                token,
                noop_transcript(),
                on_final,
                noop_level(),
                noop_spectrum(),
                noop_error(),
                noop_quality(),
                Arc::new(AtomicBool::new(false)),
            )
            .await
    }
}

fn noop_transcript() -> TranscriptionCallback {
    Arc::new(|_| {})
}
fn noop_level() -> AudioLevelCallback {
    Arc::new(|_, _| {})
}
fn noop_spectrum() -> AudioSpectrumCallback {
    Arc::new(|_, _| {})
}
fn noop_error() -> ErrorCallback {
    Arc::new(|_| {})
}
fn noop_quality() -> ConnectionQualityCallback {
    Arc::new(|_, _| {})
}

async fn eventually(condition: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("condition timed out");
}

#[tokio::test]
async fn prepared_audio_is_flushed_fifo_exactly_once_then_live_audio_continues() {
    let h = Harness::new();
    let token = h.prepare(1, noop_error()).await;
    for marker in [11, 22, 33] {
        h.capture.emit_current(marker);
    }
    h.connect(token, noop_transcript()).await.unwrap();
    h.capture.emit_current(44);
    eventually(|| h.provider.sent.lock().unwrap().len() == 4).await;
    assert_eq!(
        *h.provider.sent.lock().unwrap(),
        vec![(1, 11), (1, 22), (1, 33), (1, 44)]
    );
    h.service.stop_capture_for_run(1).await.unwrap();
    h.service.finalize_provider_for_run(1).await.unwrap();
}

#[tokio::test]
async fn old_tail_finishes_before_new_provider_and_never_mixes_sessions() {
    let h = Harness::new();
    h.provider
        .stop_tails
        .lock()
        .unwrap()
        .push_back(Some("old-tail".into()));
    h.provider
        .stop_tails
        .lock()
        .unwrap()
        .push_back(Some("new-tail".into()));
    let old_finals = Arc::new(Mutex::new(Vec::<String>::new()));
    let new_finals = Arc::new(Mutex::new(Vec::<String>::new()));

    let one = h.prepare(1, noop_error()).await;
    h.connect(one, {
        let out = old_finals.clone();
        Arc::new(move |t| out.lock().unwrap().push(t.text))
    })
    .await
    .unwrap();
    h.capture.emit_current(101);
    h.service.stop_capture_for_run(1).await.unwrap();
    let two = h.prepare(2, noop_error()).await;
    h.capture.emit_current(202);

    h.provider.block_next_stop.store(true, Ordering::SeqCst);
    let service = h.service.clone();
    let finalize = tokio::spawn(async move { service.finalize_provider_for_run(1).await });
    eventually(|| {
        h.provider
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|e| e == "stop-begin:1")
    })
    .await;
    let service = h.service.clone();
    let new_sink = new_finals.clone();
    let connect = tokio::spawn(async move {
        service
            .connect_prepared_recording(
                two,
                noop_transcript(),
                Arc::new(move |t| new_sink.lock().unwrap().push(t.text)),
                noop_level(),
                noop_spectrum(),
                noop_error(),
                noop_quality(),
                Arc::new(AtomicBool::new(false)),
            )
            .await
    });
    tokio::task::yield_now().await;
    assert_eq!(h.provider.created.load(Ordering::SeqCst), 1);
    assert_eq!(h.provider.max_active.load(Ordering::SeqCst), 1);
    h.provider.stop_release.add_permits(1);
    finalize.await.unwrap().unwrap();
    connect.await.unwrap().unwrap();
    eventually(|| h.provider.sent.lock().unwrap().contains(&(2, 202))).await;
    assert_eq!(*old_finals.lock().unwrap(), vec!["old-tail"]);
    assert!(new_finals.lock().unwrap().is_empty());
    assert_eq!(h.provider.max_active.load(Ordering::SeqCst), 1);
    h.service.finalize_provider_for_run(1).await.unwrap();
    assert!(h.service.capture_is_active_for_run(2).await);
    assert_eq!(h.provider.active.load(Ordering::SeqCst), 1);
    let events = h.provider.events.lock().unwrap().clone();
    assert!(
        events.iter().position(|e| e == "stop-end:1").unwrap()
            < events.iter().position(|e| e == "start:2").unwrap()
    );
    h.service.stop_capture_for_run(2).await.unwrap();
    h.service.finalize_provider_for_run(2).await.unwrap();
    assert_eq!(*new_finals.lock().unwrap(), vec!["new-tail"]);
}

#[tokio::test]
async fn concurrent_prepare_has_one_owner_and_forged_token_cannot_steal_it() {
    let h = Harness::new();
    let a = {
        let s = h.service.clone();
        tokio::spawn(async move {
            s.prepare_recording_capture(
                10,
                SttConfig::default(),
                noop_level(),
                noop_spectrum(),
                noop_error(),
            )
            .await
        })
    };
    let b = {
        let s = h.service.clone();
        tokio::spawn(async move {
            s.prepare_recording_capture(
                20,
                SttConfig::default(),
                noop_level(),
                noop_spectrum(),
                noop_error(),
            )
            .await
        })
    };
    let (a, b) = tokio::join!(a, b);
    let results = [a.unwrap(), b.unwrap()];
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    let winner = results.into_iter().find_map(Result::ok).unwrap();
    let loser = if winner.run_id == 10 { 20 } else { 10 };
    assert!(h.service.stop_capture_for_run(loser).await.is_err());
    let forged = PreparedCaptureToken {
        run_id: winner.run_id,
        generation: winner.generation + 1,
    };
    assert!(h.connect(forged, noop_transcript()).await.is_err());
    assert!(h.service.capture_token_is_current(winner));
    h.service.stop_capture_for_run(winner.run_id).await.unwrap();
}

#[tokio::test]
async fn cancellation_while_provider_start_is_blocked_releases_capture_promptly() {
    let h = Harness::new();
    let token = h.prepare(7, noop_error()).await;
    h.provider.block_next_start.store(true, Ordering::SeqCst);
    let cancelled = Arc::new(AtomicBool::new(false));
    let service = h.service.clone();
    let flag = cancelled.clone();
    let connect = tokio::spawn(async move {
        service
            .connect_prepared_recording(
                token,
                noop_transcript(),
                noop_transcript(),
                noop_level(),
                noop_spectrum(),
                noop_error(),
                noop_quality(),
                flag,
            )
            .await
    });
    eventually(|| {
        h.provider
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|e| e == "start:1")
    })
    .await;
    cancelled.store(true, Ordering::SeqCst);
    let result = tokio::time::timeout(Duration::from_millis(500), connect)
        .await
        .expect("cancel should not wait for provider timeout")
        .unwrap();
    assert!(result.is_err());
    assert!(!h.capture.active.load(Ordering::SeqCst));
    assert!(!h.service.capture_token_is_current(token));
    assert_eq!(
        h.provider.aborted.load(Ordering::SeqCst),
        1,
        "cancelled startup must explicitly abort the provider"
    );
    assert_eq!(h.provider.active.load(Ordering::SeqCst), 0);
    assert_eq!(h.provider.dropped.load(Ordering::SeqCst), 1);
    assert_eq!(h.service.get_status().await, RecordingStatus::Idle);

    let recovered = h.prepare(8, noop_error()).await;
    h.capture.emit_current(808);
    h.connect(recovered, noop_transcript()).await.unwrap();
    eventually(|| h.provider.sent.lock().unwrap().contains(&(2, 808))).await;
    h.service.stop_capture_for_run(8).await.unwrap();
    h.service.finalize_provider_for_run(8).await.unwrap();
    assert_eq!(h.service.get_status().await, RecordingStatus::Idle);
    assert!(!h.capture.active.load(Ordering::SeqCst));
    assert_eq!(h.provider.active.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn overflowing_thirty_second_fifo_reports_once_and_cleans_up() {
    let h = Harness::new();
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let token = h
        .prepare(9, {
            let errors = errors.clone();
            Arc::new(move |e| errors.lock().unwrap().push(e.to_string()))
        })
        .await;
    let callback = h.capture.callbacks.lock().unwrap().last().cloned().unwrap();
    callback(AudioChunk::new(vec![1; 480_000], 16_000, 1));
    assert!(
        errors.lock().unwrap().is_empty(),
        "exactly 30 seconds fits the PCM budget"
    );
    assert!(h.service.capture_token_is_current(token));
    callback(AudioChunk::new(vec![2; 1], 16_000, 1));
    callback(AudioChunk::new(vec![3; 480], 16_000, 1));
    assert_eq!(
        errors.lock().unwrap().len(),
        1,
        "overflow must be explicit and exactly once"
    );
    assert!(h.connect(token, noop_transcript()).await.is_err());
    assert!(!h.capture.active.load(Ordering::SeqCst));
    assert!(!h.service.capture_token_is_current(token));
}

#[tokio::test]
async fn authentication_failure_discards_buffer_and_next_run_recovers() {
    let h = Harness::new();
    h.provider
        .fail_next_start_auth
        .store(true, Ordering::SeqCst);
    let failed = h.prepare(41, noop_error()).await;
    h.capture.emit_current(411);

    let error = h.connect(failed, noop_transcript()).await.unwrap_err();
    assert!(format!("{error:#}").contains("injected auth rejection"));
    assert!(h.provider.sent.lock().unwrap().is_empty());
    assert!(!h.capture.active.load(Ordering::SeqCst));
    assert_eq!(h.provider.aborted.load(Ordering::SeqCst), 1);
    assert_eq!(h.provider.active.load(Ordering::SeqCst), 0);
    assert!(!h.service.capture_token_is_current(failed));
    assert!(h.connect(failed, noop_transcript()).await.is_err());

    let recovered = h.prepare(42, noop_error()).await;
    h.capture.emit_current(422);
    h.connect(recovered, noop_transcript()).await.unwrap();
    eventually(|| h.provider.sent.lock().unwrap().contains(&(2, 422))).await;
    assert!(h.service.capture_is_active_for_run(42).await);
    h.service.stop_capture_for_run(42).await.unwrap();
    h.service.finalize_provider_for_run(42).await.unwrap();
}

#[tokio::test]
async fn failed_stop_that_leaves_capture_active_blocks_successor() {
    let h = Harness::new();
    let token = h.prepare(1, noop_error()).await;
    h.capture.fail_stop.store(true, Ordering::SeqCst);
    assert!(h.service.stop_capture_for_run(1).await.is_err());
    assert!(h.capture.active.load(Ordering::SeqCst));
    assert!(h.service.capture_token_is_current(token));
    assert!(h
        .service
        .prepare_recording_capture(
            2,
            SttConfig::default(),
            noop_level(),
            noop_spectrum(),
            noop_error()
        )
        .await
        .is_err());
    h.capture.fail_stop.store(false, Ordering::SeqCst);
    h.service.stop_capture_for_run(1).await.unwrap();
}

#[tokio::test]
async fn thirty_rapid_cycles_leave_no_capture_provider_or_token() {
    let h = Harness::new();
    for run_id in 1..=30 {
        let token = h.prepare(run_id, noop_error()).await;
        h.capture.emit_current(run_id as i16);
        h.connect(token, noop_transcript()).await.unwrap();
        h.service.stop_capture_for_run(run_id).await.unwrap();
        h.service.finalize_provider_for_run(run_id).await.unwrap();
    }
    assert_eq!(h.capture.starts.load(Ordering::SeqCst), 30);
    assert_eq!(h.capture.stops.load(Ordering::SeqCst), 30);
    assert_eq!(h.provider.created.load(Ordering::SeqCst), 30);
    assert_eq!(h.provider.stopped.load(Ordering::SeqCst), 30);
    assert_eq!(h.provider.active.load(Ordering::SeqCst), 0);
    assert_eq!(h.provider.max_active.load(Ordering::SeqCst), 1);
    assert!(!h.capture.active.load(Ordering::SeqCst));
}
