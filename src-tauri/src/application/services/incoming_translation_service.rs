//! IncomingTranslationService - text subtitles for system audio.
//!
//! Pipeline:
//! - platform system audio capture, 16 kHz mono PCM16
//! - STT provider from current app config
//! - finalized transcript chunks -> OpenAI text translation
//! - translated text -> UI events
//!
//! This is separate from dictation and outgoing live translation:
//! - no auto-paste/copy/history
//! - no recording hotkey ownership
//! - no virtual microphone output

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::task::JoinHandle;

use crate::domain::{
    AudioCapture, AudioCaptureTarget, AudioChunk, AudioChunkCallback, AudioConfig,
    ConnectionQualityCallback, ErrorCallback, PlatformAudioFactory, RecordingStatus, SttConfig,
    SttError, SttProvider, SttProviderFactory, Transcription, TranscriptionCallback,
};
use crate::infrastructure::audio::DefaultPlatformAudioFactory;
use crate::infrastructure::openai::{OpenAITextTranslationClient, OpenAITextTranslationError};
use crate::infrastructure::DefaultSttProviderFactory;

const TARGET_LANGUAGE_DEFAULT: &str = "ru";
const AUDIO_QUEUE_CAPACITY: usize = 256;
const TRANSLATION_QUEUE_CAPACITY: usize = 64;
const STOP_DRAIN_TIMEOUT_MS: u64 = 1_800;
const STOP_TRANSLATION_DRAIN_TIMEOUT_MS: u64 = 3_000;
const STOP_TRANSLATION_DRAIN_POLL_MS: u64 = 20;
const SILENCE_PEAK_THRESHOLD: i32 = 220;
const SILENCE_KEEPALIVE_CHUNKS: u32 = 25;
const TRANSLATION_FAILURES_BEFORE_UI_ERROR: u32 = 3;
const TRANSLATED_SEGMENT_DEDUPE_CAPACITY: usize = 2048;

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

fn normalize_incoming_translation_target_language(value: &str) -> String {
    let language = value.trim();
    if language.is_empty()
        || language.eq_ignore_ascii_case("auto")
        || language.eq_ignore_ascii_case("multi")
    {
        TARGET_LANGUAGE_DEFAULT.to_string()
    } else {
        language.to_string()
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
    audio_factory: Arc<dyn PlatformAudioFactory>,
    translator_factory: Arc<dyn TextTranslatorFactory>,
    inner: Arc<Mutex<Option<RunningIncomingSession>>>,
}

struct RunningIncomingSession {
    capture: Box<dyn AudioCapture>,
    stt_provider: Arc<Mutex<Box<dyn SttProvider>>>,
    audio_pump_task: JoinHandle<()>,
    translation_task: JoinHandle<()>,
    pending_translations: Arc<AtomicUsize>,
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

#[derive(Debug)]
struct BoundedSegmentDedupe {
    capacity: usize,
    keys: HashSet<String>,
    order: VecDeque<String>,
}

impl BoundedSegmentDedupe {
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            keys: HashSet::new(),
            order: VecDeque::new(),
        }
    }

    fn remember(&mut self, key: String) -> bool {
        if self.keys.contains(&key) {
            return false;
        }

        if self.keys.len() >= self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.keys.remove(&oldest);
            }
        }

        self.keys.insert(key.clone());
        self.order.push_back(key);
        true
    }

    fn forget(&mut self, key: &str) {
        if self.keys.remove(key) {
            self.order.retain(|stored| stored != key);
        }
    }

    #[cfg(test)]
    fn contains(&self, key: &str) -> bool {
        self.keys.contains(key)
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

#[async_trait]
trait TextTranslator: Send + Sync {
    async fn translate_text(
        &self,
        text: &str,
        target_language: &str,
    ) -> Result<String, OpenAITextTranslationError>;
}

trait TextTranslatorFactory: Send + Sync {
    fn create(
        &self,
        api_key: String,
    ) -> Result<Arc<dyn TextTranslator>, OpenAITextTranslationError>;
}

struct OpenAITextTranslatorFactory;

impl TextTranslatorFactory for OpenAITextTranslatorFactory {
    fn create(
        &self,
        api_key: String,
    ) -> Result<Arc<dyn TextTranslator>, OpenAITextTranslationError> {
        Ok(Arc::new(OpenAITextTranslationClient::new(api_key)?))
    }
}

#[async_trait]
impl TextTranslator for OpenAITextTranslationClient {
    async fn translate_text(
        &self,
        text: &str,
        target_language: &str,
    ) -> Result<String, OpenAITextTranslationError> {
        OpenAITextTranslationClient::translate_text(self, text, target_language).await
    }
}

impl Default for IncomingTranslationService {
    fn default() -> Self {
        Self::new()
    }
}

impl IncomingTranslationService {
    pub fn new() -> Self {
        Self::new_with_factories(
            Arc::new(DefaultSttProviderFactory::new()),
            Arc::new(DefaultPlatformAudioFactory::new()),
        )
    }

    pub fn new_with_factories(
        stt_factory: Arc<dyn SttProviderFactory>,
        audio_factory: Arc<dyn PlatformAudioFactory>,
    ) -> Self {
        Self {
            status: Arc::new(RwLock::new(RecordingStatus::Idle)),
            stt_factory,
            audio_factory,
            translator_factory: Arc::new(OpenAITextTranslatorFactory),
            inner: Arc::new(Mutex::new(None)),
        }
    }

    #[cfg(test)]
    pub fn new_with_factory(stt_factory: Arc<dyn SttProviderFactory>) -> Self {
        Self {
            status: Arc::new(RwLock::new(RecordingStatus::Idle)),
            stt_factory,
            audio_factory: Arc::new(DefaultPlatformAudioFactory::new()),
            translator_factory: Arc::new(OpenAITextTranslatorFactory),
            inner: Arc::new(Mutex::new(None)),
        }
    }

    #[cfg(test)]
    fn new_with_all_factories(
        stt_factory: Arc<dyn SttProviderFactory>,
        audio_factory: Arc<dyn PlatformAudioFactory>,
        translator_factory: Arc<dyn TextTranslatorFactory>,
    ) -> Self {
        Self {
            status: Arc::new(RwLock::new(RecordingStatus::Idle)),
            stt_factory,
            audio_factory,
            translator_factory,
            inner: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn get_status(&self) -> RecordingStatus {
        *self.status.read().await
    }

    pub async fn active_session_id(&self) -> Option<u64> {
        self.inner
            .lock()
            .await
            .as_ref()
            .map(|session| session.session_id)
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
        let target_language =
            normalize_incoming_translation_target_language(&config.target_language);

        *self.status.write().await = RecordingStatus::Starting;
        (callbacks.on_status)(RecordingStatus::Starting);

        let audio_target = AudioCaptureTarget::incoming_subtitles();
        let mut capture = match self
            .audio_factory
            .create_system_loopback_capture(audio_target)
        {
            Ok(capture) => capture,
            Err(e) => {
                self.reset_failed_start().await;
                return Err(IncomingTranslationError::Configuration(e.to_string()));
            }
        };
        if let Err(e) = capture
            .initialize(AudioConfig {
                sample_rate: audio_target.sample_rate,
                channels: audio_target.channels,
                buffer_size: AudioConfig::default().buffer_size,
            })
            .await
        {
            self.reset_failed_start().await;
            return Err(IncomingTranslationError::Configuration(e.to_string()));
        }

        let mut provider = match self.stt_factory.create(&config.stt_config) {
            Ok(provider) => provider,
            Err(e) => {
                self.reset_failed_start().await;
                return Err(e.into());
            }
        };
        if let Err(e) = provider.initialize(&config.stt_config).await {
            self.reset_failed_start().await;
            return Err(e.into());
        }

        let translator = match self
            .translator_factory
            .create(config.openai_api_key.clone())
        {
            Ok(translator) => translator,
            Err(e) => {
                abort_initialized_stt_after_start_failure(
                    &mut provider,
                    config.session_id,
                    "translator create",
                )
                .await;
                self.reset_failed_start().await;
                return Err(e.into());
            }
        };
        let running = Arc::new(AtomicBool::new(true));
        let pending_translations = Arc::new(AtomicUsize::new(0));
        let translated_segment_keys = Arc::new(StdMutex::new(BoundedSegmentDedupe::new(
            TRANSLATED_SEGMENT_DEDUPE_CAPACITY,
        )));
        let (translation_tx, translation_rx) =
            mpsc::channel::<TranslationJob>(TRANSLATION_QUEUE_CAPACITY);
        let (runtime_cleanup_tx, runtime_cleanup_rx) = mpsc::unbounded_channel::<()>();
        let translation_task = tokio::spawn(run_translation_worker(
            translation_rx,
            translator,
            callbacks.clone(),
            target_language.clone(),
            running.clone(),
            pending_translations.clone(),
            self.status.clone(),
            runtime_cleanup_tx.clone(),
        ));

        let callbacks_for_final = callbacks.clone();
        let running_for_final = running.clone();
        let translated_segment_keys_for_final = translated_segment_keys.clone();
        let translation_tx_for_final = translation_tx.clone();
        let pending_translations_for_final = pending_translations.clone();
        let on_final: TranscriptionCallback = Arc::new(move |transcription: Transcription| {
            handle_finalized_transcription(
                transcription,
                callbacks_for_final.clone(),
                running_for_final.clone(),
                translated_segment_keys_for_final.clone(),
                translation_tx_for_final.clone(),
                pending_translations_for_final.clone(),
                "final",
            );
        });

        let callbacks_for_partial = callbacks.clone();
        let running_for_partial = running.clone();
        let translated_segment_keys_for_partial = translated_segment_keys.clone();
        let translation_tx_for_partial = translation_tx.clone();
        let pending_translations_for_partial = pending_translations.clone();
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
                pending_translations_for_partial.clone(),
                "partial_final",
            );
        });

        let callbacks_for_error = callbacks.clone();
        let running_for_error = running.clone();
        let status_for_error = self.status.clone();
        let runtime_cleanup_tx_for_error = runtime_cleanup_tx.clone();
        let on_error: ErrorCallback = Arc::new(move |err: SttError| {
            if running_for_error
                .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                let status = status_for_error.clone();
                let callbacks = callbacks_for_error.clone();
                let runtime_cleanup_tx = runtime_cleanup_tx_for_error.clone();
                tokio::spawn(async move {
                    *status.write().await = RecordingStatus::Error;
                    (callbacks.on_error)(err.into());
                    (callbacks.on_status)(RecordingStatus::Error);
                    let _ = runtime_cleanup_tx.send(());
                });
            }
        });
        let on_connection_quality: ConnectionQualityCallback =
            Arc::new(move |_quality: String, _reason: Option<String>| {});

        if let Err(e) = provider
            .start_stream(on_partial, on_final, on_error, on_connection_quality)
            .await
        {
            running.store(false, Ordering::SeqCst);
            translation_task.abort();
            let _ = translation_task.await;
            abort_initialized_stt_after_start_failure(
                &mut provider,
                config.session_id,
                "stt start_stream",
            )
            .await;
            self.reset_failed_start().await;
            return Err(e.into());
        }

        let provider = Arc::new(Mutex::new(provider));
        let (audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(AUDIO_QUEUE_CAPACITY);
        let callback_running = running.clone();
        let dropped_audio_chunks = Arc::new(AtomicU64::new(0));
        let on_chunk: AudioChunkCallback = Arc::new(move |chunk: AudioChunk| {
            if !callback_running.load(Ordering::Relaxed) {
                return;
            }
            try_enqueue_audio_chunk(&audio_tx, chunk, &dropped_audio_chunks);
        });

        if let Err(e) = capture.start_capture(on_chunk).await {
            cleanup_started_stt_after_capture_failure(
                provider.clone(),
                translation_task,
                running.clone(),
                config.session_id,
            )
            .await;
            self.reset_failed_start().await;
            return Err(IncomingTranslationError::Configuration(e.to_string()));
        }

        let pump_provider = provider.clone();
        let pump_callbacks = callbacks.clone();
        let pump_running = running.clone();
        let pump_status = self.status.clone();
        let pump_runtime_cleanup_tx = runtime_cleanup_tx.clone();
        let audio_pump_task = tokio::spawn(async move {
            run_audio_pump(
                audio_rx,
                pump_provider,
                pump_callbacks,
                pump_running,
                pump_status,
                pump_runtime_cleanup_tx,
            )
            .await;
        });

        *guard = Some(RunningIncomingSession {
            capture,
            stt_provider: provider,
            audio_pump_task,
            translation_task,
            pending_translations,
            running: running.clone(),
            session_id: config.session_id,
        });
        spawn_runtime_cleanup_monitor(
            self.inner.clone(),
            self.status.clone(),
            runtime_cleanup_rx,
            config.session_id,
        );
        if mark_incoming_recording_started(&self.status, &running, &callbacks).await {
            log::info!(
                "IncomingTranslationService: session {} started, target={}",
                config.session_id,
                target_language
            );
        } else {
            log::warn!(
                "IncomingTranslationService: session {} failed before start completed",
                config.session_id
            );
        }
        Ok(())
    }

    async fn reset_failed_start(&self) {
        *self.status.write().await = RecordingStatus::Idle;
    }

    pub async fn stop(&self) -> Result<(), IncomingTranslationError> {
        let mut guard = self.inner.lock().await;
        let Some(mut session) = guard.take() else {
            *self.status.write().await = RecordingStatus::Idle;
            return Ok(());
        };

        *self.status.write().await = RecordingStatus::Processing;

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

        wait_pending_translations(
            session.pending_translations.clone(),
            Duration::from_millis(STOP_TRANSLATION_DRAIN_TIMEOUT_MS),
            session.session_id,
        )
        .await;

        session.running.store(false, Ordering::SeqCst);
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
    translated_segment_keys: Arc<StdMutex<BoundedSegmentDedupe>>,
    translation_tx: mpsc::Sender<TranslationJob>,
    pending_translations: Arc<AtomicUsize>,
    source: &'static str,
) {
    let text = transcription.text.trim().to_string();
    if text.is_empty() || !running.load(Ordering::Relaxed) {
        return;
    }

    let key = finalized_segment_key(&transcription, &text);
    let should_translate = match key.as_ref() {
        Some(key) => remember_translated_segment_key(&translated_segment_keys, key),
        None => true,
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

    pending_translations.fetch_add(1, Ordering::SeqCst);
    if let Err(err) = translation_tx.try_send(TranslationJob {
        text,
        source,
        start: transcription.start,
        duration: transcription.duration,
    }) {
        decrement_pending_translations(&pending_translations);
        if let Some(key) = key.as_deref() {
            forget_translated_segment_key(&translated_segment_keys, key);
        }
        log::warn!(
            "IncomingTranslationService: translation queue full or closed, dropping {} segment: {}",
            source,
            err
        );
    }
}

fn remember_translated_segment_key(
    translated_segment_keys: &StdMutex<BoundedSegmentDedupe>,
    key: &str,
) -> bool {
    match translated_segment_keys.lock() {
        Ok(mut seen) => seen.remember(key.to_string()),
        Err(err) => {
            log::warn!(
                "IncomingTranslationService: segment dedupe lock poisoned: {}",
                err
            );
            true
        }
    }
}

fn forget_translated_segment_key(
    translated_segment_keys: &StdMutex<BoundedSegmentDedupe>,
    key: &str,
) {
    if let Ok(mut seen) = translated_segment_keys.lock() {
        seen.forget(key);
    }
}

fn should_mark_incoming_recording_started(
    current_status: RecordingStatus,
    is_running: bool,
) -> bool {
    is_running && current_status == RecordingStatus::Starting
}

async fn mark_incoming_recording_started(
    status: &Arc<RwLock<RecordingStatus>>,
    running: &AtomicBool,
    callbacks: &IncomingTranslationCallbacks,
) -> bool {
    let mut status_guard = status.write().await;
    if !should_mark_incoming_recording_started(*status_guard, running.load(Ordering::SeqCst)) {
        return false;
    }

    *status_guard = RecordingStatus::Recording;
    drop(status_guard);
    (callbacks.on_status)(RecordingStatus::Recording);
    true
}

async fn run_translation_worker(
    mut translation_rx: mpsc::Receiver<TranslationJob>,
    translator: Arc<dyn TextTranslator>,
    callbacks: IncomingTranslationCallbacks,
    target_language: String,
    running: Arc<AtomicBool>,
    pending_translations: Arc<AtomicUsize>,
    status: Arc<RwLock<RecordingStatus>>,
    runtime_cleanup_tx: mpsc::UnboundedSender<()>,
) {
    let mut consecutive_failures = 0u32;

    while let Some(job) = translation_rx.recv().await {
        let mut stop_after_error = false;
        if !running.load(Ordering::Relaxed) {
            decrement_pending_translations(&pending_translations);
            drain_pending_translation_jobs(&mut translation_rx, &pending_translations);
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
                    running.store(false, Ordering::SeqCst);
                    *status.write().await = RecordingStatus::Error;
                    (callbacks.on_error)(err.into());
                    (callbacks.on_status)(RecordingStatus::Error);
                    let _ = runtime_cleanup_tx.send(());
                    stop_after_error = true;
                }
            }
        }

        decrement_pending_translations(&pending_translations);
        if stop_after_error {
            drain_pending_translation_jobs(&mut translation_rx, &pending_translations);
            break;
        }
    }
}

fn drain_pending_translation_jobs(
    translation_rx: &mut mpsc::Receiver<TranslationJob>,
    pending_translations: &AtomicUsize,
) {
    while translation_rx.try_recv().is_ok() {
        decrement_pending_translations(pending_translations);
    }
}

fn decrement_pending_translations(pending_translations: &AtomicUsize) {
    let _ = pending_translations.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
        Some(value.saturating_sub(1))
    });
}

fn finalized_segment_key(transcription: &Transcription, text: &str) -> Option<String> {
    if transcription.start <= 0.0 && transcription.duration <= 0.0 {
        return None;
    }

    Some(format!(
        "{:.3}:{:.3}:{}",
        transcription.start, transcription.duration, text
    ))
}

fn try_enqueue_audio_chunk(
    audio_tx: &mpsc::Sender<AudioChunk>,
    chunk: AudioChunk,
    dropped_audio_chunks: &AtomicU64,
) {
    match audio_tx.try_send(chunk) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(_chunk)) => {
            let dropped = dropped_audio_chunks.fetch_add(1, Ordering::Relaxed) + 1;
            if dropped == 1 || dropped % 100 == 0 {
                log::warn!(
                    "IncomingTranslationService: dropped {} system audio chunks because STT input queue is full",
                    dropped
                );
            }
        }
        Err(mpsc::error::TrySendError::Closed(_chunk)) => {}
    }
}

async fn run_audio_pump(
    mut audio_rx: mpsc::Receiver<AudioChunk>,
    provider: Arc<Mutex<Box<dyn SttProvider>>>,
    callbacks: IncomingTranslationCallbacks,
    running: Arc<AtomicBool>,
    status: Arc<RwLock<RecordingStatus>>,
    runtime_cleanup_tx: mpsc::UnboundedSender<()>,
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
                running.store(false, Ordering::SeqCst);
                *status.write().await = RecordingStatus::Error;
                (callbacks.on_error)(err.into());
                (callbacks.on_status)(RecordingStatus::Error);
                let _ = runtime_cleanup_tx.send(());
            }
            break;
        }
    }
}

fn spawn_runtime_cleanup_monitor(
    inner: Arc<Mutex<Option<RunningIncomingSession>>>,
    status: Arc<RwLock<RecordingStatus>>,
    mut runtime_cleanup_rx: mpsc::UnboundedReceiver<()>,
    session_id: u64,
) {
    tokio::spawn(async move {
        if runtime_cleanup_rx.recv().await.is_none() {
            return;
        }

        let mut guard = inner.lock().await;
        let is_current = guard
            .as_ref()
            .map(|session| session.session_id == session_id)
            .unwrap_or(false);
        let session = if is_current { guard.take() } else { None };

        let Some(session) = session else {
            return;
        };

        *status.write().await = RecordingStatus::Error;
        cleanup_session_after_runtime_error(session).await;
        drop(guard);
    });
}

async fn cleanup_session_after_runtime_error(mut session: RunningIncomingSession) {
    let session_id = session.session_id;

    if let Err(e) = session.capture.stop_capture().await {
        log::warn!(
            "IncomingTranslationService runtime cleanup: stop capture failed for session {}: {}",
            session_id,
            e
        );
    }

    let _ = wait_task_done(
        &mut session.audio_pump_task,
        Duration::from_millis(STOP_DRAIN_TIMEOUT_MS),
        session_id,
    )
    .await;

    if let Err(e) = session.stt_provider.lock().await.stop_stream().await {
        log::warn!(
            "IncomingTranslationService runtime cleanup: stt stop failed for session {}: {}",
            session_id,
            e
        );
        let _ = session.stt_provider.lock().await.abort().await;
    }

    session.running.store(false, Ordering::SeqCst);
    session.translation_task.abort();
    let _ = session.translation_task.await;

    log::info!(
        "IncomingTranslationService: session {} cleaned up after runtime error",
        session_id
    );
}

async fn cleanup_started_stt_after_capture_failure(
    provider: Arc<Mutex<Box<dyn SttProvider>>>,
    translation_task: JoinHandle<()>,
    running: Arc<AtomicBool>,
    session_id: u64,
) {
    running.store(false, Ordering::SeqCst);

    {
        let mut provider = provider.lock().await;
        if let Err(e) = provider.stop_stream().await {
            log::warn!(
                "IncomingTranslationService: stt stop after capture start failure failed for session {}: {}",
                session_id,
                e
            );
            if let Err(abort_err) = provider.abort().await {
                log::warn!(
                    "IncomingTranslationService: stt abort after capture start failure failed for session {}: {}",
                    session_id,
                    abort_err
                );
            }
        }
    }

    translation_task.abort();
    let _ = translation_task.await;
}

async fn abort_initialized_stt_after_start_failure(
    provider: &mut Box<dyn SttProvider>,
    session_id: u64,
    reason: &str,
) {
    if let Err(abort_err) = provider.abort().await {
        log::warn!(
            "IncomingTranslationService: stt abort after {} failure failed for session {}: {}",
            reason,
            session_id,
            abort_err
        );
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

async fn wait_pending_translations(
    pending_translations: Arc<AtomicUsize>,
    timeout: Duration,
    session_id: u64,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    while pending_translations.load(Ordering::SeqCst) > 0 {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            log::warn!(
                "IncomingTranslationService: translation drain timeout for session {} (pending={})",
                session_id,
                pending_translations.load(Ordering::SeqCst)
            );
            return;
        }

        tokio::time::sleep(
            Duration::from_millis(STOP_TRANSLATION_DRAIN_POLL_MS)
                .min(deadline.saturating_duration_since(now)),
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct TrackingProviderState {
        initialized: std::sync::atomic::AtomicBool,
        started: std::sync::atomic::AtomicBool,
        stopped: std::sync::atomic::AtomicBool,
        aborted: std::sync::atomic::AtomicBool,
    }

    struct TrackingSttProvider {
        state: std::sync::Arc<TrackingProviderState>,
        fail_stop: bool,
    }

    #[async_trait::async_trait]
    impl SttProvider for TrackingSttProvider {
        async fn initialize(&mut self, _config: &SttConfig) -> crate::domain::SttResult<()> {
            self.state.initialized.store(true, Ordering::SeqCst);
            Ok(())
        }

        async fn start_stream(
            &mut self,
            _on_partial: TranscriptionCallback,
            _on_final: TranscriptionCallback,
            _on_error: ErrorCallback,
            _on_connection_quality: ConnectionQualityCallback,
        ) -> crate::domain::SttResult<()> {
            self.state.started.store(true, Ordering::SeqCst);
            Ok(())
        }

        async fn send_audio(&mut self, _chunk: &AudioChunk) -> crate::domain::SttResult<()> {
            Ok(())
        }

        async fn stop_stream(&mut self) -> crate::domain::SttResult<()> {
            self.state.stopped.store(true, Ordering::SeqCst);
            if self.fail_stop {
                Err(SttError::Connection(
                    crate::domain::SttConnectionError::simple("simulated stop failure"),
                ))
            } else {
                Ok(())
            }
        }

        async fn abort(&mut self) -> crate::domain::SttResult<()> {
            self.state.aborted.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn name(&self) -> &str {
            "tracking"
        }

        fn is_online(&self) -> bool {
            true
        }
    }

    struct TrackingSttFactory {
        state: std::sync::Arc<TrackingProviderState>,
        fail_stop: bool,
    }

    impl SttProviderFactory for TrackingSttFactory {
        fn create(&self, _config: &SttConfig) -> crate::domain::SttResult<Box<dyn SttProvider>> {
            Ok(Box::new(TrackingSttProvider {
                state: self.state.clone(),
                fail_stop: self.fail_stop,
            }))
        }
    }

    #[derive(Default)]
    struct FailingLoopbackCapture {
        config: crate::domain::AudioConfig,
    }

    #[async_trait::async_trait]
    impl AudioCapture for FailingLoopbackCapture {
        async fn initialize(
            &mut self,
            config: crate::domain::AudioConfig,
        ) -> crate::domain::AudioResult<()> {
            self.config = config;
            Ok(())
        }

        async fn start_capture(
            &mut self,
            _on_chunk: AudioChunkCallback,
        ) -> crate::domain::AudioResult<()> {
            Err(crate::domain::AudioError::Capture(
                "simulated loopback start failure".to_string(),
            ))
        }

        async fn stop_capture(&mut self) -> crate::domain::AudioResult<()> {
            Ok(())
        }

        fn is_capturing(&self) -> bool {
            false
        }

        fn config(&self) -> crate::domain::AudioConfig {
            self.config
        }
    }

    struct FailingLoopbackAudioFactory;

    #[async_trait::async_trait]
    impl PlatformAudioFactory for FailingLoopbackAudioFactory {
        fn create_microphone_capture(
            &self,
            _device_name: Option<String>,
            _target: AudioCaptureTarget,
        ) -> crate::domain::AudioResult<Box<dyn AudioCapture>> {
            Err(crate::domain::AudioError::Configuration(
                "not used in incoming translation tests".to_string(),
            ))
        }

        fn create_translation_output(
            &self,
        ) -> crate::domain::TranslationAudioOutputResult<
            Box<dyn crate::domain::TranslationAudioOutput>,
        > {
            Err(crate::domain::TranslationAudioOutputError::Configuration(
                "not used in incoming translation tests".to_string(),
            ))
        }

        fn create_system_loopback_capture(
            &self,
            _target: AudioCaptureTarget,
        ) -> crate::domain::AudioResult<Box<dyn AudioCapture>> {
            Ok(Box::new(FailingLoopbackCapture::default()))
        }

        async fn setup_status(&self) -> crate::domain::PlatformAudioSetupStatus {
            crate::domain::PlatformAudioSetupStatus {
                platform: "test".to_string(),
                status: crate::domain::PlatformAudioSetupState::Ready,
                outgoing_supported: true,
                incoming_supported: true,
                virtual_microphone_name: "test".to_string(),
                message: "ready".to_string(),
            }
        }

        fn is_virtual_microphone_input(&self, _name: &str) -> bool {
            false
        }
    }

    #[derive(Default)]
    struct SyntheticIncomingCaptureState {
        initialized_config: StdMutex<Option<AudioConfig>>,
        started: std::sync::atomic::AtomicBool,
        stopped: std::sync::atomic::AtomicBool,
    }

    struct SyntheticIncomingCapture {
        state: std::sync::Arc<SyntheticIncomingCaptureState>,
        chunks: Vec<AudioChunk>,
        config: AudioConfig,
    }

    #[async_trait::async_trait]
    impl AudioCapture for SyntheticIncomingCapture {
        async fn initialize(&mut self, config: AudioConfig) -> crate::domain::AudioResult<()> {
            self.config = config;
            *self.state.initialized_config.lock().unwrap() = Some(config);
            Ok(())
        }

        async fn start_capture(
            &mut self,
            on_chunk: AudioChunkCallback,
        ) -> crate::domain::AudioResult<()> {
            self.state.started.store(true, Ordering::SeqCst);
            for chunk in self.chunks.clone() {
                on_chunk(chunk);
            }
            Ok(())
        }

        async fn stop_capture(&mut self) -> crate::domain::AudioResult<()> {
            self.state.stopped.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn is_capturing(&self) -> bool {
            self.state.started.load(Ordering::SeqCst) && !self.state.stopped.load(Ordering::SeqCst)
        }

        fn config(&self) -> AudioConfig {
            self.config
        }
    }

    struct SyntheticIncomingAudioFactory {
        capture_state: std::sync::Arc<SyntheticIncomingCaptureState>,
        requested_target: std::sync::Arc<StdMutex<Option<AudioCaptureTarget>>>,
    }

    #[async_trait::async_trait]
    impl PlatformAudioFactory for SyntheticIncomingAudioFactory {
        fn create_microphone_capture(
            &self,
            _device_name: Option<String>,
            _target: AudioCaptureTarget,
        ) -> crate::domain::AudioResult<Box<dyn AudioCapture>> {
            Err(crate::domain::AudioError::Configuration(
                "not used in incoming translation e2e".to_string(),
            ))
        }

        fn create_translation_output(
            &self,
        ) -> crate::domain::TranslationAudioOutputResult<
            Box<dyn crate::domain::TranslationAudioOutput>,
        > {
            Err(crate::domain::TranslationAudioOutputError::Configuration(
                "not used in incoming translation e2e".to_string(),
            ))
        }

        fn create_system_loopback_capture(
            &self,
            target: AudioCaptureTarget,
        ) -> crate::domain::AudioResult<Box<dyn AudioCapture>> {
            *self.requested_target.lock().unwrap() = Some(target);
            Ok(Box::new(SyntheticIncomingCapture {
                state: self.capture_state.clone(),
                chunks: vec![AudioChunk::new(vec![1_000; 1_600], 16_000, 1)],
                config: AudioConfig::default(),
            }))
        }

        async fn setup_status(&self) -> crate::domain::PlatformAudioSetupStatus {
            crate::domain::PlatformAudioSetupStatus {
                platform: "test".to_string(),
                status: crate::domain::PlatformAudioSetupState::Ready,
                outgoing_supported: true,
                incoming_supported: true,
                virtual_microphone_name: "synthetic".to_string(),
                message: "ready".to_string(),
            }
        }

        fn is_virtual_microphone_input(&self, _name: &str) -> bool {
            false
        }
    }

    #[derive(Default)]
    struct SyntheticIncomingProviderState {
        initialized: std::sync::atomic::AtomicBool,
        started: std::sync::atomic::AtomicBool,
        stopped: std::sync::atomic::AtomicBool,
        fail_send: std::sync::atomic::AtomicBool,
        sent_chunks: std::sync::atomic::AtomicUsize,
        final_callback: StdMutex<Option<TranscriptionCallback>>,
        error_callback: StdMutex<Option<ErrorCallback>>,
    }

    struct SyntheticIncomingSttProvider {
        state: std::sync::Arc<SyntheticIncomingProviderState>,
    }

    #[async_trait::async_trait]
    impl SttProvider for SyntheticIncomingSttProvider {
        async fn initialize(&mut self, _config: &SttConfig) -> crate::domain::SttResult<()> {
            self.state.initialized.store(true, Ordering::SeqCst);
            Ok(())
        }

        async fn start_stream(
            &mut self,
            _on_partial: TranscriptionCallback,
            on_final: TranscriptionCallback,
            on_error: ErrorCallback,
            _on_connection_quality: ConnectionQualityCallback,
        ) -> crate::domain::SttResult<()> {
            self.state.started.store(true, Ordering::SeqCst);
            *self.state.final_callback.lock().unwrap() = Some(on_final);
            *self.state.error_callback.lock().unwrap() = Some(on_error);
            Ok(())
        }

        async fn send_audio(&mut self, chunk: &AudioChunk) -> crate::domain::SttResult<()> {
            if self.state.fail_send.load(Ordering::SeqCst) {
                return Err(SttError::Processing(
                    "simulated send_audio failure".to_string(),
                ));
            }

            let call = self.state.sent_chunks.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                if let Some(on_final) = self.state.final_callback.lock().unwrap().as_ref() {
                    on_final(
                        Transcription::final_result("hello from zoom".to_string())
                            .with_timing(0.0, chunk.duration_ms() as f64 / 1000.0),
                    );
                }
            }
            Ok(())
        }

        async fn stop_stream(&mut self) -> crate::domain::SttResult<()> {
            self.state.stopped.store(true, Ordering::SeqCst);
            Ok(())
        }

        async fn abort(&mut self) -> crate::domain::SttResult<()> {
            Ok(())
        }

        fn name(&self) -> &str {
            "synthetic-incoming-stt"
        }

        fn is_online(&self) -> bool {
            true
        }
    }

    struct SyntheticIncomingSttFactory {
        state: std::sync::Arc<SyntheticIncomingProviderState>,
    }

    impl SttProviderFactory for SyntheticIncomingSttFactory {
        fn create(&self, _config: &SttConfig) -> crate::domain::SttResult<Box<dyn SttProvider>> {
            Ok(Box::new(SyntheticIncomingSttProvider {
                state: self.state.clone(),
            }))
        }
    }

    #[derive(Default)]
    struct StopFinalProviderState {
        stopped: std::sync::atomic::AtomicBool,
        final_callback: StdMutex<Option<TranscriptionCallback>>,
    }

    struct StopFinalSttProvider {
        state: std::sync::Arc<StopFinalProviderState>,
    }

    #[async_trait::async_trait]
    impl SttProvider for StopFinalSttProvider {
        async fn initialize(&mut self, _config: &SttConfig) -> crate::domain::SttResult<()> {
            Ok(())
        }

        async fn start_stream(
            &mut self,
            _on_partial: TranscriptionCallback,
            on_final: TranscriptionCallback,
            _on_error: ErrorCallback,
            _on_connection_quality: ConnectionQualityCallback,
        ) -> crate::domain::SttResult<()> {
            *self.state.final_callback.lock().unwrap() = Some(on_final);
            Ok(())
        }

        async fn send_audio(&mut self, _chunk: &AudioChunk) -> crate::domain::SttResult<()> {
            Ok(())
        }

        async fn stop_stream(&mut self) -> crate::domain::SttResult<()> {
            self.state.stopped.store(true, Ordering::SeqCst);
            if let Some(on_final) = self.state.final_callback.lock().unwrap().as_ref() {
                on_final(
                    Transcription::final_result("late final from stop".to_string())
                        .with_timing(1.0, 0.5),
                );
            }
            Ok(())
        }

        async fn abort(&mut self) -> crate::domain::SttResult<()> {
            Ok(())
        }

        fn name(&self) -> &str {
            "stop-final-stt"
        }

        fn is_online(&self) -> bool {
            true
        }
    }

    struct StopFinalSttFactory {
        state: std::sync::Arc<StopFinalProviderState>,
    }

    impl SttProviderFactory for StopFinalSttFactory {
        fn create(&self, _config: &SttConfig) -> crate::domain::SttResult<Box<dyn SttProvider>> {
            Ok(Box::new(StopFinalSttProvider {
                state: self.state.clone(),
            }))
        }
    }

    #[derive(Default)]
    struct SyntheticTextTranslatorState {
        requests: StdMutex<Vec<(String, String)>>,
    }

    struct SyntheticTextTranslatorFactory {
        state: std::sync::Arc<SyntheticTextTranslatorState>,
    }

    impl TextTranslatorFactory for SyntheticTextTranslatorFactory {
        fn create(
            &self,
            _api_key: String,
        ) -> Result<Arc<dyn TextTranslator>, OpenAITextTranslationError> {
            Ok(Arc::new(SyntheticTextTranslator {
                state: self.state.clone(),
            }))
        }
    }

    struct SyntheticTextTranslator {
        state: std::sync::Arc<SyntheticTextTranslatorState>,
    }

    #[async_trait::async_trait]
    impl TextTranslator for SyntheticTextTranslator {
        async fn translate_text(
            &self,
            text: &str,
            target_language: &str,
        ) -> Result<String, OpenAITextTranslationError> {
            self.state
                .requests
                .lock()
                .unwrap()
                .push((text.to_string(), target_language.to_string()));
            Ok("привет из zoom".to_string())
        }
    }

    struct FlakyTextTranslator {
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl TextTranslator for FlakyTextTranslator {
        async fn translate_text(
            &self,
            text: &str,
            _target_language: &str,
        ) -> Result<String, OpenAITextTranslationError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                Err(OpenAITextTranslationError::Connection(
                    "temporary network blip".to_string(),
                ))
            } else {
                Ok(format!("{text} translated"))
            }
        }
    }

    struct RateLimitedTextTranslator;

    #[async_trait::async_trait]
    impl TextTranslator for RateLimitedTextTranslator {
        async fn translate_text(
            &self,
            _text: &str,
            _target_language: &str,
        ) -> Result<String, OpenAITextTranslationError> {
            Err(OpenAITextTranslationError::RateLimited(
                "simulated rate limit".to_string(),
            ))
        }
    }

    struct RateLimitedTextTranslatorFactory;

    impl TextTranslatorFactory for RateLimitedTextTranslatorFactory {
        fn create(
            &self,
            _api_key: String,
        ) -> Result<Arc<dyn TextTranslator>, OpenAITextTranslationError> {
            Ok(Arc::new(RateLimitedTextTranslator))
        }
    }

    struct FailingTextTranslatorFactory;

    impl TextTranslatorFactory for FailingTextTranslatorFactory {
        fn create(
            &self,
            _api_key: String,
        ) -> Result<Arc<dyn TextTranslator>, OpenAITextTranslationError> {
            Err(OpenAITextTranslationError::Authentication(
                "simulated translator create failure".to_string(),
            ))
        }
    }

    fn test_callbacks(
        statuses: std::sync::Arc<StdMutex<Vec<RecordingStatus>>>,
    ) -> IncomingTranslationCallbacks {
        IncomingTranslationCallbacks {
            on_source_final: Arc::new(|_| {}),
            on_translation_delta: Arc::new(|_| {}),
            on_error: Arc::new(|_| {}),
            on_status: Arc::new(move |status| {
                statuses.lock().unwrap().push(status);
            }),
        }
    }

    async fn wait_until_runtime_cleanup(
        service: &IncomingTranslationService,
        capture_state: &SyntheticIncomingCaptureState,
        provider_state: &SyntheticIncomingProviderState,
    ) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            if service.active_session_id().await.is_none()
                && capture_state.stopped.load(Ordering::SeqCst)
                && provider_state.stopped.load(Ordering::SeqCst)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        panic!(
            "runtime cleanup did not finish: active_session={:?}, capture_stopped={}, provider_stopped={}",
            service.active_session_id().await,
            capture_state.stopped.load(Ordering::SeqCst),
            provider_state.stopped.load(Ordering::SeqCst)
        );
    }

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
            Some("1.250:0.500:hello".to_string())
        );
    }

    #[test]
    fn finalized_segment_key_skips_segments_without_timing() {
        let transcription = Transcription::final_result("hello".to_string());

        assert_eq!(finalized_segment_key(&transcription, "hello"), None);
    }

    #[test]
    fn finalized_segments_without_timing_can_repeat_same_text() {
        let source_finals = Arc::new(StdMutex::new(Vec::<String>::new()));
        let callbacks = IncomingTranslationCallbacks {
            on_source_final: {
                let source_finals = source_finals.clone();
                Arc::new(move |text| source_finals.lock().unwrap().push(text))
            },
            on_translation_delta: Arc::new(|_| {}),
            on_error: Arc::new(|_| {}),
            on_status: Arc::new(|_| {}),
        };
        let running = Arc::new(AtomicBool::new(true));
        let seen = Arc::new(StdMutex::new(BoundedSegmentDedupe::new(2)));
        let pending_translations = Arc::new(AtomicUsize::new(0));
        let (tx, mut rx) = mpsc::channel::<TranslationJob>(2);
        let transcription = Transcription::final_result("repeat".to_string());

        handle_finalized_transcription(
            transcription.clone(),
            callbacks.clone(),
            running.clone(),
            seen.clone(),
            tx.clone(),
            pending_translations.clone(),
            "final",
        );
        handle_finalized_transcription(
            transcription,
            callbacks,
            running,
            seen.clone(),
            tx,
            pending_translations.clone(),
            "final",
        );

        assert!(seen.lock().unwrap().is_empty());
        assert_eq!(
            source_finals.lock().unwrap().as_slice(),
            &["repeat", "repeat"]
        );
        assert_eq!(rx.try_recv().unwrap().text, "repeat");
        assert_eq!(rx.try_recv().unwrap().text, "repeat");
        assert_eq!(pending_translations.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn try_enqueue_audio_chunk_counts_drops_when_queue_is_full() {
        let (tx, mut rx) = mpsc::channel::<AudioChunk>(1);
        let dropped = AtomicU64::new(0);
        let first = AudioChunk::new(vec![1; 8], 16_000, 1);
        let second = AudioChunk::new(vec![2; 8], 16_000, 1);

        try_enqueue_audio_chunk(&tx, first.clone(), &dropped);
        try_enqueue_audio_chunk(&tx, second, &dropped);

        assert_eq!(dropped.load(Ordering::Relaxed), 1);
        assert_eq!(rx.try_recv().unwrap().data, first.data);
    }

    #[test]
    fn pending_translation_decrement_does_not_underflow() {
        let pending = AtomicUsize::new(0);

        decrement_pending_translations(&pending);
        assert_eq!(pending.load(Ordering::SeqCst), 0);

        pending.store(2, Ordering::SeqCst);
        decrement_pending_translations(&pending);
        decrement_pending_translations(&pending);
        decrement_pending_translations(&pending);

        assert_eq!(pending.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn finalized_segment_dedupe_recovers_after_translation_queue_full() {
        let callbacks = IncomingTranslationCallbacks {
            on_source_final: Arc::new(|_| {}),
            on_translation_delta: Arc::new(|_| {}),
            on_error: Arc::new(|_| {}),
            on_status: Arc::new(|_| {}),
        };
        let running = Arc::new(AtomicBool::new(true));
        let seen = Arc::new(StdMutex::new(BoundedSegmentDedupe::new(2)));
        let pending_translations = Arc::new(AtomicUsize::new(0));
        let (tx, mut rx) = mpsc::channel::<TranslationJob>(1);
        tx.try_send(TranslationJob {
            text: "occupied".to_string(),
            source: "test",
            start: 0.0,
            duration: 0.0,
        })
        .unwrap();

        let transcription =
            Transcription::final_result("hello from call".to_string()).with_timing(2.0, 0.4);
        handle_finalized_transcription(
            transcription.clone(),
            callbacks.clone(),
            running.clone(),
            seen.clone(),
            tx.clone(),
            pending_translations.clone(),
            "final",
        );

        assert!(!seen.lock().unwrap().contains("2.000:0.400:hello from call"));
        assert_eq!(pending_translations.load(Ordering::SeqCst), 0);
        assert_eq!(rx.try_recv().unwrap().text, "occupied");

        handle_finalized_transcription(
            transcription,
            callbacks,
            running,
            seen,
            tx,
            pending_translations.clone(),
            "final",
        );

        let queued = rx.try_recv().unwrap();
        assert_eq!(queued.text, "hello from call");
        assert_eq!(queued.start, 2.0);
        assert_eq!(queued.duration, 0.4);
        assert_eq!(pending_translations.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn bounded_segment_dedupe_evicts_oldest_key() {
        let mut dedupe = BoundedSegmentDedupe::new(2);

        assert!(dedupe.remember("one".to_string()));
        assert!(dedupe.remember("two".to_string()));
        assert!(!dedupe.remember("one".to_string()));
        assert!(dedupe.remember("three".to_string()));

        assert!(!dedupe.contains("one"));
        assert!(dedupe.contains("two"));
        assert!(dedupe.contains("three"));
        assert!(dedupe.remember("one".to_string()));
    }

    #[test]
    fn incoming_start_status_guard_does_not_overwrite_error() {
        assert!(should_mark_incoming_recording_started(
            RecordingStatus::Starting,
            true,
        ));
        assert!(!should_mark_incoming_recording_started(
            RecordingStatus::Error,
            true,
        ));
        assert!(!should_mark_incoming_recording_started(
            RecordingStatus::Starting,
            false,
        ));
    }

    #[test]
    fn incoming_translation_target_language_is_trimmed_and_defaulted() {
        assert_eq!(
            normalize_incoming_translation_target_language("  de\n"),
            "de"
        );
        assert_eq!(normalize_incoming_translation_target_language(""), "ru");
        assert_eq!(normalize_incoming_translation_target_language("auto"), "ru");
        assert_eq!(
            normalize_incoming_translation_target_language("MULTI"),
            "ru"
        );
    }

    #[tokio::test]
    async fn start_cleans_stt_stream_when_loopback_capture_start_fails() {
        let provider_state = std::sync::Arc::new(TrackingProviderState::default());
        let service = IncomingTranslationService::new_with_factories(
            std::sync::Arc::new(TrackingSttFactory {
                state: provider_state.clone(),
                fail_stop: false,
            }),
            std::sync::Arc::new(FailingLoopbackAudioFactory),
        );
        let statuses = std::sync::Arc::new(StdMutex::new(Vec::new()));
        let mut config = IncomingTranslationConfig::new_with_defaults(SttConfig::default(), 77);
        config.openai_api_key = "sk-test".to_string();

        let err = service
            .start(config, test_callbacks(statuses.clone()))
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            IncomingTranslationError::Configuration(msg)
                if msg.contains("simulated loopback start failure")
        ));
        assert!(provider_state.initialized.load(Ordering::SeqCst));
        assert!(provider_state.started.load(Ordering::SeqCst));
        assert!(provider_state.stopped.load(Ordering::SeqCst));
        assert!(!provider_state.aborted.load(Ordering::SeqCst));
        assert_eq!(service.get_status().await, RecordingStatus::Idle);
        assert!(service.inner.lock().await.is_none());
        assert_eq!(
            statuses.lock().unwrap().as_slice(),
            &[RecordingStatus::Starting]
        );
    }

    #[tokio::test]
    async fn start_aborts_stt_stream_if_cleanup_stop_fails() {
        let provider_state = std::sync::Arc::new(TrackingProviderState::default());
        let service = IncomingTranslationService::new_with_factories(
            std::sync::Arc::new(TrackingSttFactory {
                state: provider_state.clone(),
                fail_stop: true,
            }),
            std::sync::Arc::new(FailingLoopbackAudioFactory),
        );
        let statuses = std::sync::Arc::new(StdMutex::new(Vec::new()));
        let mut config = IncomingTranslationConfig::new_with_defaults(SttConfig::default(), 78);
        config.openai_api_key = "sk-test".to_string();

        let err = service
            .start(config, test_callbacks(statuses))
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            IncomingTranslationError::Configuration(msg)
                if msg.contains("simulated loopback start failure")
        ));
        assert!(provider_state.started.load(Ordering::SeqCst));
        assert!(provider_state.stopped.load(Ordering::SeqCst));
        assert!(provider_state.aborted.load(Ordering::SeqCst));
        assert_eq!(service.get_status().await, RecordingStatus::Idle);
        assert!(service.inner.lock().await.is_none());
    }

    #[tokio::test]
    async fn start_aborts_initialized_stt_when_translator_create_fails() {
        let provider_state = std::sync::Arc::new(TrackingProviderState::default());
        let capture_state = std::sync::Arc::new(SyntheticIncomingCaptureState::default());
        let requested_target = std::sync::Arc::new(StdMutex::new(None));
        let service = IncomingTranslationService::new_with_all_factories(
            std::sync::Arc::new(TrackingSttFactory {
                state: provider_state.clone(),
                fail_stop: false,
            }),
            std::sync::Arc::new(SyntheticIncomingAudioFactory {
                capture_state: capture_state.clone(),
                requested_target,
            }),
            std::sync::Arc::new(FailingTextTranslatorFactory),
        );
        let statuses = std::sync::Arc::new(StdMutex::new(Vec::new()));
        let mut config = IncomingTranslationConfig::new_with_defaults(SttConfig::default(), 79);
        config.openai_api_key = "sk-test".to_string();

        let err = service
            .start(config, test_callbacks(statuses.clone()))
            .await
            .unwrap_err();

        assert!(
            matches!(err, IncomingTranslationError::Authentication(msg) if msg.contains("simulated translator create failure"))
        );
        assert!(provider_state.initialized.load(Ordering::SeqCst));
        assert!(!provider_state.started.load(Ordering::SeqCst));
        assert!(provider_state.aborted.load(Ordering::SeqCst));
        assert!(!capture_state.started.load(Ordering::SeqCst));
        assert_eq!(service.get_status().await, RecordingStatus::Idle);
        assert!(service.inner.lock().await.is_none());
        assert_eq!(
            statuses.lock().unwrap().as_slice(),
            &[RecordingStatus::Starting]
        );
    }

    #[tokio::test]
    async fn synthetic_incoming_translation_e2e_captures_transcribes_and_translates() {
        let capture_state = std::sync::Arc::new(SyntheticIncomingCaptureState::default());
        let requested_target = std::sync::Arc::new(StdMutex::new(None));
        let provider_state = std::sync::Arc::new(SyntheticIncomingProviderState::default());
        let translator_state = std::sync::Arc::new(SyntheticTextTranslatorState::default());
        let service = IncomingTranslationService::new_with_all_factories(
            std::sync::Arc::new(SyntheticIncomingSttFactory {
                state: provider_state.clone(),
            }),
            std::sync::Arc::new(SyntheticIncomingAudioFactory {
                capture_state: capture_state.clone(),
                requested_target: requested_target.clone(),
            }),
            std::sync::Arc::new(SyntheticTextTranslatorFactory {
                state: translator_state.clone(),
            }),
        );

        let statuses = std::sync::Arc::new(StdMutex::new(Vec::new()));
        let source_text = std::sync::Arc::new(StdMutex::new(String::new()));
        let translated_text = std::sync::Arc::new(StdMutex::new(String::new()));
        let errors = std::sync::Arc::new(StdMutex::new(Vec::new()));

        let callbacks = IncomingTranslationCallbacks {
            on_source_final: {
                let source_text = source_text.clone();
                Arc::new(move |text| source_text.lock().unwrap().push_str(&text))
            },
            on_translation_delta: {
                let translated_text = translated_text.clone();
                Arc::new(move |text| translated_text.lock().unwrap().push_str(&text))
            },
            on_error: {
                let errors = errors.clone();
                Arc::new(move |err| errors.lock().unwrap().push(err.to_string()))
            },
            on_status: {
                let statuses = statuses.clone();
                Arc::new(move |status| statuses.lock().unwrap().push(status))
            },
        };

        let mut config = IncomingTranslationConfig::new_with_defaults(SttConfig::default(), 102);
        config.openai_api_key = "sk-test".to_string();
        config.target_language = "  ru\n".to_string();

        service.start(config, callbacks).await.unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            if !translated_text.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        assert_eq!(service.get_status().await, RecordingStatus::Recording);
        assert!(capture_state.started.load(Ordering::SeqCst));
        let requested_target = requested_target.lock().unwrap().unwrap();
        assert_eq!(
            requested_target.sample_rate,
            AudioCaptureTarget::incoming_subtitles().sample_rate
        );
        assert_eq!(
            requested_target.channels,
            AudioCaptureTarget::incoming_subtitles().channels
        );
        let capture_config = capture_state.initialized_config.lock().unwrap().unwrap();
        assert_eq!(capture_config.sample_rate, 16_000);
        assert_eq!(capture_config.channels, 1);
        assert!(provider_state.initialized.load(Ordering::SeqCst));
        assert!(provider_state.started.load(Ordering::SeqCst));
        assert_eq!(provider_state.sent_chunks.load(Ordering::SeqCst), 1);
        assert_eq!(source_text.lock().unwrap().as_str(), "hello from zoom");
        assert_eq!(translated_text.lock().unwrap().as_str(), "привет из zoom");
        assert!(errors.lock().unwrap().is_empty());
        assert_eq!(
            translator_state.requests.lock().unwrap().as_slice(),
            &[("hello from zoom".to_string(), "ru".to_string())]
        );
        assert_eq!(
            statuses.lock().unwrap().as_slice(),
            &[RecordingStatus::Starting, RecordingStatus::Recording]
        );

        service.stop().await.unwrap();

        assert_eq!(service.get_status().await, RecordingStatus::Idle);
        assert!(capture_state.stopped.load(Ordering::SeqCst));
        assert!(provider_state.stopped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn terminal_translation_error_cleans_resources_without_manual_stop() {
        let capture_state = std::sync::Arc::new(SyntheticIncomingCaptureState::default());
        let requested_target = std::sync::Arc::new(StdMutex::new(None));
        let provider_state = std::sync::Arc::new(SyntheticIncomingProviderState::default());
        let service = IncomingTranslationService::new_with_all_factories(
            std::sync::Arc::new(SyntheticIncomingSttFactory {
                state: provider_state.clone(),
            }),
            std::sync::Arc::new(SyntheticIncomingAudioFactory {
                capture_state: capture_state.clone(),
                requested_target,
            }),
            std::sync::Arc::new(RateLimitedTextTranslatorFactory),
        );

        let errors = std::sync::Arc::new(StdMutex::new(Vec::new()));
        let statuses = std::sync::Arc::new(StdMutex::new(Vec::new()));
        let callbacks = IncomingTranslationCallbacks {
            on_source_final: Arc::new(|_| {}),
            on_translation_delta: Arc::new(|_| {}),
            on_error: {
                let errors = errors.clone();
                Arc::new(move |err| errors.lock().unwrap().push(err.to_string()))
            },
            on_status: {
                let statuses = statuses.clone();
                Arc::new(move |status| statuses.lock().unwrap().push(status))
            },
        };

        let mut config = IncomingTranslationConfig::new_with_defaults(SttConfig::default(), 104);
        config.openai_api_key = "sk-test".to_string();
        config.target_language = "ru".to_string();

        service.start(config, callbacks).await.unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            if service.get_status().await == RecordingStatus::Error {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        assert_eq!(service.get_status().await, RecordingStatus::Error);
        assert!(
            errors
                .lock()
                .unwrap()
                .iter()
                .any(|err| err.contains("simulated rate limit")),
            "expected rate limit error, got {:?}",
            errors.lock().unwrap()
        );
        assert!(
            statuses.lock().unwrap().contains(&RecordingStatus::Error),
            "expected Error status, got {:?}",
            statuses.lock().unwrap()
        );

        wait_until_runtime_cleanup(&service, &capture_state, &provider_state).await;
        assert_eq!(service.get_status().await, RecordingStatus::Error);

        service.stop().await.unwrap();

        assert_eq!(service.get_status().await, RecordingStatus::Idle);
        assert!(capture_state.stopped.load(Ordering::SeqCst));
        assert!(provider_state.stopped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn stt_send_audio_failure_cleans_resources_without_manual_stop() {
        let capture_state = std::sync::Arc::new(SyntheticIncomingCaptureState::default());
        let requested_target = std::sync::Arc::new(StdMutex::new(None));
        let provider_state = std::sync::Arc::new(SyntheticIncomingProviderState::default());
        provider_state.fail_send.store(true, Ordering::SeqCst);
        let translator_state = std::sync::Arc::new(SyntheticTextTranslatorState::default());
        let service = IncomingTranslationService::new_with_all_factories(
            std::sync::Arc::new(SyntheticIncomingSttFactory {
                state: provider_state.clone(),
            }),
            std::sync::Arc::new(SyntheticIncomingAudioFactory {
                capture_state: capture_state.clone(),
                requested_target,
            }),
            std::sync::Arc::new(SyntheticTextTranslatorFactory {
                state: translator_state,
            }),
        );

        let errors = std::sync::Arc::new(StdMutex::new(Vec::new()));
        let statuses = std::sync::Arc::new(StdMutex::new(Vec::new()));
        let callbacks = IncomingTranslationCallbacks {
            on_source_final: Arc::new(|_| {}),
            on_translation_delta: Arc::new(|_| {}),
            on_error: {
                let errors = errors.clone();
                Arc::new(move |err| errors.lock().unwrap().push(err.to_string()))
            },
            on_status: {
                let statuses = statuses.clone();
                Arc::new(move |status| statuses.lock().unwrap().push(status))
            },
        };

        let mut config = IncomingTranslationConfig::new_with_defaults(SttConfig::default(), 105);
        config.openai_api_key = "sk-test".to_string();
        config.target_language = "ru".to_string();

        service.start(config, callbacks).await.unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            if service.get_status().await == RecordingStatus::Error {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        assert_eq!(service.get_status().await, RecordingStatus::Error);
        assert!(
            errors
                .lock()
                .unwrap()
                .iter()
                .any(|err| err.contains("simulated send_audio failure")),
            "expected send_audio error, got {:?}",
            errors.lock().unwrap()
        );
        assert!(
            statuses.lock().unwrap().contains(&RecordingStatus::Error),
            "expected Error status, got {:?}",
            statuses.lock().unwrap()
        );

        wait_until_runtime_cleanup(&service, &capture_state, &provider_state).await;
        assert_eq!(service.get_status().await, RecordingStatus::Error);

        service.stop().await.unwrap();

        assert_eq!(service.get_status().await, RecordingStatus::Idle);
        assert!(capture_state.stopped.load(Ordering::SeqCst));
        assert!(provider_state.stopped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn stt_error_callback_cleans_resources_without_manual_stop() {
        let capture_state = std::sync::Arc::new(SyntheticIncomingCaptureState::default());
        let requested_target = std::sync::Arc::new(StdMutex::new(None));
        let provider_state = std::sync::Arc::new(SyntheticIncomingProviderState::default());
        let translator_state = std::sync::Arc::new(SyntheticTextTranslatorState::default());
        let service = IncomingTranslationService::new_with_all_factories(
            std::sync::Arc::new(SyntheticIncomingSttFactory {
                state: provider_state.clone(),
            }),
            std::sync::Arc::new(SyntheticIncomingAudioFactory {
                capture_state: capture_state.clone(),
                requested_target,
            }),
            std::sync::Arc::new(SyntheticTextTranslatorFactory {
                state: translator_state,
            }),
        );

        let errors = std::sync::Arc::new(StdMutex::new(Vec::new()));
        let statuses = std::sync::Arc::new(StdMutex::new(Vec::new()));
        let callbacks = IncomingTranslationCallbacks {
            on_source_final: Arc::new(|_| {}),
            on_translation_delta: Arc::new(|_| {}),
            on_error: {
                let errors = errors.clone();
                Arc::new(move |err| errors.lock().unwrap().push(err.to_string()))
            },
            on_status: {
                let statuses = statuses.clone();
                Arc::new(move |status| statuses.lock().unwrap().push(status))
            },
        };

        let mut config = IncomingTranslationConfig::new_with_defaults(SttConfig::default(), 106);
        config.openai_api_key = "sk-test".to_string();
        config.target_language = "ru".to_string();

        service.start(config, callbacks).await.unwrap();

        let callback = provider_state
            .error_callback
            .lock()
            .unwrap()
            .clone()
            .expect("stt error callback");
        callback(SttError::Connection(
            crate::domain::SttConnectionError::simple("simulated async receiver failure"),
        ));

        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            if service.get_status().await == RecordingStatus::Error {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        assert_eq!(service.get_status().await, RecordingStatus::Error);
        assert!(
            errors
                .lock()
                .unwrap()
                .iter()
                .any(|err| err.contains("simulated async receiver failure")),
            "expected async receiver error, got {:?}",
            errors.lock().unwrap()
        );
        assert!(
            statuses.lock().unwrap().contains(&RecordingStatus::Error),
            "expected Error status, got {:?}",
            statuses.lock().unwrap()
        );

        wait_until_runtime_cleanup(&service, &capture_state, &provider_state).await;
        assert_eq!(service.get_status().await, RecordingStatus::Error);

        service.stop().await.unwrap();

        assert_eq!(service.get_status().await, RecordingStatus::Idle);
        assert!(capture_state.stopped.load(Ordering::SeqCst));
        assert!(provider_state.stopped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn stop_drains_final_transcription_emitted_by_stt_stop_stream() {
        let capture_state = std::sync::Arc::new(SyntheticIncomingCaptureState::default());
        let requested_target = std::sync::Arc::new(StdMutex::new(None));
        let provider_state = std::sync::Arc::new(StopFinalProviderState::default());
        let translator_state = std::sync::Arc::new(SyntheticTextTranslatorState::default());
        let service = IncomingTranslationService::new_with_all_factories(
            std::sync::Arc::new(StopFinalSttFactory {
                state: provider_state.clone(),
            }),
            std::sync::Arc::new(SyntheticIncomingAudioFactory {
                capture_state,
                requested_target,
            }),
            std::sync::Arc::new(SyntheticTextTranslatorFactory {
                state: translator_state.clone(),
            }),
        );

        let source_text = std::sync::Arc::new(StdMutex::new(String::new()));
        let translated_text = std::sync::Arc::new(StdMutex::new(String::new()));
        let callbacks = IncomingTranslationCallbacks {
            on_source_final: {
                let source_text = source_text.clone();
                Arc::new(move |text| source_text.lock().unwrap().push_str(&text))
            },
            on_translation_delta: {
                let translated_text = translated_text.clone();
                Arc::new(move |text| translated_text.lock().unwrap().push_str(&text))
            },
            on_error: Arc::new(|err| panic!("unexpected incoming translation error: {err}")),
            on_status: Arc::new(|_| {}),
        };

        let mut config = IncomingTranslationConfig::new_with_defaults(SttConfig::default(), 103);
        config.openai_api_key = "sk-test".to_string();
        config.target_language = "ru".to_string();

        service.start(config, callbacks).await.unwrap();
        service.stop().await.unwrap();

        assert!(provider_state.stopped.load(Ordering::SeqCst));
        assert_eq!(source_text.lock().unwrap().as_str(), "late final from stop");
        assert_eq!(translated_text.lock().unwrap().as_str(), "привет из zoom");
        assert_eq!(
            translator_state.requests.lock().unwrap().as_slice(),
            &[("late final from stop".to_string(), "ru".to_string())]
        );
        assert_eq!(service.get_status().await, RecordingStatus::Idle);
    }

    #[tokio::test]
    async fn translation_worker_recovers_after_transient_connection_error() {
        let (tx, rx) = mpsc::channel::<TranslationJob>(4);
        let running = Arc::new(AtomicBool::new(true));
        let pending = Arc::new(AtomicUsize::new(0));
        let status = Arc::new(RwLock::new(RecordingStatus::Recording));
        let (cleanup_tx, mut cleanup_rx) = mpsc::unbounded_channel();
        let translated = Arc::new(StdMutex::new(Vec::<String>::new()));
        let errors = Arc::new(StdMutex::new(Vec::<String>::new()));
        let callbacks = IncomingTranslationCallbacks {
            on_source_final: Arc::new(|_| {}),
            on_translation_delta: {
                let translated = translated.clone();
                Arc::new(move |text| translated.lock().unwrap().push(text))
            },
            on_error: {
                let errors = errors.clone();
                Arc::new(move |err| errors.lock().unwrap().push(err.to_string()))
            },
            on_status: Arc::new(|_| {}),
        };

        let worker = tokio::spawn(run_translation_worker(
            rx,
            Arc::new(FlakyTextTranslator {
                calls: std::sync::atomic::AtomicUsize::new(0),
            }),
            callbacks,
            "ru".to_string(),
            running,
            pending.clone(),
            status.clone(),
            cleanup_tx,
        ));

        pending.fetch_add(2, Ordering::SeqCst);
        tx.send(TranslationJob {
            text: "first".to_string(),
            source: "test",
            start: 0.0,
            duration: 1.0,
        })
        .await
        .unwrap();
        tx.send(TranslationJob {
            text: "second".to_string(),
            source: "test",
            start: 1.0,
            duration: 1.0,
        })
        .await
        .unwrap();
        drop(tx);
        worker.await.unwrap();

        assert_eq!(
            translated.lock().unwrap().as_slice(),
            &[String::from("second translated")]
        );
        assert!(errors.lock().unwrap().is_empty());
        assert_eq!(pending.load(Ordering::SeqCst), 0);
        assert_eq!(*status.read().await, RecordingStatus::Recording);
        assert!(cleanup_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn translation_worker_surfaces_rate_limit_error() {
        let (tx, rx) = mpsc::channel::<TranslationJob>(2);
        let running = Arc::new(AtomicBool::new(true));
        let pending = Arc::new(AtomicUsize::new(0));
        let status = Arc::new(RwLock::new(RecordingStatus::Recording));
        let (cleanup_tx, mut cleanup_rx) = mpsc::unbounded_channel();
        let errors = Arc::new(StdMutex::new(Vec::<String>::new()));
        let statuses = Arc::new(StdMutex::new(Vec::<RecordingStatus>::new()));
        let callbacks = IncomingTranslationCallbacks {
            on_source_final: Arc::new(|_| {}),
            on_translation_delta: Arc::new(|_| {}),
            on_error: {
                let errors = errors.clone();
                Arc::new(move |err| errors.lock().unwrap().push(err.to_string()))
            },
            on_status: {
                let statuses = statuses.clone();
                Arc::new(move |status| statuses.lock().unwrap().push(status))
            },
        };

        let worker = tokio::spawn(run_translation_worker(
            rx,
            Arc::new(RateLimitedTextTranslator),
            callbacks,
            "ru".to_string(),
            running.clone(),
            pending.clone(),
            status.clone(),
            cleanup_tx,
        ));

        pending.fetch_add(2, Ordering::SeqCst);
        tx.send(TranslationJob {
            text: "hello".to_string(),
            source: "test",
            start: 0.0,
            duration: 1.0,
        })
        .await
        .unwrap();
        tx.send(TranslationJob {
            text: "queued after terminal error".to_string(),
            source: "test",
            start: 1.0,
            duration: 1.0,
        })
        .await
        .unwrap();
        drop(tx);
        worker.await.unwrap();

        assert!(
            errors
                .lock()
                .unwrap()
                .iter()
                .any(|err| err.contains("Rate limited")),
            "expected rate limit error, got {:?}",
            errors.lock().unwrap()
        );
        assert_eq!(pending.load(Ordering::SeqCst), 0);
        assert!(!running.load(Ordering::SeqCst));
        assert_eq!(*status.read().await, RecordingStatus::Error);
        assert_eq!(
            statuses.lock().unwrap().as_slice(),
            &[RecordingStatus::Error]
        );
        assert!(cleanup_rx.try_recv().is_ok());
    }
}
