/// Infrastructure layer - contains concrete implementations of domain interfaces
/// This layer depends on domain layer but is independent of application layer

pub mod stt;
pub mod audio;
pub mod factory;
pub mod config_store;
pub mod updater;
pub mod models;
pub mod embedded_keys; // API ключи встроенные в build
pub mod auto_paste; // Автоматическая вставка текста
pub mod microphone_permission; // Проверка разрешения на микрофон (macOS)
pub mod clipboard; // Кроссплатформенная работа с clipboard
pub mod hotkey; // Нормализация/миграция хоткеев
pub mod auth_store; // Auth session + device_id (Rust SoT)

pub use factory::*;
pub use config_store::ConfigStore;
pub use auth_store::{AuthSession, AuthStore, AuthStoreData, AuthUser};
pub use clipboard::copy_to_clipboard;
