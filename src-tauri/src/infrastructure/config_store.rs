use std::path::{Path, PathBuf};
use anyhow::Result;

use crate::domain::{SttConfig, AppConfig, UiPreferences};

/// Маркер "приложение только что обновилось".
///
/// Нужен как safety-net для Windows: после перезапуска приложение по умолчанию стартует скрытым
/// (без taskbar), и пользователь может подумать, что оно не запустилось.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PostUpdateMarker {
    pub version: String,
    pub created_at_ms: i64,
}

/// Персистентное хранилище конфигурации STT
pub struct ConfigStore;

impl ConfigStore {
    fn backup_path(path: &Path) -> PathBuf {
        PathBuf::from(format!("{}.bak", path.display()))
    }

    async fn write_backup_best_effort(path: &Path) {
        if !path.exists() {
            return;
        }
        let bak = Self::backup_path(path);
        if let Err(e) = tokio::fs::copy(path, &bak).await {
            log::warn!("Failed to write config backup {:?}: {}", bak, e);
        }
    }

    async fn write_file_atomic(path: &Path, contents: &str) -> Result<()> {
        // Пишем во временный файл и только потом атомарно подменяем.
        // На Windows rename может падать, если цель уже существует, поэтому делаем best-effort remove.
        // Важно: tmp-файл должен быть уникальным, иначе параллельные save() будут конфликтовать.
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Invalid config path (no parent)"))?;
        let file_name = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow::anyhow!("Invalid config path (bad filename)"))?;
        let tmp = parent.join(format!("{}.tmp.{}", file_name, uuid::Uuid::new_v4()));

        tokio::fs::write(&tmp, contents).await?;

        // Best-effort: если файл уже был — убираем, иначе rename может упасть на Windows.
        let _ = tokio::fs::remove_file(path).await;

        match tokio::fs::rename(&tmp, path).await {
            Ok(_) => Ok(()),
            Err(e) => {
                // Фоллбек на прямую запись (на случай нестандартных FS ограничений).
                log::warn!(
                    "Atomic rename failed for {:?}: {}. Falling back to direct write.",
                    path,
                    e
                );
                tokio::fs::write(path, contents).await?;
                let _ = tokio::fs::remove_file(&tmp).await;
                Ok(())
            }
        }
    }

    /// Получить директорию конфигурации приложения
    fn config_dir() -> Result<PathBuf> {
        // Для тестов и отладки даём возможность переопределить директорию хранения конфигов.
        // В проде переменная окружения обычно не задана → используем стандартный OS config dir.
        if let Ok(custom) = std::env::var("VOICE_TO_TEXT_CONFIG_DIR") {
            let custom = custom.trim();
            if !custom.is_empty() {
                let dir = PathBuf::from(custom);
                std::fs::create_dir_all(&dir)?;
                return Ok(dir);
            }
        }

        let config_dir = dirs::config_dir()
            .ok_or_else(|| anyhow::anyhow!("Failed to get config directory"))?;

        let app_config_dir = config_dir.join("voice-to-text");

        // Важно: create_dir_all идемпотентен и надёжнее, чем exists() (race).
        std::fs::create_dir_all(&app_config_dir)?;

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
        Self::write_backup_best_effort(&path).await;
        Self::write_file_atomic(&path, &json).await?;

        log::debug!("STT config saved to disk");
        Ok(())
    }

    /// Загрузить конфигурацию STT
    pub async fn load_config() -> Result<SttConfig> {
        let path = Self::config_path()?;

        if !path.exists() {
            log::info!("No saved config found, using defaults");
            return Ok(SttConfig::default());
        }

        let json = match tokio::fs::read_to_string(&path).await {
            Ok(v) => v,
            Err(e) => {
                let bak = Self::backup_path(&path);
                log::warn!("Failed to read STT config {:?}: {}. Trying backup {:?}.", path, e, bak);
                let json_bak = tokio::fs::read_to_string(&bak).await?;
                let cfg_bak: SttConfig = serde_json::from_str(&json_bak)?;
                // Best-effort: восстанавливаем основной файл, чтобы следующий старт был стабильным.
                if let Ok(pretty) = serde_json::to_string_pretty(&cfg_bak) {
                    let _ = Self::write_file_atomic(&path, &pretty).await;
                }
                return Ok(cfg_bak);
            }
        };

        let config: SttConfig = match serde_json::from_str(&json) {
            Ok(v) => v,
            Err(e) => {
                let bak = Self::backup_path(&path);
                log::warn!(
                    "Failed to parse STT config {:?}: {}. Trying backup {:?}.",
                    path,
                    e,
                    bak
                );
                let json_bak = tokio::fs::read_to_string(&bak).await?;
                let cfg_bak: SttConfig = serde_json::from_str(&json_bak)?;
                if let Ok(pretty) = serde_json::to_string_pretty(&cfg_bak) {
                    let _ = Self::write_file_atomic(&path, &pretty).await;
                }
                cfg_bak
            }
        };

        log::debug!("STT config loaded from disk");
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
        Self::write_backup_best_effort(&path).await;
        Self::write_file_atomic(&path, &json).await?;

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

        let json = match tokio::fs::read_to_string(&path).await {
            Ok(v) => v,
            Err(e) => {
                let bak = Self::backup_path(&path);
                log::warn!("Failed to read app config {:?}: {}. Trying backup {:?}.", path, e, bak);
                let json_bak = tokio::fs::read_to_string(&bak).await?;
                let cfg_bak: AppConfig = serde_json::from_str(&json_bak)?;
                if let Ok(pretty) = serde_json::to_string_pretty(&cfg_bak) {
                    let _ = Self::write_file_atomic(&path, &pretty).await;
                }
                return Ok(cfg_bak);
            }
        };

        let config: AppConfig = match serde_json::from_str(&json) {
            Ok(v) => v,
            Err(e) => {
                let bak = Self::backup_path(&path);
                log::warn!(
                    "Failed to parse app config {:?}: {}. Trying backup {:?}.",
                    path,
                    e,
                    bak
                );
                let json_bak = tokio::fs::read_to_string(&bak).await?;
                let cfg_bak: AppConfig = serde_json::from_str(&json_bak)?;
                if let Ok(pretty) = serde_json::to_string_pretty(&cfg_bak) {
                    let _ = Self::write_file_atomic(&path, &pretty).await;
                }
                cfg_bak
            }
        };

        log::info!("App config loaded from disk");
        Ok(config)
    }

    /// Получить путь к файлу UI-настроек
    fn ui_preferences_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("ui_preferences.json"))
    }

    /// Получить путь к маркеру пост-апдейта
    fn post_update_marker_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("post_update.json"))
    }

    /// Сохранить маркер пост-апдейта (перед перезапуском приложения).
    pub async fn save_post_update_marker(version: &str) -> Result<()> {
        let path = Self::post_update_marker_path()?;
        let marker = PostUpdateMarker {
            version: version.to_string(),
            created_at_ms: chrono::Utc::now().timestamp_millis(),
        };
        let json = serde_json::to_string_pretty(&marker)?;
        Self::write_file_atomic(&path, &json).await?;
        Ok(())
    }

    /// Прочитать и удалить маркер пост-апдейта (one-shot).
    pub async fn take_post_update_marker() -> Result<Option<PostUpdateMarker>> {
        let path = Self::post_update_marker_path()?;
        if !path.exists() {
            return Ok(None);
        }

        // Best-effort: если файл битый или не читается — всё равно удаляем,
        // чтобы не зацикливаться на каждом старте.
        let marker = match tokio::fs::read_to_string(&path).await {
            Ok(json) => serde_json::from_str::<PostUpdateMarker>(&json).ok(),
            Err(e) => {
                log::warn!("Failed to read post-update marker: {}", e);
                // Файл существует → считаем что апдейт был, даже если payload не удалось прочитать.
                Some(PostUpdateMarker {
                    version: "unknown".to_string(),
                    created_at_ms: 0,
                })
            }
        };

        let _ = tokio::fs::remove_file(&path).await;
        Ok(marker)
    }

    /// Сохранить UI-настройки (тема, локаль)
    pub async fn save_ui_preferences(prefs: &UiPreferences) -> Result<()> {
        let path = Self::ui_preferences_path()?;
        let json = serde_json::to_string_pretty(prefs)?;
        Self::write_backup_best_effort(&path).await;
        Self::write_file_atomic(&path, &json).await?;
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

        let json = match tokio::fs::read_to_string(&path).await {
            Ok(v) => v,
            Err(e) => {
                let bak = Self::backup_path(&path);
                log::warn!("Failed to read UI preferences {:?}: {}. Trying backup {:?}.", path, e, bak);
                let json_bak = tokio::fs::read_to_string(&bak).await?;
                let prefs_bak: UiPreferences = serde_json::from_str(&json_bak)?;
                if let Ok(pretty) = serde_json::to_string_pretty(&prefs_bak) {
                    let _ = Self::write_file_atomic(&path, &pretty).await;
                }
                return Ok(prefs_bak);
            }
        };

        let prefs: UiPreferences = match serde_json::from_str(&json) {
            Ok(v) => v,
            Err(e) => {
                let bak = Self::backup_path(&path);
                log::warn!(
                    "Failed to parse UI preferences {:?}: {}. Trying backup {:?}.",
                    path,
                    e,
                    bak
                );
                let json_bak = tokio::fs::read_to_string(&bak).await?;
                let prefs_bak: UiPreferences = serde_json::from_str(&json_bak)?;
                if let Ok(pretty) = serde_json::to_string_pretty(&prefs_bak) {
                    let _ = Self::write_file_atomic(&path, &pretty).await;
                }
                prefs_bak
            }
        };
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
    use uuid::Uuid;

    const CONFIG_DIR_ENV: &str = "VOICE_TO_TEXT_CONFIG_DIR";

    struct TestConfigDir {
        dir: PathBuf,
    }

    impl TestConfigDir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("voice-to-text-test-{}", Uuid::new_v4()));
            let _ = std::fs::create_dir_all(&dir);
            std::env::set_var(CONFIG_DIR_ENV, dir.to_string_lossy().to_string());
            Self { dir }
        }
    }

    impl Drop for TestConfigDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
            std::env::remove_var(CONFIG_DIR_ENV);
        }
    }

    #[tokio::test]
    #[serial]
    async fn test_save_and_load_stt_config() {
        let _guard = TestConfigDir::new();
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
        let _guard = TestConfigDir::new();
        let _ = ConfigStore::delete_config().await;

        let loaded = ConfigStore::load_config().await.unwrap();
        assert_eq!(loaded.provider, SttConfig::default().provider);
    }

    #[tokio::test]
    #[serial]
    async fn test_save_and_load_app_config() {
        let _guard = TestConfigDir::new();
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
        let _guard = TestConfigDir::new();
        // Удаление несуществующего конфига должно быть безопасным
        let result = ConfigStore::delete_config().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    #[serial]
    async fn test_config_paths() {
        let _guard = TestConfigDir::new();
        let stt_path = ConfigStore::config_path().unwrap();
        let app_path = ConfigStore::app_config_path().unwrap();

        assert!(stt_path.to_str().unwrap().contains("stt_config.json"));
        assert!(app_path.to_str().unwrap().contains("app_config.json"));
    }

    #[tokio::test]
    #[serial]
    async fn post_update_marker_is_one_shot() {
        let _guard = TestConfigDir::new();
        // Убедимся, что маркер можно поставить и снять.
        ConfigStore::save_post_update_marker("9.9.9").await.unwrap();
        let marker = ConfigStore::take_post_update_marker().await.unwrap();
        assert!(marker.is_some());
        assert_eq!(marker.unwrap().version, "9.9.9");

        // Второй раз маркера уже быть не должно.
        let marker2 = ConfigStore::take_post_update_marker().await.unwrap();
        assert!(marker2.is_none());
    }
}
