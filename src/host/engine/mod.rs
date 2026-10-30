//! V8 process initialization and engine-owned isolate state.
//!
//! [`capsule`] runs turns inside one persistent isolate, [`execution`] holds
//! the futures and handles for work submitted to it, [`pool`] owns capsule
//! lifetimes and loading, [`modules`] compiles and resolves module graphs, and
//! [`marshal`] converts between V8 values and the typed host protocol.

use std::{
    collections::{HashMap, HashSet},
    ffi::c_void,
    fmt,
    pin::pin,
    ptr,
    sync::{
        Arc, Mutex, Once,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    thread::ThreadId,
    time::{Duration, Instant},
};

use url::Url;

#[cfg(test)]
use super::protocol::{
    CommandInvokeDispatch, CommandRegistrationId, CompletionProviderRegistrationId,
    TreeProviderRegistrationId,
};
#[cfg(test)]
use std::sync::mpsc;

use super::{
    declarations::{HostError, HostFailure, TransportError},
    lifecycle::{ExtensionKey, Failure},
    module_graph::{ConfigPhase, ModuleGraph},
    protocol::{
        ApplicationChange, ApplicationEvent, BufferChange, BufferHandle, BufferSubscriptionId,
        ByteRange, Command, CommandArgumentValue, CommandInvocation, CommandInvocationId,
        CommandOutcome, CompletionProviderError, CompletionRequest, CompletionResponse,
        CompletionResultItem, EditorContribution, ExtensionId, HostRequest, HostResponse,
        KeybindingMutation, ProviderIdentity, RequestId, SettingsSubscriptionId, SnapshotText,
        TextEdit, TextSnapshot, TreeAction, TreeChildrenRequest, TreeChildrenResponse,
        TreeCollapsibleState, TreeIcon, TreeItem, TreeItemKind, TreeProviderError,
        WorkbenchSubscriptionId,
    },
    scheduler::{
        CompletionOutcome, CompletionReceiver, PoolConfig, RootId, SchedulerError, SchedulerHandle,
        SchedulerPool, Turn, TurnOutcome,
    },
};

static V8_INITIALIZATION: Once = Once::new();
const DEFAULT_HEAP_LIMIT_BYTES: usize = 32 * 1024 * 1024;
const ACTIVE: u8 = 0;
const TERMINATED: u8 = 1;
const HEAP_LIMIT_EXCEEDED: u8 = 2;
const DISPOSED: u8 = 3;
const PRIVATE_BOOTSTRAP_SPECIFIER: &str = "knot:bootstrap";
const PUBLIC_FACADE_SPECIFIER: &str = "knot";
const NATIVE_BINDINGS_GLOBAL: &str = "__knotNativeBindings";
const PRIVATE_BOOTSTRAP_SOURCE: &str = include_str!("../js/bootstrap.js");
const PUBLIC_FACADE_SOURCE: &str = include_str!("../js/knot.js");

type ResultSender<T> = tokio::sync::oneshot::Sender<Result<T, RuntimeError>>;
type ResultReceiver<T> = tokio::sync::oneshot::Receiver<Result<T, RuntimeError>>;

/// Initializes V8 before any extension worker thread is created.
pub(crate) fn initialize() {
    V8_INITIALIZATION.call_once(|| {
        let platform = v8::new_default_platform(0, false).make_shared();
        v8::V8::initialize_platform(platform);
        v8::V8::initialize();
    });
}

/// Engine configuration for one extension lifetime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct IsolateConfig {
    heap_limit_bytes: usize,
}

impl IsolateConfig {
    pub(crate) const fn with_heap_limit(heap_limit_bytes: usize) -> Self {
        Self { heap_limit_bytes }
    }
}

impl Default for IsolateConfig {
    fn default() -> Self {
        Self::with_heap_limit(DEFAULT_HEAP_LIMIT_BYTES)
    }
}

/// Stable, Knot-owned classification of a JavaScript runtime failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeErrorKind {
    Compilation,
    InvalidModuleSpecifier,
    ModuleResolution,
    Exception,
    Rejection,
    Terminated,
    HeapLimitExceeded,
    Cancelled,
    LifecycleFailed,
    Disposed,
    Engine,
}

/// A runtime failure containing no V8-owned values or handles.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeError {
    kind: RuntimeErrorKind,
    message: Arc<str>,
    source: Option<Arc<str>>,
    line: Option<u32>,
    column: Option<u32>,
    stack: Option<Arc<str>>,
}

impl RuntimeError {
    #[cfg(test)]
    pub(crate) fn kind(&self) -> RuntimeErrorKind {
        self.kind
    }

    pub(crate) fn message(&self) -> &str {
        &self.message
    }

    pub(crate) fn source(&self) -> Option<&str> {
        self.source.as_deref()
    }

    pub(crate) fn line(&self) -> Option<u32> {
        self.line
    }

    pub(crate) fn column(&self) -> Option<u32> {
        self.column
    }

    pub(crate) fn stack(&self) -> Option<&str> {
        self.stack.as_deref()
    }

    fn fatal(kind: RuntimeErrorKind, message: impl Into<Arc<str>>) -> Self {
        Self {
            kind,
            message: message.into(),
            source: None,
            line: None,
            column: None,
            stack: None,
        }
    }

    fn disposed() -> Self {
        Self::fatal(RuntimeErrorKind::Disposed, "extension isolate is disposed")
    }

    fn is_fatal(&self) -> bool {
        matches!(
            self.kind,
            RuntimeErrorKind::Terminated
                | RuntimeErrorKind::HeapLimitExceeded
                | RuntimeErrorKind::Disposed
                | RuntimeErrorKind::Engine
        )
    }
}

/// A failure to admit or address a runtime lifecycle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RuntimePoolError {
    Scheduler(SchedulerError),
    Runtime(RuntimeError),
}

/// A host response that cannot address one live pending request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeResponseError {
    Scheduler(SchedulerError),
    WrongExtension,
    UnknownRequest,
}

impl From<SchedulerError> for RuntimeResponseError {
    fn from(error: SchedulerError) -> Self {
        Self::Scheduler(error)
    }
}

impl From<SchedulerError> for RuntimePoolError {
    fn from(error: SchedulerError) -> Self {
        Self::Scheduler(error)
    }
}

impl From<RuntimeError> for RuntimePoolError {
    fn from(error: RuntimeError) -> Self {
        Self::Runtime(error)
    }
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.message)?;
        if let (Some(source), Some(line), Some(column)) = (&self.source, self.line, self.column) {
            write!(formatter, " ({source}:{line}:{column})")?;
        }
        Ok(())
    }
}

impl std::error::Error for RuntimeError {}

impl fmt::Display for RuntimePoolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Scheduler(error) => error.fmt(formatter),
            Self::Runtime(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for RuntimePoolError {}

/// Knot-owned result diagnostics for one isolate turn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ExecutionReport {
    pub(crate) value: Arc<str>,
    pub(crate) worker: ThreadId,
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
enum RuntimeCallback {
    ApplicationChange,
    BufferChange,
    SettingChange,
    Command,
    CancelCommand,
    Tree,
    Completion,
}

struct RuntimeLocalState {
    key: ExtensionKey,
    status: Arc<AtomicU8>,
    next_request: AtomicU64,
    event_sender: Mutex<Option<tokio::sync::mpsc::UnboundedSender<EngineEvent>>>,
    current_root: Mutex<Option<RootId>>,
    pending: Mutex<HashMap<RequestId, PendingRequest>>,
    active: Mutex<Option<ActiveExecution>>,
    rejections: Mutex<Vec<RejectionReport>>,
    modules: Mutex<ModuleRegistry>,
    callbacks: Mutex<HashMap<RuntimeCallback, v8::Global<v8::Function>>>,
    buffer_changes: Mutex<BufferChangeQueueState>,
    cancelled_commands: Mutex<HashSet<CommandInvocationId>>,
    #[cfg(test)]
    command_start_gate: Mutex<Option<Arc<std::sync::Barrier>>>,
    keybinding_mutations: Mutex<Vec<KeybindingMutation>>,
    keybinding_batch_pending: Mutex<Option<RequestId>>,
    deferred_responses: Mutex<Vec<HostResponse>>,
    #[cfg(test)]
    host_error: Mutex<Option<HostFailure>>,
    #[cfg(test)]
    barrier: Mutex<Option<Arc<TestGate>>>,
    #[cfg(test)]
    entered: Mutex<Option<mpsc::Sender<()>>>,
}

impl RuntimeLocalState {
    fn new(key: ExtensionKey, graph: Option<&ModuleGraph>, status: Arc<AtomicU8>) -> Self {
        Self {
            key,
            status,
            next_request: AtomicU64::new(1),
            event_sender: Mutex::new(None),
            current_root: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            active: Mutex::new(None),
            rejections: Mutex::new(Vec::new()),
            modules: Mutex::new(ModuleRegistry::new(graph)),
            callbacks: Mutex::new(HashMap::new()),
            buffer_changes: Mutex::new(BufferChangeQueueState::default()),
            cancelled_commands: Mutex::new(HashSet::new()),
            #[cfg(test)]
            command_start_gate: Mutex::new(None),
            keybinding_mutations: Mutex::new(Vec::new()),
            keybinding_batch_pending: Mutex::new(None),
            deferred_responses: Mutex::new(Vec::new()),
            #[cfg(test)]
            host_error: Mutex::new(None),
            #[cfg(test)]
            barrier: Mutex::new(None),
            #[cfg(test)]
            entered: Mutex::new(None),
        }
    }

    fn callback<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        callback: RuntimeCallback,
    ) -> Option<v8::Local<'s, v8::Function>> {
        self.callbacks
            .lock()
            .unwrap()
            .get(&callback)
            .map(|function| v8::Local::new(scope, function))
    }

    fn buffer_change_enqueued(&self) {
        let mut state = self.buffer_changes.lock().unwrap();
        state.queued += 1;
        state.metrics.max_depth = state.metrics.max_depth.max(state.queued);
    }

    fn buffer_change_discarded(&self) {
        let mut state = self.buffer_changes.lock().unwrap();
        state.queued = state.queued.saturating_sub(1);
    }

    fn buffer_change_started(&self, enqueued_at: Instant) {
        let mut state = self.buffer_changes.lock().unwrap();
        state.queued = state.queued.saturating_sub(1);
        state.metrics.max_enqueue_to_start_lag = state
            .metrics
            .max_enqueue_to_start_lag
            .max(enqueued_at.elapsed());
    }

    #[cfg(test)]
    fn buffer_change_queue_metrics(&self) -> BufferChangeQueueMetrics {
        self.buffer_changes.lock().unwrap().metrics
    }
}

struct PendingRequest {
    root: RootId,
    invocation: Option<CommandInvocationId>,
    operation: &'static str,
    resolver: Option<v8::Global<v8::PromiseResolver>>,
    response_queued: bool,
    provider_mutation: Option<ProviderIdentity>,
}

struct ActiveExecution {
    root: RootId,
    promise: v8::Global<v8::Promise>,
    source: Arc<str>,
    completion: ActiveCompletion,
}

enum ActiveCompletion {
    Report(ResultSender<ExecutionReport>),
    Command(ResultSender<CommandOutcome>),
    Tree {
        request: TreeChildrenRequest,
        result: ResultSender<TreeChildrenResponse>,
        completed: Arc<AtomicBool>,
    },
    Completion {
        request: CompletionRequest,
        result: ResultSender<CompletionResponse>,
        completed: Arc<AtomicBool>,
    },
    Detached(&'static str),
}

/// Observed buffer-change callback pressure for one extension lifetime.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct BufferChangeQueueMetrics {
    pub(crate) max_depth: usize,
    pub(crate) max_enqueue_to_start_lag: Duration,
}

#[derive(Default)]
struct BufferChangeQueueState {
    metrics: BufferChangeQueueMetrics,
    queued: usize,
}

enum RootEvaluation {
    #[cfg(test)]
    Script {
        source: Arc<str>,
    },
    Module {
        source: Option<Arc<str>>,
    },
}

enum ProviderCall {
    Tree {
        request: TreeChildrenRequest,
        result: ResultSender<TreeChildrenResponse>,
        cancelled: Arc<AtomicBool>,
        completed: Arc<AtomicBool>,
    },
    Completion {
        request: CompletionRequest,
        result: ResultSender<CompletionResponse>,
        cancelled: Arc<AtomicBool>,
        completed: Arc<AtomicBool>,
    },
}

impl ProviderCall {
    fn cancelled(&self) -> bool {
        match self {
            Self::Tree { cancelled, .. } | Self::Completion { cancelled, .. } => {
                cancelled.load(Ordering::Acquire)
            }
        }
    }

    fn complete_with_error(self, error: RuntimeError) {
        match self {
            Self::Tree {
                result, completed, ..
            } => {
                completed.store(true, Ordering::Release);
                let _ = result.send(Err(error));
            }
            Self::Completion {
                result, completed, ..
            } => {
                completed.store(true, Ordering::Release);
                let _ = result.send(Err(error));
            }
        }
    }

    fn into_active_completion(self) -> ActiveCompletion {
        match self {
            Self::Tree {
                request,
                result,
                completed,
                ..
            } => ActiveCompletion::Tree {
                request,
                result,
                completed,
            },
            Self::Completion {
                request,
                result,
                completed,
                ..
            } => ActiveCompletion::Completion {
                request,
                result,
                completed,
            },
        }
    }
}

struct ModuleRegistry {
    sources: HashMap<String, Arc<str>>,
    modules: Vec<CompiledModule>,
    resolutions: Vec<ModuleResolution>,
    package_root: Option<Arc<str>>,
    entries: HashMap<String, Arc<str>>,
}

impl ModuleRegistry {
    fn new(graph: Option<&ModuleGraph>) -> Self {
        let mut sources = HashMap::from([
            (
                PRIVATE_BOOTSTRAP_SPECIFIER.to_owned(),
                Arc::from(PRIVATE_BOOTSTRAP_SOURCE),
            ),
            (
                PUBLIC_FACADE_SPECIFIER.to_owned(),
                Arc::from(PUBLIC_FACADE_SOURCE),
            ),
        ]);
        if let Some(graph) = graph {
            sources.extend(
                graph
                    .sources()
                    .iter()
                    .map(|(name, source)| (name.clone(), source.clone())),
            );
        }
        Self {
            sources,
            modules: Vec::new(),
            resolutions: Vec::new(),
            package_root: graph.map(|graph| Arc::from(graph.root())),
            entries: graph
                .map(|graph| {
                    graph
                        .entries()
                        .iter()
                        .map(|(name, url)| (name.clone(), url.clone()))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

struct CompiledModule {
    specifier: String,
    module: v8::Global<v8::Module>,
}

struct ModuleResolution {
    referrer: v8::Global<v8::Module>,
    request: String,
    resolved: String,
}

struct ModuleCompileFailure {
    kind: RuntimeErrorKind,
    location: Option<(Arc<str>, u32, u32)>,
}

impl From<RuntimeErrorKind> for ModuleCompileFailure {
    fn from(kind: RuntimeErrorKind) -> Self {
        Self {
            kind,
            location: None,
        }
    }
}

impl ModuleCompileFailure {
    fn at_import(kind: RuntimeErrorKind, source: &str, location: v8::Location) -> Self {
        let line = u32::try_from(location.get_line_number())
            .ok()
            .and_then(|value| value.checked_add(1));
        let column = u32::try_from(location.get_column_number())
            .ok()
            .and_then(|value| value.checked_add(1));
        Self {
            kind,
            location: line
                .zip(column)
                .map(|(line, column)| (source.into(), line, column)),
        }
    }

    fn report(
        self,
        try_catch: &mut v8::PinnedRef<'_, v8::TryCatch<v8::HandleScope>>,
        fallback_source: &str,
    ) -> RuntimeError {
        let mut error = runtime_error_from_try_catch(try_catch, self.kind, fallback_source);
        if let Some((source, line, column)) = self.location {
            error.source = Some(source);
            error.line = Some(line);
            error.column = Some(column);
        }
        error
    }
}

#[cfg(test)]
#[derive(Default)]
struct TestGate {
    entered: Mutex<usize>,
    ready: std::sync::Condvar,
    timed_out: AtomicBool,
}

#[cfg(test)]
impl TestGate {
    fn wait(&self) {
        let mut entered = self
            .entered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *entered += 1;
        self.ready.notify_all();
        let (_entered, timeout) = self
            .ready
            .wait_timeout_while(entered, std::time::Duration::from_secs(2), |entered| {
                *entered < 2
            })
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if timeout.timed_out() {
            self.timed_out.store(true, Ordering::Release);
        }
    }
}

struct RejectionReport {
    promise: v8::Global<v8::Promise>,
    message: String,
    source: Option<String>,
    line: Option<u32>,
    column: Option<u32>,
    stack: Option<String>,
}

struct HeapLimitState {
    status: Arc<AtomicU8>,
    isolate: v8::IsolateHandle,
}

struct IsolateData {
    isolate: v8::SharedIsolate,
    context: v8::Global<v8::Context>,
}

mod capsule;
mod execution;
mod marshal;
mod modules;
mod pool;
#[cfg(test)]
mod tests;

pub(crate) use capsule::*;
pub(crate) use execution::*;
use marshal::*;
pub(crate) use modules::validate_module_graph_entry;
use modules::*;
pub(crate) use pool::*;
