//! V8 process initialization and engine-owned bindings.

use std::sync::Once;

use super::scheduler::WorkerThreadPermit;

static V8_INITIALIZATION: Once = Once::new();

/// Initializes V8 before any extension worker thread is created.
pub fn initialize() -> WorkerThreadPermit {
    V8_INITIALIZATION.call_once(|| {
        let platform = v8::new_default_platform(0, false).make_shared();
        v8::V8::initialize_platform(platform);
        v8::V8::initialize();
    });
    WorkerThreadPermit::new()
}

#[cfg(test)]
mod tests {
    #[test]
    fn process_initialization_is_idempotent() {
        super::initialize();
        super::initialize();
    }
}
