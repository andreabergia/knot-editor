//! Knot editor library.

mod app;
pub mod core;
mod host;

pub use app::run;
/// Host operation declarations linked into the product library.
pub use host::declarations::registry as host_operation_registry;
