//! IncomingTranslationService - text subtitles for system audio.
//!
//! Pipeline:
//! - macOS ScreenCaptureKit system audio capture, 16 kHz mono PCM16
//! - STT provider from current app config
//! - finalized transcript chunks -> OpenAI text translation
//! - translated text -> UI events
//!
//! This is separate from dictation and outgoing live translation:
//! - no auto-paste/copy/history
//! - no recording hotkey ownership
//! - no virtual microphone output

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::task::JoinHandle;

use crate::domain::{
    AudioCapture, AudioChunk, AudioChunkCallback, ConnectionQualityCallback, ErrorCallback,
    RecordingStatus, SttConfig, SttError, SttProvider, SttProviderFactory, Transcription,
    TranscriptionCallback,
};
#[cfg(target_os = "macos")]
use crate::infrastructure::audio::MacosSystemAudioCapture;
use crate::infrastructure::openai::{OpenAITextTranslationClient, OpenAITextTranslationError};
use crate::infrastructure::DefaultSttProviderFactory;

const TARGET_LANGUAGE_DEFAULT: &str = "ru";
const AUDIO_QUEUE_CAPACITY: usize = 256;
const TRANSLATION_QUEUE_CAPACITY: usize = 64;
const STOP_DRAIN_TIMEOUT_MS: u64 = 1_800;
const SILENCE_PEAK_THRESHOLD: i32 = 220;
const SILENCE_KEEPALIVE_CHUNKS: u32 = 25;
const TRANSLATION_FAILURES_BEFORE_UI_ERROR: u32 = 3;

#[derive(Debug, Clone)]
pub struct IncomingTranslationConfig {
    pub stt_config: SttConfig,
    pub openai_api_key: String,
    pub target_language: String,
    pub session_id: u64,
}

impl IncomingTranslationConfig {
    pub fn new_with_defaults(stt_config: SttConfig, session_id: u64) -> Self {
        Self {
            stt_config,
            openai_api_key: std::env::var("OPENAI_API_KEY").unwrap_or_default(),
            target_language: TARGET_LANGUAGE_DEFAULT.to_string(),
            session_id,
        }
    }
}

#[derive(Clone)]
pub struct IncomingTranslationCallbacks {
    pub on_source_final: Arc<dyn Fn(String) + Send + Sync>,
    pub on_translation_delta: Arc<dyn Fn(String) + Send + Sync>,
    pub on_error: Arc<dyn Fn(IncomingTranslationError) + Send + Sync>,
    pub on_status: Arc<dyn Fn(RecordingStatus) + Send + Sync>,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum IncomingTranslationError {
    #[error("configuration: {0}")]
    Configuration(String),
    #[error("authentication: {0}")]
    Authentication(String),
    #[error("rate_limited: {0}")]
    RateLimited(String),
    #[error("connection: {0}")]
    Connection(String),
    #[error("processing: {0}")]
    Processing(String),
}

impl IncomingTranslationError {
    pub fn error_type(&self) -> &'static str {
        match self {
            Self::Configuration(_) => "configuration",
            Self::Authentication(_) => "authentication",
            Self::RateLimited(_) => "rate_limited",
            Self::Connection(_) => "connection",
            Self::Processing(_) => "processing",
        }
    }
}

impl From<SttError> for IncomingTranslationError {
    fn from(err: SttError) -> Self {
        match err {
            SttError::Configuration(msg) => Self::Configuration(msg),
            SttError::Authentication(msg) => Self::Authentication(msg),
            SttError::Connection(conn) => Self::Connection(conn.to_string()),
            SttError::Processing(msg) | SttError::Unsupported(msg) | SttError::Internal(msg) => {
                Self::Processing(msg)
            }
        }
    }
}

impl From<OpenAITextTranslationError> for IncomingTranslationError {
    fn from(err: OpenAITextTranslationError) -> Self {
        let message = err.to_string();
        match err {
            OpenAITextTranslationError::Authentication(_) => Self::Authentication(message),
            OpenAITextTranslationError::RateLimited(_) => Self::RateLimited(message),
            OpenAITextTranslationError::Connection(_) => Self::Connection(message),
            OpenAITextTranslationError::Protocol(_) => Self::Processing(message),
        }
    }
}

pub struct IncomingTranslationService {
    status: Arc<RwLock<RecordingStatus>>,
    stt_factory: Arc<dyn SttProviderFactory>,
    inner: Arc<Mutex<Option<RunningIncomingSession>>>,
}

struct RunningIncomingSession {
    capture: Box<dyn AudioCapture>,
    stt_provider: Arc<Mutex<Box<dyn SttProvider>>>,
    audio_pump_task: JoinHandle<()>,
    translation_task: JoinHandle<()>,
    running: Arc<AtomicBool>,
    session_id: u64,
}

#[derive(Debug, Clone)]
struct TranslationJob {
    text: String,
    source: &'static str,
    start: f64,
    duration: f64,
}

impl Default for IncomingTranslationService {
    fn default() -> Self {
        Self::new()
    }
}

impl IncomingTranslationService {
    pub fn new() -> Self {
        Self {
            status: Arc::new(RwLock::new(RecordingStatus::Idle)),
            stt_factory: Arc::new(DefaultSttProviderFactory::new()),
            inner: Arc::new(Mutex::new(None)),
        }
    }

    #[cfg(test)]
    pub fn new_with_factory(stt_factory: Arc<dyn SttProviderFactory>) -> Self {
        Self {
            status: Arc::new(RwLock::new(RecordingStatus::Idle)),
            stt_factory,
            inner: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn get_status(&self) -> RecordingStatus {
        *self.status.read().await
    }

    pub async fn start(
        &self,
        config: IncomingTranslationConfig,
        callbacks: IncomingTranslationCallbacks,
    ) -> Result<(), IncomingTranslationError> {
        let mut guard = self.inner.lock().await;
        if guard.is_some() {
            return Err(IncomingTranslationError::Configuration(
                "Incoming translation session already active".to_string(),
            ));
        }

        if config.openai_api_key.trim().is_empty() {
            return Err(IncomingTranslationError::Configuration(
                "OpenAI API key не задан. Укажите ключ в Settings или задайте OPENAI_API_KEY"
                    .to_string(),
            ));
        }

        *self.status.write().await = RecordingStatus::Starting;
        (callbacks.on_status)(RecordingStatus::Starting);

        let mut capture = create_system_audio_capture()?;
        capture
            .initialize(crate::domain::AudioConfig::default())
            .await
            .map_err(|e| IncomingTranslationError::Configuration(e.to_string()))?;

        let mut provider = self.stt_factory.create(&config.stt_config)?;
        provider.initialize(&config.stt_config).await?;

        let translator = Arc::new(OpenAITextTranslationClient::new(
            config.openai_api_key.clone(),
        )?);
        let running = Arc::new(AtomicBool::new(true));
        let translated_segment_keys = Arc::new(StdMutex::new(HashSet::new()));
        let (translation_tx, translation_rx) =
            mpsc::channel::<TranslationJob>(TRANSLATION_QUEUE_CAPACITY);
        let translation_task = tokio::spawn(run_translation_worker(
            translation_rx,
            translator,
            callbacks.clone(),
            config.target_language.clone(),
            running.clone(),
        ));

        let callbacks_for_final = callbacks.clone();
        let running_for_final = running.clone();
        let translated_segment_keys_for_final = translated_segment_keys.clone();
        let translation_tx_for_final = translation_tx.clone();
        let on_final: TranscriptionCallback = Arc::new(move |transcription: Transcription| {
            handle_finalized_transcription(
                transcription,
                callbacks_for_final.clone(),
                running_for_final.clone(),
                translated_segment_keys_for_final.clone(),
                translation_tx_for_final.clone(),
                "final",
            );
        });

        let callbacks_for_partial = callbacks.clone();
        let running_for_partial = running.clone();
        let translated_segment_keys_for_partial = translated_segment_keys.clone();
        let translation_tx_for_partial = translation_tx.clone();
        let on_partial: TranscriptionCallback = Arc::new(move |transcription: Transcription| {
            if !transcription.is_final {
                return;
            }
            handle_finalized_transcription(
                transcription,
                callbacks_for_partial.clone(),
                running_for_partial.clone(),
                translated_segment_keys_for_partial.clone(),
                translation_tx_for_partial.clone(),
                "partial_final",
            );
        });

        let callbacks_for_error = callbacks.clone();
        let running_for_error = running.clone();
        let on_error: ErrorCallback = Arc::new(move |err: SttError| {
            if running_for_error.load(Ordering::Relaxed) {
                (callbacks_for_error.on_error)(err.into());
            }
        });
        let on_connection_quality: ConnectionQualityCallback =
            Arc::new(move |_quality: String, _reason: Option<String>| {});

        provider
            .start_stream(on_partial, on_final, on_error, on_connection_quality)
            .await?;

        let provider = Arc::new(Mutex::new(provider));
        let (audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(AUDIO_QUEUE_CAPACITY);
        let callback_running = running.clone();
        let on_chunk: AudioChunkCallback = Arc::new(move |chunk: AudioChunk| {
            if !callback_running.load(Ordering::Relaxed) {
                return;
            }
            let _ = audio_tx.try_send(chunk);
        });

        capture
            .start_capture(on_chunk)
            .await
            .map_err(|e| IncomingTranslationError::Configuration(e.to_string()))?;

        let pump_provider = provider.clone();
        let pump_callbacks = callbacks.clone();
        let pump_running = running.clone();
        let audio_pump_task = tokio::spawn(async move {
            run_audio_pump(audio_rx, pump_provider, pump_callbacks, pump_running).await;
        });

        *guard = Some(RunningIncomingSession {
            capture,
            stt_provider: provider,
            audio_pump_task,
            translation_task,
            running,
            session_id: config.session_id,
        });
        *self.status.write().await = RecordingStatus::Recording;
        (callbacks.on_status)(RecordingStatus::Recording);
        log::info!(
            "IncomingTranslationService: session {} started, target={}",
            config.session_id,
            config.target_language
        );
        Ok(())
    }

    pub async fn stop(&self) -> Result<(), IncomingTranslationError> {
        let mut guard = self.inner.lock().await;
        let Some(mut session) = guard.take() else {
            return Ok(());
        };

        *self.status.write().await = RecordingStatus::Processing;
        session.running.store(false, Ordering::SeqCst);

        if let Err(e) = session.capture.stop_capture().await {
            log::warn!(
                "IncomingTranslationService: stop capture failed for session {}: {}",
                session.session_id,
                e
            );
        }

        let _ = wait_task_done(
            &mut session.audio_pump_task,
            Duration::from_millis(STOP_DRAIN_TIMEOUT_MS),
            session.session_id,
        )
        .await;

        if let Err(e) = session.stt_provider.lock().await.stop_stream().await {
            log::warn!(
                "IncomingTranslationService: stt stop failed for session {}: {}",
                session.session_id,
                e
            );
            let _ = session.stt_provider.lock().await.abort().await;
        }

        session.translation_task.abort();
        let _ = session.translation_task.await;

        *self.status.write().await = RecordingStatus::Idle;
        Ok(())
    }
}

fn handle_finalized_transcription(
    transcription: Transcription,
    callbacks: IncomingTranslationCallbacks,
    running: Arc<AtomicBool>,
    translated_segment_keys: Arc<StdMutex<HashSet<String>>>,
    translation_tx: mpsc::Sender<TranslationJob>,
    source: &'static str,
) {
    let text = transcription.text.trim().to_string();
    if text.is_empty() || !running.load(Ordering::Relaxed) {
        return;
    }

    let key = finalized_segment_key(&transcription, &text);
    let should_translate = match translated_segment_keys.lock() {
        Ok(mut seen) => seen.insert(key),
        Err(err) => {
            log::warn!(
                "IncomingTranslationService: segment dedupe lock poisoned: {}",
                err
            );
            true
        }
    };

    if !should_translate {
        log::debug!(
            "IncomingTranslationService: skip duplicate {} segment '{}'",
            source,
            text
        );
        return;
    }

    log::info!(
        "IncomingTranslationService: translate {} segment len={}, start={:.2}s, duration={:.2}s",
        source,
        text.len(),
        transcription.start,
        transcription.duration
    );
    (callbacks.on_source_final)(text.clone());

    if let Err(err) = translation_tx.try_send(TranslationJob {
        text,
        source,
        start: transcription.start,
        duration: transcription.duration,
    }) {
        log::warn!(
            "IncomingTranslationService: translation queue full or closed, dropping {} segment: {}",
            source,
            err
        );
    }
}

async fn run_translation_worker(
    mut translation_rx: mpsc::Receiver<TranslationJob>,
    translator: Arc<OpenAITextTranslationClient>,
    callbacks: IncomingTranslationCallbacks,
    target_language: String,
    running: Arc<AtomicBool>,
) {
    let mut consecutive_failures = 0u32;

    while let Some(job) = translation_rx.recv().await {
        if !running.load(Ordering::Relaxed) {
            break;
        }

        log::info!(
            "IncomingTranslationService: request {} translation len={}, start={:.2}s, duration={:.2}s",
            job.source,
            job.text.len(),
            job.start,
            job.duration
        );

        match translator.translate_text(&job.text, &target_language).await {
            Ok(translated) => {
                consecutive_failures = 0;
                if running.load(Ordering::Relaxed) && !translated.trim().is_empty() {
                    (callbacks.on_translation_delta)(translated);
                }
            }
            Err(err) => {
                consecutive_failures = consecutive_failures.saturating_add(1);
                let should_emit = matches!(
                    err,
                    OpenAITextTranslationError::Authentication(_)
                        | OpenAITextTranslationError::RateLimited(_)
                ) || consecutive_failures >= TRANSLATION_FAILURES_BEFORE_UI_ERROR;

                log::warn!(
                    "IncomingTranslationService: translation failed ({}/{} before UI error): {}",
                    consecutive_failures,
                    TRANSLATION_FAILURES_BEFORE_UI_ERROR,
                    err
                );
                if should_emit && running.load(Ordering::Relaxed) {
                    (callbacks.on_error)(err.into());
                }
            }
        }
    }
}

fn finalized_segment_key(transcription: &Transcription, text: &str) -> String {
    if transcription.start > 0.0 || transcription.duration > 0.0 {
        return format!(
            "{:.3}:{:.3}:{}",
            transcription.start, transcription.duration, text
        );
    }

    text.to_string()
}

async fn run_audio_pump(
    mut audio_rx: mpsc::Receiver<AudioChunk>,
    provider: Arc<Mutex<Box<dyn SttProvider>>>,
    callbacks: IncomingTranslationCallbacks,
    running: Arc<AtomicBool>,
) {
    let mut silence_chunks = 0u32;

    while let Some(chunk) = audio_rx.recv().await {
        if !running.load(Ordering::Relaxed) {
            break;
        }
        if should_skip_silent_chunk(&chunk, &mut silence_chunks) {
            continue;
        }

        let result = provider.lock().await.send_audio(&chunk).await;
        if let Err(err) = result {
            if running.load(Ordering::Relaxed) {
                (callbacks.on_error)(err.into());
            }
            break;
        }
    }
}

fn should_skip_silent_chunk(chunk: &AudioChunk, silence_chunks: &mut u32) -> bool {
    let peak = chunk
        .data
        .iter()
        .map(|&sample| (sample as i32).abs())
        .max()
        .unwrap_or(0);

    if peak > SILENCE_PEAK_THRESHOLD {
        *silence_chunks = 0;
        return false;
    }

    *silence_chunks = silence_chunks.saturating_add(1);
    *silence_chunks > SILENCE_KEEPALIVE_CHUNKS
}

async fn wait_task_done(task: &mut JoinHandle<()>, timeout: Duration, session_id: u64) -> bool {
    tokio::select! {
        result = &mut *task => {
            if let Err(e) = result {
                if !e.is_cancelled() {
                    log::warn!("IncomingTranslationService: audio pump join failed for session {}: {}", session_id, e);
                }
            }
            true
        }
        _ = tokio::time::sleep(timeout) => {
            task.abort();
            let _ = task.await;
            false
        }
    }
}

#[cfg(target_os = "macos")]
fn create_system_audio_capture() -> Result<Box<dyn AudioCapture>, IncomingTranslationError> {
    Ok(Box::new(MacosSystemAudioCapture::new().map_err(|e| {
        IncomingTranslationError::Configuration(e.to_string())
    })?))
}

#[cfg(not(target_os = "macos"))]
fn create_system_audio_capture() -> Result<Box<dyn AudioCapture>, IncomingTranslationError> {
    Err(IncomingTranslationError::Configuration(
        "Incoming system audio translation is only supported on macOS in this MVP".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_gate_keeps_initial_silence_then_skips() {
        let mut silence = 0;
        let chunk = AudioChunk::new(vec![0; 480], 16_000, 1);

        for _ in 0..SILENCE_KEEPALIVE_CHUNKS {
            assert!(!should_skip_silent_chunk(&chunk, &mut silence));
        }
        assert!(should_skip_silent_chunk(&chunk, &mut silence));
    }

    #[test]
    fn silent_gate_resets_on_audio() {
        let mut silence = SILENCE_KEEPALIVE_CHUNKS + 10;
        let chunk = AudioChunk::new(vec![SILENCE_PEAK_THRESHOLD as i16 + 1; 480], 16_000, 1);

        assert!(!should_skip_silent_chunk(&chunk, &mut silence));
        assert_eq!(silence, 0);
    }

    #[test]
    fn finalized_segment_key_uses_timing_when_available() {
        let transcription = Transcription::final_result("hello".to_string()).with_timing(1.25, 0.5);

        assert_eq!(
            finalized_segment_key(&transcription, "hello"),
            "1.250:0.500:hello"
        );
    }
}
