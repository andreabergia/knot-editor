//! Knot-owned scripting runtime boundary.
//!
//! `deno_core` is deliberately contained in this module. The rest of the
//! editor will communicate with it through Knot request/response types rather
//! than through V8 or Deno runtime objects.

use std::{
    cell::RefCell,
    collections::HashSet,
    marker::PhantomData,
    rc::Rc,
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
};

use deno_core::OpState;

pub mod protocol;

use protocol::{ExtensionId, HostOperation, HostRequest, HostResponse, RequestId};

deno_core::extension!(knot_runtime, ops = [op_fixture_shared_host_runtime],);

/// Shared native work available to ops without moving V8 off its extension
/// thread.
#[derive(Clone)]
struct HostAsyncRuntime(tokio::runtime::Handle);

#[deno_core::op2]
#[string]
async fn op_fixture_shared_host_runtime(state: Rc<RefCell<OpState>>) -> String {
    let runtime = state.borrow().borrow::<HostAsyncRuntime>().0.clone();
    runtime
        .spawn(async {
            std::thread::current()
                .name()
                .unwrap_or("unnamed")
                .to_owned()
        })
        .await
        .expect("Knot shared host runtime task panicked")
}

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

    /// Start one extension-owned runtime thread.
    ///
    /// The thread owns its `JsRuntime` and all extension-local state. The
    /// initial scripts are host fixtures; module loading follows in a later
    /// Step 7 slice.
    pub fn spawn_extension(&self, extension: ExtensionId) -> ExtensionRuntimeHandle {
        ExtensionRuntimeHandle::spawn(extension, Arc::clone(&self.async_runtime))
    }
}

/// Host-side control endpoint for one extension runtime.
///
/// `ExtensionRuntimeHandle` never exposes the thread-owned state directly. The
/// foreground host can receive requests and return replies, while request-id
/// allocation and response validation stay on the extension's dedicated
/// thread.
pub struct ExtensionRuntimeHandle {
    extension: ExtensionId,
    commands: Sender<RuntimeCommand>,
    requests: Receiver<HostRequest>,
    thread: Option<JoinHandle<()>>,
}

impl ExtensionRuntimeHandle {
    fn spawn(extension: ExtensionId, async_runtime: Arc<tokio::runtime::Runtime>) -> Self {
        let (commands, command_receiver) = mpsc::channel();
        let (request_sender, requests) = mpsc::channel();
        let thread = thread::Builder::new()
            .name(format!("knot-extension-{}", extension.value()))
            .spawn(move || {
                ExtensionRuntime::new(extension, request_sender, async_runtime)
                    .run(command_receiver)
            })
            .expect("Knot extension runtime thread construction failed");

        Self {
            extension,
            commands,
            requests,
            thread: Some(thread),
        }
    }

    /// Ask the extension thread to issue one typed host request.
    pub fn request(&self, operation: HostOperation) -> Result<(), ExtensionRuntimeClosed> {
        self.commands
            .send(RuntimeCommand::Request(operation))
            .map_err(|_| ExtensionRuntimeClosed)
    }

    /// Execute a fixture script inside this extension's thread-affine
    /// runtime. This is intentionally not an extension-loading API.
    pub fn execute_fixture_script(
        &self,
        name: impl Into<String>,
        source: impl Into<String>,
    ) -> Result<(), ExtensionRuntimeExecutionError> {
        let (completion, completed) = mpsc::sync_channel(0);
        self.commands
            .send(RuntimeCommand::ExecuteFixtureScript {
                name: name.into(),
                source: source.into(),
                completion,
            })
            .map_err(|_| ExtensionRuntimeExecutionError::Closed)?;
        completed
            .recv()
            .unwrap_or(Err(ExtensionRuntimeExecutionError::Closed))
    }

    /// Receive the next request emitted by the extension thread.
    pub fn receive_request(&self) -> Option<HostRequest> {
        self.requests.recv().ok()
    }

    /// Return a host response to the extension thread that issued it.
    pub fn respond(&self, response: HostResponse) -> Result<(), ExtensionRuntimeResponseError> {
        if response.extension != self.extension {
            return Err(ExtensionRuntimeResponseError::WrongExtension);
        }

        let (completion, completed) = mpsc::sync_channel(0);
        self.commands
            .send(RuntimeCommand::Response {
                response,
                completion,
            })
            .map_err(|_| ExtensionRuntimeResponseError::Closed)?;
        completed
            .recv()
            .unwrap_or(Err(ExtensionRuntimeResponseError::Closed))
    }

    /// Stop the extension thread and wait for its thread-affine state to drop.
    pub fn shutdown(mut self) {
        let _ = self.commands.send(RuntimeCommand::Shutdown);
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .expect("Knot extension runtime thread panicked");
        }
    }
}

impl Drop for ExtensionRuntimeHandle {
    fn drop(&mut self) {
        let _ = self.commands.send(RuntimeCommand::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Returned when an operation targets a runtime whose extension thread ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExtensionRuntimeClosed;

/// Returned when a host reply does not belong to this runtime's pending work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtensionRuntimeResponseError {
    Closed,
    WrongExtension,
    UnknownRequest,
}

/// Returned when a fixture script cannot run in an extension runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtensionRuntimeExecutionError {
    Closed,
    JavaScriptException,
}

enum RuntimeCommand {
    Request(HostOperation),
    ExecuteFixtureScript {
        name: String,
        source: String,
        completion: mpsc::SyncSender<Result<(), ExtensionRuntimeExecutionError>>,
    },
    Response {
        response: HostResponse,
        completion: mpsc::SyncSender<Result<(), ExtensionRuntimeResponseError>>,
    },
    Shutdown,
}

/// State confined to one extension's OS thread.
///
/// `Rc` makes that confinement explicit: when this grows to hold
/// `deno_core::JsRuntime`, neither Rust's type system nor this host endpoint
/// can accidentally move it to another thread.
struct ExtensionRuntime {
    extension: ExtensionId,
    next_request_id: u64,
    pending_requests: HashSet<RequestId>,
    request_sender: Sender<HostRequest>,
    event_loop_runtime: tokio::runtime::Runtime,
    js_runtime: deno_core::JsRuntime,
    _thread_affine: PhantomData<Rc<()>>,
}

impl ExtensionRuntime {
    fn new(
        extension: ExtensionId,
        request_sender: Sender<HostRequest>,
        async_runtime: Arc<tokio::runtime::Runtime>,
    ) -> Self {
        let js_runtime = deno_core::JsRuntime::new(deno_core::RuntimeOptions {
            extensions: vec![knot_runtime::init()],
            ..Default::default()
        });
        js_runtime
            .op_state()
            .borrow_mut()
            .put(HostAsyncRuntime(async_runtime.handle().clone()));

        Self {
            extension,
            next_request_id: 0,
            pending_requests: HashSet::new(),
            request_sender,
            event_loop_runtime: tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("Knot extension Tokio runtime construction failed"),
            js_runtime,
            _thread_affine: PhantomData,
        }
    }

    fn run(mut self, commands: Receiver<RuntimeCommand>) {
        while let Ok(command) = commands.recv() {
            match command {
                RuntimeCommand::Request(operation) => {
                    self.next_request_id = self
                        .next_request_id
                        .checked_add(1)
                        .expect("extension request id overflowed");
                    let request = HostRequest {
                        extension: self.extension,
                        id: RequestId::new(self.next_request_id),
                        operation,
                    };
                    self.pending_requests.insert(request.id);
                    if self.request_sender.send(request).is_err() {
                        break;
                    }
                }
                RuntimeCommand::ExecuteFixtureScript {
                    name,
                    source,
                    completion,
                } => {
                    let _runtime_guard = self.event_loop_runtime.enter();
                    let result = self
                        .js_runtime
                        .execute_script(name, source)
                        .map_err(|_| ExtensionRuntimeExecutionError::JavaScriptException)
                        .and_then(|_| {
                            self.event_loop_runtime
                                .block_on(self.js_runtime.run_event_loop(Default::default()))
                                .map_err(|_| ExtensionRuntimeExecutionError::JavaScriptException)
                        });
                    let _ = completion.send(result);
                }
                RuntimeCommand::Response {
                    response,
                    completion,
                } => {
                    debug_assert_eq!(response.extension, self.extension);
                    let result = if self.pending_requests.remove(&response.id) {
                        // Promise resolution is added with the JsRuntime driver.
                        Ok(())
                    } else {
                        Err(ExtensionRuntimeResponseError::UnknownRequest)
                    };
                    let _ = completion.send(result);
                }
                RuntimeCommand::Shutdown => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ExtensionRuntimeExecutionError, ExtensionRuntimeResponseError, V8Host};
    use crate::host::protocol::{
        ExtensionId, HostOperation, HostResponse, HostResponseValue, RequestId,
    };

    #[test]
    fn initializes_v8_and_the_shared_async_runtime() {
        let host = V8Host::new();
        host.async_runtime.block_on(async {
            assert!(tokio::runtime::Handle::try_current().is_ok());
        });
    }

    #[test]
    fn extension_runtime_allocates_requests_on_its_dedicated_thread() {
        let host = V8Host::new();
        let extension = ExtensionId::new(7);
        let runtime = host.spawn_extension(extension);

        runtime.request(HostOperation::ActiveBuffer).unwrap();
        runtime.request(HostOperation::ActiveBuffer).unwrap();

        let first = runtime.receive_request().unwrap();
        let second = runtime.receive_request().unwrap();
        assert_eq!(first.extension, extension);
        assert_eq!(first.id, RequestId::new(1));
        assert_eq!(second.id, RequestId::new(2));

        runtime
            .respond(HostResponse {
                extension,
                id: first.id,
                result: Ok(HostResponseValue::ActiveBuffer(None)),
            })
            .unwrap();
        runtime.shutdown();
    }

    #[test]
    fn extension_runtime_owns_and_drives_one_javascript_isolate() {
        let host = V8Host::new();
        let runtime = host.spawn_extension(ExtensionId::new(7));

        runtime
            .execute_fixture_script(
                "initialize.js",
                "globalThis.runs = 1; Deno.core.ops.op_void_async_deferred().then(() => globalThis.runs = 2)",
            )
            .unwrap();
        runtime
            .execute_fixture_script(
                "verify.js",
                "if (globalThis.runs !== 2) throw new Error('event loop was not driven')",
            )
            .unwrap();

        assert_eq!(
            runtime.execute_fixture_script("failure.js", "throw new Error('expected')"),
            Err(ExtensionRuntimeExecutionError::JavaScriptException)
        );
        runtime.shutdown();
    }

    #[test]
    fn extension_async_op_runs_native_work_on_the_shared_host_runtime() {
        let host = V8Host::new();
        let runtime = host.spawn_extension(ExtensionId::new(7));

        runtime
            .execute_fixture_script(
                "shared-runtime.js",
                r#"
                    Deno.core.ops.op_fixture_shared_host_runtime()
                        .then((threadName) => globalThis.hostThread = threadName)
                "#,
            )
            .unwrap();
        runtime
            .execute_fixture_script(
                "verify-shared-runtime.js",
                r#"
                    if (!globalThis.hostThread.startsWith('knot-host')) {
                        throw new Error(`native work ran on ${globalThis.hostThread}`)
                    }
                "#,
            )
            .unwrap();
        runtime.shutdown();
    }

    #[test]
    fn javascript_exception_does_not_poison_the_extension_runtime() {
        let host = V8Host::new();
        let runtime = host.spawn_extension(ExtensionId::new(7));

        assert_eq!(
            runtime.execute_fixture_script("failure.js", "throw new Error('expected')"),
            Err(ExtensionRuntimeExecutionError::JavaScriptException)
        );
        runtime
            .execute_fixture_script("recovery.js", "globalThis.recovered = true")
            .unwrap();
        runtime.shutdown();
    }

    #[test]
    fn extension_runtime_rejects_a_response_for_another_extension() {
        let host = V8Host::new();
        let runtime = host.spawn_extension(ExtensionId::new(7));

        let result = runtime.respond(HostResponse {
            extension: ExtensionId::new(8),
            id: RequestId::new(1),
            result: Ok(HostResponseValue::ActiveBuffer(None)),
        });

        assert_eq!(result, Err(ExtensionRuntimeResponseError::WrongExtension));
    }

    #[test]
    fn extension_runtime_rejects_a_response_for_an_unknown_request() {
        let host = V8Host::new();
        let extension = ExtensionId::new(7);
        let runtime = host.spawn_extension(extension);

        let result = runtime.respond(HostResponse {
            extension,
            id: RequestId::new(1),
            result: Ok(HostResponseValue::ActiveBuffer(None)),
        });

        assert_eq!(result, Err(ExtensionRuntimeResponseError::UnknownRequest));
    }
}
