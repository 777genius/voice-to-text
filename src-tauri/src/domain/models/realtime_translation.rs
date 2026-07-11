/// Provider-neutral configuration for one realtime translation session.
#[derive(Clone, PartialEq, Eq)]
pub struct RealtimeTranslationConfig {
    pub credential: String,
    pub target_language: String,
}

impl std::fmt::Debug for RealtimeTranslationConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RealtimeTranslationConfig")
            .field("credential", &"<redacted>")
            .field("target_language", &self.target_language)
            .finish()
    }
}

impl RealtimeTranslationConfig {
    pub fn new(credential: String, target_language: String) -> Self {
        Self {
            credential,
            target_language,
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
        let config = RealtimeTranslationConfig::new("sk-secret-value".into(), "en".into());

        let debug = format!("{config:?}");

        assert!(!debug.contains("sk-secret-value"));
        assert!(debug.contains("<redacted>"));
        assert!(debug.contains("en"));
    }
}
