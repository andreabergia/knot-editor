//! Knot-owned scripting runtime boundary.
//!
//! `deno_core` is deliberately contained in this module. The rest of the
//! editor will communicate with it through Knot request/response types rather
//! than through V8 or Deno runtime objects.

use std::sync::Arc;

pub mod protocol;

/// Process-wide owner of the V8 platform and host asynchronous work.
///
/// Construct this on the process's parent thread before loading any
/// extensions. `JsRuntime::init_platform` is idempotent, but V8 itself is a
/// process-global resource, so `V8Host` intentionally does not offer a
/// per-extension initializer.
pub struct V8Host {
    #[allow(
        dead_code,
        reason = "typed host requests consume this in the next Step 7 slice"
    )]
    async_runtime: Arc<tokio::runtime::Runtime>,
}

impl V8Host {
    /// Initialize V8 and create Knot's shared runtime for host-side async
    /// work. JavaScript will remain confined to extension-owned threads.
    pub fn new() -> Self {
        deno_core::JsRuntime::init_platform(None);

        let async_runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_time()
            .thread_name("knot-host")
            .build()
            .expect("Knot host Tokio runtime construction failed");

        Self {
            async_runtime: Arc::new(async_runtime),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::V8Host;

    #[test]
    fn initializes_v8_and_the_shared_async_runtime() {
        let host = V8Host::new();
        host.async_runtime.block_on(async {
            assert!(tokio::runtime::Handle::try_current().is_ok());
        });
    }
}
