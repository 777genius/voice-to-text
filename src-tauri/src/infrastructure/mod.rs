/// Infrastructure layer - contains concrete implementations of domain interfaces
/// This layer depends on domain layer but is independent of application layer

pub mod stt;
pub mod audio;
pub mod factory;
pub mod config_store;
pub mod updater;
pub mod models;

pub use factory::*;
pub use config_store::ConfigStore;
