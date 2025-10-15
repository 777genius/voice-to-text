use std::path::PathBuf;
use anyhow::Result;

use crate::domain::{SttConfig, AppConfig};

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
