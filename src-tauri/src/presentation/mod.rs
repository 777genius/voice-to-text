/// Presentation layer - Tauri commands, events, and application state
/// This layer handles communication with the frontend

pub mod commands;
pub mod state;
pub mod events;

pub use state::AppState;
pub use events::*;
