use async_trait::async_trait;
use serde::Serialize;
use std::time::Duration;

use crate::domain::{AudioCapture, AudioError, AudioResult};

#[derive(Debug, thiserror::Error)]
pub enum TranslationAudioOutputError {
    #[error("Configuration: {0}")]
    Configuration(String),

    #[error("Device: {0}")]
    Device(String),

    #[error("Stream: {0}")]
    Stream(String),

    #[error("Resample: {0}")]
    Resample(String),

    #[error("Output is closed")]
    Closed,
}

pub type TranslationAudioOutputResult<T> = Result<T, TranslationAudioOutputError>;

#[derive(Debug, Clone, Copy)]
pub struct TranslationAudioOutputConfig {
    pub source_sample_rate: u32,
    pub source_channels: u16,
    pub prebuffer_ms: u64,
    pub max_buffered_frames: usize,
    pub drain_max_buffered_frames: usize,
}

impl TranslationAudioOutputConfig {
    pub fn openai_translation() -> Self {
        Self {
            source_sample_rate: 24_000,
            source_channels: 1,
            prebuffer_ms: 200,
            max_buffered_frames: 300_000,
            drain_max_buffered_frames: 720_000,
        }
    }
}

#[async_trait]
pub trait TranslationAudioOutput: Send + Sync {
    async fn open(
        &mut self,
        config: TranslationAudioOutputConfig,
    ) -> TranslationAudioOutputResult<()>;
    async fn enqueue_pcm16(&self, samples: &[i16]) -> TranslationAudioOutputResult<()>;
    async fn close(&mut self) -> TranslationAudioOutputResult<()>;
    fn is_open(&self) -> bool;
    fn device_name(&self) -> Option<String>;
    fn begin_drain_mode(&self);
    fn prepare_for_drain(&self) -> TranslationAudioOutputResult<Duration>;
    fn pending_playback_duration(&self) -> Duration;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioCaptureTarget {
    pub sample_rate: u32,
    pub channels: u16,
}

impl AudioCaptureTarget {
    pub fn dictation() -> Self {
        Self {
            sample_rate: 16_000,
            channels: 1,
        }
    }

    pub fn outgoing_translation() -> Self {
        Self {
            sample_rate: 24_000,
            channels: 1,
        }
    }

    pub fn incoming_subtitles() -> Self {
        Self {
            sample_rate: 16_000,
            channels: 1,
        }
    }

    pub fn incoming_realtime_translation() -> Self {
        Self {
            sample_rate: 24_000,
            channels: 1,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PlatformAudioSetupStatus {
    pub platform: String,
    pub status: PlatformAudioSetupState,
    pub outgoing_supported: bool,
    pub incoming_supported: bool,
    pub virtual_microphone_name: String,
    pub message: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlatformAudioSetupState {
    Ready,
    MissingDependency,
    MissingVirtualDevice,
    Unsupported,
    Error,
}

#[async_trait]
pub trait PlatformAudioFactory: Send + Sync {
    fn create_microphone_capture(
        &self,
        device_name: Option<String>,
        target: AudioCaptureTarget,
    ) -> AudioResult<Box<dyn AudioCapture>>;

    fn create_translation_output(
        &self,
    ) -> TranslationAudioOutputResult<Box<dyn TranslationAudioOutput>>;

    fn create_system_loopback_capture(
        &self,
        target: AudioCaptureTarget,
    ) -> AudioResult<Box<dyn AudioCapture>>;

    async fn setup_status(&self) -> PlatformAudioSetupStatus;

    fn is_virtual_microphone_input(&self, name: &str) -> bool;

    fn microphone_preflight(&self) -> Result<(), AudioError> {
        Ok(())
    }
}
