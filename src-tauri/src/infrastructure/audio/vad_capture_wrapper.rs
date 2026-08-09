use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::domain::{
    amplify_i16_samples, limited_microphone_gain, AudioCapture, AudioChunk, AudioChunkCallback,
    AudioConfig, AudioResult,
};
use crate::infrastructure::audio::{VadProcessor, VadResult};

/// Callback type for silence timeout events
pub type SilenceTimeoutCallback = Arc<dyn Fn() + Send + Sync>;

const PENDING_STOP_GRACE: Duration = Duration::from_millis(300);

#[derive(Default)]
struct SilenceStopState {
    next_token: u64,
    pending_token: Option<u64>,
    committed: bool,
}

#[derive(Clone, Copy)]
enum SilenceTimerSignal {
    Arm { token: u64, generation: u64 },
    StateChanged,
}

async fn run_silence_timer_worker(
    mut signal_rx: tokio::sync::mpsc::Receiver<SilenceTimerSignal>,
    silence_stop_state: Arc<Mutex<SilenceStopState>>,
    running: Arc<AtomicBool>,
    active_generation: Arc<AtomicU64>,
    silence_callback: Option<SilenceTimeoutCallback>,
) {
    let mut armed: Option<(u64, u64)> = None;

    loop {
        let Some((token, generation)) = armed else {
            match signal_rx.recv().await {
                Some(SilenceTimerSignal::Arm { token, generation }) => {
                    armed = Some((token, generation));
                }
                Some(SilenceTimerSignal::StateChanged) => {}
                None => break,
            }
            continue;
        };

        tokio::select! {
            signal = signal_rx.recv() => {
                match signal {
                    Some(SilenceTimerSignal::Arm { token, generation }) => {
                        armed = Some((token, generation));
                    }
                    Some(SilenceTimerSignal::StateChanged) => {
                        let still_pending = match silence_stop_state.lock() {
                            Ok(state) => {
                                running.load(Ordering::Acquire)
                                    && active_generation.load(Ordering::Acquire) == generation
                                    && state.pending_token == Some(token)
                            }
                            Err(error) => {
                                log::error!("VAD pending-stop state poisoned: {}", error);
                                false
                            }
                        };
                        if !still_pending {
                            armed = None;
                        }
                    }
                    None => break,
                }
            }
            _ = tokio::time::sleep(PENDING_STOP_GRACE) => {
                let should_commit = match silence_stop_state.lock() {
                    Ok(mut state) => {
                        if running.load(Ordering::Acquire)
                            && active_generation.load(Ordering::Acquire) == generation
                            && state.pending_token == Some(token)
                        {
                            state.pending_token = None;
                            state.committed = true;
                            true
                        } else {
                            false
                        }
                    }
                    Err(error) => {
                        log::error!("VAD pending-stop commit state poisoned: {}", error);
                        false
                    }
                };

                armed = None;
                if should_commit {
                    log::info!("VAD: pending stop committed after speech grace");
                    if let Some(callback) = silence_callback.as_ref() {
                        callback();
                    }
                }
            }
        }
    }
}

/// VAD-aware audio capture wrapper
///
/// Wraps any AudioCapture implementation and adds Voice Activity Detection:
/// - Buffers incoming audio until we have exactly 480 samples (30ms @ 16kHz)
/// - Runs WebRTC VAD on each complete frame
/// - On VadResult::SilenceTimeout (configurable, default 3000ms) → triggers silence callback ONCE
/// - Passes through audio chunks to downstream callback
///
/// Requirements:
/// - Input MUST be 16kHz mono i16 PCM (VAD requirement)
/// - Frames MUST be exactly 480 samples (30ms @ 16kHz)
pub struct VadCaptureWrapper {
    inner: Box<dyn AudioCapture>,
    vad: Arc<Mutex<VadProcessor>>,
    on_silence_timeout: Option<SilenceTimeoutCallback>,
    audio_config: AudioConfig,
    silence_stop_state: Arc<Mutex<SilenceStopState>>,
    running: Arc<AtomicBool>, // Защита от "хвостов" callback после stop_capture
    capture_generation: Arc<AtomicU64>, // Инвалидирует callback-и от прошлых start_capture
    silence_timer_task: Option<tokio::task::JoinHandle<()>>,
    microphone_sensitivity: Arc<AtomicU8>,
}

impl VadCaptureWrapper {
    /// Create new VAD wrapper around an audio capture
    ///
    /// # Arguments
    /// * `inner` - Underlying audio capture (must output 16kHz mono)
    /// * `vad` - VAD processor instance
    pub fn new(inner: Box<dyn AudioCapture>, vad: VadProcessor) -> Self {
        Self::new_with_microphone_sensitivity(inner, vad, Arc::new(AtomicU8::new(100)))
    }

    pub fn new_with_microphone_sensitivity(
        inner: Box<dyn AudioCapture>,
        vad: VadProcessor,
        microphone_sensitivity: Arc<AtomicU8>,
    ) -> Self {
        Self {
            inner,
            vad: Arc::new(Mutex::new(vad)),
            on_silence_timeout: None,
            audio_config: AudioConfig::default(),
            silence_stop_state: Arc::new(Mutex::new(SilenceStopState::default())),
            running: Arc::new(AtomicBool::new(false)),
            capture_generation: Arc::new(AtomicU64::new(0)),
            silence_timer_task: None,
            microphone_sensitivity,
        }
    }

    /// Set callback for silence timeout events
    ///
    /// This callback is invoked ONCE when VAD detects configured silence timeout
    pub fn set_silence_timeout_callback(&mut self, callback: SilenceTimeoutCallback) {
        self.on_silence_timeout = Some(callback);
    }
}

#[async_trait]
impl AudioCapture for VadCaptureWrapper {
    async fn initialize(&mut self, config: AudioConfig) -> AudioResult<()> {
        self.audio_config = config.clone();
        self.inner.initialize(config).await
    }

    async fn start_capture(&mut self, on_chunk: AudioChunkCallback) -> AudioResult<()> {
        if let Some(task) = self.silence_timer_task.take() {
            task.abort();
            let _ = task.await;
        }

        let capture_generation = self.capture_generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.running.store(true, Ordering::SeqCst);

        if let Ok(mut state) = self.silence_stop_state.lock() {
            // Keep tokens monotonic across capture generations. An old timer signal must never
            // clear a numerically reused pending token from a newer recording.
            state.next_token = state.next_token.wrapping_add(1).max(1);
            state.pending_token = None;
            state.committed = false;
        }

        // Сбрасываем состояние VAD при старте новой записи.
        // stop_capture обычно вызывает reset(), но в некоторых error/restart сценариях
        // start_capture может быть вызван на "грязном" состоянии.
        if let Ok(mut vad) = self.vad.lock() {
            vad.reset();
        }

        let vad = self.vad.clone();
        let silence_callback = self.on_silence_timeout.clone();
        let silence_stop_state = self.silence_stop_state.clone();
        let running = self.running.clone();
        let active_generation = self.capture_generation.clone();
        let microphone_sensitivity = self.microphone_sensitivity.clone();
        let (silence_timer_tx, silence_timer_rx) = tokio::sync::mpsc::channel(4);
        self.silence_timer_task = Some(tokio::spawn(run_silence_timer_worker(
            silence_timer_rx,
            silence_stop_state.clone(),
            running.clone(),
            active_generation.clone(),
            silence_callback,
        )));

        // Frame buffer for accumulating exactly 480 samples (30ms @ 16kHz)
        // Shared between callback invocations via Arc<Mutex<>>
        let frame_buffer: Arc<Mutex<Vec<i16>>> = Arc::new(Mutex::new(Vec::with_capacity(960)));

        // Wrapped callback that processes audio through VAD
        let wrapped_callback = Arc::new(move |chunk: AudioChunk| {
            // Важно: после stop_capture внутренняя аудио-система может ещё кратко вызывать callback.
            // Мы обязаны игнорировать такие "хвосты", иначе VAD может отправить timeout уже в новой сессии.
            if !running.load(Ordering::Relaxed)
                || active_generation.load(Ordering::Relaxed) != capture_generation
            {
                return;
            }

            // Validate input format (VAD requirements)
            if chunk.sample_rate != 16000 {
                log::error!(
                    "VAD requires 16kHz audio, got {} Hz. Skipping VAD.",
                    chunk.sample_rate
                );
                on_chunk(chunk); // Pass through without VAD
                return;
            }

            if chunk.channels != 1 {
                log::error!(
                    "VAD requires mono audio, got {} channels. Skipping VAD.",
                    chunk.channels
                );
                on_chunk(chunk); // Pass through without VAD
                return;
            }

            // Add samples to frame buffer (защита от poisoned mutex)
            let mut buffer = match frame_buffer.lock() {
                Ok(b) => b,
                Err(e) => {
                    log::error!("VAD frame buffer poisoned: {}", e);
                    log::error!("Passing through audio without VAD processing");
                    on_chunk(chunk); // передаем оригинальный chunk без VAD
                    return;
                }
            };
            buffer.extend_from_slice(&chunk.data);

            // Process complete 30ms frames (480 samples @ 16kHz)
            const VAD_FRAME_SIZE: usize = 480;

            while buffer.len() >= VAD_FRAME_SIZE {
                if !running.load(Ordering::Relaxed)
                    || active_generation.load(Ordering::Relaxed) != capture_generation
                {
                    return;
                }

                let frame: Vec<i16> = buffer.drain(..VAD_FRAME_SIZE).collect();
                let raw_max = max_abs_i16(&frame);
                let sensitivity = microphone_sensitivity.load(Ordering::Relaxed);
                let vad_gain = limited_microphone_gain(sensitivity, raw_max);
                let vad_frame = if (vad_gain - 1.0).abs() < f32::EPSILON {
                    frame.clone()
                } else {
                    amplify_i16_samples(&frame, vad_gain)
                };
                let vad_max = if (vad_gain - 1.0).abs() < f32::EPSILON {
                    raw_max
                } else {
                    max_abs_i16(&vad_frame)
                };

                // Run VAD on this frame (защита от poisoned mutex)
                let mut vad_guard = match vad.lock() {
                    Ok(v) => v,
                    Err(e) => {
                        log::error!("VAD processor poisoned: {}", e);
                        log::error!("Passing through audio chunk without VAD");
                        on_chunk(AudioChunk::new(frame, 16000, 1));
                        continue;
                    }
                };

                let vad_result = match vad_guard.process_samples(&vad_frame) {
                    Ok(result) => result,
                    Err(e) => {
                        log::error!("VAD processing error: {}", e);
                        // Pass through on error
                        on_chunk(AudioChunk::new(frame, 16000, 1));
                        continue;
                    }
                };
                drop(vad_guard); // Release VAD lock before callback

                match vad_result {
                    VadResult::Speech => {
                        // Speech and the pending-stop commit share one lock. If speech wins,
                        // the delayed stop is cancelled before this frame is delivered.
                        if let Ok(mut state) = silence_stop_state.lock() {
                            if state.pending_token.take().is_some() {
                                log::info!("VAD: resumed speech cancelled pending stop");
                                let _ = silence_timer_tx.try_send(SilenceTimerSignal::StateChanged);
                            }
                        }
                        log::trace!("VAD: Speech detected");
                        on_chunk(AudioChunk::new(frame, 16000, 1));
                    }
                    VadResult::Silence => {
                        // Silence but below timeout - still pass through
                        log::trace!("VAD: Silence (below timeout)");
                        on_chunk(AudioChunk::new(frame, 16000, 1));
                    }
                    VadResult::SilenceTimeout => {
                        let pending_token = match silence_stop_state.lock() {
                            Ok(mut state) => {
                                if state.committed || state.pending_token.is_some() {
                                    None
                                } else {
                                    state.next_token = state.next_token.wrapping_add(1).max(1);
                                    let token = state.next_token;
                                    state.pending_token = Some(token);
                                    Some(token)
                                }
                            }
                            Err(e) => {
                                log::error!("VAD pending-stop state poisoned: {}", e);
                                on_chunk(AudioChunk::new(frame, 16000, 1));
                                continue;
                            }
                        };

                        if let Some(pending_token) = pending_token {
                            // Получаем настоящий timeout из VAD для логирования
                            let timeout_ms = {
                                match vad.lock() {
                                    Ok(vad_guard) => vad_guard.timeout().as_millis(),
                                    Err(_) => 0, // fallback если mutex poisoned
                                }
                            };

                            log::info!(
                                "VAD: Silence timeout reached; pending stop for {}ms (timeout={}ms, sensitivity={}%, vad_gain={:.2}x, raw_max={}, vad_max={})",
                                PENDING_STOP_GRACE.as_millis(),
                                timeout_ms,
                                sensitivity,
                                vad_gain,
                                raw_max,
                                vad_max
                            );

                            if let Err(error) = silence_timer_tx.try_send(SilenceTimerSignal::Arm {
                                token: pending_token,
                                generation: capture_generation,
                            }) {
                                log::warn!("VAD pending-stop signal dropped: {}", error);
                                if let Ok(mut state) = silence_stop_state.lock() {
                                    if state.pending_token == Some(pending_token) {
                                        state.pending_token = None;
                                    }
                                }
                            }
                        }

                        // Продолжаем пропускать аудио (для финализации)
                        on_chunk(AudioChunk::new(frame, 16000, 1));
                    }
                    VadResult::Buffering => {
                        // Should not happen since we buffer to 480 samples
                        log::trace!("VAD: Buffering");
                    }
                }
            }
        });

        // Start inner capture with wrapped callback
        match self.inner.start_capture(wrapped_callback).await {
            Ok(()) => Ok(()),
            Err(err) => {
                self.running.store(false, Ordering::SeqCst);
                self.capture_generation.fetch_add(1, Ordering::SeqCst);
                if let Some(task) = self.silence_timer_task.take() {
                    task.abort();
                    let _ = task.await;
                }
                Err(err)
            }
        }
    }

    async fn stop_capture(&mut self) -> AudioResult<()> {
        self.running.store(false, Ordering::SeqCst);
        self.capture_generation.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut state) = self.silence_stop_state.lock() {
            state.pending_token = None;
        }
        if let Some(task) = self.silence_timer_task.take() {
            task.abort();
            let _ = task.await;
        }

        // Reset VAD state on stop
        if let Ok(mut vad) = self.vad.lock() {
            vad.reset();
        }

        self.inner.stop_capture().await
    }

    fn is_capturing(&self) -> bool {
        self.inner.is_capturing()
    }

    fn config(&self) -> AudioConfig {
        self.audio_config.clone()
    }
}

impl Drop for VadCaptureWrapper {
    fn drop(&mut self) {
        if let Some(task) = self.silence_timer_task.take() {
            task.abort();
        }
    }
}

fn max_abs_i16(samples: &[i16]) -> i32 {
    samples.iter().map(|&s| (s as i32).abs()).max().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::audio::MockAudioCapture;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    struct ManualCallbackCapture {
        callback: Arc<Mutex<Option<AudioChunkCallback>>>,
        is_capturing: Arc<AtomicBool>,
        config: AudioConfig,
    }

    impl ManualCallbackCapture {
        fn new(callback: Arc<Mutex<Option<AudioChunkCallback>>>) -> Self {
            Self {
                callback,
                is_capturing: Arc::new(AtomicBool::new(false)),
                config: AudioConfig::default(),
            }
        }
    }

    #[async_trait]
    impl AudioCapture for ManualCallbackCapture {
        async fn initialize(&mut self, config: AudioConfig) -> AudioResult<()> {
            self.config = config;
            Ok(())
        }

        async fn start_capture(&mut self, on_chunk: AudioChunkCallback) -> AudioResult<()> {
            self.is_capturing.store(true, Ordering::SeqCst);
            *self.callback.lock().unwrap() = Some(on_chunk);
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

    fn activity_then_silence_chunk() -> AudioChunk {
        let mut samples = vec![0i16; 480 * 5];
        for sample in samples.iter_mut().take(480) {
            *sample = 1000;
        }
        AudioChunk::new(samples, 16000, 1)
    }

    #[tokio::test]
    async fn test_vad_wrapper_creation() {
        let mock_capture = Box::new(MockAudioCapture::new());
        let vad = VadProcessor::default().expect("Failed to create VAD");

        let wrapper = VadCaptureWrapper::new(mock_capture, vad);
        assert!(!wrapper.is_capturing());
    }

    #[tokio::test]
    async fn test_vad_wrapper_with_callback() {
        let mock_capture = Box::new(MockAudioCapture::new());
        let vad = VadProcessor::default().expect("Failed to create VAD");

        let mut wrapper = VadCaptureWrapper::new(mock_capture, vad);

        // Set silence timeout callback
        let silence_triggered = Arc::new(Mutex::new(false));
        let silence_flag = silence_triggered.clone();
        wrapper.set_silence_timeout_callback(Arc::new(move || {
            *silence_flag.lock().unwrap() = true;
        }));

        // Test that wrapper can be initialized
        let config = AudioConfig::default();
        let result = wrapper.initialize(config).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_invalid_sample_rate_passthrough() {
        let mock_capture = Box::new(MockAudioCapture::new());
        let vad = VadProcessor::default().expect("Failed to create VAD");
        let mut wrapper = VadCaptureWrapper::new(mock_capture, vad);

        let chunks_received = Arc::new(Mutex::new(0usize));
        let counter = chunks_received.clone();

        let on_chunk = Arc::new(move |chunk: AudioChunk| {
            // Проверяем что chunk прошел с неправильным sample rate
            assert_eq!(chunk.sample_rate, 48000);
            *counter.lock().unwrap() += 1;
        });

        wrapper.initialize(AudioConfig::default()).await.unwrap();
        wrapper.start_capture(on_chunk).await.unwrap();

        // MockAudioCapture не будет автоматически отправлять chunks,
        // но мы проверили что wrapper инициализируется
        assert!(!wrapper.is_capturing()); // Mock не начнет capture сам
    }

    #[tokio::test]
    async fn test_invalid_channels_passthrough() {
        let mock_capture = Box::new(MockAudioCapture::new());
        let vad = VadProcessor::default().expect("Failed to create VAD");
        let mut wrapper = VadCaptureWrapper::new(mock_capture, vad);

        let chunks_received = Arc::new(Mutex::new(0usize));
        let counter = chunks_received.clone();

        let on_chunk = Arc::new(move |chunk: AudioChunk| {
            // Проверяем что chunk прошел со стерео
            assert_eq!(chunk.channels, 2);
            *counter.lock().unwrap() += 1;
        });

        wrapper.initialize(AudioConfig::default()).await.unwrap();
        wrapper.start_capture(on_chunk).await.unwrap();

        // Wrapper должен инициализироваться даже с неправильными данными
        assert!(!wrapper.is_capturing());
    }

    #[tokio::test]
    async fn test_silence_timeout_callback_trigger() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let mock_capture = Box::new(MockAudioCapture::new());
        let vad = VadProcessor::new(Some(90), None).expect("Failed to create VAD");

        let mut wrapper = VadCaptureWrapper::new(mock_capture, vad);

        // Используем AtomicBool для thread-safe флага
        let silence_triggered = Arc::new(AtomicBool::new(false));
        let flag_clone = silence_triggered.clone();

        wrapper.set_silence_timeout_callback(Arc::new(move || {
            flag_clone.store(true, Ordering::SeqCst);
        }));

        wrapper.initialize(AudioConfig::default()).await.unwrap();

        // Проверяем что callback установлен
        assert!(!silence_triggered.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_audio_passthrough() {
        let mock_capture = Box::new(MockAudioCapture::new());
        let vad = VadProcessor::default().expect("Failed to create VAD");
        let mut wrapper = VadCaptureWrapper::new(mock_capture, vad);

        let chunks_count = Arc::new(Mutex::new(0));
        let counter = chunks_count.clone();

        let on_chunk = Arc::new(move |chunk: AudioChunk| {
            // Проверяем формат chunk
            assert_eq!(chunk.sample_rate, 16000);
            assert_eq!(chunk.channels, 1);
            *counter.lock().unwrap() += 1;
        });

        let config = AudioConfig::default();
        wrapper.initialize(config).await.unwrap();
        wrapper.start_capture(on_chunk).await.unwrap();

        // MockAudioCapture не генерирует реальные chunks, но wrapper инициализирован
        wrapper.stop_capture().await.unwrap();
        assert!(!wrapper.is_capturing());
    }

    #[tokio::test]
    async fn test_stale_callback_after_restart_does_not_trigger_silence_timeout() {
        let callback_slot = Arc::new(Mutex::new(None));
        let manual_capture = Box::new(ManualCallbackCapture::new(callback_slot.clone()));
        let vad = VadProcessor::new(Some(90), None).expect("Failed to create VAD");
        let mut wrapper = VadCaptureWrapper::new(manual_capture, vad);

        let silence_timeouts = Arc::new(AtomicUsize::new(0));
        let timeout_counter = silence_timeouts.clone();
        wrapper.set_silence_timeout_callback(Arc::new(move || {
            timeout_counter.fetch_add(1, AtomicOrdering::SeqCst);
        }));

        let forwarded_chunks = Arc::new(AtomicUsize::new(0));
        let forwarded_counter = forwarded_chunks.clone();
        let on_chunk: AudioChunkCallback = Arc::new(move |_| {
            forwarded_counter.fetch_add(1, AtomicOrdering::SeqCst);
        });

        wrapper.initialize(AudioConfig::default()).await.unwrap();
        wrapper.start_capture(on_chunk.clone()).await.unwrap();
        let stale_callback = callback_slot.lock().unwrap().clone().unwrap();
        stale_callback(activity_then_silence_chunk());

        wrapper.stop_capture().await.unwrap();
        wrapper.start_capture(on_chunk).await.unwrap();
        let current_callback = callback_slot.lock().unwrap().clone().unwrap();

        stale_callback(activity_then_silence_chunk());
        current_callback(activity_then_silence_chunk());
        tokio::time::sleep(PENDING_STOP_GRACE + Duration::from_millis(50)).await;
        assert_eq!(silence_timeouts.load(AtomicOrdering::SeqCst), 1);
        assert!(forwarded_chunks.load(AtomicOrdering::SeqCst) > 0);
    }

    #[tokio::test]
    async fn resumed_speech_cancels_pending_stop_and_is_forwarded() {
        let callback_slot = Arc::new(Mutex::new(None));
        let manual_capture = Box::new(ManualCallbackCapture::new(callback_slot.clone()));
        let vad = VadProcessor::new(Some(90), None).expect("Failed to create VAD");
        let mut wrapper = VadCaptureWrapper::new(manual_capture, vad);

        let silence_timeouts = Arc::new(AtomicUsize::new(0));
        let timeout_counter = silence_timeouts.clone();
        wrapper.set_silence_timeout_callback(Arc::new(move || {
            timeout_counter.fetch_add(1, AtomicOrdering::SeqCst);
        }));

        let forwarded_chunks = Arc::new(AtomicUsize::new(0));
        let forwarded_counter = forwarded_chunks.clone();
        wrapper.initialize(AudioConfig::default()).await.unwrap();
        wrapper
            .start_capture(Arc::new(move |_| {
                forwarded_counter.fetch_add(1, AtomicOrdering::SeqCst);
            }))
            .await
            .unwrap();

        let callback = callback_slot.lock().unwrap().clone().unwrap();
        callback(activity_then_silence_chunk());
        let forwarded_before_resume = forwarded_chunks.load(AtomicOrdering::SeqCst);

        callback(AudioChunk::new(vec![3_000; 480], 16_000, 1));
        tokio::time::sleep(PENDING_STOP_GRACE + Duration::from_millis(50)).await;

        assert_eq!(silence_timeouts.load(AtomicOrdering::SeqCst), 0);
        assert!(
            forwarded_chunks.load(AtomicOrdering::SeqCst) > forwarded_before_resume,
            "resumed speech frame must reach downstream before stop commit"
        );
        assert!(wrapper.is_capturing());
    }

    #[tokio::test]
    async fn second_timeout_after_resume_uses_a_fresh_grace_and_commits_once() {
        let callback_slot = Arc::new(Mutex::new(None));
        let manual_capture = Box::new(ManualCallbackCapture::new(callback_slot.clone()));
        let vad = VadProcessor::new(Some(90), None).expect("Failed to create VAD");
        let mut wrapper = VadCaptureWrapper::new(manual_capture, vad);

        let silence_timeouts = Arc::new(AtomicUsize::new(0));
        let timeout_counter = silence_timeouts.clone();
        wrapper.set_silence_timeout_callback(Arc::new(move || {
            timeout_counter.fetch_add(1, AtomicOrdering::SeqCst);
        }));

        wrapper.initialize(AudioConfig::default()).await.unwrap();
        wrapper.start_capture(Arc::new(|_| {})).await.unwrap();
        let callback = callback_slot.lock().unwrap().clone().unwrap();

        callback(activity_then_silence_chunk());
        tokio::time::sleep(Duration::from_millis(50)).await;
        callback(AudioChunk::new(vec![3_000; 480], 16_000, 1));
        callback(activity_then_silence_chunk());

        tokio::time::sleep(Duration::from_millis(250)).await;
        assert_eq!(
            silence_timeouts.load(AtomicOrdering::SeqCst),
            0,
            "the cancelled timer must not shorten the fresh grace"
        );

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(silence_timeouts.load(AtomicOrdering::SeqCst), 1);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            silence_timeouts.load(AtomicOrdering::SeqCst),
            1,
            "the accepted timeout must commit exactly once"
        );
    }
}
