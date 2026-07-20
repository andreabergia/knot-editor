//! Knot-owned scripting runtime boundary.
//!
//! `deno_core` is deliberately contained in this module. The rest of the
//! editor will communicate with it through Knot request/response types rather
//! than through V8 or Deno runtime objects.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    marker::PhantomData,
    rc::Rc,
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
};

use deno_core::{ModuleLoadOptions, ModuleLoadReferrer, ModuleLoadResponse, ModuleLoader};
use deno_core::{ModuleResolveResponse, ModuleSource, ModuleSourceCode, ModuleSpecifier, OpState};
use deno_error::JsErrorBox;

pub mod protocol;

use protocol::{ExtensionId, HostOperation, HostRequest, HostResponse, RequestId};

const PRIVATE_BOOTSTRAP_SPECIFIER: &str = "knot:bootstrap";
const PUBLIC_FACADE_SPECIFIER: &str = "knot:editor";
const PRIVATE_BOOTSTRAP_SOURCE: &str = r#"
const nativeOps = Deno.core.ops;

export function activeBuffer() {
  return nativeOps.op_fixture_active_buffer();
}
"#;
const PUBLIC_FACADE_SOURCE: &str = r#"
import { activeBuffer } from "knot:bootstrap";

export const editor = {
  activeBuffer,
};
"#;

/// Static source storage for the prototype's fixture-only module loader.
///
/// Sources enter this map only through an extension runtime command before
/// evaluation. There is deliberately no filesystem or package resolution in
/// this slice.
#[derive(Clone, Default)]
struct FixtureModuleLoader {
    sources: Arc<std::sync::Mutex<HashMap<ModuleSpecifier, String>>>,
}

impl FixtureModuleLoader {
    fn with_private_bootstrap() -> Self {
        let loader = Self::default();
        loader.insert(
            ModuleSpecifier::parse(PRIVATE_BOOTSTRAP_SPECIFIER)
                .expect("Knot private bootstrap specifier must be valid"),
            PRIVATE_BOOTSTRAP_SOURCE.into(),
        );
        loader.insert(
            ModuleSpecifier::parse(PUBLIC_FACADE_SPECIFIER)
                .expect("Knot public facade specifier must be valid"),
            PUBLIC_FACADE_SOURCE.into(),
        );
        loader
    }

    fn insert(&self, specifier: ModuleSpecifier, source: String) {
        self.sources
            .lock()
            .expect("Knot fixture module source lock poisoned")
            .insert(specifier, source);
    }
}

impl ModuleLoader for FixtureModuleLoader {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        _kind: deno_core::ResolutionKind,
    ) -> ModuleResolveResponse {
        if specifier == PRIVATE_BOOTSTRAP_SPECIFIER
            && referrer != "."
            && referrer != PUBLIC_FACADE_SPECIFIER
        {
            return Err(JsErrorBox::generic(
                "Knot private bootstrap bindings are not importable by extensions",
            ));
        }
        deno_core::resolve_import(specifier, referrer).map_err(JsErrorBox::from_err)
    }

    fn load(
        &self,
        specifier: &ModuleSpecifier,
        _referrer: Option<&ModuleLoadReferrer>,
        _options: ModuleLoadOptions,
    ) -> ModuleLoadResponse {
        let source = self
            .sources
            .lock()
            .expect("Knot fixture module source lock poisoned")
            .get(specifier)
            .cloned()
            .ok_or_else(|| {
                JsErrorBox::generic(format!("Knot fixture module not found: {specifier}"))
            });

        ModuleLoadResponse::Sync(source.map(|source| {
            ModuleSource::new(
                deno_core::ModuleType::JavaScript,
                ModuleSourceCode::String(source.into()),
                specifier,
                None,
            )
        }))
    }
}

deno_core::extension!(
    knot_runtime,
    ops = [op_fixture_active_buffer, op_fixture_shared_host_runtime],
);

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

#[deno_core::op2]
async fn op_fixture_active_buffer(state: Rc<RefCell<OpState>>) -> Result<bool, JsErrorBox> {
    let response = state
        .borrow_mut()
        .borrow_mut::<ExtensionRequestRouter>()
        .request_active_buffer()
        .map_err(|_| JsErrorBox::generic("Knot host closed the active-buffer request"))?;
    let response = response
        .await
        .map_err(|_| JsErrorBox::generic("Knot host closed the active-buffer request"))?;

    match response.result {
        Ok(protocol::HostResponseValue::ActiveBuffer(buffer)) => Ok(buffer.is_some()),
        Ok(_) => Err(JsErrorBox::generic(
            "Knot host returned the wrong response type",
        )),
        Err(error) => Err(JsErrorBox::generic(format!(
            "Knot host rejected active-buffer request: {error:?}"
        ))),
    }
}

/// Process-wide owner of the V8 platform and host asynchronous work.
///
/// Construct this on the process's parent thread before loading any
/// extensions. `JsRuntime::init_platform` is idempotent, but V8 itself is a
/// process-global resource, so `V8Host` intentionally does not offer a
/// per-extension initializer.
pub struct V8Host {
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
    lifecycle: Arc<ExtensionLifecycle>,
    thread: Option<JoinHandle<()>>,
}

impl ExtensionRuntimeHandle {
    fn spawn(extension: ExtensionId, async_runtime: Arc<tokio::runtime::Runtime>) -> Self {
        let (commands, command_receiver) = mpsc::channel();
        let (request_sender, requests) = mpsc::channel();
        let lifecycle = Arc::new(ExtensionLifecycle::default());
        let extension_lifecycle = Arc::clone(&lifecycle);
        let teardown_lifecycle = Arc::clone(&lifecycle);
        let thread = thread::Builder::new()
            .name(format!("knot-extension-{}", extension.value()))
            .spawn(move || {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    ExtensionRuntime::new(
                        extension,
                        request_sender,
                        extension_lifecycle,
                        async_runtime,
                    )
                    .run(command_receiver)
                }));
                teardown_lifecycle.teardown();
            })
            .expect("Knot extension runtime thread construction failed");

        Self {
            extension,
            commands,
            requests,
            lifecycle,
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

    /// Load and evaluate one fixture ES module from the prototype's static
    /// in-memory module set. `specifier` must be an absolute URL.
    pub fn execute_fixture_module(
        &self,
        specifier: impl Into<String>,
        source: impl Into<String>,
    ) -> Result<(), ExtensionRuntimeExecutionError> {
        let (completion, completed) = mpsc::sync_channel(0);
        self.commands
            .send(RuntimeCommand::ExecuteFixtureModule {
                specifier: specifier.into(),
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

        self.lifecycle.respond(response)
    }

    /// Stop the extension thread and wait for its thread-affine state to drop.
    pub fn shutdown(mut self) {
        self.lifecycle.teardown();
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
        self.lifecycle.teardown();
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExtensionRuntimeExecutionError {
    Closed,
    InvalidModuleSpecifier,
    JavaScriptException { report: String },
}

impl ExtensionRuntimeExecutionError {
    fn javascript_exception(error: impl std::fmt::Display) -> Self {
        Self::JavaScriptException {
            report: format!("{error:#}"),
        }
    }
}

enum RuntimeCommand {
    Request(HostOperation),
    ExecuteFixtureScript {
        name: String,
        source: String,
        completion: mpsc::SyncSender<Result<(), ExtensionRuntimeExecutionError>>,
    },
    ExecuteFixtureModule {
        specifier: String,
        source: String,
        completion: mpsc::SyncSender<Result<(), ExtensionRuntimeExecutionError>>,
    },
    Shutdown,
}

/// Extension-owned resources which must not outlive its isolate.
///
/// This is the common teardown point for normal unload and failed runtime
/// initialization. Pending JavaScript promises are the only registered
/// extension resource in this slice; command registrations, subscriptions,
/// and queued callbacks will register here as those APIs are introduced.
#[derive(Default)]
struct ExtensionLifecycle {
    state: std::sync::Mutex<ExtensionLifecycleState>,
}

#[derive(Default)]
struct ExtensionLifecycleState {
    torn_down: bool,
    pending_requests: PendingRequests,
}

impl ExtensionLifecycle {
    fn register_manual(&self, id: RequestId) -> Result<(), ExtensionRuntimeClosed> {
        let mut state = self
            .state
            .lock()
            .expect("Knot extension lifecycle lock poisoned");
        if state.torn_down {
            return Err(ExtensionRuntimeClosed);
        }
        state.pending_requests.manual.insert(id);
        Ok(())
    }

    fn register_javascript(
        &self,
        id: RequestId,
        sender: tokio::sync::oneshot::Sender<HostResponse>,
    ) -> Result<(), ExtensionRuntimeClosed> {
        let mut state = self
            .state
            .lock()
            .expect("Knot extension lifecycle lock poisoned");
        if state.torn_down {
            return Err(ExtensionRuntimeClosed);
        }
        state.pending_requests.javascript.insert(id, sender);
        Ok(())
    }

    fn respond(&self, response: HostResponse) -> Result<(), ExtensionRuntimeResponseError> {
        let mut state = self
            .state
            .lock()
            .expect("Knot extension lifecycle lock poisoned");
        if state.torn_down {
            return Err(ExtensionRuntimeResponseError::Closed);
        }
        state.pending_requests.respond(response)
    }

    fn teardown(&self) {
        let mut state = self
            .state
            .lock()
            .expect("Knot extension lifecycle lock poisoned");
        if state.torn_down {
            return;
        }
        state.torn_down = true;
        state.pending_requests = PendingRequests::default();
    }
}

#[derive(Default)]
struct PendingRequests {
    manual: HashSet<RequestId>,
    javascript: HashMap<RequestId, tokio::sync::oneshot::Sender<HostResponse>>,
}

impl PendingRequests {
    fn respond(&mut self, response: HostResponse) -> Result<(), ExtensionRuntimeResponseError> {
        if let Some(sender) = self.javascript.remove(&response.id) {
            sender
                .send(response)
                .map_err(|_| ExtensionRuntimeResponseError::UnknownRequest)
        } else if self.manual.remove(&response.id) {
            Ok(())
        } else {
            Err(ExtensionRuntimeResponseError::UnknownRequest)
        }
    }
}

/// Extension-thread request allocator and JavaScript-promise response router.
struct ExtensionRequestRouter {
    extension: ExtensionId,
    next_request_id: u64,
    request_sender: Sender<HostRequest>,
    lifecycle: Arc<ExtensionLifecycle>,
}

impl ExtensionRequestRouter {
    fn next_request_id(&mut self) -> RequestId {
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .expect("extension request id overflowed");
        RequestId::new(self.next_request_id)
    }

    fn send(&self, id: RequestId, operation: HostOperation) -> Result<(), ExtensionRuntimeClosed> {
        self.request_sender
            .send(HostRequest {
                extension: self.extension,
                id,
                operation,
            })
            .map_err(|_| ExtensionRuntimeClosed)
    }

    fn request_manual(&mut self, operation: HostOperation) -> Result<(), ExtensionRuntimeClosed> {
        let id = self.next_request_id();
        self.lifecycle.register_manual(id)?;
        self.send(id, operation)
    }

    fn request_active_buffer(
        &mut self,
    ) -> Result<tokio::sync::oneshot::Receiver<HostResponse>, ExtensionRuntimeClosed> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let id = self.next_request_id();
        self.lifecycle.register_javascript(id, sender)?;
        self.send(id, HostOperation::ActiveBuffer)?;
        Ok(receiver)
    }
}

/// State confined to one extension's OS thread.
///
/// `Rc` makes that confinement explicit: when this grows to hold
/// `deno_core::JsRuntime`, neither Rust's type system nor this host endpoint
/// can accidentally move it to another thread.
struct ExtensionRuntime {
    event_loop_runtime: tokio::runtime::Runtime,
    js_runtime: deno_core::JsRuntime,
    fixture_modules: FixtureModuleLoader,
    _thread_affine: PhantomData<Rc<()>>,
}

impl ExtensionRuntime {
    fn new(
        extension: ExtensionId,
        request_sender: Sender<HostRequest>,
        lifecycle: Arc<ExtensionLifecycle>,
        async_runtime: Arc<tokio::runtime::Runtime>,
    ) -> Self {
        let fixture_modules = FixtureModuleLoader::with_private_bootstrap();
        let event_loop_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("Knot extension Tokio runtime construction failed");
        let mut js_runtime = deno_core::JsRuntime::new(deno_core::RuntimeOptions {
            extensions: vec![knot_runtime::init()],
            module_loader: Some(Rc::new(fixture_modules.clone())),
            ..Default::default()
        });
        let bootstrap = ModuleSpecifier::parse(PRIVATE_BOOTSTRAP_SPECIFIER)
            .expect("Knot private bootstrap specifier must be valid");
        let bootstrap_module = event_loop_runtime
            .block_on(js_runtime.load_side_es_module(&bootstrap))
            .expect("Knot private bootstrap module must load");
        let bootstrap_evaluation = js_runtime.mod_evaluate(bootstrap_module);
        event_loop_runtime
            .block_on(js_runtime.run_event_loop(Default::default()))
            .expect("Knot private bootstrap module must evaluate");
        event_loop_runtime
            .block_on(bootstrap_evaluation)
            .expect("Knot private bootstrap module evaluation must succeed");
        let facade = ModuleSpecifier::parse(PUBLIC_FACADE_SPECIFIER)
            .expect("Knot public facade specifier must be valid");
        let facade_module = event_loop_runtime
            .block_on(js_runtime.load_side_es_module(&facade))
            .expect("Knot public facade module must load");
        let facade_evaluation = js_runtime.mod_evaluate(facade_module);
        event_loop_runtime
            .block_on(js_runtime.run_event_loop(Default::default()))
            .expect("Knot public facade module must evaluate");
        event_loop_runtime
            .block_on(facade_evaluation)
            .expect("Knot public facade module evaluation must succeed");
        js_runtime
            .execute_script("knot:remove-private-globals", "delete globalThis.Deno;")
            .expect("Knot must remove private Deno bindings before extension code runs");
        js_runtime
            .op_state()
            .borrow_mut()
            .put(HostAsyncRuntime(async_runtime.handle().clone()));
        js_runtime
            .op_state()
            .borrow_mut()
            .put(ExtensionRequestRouter {
                extension,
                next_request_id: 0,
                request_sender,
                lifecycle,
            });

        Self {
            event_loop_runtime,
            js_runtime,
            fixture_modules,
            _thread_affine: PhantomData,
        }
    }

    fn run(mut self, commands: Receiver<RuntimeCommand>) {
        while let Ok(command) = commands.recv() {
            match command {
                RuntimeCommand::Request(operation) => {
                    if self
                        .js_runtime
                        .op_state()
                        .borrow_mut()
                        .borrow_mut::<ExtensionRequestRouter>()
                        .request_manual(operation)
                        .is_err()
                    {
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
                        .map_err(ExtensionRuntimeExecutionError::javascript_exception)
                        .and_then(|_| {
                            self.event_loop_runtime
                                .block_on(self.js_runtime.run_event_loop(Default::default()))
                                .map_err(ExtensionRuntimeExecutionError::javascript_exception)
                        });
                    let _ = completion.send(result);
                }
                RuntimeCommand::ExecuteFixtureModule {
                    specifier,
                    source,
                    completion,
                } => {
                    let _runtime_guard = self.event_loop_runtime.enter();
                    let result = ModuleSpecifier::parse(&specifier)
                        .map_err(|_| ExtensionRuntimeExecutionError::InvalidModuleSpecifier)
                        .and_then(|specifier| {
                            self.fixture_modules.insert(specifier.clone(), source);
                            self.event_loop_runtime
                                .block_on(self.js_runtime.load_main_es_module(&specifier))
                                .map_err(ExtensionRuntimeExecutionError::javascript_exception)
                        })
                        .and_then(|module_id| {
                            let evaluation = self.js_runtime.mod_evaluate(module_id);
                            self.event_loop_runtime
                                .block_on(self.js_runtime.run_event_loop(Default::default()))
                                .map_err(ExtensionRuntimeExecutionError::javascript_exception)?;
                            self.event_loop_runtime
                                .block_on(evaluation)
                                .map_err(ExtensionRuntimeExecutionError::javascript_exception)
                        });
                    let _ = completion.send(result);
                }
                RuntimeCommand::Shutdown => break,
            }
        }
        self.js_runtime
            .op_state()
            .borrow()
            .borrow::<ExtensionRequestRouter>()
            .lifecycle
            .teardown();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::{
        ExtensionLifecycle, ExtensionRuntimeClosed, ExtensionRuntimeExecutionError,
        ExtensionRuntimeResponseError, RuntimeCommand, V8Host,
    };
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
                "globalThis.runs = 1; Promise.resolve().then(() => globalThis.runs = 2)",
            )
            .unwrap();
        runtime
            .execute_fixture_script(
                "verify.js",
                "if (globalThis.runs !== 2) throw new Error('event loop was not driven')",
            )
            .unwrap();

        let error = runtime
            .execute_fixture_script("failure.js", "throw new Error('expected')")
            .unwrap_err();
        let ExtensionRuntimeExecutionError::JavaScriptException { report } = error else {
            panic!("expected JavaScript exception");
        };
        assert!(report.contains("Error: expected"));
        assert!(report.contains("failure.js:1:7"));
        runtime.shutdown();
    }

    #[test]
    fn extension_runtime_loads_static_fixture_modules() {
        let host = V8Host::new();
        let runtime = host.spawn_extension(ExtensionId::new(7));

        runtime
            .execute_fixture_module(
                "file:///fixtures/private-import.js",
                r#"
                    import { activeBuffer } from "knot:bootstrap";
                    globalThis.fixtureModuleLoaded = 7;
                "#,
            )
            .unwrap_err();
        runtime
            .execute_fixture_module(
                "file:///fixtures/extension.js",
                "globalThis.fixtureModuleLoaded = 7",
            )
            .unwrap();
        runtime
            .execute_fixture_script(
                "verify-module.js",
                "if (globalThis.fixtureModuleLoaded !== 7) throw new Error('module did not run')",
            )
            .unwrap();

        assert_eq!(
            runtime.execute_fixture_module("not a URL", ""),
            Err(ExtensionRuntimeExecutionError::InvalidModuleSpecifier)
        );
        runtime.shutdown();
    }

    #[test]
    fn public_facade_resolves_a_host_request_without_exposing_deno() {
        let host = V8Host::new();
        let extension = ExtensionId::new(7);
        let runtime = host.spawn_extension(extension);
        let (completion, completed) = mpsc::sync_channel(0);

        runtime
            .commands
            .send(RuntimeCommand::ExecuteFixtureModule {
                specifier: "file:///fixtures/active-buffer.js".into(),
                source: r#"
                    import { editor } from "knot:editor";

                    if (typeof Deno !== "undefined") {
                        throw new Error("extension can access Deno");
                    }
                    editor.activeBuffer()
                        .then((hasBuffer) => globalThis.hasBuffer = hasBuffer)
                "#
                .into(),
                completion,
            })
            .unwrap();

        let request = runtime.receive_request().unwrap();
        assert_eq!(request.extension, extension);
        assert_eq!(request.operation, HostOperation::ActiveBuffer);
        runtime
            .respond(HostResponse {
                extension,
                id: request.id,
                result: Ok(HostResponseValue::ActiveBuffer(None)),
            })
            .unwrap();
        completed.recv().unwrap().unwrap();

        runtime
            .execute_fixture_script(
                "verify-active-buffer.js",
                "if (globalThis.hasBuffer !== false) throw new Error('unexpected active buffer')",
            )
            .unwrap();
        runtime.shutdown();
    }

    #[test]
    fn javascript_rejection_reports_its_source_without_poisoning_the_runtime() {
        let host = V8Host::new();
        let runtime = host.spawn_extension(ExtensionId::new(7));

        let error = runtime
            .execute_fixture_script("rejection.js", "Promise.reject(new Error('expected'))")
            .unwrap_err();
        let ExtensionRuntimeExecutionError::JavaScriptException { report } = error else {
            panic!("expected JavaScript rejection");
        };
        assert!(report.contains("Error: expected"));
        assert!(report.contains("rejection.js:1:16"));
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

    #[test]
    fn lifecycle_teardown_cancels_pending_promises_idempotently() {
        let lifecycle = ExtensionLifecycle::default();
        let extension = ExtensionId::new(7);
        let (sender, mut receiver) = tokio::sync::oneshot::channel();

        lifecycle
            .register_javascript(RequestId::new(1), sender)
            .unwrap();
        lifecycle.teardown();
        lifecycle.teardown();

        assert_eq!(
            receiver.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        );
        assert_eq!(
            lifecycle.respond(HostResponse {
                extension,
                id: RequestId::new(1),
                result: Ok(HostResponseValue::ActiveBuffer(None)),
            }),
            Err(ExtensionRuntimeResponseError::Closed)
        );
        assert_eq!(
            lifecycle.register_manual(RequestId::new(2)),
            Err(ExtensionRuntimeClosed)
        );
    }
}
