/// Presentation layer - Tauri commands, events, and application state
/// This layer handles communication with the frontend
pub mod commands;
#[cfg(debug_assertions)]
mod e2e_translation;
pub mod events;
pub mod state;
pub mod tray;

pub use events::*;
pub use state::AppState;
