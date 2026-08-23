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
    time::{Duration, Instant},
};

use deno_core::v8;
use deno_core::{ModuleLoadOptions, ModuleLoadReferrer, ModuleLoadResponse, ModuleLoader};
use deno_core::{ModuleResolveResponse, ModuleSource, ModuleSourceCode, ModuleSpecifier, OpState};
use deno_error::JsErrorBox;

pub mod bench;
pub mod protocol;

use protocol::{
    BufferChange, BufferHandle, BufferSubscriptionId, ByteRange, CommandInvocation,
    CommandInvocationId, CommandInvokeDispatch, CommandOutcome, CompletionProviderError,
    CompletionRequest, CompletionResponse, ExtensionId, ExtensionLifecycleId, HostOperation,
    HostRequest, HostResponse, HostResponseValue, RequestId, TextEdit, TreeChildrenRequest,
    TreeChildrenResponse, TreeProviderError,
};

const PRIVATE_BOOTSTRAP_SPECIFIER: &str = "knot:bootstrap";
const PUBLIC_FACADE_SPECIFIER: &str = "knot:editor";
const EXTENSION_HEAP_LIMIT_BYTES: usize = 32 * 1024 * 1024;
const PRIVATE_BOOTSTRAP_SOURCE: &str = r#"
const nativeOps = Deno.core.ops;

async function nativeResponse(request) {
  const response = await request;
  return nativeOps.op_native_response_take(response);
}

function bufferRequest(request) {
  return nativeResponse(nativeOps.op_buffer_request(request));
}

const buffers = new Map();
const commandHandlers = new Map();
const bufferChangeListeners = new Map();
const treeProviders = new Map();
const completionProviders = new Map();
const activeCommandFrames = [];
const snapshotTables = new WeakMap();

class KnotAbortSignal {
  #aborted = false;
  #reason = undefined;
  #listeners = new Set();
  get aborted() { return this.#aborted; }
  get reason() { return this.#reason; }
  addEventListener(type, listener) {
    if (type === "abort") this.#listeners.add(listener);
  }
  removeEventListener(type, listener) {
    if (type === "abort") this.#listeners.delete(listener);
  }
  throwIfAborted() {
    if (this.#aborted) throw this.#reason;
  }
  abort(reason) {
    if (this.#aborted) return;
    this.#aborted = true;
    this.#reason = reason;
    for (const listener of this.#listeners) listener.call(this, { type: "abort", target: this });
    this.#listeners.clear();
  }
}

class KnotAbortController {
  signal = new KnotAbortSignal();
  abort(reason = new Error("command cancelled")) { this.signal.abort(reason); }
}

function hostError(error) {
  if (error === "Cancelled") {
    activeCommandFrames.at(-1)?.controller.abort(new Error("command cancelled"));
  }
  const names = {
    BufferClosed: "BufferClosedError",
    InvalidRange: "RangeError",
    InvalidEditBatch: "InvalidEditBatchError",
    RevisionConflict: "RevisionConflictError",
    ContributionSetNotFound: "ContributionSetNotFoundError",
    TreeViewNotFound: "TreeViewNotFoundError",
    TreeProviderInUse: "TreeProviderInUseError",
    TreeProviderNotFound: "TreeProviderNotFoundError",
    CommandNameInUse: "CommandNameInUseError",
    CommandNotFound: "CommandNotFoundError",
    Cancelled: "AbortError",
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
      startByteOffset: snapshot.range.startByteOffset,
      endByteOffset: snapshot.range.endByteOffset,
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
  let contributionsDisposed = false;
  const contributions = Object.freeze({
    async replace(items, options) {
      if (contributionsDisposed) {
        const error = new Error("editor contribution set is disposed");
        error.name = "ContributionSetDisposedError";
        throw error;
      }
      const result = await bufferRequest({
        kind: "replaceEditorContributions",
        handle,
        contributions: items,
        ifRevision: options?.ifRevision,
      });
      if (result.kind === "error") hostError(result.error);
    },
    async dispose() {
      if (contributionsDisposed) return;
      contributionsDisposed = true;
      const result = await bufferRequest({ kind: "disposeEditorContributions", handle });
      if (result.kind === "error") hostError(result.error);
    },
  });
  buffer = Object.freeze({
    async snapshot(range) {
      const result = await bufferRequest({ kind: "snapshot", handle, range });
      if (result.kind === "error") hostError(result.error);
      return snapshotFromNative(result.snapshot);
    },
    async applyEdits(edits, options) {
      const result = await bufferRequest({ kind: "applyEdits", handle, edits, ifRevision: options?.ifRevision });
      if (result.kind === "error") hostError(result.error);
      return Object.freeze({ revision: result.revision });
    },
    onDidChange(listener) {
      if (typeof listener !== "function") throw new TypeError("onDidChange requires a listener");
      return nativeOps.op_buffer_subscribe(handle).then((subscription) => {
        bufferChangeListeners.set(subscription, { handle, listener });
        let disposed = false;
        return Object.freeze({ dispose() {
          if (disposed) return;
          disposed = true;
          bufferChangeListeners.delete(subscription);
          void nativeOps.op_buffer_unsubscribe(subscription);
        }});
      });
    },
    contributions,
  });
  buffers.set(handle, buffer);
  return buffer;
}

export async function activeBuffer() {
  const handle = await nativeOps.op_buffer_active();
  return handle === null ? null : bufferFor(handle);
}

export async function registerCommand(name, handler) {
  if (typeof name !== "string" || typeof handler !== "function") {
    throw new TypeError("commands.register requires a name and handler");
  }
  const result = await nativeResponse(nativeOps.op_command_register(name));
  if (result.kind === "error") hostError(result.error);
  const registration = result.registration;
  commandHandlers.set(registration, handler);
  let disposed = false;
  return Object.freeze({
    dispose() {
      if (disposed) return;
      disposed = true;
      commandHandlers.delete(registration);
      void nativeResponse(nativeOps.op_command_unregister(registration)).then((result) => {
        if (result.kind === "error") hostError(result.error);
      });
    },
  });
}

export async function invokeCommand(name, commandArguments) {
  if (typeof name !== "string") {
    throw new TypeError("commands.invoke requires a command name");
  }
  const dispatch = await nativeOps.op_command_invoke(
    name,
    commandArguments === undefined ? null : commandArguments,
  );
  if (dispatch.kind === "outcome") return Object.freeze(dispatch.outcome);

  const outcome = await invokeRegisteredCommand(
    dispatch.invocation,
    dispatch.registration,
    activeCommandFrames.at(-1)?.buffer ?? null,
    commandArguments === undefined ? null : commandArguments,
    true,
  );
  await nativeOps.op_command_inline_complete(dispatch.invocation, outcome);
  return Object.freeze(outcome);
}

export function invalidCommandArguments(message) {
  const error = new Error(String(message));
  error.name = "InvalidCommandArgumentsError";
  throw error;
}

export async function registerTreeDataProvider(viewId, provider) {
  if (typeof viewId !== "string" || typeof provider?.getChildren !== "function") {
    throw new TypeError("workbench.registerTreeDataProvider requires a view ID and getChildren provider");
  }
  const registration = await nativeOps.op_tree_register(viewId);
  treeProviders.set(registration, provider);
  let disposed = false;
  return Object.freeze({
    invalidate(parentId) {
      if (disposed) return;
      void nativeOps.op_tree_invalidate(registration, parentId ?? null);
    },
    dispose() {
      if (disposed) return;
      disposed = true;
      treeProviders.delete(registration);
      void nativeOps.op_tree_unregister(registration);
    },
  });
}

export async function registerCompletionProvider(label, provider) {
  if (typeof label !== "string" || typeof provider?.provideCompletions !== "function") {
    throw new TypeError("editor.registerCompletionProvider requires a label and provideCompletions provider");
  }
  const registration = await nativeOps.op_completion_register(label);
  completionProviders.set(registration, provider);
  let disposed = false;
  return Object.freeze({
    dispose() {
      if (disposed) return;
      disposed = true;
      completionProviders.delete(registration);
      void nativeOps.op_completion_unregister(registration);
    },
  });
}

globalThis.__knotRequestCompletions = async (
  registration,
  handle,
  revision,
  cursorByteOffset,
  prefix,
  generation,
) => {
  const provider = completionProviders.get(registration);
  if (!provider) {
    nativeOps.op_completion_complete({
      registration, revision, generation, error: "completion provider is disposed",
    });
    return;
  }
  try {
    const provided = await provider.provideCompletions(Object.freeze({
      buffer: bufferFor(handle), revision, cursorByteOffset, prefix, generation,
    }));
    const items = Array.from(provided, (item) => {
      if (typeof item?.label !== "string" || typeof item?.insertText !== "string") {
        throw new TypeError("completion items require string label and insertText properties");
      }
      return Object.freeze({ label: item.label, insertText: item.insertText });
    });
    nativeOps.op_completion_complete({ registration, revision, generation, items });
  } catch (error) {
    nativeOps.op_completion_complete({
      registration, revision, generation, error: String(error?.stack ?? error),
    });
  }
};

globalThis.__knotRequestTreeChildren = async (registration, parentId, generation) => {
  const provider = treeProviders.get(registration);
  if (!provider) {
    nativeOps.op_tree_children_complete({
      registration,
      parentId,
      generation,
      error: "tree data provider is disposed",
    });
    return;
  }
  try {
    const items = await provider.getChildren(parentId);
    nativeOps.op_tree_children_complete({
      registration,
      parentId,
      generation,
      items: Array.from(items),
    });
  } catch (error) {
    nativeOps.op_tree_children_complete({
      registration,
      parentId,
      generation,
      error: String(error?.stack ?? error),
    });
  }
};

globalThis.__knotFixtureDelay = (milliseconds) =>
  nativeOps.op_fixture_delay(milliseconds);

async function invokeRegisteredCommand(
  invocation,
  registration,
  activeHandle,
  commandArguments,
  classifyFailure,
) {
  const handler = commandHandlers.get(registration);
  if (!handler) {
    const error = new Error("Knot command registration is disposed");
    if (classifyFailure) {
      return { kind: "handlerFailure", message: String(error.stack ?? error) };
    }
    throw error;
  }
  const controller = new KnotAbortController();
  const frame = { invocation, controller, buffer: activeHandle };
  activeCommandFrames.push(frame);
  const cancellation = nativeOps.op_command_cancellation(invocation).then((cancelled) => {
    if (cancelled) controller.abort(new Error("command cancelled"));
  });
  try {
    try {
      await handler(Object.freeze({
        buffer: activeHandle === null ? null : bufferFor(activeHandle),
        arguments: commandArguments,
        signal: controller.signal,
      }));
      if (controller.signal.aborted) return { kind: "cancelled" };
      return classifyFailure ? { kind: "completed" } : undefined;
    } catch (error) {
      if (!classifyFailure) throw error;
      if (controller.signal.aborted || error?.name === "AbortError") {
        return { kind: "cancelled" };
      }
      const message = String(error?.stack ?? error);
      if (error?.name === "InvalidCommandArgumentsError") {
        return { kind: "invalidArgument", message };
      }
      return { kind: "handlerFailure", message };
    }
  } finally {
    nativeOps.op_command_cancellation_finish(invocation);
    await cancellation;
    const popped = activeCommandFrames.pop();
    if (popped !== frame) throw new Error("Knot command frame stack is corrupted");
  }
}

globalThis.__knotInvokeCommand = async (
  invocation,
  registration,
  activeHandle,
  commandArguments,
) => {
  await invokeRegisteredCommand(
    invocation,
    registration,
    activeHandle,
    commandArguments,
    false,
  );
};

globalThis.__knotDispatchBufferChange = async (subscription, change) => {
  const entry = bufferChangeListeners.get(subscription);
  if (!entry) return;
  await entry.listener(Object.freeze({
    buffer: bufferFor(entry.handle),
    beforeRevision: change.beforeRevision,
    revision: change.revision,
    edits: Object.freeze(change.edits.map((edit) => Object.freeze({
      range: Object.freeze({ startByteOffset: edit.range.startByteOffset, endByteOffset: edit.range.endByteOffset }),
      text: edit.text,
    }))),
  }));
};
"#;
const PUBLIC_FACADE_SOURCE: &str = r#"
import {
  activeBuffer,
  invalidCommandArguments,
  invokeCommand,
  registerCommand,
  registerCompletionProvider,
  registerTreeDataProvider,
} from "knot:bootstrap";

export const editor = {
  activeBuffer,
  registerCompletionProvider,
};

export const commands = {
  invalidArguments: invalidCommandArguments,
  invoke: invokeCommand,
  register: registerCommand,
};

export const workbench = {
  registerTreeDataProvider,
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
        op_buffer_request,
        op_native_response_take,
        op_command_register,
        op_command_unregister,
        op_command_invoke,
        op_command_inline_complete,
        op_command_cancellation,
        op_command_cancellation_finish,
        op_buffer_subscribe,
        op_buffer_unsubscribe,
        op_tree_register,
        op_tree_invalidate,
        op_tree_unregister,
        op_tree_children_complete,
        op_completion_register,
        op_completion_unregister,
        op_completion_complete,
        op_fixture_delay,
        op_fixture_shared_host_runtime,
    ],
);

/// Shared native work available to ops without moving V8 off its extension
/// thread.
#[derive(Clone)]
struct HostAsyncRuntime(Arc<tokio::runtime::Runtime>);

#[deno_core::op2]
async fn op_fixture_delay(state: Rc<RefCell<OpState>>, #[number] milliseconds: u64) {
    let runtime = state.borrow().borrow::<HostAsyncRuntime>().0.clone();
    runtime
        .spawn(async move {
            tokio::time::sleep(Duration::from_millis(milliseconds)).await;
        })
        .await
        .expect("Knot fixture delay task panicked");
}

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
struct NativeBufferRequest {
    handle: u64,
    #[serde(flatten)]
    operation: NativeBufferOperation,
}

#[derive(serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum NativeBufferOperation {
    Snapshot {
        range: Option<ByteRange>,
    },
    ApplyEdits {
        edits: Vec<TextEdit>,
        #[serde(rename = "ifRevision")]
        if_revision: u64,
    },
    ReplaceEditorContributions {
        contributions: Vec<protocol::EditorContribution>,
        #[serde(rename = "ifRevision")]
        if_revision: u64,
    },
    DisposeEditorContributions,
}

enum NativeResponse {
    Snapshot {
        snapshot: protocol::TextSnapshot,
    },
    AppliedEdits {
        revision: u64,
    },
    EditorContributionsReplaced,
    EditorContributionsDisposed,
    CommandRegistered {
        registration: protocol::CommandRegistrationId,
    },
    CommandUnregistered,
    HostError {
        error: protocol::HostRequestError,
    },
}

struct NativeResponseStore {
    next: u32,
    responses: HashMap<u32, NativeResponse>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeTreeChildrenCompletion {
    registration: u64,
    parent_id: Option<String>,
    generation: u64,
    items: Option<Vec<protocol::TreeItem>>,
    error: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeCompletionCompletion {
    registration: u64,
    revision: u64,
    generation: u64,
    items: Option<Vec<protocol::CompletionResultItem>>,
    error: Option<String>,
}

#[derive(Default)]
struct TreeCallbackStore {
    response: Option<TreeChildrenResponse>,
}

#[derive(Default)]
struct CompletionCallbackStore {
    response: Option<CompletionResponse>,
}

impl NativeResponseStore {
    fn new() -> Self {
        Self {
            next: 1,
            responses: HashMap::new(),
        }
    }

    fn insert(&mut self, response: NativeResponse) -> u32 {
        let id = self.next;
        self.next = self
            .next
            .checked_add(1)
            .expect("native response ID overflowed");
        assert!(self.responses.insert(id, response).is_none());
        id
    }
}

impl<'a> deno_core::ToV8<'a> for protocol::SnapshotText {
    type Error = JsErrorBox;

    fn to_v8<'i>(
        self,
        scope: &mut deno_core::v8::PinScope<'a, 'i>,
    ) -> Result<deno_core::v8::Local<'a, deno_core::v8::Value>, Self::Error> {
        let string = match self {
            protocol::SnapshotText::Utf8(text) => deno_core::v8::String::new_from_utf8(
                scope,
                text.as_bytes(),
                deno_core::v8::NewStringType::Normal,
            ),
            protocol::SnapshotText::Utf16(text) if text.is_empty() => {
                return Ok(deno_core::v8::String::empty(scope).into());
            }
            protocol::SnapshotText::Utf16(text) => {
                let len = text.len();
                let raw = Arc::into_raw(text);
                unsafe {
                    deno_core::v8::String::new_external_twobyte_raw(
                        scope,
                        raw.cast::<u16>().cast_mut(),
                        len,
                        drop_external_utf16,
                    )
                }
            }
        }
        .ok_or_else(|| JsErrorBox::range_error("snapshot string too long"))?;
        Ok(string.into())
    }
}

unsafe extern "C" fn drop_external_utf16(data: *mut u16, len: usize) {
    let slice = std::ptr::slice_from_raw_parts(data.cast_const(), len);
    drop(unsafe { Arc::<[u16]>::from_raw(slice) });
}

impl NativeResponse {
    fn to_v8<'a>(
        self,
        scope: &mut deno_core::v8::PinScope<'a, '_>,
    ) -> Result<deno_core::v8::Local<'a, deno_core::v8::Value>, JsErrorBox> {
        use deno_core::ToV8;

        let response = deno_core::v8::Object::new(scope);
        match self {
            Self::Snapshot { snapshot } => {
                let kind = v8_text(scope, "snapshot")?;
                set_v8_property(scope, response, "kind", kind.into())?;
                let value = deno_core::v8::Object::new(scope);
                let text = snapshot.text.to_v8(scope)?;
                set_v8_property(scope, value, "text", text)?;

                let range = deno_core::v8::Object::new(scope);
                let start =
                    deno_core::v8::Number::new(scope, snapshot.range.start_byte_offset as f64);
                set_v8_property(scope, range, "startByteOffset", start.into())?;
                let end = deno_core::v8::Number::new(scope, snapshot.range.end_byte_offset as f64);
                set_v8_property(scope, range, "endByteOffset", end.into())?;
                set_v8_property(scope, value, "range", range.into())?;

                let revision = deno_core::v8::Number::new(scope, snapshot.revision as f64);
                set_v8_property(scope, value, "revision", revision.into())?;
                set_v8_property(scope, response, "snapshot", value.into())?;
            }
            Self::AppliedEdits { revision } => {
                let kind = v8_text(scope, "appliedEdits")?;
                set_v8_property(scope, response, "kind", kind.into())?;
                let revision = deno_core::v8::Number::new(scope, revision as f64);
                set_v8_property(scope, response, "revision", revision.into())?;
            }
            Self::EditorContributionsReplaced => {
                let kind = v8_text(scope, "editorContributionsReplaced")?;
                set_v8_property(scope, response, "kind", kind.into())?;
            }
            Self::EditorContributionsDisposed => {
                let kind = v8_text(scope, "editorContributionsDisposed")?;
                set_v8_property(scope, response, "kind", kind.into())?;
            }
            Self::CommandRegistered { registration } => {
                let kind = v8_text(scope, "commandRegistered")?;
                set_v8_property(scope, response, "kind", kind.into())?;
                let registration = deno_core::v8::Number::new(scope, registration.value() as f64);
                set_v8_property(scope, response, "registration", registration.into())?;
            }
            Self::CommandUnregistered => {
                let kind = v8_text(scope, "commandUnregistered")?;
                set_v8_property(scope, response, "kind", kind.into())?;
            }
            Self::HostError { error } => {
                let kind = v8_text(scope, "error")?;
                set_v8_property(scope, response, "kind", kind.into())?;
                let error = v8_text(scope, &format!("{error:?}"))?;
                set_v8_property(scope, response, "error", error.into())?;
            }
        }
        Ok(response.into())
    }
}

fn v8_text<'a>(
    scope: &mut deno_core::v8::PinScope<'a, '_>,
    text: &str,
) -> Result<deno_core::v8::Local<'a, deno_core::v8::String>, JsErrorBox> {
    deno_core::v8::String::new(scope, text)
        .ok_or_else(|| JsErrorBox::range_error("native response string too long"))
}

fn set_v8_property<'a>(
    scope: &mut deno_core::v8::PinScope<'a, '_>,
    object: deno_core::v8::Local<'a, deno_core::v8::Object>,
    name: &str,
    value: deno_core::v8::Local<'a, deno_core::v8::Value>,
) -> Result<(), JsErrorBox> {
    let key = v8_text(scope, name)?;
    match object.set(scope, key.into(), value) {
        Some(true) => Ok(()),
        _ => Err(JsErrorBox::generic("failed to construct native response")),
    }
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
#[smi]
async fn op_buffer_request(
    state: Rc<RefCell<OpState>>,
    #[serde] request: NativeBufferRequest,
) -> Result<u32, JsErrorBox> {
    let operation = match request.operation {
        NativeBufferOperation::Snapshot { range } => HostOperation::Snapshot {
            buffer: BufferHandle::new(request.handle),
            range,
        },
        NativeBufferOperation::ApplyEdits { edits, if_revision } => HostOperation::ApplyEdits {
            buffer: BufferHandle::new(request.handle),
            edits,
            if_revision,
        },
        NativeBufferOperation::ReplaceEditorContributions {
            contributions,
            if_revision,
        } => HostOperation::ReplaceEditorContributions {
            buffer: BufferHandle::new(request.handle),
            contributions,
            if_revision,
        },
        NativeBufferOperation::DisposeEditorContributions => {
            HostOperation::DisposeEditorContributions {
                buffer: BufferHandle::new(request.handle),
            }
        }
    };
    let response = request_host_operation(Rc::clone(&state), operation).await?;
    let response = match response.result {
        Ok(HostResponseValue::Snapshot(snapshot)) => NativeResponse::Snapshot { snapshot },
        Ok(HostResponseValue::AppliedEdits { revision }) => {
            NativeResponse::AppliedEdits { revision }
        }
        Ok(HostResponseValue::EditorContributionsReplaced) => {
            NativeResponse::EditorContributionsReplaced
        }
        Ok(HostResponseValue::EditorContributionsDisposed) => {
            NativeResponse::EditorContributionsDisposed
        }
        Ok(_) => {
            return Err(JsErrorBox::generic(
                "Knot host returned the wrong response type",
            ));
        }
        Err(error) => NativeResponse::HostError { error },
    };
    Ok(state
        .borrow_mut()
        .borrow_mut::<NativeResponseStore>()
        .insert(response))
}

#[deno_core::op2]
fn op_native_response_take<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    state: &mut OpState,
    #[smi] response: u32,
) -> Result<v8::Local<'a, v8::Value>, JsErrorBox> {
    let response = state
        .borrow_mut::<NativeResponseStore>()
        .responses
        .remove(&response)
        .ok_or_else(|| JsErrorBox::generic("Knot native response is missing"))?;
    response.to_v8(scope)
}

#[deno_core::op2]
#[smi]
async fn op_command_register(
    state: Rc<RefCell<OpState>>,
    #[string] name: String,
) -> Result<u32, JsErrorBox> {
    let response = request_host_operation(
        Rc::clone(&state),
        HostOperation::RegisterCommand {
            title: name.clone(),
            name: name.into(),
        },
    )
    .await?;
    let response = match response.result {
        Ok(HostResponseValue::CommandRegistered { registration }) => {
            NativeResponse::CommandRegistered { registration }
        }
        Ok(_) => Err(JsErrorBox::generic(
            "Knot host returned the wrong response type",
        ))?,
        Err(error) => NativeResponse::HostError { error },
    };
    Ok(state
        .borrow_mut()
        .borrow_mut::<NativeResponseStore>()
        .insert(response))
}

#[deno_core::op2]
#[smi]
async fn op_command_unregister(
    state: Rc<RefCell<OpState>>,
    #[number] registration: u64,
) -> Result<u32, JsErrorBox> {
    let response = request_host_operation(
        Rc::clone(&state),
        HostOperation::UnregisterCommand {
            registration: protocol::CommandRegistrationId::new(registration),
        },
    )
    .await?;
    let response = match response.result {
        Ok(HostResponseValue::CommandUnregistered { .. }) => NativeResponse::CommandUnregistered,
        Ok(_) => Err(JsErrorBox::generic(
            "Knot host returned the wrong response type",
        ))?,
        Err(error) => NativeResponse::HostError { error },
    };
    Ok(state
        .borrow_mut()
        .borrow_mut::<NativeResponseStore>()
        .insert(response))
}

#[deno_core::op2]
#[serde]
async fn op_command_invoke(
    state: Rc<RefCell<OpState>>,
    #[string] name: String,
    #[serde] arguments: protocol::CommandArgumentValue,
) -> Result<CommandInvokeDispatch, JsErrorBox> {
    let response = request_host_operation(
        Rc::clone(&state),
        HostOperation::InvokeCommand {
            command: protocol::Command {
                name: name.into(),
                arguments,
            },
        },
    )
    .await?;
    match response.result {
        Ok(HostResponseValue::CommandInvoked { dispatch }) => {
            if let CommandInvokeDispatch::Inline { invocation, .. } = dispatch {
                state
                    .borrow_mut()
                    .borrow_mut::<ExtensionRequestRouter>()
                    .invocations
                    .push(invocation);
            }
            Ok(dispatch)
        }
        Ok(_) => Err(JsErrorBox::generic(
            "Knot host returned the wrong response type",
        )),
        Err(error) => Err(JsErrorBox::generic(format!(
            "Knot host rejected command invocation: {error:?}"
        ))),
    }
}

#[deno_core::op2]
async fn op_command_inline_complete(
    state: Rc<RefCell<OpState>>,
    #[number] invocation: u64,
    #[serde] outcome: CommandOutcome,
) -> Result<(), JsErrorBox> {
    let invocation = CommandInvocationId::new(invocation);
    let active = state
        .borrow()
        .borrow::<ExtensionRequestRouter>()
        .invocations
        .last()
        .copied();
    if active != Some(invocation) {
        return Err(JsErrorBox::generic(
            "Knot inline command completion does not match the active frame",
        ));
    }
    let response = request_host_operation(
        Rc::clone(&state),
        HostOperation::CompleteInlineCommand {
            invocation,
            outcome,
        },
    )
    .await;
    let popped = state
        .borrow_mut()
        .borrow_mut::<ExtensionRequestRouter>()
        .invocations
        .pop();
    debug_assert_eq!(popped, Some(invocation));
    let response = response?;
    match response.result {
        Ok(HostResponseValue::InlineCommandCompleted {
            invocation: completed,
        }) if completed == invocation => Ok(()),
        Ok(_) => Err(JsErrorBox::generic(
            "Knot host returned the wrong inline command completion",
        )),
        Err(error) => Err(JsErrorBox::generic(format!(
            "Knot host rejected inline command completion: {error:?}"
        ))),
    }
}

#[deno_core::op2]
async fn op_command_cancellation(state: Rc<RefCell<OpState>>, #[number] invocation: u64) -> bool {
    let lifecycle = Arc::clone(&state.borrow().borrow::<ExtensionRequestRouter>().lifecycle);
    lifecycle
        .wait_for_command_cancellation(CommandInvocationId::new(invocation))
        .await
}

#[deno_core::op2(fast)]
fn op_command_cancellation_finish(state: &mut OpState, #[number] invocation: u64) {
    let lifecycle = Arc::clone(&state.borrow::<ExtensionRequestRouter>().lifecycle);
    lifecycle.finish_command_cancellation(CommandInvocationId::new(invocation));
}

#[deno_core::op2]
#[number]
async fn op_buffer_subscribe(
    state: Rc<RefCell<OpState>>,
    #[number] handle: u64,
) -> Result<u64, JsErrorBox> {
    let response = request_host_operation(
        state,
        HostOperation::SubscribeBufferChanges {
            buffer: BufferHandle::new(handle),
        },
    )
    .await?;
    match response.result {
        Ok(HostResponseValue::BufferChangesSubscribed { subscription }) => Ok(subscription.value()),
        Ok(_) => Err(JsErrorBox::generic(
            "Knot host returned the wrong response type",
        )),
        Err(error) => Err(JsErrorBox::generic(format!(
            "Knot host rejected buffer subscription: {error:?}"
        ))),
    }
}

#[deno_core::op2]
async fn op_buffer_unsubscribe(
    state: Rc<RefCell<OpState>>,
    #[number] subscription: u64,
) -> Result<(), JsErrorBox> {
    let response = request_host_operation(
        state,
        HostOperation::UnsubscribeBufferChanges {
            subscription: BufferSubscriptionId::new(subscription),
        },
    )
    .await?;
    match response.result {
        Ok(HostResponseValue::BufferChangesUnsubscribed { .. }) => Ok(()),
        Ok(_) => Err(JsErrorBox::generic(
            "Knot host returned the wrong response type",
        )),
        Err(error) => Err(JsErrorBox::generic(format!(
            "Knot host rejected buffer unsubscription: {error:?}"
        ))),
    }
}

#[deno_core::op2]
#[number]
async fn op_tree_register(
    state: Rc<RefCell<OpState>>,
    #[string] view_id: String,
) -> Result<u64, JsErrorBox> {
    let response =
        request_host_operation(state, HostOperation::RegisterTreeProvider { view_id }).await?;
    match response.result {
        Ok(HostResponseValue::TreeProviderRegistered { registration }) => Ok(registration.value()),
        Ok(_) => Err(JsErrorBox::generic(
            "Knot host returned the wrong response type",
        )),
        Err(error) => Err(JsErrorBox::generic(format!(
            "Knot host rejected tree provider registration: {error:?}"
        ))),
    }
}

#[deno_core::op2]
async fn op_tree_invalidate(
    state: Rc<RefCell<OpState>>,
    #[number] registration: u64,
    #[string] parent_id: Option<String>,
) -> Result<(), JsErrorBox> {
    let response = request_host_operation(
        state,
        HostOperation::InvalidateTreeProvider {
            registration: protocol::TreeProviderRegistrationId::new(registration),
            parent_id,
        },
    )
    .await?;
    match response.result {
        Ok(HostResponseValue::TreeProviderInvalidated) => Ok(()),
        Ok(_) => Err(JsErrorBox::generic(
            "Knot host returned the wrong response type",
        )),
        Err(error) => Err(JsErrorBox::generic(format!(
            "Knot host rejected tree provider invalidation: {error:?}"
        ))),
    }
}

#[deno_core::op2]
async fn op_tree_unregister(
    state: Rc<RefCell<OpState>>,
    #[number] registration: u64,
) -> Result<(), JsErrorBox> {
    let registration = protocol::TreeProviderRegistrationId::new(registration);
    let response = request_host_operation(
        state,
        HostOperation::UnregisterTreeProvider { registration },
    )
    .await?;
    match response.result {
        Ok(HostResponseValue::TreeProviderUnregistered { .. }) => Ok(()),
        Ok(_) => Err(JsErrorBox::generic(
            "Knot host returned the wrong response type",
        )),
        Err(error) => Err(JsErrorBox::generic(format!(
            "Knot host rejected tree provider disposal: {error:?}"
        ))),
    }
}

#[deno_core::op2]
fn op_tree_children_complete(
    state: &mut OpState,
    #[serde] completion: NativeTreeChildrenCompletion,
) -> Result<(), JsErrorBox> {
    let result = match (completion.items, completion.error) {
        (Some(items), None) => Ok(items),
        (_, Some(message)) => Err(TreeProviderError { message }),
        _ => Err(TreeProviderError {
            message: "tree provider returned no items".into(),
        }),
    };
    let response = TreeChildrenResponse {
        registration: protocol::TreeProviderRegistrationId::new(completion.registration),
        parent_id: completion.parent_id,
        generation: completion.generation,
        result,
    };
    let store = state.borrow_mut::<TreeCallbackStore>();
    if store.response.replace(response).is_some() {
        return Err(JsErrorBox::generic(
            "tree provider completed one request more than once",
        ));
    }
    Ok(())
}

#[deno_core::op2]
#[number]
async fn op_completion_register(
    state: Rc<RefCell<OpState>>,
    #[string] label: String,
) -> Result<u64, JsErrorBox> {
    let response =
        request_host_operation(state, HostOperation::RegisterCompletionProvider { label }).await?;
    match response.result {
        Ok(HostResponseValue::CompletionProviderRegistered { registration }) => {
            Ok(registration.value())
        }
        Ok(_) => Err(JsErrorBox::generic(
            "Knot host returned the wrong response type",
        )),
        Err(error) => Err(JsErrorBox::generic(format!(
            "Knot host rejected completion provider registration: {error:?}"
        ))),
    }
}

#[deno_core::op2]
async fn op_completion_unregister(
    state: Rc<RefCell<OpState>>,
    #[number] registration: u64,
) -> Result<(), JsErrorBox> {
    let registration = protocol::CompletionProviderRegistrationId::new(registration);
    let response = request_host_operation(
        state,
        HostOperation::UnregisterCompletionProvider { registration },
    )
    .await?;
    match response.result {
        Ok(HostResponseValue::CompletionProviderUnregistered { .. }) => Ok(()),
        Ok(_) => Err(JsErrorBox::generic(
            "Knot host returned the wrong response type",
        )),
        Err(error) => Err(JsErrorBox::generic(format!(
            "Knot host rejected completion provider disposal: {error:?}"
        ))),
    }
}

#[deno_core::op2]
fn op_completion_complete(
    state: &mut OpState,
    #[serde] completion: NativeCompletionCompletion,
) -> Result<(), JsErrorBox> {
    let result = match (completion.items, completion.error) {
        (Some(items), None) => Ok(items),
        (_, Some(message)) => Err(CompletionProviderError { message }),
        _ => Err(CompletionProviderError {
            message: "completion provider returned no items".into(),
        }),
    };
    let response = CompletionResponse {
        registration: protocol::CompletionProviderRegistrationId::new(completion.registration),
        revision: completion.revision,
        generation: completion.generation,
        result,
    };
    let store = state.borrow_mut::<CompletionCallbackStore>();
    if store.response.replace(response).is_some() {
        return Err(JsErrorBox::generic(
            "completion provider completed one request more than once",
        ));
    }
    Ok(())
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

/// Process-wide owner of the V8 platform and host asynchronous work.
///
/// Construct this on the process's parent thread before loading any
/// extensions. `JsRuntime::init_platform` is idempotent, but V8 itself is a
/// process-global resource, so `V8Host` intentionally does not offer a
/// per-extension initializer.
pub struct V8Host {
    async_runtime: Arc<tokio::runtime::Runtime>,
    next_lifecycle_id: std::sync::atomic::AtomicU64,
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
            next_lifecycle_id: std::sync::atomic::AtomicU64::new(1),
        }
    }

    /// Start one extension-owned runtime thread.
    ///
    /// The thread owns its `JsRuntime` and all extension-local state. Script
    /// and module execution remain fixture-level Step 7 probes.
    pub fn spawn_extension(&self, extension: ExtensionId) -> ExtensionRuntimeHandle {
        let lifecycle = ExtensionLifecycleId::new(
            self.next_lifecycle_id
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        );
        ExtensionRuntimeHandle::spawn(extension, lifecycle, Arc::clone(&self.async_runtime))
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
    fn spawn(
        extension: ExtensionId,
        lifecycle_id: ExtensionLifecycleId,
        async_runtime: Arc<tokio::runtime::Runtime>,
    ) -> Self {
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
                        lifecycle_id,
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
            lifecycle_id,
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

    /// Invoke one registered command on this extension's thread.
    pub fn invoke_command(
        &self,
        invocation: CommandInvocation,
        active_buffer: Option<BufferHandle>,
    ) -> ExtensionRuntimeExecution {
        self.control.invoke_command(invocation, active_buffer)
    }

    /// Abort one active command without queueing work behind its JavaScript.
    pub fn cancel_command(
        &self,
        invocation: CommandInvocationId,
    ) -> Result<(), ExtensionRuntimeClosed> {
        self.control.cancel_command(invocation)
    }

    /// Queue one committed buffer change for this extension's subscription.
    pub fn dispatch_buffer_change(
        &self,
        subscription: BufferSubscriptionId,
        change: BufferChange,
    ) -> Result<(), ExtensionRuntimeClosed> {
        self.control.dispatch_buffer_change(subscription, change)
    }

    /// Queue one native tree-view child request on this extension's thread.
    pub fn request_tree_children(&self, request: TreeChildrenRequest) -> ExtensionTreeRequest {
        self.control.request_tree_children(request)
    }

    /// Queue one native editor completion request on this extension's thread.
    pub fn request_completions(&self, request: CompletionRequest) -> ExtensionCompletionRequest {
        self.control.request_completions(request)
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

    /// Return recorded buffer-change delivery measurements for this runtime.
    pub fn buffer_change_queue_metrics(&self) -> BufferChangeQueueMetrics {
        self.control.buffer_change_queue_metrics()
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
    lifecycle_id: ExtensionLifecycleId,
    commands: Sender<RuntimeCommand>,
    lifecycle: Arc<ExtensionLifecycle>,
}

impl ExtensionRuntimeControl {
    pub(crate) fn identity(&self) -> (ExtensionId, ExtensionLifecycleId) {
        (self.extension, self.lifecycle_id)
    }

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

    /// Invoke one registered command on this extension's thread.
    pub fn invoke_command(
        &self,
        invocation: CommandInvocation,
        active_buffer: Option<BufferHandle>,
    ) -> ExtensionRuntimeExecution {
        let (completion, completed) = tokio::sync::oneshot::channel();
        let _ = self.commands.send(RuntimeCommand::InvokeCommand {
            invocation,
            active_buffer,
            completion,
        });
        ExtensionRuntimeExecution {
            completion: completed,
        }
    }

    /// Abort one active command without queueing work behind its JavaScript.
    pub fn cancel_command(
        &self,
        invocation: CommandInvocationId,
    ) -> Result<(), ExtensionRuntimeClosed> {
        self.lifecycle.cancel_command(invocation)
    }

    pub fn dispatch_buffer_change(
        &self,
        subscription: BufferSubscriptionId,
        change: BufferChange,
    ) -> Result<(), ExtensionRuntimeClosed> {
        let enqueued_at = Instant::now();
        self.lifecycle.buffer_change_enqueued();
        self.commands
            .send(RuntimeCommand::DispatchBufferChange {
                subscription,
                change,
                enqueued_at,
            })
            .map_err(|_| {
                self.lifecycle.buffer_change_discarded();
                ExtensionRuntimeClosed
            })
    }

    pub fn request_tree_children(&self, request: TreeChildrenRequest) -> ExtensionTreeRequest {
        let (completion, completed) = tokio::sync::oneshot::channel();
        let _ = self.commands.send(RuntimeCommand::RequestTreeChildren {
            request,
            completion,
        });
        ExtensionTreeRequest {
            completion: completed,
        }
    }

    pub fn request_completions(&self, request: CompletionRequest) -> ExtensionCompletionRequest {
        let (completion, completed) = tokio::sync::oneshot::channel();
        let _ = self.commands.send(RuntimeCommand::RequestCompletions {
            request,
            completion,
        });
        ExtensionCompletionRequest {
            completion: completed,
        }
    }

    /// Return measurements for buffer-change callback delivery on this extension.
    ///
    /// These are evidence only: the runtime does not drop, coalesce, block, or
    /// otherwise apply a slow-consumer policy based on them.
    pub fn buffer_change_queue_metrics(&self) -> BufferChangeQueueMetrics {
        self.lifecycle.buffer_change_queue_metrics()
    }

    /// Return a host response to this extension runtime.
    pub fn respond(&self, response: HostResponse) -> Result<(), ExtensionRuntimeResponseError> {
        if response.extension != self.extension || response.lifecycle != self.lifecycle_id {
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

/// Awaitable response to one asynchronous tree-provider callback.
#[must_use = "tree child requests must be awaited or explicitly discarded"]
pub struct ExtensionTreeRequest {
    completion: tokio::sync::oneshot::Receiver<
        Result<TreeChildrenResponse, ExtensionRuntimeExecutionError>,
    >,
}

impl Future for ExtensionTreeRequest {
    type Output = Result<TreeChildrenResponse, ExtensionRuntimeExecutionError>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.completion)
            .poll(context)
            .map(|completion| completion.unwrap_or(Err(ExtensionRuntimeExecutionError::Closed)))
    }
}

/// Awaitable response to one asynchronous completion-provider callback.
#[must_use = "completion requests must be awaited or explicitly discarded"]
pub struct ExtensionCompletionRequest {
    completion:
        tokio::sync::oneshot::Receiver<Result<CompletionResponse, ExtensionRuntimeExecutionError>>,
}

impl Future for ExtensionCompletionRequest {
    type Output = Result<CompletionResponse, ExtensionRuntimeExecutionError>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.completion)
            .poll(context)
            .map(|completion| completion.unwrap_or(Err(ExtensionRuntimeExecutionError::Closed)))
    }
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
    InvalidCommandArguments { report: String },
    JavaScriptException { report: String },
}

impl ExtensionRuntimeExecutionError {
    fn javascript_exception(error: impl std::fmt::Display) -> Self {
        Self::JavaScriptException {
            report: format!("{error:#}"),
        }
    }

    fn command_handler(error: impl std::fmt::Display) -> Self {
        let report = format!("{error:#}");
        if report.contains("InvalidCommandArgumentsError") {
            Self::InvalidCommandArguments { report }
        } else {
            Self::JavaScriptException { report }
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
    InvokeCommand {
        invocation: CommandInvocation,
        active_buffer: Option<BufferHandle>,
        completion: tokio::sync::oneshot::Sender<Result<(), ExtensionRuntimeExecutionError>>,
    },
    DispatchBufferChange {
        subscription: BufferSubscriptionId,
        change: BufferChange,
        enqueued_at: Instant,
    },
    RequestTreeChildren {
        request: TreeChildrenRequest,
        completion: tokio::sync::oneshot::Sender<
            Result<TreeChildrenResponse, ExtensionRuntimeExecutionError>,
        >,
    },
    RequestCompletions {
        request: CompletionRequest,
        completion: tokio::sync::oneshot::Sender<
            Result<CompletionResponse, ExtensionRuntimeExecutionError>,
        >,
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
    buffer_change_queue: BufferChangeQueueMetrics,
    queued_buffer_change_callbacks: usize,
    command_cancellations: HashMap<CommandInvocationId, CommandCancellationEntry>,
}

struct CommandCancellationEntry {
    sender: tokio::sync::watch::Sender<CommandCancellationState>,
    watcher_finished: bool,
}

impl CommandCancellationEntry {
    fn running() -> Self {
        Self {
            sender: tokio::sync::watch::channel(CommandCancellationState::Running).0,
            watcher_finished: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CommandCancellationState {
    Running,
    Cancelled,
    Finished,
}

/// Observed buffer-change callback pressure for one extension runtime.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BufferChangeQueueMetrics {
    /// Largest number of queued callbacks awaiting their start.
    pub max_depth: usize,
    /// Largest delay from enqueueing a callback until its execution starts.
    pub max_enqueue_to_start_lag: Duration,
}

impl ExtensionLifecycle {
    async fn wait_for_command_cancellation(&self, invocation: CommandInvocationId) -> bool {
        let mut receiver = {
            let mut state = self
                .state
                .lock()
                .expect("extension lifecycle lock poisoned");
            state
                .command_cancellations
                .entry(invocation)
                .or_insert_with(CommandCancellationEntry::running)
                .sender
                .subscribe()
        };
        let cancelled = loop {
            let current = *receiver.borrow_and_update();
            match current {
                CommandCancellationState::Cancelled => break true,
                CommandCancellationState::Finished => break false,
                CommandCancellationState::Running => {}
            }
            if receiver.changed().await.is_err() {
                break false;
            }
        };
        let mut state = self
            .state
            .lock()
            .expect("extension lifecycle lock poisoned");
        if let Some(entry) = state.command_cancellations.get_mut(&invocation) {
            entry.watcher_finished = true;
            if *entry.sender.borrow() == CommandCancellationState::Finished {
                state.command_cancellations.remove(&invocation);
            }
        }
        cancelled
    }

    fn cancel_command(
        &self,
        invocation: CommandInvocationId,
    ) -> Result<(), ExtensionRuntimeClosed> {
        let mut state = self
            .state
            .lock()
            .expect("extension lifecycle lock poisoned");
        if state.torn_down {
            return Err(ExtensionRuntimeClosed);
        }
        let entry = state
            .command_cancellations
            .entry(invocation)
            .or_insert_with(CommandCancellationEntry::running);
        entry
            .sender
            .send_replace(CommandCancellationState::Cancelled);
        Ok(())
    }

    fn finish_command_cancellation(&self, invocation: CommandInvocationId) {
        let mut state = self
            .state
            .lock()
            .expect("extension lifecycle lock poisoned");
        let entry = state
            .command_cancellations
            .entry(invocation)
            .or_insert_with(CommandCancellationEntry::running);
        if entry.watcher_finished {
            state.command_cancellations.remove(&invocation);
        } else {
            entry
                .sender
                .send_replace(CommandCancellationState::Finished);
        }
    }

    fn buffer_change_enqueued(&self) {
        let mut state = self
            .state
            .lock()
            .expect("extension lifecycle lock poisoned");
        state.queued_buffer_change_callbacks += 1;
        state.buffer_change_queue.max_depth = state
            .buffer_change_queue
            .max_depth
            .max(state.queued_buffer_change_callbacks);
    }

    fn buffer_change_discarded(&self) {
        let mut state = self
            .state
            .lock()
            .expect("extension lifecycle lock poisoned");
        state.queued_buffer_change_callbacks =
            state.queued_buffer_change_callbacks.saturating_sub(1);
    }

    fn buffer_change_started(&self, enqueued_at: Instant) {
        let mut state = self
            .state
            .lock()
            .expect("extension lifecycle lock poisoned");
        state.queued_buffer_change_callbacks =
            state.queued_buffer_change_callbacks.saturating_sub(1);
        state.buffer_change_queue.max_enqueue_to_start_lag = state
            .buffer_change_queue
            .max_enqueue_to_start_lag
            .max(enqueued_at.elapsed());
    }

    fn buffer_change_queue_metrics(&self) -> BufferChangeQueueMetrics {
        self.state
            .lock()
            .expect("extension lifecycle lock poisoned")
            .buffer_change_queue
    }
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
        for cancellation in state.command_cancellations.values() {
            let _ = cancellation
                .sender
                .send(CommandCancellationState::Cancelled);
        }
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
    lifecycle_id: ExtensionLifecycleId,
    next_request_id: u64,
    invocations: Vec<CommandInvocationId>,
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
                lifecycle: self.lifecycle_id,
                id,
                invocation: self.invocations.last().copied(),
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
        lifecycle_id: ExtensionLifecycleId,
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
            .put(HostAsyncRuntime(async_runtime));
        js_runtime
            .op_state()
            .borrow_mut()
            .put(NativeResponseStore::new());
        js_runtime
            .op_state()
            .borrow_mut()
            .put(TreeCallbackStore::default());
        js_runtime
            .op_state()
            .borrow_mut()
            .put(CompletionCallbackStore::default());
        js_runtime
            .op_state()
            .borrow_mut()
            .put(ExtensionRequestRouter {
                extension,
                lifecycle_id,
                next_request_id: 0,
                invocations: Vec::new(),
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
                RuntimeCommand::InvokeCommand {
                    invocation,
                    active_buffer,
                    completion,
                } => {
                    let _runtime_guard = self.event_loop_runtime.enter();
                    self.js_runtime
                        .op_state()
                        .borrow_mut()
                        .borrow_mut::<ExtensionRequestRouter>()
                        .invocations
                        .push(invocation.id);
                    let arguments = serde_json::to_string(&invocation.arguments)
                        .expect("command arguments are JSON-compatible");
                    let source = format!(
                        "globalThis.__knotInvokeCommand({}, {}, {}, {arguments})",
                        invocation.id.value(),
                        invocation.registration.value(),
                        active_buffer.map_or("null".into(), |buffer| buffer.value().to_string()),
                    );
                    let result = self
                        .js_runtime
                        .execute_script("knot:command-invocation", source)
                        .map_err(ExtensionRuntimeExecutionError::command_handler)
                        .and_then(|_| {
                            self.event_loop_runtime
                                .block_on(self.js_runtime.run_event_loop(Default::default()))
                                .map_err(ExtensionRuntimeExecutionError::command_handler)
                        });
                    self.js_runtime
                        .op_state()
                        .borrow_mut()
                        .borrow_mut::<ExtensionRequestRouter>()
                        .invocations
                        .pop();
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
                RuntimeCommand::DispatchBufferChange {
                    subscription,
                    change,
                    enqueued_at,
                } => {
                    self.lifecycle.buffer_change_started(enqueued_at);
                    let _runtime_guard = self.event_loop_runtime.enter();
                    let change = serde_json::json!({
                        "beforeRevision": change.before_revision,
                        "revision": change.revision,
                        "edits": change.edits,
                    });
                    let source = format!(
                        "globalThis.__knotDispatchBufferChange({}, {})",
                        subscription.value(),
                        change,
                    );
                    let result = self
                        .js_runtime
                        .execute_script("knot:buffer-change", source)
                        .map_err(ExtensionRuntimeExecutionError::javascript_exception)
                        .and_then(|_| {
                            self.event_loop_runtime
                                .block_on(self.js_runtime.run_event_loop(Default::default()))
                                .map_err(ExtensionRuntimeExecutionError::javascript_exception)
                        });
                    if let Err(error) = result {
                        eprintln!("[knot] buffer-change listener failed: {error:?}");
                    }
                    if self.lifecycle.termination().is_some() {
                        break;
                    }
                }
                RuntimeCommand::RequestTreeChildren {
                    request,
                    completion,
                } => {
                    let _runtime_guard = self.event_loop_runtime.enter();
                    self.js_runtime
                        .op_state()
                        .borrow_mut()
                        .borrow_mut::<TreeCallbackStore>()
                        .response = None;
                    let parent_id =
                        serde_json::to_string(&request.parent_id).expect("tree parent serializes");
                    let source = format!(
                        "globalThis.__knotRequestTreeChildren({}, {}, {})",
                        request.registration.value(),
                        parent_id,
                        request.generation,
                    );
                    let result = self
                        .js_runtime
                        .execute_script("knot:tree-children", source)
                        .map_err(ExtensionRuntimeExecutionError::javascript_exception)
                        .and_then(|_| {
                            self.event_loop_runtime
                                .block_on(self.js_runtime.run_event_loop(Default::default()))
                                .map_err(ExtensionRuntimeExecutionError::javascript_exception)
                        })
                        .and_then(|()| {
                            self.js_runtime
                                .op_state()
                                .borrow_mut()
                                .borrow_mut::<TreeCallbackStore>()
                                .response
                                .take()
                                .ok_or_else(|| {
                                    ExtensionRuntimeExecutionError::javascript_exception(
                                        "tree provider did not complete its request",
                                    )
                                })
                        });
                    if let Ok(response) = &result
                        && let Err(error) = &response.result
                    {
                        eprintln!("[knot] tree provider callback failed: {}", error.message);
                    }
                    let _ = completion.send(result);
                    if self.lifecycle.termination().is_some() {
                        break;
                    }
                }
                RuntimeCommand::RequestCompletions {
                    request,
                    completion,
                } => {
                    let _runtime_guard = self.event_loop_runtime.enter();
                    self.js_runtime
                        .op_state()
                        .borrow_mut()
                        .borrow_mut::<CompletionCallbackStore>()
                        .response = None;
                    let prefix = serde_json::to_string(&request.prefix)
                        .expect("completion prefix serializes");
                    let source = format!(
                        "globalThis.__knotRequestCompletions({}, {}, {}, {}, {}, {})",
                        request.registration.value(),
                        request.buffer.value(),
                        request.revision,
                        request.cursor_byte_offset,
                        prefix,
                        request.generation,
                    );
                    let result = self
                        .js_runtime
                        .execute_script("knot:completions", source)
                        .map_err(ExtensionRuntimeExecutionError::javascript_exception)
                        .and_then(|_| {
                            self.event_loop_runtime
                                .block_on(self.js_runtime.run_event_loop(Default::default()))
                                .map_err(ExtensionRuntimeExecutionError::javascript_exception)
                        })
                        .and_then(|()| {
                            self.js_runtime
                                .op_state()
                                .borrow_mut()
                                .borrow_mut::<CompletionCallbackStore>()
                                .response
                                .take()
                                .ok_or_else(|| {
                                    ExtensionRuntimeExecutionError::javascript_exception(
                                        "completion provider did not complete its request",
                                    )
                                })
                        });
                    let _ = completion.send(result);
                    if self.lifecycle.termination().is_some() {
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
    use std::{
        process::Command,
        sync::Arc,
        time::{Duration, Instant},
    };

    use super::{
        ExtensionLifecycle, ExtensionRuntimeClosed, ExtensionRuntimeExecutionError,
        ExtensionRuntimeResponseError, V8Host,
    };
    use crate::host::protocol::{
        BufferChange, BufferHandle, BufferSubscriptionId, ByteRange, CommandArgumentValue,
        CommandInvocation, CommandInvocationId, CommandInvokeDispatch, CommandOutcome,
        CommandRegistrationId, CompletionProviderRegistrationId, CompletionRequest, ExtensionId,
        ExtensionLifecycleId, HostOperation, HostRequestError, HostResponse, HostResponseValue,
        RequestId, SnapshotText, TextEdit, TextSnapshot, TreeChildrenRequest, TreeCollapsibleState,
        TreeIcon, TreeProviderRegistrationId,
    };

    #[test]
    fn initializes_v8_and_the_shared_async_runtime() {
        let host = V8Host::new();
        host.async_runtime.block_on(async {
            assert!(tokio::runtime::Handle::try_current().is_ok());
        });
    }

    #[test]
    fn external_utf16_snapshot_storage_is_shared_and_released_by_isolates() {
        let host = V8Host::new();
        let mut first = host.spawn_extension(ExtensionId::new(70));
        let mut second = host.spawn_extension(ExtensionId::new(71));
        let text: Arc<[u16]> = "héllo".encode_utf16().collect::<Vec<_>>().into();
        let weak = Arc::downgrade(&text);

        for (runtime, extension, module) in [
            (
                &mut first,
                ExtensionId::new(70),
                "file:///external-first.js",
            ),
            (
                &mut second,
                ExtensionId::new(71),
                "file:///external-second.js",
            ),
        ] {
            let execution = runtime.execute_fixture_module(
                module,
                r#"
                    import { editor } from "knot:editor";
                    const buffer = await editor.activeBuffer();
                    globalThis.heldSnapshot = await buffer.snapshot();
                    if (globalThis.heldSnapshot.text !== "héllo") throw new Error("snapshot");
                "#,
            );
            let active = pollster::block_on(runtime.receive_request()).unwrap();
            runtime
                .respond(HostResponse {
                    extension,
                    lifecycle: active.lifecycle,
                    id: active.id,
                    result: Ok(HostResponseValue::ActiveBuffer(Some(BufferHandle::new(1)))),
                })
                .unwrap();
            let snapshot = pollster::block_on(runtime.receive_request()).unwrap();
            runtime
                .respond(HostResponse {
                    extension,
                    lifecycle: snapshot.lifecycle,
                    id: snapshot.id,
                    result: Ok(HostResponseValue::Snapshot(TextSnapshot {
                        text: SnapshotText::Utf16(Arc::clone(&text)),
                        range: ByteRange {
                            start_byte_offset: 0,
                            end_byte_offset: 6,
                        },
                        revision: 0,
                    })),
                })
                .unwrap();
            pollster::block_on(execution).unwrap();
        }

        drop(text);
        assert_eq!(weak.strong_count(), 2);
        first.shutdown();
        assert_eq!(weak.strong_count(), 1);
        second.shutdown();
        assert_eq!(weak.strong_count(), 0);
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
                lifecycle: first.lifecycle,
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
                lifecycle: request.lifecycle,
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
    fn tree_provider_registration_invalidation_and_children_are_asynchronous() {
        let host = V8Host::new();
        let extension = ExtensionId::new(72);
        let mut runtime = host.spawn_extension(extension);
        let registration = TreeProviderRegistrationId::new(4);
        let execution = runtime.execute_fixture_module(
            "file:///fixtures/tree-provider.js",
            r#"
                import { workbench } from "knot:editor";
                globalThis.treeRegistration = await workbench.registerTreeDataProvider(
                  "outline",
                  {
                    async getChildren(parentId) {
                      if (parentId === "failure") throw new Error("expected tree failure");
                      return [{
                        id: parentId === null ? "root" : `${parentId}.child`,
                        label: "Tree item",
                        description: "fixture",
                        icon: "symbol",
                        collapsibleState: parentId === null ? "expanded" : "none",
                      }];
                    },
                  },
                );
            "#,
        );
        let register_request = pollster::block_on(runtime.receive_request()).unwrap();
        assert_eq!(
            register_request.operation,
            HostOperation::RegisterTreeProvider {
                view_id: "outline".into()
            }
        );
        runtime
            .respond(HostResponse {
                extension,
                lifecycle: register_request.lifecycle,
                id: register_request.id,
                result: Ok(HostResponseValue::TreeProviderRegistered { registration }),
            })
            .unwrap();
        pollster::block_on(execution).unwrap();

        let response = pollster::block_on(runtime.request_tree_children(TreeChildrenRequest {
            registration,
            parent_id: None,
            generation: 8,
        }))
        .unwrap();
        assert_eq!(response.generation, 8);
        let items = response.result.unwrap();
        assert_eq!(items[0].id, "root");
        assert_eq!(items[0].icon, Some(TreeIcon::Symbol));
        assert_eq!(items[0].collapsible_state, TreeCollapsibleState::Expanded);

        let invalidation_execution = runtime.execute_fixture_script(
            "invalidate-tree.js",
            "globalThis.treeRegistration.invalidate('root')",
        );
        let invalidation = pollster::block_on(runtime.receive_request()).unwrap();
        assert_eq!(
            invalidation.operation,
            HostOperation::InvalidateTreeProvider {
                registration,
                parent_id: Some("root".into()),
            }
        );
        runtime
            .respond(HostResponse {
                extension,
                lifecycle: invalidation.lifecycle,
                id: invalidation.id,
                result: Ok(HostResponseValue::TreeProviderInvalidated),
            })
            .unwrap();
        pollster::block_on(invalidation_execution).unwrap();

        let failure = pollster::block_on(runtime.request_tree_children(TreeChildrenRequest {
            registration,
            parent_id: Some("failure".into()),
            generation: 9,
        }))
        .unwrap();
        assert!(
            failure
                .result
                .unwrap_err()
                .message
                .contains("expected tree failure")
        );
        runtime.shutdown();
    }

    #[test]
    fn completion_provider_registration_and_callback_use_typed_reverse_transport() {
        let host = V8Host::new();
        let extension = ExtensionId::new(73);
        let mut runtime = host.spawn_extension(extension);
        let registration = CompletionProviderRegistrationId::new(5);
        let execution = runtime.execute_fixture_module(
            "file:///fixtures/completion-provider.js",
            r#"
                import { editor } from "knot:editor";
                await editor.registerCompletionProvider("fixture", {
                  async provideCompletions(context) {
                    return [{ label: `${context.prefix}Item`, insertText: "inserted" }];
                  },
                });
            "#,
        );
        let register_request = pollster::block_on(runtime.receive_request()).unwrap();
        assert_eq!(
            register_request.operation,
            HostOperation::RegisterCompletionProvider {
                label: "fixture".into()
            }
        );
        runtime
            .respond(HostResponse {
                extension,
                lifecycle: register_request.lifecycle,
                id: register_request.id,
                result: Ok(HostResponseValue::CompletionProviderRegistered { registration }),
            })
            .unwrap();
        pollster::block_on(execution).unwrap();

        let response = pollster::block_on(runtime.request_completions(CompletionRequest {
            registration,
            buffer: BufferHandle::new(2),
            revision: 11,
            cursor_byte_offset: 4,
            prefix: "pre".into(),
            generation: 9,
        }))
        .unwrap();
        assert_eq!(response.registration, registration);
        assert_eq!(response.revision, 11);
        assert_eq!(response.generation, 9);
        assert_eq!(response.result.unwrap()[0].label, "preItem");
        runtime.shutdown();
    }

    #[test]
    fn buffer_change_callbacks_are_serial_and_survive_listener_failures() {
        let host = V8Host::new();
        let extension = ExtensionId::new(7);
        let mut runtime = host.spawn_extension(extension);
        let lifecycle = runtime.control.lifecycle_id;
        let buffer = BufferHandle::new(3);
        let subscription = BufferSubscriptionId::new(9);
        let execution = runtime.execute_fixture_module(
            "file:///fixtures/subscription.js",
            r#"
                import { editor } from "knot:editor";
                const buffer = await editor.activeBuffer();
                buffer.onDidChange(async (event) => {
                  globalThis.events = [...(globalThis.events ?? []), event.revision];
                  if (event.revision === 1) throw new Error("expected listener failure");
                });
            "#,
        );
        for response in [
            HostResponseValue::ActiveBuffer(Some(buffer)),
            HostResponseValue::BufferChangesSubscribed { subscription },
        ] {
            let request = pollster::block_on(runtime.receive_request()).unwrap();
            runtime
                .respond(HostResponse {
                    extension,
                    lifecycle: request.lifecycle,
                    id: request.id,
                    result: Ok(response),
                })
                .unwrap();
        }
        pollster::block_on(execution).unwrap();

        for revision in 1..=2 {
            runtime
                .dispatch_buffer_change(
                    subscription,
                    BufferChange {
                        buffer,
                        before_revision: revision - 1,
                        revision,
                        edits: vec![TextEdit {
                            range: ByteRange {
                                start_byte_offset: 0,
                                end_byte_offset: 0,
                            },
                            text: revision.to_string(),
                        }],
                    },
                )
                .unwrap();
        }
        pollster::block_on(runtime.execute_fixture_script(
            "verify-events.js",
            "if (globalThis.events.join(',') !== '1,2') throw new Error('events were not serial')",
        ))
        .unwrap();
        let metrics = runtime.control.buffer_change_queue_metrics();
        assert!(metrics.max_depth >= 1);
        assert!(metrics.max_enqueue_to_start_lag > std::time::Duration::ZERO);
        assert_eq!(runtime.control.identity().1, lifecycle);
        runtime.shutdown();
    }

    #[test]
    fn cpu_bound_buffer_callbacks_run_in_parallel_across_isolates_and_serially_within_one() {
        if std::thread::available_parallelism().is_ok_and(|parallelism| parallelism.get() < 2) {
            return;
        }

        let host = V8Host::new();
        let buffer = BufferHandle::new(3);
        let mut first = host.spawn_extension(ExtensionId::new(7));
        let mut second = host.spawn_extension(ExtensionId::new(8));
        let first_subscription = BufferSubscriptionId::new(9);
        let second_subscription = BufferSubscriptionId::new(10);
        let listener_source = r#"
            import { editor } from "knot:editor";
            const buffer = await editor.activeBuffer();
            globalThis.callbackRunning = 0;
            globalThis.maxConcurrentCallbacks = 0;
            globalThis.events = [];
            await buffer.onDidChange((event) => {
              globalThis.callbackRunning += 1;
              globalThis.maxConcurrentCallbacks = Math.max(
                globalThis.maxConcurrentCallbacks,
                globalThis.callbackRunning,
              );
              const end = Date.now() + 120;
              while (Date.now() < end) {}
              globalThis.events.push(event.revision);
              globalThis.callbackRunning -= 1;
            });
        "#;

        for (runtime, extension, subscription) in [
            (&mut first, ExtensionId::new(7), first_subscription),
            (&mut second, ExtensionId::new(8), second_subscription),
        ] {
            let execution = runtime.execute_fixture_module(
                format!("file:///fixtures/cpu-listener-{}.js", extension.value()),
                listener_source,
            );
            for response in [
                HostResponseValue::ActiveBuffer(Some(buffer)),
                HostResponseValue::BufferChangesSubscribed { subscription },
            ] {
                let request = pollster::block_on(runtime.receive_request()).unwrap();
                runtime
                    .respond(HostResponse {
                        extension,
                        lifecycle: request.lifecycle,
                        id: request.id,
                        result: Ok(response),
                    })
                    .unwrap();
            }
            pollster::block_on(execution).unwrap();
        }

        let started = Instant::now();
        first
            .dispatch_buffer_change(
                first_subscription,
                BufferChange {
                    buffer,
                    before_revision: 0,
                    revision: 1,
                    edits: vec![],
                },
            )
            .unwrap();
        second
            .dispatch_buffer_change(
                second_subscription,
                BufferChange {
                    buffer,
                    before_revision: 0,
                    revision: 1,
                    edits: vec![],
                },
            )
            .unwrap();
        first
            .dispatch_buffer_change(
                first_subscription,
                BufferChange {
                    buffer,
                    before_revision: 1,
                    revision: 2,
                    edits: vec![],
                },
            )
            .unwrap();

        pollster::block_on(first.execute_fixture_script(
            "verify-first-cpu-listeners.js",
            "if (globalThis.events.join(',') !== '1,2' || globalThis.maxConcurrentCallbacks !== 1) throw new Error('first extension callbacks overlapped or reordered')",
        ))
        .unwrap();
        pollster::block_on(second.execute_fixture_script(
            "verify-second-cpu-listener.js",
            "if (globalThis.events.join(',') !== '1' || globalThis.maxConcurrentCallbacks !== 1) throw new Error('second extension callback did not finish')",
        ))
        .unwrap();

        assert!(
            started.elapsed() < Duration::from_millis(320),
            "callbacks did not overlap across extension threads"
        );
        first.shutdown();
        second.shutdown();
    }

    #[test]
    fn finite_slow_subscriber_burst_records_queue_depth_and_lag() {
        let host = V8Host::new();
        let extension = ExtensionId::new(7);
        let buffer = BufferHandle::new(3);
        let subscription = BufferSubscriptionId::new(9);
        let mut runtime = host.spawn_extension(extension);
        let execution = runtime.execute_fixture_module(
            "file:///fixtures/slow-subscriber.js",
            r#"
                import { editor } from "knot:editor";
                const buffer = await editor.activeBuffer();
                globalThis.events = [];
                await buffer.onDidChange((event) => {
                  const end = Date.now() + 40;
                  while (Date.now() < end) {}
                  globalThis.events.push(event.revision);
                });
            "#,
        );
        for response in [
            HostResponseValue::ActiveBuffer(Some(buffer)),
            HostResponseValue::BufferChangesSubscribed { subscription },
        ] {
            let request = pollster::block_on(runtime.receive_request()).unwrap();
            runtime
                .respond(HostResponse {
                    extension,
                    lifecycle: request.lifecycle,
                    id: request.id,
                    result: Ok(response),
                })
                .unwrap();
        }
        pollster::block_on(execution).unwrap();

        for revision in 1..=8 {
            runtime
                .dispatch_buffer_change(
                    subscription,
                    BufferChange {
                        buffer,
                        before_revision: revision - 1,
                        revision,
                        edits: vec![],
                    },
                )
                .unwrap();
        }
        pollster::block_on(runtime.execute_fixture_script(
            "verify-slow-subscriber.js",
            "if (globalThis.events.join(',') !== '1,2,3,4,5,6,7,8') throw new Error('slow subscriber lost or reordered events')",
        ))
        .unwrap();

        let metrics = runtime.control.buffer_change_queue_metrics();
        assert!(metrics.max_depth >= 4, "metrics: {metrics:?}");
        assert!(
            metrics.max_enqueue_to_start_lag >= Duration::from_millis(100),
            "metrics: {metrics:?}"
        );
        runtime.shutdown();
    }

    #[test]
    fn public_facade_registers_and_invokes_a_command_on_its_extension_thread() {
        let host = V8Host::new();
        let extension = ExtensionId::new(7);
        let mut runtime = host.spawn_extension(extension);
        let registration = CommandRegistrationId::new(42);
        let execution = runtime.execute_fixture_module(
            "file:///fixtures/command.js",
            r#"
                import { commands } from "knot:editor";
                await commands.register("knot.fixture.command", (context) => {
                    globalThis.commandRuns = (globalThis.commandRuns ?? 0) + 1;
                    globalThis.commandArguments = context.arguments;
                });
            "#,
        );
        let request = pollster::block_on(runtime.receive_request()).unwrap();
        assert_eq!(
            request.operation,
            HostOperation::RegisterCommand {
                name: "knot.fixture.command".into(),
                title: "knot.fixture.command".into(),
            }
        );
        runtime
            .respond(HostResponse {
                extension,
                lifecycle: request.lifecycle,
                id: request.id,
                result: Ok(HostResponseValue::CommandRegistered { registration }),
            })
            .unwrap();
        pollster::block_on(execution).unwrap();

        pollster::block_on(runtime.invoke_command(
            CommandInvocation {
                id: CommandInvocationId::new(1),
                registration,
                extension,
                lifecycle: request.lifecycle,
                arguments: CommandArgumentValue::String("fixture-argument".into()),
            },
            None,
        ))
        .unwrap();
        pollster::block_on(runtime.execute_fixture_script(
            "verify-command.js",
            r#"
                if (globalThis.commandRuns !== 1) throw new Error('command did not run');
                if (globalThis.commandArguments !== "fixture-argument") {
                  throw new Error('command arguments were not delivered');
                }
            "#,
        ))
        .unwrap();
        runtime.shutdown();
    }

    #[test]
    fn same_runtime_command_invocation_executes_inline_and_restores_the_parent_frame() {
        let host = V8Host::new();
        let extension = ExtensionId::new(70);
        let mut runtime = host.spawn_extension(extension);
        let outer_registration = CommandRegistrationId::new(70);
        let inner_registration = CommandRegistrationId::new(71);
        let registration = runtime.execute_fixture_module(
            "file:///fixtures/inline-commands.js",
            r#"
                import { commands } from "knot:editor";
                globalThis.inlineEvents = [];
                await commands.register("knot.fixture.outer", async () => {
                  globalThis.inlineEvents.push("outer-before");
                  const outcome = await commands.invoke("knot.fixture.inner", null);
                  globalThis.inlineEvents.push(`inner-${outcome.kind}`);
                  globalThis.inlineEvents.push("outer-after");
                });
                await commands.register("knot.fixture.inner", async () => {
                  globalThis.inlineEvents.push("inner-before");
                  await Promise.resolve();
                  globalThis.inlineEvents.push("inner-after");
                });
            "#,
        );
        for expected in [outer_registration, inner_registration] {
            let request = pollster::block_on(runtime.receive_request()).unwrap();
            runtime
                .respond(HostResponse {
                    extension,
                    lifecycle: request.lifecycle,
                    id: request.id,
                    result: Ok(HostResponseValue::CommandRegistered {
                        registration: expected,
                    }),
                })
                .unwrap();
        }
        pollster::block_on(registration).unwrap();

        let lifecycle = runtime.control.lifecycle_id;
        let invocation = runtime.invoke_command(
            CommandInvocation {
                id: CommandInvocationId::new(700),
                registration: outer_registration,
                extension,
                lifecycle,
                arguments: CommandArgumentValue::Null,
            },
            None,
        );
        let request = pollster::block_on(runtime.receive_request()).unwrap();
        assert_eq!(request.invocation, Some(CommandInvocationId::new(700)));
        assert!(matches!(
            request.operation,
            HostOperation::InvokeCommand { .. }
        ));
        runtime
            .respond(HostResponse {
                extension,
                lifecycle,
                id: request.id,
                result: Ok(HostResponseValue::CommandInvoked {
                    dispatch: CommandInvokeDispatch::Inline {
                        invocation: CommandInvocationId::new(701),
                        registration: inner_registration,
                    },
                }),
            })
            .unwrap();

        let completion = pollster::block_on(runtime.receive_request()).unwrap();
        assert_eq!(completion.invocation, Some(CommandInvocationId::new(701)));
        assert_eq!(
            completion.operation,
            HostOperation::CompleteInlineCommand {
                invocation: CommandInvocationId::new(701),
                outcome: CommandOutcome::Completed,
            }
        );
        runtime
            .respond(HostResponse {
                extension,
                lifecycle,
                id: completion.id,
                result: Ok(HostResponseValue::InlineCommandCompleted {
                    invocation: CommandInvocationId::new(701),
                }),
            })
            .unwrap();

        pollster::block_on(invocation).unwrap();

        pollster::block_on(runtime.execute_fixture_script(
            "verify-inline-commands.js",
            r#"
                if (globalThis.inlineEvents.join(",") !==
                    "outer-before,inner-before,inner-after,inner-completed,outer-after") {
                  throw new Error(`unexpected inline ordering: ${globalThis.inlineEvents}`);
                }
            "#,
        ))
        .unwrap();
        runtime.shutdown();
    }

    #[test]
    fn command_cancellation_aborts_a_suspended_handler_without_queueing_runtime_work() {
        let host = V8Host::new();
        let extension = ExtensionId::new(72);
        let mut runtime = host.spawn_extension(extension);
        let registration = CommandRegistrationId::new(72);
        let setup = runtime.execute_fixture_module(
            "file:///fixtures/cancellable-command.js",
            r#"
                import { commands } from "knot:editor";
                await commands.register("knot.fixture.cancellable", async ({ signal }) => {
                  await new Promise((resolve) => signal.addEventListener("abort", resolve));
                  globalThis.cancellationObserved = signal.aborted;
                });
            "#,
        );
        let request = pollster::block_on(runtime.receive_request()).unwrap();
        runtime
            .respond(HostResponse {
                extension,
                lifecycle: request.lifecycle,
                id: request.id,
                result: Ok(HostResponseValue::CommandRegistered { registration }),
            })
            .unwrap();
        pollster::block_on(setup).unwrap();

        let invocation_id = CommandInvocationId::new(720);
        let invocation = runtime.invoke_command(
            CommandInvocation {
                id: invocation_id,
                registration,
                extension,
                lifecycle: request.lifecycle,
                arguments: CommandArgumentValue::Null,
            },
            None,
        );
        runtime.cancel_command(invocation_id).unwrap();
        pollster::block_on(invocation).unwrap();
        pollster::block_on(runtime.execute_fixture_script(
            "verify-command-cancellation.js",
            r#"
                if (!globalThis.cancellationObserved) {
                  throw new Error("command did not observe prompt cancellation");
                }
            "#,
        ))
        .unwrap();
        runtime.shutdown();
    }

    #[test]
    fn command_registration_errors_reach_javascript_with_stable_names() {
        let host = V8Host::new();
        let extension = ExtensionId::new(7);
        let mut runtime = host.spawn_extension(extension);
        let execution = runtime.execute_fixture_module(
            "file:///fixtures/duplicate-command.js",
            r#"
                import { commands } from "knot:editor";
                await commands.register("editor.copy", () => {});
            "#,
        );
        let request = pollster::block_on(runtime.receive_request()).unwrap();
        runtime
            .respond(HostResponse {
                extension,
                lifecycle: request.lifecycle,
                id: request.id,
                result: Err(HostRequestError::CommandNameInUse),
            })
            .unwrap();

        let error = pollster::block_on(execution).unwrap_err();
        let ExtensionRuntimeExecutionError::JavaScriptException { report } = error else {
            panic!("expected JavaScript exception: {error:?}");
        };
        assert!(report.contains("CommandNameInUseError"), "{report}");
        runtime.shutdown();
    }

    #[test]
    fn disposing_a_command_prevents_later_invocation() {
        let host = V8Host::new();
        let extension = ExtensionId::new(7);
        let mut runtime = host.spawn_extension(extension);
        let registration = CommandRegistrationId::new(42);
        let execution = runtime.execute_fixture_module(
            "file:///fixtures/dispose-command.js",
            r#"
                import { commands } from "knot:editor";
                globalThis.disposable = await commands.register("knot.fixture.dispose", () => {});
            "#,
        );
        let request = pollster::block_on(runtime.receive_request()).unwrap();
        runtime
            .respond(HostResponse {
                extension,
                lifecycle: request.lifecycle,
                id: request.id,
                result: Ok(HostResponseValue::CommandRegistered { registration }),
            })
            .unwrap();
        pollster::block_on(execution).unwrap();

        let disposal =
            runtime.execute_fixture_script("dispose-command.js", "globalThis.disposable.dispose()");
        let request = pollster::block_on(runtime.receive_request()).unwrap();
        assert_eq!(
            request.operation,
            HostOperation::UnregisterCommand { registration }
        );
        runtime
            .respond(HostResponse {
                extension,
                lifecycle: request.lifecycle,
                id: request.id,
                result: Ok(HostResponseValue::CommandUnregistered { registration }),
            })
            .unwrap();
        pollster::block_on(disposal).unwrap();

        let error = pollster::block_on(runtime.invoke_command(
            CommandInvocation {
                id: CommandInvocationId::new(1),
                registration,
                extension,
                lifecycle: request.lifecycle,
                arguments: CommandArgumentValue::Null,
            },
            None,
        ))
        .unwrap_err();
        assert!(matches!(
            error,
            ExtensionRuntimeExecutionError::JavaScriptException { .. }
        ));
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
            lifecycle: runtime.control.lifecycle_id,
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
            lifecycle: runtime.control.lifecycle_id,
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
                lifecycle: ExtensionLifecycleId::new(1),
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
                lifecycle: request.lifecycle,
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
                lifecycle: request.lifecycle,
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
                lifecycle: request.lifecycle,
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
