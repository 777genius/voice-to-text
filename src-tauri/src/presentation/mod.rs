/// Presentation layer - Tauri commands, events, and application state
/// This layer handles communication with the frontend
pub mod commands;
#[cfg(all(debug_assertions, feature = "webdriver-e2e"))]
mod e2e_translation;
pub mod events;
#[cfg(all(debug_assertions, feature = "native-window-e2e"))]
pub(crate) mod native_e2e;
pub mod recording_hotkey_gestures;
pub mod recording_intent_coordinator;
pub mod recording_window_lifecycle;
pub mod state;
pub mod system_lifecycle;
pub mod tray;

pub use events::*;
pub use state::AppState;
