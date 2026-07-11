use crate::domain::{AudioCapture, AudioCaptureTarget, AudioResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelfAudioExclusionRequirement {
    Required,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemAudioCaptureRequest {
    pub target: AudioCaptureTarget,
    pub self_audio_exclusion: SelfAudioExclusionRequirement,
}

impl SystemAudioCaptureRequest {
    pub fn isolated(target: AudioCaptureTarget) -> Self {
        Self {
            target,
            self_audio_exclusion: SelfAudioExclusionRequirement::Required,
        }
    }
}

pub trait SystemAudioCaptureFactory: Send + Sync {
    fn preflight_system_audio_capture(&self, request: SystemAudioCaptureRequest)
        -> AudioResult<()>;

    fn create_system_audio_capture(
        &self,
        request: SystemAudioCaptureRequest,
    ) -> AudioResult<Box<dyn AudioCapture>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolated_request_cannot_relax_self_audio_exclusion() {
        let request = SystemAudioCaptureRequest::isolated(
            AudioCaptureTarget::incoming_realtime_translation(),
        );

        assert_eq!(
            request.self_audio_exclusion,
            SelfAudioExclusionRequirement::Required
        );
        assert_eq!(request.target.sample_rate, 24_000);
        assert_eq!(request.target.channels, 1);
    }
}
