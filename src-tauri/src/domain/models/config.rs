use serde::{Deserialize, Serialize};

/// Supported STT provider types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SttProviderType {
    /// Local Whisper.cpp implementation (offline)
    WhisperLocal,
    /// AssemblyAI Universal-Streaming v3 (low cost, ultra-low latency)
    AssemblyAI,
    /// Deepgram cloud service (Nova-3 model)
    Deepgram,
    /// Google Cloud Speech-to-Text v2
    GoogleCloud,
    /// Azure Speech Services
    Azure,
    /// Backend API (через наш сервер с лицензией)
    Backend,
}

impl Default for SttProviderType {
    fn default() -> Self {
        Self::Backend // Через наш API с лицензией и usage tracking
    }
}

/// Configuration for STT provider
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SttConfig {
    /// Provider type
    pub provider: SttProviderType,

    /// Language code (e.g., "en", "ru")
    pub language: String,

    /// Enable automatic language detection
    pub auto_detect_language: bool,

    /// Enable automatic punctuation
    pub enable_punctuation: bool,

    /// Enable profanity filter
    pub filter_profanity: bool,

    /// API key для Deepgram (если пользователь хочет использовать свой ключ)
    /// Если None, используется встроенный ключ из embedded_keys
    pub deepgram_api_key: Option<String>,

    /// API key для AssemblyAI (если пользователь хочет использовать свой ключ)
    /// Если None, используется встроенный ключ из embedded_keys
    pub assemblyai_api_key: Option<String>,

    /// Model name/ID for local providers
    pub model: Option<String>,

    /// Auth token для нашего Backend API (получается при активации лицензии)
    /// Используется для подключения к api.voicetotext.app
    pub backend_auth_token: Option<String>,

    /// URL нашего Backend API (по умолчанию wss://api.voicetotext.app)
    pub backend_url: Option<String>,

    /// Keep WebSocket connection alive between recording sessions (only for providers that support it)
    /// Deepgram: safe (bills by audio duration, not connection time)
    /// AssemblyAI: dangerous (bills by connection time)
    pub keep_connection_alive: bool,

    /// Сколько держать соединение живым после остановки записи (если keep_connection_alive=true).
    ///
    /// Важно: keep-alive удерживает streaming соединение на стороне провайдера (Deepgram) и занимает слот
    /// по лимиту параллельных соединений. Поэтому TTL должен быть коротким (по умолчанию 2 минуты).
    #[serde(default = "default_keep_alive_ttl_secs")]
    pub keep_alive_ttl_secs: u64,
}

fn default_keep_alive_ttl_secs() -> u64 {
    120
}

impl Default for SttConfig {
    fn default() -> Self {
        Self {
            provider: SttProviderType::default(),
            language: "ru".to_string(),
            auto_detect_language: false,
            enable_punctuation: true,
            filter_profanity: false,
            deepgram_api_key: None,
            assemblyai_api_key: None,
            model: None,
            backend_auth_token: None,
            backend_url: None,
            keep_connection_alive: false, // Безопасно по умолчанию для всех провайдеров
            keep_alive_ttl_secs: default_keep_alive_ttl_secs(),
        }
    }
}

impl SttConfig {
    pub fn new(provider: SttProviderType) -> Self {
        Self {
            provider,
            ..Default::default()
        }
    }

    pub fn with_language(mut self, language: impl Into<String>) -> Self {
        self.language = language.into();
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }
}

/// Application-wide configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// STT configuration
    pub stt: SttConfig,

    /// Горячая клавиша для записи (например "Ctrl+X")
    pub recording_hotkey: String,

    /// Auto-copy transcription to clipboard
    pub auto_copy_to_clipboard: bool,

    /// Auto-paste transcription text incrementally (copies displayText to clipboard during recognition)
    pub auto_paste_text: bool,

    /// Auto-close window after transcription
    pub auto_close_window: bool,

    /// VAD silence timeout in milliseconds
    pub vad_silence_timeout_ms: u64,

    /// Microphone sensitivity / gain (0-200, default 95)
    /// Controls audio amplification level:
    /// - 0%:   gain 0.0x (complete silence)
    /// - 100%: gain 1.0x (no change, as recorded by microphone)
    /// - 200%: gain 5.0x (maximum amplification for quiet microphones)
    /// Formula: gain = sensitivity/100 for 0-100%, gain = 1.0 + (sensitivity-100)/100*4.0 for 100-200%
    pub microphone_sensitivity: u8,

    /// Selected audio input device name (None = use system default)
    pub selected_audio_device: Option<String>,

    /// Keep history of transcriptions
    pub keep_history: bool,

    /// Maximum number of history items
    pub max_history_items: usize,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            stt: SttConfig::default(),
            recording_hotkey: "CmdOrCtrl+Shift+X".to_string(), // Cmd на Mac, Ctrl на Win/Linux
            auto_copy_to_clipboard: true,
            auto_paste_text: false, // По умолчанию выключено (может раздражать)
            auto_close_window: true,
            vad_silence_timeout_ms: 3000, // 3 секунды тишины перед авто-остановкой
            microphone_sensitivity: 95, // Очень высокая чувствительность по умолчанию (порог ~1638)
            selected_audio_device: None, // По умолчанию используем системное устройство
            keep_history: true,
            max_history_items: 20,
        }
    }
}

/// Пользовательские UI-настройки (тема, локаль), синхронизируются между окнами через state-sync
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiPreferences {
    pub theme: String,
    pub locale: String,
}

impl Default for UiPreferences {
    fn default() -> Self {
        Self {
            theme: "dark".to_string(),
            locale: "ru".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stt_provider_type_default() {
        assert_eq!(SttProviderType::default(), SttProviderType::Backend);
    }

    #[test]
    fn test_stt_config_default() {
        let config = SttConfig::default();
        assert_eq!(config.provider, SttProviderType::Backend);
        assert_eq!(config.language, "ru");
        assert!(!config.auto_detect_language);
        assert!(config.enable_punctuation);
        assert!(!config.filter_profanity);
        assert!(config.deepgram_api_key.is_none());
        assert!(config.assemblyai_api_key.is_none());
        assert!(config.model.is_none());
        assert!(config.backend_auth_token.is_none());
        assert!(config.backend_url.is_none());
        assert!(!config.keep_connection_alive);
        assert_eq!(config.keep_alive_ttl_secs, 120);
    }

    #[test]
    fn test_stt_config_new() {
        let config = SttConfig::new(SttProviderType::AssemblyAI);
        assert_eq!(config.provider, SttProviderType::AssemblyAI);
        assert_eq!(config.language, "ru");
    }

    #[test]
    fn test_stt_config_with_language() {
        let config = SttConfig::new(SttProviderType::Deepgram)
            .with_language("en");
        assert_eq!(config.language, "en");
    }

    #[test]
    fn test_stt_config_with_model() {
        let config = SttConfig::new(SttProviderType::WhisperLocal)
            .with_model("base");
        assert_eq!(config.model, Some("base".to_string()));
    }

    #[test]
    fn test_stt_config_builder_chain() {
        let config = SttConfig::new(SttProviderType::Deepgram)
            .with_language("en")
            .with_model("nova-2");

        assert_eq!(config.provider, SttProviderType::Deepgram); // Явно создан с Deepgram
        assert_eq!(config.language, "en");
        assert_eq!(config.model, Some("nova-2".to_string()));
    }

    #[test]
    fn test_app_config_default() {
        let config = AppConfig::default();
        assert_eq!(config.recording_hotkey, "CmdOrCtrl+Shift+X");
        assert!(config.auto_copy_to_clipboard);
        assert!(config.auto_close_window);
        assert_eq!(config.vad_silence_timeout_ms, 3000);
        assert_eq!(config.microphone_sensitivity, 95);
        assert!(config.keep_history);
        assert_eq!(config.max_history_items, 20);
    }

    #[test]
    fn test_stt_provider_type_equality() {
        assert_eq!(SttProviderType::Deepgram, SttProviderType::Deepgram);
        assert_ne!(SttProviderType::Deepgram, SttProviderType::AssemblyAI);
    }

    #[test]
    fn test_stt_config_clone() {
        let config1 = SttConfig::new(SttProviderType::Deepgram)
            .with_language("en");
        let config2 = config1.clone();
        assert_eq!(config1.provider, config2.provider);
        assert_eq!(config1.language, config2.language);
    }

    #[test]
    fn test_app_config_clone() {
        let config1 = AppConfig::default();
        let config2 = config1.clone();
        assert_eq!(config1.recording_hotkey, config2.recording_hotkey);
        assert_eq!(config1.microphone_sensitivity, config2.microphone_sensitivity);
    }
}
