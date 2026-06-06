use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{Duration, Instant};

use crate::domain::{
    amplify_i16_samples, limited_microphone_gain, microphone_sensitivity_gain, AudioCapture,
    AudioChunk, AudioConfig, AudioLevelCallback, AudioSpectrumCallback, ConnectionQualityCallback,
    ErrorCallback, RecordingStatus, SttConfig, SttError, SttProvider, SttProviderFactory,
    SttProviderType, TranscriptionCallback,
};

use crate::application::AudioSpectrumAnalyzer;

type Result<T> = anyhow::Result<T>;

const AUDIO_PROCESSOR_STOP_DRAIN_TIMEOUT: Duration = Duration::from_millis(2500);
const PRESTART_VISUALIZER_QUEUE_CAPACITY: usize = 64;

struct PreparedAudioChunk {
    max_amplitude: i32,
    normalized_level: f32,
    requested_gain: f32,
    effective_gain: f32,
    amplified_chunk: AudioChunk,
}

fn keep_alive_enabled_for_config(config: &SttConfig) -> bool {
    config.keep_connection_alive && config.provider != SttProviderType::Backend
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

fn emit_audio_visualization(
    chunk_count: usize,
    prepared: &PreparedAudioChunk,
    spectrum: &mut AudioSpectrumAnalyzer,
    on_audio_level: &AudioLevelCallback,
    on_audio_spectrum: &AudioSpectrumCallback,
) {
    // На первом чанке эмитим сразу, чтобы mini-window ожило ещё во время STT startup.
    if chunk_count == 1 || chunk_count % 2 == 0 {
        on_audio_level(prepared.normalized_level);
    }

    // Берем усиленный звук, чтобы визуализация соответствовала тому, что слышит STT.
    if let Some(bars) = spectrum.push_samples(&prepared.amplified_chunk.data) {
        on_audio_spectrum(bars);
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
    stt_factory: Arc<dyn SttProviderFactory>,
    stt_provider: Arc<RwLock<Option<Box<dyn SttProvider>>>>,
    status: Arc<RwLock<RecordingStatus>>,
    config: Arc<RwLock<SttConfig>>,
    microphone_sensitivity: Arc<AtomicU8>, // 0-200, default 100
    inactivity_timer_task: Arc<RwLock<Option<tokio::task::JoinHandle<()>>>>, // таймер для автоочистки соединения
    audio_processor_task: Arc<RwLock<Option<tokio::task::JoinHandle<()>>>>, // обработчик аудио-чанков → STT
}

impl TranscriptionService {
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
        Self {
            audio_capture: Arc::new(RwLock::new(audio_capture)),
            stt_factory,
            stt_provider: Arc::new(RwLock::new(None)),
            status: Arc::new(RwLock::new(RecordingStatus::Idle)),
            config: Arc::new(RwLock::new(SttConfig::default())),
            microphone_sensitivity,
            inactivity_timer_task: Arc::new(RwLock::new(None)),
            audio_processor_task: Arc::new(RwLock::new(None)),
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

    async fn drain_audio_processor_task(&self, reason: &str) {
        let Some(mut task) = self.audio_processor_task.write().await.take() else {
            return;
        };

        tokio::select! {
            result = &mut task => {
                if let Err(e) = result {
                    if e.is_cancelled() {
                        log::debug!("Audio processor task cancelled while draining: {}", reason);
                    } else {
                        log::warn!("Audio processor task failed while draining ({}): {}", reason, e);
                    }
                }
            }
            _ = tokio::time::sleep(AUDIO_PROCESSOR_STOP_DRAIN_TIMEOUT) => {
                log::warn!(
                    "Audio processor did not drain within {:?} while {}; aborting",
                    AUDIO_PROCESSOR_STOP_DRAIN_TIMEOUT,
                    reason
                );
                task.abort();
                let _ = task.await;
            }
        }
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
        let mut status = self.status.write().await;

        if *status != RecordingStatus::Idle {
            anyhow::bail!("Already recording or starting");
        }

        // Устанавливаем статус Starting чтобы заблокировать повторные вызовы
        *status = RecordingStatus::Starting;
        drop(status);

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

        let startup_started_at = Instant::now();

        // Канал для передачи аудио чанков из нативного потока в async контекст.
        //
        // Важно: канал ДОЛЖЕН быть bounded. Иначе при плохой сети/подвисшем WS send()
        // мы можем накопить гигабайты аудио в памяти и уронить приложение.
        let (tx, mut rx) = tokio::sync::mpsc::channel(256);
        let (visual_tx, mut visual_rx) =
            tokio::sync::mpsc::channel(PRESTART_VISUALIZER_QUEUE_CAPACITY);

        let dropped_chunks = Arc::new(AtomicUsize::new(0));
        let dropped_chunks_for_cb = dropped_chunks.clone();
        let dropped_chunks_for_processor = dropped_chunks.clone();
        let prestart_visual_active = Arc::new(AtomicBool::new(true));
        let prestart_visual_active_for_cb = prestart_visual_active.clone();
        let on_chunk = Arc::new(move |chunk: crate::domain::AudioChunk| {
            let visual_chunk = if prestart_visual_active_for_cb.load(Ordering::Relaxed) {
                Some(chunk.clone())
            } else {
                None
            };

            // Не блокируем захват аудио: если бэкенд не успевает принимать,
            // просто дропаем чанки. Пользователь всё равно в этот момент получит
            // либо деградацию качества, либо ошибку/остановку записи.
            match tx.try_send(chunk) {
                Ok(_) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Full(_chunk)) => {
                    let dropped = dropped_chunks_for_cb.fetch_add(1, Ordering::Relaxed) + 1;
                    // Логируем редко, чтобы не спамить.
                    if dropped == 1 || dropped % 100 == 0 {
                        log::warn!(
                            "Audio queue is full (dropping chunks) - likely network/WS stall (dropped so far: {})",
                            dropped
                        );
                    }
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_chunk)) => {
                    // Запись уже остановлена/перезапущена - молча игнорируем.
                }
            }

            // Отдельная bounded-очередь только для prestart-визуализации.
            // Основной STT audio queue остаётся нетронутым, поэтому ранняя речь не теряется.
            if let Some(visual_chunk) = visual_chunk {
                let _ = visual_tx.try_send(visual_chunk);
            }
        });

        if let Err(e) = self
            .audio_capture
            .write()
            .await
            .start_capture(on_chunk.clone())
            .await
        {
            log::error!("Failed to start audio capture: {}", e);

            // Возвращаем статус в Idle, чтобы UI мог восстановиться.
            *self.status.write().await = RecordingStatus::Idle;

            return Err(anyhow::Error::new(e).context("Failed to start audio capture"));
        }

        let audio_capture_started_after = startup_started_at.elapsed();
        log::info!(
            "[StartLatencyDiag] audio capture started before STT setup (after_ms={})",
            audio_capture_started_after.as_millis()
        );

        let prestart_visual_status = self.status.clone();
        let prestart_visual_sensitivity = self.microphone_sensitivity.clone();
        let prestart_on_audio_level = on_audio_level.clone();
        let prestart_on_audio_spectrum = on_audio_spectrum.clone();
        let mut prestart_visual_task = Some(tokio::spawn(async move {
            let mut chunk_count = 0usize;
            let mut spectrum = AudioSpectrumAnalyzer::new();

            while let Some(chunk) = visual_rx.recv().await {
                let status = *prestart_visual_status.read().await;
                if status != RecordingStatus::Starting {
                    break;
                }

                chunk_count += 1;
                let sensitivity = prestart_visual_sensitivity.load(Ordering::Relaxed);
                let prepared = prepare_audio_chunk_for_processing(&chunk, sensitivity);
                emit_audio_visualization(
                    chunk_count,
                    &prepared,
                    &mut spectrum,
                    &prestart_on_audio_level,
                    &prestart_on_audio_spectrum,
                );
            }

            log::debug!(
                "Prestart audio visualizer finished, total chunks: {}",
                chunk_count
            );
        }));

        // Проверяем можно ли переиспользовать существующее соединение
        let config = self.config.read().await.clone();
        let (mut can_reuse_connection, mut reuse_decision_reason) = {
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
                    supports_keep_alive && is_connection_alive && keep_alive_enabled,
                    format!(
                        "provider={}, supports_keep_alive={}, is_connection_alive={}, keep_alive_enabled={}",
                        provider.name(),
                        supports_keep_alive,
                        is_connection_alive,
                        keep_alive_enabled
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
                    provider
                        .resume_stream(
                            on_partial.clone(),
                            on_final.clone(),
                            on_error.clone(),
                            on_connection_quality.clone(),
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
                        let _ = provider.abort().await;
                    }
                    can_reuse_connection = false;
                }
            }
        }

        if !can_reuse_connection {
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
                    let _ = self.audio_capture.write().await.stop_capture().await;
                    *self.status.write().await = RecordingStatus::Idle;
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
                let _ = self.audio_capture.write().await.stop_capture().await;
                *self.status.write().await = RecordingStatus::Idle;
                let _ = provider.abort().await;
                abort_prestart_visualizer_task(
                    &mut prestart_visual_task,
                    &prestart_visual_active,
                    "failed to initialize STT provider",
                );
                return Err(anyhow::Error::new(e).context("Failed to initialize STT provider"));
            }

            if let Err(e) = provider
                .start_stream(
                    on_partial.clone(),
                    on_final.clone(),
                    on_error.clone(),
                    on_connection_quality.clone(),
                )
                .await
            {
                let _ = self.audio_capture.write().await.stop_capture().await;
                *self.status.write().await = RecordingStatus::Idle;
                let _ = provider.abort().await;
                abort_prestart_visualizer_task(
                    &mut prestart_visual_task,
                    &prestart_visual_active,
                    "failed to start STT stream",
                );
                return Err(anyhow::Error::new(e).context("Failed to start STT stream"));
            }

            *self.stt_provider.write().await = Some(provider);
        }

        // Теперь STT готов принимать аудио. Переводим статус в Recording до запуска processor task,
        // чтобы предзахваченные чанки из очереди не были отброшены как "ещё Starting".
        *self.status.write().await = RecordingStatus::Recording;
        abort_prestart_visualizer_task(
            &mut prestart_visual_task,
            &prestart_visual_active,
            "STT stream started",
        );

        // Запускаем обработчик чанков в async контексте
        let stt_provider = self.stt_provider.clone();
        let status_arc = self.status.clone();
        let sensitivity_arc = self.microphone_sensitivity.clone();
        let on_error_for_processor = on_error.clone();
        let audio_capture = self.audio_capture.clone();
        let on_connection_quality_for_processor = on_connection_quality.clone();
        let on_chunk_for_restart = on_chunk.clone();

        let processor_task = tokio::spawn(async move {
            let mut chunk_count = 0;
            let mut consecutive_errors: u32 = 0;
            const MAX_CONSECUTIVE_ERRORS: u32 = 10;
            let mut spectrum = AudioSpectrumAnalyzer::new();
            let mut last_quality: Option<&'static str> = None;
            let mut good_streak: u32 = 0;
            let mut last_dropped_seen: usize = 0;
            let mut last_audio_at = Instant::now();
            let mut stall_restarts: u32 = 0;

            // На macOS/некоторых девайсах при отсутствии разрешения на микрофон или при "пустом" input
            // CoreAudio может отдавать строго нулевые семплы. Это выглядит как "всё работает", но речи нет.
            let mut consecutive_all_zero_chunks: u32 = 0;
            const ALL_ZERO_WARN_THRESHOLD: u32 = 60; // ~1-2 секунды (зависит от размера чанка)
            const ALL_ZERO_FATAL_THRESHOLD: u32 = 240; // ~6-8 секунд

            const AUDIO_STALL_TIMEOUT: Duration = Duration::from_millis(2200);
            const AUDIO_STALL_CHECK_INTERVAL: Duration = Duration::from_millis(650);
            const MAX_AUDIO_STALL_RESTARTS: u32 = 3;

            loop {
                let maybe_chunk = tokio::select! {
                    v = rx.recv() => v,
                    _ = tokio::time::sleep(AUDIO_STALL_CHECK_INTERVAL) => {
                        // Если долго не приходят чанки — захват аудио мог "отвалиться"
                        // (например, микрофон был отключён/переключён на уровне ОС).
                        let status = status_arc.read().await;
                        if *status == RecordingStatus::Processing {
                            log::debug!("Audio processor finished drain after recording stop");
                            break;
                        }
                        if *status != RecordingStatus::Recording {
                            continue;
                        }
                        drop(status);

                        if last_audio_at.elapsed() < AUDIO_STALL_TIMEOUT {
                            continue;
                        }

                        stall_restarts = stall_restarts.saturating_add(1);
                        log::warn!(
                            "Audio capture stalled (no chunks for {:?}). Restart attempt {}/{}",
                            AUDIO_STALL_TIMEOUT,
                            stall_restarts,
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
                            let _ = cap.stop_capture().await;
                            cap.start_capture(on_chunk_for_restart.clone()).await
                        };

                        match restart_result {
                            Ok(_) => {
                                log::info!("Audio capture restarted successfully after stall");
                                last_audio_at = Instant::now();
                                stall_restarts = 0;
                                on_connection_quality_for_processor(
                                    "Recovering".to_string(),
                                    Some("Аудио восстановлено".to_string()),
                                );
                                last_quality = Some("Recovering");
                                continue;
                            }
                            Err(e) => {
                                log::error!("Failed to restart audio capture after stall: {}", e);
                                if stall_restarts < MAX_AUDIO_STALL_RESTARTS {
                                    // Дадим шанс восстановиться (например, устройство вот-вот появится).
                                    continue;
                                }

                                // Фатально: возвращаем сервис в Idle, чтобы UI/хоткей не залипали,
                                // и отправляем ошибку в UI.
                                let raw = format!("Audio device is no longer available: {}", e);
                                on_error_for_processor(SttError::Processing(raw));
                                *status_arc.write().await = RecordingStatus::Idle;
                                let _ = audio_capture.write().await.stop_capture().await;
                                break;
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
                if status != RecordingStatus::Recording && status != RecordingStatus::Processing {
                    continue;
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
                    *status_arc.write().await = RecordingStatus::Idle;
                    let _ = audio_capture.write().await.stop_capture().await;
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

                emit_audio_visualization(
                    chunk_count,
                    &prepared,
                    &mut spectrum,
                    &on_audio_level,
                    &on_audio_spectrum,
                );
                let amplified_chunk = prepared.amplified_chunk;

                // Логируем каждый 20-й чанк для отладки
                if chunk_count % 20 == 0 {
                    let amplified_max: i32 = amplified_chunk
                        .data
                        .iter()
                        .map(|&s| (s as i32).abs())
                        .max()
                        .unwrap_or(0);
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
                if provider_guard.is_none() {
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
                    *status_arc.write().await = RecordingStatus::Idle;
                    let _ = audio_capture.write().await.stop_capture().await;
                    break;
                }

                if chunk_count == 1 || chunk_count % 50 == 0 {
                    log::debug!(
                        "Processing audio chunk #{}, {} samples, max_amp={}",
                        chunk_count,
                        amplified_chunk.data.len(),
                        max_amplitude
                    );
                }

                let send_result = provider_guard
                    .as_mut()
                    .expect("checked above")
                    .send_audio(&amplified_chunk)
                    .await;

                match send_result {
                    Ok(_) => {
                        // Успешная отправка — сбрасываем счётчик ошибок
                        if consecutive_errors > 0 {
                            // Мы только что восстановились после ошибок отправки.
                            on_connection_quality_for_processor(
                                "Recovering".to_string(),
                                Some("Соединение восстанавливается".to_string()),
                            );
                            last_quality = Some("Recovering");
                            good_streak = 0;
                        }
                        consecutive_errors = 0;
                        if last_quality == Some("Recovering") {
                            good_streak += 1;
                            if good_streak >= 20 {
                                on_connection_quality_for_processor("Good".to_string(), None);
                                last_quality = Some("Good");
                                good_streak = 0;
                            }
                        }
                    }
                    Err(e) => {
                        // Определяем тип ошибки и критичность по ТИПУ, а не по парсингу строки.
                        let (error_type, is_critical) = match &e {
                            SttError::Authentication(_) => ("authentication", true),
                            SttError::Configuration(_) => ("configuration", true),
                            SttError::Connection(conn) => {
                                if conn.details.category
                                    == Some(crate::domain::SttConnectionCategory::LimitExceeded)
                                {
                                    ("limit_exceeded", true)
                                } else if conn.details.category
                                    == Some(crate::domain::SttConnectionCategory::Timeout)
                                {
                                    ("timeout", false)
                                } else {
                                    ("connection", false)
                                }
                            }
                            SttError::Processing(_) | SttError::Internal(_) => {
                                ("processing", false)
                            }
                            SttError::Unsupported(_) => ("processing", true),
                        };

                        if is_critical {
                            log::error!("STT critical error ({}): {}", error_type, e);
                            on_error_for_processor(e.clone());
                            on_connection_quality_for_processor(
                                "Poor".to_string(),
                                Some("Критическая ошибка соединения".to_string()),
                            );

                            // Критическая ошибка — останавливаем запись аккуратно.
                            *status_arc.write().await = RecordingStatus::Idle;
                            let _ = audio_capture.write().await.stop_capture().await;

                            // И выкидываем провайдера, чтобы не оставлять "висящие" WS/таски.
                            let old_provider = provider_guard.take();
                            drop(provider_guard);
                            if let Some(mut old) = old_provider {
                                let _ = old.abort().await;
                            }

                            break;
                        }

                        consecutive_errors += 1;
                        good_streak = 0;

                        // Логируем не слишком часто чтобы не спамить
                        if consecutive_errors <= 3 {
                            log::warn!(
                                "STT temporary error ({}): {} - continuing ({}/{})",
                                error_type,
                                e,
                                consecutive_errors,
                                MAX_CONSECUTIVE_ERRORS
                            );
                        }

                        // Если слишком много ошибок подряд — останавливаем запись, иначе UI может "залипнуть".
                        if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                            log::error!(
                                "Too many consecutive errors ({}), stopping recording to avoid stuck state",
                                consecutive_errors
                            );
                            on_error_for_processor(e.clone());
                            on_connection_quality_for_processor(
                                "Poor".to_string(),
                                Some("Соединение нестабильно, запись остановлена".to_string()),
                            );

                            *status_arc.write().await = RecordingStatus::Idle;
                            let _ = audio_capture.write().await.stop_capture().await;

                            let old_provider = provider_guard.take();
                            drop(provider_guard);
                            if let Some(mut old) = old_provider {
                                let _ = old.abort().await;
                            }

                            break;
                        }

                        // На первой же ошибке сигнализируем Poor (если ещё не сигнализировали).
                        if consecutive_errors == 1 && last_quality != Some("Poor") {
                            on_connection_quality_for_processor(
                                "Poor".to_string(),
                                Some(format!("{}: {}", error_type, e)),
                            );
                            last_quality = Some("Poor");
                        }
                    }
                }

                // При штатной остановке запись уже в Processing, capture остановлен,
                // но processor мог держать служебный clone sender для restart-аудио.
                // Поэтому не ждём закрытия канала бесконечно: когда очередь пуста, drain завершён.
                if *status_arc.read().await == RecordingStatus::Processing && rx.is_empty() {
                    log::debug!("Audio processor queue drained after recording stop");
                    break;
                }
            }
            log::info!(
                "Audio chunk processor finished, total chunks: {}",
                chunk_count
            );
        });

        *self.audio_processor_task.write().await = Some(processor_task);

        log::info!(
            "Recording started (audio_capture_started_after_ms={}, total_start_ms={}, prebuffer_dropped_chunks={})",
            audio_capture_started_after.as_millis(),
            startup_started_at.elapsed().as_millis(),
            dropped_chunks.load(Ordering::Relaxed)
        );
        Ok(())
    }

    /// Stop recording and finalize transcription
    pub async fn stop_recording(&self) -> Result<String> {
        let mut status = self.status.write().await;

        if *status != RecordingStatus::Recording {
            anyhow::bail!("Not recording");
        }

        *status = RecordingStatus::Processing;
        drop(status);

        // Stop audio capture
        let stop_capture_result = self.audio_capture.write().await.stop_capture().await;

        // Если не смогли остановить захват аудио — считаем это критическим сценарием:
        // лучше упасть с ошибкой, но гарантированно вернуть сервис в Idle, чем зависнуть в Processing.
        if let Err(e) = stop_capture_result {
            log::error!("Failed to stop audio capture: {}", e);

            self.abort_audio_processor_task("audio capture failed to stop")
                .await;

            // Закрываем провайдера, чтобы не оставлять "полуживой" WS/таски.
            if let Some(mut provider) = self.stt_provider.write().await.take() {
                let _ = provider.abort().await;
            }

            *self.status.write().await = RecordingStatus::Idle;
            return Err(anyhow::anyhow!("Failed to stop audio capture: {}", e));
        }

        // После stop_capture sender аудио-чанков должен закрыться. Не abort'им processor сразу:
        // в очереди могли остаться последние чанки речи, и именно они дают "обрезанный хвост".
        self.drain_audio_processor_task("stopping recording").await;

        // Проверяем нужно ли держать соединение открытым (keep-alive режим)
        let config = self.config.read().await.clone();
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
                    supports_keep_alive && keep_alive_enabled,
                    format!(
                        "provider={}, supports_keep_alive={}, keep_alive_enabled={}, alive_before_pause={}",
                        provider.name(),
                        supports_keep_alive,
                        keep_alive_enabled,
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
                None => {
                    // Провайдера нет, но захват аудио уже остановили — считаем что запись завершена.
                    *self.status.write().await = RecordingStatus::Idle;
                    return Ok("Recording stopped".to_string());
                }
            };

            if let Err(e) = provider.pause_stream().await {
                log::warn!(
                    "[ReconnectDiag] failed to pause STT stream (keep-alive). Falling back to hard close: {}",
                    e
                );

                // Фоллбек: закрываем соединение полностью, чтобы не держать "полуживой" провайдер.
                let _ = provider.abort().await;

                *self.status.write().await = RecordingStatus::Idle;
                return Ok("Recording stopped".to_string());
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
                        let _ = provider.stop_stream().await;
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

            if let Some(mut provider) = self.stt_provider.write().await.take() {
                if let Err(e) = provider.stop_stream().await {
                    log::warn!("Failed to stop STT stream cleanly, aborting: {}", e);
                    let _ = provider.abort().await;
                }
            }

            *self.status.write().await = RecordingStatus::Idle;

            log::info!("Recording stopped");
            Ok("Transcription completed".to_string())
        }
    }

    /// Жёсткая остановка: всегда закрывает STT stream и выкидывает провайдера, без keep-alive.
    ///
    /// Нужна для hotkey сценария: пользователь ожидает новую "сессию" с чистого листа при следующем открытии окна,
    /// и мы не должны получать отложенные partial/final от предыдущей речи после возобновления соединения.
    pub async fn stop_recording_hard(&self) -> Result<String> {
        let mut status = self.status.write().await;

        if *status != RecordingStatus::Recording {
            anyhow::bail!("Not recording");
        }

        *status = RecordingStatus::Processing;
        drop(status);

        // Stop audio capture
        let stop_capture_result = self.audio_capture.write().await.stop_capture().await;

        if let Err(e) = stop_capture_result {
            log::error!("Failed to stop audio capture: {}", e);

            self.abort_audio_processor_task("audio capture failed to stop during hard stop")
                .await;

            // Жёсткий фоллбек: закрываем провайдера, чтобы гарантировать чистое состояние.
            if let Some(mut provider) = self.stt_provider.write().await.take() {
                let _ = provider.abort().await;
            }

            *self.status.write().await = RecordingStatus::Idle;
            return Err(anyhow::anyhow!("Failed to stop audio capture: {}", e));
        }

        // Даже при hard-stop сначала досылаем уже принятый хвост аудио в STT,
        // потом закрываем stream. Иначе последние слова могут не попасть в финализацию.
        self.drain_audio_processor_task("hard-stopping recording")
            .await;

        // Отменяем таймер неактивности, если он был запущен (на всякий случай)
        if let Some(timer) = self.inactivity_timer_task.write().await.take() {
            timer.abort();
            let _ = timer.await;
        }

        // Жёстко закрываем провайдера и соединение
        if let Some(mut provider) = self.stt_provider.write().await.take() {
            if let Err(e) = provider.stop_stream().await {
                log::warn!("Failed to stop STT stream cleanly, aborting: {}", e);
                let _ = provider.abort().await;
            }
        }

        *self.status.write().await = RecordingStatus::Idle;
        log::info!("Recording stopped (hard), provider connection closed");
        Ok("Transcription completed".to_string())
    }

    /// Get current recording status
    pub async fn get_status(&self) -> RecordingStatus {
        *self.status.read().await
    }

    /// Returns true when the next start can resume an already-open keep-alive stream
    /// without creating a new WebSocket connection.
    pub async fn can_resume_keep_alive_connection(&self) -> bool {
        let config = self.config.read().await.clone();
        let keep_alive_enabled = keep_alive_enabled_for_config(&config);

        if !keep_alive_enabled {
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
        let prev_config = self.config.read().await.clone();

        // Не принуждаем backend к runtime keep-alive: после Finalize backend/provider stream
        // может остаться живым, но перестать отдавать transcript для следующей записи.
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
        let config_requires_new_connection = prev_config.provider != config.provider
            || prev_config.backend_streaming_provider != config.backend_streaming_provider
            || prev_config.language != config.language
            || prev_config.streaming_keyterms != config.streaming_keyterms;

        if config_requires_new_connection {
            let status = *self.status.read().await;
            if status == RecordingStatus::Idle {
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
                        if let Err(e) = provider.stop_stream().await {
                            log::warn!(
                                "Failed to stop keep-alive stream on config change, aborting: {}",
                                e
                            );
                            let _ = provider.abort().await;
                        }
                    }
                }
            } else {
                // Если запись идёт — не вмешиваемся. Новая конфигурация применится на следующей сессии.
                log::info!(
                    "STT config updated while status={:?}; keep-alive connection will not be reset until idle",
                    status
                );
            }
        }

        *self.config.write().await = config;
        Ok(())
    }

    /// Get current configuration
    pub async fn get_config(&self) -> SttConfig {
        self.config.read().await.clone()
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
        let status = self.status.read().await;

        // Нельзя менять устройство во время записи
        if *status != RecordingStatus::Idle {
            anyhow::bail!(
                "Cannot replace audio capture while recording (current status: {:?})",
                *status
            );
        }

        drop(status); // освобождаем read lock

        log::info!("Replacing audio capture device");
        *self.audio_capture.write().await = new_capture;
        log::info!("Audio capture device replaced successfully");

        Ok(())
    }
}

// Ensure TranscriptionService is thread-safe
unsafe impl Send for TranscriptionService {}
unsafe impl Sync for TranscriptionService {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{AudioResult, SttResult};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::time::Duration;

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
        aborted: Arc<AtomicBool>,
    }

    impl SttProviderFactory for TestFactory {
        fn create(&self, _config: &SttConfig) -> SttResult<Box<dyn SttProvider>> {
            Ok(Box::new(AlwaysFailSendProvider {
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
        stopped_count: Arc<AtomicUsize>,
        aborted_count: Arc<AtomicUsize>,
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
        stopped_count: Arc<AtomicUsize>,
        aborted_count: Arc<AtomicUsize>,
    }

    impl SttProviderFactory for KeepAliveFactory {
        fn create(&self, config: &SttConfig) -> SttResult<Box<dyn SttProvider>> {
            self.selected_providers
                .lock()
                .expect("selected providers mutex poisoned")
                .push(config.backend_streaming_provider);

            Ok(Box::new(KeepAliveProvider {
                paused_count: self.paused_count.clone(),
                stopped_count: self.stopped_count.clone(),
                aborted_count: self.aborted_count.clone(),
                is_paused: false,
                is_closed: false,
            }))
        }
    }

    #[tokio::test]
    async fn backend_provider_does_not_reuse_keep_alive_between_recordings() {
        let on_chunk_slot: Arc<std::sync::Mutex<Option<crate::domain::AudioChunkCallback>>> =
            Arc::new(std::sync::Mutex::new(None));
        let selected_providers = Arc::new(std::sync::Mutex::new(Vec::new()));
        let paused_count = Arc::new(AtomicUsize::new(0));
        let stopped_count = Arc::new(AtomicUsize::new(0));
        let aborted_count = Arc::new(AtomicUsize::new(0));

        let audio_capture = ManualAudioCapture::new(on_chunk_slot);
        let factory = Arc::new(KeepAliveFactory {
            selected_providers: selected_providers.clone(),
            paused_count: paused_count.clone(),
            stopped_count: stopped_count.clone(),
            aborted_count: aborted_count.clone(),
        });
        let service = TranscriptionService::new(Box::new(audio_capture), factory);

        let mut initial = SttConfig::new(SttProviderType::Backend);
        initial.backend_streaming_provider = crate::domain::BackendStreamingProvider::Deepgram;
        service.update_config(initial.clone()).await.unwrap();

        let on_partial: TranscriptionCallback = Arc::new(|_t| {});
        let on_final: TranscriptionCallback = Arc::new(|_t| {});
        let on_audio_level: AudioLevelCallback = Arc::new(|_l| {});
        let on_audio_spectrum: AudioSpectrumCallback = Arc::new(|_b| {});
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

        assert_eq!(paused_count.load(Ordering::SeqCst), 0);
        assert_eq!(stopped_count.load(Ordering::SeqCst), 1);
        assert!(!service.can_resume_keep_alive_connection().await);

        let mut next = initial;
        next.backend_streaming_provider = crate::domain::BackendStreamingProvider::ElevenLabs;
        service.update_config(next).await.unwrap();

        assert_eq!(stopped_count.load(Ordering::SeqCst), 1);
        assert_eq!(aborted_count.load(Ordering::SeqCst), 0);
        assert!(!service.can_resume_keep_alive_connection().await);

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
            .expect("second recording must start with new provider");
        service
            .stop_recording()
            .await
            .expect("second recording must stop");

        assert_eq!(paused_count.load(Ordering::SeqCst), 0);
        assert_eq!(stopped_count.load(Ordering::SeqCst), 2);

        let selected = selected_providers
            .lock()
            .expect("selected providers mutex poisoned")
            .clone();
        assert_eq!(
            selected,
            vec![
                crate::domain::BackendStreamingProvider::Deepgram,
                crate::domain::BackendStreamingProvider::ElevenLabs
            ]
        );

        let mut cleanup = SttConfig::new(SttProviderType::Backend);
        cleanup.backend_streaming_provider = crate::domain::BackendStreamingProvider::Deepgram;
        service.update_config(cleanup).await.unwrap();
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
        let on_audio_level: AudioLevelCallback = Arc::new(|_l| {});
        let on_audio_spectrum: AudioSpectrumCallback = Arc::new(|_b| {});
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
        let on_audio_level: AudioLevelCallback = Arc::new(|_l| {});
        let on_audio_spectrum: AudioSpectrumCallback = Arc::new(move |_b| {
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
        let on_audio_level: AudioLevelCallback = Arc::new(|_l| {});
        let on_audio_spectrum: AudioSpectrumCallback = Arc::new(|_b| {});
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
    async fn stops_recording_and_cleans_up_after_many_connection_errors() {
        let provider_aborted = Arc::new(AtomicBool::new(false));
        let capture_stopped = Arc::new(AtomicBool::new(false));
        let got_poor_quality = Arc::new(AtomicBool::new(false));

        let audio_capture = BurstAudioCapture::new(capture_stopped.clone(), 32);
        let factory = Arc::new(TestFactory {
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
                SttError::Processing(_) | SttError::Internal(_) | SttError::Unsupported(_) => {
                    "processing"
                }
            }
            .to_string();
            let _ = err_tx.send((err.to_string(), typ));
        });

        let on_partial: TranscriptionCallback = Arc::new(|_t| {});
        let on_final: TranscriptionCallback = Arc::new(|_t| {});
        let on_audio_level: AudioLevelCallback = Arc::new(|_l| {});
        let on_audio_spectrum: AudioSpectrumCallback = Arc::new(|_b| {});
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

        // Должны получить ошибку после накопления MAX_CONSECUTIVE_ERRORS.
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
    async fn does_not_start_provider_if_audio_capture_fails_to_start() {
        let provider_aborted = Arc::new(AtomicBool::new(false));

        let audio_capture = FailingStartAudioCapture::default();
        let factory = Arc::new(TestFactory {
            aborted: provider_aborted.clone(),
        });
        let service = TranscriptionService::new(Box::new(audio_capture), factory);

        let on_partial: TranscriptionCallback = Arc::new(|_t| {});
        let on_final: TranscriptionCallback = Arc::new(|_t| {});
        let on_audio_level: AudioLevelCallback = Arc::new(|_l| {});
        let on_audio_spectrum: AudioSpectrumCallback = Arc::new(|_b| {});
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
            aborted: provider_aborted.clone(),
        });
        let service = TranscriptionService::new(Box::new(audio_capture), factory);

        let on_partial: TranscriptionCallback = Arc::new(|_t| {});
        let on_final: TranscriptionCallback = Arc::new(|_t| {});
        let on_audio_level: AudioLevelCallback = Arc::new(|_l| {});
        let on_audio_spectrum: AudioSpectrumCallback = Arc::new(|_b| {});
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

        // stop_recording вернёт ошибку, но статус обязан откатиться в Idle.
        let result = service.stop_recording().await;
        assert!(result.is_err());
        assert!(stop_called.load(Ordering::SeqCst));
        assert_eq!(service.get_status().await, RecordingStatus::Idle);
        assert!(provider_aborted.load(Ordering::SeqCst));
    }
}
