use std::sync::Arc;

use super::{IncomingTranslationFacade, LiveTranslationService};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct TranslationRuntimeShutdownResult {
    pub incoming_error: Option<String>,
    pub outgoing_error: Option<String>,
}

impl TranslationRuntimeShutdownResult {
    pub fn is_ok(&self) -> bool {
        self.incoming_error.is_none() && self.outgoing_error.is_none()
    }
}

pub async fn shutdown_translation_runtimes(
    incoming: Option<Arc<IncomingTranslationFacade>>,
    outgoing: Option<Arc<LiveTranslationService>>,
) -> TranslationRuntimeShutdownResult {
    let incoming_stop = async move {
        match incoming {
            Some(service) => service.stop().await.err().map(|error| error.to_string()),
            None => None,
        }
    };
    let outgoing_stop = async move {
        match outgoing {
            Some(service) => service
                .stop_translation()
                .await
                .err()
                .map(|error| error.to_string()),
            None => None,
        }
    };
    let (incoming_error, outgoing_error) = tokio::join!(incoming_stop, outgoing_stop);

    TranslationRuntimeShutdownResult {
        incoming_error,
        outgoing_error,
    }
}
