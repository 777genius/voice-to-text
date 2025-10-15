/// Domain layer - contains business logic, entities, and interfaces (ports)
/// This layer is independent of external concerns and frameworks

pub mod models;
pub mod ports;

pub use models::*;
pub use ports::*;
