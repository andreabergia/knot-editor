//! Knot-owned extension host boundaries.
//!
//! V8 implementation details stay in [`engine`]. The scheduler and lifecycle
//! modules own policy independently of JavaScript mechanics, while [`protocol`]
//! is the typed boundary shared with the application.

pub mod bench;
#[allow(
    dead_code,
    reason = "the pooled engine is connected to the product bridge in later D017 checkpoints"
)]
pub mod engine;
pub mod lifecycle;
#[allow(
    dead_code,
    reason = "protocol consumers return in later D017 checkpoints"
)]
pub mod protocol;
pub mod scheduler;
