use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::domain::{
    RealtimeTranslationConfig, RealtimeTranslationError, RealtimeTranslationEvent,
};

#[async_trait]
pub trait RealtimeTranslationSession: Send {
    async fn connect(
        &mut self,
        config: RealtimeTranslationConfig,
    ) -> Result<mpsc::Receiver<RealtimeTranslationEvent>, RealtimeTranslationError>;

    async fn append_pcm16(&mut self, samples: &[i16]) -> Result<(), RealtimeTranslationError>;

    async fn finish(&mut self, timeout: Duration) -> Result<(), RealtimeTranslationError>;

    async fn abort(&mut self);
}

pub trait RealtimeTranslationFactory: Send + Sync {
    fn create(&self) -> Box<dyn RealtimeTranslationSession>;
}
