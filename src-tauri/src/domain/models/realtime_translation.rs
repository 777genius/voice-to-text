/// Provider-neutral configuration for one realtime translation session.
pub const REALTIME_TRANSLATION_LANGUAGES: &[&str] = &[
    "en", "es", "pt", "fr", "ja", "ru", "zh", "de", "ko", "hi", "id", "vi", "it",
];

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TranslationLanguage(String);

impl TranslationLanguage {
    pub fn parse(value: &str) -> Result<Self, UnsupportedTranslationLanguage> {
        let normalized = value.trim().to_ascii_lowercase();
        if REALTIME_TRANSLATION_LANGUAGES.contains(&normalized.as_str()) {
            Ok(Self(normalized))
        } else {
            Err(UnsupportedTranslationLanguage(value.trim().to_string()))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unsupported realtime translation target language: {0}")]
pub struct UnsupportedTranslationLanguage(pub String);

#[derive(Clone, PartialEq, Eq)]
pub struct RealtimeTranslationConfig {
    pub credential: String,
    pub target_language: String,
    pub input_noise_reduction: RealtimeInputNoiseReduction,
}

/// Input conditioning selected by the capture use case, not by the provider adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RealtimeInputNoiseReduction {
    /// Clean digital audio such as an isolated system-output capture.
    Disabled,
    /// A close-talking microphone such as a headset or external speech microphone.
    NearField,
    /// A laptop or conference-room microphone at a distance from the speaker.
    FarField,
}

impl std::fmt::Debug for RealtimeTranslationConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RealtimeTranslationConfig")
            .field("credential", &"<redacted>")
            .field("target_language", &self.target_language)
            .field("input_noise_reduction", &self.input_noise_reduction)
            .finish()
    }
}

impl RealtimeTranslationConfig {
    pub fn new(
        credential: String,
        target_language: String,
        input_noise_reduction: RealtimeInputNoiseReduction,
    ) -> Self {
        Self {
            credential,
            target_language,
            input_noise_reduction,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RealtimeTranslationErrorKind {
    Authentication,
    RateLimited,
    Connection,
    Timeout,
    Protocol,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RealtimeTranslationError {
    #[error("Authentication: {0}")]
    Authentication(String),
    #[error("Rate limited: {0}")]
    RateLimited(String),
    #[error("Connection: {0}")]
    Connection(String),
    #[error("Timeout: {0}")]
    Timeout(String),
    #[error("Protocol: {0}")]
    Protocol(String),
    #[error("Internal: {0}")]
    Internal(String),
}

impl RealtimeTranslationError {
    pub fn kind(&self) -> RealtimeTranslationErrorKind {
        match self {
            Self::Authentication(_) => RealtimeTranslationErrorKind::Authentication,
            Self::RateLimited(_) => RealtimeTranslationErrorKind::RateLimited,
            Self::Connection(_) => RealtimeTranslationErrorKind::Connection,
            Self::Timeout(_) => RealtimeTranslationErrorKind::Timeout,
            Self::Protocol(_) => RealtimeTranslationErrorKind::Protocol,
            Self::Internal(_) => RealtimeTranslationErrorKind::Internal,
        }
    }
}

/// Runtime events emitted after the provider has confirmed session readiness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RealtimeTranslationEvent {
    TranslatedAudio {
        pcm16: Vec<i16>,
        sample_rate: u32,
        channels: u16,
    },
    TranslatedTextDelta(String),
    SourceTextDelta(String),
    Closed,
    Failed(RealtimeTranslationError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_kind_is_stable_for_every_variant() {
        let cases = [
            (
                RealtimeTranslationError::Authentication("auth".into()),
                RealtimeTranslationErrorKind::Authentication,
            ),
            (
                RealtimeTranslationError::RateLimited("rate".into()),
                RealtimeTranslationErrorKind::RateLimited,
            ),
            (
                RealtimeTranslationError::Connection("connection".into()),
                RealtimeTranslationErrorKind::Connection,
            ),
            (
                RealtimeTranslationError::Timeout("timeout".into()),
                RealtimeTranslationErrorKind::Timeout,
            ),
            (
                RealtimeTranslationError::Protocol("protocol".into()),
                RealtimeTranslationErrorKind::Protocol,
            ),
            (
                RealtimeTranslationError::Internal("internal".into()),
                RealtimeTranslationErrorKind::Internal,
            ),
        ];

        for (error, expected) in cases {
            assert_eq!(error.kind(), expected);
        }
    }

    #[test]
    fn config_debug_never_exposes_credential() {
        let config = RealtimeTranslationConfig::new(
            "sk-secret-value".into(),
            "en".into(),
            RealtimeInputNoiseReduction::NearField,
        );

        let debug = format!("{config:?}");

        assert!(!debug.contains("sk-secret-value"));
        assert!(debug.contains("<redacted>"));
        assert!(debug.contains("en"));
        assert!(debug.contains("NearField"));
    }

    #[test]
    fn config_can_disable_noise_reduction_for_clean_digital_audio() {
        let config = RealtimeTranslationConfig::new(
            "credential".into(),
            "ru".into(),
            RealtimeInputNoiseReduction::Disabled,
        );

        assert_eq!(
            config.input_noise_reduction,
            RealtimeInputNoiseReduction::Disabled
        );
    }

    #[test]
    fn translation_language_normalizes_only_officially_supported_targets() {
        for language in REALTIME_TRANSLATION_LANGUAGES {
            assert_eq!(
                TranslationLanguage::parse(&language.to_ascii_uppercase())
                    .unwrap()
                    .as_str(),
                *language
            );
        }
        for unsupported in ["", "auto", "multi", "uk", "pl"] {
            assert!(TranslationLanguage::parse(unsupported).is_err());
        }
    }
}
