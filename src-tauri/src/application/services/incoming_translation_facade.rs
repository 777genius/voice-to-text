use std::sync::Arc;

use crate::domain::{
    AudioCaptureTarget, AudioError, IncomingTranslationDelivery, LocalPlaybackOutputFactory,
    PlatformAudioFactory, RealtimeTranslationFactory, RecordingStatus, SpokenIncomingCapability,
    SpokenTranslationCapability, SttProviderFactory, SystemAudioCaptureFactory,
    SystemAudioCaptureRequest, TranslationLanguage,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IncomingTranslationStateSnapshot {
    pub session_id: Option<u64>,
    pub status: RecordingStatus,
    pub playback_state: Option<IncomingPlaybackState>,
    pub muted: bool,
}

/// Stable application boundary for incoming translation delivery modes.
pub struct IncomingTranslationFacade {
    runtime: IncomingRuntime,
}

#[derive(Clone)]
pub struct IncomingSpokenTranslationPorts {
    capture_factory: Arc<dyn SystemAudioCaptureFactory>,
    output_factory: Arc<dyn LocalPlaybackOutputFactory>,
    translation_factory: Arc<dyn RealtimeTranslationFactory>,
    capability: Arc<dyn SpokenTranslationCapability>,
}

#[derive(Clone)]
pub struct IncomingTranslationFacadeFactory {
    captions_stt_factory: Arc<dyn SttProviderFactory>,
    captions_audio_factory: Arc<dyn PlatformAudioFactory>,
    spoken_ports: IncomingSpokenTranslationPorts,
}

impl IncomingTranslationFacadeFactory {
    pub fn new(
        captions_stt_factory: Arc<dyn SttProviderFactory>,
        captions_audio_factory: Arc<dyn PlatformAudioFactory>,
        spoken_ports: IncomingSpokenTranslationPorts,
    ) -> Self {
        Self {
            captions_stt_factory,
            captions_audio_factory,
            spoken_ports,
        }
    }

    pub fn create(&self, delivery: IncomingTranslationDelivery) -> IncomingTranslationFacade {
        match delivery {
            IncomingTranslationDelivery::CaptionsOnly => {
                IncomingTranslationFacade::new_with_factories(
                    self.captions_stt_factory.clone(),
                    self.captions_audio_factory.clone(),
                )
            }
            IncomingTranslationDelivery::TextAndAudio => self.spoken_ports.create_facade(),
        }
    }

    pub fn check_spoken_capability(&self, target_language: &str) -> SpokenIncomingCapability {
        self.spoken_ports.check_capability(target_language)
    }
}

impl IncomingSpokenTranslationPorts {
    pub fn new(
        capture_factory: Arc<dyn SystemAudioCaptureFactory>,
        output_factory: Arc<dyn LocalPlaybackOutputFactory>,
        translation_factory: Arc<dyn RealtimeTranslationFactory>,
        capability: Arc<dyn SpokenTranslationCapability>,
    ) -> Self {
        Self {
            capture_factory,
            output_factory,
            translation_factory,
            capability,
        }
    }

    pub fn create_facade(&self) -> IncomingTranslationFacade {
        IncomingTranslationFacade::new_spoken_with_factories(
            self.capture_factory.clone(),
            self.output_factory.clone(),
            self.translation_factory.clone(),
            self.capability.clone(),
        )
    }

    pub fn check_capability(&self, target_language: &str) -> SpokenIncomingCapability {
        let capability = self.capability.check(target_language);
        if capability != SpokenIncomingCapability::Ready {
            return capability;
        }

        let request = SystemAudioCaptureRequest::isolated(
            AudioCaptureTarget::incoming_realtime_translation(),
        );
        match self.capture_factory.preflight_system_audio_capture(request) {
            Ok(()) => SpokenIncomingCapability::Ready,
            Err(AudioError::AccessDenied(_)) => SpokenIncomingCapability::PermissionRequired,
            Err(AudioError::Configuration(_)) => SpokenIncomingCapability::UnsafeSelfCapture,
            Err(
                AudioError::DeviceNotFound(_) | AudioError::Capture(_) | AudioError::Internal(_),
            ) => SpokenIncomingCapability::UnsupportedPlatform,
        }
    }
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

    pub async fn state_snapshot(&self) -> IncomingTranslationStateSnapshot {
        match &self.runtime {
            IncomingRuntime::Captions(service) => {
                let (session_id, status) = service.state_snapshot().await;
                IncomingTranslationStateSnapshot {
                    session_id,
                    status,
                    playback_state: None,
                    muted: false,
                }
            }
            IncomingRuntime::Spoken(service) => {
                let (session_id, status, playback_state, muted) = service.state_snapshot().await;
                IncomingTranslationStateSnapshot {
                    session_id,
                    status,
                    playback_state: Some(playback_state),
                    muted,
                }
            }
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

    pub async fn abort(&self) -> Result<(), IncomingTranslationError> {
        match &self.runtime {
            IncomingRuntime::Captions(service) => service.abort().await,
            IncomingRuntime::Spoken(service) => service.abort().await.map_err(map_spoken_error),
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

    #[tokio::test]
    async fn captions_runtime_starts_idle_without_active_session() {
        let facade = IncomingTranslationFacade::new();

        assert_eq!(facade.get_status().await, RecordingStatus::Idle);
        assert_eq!(facade.active_session_id().await, None);
        assert_eq!(
            facade.state_snapshot().await,
            IncomingTranslationStateSnapshot {
                session_id: None,
                status: RecordingStatus::Idle,
                playback_state: None,
                muted: false,
            }
        );
    }

    #[test]
    fn facade_reports_its_delivery_mode_without_exposing_runtime_details() {
        assert_eq!(
            IncomingTranslationFacade::new().delivery(),
            IncomingTranslationDelivery::CaptionsOnly
        );
    }
}
