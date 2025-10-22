use crate::domain::{SttConfig, SttError, SttProvider, SttProviderFactory, SttProviderType, SttResult};
use crate::infrastructure::stt::{AssemblyAIProvider, DeepgramProvider, WhisperLocalProvider};

/// Factory for creating STT providers based on configuration
///
/// This implements the Factory pattern and allows dependency injection
pub struct DefaultSttProviderFactory;

impl DefaultSttProviderFactory {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DefaultSttProviderFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl SttProviderFactory for DefaultSttProviderFactory {
    fn create(&self, config: &SttConfig) -> SttResult<Box<dyn SttProvider>> {
        log::info!("Creating STT provider: {:?}", config.provider);

        match config.provider {
            SttProviderType::WhisperLocal => Ok(Box::new(WhisperLocalProvider::new())),

            SttProviderType::AssemblyAI => Ok(Box::new(AssemblyAIProvider::new())),

            SttProviderType::Deepgram => Ok(Box::new(DeepgramProvider::new())),

            SttProviderType::GoogleCloud => Err(SttError::Unsupported(
                "Google Cloud STT provider not yet implemented".to_string(),
            )),

            SttProviderType::Azure => Err(SttError::Unsupported(
                "Azure STT provider not yet implemented".to_string(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_factory_new() {
        let factory = DefaultSttProviderFactory::new();
        // Просто проверяем что создается
        assert!(true);
    }

    #[test]
    fn test_factory_default() {
        let _ = DefaultSttProviderFactory::default();
        assert!(true);
    }

    #[test]
    fn test_create_whisper_local() {
        let factory = DefaultSttProviderFactory::new();
        let config = SttConfig::new(SttProviderType::WhisperLocal);
        let result = factory.create(&config);
        assert!(result.is_ok());
    }

    #[test]
    fn test_create_assemblyai() {
        let factory = DefaultSttProviderFactory::new();
        let config = SttConfig::new(SttProviderType::AssemblyAI);
        let result = factory.create(&config);
        assert!(result.is_ok());
    }

    #[test]
    fn test_create_deepgram() {
        let factory = DefaultSttProviderFactory::new();
        let config = SttConfig::new(SttProviderType::Deepgram);
        let result = factory.create(&config);
        assert!(result.is_ok());
    }

    #[test]
    fn test_create_google_cloud_unsupported() {
        let factory = DefaultSttProviderFactory::new();
        let config = SttConfig::new(SttProviderType::GoogleCloud);
        let result = factory.create(&config);
        assert!(result.is_err());
    }

    #[test]
    fn test_create_azure_unsupported() {
        let factory = DefaultSttProviderFactory::new();
        let config = SttConfig::new(SttProviderType::Azure);
        let result = factory.create(&config);
        assert!(result.is_err());
    }
}
