use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{mpsc, Notify};

use crate::application::services::IncomingSpokenTranslationPorts;
use crate::domain::{
    AudioCapture, AudioChunk, AudioChunkCallback, AudioConfig, AudioEnqueueOutcome, AudioError,
    AudioResult, LocalPlaybackOutputFactory, LocalPlaybackRoute, RealtimeTranslationConfig,
    RealtimeTranslationError, RealtimeTranslationEvent, RealtimeTranslationFactory,
    RealtimeTranslationSession, SpokenIncomingCapability, SpokenTranslationCapability,
    SystemAudioCaptureFactory, SystemAudioCaptureRequest, TranslationAudioOutput,
    TranslationAudioOutputConfig, TranslationAudioOutputResult,
};

const EXPECTED_TRANSLATED_SAMPLE: i16 = 1_200;
const EXPECTED_TRANSLATED_SAMPLES: usize = 2_400;
const EXPECTED_TRANSLATED_SAMPLE_RATE: u32 = 24_000;
const EXPECTED_TRANSLATED_CHANNELS: u16 = 1;

pub(super) fn spoken_translation_ports() -> IncomingSpokenTranslationPorts {
    let harness = Arc::new(E2eHarness::default());
    IncomingSpokenTranslationPorts::new(
        Arc::new(E2eCaptureFactory),
        Arc::new(E2eOutputFactory {
            harness: harness.clone(),
        }),
        Arc::new(E2eTranslationFactory { harness }),
        Arc::new(E2eCapability),
    )
}

#[derive(Default)]
struct E2eHarness {
    output: Mutex<Option<Arc<Mutex<E2eOutputState>>>>,
    audio_enqueued: Notify,
}

impl E2eHarness {
    fn install_output(&self) -> TranslationAudioOutputResult<Arc<Mutex<E2eOutputState>>> {
        let mut output = self.output.lock().map_err(|_| {
            crate::domain::TranslationAudioOutputError::Stream(
                "E2E output lifecycle state is poisoned".into(),
            )
        })?;
        if let Some(previous) = output.as_ref() {
            previous
                .lock()
                .map_err(|_| {
                    crate::domain::TranslationAudioOutputError::Stream(
                        "E2E previous output state is poisoned".into(),
                    )
                })?
                .validate_completed()?;
        }

        let state = Arc::new(Mutex::new(E2eOutputState::default()));
        *output = Some(state.clone());
        Ok(state)
    }

    async fn wait_for_audio_enqueue(&self) -> Result<(), RealtimeTranslationError> {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let notified = self.audio_enqueued.notified();
                let enqueued = self
                    .output
                    .lock()
                    .ok()
                    .and_then(|output| output.clone())
                    .and_then(|output| output.lock().ok().map(|state| state.audio_enqueued))
                    .unwrap_or(false);
                if enqueued {
                    return;
                }
                notified.await;
            }
        })
        .await
        .map_err(|_| {
            RealtimeTranslationError::Internal(
                "E2E translated PCM was not enqueued before translated text".into(),
            )
        })
    }
}

struct E2eCapability;

impl SpokenTranslationCapability for E2eCapability {
    fn check(&self, _target_language: &str) -> SpokenIncomingCapability {
        SpokenIncomingCapability::Ready
    }
}

struct E2eCaptureFactory;

impl SystemAudioCaptureFactory for E2eCaptureFactory {
    fn preflight_system_audio_capture(
        &self,
        request: SystemAudioCaptureRequest,
    ) -> AudioResult<()> {
        if request.target.sample_rate != 24_000 || request.target.channels != 1 {
            return Err(AudioError::Configuration(
                "E2E spoken capture requires 24 kHz mono".into(),
            ));
        }
        Ok(())
    }

    fn create_system_audio_capture(
        &self,
        request: SystemAudioCaptureRequest,
    ) -> AudioResult<Box<dyn AudioCapture>> {
        self.preflight_system_audio_capture(request)?;
        Ok(Box::new(E2eCapture {
            config: AudioConfig::default(),
            capturing: false,
            callback_delivered: false,
        }))
    }
}

struct E2eCapture {
    config: AudioConfig,
    capturing: bool,
    callback_delivered: bool,
}

#[async_trait]
impl AudioCapture for E2eCapture {
    async fn initialize(&mut self, config: AudioConfig) -> AudioResult<()> {
        self.config = config;
        Ok(())
    }

    async fn start_capture(&mut self, on_chunk: AudioChunkCallback) -> AudioResult<()> {
        if self.capturing || self.callback_delivered {
            return Err(AudioError::Capture(
                "E2E capture can only be started once".into(),
            ));
        }
        self.capturing = true;
        if self.capturing {
            on_chunk(AudioChunk::new(
                vec![800; 4_800],
                self.config.sample_rate,
                self.config.channels,
            ));
            self.callback_delivered = true;
        }
        Ok(())
    }

    async fn stop_capture(&mut self) -> AudioResult<()> {
        self.capturing = false;
        Ok(())
    }

    fn is_capturing(&self) -> bool {
        self.capturing
    }

    fn config(&self) -> AudioConfig {
        self.config
    }
}

struct E2eOutputFactory {
    harness: Arc<E2eHarness>,
}

impl LocalPlaybackOutputFactory for E2eOutputFactory {
    fn create_local_playback_output(
        &self,
        route: LocalPlaybackRoute,
    ) -> TranslationAudioOutputResult<Box<dyn TranslationAudioOutput>> {
        if route != LocalPlaybackRoute::SystemDefault {
            return Err(crate::domain::TranslationAudioOutputError::Configuration(
                "E2E spoken output requires the system default route".into(),
            ));
        }
        Ok(Box::new(E2eOutput {
            harness: self.harness.clone(),
            state: self.harness.install_output()?,
        }))
    }
}

struct E2eOutput {
    harness: Arc<E2eHarness>,
    state: Arc<Mutex<E2eOutputState>>,
}

#[derive(Default)]
struct E2eOutputState {
    open: bool,
    closed: bool,
    gain: f32,
    queued_samples: usize,
    pending_samples: usize,
    drained_samples: usize,
    audio_enqueued: bool,
    drain_mode: bool,
    drain_prepared: bool,
}

impl E2eOutputState {
    fn pending_duration(&self) -> Duration {
        Duration::from_secs_f64(
            self.pending_samples as f64
                / f64::from(EXPECTED_TRANSLATED_SAMPLE_RATE)
                / f64::from(EXPECTED_TRANSLATED_CHANNELS),
        )
    }

    fn validate_completed(&self) -> TranslationAudioOutputResult<()> {
        if self.closed
            && !self.open
            && self.audio_enqueued
            && self.queued_samples == EXPECTED_TRANSLATED_SAMPLES
            && self.pending_samples == 0
            && self.drained_samples == EXPECTED_TRANSLATED_SAMPLES
            && self.drain_mode
            && self.drain_prepared
        {
            return Ok(());
        }

        Err(crate::domain::TranslationAudioOutputError::Stream(
            "E2E previous output did not enqueue, drain, and close translated PCM".into(),
        ))
    }
}

#[async_trait]
impl TranslationAudioOutput for E2eOutput {
    async fn open(
        &mut self,
        config: TranslationAudioOutputConfig,
    ) -> TranslationAudioOutputResult<()> {
        if config.source_sample_rate != EXPECTED_TRANSLATED_SAMPLE_RATE
            || config.source_channels != EXPECTED_TRANSLATED_CHANNELS
        {
            return Err(crate::domain::TranslationAudioOutputError::Configuration(
                "E2E spoken output requires 24 kHz mono PCM".into(),
            ));
        }
        let mut state = self.state.lock().map_err(|_| {
            crate::domain::TranslationAudioOutputError::Stream(
                "E2E output state is poisoned".into(),
            )
        })?;
        if state.open || state.closed {
            return Err(crate::domain::TranslationAudioOutputError::Stream(
                "E2E output can only be opened once".into(),
            ));
        }
        state.open = true;
        state.gain = config.gain;
        Ok(())
    }

    async fn enqueue_pcm16(
        &self,
        samples: &[i16],
    ) -> TranslationAudioOutputResult<AudioEnqueueOutcome> {
        let pending = {
            let mut state = self.state.lock().map_err(|_| {
                crate::domain::TranslationAudioOutputError::Stream(
                    "E2E output state is poisoned".into(),
                )
            })?;
            if !state.open || state.closed {
                return Err(crate::domain::TranslationAudioOutputError::Closed);
            }
            if state.audio_enqueued
                || samples.len() != EXPECTED_TRANSLATED_SAMPLES
                || samples
                    .iter()
                    .any(|sample| *sample != EXPECTED_TRANSLATED_SAMPLE)
            {
                return Err(crate::domain::TranslationAudioOutputError::Stream(
                    "E2E output received unexpected translated PCM".into(),
                ));
            }
            state.audio_enqueued = true;
            state.queued_samples = samples.len();
            state.pending_samples = samples.len();
            state.pending_duration()
        };
        self.harness.audio_enqueued.notify_one();
        Ok(AudioEnqueueOutcome::Queued { pending })
    }

    async fn close(&mut self) -> TranslationAudioOutputResult<()> {
        let mut state = self.state.lock().map_err(|_| {
            crate::domain::TranslationAudioOutputError::Stream(
                "E2E output state is poisoned".into(),
            )
        })?;
        if !state.open || state.closed {
            return Err(crate::domain::TranslationAudioOutputError::Closed);
        }
        if !state.audio_enqueued
            || !state.drain_mode
            || !state.drain_prepared
            || state.pending_samples != 0
            || state.drained_samples != state.queued_samples
        {
            return Err(crate::domain::TranslationAudioOutputError::Stream(
                "E2E output closed before translated PCM completed".into(),
            ));
        }
        state.open = false;
        state.closed = true;
        Ok(())
    }

    fn set_gain(&mut self, gain: f32) -> TranslationAudioOutputResult<()> {
        let mut state = self.state.lock().map_err(|_| {
            crate::domain::TranslationAudioOutputError::Stream(
                "E2E output state is poisoned".into(),
            )
        })?;
        if !state.open || state.closed {
            return Err(crate::domain::TranslationAudioOutputError::Closed);
        }
        state.gain = gain;
        Ok(())
    }

    fn is_open(&self) -> bool {
        self.state
            .lock()
            .map(|state| state.open && !state.closed)
            .unwrap_or(false)
    }

    fn device_name(&self) -> Option<String> {
        Some("e2e-system-default".into())
    }

    fn begin_drain_mode(&self) {
        if let Ok(mut state) = self.state.lock() {
            if state.open && !state.closed {
                state.drain_mode = true;
            }
        }
    }

    fn prepare_for_drain(&self) -> TranslationAudioOutputResult<Duration> {
        let mut state = self.state.lock().map_err(|_| {
            crate::domain::TranslationAudioOutputError::Stream(
                "E2E output state is poisoned".into(),
            )
        })?;
        if !state.open || state.closed {
            return Err(crate::domain::TranslationAudioOutputError::Closed);
        }
        if !state.audio_enqueued || !state.drain_mode {
            return Err(crate::domain::TranslationAudioOutputError::Stream(
                "E2E output drain started before translated PCM was queued".into(),
            ));
        }
        let pending = state.pending_duration();
        state.drain_prepared = true;
        state.drained_samples = state.drained_samples.saturating_add(state.pending_samples);
        state.pending_samples = 0;
        Ok(pending)
    }

    fn pending_playback_duration(&self) -> Duration {
        self.state
            .lock()
            .map(|state| state.pending_duration())
            .unwrap_or(Duration::MAX)
    }
}

struct E2eTranslationFactory {
    harness: Arc<E2eHarness>,
}

impl RealtimeTranslationFactory for E2eTranslationFactory {
    fn create(&self) -> Box<dyn RealtimeTranslationSession> {
        Box::new(E2eTranslationSession {
            harness: self.harness.clone(),
            events: None,
            emitted: false,
        })
    }
}

struct E2eTranslationSession {
    harness: Arc<E2eHarness>,
    events: Option<mpsc::Sender<RealtimeTranslationEvent>>,
    emitted: bool,
}

#[async_trait]
impl RealtimeTranslationSession for E2eTranslationSession {
    async fn connect(
        &mut self,
        _config: RealtimeTranslationConfig,
    ) -> Result<mpsc::Receiver<RealtimeTranslationEvent>, RealtimeTranslationError> {
        let (events, receiver) = mpsc::channel(8);
        self.events = Some(events);
        Ok(receiver)
    }

    async fn append_pcm16(&mut self, _samples: &[i16]) -> Result<(), RealtimeTranslationError> {
        if self.emitted {
            return Ok(());
        }
        self.emitted = true;
        let events = self.events.as_ref().ok_or_else(|| {
            RealtimeTranslationError::Internal("E2E session is not connected".into())
        })?;
        events
            .send(RealtimeTranslationEvent::TranslatedAudio {
                pcm16: vec![EXPECTED_TRANSLATED_SAMPLE; EXPECTED_TRANSLATED_SAMPLES],
                sample_rate: EXPECTED_TRANSLATED_SAMPLE_RATE,
                channels: EXPECTED_TRANSLATED_CHANNELS,
            })
            .await
            .map_err(|_| {
                RealtimeTranslationError::Connection("E2E event receiver closed".into())
            })?;
        self.harness.wait_for_audio_enqueue().await?;
        events
            .send(RealtimeTranslationEvent::SourceTextDelta(
                "hello from e2e call".into(),
            ))
            .await
            .map_err(|_| {
                RealtimeTranslationError::Connection("E2E event receiver closed".into())
            })?;
        events
            .send(RealtimeTranslationEvent::TranslatedTextDelta(
                "привет из e2e звонка".into(),
            ))
            .await
            .map_err(|_| {
                RealtimeTranslationError::Connection("E2E event receiver closed".into())
            })?;
        Ok(())
    }

    async fn finish(&mut self, _timeout: Duration) -> Result<(), RealtimeTranslationError> {
        if let Some(events) = self.events.take() {
            let _ = events.send(RealtimeTranslationEvent::Closed).await;
        }
        Ok(())
    }

    async fn abort(&mut self) {
        self.events = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn capture_emits_once_and_never_after_stop() {
        let callbacks = Arc::new(AtomicUsize::new(0));
        let callback_count = callbacks.clone();
        let callback: AudioChunkCallback = Arc::new(move |_| {
            callback_count.fetch_add(1, Ordering::SeqCst);
        });
        let mut capture = E2eCapture {
            config: AudioConfig::default(),
            capturing: false,
            callback_delivered: false,
        };

        capture
            .start_capture(callback.clone())
            .await
            .expect("E2E capture should start");
        assert_eq!(callbacks.load(Ordering::SeqCst), 1);
        capture
            .stop_capture()
            .await
            .expect("E2E capture should stop");
        tokio::task::yield_now().await;
        assert_eq!(callbacks.load(Ordering::SeqCst), 1);
        assert!(!capture.is_capturing());
        assert!(capture.start_capture(callback).await.is_err());
        assert_eq!(callbacks.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn output_requires_exact_pcm_and_completed_drain_before_restart() {
        let harness = Arc::new(E2eHarness::default());
        let factory = E2eOutputFactory {
            harness: harness.clone(),
        };
        let mut output = factory
            .create_local_playback_output(LocalPlaybackRoute::SystemDefault)
            .expect("first E2E output should be created");

        assert!(matches!(
            output
                .enqueue_pcm16(&[EXPECTED_TRANSLATED_SAMPLE; EXPECTED_TRANSLATED_SAMPLES])
                .await,
            Err(crate::domain::TranslationAudioOutputError::Closed)
        ));
        output
            .open(TranslationAudioOutputConfig::incoming_spoken_translation())
            .await
            .expect("E2E output should open");
        assert!(output.enqueue_pcm16(&[7; 32]).await.is_err());

        let queued = output
            .enqueue_pcm16(&[EXPECTED_TRANSLATED_SAMPLE; EXPECTED_TRANSLATED_SAMPLES])
            .await
            .expect("expected translated PCM should be queued");
        assert!(matches!(
            queued,
            AudioEnqueueOutcome::Queued { pending }
                if pending == Duration::from_millis(100)
        ));
        assert_eq!(
            output.pending_playback_duration(),
            Duration::from_millis(100)
        );

        output.begin_drain_mode();
        assert_eq!(
            output
                .prepare_for_drain()
                .expect("queued audio should prepare for drain"),
            Duration::from_millis(100)
        );
        assert_eq!(output.pending_playback_duration(), Duration::ZERO);
        output.close().await.expect("drained output should close");
        assert!(matches!(
            output
                .enqueue_pcm16(&[EXPECTED_TRANSLATED_SAMPLE; EXPECTED_TRANSLATED_SAMPLES])
                .await,
            Err(crate::domain::TranslationAudioOutputError::Closed)
        ));

        factory
            .create_local_playback_output(LocalPlaybackRoute::SystemDefault)
            .expect("restart should accept the completed previous output");
    }

    #[test]
    fn output_factory_rejects_restart_when_previous_output_was_not_closed() {
        let harness = Arc::new(E2eHarness::default());
        let factory = E2eOutputFactory { harness };
        let _output = factory
            .create_local_playback_output(LocalPlaybackRoute::SystemDefault)
            .expect("first E2E output should be created");

        assert!(factory
            .create_local_playback_output(LocalPlaybackRoute::SystemDefault)
            .is_err());
    }

    #[tokio::test]
    async fn translated_text_waits_until_exact_audio_reaches_output() {
        let harness = Arc::new(E2eHarness::default());
        let factory = E2eOutputFactory {
            harness: harness.clone(),
        };
        let mut output = factory
            .create_local_playback_output(LocalPlaybackRoute::SystemDefault)
            .expect("E2E output should be created");
        output
            .open(TranslationAudioOutputConfig::incoming_spoken_translation())
            .await
            .expect("E2E output should open");

        let mut translation = E2eTranslationSession {
            harness,
            events: None,
            emitted: false,
        };
        let mut events = translation
            .connect(RealtimeTranslationConfig::new(
                "e2e-credential".into(),
                "ru".into(),
                crate::domain::RealtimeInputNoiseReduction::Disabled,
            ))
            .await
            .expect("E2E translator should connect");
        let append = tokio::spawn(async move {
            let result = translation.append_pcm16(&[800; 4_800]).await;
            (translation, result)
        });

        let audio = events.recv().await.expect("translated audio should arrive");
        let RealtimeTranslationEvent::TranslatedAudio { pcm16, .. } = audio else {
            panic!("translated audio must be emitted before text");
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(25), events.recv())
                .await
                .is_err()
        );

        output
            .enqueue_pcm16(&pcm16)
            .await
            .expect("translated audio should reach output");
        let (_translation, result) = append.await.expect("append task should complete");
        result.expect("append should complete after audio enqueue");
        assert!(matches!(
            events.recv().await,
            Some(RealtimeTranslationEvent::SourceTextDelta(_))
        ));
        assert!(matches!(
            events.recv().await,
            Some(RealtimeTranslationEvent::TranslatedTextDelta(_))
        ));
    }
}
