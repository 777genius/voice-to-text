use serde::{Deserialize, Serialize};
use std::str::FromStr;

/// Active recording mode. Чем-то управляет hotkey: dictation = STT в текст,
/// live_translation = OpenAI realtime translate в virtual mic + текст в popover.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingMode {
    Dictation,
    LiveTranslation,
}

impl Default for RecordingMode {
    fn default() -> Self {
        Self::Dictation
    }
}

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

/// Streaming STT provider selected behind our Backend API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackendStreamingProvider {
    /// Deepgram realtime STT
    Deepgram,
    /// ElevenLabs realtime speech-to-text
    ElevenLabs,
}

impl BackendStreamingProvider {
    pub fn as_protocol_name(self) -> &'static str {
        match self {
            Self::Deepgram => "deepgram",
            Self::ElevenLabs => "elevenlabs",
        }
    }
}

impl Default for BackendStreamingProvider {
    fn default() -> Self {
        Self::Deepgram
    }
}

impl FromStr for BackendStreamingProvider {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "deepgram" => Ok(Self::Deepgram),
            "elevenlabs" | "eleven_labs" | "eleven-labs" => Ok(Self::ElevenLabs),
            other => Err(format!(
                "Unsupported backend streaming provider: {}. Expected deepgram or elevenlabs",
                other
            )),
        }
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
    /// Используется для подключения к api.voicetext.site
    pub backend_auth_token: Option<String>,

    /// URL нашего Backend API (по умолчанию wss://api.voicetext.site)
    pub backend_url: Option<String>,

    /// Streaming provider used by our Backend API when `provider = Backend`.
    #[serde(default)]
    pub backend_streaming_provider: BackendStreamingProvider,

    /// Keep WebSocket connection alive between recording sessions (only for providers that support it)
    /// Deepgram: safe (bills by audio duration, not connection time)
    /// AssemblyAI: dangerous (bills by connection time)
    pub keep_connection_alive: bool,

    /// Сколько держать соединение живым после остановки записи (если keep_connection_alive=true).
    ///
    /// Важно: keep-alive удерживает streaming соединение на стороне провайдера (Deepgram) и занимает слот
    /// по лимиту параллельных соединений. Для backend-only режима держим TTL чуть ниже серверного
    /// audio_idle_ttl_secs=3600, чтобы idle клиенты закрывались до серверного timeout.
    #[serde(default = "default_keep_alive_ttl_secs")]
    pub keep_alive_ttl_secs: u64,

    /// Ключевые термины для улучшения streaming-распознавания (через запятую).
    /// Например: "Kubernetes, VoicetextAI"
    #[serde(default, alias = "deepgram_keyterms")]
    pub streaming_keyterms: Option<String>,
}

pub const BACKEND_KEEPALIVE_TTL_SECS: u64 = 59 * 60;

fn default_keep_alive_ttl_secs() -> u64 {
    BACKEND_KEEPALIVE_TTL_SECS
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
            backend_streaming_provider: BackendStreamingProvider::default(),
            keep_connection_alive: false, // Безопасно по умолчанию для всех провайдеров
            keep_alive_ttl_secs: default_keep_alive_ttl_secs(),
            streaming_keyterms: None,
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

/// Last saved recording window position in physical screen coordinates.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecordingWindowPosition {
    pub x: i32,
    pub y: i32,
}

/// Application-wide configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// STT configuration
    pub stt: SttConfig,

    /// Горячая клавиша для записи (например "Ctrl+X")
    pub recording_hotkey: String,

    /// Auto-copy transcription to clipboard
    pub auto_copy_to_clipboard: bool,

    /// Auto-paste transcription text incrementally (copies displayText to clipboard during recognition)
    pub auto_paste_text: bool,

    /// Play UI sound when transcription completes
    pub play_completion_sound: bool,

    /// Start/stop recording from the global hotkey without showing the recording window
    pub hide_recording_window_on_hotkey: bool,

    /// Show a compact recording window instead of the full recording controls
    #[serde(alias = "recording_window_bottom_right")]
    pub show_mini_recording_window: bool,

    /// User-adjusted recording window position (only used when mini window mode is enabled)
    pub recording_window_position: Option<RecordingWindowPosition>,

    /// Keep listening until the user stops recording manually; disables VAD silence auto-stop
    pub keep_recording_until_manual_stop: bool,

    /// Auto-close window after transcription
    pub auto_close_window: bool,

    /// VAD silence timeout in milliseconds
    pub vad_silence_timeout_ms: u64,

    /// Microphone sensitivity / gain (0-200, default 100)
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

    /// Активный режим записи. dictation = STT в текст, live_translation = OpenAI realtime translate.
    #[serde(default)]
    pub recording_mode: RecordingMode,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            stt: SttConfig::default(),
            recording_hotkey: "CmdOrCtrl+Shift+X".to_string(), // Cmd на Mac, Ctrl на Win/Linux
            auto_copy_to_clipboard: false,
            auto_paste_text: true,
            play_completion_sound: false,
            hide_recording_window_on_hotkey: false,
            show_mini_recording_window: true,
            recording_window_position: None,
            keep_recording_until_manual_stop: false,
            auto_close_window: true,
            vad_silence_timeout_ms: 5000, // 5 секунд тишины перед авто-остановкой
            microphone_sensitivity: 100,  // Нейтральный уровень: как записывает микрофон
            selected_audio_device: None,  // По умолчанию используем системное устройство
            keep_history: true,
            max_history_items: 20,
            recording_mode: RecordingMode::default(),
        }
    }
}

/// Пользовательские UI-настройки (тема, локаль), синхронизируются между окнами через state-sync
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiPreferences {
    pub theme: String,
    pub locale: String,
    #[serde(default)]
    pub use_system_theme: bool,
}

impl Default for UiPreferences {
    fn default() -> Self {
        Self {
            theme: "dark".to_string(),
            locale: "ru".to_string(),
            use_system_theme: false,
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
        assert_eq!(
            config.backend_streaming_provider,
            BackendStreamingProvider::Deepgram
        );
        assert!(!config.keep_connection_alive);
        assert_eq!(config.keep_alive_ttl_secs, BACKEND_KEEPALIVE_TTL_SECS);
        assert!(config.streaming_keyterms.is_none());
    }

    #[test]
    fn test_backend_streaming_provider_parse_aliases() {
        assert_eq!(
            "deepgram".parse::<BackendStreamingProvider>().unwrap(),
            BackendStreamingProvider::Deepgram
        );
        assert_eq!(
            "eleven_labs".parse::<BackendStreamingProvider>().unwrap(),
            BackendStreamingProvider::ElevenLabs
        );
        assert!("assemblyai".parse::<BackendStreamingProvider>().is_err());
    }

    #[test]
    fn test_backend_streaming_provider_protocol_names() {
        assert_eq!(
            BackendStreamingProvider::Deepgram.as_protocol_name(),
            "deepgram"
        );
        assert_eq!(
            BackendStreamingProvider::ElevenLabs.as_protocol_name(),
            "elevenlabs"
        );
    }

    #[test]
    fn test_stt_config_deserializes_legacy_config_without_backend_streaming_provider() {
        let mut value = serde_json::to_value(SttConfig::default()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("backend_streaming_provider");

        let config: SttConfig = serde_json::from_value(value).unwrap();

        assert_eq!(
            config.backend_streaming_provider,
            BackendStreamingProvider::Deepgram
        );
    }

    #[test]
    fn test_stt_config_deserializes_saved_elevenlabs_backend_streaming_provider() {
        let mut value = serde_json::to_value(SttConfig::default()).unwrap();
        value["backend_streaming_provider"] = serde_json::Value::String("elevenlabs".to_string());

        let config: SttConfig = serde_json::from_value(value).unwrap();

        assert_eq!(
            config.backend_streaming_provider,
            BackendStreamingProvider::ElevenLabs
        );
    }

    #[test]
    fn test_stt_config_deserializes_legacy_deepgram_keyterms_alias() {
        let mut value = serde_json::to_value(SttConfig::default()).unwrap();
        value.as_object_mut().unwrap().remove("streaming_keyterms");
        value["deepgram_keyterms"] =
            serde_json::Value::String("Kubernetes, VoicetextAI".to_string());

        let config: SttConfig = serde_json::from_value(value).unwrap();

        assert_eq!(
            config.streaming_keyterms.as_deref(),
            Some("Kubernetes, VoicetextAI")
        );
    }

    #[test]
    fn test_stt_config_serializes_streaming_keyterms_canonical_name() {
        let mut config = SttConfig::default();
        config.streaming_keyterms = Some("Kubernetes, VoicetextAI".to_string());

        let value = serde_json::to_value(config).unwrap();

        assert_eq!(
            value.get("streaming_keyterms").and_then(|v| v.as_str()),
            Some("Kubernetes, VoicetextAI")
        );
        assert!(value.get("deepgram_keyterms").is_none());
    }

    #[test]
    fn test_stt_config_new() {
        let config = SttConfig::new(SttProviderType::AssemblyAI);
        assert_eq!(config.provider, SttProviderType::AssemblyAI);
        assert_eq!(config.language, "ru");
    }

    #[test]
    fn test_stt_config_with_language() {
        let config = SttConfig::new(SttProviderType::Deepgram).with_language("en");
        assert_eq!(config.language, "en");
    }

    #[test]
    fn test_stt_config_with_model() {
        let config = SttConfig::new(SttProviderType::WhisperLocal).with_model("base");
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
        assert!(!config.auto_copy_to_clipboard);
        assert!(config.auto_paste_text);
        assert!(!config.play_completion_sound);
        assert!(!config.hide_recording_window_on_hotkey);
        assert!(config.show_mini_recording_window);
        assert!(config.recording_window_position.is_none());
        assert!(!config.keep_recording_until_manual_stop);
        assert!(config.auto_close_window);
        assert_eq!(config.vad_silence_timeout_ms, 5000);
        assert_eq!(config.microphone_sensitivity, 100);
        assert!(config.keep_history);
        assert_eq!(config.max_history_items, 20);
        assert_eq!(config.recording_mode, RecordingMode::Dictation);
    }

    #[test]
    fn test_recording_mode_default_is_dictation() {
        assert_eq!(RecordingMode::default(), RecordingMode::Dictation);
    }

    #[test]
    fn test_recording_mode_serde_snake_case() {
        let dict = serde_json::to_string(&RecordingMode::Dictation).unwrap();
        assert_eq!(dict, "\"dictation\"");
        let lt = serde_json::to_string(&RecordingMode::LiveTranslation).unwrap();
        assert_eq!(lt, "\"live_translation\"");

        let parsed: RecordingMode = serde_json::from_str("\"dictation\"").unwrap();
        assert_eq!(parsed, RecordingMode::Dictation);
        let parsed: RecordingMode = serde_json::from_str("\"live_translation\"").unwrap();
        assert_eq!(parsed, RecordingMode::LiveTranslation);
    }

    #[test]
    fn test_app_config_accepts_legacy_config_without_recording_mode() {
        let legacy = r#"{
            "recording_hotkey": "CmdOrCtrl+Shift+X",
            "auto_copy_to_clipboard": false,
            "auto_paste_text": true,
            "play_completion_sound": false,
            "hide_recording_window_on_hotkey": false,
            "show_mini_recording_window": true,
            "keep_recording_until_manual_stop": false,
            "auto_close_window": true,
            "vad_silence_timeout_ms": 5000,
            "microphone_sensitivity": 100,
            "keep_history": true,
            "max_history_items": 20
        }"#;
        let config: AppConfig =
            serde_json::from_str(legacy).expect("legacy config must deserialize");
        assert_eq!(config.recording_mode, RecordingMode::Dictation);
    }

    #[test]
    fn test_app_config_accepts_legacy_bottom_right_window_key() {
        let config: AppConfig =
            serde_json::from_str(r#"{"recording_window_bottom_right": true}"#).unwrap();

        assert!(config.show_mini_recording_window);
    }

    #[test]
    fn test_stt_provider_type_equality() {
        assert_eq!(SttProviderType::Deepgram, SttProviderType::Deepgram);
        assert_ne!(SttProviderType::Deepgram, SttProviderType::AssemblyAI);
    }

    #[test]
    fn test_stt_config_clone() {
        let config1 = SttConfig::new(SttProviderType::Deepgram).with_language("en");
        let config2 = config1.clone();
        assert_eq!(config1.provider, config2.provider);
        assert_eq!(config1.language, config2.language);
    }

    #[test]
    fn test_app_config_clone() {
        let config1 = AppConfig::default();
        let config2 = config1.clone();
        assert_eq!(config1.recording_hotkey, config2.recording_hotkey);
        assert_eq!(
            config1.microphone_sensitivity,
            config2.microphone_sensitivity
        );
    }
}
