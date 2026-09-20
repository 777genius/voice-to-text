//! Shared runtime for one realtime speech interpretation session.
//!
//! The core owns capture, translation and output ports through dedicated tasks. Facades provide
//! preflighted ports and callbacks without duplicating queueing, framing, supervision or cleanup.

mod frame_assembler;
mod runtime_supervisor;
mod session;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{oneshot, Notify};

use crate::domain::{
    AudioCapture, AudioChunkCallback, AudioConfig, AudioError, RealtimeTranslationSession,
    TranslationAudioOutput, TranslationAudioOutputConfig, TranslationAudioOutputError,
};

#[derive(Clone, Copy)]
pub(crate) struct RealtimeStartupPolicy {
    pub(crate) device_start_timeout: Duration,
    pub(crate) network_connect_timeout: Duration,
}

impl Default for RealtimeStartupPolicy {
    fn default() -> Self {
        Self {
            device_start_timeout: Duration::from_secs(5),
            network_connect_timeout: Duration::from_secs(15),
        }
    }
}

const REALTIME_STARTUP_CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) enum StartupOutputError {
    Operation(TranslationAudioOutputError),
    Timeout,
    Worker(String),
}

#[derive(Clone, Debug)]
pub(crate) struct StartupCleanup {
    confirmed: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl StartupCleanup {
    fn new() -> Self {
        Self {
            confirmed: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(Notify::new()),
        }
    }

    fn confirm(&self) {
        self.confirmed.store(true, Ordering::Release);
        self.notify.notify_waiters();
        // Keep one permit for a waiter that observed `false` immediately before
        // this confirmation but had not yet registered its Notified future.
        self.notify.notify_one();
    }

    pub(crate) fn is_confirmed(&self) -> bool {
        self.confirmed.load(Ordering::Acquire)
    }

    pub(crate) fn same_as(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.confirmed, &other.confirmed)
    }

    pub(crate) async fn wait(&self) {
        loop {
            if self.is_confirmed() {
                return;
            }
            let notified = self.notify.notified();
            if self.is_confirmed() {
                return;
            }
            notified.await;
        }
    }
}

pub(crate) enum StartupCaptureError {
    Operation(AudioError),
    Timeout(Option<StartupCleanup>),
    Worker(String),
}

async fn close_output_after_cancel(mut output: Box<dyn TranslationAudioOutput>) {
    let _ = output.close().await;
}

/// Runs native output startup away from Tokio workers. If the deadline wins, the blocking task
/// owns the adapter until the OS call returns and closes it before dropping it.
pub(crate) async fn open_startup_output(
    mut output: Box<dyn TranslationAudioOutput>,
    config: TranslationAudioOutputConfig,
    timeout: Duration,
) -> Result<Box<dyn TranslationAudioOutput>, StartupOutputError> {
    let runtime = tokio::runtime::Handle::current();
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_in_worker = cancelled.clone();
    let (result_tx, result_rx) = oneshot::channel();
    let (cleanup_tx, cleanup_rx) = oneshot::channel();
    tokio::task::spawn_blocking(move || {
        let result = runtime.block_on(output.open(config));
        if cancelled_in_worker.load(Ordering::SeqCst) {
            runtime.block_on(close_output_after_cancel(output));
            let _ = cleanup_tx.send(());
            return;
        }
        let payload = match result {
            Ok(()) => Ok(output),
            Err(error) => {
                runtime.block_on(close_output_after_cancel(output));
                Err(StartupOutputError::Operation(error))
            }
        };
        if let Err(Ok(output)) = result_tx.send(payload) {
            runtime.block_on(close_output_after_cancel(output));
        }
        let _ = cleanup_tx.send(());
    });

    match tokio::time::timeout(timeout, result_rx).await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => Err(StartupOutputError::Worker(format!(
            "native output startup worker stopped: {error}"
        ))),
        Err(_) => {
            cancelled.store(true, Ordering::SeqCst);
            let _ = tokio::time::timeout(REALTIME_STARTUP_CLEANUP_TIMEOUT, cleanup_rx).await;
            Err(StartupOutputError::Timeout)
        }
    }
}

pub(crate) async fn initialize_startup_capture(
    mut capture: Box<dyn AudioCapture>,
    config: AudioConfig,
    timeout: Duration,
) -> Result<Box<dyn AudioCapture>, StartupCaptureError> {
    let runtime = tokio::runtime::Handle::current();
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_in_worker = cancelled.clone();
    let (result_tx, result_rx) = oneshot::channel();
    let cleanup = StartupCleanup::new();
    let cleanup_in_worker = cleanup.clone();
    tokio::task::spawn_blocking(move || {
        let result = runtime.block_on(capture.initialize(config));
        if cancelled_in_worker.load(Ordering::SeqCst) {
            cleanup_in_worker.confirm();
            return;
        }
        let payload = result
            .map(|()| capture)
            .map_err(StartupCaptureError::Operation);
        let _ = result_tx.send(payload);
        cleanup_in_worker.confirm();
    });

    match tokio::time::timeout(timeout, result_rx).await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => Err(StartupCaptureError::Worker(format!(
            "native capture initialization worker stopped: {error}"
        ))),
        Err(_) => {
            cancelled.store(true, Ordering::SeqCst);
            let pending = if tokio::time::timeout(REALTIME_STARTUP_CLEANUP_TIMEOUT, cleanup.wait())
                .await
                .is_ok()
            {
                None
            } else {
                Some(cleanup)
            };
            Err(StartupCaptureError::Timeout(pending))
        }
    }
}

pub(crate) async fn start_owned_capture(
    mut capture: Box<dyn AudioCapture>,
    callback: AudioChunkCallback,
    timeout: Duration,
) -> Result<Box<dyn AudioCapture>, StartupCaptureError> {
    let runtime = tokio::runtime::Handle::current();
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_in_worker = cancelled.clone();
    let (result_tx, result_rx) = oneshot::channel();
    let cleanup = StartupCleanup::new();
    let cleanup_in_worker = cleanup.clone();
    tokio::task::spawn_blocking(move || {
        let result = runtime.block_on(capture.start_capture(callback));
        if cancelled_in_worker.load(Ordering::SeqCst) {
            let _ = runtime.block_on(capture.stop_capture());
            capture.set_terminal_error_callback(None);
            cleanup_in_worker.confirm();
            return;
        }
        let payload = match result {
            Ok(()) => Ok(capture),
            Err(error) => {
                let _ = runtime.block_on(capture.stop_capture());
                capture.set_terminal_error_callback(None);
                Err(StartupCaptureError::Operation(error))
            }
        };
        if let Err(Ok(mut capture)) = result_tx.send(payload) {
            let _ = runtime.block_on(capture.stop_capture());
            capture.set_terminal_error_callback(None);
        }
        cleanup_in_worker.confirm();
    });

    match tokio::time::timeout(timeout, result_rx).await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => Err(StartupCaptureError::Worker(format!(
            "native capture startup worker stopped: {error}"
        ))),
        Err(_) => {
            cancelled.store(true, Ordering::SeqCst);
            let pending = if tokio::time::timeout(REALTIME_STARTUP_CLEANUP_TIMEOUT, cleanup.wait())
                .await
                .is_ok()
            {
                None
            } else {
                Some(cleanup)
            };
            Err(StartupCaptureError::Timeout(pending))
        }
    }
}

pub(crate) async fn close_startup_output(mut output: Box<dyn TranslationAudioOutput>) {
    let runtime = tokio::runtime::Handle::current();
    let cleanup = tokio::task::spawn_blocking(move || runtime.block_on(output.close()));
    match tokio::time::timeout(REALTIME_STARTUP_CLEANUP_TIMEOUT, cleanup).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(error))) => log::warn!("realtime startup output cleanup failed: {error}"),
        Ok(Err(error)) => log::warn!("realtime startup output cleanup worker failed: {error}"),
        Err(_) => log::warn!("realtime startup output cleanup timed out"),
    }
}

pub(crate) async fn close_startup_capture(mut capture: Box<dyn AudioCapture>) {
    let runtime = tokio::runtime::Handle::current();
    let cleanup = tokio::task::spawn_blocking(move || {
        let result = runtime.block_on(capture.stop_capture());
        capture.set_terminal_error_callback(None);
        result
    });
    match tokio::time::timeout(REALTIME_STARTUP_CLEANUP_TIMEOUT, cleanup).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(error))) => log::warn!("realtime startup capture cleanup failed: {error}"),
        Ok(Err(error)) => log::warn!("realtime startup capture cleanup worker failed: {error}"),
        Err(_) => log::warn!("realtime startup capture cleanup timed out"),
    }
}

pub(crate) async fn abort_startup_translation(translation: &mut dyn RealtimeTranslationSession) {
    if tokio::time::timeout(REALTIME_STARTUP_CLEANUP_TIMEOUT, translation.abort())
        .await
        .is_err()
    {
        log::warn!("realtime startup translation cleanup timed out");
    }
}

pub(crate) use runtime_supervisor::*;
pub(crate) use session::*;
