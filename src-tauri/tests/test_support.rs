use std::sync::Arc;

use app_lib::domain::{ConnectionQualityCallback, SttConfig, SttProviderType};

pub fn noop_connection_quality() -> ConnectionQualityCallback {
    Arc::new(|_quality: String, _reason: Option<String>| {})
}

pub trait SttConfigTestExt {
    fn with_api_key(self, api_key: impl Into<String>) -> Self;
}

impl SttConfigTestExt for SttConfig {
    fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        let api_key = api_key.into();

        match self.provider {
            SttProviderType::Deepgram => self.deepgram_api_key = Some(api_key),
            SttProviderType::AssemblyAI => self.assemblyai_api_key = Some(api_key),
            _ => {}
        }

        self
    }
}

