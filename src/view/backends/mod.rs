//! Backend implementations for the benchmark.
//!
//! All backends are compiled in unconditionally: this is a benchmark, not
//! the production release, and skipping feature gating avoids rebuilds
//! when switching backends at runtime via `--backend`.

pub mod stub;
pub mod wgpu_cosmic;

#[cfg(target_os = "macos")]
pub mod skia;
