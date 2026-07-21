//! Knot-owned scripting runtime boundary.
//!
//! `deno_core` is deliberately contained in this module. The rest of the
//! editor will communicate with it through Knot request/response types rather
//! than through V8 or Deno runtime objects.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    future::Future,
    marker::PhantomData,
    pin::Pin,
    rc::Rc,
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    task::{Context, Poll},
    thread::{self, JoinHandle},
};

use deno_core::{ModuleLoadOptions, ModuleLoadReferrer, ModuleLoadResponse, ModuleLoader};
use deno_core::{ModuleResolveResponse, ModuleSource, ModuleSourceCode, ModuleSpecifier, OpState};
use deno_error::JsErrorBox;

pub mod protocol;

use protocol::{
    BufferHandle, ByteRange, ExtensionId, HostOperation, HostRequest, HostResponse,
    HostResponseValue, RequestId, TextEdit,
};

const PRIVATE_BOOTSTRAP_SPECIFIER: &str = "knot:bootstrap";
const PUBLIC_FACADE_SPECIFIER: &str = "knot:editor";
const EXTENSION_HEAP_LIMIT_BYTES: usize = 32 * 1024 * 1024;
const PRIVATE_BOOTSTRAP_SOURCE: &str = r#"
const nativeOps = Deno.core.ops;

const buffers = new Map();
const snapshotTables = new WeakMap();

function hostError(error) {
  const names = {
    BufferClosed: "BufferClosedError",
    InvalidRange: "RangeError",
    InvalidEditBatch: "InvalidEditBatchError",
    RevisionConflict: "RevisionConflictError",
  };
  const exception = new Error(error);
  exception.name = names[error] ?? "KnotHostError";
  throw exception;
}

function byteLengthOfCodePoint(codePoint) {
  return codePoint <= 0x7f ? 1 : codePoint <= 0x7ff ? 2 : codePoint <= 0xffff ? 3 : 4;
}

function snapshotTable(snapshot) {
  let table = snapshotTables.get(snapshot);
  if (table) return table;
  const byteAtUtf16 = [0];
  const utf16AtByte = new Map([[0, 0]]);
  let byteOffset = 0;
  let utf16Offset = 0;
  for (const character of snapshot.text) {
    byteOffset += byteLengthOfCodePoint(character.codePointAt(0));
    utf16Offset += character.length;
    byteAtUtf16[utf16Offset] = byteOffset;
    utf16AtByte.set(byteOffset, utf16Offset);
  }
  table = { byteAtUtf16, utf16AtByte };
  snapshotTables.set(snapshot, table);
  return table;
}

function snapshotFromNative(snapshot) {
  const value = {
    text: snapshot.text,
    range: Object.freeze({
      startByteOffset: snapshot.startByteOffset,
      endByteOffset: snapshot.endByteOffset,
    }),
    revision: snapshot.revision,
    byteOffsetAtUtf16(offset) {
      const value = snapshotTable(this).byteAtUtf16[offset];
      if (value === undefined) throw new RangeError("UTF-16 offset splits a surrogate pair or is out of bounds");
      return value;
    },
    utf16OffsetAtByte(offset) {
      const value = snapshotTable(this).utf16AtByte.get(offset);
      if (value === undefined) throw new RangeError("byte offset splits a UTF-8 scalar or is out of bounds");
      return value;
    },
  };
  return Object.freeze(value);
}

function bufferFor(handle) {
  let buffer = buffers.get(handle);
  if (buffer) return buffer;
  buffer = Object.freeze({
    async snapshot(range) {
      const result = await nativeOps.op_buffer_snapshot({ handle, range });
      if (result.error) hostError(result.error);
      return snapshotFromNative(result.snapshot);
    },
    async applyEdits(edits, options) {
      const result = await nativeOps.op_buffer_apply_edits({ handle, edits, ifRevision: options?.ifRevision });
      if (result.error) hostError(result.error);
      return Object.freeze({ revision: result.revision });
    },
  });
  buffers.set(handle, buffer);
  return buffer;
}

export async function activeBuffer() {
  const handle = await nativeOps.op_buffer_active();
  return handle === null ? null : bufferFor(handle);
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
    ops = [
        op_buffer_active,
        op_buffer_snapshot,
        op_buffer_apply_edits,
        op_fixture_shared_host_runtime,
    ],
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

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeSnapshotRequest {
    handle: u64,
    range: Option<NativeByteRange>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeApplyEditsRequest {
    handle: u64,
    edits: Vec<NativeTextEdit>,
    if_revision: u64,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeByteRange {
    start_byte_offset: usize,
    end_byte_offset: usize,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeTextEdit {
    range: NativeByteRange,
    text: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct NativeSnapshotResponse {
    text: String,
    start_byte_offset: usize,
    end_byte_offset: usize,
    revision: u64,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct NativeBufferResponse {
    error: Option<&'static str>,
    snapshot: Option<NativeSnapshotResponse>,
    revision: Option<u64>,
}

#[deno_core::op2]
#[serde]
async fn op_buffer_active(state: Rc<RefCell<OpState>>) -> Result<Option<u64>, JsErrorBox> {
    let response = state
        .borrow_mut()
        .borrow_mut::<ExtensionRequestRouter>()
        .request_javascript(HostOperation::ActiveBuffer)
        .map_err(|_| JsErrorBox::generic("Knot host closed the active-buffer request"))?;
    let response = response
        .await
        .map_err(|_| JsErrorBox::generic("Knot host closed the active-buffer request"))?;

    match response.result {
        Ok(HostResponseValue::ActiveBuffer(buffer)) => Ok(buffer.map(BufferHandle::value)),
        Ok(_) => Err(JsErrorBox::generic(
            "Knot host returned the wrong response type",
        )),
        Err(error) => Err(JsErrorBox::generic(format!(
            "Knot host rejected active-buffer request: {error:?}"
        ))),
    }
}

#[deno_core::op2]
#[serde]
async fn op_buffer_snapshot(
    state: Rc<RefCell<OpState>>,
    #[serde] request: NativeSnapshotRequest,
) -> Result<NativeBufferResponse, JsErrorBox> {
    let range = request.range.map(|range| ByteRange {
        start_byte_offset: range.start_byte_offset,
        end_byte_offset: range.end_byte_offset,
    });
    let response = request_host_operation(
        state,
        HostOperation::Snapshot {
            buffer: BufferHandle::new(request.handle),
            range,
        },
    )
    .await?;
    match response.result {
        Ok(HostResponseValue::Snapshot(snapshot)) => Ok(NativeBufferResponse {
            error: None,
            snapshot: Some(NativeSnapshotResponse {
                text: snapshot.text,
                start_byte_offset: snapshot.range.start_byte_offset,
                end_byte_offset: snapshot.range.end_byte_offset,
                revision: snapshot.revision,
            }),
            revision: None,
        }),
        Ok(_) => Err(JsErrorBox::generic(
            "Knot host returned the wrong response type",
        )),
        Err(error) => Ok(NativeBufferResponse {
            error: Some(host_request_error_name(error)),
            snapshot: None,
            revision: None,
        }),
    }
}

#[deno_core::op2]
#[serde]
async fn op_buffer_apply_edits(
    state: Rc<RefCell<OpState>>,
    #[serde] request: NativeApplyEditsRequest,
) -> Result<NativeBufferResponse, JsErrorBox> {
    let edits = request
        .edits
        .into_iter()
        .map(|edit| TextEdit {
            range: ByteRange {
                start_byte_offset: edit.range.start_byte_offset,
                end_byte_offset: edit.range.end_byte_offset,
            },
            text: edit.text,
        })
        .collect();
    let response = request_host_operation(
        state,
        HostOperation::ApplyEdits {
            buffer: BufferHandle::new(request.handle),
            edits,
            if_revision: request.if_revision,
        },
    )
    .await?;
    match response.result {
        Ok(HostResponseValue::AppliedEdits { revision }) => Ok(NativeBufferResponse {
            error: None,
            snapshot: None,
            revision: Some(revision),
        }),
        Ok(_) => Err(JsErrorBox::generic(
            "Knot host returned the wrong response type",
        )),
        Err(error) => Ok(NativeBufferResponse {
            error: Some(host_request_error_name(error)),
            snapshot: None,
            revision: None,
        }),
    }
}

async fn request_host_operation(
    state: Rc<RefCell<OpState>>,
    operation: HostOperation,
) -> Result<HostResponse, JsErrorBox> {
    let response = state
        .borrow_mut()
        .borrow_mut::<ExtensionRequestRouter>()
        .request_javascript(operation)
        .map_err(|_| JsErrorBox::generic("Knot host closed the buffer request"))?;
    response
        .await
        .map_err(|_| JsErrorBox::generic("Knot host closed the buffer request"))
}

fn host_request_error_name(error: protocol::HostRequestError) -> &'static str {
    match error {
        protocol::HostRequestError::BufferClosed => "BufferClosed",
        protocol::HostRequestError::InvalidRange => "InvalidRange",
        protocol::HostRequestError::InvalidEditBatch => "InvalidEditBatch",
        protocol::HostRequestError::RevisionConflict => "RevisionConflict",
        protocol::HostRequestError::UnsupportedOperation => "UnsupportedOperation",
        protocol::HostRequestError::Cancelled => "Cancelled",
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
    /// The thread owns its `JsRuntime` and all extension-local state. Script
    /// and module execution remain fixture-level Step 7 probes.
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
    control: ExtensionRuntimeControl,
    requests: ExtensionRequestInbox,
    thread: ExtensionRuntimeThread,
}

impl ExtensionRuntimeHandle {
    fn spawn(extension: ExtensionId, async_runtime: Arc<tokio::runtime::Runtime>) -> Self {
        let (commands, command_receiver) = mpsc::channel();
        let (request_sender, requests) = tokio::sync::mpsc::unbounded_channel();
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

        let control = ExtensionRuntimeControl {
            extension,
            commands,
            lifecycle,
        };

        Self {
            control: control.clone(),
            requests: ExtensionRequestInbox { requests },
            thread: ExtensionRuntimeThread {
                control,
                thread: Some(thread),
            },
        }
    }

    /// Separate the unique request stream from clonable runtime controls and
    /// blocking thread ownership.
    pub fn into_parts(self) -> ExtensionRuntimeParts {
        ExtensionRuntimeParts {
            control: self.control,
            requests: self.requests,
            thread: self.thread,
        }
    }

    /// Ask the extension thread to issue one typed host request.
    pub fn request(&self, operation: HostOperation) -> Result<(), ExtensionRuntimeClosed> {
        self.control.request(operation)
    }

    /// Schedule a fixture script on this extension's thread-affine runtime.
    ///
    /// The returned completion can be awaited independently while the caller
    /// pumps and responds to host requests. This is intentionally not an
    /// extension-loading API.
    pub fn execute_fixture_script(
        &self,
        name: impl Into<String>,
        source: impl Into<String>,
    ) -> ExtensionRuntimeExecution {
        self.control.execute_fixture_script(name, source)
    }

    /// Schedule one fixture ES module from the prototype's static in-memory
    /// module set. `specifier` must be an absolute URL. The returned completion
    /// can be awaited independently while the caller handles host requests.
    pub fn execute_fixture_module(
        &self,
        specifier: impl Into<String>,
        source: impl Into<String>,
    ) -> ExtensionRuntimeExecution {
        self.control.execute_fixture_module(specifier, source)
    }

    /// Asynchronously receive the next request emitted by the extension thread.
    pub async fn receive_request(&mut self) -> Option<HostRequest> {
        self.requests.receive().await
    }

    /// Return a host response to the extension thread that issued it.
    pub fn respond(&self, response: HostResponse) -> Result<(), ExtensionRuntimeResponseError> {
        self.control.respond(response)
    }

    /// Return the host-side watchdog for this extension's isolate.
    pub fn watchdog(&self) -> ExtensionWatchdog {
        self.control.watchdog()
    }

    /// Stop the extension thread and wait for its thread-affine state to drop.
    pub fn shutdown(self) {
        self.thread.shutdown();
    }
}

/// Independently owned endpoints for an extension runtime.
pub struct ExtensionRuntimeParts {
    pub control: ExtensionRuntimeControl,
    pub requests: ExtensionRequestInbox,
    pub thread: ExtensionRuntimeThread,
}

/// Clonable, non-blocking access to an extension runtime and its response path.
#[derive(Clone)]
pub struct ExtensionRuntimeControl {
    extension: ExtensionId,
    commands: Sender<RuntimeCommand>,
    lifecycle: Arc<ExtensionLifecycle>,
}

impl ExtensionRuntimeControl {
    /// Ask the extension thread to issue one typed host request.
    pub fn request(&self, operation: HostOperation) -> Result<(), ExtensionRuntimeClosed> {
        self.commands
            .send(RuntimeCommand::Request(operation))
            .map_err(|_| ExtensionRuntimeClosed)
    }

    pub fn execute_fixture_script(
        &self,
        name: impl Into<String>,
        source: impl Into<String>,
    ) -> ExtensionRuntimeExecution {
        let (completion, completed) = tokio::sync::oneshot::channel();
        let _ = self.commands.send(RuntimeCommand::ExecuteFixtureScript {
            name: name.into(),
            source: source.into(),
            completion,
        });
        ExtensionRuntimeExecution {
            completion: completed,
        }
    }

    pub fn execute_fixture_module(
        &self,
        specifier: impl Into<String>,
        source: impl Into<String>,
    ) -> ExtensionRuntimeExecution {
        let (completion, completed) = tokio::sync::oneshot::channel();
        let _ = self.commands.send(RuntimeCommand::ExecuteFixtureModule {
            specifier: specifier.into(),
            source: source.into(),
            completion,
        });
        ExtensionRuntimeExecution {
            completion: completed,
        }
    }

    /// Return a host response to this extension runtime.
    pub fn respond(&self, response: HostResponse) -> Result<(), ExtensionRuntimeResponseError> {
        if response.extension != self.extension {
            return Err(ExtensionRuntimeResponseError::WrongExtension);
        }

        self.lifecycle.respond(response)
    }

    /// Return the host-side watchdog for this extension's isolate.
    pub fn watchdog(&self) -> ExtensionWatchdog {
        ExtensionWatchdog {
            lifecycle: Arc::clone(&self.lifecycle),
        }
    }

    /// Request teardown without waiting for the extension thread to exit.
    pub fn request_shutdown(&self) {
        // Keep the isolate handle alive until the extension thread tears down.
        // The queued command alone cannot stop JavaScript which is not yielding.
        self.lifecycle.request_shutdown();
        let _ = self.commands.send(RuntimeCommand::Shutdown);
    }
}

/// Unique receiver for requests emitted by one extension runtime.
pub struct ExtensionRequestInbox {
    requests: tokio::sync::mpsc::UnboundedReceiver<HostRequest>,
}

impl ExtensionRequestInbox {
    pub async fn receive(&mut self) -> Option<HostRequest> {
        self.requests.recv().await
    }
}

/// Blocking ownership of an extension's OS thread.
pub struct ExtensionRuntimeThread {
    control: ExtensionRuntimeControl,
    thread: Option<JoinHandle<()>>,
}

impl ExtensionRuntimeThread {
    pub fn shutdown(mut self) {
        self.stop().expect("Knot extension runtime thread panicked");
    }

    fn stop(&mut self) -> std::thread::Result<()> {
        self.control.request_shutdown();
        self.thread.take().map_or(Ok(()), JoinHandle::join)
    }
}

impl Drop for ExtensionRuntimeThread {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// Returned when an operation targets a runtime whose extension thread ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExtensionRuntimeClosed;

/// Awaitable completion of fixture JavaScript scheduled on an extension thread.
#[must_use = "fixture execution must be awaited or explicitly discarded"]
pub struct ExtensionRuntimeExecution {
    completion: tokio::sync::oneshot::Receiver<Result<(), ExtensionRuntimeExecutionError>>,
}

impl Future for ExtensionRuntimeExecution {
    type Output = Result<(), ExtensionRuntimeExecutionError>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.completion)
            .poll(context)
            .map(|completion| completion.unwrap_or(Err(ExtensionRuntimeExecutionError::Closed)))
    }
}

/// Host-side control for forcefully stopping one extension isolate.
///
/// This contains no V8 values. Its private lifecycle state owns V8's
/// thread-safe handle and clears it when the extension stops.
#[derive(Clone)]
pub struct ExtensionWatchdog {
    lifecycle: Arc<ExtensionLifecycle>,
}

impl ExtensionWatchdog {
    /// Interrupt JavaScript currently running in the extension isolate.
    pub fn terminate(&self) -> Result<(), ExtensionRuntimeClosed> {
        self.lifecycle.request_termination()
    }
}

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
    MemoryLimitExceeded,
    Terminated,
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
        completion: tokio::sync::oneshot::Sender<Result<(), ExtensionRuntimeExecutionError>>,
    },
    ExecuteFixtureModule {
        specifier: String,
        source: String,
        completion: tokio::sync::oneshot::Sender<Result<(), ExtensionRuntimeExecutionError>>,
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
    termination: Option<ExtensionTermination>,
    pending_requests: PendingRequests,
    watchdog: ExtensionWatchdogState,
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
        state.termination = None;
        state.pending_requests = PendingRequests::default();
        state.watchdog.clear_isolate();
    }

    fn install_isolate(&self, isolate: deno_core::v8::IsolateHandle) {
        self.state
            .lock()
            .expect("Knot extension lifecycle lock poisoned")
            .watchdog
            .install_isolate(isolate);
    }

    fn mark_isolate_ready(&self) {
        let mut state = self
            .state
            .lock()
            .expect("Knot extension lifecycle lock poisoned");
        state.watchdog.mark_ready();
        if state.termination.is_some() {
            let _ = state.watchdog.terminate();
        }
    }

    fn request_shutdown(&self) {
        let mut state = self
            .state
            .lock()
            .expect("Knot extension lifecycle lock poisoned");
        if state.torn_down {
            return;
        }
        state
            .termination
            .get_or_insert(ExtensionTermination::Requested);
        if state.watchdog.is_ready() {
            let _ = state.watchdog.terminate();
        }
    }

    fn request_termination(&self) -> Result<(), ExtensionRuntimeClosed> {
        let mut state = self
            .state
            .lock()
            .expect("Knot extension lifecycle lock poisoned");
        if state.torn_down || !state.watchdog.is_attached() {
            return Err(ExtensionRuntimeClosed);
        }
        if state.watchdog.is_ready() && !state.watchdog.terminate() {
            return Err(ExtensionRuntimeClosed);
        }
        state
            .termination
            .get_or_insert(ExtensionTermination::Requested);
        Ok(())
    }

    fn request_memory_limit_termination(&self) {
        let mut state = self
            .state
            .lock()
            .expect("Knot extension lifecycle lock poisoned");
        if state.torn_down {
            return;
        }
        state.termination = Some(ExtensionTermination::MemoryLimitExceeded);
        let _ = state.watchdog.terminate();
    }

    fn termination(&self) -> Option<ExtensionTermination> {
        self.state
            .lock()
            .expect("Knot extension lifecycle lock poisoned")
            .termination
    }
}

#[derive(Clone, Copy)]
enum ExtensionTermination {
    Requested,
    MemoryLimitExceeded,
}

/// Thread-safe control path from the watchdog to its extension isolate.
///
/// The handle is kept private to `host`; callers can never obtain a V8 value
/// through Knot's extension-facing API.
#[derive(Default)]
struct ExtensionWatchdogState {
    isolate: Option<deno_core::v8::IsolateHandle>,
    ready: bool,
}

impl ExtensionWatchdogState {
    fn install_isolate(&mut self, isolate: deno_core::v8::IsolateHandle) {
        self.isolate = Some(isolate);
    }

    fn mark_ready(&mut self) {
        self.ready = true;
    }

    fn clear_isolate(&mut self) {
        self.isolate = None;
        self.ready = false;
    }

    fn terminate(&self) -> bool {
        self.isolate
            .as_ref()
            .is_some_and(deno_core::v8::IsolateHandle::terminate_execution)
    }

    fn is_attached(&self) -> bool {
        self.isolate.is_some()
    }

    fn is_ready(&self) -> bool {
        self.ready
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
    request_sender: tokio::sync::mpsc::UnboundedSender<HostRequest>,
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

    fn request_javascript(
        &mut self,
        operation: HostOperation,
    ) -> Result<tokio::sync::oneshot::Receiver<HostResponse>, ExtensionRuntimeClosed> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let id = self.next_request_id();
        self.lifecycle.register_javascript(id, sender)?;
        self.send(id, operation)?;
        Ok(receiver)
    }
}

/// State confined to one extension's OS thread.
///
/// `Rc` makes that confinement explicit: neither Rust's type system nor the
/// host endpoint can accidentally move the `JsRuntime` to another thread.
struct ExtensionRuntime {
    event_loop_runtime: tokio::runtime::Runtime,
    js_runtime: deno_core::JsRuntime,
    fixture_modules: FixtureModuleLoader,
    lifecycle: Arc<ExtensionLifecycle>,
    _thread_affine: PhantomData<Rc<()>>,
}

impl ExtensionRuntime {
    fn new(
        extension: ExtensionId,
        request_sender: tokio::sync::mpsc::UnboundedSender<HostRequest>,
        lifecycle: Arc<ExtensionLifecycle>,
        async_runtime: Arc<tokio::runtime::Runtime>,
    ) -> Self {
        let fixture_modules = FixtureModuleLoader::with_private_bootstrap();
        let event_loop_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("Knot extension Tokio runtime construction failed");
        let mut js_runtime = {
            let _runtime_guard = event_loop_runtime.enter();
            deno_core::JsRuntime::new(deno_core::RuntimeOptions {
                extensions: vec![knot_runtime::init()],
                module_loader: Some(Rc::new(fixture_modules.clone())),
                create_params: Some(
                    deno_core::v8::Isolate::create_params()
                        .heap_limits(0, EXTENSION_HEAP_LIMIT_BYTES),
                ),
                ..Default::default()
            })
        };
        lifecycle.install_isolate(js_runtime.v8_isolate().thread_safe_handle());
        let heap_limit_lifecycle = Arc::clone(&lifecycle);
        js_runtime.add_near_heap_limit_callback(move |current_limit, _initial_limit| {
            heap_limit_lifecycle.request_memory_limit_termination();
            current_limit.saturating_mul(2)
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
                lifecycle: Arc::clone(&lifecycle),
            });
        lifecycle.mark_isolate_ready();

        Self {
            event_loop_runtime,
            js_runtime,
            fixture_modules,
            lifecycle,
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
                    let termination = self.lifecycle.termination();
                    let _ = completion.send(match termination {
                        Some(ExtensionTermination::MemoryLimitExceeded) => {
                            Err(ExtensionRuntimeExecutionError::MemoryLimitExceeded)
                        }
                        Some(ExtensionTermination::Requested) => {
                            Err(ExtensionRuntimeExecutionError::Terminated)
                        }
                        None => result,
                    });
                    if termination.is_some() {
                        break;
                    }
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
                                .block_on(self.js_runtime.load_side_es_module(&specifier))
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
                    let termination = self.lifecycle.termination();
                    let _ = completion.send(match termination {
                        Some(ExtensionTermination::MemoryLimitExceeded) => {
                            Err(ExtensionRuntimeExecutionError::MemoryLimitExceeded)
                        }
                        Some(ExtensionTermination::Requested) => {
                            Err(ExtensionRuntimeExecutionError::Terminated)
                        }
                        None => result,
                    });
                    if termination.is_some() {
                        break;
                    }
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
    use std::process::Command;

    use super::{
        ExtensionLifecycle, ExtensionRuntimeClosed, ExtensionRuntimeExecutionError,
        ExtensionRuntimeResponseError, V8Host,
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
        let mut runtime = host.spawn_extension(extension);

        runtime.request(HostOperation::ActiveBuffer).unwrap();
        runtime.request(HostOperation::ActiveBuffer).unwrap();

        let first = pollster::block_on(runtime.receive_request()).unwrap();
        let second = pollster::block_on(runtime.receive_request()).unwrap();
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

        pollster::block_on(runtime.execute_fixture_script(
            "initialize.js",
            "globalThis.runs = 1; Promise.resolve().then(() => globalThis.runs = 2)",
        ))
        .unwrap();
        pollster::block_on(runtime.execute_fixture_script(
            "verify.js",
            "if (globalThis.runs !== 2) throw new Error('event loop was not driven')",
        ))
        .unwrap();

        let error = pollster::block_on(
            runtime.execute_fixture_script("failure.js", "throw new Error('expected')"),
        )
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

        pollster::block_on(runtime.execute_fixture_module(
            "file:///fixtures/private-import.js",
            r#"
                    import { activeBuffer } from "knot:bootstrap";
                    globalThis.fixtureModuleLoaded = 7;
                "#,
        ))
        .unwrap_err();
        pollster::block_on(runtime.execute_fixture_module(
            "file:///fixtures/extension.js",
            "globalThis.fixtureModuleLoaded = 7",
        ))
        .unwrap();
        pollster::block_on(runtime.execute_fixture_script(
            "verify-module.js",
            "if (globalThis.fixtureModuleLoaded !== 7) throw new Error('module did not run')",
        ))
        .unwrap();

        assert_eq!(
            pollster::block_on(runtime.execute_fixture_module("not a URL", "")),
            Err(ExtensionRuntimeExecutionError::InvalidModuleSpecifier)
        );
        runtime.shutdown();
    }

    #[test]
    fn public_facade_resolves_a_host_request_without_exposing_deno() {
        let host = V8Host::new();
        let extension = ExtensionId::new(7);
        let mut runtime = host.spawn_extension(extension);
        let execution = runtime.execute_fixture_module(
            "file:///fixtures/active-buffer.js",
            r#"
                    import { editor } from "knot:editor";

                    if (typeof Deno !== "undefined") {
                        throw new Error("extension can access Deno");
                    }
                    editor.activeBuffer()
                        .then((buffer) => globalThis.activeBuffer = buffer)
                "#,
        );

        let request = pollster::block_on(runtime.receive_request()).unwrap();
        assert_eq!(request.extension, extension);
        assert_eq!(request.operation, HostOperation::ActiveBuffer);
        runtime
            .respond(HostResponse {
                extension,
                id: request.id,
                result: Ok(HostResponseValue::ActiveBuffer(None)),
            })
            .unwrap();
        pollster::block_on(execution).unwrap();

        pollster::block_on(runtime.execute_fixture_script(
            "verify-active-buffer.js",
            "if (globalThis.activeBuffer !== null) throw new Error('unexpected active buffer')",
        ))
        .unwrap();
        runtime.shutdown();
    }

    #[test]
    fn javascript_rejection_reports_its_source_without_poisoning_the_runtime() {
        let host = V8Host::new();
        let runtime = host.spawn_extension(ExtensionId::new(7));

        let error = pollster::block_on(
            runtime.execute_fixture_script("rejection.js", "Promise.reject(new Error('expected'))"),
        )
        .unwrap_err();
        let ExtensionRuntimeExecutionError::JavaScriptException { report } = error else {
            panic!("expected JavaScript rejection");
        };
        assert!(report.contains("Error: expected"));
        assert!(report.contains("rejection.js:1:16"));
        pollster::block_on(
            runtime.execute_fixture_script("recovery.js", "globalThis.recovered = true"),
        )
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

    #[test]
    fn lifecycle_attaches_the_isolate_to_its_private_watchdog() {
        let host = V8Host::new();
        let runtime = host.spawn_extension(ExtensionId::new(7));

        pollster::block_on(
            runtime.execute_fixture_script("watchdog-ready.js", "globalThis.watchdogReady = true"),
        )
        .unwrap();
        assert!(
            runtime
                .control
                .lifecycle
                .state
                .lock()
                .unwrap()
                .watchdog
                .is_attached()
        );
        runtime.shutdown();
    }

    #[test]
    fn runtime_endpoints_have_independent_ownership() {
        let host = V8Host::new();
        let extension = ExtensionId::new(7);
        let parts = host.spawn_extension(extension).into_parts();
        let control = parts.control.clone();
        let mut requests = parts.requests;

        control.request(HostOperation::ActiveBuffer).unwrap();
        let request = pollster::block_on(requests.receive()).unwrap();
        control
            .respond(HostResponse {
                extension,
                id: request.id,
                result: Ok(HostResponseValue::ActiveBuffer(None)),
            })
            .unwrap();

        parts.thread.shutdown();
    }

    #[test]
    fn watchdog_terminates_cpu_runaway_and_tears_down_the_extension() {
        let host = V8Host::new();
        let extension = ExtensionId::new(7);
        let mut runtime = host.spawn_extension(extension);
        let watchdog = runtime.watchdog();
        let execution = runtime.execute_fixture_module(
            "file:///fixtures/runaway.js",
            r#"
                    import { editor } from "knot:editor";
                    editor.activeBuffer().then(() => { while (true) {} });
                "#,
        );
        let request = pollster::block_on(runtime.receive_request()).unwrap();
        runtime
            .respond(HostResponse {
                extension,
                id: request.id,
                result: Ok(HostResponseValue::ActiveBuffer(None)),
            })
            .unwrap();

        watchdog.terminate().unwrap();
        assert_eq!(
            pollster::block_on(execution),
            Err(ExtensionRuntimeExecutionError::Terminated)
        );
        runtime.shutdown();
    }

    #[test]
    fn unload_terminates_cpu_runaway_before_joining_the_extension_thread() {
        let host = V8Host::new();
        let extension = ExtensionId::new(7);
        let mut runtime = host.spawn_extension(extension);
        let _execution = runtime.execute_fixture_module(
            "file:///fixtures/runaway-during-unload.js",
            r#"
                    import { editor } from "knot:editor";
                    editor.activeBuffer().then(() => {
                        const end = Date.now() + 3000;
                        while (Date.now() < end) {}
                    });
                "#,
        );
        let request = pollster::block_on(runtime.receive_request()).unwrap();
        runtime
            .respond(HostResponse {
                extension,
                id: request.id,
                result: Ok(HostResponseValue::ActiveBuffer(None)),
            })
            .unwrap();

        let started = std::time::Instant::now();
        runtime.shutdown();

        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "unload waited for runaway JavaScript to yield"
        );
    }

    /// Heap exhaustion can trigger a V8 process abort when a near-limit
    /// callback is configured incorrectly. Exercise the production runtime in
    /// a child process so a regression cannot take down the test runner.
    #[test]
    fn heap_exhaustion_probe_runs_in_a_sacrificial_process() {
        let status = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("host::tests::heap_exhaustion_probe_child")
            .arg("--nocapture")
            .env("KNOT_RUN_HEAP_EXHAUSTION_PROBE", "1")
            .status()
            .expect("Knot heap-exhaustion probe process must start");

        assert!(
            status.success(),
            "Knot extension heap limit must isolate the failure"
        );
    }

    #[test]
    fn heap_exhaustion_probe_child() {
        if std::env::var_os("KNOT_RUN_HEAP_EXHAUSTION_PROBE").is_none() {
            return;
        }

        let host = V8Host::new();
        let exhausted = host.spawn_extension(ExtensionId::new(7));
        pollster::block_on(exhausted.execute_fixture_script("heap-ready.js", "void 0"))
            .expect("limited extension isolate must initialize");
        let neighbor = host.spawn_extension(ExtensionId::new(8));
        pollster::block_on(neighbor.execute_fixture_script("neighbor-ready.js", "void 0"))
            .expect("neighbor extension isolate must initialize");

        assert_eq!(
            pollster::block_on(exhausted.execute_fixture_script(
                "heap-exhaustion.js",
                r#"let text = ""; while (true) { text += "Knot"; }"#,
            )),
            Err(ExtensionRuntimeExecutionError::MemoryLimitExceeded)
        );
        assert_eq!(
            pollster::block_on(
                exhausted.execute_fixture_script("after-heap-exhaustion.js", "void 0")
            ),
            Err(ExtensionRuntimeExecutionError::Closed)
        );
        pollster::block_on(
            neighbor
                .execute_fixture_script("healthy-neighbor.js", "globalThis.stillRunning = true"),
        )
        .expect("another extension isolate must remain usable");

        exhausted.shutdown();
        neighbor.shutdown();
    }
}
