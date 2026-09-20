use super::*;
use crate::infrastructure::audio::{
    VadCaptureWrapper, VadProcessor, WarmDictationInput, WarmInputFormat, WarmNativeFactory,
    WarmNativeInput, WarmRawBlock, WarmRawCallback,
};

#[derive(Default)]
struct Source {
    callback: StdMutex<Option<WarmRawCallback>>,
    opens: AtomicUsize,
}
struct Factory(Arc<Source>);
struct Input(WarmRawCallback);
impl WarmNativeFactory for Factory {
    fn open(
        &mut self,
        raw: WarmRawCallback,
        _: crate::domain::AudioCaptureErrorCallback,
    ) -> AudioResult<Box<dyn WarmNativeInput>> {
        self.0.opens.fetch_add(1, Ordering::SeqCst);
        *self.0.callback.lock().unwrap() = Some(raw.clone());
        Ok(Box::new(Input(raw)))
    }
}
impl WarmNativeInput for Input {
    fn format(&self) -> WarmInputFormat {
        WarmInputFormat {
            sample_rate: 16_000,
            channels: 1,
            effective_name: "test input".into(),
        }
    }
    fn play(&self) -> AudioResult<()> {
        (self.0)(WarmRawBlock::I16(&[0; 1024]));
        (self.0)(WarmRawBlock::I16(&[0; 1024]));
        Ok(())
    }
    fn validate_route(&self) -> AudioResult<(bool, bool)> {
        Ok((true, true))
    }
}
fn vad(capture: Box<dyn AudioCapture>) -> Box<dyn AudioCapture> {
    Box::new(VadCaptureWrapper::new_with_microphone_sensitivity(
        capture,
        VadProcessor::new(Some(60_000), None).unwrap(),
        Arc::new(AtomicU8::new(100)),
    ))
}
fn service(capture: Box<dyn AudioCapture>) -> TranscriptionService {
    TranscriptionService::new(
        capture,
        Arc::new(TestFactory {
            panic_on_send: false,
            aborted: Arc::new(AtomicBool::new(false)),
        }),
    )
}
async fn prepare(service: &TranscriptionService, run: u64) -> PreparedCaptureToken {
    service
        .prepare_recording_capture(
            run,
            SttConfig::default(),
            Arc::new(|_, _| {}),
            Arc::new(|_, _| {}),
            Arc::new(|error| panic!("unexpected failure: {error}")),
        )
        .await
        .unwrap()
}
async fn queued_samples(service: &TranscriptionService) -> Vec<i16> {
    let mut slot = service.prepared_capture.lock().await;
    let prepared = slot.as_mut().unwrap();
    let mut data = Vec::new();
    while let Ok(chunk) = prepared.rx.try_recv() {
        data.extend(chunk.data);
    }
    data
}

#[tokio::test]
async fn warm_owner_vad_fifo_matches_cold_boundary_and_discards_episode_tail() {
    let source = Arc::new(Source::default());
    let owner = WarmDictationInput::with_factory(Box::new(Factory(source.clone()))).unwrap();
    owner.prewarm().await.unwrap();
    let raw = source.callback.lock().unwrap().as_ref().unwrap().clone();
    let warm = service(vad(Box::new(owner.lease())));
    let mut run = 1;
    for samples in [0, 479, 480, 481, 1023, 1024, 1025, 2048] {
        for marker in [1200, 0] {
            warm.replace_audio_capture_with_policy(
                vad(Box::new(owner.lease())),
                CaptureRecoveryPolicy::OwnerManaged,
            )
            .await
            .unwrap();
            let token = prepare(&warm, run).await;
            run += 1;
            raw(WarmRawBlock::I16(&[3000; 1024])); // explicit excluded crossing callback
            raw(WarmRawBlock::I16(&vec![marker; samples]));
            warm.seal_prepared_capture(token).await.unwrap();
            let actual = queued_samples(&warm).await;
            assert_eq!(actual.len(), (samples / 1024 * 1024) / 480 * 480);
            assert_eq!(
                warm.prepared_capture
                    .lock()
                    .await
                    .as_ref()
                    .unwrap()
                    .accounting
                    .report(AudioDrainReason::Drained)
                    .accepted_bytes,
                actual.len() as u64 * 2
            );
            assert!(actual.iter().all(|sample| *sample == marker));
            raw(WarmRawBlock::I16(&[9999; 2048])); // idle must not enter the sealed FIFO
            assert!(queued_samples(&warm).await.is_empty());
            warm.cancel_prepared_capture(token).await.unwrap();

            // Same legacy normalization output into the production VAD/FIFO.
            let callback = Arc::new(StdMutex::new(None));
            let cold = service(vad(Box::new(ManualAudioCapture::new(callback.clone()))));
            let cold_token = prepare(&cold, run).await;
            let emit = callback.lock().unwrap().as_ref().unwrap().clone();
            for _ in 0..samples / 1024 {
                emit(AudioChunk::new(vec![marker; 1024], 16_000, 1));
            }
            cold.seal_prepared_capture(cold_token).await.unwrap();
            assert_eq!(queued_samples(&cold).await, actual);
            cold.cancel_prepared_capture(cold_token).await.unwrap();
        }
    }
    assert_eq!(source.opens.load(Ordering::SeqCst), 1);
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn vad_fifo_479_480_481_preserves_only_complete_frames_and_policy_snapshot() {
    for count in [479usize, 480, 481] {
        let callback = Arc::new(StdMutex::new(None));
        let service = service(vad(Box::new(ManualAudioCapture::new(callback.clone()))));
        let token = prepare(&service, 1).await;
        let emit = callback.lock().unwrap().as_ref().unwrap().clone();
        emit(AudioChunk::new(vec![1200; count], 16_000, 1));
        assert_eq!(
            service
                .prepared_capture
                .lock()
                .await
                .as_ref()
                .unwrap()
                .recovery_policy,
            CaptureRecoveryPolicy::LegacyRestart
        );
        service.seal_prepared_capture(token).await.unwrap();
        emit(AudioChunk::new(vec![9999; 480], 16_000, 1));
        assert_eq!(
            queued_samples(&service).await,
            vec![1200; count / 480 * 480]
        );
        service.cancel_prepared_capture(token).await.unwrap();
    }
}

struct ObservedCapture {
    inner: Box<dyn AudioCapture>,
    starts: Arc<AtomicUsize>,
    stops: Arc<AtomicUsize>,
    delivery_barrier: Arc<
        StdMutex<
            Option<(
                tokio::sync::oneshot::Sender<()>,
                std::sync::mpsc::Receiver<()>,
            )>,
        >,
    >,
}
#[async_trait]
impl AudioCapture for ObservedCapture {
    async fn initialize(&mut self, config: AudioConfig) -> AudioResult<()> {
        self.inner.initialize(config).await
    }
    async fn start_capture(
        &mut self,
        callback: crate::domain::AudioChunkCallback,
    ) -> AudioResult<()> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        let barrier = self.delivery_barrier.clone();
        self.inner
            .start_capture(Arc::new(move |chunk| {
                let barrier = barrier.lock().unwrap().take();
                if let Some((entered, release)) = barrier {
                    let _ = entered.send(());
                    release.recv().unwrap();
                }
                callback(chunk);
            }))
            .await
    }
    async fn stop_capture(&mut self) -> AudioResult<()> {
        self.stops.fetch_add(1, Ordering::SeqCst);
        self.inner.stop_capture().await
    }
    fn set_capture_identity(&mut self, identity: Option<AudioCaptureIdentity>) {
        self.inner.set_capture_identity(identity);
    }
    fn set_terminal_error_callback(
        &mut self,
        callback: Option<crate::domain::AudioCaptureErrorCallback>,
    ) {
        self.inner.set_terminal_error_callback(callback);
    }
    fn is_capturing(&self) -> bool {
        self.inner.is_capturing()
    }
    fn config(&self) -> AudioConfig {
        self.inner.config()
    }
}
struct CountedFactory {
    inner: DrainTestFactory,
    creates: Arc<AtomicUsize>,
}
impl SttProviderFactory for CountedFactory {
    fn create(&self, config: &SttConfig) -> SttResult<Box<dyn SttProvider>> {
        self.creates.fetch_add(1, Ordering::SeqCst);
        self.inner.create(config)
    }
}

#[tokio::test]
async fn warm_stop_waits_for_permitted_fifo_delivery_and_a_drain_preserves_b() {
    let source = Arc::new(Source::default());
    let owner = WarmDictationInput::with_factory(Box::new(Factory(source.clone()))).unwrap();
    owner.prewarm().await.unwrap();
    let raw = source.callback.lock().unwrap().as_ref().unwrap().clone();
    let (entered_tx, entered) = tokio::sync::oneshot::channel();
    let (release, release_rx) = std::sync::mpsc::channel();
    let (sent_tx, mut sent) = tokio::sync::mpsc::unbounded_channel();
    let providers = Arc::new(AtomicUsize::new(0));
    let stops = Arc::new(AtomicUsize::new(0));
    let service = TranscriptionService::new(
        Box::new(ObservedCapture {
            inner: vad(Box::new(owner.lease())),
            starts: Arc::new(AtomicUsize::new(0)),
            stops: stops.clone(),
            delivery_barrier: Arc::new(StdMutex::new(Some((entered_tx, release_rx)))),
        }),
        Arc::new(CountedFactory {
            creates: providers.clone(),
            inner: DrainTestFactory {
                behavior: DrainTestSend::Record,
                start_gate: None,
                preferred_samples: None,
                sent: sent_tx,
                stops: Arc::new(AtomicUsize::new(0)),
                aborts: Arc::new(AtomicUsize::new(0)),
            },
        }),
    );
    *service.capture_recovery_policy.lock().unwrap() = CaptureRecoveryPolicy::OwnerManaged;
    let a = prepare(&service, 41).await;
    raw(WarmRawBlock::I16(&[9999; 1024]));
    let raw_a = raw.clone();
    let delivery = std::thread::spawn(move || raw_a(WarmRawBlock::I16(&[1200; 1024])));
    entered.await.unwrap(); // production owner permit is held inside VAD delivery
    let seal = service.seal_prepared_capture(a);
    tokio::pin!(seal);
    assert!(futures_util::poll!(seal.as_mut()).is_pending());
    assert!(service.has_capture_owner());
    assert_eq!(stops.load(Ordering::SeqCst), 1);
    release.send(()).unwrap();
    seal.await.unwrap();
    delivery.join().unwrap();
    let a_ledger = service
        .prepared_capture
        .lock()
        .await
        .as_ref()
        .unwrap()
        .accounting
        .clone();
    assert_eq!(
        a_ledger.report(AudioDrainReason::Drained).accepted_bytes,
        480 * 2
    );
    service
        .connect_prepared_recording(
            a,
            Arc::new(|_| {}),
            Arc::new(|_| {}),
            Arc::new(|_, _| {}),
            Arc::new(|_, _| {}),
            Arc::new(|e| panic!("{e}")),
            Arc::new(|_, _| {}),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
    service
        .replace_audio_capture_with_policy(
            vad(Box::new(owner.lease())),
            CaptureRecoveryPolicy::OwnerManaged,
        )
        .await
        .unwrap();
    raw(WarmRawBlock::I16(&[9999; 1024])); // fresh idle callback, discarded
    let b = prepare(&service, 42).await;
    raw(WarmRawBlock::I16(&[9999; 1024])); // boundary omission
    raw(WarmRawBlock::I16(&[2300; 1024]));
    service.seal_prepared_capture(b).await.unwrap();
    service.finalize_provider_for_run(a.run_id).await.unwrap();
    let report = a_ledger.report(AudioDrainReason::Drained);
    assert_eq!(
        (
            report.accepted_bytes,
            report.submitted_bytes,
            report.unknown_bytes
        ),
        (960, 960, 0)
    );
    let mut delivered = Vec::new();
    while let Ok(chunk) = sent.try_recv() {
        delivered.extend(chunk.data);
    }
    assert_eq!(delivered, vec![1200; 480]);
    assert_eq!(queued_samples(&service).await, vec![2300; 960]);
    assert_eq!(providers.load(Ordering::SeqCst), 1);
    assert_eq!(source.opens.load(Ordering::SeqCst), 1);
    service.cancel_prepared_capture(b).await.unwrap();
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn owner_managed_stall_terminates_once_while_legacy_still_restarts() {
    for policy in [
        CaptureRecoveryPolicy::OwnerManaged,
        CaptureRecoveryPolicy::LegacyRestart,
    ] {
        let starts = Arc::new(AtomicUsize::new(0));
        let stops = Arc::new(AtomicUsize::new(0));
        let errors = Arc::new(AtomicUsize::new(0));
        let aborted = Arc::new(AtomicBool::new(false));
        let service = TranscriptionService::new(
            Box::new(ObservedCapture {
                inner: Box::new(ManualAudioCapture::new(Arc::new(StdMutex::new(None)))),
                starts: starts.clone(),
                stops: stops.clone(),
                delivery_barrier: Arc::new(StdMutex::new(None)),
            }),
            Arc::new(TestFactory {
                panic_on_send: false,
                aborted: aborted.clone(),
            }),
        );
        *service.capture_recovery_policy.lock().unwrap() = policy;
        let observed_errors = errors.clone();
        service
            .start_recording(
                Arc::new(|_| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                Arc::new(move |_| {
                    observed_errors.fetch_add(1, Ordering::SeqCst);
                }),
                Arc::new(|_, _| {}),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if starts.load(Ordering::SeqCst) > 1 || aborted.load(Ordering::SeqCst) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        match policy {
            CaptureRecoveryPolicy::OwnerManaged => {
                assert_eq!(
                    (
                        starts.load(Ordering::SeqCst),
                        stops.load(Ordering::SeqCst),
                        errors.load(Ordering::SeqCst)
                    ),
                    (1, 1, 1)
                );
                assert_eq!(service.get_status().await, RecordingStatus::Idle);
                assert!(service.stt_provider.read().await.is_none());
            }
            CaptureRecoveryPolicy::LegacyRestart => {
                assert_eq!(starts.load(Ordering::SeqCst), 2);
                assert_eq!(errors.load(Ordering::SeqCst), 0);
                assert_eq!(service.get_status().await, RecordingStatus::Recording);
            }
        }
        service.cleanup_runtime_failure("test cleanup").await;
    }
}
