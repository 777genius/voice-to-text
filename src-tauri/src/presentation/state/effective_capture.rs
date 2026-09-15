//! Observe the selected device after native start (which may itself fall back).
//! Tracks the physical input before VAD, independently of user selection.
use crate::domain::{
    AudioCapture, AudioCaptureErrorCallback, AudioCaptureHealthProbe, AudioCaptureIdentity,
    AudioChunkCallback, AudioConfig, AudioResult,
};
use std::sync::{Arc, Mutex};

pub(crate) struct EffectiveCapture<T> {
    inner: T,
    identity: Arc<Mutex<Option<String>>>,
    identify: fn(&T) -> Option<String>,
}
impl<T> EffectiveCapture<T> {
    pub(crate) fn new(
        inner: T,
        identity: Arc<Mutex<Option<String>>>,
        identify: fn(&T) -> Option<String>,
    ) -> Self {
        *identity.lock().unwrap() = identify(&inner);
        Self {
            inner,
            identity,
            identify,
        }
    }
}
#[async_trait::async_trait]
impl<T: AudioCapture> AudioCapture for EffectiveCapture<T> {
    async fn initialize(&mut self, config: AudioConfig) -> AudioResult<()> {
        self.inner.initialize(config).await
    }
    async fn start_capture(&mut self, on_chunk: AudioChunkCallback) -> AudioResult<()> {
        let result = self.inner.start_capture(on_chunk).await;
        *self.identity.lock().unwrap() = result
            .as_ref()
            .ok()
            .and_then(|_| (self.identify)(&self.inner));
        result
    }
    async fn stop_capture(&mut self) -> AudioResult<()> {
        self.inner.stop_capture().await
    }
    fn set_capture_identity(&mut self, identity: Option<AudioCaptureIdentity>) {
        self.inner.set_capture_identity(identity);
    }
    fn set_terminal_error_callback(&mut self, callback: Option<AudioCaptureErrorCallback>) {
        self.inner.set_terminal_error_callback(callback);
    }
    fn health_probe(&self) -> Option<AudioCaptureHealthProbe> {
        self.inner.health_probe()
    }
    fn is_capturing(&self) -> bool {
        self.inner.is_capturing()
    }
    fn config(&self) -> AudioConfig {
        self.inner.config()
    }
}
