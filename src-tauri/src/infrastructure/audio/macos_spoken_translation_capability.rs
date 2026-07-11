use crate::domain::{SpokenIncomingCapability, SpokenTranslationCapability, TranslationLanguage};

#[derive(Debug, Default)]
pub struct DefaultSpokenTranslationCapability;

impl DefaultSpokenTranslationCapability {
    pub fn new() -> Self {
        Self
    }
}

impl SpokenTranslationCapability for DefaultSpokenTranslationCapability {
    fn check(&self, target_language: &str) -> SpokenIncomingCapability {
        if TranslationLanguage::parse(target_language).is_err() {
            return SpokenIncomingCapability::UnsupportedTargetLanguage;
        }

        #[cfg(target_os = "macos")]
        {
            use cpal::traits::HostTrait;

            if cpal::default_host().default_output_device().is_none() {
                SpokenIncomingCapability::NoOutputDevice
            } else {
                SpokenIncomingCapability::Ready
            }
        }

        #[cfg(not(target_os = "macos"))]
        {
            SpokenIncomingCapability::UnsupportedPlatform
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_language_fails_before_platform_device_probe() {
        let capability = DefaultSpokenTranslationCapability::new();

        assert_eq!(
            capability.check("uk"),
            SpokenIncomingCapability::UnsupportedTargetLanguage
        );
    }
}
