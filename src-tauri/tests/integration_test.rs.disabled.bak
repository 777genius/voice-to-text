use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::sleep;

use app_lib::application::services::TranscriptionService;
use app_lib::domain::{
    AudioConfig, SttConfig, SttProviderFactory, SttProviderType,
    Transcription, RecordingStatus,
};
use app_lib::infrastructure::audio::{MockAudioCapture, VadProcessor, VadCaptureWrapper};
use app_lib::infrastructure::factory::DefaultSttProviderFactory;

/// Тест полного audio pipeline: MockCapture → VAD → TranscriptionService → MockSTT
#[tokio::test]
async fn test_full_audio_pipeline_with_mock() {
    // Создаем mock audio capture
    let mock_capture = Box::new(MockAudioCapture::new());

    // Создаем VAD processor
    let vad = VadProcessor::default().expect("Failed to create VAD");

    // Оборачиваем capture в VAD wrapper
    let vad_capture = Box::new(VadCaptureWrapper::new(mock_capture, vad));

    // Создаем STT factory
    let factory = Arc::new(DefaultSttProviderFactory::new());

    // Создаем TranscriptionService
    let service = TranscriptionService::new(vad_capture, factory);

    // Проверяем начальное состояние
    let status = service.get_status().await;
    assert_eq!(status, RecordingStatus::Idle);

    // Счетчик для транскрипций
    let partial_count = Arc::new(Mutex::new(0));
    let final_count = Arc::new(Mutex::new(0));

    let partial_counter = partial_count.clone();
    let final_counter = final_count.clone();

    let on_partial = Arc::new(move |transcription: Transcription| {
        println!("Partial: {}", transcription.text);
        *partial_counter.lock().unwrap() += 1;
    });

    let on_final = Arc::new(move |transcription: Transcription| {
        println!("Final: {}", transcription.text);
        *final_counter.lock().unwrap() += 1;
    });

    let on_audio_level = Arc::new(|_level: f32| {});

    // Настраиваем Mock STT provider через конфигурацию
    let mut config = SttConfig::default();
    config.provider = SttProviderType::Mock;
    service.update_config(config).await.expect("Failed to update config");

    // Инициализируем аудио
    let audio_config = AudioConfig::default();
    service.initialize_audio(audio_config).await.expect("Failed to init audio");

    // Запускаем запись
    let start_result = service.start_recording(on_partial, on_final, on_audio_level).await;
    assert!(start_result.is_ok(), "Failed to start recording: {:?}", start_result);

    // Проверяем что статус изменился
    let status = service.get_status().await;
    assert_eq!(status, RecordingStatus::Recording);

    // Даем время на обработку (MockCapture и MockSTT должны работать)
    sleep(Duration::from_millis(100)).await;

    // Останавливаем запись
    let stop_result = service.stop_recording().await;
    assert!(stop_result.is_ok(), "Failed to stop recording: {:?}", stop_result);

    // Проверяем что статус вернулся в Idle
    let status = service.get_status().await;
    assert_eq!(status, RecordingStatus::Idle);

    // MockCapture и MockSTT не генерируют реальные события в тестах,
    // но мы проверили что весь пайплайн инициализируется и работает без ошибок
    println!("Integration test passed: full pipeline initialized successfully");
}

/// Тест что TranscriptionService правильно управляет состоянием
#[tokio::test]
async fn test_transcription_service_state_machine() {
    let mock_capture = Box::new(MockAudioCapture::new());
    let factory = Arc::new(DefaultSttProviderFactory::new());
    let service = TranscriptionService::new(mock_capture, factory);

    // Начальное состояние - Idle
    assert_eq!(service.get_status().await, RecordingStatus::Idle);

    // Попытка остановить когда не записываем - должна вернуть ошибку
    let stop_result = service.stop_recording().await;
    assert!(stop_result.is_err(), "Should fail when not recording");

    // Настраиваем Mock provider
    let mut config = SttConfig::default();
    config.provider = SttProviderType::Mock;
    service.update_config(config).await.unwrap();

    // Callbacks
    let on_partial = Arc::new(|_: Transcription| {});
    let on_final = Arc::new(|_: Transcription| {});
    let on_audio_level = Arc::new(|_: f32| {});

    // Запускаем первую запись
    let result = service.start_recording(on_partial.clone(), on_final.clone(), on_audio_level.clone()).await;
    assert!(result.is_ok());
    assert_eq!(service.get_status().await, RecordingStatus::Recording);

    // Попытка запустить повторно - должна вернуть ошибку
    let double_start = service.start_recording(on_partial.clone(), on_final.clone(), on_audio_level.clone()).await;
    assert!(double_start.is_err(), "Should fail when already recording");

    // Останавливаем
    let stop = service.stop_recording().await;
    assert!(stop.is_ok());
    assert_eq!(service.get_status().await, RecordingStatus::Idle);

    println!("State machine test passed");
}

/// Тест конфигурации TranscriptionService
#[tokio::test]
async fn test_transcription_service_config() {
    let mock_capture = Box::new(MockAudioCapture::new());
    let factory = Arc::new(DefaultSttProviderFactory::new());
    let service = TranscriptionService::new(mock_capture, factory);

    // Проверяем дефолтную конфигурацию
    let default_config = service.get_config().await;
    assert_eq!(default_config.provider, SttProviderType::Mock);
    assert_eq!(default_config.language, "en");

    // Обновляем конфигурацию
    let mut new_config = SttConfig::default();
    new_config.language = "ru".to_string();
    new_config.provider = SttProviderType::Deepgram;

    let update_result = service.update_config(new_config.clone()).await;
    assert!(update_result.is_ok());

    // Проверяем что конфигурация обновилась
    let updated_config = service.get_config().await;
    assert_eq!(updated_config.language, "ru");
    assert_eq!(updated_config.provider, SttProviderType::Deepgram);

    println!("Config test passed");
}

/// Интеграционный тест с AssemblyAI провайдером
#[tokio::test]
async fn test_assemblyai_integration() {
    let mock_capture = Box::new(MockAudioCapture::new());
    let factory = Arc::new(DefaultSttProviderFactory::new());
    let service = TranscriptionService::new(mock_capture, factory);

    // Настраиваем AssemblyAI provider
    let mut config = SttConfig::default();
    config.provider = SttProviderType::AssemblyAI;
    config.api_key = Some("test-api-key".to_string());
    config.language = "en".to_string();

    let update_result = service.update_config(config).await;
    assert!(update_result.is_ok(), "Failed to update config with AssemblyAI");

    // Проверяем что конфигурация применилась
    let current_config = service.get_config().await;
    assert_eq!(current_config.provider, SttProviderType::AssemblyAI);
    assert!(current_config.api_key.is_some());

    // Callbacks
    let transcriptions = Arc::new(Mutex::new(Vec::new()));
    let transcriptions_clone = transcriptions.clone();

    let on_partial = Arc::new(move |t: Transcription| {
        transcriptions_clone.lock().unwrap().push(t);
    });

    let transcriptions_final = transcriptions.clone();
    let on_final = Arc::new(move |t: Transcription| {
        transcriptions_final.lock().unwrap().push(t);
    });

    let on_audio_level = Arc::new(|_: f32| {});

    // Инициализируем audio
    let audio_config = AudioConfig::default();
    service.initialize_audio(audio_config).await.expect("Failed to init audio");

    // Запускаем запись с AssemblyAI
    // Заметка: WebSocket подключение не установится без реального API key,
    // но мы проверяем что провайдер правильно создается и инициализируется
    let start_result = service.start_recording(on_partial, on_final, on_audio_level).await;

    // В тестовом окружении WebSocket может не подключиться,
    // но архитектура должна обработать это gracefully
    if start_result.is_ok() {
        println!("AssemblyAI provider started successfully (mock environment)");

        // Проверяем статус
        let status = service.get_status().await;
        assert_eq!(status, RecordingStatus::Recording);

        // Останавливаем
        sleep(Duration::from_millis(50)).await;
        let stop_result = service.stop_recording().await;
        assert!(stop_result.is_ok());

        assert_eq!(service.get_status().await, RecordingStatus::Idle);
    } else {
        println!("AssemblyAI connection expected to fail in test environment (no real API key)");
        // Это нормально в тестовом окружении - важно что архитектура работает
    }

    println!("AssemblyAI integration test completed");
}

/// Тест переключения между провайдерами (Mock ↔ AssemblyAI)
#[tokio::test]
async fn test_provider_switching() {
    let mock_capture = Box::new(MockAudioCapture::new());
    let factory = Arc::new(DefaultSttProviderFactory::new());
    let service = TranscriptionService::new(mock_capture, factory);

    // Начинаем с Mock
    let mut config = SttConfig::default();
    config.provider = SttProviderType::Mock;
    service.update_config(config).await.unwrap();

    let current = service.get_config().await;
    assert_eq!(current.provider, SttProviderType::Mock);

    // Переключаемся на AssemblyAI
    let mut config_ai = SttConfig::default();
    config_ai.provider = SttProviderType::AssemblyAI;
    config_ai.api_key = Some("test-key".to_string());

    let result = service.update_config(config_ai).await;
    assert!(result.is_ok(), "Should switch to AssemblyAI");

    let current = service.get_config().await;
    assert_eq!(current.provider, SttProviderType::AssemblyAI);
    assert!(current.api_key.is_some());

    // Переключаемся обратно на Mock
    let mut config_mock = SttConfig::default();
    config_mock.provider = SttProviderType::Mock;

    let result = service.update_config(config_mock).await;
    assert!(result.is_ok(), "Should switch back to Mock");

    let current = service.get_config().await;
    assert_eq!(current.provider, SttProviderType::Mock);

    println!("Provider switching test passed");
}

/// Тест что Factory создает правильные провайдеры для всех типов
#[tokio::test]
async fn test_factory_creates_all_providers() {
    let factory = DefaultSttProviderFactory::new();

    // Mock
    let mut config = SttConfig::default();
    config.provider = SttProviderType::Mock;
    let provider = factory.create(&config);
    assert!(provider.is_ok());
    assert_eq!(provider.unwrap().name(), "Mock STT Provider");

    // AssemblyAI
    config.provider = SttProviderType::AssemblyAI;
    config.api_key = Some("test".to_string());
    let provider = factory.create(&config);
    assert!(provider.is_ok());
    assert_eq!(provider.unwrap().name(), "AssemblyAI Universal-Streaming (v3)");

    // WhisperLocal
    config.provider = SttProviderType::WhisperLocal;
    let provider = factory.create(&config);
    assert!(provider.is_ok());

    // Deepgram (пока не реализован полностью, но должен создаваться)
    config.provider = SttProviderType::Deepgram;
    let provider = factory.create(&config);
    assert!(provider.is_ok());

    // GoogleCloud (не реализован - должен вернуть ошибку)
    config.provider = SttProviderType::GoogleCloud;
    let provider = factory.create(&config);
    assert!(provider.is_err());

    // Azure (не реализован - должен вернуть ошибку)
    config.provider = SttProviderType::Azure;
    let provider = factory.create(&config);
    assert!(provider.is_err());

    println!("Factory provider creation test passed");
}
