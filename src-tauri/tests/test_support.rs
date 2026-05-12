use std::sync::Arc;

use app_lib::domain::{
    ConnectionQualityCallback, ErrorCallback, SttConfig, SttConnectionCategory, SttError,
    SttProviderType,
};

pub fn noop_connection_quality() -> ConnectionQualityCallback {
    Arc::new(|_quality: String, _reason: Option<String>| {})
}

pub fn noop_error() -> ErrorCallback {
    Arc::new(|_err: SttError| {})
}

pub fn classify_error_type(err: &SttError) -> &'static str {
    match err {
        SttError::Authentication(_) => "authentication",
        SttError::Configuration(_) => "configuration",
        SttError::Processing(_) | SttError::Unsupported(_) | SttError::Internal(_) => "processing",
        SttError::Connection(conn) => match conn.details.category {
            Some(SttConnectionCategory::Timeout) => "timeout",
            Some(SttConnectionCategory::LimitExceeded) => "limit_exceeded",
            Some(SttConnectionCategory::RateLimited) => "rate_limited",
            _ => "connection",
        },
    }
}

pub fn stderr_error() -> ErrorCallback {
    Arc::new(|err: SttError| {
        eprintln!("❌ Error: {} (type: {})", err, classify_error_type(&err));
    })
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
