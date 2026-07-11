use crate::domain::{TranslationAudioOutput, TranslationAudioOutputResult};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AudioDeviceId(String);

impl AudioDeviceId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalPlaybackRoute {
    SystemDefault,
    Device(AudioDeviceId),
}

pub trait LocalPlaybackOutputFactory: Send + Sync {
    fn create_local_playback_output(
        &self,
        route: LocalPlaybackRoute,
    ) -> TranslationAudioOutputResult<Box<dyn TranslationAudioOutput>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_device_id_remains_an_opaque_value() {
        let id = AudioDeviceId::new("coreaudio-device-uid");

        assert_eq!(id.as_str(), "coreaudio-device-uid");
        assert_eq!(
            LocalPlaybackRoute::Device(id.clone()),
            LocalPlaybackRoute::Device(id)
        );
    }
}
