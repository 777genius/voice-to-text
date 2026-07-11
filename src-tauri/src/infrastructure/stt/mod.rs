mod assemblyai;
mod backend;
mod backend_messages;
/// STT provider implementations
mod deepgram;
mod whisper_local;

use std::future::Future;
use std::time::Duration;

use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use crate::domain::{SttConnectionCategory, SttConnectionError, SttError, SttResult};

const STREAMING_WS_MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;
const STREAMING_WS_MAX_WRITE_BUFFER_BYTES: usize = 4 * 1024 * 1024;

pub(super) fn abort_background_task(task: &mut Option<tokio::task::JoinHandle<()>>) {
    if let Some(task) = task.take() {
        task.abort();
    }
}

pub(super) fn streaming_websocket_config() -> WebSocketConfig {
    WebSocketConfig {
        max_write_buffer_size: STREAMING_WS_MAX_WRITE_BUFFER_BYTES,
        max_message_size: Some(STREAMING_WS_MAX_MESSAGE_BYTES),
        max_frame_size: Some(STREAMING_WS_MAX_MESSAGE_BYTES),
        ..WebSocketConfig::default()
    }
}

pub(super) async fn await_streaming_websocket_connect<F, T, E>(
    future: F,
    timeout: Duration,
    provider: &str,
) -> SttResult<T>
where
    F: Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    match tokio::time::timeout(timeout, future).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(SttError::Connection(SttConnectionError::simple(format!(
            "{} WebSocket connection failed: {}",
            provider, error
        )))),
        Err(_) => Err(SttError::Connection(SttConnectionError::with_category(
            format!(
                "{} WebSocket connection timed out after {} ms",
                provider,
                timeout.as_millis()
            ),
            SttConnectionCategory::Timeout,
        ))),
    }
}

pub use assemblyai::AssemblyAIProvider;
pub use backend::BackendProvider;
pub use deepgram::DeepgramProvider;
pub use whisper_local::WhisperLocalProvider;

#[cfg(test)]
mod websocket_tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio::net::TcpListener;
    use tokio_tungstenite::{accept_async, connect_async_with_config, tungstenite::Message};

    struct TaskDropSignal(Option<tokio::sync::oneshot::Sender<()>>);

    impl Drop for TaskDropSignal {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    #[test]
    fn streaming_websocket_config_bounds_reads_and_failed_writes() {
        let config = streaming_websocket_config();

        assert_eq!(
            config.max_message_size,
            Some(STREAMING_WS_MAX_MESSAGE_BYTES)
        );
        assert_eq!(config.max_frame_size, Some(STREAMING_WS_MAX_MESSAGE_BYTES));
        assert_eq!(
            config.max_write_buffer_size,
            STREAMING_WS_MAX_WRITE_BUFFER_BYTES
        );
        assert!(config.max_write_buffer_size > config.write_buffer_size);
    }

    #[tokio::test]
    async fn streaming_websocket_connect_timeout_is_typed() {
        let result = await_streaming_websocket_connect::<_, (), &'static str>(
            futures_util::future::pending(),
            Duration::from_millis(5),
            "Synthetic",
        )
        .await;

        assert!(matches!(
            result,
            Err(SttError::Connection(SttConnectionError {
                details: crate::domain::SttConnectionDetails {
                    category: Some(SttConnectionCategory::Timeout),
                    ..
                },
                ..
            }))
        ));
    }

    #[tokio::test]
    async fn streaming_websocket_connect_preserves_transport_error() {
        let result = await_streaming_websocket_connect(
            async { Err::<(), _>("connection reset") },
            Duration::from_secs(1),
            "Synthetic",
        )
        .await;

        let Err(SttError::Connection(error)) = result else {
            panic!("expected connection error");
        };
        assert!(error.message.contains("connection reset"));
        assert_eq!(error.details.category, None);
    }

    #[tokio::test]
    async fn configured_parser_rejects_oversized_provider_message() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind synthetic websocket server");
        let address = listener.local_addr().expect("synthetic server address");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept websocket client");
            let mut websocket = accept_async(stream).await.expect("accept websocket");
            let _ = websocket.send(Message::Text("x".repeat(2_048))).await;
        });

        let mut config = streaming_websocket_config();
        config.max_message_size = Some(1_024);
        config.max_frame_size = Some(1_024);
        let (mut websocket, _) =
            connect_async_with_config(format!("ws://{}", address), Some(config), false)
                .await
                .expect("connect synthetic websocket client");

        let message = websocket.next().await.expect("provider response");
        assert!(matches!(
            message,
            Err(tokio_tungstenite::tungstenite::Error::Capacity(_))
        ));
        server.await.expect("synthetic websocket server task");
    }

    #[tokio::test]
    async fn abort_background_task_does_not_detach_pending_future() {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        let mut task = Some(tokio::spawn(async move {
            let _drop_signal = TaskDropSignal(Some(dropped_tx));
            let _ = started_tx.send(());
            futures_util::future::pending::<()>().await;
        }));
        started_rx.await.expect("background task started");

        abort_background_task(&mut task);

        assert!(task.is_none());
        tokio::time::timeout(Duration::from_secs(1), dropped_rx)
            .await
            .expect("aborted task future must be dropped")
            .expect("drop signal");
    }
}
