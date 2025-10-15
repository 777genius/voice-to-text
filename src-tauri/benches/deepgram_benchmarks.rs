use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId};
use std::sync::Arc;
use tokio::runtime::Runtime;

use app_lib::domain::{AudioChunk, SttConfig, SttProvider, SttProviderType, Transcription};
use app_lib::infrastructure::stt::DeepgramProvider;

const TEST_API_KEY: &str = "c73950ca938f2b20e33eade8c6dedd7d325033cc";

/// Бенчмарк инициализации провайдера
fn bench_initialization(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    c.bench_function("deepgram_initialization", |b| {
        b.iter(|| {
            let mut provider = DeepgramProvider::new();
            let mut config = SttConfig::new(SttProviderType::Deepgram)
                .with_api_key(TEST_API_KEY)
                .with_language("ru");

            rt.block_on(async {
                provider.initialize(&config).await.unwrap();
            });

            black_box(provider);
        });
    });
}

/// Бенчмарк audio encoding (конвертация i16 семплов в байты)
fn bench_audio_encoding(c: &mut Criterion) {
    let mut group = c.benchmark_group("audio_encoding");

    for size in [160, 480, 1600, 4800, 16000].iter() {
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &size| {
            let samples = vec![1000i16; size];
            b.iter(|| {
                let bytes: Vec<u8> = samples
                    .iter()
                    .flat_map(|&sample| sample.to_le_bytes())
                    .collect();
                black_box(bytes);
            });
        });
    }

    group.finish();
}

/// Бенчмарк создания AudioChunk
fn bench_audio_chunk_creation(c: &mut Criterion) {
    let mut group = c.benchmark_group("audio_chunk_creation");

    for size in [480, 1600, 4800].iter() {
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &size| {
            let samples = vec![100i16; size];
            b.iter(|| {
                let chunk = AudioChunk::new(samples.clone(), 16000, 1);
                black_box(chunk);
            });
        });
    }

    group.finish();
}

/// Бенчмарк вычисления длительности аудио чанка
fn bench_audio_duration_calculation(c: &mut Criterion) {
    let chunk = AudioChunk::new(vec![100i16; 1600], 16000, 1);

    c.bench_function("audio_duration_ms", |b| {
        b.iter(|| {
            let duration = chunk.duration_ms();
            black_box(duration);
        });
    });
}

/// Бенчмарк to_bytes метода AudioChunk
fn bench_audio_chunk_to_bytes(c: &mut Criterion) {
    let mut group = c.benchmark_group("audio_chunk_to_bytes");

    for size in [480, 1600, 4800, 16000].iter() {
        let chunk = AudioChunk::new(vec![100i16; *size], 16000, 1);

        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, _| {
            b.iter(|| {
                let bytes = chunk.to_bytes();
                black_box(bytes);
            });
        });
    }

    group.finish();
}

/// Бенчмарк from_bytes метода AudioChunk
fn bench_audio_chunk_from_bytes(c: &mut Criterion) {
    let mut group = c.benchmark_group("audio_chunk_from_bytes");

    for size in [480, 1600, 4800, 16000].iter() {
        let samples = vec![100i16; *size];
        let bytes: Vec<u8> = samples
            .iter()
            .flat_map(|&sample| sample.to_le_bytes())
            .collect();

        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, _| {
            b.iter(|| {
                let chunk = AudioChunk::from_bytes(&bytes, 16000, 1);
                black_box(chunk);
            });
        });
    }

    group.finish();
}

/// Бенчмарк генерации синтетического голоса (синусоида)
fn bench_synthetic_audio_generation(c: &mut Criterion) {
    c.bench_function("synthetic_audio_1s", |b| {
        b.iter(|| {
            let sample_rate = 16000;
            let mut samples = Vec::with_capacity(sample_rate);

            for i in 0..sample_rate {
                let t = i as f32 / sample_rate as f32;
                let val = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 10000.0;
                samples.push(val as i16);
            }

            black_box(samples);
        });
    });
}

/// Бенчмарк многочастотного синтетического голоса
fn bench_multi_frequency_audio_generation(c: &mut Criterion) {
    c.bench_function("multi_freq_audio_1s", |b| {
        b.iter(|| {
            let sample_rate = 16000;
            let mut samples = Vec::with_capacity(sample_rate);

            for i in 0..sample_rate {
                let t = i as f32 / sample_rate as f32;
                let val = (2.0 * std::f32::consts::PI * 300.0 * t).sin() * 3000.0
                        + (2.0 * std::f32::consts::PI * 600.0 * t).sin() * 2000.0
                        + (2.0 * std::f32::consts::PI * 1200.0 * t).sin() * 1000.0;
                samples.push(val as i16);
            }

            black_box(samples);
        });
    });
}

/// Бенчмарк callback вызовов
fn bench_callback_invocation(c: &mut Criterion) {
    let call_count = Arc::new(std::sync::Mutex::new(0));
    let call_count_clone = call_count.clone();

    let callback = Arc::new(move |transcription: Transcription| {
        *call_count_clone.lock().unwrap() += 1;
        black_box(transcription);
    });

    let transcription = Transcription {
        text: "Тестовый текст для бенчмарка".to_string(),
        confidence: Some(0.95),
        is_final: false,
        language: Some("ru".to_string()),
        timestamp: 0,
    };

    c.bench_function("callback_invocation", |b| {
        b.iter(|| {
            callback(transcription.clone());
        });
    });
}

/// Бенчмарк создания конфигурации
fn bench_config_creation(c: &mut Criterion) {
    c.bench_function("stt_config_creation", |b| {
        b.iter(|| {
            let config = SttConfig::new(SttProviderType::Deepgram)
                .with_api_key(TEST_API_KEY)
                .with_language("ru")
                .with_model("nova-3");
            black_box(config);
        });
    });
}

/// Бенчмарк клонирования конфигурации
fn bench_config_cloning(c: &mut Criterion) {
    let config = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key(TEST_API_KEY)
        .with_language("ru");

    c.bench_function("stt_config_clone", |b| {
        b.iter(|| {
            let cloned = config.clone();
            black_box(cloned);
        });
    });
}

/// Бенчмарк создания провайдера через Factory
fn bench_factory_provider_creation(c: &mut Criterion) {
    use app_lib::infrastructure::factory::DefaultSttProviderFactory;
    use app_lib::domain::SttProviderFactory;

    let factory = DefaultSttProviderFactory::new();
    let config = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key(TEST_API_KEY);

    c.bench_function("factory_create_provider", |b| {
        b.iter(|| {
            let provider = factory.create(&config).unwrap();
            black_box(provider);
        });
    });
}

/// Бенчмарк сериализации транскрипции в JSON
fn bench_transcription_serialization(c: &mut Criterion) {
    let transcription = Transcription {
        text: "Привет, это тестовая транскрипция для проверки производительности сериализации".to_string(),
        confidence: Some(0.95),
        is_final: true,
        language: Some("ru".to_string()),
        timestamp: 1234567890,
    };

    c.bench_function("transcription_to_json", |b| {
        b.iter(|| {
            let json = serde_json::to_string(&transcription).unwrap();
            black_box(json);
        });
    });
}

/// Бенчмарк десериализации транскрипции из JSON
fn bench_transcription_deserialization(c: &mut Criterion) {
    let json = r#"{"text":"Привет, это тестовая транскрипция","confidence":0.95,"is_final":true,"language":"ru","timestamp":1234567890}"#;

    c.bench_function("transcription_from_json", |b| {
        b.iter(|| {
            let transcription: Transcription = serde_json::from_str(json).unwrap();
            black_box(transcription);
        });
    });
}

/// Бенчмарк обработки большого количества чанков
fn bench_multiple_chunks_processing(c: &mut Criterion) {
    let mut group = c.benchmark_group("multiple_chunks");

    for chunk_count in [10, 50, 100, 500].iter() {
        group.bench_with_input(
            BenchmarkId::from_parameter(chunk_count),
            chunk_count,
            |b, &count| {
                b.iter(|| {
                    let mut chunks = Vec::new();
                    for _ in 0..count {
                        let samples = vec![100i16; 1600];
                        let chunk = AudioChunk::new(samples, 16000, 1);
                        chunks.push(chunk);
                    }
                    black_box(chunks);
                });
            }
        );
    }

    group.finish();
}

/// Бенчмарк конвертации большого количества байтов в семплы
fn bench_bytes_to_samples_conversion(c: &mut Criterion) {
    let mut group = c.benchmark_group("bytes_to_samples");

    for sample_count in [1600, 16000, 48000, 160000].iter() {
        let bytes: Vec<u8> = (0..*sample_count)
            .flat_map(|i| (i as i16).to_le_bytes())
            .collect();

        group.bench_with_input(
            BenchmarkId::from_parameter(sample_count),
            sample_count,
            |b, _| {
                b.iter(|| {
                    let samples: Vec<i16> = bytes
                        .chunks_exact(2)
                        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
                        .collect();
                    black_box(samples);
                });
            }
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_initialization,
    bench_audio_encoding,
    bench_audio_chunk_creation,
    bench_audio_duration_calculation,
    bench_audio_chunk_to_bytes,
    bench_audio_chunk_from_bytes,
    bench_synthetic_audio_generation,
    bench_multi_frequency_audio_generation,
    bench_callback_invocation,
    bench_config_creation,
    bench_config_cloning,
    bench_factory_provider_creation,
    bench_transcription_serialization,
    bench_transcription_deserialization,
    bench_multiple_chunks_processing,
    bench_bytes_to_samples_conversion,
);

criterion_main!(benches);
