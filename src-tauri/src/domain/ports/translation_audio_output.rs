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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranslationAudioOutputMaintenance {
    Healthy,
    Recovered { dropped_audio: Duration },
}

pub const INCOMING_TRANSLATION_MAX_PLAYBACK_GAIN: f32 = 2.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioEnqueueOutcome {
    Queued {
        pending: Duration,
    },
    DroppedOldest {
        duration: Duration,
        pending: Duration,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct TranslationAudioOutputConfig {
    pub source_sample_rate: u32,
    pub source_channels: u16,
    pub prebuffer_ms: u64,
    pub max_buffered_duration: Duration,
    pub drain_max_buffered_duration: Duration,
    pub gain: f32,
}

impl TranslationAudioOutputConfig {
    pub fn openai_translation() -> Self {
        Self {
            source_sample_rate: 24_000,
            source_channels: 1,
            prebuffer_ms: 200,
            max_buffered_duration: Duration::from_millis(6_250),
            drain_max_buffered_duration: Duration::from_secs(15),
            gain: 1.0,
        }
    }

    pub fn incoming_spoken_translation() -> Self {
        Self {
            max_buffered_duration: Duration::from_secs(10),
            drain_max_buffered_duration: Duration::from_secs(25),
            ..Self::openai_translation()
        }
    }

    pub fn with_gain(mut self, gain: f32) -> Self {
        self.gain = normalize_output_gain(gain);
        self
    }

    pub fn normalized(mut self) -> Self {
        self.gain = normalize_output_gain(self.gain);
        self
    }
}

pub fn normalize_output_gain(gain: f32) -> f32 {
    if gain.is_finite() {
        gain.clamp(0.0, INCOMING_TRANSLATION_MAX_PLAYBACK_GAIN)
    } else {
        1.0
    }
}

pub fn incoming_translation_volume_gain(volume: u8) -> f32 {
    f32::from(volume.min(100)) / 100.0 * INCOMING_TRANSLATION_MAX_PLAYBACK_GAIN
}

#[async_trait]
pub trait TranslationAudioOutput: Send + Sync {
    async fn open(
        &mut self,
        config: TranslationAudioOutputConfig,
    ) -> TranslationAudioOutputResult<()>;
    async fn enqueue_pcm16(
        &self,
        samples: &[i16],
    ) -> TranslationAudioOutputResult<AudioEnqueueOutcome>;
    async fn close(&mut self) -> TranslationAudioOutputResult<()>;
    fn set_gain(&mut self, _gain: f32) -> TranslationAudioOutputResult<()> {
        Err(TranslationAudioOutputError::Configuration(
            "runtime output gain is unsupported by this adapter".into(),
        ))
    }
    fn is_open(&self) -> bool;
    fn health_check(&self) -> TranslationAudioOutputResult<()> {
        if self.is_open() {
            Ok(())
        } else {
            Err(TranslationAudioOutputError::Closed)
        }
    }
    async fn maintain(
        &mut self,
    ) -> TranslationAudioOutputResult<TranslationAudioOutputMaintenance> {
        self.health_check()?;
        Ok(TranslationAudioOutputMaintenance::Healthy)
    }
    fn device_name(&self) -> Option<String>;
    fn begin_drain_mode(&self);
    fn prepare_for_drain(&self) -> TranslationAudioOutputResult<Duration>;
    fn pending_playback_duration(&self) -> Duration;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_gain_is_clamped_and_non_finite_values_use_safe_default() {
        assert_eq!(normalize_output_gain(-0.5), 0.0);
        assert_eq!(normalize_output_gain(0.25), 0.25);
        assert_eq!(normalize_output_gain(1.5), 1.5);
        assert_eq!(normalize_output_gain(3.0), 2.0);
        assert_eq!(normalize_output_gain(f32::NAN), 1.0);
        assert_eq!(normalize_output_gain(f32::INFINITY), 1.0);
    }

    #[test]
    fn incoming_volume_uses_full_slider_range_for_six_db_makeup_gain() {
        assert_eq!(incoming_translation_volume_gain(0), 0.0);
        assert_eq!(incoming_translation_volume_gain(25), 0.5);
        assert_eq!(incoming_translation_volume_gain(50), 1.0);
        assert_eq!(incoming_translation_volume_gain(100), 2.0);
        assert_eq!(incoming_translation_volume_gain(u8::MAX), 2.0);
    }
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
