//! Knot-owned extension host boundaries.
//!
//! V8 implementation details stay in [`engine`]. The scheduler and lifecycle
//! modules own policy independently of JavaScript mechanics, while [`protocol`]
//! is the typed boundary shared with the application.

pub mod declarations;
pub mod engine;
pub mod lifecycle;
pub(crate) mod module_graph;
pub(crate) mod pool;
pub mod protocol;
pub mod scheduler;
