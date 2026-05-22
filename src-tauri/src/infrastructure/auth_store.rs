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

#[derive(Debug, Deserialize)]
struct LegacyPluginStoreData {
    #[serde(default)]
    device_id: Option<String>,
    #[serde(default)]
    auth_session: Option<LegacyStoredSession>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyStoredSession {
    access_token: String,
    refresh_token: Option<String>,
    access_expires_at: String,
    refresh_expires_at: Option<String>,
    device_id: Option<String>,
    user: Option<LegacyStoredUser>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyStoredUser {
    id: String,
    email: String,
    email_verified: bool,
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

    fn legacy_plugin_store_dir_name() -> &'static str {
        "com.voicetotext.app"
    }

    fn plugin_store_migration_marker_file_name() -> &'static str {
        "auth_plugin_store_migration_v1"
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
        if let Ok(custom) = std::env::var("VOICE_TO_TEXT_CONFIG_DIR") {
            let custom = custom.trim();
            if !custom.is_empty() {
                let dir = PathBuf::from(custom);
                std::fs::create_dir_all(&dir)?;
                return Ok(dir);
            }
        }

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

    fn legacy_plugin_store_path() -> Result<PathBuf> {
        let data_dir =
            dirs::data_dir().ok_or_else(|| anyhow::anyhow!("Failed to get data directory"))?;
        Ok(data_dir
            .join(Self::legacy_plugin_store_dir_name())
            .join("auth.json"))
    }

    fn plugin_store_migration_marker_path(store_path: &Path) -> Result<PathBuf> {
        let parent = store_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Invalid auth store path (no parent)"))?;
        Ok(parent.join(Self::plugin_store_migration_marker_file_name()))
    }

    fn new_device_id() -> String {
        format!("desktop-{}", uuid::Uuid::new_v4())
    }

    fn parse_rfc3339_to_ms(value: &str) -> Result<i64> {
        Ok(chrono::DateTime::parse_from_rfc3339(value)?.timestamp_millis())
    }

    fn legacy_session_to_auth_session(session: LegacyStoredSession) -> Result<AuthSession> {
        let access_expires_at_ms = Self::parse_rfc3339_to_ms(&session.access_expires_at)?;
        let refresh_expires_at_ms = session
            .refresh_expires_at
            .as_deref()
            .map(Self::parse_rfc3339_to_ms)
            .transpose()?;

        Ok(AuthSession {
            access_token: session.access_token,
            refresh_token: session.refresh_token,
            access_expires_at_ms,
            refresh_expires_at_ms,
            user: session.user.map(|u| AuthUser {
                id: u.id,
                email: u.email,
                email_verified: u.email_verified,
            }),
        })
    }

    async fn write_plugin_store_migration_marker(marker_path: &Path) -> Result<()> {
        let parent = marker_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Invalid migration marker path (no parent)"))?;
        std::fs::create_dir_all(parent)?;
        tokio::fs::write(marker_path, b"ok").await?;
        Ok(())
    }

    async fn migrate_legacy_plugin_store_once_for_paths(
        store_path: &Path,
        legacy_plugin_store_path: &Path,
    ) -> Result<()> {
        let marker_path = Self::plugin_store_migration_marker_path(store_path)?;
        if marker_path.exists() {
            return Ok(());
        }

        if store_path.exists() {
            match tokio::fs::read_to_string(store_path).await {
                Ok(json) => match serde_json::from_str::<AuthStoreData>(&json) {
                    Ok(current) if current.session.is_some() => {
                        Self::write_plugin_store_migration_marker(&marker_path).await?;
                        return Ok(());
                    }
                    Ok(_) => {}
                    Err(e) => {
                        log::warn!(
                            "Skipping legacy auth plugin-store migration because current auth store is invalid: {}",
                            e
                        );
                        return Ok(());
                    }
                },
                Err(e) => {
                    log::warn!(
                        "Skipping legacy auth plugin-store migration because current auth store cannot be read: {}",
                        e
                    );
                    return Ok(());
                }
            }
        }

        if !legacy_plugin_store_path.exists() {
            return Ok(());
        }

        let json = tokio::fs::read_to_string(legacy_plugin_store_path).await?;
        let legacy: LegacyPluginStoreData = match serde_json::from_str(&json) {
            Ok(v) => v,
            Err(e) => {
                log::warn!("Failed to parse legacy auth plugin-store: {}", e);
                Self::write_plugin_store_migration_marker(&marker_path).await?;
                return Ok(());
            }
        };

        let Some(legacy_session) = legacy.auth_session else {
            Self::write_plugin_store_migration_marker(&marker_path).await?;
            return Ok(());
        };

        let device_id = legacy_session
            .device_id
            .as_deref()
            .or(legacy.device_id.as_deref())
            .filter(|id| !id.trim().is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(Self::new_device_id);

        let session = Self::legacy_session_to_auth_session(legacy_session)?;
        let migrated = AuthStoreData {
            device_id,
            session: Some(session),
        };

        let json = serde_json::to_string_pretty(&migrated)?;
        Self::write_file_atomic(store_path, &json).await?;
        Self::write_plugin_store_migration_marker(&marker_path).await?;

        log::info!(
            "Migrated auth session from legacy Tauri plugin-store at {:?}",
            legacy_plugin_store_path
        );
        Ok(())
    }

    async fn migrate_legacy_plugin_store_once(store_path: &Path) -> Result<()> {
        let legacy_plugin_store_path = Self::legacy_plugin_store_path()?;
        Self::migrate_legacy_plugin_store_once_for_paths(store_path, &legacy_plugin_store_path)
            .await
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
        if let Err(e) = Self::migrate_legacy_plugin_store_once(&path).await {
            log::warn!("Legacy auth plugin-store migration failed: {}", e);
        }

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

    #[tokio::test]
    async fn migrate_legacy_plugin_store_imports_session_when_auth_store_missing() {
        let root =
            std::env::temp_dir().join(format!("voice-to-text-plugin-auth-{}", Uuid::new_v4()));
        let target_dir = root.join("voice-to-text");
        let legacy_dir = root.join("com.voicetotext.app");
        std::fs::create_dir_all(&target_dir).unwrap();
        std::fs::create_dir_all(&legacy_dir).unwrap();

        let target = target_dir.join("auth_store.json");
        let legacy = legacy_dir.join("auth.json");
        std::fs::write(
            &legacy,
            r#"{
  "device_id": "desktop-root",
  "auth_session": {
    "accessToken": "access-token",
    "refreshToken": "refresh-token",
    "accessExpiresAt": "2026-01-01T00:00:00Z",
    "refreshExpiresAt": "2026-02-01T00:00:00Z",
    "deviceId": "desktop-session",
    "user": {
      "id": "user-1",
      "email": "user@example.com",
      "emailVerified": true
    }
  }
}"#,
        )
        .unwrap();

        AuthStore::migrate_legacy_plugin_store_once_for_paths(&target, &legacy)
            .await
            .unwrap();

        let migrated: AuthStoreData =
            serde_json::from_str(&std::fs::read_to_string(&target).unwrap()).unwrap();
        assert_eq!(migrated.device_id, "desktop-session");
        let session = migrated.session.unwrap();
        assert_eq!(session.access_token, "access-token");
        assert_eq!(session.refresh_token.as_deref(), Some("refresh-token"));
        assert_eq!(session.access_expires_at_ms, 1_767_225_600_000);
        assert_eq!(session.refresh_expires_at_ms, Some(1_769_904_000_000));
        assert_eq!(session.user.unwrap().email, "user@example.com");
        assert!(target_dir
            .join(AuthStore::plugin_store_migration_marker_file_name())
            .exists());

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn migrate_legacy_plugin_store_does_not_overwrite_existing_session() {
        let root =
            std::env::temp_dir().join(format!("voice-to-text-plugin-auth-{}", Uuid::new_v4()));
        let target_dir = root.join("voice-to-text");
        let legacy_dir = root.join("com.voicetotext.app");
        std::fs::create_dir_all(&target_dir).unwrap();
        std::fs::create_dir_all(&legacy_dir).unwrap();

        let target = target_dir.join("auth_store.json");
        let legacy = legacy_dir.join("auth.json");
        std::fs::write(
            &target,
            r#"{
  "device_id": "desktop-current",
  "session": {
    "access_token": "current-access",
    "refresh_token": null,
    "access_expires_at_ms": 1767225600000,
    "refresh_expires_at_ms": null,
    "user": null
  }
}"#,
        )
        .unwrap();
        std::fs::write(
            &legacy,
            r#"{
  "device_id": "desktop-legacy",
  "auth_session": {
    "accessToken": "legacy-access",
    "refreshToken": null,
    "accessExpiresAt": "2026-01-01T00:00:00Z"
  }
}"#,
        )
        .unwrap();

        AuthStore::migrate_legacy_plugin_store_once_for_paths(&target, &legacy)
            .await
            .unwrap();

        let migrated: AuthStoreData =
            serde_json::from_str(&std::fs::read_to_string(&target).unwrap()).unwrap();
        assert_eq!(migrated.device_id, "desktop-current");
        assert_eq!(migrated.session.unwrap().access_token, "current-access");
        assert!(target_dir
            .join(AuthStore::plugin_store_migration_marker_file_name())
            .exists());

        let _ = std::fs::remove_dir_all(root);
    }
}
