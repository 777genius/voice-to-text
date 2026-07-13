use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Weak};

use tokio::sync::{Mutex, RwLock};

use crate::domain::{
    AudioCaptureTarget, AudioConfig, AudioError, LocalPlaybackOutputFactory, LocalPlaybackRoute,
    RealtimeInputNoiseReduction, RealtimeTranslationConfig, RealtimeTranslationError,
    RealtimeTranslationErrorKind, RealtimeTranslationFactory, RecordingStatus,
    SpokenIncomingCapability, SpokenTranslationCapability, SystemAudioCaptureFactory,
    SystemAudioCaptureRequest, TranslationAudioOutputConfig, TranslationAudioOutputError,
    TranslationLanguage,
};

use super::{
    spawn_realtime_interpretation_supervisor, RealtimeInterpretationCallbacks,
    RealtimeInterpretationConfig, RealtimeInterpretationError, RealtimeInterpretationOutputControl,
    RealtimeInterpretationPorts, RealtimeInterpretationSession, RealtimeInterpretationShutdown,
    RealtimeInterpretationStartError, RealtimeInterpretationStop,
};

#[derive(Clone)]
pub struct IncomingSpokenTranslationConfig {
    pub openai_api_key: String,
    pub target_language: TranslationLanguage,
    pub playback_gain: f32,
    pub session_id: u64,
}

impl std::fmt::Debug for IncomingSpokenTranslationConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IncomingSpokenTranslationConfig")
            .field("openai_api_key", &"<redacted>")
            .field("target_language", &self.target_language)
            .field("playback_gain", &self.playback_gain)
            .field("session_id", &self.session_id)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IncomingPlaybackState {
    Opening,
    Playing,
    Draining,
    Stopped,
}

#[derive(Clone)]
pub struct IncomingSpokenTranslationCallbacks {
    pub on_source_delta: Arc<dyn Fn(String) + Send + Sync>,
    pub on_translation_delta: Arc<dyn Fn(String) + Send + Sync>,
    pub on_playback_state: Arc<dyn Fn(IncomingPlaybackState) + Send + Sync>,
    pub on_error: Arc<dyn Fn(IncomingSpokenTranslationError) + Send + Sync>,
    pub on_status: Arc<dyn Fn(RecordingStatus) + Send + Sync>,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum IncomingSpokenTranslationError {
    #[error("configuration: {0}")]
    Configuration(String),
    #[error("authentication: {0}")]
    Authentication(String),
    #[error("rate_limited: {0}")]
    RateLimited(String),
    #[error("unsupported_target_language: {0}")]
    UnsupportedTargetLanguage(String),
    #[error("permission_denied: {0}")]
    PermissionDenied(String),
    #[error("unsafe_audio_route: {0}")]
    UnsafeAudioRoute(String),
    #[error("input_device_lost: {0}")]
    InputDeviceLost(String),
    #[error("output_device_lost: {0}")]
    OutputDeviceLost(String),
    #[error("input_overload: {0}")]
    InputOverload(String),
    #[error("output_overload: {0}")]
    OutputOverload(String),
    #[error("connection: {0}")]
    Connection(String),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("timeout: {0}")]
    Timeout(String),
    #[error("processing: {0}")]
    Processing(String),
}

impl IncomingSpokenTranslationError {
    pub fn error_type(&self) -> &'static str {
        match self {
            Self::Configuration(_) => "configuration",
            Self::Authentication(_) => "authentication",
            Self::RateLimited(_) => "rate_limited",
            Self::UnsupportedTargetLanguage(_) => "unsupported_target_language",
            Self::PermissionDenied(_) => "permission_denied",
            Self::UnsafeAudioRoute(_) => "unsafe_audio_route",
            Self::InputDeviceLost(_) => "input_device_lost",
            Self::OutputDeviceLost(_) => "output_device_lost",
            Self::InputOverload(_) => "input_overload",
            Self::OutputOverload(_) => "output_overload",
            Self::Connection(_) => "connection",
            Self::Protocol(_) => "protocol",
            Self::Timeout(_) => "timeout",
            Self::Processing(_) => "processing",
        }
    }
}

impl From<RealtimeTranslationError> for IncomingSpokenTranslationError {
    fn from(error: RealtimeTranslationError) -> Self {
        let message = error.to_string();
        match error.kind() {
            RealtimeTranslationErrorKind::Authentication => Self::Authentication(message),
            RealtimeTranslationErrorKind::RateLimited => Self::RateLimited(message),
            RealtimeTranslationErrorKind::Connection => Self::Connection(message),
            RealtimeTranslationErrorKind::Timeout => Self::Timeout(message),
            RealtimeTranslationErrorKind::Protocol => Self::Protocol(message),
            RealtimeTranslationErrorKind::Internal => Self::Processing(message),
        }
    }
}

impl From<RealtimeInterpretationError> for IncomingSpokenTranslationError {
    fn from(error: RealtimeInterpretationError) -> Self {
        match error {
            RealtimeInterpretationError::Authentication(message) => Self::Authentication(message),
            RealtimeInterpretationError::RateLimited(message) => Self::RateLimited(message),
            RealtimeInterpretationError::Connection(message) => Self::Connection(message),
            RealtimeInterpretationError::Timeout(message) => Self::Timeout(message),
            RealtimeInterpretationError::Processing(message) => Self::Processing(message),
            RealtimeInterpretationError::Protocol(message) => Self::Protocol(message),
            RealtimeInterpretationError::InputDeviceLost(message) => Self::InputDeviceLost(message),
            RealtimeInterpretationError::OutputDeviceLost(message) => {
                Self::OutputDeviceLost(message)
            }
            RealtimeInterpretationError::InputOverload(message) => Self::InputOverload(message),
            RealtimeInterpretationError::OutputOverload(message) => Self::OutputOverload(message),
        }
    }
}

pub(super) struct IncomingSpokenTranslationService {
    status: Arc<RwLock<RecordingStatus>>,
    inner: Arc<Mutex<Option<RunningIncomingSpokenSession>>>,
    lifecycle: Arc<Mutex<()>>,
    capture_factory: Arc<dyn SystemAudioCaptureFactory>,
    output_factory: Arc<dyn LocalPlaybackOutputFactory>,
    translation_factory: Arc<dyn RealtimeTranslationFactory>,
    capability: Arc<dyn SpokenTranslationCapability>,
}

struct RunningIncomingSpokenSession {
    session: RealtimeInterpretationSession,
    output_control: RealtimeInterpretationOutputControl,
    callbacks: IncomingSpokenTranslationCallbacks,
    playback_gain: f32,
    muted: bool,
}

impl IncomingSpokenTranslationService {
    pub(super) fn new_with_factories(
        capture_factory: Arc<dyn SystemAudioCaptureFactory>,
        output_factory: Arc<dyn LocalPlaybackOutputFactory>,
        translation_factory: Arc<dyn RealtimeTranslationFactory>,
        capability: Arc<dyn SpokenTranslationCapability>,
    ) -> Self {
        Self {
            status: Arc::new(RwLock::new(RecordingStatus::Idle)),
            inner: Arc::new(Mutex::new(None)),
            lifecycle: Arc::new(Mutex::new(())),
            capture_factory,
            output_factory,
            translation_factory,
            capability,
        }
    }

    pub(super) async fn get_status(&self) -> RecordingStatus {
        *self.status.read().await
    }

    pub(super) async fn active_session_id(&self) -> Option<u64> {
        self.inner
            .lock()
            .await
            .as_ref()
            .map(|running| running.session.session_id())
    }

    pub(super) async fn state_snapshot(
        &self,
    ) -> (Option<u64>, RecordingStatus, IncomingPlaybackState, bool) {
        let inner = self.inner.lock().await;
        let session_id = inner.as_ref().map(|running| running.session.session_id());
        let muted = inner.as_ref().map(|running| running.muted).unwrap_or(false);
        let status = normalize_spoken_snapshot_status(session_id, *self.status.read().await);
        let playback_state = match status {
            RecordingStatus::Starting => IncomingPlaybackState::Opening,
            RecordingStatus::Recording => IncomingPlaybackState::Playing,
            RecordingStatus::Processing => IncomingPlaybackState::Draining,
            RecordingStatus::Idle | RecordingStatus::Error => IncomingPlaybackState::Stopped,
        };
        (session_id, status, playback_state, muted)
    }

    pub(super) async fn set_muted(
        &self,
        muted: bool,
    ) -> Result<(), IncomingSpokenTranslationError> {
        let _lifecycle_guard = self.lifecycle.lock().await;
        let (output_control, gain) = {
            let inner = self.inner.lock().await;
            let Some(running) = inner.as_ref() else {
                return Err(IncomingSpokenTranslationError::Configuration(
                    "incoming spoken translation is not active".into(),
                ));
            };
            if !muted && running.playback_gain == 0.0 {
                return Err(IncomingSpokenTranslationError::Configuration(
                    "translated playback volume is set to zero".into(),
                ));
            }
            (
                running.output_control.clone(),
                if muted { 0.0 } else { running.playback_gain },
            )
        };
        output_control.set_gain(gain).await?;
        let mut inner = self.inner.lock().await;
        let Some(running) = inner.as_mut() else {
            return Err(IncomingSpokenTranslationError::Processing(
                "incoming spoken translation ended while playback volume was changing".into(),
            ));
        };
        running.muted = muted;
        Ok(())
    }

    pub(super) async fn start(
        &self,
        config: IncomingSpokenTranslationConfig,
        callbacks: IncomingSpokenTranslationCallbacks,
    ) -> Result<(), IncomingSpokenTranslationError> {
        let _lifecycle_guard = self.lifecycle.lock().await;
        log::info!(
            "incoming_spoken_start session_id={} target_language={} output_route=system_default",
            config.session_id,
            config.target_language.as_str()
        );
        if self.inner.lock().await.is_some() {
            return Err(IncomingSpokenTranslationError::Configuration(
                "incoming spoken translation session is already active".into(),
            ));
        }
        self.set_status(RecordingStatus::Starting, &callbacks).await;
        if config.openai_api_key.trim().is_empty() {
            self.transition_to_error().await;
            return Err(IncomingSpokenTranslationError::Configuration(
                "OpenAI API key is not configured".into(),
            ));
        }
        if let Err(error) = map_capability(self.capability.check(config.target_language.as_str())) {
            self.transition_to_error().await;
            return Err(error);
        }

        let target = AudioCaptureTarget::incoming_realtime_translation();
        let capture_request = SystemAudioCaptureRequest::isolated(target);
        if let Err(error) = self
            .capture_factory
            .preflight_system_audio_capture(capture_request)
        {
            self.transition_to_error().await;
            return Err(map_capture_start_error(error));
        }

        call_spoken_callback("playback opening", || {
            (callbacks.on_playback_state)(IncomingPlaybackState::Opening)
        });
        let mut output = match self
            .output_factory
            .create_local_playback_output(LocalPlaybackRoute::SystemDefault)
        {
            Ok(output) => output,
            Err(error) => {
                self.transition_to_error().await;
                notify_playback_stopped(&callbacks);
                return Err(map_output_start_error(error));
            }
        };
        if let Err(error) = output
            .open(
                TranslationAudioOutputConfig::incoming_spoken_translation()
                    .with_gain(config.playback_gain),
            )
            .await
        {
            let _ = output.close().await;
            self.transition_to_error().await;
            notify_playback_stopped(&callbacks);
            return Err(map_output_start_error(error));
        }

        let mut capture = match self
            .capture_factory
            .create_system_audio_capture(capture_request)
        {
            Ok(capture) => capture,
            Err(error) => {
                let _ = output.close().await;
                self.transition_to_error().await;
                notify_playback_stopped(&callbacks);
                return Err(map_capture_start_error(error));
            }
        };
        if let Err(error) = capture
            .initialize(AudioConfig {
                sample_rate: target.sample_rate,
                channels: target.channels,
                buffer_size: 720,
            })
            .await
        {
            let _ = output.close().await;
            self.transition_to_error().await;
            notify_playback_stopped(&callbacks);
            return Err(map_capture_start_error(error));
        }

        let mut translation = self.translation_factory.create();
        let translation_events = match translation
            .connect(RealtimeTranslationConfig::new(
                config.openai_api_key.clone(),
                config.target_language.as_str().to_string(),
                RealtimeInputNoiseReduction::Disabled,
            ))
            .await
        {
            Ok(events) => events,
            Err(error) => {
                let _ = output.close().await;
                self.transition_to_error().await;
                notify_playback_stopped(&callbacks);
                return Err(error.into());
            }
        };

        let core_callbacks = RealtimeInterpretationCallbacks {
            on_translated_text: callbacks.on_translation_delta.clone(),
            on_source_text: callbacks.on_source_delta.clone(),
            on_input_audio: Arc::new(|_| {}),
        };
        let (session, mut runtime_stop_rx) = match RealtimeInterpretationSession::start(
            RealtimeInterpretationConfig::incoming_spoken(config.session_id),
            RealtimeInterpretationPorts {
                capture,
                output,
                translation,
                translation_events,
            },
            core_callbacks,
        )
        .await
        {
            Ok(runtime) => runtime,
            Err(error) => {
                self.transition_to_error().await;
                notify_playback_stopped(&callbacks);
                return Err(map_interpretation_start_error(error));
            }
        };

        if let Ok(stop) = runtime_stop_rx.try_recv() {
            session
                .shutdown(RealtimeInterpretationShutdown::Abort)
                .await;
            self.transition_to_error().await;
            notify_playback_stopped(&callbacks);
            return Err(map_runtime_stop(stop));
        }

        let output_control = session.output_control();
        let playback_gain = crate::domain::normalize_output_gain(config.playback_gain);
        *self.inner.lock().await = Some(RunningIncomingSpokenSession {
            session,
            output_control,
            callbacks: callbacks.clone(),
            playback_gain,
            muted: playback_gain == 0.0,
        });
        spawn_spoken_runtime_cleanup_monitor(
            Arc::downgrade(&self.inner),
            Arc::downgrade(&self.lifecycle),
            self.status.clone(),
            callbacks.clone(),
            runtime_stop_rx,
            config.session_id,
        );
        tokio::task::yield_now().await;
        if self.mark_recording_started(&callbacks).await {
            call_spoken_callback("playback playing", || {
                (callbacks.on_playback_state)(IncomingPlaybackState::Playing)
            });
            Ok(())
        } else {
            Err(IncomingSpokenTranslationError::Processing(
                "incoming spoken translation failed during startup".into(),
            ))
        }
    }

    pub(super) async fn stop(&self) -> Result<(), IncomingSpokenTranslationError> {
        let _lifecycle_guard = self.lifecycle.lock().await;
        let running = self.inner.lock().await.take();
        let Some(running) = running else {
            *self.status.write().await = RecordingStatus::Idle;
            return Ok(());
        };
        let callbacks = running.callbacks;
        self.set_status(RecordingStatus::Processing, &callbacks)
            .await;
        call_spoken_callback("playback draining", || {
            (callbacks.on_playback_state)(IncomingPlaybackState::Draining)
        });
        running
            .session
            .shutdown(RealtimeInterpretationShutdown::Graceful)
            .await;
        self.set_status(RecordingStatus::Idle, &callbacks).await;
        call_spoken_callback("playback stopped", || {
            (callbacks.on_playback_state)(IncomingPlaybackState::Stopped)
        });
        Ok(())
    }

    pub(super) async fn abort(&self) -> Result<(), IncomingSpokenTranslationError> {
        let _lifecycle_guard = self.lifecycle.lock().await;
        let running = self.inner.lock().await.take();
        let Some(running) = running else {
            *self.status.write().await = RecordingStatus::Idle;
            return Ok(());
        };
        let callbacks = running.callbacks;
        running
            .session
            .shutdown(RealtimeInterpretationShutdown::Abort)
            .await;
        self.set_status(RecordingStatus::Idle, &callbacks).await;
        call_spoken_callback("playback stopped", || {
            (callbacks.on_playback_state)(IncomingPlaybackState::Stopped)
        });
        Ok(())
    }

    async fn set_status(
        &self,
        status: RecordingStatus,
        callbacks: &IncomingSpokenTranslationCallbacks,
    ) {
        *self.status.write().await = status;
        call_spoken_callback("status", || (callbacks.on_status)(status));
    }

    async fn transition_to_error(&self) {
        *self.status.write().await = RecordingStatus::Error;
    }

    async fn mark_recording_started(&self, callbacks: &IncomingSpokenTranslationCallbacks) -> bool {
        let mut status = self.status.write().await;
        if *status != RecordingStatus::Starting {
            return false;
        }
        *status = RecordingStatus::Recording;
        drop(status);
        call_spoken_callback("recording status", || {
            (callbacks.on_status)(RecordingStatus::Recording)
        });
        true
    }
}

fn normalize_spoken_snapshot_status(
    session_id: Option<u64>,
    status: RecordingStatus,
) -> RecordingStatus {
    if session_id.is_none() && status == RecordingStatus::Recording {
        RecordingStatus::Processing
    } else {
        status
    }
}

fn map_capability(
    capability: SpokenIncomingCapability,
) -> Result<(), IncomingSpokenTranslationError> {
    match capability {
        SpokenIncomingCapability::Ready => Ok(()),
        SpokenIncomingCapability::UnsupportedPlatform => {
            Err(IncomingSpokenTranslationError::Configuration(
                "spoken incoming translation is unsupported on this platform".into(),
            ))
        }
        SpokenIncomingCapability::PermissionRequired => {
            Err(IncomingSpokenTranslationError::PermissionDenied(
                "Screen & System Audio Recording permission is required".into(),
            ))
        }
        SpokenIncomingCapability::UnsafeSelfCapture => {
            Err(IncomingSpokenTranslationError::UnsafeAudioRoute(
                "current-process audio exclusion cannot be guaranteed".into(),
            ))
        }
        SpokenIncomingCapability::NoOutputDevice => {
            Err(IncomingSpokenTranslationError::OutputDeviceLost(
                "no system default output device is available".into(),
            ))
        }
        SpokenIncomingCapability::UnsupportedTargetLanguage => {
            Err(IncomingSpokenTranslationError::UnsupportedTargetLanguage(
                "target language is not supported by realtime translation".into(),
            ))
        }
    }
}

fn map_capture_start_error(error: AudioError) -> IncomingSpokenTranslationError {
    let message = error.to_string();
    match error {
        AudioError::AccessDenied(_) => IncomingSpokenTranslationError::PermissionDenied(message),
        AudioError::DeviceNotFound(_) | AudioError::Capture(_) => {
            IncomingSpokenTranslationError::InputDeviceLost(message)
        }
        AudioError::Configuration(_) | AudioError::Internal(_) => {
            IncomingSpokenTranslationError::Configuration(message)
        }
    }
}

fn map_output_start_error(error: TranslationAudioOutputError) -> IncomingSpokenTranslationError {
    let message = error.to_string();
    match error {
        TranslationAudioOutputError::Device(_)
        | TranslationAudioOutputError::Stream(_)
        | TranslationAudioOutputError::Closed => {
            IncomingSpokenTranslationError::OutputDeviceLost(message)
        }
        TranslationAudioOutputError::Configuration(_)
        | TranslationAudioOutputError::Resample(_) => {
            IncomingSpokenTranslationError::Configuration(message)
        }
    }
}

fn map_interpretation_start_error(
    error: RealtimeInterpretationStartError,
) -> IncomingSpokenTranslationError {
    match error {
        RealtimeInterpretationStartError::Capture(error) => map_capture_start_error(error),
    }
}

fn call_spoken_callback(label: &str, callback: impl FnOnce()) {
    if catch_unwind(AssertUnwindSafe(callback)).is_err() {
        log::error!(
            "IncomingSpokenTranslationService: {} callback panicked",
            label
        );
    }
}

fn notify_playback_stopped(callbacks: &IncomingSpokenTranslationCallbacks) {
    call_spoken_callback("playback stopped", || {
        (callbacks.on_playback_state)(IncomingPlaybackState::Stopped)
    });
}

fn map_runtime_stop(stop: RealtimeInterpretationStop) -> IncomingSpokenTranslationError {
    match stop {
        RealtimeInterpretationStop::Error(error) => error.into(),
        RealtimeInterpretationStop::Closed => IncomingSpokenTranslationError::Connection(
            "OpenAI realtime translation session closed unexpectedly".into(),
        ),
    }
}

fn spawn_spoken_runtime_cleanup_monitor(
    inner: Weak<Mutex<Option<RunningIncomingSpokenSession>>>,
    lifecycle: Weak<Mutex<()>>,
    status: Arc<RwLock<RecordingStatus>>,
    callbacks: IncomingSpokenTranslationCallbacks,
    runtime_stop_rx: tokio::sync::mpsc::UnboundedReceiver<RealtimeInterpretationStop>,
    session_id: u64,
) {
    let _supervisor = spawn_realtime_interpretation_supervisor(
        session_id,
        runtime_stop_rx,
        move |session_id, stop| async move {
            let Some(lifecycle) = lifecycle.upgrade() else {
                return;
            };
            let _lifecycle_guard = lifecycle.lock().await;
            let Some(inner) = inner.upgrade() else {
                return;
            };
            let session = {
                let mut guard = inner.lock().await;
                let is_current = guard
                    .as_ref()
                    .map(|running| running.session.session_id() == session_id)
                    .unwrap_or(false);
                if is_current {
                    guard.take()
                } else {
                    None
                }
            };
            let Some(running) = session else {
                return;
            };
            let error = map_runtime_stop(stop);
            running
                .session
                .shutdown(RealtimeInterpretationShutdown::Abort)
                .await;
            *status.write().await = RecordingStatus::Error;
            call_spoken_callback("runtime error", || (callbacks.on_error)(error));
            call_spoken_callback("runtime error status", || {
                (callbacks.on_status)(RecordingStatus::Error)
            });
            call_spoken_callback("runtime playback stopped", || {
                (callbacks.on_playback_state)(IncomingPlaybackState::Stopped)
            });
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    use async_trait::async_trait;
    use tokio::sync::mpsc;

    use crate::domain::{
        AudioCapture, AudioCaptureErrorCallback, AudioChunk, AudioChunkCallback,
        AudioEnqueueOutcome, RealtimeTranslationEvent, RealtimeTranslationSession,
        TranslationAudioOutput, TranslationAudioOutputResult,
    };

    #[derive(Default)]
    struct FakeState {
        preflight_calls: AtomicUsize,
        capture_create_calls: AtomicUsize,
        capture_initialize_calls: AtomicUsize,
        capture_start_calls: AtomicUsize,
        capture_stop_calls: AtomicUsize,
        output_create_calls: AtomicUsize,
        output_open_calls: AtomicUsize,
        output_close_calls: AtomicUsize,
        translation_create_calls: AtomicUsize,
        translation_connect_calls: AtomicUsize,
        translation_append_calls: AtomicUsize,
        translation_finish_calls: AtomicUsize,
        translation_abort_calls: AtomicUsize,
        capture_drop_calls: AtomicUsize,
        output_drop_calls: AtomicUsize,
        translation_drop_calls: AtomicUsize,
        requested_capture: StdMutex<Option<SystemAudioCaptureRequest>>,
        requested_route: StdMutex<Option<LocalPlaybackRoute>>,
        requested_language: StdMutex<Option<String>>,
        output_samples: StdMutex<Vec<i16>>,
        output_configs: StdMutex<Vec<TranslationAudioOutputConfig>>,
        output_gains: StdMutex<Vec<f32>>,
        output_health_fail: AtomicBool,
        capture_error_callbacks: StdMutex<Vec<AudioCaptureErrorCallback>>,
    }

    struct FakeCapability(SpokenIncomingCapability);

    impl SpokenTranslationCapability for FakeCapability {
        fn check(&self, _target_language: &str) -> SpokenIncomingCapability {
            self.0
        }
    }

    struct FakeCaptureFactory {
        state: Arc<FakeState>,
        permission_denied: bool,
    }

    impl SystemAudioCaptureFactory for FakeCaptureFactory {
        fn preflight_system_audio_capture(
            &self,
            request: SystemAudioCaptureRequest,
        ) -> crate::domain::AudioResult<()> {
            self.state.preflight_calls.fetch_add(1, Ordering::SeqCst);
            *self.state.requested_capture.lock().unwrap() = Some(request);
            if self.permission_denied {
                Err(AudioError::AccessDenied("permission denied".into()))
            } else {
                Ok(())
            }
        }

        fn create_system_audio_capture(
            &self,
            request: SystemAudioCaptureRequest,
        ) -> crate::domain::AudioResult<Box<dyn AudioCapture>> {
            self.state
                .capture_create_calls
                .fetch_add(1, Ordering::SeqCst);
            *self.state.requested_capture.lock().unwrap() = Some(request);
            Ok(Box::new(FakeCapture {
                state: self.state.clone(),
                config: AudioConfig::default(),
                running: false,
                callback: None,
                error_callback: None,
            }))
        }
    }

    struct FakeCapture {
        state: Arc<FakeState>,
        config: AudioConfig,
        running: bool,
        callback: Option<AudioChunkCallback>,
        error_callback: Option<AudioCaptureErrorCallback>,
    }

    impl Drop for FakeCapture {
        fn drop(&mut self) {
            self.state.capture_drop_calls.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl AudioCapture for FakeCapture {
        async fn initialize(&mut self, config: AudioConfig) -> crate::domain::AudioResult<()> {
            self.state
                .capture_initialize_calls
                .fetch_add(1, Ordering::SeqCst);
            self.config = config;
            Ok(())
        }

        async fn start_capture(
            &mut self,
            callback: AudioChunkCallback,
        ) -> crate::domain::AudioResult<()> {
            self.state
                .capture_start_calls
                .fetch_add(1, Ordering::SeqCst);
            self.running = true;
            callback(AudioChunk::new(vec![1_000; 4_800], 24_000, 1));
            self.callback = Some(callback);
            Ok(())
        }

        async fn stop_capture(&mut self) -> crate::domain::AudioResult<()> {
            self.state.capture_stop_calls.fetch_add(1, Ordering::SeqCst);
            self.running = false;
            self.callback = None;
            Ok(())
        }

        fn set_terminal_error_callback(&mut self, callback: Option<AudioCaptureErrorCallback>) {
            if let Some(callback) = callback.as_ref() {
                self.state
                    .capture_error_callbacks
                    .lock()
                    .unwrap()
                    .push(callback.clone());
            }
            self.error_callback = callback;
        }

        fn is_capturing(&self) -> bool {
            self.running
        }

        fn config(&self) -> AudioConfig {
            self.config
        }
    }

    struct FakeOutputFactory {
        state: Arc<FakeState>,
        fail_open: bool,
    }

    impl LocalPlaybackOutputFactory for FakeOutputFactory {
        fn create_local_playback_output(
            &self,
            route: LocalPlaybackRoute,
        ) -> TranslationAudioOutputResult<Box<dyn TranslationAudioOutput>> {
            self.state
                .output_create_calls
                .fetch_add(1, Ordering::SeqCst);
            *self.state.requested_route.lock().unwrap() = Some(route);
            Ok(Box::new(FakeOutput {
                state: self.state.clone(),
                fail_open: self.fail_open,
                open: false,
            }))
        }
    }

    struct FakeOutput {
        state: Arc<FakeState>,
        fail_open: bool,
        open: bool,
    }

    impl Drop for FakeOutput {
        fn drop(&mut self) {
            self.state.output_drop_calls.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl TranslationAudioOutput for FakeOutput {
        async fn open(
            &mut self,
            config: TranslationAudioOutputConfig,
        ) -> TranslationAudioOutputResult<()> {
            self.state.output_open_calls.fetch_add(1, Ordering::SeqCst);
            self.state.output_configs.lock().unwrap().push(config);
            if self.fail_open {
                return Err(TranslationAudioOutputError::Device(
                    "output unavailable".into(),
                ));
            }
            self.open = true;
            Ok(())
        }

        async fn enqueue_pcm16(
            &self,
            samples: &[i16],
        ) -> TranslationAudioOutputResult<AudioEnqueueOutcome> {
            self.state
                .output_samples
                .lock()
                .unwrap()
                .extend_from_slice(samples);
            Ok(AudioEnqueueOutcome::Queued {
                pending: Duration::ZERO,
            })
        }

        async fn close(&mut self) -> TranslationAudioOutputResult<()> {
            self.state.output_close_calls.fetch_add(1, Ordering::SeqCst);
            self.open = false;
            Ok(())
        }

        fn set_gain(&mut self, gain: f32) -> TranslationAudioOutputResult<()> {
            self.state.output_gains.lock().unwrap().push(gain);
            Ok(())
        }

        fn is_open(&self) -> bool {
            self.open
        }

        fn health_check(&self) -> TranslationAudioOutputResult<()> {
            if self.state.output_health_fail.load(Ordering::SeqCst) {
                Err(TranslationAudioOutputError::Device(
                    "output device disconnected".into(),
                ))
            } else if self.open {
                Ok(())
            } else {
                Err(TranslationAudioOutputError::Closed)
            }
        }

        fn device_name(&self) -> Option<String> {
            Some("fake-system-default".into())
        }

        fn begin_drain_mode(&self) {}

        fn prepare_for_drain(&self) -> TranslationAudioOutputResult<Duration> {
            Ok(Duration::ZERO)
        }

        fn pending_playback_duration(&self) -> Duration {
            Duration::ZERO
        }
    }

    struct FakeTranslationFactory {
        state: Arc<FakeState>,
        fail_connect: bool,
    }

    impl RealtimeTranslationFactory for FakeTranslationFactory {
        fn create(&self) -> Box<dyn RealtimeTranslationSession> {
            self.state
                .translation_create_calls
                .fetch_add(1, Ordering::SeqCst);
            Box::new(FakeTranslation {
                state: self.state.clone(),
                fail_connect: self.fail_connect,
                events: None,
                emitted: AtomicBool::new(false),
            })
        }
    }

    struct FakeTranslation {
        state: Arc<FakeState>,
        fail_connect: bool,
        events: Option<mpsc::Sender<RealtimeTranslationEvent>>,
        emitted: AtomicBool,
    }

    impl Drop for FakeTranslation {
        fn drop(&mut self) {
            self.state
                .translation_drop_calls
                .fetch_add(1, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl RealtimeTranslationSession for FakeTranslation {
        async fn connect(
            &mut self,
            config: RealtimeTranslationConfig,
        ) -> Result<mpsc::Receiver<RealtimeTranslationEvent>, RealtimeTranslationError> {
            self.state
                .translation_connect_calls
                .fetch_add(1, Ordering::SeqCst);
            *self.state.requested_language.lock().unwrap() = Some(config.target_language);
            if self.fail_connect {
                return Err(RealtimeTranslationError::Connection(
                    "connection failed".into(),
                ));
            }
            let (tx, rx) = mpsc::channel(8);
            self.events = Some(tx);
            Ok(rx)
        }

        async fn append_pcm16(&mut self, _samples: &[i16]) -> Result<(), RealtimeTranslationError> {
            self.state
                .translation_append_calls
                .fetch_add(1, Ordering::SeqCst);
            if !self.emitted.swap(true, Ordering::SeqCst) {
                if let Some(events) = &self.events {
                    let _ = events
                        .send(RealtimeTranslationEvent::SourceTextDelta("hello".into()))
                        .await;
                    let _ = events
                        .send(RealtimeTranslationEvent::TranslatedTextDelta(
                            "привет".into(),
                        ))
                        .await;
                    let _ = events
                        .send(RealtimeTranslationEvent::TranslatedAudio {
                            pcm16: vec![10, 20, 30],
                            sample_rate: 24_000,
                            channels: 1,
                        })
                        .await;
                }
            }
            Ok(())
        }

        async fn finish(&mut self, _timeout: Duration) -> Result<(), RealtimeTranslationError> {
            self.state
                .translation_finish_calls
                .fetch_add(1, Ordering::SeqCst);
            if let Some(events) = &self.events {
                let _ = events.send(RealtimeTranslationEvent::Closed).await;
            }
            Ok(())
        }

        async fn abort(&mut self) {
            self.state
                .translation_abort_calls
                .fetch_add(1, Ordering::SeqCst);
        }
    }

    fn service_with_fakes(
        state: Arc<FakeState>,
        capability: SpokenIncomingCapability,
        permission_denied: bool,
        output_fail_open: bool,
        translation_fail_connect: bool,
    ) -> IncomingSpokenTranslationService {
        IncomingSpokenTranslationService::new_with_factories(
            Arc::new(FakeCaptureFactory {
                state: state.clone(),
                permission_denied,
            }),
            Arc::new(FakeOutputFactory {
                state: state.clone(),
                fail_open: output_fail_open,
            }),
            Arc::new(FakeTranslationFactory {
                state,
                fail_connect: translation_fail_connect,
            }),
            Arc::new(FakeCapability(capability)),
        )
    }

    fn config(session_id: u64) -> IncomingSpokenTranslationConfig {
        IncomingSpokenTranslationConfig {
            openai_api_key: "test-key".into(),
            target_language: TranslationLanguage::parse("ru").unwrap(),
            playback_gain: 0.75,
            session_id,
        }
    }

    fn callbacks(
        source: Arc<StdMutex<String>>,
        translated: Arc<StdMutex<String>>,
        playback: Arc<StdMutex<Vec<IncomingPlaybackState>>>,
        statuses: Arc<StdMutex<Vec<RecordingStatus>>>,
    ) -> IncomingSpokenTranslationCallbacks {
        IncomingSpokenTranslationCallbacks {
            on_source_delta: Arc::new(move |delta| source.lock().unwrap().push_str(&delta)),
            on_translation_delta: Arc::new(move |delta| {
                translated.lock().unwrap().push_str(&delta)
            }),
            on_playback_state: Arc::new(move |state| playback.lock().unwrap().push(state)),
            on_error: Arc::new(|error| panic!("unexpected spoken runtime error: {error}")),
            on_status: Arc::new(move |status| statuses.lock().unwrap().push(status)),
        }
    }

    fn no_op_callbacks() -> IncomingSpokenTranslationCallbacks {
        callbacks(
            Arc::new(StdMutex::new(String::new())),
            Arc::new(StdMutex::new(String::new())),
            Arc::new(StdMutex::new(Vec::new())),
            Arc::new(StdMutex::new(Vec::new())),
        )
    }

    #[test]
    fn config_debug_redacts_openai_key() {
        let debug = format!("{:?}", config(41));

        assert!(!debug.contains("test-key"));
        assert!(debug.contains("<redacted>"));
    }

    #[tokio::test]
    async fn capability_failure_stops_before_permission_devices_and_network() {
        let state = Arc::new(FakeState::default());
        let service = service_with_fakes(
            state.clone(),
            SpokenIncomingCapability::UnsupportedPlatform,
            false,
            false,
            false,
        );

        let error = service
            .start(config(42), no_op_callbacks())
            .await
            .unwrap_err();

        assert_eq!(error.error_type(), "configuration");
        assert_eq!(state.preflight_calls.load(Ordering::SeqCst), 0);
        assert_eq!(state.output_create_calls.load(Ordering::SeqCst), 0);
        assert_eq!(state.translation_create_calls.load(Ordering::SeqCst), 0);
        assert_eq!(service.get_status().await, RecordingStatus::Error);
    }

    #[tokio::test]
    async fn permission_failure_stops_before_output_and_network() {
        let state = Arc::new(FakeState::default());
        let service = service_with_fakes(
            state.clone(),
            SpokenIncomingCapability::Ready,
            true,
            false,
            false,
        );

        let error = service
            .start(config(43), no_op_callbacks())
            .await
            .unwrap_err();

        assert_eq!(error.error_type(), "permission_denied");
        assert_eq!(state.output_create_calls.load(Ordering::SeqCst), 0);
        assert_eq!(state.translation_create_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn output_open_failure_closes_output_before_capture_and_network() {
        let state = Arc::new(FakeState::default());
        let service = service_with_fakes(
            state.clone(),
            SpokenIncomingCapability::Ready,
            false,
            true,
            false,
        );

        let error = service
            .start(config(44), no_op_callbacks())
            .await
            .unwrap_err();

        assert_eq!(error.error_type(), "output_device_lost");
        assert_eq!(state.output_close_calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.capture_create_calls.load(Ordering::SeqCst), 0);
        assert_eq!(state.translation_create_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn network_connect_failure_closes_output_without_starting_capture() {
        let state = Arc::new(FakeState::default());
        let service = service_with_fakes(
            state.clone(),
            SpokenIncomingCapability::Ready,
            false,
            false,
            true,
        );

        let error = service
            .start(config(45), no_op_callbacks())
            .await
            .unwrap_err();

        assert_eq!(error.error_type(), "connection");
        assert_eq!(state.output_close_calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.capture_start_calls.load(Ordering::SeqCst), 0);
        assert_eq!(state.translation_connect_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn synthetic_spoken_session_emits_matching_text_audio_and_cleans_up() {
        let state = Arc::new(FakeState::default());
        let service = service_with_fakes(
            state.clone(),
            SpokenIncomingCapability::Ready,
            false,
            false,
            false,
        );
        let source = Arc::new(StdMutex::new(String::new()));
        let translated = Arc::new(StdMutex::new(String::new()));
        let playback = Arc::new(StdMutex::new(Vec::new()));
        let statuses = Arc::new(StdMutex::new(Vec::new()));

        service
            .start(
                config(46),
                callbacks(
                    source.clone(),
                    translated.clone(),
                    playback.clone(),
                    statuses.clone(),
                ),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while state.output_samples.lock().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("translated audio must reach local output");

        assert_eq!(service.active_session_id().await, Some(46));
        assert_eq!(service.get_status().await, RecordingStatus::Recording);
        assert_eq!(source.lock().unwrap().as_str(), "hello");
        assert_eq!(translated.lock().unwrap().as_str(), "привет");
        assert_eq!(
            state.requested_capture.lock().unwrap().unwrap(),
            SystemAudioCaptureRequest::isolated(AudioCaptureTarget::incoming_realtime_translation())
        );
        assert_eq!(
            state.requested_route.lock().unwrap().as_ref(),
            Some(&LocalPlaybackRoute::SystemDefault)
        );
        assert_eq!(
            state.requested_language.lock().unwrap().as_deref(),
            Some("ru")
        );
        let output_config = state.output_configs.lock().unwrap()[0];
        assert_eq!(output_config.max_buffered_duration, Duration::from_secs(10));
        assert_eq!(output_config.gain, 0.75);

        service.stop().await.unwrap();

        assert_eq!(service.active_session_id().await, None);
        assert_eq!(service.get_status().await, RecordingStatus::Idle);
        assert_eq!(state.capture_stop_calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.translation_finish_calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.output_close_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            playback.lock().unwrap().as_slice(),
            &[
                IncomingPlaybackState::Opening,
                IncomingPlaybackState::Playing,
                IncomingPlaybackState::Draining,
                IncomingPlaybackState::Stopped,
            ]
        );
        assert!(statuses.lock().unwrap().contains(&RecordingStatus::Idle));
    }

    #[tokio::test]
    async fn spoken_service_instances_keep_lifecycle_and_session_ids_isolated() {
        let first_state = Arc::new(FakeState::default());
        let second_state = Arc::new(FakeState::default());
        let first = service_with_fakes(
            first_state,
            SpokenIncomingCapability::Ready,
            false,
            false,
            false,
        );
        let second = service_with_fakes(
            second_state,
            SpokenIncomingCapability::Ready,
            false,
            false,
            false,
        );

        first.start(config(101), no_op_callbacks()).await.unwrap();
        second.start(config(202), no_op_callbacks()).await.unwrap();
        first.stop().await.unwrap();

        assert_eq!(first.get_status().await, RecordingStatus::Idle);
        assert_eq!(first.active_session_id().await, None);
        assert_eq!(second.get_status().await, RecordingStatus::Recording);
        assert_eq!(second.active_session_id().await, Some(202));

        second.stop().await.unwrap();
    }

    #[tokio::test]
    async fn mute_updates_local_output_without_restarting_translation() {
        let state = Arc::new(FakeState::default());
        let service = service_with_fakes(
            state.clone(),
            SpokenIncomingCapability::Ready,
            false,
            false,
            false,
        );

        service.start(config(303), no_op_callbacks()).await.unwrap();
        service.set_muted(true).await.unwrap();
        let (_, _, playback_state, muted) = service.state_snapshot().await;
        assert_eq!(
            (playback_state, muted),
            (IncomingPlaybackState::Playing, true)
        );
        service.set_muted(false).await.unwrap();

        assert_eq!(state.output_gains.lock().unwrap().as_slice(), &[0.0, 0.75]);
        assert_eq!(state.translation_connect_calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.capture_start_calls.load(Ordering::SeqCst), 1);
        service.stop().await.unwrap();
    }

    #[tokio::test]
    async fn snapshot_never_reports_playing_without_an_owned_session() {
        let service = service_with_fakes(
            Arc::new(FakeState::default()),
            SpokenIncomingCapability::Ready,
            false,
            false,
            false,
        );
        *service.status.write().await = RecordingStatus::Recording;

        let (session_id, status, playback_state, muted) = service.state_snapshot().await;

        assert_eq!(session_id, None);
        assert_eq!(status, RecordingStatus::Processing);
        assert_eq!(playback_state, IncomingPlaybackState::Draining);
        assert!(!muted);
    }

    #[tokio::test]
    async fn dropping_active_service_releases_all_runtime_owners() {
        let state = Arc::new(FakeState::default());
        let service = service_with_fakes(
            state.clone(),
            SpokenIncomingCapability::Ready,
            false,
            false,
            false,
        );

        service.start(config(404), no_op_callbacks()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while state.translation_append_calls.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("active service must own all runtime workers before drop");
        drop(service);

        tokio::time::timeout(Duration::from_secs(1), async {
            while state.capture_drop_calls.load(Ordering::SeqCst) == 0
                || state.output_drop_calls.load(Ordering::SeqCst) == 0
                || state.translation_drop_calls.load(Ordering::SeqCst) == 0
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("dropping the service must release capture, output, and translation owners");

        assert_eq!(state.capture_drop_calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.output_drop_calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.translation_drop_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn terminal_capture_failure_cleans_runtime_and_allows_restart() {
        let state = Arc::new(FakeState::default());
        let service = service_with_fakes(
            state.clone(),
            SpokenIncomingCapability::Ready,
            false,
            false,
            false,
        );
        let runtime_errors = Arc::new(StdMutex::new(Vec::new()));
        let statuses = Arc::new(StdMutex::new(Vec::new()));
        let first_callbacks = IncomingSpokenTranslationCallbacks {
            on_source_delta: Arc::new(|_| {}),
            on_translation_delta: Arc::new(|_| {}),
            on_playback_state: Arc::new(|_| {}),
            on_error: {
                let runtime_errors = runtime_errors.clone();
                Arc::new(move |error| runtime_errors.lock().unwrap().push(error))
            },
            on_status: {
                let statuses = statuses.clone();
                Arc::new(move |status| statuses.lock().unwrap().push(status))
            },
        };

        service.start(config(505), first_callbacks).await.unwrap();
        let stale_error_callback = state
            .capture_error_callbacks
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("capture must install a terminal error callback");
        stale_error_callback(AudioError::Capture("system stopped stream".into()));

        tokio::time::timeout(Duration::from_secs(1), async {
            while service.active_session_id().await.is_some()
                || service.get_status().await != RecordingStatus::Error
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("terminal capture failure must clean the active runtime");

        assert!(matches!(
            runtime_errors.lock().unwrap().as_slice(),
            [IncomingSpokenTranslationError::InputDeviceLost(message)]
                if message.contains("system stopped stream")
        ));
        assert_eq!(state.capture_stop_calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.translation_abort_calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.output_close_calls.load(Ordering::SeqCst), 1);
        assert!(statuses.lock().unwrap().contains(&RecordingStatus::Error));

        service.start(config(506), no_op_callbacks()).await.unwrap();
        assert_eq!(service.active_session_id().await, Some(506));
        assert_eq!(service.get_status().await, RecordingStatus::Recording);

        stale_error_callback(AudioError::Capture("late stale failure".into()));
        tokio::task::yield_now().await;
        assert_eq!(service.active_session_id().await, Some(506));
        assert_eq!(runtime_errors.lock().unwrap().len(), 1);

        service.stop().await.unwrap();
        assert_eq!(service.get_status().await, RecordingStatus::Idle);
        assert_eq!(state.capture_start_calls.load(Ordering::SeqCst), 2);
        assert_eq!(state.output_close_calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn abort_wins_terminal_supervisor_race_without_late_callbacks() {
        let state = Arc::new(FakeState::default());
        let service = service_with_fakes(
            state.clone(),
            SpokenIncomingCapability::Ready,
            false,
            false,
            false,
        );
        let abort_completed = Arc::new(AtomicBool::new(false));
        let callbacks_after_abort = Arc::new(AtomicUsize::new(0));
        let callbacks = IncomingSpokenTranslationCallbacks {
            on_source_delta: Arc::new(|_| {}),
            on_translation_delta: Arc::new(|_| {}),
            on_playback_state: {
                let abort_completed = abort_completed.clone();
                let callbacks_after_abort = callbacks_after_abort.clone();
                Arc::new(move |_| {
                    if abort_completed.load(Ordering::SeqCst) {
                        callbacks_after_abort.fetch_add(1, Ordering::SeqCst);
                    }
                })
            },
            on_error: {
                let abort_completed = abort_completed.clone();
                let callbacks_after_abort = callbacks_after_abort.clone();
                Arc::new(move |_| {
                    if abort_completed.load(Ordering::SeqCst) {
                        callbacks_after_abort.fetch_add(1, Ordering::SeqCst);
                    }
                })
            },
            on_status: {
                let abort_completed = abort_completed.clone();
                let callbacks_after_abort = callbacks_after_abort.clone();
                Arc::new(move |_| {
                    if abort_completed.load(Ordering::SeqCst) {
                        callbacks_after_abort.fetch_add(1, Ordering::SeqCst);
                    }
                })
            },
        };

        service.start(config(509), callbacks).await.unwrap();
        let terminal_error = state
            .capture_error_callbacks
            .lock()
            .unwrap()
            .last()
            .cloned()
            .unwrap();
        terminal_error(AudioError::Capture("race".into()));
        service.abort().await.unwrap();
        abort_completed.store(true, Ordering::SeqCst);

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(service.get_status().await, RecordingStatus::Idle);
        assert_eq!(service.active_session_id().await, None);
        assert_eq!(callbacks_after_abort.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn terminal_output_failure_cleans_runtime_and_allows_restart() {
        let state = Arc::new(FakeState::default());
        let service = service_with_fakes(
            state.clone(),
            SpokenIncomingCapability::Ready,
            false,
            false,
            false,
        );
        let runtime_errors = Arc::new(StdMutex::new(Vec::new()));
        let callbacks = IncomingSpokenTranslationCallbacks {
            on_source_delta: Arc::new(|_| {}),
            on_translation_delta: Arc::new(|_| {}),
            on_playback_state: Arc::new(|_| {}),
            on_error: {
                let runtime_errors = runtime_errors.clone();
                Arc::new(move |error| runtime_errors.lock().unwrap().push(error))
            },
            on_status: Arc::new(|_| {}),
        };

        service.start(config(507), callbacks).await.unwrap();
        state.output_health_fail.store(true, Ordering::SeqCst);

        tokio::time::timeout(Duration::from_secs(1), async {
            while service.active_session_id().await.is_some()
                || service.get_status().await != RecordingStatus::Error
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("terminal output failure must clean the active runtime");

        assert!(matches!(
            runtime_errors.lock().unwrap().as_slice(),
            [IncomingSpokenTranslationError::OutputDeviceLost(message)]
                if message.contains("output device disconnected")
        ));
        assert_eq!(state.capture_stop_calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.translation_abort_calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.output_close_calls.load(Ordering::SeqCst), 1);

        state.output_health_fail.store(false, Ordering::SeqCst);
        service.start(config(508), no_op_callbacks()).await.unwrap();
        assert_eq!(service.active_session_id().await, Some(508));
        assert_eq!(service.get_status().await, RecordingStatus::Recording);
        service.stop().await.unwrap();

        assert_eq!(state.capture_start_calls.load(Ordering::SeqCst), 2);
        assert_eq!(state.output_close_calls.load(Ordering::SeqCst), 2);
        assert_eq!(service.get_status().await, RecordingStatus::Idle);
    }
}
