use async_trait::async_trait;

/// Memory-only native target validation; unknown registrations are never valid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextValidation {
    Valid { revision: u64 },
    Mismatch,
    Unavailable,
}

#[async_trait]
pub trait ContinuationContextGuard: Send + Sync {
    async fn validate(&self, logical_run_id: u64) -> ContextValidation;
}
