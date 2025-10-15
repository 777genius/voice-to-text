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
