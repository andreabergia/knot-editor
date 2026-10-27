//! Knot editor library.

mod app;
mod core;
mod host;

pub use app::run;
/// Handler declarations, for the linked-inventory test and declaration doc tests.
#[doc(hidden)]
pub use host::declarations;
