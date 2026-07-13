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
    shutdown_translation_runtimes_with_mode(incoming, outgoing, false).await
}

pub async fn abort_translation_runtimes(
    incoming: Option<Arc<IncomingTranslationFacade>>,
    outgoing: Option<Arc<LiveTranslationService>>,
) -> TranslationRuntimeShutdownResult {
    shutdown_translation_runtimes_with_mode(incoming, outgoing, true).await
}

async fn shutdown_translation_runtimes_with_mode(
    incoming: Option<Arc<IncomingTranslationFacade>>,
    outgoing: Option<Arc<LiveTranslationService>>,
    abort: bool,
) -> TranslationRuntimeShutdownResult {
    let incoming_stop = async move {
        match incoming {
            Some(service) => {
                let result = if abort {
                    service.abort().await
                } else {
                    service.stop().await
                };
                result.err().map(|error| error.to_string())
            }
            None => None,
        }
    };
    let outgoing_stop = async move {
        match outgoing {
            Some(service) => {
                let result = if abort {
                    service.abort_translation().await
                } else {
                    service.stop_translation().await
                };
                result.err().map(|error| error.to_string())
            }
            None => None,
        }
    };
    let (incoming_error, outgoing_error) = tokio::join!(incoming_stop, outgoing_stop);

    TranslationRuntimeShutdownResult {
        incoming_error,
        outgoing_error,
    }
}
