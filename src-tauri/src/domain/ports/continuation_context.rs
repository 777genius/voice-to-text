use async_trait::async_trait;
use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

/// Memory-only native target validation; unknown registrations are never valid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextValidation {
    Valid { revision: u64 },
    Mismatch,
    Unavailable,
}

pub struct ContextValidationRequest<'a> {
    pub deadline: Instant,
    pub result: Pin<Box<dyn Future<Output = ContextValidation> + Send + 'a>>,
}

#[async_trait]
pub trait ContinuationContextGuard: Send + Sync {
    async fn validate(&self, logical_run_id: u64) -> ContextValidation;

    fn begin_validation(&self, logical_run_id: u64) -> ContextValidationRequest<'_> {
        ContextValidationRequest {
            deadline: tokio::time::Instant::now().into_std() + Duration::from_millis(250),
            result: Box::pin(self.validate(logical_run_id)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DefaultGuard;
    #[async_trait]
    impl ContinuationContextGuard for DefaultGuard {
        async fn validate(&self, _: u64) -> ContextValidation {
            ContextValidation::Unavailable
        }
    }

    #[tokio::test(start_paused = true)]
    async fn default_guard_uses_tokio_clock_for_250ms_bound() {
        tokio::time::advance(Duration::from_secs(5)).await;
        let request = DefaultGuard.begin_validation(1);
        let remaining = request
            .deadline
            .duration_since(tokio::time::Instant::now().into_std());
        assert_eq!(remaining, Duration::from_millis(250));
    }
}
