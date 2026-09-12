//! Benchmark ownership for the extension host.
//!
//! Workloads will return as the pooled runtime is implemented.

use anyhow::{Result, bail};

pub fn run(args: impl Iterator<Item = String>) -> Result<()> {
    if let Some(argument) = args.into_iter().next() {
        bail!("unexpected argument while V8 benchmarks are unavailable: {argument}");
    }
    super::engine::initialize();
    println!("V8 initialized; pooled runtime benchmarks are not implemented yet");
    Ok(())
}
