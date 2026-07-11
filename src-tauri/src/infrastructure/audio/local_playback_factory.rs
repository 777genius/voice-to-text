#[cfg(not(target_os = "macos"))]
use crate::domain::TranslationAudioOutputError;
use crate::domain::{
    LocalPlaybackOutputFactory, LocalPlaybackRoute, TranslationAudioOutput,
    TranslationAudioOutputResult,
};

#[cfg(target_os = "macos")]
use super::CpalAudioOutput;

#[derive(Debug, Default)]
pub struct DefaultLocalPlaybackOutputFactory;

impl DefaultLocalPlaybackOutputFactory {
    pub fn new() -> Self {
        Self
    }
}

impl LocalPlaybackOutputFactory for DefaultLocalPlaybackOutputFactory {
    fn create_local_playback_output(
        &self,
        route: LocalPlaybackRoute,
    ) -> TranslationAudioOutputResult<Box<dyn TranslationAudioOutput>> {
        #[cfg(target_os = "macos")]
        {
            let output = match route {
                LocalPlaybackRoute::SystemDefault => CpalAudioOutput::system_default(),
                LocalPlaybackRoute::Device(device_id) => {
                    CpalAudioOutput::explicit_device(device_id)
                }
            };
            Ok(Box::new(output))
        }

        #[cfg(not(target_os = "macos"))]
        {
            let _ = route;
            Err(TranslationAudioOutputError::Configuration(format!(
                "Local translated speech playback is unsupported on {}",
                std::env::consts::OS
            )))
        }
    }
}
