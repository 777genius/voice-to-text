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
}

impl Default for SttProviderType {
    fn default() -> Self {
        Self::Deepgram
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

    /// API key for cloud providers
    pub api_key: Option<String>,

    /// Model name/ID for local providers
    pub model: Option<String>,

    /// Keep WebSocket connection alive between recording sessions (only for providers that support it)
    /// Deepgram: safe (bills by audio duration, not connection time)
    /// AssemblyAI: dangerous (bills by connection time)
    pub keep_connection_alive: bool,
}

impl Default for SttConfig {
    fn default() -> Self {
        Self {
            provider: SttProviderType::default(),
            language: "ru".to_string(),
            auto_detect_language: false,
            enable_punctuation: true,
            filter_profanity: false,
            api_key: None,
            model: None,
            keep_connection_alive: false, // Безопасно по умолчанию для всех провайдеров
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

    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
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

    /// Auto-close window after transcription
    pub auto_close_window: bool,

    /// VAD silence timeout in milliseconds
    pub vad_silence_timeout_ms: u64,

    /// Microphone sensitivity (0-200, default 95)
    /// 0-100: Controls noise gate threshold
    /// 100-200: Maximum sensitivity (threshold = 0, passes all audio)
    /// Higher = more sensitive (picks up quieter sounds)
    /// Lower = less sensitive (only loud sounds)
    pub microphone_sensitivity: u8,

    /// Keep history of transcriptions
    pub keep_history: bool,

    /// Maximum number of history items
    pub max_history_items: usize,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            stt: SttConfig::default(),
            recording_hotkey: "CmdOrCtrl+Shift+X".to_string(), // Кроссплатформенная комбинация
            auto_copy_to_clipboard: true,
            auto_close_window: true,
            vad_silence_timeout_ms: 3000, // 3 секунды тишины перед авто-остановкой
            microphone_sensitivity: 95, // Очень высокая чувствительность по умолчанию (порог ~1638)
            keep_history: true,
            max_history_items: 20,
        }
    }
}
