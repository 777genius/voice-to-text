use std::sync::Arc;

use crate::domain::{
    IncomingTranslationDelivery, LocalPlaybackOutputFactory, PlatformAudioFactory,
    RealtimeTranslationFactory, RecordingStatus, SpokenTranslationCapability, SttProviderFactory,
    SystemAudioCaptureFactory, TranslationLanguage,
};

use super::{
    incoming_caption_translation_service::IncomingCaptionTranslationService,
    incoming_spoken_translation_service::IncomingSpokenTranslationService, IncomingPlaybackState,
    IncomingSpokenTranslationCallbacks, IncomingSpokenTranslationConfig,
    IncomingSpokenTranslationError, IncomingTranslationCallbacks, IncomingTranslationConfig,
    IncomingTranslationError,
};

enum IncomingRuntime {
    Captions(IncomingCaptionTranslationService),
    Spoken(IncomingSpokenTranslationService),
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

    pub fn new_spoken() -> Self {
        Self {
            runtime: IncomingRuntime::Spoken(IncomingSpokenTranslationService::new()),
        }
    }

    pub fn new_spoken_with_factories(
        capture_factory: Arc<dyn SystemAudioCaptureFactory>,
        output_factory: Arc<dyn LocalPlaybackOutputFactory>,
        translation_factory: Arc<dyn RealtimeTranslationFactory>,
        capability: Arc<dyn SpokenTranslationCapability>,
    ) -> Self {
        Self {
            runtime: IncomingRuntime::Spoken(IncomingSpokenTranslationService::new_with_factories(
                capture_factory,
                output_factory,
                translation_factory,
                capability,
            )),
        }
    }

    pub fn delivery(&self) -> IncomingTranslationDelivery {
        match &self.runtime {
            IncomingRuntime::Captions(_) => IncomingTranslationDelivery::CaptionsOnly,
            IncomingRuntime::Spoken(_) => IncomingTranslationDelivery::TextAndAudio,
        }
    }

    pub async fn playback_snapshot(&self) -> Option<(IncomingPlaybackState, bool)> {
        match &self.runtime {
            IncomingRuntime::Captions(_) => None,
            IncomingRuntime::Spoken(service) => Some(service.playback_snapshot().await),
        }
    }

    pub async fn set_muted(&self, muted: bool) -> Result<(), IncomingTranslationError> {
        match &self.runtime {
            IncomingRuntime::Captions(_) => Err(IncomingTranslationError::Configuration(
                "incoming translated playback is disabled in captions-only mode".into(),
            )),
            IncomingRuntime::Spoken(service) => {
                service.set_muted(muted).await.map_err(map_spoken_error)
            }
        }
    }

    pub async fn get_status(&self) -> RecordingStatus {
        match &self.runtime {
            IncomingRuntime::Captions(service) => service.get_status().await,
            IncomingRuntime::Spoken(service) => service.get_status().await,
        }
    }

    pub async fn active_session_id(&self) -> Option<u64> {
        match &self.runtime {
            IncomingRuntime::Captions(service) => service.active_session_id().await,
            IncomingRuntime::Spoken(service) => service.active_session_id().await,
        }
    }

    pub async fn state_snapshot(&self) -> (Option<u64>, RecordingStatus) {
        match &self.runtime {
            IncomingRuntime::Captions(service) => service.state_snapshot().await,
            IncomingRuntime::Spoken(service) => service.state_snapshot().await,
        }
    }

    pub async fn start(
        &self,
        config: IncomingTranslationConfig,
        callbacks: IncomingTranslationCallbacks,
    ) -> Result<(), IncomingTranslationError> {
        match &self.runtime {
            IncomingRuntime::Captions(service) => service.start(config, callbacks).await,
            IncomingRuntime::Spoken(service) => {
                let target_language =
                    TranslationLanguage::parse(&config.target_language).map_err(|error| {
                        IncomingTranslationError::UnsupportedTargetLanguage(error.to_string())
                    })?;
                let spoken_callbacks = IncomingSpokenTranslationCallbacks {
                    on_source_delta: callbacks.on_source_final,
                    on_translation_delta: callbacks.on_translation_delta,
                    on_playback_state: Arc::new(|_| {}),
                    on_error: Arc::new(move |error| {
                        (callbacks.on_error)(map_spoken_error(error));
                    }),
                    on_status: callbacks.on_status,
                };
                service
                    .start(
                        IncomingSpokenTranslationConfig {
                            openai_api_key: config.openai_api_key,
                            target_language,
                            playback_gain: config.playback_gain,
                            session_id: config.session_id,
                        },
                        spoken_callbacks,
                    )
                    .await
                    .map_err(map_spoken_error)
            }
        }
    }

    pub async fn stop(&self) -> Result<(), IncomingTranslationError> {
        match &self.runtime {
            IncomingRuntime::Captions(service) => service.stop().await,
            IncomingRuntime::Spoken(service) => service.stop().await.map_err(map_spoken_error),
        }
    }
}

fn map_spoken_error(error: IncomingSpokenTranslationError) -> IncomingTranslationError {
    match error {
        IncomingSpokenTranslationError::Configuration(message) => {
            IncomingTranslationError::Configuration(message)
        }
        IncomingSpokenTranslationError::Authentication(message) => {
            IncomingTranslationError::Authentication(message)
        }
        IncomingSpokenTranslationError::RateLimited(message) => {
            IncomingTranslationError::RateLimited(message)
        }
        IncomingSpokenTranslationError::UnsupportedTargetLanguage(message) => {
            IncomingTranslationError::UnsupportedTargetLanguage(message)
        }
        IncomingSpokenTranslationError::PermissionDenied(message) => {
            IncomingTranslationError::PermissionDenied(message)
        }
        IncomingSpokenTranslationError::UnsafeAudioRoute(message) => {
            IncomingTranslationError::UnsafeAudioRoute(message)
        }
        IncomingSpokenTranslationError::InputDeviceLost(message) => {
            IncomingTranslationError::InputDeviceLost(message)
        }
        IncomingSpokenTranslationError::OutputDeviceLost(message) => {
            IncomingTranslationError::OutputDeviceLost(message)
        }
        IncomingSpokenTranslationError::InputOverload(message) => {
            IncomingTranslationError::InputOverload(message)
        }
        IncomingSpokenTranslationError::OutputOverload(message) => {
            IncomingTranslationError::OutputOverload(message)
        }
        IncomingSpokenTranslationError::Connection(message) => {
            IncomingTranslationError::Connection(message)
        }
        IncomingSpokenTranslationError::Protocol(message) => {
            IncomingTranslationError::Protocol(message)
        }
        IncomingSpokenTranslationError::Timeout(message) => {
            IncomingTranslationError::Timeout(message)
        }
        IncomingSpokenTranslationError::Processing(message) => {
            IncomingTranslationError::Processing(message)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::SttConfig;

    #[tokio::test]
    async fn captions_runtime_starts_idle_without_active_session() {
        let facade = IncomingTranslationFacade::new();

        assert_eq!(facade.get_status().await, RecordingStatus::Idle);
        assert_eq!(facade.active_session_id().await, None);
        assert_eq!(facade.state_snapshot().await, (None, RecordingStatus::Idle));
    }

    #[tokio::test]
    async fn spoken_runtime_rejects_unsupported_language_before_platform_preflight() {
        let facade = IncomingTranslationFacade::new_spoken();
        let mut config = IncomingTranslationConfig::new_with_defaults(SttConfig::default(), 91);
        config.openai_api_key = "test-key".into();
        config.target_language = "uk".into();
        let callbacks = IncomingTranslationCallbacks {
            on_source_final: Arc::new(|_| {}),
            on_translation_delta: Arc::new(|_| {}),
            on_error: Arc::new(|_| {}),
            on_status: Arc::new(|_| {}),
        };

        let error = facade.start(config, callbacks).await.unwrap_err();

        assert_eq!(error.error_type(), "unsupported_target_language");
        assert_eq!(facade.get_status().await, RecordingStatus::Idle);
    }

    #[test]
    fn facade_reports_its_delivery_mode_without_exposing_runtime_details() {
        assert_eq!(
            IncomingTranslationFacade::new().delivery(),
            IncomingTranslationDelivery::CaptionsOnly
        );
        assert_eq!(
            IncomingTranslationFacade::new_spoken().delivery(),
            IncomingTranslationDelivery::TextAndAudio
        );
    }
}
