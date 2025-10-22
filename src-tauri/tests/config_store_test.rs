use app_lib::infrastructure::ConfigStore;
use app_lib::domain::{SttConfig, SttProviderType, AppConfig};
use std::fs;
use std::path::PathBuf;
use serial_test::serial;

/// Хелпер для получения временной директории для тестов
fn get_test_config_dir() -> PathBuf {
    let temp_dir = std::env::temp_dir();
    temp_dir.join(format!("voice-to-text-test-{}", std::process::id()))
}

/// Очистка тестовой директории после теста
fn cleanup_test_dir(dir: &PathBuf) {
    if dir.exists() {
        let _ = fs::remove_dir_all(dir);
    }
}

#[tokio::test]
#[serial]
async fn test_save_and_load_stt_config() {
    // Очистка перед тестом
    let _ = ConfigStore::delete_config().await;
    let _ = ConfigStore::delete_app_config().await;

    let test_dir = get_test_config_dir();

    // Создаем тестовую конфигурацию
    let config = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key("test-api-key-123")
        .with_language("ru")
        .with_model("nova-3");

    // Сохраняем
    let save_result = ConfigStore::save_config(&config).await;
    assert!(save_result.is_ok(), "Сохранение должно пройти успешно");

    // Загружаем обратно
    let loaded = ConfigStore::load_config().await;
    assert!(loaded.is_ok(), "Загрузка должна пройти успешно");

    let loaded_config = loaded.unwrap();
    assert_eq!(loaded_config.provider, config.provider);
    assert_eq!(loaded_config.api_key, config.api_key);
    assert_eq!(loaded_config.language, config.language);
    assert_eq!(loaded_config.model, config.model);

    // Очистка
    let _ = ConfigStore::delete_config().await;
    cleanup_test_dir(&test_dir);
}

#[tokio::test]
#[serial]
async fn test_load_config_when_not_exists() {
    // Удаляем конфиг если существует
    let _ = ConfigStore::delete_config().await;

    // Пробуем загрузить - должен вернуть дефолтный
    let result = ConfigStore::load_config().await;
    assert!(result.is_ok(), "Должен вернуть дефолтную конфигурацию");

    let config = result.unwrap();
    assert_eq!(config.provider, SttConfig::default().provider);
}

#[tokio::test]
#[serial]
async fn test_delete_stt_config() {
    // Создаем и сохраняем конфиг
    let config = SttConfig::new(SttProviderType::AssemblyAI)
        .with_api_key("delete-test-key");

    ConfigStore::save_config(&config).await.unwrap();

    // Удаляем
    let delete_result = ConfigStore::delete_config().await;
    assert!(delete_result.is_ok(), "Удаление должно пройти успешно");

    // Повторное удаление должно быть безопасным
    let delete_again = ConfigStore::delete_config().await;
    assert!(delete_again.is_ok(), "Повторное удаление должно быть безопасным");
}

#[tokio::test]
#[serial]
async fn test_save_and_load_app_config() {
    // Очистка перед тестом
    let _ = ConfigStore::delete_config().await;
    let _ = ConfigStore::delete_app_config().await;

    let test_dir = get_test_config_dir();

    // Создаем конфигурацию приложения
    let mut app_config = AppConfig::default();
    app_config.auto_copy_to_clipboard = true;
    app_config.auto_close_window = false;
    app_config.vad_silence_timeout_ms = 2500;

    // Сохраняем
    let save_result = ConfigStore::save_app_config(&app_config).await;
    assert!(save_result.is_ok(), "Сохранение app config должно пройти успешно");

    // Загружаем обратно
    let loaded = ConfigStore::load_app_config().await;
    assert!(loaded.is_ok(), "Загрузка app config должна пройти успешно");

    let loaded_config = loaded.unwrap();
    assert_eq!(loaded_config.auto_copy_to_clipboard, app_config.auto_copy_to_clipboard);
    assert_eq!(loaded_config.auto_close_window, app_config.auto_close_window);
    assert_eq!(loaded_config.vad_silence_timeout_ms, app_config.vad_silence_timeout_ms);

    // Очистка
    let _ = ConfigStore::delete_app_config().await;
    cleanup_test_dir(&test_dir);
}

#[tokio::test]
#[serial]
async fn test_load_app_config_when_not_exists() {
    // Удаляем конфиг если существует
    let _ = ConfigStore::delete_app_config().await;

    // Пробуем загрузить - должен вернуть дефолтный
    let result = ConfigStore::load_app_config().await;
    assert!(result.is_ok(), "Должен вернуть дефолтную app конфигурацию");

    let config = result.unwrap();
    let default_config = AppConfig::default();
    assert_eq!(config.auto_copy_to_clipboard, default_config.auto_copy_to_clipboard);
    assert_eq!(config.vad_silence_timeout_ms, default_config.vad_silence_timeout_ms);
}

#[tokio::test]
#[serial]
async fn test_delete_app_config() {
    // Создаем и сохраняем app конфиг
    let app_config = AppConfig::default();
    ConfigStore::save_app_config(&app_config).await.unwrap();

    // Удаляем
    let delete_result = ConfigStore::delete_app_config().await;
    assert!(delete_result.is_ok(), "Удаление app config должно пройти успешно");

    // Повторное удаление должно быть безопасным
    let delete_again = ConfigStore::delete_app_config().await;
    assert!(delete_again.is_ok(), "Повторное удаление app config должно быть безопасным");
}

#[tokio::test]
#[serial]
async fn test_save_multiple_configs_sequentially() {
    // Очистка перед тестом
    let _ = ConfigStore::delete_config().await;

    // Тест на последовательное сохранение разных конфигов
    let config1 = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key("key-1");

    let config2 = SttConfig::new(SttProviderType::AssemblyAI)
        .with_api_key("key-2");

    // Сохраняем первый
    ConfigStore::save_config(&config1).await.unwrap();
    let loaded1 = ConfigStore::load_config().await.unwrap();
    assert_eq!(loaded1.provider, SttProviderType::Deepgram);

    // Перезаписываем вторым
    ConfigStore::save_config(&config2).await.unwrap();
    let loaded2 = ConfigStore::load_config().await.unwrap();
    assert_eq!(loaded2.provider, SttProviderType::AssemblyAI);
    assert_eq!(loaded2.api_key, Some("key-2".to_string()));

    // Очистка
    ConfigStore::delete_config().await.unwrap();
}

#[tokio::test]
#[serial]
async fn test_config_persistence_across_operations() {
    // Очистка перед тестом
    let _ = ConfigStore::delete_config().await;
    let _ = ConfigStore::delete_app_config().await;

    // Проверяем что конфиг сохраняется между операциями
    let original = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key("persistent-key")
        .with_language("en")
        .with_model("nova-2");

    ConfigStore::save_config(&original).await.unwrap();

    // Загружаем несколько раз - должно быть одно и то же
    let loaded1 = ConfigStore::load_config().await.unwrap();
    let loaded2 = ConfigStore::load_config().await.unwrap();
    let loaded3 = ConfigStore::load_config().await.unwrap();

    assert_eq!(loaded1.api_key, loaded2.api_key);
    assert_eq!(loaded2.api_key, loaded3.api_key);
    assert_eq!(loaded1.language, "en");
    assert_eq!(loaded2.model, Some("nova-2".to_string()));

    // Очистка
    ConfigStore::delete_config().await.unwrap();
}

#[tokio::test]
#[serial]
async fn test_config_with_empty_optional_fields() {
    // Очистка перед тестом
    let _ = ConfigStore::delete_config().await;
    let _ = ConfigStore::delete_app_config().await;

    // Тест с минимальной конфигурацией (без опциональных полей)
    let mut config = SttConfig::default();
    config.api_key = None;
    config.model = None;

    ConfigStore::save_config(&config).await.unwrap();
    let loaded = ConfigStore::load_config().await.unwrap();

    assert_eq!(loaded.api_key, None);
    assert_eq!(loaded.model, None);

    ConfigStore::delete_config().await.unwrap();
}

#[tokio::test]
#[serial]
async fn test_concurrent_config_operations() {
    // Очистка перед тестом
    let _ = ConfigStore::delete_config().await;
    let _ = ConfigStore::delete_app_config().await;

    // Тест параллельных операций чтения
    let config = SttConfig::new(SttProviderType::Deepgram)
        .with_api_key("concurrent-test");

    ConfigStore::save_config(&config).await.unwrap();

    // Запускаем несколько параллельных чтений
    let handles: Vec<_> = (0..5)
        .map(|_| {
            tokio::spawn(async {
                ConfigStore::load_config().await
            })
        })
        .collect();

    // Все операции должны завершиться успешно
    for handle in handles {
        let result = handle.await.unwrap();
        assert!(result.is_ok());
    }

    ConfigStore::delete_config().await.unwrap();
}
