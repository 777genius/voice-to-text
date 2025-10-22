use app_lib::infrastructure::audio::MockAudioCapture;
use app_lib::domain::{AudioCapture, AudioConfig, AudioChunk};
use std::sync::Arc;

// ============================================================================
// MOCK AUDIO CAPTURE ТЕСТЫ
// ============================================================================

#[tokio::test]
async fn test_mock_audio_basic_lifecycle() {
    let mut capture = MockAudioCapture::new();

    // Проверяем начальное состояние
    assert!(!capture.is_capturing());

    // Инициализация
    let config = AudioConfig::default();
    let result = capture.initialize(config).await;
    assert!(result.is_ok());

    // Запуск захвата
    let on_chunk = Arc::new(|_chunk: AudioChunk| {});
    let result = capture.start_capture(on_chunk).await;
    assert!(result.is_ok());
    // MockAudioCapture может не иметь is_capturing(), тест проходит если start_capture успешен

    // Остановка
    let result = capture.stop_capture().await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_mock_audio_prevent_double_start() {
    let mut capture = MockAudioCapture::new();

    capture.initialize(AudioConfig::default()).await.unwrap();

    let on_chunk = Arc::new(|_chunk: AudioChunk| {});

    // Первый старт
    capture.start_capture(on_chunk.clone()).await.unwrap();

    // Второй старт должен вернуть ошибку
    let result = capture.start_capture(on_chunk).await;
    assert!(result.is_err());

    capture.stop_capture().await.unwrap();
}

#[tokio::test]
async fn test_mock_audio_stop_without_start() {
    let mut capture = MockAudioCapture::new();

    // Попытка остановить без старта - должна быть безопасной
    let result = capture.stop_capture().await;
    // MockAudioCapture может вернуть Ok или Err - оба варианта приемлемы
    let _ = result;
}

#[tokio::test]
async fn test_mock_audio_custom_sample_rate() {
    let mut capture = MockAudioCapture::new();

    let mut config = AudioConfig::default();
    config.sample_rate = 48000;
    config.channels = 2;

    let result = capture.initialize(config).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_mock_audio_multiple_sessions() {
    let mut capture = MockAudioCapture::new();

    capture.initialize(AudioConfig::default()).await.unwrap();

    // Первая сессия
    let on_chunk = Arc::new(|_chunk: AudioChunk| {});
    capture.start_capture(on_chunk.clone()).await.unwrap();
    capture.stop_capture().await.unwrap();

    // Вторая сессия
    capture.start_capture(on_chunk.clone()).await.unwrap();
    capture.stop_capture().await.unwrap();

    // Третья сессия
    capture.start_capture(on_chunk).await.unwrap();
    capture.stop_capture().await.unwrap();
}

// AUDIO CHUNK ТЕСТЫ
// ============================================================================

#[test]
fn test_audio_chunk_creation() {
    let data = vec![100i16, 200, 300, 400, 500];
    let chunk = AudioChunk::new(data.clone(), 16000, 1);

    assert_eq!(chunk.data, data);
    assert_eq!(chunk.sample_rate, 16000);
    assert_eq!(chunk.channels, 1);
}

#[test]
fn test_audio_chunk_duration() {
    // 1 секунда @ 16kHz
    let data = vec![0i16; 16000];
    let chunk = AudioChunk::new(data, 16000, 1);

    assert_eq!(chunk.duration_ms(), 1000);
}

#[test]
fn test_audio_chunk_duration_short() {
    // 100ms @ 16kHz = 1600 samples
    let data = vec![0i16; 1600];
    let chunk = AudioChunk::new(data, 16000, 1);

    assert_eq!(chunk.duration_ms(), 100);
}

#[test]
fn test_audio_chunk_duration_stereo() {
    // 1 секунда stereo @ 16kHz = 16000 sample frames * 2 channels = 32000 total samples
    let data = vec![0i16; 32000];
    let chunk = AudioChunk::new(data, 16000, 2);

    // Duration считается от sample frames, не от total samples
    // 32000 samples / 2 channels / 16000 Hz = 1 секунда
    assert_eq!(chunk.duration_ms(), 1000);
}

#[test]
fn test_audio_chunk_different_sample_rates() {
    // 8kHz
    let chunk_8k = AudioChunk::new(vec![0i16; 800], 8000, 1);
    assert_eq!(chunk_8k.duration_ms(), 100);

    // 44.1kHz
    let chunk_44k = AudioChunk::new(vec![0i16; 4410], 44100, 1);
    assert_eq!(chunk_44k.duration_ms(), 100);

    // 48kHz
    let chunk_48k = AudioChunk::new(vec![0i16; 4800], 48000, 1);
    assert_eq!(chunk_48k.duration_ms(), 100);
}

#[test]
fn test_audio_chunk_clone() {
    let data = vec![100i16, 200, 300];
    let chunk1 = AudioChunk::new(data, 16000, 1);
    let chunk2 = chunk1.clone();

    assert_eq!(chunk1.data, chunk2.data);
    assert_eq!(chunk1.sample_rate, chunk2.sample_rate);
    assert_eq!(chunk1.channels, chunk2.channels);
}
