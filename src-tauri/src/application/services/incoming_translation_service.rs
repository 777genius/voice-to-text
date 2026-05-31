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

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::task::JoinHandle;

use crate::domain::{
    AudioCapture, AudioCaptureTarget, AudioChunk, AudioChunkCallback, ConnectionQualityCallback,
    ErrorCallback, PlatformAudioFactory, RecordingStatus, SttConfig, SttError, SttProvider,
    SttProviderFactory, Transcription, TranscriptionCallback,
};
use crate::infrastructure::audio::DefaultPlatformAudioFactory;
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
    audio_factory: Arc<dyn PlatformAudioFactory>,
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
            inner: Arc::new(Mutex::new(None)),
        }
    }

    #[cfg(test)]
    pub fn new_with_factory(stt_factory: Arc<dyn SttProviderFactory>) -> Self {
        Self {
            status: Arc::new(RwLock::new(RecordingStatus::Idle)),
            stt_factory,
            audio_factory: Arc::new(DefaultPlatformAudioFactory::new()),
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

        let mut capture = match self
            .audio_factory
            .create_system_loopback_capture(AudioCaptureTarget::incoming_subtitles())
        {
            Ok(capture) => capture,
            Err(e) => {
                self.reset_failed_start().await;
                return Err(IncomingTranslationError::Configuration(e.to_string()));
            }
        };
        if let Err(e) = capture
            .initialize(crate::domain::AudioConfig::default())
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

        let translator = match OpenAITextTranslationClient::new(config.openai_api_key.clone()) {
            Ok(translator) => Arc::new(translator),
            Err(e) => {
                self.reset_failed_start().await;
                return Err(e.into());
            }
        };
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

        if let Err(e) = provider
            .start_stream(on_partial, on_final, on_error, on_connection_quality)
            .await
        {
            running.store(false, Ordering::SeqCst);
            translation_task.abort();
            let _ = translation_task.await;
            self.reset_failed_start().await;
            return Err(e.into());
        }

        let provider = Arc::new(Mutex::new(provider));
        let (audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(AUDIO_QUEUE_CAPACITY);
        let callback_running = running.clone();
        let on_chunk: AudioChunkCallback = Arc::new(move |chunk: AudioChunk| {
            if !callback_running.load(Ordering::Relaxed) {
                return;
            }
            let _ = audio_tx.try_send(chunk);
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

    async fn reset_failed_start(&self) {
        *self.status.write().await = RecordingStatus::Idle;
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
}
