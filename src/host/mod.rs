//! Knot-owned extension host boundaries.
//!
//! V8 implementation details stay in [`engine`]. The scheduler and lifecycle
//! modules own policy independently of JavaScript mechanics, while [`protocol`]
//! is the typed boundary shared with the application.

pub mod bench;
#[allow(
    dead_code,
    reason = "low-level V8 probes validate the engine beneath the product-facing pool"
)]
pub mod engine;
pub mod lifecycle;
pub(crate) mod pool;
pub mod protocol;
pub mod scheduler;
