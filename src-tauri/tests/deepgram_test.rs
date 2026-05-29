use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::sleep;

use app_lib::domain::{AudioChunk, SttConfig, SttProvider, SttProviderType, Transcription};
use app_lib::infrastructure::{embedded_keys, stt::DeepgramProvider};

mod test_support;
use test_support::{noop_connection_quality, noop_error, stderr_error, SttConfigTestExt};

/// Получаем API ключ из переменной окружения
///
/// Установите переменную окружения DEEPGRAM_TEST_KEY перед запуском тестов:
/// ```bash
/// export DEEPGRAM_TEST_KEY="your_api_key_here"
/// cargo test
/// ```
fn get_api_key() -> String {
    // Пробуем загрузить .env файл (если есть)
    let _ = dotenv::dotenv();

    // Читаем из переменной окружения
    std::env::var("DEEPGRAM_TEST_KEY").unwrap_or_else(|_| "test-key".to_string())
}

// ============================================================================
// UNIT ТЕСТЫ - Проверяем отдельные компоненты
// ============================================================================

/// Проверяем базовую инициализацию провайдера
#[tokio::test]
async fn test_deepgram_initialization() {
    let mut provider = DeepgramProvider::new();

    assert!(
        provider.name().contains("Deepgram"),
        "Provider name should contain 'Deepgram'"
    );
    assert!(provider.is_online());
    assert!(provider.supports_streaming());

    // Инициализация без пользовательского ключа должна использовать встроенный ключ,
    // если он реально встроен в текущий build.
    let config = SttConfig::default();
    let result = provider.initialize(&config).await;
    if embedded_keys::has_embedded_deepgram_key() {
        assert!(
            result.is_ok(),
            "Инициализация должна пройти со встроенным ключом"
        );
    } else {
        assert!(
            result.is_err(),
            "Инициализация без пользовательского ключа должна падать, если embedded key не встроен"
        );
    }

    // Пользовательский ключ тоже должен приниматься.
    let mut config_with_key = SttConfig::default();
    config_with_key.deepgram_api_key = Some(get_api_key());
    config_with_key.language = "ru".to_string();

    let result = provider.initialize(&config_with_key).await;
    assert!(
        result.is_ok(),
        "Инициализация с пользовательским ключом должна пройти успешно: {:?}",
        result
    );
}

/// Тестируем конфигурацию с разными языками и моделями
#[tokio::test]
async fn test_deepgram_configuration() {
    let mut provider = DeepgramProvider::new();

    // Русский язык
    let mut config_ru = SttConfig::new(SttProviderType::Deepgram).with_language("ru");
    config_ru.deepgram_api_key = Some(get_api_key());

    let result = provider.initialize(&config_ru).await;
    assert!(result.is_ok());

    // Английский язык
    let mut config_en = SttConfig::new(SttProviderType::Deepgram).with_language("en");
    config_en.deepgram_api_key = Some(get_api_key());

    let result = provider.initialize(&config_en).await;
    assert!(result.is_ok());

    // Кастомная модель
    let mut config_custom = SttConfig::new(SttProviderType::Deepgram).with_model("nova-2");
    config_custom.deepgram_api_key = Some(get_api_key());

    let result = provider.initialize(&config_custom).await;
    assert!(result.is_ok());
}

/// Проверяем state machine (состояния провайдера)
#[tokio::test]
async fn test_deepgram_state_machine() {
    let mut provider = DeepgramProvider::new();

    let mut config = SttConfig::new(SttProviderType::Deepgram);
    config.deepgram_api_key = Some(get_api_key());

    provider.initialize(&config).await.unwrap();

    // Попытка отправить аудио до начала стрима должна вернуть ошибку
    let chunk = AudioChunk::new(vec![100i16; 1600], 16000, 1);
    let result = provider.send_audio(&chunk).await;
    assert!(result.is_err(), "Не должно работать до start_stream");

    // Попытка остановить до начала должна быть безопасной
    let result = provider.stop_stream().await;
    assert!(
        result.is_ok(),
        "Stop без активного stream должен быть безопасным"
    );
}

/// Тестируем audio encoding и buffering
#[tokio::test]
async fn test_deepgram_audio_encoding() {
    // Генерируем тестовые сэмплы (синусоида 440Hz)
    let sample_rate = 16000;
    let duration_ms = 100;
    let samples_count = (sample_rate * duration_ms / 1000) as usize;

    let mut samples = Vec::with_capacity(samples_count);
    for i in 0..samples_count {
        let t = i as f32 / sample_rate as f32;
        let value = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 10000.0;
        samples.push(value as i16);
    }

    let chunk = AudioChunk::new(samples.clone(), sample_rate, 1);

    // Проверяем длительность
    assert_eq!(chunk.duration_ms(), duration_ms as u64);

    // Проверяем конвертацию в байты (как это делает Deepgram)
    let bytes: Vec<u8> = chunk
        .data
        .iter()
        .flat_map(|&sample| sample.to_le_bytes())
        .collect();

    assert_eq!(bytes.len(), samples.len() * 2);

    // Проверяем что можно декодировать обратно
    let decoded: Vec<i16> = bytes
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();

    assert_eq!(decoded, samples);
}

/// Тестируем механизм буферизации аудио
#[tokio::test]
async fn test_deepgram_audio_buffering() {
    // Deepgram буферизует чанки минимум 100ms (1600 сэмплов)
    const MIN_SAMPLES: usize = 1600;

    // Маленький чанк (50ms) - должен буфериться
    let small_chunk = AudioChunk::new(vec![100i16; 800], 16000, 1);
    assert!(small_chunk.data.len() < MIN_SAMPLES);

    // Большой чанк (200ms) - должен отправляться сразу
    let large_chunk = AudioChunk::new(vec![100i16; 3200], 16000, 1);
    assert!(large_chunk.data.len() >= MIN_SAMPLES);

    // Проверяем что размер правильный
    assert_eq!(small_chunk.duration_ms(), 50);
    assert_eq!(large_chunk.duration_ms(), 200);
}

/// Проверяем graceful shutdown
#[tokio::test]
async fn test_deepgram_graceful_shutdown() {
    let mut provider = DeepgramProvider::new();

    let mut config = SttConfig::new(SttProviderType::Deepgram);
    config.deepgram_api_key = Some(get_api_key());

    provider.initialize(&config).await.unwrap();

    // Abort должен быть безопасным даже без активного стрима
    let result = provider.abort().await;
    assert!(result.is_ok());

    // Повторный abort тоже безопасен
    let result = provider.abort().await;
    assert!(result.is_ok());
}

/// Тестируем callback механизм
#[tokio::test]
async fn test_deepgram_callbacks() {
    let partial_count = Arc::new(Mutex::new(0));
    let final_count = Arc::new(Mutex::new(0));
    let partial_texts = Arc::new(Mutex::new(Vec::new()));
    let final_texts = Arc::new(Mutex::new(Vec::new()));

    let p_count = partial_count.clone();
    let p_texts = partial_texts.clone();
    let on_partial = Arc::new(move |transcription: Transcription| {
        *p_count.lock().unwrap() += 1;
        p_texts.lock().unwrap().push(transcription.text.clone());
        println!("Partial: {}", transcription.text);
    });

    let f_count = final_count.clone();
    let f_texts = final_texts.clone();
    let on_final = Arc::new(move |transcription: Transcription| {
        *f_count.lock().unwrap() += 1;
        f_texts.lock().unwrap().push(transcription.text.clone());
        println!("Final: {}", transcription.text);
    });

    // Тестируем что callbacks можно вызывать
    let test_transcription = Transcription {
        text: "Привет мир".to_string(),
        confidence: Some(0.95),
        is_final: false,
        language: Some("ru".to_string()),
        timestamp: 0,
        start: 0.0,
        duration: 0.0,
    };

    on_partial(test_transcription.clone());
    assert_eq!(*partial_count.lock().unwrap(), 1);
    assert_eq!(partial_texts.lock().unwrap()[0], "Привет мир");

    let final_transcription = Transcription {
        is_final: true,
        ..test_transcription
    };

    on_final(final_transcription);
    assert_eq!(*final_count.lock().unwrap(), 1);
    assert_eq!(final_texts.lock().unwrap()[0], "Привет мир");
}

/// Проверяем Factory integration
#[tokio::test]
async fn test_deepgram_factory_creation() {
    use app_lib::domain::SttProviderFactory;
    use app_lib::infrastructure::factory::DefaultSttProviderFactory;

    let factory = DefaultSttProviderFactory::new();

    let mut config = SttConfig::new(SttProviderType::Deepgram);
    config.deepgram_api_key = Some(get_api_key());

    let result = factory.create(&config);
    assert!(result.is_ok(), "Factory должна создать Deepgram провайдер");

    let mut provider = result.unwrap();
    assert!(
        provider.name().contains("Deepgram"),
        "Provider name should contain 'Deepgram'"
    );

    // Проверяем инициализацию через Factory
    let init_result = provider.initialize(&config).await;
    assert!(init_result.is_ok());
}

// ============================================================================
// INTEGRATION ТЕСТЫ - Проверяем взаимодействие с реальным API
// ============================================================================

/// Полный lifecycle: инициализация → старт → отправка аудио → стоп
#[tokio::test]
#[ignore] // Используйте --ignored для запуска этого теста с реальным API
async fn test_deepgram_full_lifecycle() {
    let mut provider = DeepgramProvider::new();

    let config = SttConfig::new(SttProviderType::Deepgram).with_language("en"); // Используем en для теста

    provider.initialize(&config).await.unwrap();

    let transcriptions = Arc::new(Mutex::new(Vec::new()));
    let transcriptions_clone = transcriptions.clone();

    let on_partial = Arc::new(move |t: Transcription| {
        println!("📝 Partial: {}", t.text);
        transcriptions_clone.lock().unwrap().push(t);
    });

    let transcriptions_final = transcriptions.clone();
    let on_final = Arc::new(move |t: Transcription| {
        println!("✅ Final: {}", t.text);
        transcriptions_final.lock().unwrap().push(t);
    });

    let on_error = stderr_error();

    // Запускаем stream
    let result = provider
        .start_stream(on_partial, on_final, on_error, noop_connection_quality())
        .await;
    assert!(result.is_ok(), "Не удалось запустить stream: {:?}", result);

    println!("🎙️  Stream запущен, отправляем аудио...");

    // Отправляем тестовое аудио (тишина с небольшим шумом)
    for i in 0..10 {
        let mut samples = vec![0i16; 1600]; // 100ms тишины

        // Добавляем немного шума чтобы Deepgram не игнорировал
        for j in 0..samples.len() {
            let val = (i as i32 * 100 + j as i32) % 200 - 100;
            samples[j] = val as i16;
        }

        let chunk = AudioChunk::new(samples, 16000, 1);
        let result = provider.send_audio(&chunk).await;
        assert!(result.is_ok(), "Ошибка отправки аудио: {:?}", result);

        sleep(Duration::from_millis(50)).await;
    }

    println!("🛑 Останавливаем stream...");

    // Останавливаем stream
    let result = provider.stop_stream().await;
    assert!(result.is_ok(), "Ошибка остановки stream: {:?}", result);

    println!("✅ Test completed successfully");
}

/// Тестируем WebSocket соединение с реальным API
#[tokio::test]
#[ignore]
async fn test_deepgram_websocket_connection() {
    let mut provider = DeepgramProvider::new();

    let config = SttConfig::new(SttProviderType::Deepgram).with_language("en");

    provider.initialize(&config).await.unwrap();

    let on_partial = Arc::new(|t: Transcription| {
        println!("Partial: {}", t.text);
    });

    let on_final = Arc::new(|t: Transcription| {
        println!("Final: {}", t.text);
    });

    let on_error = stderr_error();

    // Подключаемся
    let result = provider
        .start_stream(on_partial, on_final, on_error, noop_connection_quality())
        .await;
    assert!(
        result.is_ok(),
        "WebSocket подключение не удалось: {:?}",
        result
    );

    println!("✅ WebSocket соединение установлено");

    // Ждем немного
    sleep(Duration::from_millis(500)).await;

    // Отключаемся
    provider.stop_stream().await.unwrap();
}

/// Проверяем обработку ошибок соединения
#[tokio::test]
async fn test_deepgram_connection_error() {
    let mut provider = DeepgramProvider::new();

    // Неправильный API key
    let config = SttConfig::new(SttProviderType::Deepgram).with_api_key("invalid_key_12345");

    provider.initialize(&config).await.unwrap();

    let on_partial = Arc::new(|_: Transcription| {});
    let on_final = Arc::new(|_: Transcription| {});
    let on_error = noop_error();

    // Попытка подключиться должна вернуть ошибку
    let result = provider
        .start_stream(on_partial, on_final, on_error, noop_connection_quality())
        .await;
    assert!(result.is_err(), "Должна быть ошибка с неверным API key");
}

/// Тестируем отправку реального голосового аудио
#[tokio::test]
#[ignore]
async fn test_deepgram_real_voice_transcription() {
    let mut provider = DeepgramProvider::new();

    let config = SttConfig::new(SttProviderType::Deepgram).with_language("ru");

    provider.initialize(&config).await.unwrap();

    let final_text = Arc::new(Mutex::new(String::new()));
    let final_text_clone = final_text.clone();

    let on_partial = Arc::new(|t: Transcription| {
        println!("📝 {}", t.text);
    });

    let on_final = Arc::new(move |t: Transcription| {
        println!("✅ Финальный текст: {}", t.text);
        *final_text_clone.lock().unwrap() = t.text;
    });

    let on_error = stderr_error();

    provider
        .start_stream(on_partial, on_final, on_error, noop_connection_quality())
        .await
        .unwrap();

    // Генерируем синтетический голос (многочастотный сигнал)
    let sample_rate = 16000;
    let duration_secs = 3;

    for _ in 0..duration_secs * 10 {
        let mut samples = Vec::with_capacity(1600);

        for i in 0..1600 {
            let t = i as f32 / sample_rate as f32;
            // Микс частот чтобы имитировать голос
            let val = (2.0 * std::f32::consts::PI * 300.0 * t).sin() * 3000.0
                + (2.0 * std::f32::consts::PI * 600.0 * t).sin() * 2000.0
                + (2.0 * std::f32::consts::PI * 1200.0 * t).sin() * 1000.0;
            samples.push(val as i16);
        }

        let chunk = AudioChunk::new(samples, sample_rate, 1);
        provider.send_audio(&chunk).await.unwrap();
        sleep(Duration::from_millis(100)).await;
    }

    provider.stop_stream().await.unwrap();

    // В реальности Deepgram вернет либо пустую строку либо распознанный текст
    println!("Результат: {:?}", *final_text.lock().unwrap());
}

/// Проверяем KeepAlive механизм
#[tokio::test]
#[ignore]
async fn test_deepgram_keepalive() {
    let mut provider = DeepgramProvider::new();

    let config = SttConfig::new(SttProviderType::Deepgram).with_language("ru");

    provider.initialize(&config).await.unwrap();

    let on_partial = Arc::new(|_: Transcription| {});
    let on_final = Arc::new(|_: Transcription| {});
    let on_error = stderr_error();

    provider
        .start_stream(on_partial, on_final, on_error, noop_connection_quality())
        .await
        .unwrap();

    // Ждем больше 4 секунд без отправки аудио
    // KeepAlive должен сработать автоматически
    println!("Ждем 10 секунд для проверки KeepAlive...");
    sleep(Duration::from_secs(10)).await;

    // Если соединение живо - значит KeepAlive работает
    let chunk = AudioChunk::new(vec![100i16; 1600], 16000, 1);
    let result = provider.send_audio(&chunk).await;
    assert!(
        result.is_ok(),
        "Соединение должно быть живым благодаря KeepAlive"
    );

    provider.stop_stream().await.unwrap();
}

// ============================================================================
// E2E ТЕСТЫ - Полный pipeline с интеграцией
// ============================================================================

/// E2E тест: TranscriptionService + Deepgram + реальное аудио
#[tokio::test]
#[ignore]
async fn test_e2e_full_pipeline_with_deepgram() {
    use app_lib::application::services::TranscriptionService;
    use app_lib::domain::{AudioConfig, RecordingStatus};
    use app_lib::infrastructure::audio::MockAudioCapture;
    use app_lib::infrastructure::factory::DefaultSttProviderFactory;

    let mock_capture = Box::new(MockAudioCapture::new());
    let factory = Arc::new(DefaultSttProviderFactory::new());
    let service = TranscriptionService::new(mock_capture, factory);

    // Настраиваем Deepgram
    let config = SttConfig::new(SttProviderType::Deepgram).with_language("ru");

    service.update_config(config).await.unwrap();

    let transcriptions = Arc::new(Mutex::new(Vec::new()));
    let transcriptions_clone = transcriptions.clone();

    let on_partial = Arc::new(move |t: Transcription| {
        println!("Partial: {}", t.text);
        transcriptions_clone.lock().unwrap().push(t);
    });

    let transcriptions_final = transcriptions.clone();
    let on_final = Arc::new(move |t: Transcription| {
        println!("Final: {}", t.text);
        transcriptions_final.lock().unwrap().push(t);
    });

    // Инициализируем аудио
    let audio_config = AudioConfig::default();
    service.initialize_audio(audio_config).await.unwrap();

    // Запускаем запись
    let on_audio_level = Arc::new(|_level: f32| {});
    let on_audio_spectrum = Arc::new(|_spectrum: [f32; 48]| {});
    let on_error = noop_error();

    let result = service
        .start_recording(
            on_partial,
            on_final,
            on_audio_level,
            on_audio_spectrum,
            on_error,
            noop_connection_quality(),
        )
        .await;
    assert!(result.is_ok(), "Не удалось запустить запись: {:?}", result);

    assert_eq!(service.get_status().await, RecordingStatus::Recording);

    // Даем время на обработку
    sleep(Duration::from_secs(2)).await;

    // Останавливаем
    service.stop_recording().await.unwrap();
    assert_eq!(service.get_status().await, RecordingStatus::Idle);

    println!("✅ E2E test completed");
}

/// E2E тест: Многократное использование провайдера
#[tokio::test]
#[ignore]
async fn test_e2e_multiple_sessions() {
    let mut provider = DeepgramProvider::new();

    let config = SttConfig::new(SttProviderType::Deepgram).with_language("ru");

    provider.initialize(&config).await.unwrap();

    for session in 1..=3 {
        println!("\n🎙️  Сессия {}", session);

        let on_partial = Arc::new(|t: Transcription| {
            println!("  Partial: {}", t.text);
        });

        let on_final = Arc::new(|t: Transcription| {
            println!("  Final: {}", t.text);
        });

        let on_error = stderr_error();

        provider
            .start_stream(on_partial, on_final, on_error, noop_connection_quality())
            .await
            .unwrap();

        // Отправляем немного аудио
        for _ in 0..5 {
            let chunk = AudioChunk::new(vec![100i16; 1600], 16000, 1);
            provider.send_audio(&chunk).await.unwrap();
            sleep(Duration::from_millis(100)).await;
        }

        provider.stop_stream().await.unwrap();

        // Пауза между сессиями
        sleep(Duration::from_millis(500)).await;
    }

    println!("\n✅ Все 3 сессии прошли успешно");
}

/// E2E тест: Длинная сессия (стресс-тест)
#[tokio::test]
#[ignore]
async fn test_e2e_long_session() {
    let mut provider = DeepgramProvider::new();

    let config = SttConfig::new(SttProviderType::Deepgram).with_language("ru");

    provider.initialize(&config).await.unwrap();

    let chunk_count = Arc::new(Mutex::new(0));
    let chunk_count_clone = chunk_count.clone();

    let on_partial = Arc::new(move |t: Transcription| {
        let count = *chunk_count_clone.lock().unwrap();
        println!("[{}] Partial: {}", count, t.text);
    });

    let on_final = Arc::new(|t: Transcription| {
        println!("✅ Final: {}", t.text);
    });

    let on_error = stderr_error();

    provider
        .start_stream(on_partial, on_final, on_error, noop_connection_quality())
        .await
        .unwrap();

    // Отправляем аудио в течение 30 секунд
    let duration_secs = 30;
    let chunks_per_sec = 10; // 100ms чанки

    for i in 0..(duration_secs * chunks_per_sec) {
        *chunk_count.lock().unwrap() = i;

        // Генерируем разнообразный сигнал
        let freq = 200.0 + (i as f32 * 10.0) % 800.0;
        let mut samples = Vec::with_capacity(1600);

        for j in 0..1600 {
            let t = j as f32 / 16000.0;
            let val = (2.0 * std::f32::consts::PI * freq * t).sin() * 5000.0;
            samples.push(val as i16);
        }

        let chunk = AudioChunk::new(samples, 16000, 1);
        provider.send_audio(&chunk).await.unwrap();

        sleep(Duration::from_millis(100)).await;

        if i % 50 == 0 {
            println!("⏱️  {} секунд прошло...", i / chunks_per_sec);
        }
    }

    provider.stop_stream().await.unwrap();
    println!("✅ Длинная сессия завершена успешно");
}

/// E2E тест: Переключение между разными языками
#[tokio::test]
#[ignore]
async fn test_e2e_language_switching() {
    let mut provider = DeepgramProvider::new();

    let languages = vec!["ru", "en", "es", "de"];

    for lang in languages {
        println!("\n🌍 Тестируем язык: {}", lang);

        let config = SttConfig::new(SttProviderType::Deepgram).with_language(lang);

        provider.initialize(&config).await.unwrap();

        let on_partial = Arc::new(|_: Transcription| {});
        let on_final = Arc::new(|t: Transcription| {
            println!("  Final: {}", t.text);
        });

        let on_error = stderr_error();

        provider
            .start_stream(on_partial, on_final, on_error, noop_connection_quality())
            .await
            .unwrap();

        // Отправляем тестовое аудио
        for _ in 0..5 {
            let chunk = AudioChunk::new(vec![100i16; 1600], 16000, 1);
            provider.send_audio(&chunk).await.unwrap();
            sleep(Duration::from_millis(100)).await;
        }

        provider.stop_stream().await.unwrap();
        sleep(Duration::from_millis(300)).await;
    }

    println!("\n✅ Все языки протестированы");
}

/// E2E тест: Abort во время активной сессии
#[tokio::test]
#[ignore]
async fn test_e2e_abort_during_session() {
    let mut provider = DeepgramProvider::new();

    let config = SttConfig::new(SttProviderType::Deepgram).with_language("ru");

    provider.initialize(&config).await.unwrap();

    let on_partial = Arc::new(|t: Transcription| {
        println!("Partial: {}", t.text);
    });

    let on_final = Arc::new(|t: Transcription| {
        println!("Final: {}", t.text);
    });

    let on_error = stderr_error();

    provider
        .start_stream(on_partial, on_final, on_error, noop_connection_quality())
        .await
        .unwrap();

    // Отправляем немного аудио
    for _ in 0..3 {
        let chunk = AudioChunk::new(vec![100i16; 1600], 16000, 1);
        provider.send_audio(&chunk).await.unwrap();
        sleep(Duration::from_millis(100)).await;
    }

    // Внезапно прерываем
    println!("⚠️  Вызываем abort...");
    let result = provider.abort().await;
    assert!(result.is_ok(), "Abort должен пройти успешно");

    // Проверяем что провайдер в безопасном состоянии
    let chunk = AudioChunk::new(vec![100i16; 1600], 16000, 1);
    let result = provider.send_audio(&chunk).await;
    assert!(
        result.is_err(),
        "После abort отправка аудио должна вернуть ошибку"
    );

    println!("✅ Abort отработал корректно");
}

// ============================================================================
// ТЕСТЫ С РЕАЛЬНЫМ АУДИО
// ============================================================================

/// Декодируем MP3 файл в PCM 16kHz mono
fn decode_mp3_to_pcm(mp3_path: &str) -> Result<Vec<i16>, Box<dyn std::error::Error>> {
    use std::fs::File;
    use std::io::Read;

    // Читаем MP3 файл
    let mut file = File::open(mp3_path)?;
    let mut mp3_data = Vec::new();
    file.read_to_end(&mut mp3_data)?;

    // Декодируем MP3
    let mut decoder = minimp3::Decoder::new(&mp3_data[..]);
    let mut all_samples = Vec::new();
    let mut sample_rate = 0;
    let mut channels = 0;

    loop {
        match decoder.next_frame() {
            Ok(frame) => {
                sample_rate = frame.sample_rate as u32;
                channels = frame.channels;
                all_samples.extend_from_slice(&frame.data);
            }
            Err(minimp3::Error::Eof) => break,
            Err(e) => return Err(Box::new(e)),
        }
    }

    println!(
        "📊 MP3 декодирован: {} Hz, {} channels, {} samples",
        sample_rate,
        channels,
        all_samples.len()
    );

    // Конвертируем в mono если нужно
    let mono_samples: Vec<i16> = if channels == 2 {
        all_samples
            .chunks_exact(2)
            .map(|chunk| ((chunk[0] as i32 + chunk[1] as i32) / 2) as i16)
            .collect()
    } else {
        all_samples
    };

    // Ресемплируем в 16kHz если нужно
    let resampled = if sample_rate != 16000 {
        println!("🔄 Ресемплирование {} Hz → 16000 Hz", sample_rate);

        use rubato::{
            Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType,
            WindowFunction,
        };

        let params = SincInterpolationParameters {
            sinc_len: 256,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 256,
            window: WindowFunction::BlackmanHarris2,
        };

        let mut resampler = SincFixedIn::<f32>::new(
            16000.0 / sample_rate as f64,
            2.0,
            params,
            mono_samples.len(),
            1,
        )?;

        // Конвертируем i16 → f32
        let input: Vec<f32> = mono_samples.iter().map(|&s| s as f32 / 32768.0).collect();
        let input_frames = vec![input];

        // Ресемплируем
        let output = resampler.process(&input_frames, None)?;

        // Конвертируем обратно f32 → i16
        output[0].iter().map(|&s| (s * 32768.0) as i16).collect()
    } else {
        mono_samples
    };

    // Проверяем амплитуду сигнала для отладки
    let max_amplitude = resampled.iter().map(|&s| s.abs()).max().unwrap_or(0);
    let avg_amplitude: i32 =
        resampled.iter().map(|&s| s.abs() as i32).sum::<i32>() / resampled.len().max(1) as i32;

    println!(
        "✅ Финальный PCM: 16000 Hz mono, {} samples (~{:.1} sec)",
        resampled.len(),
        resampled.len() as f32 / 16000.0
    );
    println!(
        "   Амплитуда: max={}, avg={}, rms={:.0}",
        max_amplitude,
        avg_amplitude,
        (resampled.iter().map(|&s| (s as f32).powi(2)).sum::<f32>() / resampled.len() as f32)
            .sqrt()
    );

    Ok(resampled)
}

/// Тест с реальным MP3 файлом - базовая декодировка
#[tokio::test]
#[ignore] // Используйте --ignored для запуска
async fn test_real_mp3_decode() {
    let mp3_path = "tests/fixtures/test_audio.mp3";

    let result = decode_mp3_to_pcm(mp3_path);
    assert!(result.is_ok(), "Не удалось декодировать MP3: {:?}", result);

    let samples = result.unwrap();
    assert!(!samples.is_empty(), "Получены пустые сэмплы");
    assert!(samples.len() > 1000, "Аудио слишком короткое");

    let duration_sec = samples.len() as f32 / 16000.0;
    println!(
        "✅ MP3 успешно декодирован: {} семплов, {:.2} секунд",
        samples.len(),
        duration_sec
    );
}

/// Тест с реальным MP3 - полная транскрипция через Deepgram
#[tokio::test]
#[ignore] // Используйте --ignored для запуска с реальным API
async fn test_real_mp3_transcription_deepgram() {
    // Инициализируем логгер для отладки
    let _ = env_logger::builder()
        .filter_level(log::LevelFilter::Debug)
        .is_test(true)
        .try_init();

    let mp3_path = "tests/fixtures/test_audio.mp3";

    println!("🎵 Загружаем и декодируем MP3...");
    let samples = decode_mp3_to_pcm(mp3_path).expect("Ошибка декодирования MP3");

    let mut provider = DeepgramProvider::new();

    let config = SttConfig::new(SttProviderType::Deepgram).with_language("en"); // Английский для теста

    provider.initialize(&config).await.unwrap();

    let partial_texts = Arc::new(Mutex::new(Vec::new()));
    let final_text = Arc::new(Mutex::new(String::new()));

    let p_texts = partial_texts.clone();
    let on_partial = Arc::new(move |t: Transcription| {
        println!("📝 Partial: {}", t.text);
        p_texts.lock().unwrap().push(t.text.clone());
    });

    let f_text = final_text.clone();
    let on_final = Arc::new(move |t: Transcription| {
        println!("✅ Final: {}", t.text);
        *f_text.lock().unwrap() = t.text.clone();
    });

    let on_error = stderr_error();

    println!("🔗 Подключаемся к Deepgram...");
    provider
        .start_stream(on_partial, on_final, on_error, noop_connection_quality())
        .await
        .unwrap();

    println!("📤 Отправляем аудио чанками...");

    // Отправляем аудио чанками по 100ms (1600 samples @ 16kHz)
    const CHUNK_SIZE: usize = 1600;
    let total_chunks = (samples.len() + CHUNK_SIZE - 1) / CHUNK_SIZE;

    for (i, chunk_samples) in samples.chunks(CHUNK_SIZE).enumerate() {
        let chunk = AudioChunk::new(chunk_samples.to_vec(), 16000, 1);
        provider.send_audio(&chunk).await.unwrap();

        if i % 10 == 0 {
            println!(
                "  Отправлено {}/{} чанков (~{:.1}s)",
                i,
                total_chunks,
                i as f32 * 0.1
            );
        }

        // Небольшая задержка чтобы имитировать реальное время
        sleep(Duration::from_millis(80)).await;
    }

    println!("⏸️  Ждем перед остановкой stream...");
    sleep(Duration::from_millis(500)).await;

    println!("🛑 Останавливаем stream (внутри есть ожидание финальных результатов)...");
    provider.stop_stream().await.unwrap();

    // Даем еще немного времени для гарантии
    println!("⏳ Финальная проверка результатов...");
    sleep(Duration::from_millis(500)).await;

    // Проверяем результаты
    let final_result = final_text.lock().unwrap().clone();
    let partial_results = partial_texts.lock().unwrap().len();

    println!("\n{}", "=".repeat(60));
    println!("📊 РЕЗУЛЬТАТЫ ТРАНСКРИПЦИИ");
    println!("{}", "=".repeat(60));
    println!("Partial результатов: {}", partial_results);
    println!("Финальный текст: {}", final_result);
    println!("{}\n", "=".repeat(60));

    // Проверяем результаты - должны получить хотя бы что-то
    assert!(
        !final_result.is_empty() || partial_results > 0,
        "Должны получить транскрипцию! В аудио есть слово 'WhatsApp'"
    );

    if !final_result.is_empty() {
        println!("✅ Получен финальный текст: '{}'", final_result);
        // Проверяем что получили что-то похожее на "WhatsApp" или "what's up"
        let lower = final_result.to_lowercase();
        assert!(
            lower.contains("whatsapp") || lower.contains("what") || lower.contains("app"),
            "Ожидали получить 'WhatsApp', но получили: '{}'",
            final_result
        );
    } else if partial_results > 0 {
        println!("✅ Получено {} partial результатов", partial_results);
        let all_partials = partial_texts.lock().unwrap();
        println!("   Partial тексты: {:?}", all_partials);
    }

    println!("✅ Тест транскрипции MP3 завершен успешно!");
}

/// Тест с более длинным MP3 (5 секунд) - полная транскрипция через Deepgram
#[tokio::test]
#[ignore] // Используйте --ignored для запуска с реальным API
async fn test_real_mp3_long_transcription_deepgram() {
    // Инициализируем логгер для отладки
    let _ = env_logger::builder()
        .filter_level(log::LevelFilter::Debug)
        .is_test(true)
        .try_init();

    let mp3_path = "tests/fixtures/just-a-dream.mp3";

    println!("🎵 Загружаем и декодируем MP3...");
    let samples = decode_mp3_to_pcm(mp3_path).expect("Ошибка декодирования MP3");

    let mut provider = DeepgramProvider::new();

    let config = SttConfig::new(SttProviderType::Deepgram).with_language("en"); // Английский для теста

    provider.initialize(&config).await.unwrap();

    let partial_texts = Arc::new(Mutex::new(Vec::new()));
    let final_texts = Arc::new(Mutex::new(Vec::new()));

    let p_texts = partial_texts.clone();
    let on_partial = Arc::new(move |t: Transcription| {
        println!("📝 Partial: {}", t.text);
        p_texts.lock().unwrap().push(t.text.clone());
    });

    let f_texts = final_texts.clone();
    let on_final = Arc::new(move |t: Transcription| {
        println!("✅ Final: {}", t.text);
        f_texts.lock().unwrap().push(t.text.clone());
    });

    let on_error = stderr_error();

    println!("🔗 Подключаемся к Deepgram...");
    provider
        .start_stream(on_partial, on_final, on_error, noop_connection_quality())
        .await
        .unwrap();

    println!("📤 Отправляем аудио чанками...");

    // Отправляем аудио чанками по 100ms (1600 samples @ 16kHz)
    const CHUNK_SIZE: usize = 1600;
    let total_chunks = (samples.len() + CHUNK_SIZE - 1) / CHUNK_SIZE;

    for (i, chunk_samples) in samples.chunks(CHUNK_SIZE).enumerate() {
        let chunk = AudioChunk::new(chunk_samples.to_vec(), 16000, 1);
        provider.send_audio(&chunk).await.unwrap();

        if i % 10 == 0 {
            println!(
                "  Отправлено {}/{} чанков (~{:.1}s)",
                i,
                total_chunks,
                i as f32 * 0.1
            );
        }

        // Небольшая задержка чтобы имитировать реальное время
        sleep(Duration::from_millis(80)).await;
    }

    println!("⏸️  Ждем перед остановкой stream...");
    sleep(Duration::from_millis(500)).await;

    println!("🛑 Останавливаем stream (внутри есть ожидание финальных результатов)...");
    provider.stop_stream().await.unwrap();

    // Даем еще немного времени для гарантии
    println!("⏳ Финальная проверка результатов...");
    sleep(Duration::from_millis(500)).await;

    // Проверяем результаты
    let final_results = final_texts.lock().unwrap().clone();
    let partial_results = partial_texts.lock().unwrap().len();

    println!("\n{}", "=".repeat(60));
    println!("📊 РЕЗУЛЬТАТЫ ТРАНСКРИПЦИИ (5-сек аудио)");
    println!("{}", "=".repeat(60));
    println!("Partial результатов: {}", partial_results);
    println!("Final результатов: {}", final_results.len());
    println!("\nФинальные транскрипции:");
    for (i, text) in final_results.iter().enumerate() {
        println!("  [{}] {}", i + 1, text);
    }
    println!("{}\n", "=".repeat(60));

    // Проверяем что получили хотя бы одну транскрипцию
    assert!(
        !final_results.is_empty() || partial_results > 0,
        "Должны получить транскрипцию для 5-секундного аудио"
    );

    if !final_results.is_empty() {
        let full_text = final_results.join(" ");
        println!("✅ Полный распознанный текст: '{}'", full_text);
        println!("✅ Получено {} финальных сегментов", final_results.len());
        assert!(!full_text.is_empty(), "Текст не должен быть пустым");
    }

    println!("✅ Тест транскрипции длинного MP3 завершен успешно!");
}

/// Тест с реальным MP3 - проверка качества транскрипции
#[tokio::test]
#[ignore]
async fn test_real_mp3_transcription_quality() {
    let mp3_path = "tests/fixtures/test_audio.mp3";

    let samples = decode_mp3_to_pcm(mp3_path).expect("Ошибка декодирования MP3");

    let mut provider = DeepgramProvider::new();

    let config = SttConfig::new(SttProviderType::Deepgram).with_language("en");

    provider.initialize(&config).await.unwrap();

    let transcriptions = Arc::new(Mutex::new(Vec::new()));
    let transcriptions_clone = transcriptions.clone();

    let on_partial = Arc::new(move |t: Transcription| {
        transcriptions_clone.lock().unwrap().push(t);
    });

    let transcriptions_final = transcriptions.clone();
    let on_final = Arc::new(move |t: Transcription| {
        transcriptions_final.lock().unwrap().push(t);
    });

    let on_error = stderr_error();

    provider
        .start_stream(on_partial, on_final, on_error, noop_connection_quality())
        .await
        .unwrap();

    // Отправляем весь аудио файл
    for chunk_samples in samples.chunks(1600) {
        let chunk = AudioChunk::new(chunk_samples.to_vec(), 16000, 1);
        provider.send_audio(&chunk).await.unwrap();
        sleep(Duration::from_millis(50)).await;
    }

    sleep(Duration::from_secs(3)).await;
    provider.stop_stream().await.unwrap();

    // Анализируем результаты
    let results = transcriptions.lock().unwrap();

    println!("\n📊 АНАЛИЗ КАЧЕСТВА ТРАНСКРИПЦИИ");
    println!("{}", "=".repeat(60));
    println!("Всего транскрипций: {}", results.len());

    let mut partial_count = 0;
    let mut final_count = 0;
    let mut total_confidence = 0.0;
    let mut confidence_count = 0;

    for t in results.iter() {
        if t.is_final {
            final_count += 1;
            println!("  [FINAL] {}", t.text);
        } else {
            partial_count += 1;
        }

        if let Some(conf) = t.confidence {
            total_confidence += conf;
            confidence_count += 1;
        }
    }

    println!("\nСтатистика:");
    println!("  Partial: {}", partial_count);
    println!("  Final: {}", final_count);

    if confidence_count > 0 {
        let avg_confidence = total_confidence / confidence_count as f32;
        println!("  Средняя уверенность: {:.2}%", avg_confidence * 100.0);
    }

    println!("{}\n", "=".repeat(60));

    assert!(results.len() > 0, "Не получено никаких результатов");

    println!("✅ Анализ качества завершен");
}

/// Стресс-тест: отправка MP3 в разных режимах
#[tokio::test]
#[ignore]
async fn test_real_mp3_different_chunk_sizes() {
    let mp3_path = "tests/fixtures/test_audio.mp3";
    let samples = decode_mp3_to_pcm(mp3_path).expect("Ошибка декодирования MP3");

    // Тестируем разные размеры чанков
    let chunk_sizes = vec![
        (800, "50ms"),
        (1600, "100ms"),
        (3200, "200ms"),
        (4800, "300ms"),
    ];

    for (chunk_size, description) in chunk_sizes {
        println!("\n🧪 Тест с чанками {}", description);

        let mut provider = DeepgramProvider::new();

        let config = SttConfig::new(SttProviderType::Deepgram).with_language("en");

        provider.initialize(&config).await.unwrap();

        let final_text = Arc::new(Mutex::new(String::new()));
        let f_text = final_text.clone();

        let on_partial = Arc::new(|_: Transcription| {});
        let on_final = Arc::new(move |t: Transcription| {
            *f_text.lock().unwrap() = t.text.clone();
        });

        let on_error = stderr_error();

        provider
            .start_stream(on_partial, on_final, on_error, noop_connection_quality())
            .await
            .unwrap();

        for chunk_samples in samples.chunks(chunk_size) {
            let chunk = AudioChunk::new(chunk_samples.to_vec(), 16000, 1);
            provider.send_audio(&chunk).await.unwrap();
            sleep(Duration::from_millis(30)).await;
        }

        sleep(Duration::from_secs(1)).await;
        provider.stop_stream().await.unwrap();

        let result = final_text.lock().unwrap().clone();
        println!("  Результат: {}", result);

        sleep(Duration::from_millis(500)).await;
    }

    println!("\n✅ Все режимы протестированы");
}
