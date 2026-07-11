use std::sync::Arc;

use crate::domain::{PlatformAudioFactory, RecordingStatus, SttProviderFactory};

use super::{
    incoming_caption_translation_service::IncomingCaptionTranslationService,
    IncomingTranslationCallbacks, IncomingTranslationConfig, IncomingTranslationError,
};

enum IncomingRuntime {
    Captions(IncomingCaptionTranslationService),
}

/// Stable application boundary for incoming translation delivery modes.
pub struct IncomingTranslationFacade {
    runtime: IncomingRuntime,
}

impl Default for IncomingTranslationFacade {
    fn default() -> Self {
        Self::new()
    }
}

impl IncomingTranslationFacade {
    pub fn new() -> Self {
        Self {
            runtime: IncomingRuntime::Captions(IncomingCaptionTranslationService::new()),
        }
    }

    pub fn new_with_factories(
        stt_factory: Arc<dyn SttProviderFactory>,
        audio_factory: Arc<dyn PlatformAudioFactory>,
    ) -> Self {
        Self {
            runtime: IncomingRuntime::Captions(
                IncomingCaptionTranslationService::new_with_factories(stt_factory, audio_factory),
            ),
        }
    }

    pub async fn get_status(&self) -> RecordingStatus {
        match &self.runtime {
            IncomingRuntime::Captions(service) => service.get_status().await,
        }
    }

    pub async fn active_session_id(&self) -> Option<u64> {
        match &self.runtime {
            IncomingRuntime::Captions(service) => service.active_session_id().await,
        }
    }

    pub async fn state_snapshot(&self) -> (Option<u64>, RecordingStatus) {
        match &self.runtime {
            IncomingRuntime::Captions(service) => service.state_snapshot().await,
        }
    }

    pub async fn start(
        &self,
        config: IncomingTranslationConfig,
        callbacks: IncomingTranslationCallbacks,
    ) -> Result<(), IncomingTranslationError> {
        match &self.runtime {
            IncomingRuntime::Captions(service) => service.start(config, callbacks).await,
        }
    }

    pub async fn stop(&self) -> Result<(), IncomingTranslationError> {
        match &self.runtime {
            IncomingRuntime::Captions(service) => service.stop().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn captions_runtime_starts_idle_without_active_session() {
        let facade = IncomingTranslationFacade::new();

        assert_eq!(facade.get_status().await, RecordingStatus::Idle);
        assert_eq!(facade.active_session_id().await, None);
        assert_eq!(facade.state_snapshot().await, (None, RecordingStatus::Idle));
    }
}
