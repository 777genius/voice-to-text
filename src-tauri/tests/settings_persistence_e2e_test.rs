use app_lib::domain::{AppConfig, SttConfig, SttProviderType};
use app_lib::infrastructure::ConfigStore;
use serial_test::serial;
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

const CONFIG_DIR_ENV: &str = "VOICE_TO_TEXT_CONFIG_DIR";

struct TestConfigDir {
    dir: PathBuf,
}

impl TestConfigDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("voice-to-text-test-{}", Uuid::new_v4()));
        let _ = fs::create_dir_all(&dir);
        std::env::set_var(CONFIG_DIR_ENV, dir.to_string_lossy().to_string());
        Self { dir }
    }
}

impl Drop for TestConfigDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
        std::env::remove_var(CONFIG_DIR_ENV);
    }
}

#[tokio::test]
#[serial]
async fn settings_persist_across_restart_like_flow() {
    let _guard = TestConfigDir::new();

    let _ = ConfigStore::delete_config().await;
    let _ = ConfigStore::delete_app_config().await;

    // 1) "Пользователь выставил keyterms" → сохраняем stt_config.json
    let keyterms = "Kubernetes, VoicetextAI, Deepgram";
    let mut stt = SttConfig::default();
    stt.provider = SttProviderType::Backend;
    stt.language = "ru".to_string();
    stt.deepgram_keyterms = Some(keyterms.to_string());
    ConfigStore::save_config(&stt).await.unwrap();

    // 2) "Пользователь выставил чувствительность" → сохраняем app_config.json
    let mut app = AppConfig::default();
    app.microphone_sensitivity = 135;
    ConfigStore::save_app_config(&app).await.unwrap();

    // 3) Частичное обновление STT (например смена языка) не должно затирать keyterms
    let mut stt_language_only = ConfigStore::load_config().await.unwrap();
    stt_language_only.language = "en".to_string();
    ConfigStore::save_config(&stt_language_only).await.unwrap();

    // 4) "Перезапуск": читаем с диска заново
    let stt_after = ConfigStore::load_config().await.unwrap();
    let app_after = ConfigStore::load_app_config().await.unwrap();

    assert_eq!(stt_after.language, "en");
    assert_eq!(stt_after.deepgram_keyterms.as_deref(), Some(keyterms));
    assert_eq!(app_after.microphone_sensitivity, 135);
}

