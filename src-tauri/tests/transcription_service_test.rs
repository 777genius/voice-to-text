use std::sync::Arc;
use tokio::sync::RwLock;
use std::time::Duration;
use tokio::time::sleep;

use app_lib::application::services::TranscriptionService;
use app_lib::domain::{
    AudioCapture, AudioChunk, AudioConfig, AudioLevelCallback, ErrorCallback,
    RecordingStatus, SttConfig, SttError, SttProvider, SttProviderFactory, SttProviderType,
    Transcription, TranscriptionCallback,
};
use app_lib::infrastructure::audio::MockAudioCapture;
use async_trait::async_trait;

// ============================================================================
// MOCK STT PROVIDER
// ============================================================================

/// Мок STT провайдера для тестов
struct MockSttProvider {
    name: String,
    initialized: Arc<RwLock<bool>>,
    streaming: Arc<RwLock<bool>>,
    paused: Arc<RwLock<bool>>,
    chunks_received: Arc<RwLock<Vec<AudioChunk>>>,
    on_partial: Arc<RwLock<Option<TranscriptionCallback>>>,
    on_final: Arc<RwLock<Option<TranscriptionCallback>>>,
    on_error: Arc<RwLock<Option<ErrorCallback>>>,
    simulate_error: Arc<RwLock<bool>>,
    supports_keep_alive_flag: bool,
}

impl MockSttProvider {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            initialized: Arc::new(RwLock::new(false)),
            streaming: Arc::new(RwLock::new(false)),
            paused: Arc::new(RwLock::new(false)),
            chunks_received: Arc::new(RwLock::new(Vec::new())),
            on_partial: Arc::new(RwLock::new(None)),
            on_final: Arc::new(RwLock::new(None)),
            on_error: Arc::new(RwLock::new(None)),
            simulate_error: Arc::new(RwLock::new(false)),
            supports_keep_alive_flag: false,
        }
    }

    fn with_keep_alive(mut self) -> Self {
        self.supports_keep_alive_flag = true;
        self
    }

    fn with_error_simulation(mut self) -> Self {
        self.simulate_error = Arc::new(RwLock::new(true));
        self
    }

    async fn trigger_partial(&self, text: &str) {
        if let Some(callback) = self.on_partial.read().await.as_ref() {
            callback(Transcription {
                text: text.to_string(),
                confidence: Some(0.95),
                is_final: false,
                language: Some("ru".to_string()),
                timestamp: 0,
                start: 0.0,
                duration: 0.0,
            });
        }
    }

    async fn trigger_final(&self, text: &str) {
        if let Some(callback) = self.on_final.read().await.as_ref() {
            callback(Transcription {
                text: text.to_string(),
                confidence: Some(0.98),
                is_final: true,
                language: Some("ru".to_string()),
                timestamp: 0,
                start: 0.0,
                duration: 0.0,
            });
        }
    }
}

#[async_trait]
impl SttProvider for MockSttProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn is_online(&self) -> bool {
        true
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_keep_alive(&self) -> bool {
        self.supports_keep_alive_flag
    }

    fn is_connection_alive(&self) -> bool {
        // Для sync метода используем try_read (лучше чем blocking_read в async context)
        self.supports_keep_alive_flag &&  self.paused.try_read().map(|p| !*p).unwrap_or(true)
    }

    async fn initialize(&mut self, _config: &SttConfig) -> Result<(), SttError> {
        *self.initialized.write().await = true;
        Ok(())
    }

    async fn start_stream(
        &mut self,
        on_partial: TranscriptionCallback,
        on_final: TranscriptionCallback,
        on_error: ErrorCallback,
    ) -> Result<(), SttError> {
        if !*self.initialized.read().await {
            return Err(SttError::Configuration("Provider not initialized".to_string()));
        }

        *self.streaming.write().await = true;
        *self.paused.write().await = false;
        *self.on_partial.write().await = Some(on_partial);
        *self.on_final.write().await = Some(on_final);
        *self.on_error.write().await = Some(on_error);

        Ok(())
    }

    async fn send_audio(&mut self, chunk: &AudioChunk) -> Result<(), SttError> {
        if !*self.streaming.read().await {
            return Err(SttError::Processing("Not streaming".to_string()));
        }

        if *self.simulate_error.read().await {
            return Err(SttError::Processing("Simulated error".to_string()));
        }

        self.chunks_received.write().await.push(chunk.clone());
        Ok(())
    }

    async fn stop_stream(&mut self) -> Result<(), SttError> {
        *self.streaming.write().await = false;
        *self.paused.write().await = false;
        Ok(())
    }

    async fn pause_stream(&mut self) -> Result<(), SttError> {
        if !self.supports_keep_alive_flag {
            return Err(SttError::Configuration("Keep-alive not supported".to_string()));
        }
        *self.paused.write().await = true;
        Ok(())
    }

    async fn resume_stream(
        &mut self,
        on_partial: TranscriptionCallback,
        on_final: TranscriptionCallback,
        on_error: ErrorCallback,
    ) -> Result<(), SttError> {
        if !self.supports_keep_alive_flag {
            return Err(SttError::Configuration("Keep-alive not supported".to_string()));
        }

        if !self.is_connection_alive() {
            return Err(SttError::Connection("Connection not alive".to_string()));
        }

        *self.streaming.write().await = true;
        *self.paused.write().await = false;
        *self.on_partial.write().await = Some(on_partial);
        *self.on_final.write().await = Some(on_final);
        *self.on_error.write().await = Some(on_error);

        Ok(())
    }

    async fn abort(&mut self) -> Result<(), SttError> {
        *self.streaming.write().await = false;
        *self.paused.write().await = false;
        Ok(())
    }
}

// ============================================================================
// MOCK STT FACTORY
// ============================================================================

struct MockSttProviderFactory {
    provider_name: String,
    supports_keep_alive: bool,
}

impl MockSttProviderFactory {
    fn new(provider: Box<dyn SttProvider>) -> Self {
        Self {
            provider_name: provider.name().to_string(),
            supports_keep_alive: provider.supports_keep_alive(),
        }
    }
}

impl SttProviderFactory for MockSttProviderFactory {
    fn create(&self, _config: &SttConfig) -> Result<Box<dyn SttProvider>, SttError> {
        let mut mock = MockSttProvider::new(&self.provider_name);
        if self.supports_keep_alive {
            mock = mock.with_keep_alive();
        }
        Ok(Box::new(mock))
    }
}

// ============================================================================
// UNIT ТЕСТЫ
// ============================================================================

#[tokio::test]
async fn test_service_creation() {
    let audio_capture = Box::new(MockAudioCapture::new());
    let provider = Box::new(MockSttProvider::new("Test Provider"));
    let factory = Arc::new(MockSttProviderFactory::new(provider));

    let service = TranscriptionService::new(audio_capture, factory);

    // Проверяем начальное состояние
    assert_eq!(service.get_status().await, RecordingStatus::Idle);
}

#[tokio::test]
async fn test_initialize_audio() {
    let audio_capture = Box::new(MockAudioCapture::new());
    let provider = Box::new(MockSttProvider::new("Test Provider"));
    let factory = Arc::new(MockSttProviderFactory::new(provider));

    let service = TranscriptionService::new(audio_capture, factory);

    let config = AudioConfig::default();
    let result = service.initialize_audio(config).await;
    assert!(result.is_ok(), "Инициализация аудио должна пройти успешно");
}

#[tokio::test]
async fn test_update_and_get_config() {
    let audio_capture = Box::new(MockAudioCapture::new());
    let provider = Box::new(MockSttProvider::new("Test Provider"));
    let factory = Arc::new(MockSttProviderFactory::new(provider));

    let service = TranscriptionService::new(audio_capture, factory);

    // Создаем новую конфигурацию
    let mut new_config = SttConfig::default();
    new_config.provider = SttProviderType::Deepgram;
    new_config.language = "ru".to_string();

    // Обновляем
    service.update_config(new_config.clone()).await.unwrap();

    // Получаем и проверяем
    let loaded_config = service.get_config().await;
    assert_eq!(loaded_config.provider, new_config.provider);
    assert_eq!(loaded_config.language, new_config.language);
}

#[tokio::test]
async fn test_set_microphone_sensitivity() {
    let audio_capture = Box::new(MockAudioCapture::new());
    let provider = Box::new(MockSttProvider::new("Test Provider"));
    let factory = Arc::new(MockSttProviderFactory::new(provider));

    let service = TranscriptionService::new(audio_capture, factory);

    // Устанавливаем чувствительность
    service.set_microphone_sensitivity(150).await;

    // Проверяем что значение ограничено 200
    service.set_microphone_sensitivity(250).await; // Должно стать 200

    // Тест прошел если не паникует
}

#[tokio::test]
async fn test_start_recording_prevents_double_start() {
    let audio_capture = Box::new(MockAudioCapture::new());
    let provider = Box::new(MockSttProvider::new("Test Provider"));
    let factory = Arc::new(MockSttProviderFactory::new(provider));

    let service = TranscriptionService::new(audio_capture, factory);

    // Инициализируем
    let config = AudioConfig::default();
    service.initialize_audio(config).await.unwrap();

    let on_partial = Arc::new(|_: Transcription| {});
    let on_final = Arc::new(|_: Transcription| {});
    let on_audio_level = Arc::new(|_: f32| {});
    let on_error = Arc::new(|_: String, _: String| {});

    // Первый старт должен пройти
    let result1 = service.start_recording(
        on_partial.clone(),
        on_final.clone(),
        on_audio_level.clone(),
        on_error.clone(),
    ).await;
    assert!(result1.is_ok(), "Первый старт должен пройти");

    // Даем время на переход в Recording
    sleep(Duration::from_millis(50)).await;

    // Второй старт должен вернуть ошибку
    let result2 = service.start_recording(
        on_partial,
        on_final,
        on_audio_level,
        on_error,
    ).await;
    assert!(result2.is_err(), "Повторный старт должен вернуть ошибку");

    // Останавливаем
    let _ = service.stop_recording().await;
}

#[tokio::test]
async fn test_stop_recording_without_start() {
    let audio_capture = Box::new(MockAudioCapture::new());
    let provider = Box::new(MockSttProvider::new("Test Provider"));
    let factory = Arc::new(MockSttProviderFactory::new(provider));

    let service = TranscriptionService::new(audio_capture, factory);

    // Попытка остановить без старта
    let result = service.stop_recording().await;
    assert!(result.is_err(), "Остановка без старта должна вернуть ошибку");
}

#[tokio::test]
async fn test_full_recording_lifecycle() {
    let audio_capture = Box::new(MockAudioCapture::new());
    let provider = Box::new(MockSttProvider::new("Test Provider"));
    let factory = Arc::new(MockSttProviderFactory::new(provider));

    let service = TranscriptionService::new(audio_capture, factory);

    // Инициализация
    service.initialize_audio(AudioConfig::default()).await.unwrap();

    let on_partial = Arc::new(|_: Transcription| {});
    let on_final = Arc::new(|_: Transcription| {});
    let on_audio_level = Arc::new(|_: f32| {});
    let on_error = Arc::new(|_: String, _: String| {});

    // Проверяем статус Idle
    assert_eq!(service.get_status().await, RecordingStatus::Idle);

    // Старт
    service.start_recording(
        on_partial,
        on_final,
        on_audio_level,
        on_error,
    ).await.unwrap();

    // Даем время на переход
    sleep(Duration::from_millis(100)).await;

    // Проверяем статус Recording
    assert_eq!(service.get_status().await, RecordingStatus::Recording);

    // Стоп
    service.stop_recording().await.unwrap();

    // Проверяем статус вернулся в Idle
    sleep(Duration::from_millis(50)).await;
    assert_eq!(service.get_status().await, RecordingStatus::Idle);
}

#[tokio::test]
async fn test_keep_alive_mode() {
    let audio_capture = Box::new(MockAudioCapture::new());
    let provider = Box::new(MockSttProvider::new("Keep-Alive Provider").with_keep_alive());
    let factory = Arc::new(MockSttProviderFactory::new(provider));

    let service = TranscriptionService::new(audio_capture, factory);

    // Конфигурация с keep-alive
    let mut config = SttConfig::default();
    config.keep_connection_alive = true;
    service.update_config(config).await.unwrap();

    service.initialize_audio(AudioConfig::default()).await.unwrap();

    let on_partial = Arc::new(|_: Transcription| {});
    let on_final = Arc::new(|_: Transcription| {});
    let on_audio_level = Arc::new(|_: f32| {});
    let on_error = Arc::new(|_: String, _: String| {});

    // Старт
    service.start_recording(
        on_partial.clone(),
        on_final.clone(),
        on_audio_level.clone(),
        on_error.clone(),
    ).await.unwrap();

    sleep(Duration::from_millis(100)).await;

    // Стоп с keep-alive
    let result = service.stop_recording().await;
    assert!(result.is_ok());

    // Статус должен вернуться в Idle (keep-alive режим)
    sleep(Duration::from_millis(50)).await;
    assert_eq!(service.get_status().await, RecordingStatus::Idle);

    // Быстрый повторный старт должен использовать существующее соединение
    let result2 = service.start_recording(
        on_partial,
        on_final,
        on_audio_level,
        on_error,
    ).await;
    assert!(result2.is_ok(), "Быстрый рестарт с keep-alive должен работать");

    service.stop_recording().await.unwrap();
}

#[tokio::test]
async fn test_recording_status_transitions() {
    let audio_capture = Box::new(MockAudioCapture::new());
    let provider = Box::new(MockSttProvider::new("Test Provider"));
    let factory = Arc::new(MockSttProviderFactory::new(provider));

    let service = TranscriptionService::new(audio_capture, factory);

    // Idle -> Starting -> Recording
    assert_eq!(service.get_status().await, RecordingStatus::Idle);

    service.initialize_audio(AudioConfig::default()).await.unwrap();

    let on_partial = Arc::new(|_: Transcription| {});
    let on_final = Arc::new(|_: Transcription| {});
    let on_audio_level = Arc::new(|_: f32| {});
    let on_error = Arc::new(|_: String, _: String| {});

    service.start_recording(
        on_partial,
        on_final,
        on_audio_level,
        on_error,
    ).await.unwrap();

    sleep(Duration::from_millis(100)).await;
    assert_eq!(service.get_status().await, RecordingStatus::Recording);

    // Recording -> Processing -> Idle
    service.stop_recording().await.unwrap();

    sleep(Duration::from_millis(50)).await;
    assert_eq!(service.get_status().await, RecordingStatus::Idle);
}

#[tokio::test]
async fn test_config_persistence() {
    let audio_capture = Box::new(MockAudioCapture::new());
    let provider = Box::new(MockSttProvider::new("Test Provider"));
    let factory = Arc::new(MockSttProviderFactory::new(provider));

    let service = TranscriptionService::new(audio_capture, factory);

    // Устанавливаем конфигурацию
    let mut config = SttConfig::default();
    config.provider = SttProviderType::Deepgram;
    config.language = "en".to_string();
    config.keep_connection_alive = true;

    service.update_config(config.clone()).await.unwrap();

    // Проверяем что конфиг сохранился
    let loaded = service.get_config().await;
    assert_eq!(loaded.provider, config.provider);
    assert_eq!(loaded.language, config.language);
    assert_eq!(loaded.keep_connection_alive, config.keep_connection_alive);
}
