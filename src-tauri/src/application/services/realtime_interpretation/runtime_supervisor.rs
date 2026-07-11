use std::future::Future;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::RealtimeInterpretationStop;

pub(crate) fn spawn_realtime_interpretation_supervisor<F, Fut>(
    session_id: u64,
    mut stop_rx: mpsc::UnboundedReceiver<RealtimeInterpretationStop>,
    on_stop: F,
) -> JoinHandle<()>
where
    F: FnOnce(u64, RealtimeInterpretationStop) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        if let Some(stop) = stop_rx.recv().await {
            on_stop(session_id, stop).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::*;
    use crate::application::services::RealtimeInterpretationError;

    #[tokio::test]
    async fn supervisor_handles_only_first_terminal_signal() {
        let (tx, rx) = mpsc::unbounded_channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_callback = calls.clone();
        let task = spawn_realtime_interpretation_supervisor(42, rx, move |session_id, stop| {
            let calls = calls_for_callback.clone();
            async move {
                assert_eq!(session_id, 42);
                assert!(matches!(
                    stop,
                    RealtimeInterpretationStop::Error(RealtimeInterpretationError::Processing(_))
                ));
                calls.fetch_add(1, Ordering::SeqCst);
            }
        });

        tx.send(RealtimeInterpretationStop::Error(
            RealtimeInterpretationError::Processing("first".into()),
        ))
        .unwrap();
        let _ = tx.send(RealtimeInterpretationStop::Closed);
        task.await.unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
