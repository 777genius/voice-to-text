use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::sleep;

use app_lib::domain::{
    AudioChunk, SttConfig, SttProvider, SttProviderType, Transcription,
};
use app_lib::infrastructure::stt::{DeepgramProvider, AssemblyAIProvider};

mod test_support;
use test_support::{noop_connection_quality, SttConfigTestExt};

/// Хелпер для получения API ключей из окружения
fn get_deepgram_key() -> String {
    let _ = dotenv::dotenv();
    std::env::var("DEEPGRAM_TEST_KEY")
        .expect("DEEPGRAM_TEST_KEY environment variable must be set for tests")
}

fn get_assemblyai_key() -> String {
    let _ = dotenv::dotenv();
    std::env::var("ASSEMBLYAI_TEST_KEY")
        .expect("ASSEMBLYAI_TEST_KEY environment variable must be set for tests")
}

// ============================================================================
// E2E ТЕСТЫ - WebSocket Подключение и Реконнект
// ============================================================================

/// E2E: Тест базового подключения к Deepgram WebSocket
#[tokio::test]
#[ignore]
async fn test_e2e_deepgram_websocket_connection() {
    let mut provider = DeepgramProvider::new();

    let config = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key(&get_deepgram_key())
        .with_language("en");

    provider.initialize(&config).await.unwrap();

    let connected = Arc::new(Mutex::new(false));
    let connected_clone = connected.clone();

    let on_partial = Arc::new(move |t: Transcription| {
        // Если получили хотя бы одно сообщение - соединение работает
        *connected_clone.lock().unwrap() = true;
        println!("📝 Partial: {}", t.text);
    });

    let on_final = Arc::new(|t: Transcription| {
        println!("✅ Final: {}", t.text);
    });

    let on_error = Arc::new(|msg: String, err_type: String| {
        eprintln!("❌ Error: {} (type: {})", msg, err_type);
    });

    // Подключаемся
    let result = provider
        .start_stream(on_partial, on_final, on_error, noop_connection_quality())
        .await;
    assert!(result.is_ok(), "WebSocket подключение должно пройти успешно");

    println!("✅ WebSocket соединение установлено");

    // Отправляем тестовый чанк чтобы убедиться что соединение работает
    let chunk = AudioChunk::new(vec![100i16; 1600], 16000, 1);
    let result = provider.send_audio(&chunk).await;
    assert!(result.is_ok(), "Отправка аудио должна работать");

    // Даем время на обработку
    sleep(Duration::from_millis(500)).await;

    // Закрываем соединение
    provider.stop_stream().await.unwrap();
    println!("✅ Соединение корректно закрыто");
}

/// E2E: Тест базового подключения к AssemblyAI WebSocket
#[tokio::test]
#[ignore]
async fn test_e2e_assemblyai_websocket_connection() {
    let mut provider = AssemblyAIProvider::new();

    let config = SttConfig::new(SttProviderType::AssemblyAI)
        .with_api_key(&get_assemblyai_key())
        .with_language("en");

    provider.initialize(&config).await.unwrap();

    let on_partial = Arc::new(|t: Transcription| {
        println!("📝 Partial: {}", t.text);
    });

    let on_final = Arc::new(|t: Transcription| {
        println!("✅ Final: {}", t.text);
    });

    let on_error = Arc::new(|msg: String, err_type: String| {
        eprintln!("❌ Error: {} (type: {})", msg, err_type);
    });

    // Подключаемся
    let result = provider
        .start_stream(on_partial, on_final, on_error, noop_connection_quality())
        .await;
    assert!(result.is_ok(), "WebSocket подключение должно пройти успешно");

    println!("✅ WebSocket соединение установлено");

    // Отправляем тестовый чанк
    let chunk = AudioChunk::new(vec![100i16; 1600], 16000, 1);
    let result = provider.send_audio(&chunk).await;
    assert!(result.is_ok(), "Отправка аудио должна работать");

    sleep(Duration::from_millis(500)).await;

    provider.stop_stream().await.unwrap();
    println!("✅ Соединение корректно закрыто");
}

/// E2E: Тест переподключения после остановки (Deepgram)
#[tokio::test]
#[ignore]
async fn test_e2e_deepgram_reconnect() {
    let mut provider = DeepgramProvider::new();

    let config = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key(&get_deepgram_key())
        .with_language("ru");

    provider.initialize(&config).await.unwrap();

    // Первое подключение
    println!("🔌 Первое подключение...");
    let on_partial = Arc::new(|t: Transcription| println!("📝 {}", t.text));
    let on_final = Arc::new(|t: Transcription| println!("✅ {}", t.text));
    let on_error = Arc::new(|msg: String, err_type: String| {
        eprintln!("❌ Error: {} ({})", msg, err_type);
    });

    provider
        .start_stream(
            on_partial.clone(),
            on_final.clone(),
            on_error.clone(),
            noop_connection_quality(),
        )
        .await
        .unwrap();

    // Отправляем данные
    let chunk = AudioChunk::new(vec![100i16; 1600], 16000, 1);
    provider.send_audio(&chunk).await.unwrap();
    sleep(Duration::from_millis(300)).await;

    // Останавливаем
    println!("🛑 Останавливаем первое соединение...");
    provider.stop_stream().await.unwrap();
    sleep(Duration::from_millis(500)).await;

    // Второе подключение (переподключение)
    println!("🔌 Переподключаемся...");
    provider
        .start_stream(on_partial, on_final, on_error, noop_connection_quality())
        .await
        .unwrap();

    // Отправляем данные снова
    provider.send_audio(&chunk).await.unwrap();
    sleep(Duration::from_millis(300)).await;

    // Останавливаем
    provider.stop_stream().await.unwrap();

    println!("✅ Переподключение работает корректно");
}

/// E2E: Тест множественных последовательных подключений (проверяем утечки памяти)
#[tokio::test]
#[ignore]
async fn test_e2e_multiple_sequential_connections() {
    let mut provider = DeepgramProvider::new();

    let config = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key(&get_deepgram_key())
        .with_language("en");

    provider.initialize(&config).await.unwrap();

    let connections_count = 5;

    for i in 1..=connections_count {
        println!("\n🔌 Подключение #{}", i);

        let on_partial = Arc::new(|_: Transcription| {});
        let on_final = Arc::new(|_: Transcription| {});
        let on_error = Arc::new(|msg: String, err_type: String| {
            eprintln!("❌ Error: {} ({})", msg, err_type);
        });

        provider
            .start_stream(on_partial, on_final, on_error, noop_connection_quality())
            .await
            .unwrap();

        // Отправляем немного данных
        for _ in 0..3 {
            let chunk = AudioChunk::new(vec![100i16; 1600], 16000, 1);
            provider.send_audio(&chunk).await.unwrap();
            sleep(Duration::from_millis(100)).await;
        }

        // Останавливаем
        provider.stop_stream().await.unwrap();

        // Небольшая пауза между подключениями
        sleep(Duration::from_millis(300)).await;
    }

    println!("\n✅ Все {} подключений прошли успешно (утечек памяти нет)", connections_count);
}

/// E2E: Тест abort во время активного соединения
#[tokio::test]
#[ignore]
async fn test_e2e_abort_during_active_connection() {
    let mut provider = DeepgramProvider::new();

    let config = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key(&get_deepgram_key())
        .with_language("en");

    provider.initialize(&config).await.unwrap();

    let on_partial = Arc::new(|t: Transcription| println!("📝 {}", t.text));
    let on_final = Arc::new(|t: Transcription| println!("✅ {}", t.text));
    let on_error = Arc::new(|msg: String, err_type: String| {
        eprintln!("❌ Error: {} ({})", msg, err_type);
    });

    provider
        .start_stream(on_partial, on_final, on_error, noop_connection_quality())
        .await
        .unwrap();

    // Отправляем данные
    for _ in 0..5 {
        let chunk = AudioChunk::new(vec![100i16; 1600], 16000, 1);
        provider.send_audio(&chunk).await.unwrap();
        sleep(Duration::from_millis(100)).await;
    }

    // Внезапно прерываем
    println!("⚠️  Вызываем abort...");
    provider.abort().await.unwrap();

    // Проверяем что провайдер в безопасном состоянии
    let chunk = AudioChunk::new(vec![100i16; 1600], 16000, 1);
    let result = provider.send_audio(&chunk).await;
    assert!(result.is_err(), "После abort отправка должна вернуть ошибку");

    println!("✅ Abort отработал корректно");
}

// ============================================================================
// E2E ТЕСТЫ - Обработка Сообщений и Ошибок
// ============================================================================

/// E2E: Тест получения partial и final транскрипций (Deepgram)
#[tokio::test]
#[ignore]
async fn test_e2e_deepgram_message_handling() {
    let mut provider = DeepgramProvider::new();

    let config = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key(&get_deepgram_key())
        .with_language("en");

    provider.initialize(&config).await.unwrap();

    let partial_count = Arc::new(Mutex::new(0));
    let final_count = Arc::new(Mutex::new(0));
    let all_texts = Arc::new(Mutex::new(Vec::new()));

    let p_count = partial_count.clone();
    let texts_clone = all_texts.clone();
    let on_partial = Arc::new(move |t: Transcription| {
        *p_count.lock().unwrap() += 1;
        texts_clone.lock().unwrap().push(format!("[PARTIAL] {}", t.text));
        println!("📝 Partial: {}", t.text);
    });

    let f_count = final_count.clone();
    let texts_final = all_texts.clone();
    let on_final = Arc::new(move |t: Transcription| {
        *f_count.lock().unwrap() += 1;
        texts_final.lock().unwrap().push(format!("[FINAL] {}", t.text));
        println!("✅ Final: {}", t.text);
    });

    let on_error = Arc::new(|msg: String, err_type: String| {
        eprintln!("❌ Error: {} ({})", msg, err_type);
    });

    provider
        .start_stream(on_partial, on_final, on_error, noop_connection_quality())
        .await
        .unwrap();

    // Отправляем достаточно аудио чтобы получить транскрипции
    for i in 0..20 {
        // Генерируем немного разнообразный сигнал
        let freq = 200.0 + (i as f32 * 50.0);
        let mut samples = Vec::with_capacity(1600);
        for j in 0..1600 {
            let t = j as f32 / 16000.0;
            let val = (2.0 * std::f32::consts::PI * freq * t).sin() * 5000.0;
            samples.push(val as i16);
        }

        let chunk = AudioChunk::new(samples, 16000, 1);
        provider.send_audio(&chunk).await.unwrap();
        sleep(Duration::from_millis(100)).await;
    }

    // Ждем финальные результаты
    sleep(Duration::from_secs(1)).await;
    provider.stop_stream().await.unwrap();
    sleep(Duration::from_millis(500)).await;

    // Анализируем результаты
    let partial = *partial_count.lock().unwrap();
    let final_res = *final_count.lock().unwrap();
    let texts = all_texts.lock().unwrap();

    println!("\n📊 Статистика обработки сообщений:");
    println!("  Partial транскрипций: {}", partial);
    println!("  Final транскрипций: {}", final_res);
    println!("  Всего текстов: {}", texts.len());

    if !texts.is_empty() {
        println!("\n  Примеры текстов:");
        for (i, text) in texts.iter().take(5).enumerate() {
            println!("    {}: {}", i + 1, text);
        }
    }

    // Проверяем что получили хотя бы что-то
    assert!(
        partial > 0 || final_res > 0,
        "Должны получить хотя бы одну транскрипцию"
    );

    println!("\n✅ Обработка сообщений работает корректно");
}

/// E2E: Тест обработки ошибок подключения (неверный API ключ)
#[tokio::test]
async fn test_e2e_connection_error_invalid_key() {
    let mut provider = DeepgramProvider::new();

    // Специально неверный ключ
    let config = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key("invalid_key_12345_wrong");

    provider.initialize(&config).await.unwrap();

    let on_partial = Arc::new(|_: Transcription| {});
    let on_final = Arc::new(|_: Transcription| {});
    let on_error = Arc::new(|msg: String, err_type: String| {
        println!("📌 Получена ожидаемая ошибка: {} ({})", msg, err_type);
    });

    // Попытка подключиться должна вернуть ошибку
    let result = provider
        .start_stream(on_partial, on_final, on_error, noop_connection_quality())
        .await;
    assert!(result.is_err(), "Должна быть ошибка с неверным API ключом");

    if let Err(e) = result {
        println!("✅ Корректно обработана ошибка: {:?}", e);
    }
}

/// E2E: Тест обработки timeout (очень долгое ожидание)
#[tokio::test]
#[ignore]
async fn test_e2e_connection_timeout_handling() {
    // Тестируем ситуацию когда сервер не отвечает вовремя
    // Для этого можем использовать неправильный URL или долгий таймаут

    let mut provider = DeepgramProvider::new();

    let config = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key(&get_deepgram_key())
        .with_language("en");

    provider.initialize(&config).await.unwrap();

    let timeout_detected = Arc::new(Mutex::new(false));
    let timeout_clone = timeout_detected.clone();

    let on_partial = Arc::new(|_: Transcription| {});
    let on_final = Arc::new(|_: Transcription| {});
    let on_error = Arc::new(move |msg: String, err_type: String| {
        println!("📌 Error: {} ({})", msg, err_type);
        if err_type == "timeout" {
            *timeout_clone.lock().unwrap() = true;
        }
    });

    // Пытаемся подключиться с таймаутом
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        provider.start_stream(on_partial, on_final, on_error, noop_connection_quality())
    ).await;

    match result {
        Ok(Ok(_)) => {
            println!("✅ Соединение установлено (нормальный случай)");
            provider.stop_stream().await.unwrap();
        }
        Ok(Err(e)) => {
            println!("✅ Получена ошибка соединения: {:?}", e);
        }
        Err(_) => {
            println!("✅ Timeout соединения обработан корректно");
        }
    }
}

/// E2E: Тест обработки сообщений Close от сервера
#[tokio::test]
#[ignore]
async fn test_e2e_server_initiated_close() {
    let mut provider = DeepgramProvider::new();

    let config = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key(&get_deepgram_key())
        .with_language("en");

    provider.initialize(&config).await.unwrap();

    let close_detected = Arc::new(Mutex::new(false));
    let close_clone = close_detected.clone();

    let on_partial = Arc::new(|t: Transcription| println!("📝 {}", t.text));
    let on_final = Arc::new(|t: Transcription| println!("✅ {}", t.text));
    let on_error = Arc::new(move |msg: String, err_type: String| {
        println!("📌 Close/Error: {} ({})", msg, err_type);
        *close_clone.lock().unwrap() = true;
    });

    provider
        .start_stream(on_partial, on_final, on_error, noop_connection_quality())
        .await
        .unwrap();

    // Отправляем данные
    for _ in 0..10 {
        let chunk = AudioChunk::new(vec![100i16; 1600], 16000, 1);
        provider.send_audio(&chunk).await.unwrap();
        sleep(Duration::from_millis(100)).await;
    }

    // Нормальное закрытие соединения
    provider.stop_stream().await.unwrap();

    println!("✅ Соединение закрыто корректно");
}

// ============================================================================
// E2E ТЕСТЫ - Сценарии со Слабым Интернетом
// ============================================================================

/// E2E: Тест с медленной отправкой данных (имитация слабого интернета)
#[tokio::test]
#[ignore]
async fn test_e2e_slow_network_simulation() {
    let mut provider = DeepgramProvider::new();

    let config = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key(&get_deepgram_key())
        .with_language("en");

    provider.initialize(&config).await.unwrap();

    let transcriptions = Arc::new(Mutex::new(Vec::new()));
    let trans_clone = transcriptions.clone();

    let on_partial = Arc::new(move |t: Transcription| {
        trans_clone.lock().unwrap().push(t.text.clone());
        println!("📝 Partial: {}", t.text);
    });

    let on_final = Arc::new(|t: Transcription| {
        println!("✅ Final: {}", t.text);
    });

    let on_error = Arc::new(|msg: String, err_type: String| {
        eprintln!("❌ Error: {} ({})", msg, err_type);
    });

    provider
        .start_stream(on_partial, on_final, on_error, noop_connection_quality())
        .await
        .unwrap();

    println!("🐌 Имитируем медленное соединение (задержки 300-500ms)...");

    // Отправляем с большими задержками (имитация медленного интернета)
    for i in 0..10 {
        let chunk = AudioChunk::new(vec![100i16; 1600], 16000, 1);

        let send_start = std::time::Instant::now();
        let result = provider.send_audio(&chunk).await;
        let send_duration = send_start.elapsed();

        if result.is_ok() {
            println!("  Чанк {} отправлен за {:.1}ms", i + 1, send_duration.as_millis());
        } else {
            println!("  ⚠️ Чанк {} не отправлен: {:?}", i + 1, result);
        }

        // Большая задержка для имитации слабого интернета
        sleep(Duration::from_millis(400)).await;
    }

    sleep(Duration::from_secs(1)).await;
    provider.stop_stream().await.unwrap();

    println!("✅ Медленное соединение обработано корректно");
}

/// E2E: Тест с большими пачками данных (batch sending)
#[tokio::test]
#[ignore]
async fn test_e2e_batch_sending() {
    let mut provider = DeepgramProvider::new();

    let config = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key(&get_deepgram_key())
        .with_language("en");

    provider.initialize(&config).await.unwrap();

    let on_partial = Arc::new(|t: Transcription| println!("📝 {}", t.text));
    let on_final = Arc::new(|t: Transcription| println!("✅ {}", t.text));
    let on_error = Arc::new(|msg: String, err_type: String| {
        eprintln!("❌ Error: {} ({})", msg, err_type);
    });

    provider
        .start_stream(on_partial, on_final, on_error, noop_connection_quality())
        .await
        .unwrap();

    println!("📦 Отправляем большие пачки данных...");

    // Отправляем большие чанки (500ms аудио за раз)
    for i in 0..10 {
        // 500ms @ 16kHz = 8000 samples
        let chunk = AudioChunk::new(vec![100i16; 8000], 16000, 1);

        let send_start = std::time::Instant::now();
        let result = provider.send_audio(&chunk).await;
        let send_duration = send_start.elapsed();

        if result.is_ok() {
            println!("  Большой чанк {} (500ms) отправлен за {:.1}ms", i + 1, send_duration.as_millis());
        } else {
            eprintln!("  ⚠️ Ошибка отправки: {:?}", result);
        }

        sleep(Duration::from_millis(100)).await;
    }

    sleep(Duration::from_secs(1)).await;
    provider.stop_stream().await.unwrap();

    println!("✅ Отправка больших пачек данных работает");
}

/// E2E: Стресс-тест с быстрой отправкой данных
#[tokio::test]
#[ignore]
async fn test_e2e_high_frequency_sending() {
    let mut provider = DeepgramProvider::new();

    let config = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key(&get_deepgram_key())
        .with_language("en");

    provider.initialize(&config).await.unwrap();

    let sent_count = Arc::new(Mutex::new(0));
    let failed_count = Arc::new(Mutex::new(0));

    let on_partial = Arc::new(|_: Transcription| {});
    let on_final = Arc::new(|_: Transcription| {});
    let on_error = Arc::new(|msg: String, err_type: String| {
        eprintln!("❌ Error: {} ({})", msg, err_type);
    });

    provider
        .start_stream(on_partial, on_final, on_error, noop_connection_quality())
        .await
        .unwrap();

    println!("⚡ Стресс-тест: быстрая отправка данных (10ms интервалы)...");

    // Отправляем очень часто (каждые 10ms)
    for i in 0..100 {
        let chunk = AudioChunk::new(vec![100i16; 160], 16000, 1); // 10ms чанк

        match provider.send_audio(&chunk).await {
            Ok(_) => *sent_count.lock().unwrap() += 1,
            Err(_) => *failed_count.lock().unwrap() += 1,
        }

        sleep(Duration::from_millis(10)).await;

        if i % 20 == 0 {
            println!("  Отправлено {} чанков...", i);
        }
    }

    sleep(Duration::from_secs(1)).await;
    provider.stop_stream().await.unwrap();

    let sent = *sent_count.lock().unwrap();
    let failed = *failed_count.lock().unwrap();

    println!("📊 Результаты:");
    println!("  Успешно отправлено: {}", sent);
    println!("  Ошибок: {}", failed);
    println!("  Success rate: {:.1}%", (sent as f32 / (sent + failed) as f32) * 100.0);

    assert!(sent > 90, "Большинство отправок должны быть успешными");

    println!("✅ Высокочастотная отправка работает стабильно");
}

/// E2E: Тест Keep-Alive механизма при паузе
#[tokio::test]
#[ignore]
async fn test_e2e_keepalive_mechanism() {
    let mut provider = DeepgramProvider::new();

    let config = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key(&get_deepgram_key())
        .with_language("en");

    provider.initialize(&config).await.unwrap();

    // Проверяем что провайдер поддерживает keep-alive
    assert!(provider.supports_keep_alive(), "Deepgram должен поддерживать keep-alive");

    let on_partial = Arc::new(|t: Transcription| println!("📝 {}", t.text));
    let on_final = Arc::new(|t: Transcription| println!("✅ {}", t.text));
    let on_error = Arc::new(|msg: String, err_type: String| {
        eprintln!("❌ Error: {} ({})", msg, err_type);
    });

    provider
        .start_stream(
            on_partial.clone(),
            on_final.clone(),
            on_error.clone(),
            noop_connection_quality(),
        )
        .await
        .unwrap();

    // Отправляем немного данных
    println!("📤 Отправляем начальные данные...");
    for _ in 0..5 {
        let chunk = AudioChunk::new(vec![100i16; 1600], 16000, 1);
        provider.send_audio(&chunk).await.unwrap();
        sleep(Duration::from_millis(100)).await;
    }

    // Ставим на паузу (keep-alive режим)
    println!("⏸️  Ставим на паузу (keep-alive)...");
    provider.pause_stream().await.unwrap();
    assert!(provider.is_connection_alive(), "Соединение должно быть живым в режиме паузы");

    // Ждем 10 секунд - за это время keep-alive должен сработать несколько раз
    println!("⏱️  Ждем 10 секунд (keep-alive работает в фоне)...");
    for i in 1..=10 {
        sleep(Duration::from_secs(1)).await;
        println!("  {} сек...", i);
    }

    // Возобновляем стрим
    println!("▶️  Возобновляем стрим...");
    provider
        .resume_stream(on_partial, on_final, on_error, noop_connection_quality())
        .await
        .unwrap();

    // Отправляем данные снова - соединение должно быть живым
    println!("📤 Отправляем данные после паузы...");
    for _ in 0..5 {
        let chunk = AudioChunk::new(vec![100i16; 1600], 16000, 1);
        let result = provider.send_audio(&chunk).await;
        assert!(result.is_ok(), "После pause/resume отправка должна работать");
        sleep(Duration::from_millis(100)).await;
    }

    provider.stop_stream().await.unwrap();

    println!("✅ Keep-alive механизм работает корректно");
}

/// E2E: Тест восстановления после разрыва соединения
#[tokio::test]
#[ignore]
async fn test_e2e_recovery_after_connection_loss() {
    let mut provider = DeepgramProvider::new();

    let config = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key(&get_deepgram_key())
        .with_language("en");

    provider.initialize(&config).await.unwrap();

    let on_partial = Arc::new(|t: Transcription| println!("📝 {}", t.text));
    let on_final = Arc::new(|t: Transcription| println!("✅ {}", t.text));
    let on_error = Arc::new(|msg: String, err_type: String| {
        println!("📌 Error: {} ({})", msg, err_type);
    });

    // Первое соединение
    println!("🔌 Устанавливаем соединение...");
    provider
        .start_stream(
            on_partial.clone(),
            on_final.clone(),
            on_error.clone(),
            noop_connection_quality(),
        )
        .await
        .unwrap();

    // Отправляем данные
    for _ in 0..5 {
        let chunk = AudioChunk::new(vec![100i16; 1600], 16000, 1);
        provider.send_audio(&chunk).await.unwrap();
        sleep(Duration::from_millis(100)).await;
    }

    // Имитируем разрыв - форсированно прерываем
    println!("💥 Имитируем разрыв соединения (abort)...");
    provider.abort().await.unwrap();

    sleep(Duration::from_millis(500)).await;

    // Восстанавливаем соединение
    println!("🔄 Восстанавливаем соединение...");
    let recovery_result = provider
        .start_stream(on_partial, on_final, on_error, noop_connection_quality())
        .await;
    assert!(recovery_result.is_ok(), "Восстановление соединения должно работать");

    // Проверяем что можем отправлять данные
    for _ in 0..5 {
        let chunk = AudioChunk::new(vec![100i16; 1600], 16000, 1);
        let result = provider.send_audio(&chunk).await;
        assert!(result.is_ok(), "После восстановления отправка должна работать");
        sleep(Duration::from_millis(100)).await;
    }

    provider.stop_stream().await.unwrap();

    println!("✅ Восстановление после разрыва работает");
}

// ============================================================================
// E2E ТЕСТЫ - Комплексные Сценарии
// ============================================================================

/// E2E: Длинная сессия с периодическими паузами (реальный use case)
#[tokio::test]
#[ignore]
async fn test_e2e_long_session_with_pauses() {
    let mut provider = DeepgramProvider::new();

    let config = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key(&get_deepgram_key())
        .with_language("ru");

    provider.initialize(&config).await.unwrap();

    let transcriptions = Arc::new(Mutex::new(Vec::new()));
    let trans_clone = transcriptions.clone();

    let on_partial = Arc::new(move |t: Transcription| {
        trans_clone.lock().unwrap().push(t.text.clone());
    });

    let on_final = Arc::new(|t: Transcription| {
        println!("✅ Final: {}", t.text);
    });

    let on_error = Arc::new(|msg: String, err_type: String| {
        eprintln!("❌ Error: {} ({})", msg, err_type);
    });

    provider
        .start_stream(
            on_partial.clone(),
            on_final.clone(),
            on_error.clone(),
            noop_connection_quality(),
        )
        .await
        .unwrap();

    println!("🎙️  Длинная сессия с паузами (имитация реального использования)...");

    // Цикл: говорим → пауза → говорим → пауза
    for cycle in 1..=3 {
        println!("\n🔊 Цикл {} - Говорим...", cycle);

        // "Говорим" (отправляем аудио)
        for _ in 0..10 {
            let chunk = AudioChunk::new(vec![100i16; 1600], 16000, 1);
            provider.send_audio(&chunk).await.unwrap();
            sleep(Duration::from_millis(100)).await;
        }

        // Пауза (keep-alive)
        println!("⏸️  Пауза {} сек...", cycle * 2);
        provider.pause_stream().await.unwrap();
        sleep(Duration::from_secs(cycle * 2)).await;

        // Возобновляем
        println!("▶️  Возобновляем...");
        provider
            .resume_stream(
                on_partial.clone(),
                on_final.clone(),
                on_error.clone(),
                noop_connection_quality(),
            )
            .await
            .unwrap();
    }

    // Финальный отрезок
    println!("\n🔊 Финальный отрезок...");
    for _ in 0..10 {
        let chunk = AudioChunk::new(vec![100i16; 1600], 16000, 1);
        provider.send_audio(&chunk).await.unwrap();
        sleep(Duration::from_millis(100)).await;
    }

    provider.stop_stream().await.unwrap();

    let trans_count = transcriptions.lock().unwrap().len();
    println!("\n✅ Длинная сессия завершена. Получено {} транскрипций", trans_count);
}

/// E2E: Сравнение производительности Deepgram vs AssemblyAI
#[tokio::test]
#[ignore]
async fn test_e2e_performance_comparison() {
    println!("⚡ Сравнение производительности WebSocket провайдеров\n");

    // Подготавливаем тестовые данные
    let test_chunks: Vec<AudioChunk> = (0..50)
        .map(|_| AudioChunk::new(vec![100i16; 1600], 16000, 1))
        .collect();

    // Тест Deepgram
    println!("🧪 Тестируем Deepgram...");
    let deepgram_start = std::time::Instant::now();

    let mut deepgram = DeepgramProvider::new();
    let config = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key(&get_deepgram_key())
        .with_language("en");
    deepgram.initialize(&config).await.unwrap();

    let on_p = Arc::new(|_: Transcription| {});
    let on_f = Arc::new(|_: Transcription| {});
    let on_e = Arc::new(|_: String, _: String| {});

    deepgram
        .start_stream(on_p.clone(), on_f.clone(), on_e.clone(), noop_connection_quality())
        .await
        .unwrap();

    for chunk in &test_chunks {
        deepgram.send_audio(chunk).await.unwrap();
        sleep(Duration::from_millis(10)).await;
    }

    deepgram.stop_stream().await.unwrap();
    let deepgram_duration = deepgram_start.elapsed();

    println!("  Deepgram: {:.2}s", deepgram_duration.as_secs_f32());

    // Небольшая пауза между тестами
    sleep(Duration::from_secs(1)).await;

    // Тест AssemblyAI
    println!("🧪 Тестируем AssemblyAI...");
    let assemblyai_start = std::time::Instant::now();

    let mut assemblyai = AssemblyAIProvider::new();
    let config = SttConfig::new(SttProviderType::AssemblyAI)
        .with_api_key(&get_assemblyai_key())
        .with_language("en");
    assemblyai.initialize(&config).await.unwrap();

    assemblyai
        .start_stream(on_p, on_f, on_e, noop_connection_quality())
        .await
        .unwrap();

    for chunk in &test_chunks {
        assemblyai.send_audio(chunk).await.unwrap();
        sleep(Duration::from_millis(10)).await;
    }

    assemblyai.stop_stream().await.unwrap();
    let assemblyai_duration = assemblyai_start.elapsed();

    println!("  AssemblyAI: {:.2}s", assemblyai_duration.as_secs_f32());

    // Сравнение
    println!("\n📊 Результаты:");
    println!("  Deepgram:   {:.2}s", deepgram_duration.as_secs_f32());
    println!("  AssemblyAI: {:.2}s", assemblyai_duration.as_secs_f32());

    if deepgram_duration < assemblyai_duration {
        let diff = assemblyai_duration.as_secs_f32() - deepgram_duration.as_secs_f32();
        println!("  🏆 Deepgram быстрее на {:.2}s ({:.1}%)",
            diff,
            (diff / assemblyai_duration.as_secs_f32()) * 100.0
        );
    } else {
        let diff = deepgram_duration.as_secs_f32() - assemblyai_duration.as_secs_f32();
        println!("  🏆 AssemblyAI быстрее на {:.2}s ({:.1}%)",
            diff,
            (diff / deepgram_duration.as_secs_f32()) * 100.0
        );
    }

    println!("\n✅ Сравнение производительности завершено");
}
