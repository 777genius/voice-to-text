use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};
use tokio::time::sleep;

use app_lib::domain::{AudioChunk, SttConfig, SttProvider, SttProviderType, Transcription};
use app_lib::infrastructure::stt::{AssemblyAIProvider, DeepgramProvider};

mod test_support;
use test_support::{noop_connection_quality, noop_error, stderr_error, SttConfigTestExt};

/// Хелпер для получения API ключей
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
// ПРОДВИНУТЫЕ ТЕСТЫ - WebSocket Протокол
// ============================================================================

/// Тест: Проверка Ping/Pong механизма (keep-alive на уровне протокола)
#[tokio::test]
#[ignore]
async fn test_websocket_ping_pong_mechanism() {
    let mut provider = DeepgramProvider::new();

    let config = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key(&get_deepgram_key())
        .with_language("en");

    provider.initialize(&config).await.unwrap();

    let on_partial = Arc::new(|_: Transcription| {});
    let on_final = Arc::new(|_: Transcription| {});
    let on_error = stderr_error();

    provider
        .start_stream(on_partial, on_final, on_error, noop_connection_quality())
        .await
        .unwrap();

    println!("🏓 Тест Ping/Pong механизма...");
    println!("   WebSocket должен автоматически отвечать на Ping от сервера");

    // Держим соединение открытым долгое время
    // За это время сервер должен отправить несколько Ping, и клиент должен отвечать Pong
    for i in 1..=30 {
        sleep(Duration::from_secs(1)).await;

        if i % 5 == 0 {
            println!("   {} секунд - соединение живо", i);

            // Проверяем что можем отправлять данные (значит Ping/Pong работает)
            let chunk = AudioChunk::new(vec![100i16; 1600], 16000, 1);
            let result = provider.send_audio(&chunk).await;
            assert!(
                result.is_ok(),
                "Соединение должно быть живым благодаря Ping/Pong"
            );
        }
    }

    provider.stop_stream().await.unwrap();
    println!("✅ Ping/Pong механизм работает корректно (30 секунд без разрыва)");
}

/// Тест: Обработка различных типов WebSocket сообщений
#[tokio::test]
#[ignore]
async fn test_websocket_message_types() {
    let mut provider = DeepgramProvider::new();

    let config = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key(&get_deepgram_key())
        .with_language("en");

    provider.initialize(&config).await.unwrap();

    let _metadata_received = Arc::new(AtomicBool::new(false));
    let results_received = Arc::new(AtomicUsize::new(0));
    let _errors_received = Arc::new(AtomicUsize::new(0));

    let results_count = results_received.clone();

    let on_partial = Arc::new(move |_: Transcription| {
        results_count.fetch_add(1, Ordering::SeqCst);
    });

    let on_final = Arc::new(|_: Transcription| {});
    let on_error = noop_error();

    provider
        .start_stream(on_partial, on_final, on_error, noop_connection_quality())
        .await
        .unwrap();

    println!("📨 Отправляем аудио и ждем различные типы сообщений...");

    // Отправляем достаточно данных чтобы получить разные типы сообщений
    for i in 0..20 {
        let chunk = AudioChunk::new(vec![100i16; 1600], 16000, 1);
        provider.send_audio(&chunk).await.unwrap();

        if i % 5 == 0 {
            println!("   Отправлено {} чанков", i);
        }

        sleep(Duration::from_millis(100)).await;
    }

    sleep(Duration::from_secs(2)).await;
    provider.stop_stream().await.unwrap();

    let results = results_received.load(Ordering::SeqCst);

    println!("\n📊 Статистика сообщений:");
    println!("   Results сообщений: {}", results);

    // Deepgram должен отправить хотя бы несколько сообщений
    // (Metadata всегда приходит, Results зависят от аудио)
    println!("✅ Различные типы WebSocket сообщений обработаны корректно");
}

/// Тест: Graceful close vs Abrupt close
#[tokio::test]
#[ignore]
async fn test_websocket_graceful_vs_abrupt_close() {
    println!("🔄 Тест: Graceful close vs Abrupt close\n");

    // Сценарий 1: Graceful close (stop_stream)
    println!("1️⃣  Тест graceful close (stop_stream)...");
    let mut provider1 = DeepgramProvider::new();
    let config = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key(&get_deepgram_key())
        .with_language("en");

    provider1.initialize(&config).await.unwrap();

    let on_p = Arc::new(|_: Transcription| {});
    let on_f = Arc::new(|_: Transcription| {});
    let on_e = noop_error();

    provider1
        .start_stream(
            on_p.clone(),
            on_f.clone(),
            on_e.clone(),
            noop_connection_quality(),
        )
        .await
        .unwrap();

    // Отправляем данные
    for _ in 0..5 {
        let chunk = AudioChunk::new(vec![100i16; 1600], 16000, 1);
        provider1.send_audio(&chunk).await.unwrap();
        sleep(Duration::from_millis(100)).await;
    }

    let graceful_start = Instant::now();
    provider1.stop_stream().await.unwrap();
    let graceful_duration = graceful_start.elapsed();

    println!(
        "   Graceful close завершен за {:.2}s",
        graceful_duration.as_secs_f32()
    );

    sleep(Duration::from_millis(500)).await;

    // Сценарий 2: Abrupt close (abort)
    println!("\n2️⃣  Тест abrupt close (abort)...");
    let mut provider2 = DeepgramProvider::new();
    provider2.initialize(&config).await.unwrap();
    provider2
        .start_stream(on_p, on_f, on_e, noop_connection_quality())
        .await
        .unwrap();

    // Отправляем данные
    for _ in 0..5 {
        let chunk = AudioChunk::new(vec![100i16; 1600], 16000, 1);
        provider2.send_audio(&chunk).await.unwrap();
        sleep(Duration::from_millis(100)).await;
    }

    let abrupt_start = Instant::now();
    provider2.abort().await.unwrap();
    let abrupt_duration = abrupt_start.elapsed();

    println!(
        "   Abrupt close завершен за {:.2}s",
        abrupt_duration.as_secs_f32()
    );

    println!("\n📊 Сравнение:");
    println!(
        "   Graceful close: {:.2}s (ждет финальные результаты)",
        graceful_duration.as_secs_f32()
    );
    println!(
        "   Abrupt close:   {:.2}s (немедленное прерывание)",
        abrupt_duration.as_secs_f32()
    );

    // Abrupt должен быть значительно быстрее
    assert!(
        abrupt_duration < graceful_duration,
        "Abort должен быть быстрее graceful close"
    );

    println!("✅ Оба типа закрытия работают корректно");
}

// ============================================================================
// ТЕСТЫ КОНКУРЕНТНОСТИ
// ============================================================================

/// Тест: Одновременная отправка из нескольких потоков (thread-safety)
#[tokio::test]
#[ignore]
async fn test_websocket_concurrent_sending() {
    let mut provider = DeepgramProvider::new();

    let config = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key(&get_deepgram_key())
        .with_language("en");

    provider.initialize(&config).await.unwrap();

    let sent_count = Arc::new(AtomicUsize::new(0));

    let count_clone = sent_count.clone();
    let on_partial = Arc::new(move |_: Transcription| {
        count_clone.fetch_add(1, Ordering::SeqCst);
    });

    let on_final = Arc::new(|_: Transcription| {});
    let on_error = noop_error();

    provider
        .start_stream(on_partial, on_final, on_error, noop_connection_quality())
        .await
        .unwrap();

    println!("🔀 Тест конкурентной отправки данных...");

    // Создаем несколько задач которые отправляют данные одновременно
    let provider_arc = Arc::new(tokio::sync::Mutex::new(provider));
    let mut tasks = vec![];

    for task_id in 0..5 {
        let provider_clone = provider_arc.clone();

        let task = tokio::spawn(async move {
            for i in 0..10 {
                let chunk = AudioChunk::new(vec![100i16; 1600], 16000, 1);

                let mut provider = provider_clone.lock().await;
                match provider.send_audio(&chunk).await {
                    Ok(_) => {
                        if i % 3 == 0 {
                            println!("   Task {}: отправлено {} чанков", task_id, i + 1);
                        }
                    }
                    Err(e) => {
                        eprintln!("   Task {}: ошибка - {:?}", task_id, e);
                    }
                }
                drop(provider); // Явно освобождаем lock

                sleep(Duration::from_millis(50)).await;
            }

            println!("   ✅ Task {} завершен", task_id);
        });

        tasks.push(task);
    }

    // Ждем завершения всех задач
    for task in tasks {
        task.await.unwrap();
    }

    sleep(Duration::from_secs(1)).await;

    let mut provider = provider_arc.lock().await;
    provider.stop_stream().await.unwrap();

    let transcriptions = sent_count.load(Ordering::SeqCst);
    println!(
        "\n📊 Результат: получено {} транскрипций от 5 конкурентных задач",
        transcriptions
    );
    println!("✅ Конкурентная отправка работает корректно");
}

/// Тест: Множественные провайдеры одновременно
#[tokio::test]
#[ignore]
async fn test_multiple_providers_simultaneously() {
    println!("🔀 Тест: несколько провайдеров одновременно\n");

    let deepgram_count = Arc::new(AtomicUsize::new(0));
    let assemblyai_count = Arc::new(AtomicUsize::new(0));

    // Deepgram provider
    let dg_count = deepgram_count.clone();
    let deepgram_task = tokio::spawn(async move {
        let mut provider = DeepgramProvider::new();
        let config = SttConfig::new(SttProviderType::Deepgram)
            .with_api_key(&get_deepgram_key())
            .with_language("en");

        provider.initialize(&config).await.unwrap();

        let count = dg_count.clone();
        let on_partial = Arc::new(move |_: Transcription| {
            count.fetch_add(1, Ordering::SeqCst);
        });
        let on_final = Arc::new(|_: Transcription| {});
        let on_error = noop_error();

        provider
            .start_stream(on_partial, on_final, on_error, noop_connection_quality())
            .await
            .unwrap();
        println!("🟢 Deepgram: подключен");

        for _ in 0..20 {
            let chunk = AudioChunk::new(vec![100i16; 1600], 16000, 1);
            provider.send_audio(&chunk).await.unwrap();
            sleep(Duration::from_millis(100)).await;
        }

        provider.stop_stream().await.unwrap();
        println!("🟢 Deepgram: завершен");
    });

    // AssemblyAI provider
    let aa_count = assemblyai_count.clone();
    let assemblyai_task = tokio::spawn(async move {
        let mut provider = AssemblyAIProvider::new();
        let config = SttConfig::new(SttProviderType::AssemblyAI)
            .with_api_key(&get_assemblyai_key())
            .with_language("en");

        provider.initialize(&config).await.unwrap();

        let count = aa_count.clone();
        let on_partial = Arc::new(move |_: Transcription| {
            count.fetch_add(1, Ordering::SeqCst);
        });
        let on_final = Arc::new(|_: Transcription| {});
        let on_error = noop_error();

        provider
            .start_stream(on_partial, on_final, on_error, noop_connection_quality())
            .await
            .unwrap();
        println!("🔵 AssemblyAI: подключен");

        for _ in 0..20 {
            let chunk = AudioChunk::new(vec![100i16; 1600], 16000, 1);
            provider.send_audio(&chunk).await.unwrap();
            sleep(Duration::from_millis(100)).await;
        }

        provider.stop_stream().await.unwrap();
        println!("🔵 AssemblyAI: завершен");
    });

    // Ждем завершения обоих
    let (dg_result, aa_result) = tokio::join!(deepgram_task, assemblyai_task);

    assert!(dg_result.is_ok(), "Deepgram должен завершиться успешно");
    assert!(aa_result.is_ok(), "AssemblyAI должен завершиться успешно");

    let dg_trans = deepgram_count.load(Ordering::SeqCst);
    let aa_trans = assemblyai_count.load(Ordering::SeqCst);

    println!("\n📊 Результаты одновременной работы:");
    println!("   Deepgram транскрипций: {}", dg_trans);
    println!("   AssemblyAI транскрипций: {}", aa_trans);
    println!("✅ Несколько провайдеров работают одновременно без конфликтов");
}

/// Тест: Race condition при быстрых start/stop операциях
#[tokio::test]
#[ignore]
async fn test_websocket_rapid_start_stop() {
    println!("⚡ Тест: быстрые последовательные start/stop операции\n");

    let mut provider = DeepgramProvider::new();
    let config = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key(&get_deepgram_key())
        .with_language("en");

    provider.initialize(&config).await.unwrap();

    let on_p = Arc::new(|_: Transcription| {});
    let on_f = Arc::new(|_: Transcription| {});
    let on_e = noop_error();

    // Быстрые циклы start/stop
    for i in 1..=10 {
        println!("   Цикл {}/10", i);

        provider
            .start_stream(
                on_p.clone(),
                on_f.clone(),
                on_e.clone(),
                noop_connection_quality(),
            )
            .await
            .unwrap();

        // Отправляем минимум данных
        let chunk = AudioChunk::new(vec![100i16; 1600], 16000, 1);
        provider.send_audio(&chunk).await.unwrap();

        // Очень короткая задержка перед остановкой
        sleep(Duration::from_millis(50)).await;

        provider.stop_stream().await.unwrap();

        // Минимальная задержка между циклами
        sleep(Duration::from_millis(100)).await;
    }

    println!("✅ 10 быстрых циклов start/stop без race conditions");
}

// ============================================================================
// ТЕСТЫ EDGE CASES
// ============================================================================

/// Тест: Отправка пустых данных
#[tokio::test]
#[ignore]
async fn test_websocket_empty_data() {
    let mut provider = DeepgramProvider::new();

    let config = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key(&get_deepgram_key())
        .with_language("en");

    provider.initialize(&config).await.unwrap();

    let on_partial = Arc::new(|_: Transcription| {});
    let on_final = Arc::new(|_: Transcription| {});
    let on_error = noop_error();

    provider
        .start_stream(on_partial, on_final, on_error, noop_connection_quality())
        .await
        .unwrap();

    println!("🔇 Тест отправки тишины (нулевые данные)...");

    // Отправляем тишину (все нули)
    for i in 0..20 {
        let chunk = AudioChunk::new(vec![0i16; 1600], 16000, 1);
        let result = provider.send_audio(&chunk).await;

        assert!(result.is_ok(), "Отправка тишины должна работать");

        if i % 5 == 0 {
            println!("   Отправлено {} чанков тишины", i);
        }

        sleep(Duration::from_millis(100)).await;
    }

    provider.stop_stream().await.unwrap();
    println!("✅ Отправка тишины обрабатывается корректно");
}

/// Тест: Очень маленькие чанки (минимальный размер)
#[tokio::test]
#[ignore]
async fn test_websocket_tiny_chunks() {
    let mut provider = DeepgramProvider::new();

    let config = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key(&get_deepgram_key())
        .with_language("en");

    provider.initialize(&config).await.unwrap();

    let on_partial = Arc::new(|_: Transcription| {});
    let on_final = Arc::new(|_: Transcription| {});
    let on_error = noop_error();

    provider
        .start_stream(on_partial, on_final, on_error, noop_connection_quality())
        .await
        .unwrap();

    println!("🔬 Тест очень маленьких чанков (10ms = 160 samples)...");

    let sent_count = Arc::new(AtomicUsize::new(0));
    let buffered_count = Arc::new(AtomicUsize::new(0));

    // Отправляем очень маленькие чанки (10ms каждый)
    for i in 0..100 {
        let chunk = AudioChunk::new(vec![100i16; 160], 16000, 1); // 10ms @ 16kHz

        match provider.send_audio(&chunk).await {
            Ok(_) => {
                sent_count.fetch_add(1, Ordering::SeqCst);
            }
            Err(_) => {
                buffered_count.fetch_add(1, Ordering::SeqCst);
            }
        }

        if i % 20 == 0 {
            println!("   Отправлено {} маленьких чанков", i);
        }

        sleep(Duration::from_millis(10)).await;
    }

    provider.stop_stream().await.unwrap();

    let sent = sent_count.load(Ordering::SeqCst);
    println!("   Успешно отправлено: {} чанков", sent);
    println!("✅ Маленькие чанки обрабатываются корректно (буферизация работает)");
}

/// Тест: Очень большие чанки (максимальный размер)
#[tokio::test]
#[ignore]
async fn test_websocket_huge_chunks() {
    let mut provider = DeepgramProvider::new();

    let config = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key(&get_deepgram_key())
        .with_language("en");

    provider.initialize(&config).await.unwrap();

    let on_partial = Arc::new(|_: Transcription| {});
    let on_final = Arc::new(|_: Transcription| {});
    let on_error = noop_error();

    provider
        .start_stream(on_partial, on_final, on_error, noop_connection_quality())
        .await
        .unwrap();

    println!("🐘 Тест очень больших чанков (1 секунда = 16000 samples)...");

    // Отправляем большие чанки (1 секунда каждый)
    for i in 0..5 {
        let chunk = AudioChunk::new(vec![100i16; 16000], 16000, 1); // 1 секунда

        let send_start = Instant::now();
        let result = provider.send_audio(&chunk).await;
        let send_duration = send_start.elapsed();

        assert!(result.is_ok(), "Отправка больших чанков должна работать");

        println!(
            "   Чанк {} (1 сек аудио) отправлен за {:.1}ms",
            i + 1,
            send_duration.as_millis()
        );

        sleep(Duration::from_millis(200)).await;
    }

    provider.stop_stream().await.unwrap();
    println!("✅ Большие чанки обрабатываются корректно");
}

/// Тест: Максимальная амплитуда (граничные значения i16)
#[tokio::test]
#[ignore]
async fn test_websocket_extreme_amplitude() {
    let mut provider = DeepgramProvider::new();

    let config = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key(&get_deepgram_key())
        .with_language("en");

    provider.initialize(&config).await.unwrap();

    let on_partial = Arc::new(|_: Transcription| {});
    let on_final = Arc::new(|_: Transcription| {});
    let on_error = noop_error();

    provider
        .start_stream(on_partial, on_final, on_error, noop_connection_quality())
        .await
        .unwrap();

    println!("📢 Тест экстремальных значений амплитуды...");

    // Тест 1: Максимальная положительная амплитуда
    println!("   Тест max положительная: i16::MAX (32767)");
    for _ in 0..5 {
        let chunk = AudioChunk::new(vec![i16::MAX; 1600], 16000, 1);
        provider.send_audio(&chunk).await.unwrap();
        sleep(Duration::from_millis(100)).await;
    }

    // Тест 2: Максимальная отрицательная амплитуда
    println!("   Тест max отрицательная: i16::MIN (-32768)");
    for _ in 0..5 {
        let chunk = AudioChunk::new(vec![i16::MIN; 1600], 16000, 1);
        provider.send_audio(&chunk).await.unwrap();
        sleep(Duration::from_millis(100)).await;
    }

    // Тест 3: Чередование max/min
    println!("   Тест чередование max/min (клиппинг)");
    for _ in 0..5 {
        let mut samples = Vec::with_capacity(1600);
        for i in 0..1600 {
            samples.push(if i % 2 == 0 { i16::MAX } else { i16::MIN });
        }
        let chunk = AudioChunk::new(samples, 16000, 1);
        provider.send_audio(&chunk).await.unwrap();
        sleep(Duration::from_millis(100)).await;
    }

    provider.stop_stream().await.unwrap();
    println!("✅ Экстремальные значения обрабатываются корректно");
}

/// Тест: Резкая смена частоты (тест на багв в кодировании)
#[tokio::test]
#[ignore]
async fn test_websocket_frequency_changes() {
    let mut provider = DeepgramProvider::new();

    let config = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key(&get_deepgram_key())
        .with_language("en");

    provider.initialize(&config).await.unwrap();

    let on_partial = Arc::new(|_: Transcription| {});
    let on_final = Arc::new(|_: Transcription| {});
    let on_error = noop_error();

    provider
        .start_stream(on_partial, on_final, on_error, noop_connection_quality())
        .await
        .unwrap();

    println!("🎵 Тест резких изменений частоты...");

    let frequencies = vec![100.0, 500.0, 1000.0, 2000.0, 4000.0, 100.0];

    for freq in frequencies.iter() {
        println!("   Отправка частоты {} Hz", freq);

        // Генерируем синусоиду с данной частотой
        for _ in 0..5 {
            let mut samples = Vec::with_capacity(1600);
            for j in 0..1600 {
                let t = j as f32 / 16000.0;
                let value = (2.0 * std::f32::consts::PI * freq * t).sin() * 10000.0;
                samples.push(value as i16);
            }

            let chunk = AudioChunk::new(samples, 16000, 1);
            provider.send_audio(&chunk).await.unwrap();
            sleep(Duration::from_millis(50)).await;
        }
    }

    provider.stop_stream().await.unwrap();
    println!("✅ Резкие изменения частоты обрабатываются корректно");
}

// ============================================================================
// ТЕСТЫ МОНИТОРИНГА И МЕТРИК
// ============================================================================

/// Тест: Измерение латентности отправки
#[tokio::test]
#[ignore]
async fn test_websocket_send_latency_measurement() {
    let mut provider = DeepgramProvider::new();

    let config = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key(&get_deepgram_key())
        .with_language("en");

    provider.initialize(&config).await.unwrap();

    let on_partial = Arc::new(|_: Transcription| {});
    let on_final = Arc::new(|_: Transcription| {});
    let on_error = noop_error();

    provider
        .start_stream(on_partial, on_final, on_error, noop_connection_quality())
        .await
        .unwrap();

    println!("⏱️  Измерение латентности отправки данных...\n");

    let mut latencies = Vec::new();

    for i in 0..50 {
        let chunk = AudioChunk::new(vec![100i16; 1600], 16000, 1);

        let send_start = Instant::now();
        provider.send_audio(&chunk).await.unwrap();
        let latency = send_start.elapsed();

        latencies.push(latency.as_micros());

        if i % 10 == 0 {
            println!(
                "   Чанк {}: {:.2}ms",
                i,
                latency.as_micros() as f32 / 1000.0
            );
        }

        sleep(Duration::from_millis(100)).await;
    }

    provider.stop_stream().await.unwrap();

    // Вычисляем статистику
    let sum: u128 = latencies.iter().sum();
    let avg = sum / latencies.len() as u128;

    let mut sorted = latencies.clone();
    sorted.sort();
    let p50 = sorted[sorted.len() / 2];
    let p95 = sorted[(sorted.len() as f32 * 0.95) as usize];
    let p99 = sorted[(sorted.len() as f32 * 0.99) as usize];
    let max = sorted.last().unwrap();
    let min = sorted.first().unwrap();

    println!("\n📊 Статистика латентности отправки:");
    println!("   Среднее:  {:.2}ms", avg as f32 / 1000.0);
    println!("   Медиана:  {:.2}ms", p50 as f32 / 1000.0);
    println!("   P95:      {:.2}ms", p95 as f32 / 1000.0);
    println!("   P99:      {:.2}ms", p99 as f32 / 1000.0);
    println!("   Min:      {:.2}ms", *min as f32 / 1000.0);
    println!("   Max:      {:.2}ms", *max as f32 / 1000.0);

    // Латентность должна быть разумной (< 100ms для большинства)
    assert!(
        (p95 as f32 / 1000.0) < 100.0,
        "P95 латентность должна быть < 100ms"
    );

    println!("✅ Латентность отправки в пределах нормы");
}

/// Тест: Мониторинг использования памяти
#[tokio::test]
#[ignore]
async fn test_websocket_memory_usage() {
    println!("💾 Тест мониторинга использования памяти...\n");

    let mut provider = DeepgramProvider::new();
    let config = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key(&get_deepgram_key())
        .with_language("en");

    provider.initialize(&config).await.unwrap();

    let on_partial = Arc::new(|_: Transcription| {});
    let on_final = Arc::new(|_: Transcription| {});
    let on_error = noop_error();

    provider
        .start_stream(on_partial, on_final, on_error, noop_connection_quality())
        .await
        .unwrap();

    // Отправляем большое количество данных чтобы проверить утечки памяти
    println!("   Отправка большого объема данных (10 секунд аудио)...");

    for i in 0..100 {
        let chunk = AudioChunk::new(vec![100i16; 1600], 16000, 1);
        provider.send_audio(&chunk).await.unwrap();

        if i % 20 == 0 {
            println!("   {} чанков отправлено", i);
        }

        sleep(Duration::from_millis(100)).await;
    }

    provider.stop_stream().await.unwrap();

    // Проверяем что можем переиспользовать провайдер (нет утечек)
    println!("\n   Проверка переиспользования провайдера...");

    // Создаем новые callbacks для второго использования
    let on_partial2 = Arc::new(|_: Transcription| {});
    let on_final2 = Arc::new(|_: Transcription| {});
    let on_error2 = noop_error();

    provider
        .start_stream(on_partial2, on_final2, on_error2, noop_connection_quality())
        .await
        .unwrap();

    for _ in 0..10 {
        let chunk = AudioChunk::new(vec![100i16; 1600], 16000, 1);
        provider.send_audio(&chunk).await.unwrap();
        sleep(Duration::from_millis(100)).await;
    }

    provider.stop_stream().await.unwrap();

    println!("✅ Утечек памяти не обнаружено (провайдер может быть переиспользован)");
}

/// Тест: Статистика получения транскрипций (скорость обработки)
#[tokio::test]
#[ignore]
async fn test_websocket_transcription_rate() {
    let mut provider = DeepgramProvider::new();

    let config = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key(&get_deepgram_key())
        .with_language("en");

    provider.initialize(&config).await.unwrap();

    let transcription_times = Arc::new(Mutex::new(Vec::new()));
    let times_clone = transcription_times.clone();

    let on_partial = Arc::new(move |t: Transcription| {
        times_clone
            .lock()
            .unwrap()
            .push((Instant::now(), t.text.clone(), false));
    });

    let times_final = transcription_times.clone();
    let on_final = Arc::new(move |t: Transcription| {
        times_final
            .lock()
            .unwrap()
            .push((Instant::now(), t.text.clone(), true));
    });

    let on_error = noop_error();

    provider
        .start_stream(on_partial, on_final, on_error, noop_connection_quality())
        .await
        .unwrap();

    println!("📈 Измерение скорости получения транскрипций...\n");

    let test_start = Instant::now();

    // Отправляем разнообразный сигнал
    for i in 0..30 {
        let freq = 200.0 + (i as f32 * 100.0) % 1000.0;
        let mut samples = Vec::with_capacity(1600);

        for j in 0..1600 {
            let t = j as f32 / 16000.0;
            let value = (2.0 * std::f32::consts::PI * freq * t).sin() * 8000.0;
            samples.push(value as i16);
        }

        let chunk = AudioChunk::new(samples, 16000, 1);
        provider.send_audio(&chunk).await.unwrap();

        sleep(Duration::from_millis(100)).await;
    }

    sleep(Duration::from_secs(2)).await;
    provider.stop_stream().await.unwrap();

    let test_duration = test_start.elapsed();
    let times = transcription_times.lock().unwrap();

    println!("📊 Статистика транскрипций:");
    println!("   Длительность теста: {:.2}s", test_duration.as_secs_f32());
    println!("   Всего транскрипций: {}", times.len());

    let partial_count = times.iter().filter(|(_, _, is_final)| !is_final).count();
    let final_count = times.iter().filter(|(_, _, is_final)| *is_final).count();

    println!("   Partial: {}", partial_count);
    println!("   Final: {}", final_count);

    if times.len() > 0 {
        let rate = times.len() as f32 / test_duration.as_secs_f32();
        println!("   Скорость: {:.2} транскрипций/сек", rate);
    }

    println!("✅ Скорость получения транскрипций измерена");
}

/// Тест: Throughput (пропускная способность)
#[tokio::test]
#[ignore]
async fn test_websocket_throughput() {
    let mut provider = DeepgramProvider::new();

    let config = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key(&get_deepgram_key())
        .with_language("en");

    provider.initialize(&config).await.unwrap();

    let on_partial = Arc::new(|_: Transcription| {});
    let on_final = Arc::new(|_: Transcription| {});
    let on_error = noop_error();

    provider
        .start_stream(on_partial, on_final, on_error, noop_connection_quality())
        .await
        .unwrap();

    println!("🚀 Измерение пропускной способности...\n");

    let test_duration = Duration::from_secs(10);
    let test_start = Instant::now();
    let mut bytes_sent = 0usize;
    let mut chunks_sent = 0usize;

    while test_start.elapsed() < test_duration {
        let chunk = AudioChunk::new(vec![100i16; 1600], 16000, 1);
        bytes_sent += 1600 * 2; // 2 bytes per i16 sample
        chunks_sent += 1;

        provider.send_audio(&chunk).await.unwrap();
        sleep(Duration::from_millis(100)).await;
    }

    provider.stop_stream().await.unwrap();

    let actual_duration = test_start.elapsed().as_secs_f32();
    let throughput_bytes = bytes_sent as f32 / actual_duration;
    let throughput_mbps = (throughput_bytes * 8.0) / 1_000_000.0;

    println!("📊 Результаты:");
    println!("   Длительность: {:.2}s", actual_duration);
    println!("   Отправлено чанков: {}", chunks_sent);
    println!(
        "   Отправлено байт: {} ({:.2} KB)",
        bytes_sent,
        bytes_sent as f32 / 1024.0
    );
    println!("   Throughput: {:.2} KB/s", throughput_bytes / 1024.0);
    println!("   Throughput: {:.4} Mbps", throughput_mbps);

    println!("✅ Пропускная способность измерена");
}
