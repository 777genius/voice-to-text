use std::sync::Arc;
use tokio::sync::RwLock;

use crate::domain::{
    AudioCapture, AudioConfig, AudioLevelCallback, ErrorCallback, RecordingStatus, SttConfig, SttProvider,
    SttProviderFactory, TranscriptionCallback,
};

type Result<T> = anyhow::Result<T>;

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
    microphone_sensitivity: Arc<RwLock<u8>>, // 0-200, default 95
    inactivity_timer_task: Arc<RwLock<Option<tokio::task::JoinHandle<()>>>>, // таймер для автоочистки соединения
}

impl TranscriptionService {
    pub fn new(
        audio_capture: Box<dyn AudioCapture>,
        stt_factory: Arc<dyn SttProviderFactory>,
    ) -> Self {
        Self {
            audio_capture: Arc::new(RwLock::new(audio_capture)),
            stt_factory,
            stt_provider: Arc::new(RwLock::new(None)),
            status: Arc::new(RwLock::new(RecordingStatus::Idle)),
            config: Arc::new(RwLock::new(SttConfig::default())),
            microphone_sensitivity: Arc::new(RwLock::new(95)), // Default 95% (порог ~1638, более чувствительный)
            inactivity_timer_task: Arc::new(RwLock::new(None)),
        }
    }

    /// Update microphone sensitivity (0-200)
    pub async fn set_microphone_sensitivity(&self, sensitivity: u8) {
        *self.microphone_sensitivity.write().await = sensitivity.min(200);
    }

    /// Start recording and transcription
    pub async fn start_recording(
        &self,
        on_partial: TranscriptionCallback,
        on_final: TranscriptionCallback,
        on_audio_level: AudioLevelCallback,
        on_error: ErrorCallback,
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

        // Проверяем можно ли переиспользовать существующее соединение
        let config = self.config.read().await.clone();
        let can_reuse_connection = {
            let provider_opt = self.stt_provider.read().await;
            if let Some(provider) = provider_opt.as_ref() {
                provider.supports_keep_alive()
                    && provider.is_connection_alive()
                    && config.keep_connection_alive
            } else {
                false
            }
        };

        if can_reuse_connection {
            // Переиспользуем существующее соединение (мгновенный старт!)
            log::info!("Reusing existing keep-alive connection (instant start)");

            let mut provider_opt = self.stt_provider.write().await;
            if let Some(provider) = provider_opt.as_mut() {
                provider
                    .resume_stream(on_partial.clone(), on_final.clone(), on_error.clone())
                    .await
                    .map_err(|e| {
                        let status_arc = self.status.clone();
                        tokio::spawn(async move { *status_arc.write().await = RecordingStatus::Idle; });
                        anyhow::anyhow!("Failed to resume STT stream: {}", e)
                    })?;
            }
        } else {
            // Создаем новое соединение (обычный старт с задержкой)
            log::info!("Creating new STT connection");

            let mut provider = self
                .stt_factory
                .create(&config)
                .map_err(|e| {
                    let status_arc = self.status.clone();
                    tokio::spawn(async move { *status_arc.write().await = RecordingStatus::Idle; });
                    anyhow::anyhow!("Failed to create STT provider: {}", e)
                })?;

            provider
                .initialize(&config)
                .await
                .map_err(|e| {
                    log::error!("Failed to initialize STT provider: {}", e);
                    let status_arc = self.status.clone();
                    tokio::spawn(async move { *status_arc.write().await = RecordingStatus::Idle; });
                    anyhow::anyhow!("Failed to initialize STT provider: {}", e)
                })?;

            provider
                .start_stream(on_partial.clone(), on_final.clone(), on_error.clone())
                .await
                .map_err(|e| {
                    let status_arc = self.status.clone();
                    tokio::spawn(async move { *status_arc.write().await = RecordingStatus::Idle; });
                    anyhow::anyhow!("Failed to start STT stream: {}", e)
                })?;

            *self.stt_provider.write().await = Some(provider);
        }

        // Создаем канал для передачи аудио чанков из нативного потока в async контекст
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let on_chunk = Arc::new(move |chunk: crate::domain::AudioChunk| {
            // Отправляем чанк через канал (работает из любого потока)
            let _ = tx.send(chunk);
        });

        // Запускаем обработчик чанков в async контексте
        let stt_provider = self.stt_provider.clone();
        let status_arc = self.status.clone();
        let sensitivity_arc = self.microphone_sensitivity.clone();
        let on_error_for_processor = on_error.clone();

        tokio::spawn(async move {
            let mut chunk_count = 0;
            while let Some(chunk) = rx.recv().await {
                chunk_count += 1;

                let status = status_arc.read().await;
                if *status != RecordingStatus::Recording {
                    continue;
                }
                drop(status);

                // Вычисляем уровень громкости для визуализации
                // Используем перцептивную нормализацию (корень квадратный) как в VU-метрах
                // Это делает индикатор более естественным: нормальная речь ~30-50% вместо ~9-24%
                let max_amplitude = chunk.data.iter().map(|&s| s.abs()).max().unwrap_or(0);
                let normalized_level = (max_amplitude as f32 / 32767.0).sqrt().min(1.0);

                // Вызываем callback для UI (не чаще чем каждые 50ms = ~каждый второй чанк)
                if chunk_count % 2 == 0 {
                    on_audio_level(normalized_level);
                }

                // Фильтруем по чувствительности микрофона
                // sensitivity: 0-200
                //   0% = низкая чувствительность = только громкие звуки (высокий порог)
                // 100% = высокая чувствительность = все звуки (порог 0)
                // 101-200% = максимальная чувствительность (порог 0)
                let sensitivity = *sensitivity_arc.read().await;

                // Вычисляем минимальный порог амплитуды
                // Низкая чувствительность (0%) = порог 32767 (почти невозможно)
                // Высокая чувствительность (100%+) = порог 0 (всё проходит)
                // Пример: sensitivity=95% -> threshold=1638 (тихая речь проходит)
                let threshold = if sensitivity >= 100 {
                    0 // При 100%+ пропускаем весь аудио сигнал
                } else {
                    ((100 - sensitivity) as f32 / 100.0 * 32767.0) as i16
                };

                if chunk_count == 1 {
                    log::debug!("Microphone sensitivity: {}%, threshold: {}", sensitivity, threshold);
                }

                // Логируем каждый 20-й чанк для отладки уровня сигнала
                if chunk_count % 20 == 0 {
                    log::debug!("Audio level check: chunk #{}, max_amp={}, threshold={}, passed={}",
                        chunk_count, max_amplitude, threshold, max_amplitude >= threshold);
                }

                if max_amplitude < threshold {
                    // Звук слишком тихий для текущей чувствительности - пропускаем
                    continue;
                }

                if let Some(provider) = stt_provider.write().await.as_mut() {
                    if chunk_count == 1 || chunk_count % 50 == 0 {
                        log::debug!("Processing audio chunk #{}, {} samples, max_amp={}",
                            chunk_count, chunk.data.len(), max_amplitude);
                    }

                    if let Err(e) = provider.send_audio(&chunk).await {
                        let error_msg = e.to_string();

                        // Определяем тип ошибки и критичность
                        let (error_type, is_critical) = if error_msg.contains("WebSocket connection closed")
                            || error_msg.contains("connection")
                        {
                            // Временная ошибка сети - продолжаем обработку
                            // Deepgram может восстановиться когда сеть вернется
                            ("connection", false)
                        } else if error_msg.contains("timeout") {
                            // Timeout тоже временный - сеть может быть медленной
                            ("timeout", false)
                        } else if error_msg.contains("API key") || error_msg.contains("auth") || error_msg.contains("401") {
                            // Критическая ошибка - неверные credentials
                            ("authentication", true)
                        } else if error_msg.contains("configuration") || error_msg.contains("invalid") {
                            // Критическая ошибка - неверная конфигурация
                            ("configuration", true)
                        } else {
                            // Неизвестная ошибка - считаем некритической
                            ("processing", false)
                        };

                        if is_critical {
                            log::error!("STT critical error ({}): {}", error_type, error_msg);

                            // Вызываем error callback для уведомления UI
                            on_error_for_processor(error_msg.clone(), error_type.to_string());

                            // Меняем статус на Error
                            *status_arc.write().await = RecordingStatus::Error;

                            // Останавливаем обработку при критической ошибке
                            break;
                        } else {
                            // Временная ошибка - логируем как warning и продолжаем
                            log::warn!("STT temporary error ({}): {} - continuing processing",
                                error_type, error_msg);

                            // Увеличиваем счетчик временных ошибок
                            // Если их слишком много подряд - останавливаемся
                            // (пока просто пропускаем чанк)
                        }
                    }
                }
            }
            log::info!("Audio chunk processor finished, total chunks: {}", chunk_count);
        });

        self.audio_capture
            .write()
            .await
            .start_capture(on_chunk)
            .await
            .map_err(|e| {
                let status_arc = self.status.clone();
                tokio::spawn(async move { *status_arc.write().await = RecordingStatus::Idle; });
                anyhow::anyhow!("Failed to start audio capture: {}", e)
            })?;

        // Только после успешного запуска audio capture устанавливаем статус Recording
        *self.status.write().await = RecordingStatus::Recording;

        log::info!("Recording started");
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
        self.audio_capture
            .write()
            .await
            .stop_capture()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to stop audio capture: {}", e))?;

        // Проверяем нужно ли держать соединение открытым (keep-alive режим)
        let config = self.config.read().await.clone();
        let should_keep_alive = {
            let provider_opt = self.stt_provider.read().await;
            if let Some(provider) = provider_opt.as_ref() {
                provider.supports_keep_alive() && config.keep_connection_alive
            } else {
                false
            }
        };

        if should_keep_alive {
            // Ставим на паузу вместо полной остановки (keep-alive режим)
            log::info!("Pausing STT stream (keep-alive mode)");

            if let Some(provider) = self.stt_provider.write().await.as_mut() {
                provider
                    .pause_stream()
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to pause STT stream: {}", e))?;
            }

            // Запускаем таймер на 30 минут для автоматического закрытия соединения
            let stt_provider = self.stt_provider.clone();
            let status_arc = self.status.clone();
            let inactivity_timer = tokio::spawn(async move {
                log::info!("Inactivity timer started (30 minutes)");
                tokio::time::sleep(tokio::time::Duration::from_secs(30 * 60)).await;

                // Проверяем что статус все еще Idle (не началась новая запись)
                let current_status = *status_arc.read().await;
                if current_status == RecordingStatus::Idle {
                    log::info!("Inactivity timeout reached (30 min) - closing persistent connection for memory cleanup");

                    if let Some(provider) = stt_provider.write().await.as_mut() {
                        let _ = provider.stop_stream().await;
                    }
                    *stt_provider.write().await = None;

                    log::info!("Persistent connection closed, memory freed");
                } else {
                    log::debug!("Inactivity timer cancelled - recording restarted before timeout");
                }
            });

            *self.inactivity_timer_task.write().await = Some(inactivity_timer);
            *self.status.write().await = RecordingStatus::Idle;

            log::info!("Recording paused, connection kept alive (will auto-close in 30 min)");
            Ok("Recording paused, connection kept alive".to_string())
        } else {
            // Обычная остановка для провайдеров без keep-alive
            log::info!("Stopping STT stream completely");

            if let Some(provider) = self.stt_provider.write().await.as_mut() {
                provider
                    .stop_stream()
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to stop STT stream: {}", e))?;
            }

            *self.status.write().await = RecordingStatus::Idle;

            log::info!("Recording stopped");
            Ok("Transcription completed".to_string())
        }
    }

    /// Get current recording status
    pub async fn get_status(&self) -> RecordingStatus {
        *self.status.read().await
    }

    /// Update STT configuration
    pub async fn update_config(&self, config: SttConfig) -> Result<()> {
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
}

// Ensure TranscriptionService is thread-safe
unsafe impl Send for TranscriptionService {}
unsafe impl Sync for TranscriptionService {}
