use futures_util::FutureExt;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::{Mutex, RwLock};
use tokio::time::{Duration, Instant};

use crate::domain::{
    amplify_i16_samples, limited_microphone_gain, microphone_sensitivity_gain, AudioCapture,
    AudioCaptureIdentity, AudioChunk, AudioConfig, AudioLevelCallback, AudioSpectrumCallback,
    ConnectionQualityCallback, ErrorCallback, RecordingStatus, SttConfig, SttConnectionCategory,
    SttConnectionError, SttError, SttProvider, SttProviderFactory, SttProviderType, SttResult,
    TranscriptionCallback,
};

use super::audio_drain::{AudioAccounting, AudioDrainReason, AudioDrainReport};
use crate::application::AudioSpectrumAnalyzer;

#[derive(Clone, Debug, serde::Serialize)]
pub struct FinalizeReport {
    pub continuation_delivery: Option<bool>,
    pub run_id: u64,
    pub audio: AudioDrainReport,
    pub provider: Option<crate::domain::ProviderFinalizeReport>,
    pub provider_release: crate::domain::ProviderRelease,
    pub error: Option<String>,
    pub shared_failure: bool,
    pub continuation_not_started: Option<crate::domain::ContinuationNotStartedEvidence>,
}

type Result<T> = anyhow::Result<T>;

const AUDIO_PROCESSOR_STOP_DRAIN_TIMEOUT: Duration = Duration::from_millis(2500);
fn audio_processor_stop_drain_timeout(
    config: Option<&SttConfig>,
    audio: &AudioAccounting,
) -> Duration {
    // Legacy EL (and the pacing-only rollback) can still have a realtime-sized
    // prebuffer at Stop. This is a maximum wait, never an added sleep. DG keeps
    // its existing deadline; transport/write deadlines stay independent.
    if config.is_some_and(|config| {
        config.provider == SttProviderType::Backend
            && config.backend_streaming_provider
                == crate::domain::BackendStreamingProvider::ElevenLabs
    }) {
        AUDIO_PROCESSOR_STOP_DRAIN_TIMEOUT.saturating_add(
            audio.remaining_source_duration(Duration::from_secs(PREPARED_AUDIO_MAX_SECONDS as u64)),
        )
    } else {
        AUDIO_PROCESSOR_STOP_DRAIN_TIMEOUT
    }
}

const CAPTURE_METER_INTERVAL: Duration = Duration::from_millis(33);
const CAPTURE_METER_MAX_SAMPLES: usize = 1024;
const PREPARED_AUDIO_QUEUE_CAPACITY: usize = 4_096;
const PREPARED_AUDIO_MAX_SECONDS: usize = 30;
const AUDIO_LOW_SIGNAL_MIN_CHUNKS: usize = 50;
const AUDIO_RAW_NOISE_FLOOR_MAX_AMPLITUDE: i32 = 300;
const AUDIO_SENT_SPEECH_LIKELY_AMPLITUDE: i32 = 1500;
const MAX_AUDIO_STALL_RESTARTS: u32 = 3;
const STT_START_OPERATION_TIMEOUT: Duration = Duration::from_secs(20);
const STT_SEND_OPERATION_TIMEOUT: Duration = Duration::from_secs(6);
// Pause covers pending-audio flush, Finalize write, the provider-bounded 12s
// acknowledgement, and post-ack text grace. A full stop additionally sends
// both the protocol close and WebSocket close frame.
const STT_PAUSE_OPERATION_TIMEOUT: Duration = Duration::from_secs(20);
const STT_STOP_OPERATION_TIMEOUT: Duration = Duration::from_secs(26);
const STT_ABORT_OPERATION_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Default)]
struct SttStartupErrorGate {
    committed: bool,
    error: Option<SttError>,
}

fn lock_stt_startup_error_gate(
    gate: &StdMutex<SttStartupErrorGate>,
) -> std::sync::MutexGuard<'_, SttStartupErrorGate> {
    match gate.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn stt_startup_error_callback(
    on_runtime_error: ErrorCallback,
) -> (Arc<StdMutex<SttStartupErrorGate>>, ErrorCallback) {
    let gate = Arc::new(StdMutex::new(SttStartupErrorGate::default()));
    let callback_gate = gate.clone();
    let callback: ErrorCallback = Arc::new(move |error| {
        let should_report_runtime_error = {
            let mut gate = lock_stt_startup_error_gate(&callback_gate);
            if gate.committed {
                true
            } else {
                if gate.error.is_none() {
                    gate.error = Some(error.clone());
                }
                false
            }
        };

        if should_report_runtime_error {
            on_runtime_error(error);
        }
    });
    (gate, callback)
}

async fn await_stt_operation<F, T>(operation: F, timeout: Duration, label: &str) -> SttResult<T>
where
    F: Future<Output = SttResult<T>>,
{
    match tokio::time::timeout(timeout, operation).await {
        Ok(result) => result,
        Err(_) => Err(SttError::Connection(SttConnectionError::with_category(
            format!("{} timed out after {} ms", label, timeout.as_millis()),
            SttConnectionCategory::Timeout,
        ))),
    }
}

async fn abort_stt_provider(provider: &mut Box<dyn SttProvider>, reason: &str) -> SttResult<()> {
    await_stt_operation(
        provider.abort(),
        STT_ABORT_OPERATION_TIMEOUT,
        &format!("STT abort ({})", reason),
    )
    .await
}

async fn stop_stt_provider(provider: &mut Box<dyn SttProvider>, reason: &str) -> SttResult<()> {
    await_stt_operation(
        provider.stop_stream(),
        STT_STOP_OPERATION_TIMEOUT,
        &format!("STT stop_stream ({})", reason),
    )
    .await
}

fn next_audio_stall_restart_attempt(completed_attempts: &mut u32) -> Option<u32> {
    if *completed_attempts >= MAX_AUDIO_STALL_RESTARTS {
        return None;
    }

    *completed_attempts = completed_attempts.saturating_add(1);
    Some(*completed_attempts)
}

struct PreparedAudioChunk {
    max_amplitude: i32,
    normalized_level: f32,
    requested_gain: f32,
    effective_gain: f32,
    amplified_chunk: AudioChunk,
}

fn keep_alive_enabled_for_config(config: &SttConfig) -> bool {
    config.keep_connection_alive
}

fn config_requires_new_connection(previous: &SttConfig, next: &SttConfig) -> bool {
    previous.provider != next.provider
        || previous.language != next.language
        || previous.auto_detect_language != next.auto_detect_language
        || previous.enable_punctuation != next.enable_punctuation
        || previous.filter_profanity != next.filter_profanity
        || previous.deepgram_api_key != next.deepgram_api_key
        || previous.assemblyai_api_key != next.assemblyai_api_key
        || previous.model != next.model
        || previous.backend_auth_token != next.backend_auth_token
        || previous.backend_url != next.backend_url
        || previous.backend_streaming_provider != next.backend_streaming_provider
        || previous.streaming_keyterms != next.streaming_keyterms
}

/// Applies the product policy for warm backend dictation sessions.
///
/// The service itself still honors an explicit `false`, which is required by other use cases
/// such as incoming translation. Composition roots call this helper only for dictation config.
pub(crate) fn apply_backend_dictation_keep_alive_policy(config: &mut SttConfig) -> bool {
    if config.provider != SttProviderType::Backend {
        return false;
    }

    let keep_connection_alive = config
        .backend_streaming_provider
        .supports_reliable_idle_keep_alive();
    let changed = config.keep_connection_alive != keep_connection_alive
        || config.keep_alive_ttl_secs != crate::domain::BACKEND_KEEPALIVE_TTL_SECS;

    config.keep_connection_alive = keep_connection_alive;
    config.keep_alive_ttl_secs = crate::domain::BACKEND_KEEPALIVE_TTL_SECS;
    changed
}

fn prepare_audio_chunk_for_processing(chunk: &AudioChunk, sensitivity: u8) -> PreparedAudioChunk {
    let max_amplitude: i32 = chunk
        .data
        .iter()
        .map(|&s| (s as i32).abs())
        .max()
        .unwrap_or(0);
    let normalized_level = (max_amplitude as f32 / 32767.0).sqrt().min(1.0);
    let requested_gain = microphone_sensitivity_gain(sensitivity);
    let effective_gain = limited_microphone_gain(sensitivity, max_amplitude);
    let amplified_data = amplify_i16_samples(&chunk.data, effective_gain);

    PreparedAudioChunk {
        max_amplitude,
        normalized_level,
        requested_gain,
        effective_gain,
        amplified_chunk: AudioChunk {
            data: amplified_data,
            sample_rate: chunk.sample_rate,
            channels: chunk.channels,
            timestamp: chunk.timestamp,
        },
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
struct AudioSessionStats {
    chunks: usize,
    samples: usize,
    peak_raw_amplitude: i32,
    peak_sent_amplitude: i32,
    chunks_above_raw_noise_floor: usize,
    chunks_above_sent_speech_floor: usize,
    effective_gain_sum: f64,
}

impl AudioSessionStats {
    fn observe(
        &mut self,
        raw_max_amplitude: i32,
        sent_max_amplitude: i32,
        sent_samples: usize,
        effective_gain: f32,
    ) {
        self.chunks += 1;
        self.samples += sent_samples;
        self.peak_raw_amplitude = self.peak_raw_amplitude.max(raw_max_amplitude);
        self.peak_sent_amplitude = self.peak_sent_amplitude.max(sent_max_amplitude);
        self.effective_gain_sum += effective_gain as f64;

        if raw_max_amplitude > AUDIO_RAW_NOISE_FLOOR_MAX_AMPLITUDE {
            self.chunks_above_raw_noise_floor += 1;
        }
        if sent_max_amplitude >= AUDIO_SENT_SPEECH_LIKELY_AMPLITUDE {
            self.chunks_above_sent_speech_floor += 1;
        }
    }

    fn average_effective_gain(&self) -> f32 {
        if self.chunks == 0 {
            return 0.0;
        }
        (self.effective_gain_sum / self.chunks as f64) as f32
    }

    fn looks_too_quiet_for_stt(&self) -> bool {
        self.chunks >= AUDIO_LOW_SIGNAL_MIN_CHUNKS
            && self.peak_raw_amplitude <= AUDIO_RAW_NOISE_FLOOR_MAX_AMPLITUDE
            && self.chunks_above_sent_speech_floor == 0
    }
}

fn max_abs_amplitude(samples: &[i16]) -> i32 {
    samples.iter().map(|&s| (s as i32).abs()).max().unwrap_or(0)
}

fn log_audio_session_summary(stats: &AudioSessionStats) {
    log::info!(
        "Audio session summary: chunks={}, samples={}, peak_raw={}, peak_sent={}, raw_noise_chunks={}, sent_speech_chunks={}, avg_gain={:.2}x",
        stats.chunks,
        stats.samples,
        stats.peak_raw_amplitude,
        stats.peak_sent_amplitude,
        stats.chunks_above_raw_noise_floor,
        stats.chunks_above_sent_speech_floor,
        stats.average_effective_gain()
    );
}

fn emit_audio_visualization(
    identity: AudioCaptureIdentity,
    prepared: &PreparedAudioChunk,
    spectrum: &mut AudioSpectrumAnalyzer,
    on_audio_level: &AudioLevelCallback,
    on_audio_spectrum: &AudioSpectrumCallback,
    is_current: impl Fn() -> bool,
) {
    if !is_current() {
        return;
    }
    let sample = crate::domain::AudioMeterSample {
        owner: identity,
        captured_at_ms: prepared.amplified_chunk.timestamp,
    };
    on_audio_level(sample, prepared.normalized_level);
    if let Some(bars) = spectrum.push_samples(&prepared.amplified_chunk.data) {
        if is_current() {
            on_audio_spectrum(sample, bars);
        }
    }
}

/// A single bounded snapshot, never a queue of old microphone audio. The audio
/// callback only tries the lock and copies a bounded tail; FFT runs on the task.
#[derive(Default)]
struct CaptureMeterLatest(StdMutex<Option<AudioChunk>>);

impl CaptureMeterLatest {
    fn publish(&self, chunk: &AudioChunk) {
        if let Ok(mut latest) = self.0.try_lock() {
            *latest = Some(AudioChunk {
                data: chunk.data[chunk.data.len().saturating_sub(CAPTURE_METER_MAX_SAMPLES)..]
                    .to_vec(),
                sample_rate: chunk.sample_rate,
                channels: chunk.channels,
                timestamp: chunk.timestamp,
            });
        }
    }

    fn take(&self) -> Option<AudioChunk> {
        self.0.try_lock().ok()?.take()
    }
}

fn abort_prestart_visualizer_task(
    task: &mut Option<tokio::task::JoinHandle<()>>,
    active: &Arc<AtomicBool>,
    reason: &'static str,
) {
    active.store(false, Ordering::Relaxed);
    if let Some(task) = task.take() {
        log::debug!("Aborting prestart audio visualizer task: {}", reason);
        task.abort();
    }
}

/// Main application service that orchestrates transcription workflow
///
/// This service follows the Dependency Inversion Principle by depending on
/// abstractions (traits) rather than concrete implementations
pub struct TranscriptionService {
    audio_capture: Arc<RwLock<Box<dyn AudioCapture>>>,
    capture_recovery_policy: StdMutex<CaptureRecoveryPolicy>,
    stt_factory: Arc<dyn SttProviderFactory>,
    stt_provider: Arc<RwLock<Option<Box<dyn SttProvider>>>>,
    status: Arc<RwLock<RecordingStatus>>,
    config: Arc<RwLock<SttConfig>>,
    config_snapshot: Arc<StdMutex<SttConfig>>,
    microphone_sensitivity: Arc<AtomicU8>, // 0-200, default 100
    invalidate_keep_alive_on_stop: Arc<AtomicBool>,
    connection_lifecycle_guard: Arc<Mutex<()>>,
    inactivity_timer_task: Arc<RwLock<Option<tokio::task::JoinHandle<()>>>>, // таймер для автоочистки соединения
    audio_processor_task: Arc<RwLock<Option<tokio::task::JoinHandle<()>>>>, // обработчик аудио-чанков → STT
    prepared_capture: Arc<Mutex<Option<PreparedCapture>>>,
    route_attach_busy: Arc<AtomicBool>,
    capture_generation: Arc<AtomicUsize>,
    capture_meter_generation: Arc<AtomicUsize>,
    capture_run_id: Arc<AtomicUsize>,
    provider_run_id: Arc<AtomicUsize>,
    last_finalized_run_id: Arc<AtomicUsize>,
    active_audio: Arc<RwLock<Option<(u64, Arc<AudioAccounting>)>>>,
    active_episode: Arc<StdMutex<Option<PreparedCaptureToken>>>,
    active_ack_base: Arc<AtomicU64>,
    continuation_owner: AtomicU64,
    effective_capture_device: Arc<StdMutex<Option<String>>>,
    continuation_compatibility:
        StdMutex<std::collections::BTreeMap<u64, crate::domain::ContinuationCompatibility>>,
    completed_episode_audio: Arc<StdMutex<Option<(u64, AudioDrainReport)>>>,
    continuation_context: StdMutex<Option<Arc<dyn crate::domain::ContinuationContextGuard>>>,
    paused_continuation: StdMutex<Option<PausedContinuation>>,
    processor_callbacks: StdMutex<Option<(ErrorCallback, ConnectionQualityCallback)>>,
    completed_report: Arc<RwLock<Option<FinalizeReport>>>,
    provider_delivery_mode: Arc<RwLock<Option<bool>>>,
    provider_completion: Arc<RwLock<Option<crate::domain::ProviderFinalizeReport>>>,
    provider_delivery: Arc<RwLock<Option<crate::domain::AudioDeliveryProgress>>>,
    continuation_not_started: RwLock<Option<crate::domain::ContinuationNotStartedEvidence>>,
    provider_run_config: Arc<RwLock<Option<SttConfig>>>,
    legacy_run_sequence: AtomicU64,
}

/// Distinguishes intentional attach teardown from a real provider failure
/// which may race the same cancellation flag.
#[derive(Debug)]
pub(crate) struct PreparedCaptureCancelled;

impl std::fmt::Display for PreparedCaptureCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Provider connection cancelled")
    }
}
impl std::error::Error for PreparedCaptureCancelled {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedCaptureToken {
    pub run_id: u64,
    pub generation: u64,
}

/// The Stop instant is carried from intent; delayed PauseAccepted never renews it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PausedContinuation {
    pub logical_run_id: u64,
    pub session: crate::domain::ContinuationSession,
    pub pause_epoch: u64,
    pub pause_request_id: String,
    pub stopped_at: Instant,
    pub continue_window: Duration,
}
impl PausedContinuation {
    pub fn deadline(&self) -> Instant {
        self.stopped_at + self.continue_window
    }
}

const LEGACY_CONTINUE_WINDOW: Duration = Duration::from_secs(2);
const MAX_CONTINUE_WINDOW: Duration = Duration::from_secs(5);

fn negotiated_continue_window(advertised_ms: Option<u64>) -> Duration {
    let Some(window) = advertised_ms.map(Duration::from_millis) else {
        return LEGACY_CONTINUE_WINDOW;
    };
    if window.is_zero() || window > MAX_CONTINUE_WINDOW {
        LEGACY_CONTINUE_WINDOW
    } else {
        window
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContinuationRefusal {
    Unavailable,
    ContextMismatch,
    Expired,
    ConfigChanged,
    Rejected,
    ControlUnknown,
    Empty,
    Overflow,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContinueCaptureOutcome {
    Attached {
        logical_run_id: u64,
        context_revision: u64,
    },
    /// PCM remains in the exact prepared slot. Cold attach requires A's proven release.
    Unsent(ContinuationRefusal),
    Cancelled,
}

struct CaptureAttachLease(Arc<AtomicBool>);
impl CaptureAttachLease {
    fn acquire(busy: &Arc<AtomicBool>) -> Result<Self> {
        busy.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| anyhow::anyhow!("A sealed episode is still attaching"))?;
        Ok(Self(busy.clone()))
    }
}
impl Drop for CaptureAttachLease {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaptureRecoveryPolicy {
    #[default]
    LegacyRestart,
    OwnerManaged,
}

struct PreparedCapture {
    recovery_policy: CaptureRecoveryPolicy,
    device_failure_notified: Arc<AtomicBool>,
    token: PreparedCaptureToken,
    config: SttConfig,
    rx: tokio::sync::mpsc::Receiver<AudioChunk>,
    prefetched: Option<AudioChunk>,
    on_chunk: Arc<dyn Fn(AudioChunk) + Send + Sync>,
    queued_bytes: Arc<AtomicUsize>,
    accounting: Arc<AudioAccounting>,
    captured_chunks: Arc<AtomicUsize>,
    overflowed: Arc<AtomicBool>,
    prestart_visual_active: Arc<AtomicBool>,
    started_at: Instant,
}

#[derive(Clone)]
struct ProcessorOwner {
    token: PreparedCaptureToken,
    logical_run_id: u64,
    capture_run_id: Arc<AtomicUsize>,
    capture_generation: Arc<AtomicUsize>,
    capture_meter_generation: Arc<AtomicUsize>,
    provider_run_id: Arc<AtomicUsize>,
}

impl ProcessorOwner {
    fn owns_capture(&self) -> bool {
        self.capture_run_id.load(Ordering::Acquire) as u64 == self.token.run_id
            && self.capture_generation.load(Ordering::Acquire) as u64 == self.token.generation
    }

    fn owns_provider(&self) -> bool {
        self.provider_run_id.load(Ordering::Acquire) as u64 == self.logical_run_id
    }
}

fn spawn_transcription_runtime_task<F>(
    future: F,
    status: Arc<RwLock<RecordingStatus>>,
    audio_capture: Arc<RwLock<Box<dyn AudioCapture>>>,
    stt_provider: Arc<RwLock<Option<Box<dyn SttProvider>>>>,
    owner: ProcessorOwner,
    on_error: ErrorCallback,
) -> tokio::task::JoinHandle<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        if AssertUnwindSafe(future).catch_unwind().await.is_err() {
            if std::panic::catch_unwind(AssertUnwindSafe(|| {
                on_error(SttError::Internal(
                    "Audio processor task panicked".to_string(),
                ))
            }))
            .is_err()
            {
                log::error!("TranscriptionService: on_error callback panicked");
            }
            TranscriptionService::cleanup_failed_processor_session(
                &status,
                &audio_capture,
                &stt_provider,
                &owner,
                "audio processor panic",
            )
            .await;
        }
    })
}

impl TranscriptionService {
    #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
    pub async fn native_e2e_transport_observation(&self) -> Option<(bool, bool)> {
        let provider = self.stt_provider.read().await;
        match provider.as_ref() {
            Some(provider) => provider.native_e2e_transport_observation(),
            None => Some((false, false)),
        }
    }

    #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
    pub async fn native_e2e_transport_observation_at_boundary(
        &self,
        mut boundary: impl FnMut(),
    ) -> Option<(bool, bool)> {
        let provider = self.stt_provider.read().await;
        match provider.as_ref() {
            Some(provider) => provider.native_e2e_transport_observation_at_boundary(&mut boundary),
            None => {
                boundary();
                Some((false, false))
            }
        }
    }

    /// Upper bound for the service-owned portion of a normal recording stop.
    /// The app shutdown gate adds a small grace period for reducer finalization
    /// and transcript delivery after this work completes.
    pub(crate) fn maximum_stop_cleanup_timeout() -> Duration {
        AUDIO_PROCESSOR_STOP_DRAIN_TIMEOUT
            .saturating_add(Duration::from_secs(PREPARED_AUDIO_MAX_SECONDS as u64))
            .saturating_add(STT_STOP_OPERATION_TIMEOUT)
            .saturating_add(STT_ABORT_OPERATION_TIMEOUT)
    }

    pub fn new(
        audio_capture: Box<dyn AudioCapture>,
        stt_factory: Arc<dyn SttProviderFactory>,
    ) -> Self {
        Self::new_with_microphone_sensitivity(
            audio_capture,
            stt_factory,
            Arc::new(AtomicU8::new(100)),
        )
    }

    pub fn new_with_microphone_sensitivity(
        audio_capture: Box<dyn AudioCapture>,
        stt_factory: Arc<dyn SttProviderFactory>,
        microphone_sensitivity: Arc<AtomicU8>,
    ) -> Self {
        Self::new_with_microphone_sensitivity_and_device(
            audio_capture,
            stt_factory,
            microphone_sensitivity,
            Arc::new(StdMutex::new(None)),
        )
    }

    pub fn new_with_microphone_sensitivity_and_device(
        audio_capture: Box<dyn AudioCapture>,
        stt_factory: Arc<dyn SttProviderFactory>,
        microphone_sensitivity: Arc<AtomicU8>,
        effective_capture_device: Arc<StdMutex<Option<String>>>,
    ) -> Self {
        Self {
            audio_capture: Arc::new(RwLock::new(audio_capture)),
            capture_recovery_policy: StdMutex::new(CaptureRecoveryPolicy::LegacyRestart),
            stt_factory,
            stt_provider: Arc::new(RwLock::new(None)),
            status: Arc::new(RwLock::new(RecordingStatus::Idle)),
            config: Arc::new(RwLock::new(SttConfig::default())),
            config_snapshot: Arc::new(StdMutex::new(SttConfig::default())),
            microphone_sensitivity,
            invalidate_keep_alive_on_stop: Arc::new(AtomicBool::new(false)),
            connection_lifecycle_guard: Arc::new(Mutex::new(())),
            inactivity_timer_task: Arc::new(RwLock::new(None)),
            audio_processor_task: Arc::new(RwLock::new(None)),
            prepared_capture: Arc::new(Mutex::new(None)),
            route_attach_busy: Arc::new(AtomicBool::new(false)),
            capture_generation: Arc::new(AtomicUsize::new(0)),
            capture_meter_generation: Arc::new(AtomicUsize::new(0)),
            capture_run_id: Arc::new(AtomicUsize::new(0)),
            provider_run_id: Arc::new(AtomicUsize::new(0)),
            last_finalized_run_id: Arc::new(AtomicUsize::new(0)),
            active_audio: Arc::new(RwLock::new(None)),
            active_episode: Arc::new(StdMutex::new(None)),
            active_ack_base: Arc::new(AtomicU64::new(0)),
            continuation_owner: AtomicU64::new(0),
            effective_capture_device,
            continuation_compatibility: Default::default(),
            completed_episode_audio: Arc::new(StdMutex::new(None)),
            continuation_context: StdMutex::new(None),
            paused_continuation: StdMutex::new(None),
            processor_callbacks: StdMutex::new(None),
            completed_report: Arc::new(RwLock::new(None)),
            provider_delivery_mode: Arc::new(RwLock::new(None)),
            provider_completion: Arc::new(RwLock::new(None)),
            provider_delivery: Arc::new(RwLock::new(None)),
            continuation_not_started: RwLock::new(None),
            provider_run_config: Arc::new(RwLock::new(None)),
            legacy_run_sequence: AtomicU64::new(1u64 << 63),
        }
    }

    /// Starts the physical microphone for a run without touching the provider.
    /// Audio is retained in a run-scoped, byte-bounded FIFO until connect.
    pub async fn prepare_recording_capture(
        &self,
        run_id: u64,
        config: SttConfig,
        on_audio_level: AudioLevelCallback,
        on_audio_spectrum: AudioSpectrumCallback,
        on_buffer_error: ErrorCallback,
    ) -> Result<PreparedCaptureToken> {
        let prepare_started_at = Instant::now();
        let mut prepared_slot = self.prepared_capture.lock().await;
        if prepared_slot.is_some() || self.route_attach_busy.load(Ordering::Acquire) {
            anyhow::bail!("Audio capture is already owned by another run");
        }
        self.capture_run_id
            .compare_exchange(0, run_id as usize, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|owner| anyhow::anyhow!("Audio capture is already owned by run {owner}"))?;

        let generation =
            self.capture_generation
                .fetch_add(1, Ordering::AcqRel)
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("capture generation exhausted"))? as u64;
        let token = PreparedCaptureToken { run_id, generation };
        self.capture_meter_generation
            .store(generation as usize, Ordering::Release);

        let (tx, rx) = tokio::sync::mpsc::channel(PREPARED_AUDIO_QUEUE_CAPACITY);
        let visual_latest = Arc::new(CaptureMeterLatest::default());
        let queued_bytes = Arc::new(AtomicUsize::new(0));
        let accounting = Arc::new(AudioAccounting::new(queued_bytes.clone()));
        let accounting_cb = accounting.clone();
        let captured_chunks = Arc::new(AtomicUsize::new(0));
        let overflowed = Arc::new(AtomicBool::new(false));
        let prestart_visual_active = Arc::new(AtomicBool::new(true));
        let queued_bytes_cb = queued_bytes.clone();
        let captured_chunks_cb = captured_chunks.clone();
        let overflowed_cb = overflowed.clone();
        let visual_active_cb = prestart_visual_active.clone();
        let visual_latest_cb = visual_latest.clone();
        let capture_generation_cb = self.capture_generation.clone();
        let capture_run_id_cb = self.capture_run_id.clone();
        let device_failure_notified = Arc::new(AtomicBool::new(false));
        let device_failure_notified_cb = device_failure_notified.clone();
        let device_error_run = self.capture_run_id.clone();
        let device_error_generation = self.capture_generation.clone();
        let device_error_active = prestart_visual_active.clone();
        let device_error_accounting = accounting.clone();
        let on_device_error = on_buffer_error.clone();
        let terminal_error: crate::domain::AudioCaptureErrorCallback = Arc::new(move |error| {
            if device_error_run.load(Ordering::Acquire) as u64 != run_id
                || device_error_generation.load(Ordering::Acquire) as u64 != generation
                || device_failure_notified_cb.swap(true, Ordering::AcqRel)
            {
                return;
            }
            device_error_active.store(false, Ordering::Release);
            let error = SttError::Processing(format!("Audio capture failed: {error}"));
            device_error_accounting.fail(error.clone());
            on_device_error(error);
        });
        let on_chunk: Arc<dyn Fn(AudioChunk) + Send + Sync> = Arc::new(move |chunk| {
            if chunk.data.is_empty() {
                return;
            }
            if accounting_cb.sealed.load(Ordering::Acquire) {
                return;
            }
            if capture_generation_cb.load(Ordering::Acquire) as u64 != generation
                || capture_run_id_cb.load(Ordering::Acquire) as u64 != run_id
            {
                return;
            }
            if visual_active_cb.load(Ordering::Acquire) {
                visual_latest_cb.publish(&chunk);
            }
            let chunk_bytes = chunk.data.len().saturating_mul(std::mem::size_of::<i16>());
            let max_bytes = chunk.sample_rate as usize
                * chunk.channels.max(1) as usize
                * std::mem::size_of::<i16>()
                * PREPARED_AUDIO_MAX_SECONDS;
            let previous = queued_bytes_cb.fetch_add(chunk_bytes, Ordering::AcqRel);
            if previous.saturating_add(chunk_bytes) > max_bytes {
                queued_bytes_cb.fetch_sub(chunk_bytes, Ordering::AcqRel);
                if !overflowed_cb.swap(true, Ordering::AcqRel) {
                    on_buffer_error(SttError::Processing(format!(
                        "Prepared audio buffer exceeded {} bytes ({} seconds PCM16)",
                        max_bytes, PREPARED_AUDIO_MAX_SECONDS
                    )));
                }
                return;
            }
            if let Err(error) = tx.try_send(chunk.clone()) {
                queued_bytes_cb.fetch_sub(chunk_bytes, Ordering::AcqRel);
                if matches!(error, tokio::sync::mpsc::error::TrySendError::Full(_))
                    && !overflowed_cb.swap(true, Ordering::AcqRel)
                {
                    on_buffer_error(SttError::Processing(
                        "Prepared audio FIFO capacity exceeded".to_string(),
                    ));
                }
                return;
            }
            accounting_cb.observe_source_format(chunk.sample_rate, chunk.channels);
            accounting_cb.accepted(chunk_bytes);
            captured_chunks_cb.fetch_add(1, Ordering::Relaxed);
        });

        let start_result = {
            let mut capture = self.audio_capture.write().await;
            capture.set_capture_identity(Some(AudioCaptureIdentity { run_id, generation }));
            capture.set_terminal_error_callback(Some(terminal_error));
            let mut result = capture.start_capture(on_chunk.clone()).await;
            if result.is_ok() && device_failure_notified.load(Ordering::Acquire) {
                result = Err(crate::domain::AudioError::Capture(
                    "Native stream failed during capture start".into(),
                ));
            }
            if result.is_ok() {
                if let Some(policy) = self
                    .continuation_compatibility
                    .lock()
                    .unwrap()
                    .get_mut(&run_id)
                {
                    policy.effective_device = self.effective_capture_device.lock().unwrap().clone();
                    let audio = capture.config();
                    policy.sample_rate = audio.sample_rate;
                    policy.channels = audio.channels;
                }
            }
            if result.is_err() {
                let _ = capture.stop_capture().await;
                if !capture.is_capturing() {
                    capture.set_capture_identity(None);
                }
            }
            result
        };
        if let Err(error) = start_result {
            if !self.audio_capture.read().await.is_capturing() {
                self.capture_generation.fetch_add(1, Ordering::AcqRel);
                self.capture_run_id.store(0, Ordering::Release);
            }
            return Err(anyhow::Error::new(error).context("Failed to start audio capture"));
        }

        let visual_active_for_task = prestart_visual_active.clone();
        let sensitivity = self.microphone_sensitivity.clone();
        let capture_generation = self.capture_generation.clone();
        let capture_meter_generation = self.capture_meter_generation.clone();
        let capture_run_id = self.capture_run_id.clone();
        let visual_latest = Arc::downgrade(&visual_latest);
        tokio::spawn(async move {
            let mut spectrum = AudioSpectrumAnalyzer::new();
            let mut interval = tokio::time::interval(CAPTURE_METER_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let is_current = || {
                    visual_active_for_task.load(Ordering::Acquire)
                        && capture_meter_generation.load(Ordering::Acquire) as u64 == generation
                        && capture_generation.load(Ordering::Acquire) as u64 == generation
                        && capture_run_id.load(Ordering::Acquire) as u64 == run_id
                };
                if !is_current() {
                    break;
                }
                let Some(latest) = visual_latest.upgrade() else {
                    break;
                };
                let Some(chunk) = latest.take() else { continue };
                drop(latest);
                let prepared =
                    prepare_audio_chunk_for_processing(&chunk, sensitivity.load(Ordering::Relaxed));
                if !is_current() {
                    break;
                }
                emit_audio_visualization(
                    AudioCaptureIdentity { run_id, generation },
                    &prepared,
                    &mut spectrum,
                    &on_audio_level,
                    &on_audio_spectrum,
                    is_current,
                );
            }
        });

        *prepared_slot = Some(PreparedCapture {
            recovery_policy: *self.capture_recovery_policy.lock().unwrap(),
            device_failure_notified,
            token,
            config,
            rx,
            prefetched: None,
            on_chunk,
            queued_bytes,
            accounting,
            captured_chunks,
            overflowed,
            prestart_visual_active,
            started_at: prepare_started_at,
        });
        log::info!(
            "pending_capture_started_ms run_id={} capture_generation={} elapsed_ms={}",
            run_id,
            generation,
            prepare_started_at.elapsed().as_millis()
        );
        Ok(token)
    }

    pub fn set_continuation_context_guard(
        &self,
        guard: Arc<dyn crate::domain::ContinuationContextGuard>,
    ) {
        *self.continuation_context.lock().unwrap() = Some(guard);
    }

    async fn validate_continuation_context(
        &self,
        logical_run_id: u64,
    ) -> crate::domain::ContextValidation {
        let guard = self.continuation_context.lock().unwrap().clone();
        let Some(guard) = guard else {
            return crate::domain::ContextValidation::Unavailable;
        };
        // No audio, provider, or native executor lock is held across this wait.
        // Native admission can account for an already queued write of this run;
        // other guards retain the ordinary 250 ms bound.
        let request = guard.begin_validation(logical_run_id);
        tokio::time::timeout_at(
            tokio::time::Instant::from_std(request.deadline),
            request.result,
        )
        .await
        .unwrap_or(crate::domain::ContextValidation::Unavailable)
    }

    pub async fn continuation_session_for_run(
        &self,
        logical_run_id: u64,
    ) -> Option<crate::domain::ContinuationSession> {
        if self.provider_run_id.load(Ordering::Acquire) as u64 != logical_run_id {
            return None;
        }
        let provider = self.stt_provider.read().await;
        // Ownership may change while this read waits behind a provider replacement.
        // A stale logical ID must never inherit the replacement's Ready acceptance.
        if self.provider_run_id.load(Ordering::Acquire) as u64 != logical_run_id {
            return None;
        }
        provider
            .as_ref()
            .and_then(|provider| provider.continuation_session())
    }

    /// Read-only lifecycle observation; never takes connection/capture ownership.
    pub async fn continuation_observation(
        &self,
        logical_run_id: u64,
    ) -> Option<(
        crate::domain::ContinuationSession,
        Option<Arc<tokio::sync::Notify>>,
        bool,
    )> {
        let provider = self.stt_provider.read().await;
        if self.provider_run_id.load(Ordering::Acquire) as u64 != logical_run_id {
            return None;
        }
        let provider = provider.as_ref()?;
        Some((
            provider.continuation_lifecycle_session()?,
            provider.continuation_lifecycle_notify(),
            provider.finalize_evidence().is_some()
                // The legacy alive probe only admits paused idle reuse.
                // A negotiated active session is still a live transport.
                || provider.continuation_session().is_none()
                || provider.continuation_not_started().is_some(),
        ))
    }

    pub fn effective_capture_device_source(&self) -> Arc<StdMutex<Option<String>>> {
        self.effective_capture_device.clone()
    }

    /// Set only from the successfully selected native adapter, never user settings.
    pub fn set_effective_capture_device(&self, identity: Option<String>) {
        *self.effective_capture_device.lock().unwrap() = identity.filter(|s| !s.is_empty());
    }

    pub async fn register_continuation_policy(&self, run: u64, config: &crate::domain::AppConfig) {
        let audio = self.audio_capture.read().await.config();
        let policy = crate::domain::ContinuationCompatibility::from_config(
            config,
            &audio,
            self.effective_capture_device.lock().unwrap().clone(),
        );
        let owner = self.provider_run_id.load(Ordering::Acquire) as u64;
        let mut policies = self.continuation_compatibility.lock().unwrap();
        policies.retain(|id, _| *id == owner || *id == run);
        policies.entry(run).or_insert(policy);
    }

    pub fn continuation_policy(
        &self,
        run: u64,
    ) -> Option<crate::domain::ContinuationCompatibility> {
        self.continuation_compatibility
            .lock()
            .unwrap()
            .get(&run)
            .cloned()
    }

    fn continuation_policy_matches(&self, logical: u64, candidate: u64) -> bool {
        let policies = self.continuation_compatibility.lock().unwrap();
        policies.get(&logical).is_some_and(|policy| {
            policy.effective_device.is_some() && policies.get(&candidate) == Some(policy)
        })
    }

    pub fn logical_provider_run_id(&self) -> u64 {
        self.provider_run_id.load(Ordering::Acquire) as u64
    }

    pub async fn paused_continuation_snapshot(&self) -> Option<PausedContinuation> {
        self.paused_continuation.lock().unwrap().clone()
    }

    pub fn active_capture_episode(&self) -> Option<PreparedCaptureToken> {
        *self.active_episode.lock().unwrap()
    }

    /// Drain A and request reversible Pause. Does not freeze a report, tear down the
    /// provider, close delivery, or reset its callback/ACK/delivery sequence ledger.
    pub async fn pause_for_continuation(
        &self,
        logical_run_id: u64,
        stopped_at: Instant,
    ) -> Result<PausedContinuation> {
        use crate::domain::{
            ContextValidation, ContinuationOperation, ContinuationPhase, ControlDecision,
        };
        let _transition = self.connection_lifecycle_guard.lock().await;
        let session = self
            .continuation_session_for_run(logical_run_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("EL continuation was not negotiated"))?;
        self.continuation_owner
            .store(logical_run_id, Ordering::Release);
        if !matches!(
            self.validate_continuation_context(logical_run_id).await,
            ContextValidation::Valid { .. }
        ) {
            anyhow::bail!("Native continuation context unavailable");
        }
        let accounting = self
            .active_audio
            .read()
            .await
            .as_ref()
            .filter(|(owner, _)| *owner == logical_run_id)
            .map(|(_, audio)| audio.clone())
            .ok_or_else(|| anyhow::anyhow!("Logical audio owner changed"))?;
        if !accounting.sealed.load(Ordering::Acquire) {
            anyhow::bail!("Pause requires released, sealed capture");
        }
        let timeout = audio_processor_stop_drain_timeout(
            self.provider_run_config.read().await.as_ref(),
            &accounting,
        );
        let reason = self
            .drain_audio_processor_task("pausing logical recording", timeout)
            .await;
        let report = accounting.report(reason);
        if report.remaining_bytes != 0
            || report.unknown_bytes != 0
            || reason != AudioDrainReason::Drained
            || accounting.failure().is_some()
        {
            anyhow::bail!("Cannot Pause incomplete audio drain");
        }
        if report.accepted_bytes == 0 && report.submitted_bytes == 0 {
            anyhow::bail!("No source audio to retain in a reclaimable Pause");
        }
        if Instant::now() >= stopped_at + Duration::from_millis(2000) {
            anyhow::bail!("Desktop Continue window expired during A drain");
        }
        let mut provider = self.stt_provider.write().await;
        let provider = provider
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Provider disappeared before Pause"))?;
        let result = provider
            .continuation_control(
                &session,
                ContinuationOperation::Pause { logical_run_id },
                stopped_at + Duration::from_secs(2),
            )
            .await?;
        if result.decision != ControlDecision::Accepted
            || !result.eligible_now
            || result.current_phase != ContinuationPhase::PausedReclaimable
            || result.provider_session_id != session.provider_session_id
        {
            anyhow::bail!("Pause was not currently reclaimable");
        }
        let pause_epoch = result
            .pause_epoch
            .filter(|epoch| *epoch != 0)
            .ok_or_else(|| anyhow::anyhow!("Pause epoch unavailable"))?;
        let paused = PausedContinuation {
            logical_run_id,
            session,
            pause_epoch,
            pause_request_id: result.request_id,
            stopped_at,
            continue_window: negotiated_continue_window(result.continue_window_ms),
        };
        *self.paused_continuation.lock().unwrap() = Some(paused.clone());
        Ok(paused)
    }

    async fn restore_unattempted_continue(&self, paused: &PausedContinuation, request_id: String) {
        // Best effort compensation is scoped to the exact Continue. Failure never
        // permits PCM or a second attempt; A still requires terminal release proof.
        if let Some(provider) = self.stt_provider.write().await.as_mut() {
            let _ = provider
                .continuation_control(
                    &paused.session,
                    crate::domain::ContinuationOperation::Restore {
                        pause_epoch: paused.pause_epoch,
                        continue_request_id: request_id,
                    },
                    paused.stopped_at + Duration::from_secs(5),
                )
                .await;
        }
    }

    /// Resolve the same captured B episode onto A. All refusals before the first
    /// write retain its receiver for one subsequent cold route; attempted sends are
    /// never returned as Unsent. The caller must not cancel this operation task.
    pub async fn continue_prepared_capture(
        &self,
        token: PreparedCaptureToken,
        paused: PausedContinuation,
        requested_at: Instant,
        cancelled: Arc<AtomicBool>,
    ) -> Result<ContinueCaptureOutcome> {
        use crate::domain::{
            ContextValidation, ContinuationOperation, ContinuationPhase, ControlDecision,
        };
        use ContinuationRefusal::*;
        use ContinueCaptureOutcome::{Attached, Cancelled, Unsent};
        let _transition = self.connection_lifecycle_guard.lock().await;
        {
            let mut current = self.paused_continuation.lock().unwrap();
            if current.as_ref() != Some(&paused) {
                return Ok(Unsent(Unavailable));
            }
            // Consume the opportunity before any await; one attempt per epoch.
            current.take();
        }
        if requested_at < paused.stopped_at
            || requested_at >= paused.deadline()
            || Instant::now() >= paused.deadline()
        {
            return Ok(Unsent(Expired));
        }
        if cancelled.load(Ordering::Acquire) {
            return Ok(Cancelled);
        }
        let config = self.provider_run_config.read().await.clone();
        let Some(config) = config else {
            return Ok(Unsent(Unavailable));
        };
        {
            let slot = self.prepared_capture.lock().await;
            let Some(prepared) = slot.as_ref().filter(|p| p.token == token) else {
                return Ok(Cancelled);
            };
            // Eligibility belongs to the frozen run, never to persisted settings.
            let mut current_config = self.get_config_snapshot();
            current_config.continuation_target_eligible = config.continuation_target_eligible;
            if !self.continuation_policy_matches(paused.logical_run_id, token.run_id)
                || prepared.config != config
                || current_config != config
                || self.invalidate_keep_alive_on_stop.load(Ordering::Acquire)
            {
                return Ok(Unsent(ConfigChanged));
            }
        }
        let context = self
            .validate_continuation_context(paused.logical_run_id)
            .await;
        // Cancellation wins over an unattempted context refusal, including a
        // failure that settled while physical Stop was already being retried.
        if cancelled.load(Ordering::Acquire) {
            return Ok(Cancelled);
        }
        match context {
            ContextValidation::Valid { .. } => {}
            ContextValidation::Mismatch => return Ok(Unsent(ContextMismatch)),
            ContextValidation::Unavailable => return Ok(Unsent(Unavailable)),
        }
        if Instant::now() >= paused.deadline() {
            return Ok(Unsent(Expired));
        }
        let result = {
            let mut provider = self.stt_provider.write().await;
            // Context validation and provider admission can both suspend. Fence
            // the actual control attempt, preserving settlement once it starts.
            if cancelled.load(Ordering::Acquire) {
                return Ok(Cancelled);
            }
            if Instant::now() >= paused.deadline() {
                return Ok(Unsent(Expired));
            }
            let Some(provider) = provider.as_mut() else {
                return Ok(Unsent(Unavailable));
            };
            provider
                .continuation_control(
                    &paused.session,
                    ContinuationOperation::Continue {
                        pause_epoch: paused.pause_epoch,
                    },
                    paused.deadline(),
                )
                .await
        };
        let result = match result {
            Ok(result) => result,
            Err(_) => {
                let identity = self
                    .stt_provider
                    .read()
                    .await
                    .as_ref()
                    .and_then(|provider| provider.continuation_operation_identity());
                if let Some((request_id, ContinuationOperation::Continue { pause_epoch })) =
                    identity
                {
                    if pause_epoch == paused.pause_epoch {
                        self.restore_unattempted_continue(&paused, request_id).await;
                    }
                }
                return Ok(Unsent(ControlUnknown));
            }
        };
        if result.decision != ControlDecision::Accepted {
            return Ok(Unsent(Rejected));
        }
        if !result.eligible_now
            || result.current_phase != ContinuationPhase::ActiveAwaitingAudio
            || result.pause_epoch != Some(paused.pause_epoch)
            || result.provider_session_id != paused.session.provider_session_id
        {
            self.restore_unattempted_continue(&paused, result.request_id)
                .await;
            return Ok(Unsent(Rejected));
        }
        // Wait upstream-disabled for the first source frame. Stop can seal and
        // repeated pending Toggle can discard the slot throughout this interval.
        loop {
            let state = {
                let slot = self.prepared_capture.lock().await;
                slot.as_ref().filter(|p| p.token == token).map(|p| {
                    (
                        p.rx.is_empty() && p.prefetched.is_none(),
                        p.accounting.sealed.load(Ordering::Acquire),
                        p.overflowed.load(Ordering::Acquire),
                    )
                })
            };
            let refusal = if cancelled.load(Ordering::Acquire) || state.is_none() {
                Some(None)
            } else if state.is_some_and(|(_, _, overflow)| overflow) {
                Some(Some(Overflow))
            } else if state.is_some_and(|(empty, sealed, _)| empty && sealed) {
                Some(Some(Empty))
            } else if Instant::now() >= paused.deadline() {
                Some(Some(Expired))
            } else {
                None
            };
            if let Some(refusal) = refusal {
                self.restore_unattempted_continue(&paused, result.request_id)
                    .await;
                return Ok(refusal.map(Unsent).unwrap_or(Cancelled));
            }
            if state.is_some_and(|(empty, _, _)| !empty) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // Post-Accepted native validation is adjacent to the first actual B write,
        // after source PCM exists; no provider/audio lock is held during AX work.
        let context_revision = match self
            .validate_continuation_context(paused.logical_run_id)
            .await
        {
            ContextValidation::Valid { revision } => revision,
            refused => {
                self.restore_unattempted_continue(&paused, result.request_id)
                    .await;
                return Ok(Unsent(if refused == ContextValidation::Mismatch {
                    ContextMismatch
                } else {
                    Unavailable
                }));
            }
        };
        let mut provider_slot = self.stt_provider.write().await;
        let permitted = provider_slot.as_ref().is_some_and(|provider| {
            provider.continuation_session().as_ref() == Some(&paused.session)
                && provider.finalize_evidence().is_none()
        });
        let mut slot = self.prepared_capture.lock().await;
        let eligible = slot.as_ref().is_some_and(|p| {
            p.token == token
                && p.config == config
                && !p.overflowed.load(Ordering::Acquire)
                && (p.accounting.sealed.load(Ordering::Acquire)
                    || self.capture_token_is_current(token))
        });
        let mut current_config = self.get_config_snapshot();
        current_config.continuation_target_eligible = config.continuation_target_eligible;
        if !permitted
            || !eligible
            || cancelled.load(Ordering::Acquire)
            || Instant::now() >= paused.deadline()
            || current_config != config
            || self.invalidate_keep_alive_on_stop.load(Ordering::Acquire)
            || self.provider_run_id.load(Ordering::Acquire) as u64 != paused.logical_run_id
        {
            drop(slot);
            drop(provider_slot);
            self.restore_unattempted_continue(&paused, result.request_id)
                .await;
            return Ok(if cancelled.load(Ordering::Acquire) {
                Cancelled
            } else {
                Unsent(Unavailable)
            });
        }
        let provider = provider_slot.as_mut().expect("provider gate checked");
        let ack_base = provider
            .audio_delivery_progress()
            .map(|p| p.sent_bytes)
            .unwrap_or(0);
        let (on_error, on_quality) = self
            .processor_callbacks
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Logical processor callbacks unavailable"))?;
        let mut active = self.active_audio.write().await;
        let (_, previous) = active
            .as_ref()
            .filter(|(owner, _)| *owner == paused.logical_run_id)
            .ok_or_else(|| anyhow::anyhow!("Logical audio owner changed"))?;
        let previous_accounting = previous.clone();
        let previous_episode = *self.active_episode.lock().unwrap();
        let previous_ack_base = self.active_ack_base.load(Ordering::Acquire);
        let previous_completed = self.completed_episode_audio.lock().unwrap().clone();
        let mut previous_report = previous.report(AudioDrainReason::Drained);
        if let Some((owner, prior)) = self.completed_episode_audio.lock().unwrap().as_ref() {
            if *owner == paused.logical_run_id {
                previous_report.accumulate(prior);
            }
        }
        let mut status = self.status.write().await;
        // All async lock acquisition is finished before consuming even one frame.
        // The final intent/deadline check therefore still permits unsent Restore.
        if Instant::now() >= paused.deadline()
            || cancelled.load(Ordering::Acquire)
            || !self.continuation_policy_matches(paused.logical_run_id, token.run_id)
        {
            drop(status);
            drop(active);
            drop(slot);
            drop(provider_slot);
            self.restore_unattempted_continue(&paused, result.request_id)
                .await;
            return Ok(if cancelled.load(Ordering::Acquire) {
                Cancelled
            } else {
                Unsent(Expired)
            });
        }
        let _attach_lease = CaptureAttachLease::acquire(&self.route_attach_busy)?;
        let prepared = slot.as_mut().expect("prepared gate checked");
        let raw_first = prepared
            .prefetched
            .take()
            .map(Ok)
            .unwrap_or_else(|| prepared.rx.try_recv())
            .map_err(|_| anyhow::anyhow!("First B frame disappeared"))?;
        let mut prepared = slot.take().expect("prepared gate checked");
        prepared.accounting.track_acknowledgements();
        prepared.accounting.read(raw_first.data.len() * 2);
        *self.completed_episode_audio.lock().unwrap() =
            Some((paused.logical_run_id, previous_report));
        self.active_ack_base.store(ack_base, Ordering::Release);
        *active = Some((paused.logical_run_id, prepared.accounting.clone()));
        *self.active_episode.lock().unwrap() = Some(token);
        *status = if prepared.accounting.sealed.load(Ordering::Acquire) {
            RecordingStatus::Processing
        } else {
            RecordingStatus::Recording
        };
        drop(status);
        drop(active);
        drop(slot);
        let first = prepare_audio_chunk_for_processing(
            &raw_first,
            self.microphone_sensitivity.load(Ordering::Acquire),
        )
        .amplified_chunk;
        // The accounting reservation is reversible until the transport polls its
        // sink. Its shared attempt evidence survives an outer operation timeout.
        let fence =
            crate::domain::ContinuationWriteFence::new(cancelled.clone(), paused.deadline());
        let send_lease = prepared.accounting.sending(first.data.len() * 2);
        let write = await_stt_operation(
            provider.send_first_continuation_audio(
                &paused.session,
                paused.pause_epoch,
                &first,
                &fence,
            ),
            // Admission checked the absolute deadline above. Once polled, the
            // single in-flight write owns its bounded I/O budget across expiry.
            STT_SEND_OPERATION_TIMEOUT,
            "first continued audio write",
        )
        .await;
        let write = if !fence.attempted.load(Ordering::Acquire) {
            Ok(crate::domain::ContinuationFirstWrite::NotStarted)
        } else {
            write
        };
        match write {
            Ok(crate::domain::ContinuationFirstWrite::Written) => {
                #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
                crate::presentation::native_e2e::record_live_provider_pcm(
                    paused.logical_run_id,
                    token.run_id,
                    token.generation,
                    &first,
                );
                send_lease.submitted();
            }
            Ok(crate::domain::ContinuationFirstWrite::NotStarted) => {
                send_lease.not_started();
                prepared.accounting.restore_read(raw_first.data.len() * 2);
                prepared.prefetched = Some(raw_first);
                drop(provider_slot);
                *self.active_audio.write().await =
                    Some((paused.logical_run_id, previous_accounting));
                *self.active_episode.lock().unwrap() = previous_episode;
                self.active_ack_base
                    .store(previous_ack_base, Ordering::Release);
                *self.completed_episode_audio.lock().unwrap() = previous_completed;
                *self.status.write().await = RecordingStatus::Processing;
                let mut slot = self.prepared_capture.lock().await;
                // Attach reservation prevents another capture replacing this buffer.
                debug_assert!(slot.is_none());
                *slot = Some(prepared);
                drop(slot);
                self.restore_unattempted_continue(&paused, result.request_id)
                    .await;
                if cancelled.load(Ordering::Acquire) {
                    // NotStarted restored the receiver: cleanup failure cannot
                    // turn these unsent bytes into an attempted-write outcome.
                    // Cancelled does not prove microphone release. The existing
                    // Seal/retry owner must still dispose of the restored buffer.
                    if let Err(error) = self.cancel_prepared_capture(token).await {
                        log::warn!(
                            "Unattempted Continue cancellation awaits capture release: {}",
                            error
                        );
                    }
                    return Ok(Cancelled);
                }
                return Ok(Unsent(Unavailable));
            }
            Err(error) => {
                drop(send_lease);
                prepared.accounting.fail(error.clone());
                self.retain_provider_completion(provider.as_ref()).await;
                drop(provider_slot);
                let _ = self.stop_capture_for_run(token.run_id).await;
                let owner = ProcessorOwner {
                    token,
                    logical_run_id: paused.logical_run_id,
                    capture_run_id: self.capture_run_id.clone(),
                    capture_generation: self.capture_generation.clone(),
                    capture_meter_generation: self.capture_meter_generation.clone(),
                    provider_run_id: self.provider_run_id.clone(),
                };
                Self::cleanup_failed_processor_session(
                    &self.status,
                    &self.audio_capture,
                    &self.stt_provider,
                    &owner,
                    "unknown first continued write",
                )
                .await;
                return Err(anyhow::Error::new(error));
            }
        }
        if let Some(progress) = provider.audio_delivery_progress() {
            prepared
                .accounting
                .acknowledge(progress.acked_bytes.saturating_sub(ack_base));
        }
        drop(provider_slot);
        self.attach_audio_processor(
            prepared,
            paused.logical_run_id,
            ack_base,
            on_error,
            on_quality,
        )
        .await;
        Ok(Attached {
            logical_run_id: paused.logical_run_id,
            context_revision,
        })
    }

    /// Stop physical capture while retaining the exact unsent episode. Closing the
    /// receiver rejects late ingress but leaves every accepted PCM chunk readable.
    /// A sealed slot remains busy until its route consumes it or explicit Cancel.
    pub async fn seal_prepared_capture(&self, token: PreparedCaptureToken) -> Result<()> {
        let mut slot = self.prepared_capture.lock().await;
        let prepared = slot
            .as_mut()
            .filter(|p| p.token == token)
            .ok_or_else(|| anyhow::anyhow!("Prepared capture token is stale"))?;
        if prepared.accounting.sealed.load(Ordering::Acquire) {
            return Ok(());
        }
        if !self.capture_token_is_current(token) {
            anyhow::bail!("Prepared capture no longer owns the microphone");
        }
        self.capture_meter_generation.store(0, Ordering::Release);
        let (result, active) = {
            let mut capture = self.audio_capture.write().await;
            let result = capture.stop_capture().await;
            (result, capture.is_capturing())
        };
        if active {
            anyhow::bail!(
                "Prepared capture stop did not release the microphone: {:?}",
                result.err()
            );
        }
        prepared
            .prestart_visual_active
            .store(false, Ordering::Release);
        prepared.accounting.seal();
        prepared.rx.close();
        self.capture_generation.fetch_add(1, Ordering::AcqRel);
        self.capture_run_id.store(0, Ordering::Release);
        result.map_err(anyhow::Error::new)
    }

    /// Share exact prepared-stop semantics between first Seal and its bounded
    /// retry. A write that already consumed this slot must settle as ordinary Stop.
    pub async fn retains_prepared_capture(&self, token: PreparedCaptureToken) -> bool {
        self.prepared_capture
            .lock()
            .await
            .as_ref()
            .is_some_and(|prepared| prepared.token == token)
    }

    pub async fn stop_pending_capture(
        &self,
        token: PreparedCaptureToken,
        cancel: bool,
    ) -> Result<()> {
        let result = if cancel {
            self.cancel_prepared_capture(token).await
        } else {
            self.seal_prepared_capture(token).await
        };
        if result.is_err() && self.active_capture_episode() == Some(token) {
            self.stop_capture_for_run(token.run_id).await
        } else {
            result
        }
    }

    /// Only an upstream-disabled prepared episode may be discarded. Once the
    /// processor takes it, cancellation must use ordinary Stop/incomplete cleanup.
    pub async fn cancel_prepared_capture(&self, token: PreparedCaptureToken) -> Result<()> {
        let seal_result = self.seal_prepared_capture(token).await;
        let mut slot = self.prepared_capture.lock().await;
        if slot
            .as_ref()
            .is_some_and(|p| p.token == token && p.accounting.sealed.load(Ordering::Acquire))
        {
            slot.take();
        }
        seal_result
    }

    pub fn capture_token_is_current(&self, token: PreparedCaptureToken) -> bool {
        self.capture_run_id.load(Ordering::Acquire) == token.run_id as usize
            && self.capture_generation.load(Ordering::Acquire) as u64 == token.generation
    }

    pub async fn connect_prepared_recording(
        &self,
        token: PreparedCaptureToken,
        on_partial: TranscriptionCallback,
        on_final: TranscriptionCallback,
        on_audio_level: AudioLevelCallback,
        on_audio_spectrum: AudioSpectrumCallback,
        on_error: ErrorCallback,
        on_connection_quality: ConnectionQualityCallback,
        cancelled: Arc<AtomicBool>,
    ) -> Result<()> {
        let attachable = self
            .prepared_capture
            .lock()
            .await
            .as_ref()
            .is_some_and(|prepared| {
                prepared.token == token
                    && (prepared.accounting.sealed.load(Ordering::Acquire)
                        || self.capture_token_is_current(token))
            });
        if !attachable {
            anyhow::bail!("Prepared capture token is stale");
        }
        if cancelled.load(Ordering::Acquire) {
            self.cancel_prepared_capture(token).await?;
            return Err(anyhow::Error::new(PreparedCaptureCancelled));
        }
        let completed = {
            let start = self.start_recording_inner(
                on_partial,
                on_final,
                on_audio_level,
                on_audio_spectrum,
                on_error,
                on_connection_quality,
                Some(token),
                Some(cancelled.as_ref()),
            );
            tokio::pin!(start);
            loop {
                tokio::select! {
                    result = &mut start => break Some(result),
                    _ = tokio::time::sleep(Duration::from_millis(25)) => {
                        if cancelled.load(Ordering::Acquire) {
                            break None;
                        }
                    }
                }
            }
        };
        if let Some(result) = completed {
            if result
                .as_ref()
                .err()
                .and_then(|error| error.downcast_ref::<PreparedCaptureCancelled>())
                .is_none()
            {
                return result;
            }
        }
        // A cancellation observed by the start future must use the same owner-scoped
        // teardown as the timer branch; the inner lifecycle guard has been dropped.
        {
            let _ = self.capture_meter_generation.compare_exchange(
                token.generation as usize,
                0,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            let _connection_guard = self.connection_lifecycle_guard.lock().await;
            let provider_owner = self.provider_run_id.load(Ordering::Acquire) as u64;
            let owns_cancelled_provider = provider_owner == 0 || provider_owner == token.run_id;
            if owns_cancelled_provider {
                if let Some(mut provider) = self.stt_provider.write().await.take() {
                    let _ =
                        abort_stt_provider(&mut provider, "cancelled provider connection").await;
                }
                self.provider_run_id.store(0, Ordering::Release);
                *self.provider_run_config.write().await = None;
            }
            self.abort_audio_processor_task("cancelled provider connection")
                .await;
            let stop_result = self.stop_capture_for_run(token.run_id).await;
            if owns_cancelled_provider
                && self.provider_run_id.load(Ordering::Acquire) == 0
                && !self.capture_is_active_for_run(token.run_id).await
            {
                // This run never committed a provider and therefore has no
                // FinalizeRecording effect that could leave Processing. Old
                // provider finalization is serialized before connect begins.
                *self.status.write().await = RecordingStatus::Idle;
            }
            drop(_connection_guard);
            stop_result?;
            Err(anyhow::Error::new(PreparedCaptureCancelled))
        }
    }

    /// Update microphone sensitivity (0-200)
    pub async fn set_microphone_sensitivity(&self, sensitivity: u8) {
        self.microphone_sensitivity
            .store(sensitivity.min(200), Ordering::Relaxed);
    }

    pub fn microphone_sensitivity_source(&self) -> Arc<AtomicU8> {
        self.microphone_sensitivity.clone()
    }

    async fn abort_audio_processor_task(&self, reason: &str) {
        if let Some(task) = self.audio_processor_task.write().await.take() {
            log::debug!("Aborting audio processor task: {}", reason);
            task.abort();
            let _ = task.await;
        }
    }

    async fn stop_capture_after_failed_start(&self) {
        let inactive = {
            let mut capture = self.audio_capture.write().await;
            let _ = capture.stop_capture().await;
            !capture.is_capturing()
        };
        if inactive {
            self.capture_generation.fetch_add(1, Ordering::AcqRel);
            self.capture_run_id.store(0, Ordering::Release);
        }
    }

    async fn drain_audio_processor_task(
        &self,
        reason: &str,
        timeout: Duration,
    ) -> AudioDrainReason {
        let Some(mut task) = self.audio_processor_task.write().await.take() else {
            return AudioDrainReason::Drained;
        };

        tokio::select! {
            result = &mut task => {
                if let Err(e) = result {
                    if e.is_cancelled() {
                        log::debug!("Audio processor task cancelled while draining: {}", reason);
                    } else {
                        log::warn!("Audio processor task failed while draining ({}): {}", reason, e);
                    }
                    return AudioDrainReason::ProcessorError;
                }
                AudioDrainReason::Drained
            }
            _ = tokio::time::sleep(timeout) => {
                log::warn!(
                    "Audio processor did not drain within {:?} while {}; aborting",
                    timeout,
                    reason
                );
                task.abort();
                let _ = task.await;
                AudioDrainReason::Deadline
            }
        }
    }

    async fn cleanup_failed_processor_session(
        status: &Arc<RwLock<RecordingStatus>>,
        audio_capture: &Arc<RwLock<Box<dyn AudioCapture>>>,
        stt_provider: &Arc<RwLock<Option<Box<dyn SttProvider>>>>,
        owner: &ProcessorOwner,
        reason: &str,
    ) {
        {
            let mut capture = audio_capture.write().await;
            // Identity is checked under the device lock, after any wait. Global
            // Recording status can already describe a different capture lease.
            if owner.owns_capture() {
                let _ = owner.capture_meter_generation.compare_exchange(
                    owner.token.generation as usize,
                    0,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
                if let Err(e) = capture.stop_capture().await {
                    log::warn!("Failed to stop audio capture after {}: {}", reason, e);
                }
                if !capture.is_capturing() && owner.owns_capture() {
                    let mut status = status.write().await;
                    if (owner.owns_provider() || owner.provider_run_id.load(Ordering::Acquire) == 0)
                        && matches!(
                            *status,
                            RecordingStatus::Starting | RecordingStatus::Recording
                        )
                    {
                        *status = RecordingStatus::Idle;
                    }
                }
            } else {
                log::debug!(
                    "Skipping audio stop after {}: capture lease changed",
                    reason
                );
            }
        }
        let mut provider_slot = stt_provider.write().await;
        if owner.owns_provider() {
            if provider_slot
                .as_ref()
                .is_some_and(|provider| provider.continuation_lifecycle_session().is_some())
            {
                // Negotiated errors are finalized by the logical owner. Keep the
                // receiver/evidence available to the callback and terminal observer.
                return;
            }
            if let Some(mut provider) = provider_slot.take() {
                if let Err(e) = abort_stt_provider(&mut provider, reason).await {
                    log::warn!("Failed to abort STT provider after {}: {}", reason, e);
                }
            }
        }
        // Provider ownership survives teardown until the final report is frozen.
    }

    pub async fn cleanup_runtime_failure(&self, reason: &str) -> bool {
        let provider_owner = self.provider_run_id.load(Ordering::Acquire) as u64;
        let run_id = if provider_owner != 0 {
            provider_owner
        } else {
            self.capture_run_id.load(Ordering::Acquire) as u64
        };
        self.cleanup_runtime_failure_for_run(run_id, reason).await
    }

    /// A callback keeps its original run identity while waiting for finalization.
    /// Recheck ownership after acquiring the connection guard: A may have been
    /// released, and B may already own either resource by then.
    pub async fn cleanup_runtime_failure_for_run(&self, run_id: u64, reason: &str) -> bool {
        self.cleanup_runtime_failure_owner(run_id, false, reason)
            .await
            .is_some()
    }

    /// Native callbacks carry a physical run. Resolve it under the lifecycle lock,
    /// before cleanup or UI/reducer dispatch; a detached/stale episode owns nothing.
    pub async fn cleanup_capture_runtime_failure(
        &self,
        physical_run: u64,
        reason: &str,
    ) -> Option<u64> {
        self.cleanup_runtime_failure_owner(physical_run, true, reason)
            .await
    }

    async fn cleanup_runtime_failure_owner(
        &self,
        run_id: u64,
        physical: bool,
        reason: &str,
    ) -> Option<u64> {
        if run_id == 0 {
            return None;
        }
        let _connection_guard = self.connection_lifecycle_guard.lock().await;
        let run_id = if physical {
            if self.capture_run_id.load(Ordering::Acquire) as u64 != run_id {
                return None;
            }
            let attached = self.active_episode.lock().unwrap().is_some_and(|token| {
                token.run_id == run_id && self.capture_token_is_current(token)
            });
            let logical = self.provider_run_id.load(Ordering::Acquire) as u64;
            if attached && logical != 0 {
                logical
            } else {
                run_id
            }
        } else {
            run_id
        };
        let owns_provider = self.provider_run_id.load(Ordering::Acquire) as u64 == run_id;
        // Ready's retained lifecycle survives transport failure, including before
        // the first Pause has installed continuation_owner.
        let negotiated = owns_provider
            && (self.continuation_owner.load(Ordering::Acquire) == run_id
                || self
                    .stt_provider
                    .read()
                    .await
                    .as_ref()
                    .is_some_and(|provider| provider.continuation_lifecycle_session().is_some()));
        // Prepare publishes capture ownership under this lock independently of
        // provider finalization. Keep it until the matching device stop completes.
        let mut prepared_slot = self.prepared_capture.lock().await;
        let capture_owner = self.capture_run_id.load(Ordering::Acquire) as u64;
        let owns_capture = capture_owner == run_id
            || (owns_provider
                && self.active_episode.lock().unwrap().is_some_and(|token| {
                    token.run_id == capture_owner && self.capture_token_is_current(token)
                }));
        if !owns_provider && !owns_capture {
            return None;
        }
        if owns_provider
            && !owns_capture
            && !negotiated
            && *self.status.read().await == RecordingStatus::Processing
        {
            // Finalization owns the drain, teardown and terminal report. A late
            // callback must not steal its provider before that effect starts.
            return None;
        }
        let mut still_active = false;
        if owns_capture {
            self.capture_meter_generation.store(0, Ordering::Release);
            let mut capture = self.audio_capture.write().await;
            if self.capture_run_id.load(Ordering::Acquire) as u64 == capture_owner {
                if let Err(error) = capture.stop_capture().await {
                    log::warn!(
                        "Failed to stop audio capture after runtime failure ({}): {}",
                        reason,
                        error
                    );
                }
                still_active = capture.is_capturing();
                if !still_active {
                    if prepared_slot
                        .as_ref()
                        .is_some_and(|prepared| prepared.token.run_id == capture_owner)
                    {
                        if let Some(prepared) = prepared_slot.take() {
                            prepared
                                .prestart_visual_active
                                .store(false, Ordering::Release);
                        }
                    }
                    self.capture_generation.fetch_add(1, Ordering::AcqRel);
                    let _ = self.capture_run_id.compare_exchange(
                        capture_owner as usize,
                        0,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    );
                }
            }
        }
        drop(prepared_slot);
        if negotiated {
            // Keep the logical owner/config and provider evidence until the one
            // terminal report freezes. The exact physical episode was stopped
            // above; it can differ from the error callback's logical run.
            if let Some((owner, accounting)) = self.active_audio.read().await.as_ref() {
                if *owner == run_id {
                    accounting.fail(SttError::Processing(reason.to_owned()));
                }
            }
            self.abort_audio_processor_task(reason).await;
            drop(_connection_guard);
            let _ = self.finalize_provider_for_run(run_id).await;
            return Some(run_id);
        }
        if owns_provider {
            if let Some(timer) = self.inactivity_timer_task.write().await.take() {
                timer.abort();
                let _ = timer.await;
            }
            self.abort_audio_processor_task(reason).await;
            if let Some(mut provider) = self.stt_provider.write().await.take() {
                if let Err(error) = abort_stt_provider(&mut provider, reason).await {
                    log::warn!(
                        "Failed to abort STT provider after runtime failure ({}): {}",
                        reason,
                        error
                    );
                }
            }
            self.provider_run_id.store(0, Ordering::Release);
            *self.provider_run_config.write().await = None;
        }
        // Pending B capture may exist, but provider status remains independent.
        // A capture-only failure must not change an unrelated active provider.
        if owns_provider || self.provider_run_id.load(Ordering::Acquire) == 0 {
            *self.status.write().await = if still_active {
                RecordingStatus::Recording
            } else {
                RecordingStatus::Idle
            };
        }
        Some(run_id)
    }

    /// Start recording and transcription
    pub async fn start_recording(
        &self,
        on_partial: TranscriptionCallback,
        on_final: TranscriptionCallback,
        on_audio_level: AudioLevelCallback,
        on_audio_spectrum: AudioSpectrumCallback,
        on_error: ErrorCallback,
        on_connection_quality: ConnectionQualityCallback,
    ) -> Result<()> {
        if self.prepared_capture.lock().await.is_some() {
            anyhow::bail!("A run-scoped prepared capture is awaiting its exact connect token");
        }
        let run_id = self.legacy_run_sequence.fetch_add(1, Ordering::AcqRel);
        let config = self.config.read().await.clone();
        let token = self
            .prepare_recording_capture(
                run_id,
                config,
                on_audio_level.clone(),
                on_audio_spectrum.clone(),
                on_error.clone(),
            )
            .await?;
        self.start_recording_inner(
            on_partial,
            on_final,
            on_audio_level,
            on_audio_spectrum,
            on_error,
            on_connection_quality,
            Some(token),
            None,
        )
        .await
    }

    async fn start_recording_inner(
        &self,
        on_partial: TranscriptionCallback,
        on_final: TranscriptionCallback,
        _on_audio_level: AudioLevelCallback,
        _on_audio_spectrum: AudioSpectrumCallback,
        on_error: ErrorCallback,
        on_connection_quality: ConnectionQualityCallback,
        expected_token: Option<PreparedCaptureToken>,
        cancellation: Option<&AtomicBool>,
    ) -> Result<()> {
        let expected_token = expected_token
            .ok_or_else(|| anyhow::anyhow!("Exact prepared capture token is required"))?;

        let connection_transition_guard = self.connection_lifecycle_guard.lock().await;
        let mut status = self.status.write().await;

        if *status != RecordingStatus::Idle {
            anyhow::bail!("Already recording or starting");
        }
        let provider_owner = self.provider_run_id.load(Ordering::Acquire) as u64;
        if provider_owner != 0 {
            anyhow::bail!("Previous logical provider has not released ownership");
        }
        let continued_owner = self.continuation_owner.load(Ordering::Acquire);
        if continued_owner != 0 {
            let released = self
                .completed_report_for_run(continued_owner)
                .await
                .is_some_and(|report| {
                    report.provider_release == crate::domain::ProviderRelease::Released
                        && report.provider.is_some_and(|provider| {
                            provider.provider_release == crate::domain::ProviderRelease::Released
                        })
                });
            if !released {
                anyhow::bail!("Previous continuation admission release is unconfirmed; retained PCM cannot be replayed");
            }
        }
        self.continuation_owner.store(0, Ordering::Release);
        *self.continuation_not_started.write().await = None;

        // Устанавливаем статус Starting чтобы заблокировать повторные вызовы
        *status = RecordingStatus::Starting;
        drop(status);

        if self.stt_provider.read().await.is_none() {
            self.invalidate_keep_alive_on_stop
                .store(false, Ordering::SeqCst);
        }

        let _attach_lease = CaptureAttachLease::acquire(&self.route_attach_busy)?;
        let prepared = {
            let mut slot = self.prepared_capture.lock().await;
            let prepared = slot.as_ref().ok_or_else(|| {
                anyhow::anyhow!("Prepared audio capture disappeared before connect")
            })?;
            if expected_token != prepared.token {
                anyhow::bail!("Prepared capture token changed before provider connection");
            }
            // Publish ownership before releasing the slot lock. Stop observes
            // either the retained FIFO or this exact active episode, never neither.
            *self.active_audio.write().await =
                Some((prepared.token.run_id, prepared.accounting.clone()));
            *self.active_episode.lock().unwrap() = Some(prepared.token);
            slot.take().expect("prepared capture checked above")
        };
        let PreparedCapture {
            recovery_policy,
            device_failure_notified,
            token,
            config,
            rx,
            prefetched,
            on_chunk,
            queued_bytes,
            accounting,
            captured_chunks,
            overflowed,
            prestart_visual_active,
            started_at: startup_started_at,
            ..
        } = prepared;
        drop(connection_transition_guard);

        if overflowed.load(Ordering::Acquire) {
            self.stop_capture_after_failed_start().await;
            *self.status.write().await =
                if self.capture_is_active_for_run(expected_token.run_id).await {
                    RecordingStatus::Recording
                } else {
                    RecordingStatus::Idle
                };
            anyhow::bail!("Prepared audio buffer overflowed before provider connection");
        }

        // Отменяем таймер неактивности если он запущен
        if let Some(timer) = self.inactivity_timer_task.write().await.take() {
            log::info!("Cancelling inactivity timer (user started recording before timeout)");
            timer.abort();
            let _ = timer.await;
        }

        // На всякий случай прибиваем старый audio processor, если он почему-то остался висеть
        // (например, если предыдущая запись завершилась через ошибку/гонку).
        self.abort_audio_processor_task("starting a new recording")
            .await;

        self.active_ack_base.store(0, Ordering::Release);
        *self.completed_episode_audio.lock().unwrap() = None;
        *self.paused_continuation.lock().unwrap() = None;
        *self.processor_callbacks.lock().unwrap() =
            Some((on_error.clone(), on_connection_quality.clone()));
        *self.provider_completion.write().await = None;
        *self.provider_delivery_mode.write().await = None;
        *self.provider_delivery.write().await = None;

        let dropped_chunks = Arc::new(AtomicUsize::new(0));
        let audio_capture_started_after = startup_started_at.elapsed();
        let prebuffer_bytes_at_connect = queued_bytes.load(Ordering::Acquire);
        let prebuffer_chunks_at_connect = captured_chunks.load(Ordering::Acquire);
        log::info!(
            "[StartLatencyDiag] audio capture started before STT setup (after_ms={})",
            audio_capture_started_after.as_millis()
        );

        let mut prestart_visual_task = None;

        let (mut startup_error_gate, mut provider_on_error) =
            stt_startup_error_callback(on_error.clone());

        let _connection_guard = self.connection_lifecycle_guard.lock().await;

        // Проверяем можно ли переиспользовать существующее соединение
        let (mut can_reuse_connection, mut reuse_decision_reason) = {
            let keep_alive_invalidated = self.invalidate_keep_alive_on_stop.load(Ordering::SeqCst);
            let provider_opt = self.stt_provider.read().await;
            if let Some(provider) = provider_opt.as_ref() {
                let supports_keep_alive = provider.supports_keep_alive();
                let is_connection_alive = provider.is_connection_alive();
                let keep_alive_enabled = keep_alive_enabled_for_config(&config);
                log::info!(
                    "[ReconnectDiag] start probe: provider={}, supports_keep_alive={}, is_connection_alive={}, keep_alive_enabled={}, config_keep_alive={}, provider_type={:?}, ttl_secs={}",
                    provider.name(),
                    supports_keep_alive,
                    is_connection_alive,
                    keep_alive_enabled,
                    config.keep_connection_alive,
                    config.provider,
                    config.keep_alive_ttl_secs
                );
                (
                    supports_keep_alive
                        && is_connection_alive
                        && keep_alive_enabled
                        && !keep_alive_invalidated,
                    format!(
                        "provider={}, supports_keep_alive={}, is_connection_alive={}, keep_alive_enabled={}, invalidated={}",
                        provider.name(),
                        supports_keep_alive,
                        is_connection_alive,
                        keep_alive_enabled,
                        keep_alive_invalidated
                    ),
                )
            } else {
                log::info!(
                    "[ReconnectDiag] start probe: no existing provider, provider_type={:?}, config_keep_alive={}, ttl_secs={}",
                    config.provider,
                    config.keep_connection_alive,
                    config.keep_alive_ttl_secs
                );
                (false, "no_existing_provider".to_string())
            }
        };

        if can_reuse_connection {
            log::info!("[ReconnectDiag] attempting keep-alive resume");

            let resume_result = {
                let mut provider_opt = self.stt_provider.write().await;
                if let Some(provider) = provider_opt.as_mut() {
                    await_stt_operation(
                        provider.resume_stream(
                            on_partial.clone(),
                            on_final.clone(),
                            provider_on_error.clone(),
                            on_connection_quality.clone(),
                        ),
                        STT_START_OPERATION_TIMEOUT,
                        "STT resume_stream",
                    )
                    .await
                } else {
                    Err(SttError::Processing("Provider not available".to_string()))
                }
            };

            match resume_result {
                Ok(_) => {
                    log::info!("[ReconnectDiag] keep-alive resume succeeded (instant start)");
                }
                Err(e) => {
                    log::warn!(
                        "[ReconnectDiag] keep-alive resume failed: {} - creating new connection as fallback",
                        e
                    );
                    reuse_decision_reason = format!("resume_failed: {}", e);

                    // Важно: перед тем как выкинуть провайдер, аккуратно закрываем его.
                    // Иначе есть риск оставить "висящий" WebSocket/таски в фоне.
                    if let Some(mut provider) = self.stt_provider.write().await.take() {
                        let _ = abort_stt_provider(&mut provider, "resume failure").await;
                    }
                    (startup_error_gate, provider_on_error) =
                        stt_startup_error_callback(on_error.clone());
                    can_reuse_connection = false;
                }
            }
        }

        if !can_reuse_connection {
            if let Some(mut provider) = self.stt_provider.write().await.take() {
                let _ = abort_stt_provider(&mut provider, "stale keep-alive connection").await;
            }
            let latest_config = self.config.read().await.clone();
            self.invalidate_keep_alive_on_stop.store(
                config_requires_new_connection(&config, &latest_config),
                Ordering::SeqCst,
            );

            // Создаем новое соединение (обычный старт с задержкой)
            log::info!(
                "[ReconnectDiag] creating new STT connection: reason={}, provider_type={:?}, config_keep_alive={}, ttl_secs={}",
                reuse_decision_reason,
                config.provider,
                config.keep_connection_alive,
                config.keep_alive_ttl_secs
            );

            let mut provider = match self.stt_factory.create(&config) {
                Ok(p) => p,
                Err(e) => {
                    // Важно: статус откатываем СИНХРОННО. Иначе возможен race:
                    // UI уже увидел Starting, но хоткей/команды будут думать что всё ещё Starting и игнорировать toggle.
                    self.stop_capture_after_failed_start().await;
                    *self.status.write().await =
                        if self.capture_is_active_for_run(token.run_id).await {
                            RecordingStatus::Recording
                        } else {
                            RecordingStatus::Idle
                        };
                    abort_prestart_visualizer_task(
                        &mut prestart_visual_task,
                        &prestart_visual_active,
                        "failed to create STT provider",
                    );
                    return Err(anyhow::Error::new(e).context("Failed to create STT provider"));
                }
            };

            if let Err(e) = provider.initialize(&config).await {
                log::error!("Failed to initialize STT provider: {}", e);
                self.stop_capture_after_failed_start().await;
                *self.status.write().await = if self.capture_is_active_for_run(token.run_id).await {
                    RecordingStatus::Recording
                } else {
                    RecordingStatus::Idle
                };
                let _ = abort_stt_provider(&mut provider, "initialize failure").await;
                abort_prestart_visualizer_task(
                    &mut prestart_visual_task,
                    &prestart_visual_active,
                    "failed to initialize STT provider",
                );
                return Err(anyhow::Error::new(e).context("Failed to initialize STT provider"));
            }

            // Publish the provisional provider before awaiting start_stream. A
            // concurrent cancellation can then take and explicitly abort it.
            *self.stt_provider.write().await = Some(provider);
            self.provider_run_id
                .store(token.run_id as usize, Ordering::Release);
            let start_stream_result = {
                let mut provider = self.stt_provider.write().await;
                match provider.as_mut() {
                    Some(provider) => {
                        await_stt_operation(
                            provider.start_stream(
                                on_partial.clone(),
                                on_final.clone(),
                                provider_on_error,
                                on_connection_quality.clone(),
                            ),
                            STT_START_OPERATION_TIMEOUT,
                            "STT start_stream",
                        )
                        .await
                    }
                    None => Err(SttError::Processing(
                        "Provider disappeared during startup".to_string(),
                    )),
                }
            };
            if let Err(e) = start_stream_result {
                self.stop_capture_after_failed_start().await;
                *self.status.write().await = if self.capture_is_active_for_run(token.run_id).await {
                    RecordingStatus::Recording
                } else {
                    RecordingStatus::Idle
                };
                if let Some(mut provider) = self.stt_provider.write().await.take() {
                    let _ = abort_stt_provider(&mut provider, "start_stream failure").await;
                }
                self.provider_run_id.store(0, Ordering::Release);
                abort_prestart_visualizer_task(
                    &mut prestart_visual_task,
                    &prestart_visual_active,
                    "failed to start STT stream",
                );
                return Err(anyhow::Error::new(e).context("Failed to start STT stream"));
            }
        }

        // Commit startup and Recording status under one gate. A provider error either wins
        // before this point and turns start into Err, or is reported as a runtime failure.
        let (startup_error, startup_cancelled) = {
            let mut status = self.status.write().await;
            let mut gate = lock_stt_startup_error_gate(&startup_error_gate);
            if let Some(error) = gate.error.take() {
                *status = RecordingStatus::Idle;
                gate.committed = true;
                (Some(error), false)
            } else if cancellation.is_some_and(|signal| signal.load(Ordering::Acquire)) {
                *status = RecordingStatus::Idle;
                gate.committed = true;
                (None, true)
            } else {
                *status = if accounting.sealed.load(Ordering::Acquire) {
                    RecordingStatus::Processing
                } else {
                    RecordingStatus::Recording
                };
                gate.committed = true;
                (None, false)
            }
        };
        if startup_cancelled {
            abort_prestart_visualizer_task(
                &mut prestart_visual_task,
                &prestart_visual_active,
                "cancelled before STT start commit",
            );
            return Err(anyhow::Error::new(PreparedCaptureCancelled));
        }
        if let Some(error) = startup_error {
            abort_prestart_visualizer_task(
                &mut prestart_visual_task,
                &prestart_visual_active,
                "STT reported an error during startup",
            );
            self.stop_capture_after_failed_start().await;
            if let Some(mut provider) = self.stt_provider.write().await.take() {
                let _ = abort_stt_provider(&mut provider, "STT runtime error during startup").await;
            }
            *self.status.write().await = if self.capture_is_active_for_run(token.run_id).await {
                RecordingStatus::Recording
            } else {
                RecordingStatus::Idle
            };
            self.provider_run_id.store(0, Ordering::Release);
            *self.provider_run_config.write().await = None;
            return Err(anyhow::Error::new(error));
        }
        self.provider_run_id
            .store(token.run_id as usize, Ordering::Release);
        *self.provider_run_config.write().await = Some(config.clone());
        if config.provider == SttProviderType::Backend
            && self
                .stt_provider
                .read()
                .await
                .as_ref()
                .is_some_and(|provider| provider.audio_delivery_progress().is_some())
        {
            // One source budget covers the receiver, current send and unacked
            // backend PCM. Dequeue/submission alone does not release capacity.
            accounting.track_acknowledgements();
        }

        // A cancellation may arrive during the provider ownership/accounting awaits
        // above. Keep retained PCM away from the processor until the outer owner
        // performs its normal cancellation cleanup.
        if cancellation.is_some_and(|signal| signal.load(Ordering::Acquire)) {
            abort_prestart_visualizer_task(
                &mut prestart_visual_task,
                &prestart_visual_active,
                "cancelled before audio processor attach",
            );
            return Err(anyhow::Error::new(PreparedCaptureCancelled));
        }

        // Теперь STT готов принимать аудио. Recording выставлен до запуска processor task,
        // чтобы предзахваченные чанки из очереди не были отброшены как "ещё Starting".
        // Capture owns visualization throughout recording, including provider backlog.

        // Запускаем обработчик чанков в async контексте
        self.attach_audio_processor(
            PreparedCapture {
                recovery_policy,
                device_failure_notified,
                token,
                config,
                rx,
                prefetched,
                on_chunk,
                queued_bytes,
                accounting,
                captured_chunks,
                overflowed: overflowed.clone(),
                prestart_visual_active,
                started_at: startup_started_at,
            },
            token.run_id,
            0,
            on_error,
            on_connection_quality,
        )
        .await;

        log::info!(
            "provider_ready_ms run_id={} capture_generation={} audio_capture_started_after_ms={} total_start_ms={} prebuffer_duration_ms={} prebuffer_bytes={} prebuffer_chunks={} overflowed={} prebuffer_dropped_chunks={}",
            token.run_id,
            token.generation,
            audio_capture_started_after.as_millis(),
            startup_started_at.elapsed().as_millis(),
            startup_started_at.elapsed().as_millis(),
            prebuffer_bytes_at_connect,
            prebuffer_chunks_at_connect,
            overflowed.load(Ordering::Acquire),
            dropped_chunks.load(Ordering::Relaxed)
        );
        Ok(())
    }

    /// Every episode drains through the same processor implementation; Continue
    /// changes physical ownership without installing provider callbacks again.
    async fn attach_audio_processor(
        &self,
        prepared: PreparedCapture,
        logical_run_id: u64,
        ack_base: u64,
        on_error: ErrorCallback,
        on_connection_quality: ConnectionQualityCallback,
    ) {
        let PreparedCapture {
            recovery_policy,
            device_failure_notified,
            token,
            mut rx,
            mut prefetched,
            on_chunk,
            queued_bytes,
            accounting,
            ..
        } = prepared;
        let dropped_chunks_for_processor = Arc::new(AtomicUsize::new(0));
        let stt_provider = self.stt_provider.clone();
        let status_arc = self.status.clone();
        let sensitivity_arc = self.microphone_sensitivity.clone();
        let on_error_for_processor = on_error.clone();
        let audio_capture = self.audio_capture.clone();
        let on_connection_quality_for_processor = on_connection_quality.clone();
        let on_chunk_for_restart = on_chunk.clone();
        let processor_panic_status = self.status.clone();
        let processor_panic_audio_capture = self.audio_capture.clone();
        let processor_panic_stt_provider = self.stt_provider.clone();
        let processor_panic_on_error = on_error.clone();
        let processor_owner = ProcessorOwner {
            token,
            logical_run_id,
            capture_run_id: self.capture_run_id.clone(),
            capture_generation: self.capture_generation.clone(),
            capture_meter_generation: self.capture_meter_generation.clone(),
            provider_run_id: self.provider_run_id.clone(),
        };
        let processor_panic_owner = processor_owner.clone();

        let processor_future = async move {
            let mut chunk_count = 0;
            let mut last_quality: Option<&'static str> = None;
            let mut good_streak: u32 = 0;
            let mut last_dropped_seen: usize = 0;
            let mut last_audio_at = Instant::now();
            let mut stall_restarts: u32 = 0;
            let mut audio_stats = AudioSessionStats::default();
            let mut ready_packet: Option<AudioChunk> = None;
            let mut last_latency_log_at: Option<Instant> = None;

            // На macOS/некоторых девайсах при отсутствии разрешения на микрофон или при "пустом" input
            // CoreAudio может отдавать строго нулевые семплы. Это выглядит как "всё работает", но речи нет.
            let mut consecutive_all_zero_chunks: u32 = 0;
            const ALL_ZERO_WARN_THRESHOLD: u32 = 60; // ~1-2 секунды (зависит от размера чанка)
            const ALL_ZERO_FATAL_THRESHOLD: u32 = 240; // ~6-8 секунд

            const AUDIO_STALL_TIMEOUT: Duration = Duration::from_millis(2200);
            const AUDIO_STALL_CHECK_INTERVAL: Duration = Duration::from_millis(650);
            loop {
                if accounting.sealed.load(Ordering::Acquire) {
                    // Stop seals this receiver, even while a new capture owns the UI.
                    // Buffered chunks remain readable; the restart callback cannot
                    // keep an already sealed receiver alive forever.
                    rx.close();
                }
                let maybe_chunk = if let Some(chunk) = prefetched.take() {
                    accounting.read(chunk.data.len() * 2);
                    Some(chunk)
                } else {
                    tokio::select! {
                        v = rx.recv() => {
                            if let Some(chunk) = v.as_ref() {
                                accounting.read(chunk.data.len().saturating_mul(std::mem::size_of::<i16>()));
                            }
                            v
                        },
                        _ = accounting.seal_notify.notified() => { continue; },
                        _ = tokio::time::sleep(AUDIO_STALL_CHECK_INTERVAL) => {
                            // Если долго не приходят чанки — захват аудио мог "отвалиться"
                            // (например, микрофон был отключён/переключён на уровне ОС).
                            let status = status_arc.read().await;
                            if *status == RecordingStatus::Processing {
                                continue;
                            }
                            if *status != RecordingStatus::Recording {
                                continue;
                            }
                            drop(status);

                            if last_audio_at.elapsed() < AUDIO_STALL_TIMEOUT {
                                continue;
                            }

                            if recovery_policy == CaptureRecoveryPolicy::OwnerManaged {
                                if !processor_owner.owns_capture() || accounting.sealed.load(Ordering::Acquire) { break; }
                                if !device_failure_notified.swap(true, Ordering::AcqRel) {
                                    on_error_for_processor(SttError::Processing("Audio capture pipeline stalled".into()));
                                }
                                Self::cleanup_failed_processor_session(
                                    &status_arc, &audio_capture, &stt_provider, &processor_owner,
                                    "owner-managed audio pipeline stalled",
                                ).await;
                                break;
                            }

                            let Some(restart_attempt) =
                                next_audio_stall_restart_attempt(&mut stall_restarts)
                            else {
                                let raw = format!(
                                    "Audio capture remains stalled after {} restart attempts",
                                    MAX_AUDIO_STALL_RESTARTS
                                );
                                log::error!("{}", raw);
                                on_error_for_processor(SttError::Processing(raw));
                                Self::cleanup_failed_processor_session(
                                    &status_arc,
                                    &audio_capture,
                                    &stt_provider, &processor_owner,
                                    "audio capture remained stalled after restart",
                                )
                                .await;
                                break;
                            };
                            log::warn!(
                                "Audio capture stalled (no chunks for {:?}). Restart attempt {}/{}",
                                AUDIO_STALL_TIMEOUT,
                                restart_attempt,
                                MAX_AUDIO_STALL_RESTARTS
                            );

                            on_connection_quality_for_processor(
                                "Poor".to_string(),
                                Some("Потерян аудиопоток (микрофон недоступен?). Пробую восстановить...".to_string()),
                            );
                            last_quality = Some("Poor");
                            good_streak = 0;

                            // Пытаемся мягко перезапустить захват аудио.
                            let restart_result = {
                                let mut cap = audio_capture.write().await;
                                if !processor_owner.owns_capture() || accounting.sealed.load(Ordering::Acquire) { break; }
                                let _ = cap.stop_capture().await;
                                cap.set_capture_identity(Some(AudioCaptureIdentity {
                                    run_id: token.run_id,
                                    generation: token.generation,
                                }));
                                cap.start_capture(on_chunk_for_restart.clone()).await
                            };

                            match restart_result {
                                Ok(_) => {
                                    log::info!("Audio capture restarted successfully after stall");
                                    last_audio_at = Instant::now();
                                    on_connection_quality_for_processor(
                                        "Recovering".to_string(),
                                        Some("Аудио восстановлено".to_string()),
                                    );
                                    last_quality = Some("Recovering");
                                    continue;
                                }
                                Err(e) => {
                                    log::error!("Failed to restart audio capture after stall: {}", e);
                                    if restart_attempt < MAX_AUDIO_STALL_RESTARTS {
                                        // Дадим шанс восстановиться (например, устройство вот-вот появится).
                                        continue;
                                    }

                                    // Фатально: возвращаем сервис в Idle, чтобы UI/хоткей не залипали,
                                    // и отправляем ошибку в UI.
                                    let raw = format!("Audio device is no longer available: {}", e);
                                    on_error_for_processor(SttError::Processing(raw));
                                    Self::cleanup_failed_processor_session(
                                        &status_arc,
                                        &audio_capture,
                                        &stt_provider, &processor_owner,
                                        "audio capture stall",
                                    )
                                    .await;
                                    break;
                                }
                            }
                        }
                    }
                };
                let Some(chunk) = maybe_chunk else {
                    break;
                };

                chunk_count += 1;
                last_audio_at = Instant::now();
                stall_restarts = 0;

                let status = *status_arc.read().await;
                if !accounting.sealed.load(Ordering::Acquire)
                    && status != RecordingStatus::Recording
                    && status != RecordingStatus::Processing
                {
                    break;
                }

                let sensitivity = sensitivity_arc.load(Ordering::Relaxed);
                let prepared = prepare_audio_chunk_for_processing(&chunk, sensitivity);
                let max_amplitude = prepared.max_amplitude;

                if max_amplitude == 0 {
                    consecutive_all_zero_chunks = consecutive_all_zero_chunks.saturating_add(1);
                } else {
                    consecutive_all_zero_chunks = 0;
                }

                if consecutive_all_zero_chunks == ALL_ZERO_WARN_THRESHOLD {
                    on_connection_quality_for_processor(
                        "Poor".to_string(),
                        Some(
                            "Не поступает сигнал с микрофона (все семплы = 0). Проверьте выбранное устройство и разрешение на микрофон в macOS."
                                .to_string(),
                        ),
                    );
                    last_quality = Some("Poor");
                    good_streak = 0;
                }

                if consecutive_all_zero_chunks >= ALL_ZERO_FATAL_THRESHOLD {
                    on_error_for_processor(SttError::Processing(
                        "Нет аудиосигнала с микрофона (все семплы = 0). Проверьте разрешение на микрофон в macOS и выбранное устройство записи."
                            .to_string(),
                    ));
                    Self::cleanup_failed_processor_session(
                        &status_arc,
                        &audio_capture,
                        &stt_provider,
                        &processor_owner,
                        "zero audio input",
                    )
                    .await;
                    break;
                }

                if chunk_count == 1 {
                    if prepared.effective_gain < prepared.requested_gain {
                        log::debug!(
                            "Microphone sensitivity: {}%, requested_gain: {:.2}x, effective_gain: {:.2}x (limited, peak={})",
                            sensitivity,
                            prepared.requested_gain,
                            prepared.effective_gain,
                            max_amplitude
                        );
                    } else {
                        log::debug!(
                            "Microphone sensitivity: {}%, gain: {:.2}x",
                            sensitivity,
                            prepared.requested_gain
                        );
                    }
                }

                let amplified_max = max_abs_amplitude(&prepared.amplified_chunk.data);
                audio_stats.observe(
                    max_amplitude,
                    amplified_max,
                    prepared.amplified_chunk.data.len(),
                    prepared.effective_gain,
                );
                let mut amplified_chunk = prepared.amplified_chunk;

                // Логируем каждый 20-й чанк для отладки
                if chunk_count % 20 == 0 {
                    log::debug!("Audio processing: chunk #{}, original_max={}, amplified_max={}, gain={:.2}x",
                        chunk_count, max_amplitude, amplified_max, prepared.effective_gain);
                }

                // Если начали дропать аудио из-за backpressure — это почти всегда признак "плохой сети"
                // или зависшей отправки. Показываем это пользователю через connection:quality.
                let dropped_now = dropped_chunks_for_processor.load(Ordering::Relaxed);
                if dropped_now > last_dropped_seen {
                    last_dropped_seen = dropped_now;
                    if last_quality != Some("Poor") {
                        on_connection_quality_for_processor(
                            "Poor".to_string(),
                            Some("Аудио не успевает отправляться (плохое соединение?)".to_string()),
                        );
                        last_quality = Some("Poor");
                        good_streak = 0;
                    }
                }

                let mut provider_guard = stt_provider.write().await;

                // Провайдера нет → это уже "поломанное" состояние.
                // Лучше остановить запись и показать ошибку, чем молча "писать" в пустоту.
                if !processor_owner.owns_provider() {
                    break;
                }
                let Some(provider) = provider_guard.as_mut() else {
                    drop(provider_guard);
                    on_error_for_processor(SttError::Processing(
                        "STT provider is not available (stream not active)".to_string(),
                    ));
                    if last_quality != Some("Poor") {
                        on_connection_quality_for_processor(
                            "Poor".to_string(),
                            Some("Соединение с провайдером потеряно".to_string()),
                        );
                    }
                    Self::cleanup_failed_processor_session(
                        &status_arc,
                        &audio_capture,
                        &stt_provider,
                        &processor_owner,
                        "missing STT provider",
                    )
                    .await;
                    break;
                };

                if let Some(mut packet) = ready_packet.take() {
                    if packet.sample_rate != amplified_chunk.sample_rate
                        || packet.channels != amplified_chunk.channels
                    {
                        let error = SttError::Configuration(
                            "Audio format changed within one capture run".into(),
                        );
                        accounting.fail(error.clone());
                        on_error_for_processor(error);
                        drop(provider_guard);
                        Self::cleanup_failed_processor_session(
                            &status_arc,
                            &audio_capture,
                            &stt_provider,
                            &processor_owner,
                            "capture format changed",
                        )
                        .await;
                        break;
                    }
                    // Gain was applied to each original chunk independently.
                    // Packetization never changes the audio samples or their clock.
                    packet.data.append(&mut amplified_chunk.data);
                    amplified_chunk = packet;
                }
                if provider
                    .preferred_audio_batch_samples()
                    .is_some_and(|target| amplified_chunk.data.len() < target && !rx.is_empty())
                {
                    // Only combine samples already available to this sole consumer.
                    // Live capture never waits for a packet to fill.
                    ready_packet = Some(amplified_chunk);
                    drop(provider_guard);
                    continue;
                }

                if chunk_count == 1 || chunk_count % 50 == 0 {
                    log::debug!(
                        "Processing audio chunk #{}, {} samples, max_amp={}",
                        chunk_count,
                        amplified_chunk.data.len(),
                        max_amplitude
                    );
                }

                let send_lease = accounting.sending(
                    amplified_chunk
                        .data
                        .len()
                        .saturating_mul(std::mem::size_of::<i16>()),
                );
                let send_timeout = provider
                    .audio_send_timeout()
                    .unwrap_or(STT_SEND_OPERATION_TIMEOUT);
                let send_started = Instant::now();
                let log_this_send =
                    last_latency_log_at.is_none_or(|last| last.elapsed() >= Duration::from_secs(1));
                let source_bytes_per_second = u64::from(amplified_chunk.sample_rate)
                    .saturating_mul(u64::from(amplified_chunk.channels.max(1)))
                    .saturating_mul(2)
                    .max(1);
                if log_this_send {
                    last_latency_log_at = Some(send_started);
                    let progress = provider.audio_delivery_progress();
                    let source = accounting.report(AudioDrainReason::Drained);
                    let capture_queued_bytes =
                        source.accepted_bytes.saturating_sub(source.read_bytes);
                    let captured_unix_ms = amplified_chunk.timestamp;
                    let now_unix_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|v| v.as_millis() as i64)
                        .unwrap_or(0);
                    log::info!(
                        "stt_audio_submit_start run_id={} capture_generation={} chunk={} unix_ms={} captured_unix_ms={} dequeued_chunk_age_ms={} capture_queued_bytes={} capture_queued_pcm_ms={} current_pcm_ms={} source_outstanding_bytes={} submitted_unacked_bytes={}",
                        logical_run_id, token.generation, chunk_count, now_unix_ms, captured_unix_ms,
                        now_unix_ms.saturating_sub(captured_unix_ms).max(0), capture_queued_bytes,
                        capture_queued_bytes.saturating_mul(1000) / source_bytes_per_second,
                        (amplified_chunk.data.len() as u64).saturating_mul(2000) / source_bytes_per_second,
                        queued_bytes.load(Ordering::Acquire),
                        progress.map(|p| p.sent_bytes.saturating_sub(p.acked_bytes)).unwrap_or(0)
                    );
                }
                let send_result = await_stt_operation(
                    provider.send_audio(&amplified_chunk),
                    send_timeout,
                    "STT send_audio",
                )
                .await;

                #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
                if send_result.is_ok() {
                    crate::presentation::native_e2e::record_live_provider_pcm(
                        logical_run_id,
                        token.run_id,
                        token.generation,
                        &amplified_chunk,
                    );
                }

                if matches!(send_result, Err(SttError::ContinuationAudioNotStarted)) {
                    send_lease.not_started();
                    // The terminal observer seals this exact capture. Preserve
                    // A's provider/receiver so its original drain can settle.
                    break;
                } else if send_result.is_ok() {
                    send_lease.submitted();
                } else {
                    // The provider may have written part of this frame. Never
                    // replay it or hide it behind a successful terminal result.
                    drop(send_lease);
                }
                let progress = provider.audio_delivery_progress();
                if let Some(progress) = progress {
                    accounting.acknowledge(progress.acked_bytes.saturating_sub(ack_base));
                }
                let send_elapsed_ms = send_started.elapsed().as_millis();
                if log_this_send || send_elapsed_ms >= 500 {
                    let source = accounting.report(AudioDrainReason::Drained);
                    let capture_queued_bytes =
                        source.accepted_bytes.saturating_sub(source.read_bytes);
                    log::info!(
                        "stt_audio_submit_done run_id={} capture_generation={} chunk={} unix_ms={} send_elapsed_ms={} success={} capture_queued_bytes={} source_outstanding_bytes={} submitted_unacked_bytes={}",
                        logical_run_id, token.generation, chunk_count,
                        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                            .map(|v| v.as_millis()).unwrap_or(0),
                        send_elapsed_ms, send_result.is_ok(), capture_queued_bytes,
                        queued_bytes.load(Ordering::Acquire),
                        progress.map(|p| p.sent_bytes.saturating_sub(p.acked_bytes)).unwrap_or(0)
                    );
                }

                match send_result {
                    Ok(_) => {
                        // Успешная отправка — сбрасываем счётчик ошибок
                        if last_quality == Some("Recovering") {
                            good_streak += 1;
                            if good_streak >= 20 {
                                on_connection_quality_for_processor("Good".to_string(), None);
                                last_quality = Some("Good");
                                good_streak = 0;
                            }
                        }
                    }
                    Err(error) => {
                        // A send error may follow a partial transport write. Stop
                        // this owner immediately; a later chunk must not retry the
                        // provider's retained batch on a damaged stream.
                        accounting.fail(error.clone());
                        on_error_for_processor(error);
                        on_connection_quality_for_processor(
                            "Poor".to_string(),
                            Some("Передача аудио прервана".to_string()),
                        );
                        drop(provider_guard);
                        Self::cleanup_failed_processor_session(
                            &status_arc,
                            &audio_capture,
                            &stt_provider,
                            &processor_owner,
                            "uncertain STT audio delivery",
                        )
                        .await;
                        break;
                    }
                }

                // Completion is sealed EOF above, not a global UI status.
            }
            log_audio_session_summary(&audio_stats);
            if audio_stats.looks_too_quiet_for_stt() {
                log::warn!(
                    "Audio session looked too quiet for STT: peak_raw={}, peak_sent={}, chunks={}. Empty transcript is likely caused by the selected input device or microphone level.",
                    audio_stats.peak_raw_amplitude,
                    audio_stats.peak_sent_amplitude,
                    audio_stats.chunks
                );
                if last_quality != Some("Poor") {
                    on_connection_quality_for_processor(
                        "Poor".to_string(),
                        Some(
                            "Очень тихий сигнал с микрофона. Проверьте выбранный input device и уровень микрофона."
                                .to_string(),
                        ),
                    );
                }
            }
            log::info!(
                "Audio chunk processor finished, total chunks: {}",
                chunk_count
            );
        };
        let processor_task = spawn_transcription_runtime_task(
            processor_future,
            processor_panic_status,
            processor_panic_audio_capture,
            processor_panic_stt_provider,
            processor_panic_owner,
            processor_panic_on_error,
        );

        *self.audio_processor_task.write().await = Some(processor_task);
    }

    /// Releases the physical microphone and drains already queued audio. Provider
    /// finalization is deliberately separate so a following run can prebuffer.
    pub async fn stop_capture_for_run(&self, run_id: u64) -> Result<()> {
        let owner = self.capture_run_id.load(Ordering::Acquire) as u64;
        if owner != run_id {
            let same_episode = self
                .active_episode
                .lock()
                .unwrap()
                .is_some_and(|token| token.run_id == run_id);
            if owner == 0
                && same_episode
                && self
                    .active_audio
                    .read()
                    .await
                    .as_ref()
                    .is_some_and(|(_, accounting)| accounting.sealed.load(Ordering::Acquire))
            {
                return Ok(());
            }
            anyhow::bail!("Audio capture is owned by run {owner}, not {run_id}");
        }

        // Fence UI immediately without invalidating the physical lease: a failed
        // device stop must still be retryable with the same capture token.
        self.capture_meter_generation.store(0, Ordering::Release);

        let mut prepared_slot = self.prepared_capture.lock().await;
        if let Some(prepared) = prepared_slot.take() {
            if prepared.token.run_id != run_id {
                *prepared_slot = Some(prepared);
                anyhow::bail!("Prepared capture belongs to another run");
            }
            let (stop_result, still_active) = {
                let mut capture = self.audio_capture.write().await;
                let result = capture.stop_capture().await;
                let still_active = capture.is_capturing();
                (result, still_active)
            };
            match (stop_result, still_active) {
                (Ok(()), false) => {
                    prepared
                        .prestart_visual_active
                        .store(false, Ordering::Release);
                    self.capture_generation.fetch_add(1, Ordering::AcqRel);
                    self.capture_run_id.store(0, Ordering::Release);
                    return Ok(());
                }
                (Err(error), false) => {
                    prepared
                        .prestart_visual_active
                        .store(false, Ordering::Release);
                    self.capture_generation.fetch_add(1, Ordering::AcqRel);
                    self.capture_run_id.store(0, Ordering::Release);
                    return Err(anyhow::anyhow!(
                        "Failed to stop audio capture, but it is inactive: {error}"
                    ));
                }
                (Ok(()), true) => {
                    *prepared_slot = Some(prepared);
                    anyhow::bail!("Audio capture reported success but remains active");
                }
                (Err(error), true) => {
                    *prepared_slot = Some(prepared);
                    return Err(anyhow::anyhow!("Failed to stop audio capture: {error}"));
                }
            }
        }
        drop(prepared_slot);

        let mut status = self.status.write().await;

        if !matches!(
            *status,
            RecordingStatus::Idle | RecordingStatus::Starting | RecordingStatus::Recording
        ) {
            anyhow::bail!("Not recording");
        }

        *status = RecordingStatus::Processing;
        drop(status);

        // Stop audio capture and verify the adapter actually released the device.
        let (stop_capture_result, still_active) = {
            let mut capture = self.audio_capture.write().await;
            let result = capture.stop_capture().await;
            let still_active = capture.is_capturing();
            (result, still_active)
        };

        if !still_active {
            if let Some((owner, accounting)) = self.active_audio.read().await.as_ref() {
                if *owner == run_id
                    || self
                        .active_episode
                        .lock()
                        .unwrap()
                        .is_some_and(|token| token.run_id == run_id)
                {
                    accounting.seal();
                }
            }
        }

        // Если не смогли остановить захват аудио — считаем это критическим сценарием:
        // лучше упасть с ошибкой, но гарантированно вернуть сервис в Idle, чем зависнуть в Processing.
        if let Err(e) = stop_capture_result {
            log::error!("Failed to stop audio capture: {}", e);
            if still_active {
                *self.status.write().await = RecordingStatus::Recording;
            } else {
                self.capture_generation.fetch_add(1, Ordering::AcqRel);
                self.capture_run_id.store(0, Ordering::Release);
            }
            return Err(anyhow::anyhow!("Failed to stop audio capture: {}", e));
        }

        if still_active {
            *self.status.write().await = RecordingStatus::Recording;
            anyhow::bail!("Audio capture reported success but remains active");
        }

        self.capture_generation.fetch_add(1, Ordering::AcqRel);
        self.capture_run_id.store(0, Ordering::Release);
        Ok(())
    }

    pub async fn capture_is_active_for_run(&self, run_id: u64) -> bool {
        self.capture_run_id.load(Ordering::Acquire) as u64 == run_id
            && self.audio_capture.read().await.is_capturing()
    }

    /// Finalizes the provider after the microphone has already been released.
    pub async fn finalize_provider_for_run(&self, run_id: u64) -> Result<String> {
        let _connection_guard = self.connection_lifecycle_guard.lock().await;
        if let Some(report) = self.completed_report_for_run(run_id).await {
            return match report.error {
                Some(error) => Err(anyhow::anyhow!(error)),
                None => Ok("Recording already finalized".to_string()),
            };
        }
        let owner = self.provider_run_id.load(Ordering::Acquire) as u64;
        if owner != run_id {
            anyhow::bail!("Provider is owned by run {owner}, not {run_id}");
        }
        // Drain belongs to finalization rather than physical release. This lets the
        // next run claim the microphone immediately while all old chunks still reach
        // the old provider before its finalize command.
        let accounting = self
            .active_audio
            .read()
            .await
            .as_ref()
            .filter(|(owner, _)| *owner == run_id)
            .map(|(_, accounting)| accounting.clone())
            .ok_or_else(|| anyhow::anyhow!("Audio accounting for run {run_id} is unavailable"))?;
        let timeout = audio_processor_stop_drain_timeout(
            self.provider_run_config.read().await.as_ref(),
            &accounting,
        );
        log::info!(
            "audio_processor_drain_started run_id={} remaining_bytes={} budget_ms={}",
            run_id,
            accounting.report(AudioDrainReason::Drained).remaining_bytes,
            timeout.as_millis()
        );
        let reason = self
            .drain_audio_processor_task("finalizing recording", timeout)
            .await;
        let mut audio = accounting.report(reason);
        let processor_error = accounting.failure();
        // Latch before any teardown, including a negotiated run's first error.
        if self
            .stt_provider
            .read()
            .await
            .as_ref()
            .is_some_and(|provider| provider.continuation_lifecycle_session().is_some())
        {
            self.continuation_owner.store(run_id, Ordering::Release);
        }
        let server_not_started = self
            .stt_provider
            .read()
            .await
            .as_ref()
            .is_some_and(|provider| provider.continuation_not_started().is_some());
        let result = if server_not_started {
            self.finalize_provider_inner(run_id).await
        } else if audio.unknown_bytes != 0 || processor_error.is_some() {
            // Cancellation/error may have interrupted a provider write. A graceful
            // stop could flush its partial internal frame; only hard teardown is safe.
            if let Some(mut provider) = self.stt_provider.write().await.take() {
                self.retain_provider_completion(provider.as_ref()).await;
                let _ = abort_stt_provider(&mut provider, "uncertain audio delivery").await;
            }
            *self.status.write().await = RecordingStatus::Idle;
            Err(processor_error.map(anyhow::Error::new).unwrap_or_else(|| {
                anyhow::anyhow!("Cannot finalize after uncertain audio delivery")
            }))
        } else {
            self.finalize_provider_inner(run_id).await
        };
        let mut provider_release = if self.stt_provider.read().await.is_some() {
            if result.is_ok() {
                crate::domain::ProviderRelease::Reusable
            } else {
                crate::domain::ProviderRelease::Unconfirmed
            }
        } else {
            crate::domain::ProviderRelease::Released
        };
        let provider = self.provider_completion.read().await.clone();
        if self.continuation_owner.load(Ordering::Acquire) == run_id {
            // Destroying a local socket is not server admission release evidence.
            provider_release = provider
                .as_ref()
                .map_or(crate::domain::ProviderRelease::Unconfirmed, |report| {
                    report.provider_release
                });
        }
        // BackendProvider sends the source PCM16 bytes unchanged. Other providers
        // may resample, so their transport counters must not be called source ACKs.
        if self
            .provider_run_config
            .read()
            .await
            .as_ref()
            .is_some_and(|config| config.provider == SttProviderType::Backend)
        {
            if let Some(progress) = *self.provider_delivery.read().await {
                accounting.acknowledge(
                    progress
                        .acked_bytes
                        .saturating_sub(self.active_ack_base.load(Ordering::Acquire)),
                );
                if let Some((owner, previous)) =
                    self.completed_episode_audio.lock().unwrap().as_ref()
                {
                    if *owner == run_id {
                        audio.accumulate(previous);
                    }
                }
                audio.acknowledged_bytes = Some(progress.acked_bytes.min(audio.submitted_bytes));
                audio.unacknowledged_bytes =
                    Some(audio.submitted_bytes.saturating_sub(progress.acked_bytes));
            }
        }
        if self.provider_delivery.read().await.is_none() {
            if let Some((owner, previous)) = self.completed_episode_audio.lock().unwrap().as_ref() {
                if *owner == run_id {
                    audio.accumulate(previous);
                }
            }
        }
        let shared_failure = result.as_ref().err().is_some_and(|error| {
            error.chain().filter_map(|cause| cause.downcast_ref::<SttError>()).any(|error| {
                matches!(error, SttError::Authentication(_) | SttError::Configuration(_))
                    || matches!(error, SttError::Connection(connection)
                        if matches!(connection.details.category,
                            Some(SttConnectionCategory::LimitExceeded | SttConnectionCategory::ProviderQuotaExceeded)))
            })
        });
        let error = result
            .as_ref()
            .err()
            .map(|error| format!("{error:#}"))
            .or_else(|| {
                audio.is_incomplete().then(|| {
                    format!(
                        "Audio drain incomplete: {:?}; remaining={} unknown={} unacknowledged={:?}",
                        audio.reason,
                        audio.remaining_bytes,
                        audio.unknown_bytes,
                        audio.unacknowledged_bytes
                    )
                })
            });
        let error = error.or_else(|| {
            (provider_release == crate::domain::ProviderRelease::Unconfirmed)
                .then(|| "Provider admission release remains unconfirmed".to_owned())
        });
        *self.completed_report.write().await = Some(FinalizeReport {
            continuation_delivery: *self.provider_delivery_mode.read().await,
            run_id,
            audio,
            provider,
            provider_release,
            error: error.clone(),
            shared_failure,
            continuation_not_started: self.continuation_not_started.read().await.clone(),
        });
        // A close failure no longer leaves the old run owning an already destroyed
        // provider. The backend independently enforces its own admission/lease.
        if provider_release != crate::domain::ProviderRelease::Unconfirmed {
            self.provider_run_id.store(0, Ordering::Release);
            self.last_finalized_run_id
                .store(run_id as usize, Ordering::Release);
            // The report is frozen and logical ownership is authoritatively released.
            // Pause/Continue share this lifecycle guard; retire only this run's lease.
            let mut paused = self.paused_continuation.lock().unwrap();
            if paused
                .as_ref()
                .is_some_and(|lease| lease.logical_run_id == run_id)
            {
                paused.take();
            }
        }
        match error {
            Some(error) => Err(anyhow::anyhow!(error)),
            None => result,
        }
    }

    /// Retained negotiation is authoritative even after local provider removal.
    pub async fn runtime_failure_requires_terminal(&self, run_id: u64) -> bool {
        let _guard = self.connection_lifecycle_guard.lock().await;
        self.continuation_owner.load(Ordering::Acquire) == run_id
            || (self.provider_run_id.load(Ordering::Acquire) as u64 == run_id
                && self
                    .stt_provider
                    .read()
                    .await
                    .as_ref()
                    .is_some_and(|provider| provider.continuation_lifecycle_session().is_some()))
    }

    pub async fn completed_report_for_run(&self, run_id: u64) -> Option<FinalizeReport> {
        self.completed_report
            .read()
            .await
            .as_ref()
            .filter(|report| report.run_id == run_id)
            .cloned()
    }

    async fn retain_provider_completion(&self, provider: &dyn SttProvider) {
        if let Some(mode) = provider.continuation_delivery_mode() {
            self.provider_delivery_mode
                .write()
                .await
                .get_or_insert(mode);
        }
        if let Some(disposition) = provider.continuation_not_started() {
            *self.continuation_not_started.write().await = Some(disposition);
        }
        if let Some(report) = provider.finalize_evidence() {
            *self.provider_completion.write().await = Some(report);
        }
        if let Some(progress) = provider.audio_delivery_progress() {
            *self.provider_delivery.write().await = Some(progress);
        }
    }

    async fn finalize_provider_inner(&self, run_id: u64) -> Result<String> {
        // Проверяем нужно ли держать соединение открытым (keep-alive режим)
        let config = self
            .provider_run_config
            .read()
            .await
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Provider config for run {run_id} is unavailable"))?;
        let keep_alive_invalidated = self.invalidate_keep_alive_on_stop.load(Ordering::SeqCst);
        let (should_keep_alive, keep_alive_reason) = {
            let provider_opt = self.stt_provider.read().await;
            if let Some(provider) = provider_opt.as_ref() {
                let supports_keep_alive = provider.supports_keep_alive();
                let keep_alive_enabled = keep_alive_enabled_for_config(&config);
                let is_connection_alive_before_pause = provider.is_connection_alive();
                log::info!(
                    "[ReconnectDiag] stop probe: provider={}, supports_keep_alive={}, is_connection_alive_before_pause={}, keep_alive_enabled={}, config_keep_alive={}, provider_type={:?}, ttl_secs={}",
                    provider.name(),
                    supports_keep_alive,
                    is_connection_alive_before_pause,
                    keep_alive_enabled,
                    config.keep_connection_alive,
                    config.provider,
                    config.keep_alive_ttl_secs
                );
                (
                    supports_keep_alive && keep_alive_enabled && !keep_alive_invalidated,
                    format!(
                        "provider={}, supports_keep_alive={}, keep_alive_enabled={}, invalidated={}, alive_before_pause={}",
                        provider.name(),
                        supports_keep_alive,
                        keep_alive_enabled,
                        keep_alive_invalidated,
                        is_connection_alive_before_pause
                    ),
                )
            } else {
                log::info!(
                    "[ReconnectDiag] stop probe: no provider, provider_type={:?}, config_keep_alive={}",
                    config.provider,
                    config.keep_connection_alive
                );
                (false, "no_provider".to_string())
            }
        };
        if self.stt_provider.read().await.is_none() {
            anyhow::bail!("Provider disappeared before run {run_id} could finalize");
        }

        if should_keep_alive {
            // Ставим на паузу вместо полной остановки (keep-alive режим)
            log::info!(
                "[ReconnectDiag] pausing STT stream (keep-alive mode): {}",
                keep_alive_reason
            );

            // Важно: остановка записи должна быть максимально надёжной.
            // Даже если pause_stream фейлится (например, сеть отвалилась в момент stop),
            // мы всё равно должны вернуть статус в Idle и не оставлять сервис в Processing.
            let mut provider = match self.stt_provider.write().await.take() {
                Some(p) => p,
                None => anyhow::bail!("Provider disappeared while finalizing run {run_id}"),
            };

            let pause_result = await_stt_operation(
                provider.pause_stream(),
                STT_PAUSE_OPERATION_TIMEOUT,
                "STT pause_stream",
            )
            .await;
            self.retain_provider_completion(provider.as_ref()).await;
            if let Err(e) = pause_result {
                log::warn!(
                    "[ReconnectDiag] failed to pause STT stream (keep-alive). Falling back to hard close: {}",
                    e
                );

                // Фоллбек: закрываем соединение полностью, чтобы не держать "полуживой" провайдер.
                let _ = abort_stt_provider(&mut provider, "pause_stream failure").await;
                self.invalidate_keep_alive_on_stop
                    .store(false, Ordering::SeqCst);

                *self.status.write().await = RecordingStatus::Idle;
                return Err(anyhow::Error::new(e).context("Failed to finalize STT stream"));
            }

            log::info!(
                "[ReconnectDiag] pause_stream succeeded: is_connection_alive_after_pause={}",
                provider.is_connection_alive()
            );

            // Возвращаем провайдера назад в состояние сервиса (keep-alive продолжается)
            *self.stt_provider.write().await = Some(provider);

            // Запускаем таймер на TTL (keep_alive_ttl_secs) для автоматического закрытия соединения.
            //
            // Важно: keep-alive удерживает WS соединение открытым. Если держать слишком долго,
            // можно упереться в лимиты провайдера на параллельные соединения (например Deepgram).
            // Поэтому TTL должен быть коротким и конфигурируемым.
            let stt_provider = self.stt_provider.clone();
            let status_arc = self.status.clone();
            let ttl_secs = config.keep_alive_ttl_secs.max(10); // защитный минимум
            let inactivity_timer = tokio::spawn(async move {
                log::info!("Inactivity timer started ({} seconds)", ttl_secs);
                tokio::time::sleep(tokio::time::Duration::from_secs(ttl_secs)).await;

                // Проверяем что статус все еще Idle (не началась новая запись)
                let current_status = *status_arc.read().await;
                if current_status == RecordingStatus::Idle {
                    log::info!(
                        "Inactivity timeout reached ({}s) - closing persistent connection",
                        ttl_secs
                    );

                    if let Some(mut provider) = stt_provider.write().await.take() {
                        if stop_stt_provider(&mut provider, "inactivity timeout")
                            .await
                            .is_err()
                        {
                            let _ = abort_stt_provider(&mut provider, "inactivity timeout").await;
                        }
                    }

                    log::info!("Persistent connection closed");
                } else {
                    log::debug!("Inactivity timer cancelled - recording restarted before timeout");
                }
            });

            *self.inactivity_timer_task.write().await = Some(inactivity_timer);
            *self.status.write().await = RecordingStatus::Idle;

            let ttl_secs_for_log = ttl_secs;
            if ttl_secs_for_log >= 60 {
                log::info!(
                    "Recording paused, connection kept alive (will auto-close in {} min)",
                    (ttl_secs_for_log + 59) / 60
                );
            } else {
                log::info!(
                    "Recording paused, connection kept alive (will auto-close in {}s)",
                    ttl_secs_for_log
                );
            }
            Ok("Recording paused, connection kept alive".to_string())
        } else {
            // Обычная остановка для провайдеров без keep-alive
            log::info!("Stopping STT stream completely");

            let mut finalize_error = None;
            if let Some(mut provider) = self.stt_provider.write().await.take() {
                let result = stop_stt_provider(&mut provider, "recording stop").await;
                self.retain_provider_completion(provider.as_ref()).await;
                if let Err(e) = result {
                    log::warn!("Failed to stop STT stream cleanly, aborting: {}", e);
                    let _ = abort_stt_provider(&mut provider, "recording stop failure").await;
                    finalize_error = Some(e);
                }
            }
            self.invalidate_keep_alive_on_stop
                .store(false, Ordering::SeqCst);

            *self.status.write().await = RecordingStatus::Idle;

            if let Some(error) = finalize_error {
                return Err(anyhow::Error::new(error).context("Failed to finalize STT stream"));
            }

            log::info!("Recording stopped");
            Ok("Transcription completed".to_string())
        }
    }

    /// Compatibility wrapper for callers that do not use the split lifecycle.
    pub async fn stop_recording(&self) -> Result<String> {
        let run_id = self.capture_run_id.load(Ordering::Acquire) as u64;
        self.stop_capture_for_run(run_id).await?;
        self.finalize_provider_for_run(run_id).await
    }

    /// Get current recording status
    pub async fn get_status(&self) -> RecordingStatus {
        *self.status.read().await
    }

    /// Returns true when the next start can resume an already-open keep-alive stream
    /// without creating a new WebSocket connection.
    pub async fn can_resume_keep_alive_connection(&self) -> bool {
        let status = *self.status.read().await;
        if status != RecordingStatus::Idle {
            return false;
        }

        let config = self.config.read().await.clone();
        let keep_alive_enabled = keep_alive_enabled_for_config(&config);

        if !keep_alive_enabled {
            return false;
        }
        if self.invalidate_keep_alive_on_stop.load(Ordering::SeqCst) {
            return false;
        }

        let provider_opt = self.stt_provider.read().await;
        provider_opt
            .as_ref()
            .map(|provider| provider.supports_keep_alive() && provider.is_connection_alive())
            .unwrap_or(false)
    }

    /// Update STT configuration
    pub async fn update_config(&self, config: SttConfig) -> Result<()> {
        let _connection_guard = self.connection_lifecycle_guard.lock().await;
        let prev_config = self.config.read().await.clone();

        let mut config = config;
        if config.provider == SttProviderType::Backend {
            // Держим клиентский TTL ниже backend audio_idle_ttl_secs=3600, чтобы не переиспользовать
            // WS в момент, когда сервер уже закрывает idle stream.
            if config.keep_alive_ttl_secs != crate::domain::BACKEND_KEEPALIVE_TTL_SECS {
                config.keep_alive_ttl_secs = crate::domain::BACKEND_KEEPALIVE_TTL_SECS;
            }
        }

        // Важно: если в keep-alive режиме уже есть "живое" соединение (пауза между сессиями),
        // смена критичных параметров (язык/кейтермы/провайдер) должна сбросить это соединение.
        // Иначе следующий старт записи может сделать resume_stream() и фактически продолжить старую сессию,
        // где язык уже "залип" на предыдущем Config message.
        let requires_new_connection = config_requires_new_connection(&prev_config, &config);

        // Serialize the config write and any keep-alive teardown against start_recording's
        // Idle -> Starting transition. Otherwise a new recording can resume the provider
        // between the Idle check and the teardown below.
        let status = self.status.read().await;

        if requires_new_connection {
            if *status == RecordingStatus::Idle {
                let has_keep_alive_connection = {
                    let provider_opt = self.stt_provider.read().await;
                    provider_opt
                        .as_ref()
                        .map(|p| p.supports_keep_alive() && p.is_connection_alive())
                        .unwrap_or(false)
                };

                if has_keep_alive_connection {
                    // Отменяем таймер TTL (если был), чтобы он не "стрельнул" после того как мы уже закрыли провайдера.
                    if let Some(timer) = self.inactivity_timer_task.write().await.take() {
                        timer.abort();
                        let _ = timer.await;
                    }

                    // Закрываем провайдера целиком: следующий start_recording создаст новое соединение
                    // и отправит новый Config message (с новым языком и т.д.).
                    if let Some(mut provider) = self.stt_provider.write().await.take() {
                        if let Err(e) = stop_stt_provider(&mut provider, "config change").await {
                            log::warn!(
                                "Failed to stop keep-alive stream on config change, aborting: {}",
                                e
                            );
                            let _ =
                                abort_stt_provider(&mut provider, "config change failure").await;
                        }
                    }
                }
                self.invalidate_keep_alive_on_stop
                    .store(false, Ordering::SeqCst);
            } else {
                // Не прерываем текущую запись, но запрещаем переиспользовать её connection identity.
                self.invalidate_keep_alive_on_stop
                    .store(true, Ordering::SeqCst);
                log::info!(
                    "STT config updated while status={:?}; connection will close when recording stops",
                    *status
                );
            }
        }

        *self
            .config_snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = config.clone();
        *self.config.write().await = config;
        drop(status);
        Ok(())
    }

    /// Get current configuration
    pub async fn get_config(&self) -> SttConfig {
        self.config.read().await.clone()
    }

    pub fn get_config_snapshot(&self) -> SttConfig {
        self.config_snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Initialize audio capture with configuration
    pub async fn initialize_audio(&self, config: AudioConfig) -> Result<()> {
        self.audio_capture
            .write()
            .await
            .initialize(config)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to initialize audio: {}", e))
    }

    /// Replace audio capture device (only when not recording)
    /// Полезно для смены микрофона без перезапуска приложения
    pub async fn replace_audio_capture(&self, new_capture: Box<dyn AudioCapture>) -> Result<()> {
        self.replace_audio_capture_with_policy(new_capture, CaptureRecoveryPolicy::LegacyRestart)
            .await
    }

    pub fn uses_owner_managed_capture(&self) -> bool {
        *self.capture_recovery_policy.lock().unwrap() == CaptureRecoveryPolicy::OwnerManaged
    }

    pub fn has_capture_owner(&self) -> bool {
        self.capture_run_id.load(Ordering::Acquire) != 0
    }

    pub async fn replace_audio_capture_with_policy(
        &self,
        new_capture: Box<dyn AudioCapture>,
        policy: CaptureRecoveryPolicy,
    ) -> Result<()> {
        let mut capture = self.audio_capture.write().await;
        let capture_owner = self.capture_run_id.load(Ordering::Acquire);
        if capture_owner != 0 {
            anyhow::bail!(
                "Cannot replace audio capture while physical capture is owned by run {}",
                capture_owner
            );
        }

        log::info!("Replacing audio capture device");
        *capture = new_capture;
        *self.capture_recovery_policy.lock().unwrap() = policy;
        self.set_effective_capture_device(None);
        log::info!("Audio capture device replaced successfully");

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[path = "warm_capture_tests.rs"]
    mod warm_capture_tests;
    #[test]
    fn continue_window_negotiation_preserves_legacy_and_caps_untrusted_values() {
        assert_eq!(negotiated_continue_window(None), Duration::from_secs(2));
        assert_eq!(
            negotiated_continue_window(Some(2000)),
            Duration::from_secs(2)
        );
        assert_eq!(
            negotiated_continue_window(Some(5000)),
            Duration::from_secs(5)
        );
        assert_eq!(negotiated_continue_window(Some(0)), Duration::from_secs(2));
        assert_eq!(
            negotiated_continue_window(Some(5001)),
            Duration::from_secs(2)
        );
    }

    #[test]
    fn el_stop_budget_tracks_own_source_backlog_and_preserves_dg_deadline() {
        let a = AudioAccounting::new(Arc::new(AtomicUsize::new(0)));
        a.observe_source_format(16_000, 1);
        let mut config = SttConfig::default();
        config.backend_streaming_provider = crate::domain::BackendStreamingProvider::ElevenLabs;
        assert_eq!(
            audio_processor_stop_drain_timeout(Some(&config), &a),
            Duration::from_millis(2500)
        );
        a.accepted(8 * 32_000);
        assert_eq!(
            audio_processor_stop_drain_timeout(Some(&config), &a),
            Duration::from_millis(10500)
        );
        let b = AudioAccounting::new(Arc::new(AtomicUsize::new(0)));
        b.observe_source_format(48_000, 2);
        b.accepted(192_000);
        assert_eq!(
            audio_processor_stop_drain_timeout(Some(&config), &b),
            Duration::from_millis(3500)
        );
        assert_eq!(
            audio_processor_stop_drain_timeout(Some(&config), &a),
            Duration::from_millis(10500)
        );
        a.accepted(100 * 32_000);
        assert_eq!(
            audio_processor_stop_drain_timeout(Some(&config), &a),
            Duration::from_millis(32500)
        );
        config.backend_streaming_provider = crate::domain::BackendStreamingProvider::Deepgram;
        assert_eq!(
            audio_processor_stop_drain_timeout(Some(&config), &a),
            Duration::from_millis(2500)
        );
        assert!(
            TranscriptionService::maximum_stop_cleanup_timeout()
                >= Duration::from_millis(32500)
                    + STT_STOP_OPERATION_TIMEOUT
                    + STT_ABORT_OPERATION_TIMEOUT
        );
    }

    #[tokio::test]
    async fn processor_cleanup_rejects_replaced_capture_lease_and_provider_owner() {
        for current_capture_run in [7, 8] {
            let capture_stopped = Arc::new(AtomicBool::new(false));
            let provider_aborted = Arc::new(AtomicBool::new(false));
            let capture = BurstAudioCapture::new(capture_stopped.clone(), 0);
            capture.is_capturing.store(true, Ordering::Release);
            let audio_capture: Arc<RwLock<Box<dyn AudioCapture>>> =
                Arc::new(RwLock::new(Box::new(capture)));
            let stt_provider: Arc<RwLock<Option<Box<dyn SttProvider>>>> =
                Arc::new(RwLock::new(Some(Box::new(AlwaysFailSendProvider {
                    panic_on_send: false,
                    aborted: provider_aborted.clone(),
                }))));
            let status = Arc::new(RwLock::new(RecordingStatus::Recording));
            let owner = ProcessorOwner {
                token: PreparedCaptureToken {
                    run_id: 7,
                    generation: 1,
                },
                logical_run_id: 7,
                capture_run_id: Arc::new(AtomicUsize::new(current_capture_run)),
                capture_generation: Arc::new(AtomicUsize::new(2)),
                capture_meter_generation: Arc::new(AtomicUsize::new(2)),
                provider_run_id: Arc::new(AtomicUsize::new(8)),
            };
            TranscriptionService::cleanup_failed_processor_session(
                &status,
                &audio_capture,
                &stt_provider,
                &owner,
                "delayed A cleanup",
            )
            .await;
            assert!(!capture_stopped.load(Ordering::Acquire));
            assert!(!provider_aborted.load(Ordering::Acquire));
            assert!(audio_capture.read().await.is_capturing());
            assert!(stt_provider.read().await.is_some());
            assert_eq!(*status.read().await, RecordingStatus::Recording);
            assert_eq!(owner.capture_meter_generation.load(Ordering::Acquire), 2);
        }
    }

    #[tokio::test]
    async fn processor_cleanup_aborts_only_a_provider_while_b_capture_is_pending() {
        let capture_stopped = Arc::new(AtomicBool::new(false));
        let provider_aborted = Arc::new(AtomicBool::new(false));
        let capture = BurstAudioCapture::new(capture_stopped.clone(), 0);
        capture.is_capturing.store(true, Ordering::Release);
        let audio_capture: Arc<RwLock<Box<dyn AudioCapture>>> =
            Arc::new(RwLock::new(Box::new(capture)));
        let stt_provider: Arc<RwLock<Option<Box<dyn SttProvider>>>> =
            Arc::new(RwLock::new(Some(Box::new(AlwaysFailSendProvider {
                panic_on_send: false,
                aborted: provider_aborted.clone(),
            }))));
        let status = Arc::new(RwLock::new(RecordingStatus::Processing));
        let owner = ProcessorOwner {
            token: PreparedCaptureToken {
                run_id: 7,
                generation: 1,
            },
            logical_run_id: 7,
            capture_run_id: Arc::new(AtomicUsize::new(8)),
            capture_generation: Arc::new(AtomicUsize::new(2)),
            capture_meter_generation: Arc::new(AtomicUsize::new(2)),
            provider_run_id: Arc::new(AtomicUsize::new(7)),
        };
        TranscriptionService::cleanup_failed_processor_session(
            &status,
            &audio_capture,
            &stt_provider,
            &owner,
            "A drain failure",
        )
        .await;
        assert!(!capture_stopped.load(Ordering::Acquire));
        assert!(provider_aborted.load(Ordering::Acquire));
        assert!(stt_provider.read().await.is_none());
        assert_eq!(*status.read().await, RecordingStatus::Processing);
        assert_eq!(
            owner.provider_run_id.load(Ordering::Acquire),
            7,
            "final report still owns A"
        );
    }

    #[test]
    fn capture_meter_latest_replaces_backlog_with_bounded_current_tail() {
        let latest = super::CaptureMeterLatest::default();
        for value in 1..100 {
            latest.publish(&crate::domain::AudioChunk::new(vec![value; 4096], 16000, 1));
        }
        latest.publish(&crate::domain::AudioChunk::new(vec![0; 4096], 16000, 1));
        let snapshot = latest.take().expect("latest frame");
        assert_eq!(snapshot.data, vec![0; super::CAPTURE_METER_MAX_SAMPLES]);
        assert!(latest.take().is_none(), "never replay a consumed frame");
    }

    #[test]
    fn capture_meter_callback_does_not_wait_for_busy_slot() {
        let latest = super::CaptureMeterLatest::default();
        let guard = latest.0.lock().unwrap();
        latest.publish(&crate::domain::AudioChunk::new(vec![100; 512], 16000, 1));
        drop(guard);
        assert!(latest.take().is_none());
    }

    #[test]
    fn capture_meter_rechecks_fence_between_level_and_spectrum() {
        let current = Arc::new(AtomicBool::new(true));
        let fence = current.clone();
        let level: AudioLevelCallback = Arc::new(move |_, _| {
            fence.store(false, Ordering::Release);
        });
        let spectrum: AudioSpectrumCallback =
            Arc::new(|_, _| panic!("stale spectrum crossed stop fence"));
        let chunk = AudioChunk::new(vec![1000; 512], 16000, 1);
        let prepared = super::prepare_audio_chunk_for_processing(&chunk, 100);
        super::emit_audio_visualization(
            crate::domain::AudioCaptureIdentity {
                run_id: 1,
                generation: 1,
            },
            &prepared,
            &mut crate::application::AudioSpectrumAnalyzer::new(),
            &level,
            &spectrum,
            || current.load(Ordering::Acquire),
        );
        assert!(!current.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn capture_meter_stays_current_during_provider_backlog_and_fences_stop() {
        let sent_chunks = Arc::new(AtomicUsize::new(0));
        let service = TranscriptionService::new(
            Box::new(ImmediateAudioCapture::new()),
            Arc::new(CountingFactory {
                sent_chunks: sent_chunks.clone(),
                stopped: Arc::new(AtomicBool::new(false)),
                delay_per_chunk: Duration::from_millis(100),
                start_stream_delay: Duration::ZERO,
            }),
        );
        let (tx, mut levels) = tokio::sync::mpsc::unbounded_channel();
        let level: AudioLevelCallback = Arc::new(move |identity, value| {
            let _ = tx.send((identity, value));
        });
        let token = service
            .prepare_recording_capture(
                71,
                SttConfig::default(),
                level.clone(),
                Arc::new(|_, _| {}),
                Arc::new(|_| {}),
            )
            .await
            .unwrap();
        let on_chunk = service
            .prepared_capture
            .lock()
            .await
            .as_ref()
            .unwrap()
            .on_chunk
            .clone();
        for _ in 0..6 {
            on_chunk(AudioChunk::new(vec![9000; 480], 16000, 1));
        }
        service
            .connect_prepared_recording(
                token,
                Arc::new(|_| {}),
                Arc::new(|_| {}),
                level,
                Arc::new(|_, _| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
                Arc::new(AtomicBool::new(false)),
            )
            .await
            .unwrap();
        while levels.try_recv().is_ok() {}
        let mut live_silence = AudioChunk::new(vec![0; 480], 16000, 1);
        live_silence.timestamp = 12345;
        on_chunk(live_silence);
        let (identity, value) = tokio::time::timeout(Duration::from_millis(150), levels.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(identity.owner.run_id, token.run_id);
        assert_eq!(identity.owner.generation, token.generation);
        assert_eq!(identity.captured_at_ms, 12345);
        assert_eq!(
            value, 0.0,
            "meter must show live silence, not queued speech"
        );
        assert!(
            sent_chunks.load(Ordering::SeqCst) < 6,
            "provider backlog still exists"
        );
        service.stop_capture_for_run(token.run_id).await.unwrap();
        while levels.try_recv().is_ok() {}
        on_chunk(AudioChunk::new(vec![9000; 480], 16000, 1));
        tokio::time::sleep(Duration::from_millis(75)).await;
        assert!(
            levels.try_recv().is_err(),
            "late capture callback cannot emit after stop"
        );
        service
            .finalize_provider_for_run(token.run_id)
            .await
            .unwrap();
    }

    use super::*;
    use crate::domain::{AudioResult, SttResult};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::time::Duration;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn transcription_service_thread_safety_is_compiler_checked() {
        assert_send_sync::<TranscriptionService>();
    }

    #[tokio::test]
    async fn stt_operation_timeout_has_typed_timeout_category() {
        let err = await_stt_operation(
            std::future::pending::<SttResult<()>>(),
            Duration::from_millis(20),
            "test STT operation",
        )
        .await
        .unwrap_err();

        assert!(matches!(
            err,
            SttError::Connection(connection)
                if connection.details.category == Some(SttConnectionCategory::Timeout)
                    && connection.message.contains("test STT operation timed out")
        ));
    }

    #[test]
    fn audio_stall_restart_budget_resets_only_after_real_audio() {
        let mut completed_attempts = 0;

        assert_eq!(
            next_audio_stall_restart_attempt(&mut completed_attempts),
            Some(1)
        );
        assert_eq!(
            next_audio_stall_restart_attempt(&mut completed_attempts),
            Some(2)
        );
        assert_eq!(
            next_audio_stall_restart_attempt(&mut completed_attempts),
            Some(3)
        );
        assert_eq!(
            next_audio_stall_restart_attempt(&mut completed_attempts),
            None
        );

        completed_attempts = 0;
        assert_eq!(
            next_audio_stall_restart_attempt(&mut completed_attempts),
            Some(1)
        );
    }

    struct BurstAudioCapture {
        config: AudioConfig,
        is_capturing: Arc<AtomicBool>,
        stop_called: Arc<AtomicBool>,
        chunks_to_send: usize,
    }

    impl BurstAudioCapture {
        fn new(stop_called: Arc<AtomicBool>, chunks_to_send: usize) -> Self {
            Self {
                config: AudioConfig::default(),
                is_capturing: Arc::new(AtomicBool::new(false)),
                stop_called,
                chunks_to_send,
            }
        }
    }

    #[async_trait]
    impl AudioCapture for BurstAudioCapture {
        async fn initialize(&mut self, config: AudioConfig) -> AudioResult<()> {
            self.config = config;
            Ok(())
        }

        async fn start_capture(
            &mut self,
            on_chunk: crate::domain::AudioChunkCallback,
        ) -> AudioResult<()> {
            self.is_capturing.store(true, Ordering::SeqCst);

            let is_capturing = self.is_capturing.clone();
            let cfg = self.config;
            let chunks_to_send = self.chunks_to_send;

            // Важно: отправляем чанки асинхронно и с небольшой задержкой,
            // чтобы сервис успел перевести статус в Recording.
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(25)).await;
                for _ in 0..chunks_to_send {
                    if !is_capturing.load(Ordering::SeqCst) {
                        break;
                    }

                    let data = vec![0i16; 160]; // маленький чанк, нам важен сам факт send_audio()
                    let chunk = crate::domain::AudioChunk::new(data, cfg.sample_rate, cfg.channels);
                    on_chunk(chunk);
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            });

            Ok(())
        }

        async fn stop_capture(&mut self) -> AudioResult<()> {
            self.is_capturing.store(false, Ordering::SeqCst);
            self.stop_called.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn is_capturing(&self) -> bool {
            self.is_capturing.load(Ordering::SeqCst)
        }

        fn config(&self) -> AudioConfig {
            self.config
        }
    }

    struct FailingStartAudioCapture {
        config: AudioConfig,
    }

    impl Default for FailingStartAudioCapture {
        fn default() -> Self {
            Self {
                config: AudioConfig::default(),
            }
        }
    }

    #[async_trait]
    impl AudioCapture for FailingStartAudioCapture {
        async fn initialize(&mut self, config: AudioConfig) -> AudioResult<()> {
            self.config = config;
            Ok(())
        }

        async fn start_capture(
            &mut self,
            _on_chunk: crate::domain::AudioChunkCallback,
        ) -> AudioResult<()> {
            Err(crate::domain::AudioError::Capture(
                "simulated start_capture failure".to_string(),
            ))
        }

        async fn stop_capture(&mut self) -> AudioResult<()> {
            Ok(())
        }

        fn is_capturing(&self) -> bool {
            false
        }

        fn config(&self) -> AudioConfig {
            self.config
        }
    }

    struct AlwaysFailSendProvider {
        panic_on_send: bool,
        aborted: Arc<AtomicBool>,
    }

    #[async_trait]
    impl SttProvider for AlwaysFailSendProvider {
        async fn initialize(&mut self, _config: &SttConfig) -> SttResult<()> {
            Ok(())
        }

        async fn start_stream(
            &mut self,
            _on_partial: TranscriptionCallback,
            _on_final: TranscriptionCallback,
            _on_error: ErrorCallback,
            _on_connection_quality: ConnectionQualityCallback,
        ) -> SttResult<()> {
            Ok(())
        }

        async fn send_audio(&mut self, _chunk: &crate::domain::AudioChunk) -> SttResult<()> {
            assert!(!self.panic_on_send, "simulated provider send panic");
            Err(SttError::Connection(
                crate::domain::SttConnectionError::simple("simulated connection drop"),
            ))
        }

        async fn stop_stream(&mut self) -> SttResult<()> {
            Ok(())
        }

        async fn abort(&mut self) -> SttResult<()> {
            self.aborted.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn name(&self) -> &str {
            "always_fail_send"
        }

        fn is_online(&self) -> bool {
            true
        }
    }

    struct TestFactory {
        panic_on_send: bool,
        aborted: Arc<AtomicBool>,
    }

    impl SttProviderFactory for TestFactory {
        fn create(&self, _config: &SttConfig) -> SttResult<Box<dyn SttProvider>> {
            Ok(Box::new(AlwaysFailSendProvider {
                panic_on_send: self.panic_on_send,
                aborted: self.aborted.clone(),
            }))
        }
    }

    struct StartupErrorProvider {
        aborted: Arc<AtomicBool>,
    }

    #[async_trait]
    impl SttProvider for StartupErrorProvider {
        async fn initialize(&mut self, _config: &SttConfig) -> SttResult<()> {
            Ok(())
        }

        async fn start_stream(
            &mut self,
            _on_partial: TranscriptionCallback,
            _on_final: TranscriptionCallback,
            on_error: ErrorCallback,
            _on_connection_quality: ConnectionQualityCallback,
        ) -> SttResult<()> {
            on_error(SttError::Connection(
                crate::domain::SttConnectionError::simple(
                    "simulated receiver failure during dictation startup",
                ),
            ));
            Ok(())
        }

        async fn send_audio(&mut self, _chunk: &crate::domain::AudioChunk) -> SttResult<()> {
            Ok(())
        }

        async fn stop_stream(&mut self) -> SttResult<()> {
            Ok(())
        }

        async fn abort(&mut self) -> SttResult<()> {
            self.aborted.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn name(&self) -> &str {
            "startup_error"
        }

        fn is_online(&self) -> bool {
            true
        }
    }

    struct StartupErrorFactory {
        aborted: Arc<AtomicBool>,
    }

    impl SttProviderFactory for StartupErrorFactory {
        fn create(&self, _config: &SttConfig) -> SttResult<Box<dyn SttProvider>> {
            Ok(Box::new(StartupErrorProvider {
                aborted: self.aborted.clone(),
            }))
        }
    }

    struct ManualAudioCapture {
        config: AudioConfig,
        is_capturing: Arc<AtomicBool>,
        on_chunk: Arc<std::sync::Mutex<Option<crate::domain::AudioChunkCallback>>>,
    }

    impl ManualAudioCapture {
        fn new(on_chunk: Arc<std::sync::Mutex<Option<crate::domain::AudioChunkCallback>>>) -> Self {
            Self {
                config: AudioConfig::default(),
                is_capturing: Arc::new(AtomicBool::new(false)),
                on_chunk,
            }
        }
    }

    #[async_trait]
    impl AudioCapture for ManualAudioCapture {
        async fn initialize(&mut self, config: AudioConfig) -> AudioResult<()> {
            self.config = config;
            Ok(())
        }

        async fn start_capture(
            &mut self,
            on_chunk: crate::domain::AudioChunkCallback,
        ) -> AudioResult<()> {
            self.is_capturing.store(true, Ordering::SeqCst);
            *self.on_chunk.lock().expect("callback mutex poisoned") = Some(on_chunk);
            Ok(())
        }

        async fn stop_capture(&mut self) -> AudioResult<()> {
            self.is_capturing.store(false, Ordering::SeqCst);
            *self.on_chunk.lock().expect("callback mutex poisoned") = None;
            Ok(())
        }

        fn is_capturing(&self) -> bool {
            self.is_capturing.load(Ordering::SeqCst)
        }

        fn config(&self) -> AudioConfig {
            self.config
        }
    }

    struct ImmediateAudioCapture {
        config: AudioConfig,
        is_capturing: Arc<AtomicBool>,
    }

    struct BlockingStartAudioCapture {
        config: AudioConfig,
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        is_capturing: Arc<AtomicBool>,
        fail_after_release: bool,
    }

    #[async_trait]
    impl AudioCapture for BlockingStartAudioCapture {
        async fn initialize(&mut self, config: AudioConfig) -> AudioResult<()> {
            self.config = config;
            Ok(())
        }

        async fn start_capture(
            &mut self,
            _on_chunk: crate::domain::AudioChunkCallback,
        ) -> AudioResult<()> {
            self.is_capturing.store(true, Ordering::SeqCst);
            self.entered.notify_one();
            self.release.notified().await;
            if self.fail_after_release {
                self.is_capturing.store(false, Ordering::SeqCst);
                return Err(crate::domain::AudioError::Capture(
                    "simulated delayed start_capture failure".to_string(),
                ));
            }
            Ok(())
        }

        async fn stop_capture(&mut self) -> AudioResult<()> {
            self.is_capturing.store(false, Ordering::SeqCst);
            Ok(())
        }

        fn is_capturing(&self) -> bool {
            self.is_capturing.load(Ordering::SeqCst)
        }

        fn config(&self) -> AudioConfig {
            self.config
        }
    }

    impl ImmediateAudioCapture {
        fn new() -> Self {
            Self {
                config: AudioConfig::default(),
                is_capturing: Arc::new(AtomicBool::new(false)),
            }
        }
    }

    #[async_trait]
    impl AudioCapture for ImmediateAudioCapture {
        async fn initialize(&mut self, config: AudioConfig) -> AudioResult<()> {
            self.config = config;
            Ok(())
        }

        async fn start_capture(
            &mut self,
            on_chunk: crate::domain::AudioChunkCallback,
        ) -> AudioResult<()> {
            self.is_capturing.store(true, Ordering::SeqCst);
            on_chunk(crate::domain::AudioChunk::new(
                vec![1200i16; 480],
                self.config.sample_rate,
                self.config.channels,
            ));
            Ok(())
        }

        async fn stop_capture(&mut self) -> AudioResult<()> {
            self.is_capturing.store(false, Ordering::SeqCst);
            Ok(())
        }

        fn is_capturing(&self) -> bool {
            self.is_capturing.load(Ordering::SeqCst)
        }

        fn config(&self) -> AudioConfig {
            self.config
        }
    }

    struct ErrorInjectCapture {
        inner: ImmediateAudioCapture,
        callback: Arc<StdMutex<Option<crate::domain::AudioCaptureErrorCallback>>>,
        fail_during_start: bool,
    }

    #[async_trait]
    impl AudioCapture for ErrorInjectCapture {
        async fn initialize(&mut self, config: AudioConfig) -> AudioResult<()> {
            self.inner.initialize(config).await
        }
        fn set_terminal_error_callback(
            &mut self,
            callback: Option<crate::domain::AudioCaptureErrorCallback>,
        ) {
            *self.callback.lock().unwrap() = callback;
        }
        async fn start_capture(
            &mut self,
            on_chunk: crate::domain::AudioChunkCallback,
        ) -> AudioResult<()> {
            self.inner.start_capture(on_chunk).await?;
            if self.fail_during_start {
                self.callback.lock().unwrap().as_ref().unwrap()(
                    crate::domain::AudioError::Capture("device disappeared".into()),
                );
            }
            Ok(())
        }
        async fn stop_capture(&mut self) -> AudioResult<()> {
            self.inner.stop_capture().await
        }
        fn is_capturing(&self) -> bool {
            self.inner.is_capturing()
        }
        fn config(&self) -> AudioConfig {
            self.inner.config()
        }
    }

    fn device_loss_fixture(
        fail_during_start: bool,
    ) -> (
        TranscriptionService,
        Arc<StdMutex<Option<crate::domain::AudioCaptureErrorCallback>>>,
    ) {
        let callback = Arc::new(StdMutex::new(None));
        let service = TranscriptionService::new(
            Box::new(ErrorInjectCapture {
                inner: ImmediateAudioCapture::new(),
                callback: callback.clone(),
                fail_during_start,
            }),
            Arc::new(CountingFactory {
                sent_chunks: Arc::new(AtomicUsize::new(0)),
                stopped: Arc::new(AtomicBool::new(false)),
                delay_per_chunk: Duration::ZERO,
                start_stream_delay: Duration::ZERO,
            }),
        );
        (service, callback)
    }

    #[tokio::test]
    async fn sealed_episode_retains_exact_pcm_rejects_stale_ingress_and_stays_busy() {
        let (service, _) = device_loss_fixture(false);
        let token = service
            .prepare_recording_capture(
                101,
                SttConfig::default(),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_| {}),
            )
            .await
            .unwrap();
        let ingress = service
            .prepared_capture
            .lock()
            .await
            .as_ref()
            .unwrap()
            .on_chunk
            .clone();
        ingress(AudioChunk::new(vec![321; 17], 16000, 1));
        service.seal_prepared_capture(token).await.unwrap();
        assert!(!service.capture_token_is_current(token));
        assert!(!service.audio_capture.read().await.is_capturing());
        ingress(AudioChunk::new(vec![999; 8], 16000, 1));
        assert!(service
            .prepare_recording_capture(
                102,
                SttConfig::default(),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_| {})
            )
            .await
            .is_err());
        let mut slot = service.prepared_capture.lock().await;
        let prepared = slot.as_mut().unwrap();
        assert_eq!(prepared.rx.recv().await.unwrap().data, vec![1200; 480]);
        assert_eq!(prepared.rx.recv().await.unwrap().data, vec![321; 17]);
        assert!(prepared.rx.recv().await.is_none());
        assert_eq!(prepared.captured_chunks.load(Ordering::Acquire), 2);
        drop(slot);
        service.cancel_prepared_capture(token).await.unwrap();
        assert!(service.prepared_capture.lock().await.is_none());
        let next = service
            .prepare_recording_capture(
                102,
                SttConfig::default(),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_| {}),
            )
            .await
            .unwrap();
        assert!(service.cancel_prepared_capture(token).await.is_err());
        assert!(service.capture_token_is_current(next));
        service.cancel_prepared_capture(next).await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn stop_before_ready_cancels_connection_and_releases_own_capture() {
        let sent = Arc::new(AtomicUsize::new(0));
        let service = Arc::new(TranscriptionService::new(
            Box::new(ImmediateAudioCapture::new()),
            Arc::new(CountingFactory {
                sent_chunks: sent.clone(),
                stopped: Arc::new(AtomicBool::new(false)),
                delay_per_chunk: Duration::ZERO,
                start_stream_delay: Duration::from_secs(8),
            }),
        ));
        let token = service
            .prepare_recording_capture(
                101,
                SttConfig::default(),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_| {}),
            )
            .await
            .unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let work = service.clone();
        let cancel = cancelled.clone();
        let task = tokio::spawn(async move {
            work.connect_prepared_recording(
                token,
                Arc::new(|_| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
                cancel,
            )
            .await
        });
        tokio::task::yield_now().await;
        assert!(!task.is_finished());
        assert!(service.audio_capture.read().await.is_capturing());
        assert_eq!(sent.load(Ordering::Acquire), 0);
        cancelled.store(true, Ordering::Release);
        let result = tokio::time::timeout(Duration::from_millis(100), task)
            .await
            .unwrap()
            .unwrap();
        assert!(result.unwrap_err().to_string().contains("cancelled"));
        assert!(!service.audio_capture.read().await.is_capturing());
        assert!(service.stt_provider.read().await.is_none());
        assert!(service.paused_continuation_snapshot().await.is_none());
        assert_eq!(service.logical_provider_run_id(), 0);
        assert_eq!(service.get_status().await, RecordingStatus::Idle);
        assert_eq!(sent.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn thirty_second_prepared_buffer_reports_overflow_without_evicting_prefix() {
        let (service, _) = device_loss_fixture(false);
        let errors = Arc::new(StdMutex::new(Vec::new()));
        let observed = errors.clone();
        let token = service
            .prepare_recording_capture(
                101,
                SttConfig::default(),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                Arc::new(move |e| observed.lock().unwrap().push(e.to_string())),
            )
            .await
            .unwrap();
        let ingress = service
            .prepared_capture
            .lock()
            .await
            .as_ref()
            .unwrap()
            .on_chunk
            .clone();
        // The fixture already queued 480 samples. Fill exactly 30 seconds at 16 kHz.
        ingress(AudioChunk::new(vec![321; 16_000 * 30 - 480], 16000, 1));
        assert!(errors.lock().unwrap().is_empty());
        ingress(AudioChunk::new(vec![999; 1], 16000, 1));
        ingress(AudioChunk::new(vec![999; 1], 16000, 1));
        assert_eq!(errors.lock().unwrap().len(), 1);
        assert!(errors.lock().unwrap()[0].contains("30 seconds PCM16"));
        {
            let mut slot = service.prepared_capture.lock().await;
            let prepared = slot.as_mut().unwrap();
            assert!(prepared.overflowed.load(Ordering::Acquire));
            assert_eq!(prepared.rx.try_recv().unwrap().data, vec![1200; 480]);
            assert_eq!(
                prepared.rx.try_recv().unwrap().data,
                vec![321; 16_000 * 30 - 480]
            );
            assert!(prepared.rx.try_recv().is_err());
        }
        let result = service
            .connect_prepared_recording(
                token,
                Arc::new(|_| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
                Default::default(),
            )
            .await;
        assert!(result.unwrap_err().to_string().contains("overflow"));
        assert!(!service.audio_capture.read().await.is_capturing());
        assert!(service.prepared_capture.lock().await.is_none());
        assert!(service.stt_provider.read().await.is_none());
    }

    #[tokio::test]
    async fn sealed_prepared_episode_connects_and_drains_without_restarting_microphone() {
        let sent = Arc::new(AtomicUsize::new(0));
        let service = TranscriptionService::new(
            Box::new(ImmediateAudioCapture::new()),
            Arc::new(CountingFactory {
                sent_chunks: sent.clone(),
                stopped: Arc::new(AtomicBool::new(false)),
                delay_per_chunk: Duration::ZERO,
                start_stream_delay: Duration::ZERO,
            }),
        );
        let token = service
            .prepare_recording_capture(
                101,
                SttConfig::default(),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_| {}),
            )
            .await
            .unwrap();
        service.seal_prepared_capture(token).await.unwrap();
        service
            .connect_prepared_recording(
                token,
                Arc::new(|_| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
                Arc::new(AtomicBool::new(false)),
            )
            .await
            .unwrap();
        assert!(!service.audio_capture.read().await.is_capturing());
        service.finalize_provider_for_run(101).await.unwrap();
        assert_eq!(sent.load(Ordering::Acquire), 1);
        assert!(!service.audio_capture.read().await.is_capturing());
    }

    #[tokio::test]
    async fn device_loss_during_start_cannot_report_capture_ready() {
        let (service, _) = device_loss_fixture(true);
        let errors = Arc::new(AtomicUsize::new(0));
        let observed = errors.clone();
        let result = service
            .prepare_recording_capture(
                101,
                SttConfig::default(),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                Arc::new(move |_| {
                    observed.fetch_add(1, Ordering::SeqCst);
                }),
            )
            .await;
        assert!(result.is_err());
        assert_eq!(errors.load(Ordering::SeqCst), 1);
        assert!(!service.audio_capture.read().await.is_capturing());
        assert!(service.prepared_capture.lock().await.is_none());
        assert_eq!(service.capture_run_id.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn device_loss_is_once_and_stale_error_preserves_replacement_capture() {
        let (service, slot) = device_loss_fixture(false);
        let errors = Arc::new(AtomicUsize::new(0));
        let observed = errors.clone();
        let on_error: ErrorCallback = Arc::new(move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
        });
        service
            .prepare_recording_capture(
                101,
                SttConfig::default(),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                on_error.clone(),
            )
            .await
            .unwrap();
        let old_error = slot.lock().unwrap().as_ref().unwrap().clone();
        service.stop_capture_for_run(101).await.unwrap();
        let token = service
            .prepare_recording_capture(
                102,
                SttConfig::default(),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                on_error,
            )
            .await
            .unwrap();
        let new_error = slot.lock().unwrap().as_ref().unwrap().clone();
        let active = service
            .prepared_capture
            .lock()
            .await
            .as_ref()
            .unwrap()
            .prestart_visual_active
            .clone();
        old_error(crate::domain::AudioError::Capture(
            "late old device error".into(),
        ));
        assert_eq!(errors.load(Ordering::SeqCst), 0);
        assert!(active.load(Ordering::Acquire));
        assert!(service.capture_token_is_current(token));
        assert!(service.capture_is_active_for_run(102).await);
        for _ in 0..2 {
            new_error(crate::domain::AudioError::Capture(
                "current device error".into(),
            ));
        }
        assert_eq!(errors.load(Ordering::SeqCst), 1);
        assert!(!active.load(Ordering::Acquire));
        service.stop_capture_for_run(102).await.unwrap();
        assert!(service.prepared_capture.lock().await.is_none());
        assert!(!service.audio_capture.read().await.is_capturing());
    }

    #[derive(Default)]
    struct ContinuationTestLog {
        explicit_report: Option<crate::domain::ProviderFinalizeReport>,
        error_callback: Option<ErrorCallback>,
        starts: usize,
        stops: usize,
        aborts: usize,
        samples: Vec<i16>,
        operations: Vec<crate::domain::ContinuationOperation>,
        pause_sample_counts: Vec<usize>,
        send_delay: Option<Duration>,
        first_write_barrier: Option<Arc<tokio::sync::Notify>>,
    }
    struct ContinuationTestGuard {
        revision: AtomicU64,
        refuse: AtomicBool,
    }
    #[async_trait]
    impl crate::domain::ContinuationContextGuard for ContinuationTestGuard {
        async fn validate(&self, _: u64) -> crate::domain::ContextValidation {
            if self.refuse.load(Ordering::Acquire) {
                crate::domain::ContextValidation::Mismatch
            } else {
                crate::domain::ContextValidation::Valid {
                    revision: self.revision.load(Ordering::Acquire),
                }
            }
        }
    }
    struct ContinuationTestFactory {
        log: Arc<StdMutex<ContinuationTestLog>>,
        guard: Arc<ContinuationTestGuard>,
        refuse_after_accepted: bool,
        first_b_mode: u8,
        continue_entered: Arc<tokio::sync::Notify>,
        continue_release: Option<Arc<tokio::sync::Notify>>,
    }
    impl SttProviderFactory for ContinuationTestFactory {
        fn create(&self, _: &SttConfig) -> SttResult<Box<dyn SttProvider>> {
            Ok(Box::new(ContinuationTestProvider {
                log: self.log.clone(),
                guard: self.guard.clone(),
                refuse_after_accepted: self.refuse_after_accepted,
                first_b_mode: self.first_b_mode,
                continue_entered: self.continue_entered.clone(),
                continue_release: self.continue_release.clone(),
                final_callback: None,
                progress: crate::domain::AudioDeliveryProgress::default(),
                continued: false,
                terminal: false,
                unknown_closed: false,
                epoch: 0,
                control_seq: 0,
            }))
        }
    }
    struct ContinuationTestProvider {
        log: Arc<StdMutex<ContinuationTestLog>>,
        guard: Arc<ContinuationTestGuard>,
        refuse_after_accepted: bool,
        first_b_mode: u8,
        continue_entered: Arc<tokio::sync::Notify>,
        continue_release: Option<Arc<tokio::sync::Notify>>,
        final_callback: Option<TranscriptionCallback>,
        progress: crate::domain::AudioDeliveryProgress,
        continued: bool,
        terminal: bool,
        unknown_closed: bool,
        epoch: u64,
        control_seq: u64,
    }
    #[async_trait]
    impl SttProvider for ContinuationTestProvider {
        async fn initialize(&mut self, _: &SttConfig) -> SttResult<()> {
            Ok(())
        }
        async fn start_stream(
            &mut self,
            _: TranscriptionCallback,
            on_final: TranscriptionCallback,
            on_error: ErrorCallback,
            _: ConnectionQualityCallback,
        ) -> SttResult<()> {
            self.log.lock().unwrap().starts += 1;
            self.log.lock().unwrap().error_callback = Some(on_error);
            self.final_callback = Some(on_final);
            Ok(())
        }
        async fn send_audio(&mut self, chunk: &AudioChunk) -> SttResult<()> {
            let delay = self.log.lock().unwrap().send_delay;
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            self.log.lock().unwrap().samples.extend(&chunk.data);
            if self.continued && self.first_b_mode == 3 {
                self.continue_entered.notify_one();
                if let Some(release) = &self.continue_release {
                    release.notified().await;
                }
            }
            if self.continued && self.first_b_mode == 1 {
                return Err(SttError::Processing("unknown B write".into()));
            }
            self.progress.sent_bytes += chunk.data.len() as u64 * 2;
            if !(self.continued && self.first_b_mode == 6) {
                self.progress.acked_bytes = self.progress.sent_bytes;
            }
            Ok(())
        }
        async fn stop_stream(&mut self) -> SttResult<()> {
            self.log.lock().unwrap().stops += 1;
            self.terminal = true;
            Ok(())
        }
        async fn abort(&mut self) -> SttResult<()> {
            self.log.lock().unwrap().aborts += 1;
            self.terminal = true;
            Ok(())
        }
        fn name(&self) -> &str {
            "fake EL continuation"
        }
        fn is_online(&self) -> bool {
            true
        }
        fn continuation_session(&self) -> Option<crate::domain::ContinuationSession> {
            (!self.terminal && !self.unknown_closed).then(|| crate::domain::ContinuationSession {
                connection_generation: 1,
                provider_session_id: "fake-el".into(),
            })
        }
        fn continuation_lifecycle_session(&self) -> Option<crate::domain::ContinuationSession> {
            Some(crate::domain::ContinuationSession {
                connection_generation: 1,
                provider_session_id: "fake-el".into(),
            })
        }
        fn is_connection_alive(&self) -> bool {
            !self.terminal && !self.unknown_closed
        }
        fn audio_delivery_progress(&self) -> Option<crate::domain::AudioDeliveryProgress> {
            Some(self.progress)
        }
        async fn send_first_continuation_audio(
            &mut self,
            _: &crate::domain::ContinuationSession,
            _: u64,
            chunk: &AudioChunk,
            fence: &crate::domain::ContinuationWriteFence,
        ) -> SttResult<crate::domain::ContinuationFirstWrite> {
            let barrier = self.log.lock().unwrap().first_write_barrier.clone();
            if let Some(barrier) = barrier {
                barrier.notified().await;
            }
            if self.continued && self.first_b_mode == 2 {
                return Ok(crate::domain::ContinuationFirstWrite::NotStarted);
            }
            if fence.revoked() {
                return Ok(crate::domain::ContinuationFirstWrite::NotStarted);
            }
            fence.attempted.store(true, Ordering::Release);
            self.send_audio(chunk).await?;
            Ok(crate::domain::ContinuationFirstWrite::Written)
        }
        fn finalize_evidence(&self) -> Option<crate::domain::ProviderFinalizeReport> {
            if let Some(report) = self.log.lock().unwrap().explicit_report.clone() {
                return Some(report);
            }
            (self.terminal && !self.unknown_closed).then(|| crate::domain::ProviderFinalizeReport {
                reason: crate::domain::FinalizeReason::Drained,
                tail_evidence: crate::domain::TailEvidence::SegmentObserved,
                provider_release: crate::domain::ProviderRelease::Released,
                last_delivery_seq: 2,
                stable_snapshot: "A late A".into(),
                error: None,
            })
        }
        async fn continuation_control(
            &mut self,
            _: &crate::domain::ContinuationSession,
            operation: crate::domain::ContinuationOperation,
            deadline: Instant,
        ) -> SttResult<crate::domain::ContinuationControlResult> {
            use crate::domain::{ContinuationOperation::*, ContinuationPhase::*};
            self.log.lock().unwrap().operations.push(operation.clone());
            self.control_seq += 1;
            let phase = match operation {
                Pause { .. } => {
                    {
                        let mut log = self.log.lock().unwrap();
                        let count = log.samples.len();
                        log.pause_sample_counts.push(count);
                    }
                    if self.first_b_mode == 5 {
                        tokio::time::sleep(Duration::from_millis(1500)).await;
                    }
                    self.epoch += 1;
                    PausedReclaimable
                }
                Continue { pause_epoch } => {
                    if self.first_b_mode == 4 {
                        tokio::time::sleep_until(deadline).await;
                        self.unknown_closed = true;
                        return Err(SttError::Processing(
                            "control/status outcome unknown".into(),
                        ));
                    }
                    assert_eq!(pause_epoch, self.epoch);
                    self.continue_entered.notify_one();
                    if self.first_b_mode != 3 {
                        if let Some(release) = &self.continue_release {
                            release.notified().await;
                        }
                    }
                    self.continued = true;
                    self.guard
                        .refuse
                        .store(self.refuse_after_accepted, Ordering::Release);
                    let mut t = crate::domain::Transcription::final_result("late A".into());
                    t.delivery_seq = Some(self.control_seq);
                    self.final_callback.as_ref().unwrap()(t);
                    ActiveAwaitingAudio
                }
                Restore {
                    pause_epoch,
                    continue_request_id,
                } => {
                    assert_eq!(pause_epoch, self.epoch);
                    assert_eq!(continue_request_id, (self.control_seq - 1).to_string());
                    self.continued = false;
                    PausedReclaimable
                }
            };
            Ok(crate::domain::ContinuationControlResult {
                request_id: self.control_seq.to_string(),
                provider_session_id: "fake-el".into(),
                pause_epoch: Some(self.epoch),
                decision: crate::domain::ControlDecision::Accepted,
                current_phase: phase,
                eligible_now: true,
                reason: None,
                continue_window_ms: Some(5000),
            })
        }
    }
    async fn continuation_service_fixture(
        refuse: bool,
        first_b_mode: u8,
        block: Option<Arc<tokio::sync::Notify>>,
    ) -> (
        Arc<TranscriptionService>,
        Arc<StdMutex<ContinuationTestLog>>,
        Arc<tokio::sync::Notify>,
        Arc<StdMutex<Vec<String>>>,
    ) {
        continuation_service_fixture_with_capture(refuse, first_b_mode, block, None).await
    }

    async fn continuation_service_fixture_with_capture(
        refuse: bool,
        first_b_mode: u8,
        block: Option<Arc<tokio::sync::Notify>>,
        manual: Option<Arc<StdMutex<Option<crate::domain::AudioChunkCallback>>>>,
    ) -> (
        Arc<TranscriptionService>,
        Arc<StdMutex<ContinuationTestLog>>,
        Arc<tokio::sync::Notify>,
        Arc<StdMutex<Vec<String>>>,
    ) {
        let log = Arc::new(StdMutex::new(ContinuationTestLog::default()));
        let guard = Arc::new(ContinuationTestGuard {
            revision: AtomicU64::new(7),
            refuse: AtomicBool::new(false),
        });
        let entered = Arc::new(tokio::sync::Notify::new());
        let device_identity = Arc::new(StdMutex::new(None));
        let capture = crate::presentation::state::effective_capture::EffectiveCapture::new(
            SelectingCapture {
                inner: ImmediateAudioCapture::new(),
                selected: Some("D".into()),
                after_start: Some("fixture microphone".into()),
            },
            device_identity.clone(),
            |capture| capture.selected.clone(),
        );
        let capture: Box<dyn AudioCapture> = if let Some(chunks) = manual {
            *device_identity.lock().unwrap() = Some("fixture microphone".into());
            Box::new(ManualAudioCapture::new(chunks))
        } else {
            Box::new(capture)
        };
        let service = Arc::new(
            TranscriptionService::new_with_microphone_sensitivity_and_device(
                capture,
                Arc::new(ContinuationTestFactory {
                    log: log.clone(),
                    guard: guard.clone(),
                    refuse_after_accepted: refuse,
                    first_b_mode: first_b_mode,
                    continue_entered: entered.clone(),
                    continue_release: block,
                }),
                Arc::new(AtomicU8::new(100)),
                device_identity,
            ),
        );
        service.set_continuation_context_guard(guard);
        let mut config = SttConfig::new(SttProviderType::Backend);
        config.backend_streaming_provider = crate::domain::BackendStreamingProvider::ElevenLabs;
        config.continuation_target_eligible = true;
        config.backend_auth_token = Some("fixture-account".into());
        service.update_config(config).await.unwrap();
        let mut requested = crate::domain::AppConfig::default();
        requested.selected_audio_device = Some("D".into());
        service.register_continuation_policy(101, &requested).await;
        let token = service
            .prepare_recording_capture(
                101,
                service.get_config_snapshot(),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_| {}),
            )
            .await
            .unwrap();
        let deliveries = Arc::new(StdMutex::new(Vec::new()));
        let observed = deliveries.clone();
        let weak_service = Arc::downgrade(&service);
        let on_error: ErrorCallback = Arc::new(move |_| {
            let weak_service = weak_service.clone();
            tokio::spawn(async move {
                if let Some(service) = weak_service.upgrade() {
                    service
                        .cleanup_runtime_failure_for_run(101, "provider runtime error callback")
                        .await;
                }
            });
        });
        service
            .connect_prepared_recording(
                token,
                Arc::new(|_| {}),
                Arc::new(move |t| observed.lock().unwrap().push(t.text)),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                on_error,
                Arc::new(|_, _| {}),
                Arc::new(AtomicBool::new(false)),
            )
            .await
            .unwrap();
        (service, log, entered, deliveries)
    }
    // Real transport + production observation: the fake provider's active alive=true
    // cannot exercise the paused-only idle probe that caused the spontaneous Stop.
    #[tokio::test]
    async fn real_backend_active_observer_survives_audio_and_observes_terminal() {
        use crate::infrastructure::stt::BackendProvider;
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        struct RealFactory;
        impl SttProviderFactory for RealFactory {
            fn create(&self, _: &SttConfig) -> SttResult<Box<dyn SttProvider>> {
                Ok(Box::new(BackendProvider::with_continuation_for_test()))
            }
        }

        for terminal in ["close", "eof", "error", "finalize", "abort"] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("ws://{}", listener.local_addr().unwrap());
            let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);
            let pcm = Arc::new(StdMutex::new(Vec::new()));
            let received = pcm.clone();
            let errors = Arc::new(StdMutex::new(Vec::new()));
            let observed_errors = errors.clone();
            let peer = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                let Message::Text(config) = ws.next().await.unwrap().unwrap() else {
                    panic!("expected Config text frame");
                };
                let config: serde_json::Value = serde_json::from_str(&config).unwrap();
                assert_eq!(config["type"], "config");
                assert_eq!(config["protocol_v"], 2);
                assert_eq!(config["provider"], "elevenlabs");
                assert_eq!(config["sample_rate"], 16000);
                assert_eq!(config["channels"], 1);
                assert_eq!(config["encoding"], "pcm_s16le");
                let capabilities = config["capabilities"].as_array().unwrap();
                for capability in ["finalize_outcome_v1", "el_pause_continue_v1"] {
                    assert!(capabilities.iter().any(|value| value == capability));
                }
                ws.send(Message::Text(
                    serde_json::json!({"type":"ready",
                    "session_id":"observer-real", "accepted_capabilities":
                    ["finalize_outcome_v1", "el_pause_continue_v1"]})
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
                loop {
                    tokio::select! {
                        _ = rx.recv() => {
                            match terminal {
                                "close" => { ws.close(None).await.unwrap(); }
                                "error" => { ws.send(Message::Text(serde_json::json!({
                                    "type":"error", "code":"PROVIDER_ERROR", "message":"fixture error"
                                }).to_string().into())).await.unwrap(); }
                                "finalize" => { ws.send(Message::Text(serde_json::json!({
                                    "type":"finalize_complete", "status":"drained", "saw_result":false,
                                    "outcome":{"reason":"drained", "tail_evidence":"unconfirmed",
                                    "provider_release":"released", "last_delivery_seq":0, "stable_snapshot":""}
                                }).to_string().into())).await.unwrap(); }
                                _ => {}
                            }
                            if terminal == "eof" || terminal == "close" { return; }
                            // Keep the socket open: error/finalize must independently terminate.
                            std::future::pending::<()>().await;
                        }
                        message = ws.next() => match message {
                            Some(Ok(Message::Binary(bytes))) => { received.lock().unwrap().extend_from_slice(&bytes); }
                            Some(Ok(_)) => {}
                            _ => return,
                        }
                    }
                }
            });
            let chunks = Arc::new(StdMutex::new(None));
            let service = TranscriptionService::new(
                Box::new(ManualAudioCapture::new(chunks.clone())),
                Arc::new(RealFactory),
            );
            let mut config = SttConfig::new(SttProviderType::Backend);
            config.backend_url = Some(url);
            config.backend_auth_token = Some("local-test".into());
            config.backend_streaming_provider = crate::domain::BackendStreamingProvider::ElevenLabs;
            config.continuation_target_eligible = true;
            service.update_config(config.clone()).await.unwrap();
            let token = service
                .prepare_recording_capture(
                    101,
                    config,
                    Arc::new(|_, _| {}),
                    Arc::new(|_, _| {}),
                    Arc::new(|_| {}),
                )
                .await
                .unwrap();
            service
                .connect_prepared_recording(
                    token,
                    Arc::new(|_| {}),
                    Arc::new(|_| {}),
                    Arc::new(|_, _| {}),
                    Arc::new(|_, _| {}),
                    Arc::new(move |error| observed_errors.lock().unwrap().push(error)),
                    Arc::new(|_, _| {}),
                    Arc::new(AtomicBool::new(false)),
                )
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while service.continuation_session_for_run(101).await.is_none() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert!(!service
                .stt_provider
                .read()
                .await
                .as_ref()
                .unwrap()
                .is_connection_alive());
            for chunk_index in 1..=3 {
                // Old production source fails here, before any terminal stimulus.
                assert!(
                    !service.continuation_observation(101).await.unwrap().2,
                    "Ready active transport must not trigger ObserveTerminal Stop"
                );
                assert!(service.audio_capture.read().await.is_capturing());
                chunks.lock().unwrap().as_ref().unwrap()(AudioChunk::new(vec![200; 480], 16000, 1));
                // Receipt gates the next injection, independent of processor batching/load.
                tokio::time::timeout(Duration::from_secs(2), async {
                    while pcm.lock().unwrap().len() < chunk_index * 960 {
                        assert!(!peer.is_finished(), "peer exited before PCM receipt");
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("peer PCM receipt deadline");
            }
            assert_eq!(pcm.lock().unwrap().len(), 2880);
            assert_eq!(*pcm.lock().unwrap(), 200_i16.to_le_bytes().repeat(1440));
            assert!(!service.continuation_observation(101).await.unwrap().2);
            assert!(service.continuation_observation(100).await.is_none());
            if terminal == "abort" {
                service
                    .stt_provider
                    .write()
                    .await
                    .as_mut()
                    .unwrap()
                    .abort()
                    .await
                    .unwrap();
            } else {
                tx.send(()).await.unwrap();
            }
            tokio::time::timeout(Duration::from_secs(2), async {
                while !service.continuation_observation(101).await.unwrap().2 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            if terminal == "error" || terminal == "finalize" {
                assert!(
                    !peer.is_finished(),
                    "terminal evidence requires a live peer"
                );
                if terminal == "error" {
                    tokio::time::timeout(Duration::from_secs(2), async {
                        while errors.lock().unwrap().is_empty() {
                            assert!(!peer.is_finished());
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .expect("server error callback deadline");
                    let errors = errors.lock().unwrap();
                    assert_eq!(errors.len(), 1);
                    let SttError::Connection(error) = &errors[0] else {
                        panic!("expected server connection error");
                    };
                    assert_eq!(error.message, "fixture error");
                    assert_eq!(error.details.server_code.as_deref(), Some("PROVIDER_ERROR"));
                } else {
                    let provider = service.stt_provider.read().await;
                    let evidence = provider.as_ref().unwrap().finalize_evidence().unwrap();
                    assert_eq!(evidence.reason, crate::domain::FinalizeReason::Drained);
                    assert_eq!(
                        evidence.tail_evidence,
                        crate::domain::TailEvidence::Unconfirmed
                    );
                    assert_eq!(
                        evidence.provider_release,
                        crate::domain::ProviderRelease::Released
                    );
                    assert_eq!(evidence.last_delivery_seq, 0);
                    assert_eq!(evidence.stable_snapshot, "");
                }
                assert!(!peer.is_finished());
            }
            service.stop_capture_for_run(101).await.unwrap();
            service.provider_run_id.store(102, Ordering::Release);
            assert!(service.continuation_observation(101).await.is_none());
            service
                .stt_provider
                .write()
                .await
                .as_mut()
                .unwrap()
                .abort()
                .await
                .unwrap();
            peer.abort();
            match peer.await {
                Ok(()) => {}
                Err(error) => assert!(error.is_cancelled(), "peer panicked: {error}"),
            }
        }
    }

    // E09/E15/E61: only the peer's replies vary; capture, transport, service and
    // coordinator are real. No task owns a service operation: join! polls it inline.
    #[tokio::test]
    async fn real_backend_e09_pending_on_waits_for_pause_receipt() {
        composed_control_case("pause", "direct").await;
    }

    #[tokio::test]
    async fn real_backend_e15_continue_status_uses_current_permission() {
        for answer in ["permit", "finalizing", "unknown", "silent"] {
            composed_control_case("continue", answer).await;
        }
    }

    #[tokio::test]
    async fn real_backend_e61_pause_status_learns_epoch_or_refuses() {
        for answer in ["permit", "finalizing", "unknown", "silent"] {
            composed_control_case("pause", answer).await;
        }
    }

    async fn composed_control_case(operation: &'static str, answer: &'static str) {
        use crate::presentation::recording_intent_coordinator as c;
        use futures_util::{FutureExt, SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        struct RealFactory;
        impl SttProviderFactory for RealFactory {
            fn create(&self, _: &SttConfig) -> SttResult<Box<dyn SttProvider>> {
                Ok(Box::new(
                    crate::infrastructure::stt::BackendProvider::with_continuation_for_test(),
                ))
            }
        }
        #[derive(Default)]
        struct Trace {
            pcm: Vec<u8>,
            controls: Vec<serde_json::Value>,
            queries: Vec<serde_json::Value>,
            configs: usize,
            query_at: Option<Instant>,
            closed: bool,
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let trace = Arc::new(StdMutex::new(Trace::default()));
        let observed = trace.clone();
        let (receipt, mut receipts) = tokio::sync::mpsc::unbounded_channel();
        let (release, mut releases) = tokio::sync::mpsc::unbounded_channel::<&str>();
        let mut peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut delayed = None;
            let mut status = None;
            let mut seq = 0;
            let mut late_barrier = false;
            loop {
                let response = tokio::select! {
                    command = releases.recv() => {
                        let command = command.expect("scenario retains peer control until teardown");
                        late_barrier = command == "late";
                        if command == "barrier" {
                            ws.send(Message::Ping(b"held".to_vec().into())).await.unwrap();
                            None
                        } else if command == "terminal" {
                            Some(serde_json::json!({"type":"finalize_complete","status":"incomplete","saw_result":true,
                                "outcome":{"reason":"provider_error","tail_evidence":"segment_observed",
                                "provider_release":"unconfirmed","last_delivery_seq":1,"stable_snapshot":"late-barrier"}}))
                        } else if late_barrier || answer == "direct" { delayed.clone() }
                        else if answer == "silent" { None } else { status.clone() }
                    }
                    message = ws.next() => {
                        let message = match message {
                            Some(Ok(message)) => message,
                            Some(Err(tokio_tungstenite::tungstenite::Error::Protocol(
                                tokio_tungstenite::tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
                            ))) if answer == "finalizing" => {
                                // Cached terminal finalization hard-drops the socket by design.
                                // EOF follows all received PCM; this peer still exits normally.
                                observed.lock().unwrap().closed = true;
                                break;
                            }
                            other => panic!("expected ordered service teardown: {other:?}"),
                        };
                        match message {
                            Message::Binary(bytes) => {
                                observed.lock().unwrap().pcm.extend_from_slice(&bytes);
                                seq += 1;
                                ws.send(Message::Text(serde_json::json!({"type":"ack","seq":seq}).to_string().into())).await.unwrap();
                                receipt.send("audio").unwrap();
                                None
                            }
                            Message::Text(text) => {
                                let v: serde_json::Value = serde_json::from_str(&text).unwrap();
                                match v["type"].as_str().unwrap() {
                                    "config" => {
                                        observed.lock().unwrap().configs += 1;
                                        assert_eq!(v["provider"], "elevenlabs");
                                        Some(serde_json::json!({"type":"ready","session_id":"composed",
                                            "accepted_capabilities":["finalize_outcome_v1","el_pause_continue_v1"]}))
                                    }
                                    kind @ ("pause" | "continue" | "pause_restore") => {
                                        observed.lock().unwrap().controls.push(v.clone());
                                        let reply = serde_json::json!({
                                            "type": match kind { "pause" => "pause_accepted", "continue" => "continue_result", _ => "pause_restore_result" },
                                            "request_id":v["request_id"],"provider_session_id":"composed",
                                            "pause_epoch":7,"decision":if kind == "pause_restore" {"rejected"} else {"accepted"},
                                            "eligible_now":kind != "pause_restore", "reason":null, "continue_window_ms":5000,
                                            "current_phase":if kind == "pause" {"paused_reclaimable"} else {"active_awaiting_audio"}});
                                        if kind == operation {
                                            delayed = Some(reply);
                                            receipt.send("mutation").unwrap();
                                            None
                                        } else { Some(reply) }
                                    }
                                    "control_status" => {
                                        let mut log = observed.lock().unwrap();
                                        assert_eq!(v["operation_request_id"], log.controls.last().unwrap()["request_id"]);
                                        assert!(v.get("pause_epoch").is_none(), "lookup must need only operation ID");
                                        assert_eq!(log.pcm, 200_i16.to_le_bytes().repeat(480));
                                        log.queries.push(v.clone());
                                        log.query_at = Some(Instant::now());
                                        status = Some(serde_json::json!({"type":"control_status_result",
                                            "query_id":v["query_id"],"operation_request_id":v["operation_request_id"],
                                            "provider_session_id":"composed", "pause_epoch":if answer == "unknown" {None} else {Some(7)},
                                            "original_decision":if answer == "unknown" {None} else {Some("accepted")},
                                            "eligible_now":answer == "permit", "current_phase":if answer == "permit" {
                                                if operation == "pause" {"paused_reclaimable"} else {"active_awaiting_audio"}
                                            } else {"finalizing"}}));
                                        receipt.send("query").unwrap();
                                        None
                                    }
                                    "finalize" => Some(serde_json::json!({"type":"finalize_complete","status":"drained","saw_result":true,
                                        "outcome":{"reason":"drained","tail_evidence":"segment_observed",
                                        "provider_release":"released","last_delivery_seq":1,"stable_snapshot":"retained A"}})),
                                    _ => None,
                                }
                            }
                            Message::Pong(payload) => {
                                assert_eq!(&payload[..], b"held");
                                receipt.send("barrier").unwrap();
                                None
                            }
                            Message::Close(_) => { observed.lock().unwrap().closed = true; break; },
                            _ => None,
                        }
                    }
                };
                if let Some(response) = response {
                    ws.send(Message::Text(response.to_string().into()))
                        .await
                        .unwrap();
                    if late_barrier {
                        ws.send(Message::Text(serde_json::json!({"type":"stable","delivery_seq":1,"text":"late-barrier"}).to_string().into())).await.unwrap();
                        ws.send(Message::Ping(b"held".to_vec().into()))
                            .await
                            .unwrap();
                        late_barrier = false;
                    }
                }
            }
        });
        let mut peer_joined = false;
        let deliveries = Arc::new(StdMutex::new(Vec::new()));
        let delivered = deliveries.clone();
        let chunks = Arc::new(StdMutex::new(None));
        let service = TranscriptionService::new_with_microphone_sensitivity_and_device(
            Box::new(ManualAudioCapture::new(chunks.clone())),
            Arc::new(RealFactory),
            Arc::new(AtomicU8::new(100)),
            Arc::new(StdMutex::new(Some("fixture-device".into()))),
        );
        service.set_continuation_context_guard(Arc::new(ContinuationTestGuard {
            revision: AtomicU64::new(7),
            refuse: AtomicBool::new(false),
        }));
        // Success joins normal teardown; only failure/timeout aborts and joins the peer.
        let result = std::panic::AssertUnwindSafe(tokio::time::timeout(Duration::from_secs(12), async {
            let mut config = SttConfig::new(SttProviderType::Backend);
            config.keep_connection_alive = false;
            config.backend_url = Some(url);
            config.backend_auth_token = Some("local-test".into());
            config.backend_streaming_provider = crate::domain::BackendStreamingProvider::ElevenLabs;
            config.continuation_target_eligible = true;
            service.update_config(config).await.unwrap();
            service.register_continuation_policy(101, &Default::default()).await;
            let mut state = c::CoordinatorState::with_next_run_id_for_test(101);
            let effects = c::reduce(&mut state, c::CoordinatorEvent::Intent(c::RecordingIntent::start(c::IntentSource::Frontend, None)));
            let (prepare, a) = effects.iter().find_map(|e| match e {
                c::CoordinatorEffect::PrepareCapture { effect_id, run } => Some((*effect_id, *run)), _ => None,
            }).unwrap();
            let token_a = service.prepare_recording_capture(101, service.get_config_snapshot(),
                Arc::new(|_, _| {}), Arc::new(|_, _| {}), Arc::new(|_| {})).await.unwrap();
            let effects = c::reduce(&mut state, c::CoordinatorEvent::PrepareFinished {
                effect_id: prepare, run_id: a.run_id, outcome: c::PrepareOutcome::Succeeded { generation: token_a.generation },
            });
            let start = effects.iter().find_map(|e| match e {
                c::CoordinatorEffect::StartRecording { effect_id, .. } => Some(*effect_id), _ => None,
            }).unwrap();
            service.connect_prepared_recording(token_a, Arc::new(|_| {}), Arc::new(move |t| delivered.lock().unwrap().push(t.text)),
                Arc::new(|_, _| {}), Arc::new(|_, _| {}), Arc::new(|_| {}), Arc::new(|_, _| {}), Default::default()).await.unwrap();
            c::reduce(&mut state, c::CoordinatorEvent::StartFinished { effect_id: start, run_id: a.run_id, outcome: c::StartOutcome::Succeeded });
            let session = tokio::time::timeout(Duration::from_secs(2), async {
                loop { if let Some(s) = service.continuation_session_for_run(101).await { break s; } tokio::task::yield_now().await; }
            }).await.unwrap();
            c::reduce(&mut state, c::CoordinatorEvent::Continuation(c::ContinuationEvent::Negotiated {
                logical_run_id: a.run_id, connection_generation: session.connection_generation,
            }));
            chunks.lock().unwrap().as_ref().unwrap()(AudioChunk::new(vec![200; 480], 16000, 1));
            assert_eq!(receipts.recv().await, Some("audio"));
            let stopped = Instant::now();
            let effects = c::reduce(&mut state, c::CoordinatorEvent::Intent(c::RecordingIntent::stop(c::IntentSource::Frontend, None)));
            let stop = effects.iter().find_map(|e| match e { c::CoordinatorEffect::StopRecording { effect_id, .. } => Some(*effect_id), _ => None }).unwrap();
            service.stop_capture_for_run(101).await.unwrap();
            let effects = c::reduce(&mut state, c::CoordinatorEvent::CaptureStopped { effect_id: stop, run_id: a.run_id, outcome: c::CaptureStopOutcome::Inactive });
            let (pause, key) = effects.iter().find_map(|e| match e {
                c::CoordinatorEffect::Continuation(c::ContinuationEffect::Pause { effect_id, key, .. }) => Some((*effect_id, *key)), _ => None,
            }).unwrap();
            let stop_ns = state.continuation.as_ref().unwrap().stopped_at_ns;
            assert!(stop_ns.is_some());
            let control_settled = AtomicBool::new(false);
            // Poll the service first on every barrier wake before checking it is pending.
            let ((paused, pause_settled), (token, b)) = tokio::join!(
                biased;
                async {
                    let result = service.pause_for_continuation(101, stopped).await;
                    control_settled.store(true, Ordering::Release);
                    (result, Instant::now())
                },
                async {
                    if operation == "pause" { assert_eq!(receipts.recv().await, Some("mutation")); }
                    let effects = c::reduce(&mut state, c::CoordinatorEvent::Intent(c::RecordingIntent::start(c::IntentSource::Frontend, None)));
                    let (prepare, b) = effects.iter().find_map(|e| match e {
                        c::CoordinatorEffect::PrepareCapture { effect_id, run } => Some((*effect_id, *run)), _ => None,
                    }).unwrap();
                    let token = prepare_continued_b(&service).await;
                    chunks.lock().unwrap().as_ref().unwrap()(AudioChunk::new(vec![600; 480], 16000, 1));
                    let effects = c::reduce(&mut state, c::CoordinatorEvent::PrepareFinished { effect_id: prepare, run_id: b.run_id,
                        outcome: c::PrepareOutcome::Succeeded { generation: token.generation } });
                    assert!(state.desired_recording.is_on());
                    assert!(matches!(state.capture, c::CaptureState::Buffering { run, .. } if run == b));
                    assert!(!effects.iter().any(|e| matches!(e, c::CoordinatorEffect::StartRecording { .. } |
                        c::CoordinatorEffect::Continuation(c::ContinuationEffect::Continue { .. }))));
                    assert_eq!(service.prepared_capture.lock().await.as_ref().unwrap().queued_bytes.load(Ordering::Acquire), 960);
                    if operation == "pause" {
                        if answer != "direct" { assert_eq!(receipts.recv().await, Some("query")); }
                        release.send(if answer == "direct" { "barrier" } else { "late" }).unwrap();
                        assert_eq!(receipts.recv().await, Some("barrier"));
                        assert!(!control_settled.load(Ordering::Acquire), "Pause settled before status release");
                        if answer != "direct" { assert_eq!(*deliveries.lock().unwrap(), vec!["late-barrier"]); }
                        assert_eq!(service.prepared_capture.lock().await.as_ref().unwrap().queued_bytes.load(Ordering::Acquire), 960);
                        assert_eq!(trace.lock().unwrap().pcm, 200_i16.to_le_bytes().repeat(480));
                        assert_eq!(trace.lock().unwrap().controls.len(), 1);
                        if answer == "direct" { assert!(stopped.elapsed() < Duration::from_millis(750)); }
                        release.send("status").unwrap();
                    }
                    (token, b)
                }
            );
            let mut settled = pause_settled;
            let mut finalize_effects = Vec::new();
            let epoch = paused.as_ref().ok().map(|p| p.pause_epoch);
            let continue_window = paused
                .as_ref()
                .ok()
                .map(|p| p.continue_window)
                .unwrap_or(Duration::from_secs(2));
            let effects = c::reduce(&mut state, c::CoordinatorEvent::Continuation(c::ContinuationEvent::PauseFinished { effect_id: pause, key, pause_epoch: epoch, continue_window }));
            let permits = answer == "direct" || answer == "permit";
            if operation == "pause" && !permits {
                let error = paused.as_ref().unwrap_err();
                if answer == "finalizing" {
                    assert_eq!(error.to_string(), "Pause was not currently reclaimable");
                } else {
                    let expected = if answer == "unknown" { "Continuation decision unknown" }
                        else { "Continuation decision unknown after bounded status" };
                    assert!(matches!(error.downcast_ref::<SttError>(), Some(SttError::Processing(message)) if message == expected), "{error:#}");
                }
                finalize_effects.extend(effects.iter().cloned());
                assert!(service.paused_continuation_snapshot().await.is_none());
                assert!(state.desired_recording.is_on());
                assert!(!effects.iter().any(|e| matches!(e, c::CoordinatorEffect::StartRecording { .. } | c::CoordinatorEffect::Continuation(c::ContinuationEffect::Continue { .. }))));
            } else {
                let lease = paused.unwrap();
                assert_eq!(lease.session, session);
                assert_eq!(lease.pause_epoch, 7);
                assert_eq!(lease.stopped_at, stopped);
                let expected_window = if operation == "continue" || answer == "direct" {
                    Duration::from_secs(5)
                } else {
                    // A lost PauseAccepted recovered only through legacy status
                    // metadata cannot safely infer the newer server window.
                    Duration::from_secs(2)
                };
                assert_eq!(lease.deadline(), stopped + expected_window);
                assert_eq!(state.continuation.as_ref().unwrap().stopped_at_ns, stop_ns);
                let (attach, key) = effects.iter().find_map(|e| match e {
                    c::CoordinatorEffect::Continuation(c::ContinuationEffect::Continue { effect_id, key, .. }) => Some((*effect_id, *key)), _ => None,
                }).unwrap();
                assert_eq!(key.pause_epoch, 7);
                control_settled.store(false, Ordering::Release);
                let ((outcome, continue_settled), ()) = tokio::join!(
                    biased;
                    async {
                        let result = service.continue_prepared_capture(token, lease, Instant::now(), Default::default()).await;
                        control_settled.store(true, Ordering::Release);
                        (result, Instant::now())
                    },
                    async {
                        if operation == "continue" {
                            assert_eq!(receipts.recv().await, Some("mutation"));
                            assert_eq!(receipts.recv().await, Some("query"));
                            release.send("late").unwrap();
                            assert_eq!(receipts.recv().await, Some("barrier"));
                            assert!(!control_settled.load(Ordering::Acquire), "Continue settled before status release");
                            assert_eq!(*deliveries.lock().unwrap(), vec!["late-barrier"]);
                            assert_eq!(trace.lock().unwrap().controls.len(), 2);
                            assert_eq!(service.prepared_capture.lock().await.as_ref().unwrap().queued_bytes.load(Ordering::Acquire), 960);
                            assert_eq!(trace.lock().unwrap().pcm, 200_i16.to_le_bytes().repeat(480));
                            release.send("status").unwrap();
                        }
                    }
                );
                settled = continue_settled;
                assert!(settled <= stopped + Duration::from_millis(2100), "absolute lease + 100ms scheduling tolerance");
                let outcome = outcome.unwrap();
                let event_outcome = match outcome {
                    ContinueCaptureOutcome::Attached { logical_run_id, context_revision } => {
                        assert!(permits); assert_eq!(logical_run_id, 101);
                        c::ContinueAttachOutcome::Attached { context_revision }
                    }
                    ContinueCaptureOutcome::Unsent(ref refusal) => {
                        assert!(!permits);
                        assert_eq!(*refusal, if answer == "silent" || answer == "unknown" { ContinuationRefusal::ControlUnknown } else { ContinuationRefusal::Rejected });
                        c::ContinueAttachOutcome::Unsent
                    }
                    other => panic!("unexpected service result: {other:?}"),
                };
                let effects = c::reduce(&mut state, c::CoordinatorEvent::Continuation(c::ContinuationEvent::ContinueFinished {
                    effect_id: attach, key, run_id: b.run_id, generation: token.generation, outcome: event_outcome,
                }));
                assert!(!effects.iter().any(|e| matches!(e, c::CoordinatorEffect::StartRecording { .. })));
                if permits {
                    assert!(matches!(state.capture, c::CaptureState::Recording { run } if run == b));
                    assert_eq!(receipts.recv().await, Some("audio"));
                } else {
                    assert!(state.desired_recording.is_on());
                    assert!(matches!(state.capture, c::CaptureState::Buffering { run, .. } if run == b));
                }
            }
            // Peer receipt timestamps bound the actual recovery, not merely total Stop time.
            assert!(settled <= stopped + Duration::from_millis(2100));
            if let Some(query_at) = trace.lock().unwrap().query_at {
                let recovery = settled.duration_since(query_at);
                assert!(recovery <= Duration::from_millis(600), "500ms recovery + 100ms scheduling tolerance");
                if answer == "silent" { assert!(recovery >= Duration::from_millis(450)); }
                else { assert!(recovery < Duration::from_millis(450), "answered status must beat timeout"); }
            }
            if !permits {
                assert!(state.desired_recording.is_on());
                assert_eq!(service.prepared_capture.lock().await.as_ref().unwrap().queued_bytes.load(Ordering::Acquire), 960);
                let audio = service.active_audio.read().await;
                let (owner, accounting) = audio.as_ref().unwrap();
                assert_eq!(*owner, 101);
                let retained = accounting.report(AudioDrainReason::Drained);
                assert_eq!((retained.accepted_bytes, retained.submitted_bytes, retained.unknown_bytes), (960, 960, 0));
            }
            if permits {
                service.stop_capture_for_run(102).await.unwrap();
            } else {
                // A wrong-phase reply is a refusal, not itself terminal evidence.
                if answer == "finalizing" { release.send("terminal").unwrap(); }
                let observation = loop {
                    let observation = service.continuation_observation(101).await.unwrap();
                    if observation.2 { break observation; }
                    tokio::task::yield_now().await;
                };
                assert_eq!(observation.0, session);
                finalize_effects.extend(c::reduce(&mut state, c::CoordinatorEvent::Continuation(c::ContinuationEvent::TerminalObserved {
                    logical_run_id: a.run_id, connection_generation: observation.0.connection_generation,
                })));
                assert!(!finalize_effects.iter().any(|e| matches!(e, c::CoordinatorEffect::StartRecording { .. })));
            }
            assert_eq!(service.drain_audio_processor_task("composed final snapshot", Duration::from_secs(2)).await, AudioDrainReason::Drained);
            assert!(service.audio_processor_task.read().await.is_none());
            let finalizers: Vec<_> = finalize_effects.iter().filter_map(|e| match e {
                c::CoordinatorEffect::FinalizeRecording { effect_id, run_id, .. } => {
                    assert_eq!(*run_id, a.run_id); Some(*effect_id)
                }, _ => None,
            }).collect();
            assert_eq!(finalizers.len(), usize::from(!permits));
            let finalized = service.finalize_provider_for_run(101).await;
            let report = service.completed_report_for_run(101).await.unwrap();
            assert_eq!(report.run_id, 101);
            assert_eq!(report.audio.reason, AudioDrainReason::Drained);
            assert_eq!(report.audio.remaining_bytes, 0);
            assert_eq!(report.audio.acknowledged_bytes, Some(if permits {1920} else {960}));
            assert_eq!((report.audio.accepted_bytes, report.audio.submitted_bytes, report.audio.unknown_bytes),
                (if permits {1920} else {960}, if permits {1920} else {960}, 0));
            if permits {
                finalized.unwrap();
                assert_eq!(report.provider_release, crate::domain::ProviderRelease::Released);
            } else {
                assert!(finalized.is_err());
                assert!(report.error.is_some());
                assert_eq!(report.provider_release, crate::domain::ProviderRelease::Unconfirmed);
                if answer == "finalizing" {
                    assert_eq!(report.provider.as_ref().unwrap().stable_snapshot, "late-barrier");
                } else { assert!(report.provider.is_none()); }
                let effects = c::reduce(&mut state, c::CoordinatorEvent::FinalizeFinished {
                    effect_id: finalizers[0], run_id: a.run_id,
                    outcome: c::FinalizeOutcome::ReleaseUnconfirmed(c::ErrorCode(61)),
                });
                assert!(!effects.iter().any(|e| matches!(e, c::CoordinatorEffect::StartRecording { .. })));
                let stop_b = effects.iter().find_map(|e| match e {
                    c::CoordinatorEffect::Continuation(c::ContinuationEffect::SealPending { effect_id, run_id, generation, cancel }) if *run_id == b.run_id => {
                        assert_eq!(*generation, token.generation); assert!(*cancel); Some(*effect_id)
                    }, _ => None,
                }).expect("unconfirmed release stops retained B");
                service.cancel_prepared_capture(token).await.unwrap();
                assert!(!service.capture_is_active_for_run(102).await);
                let effects = c::reduce(&mut state, c::CoordinatorEvent::Continuation(c::ContinuationEvent::PendingCaptureStopped {
                    effect_id: stop_b, run_id: b.run_id, generation: token.generation, outcome: c::CaptureStopOutcome::Inactive,
                }));
                assert!(matches!(state.capture, c::CaptureState::Idle));
                assert!(!state.desired_recording.is_on());
                assert!(!effects.iter().any(|e| matches!(e, c::CoordinatorEffect::StartRecording { .. })));
            }
            // Close/terminal EOF is consumed in the same peer loop as PCM after processor join.
            // No final snapshot can hide outbound bytes behind a sleep or peer abort.
            let joined = (&mut peer).await;
            peer_joined = true;
            joined.expect("normal peer teardown");
            let retained = service.completed_report_for_run(101).await.unwrap();
            assert_eq!(retained.audio.submitted_bytes, report.audio.submitted_bytes);
            assert_eq!(retained.provider_release, report.provider_release);
            assert!(service.stt_provider.read().await.is_none());
            assert!(trace.lock().unwrap().closed);
            assert!(service.prepared_capture.lock().await.is_none());
            assert!(!service.audio_capture.read().await.is_capturing());
            assert!(state.validate().is_ok());
            let log = trace.lock().unwrap();
            let mut expected = 200_i16.to_le_bytes().repeat(480);
            if permits { expected.extend(600_i16.to_le_bytes().repeat(480)); }
            assert_eq!(log.pcm, expected, "{operation}/{answer}: no B without permission or replay");
            assert_eq!(log.configs, 1);
            assert_eq!(log.queries.len(), usize::from(answer != "direct"));
            assert_eq!(log.controls.iter().filter(|v| v["type"] == "pause").count(), 1);
            let continues: Vec<_> = log.controls.iter().filter(|v| v["type"] == "continue").collect();
            assert_eq!(continues.len(), usize::from(operation == "continue" || permits));
            for control in continues { assert_eq!(control["pause_epoch"], 7); }
        })).catch_unwind().await;
        // Emergency cleanup only; successful cleanup was asserted inside the scenario.
        if matches!(&result, Ok(Ok(()))) {
            return;
        }
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            let token = service
                .prepared_capture
                .lock()
                .await
                .as_ref()
                .map(|p| p.token);
            if let Some(token) = token {
                let _ = service.cancel_prepared_capture(token).await;
            }
        })
        .await;
        let _ =
            tokio::time::timeout(Duration::from_secs(2), service.stop_capture_for_run(101)).await;
        let _ =
            tokio::time::timeout(Duration::from_secs(2), service.stop_capture_for_run(102)).await;
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            if let Some(provider) = service.stt_provider.write().await.as_mut() {
                let _ = provider.abort().await;
            }
        })
        .await;
        if !peer_joined {
            peer.abort();
            let joined = peer.await;
            if let Err(error) = joined {
                assert!(error.is_cancelled(), "peer failed: {error}");
            }
        }
        match result {
            Err(panic) => std::panic::resume_unwind(panic),
            Ok(result) => result.expect("composed service deadline"),
        }
    }

    async fn prepare_continued_b(service: &TranscriptionService) -> PreparedCaptureToken {
        service
            .register_continuation_policy(102, &crate::domain::AppConfig::default())
            .await;
        service
            .prepare_recording_capture(
                102,
                service.get_config_snapshot(),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_| {}),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn frozen_eligible_run_continues_with_unqualified_persisted_settings() {
        let (service, _, _, _) = continuation_service_fixture(false, 0, None).await;
        service.stop_capture_for_run(101).await.unwrap();
        let paused = service
            .pause_for_continuation(101, Instant::now())
            .await
            .unwrap();
        let token = prepare_continued_b(&service).await;
        service.seal_prepared_capture(token).await.unwrap();
        // Persisted/user settings cannot carry native target eligibility. Both
        // prepared episodes retain their independently frozen qualified target.
        let mut settings = service.get_config_snapshot();
        settings.continuation_target_eligible = false;
        service.update_config(settings).await.unwrap();
        assert!(matches!(
            service
                .continue_prepared_capture(token, paused, Instant::now(), Default::default())
                .await
                .unwrap(),
            ContinueCaptureOutcome::Attached { .. }
        ));
        service.finalize_provider_for_run(101).await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn unknown_control_remains_observable_and_freezes_unconfirmed_terminal_without_cold_replay(
    ) {
        for released in [false, true] {
            let (service, log, _, _) = continuation_service_fixture(false, 4, None).await;
            service.stop_capture_for_run(101).await.unwrap();
            let paused = service
                .pause_for_continuation(101, Instant::now())
                .await
                .unwrap();
            let token = prepare_continued_b(&service).await;
            service.seal_prepared_capture(token).await.unwrap();
            assert_eq!(
                service
                    .continue_prepared_capture(token, paused, Instant::now(), Default::default())
                    .await
                    .unwrap(),
                ContinueCaptureOutcome::Unsent(ContinuationRefusal::ControlUnknown)
            );
            assert!(service.continuation_observation(101).await.unwrap().2);
            if released {
                log.lock().unwrap().explicit_report = Some(crate::domain::ProviderFinalizeReport {
                    reason: crate::domain::FinalizeReason::ProviderError,
                    tail_evidence: crate::domain::TailEvidence::SegmentObserved,
                    provider_release: crate::domain::ProviderRelease::Released,
                    last_delivery_seq: 1,
                    stable_snapshot: "retained A".into(),
                    error: Some("control unknown, release confirmed".into()),
                });
            }
            let _ = service.finalize_provider_for_run(101).await;
            let report = service.completed_report_for_run(101).await.unwrap();
            assert_eq!(
                report.provider_release,
                if released {
                    crate::domain::ProviderRelease::Released
                } else {
                    crate::domain::ProviderRelease::Unconfirmed
                }
            );
            if released {
                assert!(report.provider.as_ref().unwrap().error.is_some());
            } else {
                assert!(report.error.is_some());
            }
            assert_eq!(log.lock().unwrap().starts, 1);
            assert_eq!(log.lock().unwrap().samples.len(), 480);
            if released {
                assert_eq!(service.logical_provider_run_id(), 0);
                assert!(service.stt_provider.read().await.is_none());
                log.lock().unwrap().explicit_report = None;
                service
                    .connect_prepared_recording(
                        token,
                        Arc::new(|_| {}),
                        Arc::new(|_| {}),
                        Arc::new(|_, _| {}),
                        Arc::new(|_, _| {}),
                        Arc::new(|_| {}),
                        Arc::new(|_, _| {}),
                        Default::default(),
                    )
                    .await
                    .unwrap();
                service.finalize_provider_for_run(102).await.unwrap();
                assert_eq!(log.lock().unwrap().starts, 2);
                assert_eq!(log.lock().unwrap().samples, vec![1200; 960]);
                assert!(service
                    .connect_prepared_recording(
                        token,
                        Arc::new(|_| {}),
                        Arc::new(|_| {}),
                        Arc::new(|_, _| {}),
                        Arc::new(|_, _| {}),
                        Arc::new(|_| {}),
                        Arc::new(|_, _| {}),
                        Default::default()
                    )
                    .await
                    .is_err());
                assert_eq!(log.lock().unwrap().starts, 2);
            } else {
                service.cancel_prepared_capture(token).await.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn grace_close_and_session_limit_preserve_stable_without_continue_or_reconnect() {
        for category in [
            SttConnectionCategory::Closed,
            SttConnectionCategory::ServerError,
            SttConnectionCategory::LimitExceeded,
        ] {
            let (service, log, _, _) = continuation_service_fixture(false, 0, None).await;
            service.stop_capture_for_run(101).await.unwrap();
            let paused = service
                .pause_for_continuation(101, Instant::now())
                .await
                .unwrap();
            log.lock().unwrap().explicit_report = Some(crate::domain::ProviderFinalizeReport {
                reason: crate::domain::FinalizeReason::ProviderError,
                tail_evidence: crate::domain::TailEvidence::SegmentObserved,
                provider_release: crate::domain::ProviderRelease::Released,
                last_delivery_seq: 1,
                stable_snapshot: "stable before close".into(),
                error: Some(format!("{category:?}")),
            });
            let callback = log.lock().unwrap().error_callback.clone().unwrap();
            callback(SttError::Connection(
                crate::domain::SttConnectionError::with_category("grace close", category),
            ));
            tokio::time::timeout(Duration::from_secs(1), async {
                while service.completed_report_for_run(101).await.is_none() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let report = service.completed_report_for_run(101).await.unwrap();
            assert_eq!(
                report.provider.as_ref().unwrap().stable_snapshot,
                "stable before close"
            );
            assert_eq!(
                report.provider_release,
                crate::domain::ProviderRelease::Released
            );
            assert!(service.continuation_session_for_run(101).await.is_none());
            let token = prepare_continued_b(&service).await;
            assert!(matches!(
                service
                    .continue_prepared_capture(token, paused, Instant::now(), Default::default())
                    .await
                    .unwrap(),
                ContinueCaptureOutcome::Unsent(_)
            ));
            service.cancel_prepared_capture(token).await.unwrap();
            service
                .cleanup_runtime_failure_for_run(101, "duplicate close")
                .await;
            assert_eq!(log.lock().unwrap().aborts, 1);
            assert_eq!(log.lock().unwrap().starts, 1);
            assert_eq!(log.lock().unwrap().operations.len(), 1);
            assert_eq!(log.lock().unwrap().samples.len(), 480);
            assert!(!service.audio_capture.read().await.is_capturing());
            assert_eq!(
                service
                    .completed_report_for_run(101)
                    .await
                    .unwrap()
                    .provider,
                report.provider
            );
        }
    }

    #[tokio::test]
    async fn logout_during_grace_refuses_old_account_continuation_and_clears_pause_on_finalize() {
        for account in [None, Some("replacement-account".to_owned())] {
            let (service, log, _, _) = continuation_service_fixture(false, 0, None).await;
            service.stop_capture_for_run(101).await.unwrap();
            let paused = service
                .pause_for_continuation(101, Instant::now())
                .await
                .unwrap();
            let mut config = service.get_config_snapshot();
            config.backend_auth_token = account;
            service.update_config(config).await.unwrap();
            let token = prepare_continued_b(&service).await;
            assert_eq!(
                service
                    .continue_prepared_capture(token, paused, Instant::now(), Default::default())
                    .await
                    .unwrap(),
                ContinueCaptureOutcome::Unsent(ContinuationRefusal::ConfigChanged)
            );
            assert_eq!(log.lock().unwrap().operations.len(), 1);
            assert!(!service.can_resume_keep_alive_connection().await);
            // The service owns an adapter, not the platform's account/context cache.
            // Config fencing must win even when that adapter still reports Valid.
            assert!(matches!(
                service.validate_continuation_context(101).await,
                crate::domain::ContextValidation::Valid { revision: 7 }
            ));
            assert_eq!(log.lock().unwrap().samples, vec![1200; 480]);
            service.cancel_prepared_capture(token).await.unwrap();
            service.finalize_provider_for_run(101).await.unwrap();
            assert!(service.paused_continuation_snapshot().await.is_none());
            assert!(service.continuation_session_for_run(101).await.is_none());
            assert_eq!(service.logical_provider_run_id(), 0);
            assert!(service.continuation_observation(101).await.is_none());
            assert!(!service.can_resume_keep_alive_connection().await);
            assert!(service.stt_provider.read().await.is_none());
            assert!(service.prepared_capture.lock().await.is_none());
            assert!(!service.audio_capture.read().await.is_capturing());
            assert_eq!(log.lock().unwrap().starts, 1);
        }
    }

    #[tokio::test]
    async fn lost_b_audio_ack_is_unacknowledged_without_replay_and_retains_stable() {
        let (service, log, _, _) = continuation_service_fixture(false, 6, None).await;
        service.stop_capture_for_run(101).await.unwrap();
        let paused = service
            .pause_for_continuation(101, Instant::now())
            .await
            .unwrap();
        let token = prepare_continued_b(&service).await;
        service.seal_prepared_capture(token).await.unwrap();
        assert!(matches!(
            service
                .continue_prepared_capture(token, paused, Instant::now(), Default::default())
                .await
                .unwrap(),
            ContinueCaptureOutcome::Attached { .. }
        ));
        let _ = service.finalize_provider_for_run(101).await;
        let report = service.completed_report_for_run(101).await.unwrap();
        assert_eq!(report.audio.acknowledged_bytes, Some(960));
        assert_eq!(report.audio.unacknowledged_bytes, Some(960));
        assert_eq!(
            report.provider.as_ref().unwrap().stable_snapshot,
            "A late A"
        );
        assert_eq!(log.lock().unwrap().samples, vec![1200; 960]);
        assert_eq!(log.lock().unwrap().starts, 1);
        assert_eq!(log.lock().unwrap().operations.len(), 2);
        assert!(service.prepared_capture.lock().await.is_none());
        assert!(!service.audio_capture.read().await.is_capturing());
    }

    #[tokio::test]
    async fn negotiated_provider_error_before_first_pause_retains_report() {
        let (service, log, _, _) = continuation_service_fixture(false, 0, None).await;
        assert!(service.continuation_session_for_run(101).await.is_some());
        assert_eq!(service.continuation_owner.load(Ordering::Acquire), 0);
        let callback = log.lock().unwrap().error_callback.clone().unwrap();
        callback(SttError::Processing("before first Pause".into()));
        tokio::time::timeout(Duration::from_secs(2), async {
            while service.completed_report_for_run(101).await.is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let report = service.completed_report_for_run(101).await.unwrap();
        assert_eq!(report.run_id, 101);
        assert!(report.error.is_some());
        assert_eq!(
            report.provider_release,
            crate::domain::ProviderRelease::Unconfirmed
        );
        assert!(!service.audio_capture.read().await.is_capturing());
        assert!(!service
            .finalize_provider_for_run(101)
            .await
            .unwrap_err()
            .to_string()
            .contains("owned by run 0"));
        assert_eq!(log.lock().unwrap().aborts, 1);
    }

    #[tokio::test]
    async fn pre_pause_error_with_explicit_server_report_releases_admission() {
        let (service, log, _, _) = continuation_service_fixture(false, 0, None).await;
        log.lock().unwrap().explicit_report = Some(crate::domain::ProviderFinalizeReport {
            reason: crate::domain::FinalizeReason::ProviderError,
            tail_evidence: crate::domain::TailEvidence::Unconfirmed,
            provider_release: crate::domain::ProviderRelease::Released,
            last_delivery_seq: 1,
            stable_snapshot: "retained server text".into(),
            error: Some("server error with release confirmation".into()),
        });
        service.cleanup_runtime_failure_for_run(101, "error").await;
        let report = service.completed_report_for_run(101).await.unwrap();
        assert_eq!(
            report.provider_release,
            crate::domain::ProviderRelease::Released
        );
        assert_eq!(
            report.provider.unwrap().stable_snapshot,
            "retained server text"
        );
        assert_eq!(service.logical_provider_run_id(), 0);
        assert_eq!(log.lock().unwrap().aborts, 1);
        use crate::presentation::recording_intent_coordinator::*;
        let run = RunContext {
            run_id: RunId::new(101),
            revision: IntentRevision::new(1),
            source: IntentSource::Frontend,
            policy: RuntimePolicySnapshot::default(),
        };
        let mut coordinator = CoordinatorState::default();
        coordinator.capture = CaptureState::Recording { run };
        let failure = crate::presentation::commands::runtime_failure::runtime_failure_event(
            &service,
            101,
            ErrorCode(8),
        )
        .await;
        let effects = reduce(&mut coordinator, failure);
        let stop = effects
            .iter()
            .find_map(|e| match e {
                CoordinatorEffect::StopRecording { effect_id, .. } => Some(*effect_id),
                _ => None,
            })
            .unwrap();
        let effects = reduce(
            &mut coordinator,
            CoordinatorEvent::CaptureStopped {
                effect_id: stop,
                run_id: run.run_id,
                outcome: CaptureStopOutcome::Inactive,
            },
        );
        let finalize = effects
            .iter()
            .find_map(|e| match e {
                CoordinatorEffect::FinalizeRecording { effect_id, .. } => Some(*effect_id),
                _ => None,
            })
            .unwrap();
        reduce(
            &mut coordinator,
            CoordinatorEvent::FinalizeFinished {
                effect_id: finalize,
                run_id: run.run_id,
                outcome: FinalizeOutcome::FailedReleased(ErrorCode(8)),
            },
        );
        assert!(coordinator.processing_jobs.is_empty());
        let effects = reduce(
            &mut coordinator,
            CoordinatorEvent::Intent(RecordingIntent::start(IntentSource::Frontend, None)),
        );
        assert!(effects
            .iter()
            .any(|e| matches!(e, CoordinatorEffect::PrepareCapture { .. })));
        assert!(coordinator.fault.is_none());
        let c = service
            .prepare_recording_capture(
                103,
                service.get_config_snapshot(),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_| {}),
            )
            .await
            .unwrap();
        service
            .connect_prepared_recording(
                c,
                Arc::new(|_| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
                Arc::new(AtomicBool::new(false)),
            )
            .await
            .unwrap();
        assert_eq!(service.logical_provider_run_id(), 103);
        assert_eq!(service.continuation_owner.load(Ordering::Acquire), 0);
        assert_eq!(log.lock().unwrap().starts, 2);
        service
            .cleanup_runtime_failure_for_run(103, "fixture cleanup")
            .await;
    }

    #[tokio::test]
    async fn ready_error_orders_publish_cached_terminal_once_and_fence_late_ready() {
        use crate::presentation::commands::runtime_failure as lifecycle;
        use crate::presentation::recording_intent_coordinator::*;
        for (ready_first, retire_before_ready) in [(false, false), (false, true), (true, false)] {
            let (service, log, _, _) = continuation_service_fixture(false, 0, None).await;
            let run = RunContext {
                run_id: RunId::new(101),
                revision: IntentRevision::new(1),
                source: IntentSource::Frontend,
                policy: RuntimePolicySnapshot::default(),
            };
            let mut coordinator = CoordinatorState::default();
            coordinator.capture = CaptureState::Recording { run };
            assert!(coordinator.register_native_candidate(run.run_id));
            let delivery = Arc::new(StdMutex::new(
                crate::domain::ContinuationDeliveryOwnership::default(),
            ));
            let read = Arc::new(tokio::sync::Barrier::new(2));
            let resume = Arc::new(tokio::sync::Barrier::new(2));
            let ready = {
                let service = service.clone();
                let delivery = delivery.clone();
                let read = read.clone();
                let resume = resume.clone();
                tokio::spawn(async move {
                    let session = service.continuation_session_for_run(101).await.unwrap();
                    read.wait().await;
                    resume.wait().await;
                    let accepted = delivery.lock().unwrap().accept(101, true);
                    (
                        accepted,
                        CoordinatorEvent::Continuation(ContinuationEvent::Negotiated {
                            logical_run_id: RunId::new(101),
                            connection_generation: session.connection_generation,
                        }),
                    )
                })
            };
            read.wait().await; // actual live Ready snapshot, before error teardown
            let mut ready = Some(ready);
            let mut issued_effects = Vec::new();
            if ready_first {
                resume.wait().await;
                let (accepted, event) = ready.take().unwrap().await.unwrap();
                assert!(accepted);
                issued_effects.extend(reduce(&mut coordinator, event));
            }
            let callback = log.lock().unwrap().error_callback.clone().unwrap();
            callback(SttError::Processing("Ready race".into()));
            tokio::time::timeout(Duration::from_secs(2), async {
                while service.completed_report_for_run(101).await.is_none() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let failure = lifecycle::runtime_failure_event(&service, 101, ErrorCode(8)).await;
            assert!(matches!(
                failure,
                CoordinatorEvent::NegotiatedRuntimeFailed { .. }
            ));
            issued_effects.extend(reduce(&mut coordinator, failure));
            // Ready may already have issued the physical Stop. Runtime failure
            // transfers finalization to that exact owner instead of replacing it.
            assert_eq!(
                issued_effects
                    .iter()
                    .filter(|effect| matches!(effect, CoordinatorEffect::StopRecording { .. }))
                    .count(),
                1
            );
            let stop = issued_effects
                .iter()
                .find_map(|e| match e {
                    CoordinatorEffect::StopRecording {
                        effect_id, run_id, ..
                    } if *run_id == run.run_id => Some(*effect_id),
                    _ => None,
                })
                .unwrap();
            assert!(!reduce(&mut coordinator, failure).iter().any(|e| matches!(
                e,
                CoordinatorEffect::StopRecording { .. }
                    | CoordinatorEffect::FinalizeRecording { .. }
            )));
            let effects = reduce(
                &mut coordinator,
                CoordinatorEvent::CaptureStopped {
                    effect_id: stop,
                    run_id: run.run_id,
                    outcome: CaptureStopOutcome::Inactive,
                },
            );
            assert_eq!(
                effects
                    .iter()
                    .filter(|e| matches!(e, CoordinatorEffect::FinalizeRecording { .. }))
                    .count(),
                1
            );
            let finalize = effects
                .iter()
                .find_map(|e| match e {
                    CoordinatorEffect::FinalizeRecording { effect_id, .. } => Some(*effect_id),
                    _ => None,
                })
                .unwrap();
            // The existing FinalizeRecording executor calls these exact service
            // and transcript barrier transitions; no Tauri event sink is needed.
            let frozen = service.completed_report_for_run(101).await.unwrap();
            assert!(service.finalize_provider_for_run(101).await.is_err());
            assert_eq!(
                service
                    .completed_report_for_run(101)
                    .await
                    .unwrap()
                    .provider_release,
                frozen.provider_release
            );
            let mut barrier = lifecycle::RunTranscriptDelivery::default();
            let mut stable = crate::domain::Transcription::new("stable before error".into(), true);
            stable.delivery_seq = Some(1);
            assert!(barrier.accept_stable(&stable));
            assert_eq!(barrier.close().as_deref(), Some("stable before error"));
            stable.delivery_seq = Some(2);
            assert!(!barrier.accept_stable(&stable));
            delivery.lock().unwrap().terminal(101);
            assert!(barrier.close().is_none());
            if retire_before_ready {
                assert!(lifecycle::finish_registration(
                    &mut coordinator,
                    &mut delivery.lock().unwrap(),
                    run.run_id
                ));
            }
            if !ready_first {
                // Resume either immediately after terminal publication or after
                // retirement's decision but before its native release executes.
                resume.wait().await;
                let (accepted, stale) = ready.take().unwrap().await.unwrap();
                assert!(!accepted);
                assert!(reduce(&mut coordinator, stale).is_empty());
            } else {
                assert!(delivery.lock().unwrap().contains(&101));
                assert!(delivery.lock().unwrap().settle(101, 1));
                assert!(!delivery.lock().unwrap().settle(101, 1));
            }
            if !retire_before_ready {
                // An accepted owner still belongs to ACK (settled above); the
                // unaccepted run can now retire without awaiting an impossible ACK.
                assert!(lifecycle::finish_registration(
                    &mut coordinator,
                    &mut delivery.lock().unwrap(),
                    run.run_id
                ));
            }
            reduce(
                &mut coordinator,
                CoordinatorEvent::FinalizeFinished {
                    effect_id: finalize,
                    run_id: run.run_id,
                    outcome: FinalizeOutcome::ReleaseUnconfirmed(ErrorCode(8)),
                },
            );
            assert!(!reduce(&mut coordinator, failure).iter().any(|e| matches!(
                e,
                CoordinatorEffect::StopRecording { .. }
                    | CoordinatorEffect::FinalizeRecording { .. }
            )));
            service
                .cleanup_runtime_failure_for_run(101, "duplicate error")
                .await;
            assert_eq!(service.logical_provider_run_id(), 101);
            assert_eq!(log.lock().unwrap().aborts, 1);
            assert_eq!(
                frozen.provider_release,
                crate::domain::ProviderRelease::Unconfirmed
            );
            // Fresh C and repeated gestures use the production reducer. No
            // capture/start effect may reach the production async executor.
            let generation = service.capture_generation.load(Ordering::Acquire);
            let samples = log.lock().unwrap().samples.len();
            for gesture in 1..=12 {
                for intent in [
                    RecordingIntent::start(IntentSource::Frontend, None),
                    RecordingIntent::start(IntentSource::HoldHotkey, Some(GestureId::new(gesture))),
                    RecordingIntent::stop(IntentSource::HoldHotkey, Some(GestureId::new(gesture))),
                    RecordingIntent::stop(IntentSource::Frontend, None),
                    RecordingIntent::toggle(IntentSource::CarbonHotkey, GestureId::new(gesture)),
                ] {
                    let effects = reduce(&mut coordinator, CoordinatorEvent::Intent(intent));
                    assert!(
                        effects.iter().all(|effect| matches!(
                            effect,
                            CoordinatorEffect::EmitProjection(_)
                                | CoordinatorEffect::HidePanel { .. }
                        )),
                        "unexpected effect: {effects:?}"
                    );
                    assert!(matches!(coordinator.capture, CaptureState::Idle));
                    assert!(!coordinator.desired_recording.is_on());
                    let projection = coordinator.projection();
                    assert_eq!(projection.status, ProjectionStatus::Error);
                    assert_eq!(projection.fault, Some(ProjectionFault::FinalizeFailed));
                    assert_eq!(projection.fault_run_id, Some(run.run_id));
                    assert!(!projection.pending_start);
                    if intent.kind != IntentKind::Stop {
                        assert!(effects.iter().any(|effect| matches!(effect,
                            CoordinatorEffect::EmitProjection(p) if p.status == ProjectionStatus::Error
                        )));
                    }
                    assert_eq!(coordinator.processing_jobs.len(), 1);
                    assert!(service.prepared_capture.lock().await.is_none());
                    assert!(!service.audio_capture.read().await.is_capturing());
                    assert_eq!(
                        service.capture_generation.load(Ordering::Acquire),
                        generation
                    );
                    assert_eq!(log.lock().unwrap().starts, 1);
                    assert_eq!(log.lock().unwrap().samples.len(), samples);
                }
            }
            let b = service
                .prepare_recording_capture(
                    102,
                    service.get_config_snapshot(),
                    Arc::new(|_, _| {}),
                    Arc::new(|_, _| {}),
                    Arc::new(|_| {}),
                )
                .await
                .unwrap();
            assert!(service
                .connect_prepared_recording(
                    b,
                    Arc::new(|_| {}),
                    Arc::new(|_| {}),
                    Arc::new(|_, _| {}),
                    Arc::new(|_, _| {}),
                    Arc::new(|_| {}),
                    Arc::new(|_, _| {}),
                    Arc::new(AtomicBool::new(false))
                )
                .await
                .is_err());
            assert_eq!(log.lock().unwrap().starts, 1);
            service.cancel_prepared_capture(b).await.unwrap();
        }
    }

    #[tokio::test]
    async fn native_b_error_dispatch_resolves_a_and_ignores_duplicate_and_detached_b() {
        let (service, log, _, _) = continuation_service_fixture(false, 0, None).await;
        service.stop_capture_for_run(101).await.unwrap();
        let paused = service
            .pause_for_continuation(101, Instant::now())
            .await
            .unwrap();
        let native_callback = Arc::new(StdMutex::new(None));
        let capture = crate::presentation::state::effective_capture::EffectiveCapture::new(
            ErrorInjectCapture {
                inner: ImmediateAudioCapture::new(),
                callback: native_callback.clone(),
                fail_during_start: false,
            },
            service.effective_capture_device_source(),
            |_| Some("fixture microphone".into()),
        );
        service
            .replace_audio_capture(Box::new(capture))
            .await
            .unwrap();
        service
            .register_continuation_policy(102, &crate::domain::AppConfig::default())
            .await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let weak = Arc::downgrade(&service);
        let on_error = crate::presentation::commands::runtime_failure::capture_error_callback(
            102,
            move |source, err| {
                let weak = weak.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    // Actual command callback constructor and resolver, with the test executor.
                    if let Some(service) = weak.upgrade() {
                        if let Some(logical) =
                            crate::presentation::commands::runtime_failure::resolve_runtime_failure(
                                &service,
                                source,
                                &err.to_string(),
                            )
                            .await
                        {
                            tx.send(logical).unwrap();
                        }
                    }
                });
            },
        );
        let token = service
            .prepare_recording_capture(
                102,
                service.get_config_snapshot(),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                on_error,
            )
            .await
            .unwrap();
        assert!(matches!(
            service
                .continue_prepared_capture(token, paused, Instant::now(), Default::default())
                .await
                .unwrap(),
            ContinueCaptureOutcome::Attached { .. }
        ));
        let callback = native_callback.lock().unwrap().clone().unwrap();
        callback(crate::domain::AudioError::Capture(
            "attached B failed".into(),
        ));
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .unwrap(),
            Some(101)
        );
        callback(crate::domain::AudioError::Capture("duplicate B".into()));
        assert!(rx.try_recv().is_err());
        assert_eq!(
            service.completed_report_for_run(101).await.unwrap().run_id,
            101
        );
        assert!(!service.audio_capture.read().await.is_capturing());
        service.finalize_provider_for_run(101).await.unwrap_err();
        assert_eq!(log.lock().unwrap().aborts, 1);
        // A delayed task which passed the native callback fence must also be rejected.
        let c = service
            .prepare_recording_capture(
                103,
                service.get_config_snapshot(),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_| {}),
            )
            .await
            .unwrap();
        assert_eq!(
            service
                .cleanup_capture_runtime_failure(102, "late B task")
                .await,
            None
        );
        callback(crate::domain::AudioError::Capture(
            "stale B while C captures".into(),
        ));
        assert!(service.capture_is_active_for_run(103).await);
        assert_eq!(log.lock().unwrap().aborts, 1);
        service.cancel_prepared_capture(c).await.unwrap();
    }

    struct SelectingCapture {
        inner: ImmediateAudioCapture,
        selected: Option<String>,
        after_start: Option<String>,
    }
    #[async_trait]
    impl AudioCapture for SelectingCapture {
        async fn initialize(&mut self, config: AudioConfig) -> AudioResult<()> {
            self.inner.initialize(config).await
        }
        async fn start_capture(
            &mut self,
            callback: crate::domain::AudioChunkCallback,
        ) -> AudioResult<()> {
            self.selected = self.after_start.clone(); // native open may fall back again
            self.inner.start_capture(callback).await
        }
        async fn stop_capture(&mut self) -> AudioResult<()> {
            self.inner.stop_capture().await
        }
        fn is_capturing(&self) -> bool {
            self.inner.is_capturing()
        }
        fn config(&self) -> AudioConfig {
            self.inner.config()
        }
    }

    #[tokio::test]
    async fn effective_device_after_native_start_is_frozen_and_unknown_refuses() {
        for actual_b in [
            Some("D"),
            Some("other default"),
            None,
            Some("fixture microphone"),
        ] {
            let (service, log, _, _) = continuation_service_fixture(false, 0, None).await;
            // A's policy is frozen from its successfully opened capture, not D.
            let a_policy = service.continuation_policy(101).unwrap();
            assert_eq!(
                a_policy.effective_device.as_deref(),
                Some("fixture microphone")
            );
            service.stop_capture_for_run(101).await.unwrap();
            let paused = service
                .pause_for_continuation(101, Instant::now())
                .await
                .unwrap();
            let mut requested = crate::domain::AppConfig::default();
            requested.selected_audio_device = Some("D".into());
            // This is the state preparation wrapper used around SystemAudioCapture.
            // Even if pre-open identity matches A, a native fallback on start wins.
            let capture = crate::presentation::state::effective_capture::EffectiveCapture::new(
                SelectingCapture {
                    inner: ImmediateAudioCapture::new(),
                    selected: Some("fixture microphone".into()),
                    after_start: actual_b.map(str::to_owned),
                },
                service.effective_capture_device_source(),
                |capture| capture.selected.clone(),
            );
            service
                .replace_audio_capture(Box::new(capture))
                .await
                .unwrap();
            service.register_continuation_policy(102, &requested).await;
            let token = service
                .prepare_recording_capture(
                    102,
                    service.get_config_snapshot(),
                    Arc::new(|_, _| {}),
                    Arc::new(|_, _| {}),
                    Arc::new(|_| {}),
                )
                .await
                .unwrap();
            assert_eq!(
                service
                    .continuation_policy(102)
                    .unwrap()
                    .effective_device
                    .as_deref(),
                actual_b
            );
            assert_eq!(service.continuation_policy(101), Some(a_policy));
            service.seal_prepared_capture(token).await.unwrap();
            let outcome = service
                .continue_prepared_capture(token, paused, Instant::now(), Default::default())
                .await
                .unwrap();
            if actual_b == Some("fixture microphone") {
                assert!(matches!(outcome, ContinueCaptureOutcome::Attached { .. }));
                assert_eq!(log.lock().unwrap().starts, 1);
            } else {
                assert_eq!(
                    outcome,
                    ContinueCaptureOutcome::Unsent(ContinuationRefusal::ConfigChanged)
                );
                assert!(!log
                    .lock()
                    .unwrap()
                    .operations
                    .iter()
                    .any(|op| matches!(op, crate::domain::ContinuationOperation::Continue { .. })));
                assert!(
                    service
                        .prepared_capture
                        .lock()
                        .await
                        .as_ref()
                        .unwrap()
                        .queued_bytes
                        .load(Ordering::Acquire)
                        > 0
                );
                service.cancel_prepared_capture(token).await.unwrap();
            }
            service.finalize_provider_for_run(101).await.unwrap();
        }
    }

    #[tokio::test]
    async fn negotiated_provider_error_callback_freezes_report_and_stops_physical_b() {
        let (service, log, _, _) = continuation_service_fixture(false, 0, None).await;
        service.stop_capture_for_run(101).await.unwrap();
        let paused = service
            .pause_for_continuation(101, Instant::now())
            .await
            .unwrap();
        let token = prepare_continued_b(&service).await;
        assert!(matches!(
            service
                .continue_prepared_capture(token, paused, Instant::now(), Default::default())
                .await
                .unwrap(),
            ContinueCaptureOutcome::Attached { .. }
        ));
        assert!(service.capture_is_active_for_run(102).await);
        let callback = log.lock().unwrap().error_callback.clone().unwrap();
        callback(SttError::Processing("injected provider error".into()));
        let report = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(report) = service.completed_report_for_run(101).await {
                    break report;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(report.run_id, 101);
        assert!(report.error.is_some());
        assert!(!service.audio_capture.read().await.is_capturing());
        let error = service
            .finalize_provider_for_run(101)
            .await
            .unwrap_err()
            .to_string();
        assert!(!error.contains("owned by run 0"));
        assert_eq!(log.lock().unwrap().aborts, 1);
    }

    #[tokio::test]
    async fn continuation_candidate_policy_change_refuses_before_control_and_preserves_pcm() {
        for field in 0..10 {
            let (service, log, _, _) = continuation_service_fixture(false, 0, None).await;
            service.stop_capture_for_run(101).await.unwrap();
            let paused = service
                .pause_for_continuation(101, Instant::now())
                .await
                .unwrap();
            let token = prepare_continued_b(&service).await;
            {
                let mut policies = service.continuation_compatibility.lock().unwrap();
                let policy = policies.get_mut(&102).unwrap();
                match field {
                    0 => policy.effective_device = Some("different device".into()),
                    1 => policy.sample_rate += 1,
                    2 => policy.channels += 1,
                    3 => policy.recording_mode = crate::domain::RecordingMode::LiveTranslation,
                    4 => policy.auto_paste = !policy.auto_paste,
                    5 => policy.auto_copy = !policy.auto_copy,
                    6 => policy.microphone_sensitivity += 1,
                    7 => policy.manual_stop_only = !policy.manual_stop_only,
                    _ => {}
                }
            }
            if field >= 8 {
                let mut config = service.get_config_snapshot();
                if field == 8 {
                    config.model = Some("changed-model".into());
                } else {
                    config.language = "changed-language".into();
                }
                service
                    .prepared_capture
                    .lock()
                    .await
                    .as_mut()
                    .unwrap()
                    .config = config;
            }
            assert_eq!(
                service
                    .continue_prepared_capture(token, paused, Instant::now(), Default::default())
                    .await
                    .unwrap(),
                ContinueCaptureOutcome::Unsent(ContinuationRefusal::ConfigChanged)
            );
            assert_eq!(log.lock().unwrap().operations.len(), 1);
            assert_eq!(log.lock().unwrap().samples.len(), 480);
            service.cancel_prepared_capture(token).await.unwrap();
            let _ = service.finalize_provider_for_run(101).await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn negotiated_continue_accepts_buffered_b_after_three_seconds_on_same_provider() {
        let (service, log, _, _) = continuation_service_fixture(false, 0, None).await;
        service.stop_capture_for_run(101).await.unwrap();
        let stopped_at = Instant::now();
        let paused = service
            .pause_for_continuation(101, stopped_at)
            .await
            .unwrap();
        let token = prepare_continued_b(&service).await;
        tokio::time::advance(Duration::from_secs(3)).await;
        assert!(matches!(
            service
                .continue_prepared_capture(token, paused, Instant::now(), Default::default())
                .await
                .unwrap(),
            ContinueCaptureOutcome::Attached {
                logical_run_id: 101,
                ..
            }
        ));
        assert_eq!(log.lock().unwrap().starts, 1);
        assert_eq!(log.lock().unwrap().samples.len(), 960);
        service.stop_capture_for_run(token.run_id).await.unwrap();
        service.finalize_provider_for_run(101).await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn negotiated_continue_rejects_exact_desktop_deadline_without_b_write() {
        let (service, log, _, _) = continuation_service_fixture(false, 0, None).await;
        service.stop_capture_for_run(101).await.unwrap();
        let stopped_at = Instant::now();
        let paused = service
            .pause_for_continuation(101, stopped_at)
            .await
            .unwrap();
        let token = prepare_continued_b(&service).await;
        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(
            service
                .continue_prepared_capture(token, paused, Instant::now(), Default::default())
                .await
                .unwrap(),
            ContinueCaptureOutcome::Unsent(ContinuationRefusal::Expired)
        );
        assert_eq!(log.lock().unwrap().operations.len(), 1);
        assert_eq!(log.lock().unwrap().samples.len(), 480);
        service.cancel_prepared_capture(token).await.unwrap();
        service.finalize_provider_for_run(101).await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn hundred_unsent_start_cancel_cycles_keep_one_lease_and_bounded_slots() {
        let (service, log, _, _) = continuation_service_fixture(false, 0, None).await;
        service.stop_capture_for_run(101).await.unwrap();
        let paused = service
            .pause_for_continuation(101, Instant::now())
            .await
            .unwrap();
        for run in 102..202 {
            service
                .register_continuation_policy(run, &crate::domain::AppConfig::default())
                .await;
            let token = service
                .prepare_recording_capture(
                    run,
                    service.get_config_snapshot(),
                    Arc::new(|_, _| {}),
                    Arc::new(|_, _| {}),
                    Arc::new(|_| {}),
                )
                .await
                .unwrap();
            service.cancel_prepared_capture(token).await.unwrap();
            assert!(service.prepared_capture.lock().await.is_none());
            assert!(!service.audio_capture.read().await.is_capturing());
            assert!(service.continuation_compatibility.lock().unwrap().len() <= 2);
            assert_eq!(
                service.paused_continuation_snapshot().await,
                Some(paused.clone())
            );
            assert_eq!(log.lock().unwrap().starts, 1);
            assert_eq!(log.lock().unwrap().operations.len(), 1);
            assert_eq!(log.lock().unwrap().samples.len(), 480);
            tokio::time::advance(Duration::from_millis(30)).await;
        }
        tokio::time::advance(Duration::from_secs(2)).await;
        assert!(Instant::now() >= paused.deadline());
        service.finalize_provider_for_run(101).await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn late_pause_acceptance_does_not_extend_user_stop_window() {
        let (service, log, _, _) = continuation_service_fixture(false, 5, None).await;
        service.stop_capture_for_run(101).await.unwrap();
        let stopped_at = Instant::now();
        let paused = service
            .pause_for_continuation(101, stopped_at)
            .await
            .unwrap();
        assert!(Instant::now() >= stopped_at + Duration::from_millis(1500));
        assert_eq!(paused.stopped_at, stopped_at);
        assert_eq!(paused.deadline(), stopped_at + Duration::from_secs(5));
        let token = prepare_continued_b(&service).await;
        tokio::time::sleep_until(paused.deadline()).await;
        assert_eq!(
            service
                .continue_prepared_capture(token, paused, Instant::now(), Default::default())
                .await
                .unwrap(),
            ContinueCaptureOutcome::Unsent(ContinuationRefusal::Expired)
        );
        assert_eq!(log.lock().unwrap().operations.len(), 1);
        assert_eq!(log.lock().unwrap().samples.len(), 480);
        service.cancel_prepared_capture(token).await.unwrap();
        service.finalize_provider_for_run(101).await.unwrap();
    }

    #[tokio::test]
    async fn continuation_reducer_drives_real_service_through_sealed_second_episode() {
        use crate::presentation::recording_intent_coordinator as c;
        let (service, log, _, deliveries) = continuation_service_fixture(false, 0, None).await;
        let mut coordinator = c::CoordinatorState::with_next_run_id_for_test(101);
        let effects = c::reduce(
            &mut coordinator,
            c::CoordinatorEvent::Intent(c::RecordingIntent::start(c::IntentSource::Frontend, None)),
        );
        let (prepare, a) = effects
            .iter()
            .find_map(|effect| match effect {
                c::CoordinatorEffect::PrepareCapture { effect_id, run } => Some((*effect_id, *run)),
                _ => None,
            })
            .unwrap();
        let generation = service.active_capture_episode().unwrap().generation;
        let effects = c::reduce(
            &mut coordinator,
            c::CoordinatorEvent::PrepareFinished {
                effect_id: prepare,
                run_id: a.run_id,
                outcome: c::PrepareOutcome::Succeeded { generation },
            },
        );
        let start = effects
            .iter()
            .find_map(|effect| match effect {
                c::CoordinatorEffect::StartRecording { effect_id, .. } => Some(*effect_id),
                _ => None,
            })
            .unwrap();
        c::reduce(
            &mut coordinator,
            c::CoordinatorEvent::StartFinished {
                effect_id: start,
                run_id: a.run_id,
                outcome: c::StartOutcome::Succeeded,
            },
        );
        let session = service.continuation_session_for_run(101).await.unwrap();
        c::reduce(
            &mut coordinator,
            c::CoordinatorEvent::Continuation(c::ContinuationEvent::Negotiated {
                logical_run_id: a.run_id,
                connection_generation: session.connection_generation,
            }),
        );
        let stopped = Instant::now();
        let effects = c::reduce(
            &mut coordinator,
            c::CoordinatorEvent::Intent(c::RecordingIntent::stop(c::IntentSource::Frontend, None)),
        );
        let stop = effects
            .iter()
            .find_map(|effect| match effect {
                c::CoordinatorEffect::StopRecording { effect_id, .. } => Some(*effect_id),
                _ => None,
            })
            .unwrap();
        service.stop_capture_for_run(101).await.unwrap();
        let effects = c::reduce(
            &mut coordinator,
            c::CoordinatorEvent::CaptureStopped {
                effect_id: stop,
                run_id: a.run_id,
                outcome: c::CaptureStopOutcome::Inactive,
            },
        );
        let (pause, key) = effects
            .iter()
            .find_map(|effect| match effect {
                c::CoordinatorEffect::Continuation(c::ContinuationEffect::Pause {
                    effect_id,
                    key,
                    ..
                }) => Some((*effect_id, *key)),
                _ => None,
            })
            .unwrap();
        let lease = service
            .pause_for_continuation(key.logical_run_id.get(), stopped)
            .await
            .unwrap();
        c::reduce(
            &mut coordinator,
            c::CoordinatorEvent::Continuation(c::ContinuationEvent::PauseFinished {
                effect_id: pause,
                key,
                pause_epoch: Some(lease.pause_epoch),
                continue_window: lease.continue_window,
            }),
        );
        assert_eq!(coordinator.processing_jobs.len(), 1);
        assert!(service.completed_report_for_run(101).await.is_none());
        let effects = c::reduce(
            &mut coordinator,
            c::CoordinatorEvent::Intent(c::RecordingIntent::start(c::IntentSource::Frontend, None)),
        );
        let (prepare, b) = effects
            .iter()
            .find_map(|effect| match effect {
                c::CoordinatorEffect::PrepareCapture { effect_id, run } => Some((*effect_id, *run)),
                _ => None,
            })
            .unwrap();
        let token = prepare_continued_b(&service).await;
        assert_eq!(token.run_id, b.run_id.get());
        c::reduce(
            &mut coordinator,
            c::CoordinatorEvent::Intent(c::RecordingIntent::stop(
                c::IntentSource::HoldHotkey,
                None,
            )),
        );
        let effects = c::reduce(
            &mut coordinator,
            c::CoordinatorEvent::PrepareFinished {
                effect_id: prepare,
                run_id: b.run_id,
                outcome: c::PrepareOutcome::Succeeded {
                    generation: token.generation,
                },
            },
        );
        let seal = effects
            .iter()
            .find_map(|effect| match effect {
                c::CoordinatorEffect::Continuation(c::ContinuationEffect::SealPending {
                    effect_id,
                    cancel: false,
                    ..
                }) => Some(*effect_id),
                _ => None,
            })
            .unwrap();
        service.seal_prepared_capture(token).await.unwrap();
        let effects = c::reduce(
            &mut coordinator,
            c::CoordinatorEvent::Continuation(c::ContinuationEvent::PendingCaptureStopped {
                effect_id: seal,
                run_id: b.run_id,
                generation: token.generation,
                outcome: c::CaptureStopOutcome::Inactive,
            }),
        );
        let (attach, key) = effects
            .iter()
            .find_map(|effect| match effect {
                c::CoordinatorEffect::Continuation(c::ContinuationEffect::Continue {
                    effect_id,
                    key,
                    ..
                }) => Some((*effect_id, *key)),
                _ => None,
            })
            .unwrap();
        let result = service
            .continue_prepared_capture(
                token,
                lease,
                Instant::now(),
                Arc::new(AtomicBool::new(false)),
            )
            .await
            .unwrap();
        let ContinueCaptureOutcome::Attached {
            logical_run_id,
            context_revision,
        } = result
        else {
            panic!("real continuation did not attach");
        };
        assert_eq!(logical_run_id, 101);
        let effects = c::reduce(
            &mut coordinator,
            c::CoordinatorEvent::Continuation(c::ContinuationEvent::ContinueFinished {
                effect_id: attach,
                key,
                run_id: b.run_id,
                generation: token.generation,
                outcome: c::ContinueAttachOutcome::Attached { context_revision },
            }),
        );
        assert!(effects.iter().any(|effect|matches!(effect,c::CoordinatorEffect::StopRecording{run_id,..} if *run_id==b.run_id)));
        assert_eq!(coordinator.processing_jobs.len(), 1);
        assert_eq!(*deliveries.lock().unwrap(), vec!["late A"]);
        assert!(!service.audio_capture.read().await.is_capturing());
        service.stop_capture_for_run(token.run_id).await.unwrap();
        service
            .finalize_provider_for_run(logical_run_id)
            .await
            .unwrap();
        let report = service
            .completed_report_for_run(logical_run_id)
            .await
            .unwrap();
        assert_eq!(
            (
                report.audio.accepted_bytes,
                report.audio.submitted_bytes,
                report.audio.unknown_bytes
            ),
            (1920, 1920, 0)
        );
        let log = log.lock().unwrap();
        assert_eq!((log.starts, log.stops), (1, 1));
    }

    #[tokio::test]
    async fn continuation_session_rechecks_logical_owner_after_waiting_for_provider() {
        let (service, _, _, _) = continuation_service_fixture(false, 0, None).await;
        assert!(service.continuation_session_for_run(101).await.is_some());
        let provider_write = service.stt_provider.write().await;
        let lookup = service.continuation_session_for_run(101);
        tokio::pin!(lookup);
        assert!(futures_util::poll!(lookup.as_mut()).is_pending());
        service.provider_run_id.store(102, Ordering::Release);
        drop(provider_write);
        assert!(lookup.await.is_none());
        // Restore fixture ownership solely for ordinary teardown.
        service.provider_run_id.store(101, Ordering::Release);
        service.stop_capture_for_run(101).await.unwrap();
        service.finalize_provider_for_run(101).await.unwrap();
    }

    #[tokio::test]
    async fn continuation_service_refuses_unregistered_native_context_without_pause_control() {
        let (service, log, _, _) = continuation_service_fixture(false, 0, None).await;
        *service.continuation_context.lock().unwrap() = None;
        service.stop_capture_for_run(101).await.unwrap();
        assert!(service
            .pause_for_continuation(101, Instant::now())
            .await
            .is_err());
        assert!(log.lock().unwrap().operations.is_empty());
        assert!(service.completed_report_for_run(101).await.is_none());
        service.finalize_provider_for_run(101).await.unwrap();
        assert_eq!(log.lock().unwrap().stops, 1);
    }

    #[tokio::test]
    async fn terminal_first_report_retains_delivery_mode_and_run_identity() {
        let (service, _, _, deliveries) = continuation_service_fixture(false, 0, None).await;
        service.stop_capture_for_run(101).await.unwrap();
        assert!(deliveries.lock().unwrap().is_empty());
        service.finalize_provider_for_run(101).await.unwrap();
        let report = service.completed_report_for_run(101).await.unwrap();
        assert_eq!(report.continuation_delivery, Some(true));
        assert_eq!(report.run_id, 101);
        assert!(service.completed_report_for_run(102).await.is_none());
        assert!(service.finalize_provider_for_run(102).await.is_err());
        assert_eq!(
            service
                .completed_report_for_run(101)
                .await
                .unwrap()
                .continuation_delivery,
            Some(true)
        );
    }

    #[tokio::test]
    async fn continuation_service_keeps_one_logical_run_and_cumulative_audio_across_sealed_b() {
        let (service, log, _, deliveries) = continuation_service_fixture(false, 0, None).await;
        service.stop_capture_for_run(101).await.unwrap();
        let paused = service
            .pause_for_continuation(101, Instant::now())
            .await
            .unwrap();
        assert!(service.completed_report_for_run(101).await.is_none());
        assert_eq!(log.lock().unwrap().stops, 0);
        let token = prepare_continued_b(&service).await;
        service.seal_prepared_capture(token).await.unwrap();
        let result = service
            .continue_prepared_capture(
                token,
                paused,
                Instant::now(),
                Arc::new(AtomicBool::new(false)),
            )
            .await
            .unwrap();
        assert_eq!(
            result,
            ContinueCaptureOutcome::Attached {
                logical_run_id: 101,
                context_revision: 7
            }
        );
        assert_eq!(service.provider_run_id.load(Ordering::Acquire), 101);
        assert_eq!(service.active_capture_episode(), Some(token));
        assert!(!service.audio_capture.read().await.is_capturing());
        assert_eq!(*deliveries.lock().unwrap(), vec!["late A"]);
        service.finalize_provider_for_run(101).await.unwrap();
        let report = service.completed_report_for_run(101).await.unwrap();
        assert_eq!(report.audio.accepted_bytes, 1920);
        assert_eq!(report.audio.submitted_bytes, 1920);
        assert_eq!(report.audio.acknowledged_bytes, Some(1920));
        assert_eq!(report.audio.unknown_bytes, 0);
        let log = log.lock().unwrap();
        assert_eq!((log.starts, log.stops, log.aborts), (1, 1, 0));
        assert_eq!(log.samples, vec![1200; 960]);
    }

    #[tokio::test]
    async fn e63_second_pause_retires_only_after_confirmed_terminal_release() {
        for confirmed in [true, false] {
            let (service, log, _, _) = continuation_service_fixture(false, 0, None).await;
            service.stop_capture_for_run(101).await.unwrap();
            let first = service
                .pause_for_continuation(101, Instant::now())
                .await
                .unwrap();
            let b = prepare_continued_b(&service).await;
            assert!(matches!(
                service
                    .continue_prepared_capture(b, first.clone(), Instant::now(), Default::default())
                    .await
                    .unwrap(),
                ContinueCaptureOutcome::Attached {
                    logical_run_id: 101,
                    ..
                }
            ));
            assert!(service.paused_continuation_snapshot().await.is_none());
            // Continue has written B's first PCM through the production processor.
            assert_eq!(log.lock().unwrap().samples, vec![1200; 960]);
            service.stop_capture_for_run(b.run_id).await.unwrap();
            let second = service
                .pause_for_continuation(101, Instant::now())
                .await
                .unwrap();
            assert!(second.pause_epoch > first.pause_epoch);
            assert_eq!(
                service.paused_continuation_snapshot().await,
                Some(second.clone())
            );
            if !confirmed {
                // Existing provider fixture evidence injection, not service state mutation.
                log.lock().unwrap().explicit_report = Some(crate::domain::ProviderFinalizeReport {
                    reason: crate::domain::FinalizeReason::Drained,
                    tail_evidence: crate::domain::TailEvidence::SegmentObserved,
                    provider_release: crate::domain::ProviderRelease::Unconfirmed,
                    last_delivery_seq: 2,
                    stable_snapshot: "A late A".into(),
                    error: None,
                });
            }
            assert_eq!(
                service.finalize_provider_for_run(101).await.is_ok(),
                confirmed
            );
            let report = service.completed_report_for_run(101).await.unwrap();
            assert_eq!(report.run_id, 101);
            assert!(service.completed_report_for_run(b.run_id).await.is_none());
            assert_eq!(service.get_status().await, RecordingStatus::Idle);
            assert!(service.stt_provider.read().await.is_none());
            assert_eq!(
                service.provider_run_id.load(Ordering::Acquire),
                if confirmed { 0 } else { 101 }
            );
            assert_eq!(
                service.paused_continuation_snapshot().await,
                if confirmed { None } else { Some(second) }
            );
            assert_eq!(
                report.provider_release,
                if confirmed {
                    crate::domain::ProviderRelease::Released
                } else {
                    crate::domain::ProviderRelease::Unconfirmed
                }
            );
            assert_eq!(report.error.is_none(), confirmed);
            assert_eq!(
                service.last_finalized_run_id.load(Ordering::Acquire),
                if confirmed { 101 } else { 0 }
            );
            assert_eq!(
                (
                    report.audio.accepted_bytes,
                    report.audio.read_bytes,
                    report.audio.submitted_bytes
                ),
                (1920, 1920, 1920)
            );
            assert_eq!(report.audio.acknowledged_bytes, Some(1920));
            assert_eq!(
                (report.audio.remaining_bytes, report.audio.unknown_bytes),
                (0, 0)
            );
            let frozen = serde_json::to_value(&report).unwrap();
            assert_eq!(
                service.finalize_provider_for_run(101).await.is_ok(),
                confirmed
            );
            assert_eq!(
                serde_json::to_value(service.completed_report_for_run(101).await.unwrap()).unwrap(),
                frozen
            );
            let log = log.lock().unwrap();
            assert_eq!((log.starts, log.stops, log.aborts), (1, 1, 0));
            assert_eq!(log.pause_sample_counts, vec![480, 960]);
            // All assertions precede teardown; no harness cleanup retires the lease.
        }
    }

    #[tokio::test]
    async fn e63_stale_finalization_preserves_newer_paused_owner() {
        let (service, log, _, _) = continuation_service_fixture(false, 0, None).await;
        service.stop_capture_for_run(101).await.unwrap();
        service
            .pause_for_continuation(101, Instant::now())
            .await
            .unwrap();
        service.finalize_provider_for_run(101).await.unwrap();
        let frozen =
            serde_json::to_value(service.completed_report_for_run(101).await.unwrap()).unwrap();
        let b = prepare_continued_b(&service).await;
        service
            .connect_prepared_recording(
                b,
                Arc::new(|_| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
                Default::default(),
            )
            .await
            .unwrap();
        service.stop_capture_for_run(b.run_id).await.unwrap();
        let newer = service
            .pause_for_continuation(b.run_id, Instant::now())
            .await
            .unwrap();
        // Cached finalization and a non-owner request must both leave B untouched.
        service.finalize_provider_for_run(101).await.unwrap();
        assert!(service.finalize_provider_for_run(100).await.is_err());
        assert_eq!(service.paused_continuation_snapshot().await, Some(newer));
        assert_eq!(
            service.provider_run_id.load(Ordering::Acquire),
            b.run_id as usize
        );
        assert!(service.stt_provider.read().await.is_some());
        assert_eq!(
            serde_json::to_value(service.completed_report_for_run(101).await.unwrap()).unwrap(),
            frozen
        );
        {
            let log = log.lock().unwrap();
            assert_eq!((log.starts, log.stops), (2, 1));
        }
        service.finalize_provider_for_run(b.run_id).await.unwrap();
        assert!(service.paused_continuation_snapshot().await.is_none());
    }

    #[tokio::test]
    async fn continuation_service_multiple_episodes_accumulate_once_and_keep_logical_identity() {
        let (service, log, _, deliveries) = continuation_service_fixture(false, 0, None).await;
        service.stop_capture_for_run(101).await.unwrap();
        let mut previous_generation = service.active_capture_episode().unwrap().generation;
        for episode in [102, 103] {
            service
                .register_continuation_policy(episode, &crate::domain::AppConfig::default())
                .await;
            let paused = service
                .pause_for_continuation(101, Instant::now())
                .await
                .unwrap();
            let token = service
                .prepare_recording_capture(
                    episode,
                    service.get_config_snapshot(),
                    Arc::new(|_, _| {}),
                    Arc::new(|_, _| {}),
                    Arc::new(|_| {}),
                )
                .await
                .unwrap();
            assert!(token.generation > previous_generation);
            previous_generation = token.generation;
            service.seal_prepared_capture(token).await.unwrap();
            assert!(matches!(
                service
                    .continue_prepared_capture(
                        token,
                        paused,
                        Instant::now(),
                        Arc::new(AtomicBool::new(false))
                    )
                    .await
                    .unwrap(),
                ContinueCaptureOutcome::Attached {
                    logical_run_id: 101,
                    ..
                }
            ));
        }
        service.finalize_provider_for_run(101).await.unwrap();
        let report = service.completed_report_for_run(101).await.unwrap();
        assert_eq!(
            (
                report.audio.accepted_bytes,
                report.audio.read_bytes,
                report.audio.submitted_bytes
            ),
            (2880, 2880, 2880)
        );
        assert_eq!(report.audio.acknowledged_bytes, Some(2880));
        assert_eq!(log.lock().unwrap().samples, vec![1200; 1440]);
        let log = log.lock().unwrap();
        assert_eq!((log.starts, log.stops), (1, 1));
        assert_eq!(*deliveries.lock().unwrap(), vec!["late A", "late A"]);
    }

    #[tokio::test]
    async fn continuation_service_post_accepted_mismatch_restores_and_preserves_same_cold_buffer() {
        let (service, log, _, deliveries) = continuation_service_fixture(true, 0, None).await;
        service.stop_capture_for_run(101).await.unwrap();
        let paused = service
            .pause_for_continuation(101, Instant::now())
            .await
            .unwrap();
        let token = prepare_continued_b(&service).await;
        service.seal_prepared_capture(token).await.unwrap();
        assert_eq!(
            service
                .continue_prepared_capture(
                    token,
                    paused,
                    Instant::now(),
                    Arc::new(AtomicBool::new(false))
                )
                .await
                .unwrap(),
            ContinueCaptureOutcome::Unsent(ContinuationRefusal::ContextMismatch)
        );
        assert_eq!(log.lock().unwrap().samples.len(), 480);
        assert_eq!(*deliveries.lock().unwrap(), vec!["late A"]);
        assert!(matches!(
            log.lock().unwrap().operations.last(),
            Some(crate::domain::ContinuationOperation::Restore { .. })
        ));
        assert_eq!(
            service
                .prepared_capture
                .lock()
                .await
                .as_ref()
                .unwrap()
                .token,
            token
        );
        service.finalize_provider_for_run(101).await.unwrap();
        assert_eq!(
            service
                .completed_report_for_run(101)
                .await
                .unwrap()
                .provider_release,
            crate::domain::ProviderRelease::Released
        );
        service
            .connect_prepared_recording(
                token,
                Arc::new(|_| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
                Arc::new(AtomicBool::new(false)),
            )
            .await
            .unwrap();
        service.finalize_provider_for_run(102).await.unwrap();
        assert!(service.prepared_capture.lock().await.is_none());
        assert_eq!(log.lock().unwrap().samples, vec![1200; 960]);
        assert_eq!(log.lock().unwrap().starts, 2);
    }

    #[tokio::test]
    async fn e45_real_backend_legacy_capabilities_finalize_without_pause_or_continue() {
        use crate::infrastructure::stt::BackendProvider;
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        struct CompatFactory(bool);
        impl SttProviderFactory for CompatFactory {
            fn create(&self, _: &SttConfig) -> SttResult<Box<dyn SttProvider>> {
                Ok(Box::new(if self.0 {
                    BackendProvider::with_continuation_for_test()
                } else {
                    BackendProvider::new()
                }))
            }
        }
        // Exercise the default legacy desktop's real Config generation. Run
        // with the experimental opt-in unset; do not rewrite negotiated state.
        for (new_desktop, missing_capabilities) in [(true, false), (false, false), (true, true)] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("ws://{}", listener.local_addr().unwrap());
            let peer = tokio::spawn(async move {
                let (socket, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
                let Message::Text(config) = ws.next().await.unwrap().unwrap() else {
                    panic!("Config text required")
                };
                let config: serde_json::Value = serde_json::from_str(&config).unwrap();
                let offered = config["capabilities"].as_array().unwrap();
                assert_eq!(
                    offered.iter().any(|c| c == "el_pause_continue_v1"),
                    new_desktop
                );
                // Also cover the old Ready shape with no capabilities field.
                let ready = if missing_capabilities {
                    serde_json::json!({"type":"ready","session_id":"compat"})
                } else {
                    serde_json::json!({"type":"ready","session_id":"compat",
                        "accepted_capabilities":["finalize_outcome_v1"]})
                };
                ws.send(Message::Text(ready.to_string().into()))
                    .await
                    .unwrap();
                let mut pcm = Vec::new();
                let mut finalized = 0;
                let mut closed = 0;
                let mut seq = 0;
                loop {
                    match ws.next().await.unwrap().unwrap() {
                        Message::Binary(bytes) => {
                            assert_eq!(finalized, 0);
                            pcm.extend_from_slice(&bytes);
                            seq += 1;
                            ws.send(Message::Text(
                                serde_json::json!({"type":"ack","seq":seq})
                                    .to_string()
                                    .into(),
                            ))
                            .await
                            .unwrap();
                        }
                        Message::Text(text) => {
                            let message: serde_json::Value = serde_json::from_str(&text).unwrap();
                            match message["type"].as_str().unwrap() {
                                "finalize" => {
                                    finalized += 1;
                                    assert_eq!(pcm, 1200_i16.to_le_bytes().repeat(480));
                                    ws.send(Message::Text(serde_json::json!({"type":"finalize_complete","status":"drained","saw_result":false,
                                        "outcome":{"reason":"drained","tail_evidence":"unconfirmed","provider_release":"released","last_delivery_seq":0,"stable_snapshot":""}}).to_string().into())).await.unwrap();
                                }
                                "close" => {
                                    assert_eq!(finalized, 1);
                                    closed += 1;
                                }
                                "keepalive" => {}
                                other => panic!("unexpected compatibility control: {other}"),
                            }
                        }
                        Message::Close(_) => {
                            assert_eq!((finalized, closed), (1, 1));
                            break;
                        }
                        Message::Ping(payload) => ws.send(Message::Pong(payload)).await.unwrap(),
                        other => panic!("unexpected frame: {other:?}"),
                    }
                }
            });
            let service = TranscriptionService::new(
                Box::new(ImmediateAudioCapture::new()),
                Arc::new(CompatFactory(new_desktop)),
            );
            let mut config = SttConfig::new(SttProviderType::Backend);
            config.backend_url = Some(url);
            config.backend_auth_token = Some("local-test".into());
            config.backend_streaming_provider = crate::domain::BackendStreamingProvider::ElevenLabs;
            config.continuation_target_eligible = true;
            service.update_config(config.clone()).await.unwrap();
            let token = service
                .prepare_recording_capture(
                    101,
                    config,
                    Arc::new(|_, _| {}),
                    Arc::new(|_, _| {}),
                    Arc::new(|_| {}),
                )
                .await
                .unwrap();
            let reader_ready = Arc::new(tokio::sync::Notify::new());
            let observed_ready = reader_ready.clone();
            tokio::time::timeout(Duration::from_secs(3), async {
                service
                    .connect_prepared_recording(
                        token,
                        Arc::new(|_| {}),
                        Arc::new(|_| {}),
                        Arc::new(|_, _| {}),
                        Arc::new(|_, _| {}),
                        Arc::new(|_| {}),
                        Arc::new(move |quality, detail| {
                            // This peer sends no Resumed: Good/None comes only
                            // from BackendProvider's reader after Ready handling.
                            if quality == "Good" && detail.is_none() {
                                observed_ready.notify_one();
                            }
                        }),
                        Default::default(),
                    )
                    .await
                    .unwrap();
                reader_ready.notified().await;
                service.stop_capture_for_run(101).await.unwrap();
                assert!(service.continuation_session_for_run(101).await.is_none());
                assert!(service
                    .pause_for_continuation(101, Instant::now())
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("not negotiated"));
                service.finalize_provider_for_run(101).await.unwrap();
                peer.await.unwrap();
            })
            .await
            .expect("legacy service Finalize/close deadline");
            assert!(service.stt_provider.read().await.is_none());
            assert!(!service.audio_capture.read().await.is_capturing());
            assert_eq!(service.logical_provider_run_id(), 0);
        }
    }

    struct RetryCapture {
        inner: ManualAudioCapture,
        stops: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl AudioCapture for RetryCapture {
        async fn initialize(&mut self, c: AudioConfig) -> AudioResult<()> {
            self.inner.initialize(c).await
        }
        async fn start_capture(
            &mut self,
            cb: crate::domain::AudioChunkCallback,
        ) -> AudioResult<()> {
            self.inner.start_capture(cb).await
        }
        async fn stop_capture(&mut self) -> AudioResult<()> {
            if self.stops.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(crate::domain::AudioError::Capture(
                    "retry fixture still active".into(),
                ));
            }
            self.inner.stop_capture().await
        }
        fn is_capturing(&self) -> bool {
            self.inner.is_capturing()
        }
        fn config(&self) -> AudioConfig {
            self.inner.config()
        }
    }

    #[tokio::test]
    async fn pending_stop_retry_preserves_every_b_sample_while_a_pause_waits() {
        for (cancel, cold) in [(false, false), (true, false), (false, true)] {
            let (service, log, _, _) = continuation_service_fixture(false, 0, None).await;
            service.stop_capture_for_run(101).await.unwrap();
            let chunks = Arc::new(StdMutex::new(None));
            let stops = Arc::new(AtomicUsize::new(0));
            *service.audio_capture.write().await = Box::new(RetryCapture {
                inner: ManualAudioCapture::new(chunks.clone()),
                stops: stops.clone(),
            });
            let provider_lock = service.stt_provider.write().await;
            let pause = service.pause_for_continuation(101, Instant::now());
            tokio::pin!(pause);
            assert!(futures_util::poll!(pause.as_mut()).is_pending());
            let token = tokio::time::timeout(Duration::from_secs(1), prepare_continued_b(&service))
                .await
                .unwrap();
            let tokens = StdMutex::new(std::collections::BTreeMap::from([(token.run_id, token)]));
            let samples: Vec<i16> = (0..480).map(|n| (n % 101) as i16 - 50).collect();
            let callback = chunks.lock().unwrap().clone().unwrap();
            callback(AudioChunk::new(samples.clone(), 16_000, 1));
            assert!(service.stop_pending_capture(token, cancel).await.is_err());
            assert!(service.capture_is_active_for_run(token.run_id).await);
            assert!(service.prepared_capture.lock().await.is_some());
            service.stop_pending_capture(token, cancel).await.unwrap();
            assert!(!service.capture_is_active_for_run(token.run_id).await);
            assert_eq!(stops.load(Ordering::SeqCst), 2);
            assert_eq!(service.prepared_capture.lock().await.is_some(), !cancel);
            crate::presentation::commands::retire_prepared_capture_after_stop(
                &tokens,
                token.run_id,
                false,
                Some((token.generation, cancel)),
            );
            assert_eq!(
                tokens.lock().unwrap().get(&token.run_id).copied(),
                if cancel { None } else { Some(token) }
            );
            drop(provider_lock);
            let paused = tokio::time::timeout(Duration::from_secs(1), pause)
                .await
                .unwrap()
                .unwrap();
            if cold {
                // A has released its provider; cold Start must recover the same
                // command-side token, not open a new physical capture over B.
                service.finalize_provider_for_run(101).await.unwrap();
                let routed = tokens.lock().unwrap().get(&token.run_id).copied().unwrap();
                service
                    .connect_prepared_recording(
                        routed,
                        Arc::new(|_| {}),
                        Arc::new(|_| {}),
                        Arc::new(|_, _| {}),
                        Arc::new(|_, _| {}),
                        Arc::new(|_| {}),
                        Arc::new(|_, _| {}),
                        Default::default(),
                    )
                    .await
                    .unwrap();
                service
                    .finalize_provider_for_run(token.run_id)
                    .await
                    .unwrap();
                assert_eq!(log.lock().unwrap().starts, 2);
            } else if !cancel {
                assert!(matches!(
                    service
                        .continue_prepared_capture(
                            token,
                            paused,
                            Instant::now(),
                            Arc::new(AtomicBool::new(false))
                        )
                        .await
                        .unwrap(),
                    ContinueCaptureOutcome::Attached { .. }
                ));
                service.stop_capture_for_run(token.run_id).await.unwrap();
            }
            if !cold {
                service.finalize_provider_for_run(101).await.unwrap();
            }
            crate::presentation::commands::retire_prepared_capture_after_stop(
                &tokens,
                token.run_id,
                false,
                None,
            );
            assert!(tokens.lock().unwrap().is_empty());
            let mut expected = vec![1200; 480];
            if !cancel {
                expected.extend(samples);
            }
            assert_eq!(log.lock().unwrap().samples, expected, "cancel={cancel}");
            assert!(service.prepared_capture.lock().await.is_none());
        }
    }

    #[tokio::test]
    async fn teardown_during_stop_retry_cancels_actual_context_wait_via_ordered_registry() {
        use crate::presentation::commands::{
            reduce_recording_event_with_cancellation, register_recording_effect_cancellations,
        };
        use crate::presentation::recording_intent_coordinator as ri;
        struct ContextWait(tokio::sync::Notify);
        #[async_trait]
        impl crate::domain::ContinuationContextGuard for ContextWait {
            async fn validate(&self, _: u64) -> crate::domain::ContextValidation {
                self.0.notified().await;
                crate::domain::ContextValidation::Valid { revision: 7 }
            }
        }
        for teardown in [
            ri::CoordinatorEvent::ShutdownRequested,
            ri::CoordinatorEvent::ForceOff(ri::StopReason::SystemSleep),
        ] {
            for retry_before_cancel in [false, true] {
                let (service, log, _, _) = continuation_service_fixture(false, 0, None).await;
                service.stop_capture_for_run(101).await.unwrap();
                let paused = service
                    .pause_for_continuation(101, Instant::now())
                    .await
                    .unwrap();
                let chunks = Arc::new(StdMutex::new(None));
                let stops = Arc::new(AtomicUsize::new(0));
                *service.audio_capture.write().await = Box::new(RetryCapture {
                    inner: ManualAudioCapture::new(chunks.clone()),
                    stops: stops.clone(),
                });
                let token = prepare_continued_b(&service).await;
                chunks.lock().unwrap().as_ref().unwrap()(AudioChunk::new(
                    vec![2400; 480],
                    16_000,
                    1,
                ));
                let barrier = Arc::new(ContextWait(Default::default()));
                service.set_continuation_context_guard(barrier.clone());
                let (mut coordinator, b, attach_id, key, generation, retry) =
                    ri::pending_continue_with_failed_seal();
                let cancellations = StdMutex::new(std::collections::BTreeMap::new());
                register_recording_effect_cancellations(
                    &cancellations,
                    &[ri::CoordinatorEffect::Continuation(
                        ri::ContinuationEffect::Continue {
                            effect_id: attach_id,
                            key,
                            run: b,
                            generation,
                        },
                    )],
                );
                let cancelled = cancellations
                    .lock()
                    .unwrap()
                    .get(&attach_id.get())
                    .unwrap()
                    .clone();
                let attach = service.continue_prepared_capture(
                    token,
                    paused,
                    Instant::now(),
                    cancelled.clone(),
                );
                tokio::pin!(attach);
                assert!(futures_util::poll!(attach.as_mut()).is_pending());
                assert!(service.stop_pending_capture(token, false).await.is_err());
                assert!(service.capture_is_active_for_run(token.run_id).await);
                if retry_before_cancel {
                    service.stop_pending_capture(token, false).await.unwrap();
                    reduce_recording_event_with_cancellation(
                        &mut coordinator,
                        &cancellations,
                        ri::CoordinatorEvent::CaptureStopped {
                            effect_id: retry,
                            run_id: b.run_id,
                            outcome: ri::CaptureStopOutcome::Inactive,
                        },
                        998,
                    );
                }
                let owner = coordinator.capture;
                let effects = reduce_recording_event_with_cancellation(
                    &mut coordinator,
                    &cancellations,
                    teardown,
                    999,
                );
                assert_eq!(coordinator.capture, owner);
                assert!(effects.iter().any(|e| matches!(e, ri::CoordinatorEffect::CancelStart { effect_id, .. } if *effect_id == attach_id)));
                assert!(
                    cancelled.load(Ordering::Acquire),
                    "ordered event must revoke admission before its async executor"
                );
                assert_eq!(
                    coordinator.pending_capture_retry(retry, b.run_id),
                    if retry_before_cancel {
                        None
                    } else {
                        Some((generation, false))
                    }
                );
                barrier.0.notify_one();
                assert_eq!(
                    tokio::time::timeout(Duration::from_secs(1), attach)
                        .await
                        .unwrap()
                        .unwrap(),
                    ContinueCaptureOutcome::Cancelled
                );
                assert_eq!(log.lock().unwrap().samples, vec![1200; 480]);
                assert!(matches!(
                    log.lock().unwrap().operations.as_slice(),
                    [crate::domain::ContinuationOperation::Pause { .. }]
                ));
                let mut queue = std::collections::VecDeque::from(effects);
                queue.extend(reduce_recording_event_with_cancellation(
                    &mut coordinator,
                    &cancellations,
                    ri::CoordinatorEvent::Continuation(ri::ContinuationEvent::ContinueFinished {
                        effect_id: attach_id,
                        key,
                        run_id: b.run_id,
                        generation,
                        outcome: ri::ContinueAttachOutcome::Cancelled,
                    }),
                    1000,
                ));
                if !retry_before_cancel {
                    service.stop_pending_capture(token, false).await.unwrap();
                    queue.extend(reduce_recording_event_with_cancellation(
                        &mut coordinator,
                        &cancellations,
                        ri::CoordinatorEvent::CaptureStopped {
                            effect_id: retry,
                            run_id: b.run_id,
                            outcome: ri::CaptureStopOutcome::Inactive,
                        },
                        1001,
                    ));
                }
                let tokens =
                    StdMutex::new(std::collections::BTreeMap::from([(token.run_id, token)]));
                let mut disposal_count = 0;
                let mut finalized = 0;
                while let Some(effect) = queue.pop_front() {
                    let completion = match effect {
                        ri::CoordinatorEffect::Continuation(
                            ri::ContinuationEffect::SealPending {
                                effect_id,
                                run_id,
                                generation,
                                cancel,
                            },
                        ) => {
                            assert!(cancel);
                            service.stop_pending_capture(token, cancel).await.unwrap();
                            crate::presentation::commands::retire_prepared_capture_after_stop(
                                &tokens,
                                token.run_id,
                                false,
                                Some((token.generation, cancel)),
                            );
                            disposal_count += 1;
                            Some(ri::CoordinatorEvent::Continuation(
                                ri::ContinuationEvent::PendingCaptureStopped {
                                    effect_id,
                                    run_id,
                                    generation,
                                    outcome: ri::CaptureStopOutcome::Inactive,
                                },
                            ))
                        }
                        ri::CoordinatorEffect::FinalizeRecording {
                            effect_id, run_id, ..
                        } => {
                            service.finalize_provider_for_run(101).await.unwrap();
                            finalized += 1;
                            Some(ri::CoordinatorEvent::FinalizeFinished {
                                effect_id,
                                run_id,
                                outcome: ri::FinalizeOutcome::Committed,
                            })
                        }
                        _ => None,
                    };
                    if let Some(completion) = completion {
                        queue.extend(reduce_recording_event_with_cancellation(
                            &mut coordinator,
                            &cancellations,
                            completion,
                            1002,
                        ));
                    }
                }
                assert_eq!(disposal_count, 1);
                assert_eq!(finalized, 1);
                assert!(tokens.lock().unwrap().is_empty());
                assert!(matches!(coordinator.capture, ri::CaptureState::Idle));
                assert!(coordinator.processing_jobs.is_empty());
                assert!(service.prepared_capture.lock().await.is_none());
                assert!(!service.capture_is_active_for_run(token.run_id).await);
                assert_eq!(stops.load(Ordering::SeqCst), 2);
            }
        }
    }

    #[tokio::test]
    async fn not_started_cancel_cleanup_failure_disposes_after_outstanding_ordinary_seal() {
        use crate::presentation::commands::{
            reduce_recording_event_with_cancellation, register_recording_effect_cancellations,
        };
        use crate::presentation::recording_intent_coordinator as ri;
        struct ReleaseCapture {
            inner: ManualAudioCapture,
            release: Arc<AtomicBool>,
        }
        #[async_trait]
        impl AudioCapture for ReleaseCapture {
            async fn initialize(&mut self, c: AudioConfig) -> AudioResult<()> {
                self.inner.initialize(c).await
            }
            async fn start_capture(
                &mut self,
                cb: crate::domain::AudioChunkCallback,
            ) -> AudioResult<()> {
                self.inner.start_capture(cb).await
            }
            async fn stop_capture(&mut self) -> AudioResult<()> {
                if !self.release.load(Ordering::Acquire) {
                    return Err(crate::domain::AudioError::Capture(
                        "test release failure".into(),
                    ));
                }
                self.inner.stop_capture().await
            }
            fn is_capturing(&self) -> bool {
                self.inner.is_capturing()
            }
            fn config(&self) -> AudioConfig {
                self.inner.config()
            }
        }
        let (service, log, _, _) = continuation_service_fixture(false, 0, None).await;
        service.stop_capture_for_run(101).await.unwrap();
        let paused = service
            .pause_for_continuation(101, Instant::now())
            .await
            .unwrap();
        let release = Arc::new(AtomicBool::new(false));
        let chunks = Arc::new(StdMutex::new(None));
        *service.audio_capture.write().await = Box::new(ReleaseCapture {
            inner: ManualAudioCapture::new(chunks.clone()),
            release: release.clone(),
        });
        let token = prepare_continued_b(&service).await;
        chunks.lock().unwrap().as_ref().unwrap()(AudioChunk::new(vec![2400; 480], 16_000, 1));
        let barrier = Arc::new(tokio::sync::Notify::new());
        log.lock().unwrap().first_write_barrier = Some(barrier.clone());
        let (mut coordinator, b, attach_id, key, generation, seal) =
            ri::pending_continue_with_outstanding_seal();
        let cancellations = StdMutex::new(std::collections::BTreeMap::new());
        register_recording_effect_cancellations(
            &cancellations,
            &[ri::CoordinatorEffect::Continuation(
                ri::ContinuationEffect::Continue {
                    effect_id: attach_id,
                    key,
                    run: b,
                    generation,
                },
            )],
        );
        let cancelled = cancellations
            .lock()
            .unwrap()
            .get(&attach_id.get())
            .unwrap()
            .clone();
        let attach =
            service.continue_prepared_capture(token, paused, Instant::now(), cancelled.clone());
        tokio::pin!(attach);
        assert!(futures_util::poll!(attach.as_mut()).is_pending());
        assert!(
            service.prepared_capture.lock().await.is_none(),
            "receiver consumed before first transport attempt"
        );
        let effects = reduce_recording_event_with_cancellation(
            &mut coordinator,
            &cancellations,
            ri::CoordinatorEvent::ForceOff(ri::StopReason::SystemSleep),
            999,
        );
        assert!(cancelled.load(Ordering::Acquire));
        assert!(!effects.iter().any(|e| matches!(
            e,
            ri::CoordinatorEffect::StopRecording { .. }
                | ri::CoordinatorEffect::Continuation(ri::ContinuationEffect::SealPending { .. })
        )));
        barrier.notify_one();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), attach)
                .await
                .unwrap()
                .unwrap(),
            ContinueCaptureOutcome::Cancelled
        );
        assert!(service.retains_prepared_capture(token).await);
        assert!(service.capture_is_active_for_run(token.run_id).await);
        let tokens = StdMutex::new(std::collections::BTreeMap::from([(token.run_id, token)]));
        let effects = ri::reduce(
            &mut coordinator,
            ri::CoordinatorEvent::Continuation(ri::ContinuationEvent::ContinueFinished {
                effect_id: attach_id,
                key,
                run_id: b.run_id,
                generation,
                outcome: ri::ContinueAttachOutcome::Cancelled,
            }),
        );
        assert!(!effects
            .iter()
            .any(|e| matches!(e, ri::CoordinatorEffect::StopRecording { .. })));
        // A may finalize independently of an unsent B; retain every issued effect.
        let mut queue = std::collections::VecDeque::from(effects);
        release.store(true, Ordering::Release);
        service.stop_pending_capture(token, false).await.unwrap();
        assert!(service.retains_prepared_capture(token).await);
        let effects = ri::reduce(
            &mut coordinator,
            ri::CoordinatorEvent::Continuation(ri::ContinuationEvent::PendingCaptureStopped {
                effect_id: seal,
                run_id: b.run_id,
                generation,
                outcome: ri::CaptureStopOutcome::Inactive,
            }),
        );
        queue.extend(effects);
        let mut disposed = 0;
        let mut finalized = 0;
        while let Some(effect) = queue.pop_front() {
            let completion = match effect {
                ri::CoordinatorEffect::Continuation(ri::ContinuationEffect::SealPending {
                    effect_id,
                    run_id,
                    generation,
                    cancel,
                }) => {
                    assert!(cancel);
                    service.stop_pending_capture(token, cancel).await.unwrap();
                    crate::presentation::commands::retire_prepared_capture_after_stop(
                        &tokens,
                        token.run_id,
                        false,
                        Some((token.generation, cancel)),
                    );
                    disposed += 1;
                    Some(ri::CoordinatorEvent::Continuation(
                        ri::ContinuationEvent::PendingCaptureStopped {
                            effect_id,
                            run_id,
                            generation,
                            outcome: ri::CaptureStopOutcome::Inactive,
                        },
                    ))
                }
                ri::CoordinatorEffect::FinalizeRecording {
                    effect_id, run_id, ..
                } => {
                    service.finalize_provider_for_run(101).await.unwrap();
                    finalized += 1;
                    Some(ri::CoordinatorEvent::FinalizeFinished {
                        effect_id,
                        run_id,
                        outcome: ri::FinalizeOutcome::Committed,
                    })
                }
                ri::CoordinatorEffect::StopRecording { .. } => {
                    panic!("existing Seal owner must not be replaced")
                }
                _ => None,
            };
            if let Some(completion) = completion {
                queue.extend(ri::reduce(&mut coordinator, completion));
            }
        }
        assert_eq!((disposed, finalized), (1, 1));
        assert!(tokens.lock().unwrap().is_empty());
        assert!(!service.retains_prepared_capture(token).await);
        assert!(!service.capture_is_active_for_run(token.run_id).await);
        assert!(matches!(coordinator.capture, ri::CaptureState::Idle));
        assert!(coordinator.processing_jobs.is_empty());
        assert_eq!(log.lock().unwrap().samples, vec![1200; 480]);
        let next = prepare_continued_b(&service).await;
        service.cancel_prepared_capture(next).await.unwrap();
    }

    #[tokio::test]
    async fn attempted_write_failure_keeps_retry_owner_when_actual_cleanup_cannot_stop_mic() {
        use crate::presentation::recording_intent_coordinator as ri;
        struct FailingStopCapture {
            inner: ManualAudioCapture,
            release: Arc<AtomicBool>,
            stops: Arc<AtomicUsize>,
        }
        #[async_trait]
        impl AudioCapture for FailingStopCapture {
            async fn initialize(&mut self, c: AudioConfig) -> AudioResult<()> {
                self.inner.initialize(c).await
            }
            async fn start_capture(
                &mut self,
                cb: crate::domain::AudioChunkCallback,
            ) -> AudioResult<()> {
                self.inner.start_capture(cb).await
            }
            async fn stop_capture(&mut self) -> AudioResult<()> {
                self.stops.fetch_add(1, Ordering::SeqCst);
                if !self.release.load(Ordering::Acquire) {
                    return Err(crate::domain::AudioError::Capture(
                        "owned microphone still active".into(),
                    ));
                }
                self.inner.stop_capture().await
            }
            fn is_capturing(&self) -> bool {
                self.inner.is_capturing()
            }
            fn config(&self) -> AudioConfig {
                self.inner.config()
            }
        }
        let (service, log, _, _) = continuation_service_fixture(false, 1, None).await;
        service.stop_capture_for_run(101).await.unwrap();
        let paused = service
            .pause_for_continuation(101, Instant::now())
            .await
            .unwrap();
        let release = Arc::new(AtomicBool::new(false));
        let stops = Arc::new(AtomicUsize::new(0));
        let chunks = Arc::new(StdMutex::new(None));
        *service.audio_capture.write().await = Box::new(FailingStopCapture {
            inner: ManualAudioCapture::new(chunks.clone()),
            release: release.clone(),
            stops: stops.clone(),
        });
        let token = prepare_continued_b(&service).await;
        let tokens = StdMutex::new(std::collections::BTreeMap::from([(token.run_id, token)]));
        assert!(
            !crate::presentation::commands::retire_consumed_prepared_capture(
                &service, &tokens, token
            )
            .await
        );
        assert_eq!(tokens.lock().unwrap().get(&token.run_id), Some(&token));
        chunks.lock().unwrap().as_ref().unwrap()(AudioChunk::new(vec![2400; 480], 16_000, 1));
        assert!(service.stop_pending_capture(token, false).await.is_err());
        let (mut coordinator, b, attach, key, generation, retry) =
            ri::pending_continue_with_failed_seal();
        assert!(service
            .continue_prepared_capture(token, paused, Instant::now(), Default::default())
            .await
            .is_err());
        assert!(
            crate::presentation::commands::retire_consumed_prepared_capture(
                &service, &tokens, token
            )
            .await
        );
        assert!(
            tokens.lock().unwrap().is_empty(),
            "adapter retires consumed token before any later Stop"
        );
        let newer = PreparedCaptureToken {
            generation: token.generation + 1,
            ..token
        };
        tokens.lock().unwrap().insert(token.run_id, newer);
        assert!(
            crate::presentation::commands::retire_consumed_prepared_capture(
                &service, &tokens, token
            )
            .await
        );
        assert_eq!(
            tokens.lock().unwrap().remove(&token.run_id),
            Some(newer),
            "stale cleanup cannot remove another generation"
        );
        assert!(
            stops.load(Ordering::SeqCst) >= 3,
            "both actual failure-cleanup stops must run"
        );
        assert!(service.audio_capture.read().await.is_capturing());
        assert!(service.capture_is_active_for_run(token.run_id).await);
        let effects = ri::reduce(
            &mut coordinator,
            ri::CoordinatorEvent::Continuation(ri::ContinuationEvent::ContinueFinished {
                effect_id: attach,
                key,
                run_id: b.run_id,
                generation,
                outcome: ri::ContinueAttachOutcome::AttemptedFailure(ri::ErrorCode(101)),
            }),
        );
        assert!(
            matches!(coordinator.capture, ri::CaptureState::Stopping { effect_id, finalize_after: true, .. } if effect_id == retry)
        );
        assert!(!effects.iter().any(|e| matches!(
            e,
            ri::CoordinatorEffect::StopRecording { .. }
                | ri::CoordinatorEffect::FinalizeRecording { .. }
        )));
        assert!(service.stop_capture_for_run(token.run_id).await.is_err());
        let effects = ri::reduce(
            &mut coordinator,
            ri::CoordinatorEvent::CaptureStopped {
                effect_id: retry,
                run_id: b.run_id,
                outcome: ri::CaptureStopOutcome::StillActive(ri::ErrorCode(102)),
            },
        );
        let next = effects
            .iter()
            .find_map(|e| match e {
                ri::CoordinatorEffect::StopRecording {
                    effect_id,
                    attempt: 2,
                    ..
                } => Some(*effect_id),
                _ => None,
            })
            .unwrap();
        assert!(
            service.prepared_capture.lock().await.is_none(),
            "attempted B cannot become replayable"
        );
        let attempted_samples = log.lock().unwrap().samples.clone();
        release.store(true, Ordering::Release);
        service.stop_capture_for_run(token.run_id).await.unwrap();
        let effects = ri::reduce(
            &mut coordinator,
            ri::CoordinatorEvent::CaptureStopped {
                effect_id: next,
                run_id: b.run_id,
                outcome: ri::CaptureStopOutcome::Inactive,
            },
        );
        assert_eq!(
            effects
                .iter()
                .filter(|e| matches!(e, ri::CoordinatorEffect::FinalizeRecording { .. }))
                .count(),
            1
        );
        let failure = service.finalize_provider_for_run(101).await.unwrap_err();
        assert!(failure.to_string().contains("unknown B write"));
        let report = service.completed_report_for_run(101).await.unwrap();
        // A local abort is not server admission release evidence.
        assert_eq!(
            report.provider_release,
            crate::domain::ProviderRelease::Unconfirmed
        );
        assert!(report.audio.unknown_bytes > 0);
        for effect in effects {
            if let ri::CoordinatorEffect::FinalizeRecording {
                effect_id, run_id, ..
            } = effect
            {
                ri::reduce(
                    &mut coordinator,
                    ri::CoordinatorEvent::FinalizeFinished {
                        effect_id,
                        run_id,
                        outcome: ri::FinalizeOutcome::ReleaseUnconfirmed(ri::ErrorCode(101)),
                    },
                );
            }
        }
        assert_eq!(coordinator.processing_jobs.len(), 1);
        let blocked = ri::reduce(
            &mut coordinator,
            ri::CoordinatorEvent::Intent(ri::RecordingIntent::start(
                ri::IntentSource::Frontend,
                None,
            )),
        );
        assert!(!blocked.iter().any(|e| matches!(
            e,
            ri::CoordinatorEffect::PrepareCapture { .. }
                | ri::CoordinatorEffect::StartRecording { .. }
        )));
        assert!(!service.audio_capture.read().await.is_capturing());
        assert!(matches!(coordinator.capture, ri::CaptureState::Idle));
        assert_eq!(log.lock().unwrap().samples, attempted_samples);
        assert_eq!(log.lock().unwrap().starts, 1);
    }

    #[tokio::test]
    async fn continue_cancelled_during_precontrol_waits_never_sends_control_or_b_audio() {
        struct ContextBarrier(tokio::sync::Notify, crate::domain::ContextValidation);
        #[async_trait]
        impl crate::domain::ContinuationContextGuard for ContextBarrier {
            async fn validate(&self, _: u64) -> crate::domain::ContextValidation {
                self.0.notified().await;
                self.1.clone()
            }
        }
        for (provider_lock, context) in [
            (
                true,
                crate::domain::ContextValidation::Valid { revision: 7 },
            ),
            (
                false,
                crate::domain::ContextValidation::Valid { revision: 7 },
            ),
            (false, crate::domain::ContextValidation::Unavailable),
            (false, crate::domain::ContextValidation::Mismatch),
        ] {
            let (service, log, _, _) = continuation_service_fixture(false, 0, None).await;
            service.stop_capture_for_run(101).await.unwrap();
            let paused = service
                .pause_for_continuation(101, Instant::now())
                .await
                .unwrap();
            let token = prepare_continued_b(&service).await;
            let barrier = Arc::new(ContextBarrier(Default::default(), context));
            if !provider_lock {
                service.set_continuation_context_guard(barrier.clone());
            }
            let held = if provider_lock {
                Some(service.stt_provider.write().await)
            } else {
                None
            };
            let cancelled = Arc::new(AtomicBool::new(false));
            let attach =
                service.continue_prepared_capture(token, paused, Instant::now(), cancelled.clone());
            tokio::pin!(attach);
            assert!(futures_util::poll!(attach.as_mut()).is_pending());
            cancelled.store(true, Ordering::Release);
            drop(held);
            barrier.0.notify_one();
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(1), attach)
                    .await
                    .unwrap()
                    .unwrap(),
                ContinueCaptureOutcome::Cancelled
            );
            {
                let observed = log.lock().unwrap();
                assert!(matches!(
                    observed.operations.as_slice(),
                    [crate::domain::ContinuationOperation::Pause { .. }]
                ));
                assert_eq!(observed.samples, vec![1200; 480]);
            }
            service.cancel_prepared_capture(token).await.unwrap();
            service.finalize_provider_for_run(101).await.unwrap();
            assert!(!service.audio_capture.read().await.is_capturing());
            assert!(service.prepared_capture.lock().await.is_none());
        }
    }

    #[tokio::test]
    async fn e43_shutdown_after_accepted_before_first_b_write_releases_service_owners() {
        struct AcceptedBarrier {
            calls: AtomicU64,
            release: tokio::sync::Notify,
        }
        #[async_trait]
        impl crate::domain::ContinuationContextGuard for AcceptedBarrier {
            async fn validate(&self, _: u64) -> crate::domain::ContextValidation {
                if self.calls.fetch_add(1, Ordering::AcqRel) == 1 {
                    self.release.notified().await;
                }
                crate::domain::ContextValidation::Valid { revision: 7 }
            }
        }
        let (service, log, _, _) = continuation_service_fixture(false, 0, None).await;
        service.stop_capture_for_run(101).await.unwrap();
        let paused = service
            .pause_for_continuation(101, Instant::now())
            .await
            .unwrap();
        let token = prepare_continued_b(&service).await;
        let barrier = Arc::new(AcceptedBarrier {
            calls: AtomicU64::new(0),
            release: Default::default(),
        });
        service.set_continuation_context_guard(barrier.clone());
        let cancelled = Arc::new(AtomicBool::new(false));
        let attach =
            service.continue_prepared_capture(token, paused, Instant::now(), cancelled.clone());
        tokio::pin!(attach);
        assert!(futures_util::poll!(attach.as_mut()).is_pending());
        // The second native validation is reached only after the service has
        // consumed and checked Accepted/ActiveAwaitingAudio and found B PCM.
        // Unlike the older fixture's control wait, the response has returned.
        assert_eq!(barrier.calls.load(Ordering::Acquire), 2);
        assert_eq!(log.lock().unwrap().samples, vec![1200; 480]);
        tokio::time::timeout(
            Duration::from_secs(1),
            service.cancel_prepared_capture(token),
        )
        .await
        .unwrap()
        .unwrap();
        cancelled.store(true, Ordering::Release);
        barrier.release.notify_one();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), attach)
                .await
                .unwrap()
                .unwrap(),
            ContinueCaptureOutcome::Cancelled
        );
        tokio::time::timeout(
            Duration::from_secs(1),
            service.finalize_provider_for_run(101),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(service.prepared_capture.lock().await.is_none());
        assert!(service.stt_provider.read().await.is_none());
        assert!(!service.audio_capture.read().await.is_capturing());
        assert_eq!(service.logical_provider_run_id(), 0);
        assert_eq!(service.capture_run_id.load(Ordering::Acquire), 0);
        assert!(service.paused_continuation_snapshot().await.is_none());
        let log = log.lock().unwrap();
        assert_eq!(log.samples, vec![1200; 480]);
        assert_eq!((log.starts, log.stops, log.aborts), (1, 1, 0));
        assert!(matches!(
            log.operations.as_slice(),
            [
                crate::domain::ContinuationOperation::Pause { .. },
                crate::domain::ContinuationOperation::Continue { .. },
                crate::domain::ContinuationOperation::Restore { .. }
            ]
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn e06_stop_backlog_four_and_eight_seconds_drains_before_pause_or_expires() {
        for seconds in [4usize, 8] {
            for expired in [false, true] {
                let chunks = Arc::new(StdMutex::new(None));
                let (service, log, _, _) =
                    continuation_service_fixture_with_capture(false, 0, None, Some(chunks.clone()))
                        .await;
                // Hold the actual provider writer: all injected A is accepted but
                // cannot be submitted until Stop has sealed ingress.
                let provider = service.stt_provider.write().await;
                let emit = chunks.lock().unwrap().as_ref().unwrap().clone();
                let samples = seconds * 16_000;
                let mut expected_a = Vec::with_capacity(samples);
                for index in 0..seconds * 100 {
                    let frame = vec![1200 + index as i16; 160];
                    expected_a.extend_from_slice(&frame);
                    emit(AudioChunk::new(frame, 16_000, 1));
                }
                if expired {
                    log.lock().unwrap().send_delay = Some(Duration::from_millis(10));
                }
                let stopped = Instant::now();
                service.stop_capture_for_run(101).await.unwrap();
                assert!(log.lock().unwrap().samples.is_empty());
                let accepted = service
                    .active_audio
                    .read()
                    .await
                    .as_ref()
                    .unwrap()
                    .1
                    .report(AudioDrainReason::Drained);
                assert_eq!(accepted.accepted_bytes, samples as u64 * 2);
                assert_eq!(accepted.submitted_bytes, 0);
                // Release session lookup before polling Pause. On this current-
                // thread test no processor can run between drop and that poll.
                assert!(service.audio_processor_task.read().await.is_some());
                drop(provider);
                let pause = service.pause_for_continuation(101, stopped);
                tokio::pin!(pause);
                assert!(futures_util::poll!(pause.as_mut()).is_pending());
                // take() occurs inside the actual drain call, before its select
                // registers the deadline. Pending + None proves drain entry,
                // rather than a wait on the provider/session lock.
                assert!(service.audio_processor_task.read().await.is_none());
                assert!(log.lock().unwrap().samples.is_empty());
                let drain_entered = Instant::now();
                let token = prepare_continued_b(&service).await;
                let b = chunks.lock().unwrap().as_ref().unwrap().clone();
                b(AudioChunk::new(vec![2400; 480], 16_000, 1));
                service.seal_prepared_capture(token).await.unwrap();
                // Auto-advance visits each per-frame Tokio sleep; no bulk time
                // jump can substitute for 400/800 completed ordered sends.
                let result = tokio::time::timeout(Duration::from_secs(12), pause)
                    .await
                    .unwrap();
                assert_eq!(log.lock().unwrap().samples, expected_a);
                if expired {
                    assert!(drain_entered.elapsed() >= Duration::from_secs(seconds as u64));
                    assert!(result.unwrap_err().to_string().contains("window expired"));
                    assert!(log.lock().unwrap().operations.is_empty());
                    assert!(service.paused_continuation_snapshot().await.is_none());
                } else {
                    assert!(result.is_ok());
                    assert_eq!(log.lock().unwrap().pause_sample_counts, vec![samples]);
                    assert!(matches!(
                        log.lock().unwrap().operations.as_slice(),
                        [crate::domain::ContinuationOperation::Pause { .. }]
                    ));
                }
                {
                    let slot = service.prepared_capture.lock().await;
                    let pending = slot.as_ref().unwrap();
                    assert_eq!(pending.token, token);
                    assert_eq!(
                        pending
                            .accounting
                            .report(AudioDrainReason::Drained)
                            .accepted_bytes,
                        960
                    );
                    assert_eq!(pending.rx.len(), 1);
                    assert!(!pending.overflowed.load(Ordering::Acquire));
                }
                // Expired-window fallback is ordinary A finalization with B
                // retained for cold connect; it never sends a stale Pause.
                service.finalize_provider_for_run(101).await.unwrap();
                let report = service.completed_report_for_run(101).await.unwrap();
                assert_eq!(
                    (
                        report.audio.accepted_bytes,
                        report.audio.submitted_bytes,
                        report.audio.unknown_bytes
                    ),
                    (samples as u64 * 2, samples as u64 * 2, 0)
                );
                assert_eq!(log.lock().unwrap().samples.len(), samples);
                assert!(service.prepared_capture.lock().await.is_some());
                if expired {
                    service
                        .connect_prepared_recording(
                            token,
                            Arc::new(|_| {}),
                            Arc::new(|_| {}),
                            Arc::new(|_, _| {}),
                            Arc::new(|_, _| {}),
                            Arc::new(|_| {}),
                            Arc::new(|_, _| {}),
                            Default::default(),
                        )
                        .await
                        .unwrap();
                    service.finalize_provider_for_run(102).await.unwrap();
                    let mut expected = expected_a.clone();
                    expected.extend(vec![2400; 480]);
                    assert_eq!(log.lock().unwrap().samples, expected);
                    assert_eq!(log.lock().unwrap().starts, 2);
                    assert!(log.lock().unwrap().operations.is_empty());
                } else {
                    service.cancel_prepared_capture(token).await.unwrap();
                }
                assert!(service.prepared_capture.lock().await.is_none());
                assert_eq!(service.logical_provider_run_id(), 0);
            }
        }
    }

    #[tokio::test]
    async fn continuation_service_cancel_during_accepted_restores_without_b_audio() {
        let release = Arc::new(tokio::sync::Notify::new());
        let (service, log, entered, _) =
            continuation_service_fixture(false, 0, Some(release.clone())).await;
        service.stop_capture_for_run(101).await.unwrap();
        let paused = service
            .pause_for_continuation(101, Instant::now())
            .await
            .unwrap();
        let token = prepare_continued_b(&service).await;
        let work = service.clone();
        let task = tokio::spawn(async move {
            work.continue_prepared_capture(
                token,
                paused,
                Instant::now(),
                Arc::new(AtomicBool::new(false)),
            )
            .await
        });
        entered.notified().await;
        service.cancel_prepared_capture(token).await.unwrap();
        release.notify_one();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            ContinueCaptureOutcome::Cancelled
        );
        assert_eq!(log.lock().unwrap().samples.len(), 480);
        assert_eq!(log.lock().unwrap().starts, 1);
        assert!(matches!(
            log.lock().unwrap().operations.last(),
            Some(crate::domain::ContinuationOperation::Restore { .. })
        ));
        assert!(service.completed_report_for_run(101).await.is_none());
        tokio::time::timeout(
            Duration::from_secs(1),
            service.finalize_provider_for_run(101),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(service.prepared_capture.lock().await.is_none());
        assert!(service.stt_provider.read().await.is_none());
        assert!(!service.audio_capture.read().await.is_capturing());
        assert_eq!(service.logical_provider_run_id(), 0);
    }

    #[tokio::test]
    async fn continuation_service_first_write_crosses_grace_with_one_bounded_owner() {
        let release = Arc::new(tokio::sync::Notify::new());
        let (service, log, _, _) =
            continuation_service_fixture(false, 3, Some(release.clone())).await;
        service.stop_capture_for_run(101).await.unwrap();
        let paused = service
            .pause_for_continuation(101, Instant::now())
            .await
            .unwrap();
        let deadline = paused.deadline();
        let token = prepare_continued_b(&service).await;
        let work = service.clone();
        let task = tokio::spawn(async move {
            work.continue_prepared_capture(
                token,
                paused,
                Instant::now(),
                Arc::new(AtomicBool::new(false)),
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while log.lock().unwrap().samples.len() != 960 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        // Physical Stop must not wait for the blocked first-B provider write.
        tokio::time::timeout(
            Duration::from_millis(100),
            service.stop_capture_for_run(token.run_id),
        )
        .await
        .unwrap()
        .unwrap();
        tokio::time::sleep_until(deadline + Duration::from_millis(20)).await;
        assert!(
            !task.is_finished(),
            "grace expiry must not cancel an already attempted write"
        );
        assert_eq!((log.lock().unwrap().stops), 0);
        release.notify_one();
        assert!(matches!(
            task.await.unwrap().unwrap(),
            ContinueCaptureOutcome::Attached {
                logical_run_id: 101,
                ..
            }
        ));
        assert!(!service.audio_capture.read().await.is_capturing());
        service.finalize_provider_for_run(101).await.unwrap();
        assert_eq!(
            service
                .completed_report_for_run(101)
                .await
                .unwrap()
                .audio
                .unknown_bytes,
            0
        );
        assert_eq!(log.lock().unwrap().samples.len(), 960);
    }

    #[tokio::test]
    async fn continuation_service_known_not_started_restores_prefix_without_double_accounting() {
        let (service, log, _, _) = continuation_service_fixture(false, 2, None).await;
        service.stop_capture_for_run(101).await.unwrap();
        let paused = service
            .pause_for_continuation(101, Instant::now())
            .await
            .unwrap();
        let token = prepare_continued_b(&service).await;
        service.seal_prepared_capture(token).await.unwrap();
        assert_eq!(
            service
                .continue_prepared_capture(
                    token,
                    paused,
                    Instant::now(),
                    Arc::new(AtomicBool::new(false))
                )
                .await
                .unwrap(),
            ContinueCaptureOutcome::Unsent(ContinuationRefusal::Unavailable)
        );
        {
            let slot = service.prepared_capture.lock().await;
            let prepared = slot.as_ref().unwrap();
            assert_eq!(prepared.prefetched.as_ref().unwrap().data, vec![1200; 480]);
            let report = prepared.accounting.report(AudioDrainReason::Drained);
            assert_eq!(
                (
                    report.read_bytes,
                    report.submitted_bytes,
                    report.unknown_bytes
                ),
                (0, 0, 0)
            );
        }
        assert_eq!(log.lock().unwrap().samples.len(), 480);
        service.finalize_provider_for_run(101).await.unwrap();
        assert_eq!(
            service
                .completed_report_for_run(101)
                .await
                .unwrap()
                .audio
                .accepted_bytes,
            960
        );
        service
            .connect_prepared_recording(
                token,
                Arc::new(|_| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
                Arc::new(AtomicBool::new(false)),
            )
            .await
            .unwrap();
        service.finalize_provider_for_run(102).await.unwrap();
        let report = service.completed_report_for_run(102).await.unwrap();
        assert_eq!(
            (
                report.audio.read_bytes,
                report.audio.submitted_bytes,
                report.audio.unknown_bytes
            ),
            (960, 960, 0)
        );
        assert_eq!(log.lock().unwrap().samples, vec![1200; 960]);
        assert_eq!(log.lock().unwrap().starts, 2);
    }

    #[tokio::test]
    async fn continuation_service_attempted_unknown_b_cannot_be_replayed() {
        let (service, log, _, _) = continuation_service_fixture(false, 1, None).await;
        service.stop_capture_for_run(101).await.unwrap();
        let paused = service
            .pause_for_continuation(101, Instant::now())
            .await
            .unwrap();
        let token = prepare_continued_b(&service).await;
        assert!(service
            .continue_prepared_capture(
                token,
                paused,
                Instant::now(),
                Arc::new(AtomicBool::new(false))
            )
            .await
            .is_err());
        assert!(service.prepared_capture.lock().await.is_none());
        assert!(!service.audio_capture.read().await.is_capturing());
        assert_eq!(log.lock().unwrap().samples.len(), 960);
        assert!(!log
            .lock()
            .unwrap()
            .operations
            .iter()
            .any(|op| matches!(op, crate::domain::ContinuationOperation::Restore { .. })));
        assert!(service.finalize_provider_for_run(101).await.is_err());
        let report = service.completed_report_for_run(101).await.unwrap();
        assert_eq!(
            (
                report.audio.accepted_bytes,
                report.audio.submitted_bytes,
                report.audio.unknown_bytes
            ),
            (1920, 960, 960)
        );
        assert_eq!(log.lock().unwrap().starts, 1);
    }

    struct CountingProvider {
        sent_chunks: Arc<AtomicUsize>,
        stopped: Arc<AtomicBool>,
        delay_per_chunk: Duration,
        start_stream_delay: Duration,
    }

    #[async_trait]
    impl SttProvider for CountingProvider {
        async fn initialize(&mut self, _config: &SttConfig) -> SttResult<()> {
            Ok(())
        }

        async fn start_stream(
            &mut self,
            _on_partial: TranscriptionCallback,
            _on_final: TranscriptionCallback,
            _on_error: ErrorCallback,
            _on_connection_quality: ConnectionQualityCallback,
        ) -> SttResult<()> {
            if !self.start_stream_delay.is_zero() {
                tokio::time::sleep(self.start_stream_delay).await;
            }
            Ok(())
        }

        async fn send_audio(&mut self, _chunk: &crate::domain::AudioChunk) -> SttResult<()> {
            tokio::time::sleep(self.delay_per_chunk).await;
            self.sent_chunks.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn stop_stream(&mut self) -> SttResult<()> {
            self.stopped.store(true, Ordering::SeqCst);
            Ok(())
        }

        async fn abort(&mut self) -> SttResult<()> {
            Ok(())
        }

        fn name(&self) -> &str {
            "counting_provider"
        }

        fn is_online(&self) -> bool {
            true
        }
    }

    struct CountingFactory {
        sent_chunks: Arc<AtomicUsize>,
        stopped: Arc<AtomicBool>,
        delay_per_chunk: Duration,
        start_stream_delay: Duration,
    }

    struct ConfigRecordingFactory {
        languages: Arc<StdMutex<Vec<String>>>,
    }

    impl SttProviderFactory for ConfigRecordingFactory {
        fn create(&self, config: &SttConfig) -> SttResult<Box<dyn SttProvider>> {
            self.languages
                .lock()
                .expect("languages mutex poisoned")
                .push(config.language.clone());
            Ok(Box::new(CountingProvider {
                sent_chunks: Arc::new(AtomicUsize::new(0)),
                stopped: Arc::new(AtomicBool::new(false)),
                delay_per_chunk: Duration::ZERO,
                start_stream_delay: Duration::ZERO,
            }))
        }
    }

    impl SttProviderFactory for CountingFactory {
        fn create(&self, _config: &SttConfig) -> SttResult<Box<dyn SttProvider>> {
            Ok(Box::new(CountingProvider {
                sent_chunks: self.sent_chunks.clone(),
                stopped: self.stopped.clone(),
                delay_per_chunk: self.delay_per_chunk,
                start_stream_delay: self.start_stream_delay,
            }))
        }
    }

    struct KeepAliveProvider {
        paused_count: Arc<AtomicUsize>,
        resumed_count: Arc<AtomicUsize>,
        stopped_count: Arc<AtomicUsize>,
        aborted_count: Arc<AtomicUsize>,
        pause_entered: Option<Arc<tokio::sync::Notify>>,
        pause_release: Option<Arc<tokio::sync::Notify>>,
        is_paused: bool,
        is_closed: bool,
    }

    #[async_trait]
    impl SttProvider for KeepAliveProvider {
        async fn initialize(&mut self, _config: &SttConfig) -> SttResult<()> {
            Ok(())
        }

        async fn start_stream(
            &mut self,
            _on_partial: TranscriptionCallback,
            _on_final: TranscriptionCallback,
            _on_error: ErrorCallback,
            _on_connection_quality: ConnectionQualityCallback,
        ) -> SttResult<()> {
            self.is_paused = false;
            self.is_closed = false;
            Ok(())
        }

        async fn send_audio(&mut self, _chunk: &crate::domain::AudioChunk) -> SttResult<()> {
            Ok(())
        }

        async fn stop_stream(&mut self) -> SttResult<()> {
            self.stopped_count.fetch_add(1, Ordering::SeqCst);
            self.is_paused = false;
            self.is_closed = true;
            Ok(())
        }

        async fn abort(&mut self) -> SttResult<()> {
            self.aborted_count.fetch_add(1, Ordering::SeqCst);
            self.is_paused = false;
            self.is_closed = true;
            Ok(())
        }

        async fn pause_stream(&mut self) -> SttResult<()> {
            self.paused_count.fetch_add(1, Ordering::SeqCst);
            if let Some(entered) = &self.pause_entered {
                entered.notify_one();
            }
            if let Some(release) = &self.pause_release {
                release.notified().await;
            }
            self.is_paused = true;
            Ok(())
        }

        async fn resume_stream(
            &mut self,
            _on_partial: TranscriptionCallback,
            _on_final: TranscriptionCallback,
            _on_error: ErrorCallback,
            _on_connection_quality: ConnectionQualityCallback,
        ) -> SttResult<()> {
            self.resumed_count.fetch_add(1, Ordering::SeqCst);
            self.is_paused = false;
            Ok(())
        }

        fn name(&self) -> &str {
            "keep_alive_provider"
        }

        fn supports_keep_alive(&self) -> bool {
            true
        }

        fn is_connection_alive(&self) -> bool {
            self.is_paused && !self.is_closed
        }

        fn is_online(&self) -> bool {
            true
        }
    }

    struct KeepAliveFactory {
        selected_providers: Arc<std::sync::Mutex<Vec<crate::domain::BackendStreamingProvider>>>,
        paused_count: Arc<AtomicUsize>,
        resumed_count: Arc<AtomicUsize>,
        stopped_count: Arc<AtomicUsize>,
        aborted_count: Arc<AtomicUsize>,
        pause_entered: Option<Arc<tokio::sync::Notify>>,
        pause_release: Option<Arc<tokio::sync::Notify>>,
    }

    impl SttProviderFactory for KeepAliveFactory {
        fn create(&self, config: &SttConfig) -> SttResult<Box<dyn SttProvider>> {
            self.selected_providers
                .lock()
                .expect("selected providers mutex poisoned")
                .push(config.backend_streaming_provider);

            Ok(Box::new(KeepAliveProvider {
                paused_count: self.paused_count.clone(),
                resumed_count: self.resumed_count.clone(),
                stopped_count: self.stopped_count.clone(),
                aborted_count: self.aborted_count.clone(),
                pause_entered: self.pause_entered.clone(),
                pause_release: self.pause_release.clone(),
                is_paused: false,
                is_closed: false,
            }))
        }
    }

    #[test]
    fn audio_session_stats_flags_long_low_signal_session() {
        let mut stats = AudioSessionStats::default();
        for _ in 0..AUDIO_LOW_SIGNAL_MIN_CHUNKS {
            stats.observe(120, 480, 480, 4.0);
        }

        assert!(stats.looks_too_quiet_for_stt());
        assert_eq!(stats.peak_raw_amplitude, 120);
        assert_eq!(stats.peak_sent_amplitude, 480);
        assert_eq!(stats.chunks_above_sent_speech_floor, 0);
    }

    #[test]
    fn audio_session_stats_does_not_flag_speech_like_peak() {
        let mut stats = AudioSessionStats::default();
        for _ in 0..(AUDIO_LOW_SIGNAL_MIN_CHUNKS - 1) {
            stats.observe(120, 480, 480, 4.0);
        }
        stats.observe(1200, 4800, 480, 4.0);

        assert!(!stats.looks_too_quiet_for_stt());
        assert_eq!(stats.peak_raw_amplitude, 1200);
        assert_eq!(stats.peak_sent_amplitude, 4800);
        assert_eq!(stats.chunks_above_sent_speech_floor, 1);
    }

    #[test]
    fn audio_session_stats_does_not_warn_for_short_prebuffer() {
        let mut stats = AudioSessionStats::default();
        for _ in 0..(AUDIO_LOW_SIGNAL_MIN_CHUNKS - 1) {
            stats.observe(0, 0, 480, 4.0);
        }

        assert!(!stats.looks_too_quiet_for_stt());
    }

    #[test]
    fn backend_dictation_keep_alive_policy_tracks_provider_capability() {
        let mut deepgram = SttConfig::new(SttProviderType::Backend);
        deepgram.backend_streaming_provider = crate::domain::BackendStreamingProvider::Deepgram;
        deepgram.keep_connection_alive = false;
        deepgram.keep_alive_ttl_secs = 10;

        assert!(apply_backend_dictation_keep_alive_policy(&mut deepgram));
        assert!(deepgram.keep_connection_alive);
        assert_eq!(
            deepgram.keep_alive_ttl_secs,
            crate::domain::BACKEND_KEEPALIVE_TTL_SECS
        );

        let mut elevenlabs = deepgram;
        elevenlabs.backend_streaming_provider = crate::domain::BackendStreamingProvider::ElevenLabs;

        assert!(apply_backend_dictation_keep_alive_policy(&mut elevenlabs));
        assert!(!elevenlabs.keep_connection_alive);
        assert!(!apply_backend_dictation_keep_alive_policy(&mut elevenlabs));
    }

    #[test]
    fn connection_identity_excludes_keep_alive_tuning_but_includes_auth_context() {
        let mut previous = SttConfig::new(SttProviderType::Backend);
        previous.backend_auth_token = Some("old-token".to_string());
        previous.backend_url = Some("wss://old.example.test".to_string());
        let mut next = previous.clone();
        next.keep_connection_alive = !previous.keep_connection_alive;
        next.keep_alive_ttl_secs = previous.keep_alive_ttl_secs.saturating_add(1);

        assert!(!config_requires_new_connection(&previous, &next));

        next.backend_auth_token = Some("new-token".to_string());
        assert!(config_requires_new_connection(&previous, &next));
        next.backend_auth_token = previous.backend_auth_token.clone();
        next.backend_url = Some("wss://new.example.test".to_string());
        assert!(config_requires_new_connection(&previous, &next));
    }

    #[tokio::test]
    async fn backend_deepgram_reuses_keep_alive_between_recordings() {
        let on_chunk_slot: Arc<std::sync::Mutex<Option<crate::domain::AudioChunkCallback>>> =
            Arc::new(std::sync::Mutex::new(None));
        let selected_providers = Arc::new(std::sync::Mutex::new(Vec::new()));
        let paused_count = Arc::new(AtomicUsize::new(0));
        let resumed_count = Arc::new(AtomicUsize::new(0));
        let stopped_count = Arc::new(AtomicUsize::new(0));
        let aborted_count = Arc::new(AtomicUsize::new(0));

        let audio_capture = ManualAudioCapture::new(on_chunk_slot);
        let factory = Arc::new(KeepAliveFactory {
            selected_providers: selected_providers.clone(),
            paused_count: paused_count.clone(),
            resumed_count: resumed_count.clone(),
            stopped_count: stopped_count.clone(),
            aborted_count: aborted_count.clone(),
            pause_entered: None,
            pause_release: None,
        });
        let service = TranscriptionService::new(Box::new(audio_capture), factory);

        let mut initial = SttConfig::new(SttProviderType::Backend);
        initial.backend_streaming_provider = crate::domain::BackendStreamingProvider::Deepgram;
        initial.keep_connection_alive = true;
        service.update_config(initial.clone()).await.unwrap();

        let on_partial: TranscriptionCallback = Arc::new(|_t| {});
        let on_final: TranscriptionCallback = Arc::new(|_t| {});
        let on_audio_level: AudioLevelCallback = Arc::new(|_, _l| {});
        let on_audio_spectrum: AudioSpectrumCallback = Arc::new(|_, _b| {});
        let on_error: ErrorCallback = Arc::new(|_err: SttError| {});
        let on_quality: ConnectionQualityCallback = Arc::new(|_q, _r| {});

        service
            .start_recording(
                on_partial.clone(),
                on_final.clone(),
                on_audio_level.clone(),
                on_audio_spectrum.clone(),
                on_error.clone(),
                on_quality.clone(),
            )
            .await
            .expect("first recording must start");
        service
            .stop_recording()
            .await
            .expect("first recording must stop");

        assert_eq!(paused_count.load(Ordering::SeqCst), 1);
        assert_eq!(resumed_count.load(Ordering::SeqCst), 0);
        assert_eq!(stopped_count.load(Ordering::SeqCst), 0);
        assert!(service.can_resume_keep_alive_connection().await);

        service
            .start_recording(
                on_partial,
                on_final,
                on_audio_level,
                on_audio_spectrum,
                on_error,
                on_quality,
            )
            .await
            .expect("second recording must resume the warm provider");
        service
            .stop_recording()
            .await
            .expect("second recording must stop");

        assert_eq!(paused_count.load(Ordering::SeqCst), 2);
        assert_eq!(resumed_count.load(Ordering::SeqCst), 1);
        assert_eq!(stopped_count.load(Ordering::SeqCst), 0);
        assert_eq!(aborted_count.load(Ordering::SeqCst), 0);
        assert!(service.can_resume_keep_alive_connection().await);

        let selected = selected_providers
            .lock()
            .expect("selected providers mutex poisoned")
            .clone();
        assert_eq!(
            selected,
            vec![crate::domain::BackendStreamingProvider::Deepgram]
        );

        let mut next = initial;
        next.backend_auth_token = Some("rotated-token".to_string());
        service.update_config(next).await.unwrap();

        assert_eq!(stopped_count.load(Ordering::SeqCst), 1);
        assert_eq!(aborted_count.load(Ordering::SeqCst), 0);
        assert!(!service.can_resume_keep_alive_connection().await);
    }

    #[tokio::test]
    async fn backend_auth_change_during_recording_forces_close_before_next_session() {
        let on_chunk_slot: Arc<std::sync::Mutex<Option<crate::domain::AudioChunkCallback>>> =
            Arc::new(std::sync::Mutex::new(None));
        let selected_providers = Arc::new(std::sync::Mutex::new(Vec::new()));
        let paused_count = Arc::new(AtomicUsize::new(0));
        let resumed_count = Arc::new(AtomicUsize::new(0));
        let stopped_count = Arc::new(AtomicUsize::new(0));
        let aborted_count = Arc::new(AtomicUsize::new(0));
        let service = TranscriptionService::new(
            Box::new(ManualAudioCapture::new(on_chunk_slot)),
            Arc::new(KeepAliveFactory {
                selected_providers: selected_providers.clone(),
                paused_count: paused_count.clone(),
                resumed_count: resumed_count.clone(),
                stopped_count: stopped_count.clone(),
                aborted_count: aborted_count.clone(),
                pause_entered: None,
                pause_release: None,
            }),
        );

        let mut initial = SttConfig::new(SttProviderType::Backend);
        initial.backend_streaming_provider = crate::domain::BackendStreamingProvider::Deepgram;
        initial.backend_auth_token = Some("old-token".to_string());
        initial.keep_connection_alive = true;
        service.update_config(initial.clone()).await.unwrap();

        service
            .start_recording(
                Arc::new(|_| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
            )
            .await
            .unwrap();
        initial.backend_auth_token = Some("new-token".to_string());
        service.update_config(initial).await.unwrap();
        service.stop_recording().await.unwrap();

        assert_eq!(paused_count.load(Ordering::SeqCst), 0);
        assert_eq!(resumed_count.load(Ordering::SeqCst), 0);
        assert_eq!(stopped_count.load(Ordering::SeqCst), 1);
        assert_eq!(aborted_count.load(Ordering::SeqCst), 0);
        assert!(!service.can_resume_keep_alive_connection().await);

        service
            .start_recording(
                Arc::new(|_| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
            )
            .await
            .unwrap();
        service.stop_recording().await.unwrap();

        assert_eq!(paused_count.load(Ordering::SeqCst), 1);
        assert_eq!(resumed_count.load(Ordering::SeqCst), 0);
        assert_eq!(
            selected_providers
                .lock()
                .expect("selected providers mutex poisoned")
                .as_slice(),
            &[
                crate::domain::BackendStreamingProvider::Deepgram,
                crate::domain::BackendStreamingProvider::Deepgram,
            ]
        );
    }

    #[tokio::test]
    async fn invalidated_warm_connection_is_not_reused_after_startup_failure() {
        let selected_providers = Arc::new(std::sync::Mutex::new(Vec::new()));
        let paused_count = Arc::new(AtomicUsize::new(0));
        let resumed_count = Arc::new(AtomicUsize::new(0));
        let stopped_count = Arc::new(AtomicUsize::new(0));
        let aborted_count = Arc::new(AtomicUsize::new(0));
        let service = Arc::new(TranscriptionService::new(
            Box::new(ManualAudioCapture::new(Arc::new(std::sync::Mutex::new(
                None,
            )))),
            Arc::new(KeepAliveFactory {
                selected_providers: selected_providers.clone(),
                paused_count: paused_count.clone(),
                resumed_count: resumed_count.clone(),
                stopped_count: stopped_count.clone(),
                aborted_count: aborted_count.clone(),
                pause_entered: None,
                pause_release: None,
            }),
        ));
        let mut config = SttConfig::new(SttProviderType::Backend);
        config.backend_auth_token = Some("old-token".to_string());
        config.keep_connection_alive = true;
        service.update_config(config.clone()).await.unwrap();

        service
            .start_recording(
                Arc::new(|_| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
            )
            .await
            .unwrap();
        service.stop_recording().await.unwrap();
        assert!(service.can_resume_keep_alive_connection().await);

        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        service
            .replace_audio_capture(Box::new(BlockingStartAudioCapture {
                config: AudioConfig::default(),
                entered: entered.clone(),
                release: release.clone(),
                is_capturing: Arc::new(AtomicBool::new(false)),
                fail_after_release: true,
            }))
            .await
            .unwrap();
        let start_service = service.clone();
        let start_task = tokio::spawn(async move {
            start_service
                .start_recording(
                    Arc::new(|_| {}),
                    Arc::new(|_| {}),
                    Arc::new(|_, _| {}),
                    Arc::new(|_, _| {}),
                    Arc::new(|_| {}),
                    Arc::new(|_, _| {}),
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), entered.notified())
            .await
            .expect("capture startup must reach the test gate");
        config.backend_auth_token = Some("new-token".to_string());
        service.update_config(config).await.unwrap();
        release.notify_one();
        assert!(start_task.await.unwrap().is_err());

        service
            .replace_audio_capture(Box::new(ManualAudioCapture::new(Arc::new(
                std::sync::Mutex::new(None),
            ))))
            .await
            .unwrap();
        service
            .start_recording(
                Arc::new(|_| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
            )
            .await
            .unwrap();

        assert_eq!(resumed_count.load(Ordering::SeqCst), 0);
        assert_eq!(
            aborted_count.load(Ordering::SeqCst) + stopped_count.load(Ordering::SeqCst),
            1,
            "the invalidated warm provider must be explicitly closed exactly once"
        );
        assert_eq!(selected_providers.lock().unwrap().len(), 2);
        service.stop_recording().await.unwrap();
        assert_eq!(paused_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn auth_update_cannot_publish_a_stale_provider_during_pause() {
        let pause_entered = Arc::new(tokio::sync::Notify::new());
        let pause_release = Arc::new(tokio::sync::Notify::new());
        let selected_providers = Arc::new(std::sync::Mutex::new(Vec::new()));
        let paused_count = Arc::new(AtomicUsize::new(0));
        let resumed_count = Arc::new(AtomicUsize::new(0));
        let stopped_count = Arc::new(AtomicUsize::new(0));
        let service = Arc::new(TranscriptionService::new(
            Box::new(ManualAudioCapture::new(Arc::new(std::sync::Mutex::new(
                None,
            )))),
            Arc::new(KeepAliveFactory {
                selected_providers: selected_providers.clone(),
                paused_count: paused_count.clone(),
                resumed_count: resumed_count.clone(),
                stopped_count: stopped_count.clone(),
                aborted_count: Arc::new(AtomicUsize::new(0)),
                pause_entered: Some(pause_entered.clone()),
                pause_release: Some(pause_release.clone()),
            }),
        ));
        let mut config = SttConfig::new(SttProviderType::Backend);
        config.backend_auth_token = Some("old-token".to_string());
        config.keep_connection_alive = true;
        service.update_config(config.clone()).await.unwrap();
        service
            .start_recording(
                Arc::new(|_| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
            )
            .await
            .unwrap();

        let stop_service = service.clone();
        let stop_task = tokio::spawn(async move { stop_service.stop_recording().await });
        tokio::time::timeout(Duration::from_secs(1), pause_entered.notified())
            .await
            .expect("pause must reach the test gate");

        config.backend_auth_token = Some("new-token".to_string());
        let update_service = service.clone();
        let update_task = tokio::spawn(async move { update_service.update_config(config).await });
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert!(!update_task.is_finished());

        let start_service = service.clone();
        let start_task = tokio::spawn(async move {
            start_service
                .start_recording(
                    Arc::new(|_| {}),
                    Arc::new(|_| {}),
                    Arc::new(|_, _| {}),
                    Arc::new(|_, _| {}),
                    Arc::new(|_| {}),
                    Arc::new(|_, _| {}),
                )
                .await
        });

        pause_release.notify_one();
        stop_task.await.unwrap().unwrap();
        update_task.await.unwrap().unwrap();
        start_task.await.unwrap().unwrap();

        assert_eq!(paused_count.load(Ordering::SeqCst), 1);
        assert_eq!(stopped_count.load(Ordering::SeqCst), 1);
        assert_eq!(resumed_count.load(Ordering::SeqCst), 0);
        assert_eq!(selected_providers.lock().unwrap().len(), 2);

        let mut cleanup_config = service.get_config().await;
        cleanup_config.keep_connection_alive = false;
        service.update_config(cleanup_config).await.unwrap();
        service.stop_recording().await.unwrap();
        assert_eq!(stopped_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn config_saved_during_starting_applies_only_to_next_recording() {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let languages = Arc::new(StdMutex::new(Vec::new()));
        let service = Arc::new(TranscriptionService::new(
            Box::new(BlockingStartAudioCapture {
                config: AudioConfig::default(),
                entered: entered.clone(),
                release: release.clone(),
                is_capturing: Arc::new(AtomicBool::new(false)),
                fail_after_release: false,
            }),
            Arc::new(ConfigRecordingFactory {
                languages: languages.clone(),
            }),
        ));

        let mut initial = SttConfig::new(SttProviderType::Deepgram);
        initial.language = "en".to_string();
        service.update_config(initial).await.unwrap();

        let start_service = service.clone();
        let start_task = tokio::spawn(async move {
            start_service
                .start_recording(
                    Arc::new(|_| {}),
                    Arc::new(|_| {}),
                    Arc::new(|_, _| {}),
                    Arc::new(|_, _| {}),
                    Arc::new(|_| {}),
                    Arc::new(|_, _| {}),
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), entered.notified())
            .await
            .expect("audio capture startup must block at the test gate");

        let mut next = SttConfig::new(SttProviderType::Deepgram);
        next.language = "fr".to_string();
        service.update_config(next).await.unwrap();
        release.notify_waiters();

        start_task
            .await
            .expect("start task must join")
            .expect("recording must start");
        assert_eq!(
            languages
                .lock()
                .expect("languages mutex poisoned")
                .as_slice(),
            &["en".to_string()]
        );
        service.stop_recording().await.unwrap();
        assert_eq!(service.get_config().await.language, "fr");
    }

    #[tokio::test]
    async fn replacing_audio_capture_serializes_on_the_physical_capture_slot() {
        let service = Arc::new(TranscriptionService::new(
            Box::new(FailingStartAudioCapture::default()),
            Arc::new(TestFactory {
                panic_on_send: false,
                aborted: Arc::new(AtomicBool::new(false)),
            }),
        ));
        let audio_guard = service.audio_capture.write().await;
        let replace_service = service.clone();
        let replace_task = tokio::spawn(async move {
            replace_service
                .replace_audio_capture(Box::new(FailingStartAudioCapture::default()))
                .await
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(50), service.status.write())
                .await
                .is_ok(),
            "provider status must remain independent while replacement waits for the device"
        );

        drop(audio_guard);
        replace_task
            .await
            .expect("replace task must join")
            .expect("replacement must succeed");
        assert_eq!(service.get_status().await, RecordingStatus::Idle);
    }

    #[tokio::test]
    async fn start_recording_preserves_audio_captured_while_stt_stream_starts() {
        let sent_chunks = Arc::new(AtomicUsize::new(0));
        let provider_stopped = Arc::new(AtomicBool::new(false));

        let audio_capture = ImmediateAudioCapture::new();
        let factory = Arc::new(CountingFactory {
            sent_chunks: sent_chunks.clone(),
            stopped: provider_stopped.clone(),
            delay_per_chunk: Duration::from_millis(0),
            start_stream_delay: Duration::from_millis(75),
        });
        let service = TranscriptionService::new(Box::new(audio_capture), factory);

        let on_partial: TranscriptionCallback = Arc::new(|_t| {});
        let on_final: TranscriptionCallback = Arc::new(|_t| {});
        let on_audio_level: AudioLevelCallback = Arc::new(|_, _l| {});
        let on_audio_spectrum: AudioSpectrumCallback = Arc::new(|_, _b| {});
        let on_error: ErrorCallback = Arc::new(|_err: SttError| {});
        let on_quality: ConnectionQualityCallback = Arc::new(|_q, _r| {});

        service
            .start_recording(
                on_partial,
                on_final,
                on_audio_level,
                on_audio_spectrum,
                on_error,
                on_quality,
            )
            .await
            .expect("recording must start");

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if sent_chunks.load(Ordering::SeqCst) >= 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("prebuffered audio must be sent after STT stream starts");

        service
            .stop_recording()
            .await
            .expect("recording must stop cleanly");
    }

    #[tokio::test]
    async fn start_recording_emits_audio_spectrum_while_stt_stream_starts() {
        let sent_chunks = Arc::new(AtomicUsize::new(0));
        let provider_stopped = Arc::new(AtomicBool::new(false));

        let audio_capture = ImmediateAudioCapture::new();
        let factory = Arc::new(CountingFactory {
            sent_chunks,
            stopped: provider_stopped,
            delay_per_chunk: Duration::from_millis(0),
            start_stream_delay: Duration::from_millis(500),
        });
        let service = Arc::new(TranscriptionService::new(Box::new(audio_capture), factory));

        let (spectrum_tx, spectrum_rx) = tokio::sync::oneshot::channel::<()>();
        let spectrum_tx = Arc::new(std::sync::Mutex::new(Some(spectrum_tx)));

        let on_partial: TranscriptionCallback = Arc::new(|_t| {});
        let on_final: TranscriptionCallback = Arc::new(|_t| {});
        let on_audio_level: AudioLevelCallback = Arc::new(|_, _l| {});
        let on_audio_spectrum: AudioSpectrumCallback = Arc::new(move |_, _b| {
            if let Some(tx) = spectrum_tx
                .lock()
                .expect("spectrum signal mutex poisoned")
                .take()
            {
                let _ = tx.send(());
            }
        });
        let on_error: ErrorCallback = Arc::new(|_err: SttError| {});
        let on_quality: ConnectionQualityCallback = Arc::new(|_q, _r| {});

        let service_for_start = service.clone();
        let start_task = tokio::spawn(async move {
            service_for_start
                .start_recording(
                    on_partial,
                    on_final,
                    on_audio_level,
                    on_audio_spectrum,
                    on_error,
                    on_quality,
                )
                .await
        });

        tokio::time::timeout(Duration::from_millis(250), spectrum_rx)
            .await
            .expect("prestart spectrum must be emitted before STT stream is ready")
            .expect("prestart spectrum signal");

        start_task
            .await
            .expect("start task must not panic")
            .expect("recording must start");

        service
            .stop_recording()
            .await
            .expect("recording must stop cleanly");
    }

    #[tokio::test]
    async fn stop_recording_drains_queued_audio_chunks_before_stopping_provider() {
        let on_chunk_slot: Arc<std::sync::Mutex<Option<crate::domain::AudioChunkCallback>>> =
            Arc::new(std::sync::Mutex::new(None));
        let sent_chunks = Arc::new(AtomicUsize::new(0));
        let provider_stopped = Arc::new(AtomicBool::new(false));

        let audio_capture = ManualAudioCapture::new(on_chunk_slot.clone());
        let factory = Arc::new(CountingFactory {
            sent_chunks: sent_chunks.clone(),
            stopped: provider_stopped.clone(),
            delay_per_chunk: Duration::from_millis(5),
            start_stream_delay: Duration::from_millis(0),
        });
        let service = TranscriptionService::new(Box::new(audio_capture), factory);

        let on_partial: TranscriptionCallback = Arc::new(|_t| {});
        let on_final: TranscriptionCallback = Arc::new(|_t| {});
        let on_audio_level: AudioLevelCallback = Arc::new(|_, _l| {});
        let on_audio_spectrum: AudioSpectrumCallback = Arc::new(|_, _b| {});
        let on_error: ErrorCallback = Arc::new(|_err: SttError| {});
        let on_quality: ConnectionQualityCallback = Arc::new(|_q, _r| {});

        service
            .start_recording(
                on_partial,
                on_final,
                on_audio_level,
                on_audio_spectrum,
                on_error,
                on_quality,
            )
            .await
            .expect("recording must start");

        const CHUNKS: usize = 48;
        {
            let callback = on_chunk_slot
                .lock()
                .expect("callback mutex poisoned")
                .clone()
                .expect("capture callback must be registered");

            for i in 0..CHUNKS {
                let sample = 1000 + i as i16;
                callback(crate::domain::AudioChunk::new(vec![sample; 480], 16_000, 1));
            }
        }

        service
            .stop_recording()
            .await
            .expect("recording must stop cleanly");

        assert_eq!(sent_chunks.load(Ordering::SeqCst), CHUNKS);
        assert!(provider_stopped.load(Ordering::SeqCst));
        assert_eq!(service.get_status().await, RecordingStatus::Idle);
    }

    #[tokio::test]
    async fn stops_recording_and_cleans_up_after_first_uncertain_send() {
        let provider_aborted = Arc::new(AtomicBool::new(false));
        let capture_stopped = Arc::new(AtomicBool::new(false));
        let got_poor_quality = Arc::new(AtomicBool::new(false));

        let audio_capture = BurstAudioCapture::new(capture_stopped.clone(), 32);
        let factory = Arc::new(TestFactory {
            panic_on_send: false,
            aborted: provider_aborted.clone(),
        });
        let service = TranscriptionService::new(Box::new(audio_capture), factory);

        let (err_tx, mut err_rx) = tokio::sync::mpsc::unbounded_channel::<(String, String)>();
        let on_error: ErrorCallback = Arc::new(move |err: SttError| {
            let typ = match &err {
                SttError::Connection(conn) => {
                    if conn.details.category == Some(crate::domain::SttConnectionCategory::Timeout)
                    {
                        "timeout"
                    } else {
                        "connection"
                    }
                }
                SttError::Authentication(_) => "authentication",
                SttError::Configuration(_) => "configuration",
                SttError::Processing(_)
                | SttError::Internal(_)
                | SttError::Unsupported(_)
                | SttError::ContinuationAudioNotStarted => "processing",
            }
            .to_string();
            let _ = err_tx.send((err.to_string(), typ));
        });

        let on_partial: TranscriptionCallback = Arc::new(|_t| {});
        let on_final: TranscriptionCallback = Arc::new(|_t| {});
        let on_audio_level: AudioLevelCallback = Arc::new(|_, _l| {});
        let on_audio_spectrum: AudioSpectrumCallback = Arc::new(|_, _b| {});
        let got_poor_quality_clone = got_poor_quality.clone();
        let on_quality: ConnectionQualityCallback = Arc::new(move |q, _r| {
            if q == "Poor" {
                got_poor_quality_clone.store(true, Ordering::SeqCst);
            }
        });

        service
            .start_recording(
                on_partial,
                on_final,
                on_audio_level,
                on_audio_spectrum,
                on_error,
                on_quality,
            )
            .await
            .expect("recording must start");

        // The first uncertain write terminates this transport without replay.
        let (_msg, typ) = tokio::time::timeout(Duration::from_secs(3), err_rx.recv())
            .await
            .expect("must not timeout waiting for error")
            .expect("must receive error payload");
        assert_eq!(typ, "connection");

        // И сервис обязан вернуться в Idle (иначе UI/хоткей могут залипнуть).
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if service.get_status().await == RecordingStatus::Idle {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("status must become Idle");

        assert!(capture_stopped.load(Ordering::SeqCst));
        assert!(provider_aborted.load(Ordering::SeqCst));
        assert!(got_poor_quality.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn zero_audio_fatal_error_aborts_provider() {
        let capture_stopped = Arc::new(AtomicBool::new(false));
        let selected_providers = Arc::new(std::sync::Mutex::new(Vec::new()));
        let paused_count = Arc::new(AtomicUsize::new(0));
        let stopped_count = Arc::new(AtomicUsize::new(0));
        let aborted_count = Arc::new(AtomicUsize::new(0));

        let audio_capture = BurstAudioCapture::new(capture_stopped.clone(), 260);
        let factory = Arc::new(KeepAliveFactory {
            selected_providers,
            paused_count,
            resumed_count: Arc::new(AtomicUsize::new(0)),
            stopped_count: stopped_count.clone(),
            aborted_count: aborted_count.clone(),
            pause_entered: None,
            pause_release: None,
        });
        let service = TranscriptionService::new(Box::new(audio_capture), factory);

        let (err_tx, mut err_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let on_error: ErrorCallback = Arc::new(move |err: SttError| {
            let _ = err_tx.send(err.to_string());
        });

        let on_partial: TranscriptionCallback = Arc::new(|_t| {});
        let on_final: TranscriptionCallback = Arc::new(|_t| {});
        let on_audio_level: AudioLevelCallback = Arc::new(|_, _l| {});
        let on_audio_spectrum: AudioSpectrumCallback = Arc::new(|_, _b| {});
        let on_quality: ConnectionQualityCallback = Arc::new(|_q, _r| {});

        service
            .start_recording(
                on_partial,
                on_final,
                on_audio_level,
                on_audio_spectrum,
                on_error,
                on_quality,
            )
            .await
            .expect("recording must start");

        let msg = tokio::time::timeout(Duration::from_secs(3), err_rx.recv())
            .await
            .expect("must not timeout waiting for zero-audio error")
            .expect("must receive zero-audio error");
        assert!(msg.contains("Нет аудиосигнала"));

        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if service.get_status().await == RecordingStatus::Idle {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("status must become Idle");

        assert!(capture_stopped.load(Ordering::SeqCst));
        assert_eq!(aborted_count.load(Ordering::SeqCst), 1);
        assert_eq!(stopped_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn provider_error_during_start_is_returned_without_false_recording_state() {
        let capture_stopped = Arc::new(AtomicBool::new(false));
        let provider_aborted = Arc::new(AtomicBool::new(false));
        let callback_errors = Arc::new(AtomicUsize::new(0));
        let service = TranscriptionService::new(
            Box::new(BurstAudioCapture::new(capture_stopped.clone(), 0)),
            Arc::new(StartupErrorFactory {
                aborted: provider_aborted.clone(),
            }),
        );

        let result = service
            .start_recording(
                Arc::new(|_| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                {
                    let callback_errors = callback_errors.clone();
                    Arc::new(move |_| {
                        callback_errors.fetch_add(1, Ordering::SeqCst);
                    })
                },
                Arc::new(|_, _| {}),
            )
            .await;

        let error = result.expect_err("startup receiver error must fail start_recording");
        assert!(error
            .downcast_ref::<SttError>()
            .expect("typed STT startup error")
            .to_string()
            .contains("simulated receiver failure during dictation startup"));
        assert_eq!(service.get_status().await, RecordingStatus::Idle);
        assert!(capture_stopped.load(Ordering::SeqCst));
        assert!(provider_aborted.load(Ordering::SeqCst));
        assert_eq!(callback_errors.load(Ordering::SeqCst), 0);
    }

    async fn runtime_failure_with_pending_capture_fixture() -> (
        TranscriptionService,
        u64,
        PreparedCaptureToken,
        Arc<AtomicBool>,
        Arc<AtomicBool>,
    ) {
        let capture_stopped = Arc::new(AtomicBool::new(false));
        let provider_aborted = Arc::new(AtomicBool::new(false));
        let service = TranscriptionService::new(
            Box::new(BurstAudioCapture::new(capture_stopped.clone(), 0)),
            Arc::new(TestFactory {
                panic_on_send: false,
                aborted: provider_aborted.clone(),
            }),
        );
        service
            .start_recording(
                Arc::new(|_| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
            )
            .await
            .unwrap();
        let run_a = service.provider_run_id.load(Ordering::Acquire) as u64;
        service.stop_capture_for_run(run_a).await.unwrap();
        let run_b = service
            .prepare_recording_capture(
                run_a + 1,
                SttConfig::default(),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_| {}),
            )
            .await
            .unwrap();
        capture_stopped.store(false, Ordering::SeqCst);
        (service, run_a, run_b, capture_stopped, provider_aborted)
    }

    #[tokio::test]
    async fn runtime_failure_of_draining_a_does_not_stop_pending_capture_b() {
        let (service, run_a, run_b, stopped, aborted) =
            runtime_failure_with_pending_capture_fixture().await;
        assert!(
            !service
                .cleanup_runtime_failure_for_run(run_a, "A drain error")
                .await
        );
        assert!(!aborted.load(Ordering::SeqCst));
        service.finalize_provider_for_run(run_a).await.unwrap();
        assert!(!stopped.load(Ordering::SeqCst));
        assert!(service.capture_token_is_current(run_b));
        assert!(service.capture_is_active_for_run(run_b.run_id).await);
        service.stop_capture_for_run(run_b.run_id).await.unwrap();
    }

    #[tokio::test]
    async fn runtime_failure_after_a_finalize_preserves_pending_capture_b() {
        let (service, run_a, run_b, stopped, aborted) =
            runtime_failure_with_pending_capture_fixture().await;
        service.finalize_provider_for_run(run_a).await.unwrap();
        assert!(
            !service
                .cleanup_runtime_failure_for_run(run_a, "late A callback")
                .await
        );
        assert!(!stopped.load(Ordering::SeqCst));
        assert!(!aborted.load(Ordering::SeqCst));
        assert!(service.capture_token_is_current(run_b));
        assert!(service.capture_is_active_for_run(run_b.run_id).await);
        service.stop_capture_for_run(run_b.run_id).await.unwrap();
    }

    #[tokio::test]
    async fn runtime_failure_rechecks_owner_after_waiting_for_connection_guard() {
        let (service, run_a, run_b, stopped, aborted) =
            runtime_failure_with_pending_capture_fixture().await;
        let guard = service.connection_lifecycle_guard.lock().await;
        let cleanup = service.cleanup_runtime_failure_for_run(run_a, "queued A callback");
        tokio::pin!(cleanup);
        assert!(futures_util::poll!(&mut cleanup).is_pending());
        // Finalization/next admission changed ownership while the callback waited.
        service
            .provider_run_id
            .store(run_b.run_id as usize, Ordering::Release);
        drop(guard);
        assert!(!cleanup.await);
        assert!(!stopped.load(Ordering::SeqCst));
        assert!(!aborted.load(Ordering::SeqCst));
        assert_eq!(
            service.provider_run_id.load(Ordering::Acquire) as u64,
            run_b.run_id
        );
        assert!(service.capture_token_is_current(run_b));
        service
            .cleanup_runtime_failure_for_run(run_b.run_id, "test teardown")
            .await;
    }

    #[tokio::test]
    async fn runtime_failure_cleanup_stops_capture_and_aborts_provider_once() {
        let capture_stopped = Arc::new(AtomicBool::new(false));
        let provider_aborted = Arc::new(AtomicBool::new(false));
        let service = TranscriptionService::new(
            Box::new(BurstAudioCapture::new(capture_stopped.clone(), 0)),
            Arc::new(TestFactory {
                panic_on_send: false,
                aborted: provider_aborted.clone(),
            }),
        );

        service
            .start_recording(
                Arc::new(|_| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
            )
            .await
            .expect("recording must start");

        assert!(service.cleanup_runtime_failure("test failure").await);
        assert_eq!(service.get_status().await, RecordingStatus::Idle);
        assert!(capture_stopped.load(Ordering::SeqCst));
        assert!(provider_aborted.load(Ordering::SeqCst));
        assert!(!service.cleanup_runtime_failure("duplicate failure").await);
    }

    #[tokio::test]
    async fn audio_processor_panic_is_contained_and_cleans_runtime_resources() {
        let capture_stopped = Arc::new(AtomicBool::new(false));
        let provider_aborted = Arc::new(AtomicBool::new(false));
        let service = TranscriptionService::new(
            Box::new(BurstAudioCapture::new(capture_stopped.clone(), 4)),
            Arc::new(TestFactory {
                panic_on_send: true,
                aborted: provider_aborted.clone(),
            }),
        );
        let (error_tx, mut error_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

        service
            .start_recording(
                Arc::new(|_| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                Arc::new(move |error| {
                    let _ = error_tx.send(error.to_string());
                }),
                Arc::new(|_, _| {}),
            )
            .await
            .expect("recording must start before processor panic");

        let error = tokio::time::timeout(Duration::from_secs(2), error_rx.recv())
            .await
            .expect("processor panic error timeout")
            .expect("processor panic error");
        assert!(error.contains("Audio processor task panicked"));

        tokio::time::timeout(Duration::from_secs(2), async {
            while service.get_status().await != RecordingStatus::Idle {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("processor panic cleanup timeout");
        assert!(capture_stopped.load(Ordering::SeqCst));
        assert!(provider_aborted.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn processor_error_callback_panic_does_not_block_cleanup() {
        let capture_stopped = Arc::new(AtomicBool::new(false));
        let provider_aborted = Arc::new(AtomicBool::new(false));
        let status = Arc::new(RwLock::new(RecordingStatus::Recording));
        let audio_capture: Arc<RwLock<Box<dyn AudioCapture>>> = Arc::new(RwLock::new(Box::new(
            BurstAudioCapture::new(capture_stopped.clone(), 0),
        )));
        let stt_provider: Arc<RwLock<Option<Box<dyn SttProvider>>>> =
            Arc::new(RwLock::new(Some(Box::new(AlwaysFailSendProvider {
                panic_on_send: false,
                aborted: provider_aborted.clone(),
            }))));

        let task = spawn_transcription_runtime_task(
            async move { panic!("simulated processor panic") },
            status.clone(),
            audio_capture,
            stt_provider,
            ProcessorOwner {
                token: PreparedCaptureToken {
                    run_id: 1,
                    generation: 1,
                },
                logical_run_id: 1,
                capture_run_id: Arc::new(AtomicUsize::new(1)),
                capture_generation: Arc::new(AtomicUsize::new(1)),
                capture_meter_generation: Arc::new(AtomicUsize::new(1)),
                provider_run_id: Arc::new(AtomicUsize::new(1)),
            },
            Arc::new(|_| panic!("simulated processor error callback panic")),
        );

        task.await
            .expect("processor and callback panics must be contained");
        assert_eq!(*status.read().await, RecordingStatus::Idle);
        assert!(capture_stopped.load(Ordering::SeqCst));
        assert!(provider_aborted.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn does_not_start_provider_if_audio_capture_fails_to_start() {
        let provider_aborted = Arc::new(AtomicBool::new(false));

        let audio_capture = FailingStartAudioCapture::default();
        let factory = Arc::new(TestFactory {
            panic_on_send: false,
            aborted: provider_aborted.clone(),
        });
        let service = TranscriptionService::new(Box::new(audio_capture), factory);

        let on_partial: TranscriptionCallback = Arc::new(|_t| {});
        let on_final: TranscriptionCallback = Arc::new(|_t| {});
        let on_audio_level: AudioLevelCallback = Arc::new(|_, _l| {});
        let on_audio_spectrum: AudioSpectrumCallback = Arc::new(|_, _b| {});
        let on_error: ErrorCallback = Arc::new(|_err: SttError| {});
        let on_quality: ConnectionQualityCallback = Arc::new(|_q, _r| {});

        let result = service
            .start_recording(
                on_partial,
                on_final,
                on_audio_level,
                on_audio_spectrum,
                on_error,
                on_quality,
            )
            .await;

        assert!(result.is_err());
        assert_eq!(service.get_status().await, RecordingStatus::Idle);
        assert!(!provider_aborted.load(Ordering::SeqCst));
    }

    struct BlockingStopAudioCapture {
        config: AudioConfig,
        is_capturing: Arc<AtomicBool>,
        stop_started: Arc<tokio::sync::Notify>,
        allow_stop: Arc<tokio::sync::Notify>,
    }

    #[async_trait]
    impl AudioCapture for BlockingStopAudioCapture {
        async fn initialize(&mut self, config: AudioConfig) -> AudioResult<()> {
            self.config = config;
            Ok(())
        }

        async fn start_capture(
            &mut self,
            _on_chunk: crate::domain::AudioChunkCallback,
        ) -> AudioResult<()> {
            self.is_capturing.store(true, Ordering::SeqCst);
            Ok(())
        }

        async fn stop_capture(&mut self) -> AudioResult<()> {
            self.stop_started.notify_one();
            self.allow_stop.notified().await;
            self.is_capturing.store(false, Ordering::SeqCst);
            Ok(())
        }

        fn is_capturing(&self) -> bool {
            self.is_capturing.load(Ordering::SeqCst)
        }

        fn config(&self) -> AudioConfig {
            self.config
        }
    }

    #[tokio::test]
    async fn stop_recording_enters_processing_before_capture_is_stopped() {
        let is_capturing = Arc::new(AtomicBool::new(false));
        let stop_started = Arc::new(tokio::sync::Notify::new());
        let allow_stop = Arc::new(tokio::sync::Notify::new());
        let service = Arc::new(TranscriptionService::new(
            Box::new(BlockingStopAudioCapture {
                config: AudioConfig::default(),
                is_capturing: is_capturing.clone(),
                stop_started: stop_started.clone(),
                allow_stop: allow_stop.clone(),
            }),
            Arc::new(TestFactory {
                panic_on_send: false,
                aborted: Arc::new(AtomicBool::new(false)),
            }),
        ));

        service
            .start_recording(
                Arc::new(|_| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
            )
            .await
            .expect("recording must start");

        let service_for_stop = service.clone();
        let stop_task = tokio::spawn(async move { service_for_stop.stop_recording().await });
        tokio::time::timeout(Duration::from_secs(1), stop_started.notified())
            .await
            .expect("stop_capture must be reached");

        assert_eq!(service.get_status().await, RecordingStatus::Processing);
        assert!(
            is_capturing.load(Ordering::SeqCst),
            "capture must still be on when Processing becomes observable"
        );

        allow_stop.notify_one();
        stop_task
            .await
            .expect("stop task must not panic")
            .expect("recording must stop");
    }

    struct FailingStopAudioCapture {
        config: AudioConfig,
        is_capturing: Arc<AtomicBool>,
        stop_called: Arc<AtomicBool>,
    }

    impl FailingStopAudioCapture {
        fn new(stop_called: Arc<AtomicBool>) -> Self {
            Self {
                config: AudioConfig::default(),
                is_capturing: Arc::new(AtomicBool::new(false)),
                stop_called,
            }
        }
    }

    #[async_trait]
    impl AudioCapture for FailingStopAudioCapture {
        async fn initialize(&mut self, config: AudioConfig) -> AudioResult<()> {
            self.config = config;
            Ok(())
        }

        async fn start_capture(
            &mut self,
            _on_chunk: crate::domain::AudioChunkCallback,
        ) -> AudioResult<()> {
            self.is_capturing.store(true, Ordering::SeqCst);
            Ok(())
        }

        async fn stop_capture(&mut self) -> AudioResult<()> {
            self.stop_called.store(true, Ordering::SeqCst);
            Err(crate::domain::AudioError::Capture(
                "simulated stop_capture failure".to_string(),
            ))
        }

        fn is_capturing(&self) -> bool {
            self.is_capturing.load(Ordering::SeqCst)
        }

        fn config(&self) -> AudioConfig {
            self.config
        }
    }

    #[tokio::test]
    async fn stop_recording_failure_does_not_leave_service_stuck_in_processing() {
        let provider_aborted = Arc::new(AtomicBool::new(false));
        let stop_called = Arc::new(AtomicBool::new(false));

        let audio_capture = FailingStopAudioCapture::new(stop_called.clone());
        let factory = Arc::new(TestFactory {
            panic_on_send: false,
            aborted: provider_aborted.clone(),
        });
        let service = TranscriptionService::new(Box::new(audio_capture), factory);

        let on_partial: TranscriptionCallback = Arc::new(|_t| {});
        let on_final: TranscriptionCallback = Arc::new(|_t| {});
        let on_audio_level: AudioLevelCallback = Arc::new(|_, _l| {});
        let on_audio_spectrum: AudioSpectrumCallback = Arc::new(|_, _b| {});
        let on_error: ErrorCallback = Arc::new(|_err: SttError| {});
        let on_quality: ConnectionQualityCallback = Arc::new(|_q, _r| {});

        service
            .start_recording(
                on_partial,
                on_final,
                on_audio_level,
                on_audio_spectrum,
                on_error,
                on_quality,
            )
            .await
            .expect("recording must start");

        // A failed physical stop must retain ownership and block another start.
        let result = service.stop_recording().await;
        assert!(result.is_err());
        assert!(stop_called.load(Ordering::SeqCst));
        assert_eq!(service.get_status().await, RecordingStatus::Recording);
        assert!(!provider_aborted.load(Ordering::SeqCst));
        assert!(service
            .start_recording(
                Arc::new(|_| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
            )
            .await
            .is_err());
    }
    #[derive(Clone, Copy)]
    enum DrainTestSend {
        Record,
        Block,
        BlockWithExtendedSendDeadline,
        DelaySevenSeconds,
        AuthenticationFailure,
    }

    struct DrainTestFactory {
        behavior: DrainTestSend,
        start_gate: Option<(Arc<tokio::sync::Notify>, bool)>,
        preferred_samples: Option<usize>,
        sent: tokio::sync::mpsc::UnboundedSender<AudioChunk>,
        stops: Arc<AtomicUsize>,
        aborts: Arc<AtomicUsize>,
    }

    struct DrainTestProvider {
        behavior: DrainTestSend,
        start_gate: Option<(Arc<tokio::sync::Notify>, bool)>,
        preferred_samples: Option<usize>,
        sent: tokio::sync::mpsc::UnboundedSender<AudioChunk>,
        stops: Arc<AtomicUsize>,
        aborts: Arc<AtomicUsize>,
    }

    impl SttProviderFactory for DrainTestFactory {
        fn create(&self, _config: &SttConfig) -> SttResult<Box<dyn SttProvider>> {
            Ok(Box::new(DrainTestProvider {
                behavior: self.behavior,
                start_gate: self.start_gate.clone(),
                preferred_samples: self.preferred_samples,
                sent: self.sent.clone(),
                stops: self.stops.clone(),
                aborts: self.aborts.clone(),
            }))
        }
    }

    #[async_trait]
    impl SttProvider for DrainTestProvider {
        async fn initialize(&mut self, _config: &SttConfig) -> SttResult<()> {
            Ok(())
        }
        async fn start_stream(
            &mut self,
            _on_partial: TranscriptionCallback,
            _on_final: TranscriptionCallback,
            _on_error: ErrorCallback,
            _on_quality: ConnectionQualityCallback,
        ) -> SttResult<()> {
            if let Some((gate, fail)) = &self.start_gate {
                gate.notified().await;
                if *fail {
                    return Err(SttError::Processing("fixture handshake failure".into()));
                }
            }
            Ok(())
        }
        async fn send_audio(&mut self, chunk: &AudioChunk) -> SttResult<()> {
            self.sent.send(chunk.clone()).unwrap();
            match self.behavior {
                DrainTestSend::Record => Ok(()),
                DrainTestSend::Block | DrainTestSend::BlockWithExtendedSendDeadline => {
                    std::future::pending().await
                }
                DrainTestSend::DelaySevenSeconds => {
                    tokio::time::sleep(Duration::from_secs(7)).await;
                    Ok(())
                }
                DrainTestSend::AuthenticationFailure => {
                    Err(SttError::Authentication("test expired subscription".into()))
                }
            }
        }
        fn audio_send_timeout(&self) -> Option<Duration> {
            matches!(
                self.behavior,
                DrainTestSend::DelaySevenSeconds | DrainTestSend::BlockWithExtendedSendDeadline
            )
            .then_some(Duration::from_secs(30))
        }
        fn preferred_audio_batch_samples(&self) -> Option<usize> {
            self.preferred_samples
        }
        async fn stop_stream(&mut self) -> SttResult<()> {
            self.stops.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn abort(&mut self) -> SttResult<()> {
            self.aborts.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn name(&self) -> &str {
            "drain_regression_test"
        }
        fn is_online(&self) -> bool {
            true
        }
    }

    struct DrainTestRun {
        service: TranscriptionService,
        token: PreparedCaptureToken,
        capture: crate::domain::AudioChunkCallback,
        sent: tokio::sync::mpsc::UnboundedReceiver<AudioChunk>,
        stops: Arc<AtomicUsize>,
        aborts: Arc<AtomicUsize>,
    }

    async fn prepare_drain_test(
        behavior: DrainTestSend,
        preferred_samples: Option<usize>,
    ) -> DrainTestRun {
        prepare_drain_test_with_start(behavior, preferred_samples, None).await
    }

    async fn prepare_drain_test_with_start(
        behavior: DrainTestSend,
        preferred_samples: Option<usize>,
        start_gate: Option<(Arc<tokio::sync::Notify>, bool)>,
    ) -> DrainTestRun {
        let callback = Arc::new(std::sync::Mutex::new(None));
        let (tx, sent) = tokio::sync::mpsc::unbounded_channel();
        let stops = Arc::new(AtomicUsize::new(0));
        let aborts = Arc::new(AtomicUsize::new(0));
        let service = TranscriptionService::new(
            Box::new(ManualAudioCapture::new(callback.clone())),
            Arc::new(DrainTestFactory {
                behavior,
                start_gate,
                preferred_samples,
                sent: tx,
                stops: stops.clone(),
                aborts: aborts.clone(),
            }),
        );
        let token = service
            .prepare_recording_capture(
                701,
                SttConfig::default(),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_| {}),
            )
            .await
            .unwrap();
        let capture = callback.lock().unwrap().clone().unwrap();
        DrainTestRun {
            service,
            token,
            capture,
            sent,
            stops,
            aborts,
        }
    }

    async fn connect_drain_test(run: &DrainTestRun) {
        run.service
            .connect_prepared_recording(
                run.token,
                Arc::new(|_| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_, _| {}),
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
                Arc::new(AtomicBool::new(false)),
            )
            .await
            .unwrap();
    }

    async fn next_drain_test_packet(run: &mut DrainTestRun) -> AudioChunk {
        tokio::time::timeout(Duration::from_secs(1), run.sent.recv())
            .await
            .expect("ready/live audio must be submitted without waiting for a packet to fill")
            .expect("test provider packet")
    }

    #[tokio::test(start_paused = true)]
    async fn service_send_audio_honors_provider_deadline_beyond_six_seconds() {
        let mut run = prepare_drain_test(DrainTestSend::DelaySevenSeconds, None).await;
        connect_drain_test(&run).await;
        (run.capture)(AudioChunk::new(vec![1200; 480], 16_000, 1));
        let sent = next_drain_test_packet(&mut run).await;
        assert_eq!(sent.data.len(), 480);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(6) + Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(1)).await;
        run.service
            .stop_capture_for_run(run.token.run_id)
            .await
            .unwrap();
        run.service
            .finalize_provider_for_run(run.token.run_id)
            .await
            .unwrap();
        let report = run
            .service
            .completed_report_for_run(run.token.run_id)
            .await
            .unwrap();
        assert_eq!(report.audio.accepted_bytes, 960);
        assert_eq!(report.audio.submitted_bytes, 960);
        assert!(!report.audio.is_incomplete());
        assert_eq!(run.aborts.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn cold_start_stop_preserves_fifo_across_attach_publication_and_finalizes_once() {
        // Exercise both a retained FIFO and the slot-to-active publication edge.
        for stop_before_attach in [true, false] {
            let gate = Arc::new(tokio::sync::Notify::new());
            let mut run = prepare_drain_test_with_start(
                DrainTestSend::Record,
                None,
                Some((gate.clone(), false)),
            )
            .await;
            (run.capture)(AudioChunk::new(vec![0; 480], 16000, 1));
            (run.capture)(AudioChunk::new(vec![1200; 480], 16000, 1));
            if stop_before_attach {
                run.service
                    .stop_pending_capture(run.token, false)
                    .await
                    .unwrap();
                run.service
                    .stop_pending_capture(run.token, false)
                    .await
                    .unwrap();
            }
            {
                // Suspend exactly at active ownership publication. Stop must wait
                // for that publication rather than losing/dropping the FIFO.
                let publication = run.service.active_audio.write().await;
                let connect = connect_drain_test(&run);
                tokio::pin!(connect);
                assert!(futures_util::poll!(connect.as_mut()).is_pending());
                let stop = run.service.stop_pending_capture(run.token, false);
                tokio::pin!(stop);
                assert!(futures_util::poll!(stop.as_mut()).is_pending());
                drop(publication);
                assert!(futures_util::poll!(connect.as_mut()).is_pending());
                stop.await.unwrap();
                assert!(
                    !run.service
                        .capture_is_active_for_run(run.token.run_id)
                        .await
                );
                assert_eq!(run.service.active_capture_episode(), Some(run.token));
                run.service
                    .stop_pending_capture(run.token, false)
                    .await
                    .unwrap();
                // Attach stays alive until this one handshake resolves.
                gate.notify_one();
                connect.await;
            }
            assert_eq!(run.service.get_status().await, RecordingStatus::Processing);
            run.service
                .stop_capture_for_run(run.token.run_id)
                .await
                .unwrap();
            run.service
                .finalize_provider_for_run(run.token.run_id)
                .await
                .unwrap();
            run.service
                .finalize_provider_for_run(run.token.run_id)
                .await
                .unwrap();
            let report = run
                .service
                .completed_report_for_run(run.token.run_id)
                .await
                .unwrap();
            assert_eq!(report.audio.accepted_bytes, 1920);
            assert_eq!(report.audio.submitted_bytes, 1920);
            assert!(!report.audio.is_incomplete());
            let mut samples = Vec::new();
            while let Ok(packet) = run.sent.try_recv() {
                samples.extend(packet.data);
            }
            assert_eq!(samples.len(), 960);
            assert!(samples[..480].iter().all(|sample| *sample == 0));
            assert!(samples[480..].iter().all(|sample| *sample > 0));
            assert_eq!(run.stops.load(Ordering::SeqCst), 1);
            assert_eq!(run.aborts.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn cold_start_stopped_handshake_error_timeout_and_force_cancel_never_replay() {
        for outcome in ["error", "error_racing_cancel", "timeout", "cancel"] {
            let gate = Arc::new(tokio::sync::Notify::new());
            let mut run = prepare_drain_test_with_start(
                DrainTestSend::Record,
                None,
                Some((gate.clone(), outcome.starts_with("error"))),
            )
            .await;
            (run.capture)(AudioChunk::new(vec![1200; 480], 16000, 1));
            let cancelled = Arc::new(AtomicBool::new(false));
            let errors = Arc::new(AtomicUsize::new(0));
            let observed = errors.clone();
            {
                let connect = run.service.connect_prepared_recording(
                    run.token,
                    Arc::new(|_| {}),
                    Arc::new(|_| {}),
                    Arc::new(|_, _| {}),
                    Arc::new(|_, _| {}),
                    Arc::new(move |_| {
                        observed.fetch_add(1, Ordering::SeqCst);
                    }),
                    Arc::new(|_, _| {}),
                    cancelled.clone(),
                );
                tokio::pin!(connect);
                assert!(futures_util::poll!(connect.as_mut()).is_pending());
                run.service
                    .stop_pending_capture(run.token, false)
                    .await
                    .unwrap();
                match outcome {
                    "error" => gate.notify_one(),
                    "error_racing_cancel" => {
                        gate.notify_one();
                        cancelled.store(true, Ordering::Release);
                    }
                    "cancel" => cancelled.store(true, Ordering::Release),
                    _ => {}
                }
                let error = connect.await.unwrap_err();
                assert_eq!(error.is::<PreparedCaptureCancelled>(), outcome == "cancel");
            }
            assert!(
                !run.service
                    .capture_is_active_for_run(run.token.run_id)
                    .await
            );
            assert_eq!(run.service.get_status().await, RecordingStatus::Idle);
            assert!(run.sent.try_recv().is_err());
            assert_eq!(run.stops.load(Ordering::SeqCst), 0);
            assert_eq!(run.aborts.load(Ordering::SeqCst), 1);
            assert_eq!(errors.load(Ordering::SeqCst), 0);
            assert_eq!(run.service.logical_provider_run_id(), 0);
        }
    }

    #[tokio::test]
    async fn drain_deadline_cancels_inflight_send_without_graceful_stop_or_replay() {
        for (selected, behavior) in [
            (
                crate::domain::BackendStreamingProvider::Deepgram,
                DrainTestSend::Block,
            ),
            (
                crate::domain::BackendStreamingProvider::ElevenLabs,
                DrainTestSend::BlockWithExtendedSendDeadline,
            ),
        ] {
            let mut run = prepare_drain_test(behavior, None).await;
            run.service
                .prepared_capture
                .lock()
                .await
                .as_mut()
                .unwrap()
                .config
                .backend_streaming_provider = selected;
            (run.capture)(AudioChunk::new(vec![1200; 480], 16000, 1));
            connect_drain_test(&run).await;
            next_drain_test_packet(&mut run).await;
            run.service
                .stop_capture_for_run(run.token.run_id)
                .await
                .unwrap();
            let error = tokio::time::timeout(
                Duration::from_secs(6),
                run.service.finalize_provider_for_run(run.token.run_id),
            )
            .await
            .expect("bounded finalize")
            .unwrap_err()
            .to_string();
            let report = run
                .service
                .completed_report_for_run(run.token.run_id)
                .await
                .unwrap();
            assert_eq!(report.audio.reason, AudioDrainReason::Deadline);
            assert_eq!(report.audio.accepted_bytes, 960);
            assert_eq!(report.audio.submitted_bytes, 0);
            assert_eq!(report.audio.unknown_bytes, 960);
            assert_eq!(
                report.provider_release,
                crate::domain::ProviderRelease::Released
            );
            assert_eq!(run.stops.load(Ordering::SeqCst), 0);
            assert_eq!(run.aborts.load(Ordering::SeqCst), 1);
            assert_eq!(
                run.service
                    .finalize_provider_for_run(run.token.run_id)
                    .await
                    .unwrap_err()
                    .to_string(),
                error
            );
            assert_eq!(
                run.service
                    .completed_report_for_run(run.token.run_id)
                    .await
                    .unwrap()
                    .audio,
                report.audio
            );
            assert_eq!(run.stops.load(Ordering::SeqCst), 0);
            assert_eq!(run.aborts.load(Ordering::SeqCst), 1);
            assert!(run.sent.try_recv().is_err());
        }
    }

    #[tokio::test]
    async fn first_auth_send_failure_survives_hard_abort_as_shared_terminal_error() {
        let mut run = prepare_drain_test(DrainTestSend::AuthenticationFailure, None).await;
        (run.capture)(AudioChunk::new(vec![1200; 480], 16000, 1));
        (run.capture)(AudioChunk::new(vec![1400; 480], 16000, 1));
        connect_drain_test(&run).await;
        next_drain_test_packet(&mut run).await;
        run.service
            .stop_capture_for_run(run.token.run_id)
            .await
            .unwrap();
        assert!(run
            .service
            .finalize_provider_for_run(run.token.run_id)
            .await
            .is_err());
        let report = run
            .service
            .completed_report_for_run(run.token.run_id)
            .await
            .unwrap();
        assert!(report.shared_failure);
        assert!(report.error.unwrap().contains("expired subscription"));
        assert_eq!(
            report.provider_release,
            crate::domain::ProviderRelease::Released
        );
        assert_eq!(report.audio.unknown_bytes, 960);
        assert_eq!(report.audio.submitted_bytes, 0);
        assert_eq!(run.stops.load(Ordering::SeqCst), 0);
        assert_eq!(run.aborts.load(Ordering::SeqCst), 1);
        assert!(
            run.sent.try_recv().is_err(),
            "first failed frame must not be retried or followed by another frame"
        );
    }

    #[tokio::test]
    async fn ready_backlog_batches_amplified_original_chunks_but_never_waits_for_live_fill() {
        let mut run = prepare_drain_test(DrainTestSend::Record, Some(4800)).await;
        run.service.set_microphone_sensitivity(200).await;
        let mut expected = Vec::new();
        for index in 0..12 {
            let amplitude = if index % 2 == 0 { 1200 } else { 28000 };
            let samples: Vec<i16> = (0..480)
                .map(|sample| {
                    if sample % 2 == 0 {
                        amplitude
                    } else {
                        -amplitude
                    }
                })
                .collect();
            let gain = limited_microphone_gain(200, amplitude as i32);
            expected.extend(amplify_i16_samples(&samples, gain));
            (run.capture)(AudioChunk::new(samples, 16000, 1));
        }
        connect_drain_test(&run).await;
        let first = next_drain_test_packet(&mut run).await;
        let second = next_drain_test_packet(&mut run).await;
        assert_eq!(first.data.len(), 4800);
        assert_eq!(second.data.len(), 960);
        assert_eq!(first.sample_rate, 16000);
        assert_eq!(first.channels, 1);
        assert_eq!([first.data, second.data].concat(), expected);
        assert!(run.sent.try_recv().is_err());
        // No future frame is supplied. Receiving this sub-target packet proves
        // live delivery does not have a hidden fill timer or minimum batch size.
        let live = vec![1700; 480];
        (run.capture)(AudioChunk::new(live.clone(), 16000, 1));
        let live_packet = next_drain_test_packet(&mut run).await;
        assert_eq!(
            live_packet.data,
            amplify_i16_samples(&live, limited_microphone_gain(200, 1700))
        );
        run.service
            .stop_capture_for_run(run.token.run_id)
            .await
            .unwrap();
        run.service
            .finalize_provider_for_run(run.token.run_id)
            .await
            .unwrap();
        let report = run
            .service
            .completed_report_for_run(run.token.run_id)
            .await
            .unwrap();
        assert_eq!(report.audio.accepted_bytes, 13 * 480 * 2);
        assert_eq!(report.audio.submitted_bytes, report.audio.accepted_bytes);
        assert!(!report.audio.is_incomplete());
        assert_eq!(run.stops.load(Ordering::SeqCst), 1);
        assert_eq!(run.aborts.load(Ordering::SeqCst), 0);
    }
    include!("transcription_service/composed_ready_tests.rs");
}
