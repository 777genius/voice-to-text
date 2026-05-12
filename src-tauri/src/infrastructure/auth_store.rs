use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Персистентное хранилище auth состояния (device_id + session).
///
/// Цели:
/// - единый source of truth в Rust (надёжно даже когда WebView "спит")
/// - общий device_id для всех окон (важно для refresh token привязки на сервере)
/// - хранение refresh/access токенов и сроков жизни для фонового refresh
pub struct AuthStore;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthUser {
    pub id: String,
    pub email: String,
    pub email_verified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthSession {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub access_expires_at_ms: i64,
    pub refresh_expires_at_ms: Option<i64>,
    pub user: Option<AuthUser>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthStoreData {
    pub device_id: String,
    pub session: Option<AuthSession>,
}

impl AuthStoreData {
    pub fn is_authenticated(&self) -> bool {
        self.session.is_some()
    }
}

impl AuthStore {
    fn app_dir_name() -> &'static str {
        if cfg!(debug_assertions) {
            "voice-to-text-dev"
        } else {
            "voice-to-text"
        }
    }

    fn legacy_shared_dir_name() -> &'static str {
        "voice-to-text"
    }

    fn scoped_config_dir(root: &Path) -> PathBuf {
        root.join(Self::app_dir_name())
    }

    fn legacy_shared_dir(root: &Path) -> PathBuf {
        root.join(Self::legacy_shared_dir_name())
    }

    fn migrate_legacy_store_once(root: &Path) -> Result<()> {
        if !cfg!(debug_assertions) {
            return Ok(());
        }

        let target_dir = Self::scoped_config_dir(root);
        let legacy_dir = Self::legacy_shared_dir(root);
        if target_dir == legacy_dir {
            return Ok(());
        }

        std::fs::create_dir_all(&target_dir)?;

        let target = target_dir.join("auth_store.json");
        if target.exists() {
            return Ok(());
        }

        let legacy = legacy_dir.join("auth_store.json");
        if !legacy.exists() {
            return Ok(());
        }

        std::fs::copy(&legacy, &target)?;
        log::info!(
            "Migrated auth store from {:?} to {:?}",
            legacy_dir,
            target_dir
        );
        Ok(())
    }

    fn config_dir() -> Result<PathBuf> {
        let config_dir =
            dirs::config_dir().ok_or_else(|| anyhow::anyhow!("Failed to get config directory"))?;
        let app_config_dir = Self::scoped_config_dir(&config_dir);

        // Важно: create_dir_all идемпотентен и надёжнее, чем exists() (race).
        std::fs::create_dir_all(&app_config_dir)?;
        Self::migrate_legacy_store_once(&config_dir)?;
        Ok(app_config_dir)
    }

    fn store_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("auth_store.json"))
    }

    fn new_device_id() -> String {
        format!("desktop-{}", uuid::Uuid::new_v4())
    }

    async fn write_file_atomic(path: &Path, contents: &str) -> Result<()> {
        // Важно: tmp-файл должен быть уникальным, иначе параллельные save() будут конфликтовать.
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Invalid auth store path (no parent)"))?;
        let file_name = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow::anyhow!("Invalid auth store path (bad filename)"))?;
        let tmp = parent.join(format!("{}.tmp.{}", file_name, uuid::Uuid::new_v4()));

        tokio::fs::write(&tmp, contents).await?;
        let _ = tokio::fs::remove_file(path).await;

        match tokio::fs::rename(&tmp, path).await {
            Ok(_) => Ok(()),
            Err(e) => {
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

    /// Загружает хранилище с диска или создаёт новое (с device_id).
    pub async fn load_or_create() -> Result<AuthStoreData> {
        let path = Self::store_path()?;
        if !path.exists() {
            let data = AuthStoreData {
                device_id: Self::new_device_id(),
                session: None,
            };
            Self::save(&data).await?;
            return Ok(data);
        }

        let json = tokio::fs::read_to_string(&path).await?;
        let mut data: AuthStoreData = serde_json::from_str(&json)?;

        // Защита: device_id обязателен
        if data.device_id.trim().is_empty() {
            data.device_id = Self::new_device_id();
            Self::save(&data).await?;
        }

        Ok(data)
    }

    pub async fn save(data: &AuthStoreData) -> Result<()> {
        let path = Self::store_path()?;
        let json = serde_json::to_string_pretty(data)?;
        Self::write_file_atomic(&path, &json).await?;
        Ok(())
    }

    pub async fn clear_session_keep_device_id() -> Result<AuthStoreData> {
        let mut data = Self::load_or_create().await?;
        data.session = None;
        Self::save(&data).await?;
        Ok(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn app_dir_name_matches_build_profile() {
        #[cfg(debug_assertions)]
        assert_eq!(AuthStore::app_dir_name(), "voice-to-text-dev");

        #[cfg(not(debug_assertions))]
        assert_eq!(AuthStore::app_dir_name(), "voice-to-text");
    }

    #[test]
    fn migrate_legacy_store_once_copies_existing_auth_store_for_dev_storage() {
        let root =
            std::env::temp_dir().join(format!("voice-to-text-auth-migrate-{}", Uuid::new_v4()));
        let legacy_dir = root.join("voice-to-text");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        std::fs::write(
            legacy_dir.join("auth_store.json"),
            "{\"device_id\":\"desktop-1\",\"session\":null}",
        )
        .unwrap();

        AuthStore::migrate_legacy_store_once(&root).unwrap();

        let target_dir = AuthStore::scoped_config_dir(&root);
        #[cfg(debug_assertions)]
        assert_eq!(
            std::fs::read_to_string(target_dir.join("auth_store.json")).unwrap(),
            "{\"device_id\":\"desktop-1\",\"session\":null}"
        );

        #[cfg(not(debug_assertions))]
        assert_eq!(
            std::fs::read_to_string(target_dir.join("auth_store.json")).unwrap(),
            "{\"device_id\":\"desktop-1\",\"session\":null}"
        );

        let _ = std::fs::remove_dir_all(root);
    }
}
