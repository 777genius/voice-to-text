use std::path::PathBuf;
use anyhow::Result;

use crate::domain::{SttConfig, AppConfig, UiPreferences};

/// Персистентное хранилище конфигурации STT
pub struct ConfigStore;

impl ConfigStore {
    /// Получить директорию конфигурации приложения
    fn config_dir() -> Result<PathBuf> {
        let config_dir = dirs::config_dir()
            .ok_or_else(|| anyhow::anyhow!("Failed to get config directory"))?;

        let app_config_dir = config_dir.join("voice-to-text");

        // Создаем директорию если не существует
        if !app_config_dir.exists() {
            std::fs::create_dir_all(&app_config_dir)?;
        }

        Ok(app_config_dir)
    }

    /// Получить путь к файлу конфигурации STT
    fn config_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("stt_config.json"))
    }

    /// Получить путь к файлу конфигурации приложения
    fn app_config_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("app_config.json"))
    }

    /// Сохранить конфигурацию STT
    pub async fn save_config(config: &SttConfig) -> Result<()> {
        let path = Self::config_path()?;

        let json = serde_json::to_string_pretty(config)?;
        tokio::fs::write(path, json).await?;

        log::info!("STT config saved to disk");
        Ok(())
    }

    /// Загрузить конфигурацию STT
    pub async fn load_config() -> Result<SttConfig> {
        let path = Self::config_path()?;

        if !path.exists() {
            log::info!("No saved config found, using defaults");
            return Ok(SttConfig::default());
        }

        let json = tokio::fs::read_to_string(path).await?;
        let config: SttConfig = serde_json::from_str(&json)?;

        log::info!("STT config loaded from disk");
        Ok(config)
    }

    /// Удалить сохраненную конфигурацию
    pub async fn delete_config() -> Result<()> {
        let path = Self::config_path()?;

        if path.exists() {
            tokio::fs::remove_file(path).await?;
            log::info!("STT config deleted");
        }

        Ok(())
    }

    /// Сохранить конфигурацию приложения
    pub async fn save_app_config(config: &AppConfig) -> Result<()> {
        let path = Self::app_config_path()?;

        let json = serde_json::to_string_pretty(config)?;
        tokio::fs::write(path, json).await?;

        log::info!("App config saved to disk");
        Ok(())
    }

    /// Загрузить конфигурацию приложения
    pub async fn load_app_config() -> Result<AppConfig> {
        let path = Self::app_config_path()?;

        if !path.exists() {
            log::info!("No saved app config found, using defaults");
            return Ok(AppConfig::default());
        }

        let json = tokio::fs::read_to_string(path).await?;
        let config: AppConfig = serde_json::from_str(&json)?;

        log::info!("App config loaded from disk");
        Ok(config)
    }

    /// Получить путь к файлу UI-настроек
    fn ui_preferences_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("ui_preferences.json"))
    }

    /// Сохранить UI-настройки (тема, локаль)
    pub async fn save_ui_preferences(prefs: &UiPreferences) -> Result<()> {
        let path = Self::ui_preferences_path()?;
        let json = serde_json::to_string_pretty(prefs)?;
        tokio::fs::write(path, json).await?;
        log::info!("UI preferences saved to disk");
        Ok(())
    }

    /// Загрузить UI-настройки
    pub async fn load_ui_preferences() -> Result<UiPreferences> {
        let path = Self::ui_preferences_path()?;
        if !path.exists() {
            log::info!("No saved UI preferences found, using defaults");
            return Ok(UiPreferences::default());
        }
        let json = tokio::fs::read_to_string(path).await?;
        let prefs: UiPreferences = serde_json::from_str(&json)?;
        log::info!("UI preferences loaded from disk");
        Ok(prefs)
    }

    /// Удалить сохраненную конфигурацию приложения
    pub async fn delete_app_config() -> Result<()> {
        let path = Self::app_config_path()?;

        if path.exists() {
            tokio::fs::remove_file(path).await?;
            log::info!("App config deleted");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::SttProviderType;
    use serial_test::serial;

    #[tokio::test]
    #[serial]
    async fn test_save_and_load_stt_config() {
        let _ = ConfigStore::delete_config().await;

        let mut config = SttConfig::default();
        config.provider = SttProviderType::Backend;
        config.language = "ru".to_string();

        ConfigStore::save_config(&config).await.unwrap();
        let loaded = ConfigStore::load_config().await.unwrap();

        assert_eq!(loaded.provider, config.provider);
        assert_eq!(loaded.language, config.language);

        ConfigStore::delete_config().await.unwrap();
    }

    #[tokio::test]
    #[serial]
    async fn test_load_nonexistent_config_returns_default() {
        let _ = ConfigStore::delete_config().await;

        let loaded = ConfigStore::load_config().await.unwrap();
        assert_eq!(loaded.provider, SttConfig::default().provider);
    }

    #[tokio::test]
    #[serial]
    async fn test_save_and_load_app_config() {
        let _ = ConfigStore::delete_app_config().await;

        let mut config = AppConfig::default();
        config.auto_copy_to_clipboard = true;
        config.vad_silence_timeout_ms = 2500;

        ConfigStore::save_app_config(&config).await.unwrap();
        let loaded = ConfigStore::load_app_config().await.unwrap();

        assert_eq!(loaded.auto_copy_to_clipboard, config.auto_copy_to_clipboard);
        assert_eq!(loaded.vad_silence_timeout_ms, config.vad_silence_timeout_ms);

        ConfigStore::delete_app_config().await.unwrap();
    }

    #[tokio::test]
    #[serial]
    async fn test_delete_config_is_safe() {
        // Удаление несуществующего конфига должно быть безопасным
        let result = ConfigStore::delete_config().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    #[serial]
    async fn test_config_paths() {
        let stt_path = ConfigStore::config_path().unwrap();
        let app_path = ConfigStore::app_config_path().unwrap();

        assert!(stt_path.to_str().unwrap().contains("stt_config.json"));
        assert!(app_path.to_str().unwrap().contains("app_config.json"));
    }
}
