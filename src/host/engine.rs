//! V8 process initialization and engine-owned isolate state.

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
        CompletionResultItem, DecorationToken, EditorContribution, ExtensionId, GutterToken,
        HostRequest, HostResponse, KeybindingMutation, ProviderIdentity, RequestId, SnapshotText,
        TextEdit, TextSnapshot, TreeChildrenRequest, TreeChildrenResponse, TreeCollapsibleState,
        TreeIcon, TreeItem, TreeProviderError, WorkbenchSubscriptionId,
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
const PRIVATE_BOOTSTRAP_SOURCE: &str = include_str!("js/bootstrap.js");
const PUBLIC_FACADE_SOURCE: &str = include_str!("js/knot.js");

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
    Script { source: Arc<str> },
    Module { source: Option<Arc<str>> },
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

/// One persistent, movable V8 isolate and context for an extension lifetime.
///
/// All state attached to the shared isolate is `Send`; this is the safety
/// condition required by `v8::Isolate::try_into_shared`.
pub(crate) struct RuntimeCapsule {
    key: ExtensionKey,
    data: Mutex<Option<IsolateData>>,
    local_state: Arc<RuntimeLocalState>,
    heap_state: Box<HeapLimitState>,
    status: Arc<AtomicU8>,
}

impl RuntimeCapsule {
    pub(crate) fn new(key: ExtensionKey, config: IsolateConfig) -> Result<Self, RuntimeError> {
        Self::new_with_graph(key, config, None)
    }

    fn new_with_graph(
        key: ExtensionKey,
        config: IsolateConfig,
        graph: Option<&ModuleGraph>,
    ) -> Result<Self, RuntimeError> {
        initialize();
        let status = Arc::new(AtomicU8::new(ACTIVE));
        let local_state = Arc::new(RuntimeLocalState::new(key, graph, Arc::clone(&status)));
        let mut isolate =
            v8::Isolate::new(v8::CreateParams::default().heap_limits(0, config.heap_limit_bytes));
        isolate.set_microtasks_policy(v8::MicrotasksPolicy::Explicit);
        isolate.set_promise_reject_callback(promise_reject_callback);
        isolate.set_host_import_module_dynamically_callback(reject_dynamic_import);
        isolate.set_slot(Arc::clone(&local_state));
        let handle = isolate.thread_safe_handle();
        let mut heap_state = Box::new(HeapLimitState {
            status: Arc::clone(&status),
            isolate: handle,
        });
        isolate.add_near_heap_limit_callback(
            near_heap_limit_callback,
            ptr::from_mut(heap_state.as_mut()).cast::<c_void>(),
        );
        let context = {
            let scope = pin!(v8::HandleScope::new(&mut isolate));
            let mut scope = scope.init();
            let context = v8::Context::new(&scope, Default::default());
            let context_handle = v8::Global::new(&scope, context);
            let scope = &mut v8::ContextScope::new(&mut scope, context);
            initialize_extension_context(scope, &local_state).map(|()| context_handle)
        };
        let context = match context {
            Ok(context) => context,
            Err(error) => {
                clear_module_registry(&local_state);
                isolate.remove_near_heap_limit_callback(near_heap_limit_callback, 0);
                let _ = isolate.remove_slot::<Arc<RuntimeLocalState>>();
                drop(isolate);
                return Err(error);
            }
        };
        // SAFETY: the context, callback data, and isolate slot above contain
        // only owned Send state. No scoped handles or references survive.
        let isolate = match unsafe { isolate.try_into_shared() } {
            Ok(isolate) => isolate,
            Err(error) => {
                let message = format!("V8 isolate could not become shared: {error}");
                let mut isolate = error.into_isolate();
                clear_module_registry(&local_state);
                isolate.remove_near_heap_limit_callback(near_heap_limit_callback, 0);
                let _ = isolate.remove_slot::<Arc<RuntimeLocalState>>();
                drop(context);
                drop(isolate);
                return Err(RuntimeError::fatal(RuntimeErrorKind::Disposed, message));
            }
        };

        Ok(Self {
            key,
            data: Mutex::new(Some(IsolateData { isolate, context })),
            local_state,
            heap_state,
            status,
        })
    }

    pub(crate) fn key(&self) -> ExtensionKey {
        self.key
    }

    pub(crate) fn termination_handle(&self) -> TerminationHandle {
        TerminationHandle {
            isolate: self.heap_state.isolate.clone(),
            status: Arc::clone(&self.status),
            wake: None,
        }
    }

    pub(crate) fn execute(
        &self,
        source_name: &str,
        source: &str,
    ) -> Result<ExecutionReport, RuntimeError> {
        if self.status.load(Ordering::Acquire) != ACTIVE {
            return Err(self.status_error());
        }
        let data = self.data.lock().unwrap();
        let data = data.as_ref().ok_or_else(RuntimeError::disposed)?;
        self.local_state.rejections.lock().unwrap().clear();
        let mut locker = data.isolate.lock();
        let scope = pin!(v8::HandleScope::new(&mut *locker));
        let mut scope = scope.init();
        let context = v8::Local::new(&scope, &data.context);
        let scope = &mut v8::ContextScope::new(&mut scope, context);
        v8::tc_scope!(let try_catch, scope);

        let name = v8::String::new(try_catch, source_name).ok_or_else(RuntimeError::disposed)?;
        let source_text = v8::String::new(try_catch, source).ok_or_else(RuntimeError::disposed)?;
        let origin = v8::ScriptOrigin::new(
            try_catch,
            name.into(),
            0,
            0,
            false,
            0,
            None,
            false,
            false,
            false,
            None,
        );
        let Some(script) = v8::Script::compile(try_catch, source_text, Some(&origin)) else {
            let error = if try_catch.has_terminated()
                || self.status.load(Ordering::Acquire) != ACTIVE
            {
                self.status_error()
            } else {
                runtime_error_from_try_catch(try_catch, RuntimeErrorKind::Compilation, source_name)
            };
            try_catch.reset();
            try_catch.perform_microtask_checkpoint();
            if try_catch.has_terminated() || self.status.load(Ordering::Acquire) != ACTIVE {
                return Err(self.status_error());
            }
            return Err(error);
        };
        let Some(value) = script.run(try_catch) else {
            let error = self.execution_error(try_catch, source_name);
            if !error.is_fatal() {
                try_catch.reset();
                try_catch.perform_microtask_checkpoint();
                if try_catch.has_terminated() || self.status.load(Ordering::Acquire) != ACTIVE {
                    return Err(self.status_error());
                }
            }
            return Err(error);
        };
        let value = value.to_rust_string_lossy(try_catch);
        try_catch.perform_microtask_checkpoint();
        if try_catch.has_terminated() || self.status.load(Ordering::Acquire) != ACTIVE {
            return Err(self.status_error());
        }
        let mut rejections = {
            let mut queued = self.local_state.rejections.lock().unwrap();
            std::mem::take(&mut *queued)
        };
        if !rejections.is_empty() {
            let rejection = rejections.remove(0);
            let mut message = rejection.message;
            for additional in &rejections {
                message.push('\n');
                message.push_str(&additional.message);
            }
            return Err(RuntimeError {
                kind: RuntimeErrorKind::Rejection,
                message: message.into(),
                source: rejection
                    .source
                    .or_else(|| Some(source_name.to_owned()))
                    .map(Into::into),
                line: rejection.line,
                column: rejection.column,
                stack: rejection.stack.map(Into::into),
            });
        }

        Ok(ExecutionReport {
            value: value.into(),
            worker: std::thread::current().id(),
        })
    }

    pub(crate) fn execute_fixture_module(
        &self,
        specifier: &str,
        source: &str,
    ) -> Result<ExecutionReport, RuntimeError> {
        let specifier = validate_root_module_specifier(specifier)?;
        if self.status.load(Ordering::Acquire) != ACTIVE {
            return Err(self.status_error());
        }
        let data = self.data.lock().unwrap();
        let data = data.as_ref().ok_or_else(RuntimeError::disposed)?;
        self.local_state.rejections.lock().unwrap().clear();
        let mut locker = data.isolate.lock();
        let scope = pin!(v8::HandleScope::new(&mut *locker));
        let mut scope = scope.init();
        let context = v8::Local::new(&scope, &data.context);
        let scope = &mut v8::ContextScope::new(&mut scope, context);
        let result = evaluate_fixture_module(
            scope,
            &self.local_state,
            &specifier,
            Some(Arc::from(source)),
            false,
        );
        if self.status.load(Ordering::Acquire) != ACTIVE {
            return Err(self.status_error());
        }
        result.map(|value| ExecutionReport {
            value: value.into(),
            worker: std::thread::current().id(),
        })
    }

    fn start_script_turn(
        &self,
        root: RootId,
        source_name: Arc<str>,
        source: Arc<str>,
        result: ResultSender<ExecutionReport>,
    ) -> TurnOutcome {
        self.run_root_turn(root, source_name, result, RootEvaluation::Script { source })
    }

    fn start_module_turn(
        &self,
        root: RootId,
        specifier: Arc<str>,
        source: Arc<str>,
        result: ResultSender<ExecutionReport>,
    ) -> TurnOutcome {
        let canonical = match validate_root_module_specifier(&specifier) {
            Ok(specifier) => Arc::<str>::from(specifier),
            Err(error) => {
                let _ = result.send(Err(error));
                return TurnOutcome::Completed;
            }
        };
        self.run_root_turn(
            root,
            canonical,
            result,
            RootEvaluation::Module {
                source: Some(source),
            },
        )
    }

    fn start_package_turn(
        &self,
        root: RootId,
        entry: Arc<str>,
        result: ResultSender<ExecutionReport>,
    ) -> TurnOutcome {
        self.run_root_turn(root, entry, result, RootEvaluation::Module { source: None })
    }

    fn start_notification_turn(
        &self,
        root: RootId,
        subscription: u64,
        notification: ListenerNotification,
        enqueued_at: Instant,
    ) -> TurnOutcome {
        let (callback, source, label) = match &notification {
            ListenerNotification::Buffer(_) => {
                self.local_state.buffer_change_started(enqueued_at);
                (
                    RuntimeCallback::BufferChange,
                    "knot:buffer-change",
                    "buffer-change listener",
                )
            }
            ListenerNotification::Application(_) => (
                RuntimeCallback::ApplicationChange,
                "knot:application-change",
                "application-change listener",
            ),
        };
        if subscription > MAX_JS_INTEGER
            || matches!(&notification, ListenerNotification::Buffer(change) if change.buffer.value() > MAX_JS_INTEGER)
        {
            eprintln!("[knot] listener identity cannot be represented exactly");
            return TurnOutcome::Completed;
        }
        if self.status.load(Ordering::Acquire) != ACTIVE {
            return fatal_or_completed(&self.status_error());
        }
        let data = self.data.lock().unwrap();
        let Some(data) = data.as_ref() else {
            return TurnOutcome::Fatal(Failure::new("extension isolate is disposed"));
        };
        self.local_state.rejections.lock().unwrap().clear();
        *self.local_state.current_root.lock().unwrap() = Some(root);
        let mut locker = data.isolate.lock();
        let scope = pin!(v8::HandleScope::new(&mut *locker));
        let mut scope = scope.init();
        let context = v8::Local::new(&scope, &data.context);
        let scope = &mut v8::ContextScope::new(&mut scope, context);
        v8::tc_scope!(let try_catch, scope);
        let evaluation = (|| {
            let function = self.local_state.callback(try_catch, callback)?;
            let subscription = v8::Number::new(try_catch, subscription as f64);
            let change = match notification {
                ListenerNotification::Buffer(change) => buffer_change_to_v8(try_catch, change),
                ListenerNotification::Application(event) => {
                    application_event_to_v8(try_catch, &event)
                }
            }
            .ok()?;
            function.call(
                try_catch,
                v8::undefined(try_catch).into(),
                &[subscription.into(), change],
            )
        })();
        let Some(value) = evaluation else {
            let error = self.execution_error(try_catch, source);
            *self.local_state.current_root.lock().unwrap() = None;
            if !error.is_fatal() {
                eprintln!("[knot] {label} failed: {error}");
                try_catch.reset();
                self.reject_pending_for_root(try_catch, root);
            }
            return fatal_or_completed(&error);
        };
        let promise = if let Ok(promise) = v8::Local::<v8::Promise>::try_from(value) {
            promise
        } else {
            let resolver = v8::PromiseResolver::new(try_catch).expect("V8 resolver allocation");
            resolver.resolve(try_catch, value);
            resolver.get_promise(try_catch)
        };
        promise.mark_as_handled();
        *self.local_state.active.lock().unwrap() = Some(ActiveExecution {
            root,
            promise: v8::Global::new(try_catch, promise),
            source: source.into(),
            completion: ActiveCompletion::Detached(label),
        });
        try_catch.perform_microtask_checkpoint();
        *self.local_state.current_root.lock().unwrap() = None;
        self.inspect_active(try_catch, root)
    }

    fn start_command_turn(
        &self,
        root: RootId,
        invocation: CommandInvocation,
        active_buffer: Option<BufferHandle>,
        result: ResultSender<CommandOutcome>,
    ) -> TurnOutcome {
        if invocation.id.value() > MAX_JS_INTEGER
            || invocation.registration.value() > MAX_JS_INTEGER
            || active_buffer.is_some_and(|buffer| buffer.value() > MAX_JS_INTEGER)
            || invocation
                .capture
                .as_ref()
                .is_some_and(|capture| capture.authority.value() > MAX_JS_INTEGER)
        {
            let _ = result.send(Ok(CommandOutcome::HandlerFailure {
                message: "native command identity cannot be represented exactly".into(),
            }));
            return TurnOutcome::Completed;
        }
        if self
            .local_state
            .cancelled_commands
            .lock()
            .unwrap()
            .remove(&invocation.id)
        {
            let _ = result.send(Ok(CommandOutcome::Cancelled));
            return TurnOutcome::Completed;
        }
        if self.status.load(Ordering::Acquire) != ACTIVE {
            let error = self.status_error();
            let _ = result.send(Err(error.clone()));
            return fatal_or_completed(&error);
        }
        let data = self.data.lock().unwrap();
        let Some(data) = data.as_ref() else {
            let error = RuntimeError::disposed();
            let _ = result.send(Err(error.clone()));
            return TurnOutcome::Fatal(Failure::new(error.to_string()));
        };
        self.local_state.rejections.lock().unwrap().clear();
        *self.local_state.current_root.lock().unwrap() = Some(root);
        let mut locker = data.isolate.lock();
        let scope = pin!(v8::HandleScope::new(&mut *locker));
        let mut scope = scope.init();
        let context = v8::Local::new(&scope, &data.context);
        let scope = &mut v8::ContextScope::new(&mut scope, context);
        v8::tc_scope!(let try_catch, scope);
        let evaluated = (|| {
            let function = self
                .local_state
                .callback(try_catch, RuntimeCallback::Command)?;
            let active_buffer = active_buffer.map_or_else(
                || v8::null(try_catch).into(),
                |buffer| v8::Number::new(try_catch, buffer.value() as f64).into(),
            );
            let capture = match &invocation.capture {
                Some(capture) => {
                    let json = serde_json::to_string(capture).ok()?;
                    let json = v8::String::new(try_catch, &json)?;
                    v8::json::parse(try_catch, json)?
                }
                None => v8::null(try_catch).into(),
            };
            let arguments = command_argument_to_v8(try_catch, &invocation.arguments).ok()?;
            function.call(
                try_catch,
                v8::undefined(try_catch).into(),
                &[
                    v8::Number::new(try_catch, invocation.id.value() as f64).into(),
                    v8::Number::new(try_catch, invocation.registration.value() as f64).into(),
                    active_buffer,
                    arguments,
                    capture,
                ],
            )
        })();
        let Some(value) = evaluated else {
            let error = self.execution_error(try_catch, "knot:command");
            *self.local_state.current_root.lock().unwrap() = None;
            let _ = result.send(Err(error.clone()));
            if !error.is_fatal() {
                try_catch.reset();
                self.reject_pending_for_root(try_catch, root);
            }
            return fatal_or_completed(&error);
        };
        let promise = if let Ok(promise) = v8::Local::<v8::Promise>::try_from(value) {
            promise
        } else {
            let resolver = v8::PromiseResolver::new(try_catch).expect("V8 resolver allocation");
            resolver.resolve(try_catch, value);
            resolver.get_promise(try_catch)
        };
        promise.mark_as_handled();
        #[cfg(test)]
        if let Some(gate) = self.local_state.command_start_gate.lock().unwrap().take() {
            gate.wait();
            gate.wait();
        }
        let cancelled = {
            // Cancellation either sees the published command or is consumed here.
            let mut cancelled = self.local_state.cancelled_commands.lock().unwrap();
            *self.local_state.active.lock().unwrap() = Some(ActiveExecution {
                root,
                promise: v8::Global::new(try_catch, promise),
                source: "knot:command".into(),
                completion: ActiveCompletion::Command(result),
            });
            cancelled.remove(&invocation.id)
        };
        if cancelled {
            self.cancel_command_in_scope(try_catch, root, invocation.id);
        }
        try_catch.perform_microtask_checkpoint();
        *self.local_state.current_root.lock().unwrap() = None;
        self.inspect_active(try_catch, root)
    }

    fn start_provider_turn(&self, root: RootId, call: ProviderCall) -> TurnOutcome {
        let exact = match &call {
            ProviderCall::Tree { request, .. } => {
                request.registration.value() <= MAX_JS_INTEGER
                    && request.generation <= MAX_JS_INTEGER
            }
            ProviderCall::Completion { request, .. } => {
                request.registration.value() <= MAX_JS_INTEGER
                    && request.buffer.value() <= MAX_JS_INTEGER
                    && request.revision <= MAX_JS_INTEGER
                    && request.generation <= MAX_JS_INTEGER
                    && request.cursor_byte_offset as u64 <= MAX_JS_INTEGER
            }
        };
        if !exact {
            call.complete_with_error(RuntimeError::fatal(
                RuntimeErrorKind::Engine,
                "native provider coordinate cannot be represented exactly",
            ));
            return TurnOutcome::Completed;
        }
        if call.cancelled() {
            call.complete_with_error(RuntimeError::fatal(
                RuntimeErrorKind::Cancelled,
                "semantic provider request was dropped",
            ));
            return TurnOutcome::Completed;
        }
        if self.status.load(Ordering::Acquire) != ACTIVE {
            let error = self.status_error();
            call.complete_with_error(error.clone());
            return fatal_or_completed(&error);
        }
        let data = self.data.lock().unwrap();
        let Some(data) = data.as_ref() else {
            let error = RuntimeError::disposed();
            call.complete_with_error(error.clone());
            return TurnOutcome::Fatal(Failure::new(error.to_string()));
        };
        self.local_state.rejections.lock().unwrap().clear();
        *self.local_state.current_root.lock().unwrap() = Some(root);
        let mut locker = data.isolate.lock();
        let scope = pin!(v8::HandleScope::new(&mut *locker));
        let mut scope = scope.init();
        let context = v8::Local::new(&scope, &data.context);
        let scope = &mut v8::ContextScope::new(&mut scope, context);
        v8::tc_scope!(let try_catch, scope);
        let (source, evaluated) = match &call {
            ProviderCall::Tree { request, .. } => {
                let source = "knot:tree-provider";
                let value = (|| {
                    let function = self
                        .local_state
                        .callback(try_catch, RuntimeCallback::Tree)?;
                    let parent = request.parent_id.as_ref().map_or_else(
                        || Some(v8::null(try_catch).into()),
                        |parent| v8::String::new(try_catch, parent).map(Into::into),
                    )?;
                    let window = v8::String::new(try_catch, &request.window)?;
                    let instance = v8::String::new(try_catch, &request.view.value().to_string())?;
                    function.call(
                        try_catch,
                        v8::undefined(try_catch).into(),
                        &[
                            v8::Number::new(try_catch, request.registration.value() as f64).into(),
                            window.into(),
                            instance.into(),
                            parent,
                            v8::Number::new(try_catch, request.generation as f64).into(),
                        ],
                    )
                })();
                (source, value)
            }
            ProviderCall::Completion { request, .. } => {
                let source = "knot:completion-provider";
                let value = (|| {
                    let function = self
                        .local_state
                        .callback(try_catch, RuntimeCallback::Completion)?;
                    let prefix = v8::String::new(try_catch, &request.prefix)?;
                    function.call(
                        try_catch,
                        v8::undefined(try_catch).into(),
                        &[
                            v8::Number::new(try_catch, request.registration.value() as f64).into(),
                            v8::Number::new(try_catch, request.buffer.value() as f64).into(),
                            v8::Number::new(try_catch, request.revision as f64).into(),
                            v8::Number::new(try_catch, request.cursor_byte_offset as f64).into(),
                            prefix.into(),
                            v8::Number::new(try_catch, request.generation as f64).into(),
                        ],
                    )
                })();
                (source, value)
            }
        };
        let Some(value) = evaluated else {
            let error = self.execution_error(try_catch, source);
            *self.local_state.current_root.lock().unwrap() = None;
            call.complete_with_error(error.clone());
            if !error.is_fatal() {
                try_catch.reset();
                self.reject_pending_for_root(try_catch, root);
            }
            return fatal_or_completed(&error);
        };
        let promise = if let Ok(promise) = v8::Local::<v8::Promise>::try_from(value) {
            promise
        } else {
            let resolver = v8::PromiseResolver::new(try_catch).expect("V8 resolver allocation");
            resolver.resolve(try_catch, value);
            resolver.get_promise(try_catch)
        };
        promise.mark_as_handled();
        *self.local_state.active.lock().unwrap() = Some(ActiveExecution {
            root,
            promise: v8::Global::new(try_catch, promise),
            source: source.into(),
            completion: call.into_active_completion(),
        });
        try_catch.perform_microtask_checkpoint();
        *self.local_state.current_root.lock().unwrap() = None;
        self.inspect_active(try_catch, root)
    }

    fn cancel_provider_turn(&self, root: RootId) -> TurnOutcome {
        let data = self.data.lock().unwrap();
        let Some(data) = data.as_ref() else {
            return TurnOutcome::Fatal(Failure::new("extension isolate is disposed"));
        };
        let mut locker = data.isolate.lock();
        let scope = pin!(v8::HandleScope::new(&mut *locker));
        let mut scope = scope.init();
        let context = v8::Local::new(&scope, &data.context);
        let scope = &mut v8::ContextScope::new(&mut scope, context);
        self.reject_pending_for_root(scope, root);
        let active = self.local_state.active.lock().unwrap().take();
        let Some(active) = active else {
            return TurnOutcome::Completed;
        };
        if active.root != root {
            *self.local_state.active.lock().unwrap() = Some(active);
            return TurnOutcome::Fatal(Failure::new("extension root execution identity changed"));
        }
        let error = RuntimeError::fatal(
            RuntimeErrorKind::Cancelled,
            "semantic provider request was dropped",
        );
        match active.completion {
            ActiveCompletion::Tree {
                result, completed, ..
            } => {
                completed.store(true, Ordering::Release);
                let _ = result.send(Err(error));
            }
            ActiveCompletion::Completion {
                result, completed, ..
            } => {
                completed.store(true, Ordering::Release);
                let _ = result.send(Err(error));
            }
            completion => {
                *self.local_state.active.lock().unwrap() = Some(ActiveExecution {
                    completion,
                    ..active
                });
                return TurnOutcome::Fatal(Failure::new(
                    "provider cancellation addressed a non-provider root",
                ));
            }
        }
        TurnOutcome::Completed
    }

    fn cancel_command_turn(&self, root: RootId, invocation: CommandInvocationId) -> TurnOutcome {
        self.local_state
            .cancelled_commands
            .lock()
            .unwrap()
            .remove(&invocation);
        let data = self.data.lock().unwrap();
        let Some(data) = data.as_ref() else {
            return TurnOutcome::Fatal(Failure::new("extension isolate is disposed"));
        };
        *self.local_state.current_root.lock().unwrap() = Some(root);
        let mut locker = data.isolate.lock();
        let scope = pin!(v8::HandleScope::new(&mut *locker));
        let mut scope = scope.init();
        let context = v8::Local::new(&scope, &data.context);
        let scope = &mut v8::ContextScope::new(&mut scope, context);
        self.cancel_command_in_scope(scope, root, invocation);
        *self.local_state.current_root.lock().unwrap() = None;
        self.inspect_active(scope, root)
    }

    fn cancel_command_in_scope(
        &self,
        scope: &mut v8::PinScope<'_, '_>,
        root: RootId,
        invocation: CommandInvocationId,
    ) {
        if let Some(function) = self
            .local_state
            .callback(scope, RuntimeCallback::CancelCommand)
        {
            function.call(
                scope,
                v8::undefined(scope).into(),
                &[v8::Number::new(scope, invocation.value() as f64).into()],
            );
        }
        self.reject_pending_for_command(scope, root);
        scope.perform_microtask_checkpoint();
    }

    fn run_root_turn(
        &self,
        root: RootId,
        source: Arc<str>,
        result: ResultSender<ExecutionReport>,
        evaluation: RootEvaluation,
    ) -> TurnOutcome {
        if self.status.load(Ordering::Acquire) != ACTIVE {
            let error = self.status_error();
            let _ = result.send(Err(error.clone()));
            return fatal_or_completed(&error);
        }
        let data = self.data.lock().unwrap();
        let Some(data) = data.as_ref() else {
            let error = RuntimeError::disposed();
            let _ = result.send(Err(error.clone()));
            return TurnOutcome::Fatal(Failure::new(error.to_string()));
        };
        self.local_state.rejections.lock().unwrap().clear();
        *self.local_state.current_root.lock().unwrap() = Some(root);
        let mut locker = data.isolate.lock();
        let scope = pin!(v8::HandleScope::new(&mut *locker));
        let mut scope = scope.init();
        let context = v8::Local::new(&scope, &data.context);
        let scope = &mut v8::ContextScope::new(&mut scope, context);
        v8::tc_scope!(let try_catch, scope);
        let evaluated = match evaluation {
            RootEvaluation::Script {
                source: script_source,
            } => {
                let name = v8::String::new(try_catch, &source).ok_or_else(RuntimeError::disposed);
                let source_text =
                    v8::String::new(try_catch, &script_source).ok_or_else(RuntimeError::disposed);
                name.and_then(|name| {
                    source_text.and_then(|source_text| {
                        let origin = v8::ScriptOrigin::new(
                            try_catch,
                            name.into(),
                            0,
                            0,
                            false,
                            0,
                            None,
                            false,
                            false,
                            false,
                            None,
                        );
                        let script = v8::Script::compile(try_catch, source_text, Some(&origin))
                            .ok_or_else(|| {
                                runtime_error_from_try_catch(
                                    try_catch,
                                    RuntimeErrorKind::Compilation,
                                    &source,
                                )
                            })?;
                        script
                            .run(try_catch)
                            .ok_or_else(|| self.execution_error(try_catch, &source))
                    })
                })
            }
            RootEvaluation::Module {
                source: module_source,
            } => (|| {
                if let Some(module_source) = module_source {
                    let mut modules = self.local_state.modules.lock().unwrap();
                    if modules.package_root.is_some() {
                        return Err(module_resolution_error(
                            &source,
                            "package module graph cannot change after load",
                        ));
                    }
                    if let Some(existing) = modules.sources.get(source.as_ref()) {
                        if existing.as_ref() != module_source.as_ref() {
                            return Err(module_resolution_error(
                                &source,
                                "fixture module source cannot change after registration",
                            ));
                        }
                    } else {
                        modules.sources.insert(source.to_string(), module_source);
                    }
                }
                let (module_count, resolution_count) = {
                    let modules = self.local_state.modules.lock().unwrap();
                    (modules.modules.len(), modules.resolutions.len())
                };
                let compiled = {
                    let mut modules = self.local_state.modules.lock().unwrap();
                    let blocked = blocked_post_init_import(&modules, &source);
                    compile_module_graph(try_catch, &mut modules, &source, blocked.as_deref())
                };
                let compiled = match compiled {
                    Ok(compiled) => compiled,
                    Err(failure) => {
                        let error = failure.report(try_catch, &source);
                        rollback_module_graph(&self.local_state, module_count, resolution_count);
                        return Err(error);
                    }
                };
                let module = v8::Local::new(try_catch, &compiled);
                if module.instantiate_module(try_catch, resolve_module_callback) != Some(true) {
                    let error = runtime_error_from_try_catch(
                        try_catch,
                        RuntimeErrorKind::ModuleResolution,
                        &source,
                    );
                    rollback_module_graph(&self.local_state, module_count, resolution_count);
                    return Err(error);
                }
                module.evaluate(try_catch).ok_or_else(|| {
                    runtime_error_from_try_catch(try_catch, RuntimeErrorKind::Exception, &source)
                })
            })(),
        };
        let value = match evaluated {
            Ok(value) => value,
            Err(error) => {
                *self.local_state.current_root.lock().unwrap() = None;
                let _ = result.send(Err(error.clone()));
                self.reject_pending_for_root(try_catch, root);
                return fatal_or_completed(&error);
            }
        };
        let promise = if let Ok(promise) = v8::Local::<v8::Promise>::try_from(value) {
            promise
        } else {
            let resolver = v8::PromiseResolver::new(try_catch).expect("V8 resolver allocation");
            resolver.resolve(try_catch, value);
            resolver.get_promise(try_catch)
        };
        promise.mark_as_handled();
        *self.local_state.active.lock().unwrap() = Some(ActiveExecution {
            root,
            promise: v8::Global::new(try_catch, promise),
            source,
            completion: ActiveCompletion::Report(result),
        });
        try_catch.perform_microtask_checkpoint();
        *self.local_state.current_root.lock().unwrap() = None;
        self.inspect_active(try_catch, root)
    }

    fn resume_response_turn(&self, root: RootId, response: HostResponse) -> TurnOutcome {
        let data = self.data.lock().unwrap();
        let Some(data) = data.as_ref() else {
            return TurnOutcome::Fatal(Failure::new("extension isolate is disposed"));
        };
        *self.local_state.current_root.lock().unwrap() = Some(root);
        let mut locker = data.isolate.lock();
        let scope = pin!(v8::HandleScope::new(&mut *locker));
        let mut scope = scope.init();
        let context = v8::Local::new(&scope, &data.context);
        let scope = &mut v8::ContextScope::new(&mut scope, context);
        let batch_id = *self.local_state.keybinding_batch_pending.lock().unwrap();
        if batch_id.is_some() && batch_id != Some(response.id) {
            self.local_state
                .deferred_responses
                .lock()
                .unwrap()
                .push(response);
            *self.local_state.current_root.lock().unwrap() = None;
            return TurnOutcome::AwaitingHostWork;
        }
        let pending = self
            .local_state
            .pending
            .lock()
            .unwrap()
            .remove(&response.id);
        let Some(pending) = pending else {
            *self.local_state.current_root.lock().unwrap() = None;
            return TurnOutcome::Fatal(Failure::new("host response lost its pending request"));
        };
        if batch_id == Some(response.id) {
            *self.local_state.keybinding_batch_pending.lock().unwrap() = None;
            let error = match response.result {
                Ok(reply) if reply.operation == pending.operation => match reply.result {
                    Ok(Ok(super::declarations::EncodedValue::Undefined)) => None,
                    Err(error) => Some(error.message),
                    _ => Some("invalid keybinding batch response".into()),
                },
                Err(error) => Some(format!("keybinding batch was rejected: {}", error.code)),
                _ => Some("invalid keybinding batch response".into()),
            };
            if let Some(message) = error {
                let source = self
                    .local_state
                    .active
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|active| active.source.clone());
                let error = RuntimeError {
                    kind: RuntimeErrorKind::Exception,
                    message: message.into(),
                    source,
                    line: None,
                    column: None,
                    stack: None,
                };
                self.local_state.deferred_responses.lock().unwrap().clear();
                self.local_state
                    .keybinding_mutations
                    .lock()
                    .unwrap()
                    .clear();
                self.fail_active_with_command_failure(error.clone());
                return fatal_or_completed(&error);
            }
            let deferred =
                std::mem::take(&mut *self.local_state.deferred_responses.lock().unwrap());
            for response in deferred {
                let Some(pending) = self
                    .local_state
                    .pending
                    .lock()
                    .unwrap()
                    .remove(&response.id)
                else {
                    let error = RuntimeError::fatal(
                        RuntimeErrorKind::Engine,
                        "deferred host response lost its pending request",
                    );
                    self.fail_active(error.clone());
                    return fatal_or_completed(&error);
                };
                Self::settle_host_response(scope, pending, response);
            }
        } else {
            Self::settle_host_response(scope, pending, response);
        }
        scope.perform_microtask_checkpoint();
        *self.local_state.current_root.lock().unwrap() = None;
        self.inspect_active(scope, root)
    }

    fn settle_host_response(
        scope: &mut v8::PinScope<'_, '_>,
        pending: PendingRequest,
        response: HostResponse,
    ) {
        let Some(resolver) = pending.resolver else {
            return;
        };
        let resolver = v8::Local::new(scope, &resolver);
        let value = match response.result {
            Ok(response) if pending.operation == response.operation => {
                declared_response_to_v8(scope, &response)
            }
            Ok(_) => Err(boundary_exception(
                scope,
                BoundaryError::Host(TransportError::UnsupportedOperation),
            )),
            Err(error) => Err(host_failure_exception(scope, &error)),
        };
        match value {
            Ok(value) => {
                resolver.resolve(scope, value);
            }
            Err(error) => {
                resolver.reject(scope, error);
            }
        }
    }

    fn finish_termination_turn(&self, root: RootId) -> TurnOutcome {
        let data = self.data.lock().unwrap();
        let Some(data) = data.as_ref() else {
            return TurnOutcome::Fatal(Failure::new("extension isolate is disposed"));
        };
        let mut locker = data.isolate.lock();
        let scope = pin!(v8::HandleScope::new(&mut *locker));
        let mut scope = scope.init();
        let context = v8::Local::new(&scope, &data.context);
        let scope = &mut v8::ContextScope::new(&mut scope, context);
        self.inspect_active(scope, root)
    }

    fn inspect_active(&self, scope: &mut v8::PinScope<'_, '_>, root: RootId) -> TurnOutcome {
        if self.status.load(Ordering::Acquire) != ACTIVE {
            let error = self.status_error();
            self.reject_pending_for_root(scope, root);
            self.fail_active(error.clone());
            return TurnOutcome::Fatal(Failure::new(error.to_string()));
        }
        if self
            .local_state
            .keybinding_batch_pending
            .lock()
            .unwrap()
            .is_some()
        {
            return TurnOutcome::AwaitingHostWork;
        }
        match self.flush_keybinding_mutations(root) {
            Ok(true) => return TurnOutcome::AwaitingHostWork,
            Ok(false) => {}
            Err(error) => {
                self.fail_active(error.clone());
                return fatal_or_completed(&error);
            }
        }
        let mut active = self.local_state.active.lock().unwrap();
        let Some(execution) = active.as_mut() else {
            return TurnOutcome::Fatal(Failure::new("extension root execution is missing"));
        };
        if execution.root != root {
            return TurnOutcome::Fatal(Failure::new("extension root execution identity changed"));
        }
        let promise = v8::Local::new(scope, &execution.promise);
        let pending = self
            .local_state
            .pending
            .lock()
            .unwrap()
            .values()
            .any(|pending| pending.root == root);
        match promise.state() {
            v8::PromiseState::Pending => TurnOutcome::AwaitingHostWork,
            v8::PromiseState::Fulfilled if pending => TurnOutcome::AwaitingHostWork,
            v8::PromiseState::Fulfilled => {
                let value = promise.result(scope);
                let execution = active.take().expect("active execution was inspected");
                let rejection = self.take_unhandled_rejection(&execution.source);
                match execution.completion {
                    ActiveCompletion::Report(sender) => {
                        let text = value.to_rust_string_lossy(scope);
                        let result = rejection.map_or_else(
                            || {
                                Ok(ExecutionReport {
                                    value: text.into(),
                                    worker: std::thread::current().id(),
                                })
                            },
                            Err,
                        );
                        let _ = sender.send(result);
                    }
                    ActiveCompletion::Command(sender) => {
                        let result =
                            rejection.map_or_else(|| command_outcome_from_v8(scope, value), Err);
                        let _ = sender.send(result);
                    }
                    ActiveCompletion::Tree {
                        request,
                        result,
                        completed,
                    } => {
                        let response = match rejection {
                            None => tree_response_from_v8(scope, request, value),
                            Some(error) => TreeChildrenResponse {
                                registration: request.registration,
                                window: request.window.clone(),
                                view: request.view,
                                parent_id: request.parent_id.clone(),
                                generation: request.generation,
                                result: Err(TreeProviderError {
                                    message: error.to_string(),
                                }),
                            },
                        };
                        completed.store(true, Ordering::Release);
                        let _ = result.send(Ok(response));
                    }
                    ActiveCompletion::Completion {
                        request,
                        result,
                        completed,
                    } => {
                        let response = rejection.map_or_else(
                            || completion_response_from_v8(scope, request.clone(), value),
                            |error| CompletionResponse {
                                registration: request.registration,
                                revision: request.revision,
                                generation: request.generation,
                                result: Err(CompletionProviderError {
                                    message: error.to_string(),
                                }),
                            },
                        );
                        completed.store(true, Ordering::Release);
                        let _ = result.send(Ok(response));
                    }
                    ActiveCompletion::Detached(label) => {
                        if let Some(error) = rejection {
                            eprintln!("[knot] {label} failed: {error}");
                        }
                    }
                }
                TurnOutcome::Completed
            }
            v8::PromiseState::Rejected => {
                let value = promise.result(scope);
                let message = v8::Exception::create_message(scope, value);
                let error = RuntimeError {
                    kind: RuntimeErrorKind::Rejection,
                    message: message.get(scope).to_rust_string_lossy(scope).into(),
                    source: message
                        .get_script_resource_name(scope)
                        .filter(|value| !value.is_null_or_undefined())
                        .and_then(|value| value.to_string(scope))
                        .map(|value| value.to_rust_string_lossy(scope).into())
                        .or_else(|| Some(Arc::clone(&execution.source))),
                    line: message
                        .get_line_number(scope)
                        .and_then(|line| u32::try_from(line).ok()),
                    column: one_based_coordinate(message.get_start_column()),
                    stack: None,
                };
                let execution = active.take().expect("active execution was inspected");
                match execution.completion {
                    ActiveCompletion::Report(sender) => {
                        let _ = sender.send(Err(error));
                    }
                    ActiveCompletion::Command(sender) => {
                        let _ = sender.send(Ok(CommandOutcome::HandlerFailure {
                            message: error.to_string(),
                        }));
                    }
                    ActiveCompletion::Tree {
                        request,
                        result,
                        completed,
                    } => {
                        completed.store(true, Ordering::Release);
                        let _ = result.send(Ok(TreeChildrenResponse {
                            registration: request.registration,
                            window: request.window.clone(),
                            view: request.view,
                            parent_id: request.parent_id.clone(),
                            generation: request.generation,
                            result: Err(TreeProviderError {
                                message: error.to_string(),
                            }),
                        }));
                    }
                    ActiveCompletion::Completion {
                        request,
                        result,
                        completed,
                    } => {
                        completed.store(true, Ordering::Release);
                        let _ = result.send(Ok(CompletionResponse {
                            registration: request.registration,
                            revision: request.revision,
                            generation: request.generation,
                            result: Err(CompletionProviderError {
                                message: error.to_string(),
                            }),
                        }));
                    }
                    ActiveCompletion::Detached(label) => {
                        eprintln!("[knot] {label} failed: {error}");
                    }
                }
                drop(active);
                self.reject_pending_for_root(scope, root);
                TurnOutcome::Completed
            }
        }
    }

    fn flush_keybinding_mutations(&self, root: RootId) -> Result<bool, RuntimeError> {
        let mutations = std::mem::take(&mut *self.local_state.keybinding_mutations.lock().unwrap());
        if mutations.is_empty() {
            return Ok(false);
        }
        let request_value = self
            .local_state
            .next_request
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(1)
            })
            .map_err(|_| {
                RuntimeError::fatal(RuntimeErrorKind::Engine, "request identity exhausted")
            })?;
        let id = RequestId::new(request_value);
        let operation = super::declarations::registry()
            .get("mutateKeybindings")
            .expect("native keymap handler")
            .request(super::protocol::KeybindingArguments { mutations });
        self.local_state.pending.lock().unwrap().insert(
            id,
            PendingRequest {
                root,
                invocation: None,
                operation: operation.descriptor.name,
                resolver: None,
                response_queued: false,
                provider_mutation: None,
            },
        );
        *self.local_state.keybinding_batch_pending.lock().unwrap() = Some(id);
        let sender = self.local_state.event_sender.lock().unwrap().clone();
        let sent = sender.is_some_and(|sender| {
            sender
                .send(EngineEvent::Request(HostRequest {
                    extension: self.key.extension,
                    lifecycle: self.key.lifecycle,
                    id,
                    invocation: None,
                    operation,
                }))
                .is_ok()
        });
        if !sent {
            self.local_state.pending.lock().unwrap().remove(&id);
            *self.local_state.keybinding_batch_pending.lock().unwrap() = None;
            return Err(RuntimeError::fatal(
                RuntimeErrorKind::Engine,
                "keybinding batch could not reach the application",
            ));
        }
        Ok(true)
    }

    fn reject_pending_for_root(&self, scope: &mut v8::PinScope<'_, '_>, root: RootId) {
        let pending = {
            let mut requests = self.local_state.pending.lock().unwrap();
            let ids: Vec<_> = requests
                .iter()
                .filter_map(|(id, pending)| (pending.root == root).then_some(*id))
                .collect();
            ids.into_iter()
                .filter_map(|id| requests.remove(&id))
                .collect::<Vec<_>>()
        };
        for pending in pending {
            if let Some(resolver) = pending.resolver {
                let resolver = v8::Local::new(scope, &resolver);
                let cancelled =
                    boundary_exception(scope, BoundaryError::Host(TransportError::Cancelled));
                resolver.reject(scope, cancelled);
            }
        }
        scope.perform_microtask_checkpoint();
    }

    fn reject_pending_for_command(&self, scope: &mut v8::PinScope<'_, '_>, root: RootId) {
        let pending = {
            let mut requests = self.local_state.pending.lock().unwrap();
            let ids: Vec<_> = requests
                .iter()
                .filter_map(|(id, pending)| {
                    (pending.root == root && pending.invocation.is_some()).then_some(*id)
                })
                .collect();
            ids.into_iter()
                .filter_map(|id| {
                    if requests
                        .get(&id)
                        .is_some_and(|pending| pending.response_queued)
                    {
                        // Keep an admitted response attached to this root until its queued turn consumes it.
                        requests
                            .get_mut(&id)
                            .and_then(|pending| pending.resolver.take())
                    } else {
                        requests.remove(&id).and_then(|pending| pending.resolver)
                    }
                })
                .collect::<Vec<_>>()
        };
        for resolver in pending {
            let resolver = v8::Local::new(scope, &resolver);
            let cancelled =
                boundary_exception(scope, BoundaryError::Host(TransportError::Cancelled));
            resolver.reject(scope, cancelled);
        }
        scope.perform_microtask_checkpoint();
    }

    fn take_unhandled_rejection(&self, fallback_source: &str) -> Option<RuntimeError> {
        let mut rejections = {
            let mut queued = self.local_state.rejections.lock().unwrap();
            std::mem::take(&mut *queued)
        };
        if rejections.is_empty() {
            return None;
        }
        let rejection = rejections.remove(0);
        let mut message = rejection.message;
        for additional in &rejections {
            message.push('\n');
            message.push_str(&additional.message);
        }
        Some(RuntimeError {
            kind: RuntimeErrorKind::Rejection,
            message: message.into(),
            source: rejection
                .source
                .or_else(|| Some(fallback_source.to_owned()))
                .map(Into::into),
            line: rejection.line,
            column: rejection.column,
            stack: rejection.stack.map(Into::into),
        })
    }

    fn fail_active(&self, error: RuntimeError) {
        self.fail_active_with_error(error, false);
    }

    fn fail_active_with_command_failure(&self, error: RuntimeError) {
        self.fail_active_with_error(error, true);
    }

    fn fail_active_with_error(&self, error: RuntimeError, command_failure: bool) {
        if let Some(active) = self.local_state.active.lock().unwrap().take() {
            match active.completion {
                ActiveCompletion::Report(result) => {
                    let _ = result.send(Err(error));
                }
                ActiveCompletion::Command(result) => {
                    let outcome = if command_failure {
                        Ok(CommandOutcome::HandlerFailure {
                            message: error.to_string(),
                        })
                    } else {
                        Err(error)
                    };
                    let _ = result.send(outcome);
                }
                ActiveCompletion::Tree {
                    result, completed, ..
                } => {
                    completed.store(true, Ordering::Release);
                    let _ = result.send(Err(error));
                }
                ActiveCompletion::Completion {
                    result, completed, ..
                } => {
                    completed.store(true, Ordering::Release);
                    let _ = result.send(Err(error));
                }
                ActiveCompletion::Detached(_) => {}
            }
        }
        self.local_state.pending.lock().unwrap().clear();
        self.local_state
            .keybinding_mutations
            .lock()
            .unwrap()
            .clear();
        *self.local_state.keybinding_batch_pending.lock().unwrap() = None;
        self.local_state.deferred_responses.lock().unwrap().clear();
        *self.local_state.current_root.lock().unwrap() = None;
    }

    pub(crate) fn hold_external_utf16(
        &self,
        global_name: &str,
        text: Arc<[u16]>,
    ) -> Result<(), RuntimeError> {
        if self.status.load(Ordering::Acquire) != ACTIVE {
            return Err(self.status_error());
        }
        let data = self.data.lock().unwrap();
        let data = data.as_ref().ok_or_else(RuntimeError::disposed)?;
        let mut locker = data.isolate.lock();
        let scope = pin!(v8::HandleScope::new(&mut *locker));
        let mut scope = scope.init();
        let context = v8::Local::new(&scope, &data.context);
        let scope = &mut v8::ContextScope::new(&mut scope, context);
        let name = v8::String::new(scope, global_name).ok_or_else(RuntimeError::disposed)?;
        let length = text.len();
        let buffer = Arc::into_raw(text).cast::<u16>() as *mut u16;
        // SAFETY: one Arc strong reference was converted into `buffer`; the
        // destructor rebuilds the same slice pointer using V8's retained length.
        let value = unsafe {
            v8::String::new_external_twobyte_raw(scope, buffer, length, drop_external_utf16)
        }
        .ok_or_else(RuntimeError::disposed)?;
        if context.global(scope).set(scope, name.into(), value.into()) != Some(true) {
            return Err(RuntimeError::fatal(
                RuntimeErrorKind::Exception,
                "could not retain external UTF-16 text in the extension context",
            ));
        }
        Ok(())
    }

    pub(crate) fn dispose(&self) {
        self.dispose_with_error(RuntimeError::fatal(
            RuntimeErrorKind::Cancelled,
            "extension JavaScript execution was cancelled",
        ));
    }

    fn dispose_with_error(&self, error: RuntimeError) {
        if self.status.load(Ordering::Acquire) == DISPOSED {
            return;
        }
        let _ =
            self.status
                .compare_exchange(ACTIVE, TERMINATED, Ordering::AcqRel, Ordering::Acquire);
        let _ = self.heap_state.isolate.terminate_execution();
        let Some(data) = self
            .data
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        else {
            return;
        };
        self.status.store(DISPOSED, Ordering::Release);
        {
            let mut locker = data.isolate.lock();
            {
                let scope = pin!(v8::HandleScope::new(&mut *locker));
                let mut scope = scope.init();
                let context = v8::Local::new(&scope, &data.context);
                let scope = &mut v8::ContextScope::new(&mut scope, context);
                let pending = std::mem::take(&mut *self.local_state.pending.lock().unwrap());
                for pending in pending.into_values() {
                    if let Some(resolver) = pending.resolver {
                        let resolver = v8::Local::new(scope, &resolver);
                        let cancelled = boundary_exception(
                            scope,
                            BoundaryError::Host(TransportError::Cancelled),
                        );
                        resolver.reject(scope, cancelled);
                    }
                }
                scope.perform_microtask_checkpoint();
            }
            self.fail_active(error);
            locker.remove_near_heap_limit_callback(near_heap_limit_callback, 0);
            self.local_state
                .rejections
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clear();
            self.local_state.callbacks.lock().unwrap().clear();
            clear_module_registry(&self.local_state);
            let _ = locker.remove_slot::<Arc<RuntimeLocalState>>();
            drop(data.context);
        }
        drop(data.isolate);
    }

    fn execution_error(
        &self,
        try_catch: &mut v8::PinnedRef<'_, v8::TryCatch<v8::HandleScope>>,
        source_name: &str,
    ) -> RuntimeError {
        if try_catch.has_terminated() || self.status.load(Ordering::Acquire) != ACTIVE {
            self.status_error()
        } else {
            runtime_error_from_try_catch(try_catch, RuntimeErrorKind::Exception, source_name)
        }
    }

    fn status_error(&self) -> RuntimeError {
        match self.status.load(Ordering::Acquire) {
            HEAP_LIMIT_EXCEEDED => RuntimeError::fatal(
                RuntimeErrorKind::HeapLimitExceeded,
                "extension exceeded its JavaScript heap limit",
            ),
            TERMINATED => RuntimeError::fatal(
                RuntimeErrorKind::Terminated,
                "extension JavaScript execution was terminated",
            ),
            _ => RuntimeError::disposed(),
        }
    }

    #[cfg(test)]
    fn install_test_barrier(&self, barrier: Arc<TestGate>) {
        *self.local_state.barrier.lock().unwrap() = Some(barrier);
        let data = self.data.lock().unwrap();
        let data = data.as_ref().unwrap();
        let mut locker = data.isolate.lock();
        let scope = pin!(v8::HandleScope::new(&mut *locker));
        let mut scope = scope.init();
        let context = v8::Local::new(&scope, &data.context);
        let scope = &mut v8::ContextScope::new(&mut scope, context);
        let name = v8::String::new(scope, "__knotTestBarrier").unwrap();
        let function = v8::Function::new(scope, test_barrier_callback).unwrap();
        context
            .global(scope)
            .set(scope, name.into(), function.into());
    }

    #[cfg(test)]
    fn install_test_entered_signal(&self, entered: mpsc::Sender<()>) {
        *self.local_state.entered.lock().unwrap() = Some(entered);
        let data = self.data.lock().unwrap();
        let data = data.as_ref().unwrap();
        let mut locker = data.isolate.lock();
        let scope = pin!(v8::HandleScope::new(&mut *locker));
        let mut scope = scope.init();
        let context = v8::Local::new(&scope, &data.context);
        let scope = &mut v8::ContextScope::new(&mut scope, context);
        let name = v8::String::new(scope, "__knotTestEntered").unwrap();
        let function = v8::Function::new(scope, test_entered_callback).unwrap();
        context
            .global(scope)
            .set(scope, name.into(), function.into());
    }

    #[cfg(test)]
    fn set_test_host_error(&self, error: HostFailure) {
        *self.local_state.host_error.lock().unwrap() = Some(error);
    }
}

impl Drop for RuntimeCapsule {
    fn drop(&mut self) {
        self.dispose();
    }
}

/// Thread-safe, V8-free control surface for forced termination.
pub(crate) struct TerminationHandle {
    isolate: v8::IsolateHandle,
    status: Arc<AtomicU8>,
    wake: Option<TerminationWake>,
}

struct TerminationWake {
    key: ExtensionKey,
    capsule: Arc<RuntimeCapsule>,
    scheduler: SchedulerHandle<EngineTurn>,
}

impl TerminationHandle {
    pub(crate) fn terminate(&self) -> bool {
        if self
            .status
            .compare_exchange(ACTIVE, TERMINATED, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        let terminated = self.isolate.terminate_execution();
        if terminated
            && let Some(wake) = &self.wake
            && let Some(root) = wake
                .capsule
                .local_state
                .active
                .lock()
                .unwrap()
                .as_ref()
                .map(|active| active.root)
        {
            let _ = wake.scheduler.enqueue_continuation(
                wake.key,
                root,
                EngineTurn {
                    capsule: Arc::clone(&wake.capsule),
                    work: EngineWork::Termination,
                },
            );
        }
        terminated
    }
}

struct EngineTurn {
    capsule: Arc<RuntimeCapsule>,
    work: EngineWork,
}

enum ListenerNotification {
    Buffer(BufferChange),
    Application(ApplicationEvent),
}

enum EngineWork {
    ApplicationChange {
        subscription: WorkbenchSubscriptionId,
        event: ApplicationEvent,
    },
    Script {
        source_name: Arc<str>,
        source: Arc<str>,
        result: ResultSender<ExecutionReport>,
    },
    FixtureModule {
        specifier: Arc<str>,
        source: Arc<str>,
        result: ResultSender<ExecutionReport>,
    },
    PackageModule {
        entry: Arc<str>,
        result: ResultSender<ExecutionReport>,
    },
    HostResponse(HostResponse),
    BufferChange {
        subscription: BufferSubscriptionId,
        change: BufferChange,
        enqueued_at: Instant,
    },
    CommandInvocation {
        invocation: CommandInvocation,
        active_buffer: Option<BufferHandle>,
        result: ResultSender<CommandOutcome>,
    },
    CancelCommand(CommandInvocationId),
    Provider(ProviderCall),
    CancelProvider,
    Termination,
    #[cfg(test)]
    Panic {
        result: ResultSender<ExecutionReport>,
    },
}

/// Completion side of one scheduled JavaScript turn.
pub(crate) struct RuntimeExecution {
    result: ResultReceiver<ExecutionReport>,
    completion: CompletionReceiver,
}

impl RuntimeExecution {
    pub(crate) fn wait(self) -> Result<ExecutionReport, RuntimeError> {
        match self.result.blocking_recv() {
            Ok(result) => {
                let _ = self.completion.recv();
                result
            }
            Err(_) => match self.completion.recv() {
                Ok(CompletionOutcome::Cancelled) => Err(RuntimeError::fatal(
                    RuntimeErrorKind::Cancelled,
                    "extension JavaScript execution was cancelled",
                )),
                Ok(CompletionOutcome::Failed(failure)) => Err(RuntimeError::fatal(
                    RuntimeErrorKind::LifecycleFailed,
                    failure.message().to_owned(),
                )),
                Ok(CompletionOutcome::Completed) | Err(_) => Err(RuntimeError::fatal(
                    RuntimeErrorKind::Engine,
                    "extension worker closed without an execution result",
                )),
            },
        }
    }
}

impl std::future::Future for RuntimeExecution {
    type Output = Result<ExecutionReport, RuntimeError>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        match std::pin::Pin::new(&mut self.result).poll(context) {
            std::task::Poll::Ready(Ok(result)) => std::task::Poll::Ready(result),
            std::task::Poll::Ready(Err(_)) => std::pin::Pin::new(&mut self.completion)
                .poll(context)
                .map(|outcome| match outcome {
                    Ok(CompletionOutcome::Cancelled) => Err(RuntimeError::fatal(
                        RuntimeErrorKind::Cancelled,
                        "extension JavaScript execution was cancelled",
                    )),
                    Ok(CompletionOutcome::Failed(failure)) => Err(RuntimeError::fatal(
                        RuntimeErrorKind::LifecycleFailed,
                        failure.message().to_owned(),
                    )),
                    Ok(CompletionOutcome::Completed) | Err(_) => Err(RuntimeError::fatal(
                        RuntimeErrorKind::Engine,
                        "extension worker closed without an execution result",
                    )),
                }),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

/// Completion side of one scheduled extension command handler.
pub(crate) struct RuntimeCommandExecution {
    result: ResultReceiver<CommandOutcome>,
    completion: CompletionReceiver,
}

impl RuntimeCommandExecution {
    pub(crate) fn wait(self) -> Result<CommandOutcome, RuntimeError> {
        match self.result.blocking_recv() {
            Ok(result) => {
                let _ = self.completion.recv();
                result
            }
            Err(_) => match self.completion.recv() {
                Ok(CompletionOutcome::Cancelled) => Ok(CommandOutcome::Cancelled),
                Ok(CompletionOutcome::Failed(failure)) => Err(RuntimeError::fatal(
                    RuntimeErrorKind::LifecycleFailed,
                    failure.message().to_owned(),
                )),
                Ok(CompletionOutcome::Completed) | Err(_) => Err(RuntimeError::fatal(
                    RuntimeErrorKind::Engine,
                    "extension worker closed without a command outcome",
                )),
            },
        }
    }
}

impl std::future::Future for RuntimeCommandExecution {
    type Output = Result<CommandOutcome, RuntimeError>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        match std::pin::Pin::new(&mut self.result).poll(context) {
            std::task::Poll::Ready(Ok(result)) => std::task::Poll::Ready(result),
            std::task::Poll::Ready(Err(_)) => std::pin::Pin::new(&mut self.completion)
                .poll(context)
                .map(|outcome| match outcome {
                    Ok(CompletionOutcome::Cancelled) => Ok(CommandOutcome::Cancelled),
                    Ok(CompletionOutcome::Failed(failure)) => Err(RuntimeError::fatal(
                        RuntimeErrorKind::LifecycleFailed,
                        failure.message().to_owned(),
                    )),
                    Ok(CompletionOutcome::Completed) | Err(_) => Err(RuntimeError::fatal(
                        RuntimeErrorKind::Engine,
                        "extension worker closed without a command outcome",
                    )),
                }),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

/// Completion side of one native-to-extension semantic provider callback.
pub(crate) struct RuntimeProviderExecution<T> {
    result: Option<ResultReceiver<T>>,
    completion: CompletionReceiver,
    cancellation: Option<ProviderCancellation>,
}

impl<T> Unpin for RuntimeProviderExecution<T> {}

struct ProviderCancellation {
    provider: ProviderIdentity,
    key: ExtensionKey,
    root: RootId,
    capsule: Arc<RuntimeCapsule>,
    scheduler: SchedulerHandle<EngineTurn>,
    cancelled: Arc<AtomicBool>,
    completed: Arc<AtomicBool>,
}

impl<T> RuntimeProviderExecution<T> {
    pub(crate) fn wait(mut self) -> Result<T, RuntimeError> {
        let result = match self
            .result
            .take()
            .expect("provider execution owns its result")
            .blocking_recv()
        {
            Ok(result) => {
                let _ = self.completion.recv();
                result
            }
            Err(_) => match self.completion.recv() {
                Ok(CompletionOutcome::Cancelled) => Err(RuntimeError::fatal(
                    RuntimeErrorKind::Cancelled,
                    "semantic provider request was cancelled",
                )),
                Ok(CompletionOutcome::Failed(failure)) => Err(RuntimeError::fatal(
                    RuntimeErrorKind::LifecycleFailed,
                    failure.message().to_owned(),
                )),
                Ok(CompletionOutcome::Completed) | Err(_) => Err(RuntimeError::fatal(
                    RuntimeErrorKind::Engine,
                    "extension worker closed without a provider response",
                )),
            },
        };
        self.cancellation = None;
        result
    }
}

impl<T> std::future::Future for RuntimeProviderExecution<T> {
    type Output = Result<T, RuntimeError>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let receiver = self
            .result
            .as_mut()
            .expect("provider execution owns its result");
        match std::pin::Pin::new(receiver).poll(context) {
            std::task::Poll::Ready(result) => match result {
                Ok(result) => {
                    self.result = None;
                    self.cancellation = None;
                    std::task::Poll::Ready(result)
                }
                Err(_) => match std::pin::Pin::new(&mut self.completion).poll(context) {
                    std::task::Poll::Ready(outcome) => {
                        self.result = None;
                        self.cancellation = None;
                        std::task::Poll::Ready(match outcome {
                            Ok(CompletionOutcome::Cancelled) => Err(RuntimeError::fatal(
                                RuntimeErrorKind::Cancelled,
                                "semantic provider request was cancelled",
                            )),
                            Ok(CompletionOutcome::Failed(failure)) => Err(RuntimeError::fatal(
                                RuntimeErrorKind::LifecycleFailed,
                                failure.message().to_owned(),
                            )),
                            Ok(CompletionOutcome::Completed) | Err(_) => Err(RuntimeError::fatal(
                                RuntimeErrorKind::Engine,
                                "extension worker closed without a provider response",
                            )),
                        })
                    }
                    std::task::Poll::Pending => std::task::Poll::Pending,
                },
            },
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

impl<T> Drop for RuntimeProviderExecution<T> {
    fn drop(&mut self) {
        let Some(cancellation) = self.cancellation.take() else {
            return;
        };
        if cancellation.completed.load(Ordering::Acquire) {
            return;
        }
        // A native ownership mutation may discard its own callback receiver.
        // Its host response must still settle the callback's awaited mutation.
        let self_mutation = cancellation
            .capsule
            .local_state
            .pending
            .lock()
            .unwrap()
            .values()
            .any(|pending| {
                pending.root == cancellation.root
                    && pending.provider_mutation == Some(cancellation.provider)
            });
        if self_mutation {
            return;
        }
        cancellation.cancelled.store(true, Ordering::Release);
        let _ = cancellation.scheduler.enqueue_continuation(
            cancellation.key,
            cancellation.root,
            EngineTurn {
                capsule: cancellation.capsule,
                work: EngineWork::CancelProvider,
            },
        );
    }
}

/// Engine-owned composition of persistent capsules and the bounded scheduler.
pub(crate) struct RuntimePool {
    scheduler: Option<SchedulerPool<EngineTurn>>,
    scheduler_handle: SchedulerHandle<EngineTurn>,
    finalizer: LifecycleFinalizer,
    events: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<EngineEvent>>>,
}

#[derive(Clone)]
struct LifecycleFinalizer {
    runtimes: Arc<Mutex<HashMap<ExtensionId, Arc<RuntimeCapsule>>>>,
    events: tokio::sync::mpsc::UnboundedSender<EngineEvent>,
}

impl LifecycleFinalizer {
    fn startup_failed(&self, key: ExtensionKey) {
        let _ = self.events.send(EngineEvent::LifecycleEnded {
            key,
            reason: LifecycleEnd::StartupFailed,
        });
    }

    fn finalize(
        &self,
        key: ExtensionKey,
        reason: LifecycleEnd,
        error: RuntimeError,
        terminate: bool,
    ) -> bool {
        let capsule = {
            let mut runtimes = self.runtimes.lock().unwrap();
            if !runtimes
                .get(&key.extension)
                .is_some_and(|current| current.key() == key)
            {
                return false;
            }
            runtimes.remove(&key.extension).expect("matching lifecycle")
        };
        if terminate {
            let _ = capsule.termination_handle().terminate();
        }
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            capsule.dispose_with_error(error);
        }));
        let _ = self
            .events
            .send(EngineEvent::LifecycleEnded { key, reason });
        true
    }

    fn capsules(&self) -> Vec<Arc<RuntimeCapsule>> {
        self.runtimes.lock().unwrap().values().cloned().collect()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum LifecycleEnd {
    StartupFailed,
    Unloaded,
    Failed(Failure),
    Shutdown,
}

#[derive(Clone, Debug)]
pub(crate) enum EngineEvent {
    Request(HostRequest),
    LifecycleEnded {
        key: ExtensionKey,
        reason: LifecycleEnd,
    },
}

pub(crate) struct EngineEventInbox {
    receiver: tokio::sync::mpsc::UnboundedReceiver<EngineEvent>,
}

impl EngineEventInbox {
    pub(crate) async fn receive(&mut self) -> Option<EngineEvent> {
        self.receiver.recv().await
    }

    #[cfg(test)]
    pub(crate) fn receive_timeout(&mut self, timeout: Duration) -> Option<EngineEvent> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.receiver.try_recv() {
                Ok(event) => return Some(event),
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return None,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                    if Instant::now() >= deadline {
                        return None;
                    }
                    std::thread::yield_now();
                }
            }
        }
    }
}

impl RuntimePool {
    pub(crate) fn new(config: PoolConfig) -> Self {
        initialize();
        let runtimes = Arc::new(Mutex::new(HashMap::new()));
        let (event_sender, events) = tokio::sync::mpsc::unbounded_channel();
        let finalizer = LifecycleFinalizer {
            runtimes,
            events: event_sender,
        };
        let executor_finalizer = finalizer.clone();
        let scheduler = SchedulerPool::new(config, move |turn| {
            execute_engine_turn(turn, &executor_finalizer)
        });
        let scheduler_handle = scheduler.handle();
        Self {
            scheduler: Some(scheduler),
            scheduler_handle,
            finalizer,
            events: Mutex::new(Some(events)),
        }
    }

    pub(crate) fn take_event_inbox(&self) -> Option<EngineEventInbox> {
        self.events
            .lock()
            .unwrap()
            .take()
            .map(|receiver| EngineEventInbox { receiver })
    }

    pub(crate) fn load(
        &self,
        key: ExtensionKey,
        config: IsolateConfig,
    ) -> Result<(), RuntimePoolError> {
        self.load_with_graph(key, config, None)
    }

    fn load_with_graph(
        &self,
        key: ExtensionKey,
        config: IsolateConfig,
        graph: Option<&ModuleGraph>,
    ) -> Result<(), RuntimePoolError> {
        let scheduler = self.scheduler.as_ref().expect("runtime pool is active");
        let handle = scheduler.handle();
        handle.admit(key)?;
        let capsule = match RuntimeCapsule::new_with_graph(key, config, graph) {
            Ok(capsule) => {
                *capsule.local_state.event_sender.lock().unwrap() =
                    Some(self.finalizer.events.clone());
                Arc::new(capsule)
            }
            Err(error) => {
                let _ = handle.finish_loading(key, Err(Failure::new(error.to_string())));
                self.finalizer.startup_failed(key);
                return Err(error.into());
            }
        };
        self.finalizer
            .runtimes
            .lock()
            .unwrap()
            .insert(key.extension, Arc::clone(&capsule));
        let finish = handle.finish_loading(key, Ok(()));
        if let Err(error) = finish {
            self.finalizer.finalize(
                key,
                LifecycleEnd::StartupFailed,
                RuntimeError::fatal(RuntimeErrorKind::Cancelled, "extension startup failed"),
                true,
            );
            return Err(error.into());
        }
        Ok(())
    }

    pub(crate) fn load_package(
        &self,
        key: ExtensionKey,
        config: IsolateConfig,
        graph: ModuleGraph,
    ) -> Result<RuntimeExecution, RuntimePoolError> {
        let entry: Arc<str> = Arc::from(graph.entry());
        self.load_with_graph(key, config, Some(&graph))?;
        let capsule = self.capsule(key)?;
        let (result_sender, result) = tokio::sync::oneshot::channel();
        let scheduled = self.scheduler_handle.enqueue_root(
            key,
            EngineTurn {
                capsule,
                work: EngineWork::PackageModule {
                    entry,
                    result: result_sender,
                },
            },
        );
        let (_, completion) = match scheduled {
            Ok(scheduled) => scheduled,
            Err(error) => {
                let _ = self.unload(key);
                return Err(error.into());
            }
        };
        Ok(RuntimeExecution { result, completion })
    }

    pub(crate) fn load_personal_config(
        &self,
        key: ExtensionKey,
        config: IsolateConfig,
        graph: ModuleGraph,
    ) -> Result<(), RuntimePoolError> {
        if graph.entries().keys().any(|name| {
            name != ConfigPhase::PreInit.entry() && name != ConfigPhase::PostInit.entry()
        }) {
            return Err(RuntimeError::fatal(
                RuntimeErrorKind::InvalidModuleSpecifier,
                "personal config has an unknown phase entry",
            )
            .into());
        }
        self.load_with_graph(key, config, Some(&graph))
    }

    pub(crate) fn evaluate_personal_config_phase(
        &self,
        key: ExtensionKey,
        phase: ConfigPhase,
    ) -> Result<RuntimeExecution, RuntimePoolError> {
        let capsule = self.capsule(key)?;
        let entry = {
            let modules = capsule.local_state.modules.lock().unwrap();
            modules.entries.get(phase.entry()).cloned().ok_or_else(|| {
                RuntimeError::fatal(
                    RuntimeErrorKind::InvalidModuleSpecifier,
                    format!("personal config {} is unavailable", phase.entry()),
                )
            })?
        };
        let (result_sender, result) = tokio::sync::oneshot::channel();
        let (_, completion) = self.scheduler_handle.enqueue_root(
            key,
            EngineTurn {
                capsule,
                work: EngineWork::PackageModule {
                    entry,
                    result: result_sender,
                },
            },
        )?;
        Ok(RuntimeExecution { result, completion })
    }

    pub(crate) fn execute(
        &self,
        key: ExtensionKey,
        source_name: impl Into<Arc<str>>,
        source: impl Into<Arc<str>>,
    ) -> Result<RuntimeExecution, RuntimePoolError> {
        let capsule = self.capsule(key)?;
        let (result_sender, result) = tokio::sync::oneshot::channel();
        let (_, completion) = self
            .scheduler
            .as_ref()
            .expect("runtime pool is active")
            .handle()
            .enqueue_root(
                key,
                EngineTurn {
                    capsule,
                    work: EngineWork::Script {
                        source_name: source_name.into(),
                        source: source.into(),
                        result: result_sender,
                    },
                },
            )?;
        Ok(RuntimeExecution { result, completion })
    }

    pub(crate) fn execute_fixture_module(
        &self,
        key: ExtensionKey,
        specifier: impl Into<Arc<str>>,
        source: impl Into<Arc<str>>,
    ) -> Result<RuntimeExecution, RuntimePoolError> {
        let capsule = self.capsule(key)?;
        let (result_sender, result) = tokio::sync::oneshot::channel();
        let (_, completion) = self
            .scheduler
            .as_ref()
            .expect("runtime pool is active")
            .handle()
            .enqueue_root(
                key,
                EngineTurn {
                    capsule,
                    work: EngineWork::FixtureModule {
                        specifier: specifier.into(),
                        source: source.into(),
                        result: result_sender,
                    },
                },
            )?;
        Ok(RuntimeExecution { result, completion })
    }

    #[cfg(test)]
    pub(crate) fn panic_worker_turn(
        &self,
        key: ExtensionKey,
    ) -> Result<RuntimeExecution, RuntimePoolError> {
        let capsule = self.capsule(key)?;
        let (result_sender, result) = tokio::sync::oneshot::channel();
        let (_, completion) = self.scheduler_handle.enqueue_root(
            key,
            EngineTurn {
                capsule,
                work: EngineWork::Panic {
                    result: result_sender,
                },
            },
        )?;
        Ok(RuntimeExecution { result, completion })
    }

    pub(crate) fn invoke_command(
        &self,
        key: ExtensionKey,
        invocation: CommandInvocation,
        active_buffer: Option<BufferHandle>,
    ) -> Result<RuntimeCommandExecution, RuntimePoolError> {
        if invocation.extension != key.extension || invocation.lifecycle != key.lifecycle {
            return Err(SchedulerError::StaleLifecycle {
                current: key.lifecycle,
                received: invocation.lifecycle,
            }
            .into());
        }
        let capsule = self.capsule(key)?;
        let (result_sender, result) = tokio::sync::oneshot::channel();
        let (_, completion) = self.scheduler_handle.enqueue_root(
            key,
            EngineTurn {
                capsule,
                work: EngineWork::CommandInvocation {
                    invocation,
                    active_buffer,
                    result: result_sender,
                },
            },
        )?;
        Ok(RuntimeCommandExecution { result, completion })
    }

    pub(crate) fn request_tree_children(
        &self,
        key: ExtensionKey,
        request: TreeChildrenRequest,
    ) -> Result<RuntimeProviderExecution<TreeChildrenResponse>, RuntimePoolError> {
        let capsule = self.capsule(key)?;
        let registration = request.registration;
        let (result_sender, result) = tokio::sync::oneshot::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let completed = Arc::new(AtomicBool::new(false));
        let (root, completion) = self.scheduler_handle.enqueue_root(
            key,
            EngineTurn {
                capsule: Arc::clone(&capsule),
                work: EngineWork::Provider(ProviderCall::Tree {
                    request,
                    result: result_sender,
                    cancelled: Arc::clone(&cancelled),
                    completed: Arc::clone(&completed),
                }),
            },
        )?;
        Ok(RuntimeProviderExecution {
            result: Some(result),
            completion,
            cancellation: Some(ProviderCancellation {
                provider: ProviderIdentity::Tree(registration),
                key,
                root,
                capsule,
                scheduler: self.scheduler_handle.clone(),
                cancelled,
                completed,
            }),
        })
    }

    pub(crate) fn request_completions(
        &self,
        key: ExtensionKey,
        request: CompletionRequest,
    ) -> Result<RuntimeProviderExecution<CompletionResponse>, RuntimePoolError> {
        let capsule = self.capsule(key)?;
        let registration = request.registration;
        let (result_sender, result) = tokio::sync::oneshot::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let completed = Arc::new(AtomicBool::new(false));
        let (root, completion) = self.scheduler_handle.enqueue_root(
            key,
            EngineTurn {
                capsule: Arc::clone(&capsule),
                work: EngineWork::Provider(ProviderCall::Completion {
                    request,
                    result: result_sender,
                    cancelled: Arc::clone(&cancelled),
                    completed: Arc::clone(&completed),
                }),
            },
        )?;
        Ok(RuntimeProviderExecution {
            result: Some(result),
            completion,
            cancellation: Some(ProviderCancellation {
                provider: ProviderIdentity::Completion(registration),
                key,
                root,
                capsule,
                scheduler: self.scheduler_handle.clone(),
                cancelled,
                completed,
            }),
        })
    }

    pub(crate) fn cancel_command(
        &self,
        key: ExtensionKey,
        invocation: CommandInvocationId,
    ) -> Result<(), RuntimePoolError> {
        if invocation.value() > MAX_JS_INTEGER {
            return Err(RuntimeError::fatal(
                RuntimeErrorKind::Engine,
                "native command identity cannot be represented exactly",
            )
            .into());
        }
        let capsule = self.capsule(key)?;
        let mut cancelled = capsule.local_state.cancelled_commands.lock().unwrap();
        cancelled.insert(invocation);
        let root = capsule
            .local_state
            .active
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|active| {
                matches!(&active.completion, ActiveCompletion::Command(_)).then_some(active.root)
            });
        drop(cancelled);
        if let Some(root) = root {
            self.scheduler_handle.enqueue_continuation(
                key,
                root,
                EngineTurn {
                    capsule,
                    work: EngineWork::CancelCommand(invocation),
                },
            )?;
        }
        Ok(())
    }

    /// Panics if the extension's lifecycle ends in failure while a test awaits a
    /// request: that always means the fixture never reached the expected call,
    /// and hiding the failure behind a generic missing-request panic makes the
    /// cause much harder to find.
    #[cfg(test)]
    fn lifecycle_failure_while_waiting(reason: &LifecycleEnd) {
        match reason {
            LifecycleEnd::Failed(failure) => panic!(
                "extension failed while a request was expected: {}",
                failure.message()
            ),
            LifecycleEnd::StartupFailed => {
                panic!("extension startup failed while a request was expected")
            }
            LifecycleEnd::Unloaded | LifecycleEnd::Shutdown => {}
        }
    }

    #[cfg(test)]
    pub(crate) fn receive_request(&self) -> Option<HostRequest> {
        loop {
            let event = self.events.lock().unwrap().as_mut()?.blocking_recv()?;
            match event {
                EngineEvent::Request(request) => return Some(request),
                EngineEvent::LifecycleEnded { reason, .. } => {
                    Self::lifecycle_failure_while_waiting(&reason)
                }
            }
        }
    }

    #[cfg(test)]
    fn receive_request_timeout(&self, timeout: std::time::Duration) -> Option<HostRequest> {
        let deadline = Instant::now() + timeout;
        loop {
            let event = self.events.lock().unwrap().as_mut()?.try_recv();
            match event {
                Ok(EngineEvent::Request(request)) => return Some(request),
                Ok(EngineEvent::LifecycleEnded { reason, .. }) => {
                    Self::lifecycle_failure_while_waiting(&reason)
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return None,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                    if Instant::now() >= deadline {
                        return None;
                    }
                    std::thread::yield_now();
                }
            }
        }
    }

    pub(crate) fn respond(&self, response: HostResponse) -> Result<(), RuntimeResponseError> {
        let request_id = response.id;
        let capsule = {
            let runtimes = self.finalizer.runtimes.lock().unwrap();
            let Some(capsule) = runtimes.get(&response.extension) else {
                return Err(RuntimeResponseError::WrongExtension);
            };
            if capsule.key().lifecycle != response.lifecycle {
                return Err(RuntimeResponseError::WrongExtension);
            }
            Arc::clone(capsule)
        };
        let root = {
            let mut pending = capsule.local_state.pending.lock().unwrap();
            let Some(pending) = pending.get_mut(&request_id) else {
                return Err(RuntimeResponseError::UnknownRequest);
            };
            if pending.response_queued {
                return Err(RuntimeResponseError::UnknownRequest);
            }
            pending.response_queued = true;
            pending.root
        };
        if let Err(error) = self.scheduler_handle.enqueue_continuation(
            capsule.key(),
            root,
            EngineTurn {
                capsule: Arc::clone(&capsule),
                work: EngineWork::HostResponse(response),
            },
        ) {
            if let Some(pending) = capsule
                .local_state
                .pending
                .lock()
                .unwrap()
                .get_mut(&request_id)
            {
                pending.response_queued = false;
            }
            return Err(error.into());
        }
        Ok(())
    }

    pub(crate) fn dispatch_application_change(
        &self,
        key: ExtensionKey,
        subscription: WorkbenchSubscriptionId,
        event: ApplicationEvent,
    ) -> Result<(), RuntimePoolError> {
        let capsule = self.capsule(key)?;
        self.scheduler_handle.enqueue_root(
            key,
            EngineTurn {
                capsule,
                work: EngineWork::ApplicationChange {
                    subscription,
                    event,
                },
            },
        )?;
        Ok(())
    }

    pub(crate) fn dispatch_buffer_change(
        &self,
        key: ExtensionKey,
        subscription: BufferSubscriptionId,
        change: BufferChange,
    ) -> Result<(), RuntimePoolError> {
        let capsule = self.capsule(key)?;
        capsule.local_state.buffer_change_enqueued();
        let result = self.scheduler_handle.enqueue_root(
            key,
            EngineTurn {
                capsule: Arc::clone(&capsule),
                work: EngineWork::BufferChange {
                    subscription,
                    change,
                    enqueued_at: Instant::now(),
                },
            },
        );
        if let Err(error) = result {
            capsule.local_state.buffer_change_discarded();
            return Err(error.into());
        }
        Ok(())
    }

    pub(crate) fn buffer_change_queue_metrics(
        &self,
        key: ExtensionKey,
    ) -> Result<BufferChangeQueueMetrics, RuntimePoolError> {
        Ok(self.capsule(key)?.local_state.buffer_change_queue_metrics())
    }

    pub(crate) fn diagnostics(&self) -> super::scheduler::SchedulerDiagnostics {
        self.scheduler_handle.diagnostics()
    }

    pub(crate) fn termination_handle(
        &self,
        key: ExtensionKey,
    ) -> Result<TerminationHandle, RuntimePoolError> {
        let capsule = self.capsule(key)?;
        Ok(TerminationHandle {
            isolate: capsule.heap_state.isolate.clone(),
            status: Arc::clone(&capsule.status),
            wake: Some(TerminationWake {
                key,
                capsule,
                scheduler: self.scheduler_handle.clone(),
            }),
        })
    }

    pub(crate) fn unload(&self, key: ExtensionKey) -> Result<(), RuntimePoolError> {
        {
            let runtimes = self.finalizer.runtimes.lock().unwrap();
            let Some(capsule) = runtimes.get(&key.extension) else {
                return Err(SchedulerError::UnknownExtension(key.extension).into());
            };
            if capsule.key() != key {
                return Err(SchedulerError::StaleLifecycle {
                    current: capsule.key().lifecycle,
                    received: key.lifecycle,
                }
                .into());
            }
        }
        self.scheduler_handle.stop(key)?;
        self.finalizer.finalize(
            key,
            LifecycleEnd::Unloaded,
            RuntimeError::fatal(RuntimeErrorKind::Cancelled, "extension unloaded"),
            true,
        );
        Ok(())
    }

    pub(crate) fn shutdown(mut self) {
        self.shutdown_inner();
    }

    fn capsule(&self, key: ExtensionKey) -> Result<Arc<RuntimeCapsule>, RuntimePoolError> {
        let runtimes = self.finalizer.runtimes.lock().unwrap();
        let Some(capsule) = runtimes.get(&key.extension) else {
            return Err(SchedulerError::UnknownExtension(key.extension).into());
        };
        if capsule.key().lifecycle != key.lifecycle {
            return Err(SchedulerError::StaleLifecycle {
                current: capsule.key().lifecycle,
                received: key.lifecycle,
            }
            .into());
        }
        Ok(Arc::clone(capsule))
    }

    fn shutdown_inner(&mut self) {
        let capsules = self.finalizer.capsules();
        for capsule in &capsules {
            let _ = capsule.termination_handle().terminate();
        }
        if let Some(scheduler) = self.scheduler.take() {
            scheduler.shutdown();
        }
        for capsule in capsules {
            let key = capsule.key();
            self.finalizer.finalize(
                key,
                LifecycleEnd::Shutdown,
                RuntimeError::fatal(RuntimeErrorKind::Cancelled, "extension pool shut down"),
                false,
            );
        }
    }
}

impl Drop for RuntimePool {
    fn drop(&mut self) {
        self.shutdown_inner();
    }
}

fn initialize_extension_context(
    scope: &mut v8::PinScope<'_, '_>,
    state: &Arc<RuntimeLocalState>,
) -> Result<(), RuntimeError> {
    let bindings_name =
        v8::String::new(scope, NATIVE_BINDINGS_GLOBAL).ok_or_else(RuntimeError::disposed)?;
    let bindings = v8::Object::new(scope);
    let request_name = v8::String::new(scope, "request").ok_or_else(RuntimeError::disposed)?;
    let request =
        v8::Function::new(scope, host_request_callback).ok_or_else(RuntimeError::disposed)?;
    let keybinding_name =
        v8::String::new(scope, "keybinding").ok_or_else(RuntimeError::disposed)?;
    let keybinding =
        v8::Function::new(scope, keybinding_callback).ok_or_else(RuntimeError::disposed)?;
    let listener_failure_name = v8::String::new(scope, "reportCancellationListenerFailure")
        .ok_or_else(RuntimeError::disposed)?;
    let listener_failure = v8::Function::new(scope, cancellation_listener_failure_callback)
        .ok_or_else(RuntimeError::disposed)?;
    if bindings.set(scope, request_name.into(), request.into()) != Some(true)
        || bindings.set(scope, keybinding_name.into(), keybinding.into()) != Some(true)
        || bindings.set(scope, listener_failure_name.into(), listener_failure.into()) != Some(true)
        || scope.get_current_context().global(scope).set(
            scope,
            bindings_name.into(),
            bindings.into(),
        ) != Some(true)
    {
        return Err(RuntimeError::fatal(
            RuntimeErrorKind::Engine,
            "could not install private native bindings",
        ));
    }

    evaluate_fixture_module(scope, state, PUBLIC_FACADE_SPECIFIER, None, true)?;
    let global = scope.get_current_context().global(scope);
    let mut callbacks = HashMap::new();
    for (kind, name) in [
        (RuntimeCallback::BufferChange, "__knotDispatchBufferChange"),
        (
            RuntimeCallback::ApplicationChange,
            "__knotDispatchApplicationChange",
        ),
        (RuntimeCallback::Command, "__knotInvokeCommand"),
        (RuntimeCallback::CancelCommand, "__knotCancelCommand"),
        (RuntimeCallback::Tree, "__knotRequestTreeChildren"),
        (RuntimeCallback::Completion, "__knotRequestCompletions"),
    ] {
        let name = v8::String::new(scope, name).ok_or_else(RuntimeError::disposed)?;
        let function = global
            .get(scope, name.into())
            .ok_or_else(RuntimeError::disposed)?;
        let function = v8::Local::<v8::Function>::try_from(function).map_err(|_| {
            RuntimeError::fatal(
                RuntimeErrorKind::Engine,
                "private runtime callback is missing",
            )
        })?;
        callbacks.insert(kind, v8::Global::new(scope, function));
        if global.delete(scope, name.into()) != Some(true) {
            return Err(RuntimeError::fatal(
                RuntimeErrorKind::Engine,
                "private runtime callback could not be hidden",
            ));
        }
    }
    *state.callbacks.lock().unwrap() = callbacks;
    Ok(())
}

fn validate_root_module_specifier(specifier: &str) -> Result<String, RuntimeError> {
    let parsed = Url::parse(specifier).map_err(|_| RuntimeError {
        kind: RuntimeErrorKind::InvalidModuleSpecifier,
        message: format!("invalid fixture module specifier: {specifier}").into(),
        source: Some(specifier.into()),
        line: None,
        column: None,
        stack: None,
    })?;
    let canonical = parsed.to_string();
    if canonical == PRIVATE_BOOTSTRAP_SPECIFIER || canonical == PUBLIC_FACADE_SPECIFIER {
        return Err(RuntimeError {
            kind: RuntimeErrorKind::InvalidModuleSpecifier,
            message: format!("fixture module specifier is reserved: {canonical}").into(),
            source: Some(canonical.into()),
            line: None,
            column: None,
            stack: None,
        });
    }
    Ok(canonical)
}

fn evaluate_fixture_module(
    scope: &mut v8::PinScope<'_, '_>,
    state: &Arc<RuntimeLocalState>,
    specifier: &str,
    source: Option<Arc<str>>,
    internal: bool,
) -> Result<String, RuntimeError> {
    if !internal && specifier == PRIVATE_BOOTSTRAP_SPECIFIER {
        return Err(module_resolution_error(
            specifier,
            "Knot private bootstrap bindings are not importable by extensions",
        ));
    }
    if let Some(source) = source {
        let mut modules = state.modules.lock().unwrap();
        if let Some(existing) = modules.sources.get(specifier) {
            if existing.as_ref() != source.as_ref() {
                return Err(module_resolution_error(
                    specifier,
                    "fixture module source cannot change after registration",
                ));
            }
        } else {
            modules.sources.insert(specifier.to_owned(), source);
        }
    }

    v8::tc_scope!(let try_catch, scope);
    let (module_count, resolution_count) = {
        let modules = state.modules.lock().unwrap();
        (modules.modules.len(), modules.resolutions.len())
    };
    let compiled = {
        let mut modules = state.modules.lock().unwrap();
        compile_module_graph(try_catch, &mut modules, specifier, None)
    };
    let compiled = match compiled {
        Ok(compiled) => compiled,
        Err(failure) => {
            let error = failure.report(try_catch, specifier);
            rollback_module_graph(state, module_count, resolution_count);
            return Err(error);
        }
    };
    let module = v8::Local::new(try_catch, &compiled);
    match module.instantiate_module(try_catch, resolve_module_callback) {
        Some(true) => {}
        Some(false) | None => {
            let error = runtime_error_from_try_catch(
                try_catch,
                RuntimeErrorKind::ModuleResolution,
                specifier,
            );
            rollback_module_graph(state, module_count, resolution_count);
            return Err(error);
        }
    }
    let Some(value) = module.evaluate(try_catch) else {
        return Err(runtime_error_from_try_catch(
            try_catch,
            RuntimeErrorKind::Exception,
            specifier,
        ));
    };
    let Ok(promise) = v8::Local::<v8::Promise>::try_from(value) else {
        return Err(RuntimeError::fatal(
            RuntimeErrorKind::Engine,
            "V8 module evaluation did not return a promise",
        ));
    };
    promise.mark_as_handled();
    try_catch.perform_microtask_checkpoint();
    match promise.state() {
        v8::PromiseState::Fulfilled => {
            let value = promise.result(try_catch).to_rust_string_lossy(try_catch);
            let mut rejections = state.rejections.lock().unwrap();
            if let Some(rejection) = rejections.first() {
                let error = RuntimeError {
                    kind: RuntimeErrorKind::Rejection,
                    message: rejection.message.clone().into(),
                    source: rejection.source.clone().map(Into::into),
                    line: rejection.line,
                    column: rejection.column,
                    stack: rejection.stack.clone().map(Into::into),
                };
                rejections.clear();
                Err(error)
            } else {
                Ok(value)
            }
        }
        v8::PromiseState::Rejected => {
            let rejection = promise.result(try_catch);
            let message = v8::Exception::create_message(try_catch, rejection);
            let error = RuntimeError {
                kind: RuntimeErrorKind::Rejection,
                message: message
                    .get(try_catch)
                    .to_rust_string_lossy(try_catch)
                    .into(),
                source: message
                    .get_script_resource_name(try_catch)
                    .and_then(|value| value.to_string(try_catch))
                    .map(|value| value.to_rust_string_lossy(try_catch).into())
                    .or_else(|| Some(specifier.into())),
                line: message
                    .get_line_number(try_catch)
                    .and_then(|line| u32::try_from(line).ok()),
                column: one_based_coordinate(message.get_start_column()),
                stack: None,
            };
            state.rejections.lock().unwrap().clear();
            Err(error)
        }
        v8::PromiseState::Pending => Err(RuntimeError {
            kind: RuntimeErrorKind::Rejection,
            message: "fixture module evaluation remained pending without host work".into(),
            source: Some(specifier.into()),
            line: None,
            column: None,
            stack: None,
        }),
    }
}

fn rollback_module_graph(state: &RuntimeLocalState, modules: usize, resolutions: usize) {
    let mut registry = state.modules.lock().unwrap();
    registry.resolutions.truncate(resolutions);
    registry.modules.truncate(modules);
}

fn clear_module_registry(state: &RuntimeLocalState) {
    let mut registry = state
        .modules
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    registry.resolutions.clear();
    registry.modules.clear();
    registry.sources.clear();
}

/// Check syntax and static imports without evaluating user code. The caller
/// supplies an immutable graph and runs this away from the foreground thread.
pub(crate) fn validate_module_graph_entry(
    graph: &ModuleGraph,
    entry: &str,
) -> Result<(), RuntimeError> {
    let entry = graph.entry_url(entry).ok_or_else(|| {
        RuntimeError::fatal(
            RuntimeErrorKind::InvalidModuleSpecifier,
            format!("module graph entry is unavailable: {entry}"),
        )
    })?;
    initialize();
    let mut isolate = v8::Isolate::new(Default::default());
    let scope = pin!(v8::HandleScope::new(&mut isolate));
    let mut scope = scope.init();
    let context = v8::Context::new(&scope, Default::default());
    let scope = &mut v8::ContextScope::new(&mut scope, context);
    v8::tc_scope!(let try_catch, scope);
    let mut modules = ModuleRegistry::new(Some(graph));
    let blocked = blocked_post_init_import(&modules, entry);
    compile_module_graph(try_catch, &mut modules, entry, blocked.as_deref())
        .map(|_| ())
        .map_err(|failure| failure.report(try_catch, entry))
}

fn blocked_post_init_import(registry: &ModuleRegistry, entry: &str) -> Option<Arc<str>> {
    (registry
        .entries
        .get(ConfigPhase::PreInit.entry())
        .map(AsRef::as_ref)
        == Some(entry))
    .then(|| registry.entries.get(ConfigPhase::PostInit.entry()).cloned())
    .flatten()
}

fn compile_module_graph(
    scope: &mut v8::PinScope<'_, '_>,
    registry: &mut ModuleRegistry,
    specifier: &str,
    blocked_import: Option<&str>,
) -> Result<v8::Global<v8::Module>, ModuleCompileFailure> {
    if let Some(record) = registry
        .modules
        .iter()
        .find(|record| record.specifier == specifier)
    {
        return Ok(record.module.clone());
    }
    let Some(source) = registry.sources.get(specifier).cloned() else {
        throw_module_error(
            scope,
            &if registry.package_root.is_some() {
                format!("Knot package module not found: {specifier}")
            } else {
                format!("Knot fixture module not found: {specifier}")
            },
        );
        return Err(RuntimeErrorKind::ModuleResolution.into());
    };
    let source_text = v8::String::new(scope, &source).ok_or(RuntimeErrorKind::Engine)?;
    let resource_name = v8::String::new(scope, specifier).ok_or(RuntimeErrorKind::Engine)?;
    let origin = v8::ScriptOrigin::new(
        scope,
        resource_name.into(),
        0,
        0,
        false,
        0,
        None,
        false,
        false,
        true,
        None,
    );
    let mut source = v8::script_compiler::Source::new(source_text, Some(&origin));
    let module = v8::script_compiler::compile_module(scope, &mut source)
        .ok_or(RuntimeErrorKind::Compilation)?;
    let global = v8::Global::new(scope, module);
    registry.modules.push(CompiledModule {
        specifier: specifier.to_owned(),
        module: global.clone(),
    });

    let requests = module.get_module_requests();
    for index in 0..requests.length() {
        let request = requests.get(scope, index).ok_or(RuntimeErrorKind::Engine)?;
        let request = v8::Local::<v8::ModuleRequest>::try_from(request)
            .map_err(|_| RuntimeErrorKind::Engine)?;
        let location = module.source_offset_to_location(request.get_source_offset());
        let request = request.get_specifier().to_rust_string_lossy(scope);
        let resolved =
            match resolve_module_specifier(&request, specifier, registry.package_root.as_deref()) {
                Ok(resolved) => resolved,
                Err(message) => {
                    throw_module_error(scope, &message);
                    return Err(ModuleCompileFailure::at_import(
                        RuntimeErrorKind::ModuleResolution,
                        specifier,
                        location,
                    ));
                }
            };
        if blocked_import == Some(resolved.as_str()) {
            throw_module_error(scope, "pre-init cannot import post-init.js");
            return Err(ModuleCompileFailure::at_import(
                RuntimeErrorKind::ModuleResolution,
                specifier,
                location,
            ));
        }
        if !registry.sources.contains_key(&resolved) {
            let kind = if registry.package_root.is_some() {
                "package"
            } else {
                "fixture"
            };
            throw_module_error(scope, &format!("Knot {kind} module not found: {resolved}"));
            return Err(ModuleCompileFailure::at_import(
                RuntimeErrorKind::ModuleResolution,
                specifier,
                location,
            ));
        }
        registry.resolutions.push(ModuleResolution {
            referrer: global.clone(),
            request,
            resolved: resolved.clone(),
        });
        compile_module_graph(scope, registry, &resolved, blocked_import)?;
    }
    Ok(global)
}

fn resolve_module_specifier(
    request: &str,
    referrer: &str,
    package_root: Option<&str>,
) -> Result<String, String> {
    if request == PRIVATE_BOOTSTRAP_SPECIFIER && referrer != PUBLIC_FACADE_SPECIFIER {
        return Err("Knot private bootstrap bindings are not importable by extensions".into());
    }
    if request == PRIVATE_BOOTSTRAP_SPECIFIER || request == PUBLIC_FACADE_SPECIFIER {
        return Ok(request.to_owned());
    }
    if let Some(package_root) = package_root {
        if referrer == PUBLIC_FACADE_SPECIFIER
            || (!request.starts_with("./") && !request.starts_with("../"))
        {
            return Err(format!(
                "extension import must be relative or knot: {request}"
            ));
        }
        let resolved = Url::parse(referrer)
            .and_then(|base| base.join(request))
            .map_err(|_| format!("invalid extension import: {request}"))?;
        if !resolved.as_str().starts_with(package_root)
            || resolved.query().is_some()
            || resolved.fragment().is_some()
        {
            return Err(format!("extension import escapes package: {request}"));
        }
        return Ok(resolved.to_string());
    }
    if let Ok(absolute) = Url::parse(request) {
        return Ok(absolute.to_string());
    }
    if !request.starts_with("./") && !request.starts_with("../") && !request.starts_with('/') {
        return Err(format!("invalid fixture module specifier: {request}"));
    }
    Url::parse(referrer)
        .and_then(|base| base.join(request))
        .map(|url| url.to_string())
        .map_err(|_| format!("invalid fixture module specifier: {request}"))
}

fn module_resolution_error(source: &str, message: &str) -> RuntimeError {
    RuntimeError {
        kind: RuntimeErrorKind::ModuleResolution,
        message: message.into(),
        source: Some(source.into()),
        line: None,
        column: None,
        stack: None,
    }
}

fn throw_module_error(scope: &mut v8::PinScope<'_, '_>, message: &str) {
    if let Some(message) = v8::String::new(scope, message) {
        let exception = v8::Exception::type_error(scope, message);
        scope.throw_exception(exception);
    }
}

fn reject_dynamic_import<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    _options: v8::Local<'s, v8::Data>,
    _resource_name: v8::Local<'s, v8::Value>,
    _specifier: v8::Local<'s, v8::String>,
    _attributes: v8::Local<'s, v8::FixedArray>,
) -> Option<v8::Local<'s, v8::Promise>> {
    let resolver = v8::PromiseResolver::new(scope)?;
    let message = v8::String::new(scope, "dynamic imports are unavailable to extensions")?;
    let error = v8::Exception::type_error(scope, message);
    resolver.reject(scope, error);
    Some(resolver.get_promise(scope))
}

fn resolve_module_callback<'s>(
    context: v8::Local<'s, v8::Context>,
    specifier: v8::Local<'s, v8::String>,
    _import_attributes: v8::Local<'s, v8::FixedArray>,
    referrer: v8::Local<'s, v8::Module>,
) -> Option<v8::Local<'s, v8::Module>> {
    v8::callback_scope!(unsafe scope, context);
    let state = scope.get_slot::<Arc<RuntimeLocalState>>()?.clone();
    let request = specifier.to_rust_string_lossy(scope);
    let registry = state.modules.lock().ok()?;
    let resolution = registry.resolutions.iter().find(|resolution| {
        resolution.request == request && v8::Local::new(scope, &resolution.referrer) == referrer
    });
    let Some(resolution) = resolution else {
        throw_module_error(scope, &format!("unresolved module import: {request}"));
        return None;
    };
    registry
        .modules
        .iter()
        .find(|record| record.specifier == resolution.resolved)
        .map(|record| v8::Local::new(scope, &record.module))
}

#[derive(Clone, Copy, Debug)]
enum BoundaryError {
    Type(&'static str),
    Range(&'static str),
    Host(TransportError),
}

impl From<&'static str> for BoundaryError {
    fn from(message: &'static str) -> Self {
        Self::Type(message)
    }
}

impl fmt::Display for BoundaryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Type(message) | Self::Range(message) => formatter.write_str(message),
            Self::Host(error) => formatter.write_str(error.code()),
        }
    }
}

fn boundary_exception<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    error: BoundaryError,
) -> v8::Local<'s, v8::Value> {
    let message = v8::String::new(scope, &error.to_string()).expect("V8 error message");
    match error {
        BoundaryError::Type(_) => v8::Exception::type_error(scope, message),
        BoundaryError::Range(_) => v8::Exception::range_error(scope, message),
        BoundaryError::Host(code) => {
            let exception = v8::Exception::error(scope, message);
            let object = v8::Local::<v8::Object>::try_from(exception).expect("V8 Error object");
            let name = v8::String::new(scope, "KnotHostError").unwrap();
            let code = v8::String::new(scope, code.code()).unwrap();
            set_property(scope, object, "name", name.into()).expect("V8 error name");
            set_property(scope, object, "code", code.into()).expect("V8 error code");
            exception
        }
    }
}

fn host_request_callback<'s, 'i>(
    scope: &mut v8::PinnedRef<'s, v8::HandleScope<'i>>,
    arguments: v8::FunctionCallbackArguments<'s>,
    mut result: v8::ReturnValue,
) {
    #[cfg(test)]
    let configured_error = scope
        .get_slot::<Arc<RuntimeLocalState>>()
        .and_then(|state| state.host_error.lock().ok()?.take());
    #[cfg(not(test))]
    let configured_error: Option<HostFailure> = None;
    let resolver = v8::PromiseResolver::new(scope).unwrap();
    if let Some(error) = configured_error {
        let exception = host_failure_exception(scope, &error);
        resolver.reject(scope, exception);
        result.set(resolver.get_promise(scope).into());
        return;
    }
    let Some(state) = scope.get_slot::<Arc<RuntimeLocalState>>().cloned() else {
        let exception = boundary_exception(
            scope,
            BoundaryError::Host(TransportError::UnsupportedOperation),
        );
        resolver.reject(scope, exception);
        result.set(resolver.get_promise(scope).into());
        return;
    };
    if state.status.load(Ordering::Acquire) != ACTIVE {
        let exception = boundary_exception(scope, BoundaryError::Host(TransportError::Cancelled));
        resolver.reject(scope, exception);
        result.set(resolver.get_promise(scope).into());
        return;
    }
    let invocation = match optional_u64(scope, arguments.get(0), "UnsupportedOperation") {
        Ok(value) => value.map(CommandInvocationId::new),
        Err(error) => {
            let exception = boundary_exception(scope, error);
            resolver.reject(scope, exception);
            result.set(resolver.get_promise(scope).into());
            return;
        }
    };
    let parsed = parse_host_operation(scope, &arguments);
    let sender = state.event_sender.lock().unwrap().clone();
    let root = *state.current_root.lock().unwrap();
    let (operation, kind) = match parsed {
        Ok(parsed) => parsed,
        Err(error) => {
            let exception = boundary_exception(scope, error);
            resolver.reject(scope, exception);
            result.set(resolver.get_promise(scope).into());
            return;
        }
    };
    let (Some(sender), Some(root)) = (sender, root) else {
        let exception = boundary_exception(
            scope,
            BoundaryError::Host(TransportError::UnsupportedOperation),
        );
        resolver.reject(scope, exception);
        result.set(resolver.get_promise(scope).into());
        return;
    };
    let Ok(request_value) =
        state
            .next_request
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(1)
            })
    else {
        let exception = boundary_exception(
            scope,
            BoundaryError::Host(TransportError::UnsupportedOperation),
        );
        resolver.reject(scope, exception);
        result.set(resolver.get_promise(scope).into());
        return;
    };
    let id = RequestId::new(request_value);
    state.pending.lock().unwrap().insert(
        id,
        PendingRequest {
            root,
            invocation,
            operation: kind,
            resolver: Some(v8::Global::new(scope, resolver)),
            response_queued: false,
            provider_mutation: operation.provider_mutation(),
        },
    );
    if sender
        .send(EngineEvent::Request(HostRequest {
            extension: state.key.extension,
            lifecycle: state.key.lifecycle,
            id,
            invocation,
            operation,
        }))
        .is_err()
    {
        state.pending.lock().unwrap().remove(&id);
        let exception = boundary_exception(scope, BoundaryError::Host(TransportError::Cancelled));
        resolver.reject(scope, exception);
    }
    result.set(resolver.get_promise(scope).into());
}

fn cancellation_listener_failure_callback<'s, 'i>(
    scope: &mut v8::PinnedRef<'s, v8::HandleScope<'i>>,
    arguments: v8::FunctionCallbackArguments<'s>,
    _result: v8::ReturnValue,
) {
    if arguments.get(0).is_string() {
        eprintln!(
            "[knot] cancellation listener failed: {}",
            arguments.get(0).to_rust_string_lossy(scope)
        );
    }
}

fn keybinding_callback<'s, 'i>(
    scope: &mut v8::PinnedRef<'s, v8::HandleScope<'i>>,
    arguments: v8::FunctionCallbackArguments<'s>,
    _result: v8::ReturnValue,
) {
    let Some(state) = scope.get_slot::<Arc<RuntimeLocalState>>().cloned() else {
        return;
    };
    let mutation = parse_keybinding_mutation(scope, &arguments)
        .unwrap_or_else(|message| KeybindingMutation::Invalid(message.to_string()));
    state.keybinding_mutations.lock().unwrap().push(mutation);
}

fn parse_keybinding_mutation<'s, 'i>(
    scope: &mut v8::PinnedRef<'s, v8::HandleScope<'i>>,
    arguments: &v8::FunctionCallbackArguments<'s>,
) -> Result<KeybindingMutation, BoundaryError> {
    let operation = required_string(scope, arguments.get(0), "invalid keybinding operation")?;
    let key = required_string(scope, arguments.get(1), "key must be a string")?;
    if operation == "invalid" {
        return Ok(KeybindingMutation::Invalid(key));
    }
    let options = arguments.get(3);
    let view = if options.is_null_or_undefined() {
        None
    } else {
        if options.is_array() || options.is_function() {
            return Err("options must be an object".into());
        }
        let object =
            v8::Local::<v8::Object>::try_from(options).map_err(|_| "options must be an object")?;
        let view = property(scope, object, "view", "invalid view selector")?;
        optional_string(scope, view, "view must be a string")?
    };
    match operation.as_str() {
        "remove" => Ok(KeybindingMutation::Remove { key, view }),
        "set" => {
            let value = arguments.get(2);
            let command = if value.is_null() {
                None
            } else if value.is_string() {
                Some(Command {
                    name: required_string(scope, value, "command must be a string or object")?
                        .into(),
                    arguments: CommandArgumentValue::Null,
                })
            } else {
                if value.is_array() || value.is_function() {
                    return Err("command must be a string or object".into());
                }
                let object = v8::Local::<v8::Object>::try_from(value)
                    .map_err(|_| "command must be a string or object")?;
                let name = property(scope, object, "name", "invalid command name")?;
                let name = required_string(scope, name, "command name must be a string")?;
                let arguments_value =
                    property(scope, object, "arguments", "invalid command arguments")?;
                let command_arguments = if arguments_value.is_undefined() {
                    CommandArgumentValue::Null
                } else {
                    command_argument_from_v8(scope, arguments_value)
                        .map_err(|_| "command arguments must be JSON-compatible")?
                };
                Some(Command {
                    name: name.into(),
                    arguments: command_arguments,
                })
            };
            Ok(KeybindingMutation::Set { key, view, command })
        }
        _ => Err("invalid keybinding operation".into()),
    }
}

fn document_destination_from_v8<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: v8::Local<'s, v8::Value>,
) -> Result<Option<super::protocol::DocumentDestination>, BoundaryError> {
    use super::protocol::{DocumentDestination, PaneTarget, PresentationTarget, WindowTarget};
    if value.is_null_or_undefined() {
        return Ok(None);
    }
    let object = v8::Local::<v8::Object>::try_from(value)
        .map_err(|_| BoundaryError::Type("invalid destination"))?;
    let window = WindowTarget {
        window: string_property(scope, object, "window")?,
    };
    let kind = string_property(scope, object, "kind")?;
    if kind == "window" {
        return Ok(Some(DocumentDestination::Window(window)));
    }
    let pane = PaneTarget {
        window,
        workbench: string_property(scope, object, "workbench")?,
        pane: string_property(scope, object, "pane")?,
    };
    match kind.as_str() {
        "pane" => Ok(Some(DocumentDestination::Pane(pane))),
        "view" => Ok(Some(DocumentDestination::View(PresentationTarget {
            pane,
            tab: string_property(scope, object, "tab")?,
            presentation: string_property(scope, object, "presentation")?,
        }))),
        _ => Err(BoundaryError::Type("invalid destination kind")),
    }
}

fn workspace_root_uris<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: v8::Local<'s, v8::Value>,
) -> Result<Vec<String>, BoundaryError> {
    let array = v8::Local::<v8::Array>::try_from(value)
        .map_err(|_| BoundaryError::Type("workspace roots must be an array"))?;
    let mut roots = Vec::with_capacity(array.length() as usize);
    for index in 0..array.length() {
        let uri = array
            .get_index(scope, index)
            .ok_or("invalid workspace root")?;
        roots.push(required_string(
            scope,
            uri,
            "workspace root URI must be text",
        )?);
    }
    Ok(roots)
}

struct DeclarationReader<'a, 's, 'i> {
    scope: &'a mut v8::PinnedRef<'s, v8::HandleScope<'i>>,
    arguments: &'a v8::FunctionCallbackArguments<'s>,
}

impl super::declarations::ArgumentReader for DeclarationReader<'_, '_, '_> {
    fn optional_integer(
        &mut self,
        index: usize,
    ) -> Result<Option<u64>, super::declarations::DecodeError> {
        optional_u64(
            self.scope,
            self.arguments.get(index as i32),
            "InvalidTarget",
        )
        .map_err(declaration_decode_error)
    }
    fn command_arguments(
        &mut self,
        index: usize,
    ) -> Result<CommandArgumentValue, super::declarations::DecodeError> {
        command_argument_from_v8(self.scope, self.arguments.get(index as i32))
            .map_err(declaration_decode_error)
    }
    fn outcome(
        &mut self,
        index: usize,
    ) -> Result<CommandOutcome, super::declarations::DecodeError> {
        command_outcome_from_v8(self.scope, self.arguments.get(index as i32))
            .map_err(|_| super::declarations::DecodeError::Type("UnsupportedOperation"))
    }

    fn range(
        &mut self,
        index: usize,
    ) -> Result<Option<ByteRange>, super::declarations::DecodeError> {
        optional_byte_range(self.scope, self.arguments.get(index as i32))
            .map_err(declaration_decode_error)
    }
    fn edits(&mut self, index: usize) -> Result<Vec<TextEdit>, super::declarations::DecodeError> {
        text_edits(self.scope, self.arguments.get(index as i32)).map_err(declaration_decode_error)
    }
    fn contributions(
        &mut self,
        index: usize,
    ) -> Result<Vec<EditorContribution>, super::declarations::DecodeError> {
        editor_contributions(self.scope, self.arguments.get(index as i32))
            .map_err(declaration_decode_error)
    }

    fn optional_text(
        &mut self,
        index: usize,
        message: &'static str,
    ) -> Result<Option<String>, super::declarations::DecodeError> {
        optional_string(self.scope, self.arguments.get(index as i32), message)
            .map_err(declaration_decode_error)
    }
    fn text_list(&mut self, index: usize) -> Result<Vec<String>, super::declarations::DecodeError> {
        workspace_root_uris(self.scope, self.arguments.get(index as i32))
            .map_err(declaration_decode_error)
    }

    fn integer(&mut self, index: usize) -> Result<u64, super::declarations::DecodeError> {
        argument_u64(self.scope, self.arguments, index as i32).map_err(declaration_decode_error)
    }

    fn text(
        &mut self,
        index: usize,
        message: &'static str,
    ) -> Result<String, super::declarations::DecodeError> {
        required_string(self.scope, self.arguments.get(index as i32), message)
            .map_err(declaration_decode_error)
    }
    fn destination(
        &mut self,
        index: usize,
    ) -> Result<Option<super::protocol::DocumentDestination>, super::declarations::DecodeError>
    {
        document_destination_from_v8(self.scope, self.arguments.get(index as i32))
            .map_err(declaration_decode_error)
    }
}

fn declaration_decode_error(error: BoundaryError) -> super::declarations::DecodeError {
    match error {
        BoundaryError::Type(message) => super::declarations::DecodeError::Type(message),
        BoundaryError::Range(message) => super::declarations::DecodeError::Range(message),
        BoundaryError::Host(_) => unreachable!("argument decoding returns author errors"),
    }
}

impl From<super::declarations::DecodeError> for BoundaryError {
    fn from(error: super::declarations::DecodeError) -> Self {
        match error {
            super::declarations::DecodeError::Type(message) => Self::Type(message),
            super::declarations::DecodeError::Range(message) => Self::Range(message),
            super::declarations::DecodeError::Host(error) => Self::Host(error),
        }
    }
}

fn parse_host_operation<'s, 'i>(
    scope: &mut v8::PinnedRef<'s, v8::HandleScope<'i>>,
    arguments: &v8::FunctionCallbackArguments<'s>,
) -> Result<(super::declarations::DeclaredRequest, &'static str), BoundaryError> {
    let operation = required_string(scope, arguments.get(1), "operation must be text")?;
    if let Some(descriptor) = super::declarations::registry().get(&operation) {
        let request = descriptor.decode(&mut DeclarationReader { scope, arguments })?;
        return Ok((request, descriptor.name));
    }
    Err(BoundaryError::Host(TransportError::UnsupportedOperation))
}

fn optional_u64(
    scope: &mut v8::PinnedRef<'_, v8::HandleScope>,
    value: v8::Local<'_, v8::Value>,
    error: &'static str,
) -> Result<Option<u64>, BoundaryError> {
    if value.is_null_or_undefined() {
        Ok(None)
    } else {
        value_u64(scope, value, error).map(Some)
    }
}

fn argument_u64(
    scope: &mut v8::PinnedRef<'_, v8::HandleScope>,
    arguments: &v8::FunctionCallbackArguments,
    index: i32,
) -> Result<u64, BoundaryError> {
    value_u64(scope, arguments.get(index), "UnsupportedOperation")
}

fn value_u64(
    scope: &mut v8::PinnedRef<'_, v8::HandleScope>,
    value: v8::Local<'_, v8::Value>,
    _error: &'static str,
) -> Result<u64, BoundaryError> {
    if !value.is_number() {
        return Err(BoundaryError::Type("expected a number"));
    }
    let number = value
        .number_value(scope)
        .ok_or(BoundaryError::Type("expected a number"))?;
    if !number.is_finite()
        || number < 0.0
        || number.fract() != 0.0
        || number > 9_007_199_254_740_991.0
    {
        return Err(BoundaryError::Range("expected a nonnegative safe integer"));
    }
    Ok(number as u64)
}

fn value_usize(
    scope: &mut v8::PinnedRef<'_, v8::HandleScope>,
    value: v8::Local<'_, v8::Value>,
    error: &'static str,
) -> Result<usize, BoundaryError> {
    usize::try_from(value_u64(scope, value, error)?)
        .map_err(|_| BoundaryError::Range("integer exceeds native coordinates"))
}

fn property<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<'s, v8::Object>,
    name: &str,
    error: &'static str,
) -> Result<v8::Local<'s, v8::Value>, BoundaryError> {
    let key = v8::String::new(scope, name).ok_or(error)?;
    object.get(scope, key.into()).ok_or(error.into())
}

fn required_string(
    scope: &mut v8::PinnedRef<'_, v8::HandleScope>,
    value: v8::Local<'_, v8::Value>,
    error: &'static str,
) -> Result<String, BoundaryError> {
    if !value.is_string() {
        return Err(error.into());
    }
    Ok(value.to_rust_string_lossy(scope))
}

fn optional_string(
    scope: &mut v8::PinnedRef<'_, v8::HandleScope>,
    value: v8::Local<'_, v8::Value>,
    error: &'static str,
) -> Result<Option<String>, BoundaryError> {
    if value.is_null_or_undefined() {
        Ok(None)
    } else {
        required_string(scope, value, error).map(Some)
    }
}

fn optional_byte_range<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: v8::Local<'s, v8::Value>,
) -> Result<Option<ByteRange>, BoundaryError> {
    if value.is_null_or_undefined() {
        return Ok(None);
    }
    byte_range(scope, value).map(Some)
}

fn byte_range<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: v8::Local<'s, v8::Value>,
) -> Result<ByteRange, BoundaryError> {
    let object = v8::Local::<v8::Object>::try_from(value).map_err(|_| "InvalidRange")?;
    let start = property(scope, object, "startByteOffset", "InvalidRange")?;
    let start_byte_offset = value_usize(scope, start, "InvalidRange")?;
    let end = property(scope, object, "endByteOffset", "InvalidRange")?;
    let end_byte_offset = value_usize(scope, end, "InvalidRange")?;
    if start_byte_offset > end_byte_offset {
        return Err(BoundaryError::Range("range endpoints must be ordered"));
    }
    Ok(ByteRange {
        start_byte_offset,
        end_byte_offset,
    })
}

fn text_edits(
    scope: &mut v8::PinnedRef<'_, v8::HandleScope>,
    value: v8::Local<'_, v8::Value>,
) -> Result<Vec<TextEdit>, BoundaryError> {
    let array = v8::Local::<v8::Array>::try_from(value).map_err(|_| "InvalidEditBatch")?;
    let mut edits = Vec::with_capacity(array.length() as usize);
    for index in 0..array.length() {
        let value = array.get_index(scope, index).ok_or("InvalidEditBatch")?;
        let object = v8::Local::<v8::Object>::try_from(value).map_err(|_| "InvalidEditBatch")?;
        let range = property(scope, object, "range", "InvalidEditBatch")?;
        let range = byte_range(scope, range)?;
        let text = property(scope, object, "text", "InvalidEditBatch")?;
        edits.push(TextEdit {
            range,
            text: required_string(scope, text, "InvalidEditBatch")?,
        });
    }
    Ok(edits)
}

fn editor_contributions(
    scope: &mut v8::PinnedRef<'_, v8::HandleScope>,
    value: v8::Local<'_, v8::Value>,
) -> Result<Vec<EditorContribution>, BoundaryError> {
    let error = "UnsupportedOperation";
    let array = v8::Local::<v8::Array>::try_from(value).map_err(|_| error)?;
    let mut contributions = Vec::with_capacity(array.length() as usize);
    for index in 0..array.length() {
        let value = array.get_index(scope, index).ok_or(error)?;
        let object = v8::Local::<v8::Object>::try_from(value).map_err(|_| error)?;
        let range_value = property(scope, object, "range", error)?;
        let range = byte_range(scope, range_value)?;
        let decoration_value = property(scope, object, "decoration", error)?;
        let decoration = optional_string(scope, decoration_value, error)?
            .map(|value| match value.as_str() {
                "info" => Ok(DecorationToken::Info),
                "warning" => Ok(DecorationToken::Warning),
                "error" => Ok(DecorationToken::Error),
                _ => Err(BoundaryError::Type(error)),
            })
            .transpose()?;
        let gutter_value = property(scope, object, "gutter", error)?;
        let gutter = optional_string(scope, gutter_value, error)?
            .map(|value| match value.as_str() {
                "info" => Ok(GutterToken::Info),
                "warning" => Ok(GutterToken::Warning),
                "error" => Ok(GutterToken::Error),
                _ => Err(BoundaryError::Type(error)),
            })
            .transpose()?;
        let command_value = property(scope, object, "command", error)?;
        let command = optional_string(scope, command_value, error)?;
        contributions.push(EditorContribution {
            range,
            decoration,
            gutter,
            command,
        });
    }
    Ok(contributions)
}

enum JsonFrame<'s> {
    Array {
        object: v8::Local<'s, v8::Object>,
        values: Vec<v8::Local<'s, v8::Value>>,
        next: usize,
        result: Vec<CommandArgumentValue>,
    },
    Object {
        object: v8::Local<'s, v8::Object>,
        keys: Vec<String>,
        values: Vec<v8::Local<'s, v8::Value>>,
        next: usize,
        result: std::collections::BTreeMap<String, CommandArgumentValue>,
    },
}

impl JsonFrame<'_> {
    fn object(&self) -> v8::Local<'_, v8::Object> {
        match self {
            Self::Array { object, .. } | Self::Object { object, .. } => *object,
        }
    }
}

fn command_argument_from_v8<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: v8::Local<'s, v8::Value>,
) -> Result<CommandArgumentValue, BoundaryError> {
    let mut frames: Vec<JsonFrame<'s>> = Vec::new();
    let mut current = value;
    loop {
        let mut decoded = if current.is_null() {
            CommandArgumentValue::Null
        } else if current.is_boolean() {
            CommandArgumentValue::Boolean(current.boolean_value(scope))
        } else if current.is_string() {
            CommandArgumentValue::String(current.to_rust_string_lossy(scope))
        } else if current.is_number() {
            let number = current
                .number_value(scope)
                .ok_or("expected a JSON number")?;
            if !number.is_finite() {
                return Err(BoundaryError::Type("JSON numbers must be finite"));
            }
            CommandArgumentValue::Number(number)
        } else {
            if current.is_function() || !current.is_object() {
                return Err(BoundaryError::Type(
                    "command arguments must be JSON-compatible",
                ));
            }
            let object =
                v8::Local::<v8::Object>::try_from(current).map_err(|_| "expected a JSON object")?;
            if frames
                .iter()
                .any(|frame| frame.object().strict_equals(object.into()))
            {
                return Err(BoundaryError::Type(
                    "command arguments cannot contain cycles",
                ));
            }
            let array = v8::Local::<v8::Array>::try_from(current).ok();
            let prototype = object
                .get_prototype(scope)
                .ok_or("invalid JSON prototype")?;
            let expected = if array.is_some() {
                v8::Array::new(scope, 0).get_prototype(scope)
            } else {
                v8::Object::new(scope).get_prototype(scope)
            }
            .ok_or("invalid JSON prototype")?;
            if !prototype.is_null() && !prototype.strict_equals(expected) {
                return Err(BoundaryError::Type(
                    "command arguments require plain JSON objects",
                ));
            }
            let to_json = property(scope, object, "toJSON", "invalid JSON conversion")?;
            if to_json.is_function() {
                return Err(BoundaryError::Type("custom JSON conversion is unavailable"));
            }
            let all_keys = object
                .get_own_property_names(
                    scope,
                    v8::GetPropertyNamesArgs {
                        property_filter: v8::PropertyFilter::ALL_PROPERTIES,
                        ..Default::default()
                    },
                )
                .ok_or("invalid JSON keys")?;
            for index in 0..all_keys.length() {
                let key = all_keys.get_index(scope, index).ok_or("invalid JSON key")?;
                if key.is_symbol() {
                    return Err(BoundaryError::Type("JSON keys must be strings"));
                }
                if let Some(array) = array {
                    if key.is_number() {
                        if value_u64(scope, key, "JSON index")? >= array.length() as u64 {
                            return Err(BoundaryError::Type("invalid JSON array index"));
                        }
                    } else {
                        let key = required_string(scope, key, "JSON keys must be strings")?;
                        if key != "length" && !(key == "toJSON" && to_json.is_undefined()) {
                            return Err(BoundaryError::Type(
                                "JSON arrays must contain only indexed data",
                            ));
                        }
                    }
                }
            }
            if let Some(array) = array {
                let mut values = Vec::with_capacity(array.length() as usize);
                for index in 0..array.length() {
                    let key =
                        v8::String::new(scope, &index.to_string()).ok_or("invalid JSON index")?;
                    if object.has_own_property(scope, key.into()) != Some(true) {
                        return Err(BoundaryError::Type("command arrays must be dense"));
                    }
                    values.push(
                        array
                            .get_index(scope, index)
                            .ok_or("invalid JSON array item")?,
                    );
                }
                if values.is_empty() {
                    CommandArgumentValue::Array(Vec::new())
                } else {
                    current = values[0];
                    frames.push(JsonFrame::Array {
                        object,
                        result: Vec::with_capacity(values.len()),
                        values,
                        next: 0,
                    });
                    continue;
                }
            } else {
                let names = object
                    .get_own_property_names(
                        scope,
                        v8::GetPropertyNamesArgs {
                            key_conversion: v8::KeyConversionMode::ConvertToString,
                            ..Default::default()
                        },
                    )
                    .ok_or("invalid JSON keys")?;
                let mut keys = Vec::with_capacity(names.length() as usize);
                let mut values = Vec::with_capacity(names.length() as usize);
                for index in 0..names.length() {
                    let key = names.get_index(scope, index).ok_or("invalid JSON key")?;
                    keys.push(required_string(scope, key, "JSON keys must be strings")?);
                    values.push(object.get(scope, key).ok_or("invalid JSON value")?);
                }
                if values.is_empty() {
                    CommandArgumentValue::Object(Default::default())
                } else {
                    current = values[0];
                    frames.push(JsonFrame::Object {
                        object,
                        keys,
                        values,
                        next: 0,
                        result: Default::default(),
                    });
                    continue;
                }
            }
        };
        loop {
            let Some(frame) = frames.last_mut() else {
                return Ok(decoded);
            };
            let next_value = match frame {
                JsonFrame::Array {
                    result,
                    values,
                    next,
                    ..
                } => {
                    result.push(decoded);
                    *next += 1;
                    values.get(*next).copied()
                }
                JsonFrame::Object {
                    result,
                    keys,
                    values,
                    next,
                    ..
                } => {
                    result.insert(keys[*next].clone(), decoded);
                    *next += 1;
                    values.get(*next).copied()
                }
            };
            if let Some(value) = next_value {
                current = value;
                break;
            }
            decoded = match frames.pop().unwrap() {
                JsonFrame::Array { result, .. } => CommandArgumentValue::Array(result),
                JsonFrame::Object { result, .. } => CommandArgumentValue::Object(result),
            };
        }
    }
}

fn command_argument_to_v8<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: &CommandArgumentValue,
) -> Result<v8::Local<'s, v8::Value>, &'static str> {
    if !command_argument_is_finite(value) {
        return Err("UnsupportedOperation");
    }
    let json = serde_json::to_string(value).map_err(|_| "UnsupportedOperation")?;
    let json = v8::String::new(scope, &json).ok_or("UnsupportedOperation")?;
    v8::json::parse(scope, json).ok_or("UnsupportedOperation")
}

fn command_argument_is_finite(value: &CommandArgumentValue) -> bool {
    match value {
        CommandArgumentValue::Number(value) => value.is_finite(),
        CommandArgumentValue::Array(values) => values.iter().all(command_argument_is_finite),
        CommandArgumentValue::Object(values) => values.values().all(command_argument_is_finite),
        CommandArgumentValue::Null
        | CommandArgumentValue::Boolean(_)
        | CommandArgumentValue::String(_) => true,
    }
}

fn command_outcome_from_v8<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: v8::Local<'s, v8::Value>,
) -> Result<CommandOutcome, RuntimeError> {
    let decoded = (|| -> Result<CommandOutcome, BoundaryError> {
        let object = v8::Local::<v8::Object>::try_from(value)
            .map_err(|_| "command outcome must be an object")?;
        let kind = string_property(scope, object, "kind")?;
        match kind.as_str() {
            "completed" => Ok(CommandOutcome::Completed),
            "unavailable" => Ok(CommandOutcome::Unavailable),
            "invalidTarget" => Ok(CommandOutcome::InvalidTarget),
            "cancelled" => Ok(CommandOutcome::Cancelled),
            "invalidArgument" => Ok(CommandOutcome::InvalidArgument {
                message: string_property(scope, object, "message")?,
            }),
            "handlerFailure" => Ok(CommandOutcome::HandlerFailure {
                message: string_property(scope, object, "message")?,
            }),
            _ => Err(BoundaryError::Type("invalid command outcome kind")),
        }
    })();
    decoded.map_err(|error| {
        RuntimeError::fatal(
            RuntimeErrorKind::Engine,
            format!("invalid command outcome from JavaScript: {error}"),
        )
    })
}

fn string_property<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<'s, v8::Object>,
    name: &str,
) -> Result<String, BoundaryError> {
    let value = property(scope, object, name, "invalid text property")?;
    required_string(scope, value, "property must be text")
}

fn optional_text_property<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<'s, v8::Object>,
    name: &str,
) -> Result<Option<String>, BoundaryError> {
    let value = property(scope, object, name, "invalid optional text property")?;
    optional_string(scope, value, "property must be text or null")
}

fn provider_items_from_v8<'s, T>(
    scope: &mut v8::PinScope<'s, '_>,
    value: v8::Local<'s, v8::Value>,
    mut convert: impl FnMut(
        &mut v8::PinScope<'s, '_>,
        v8::Local<'s, v8::Value>,
    ) -> Result<T, BoundaryError>,
) -> Result<Vec<T>, String> {
    let object = v8::Local::<v8::Object>::try_from(value)
        .map_err(|_| "semantic provider result must be an object".to_owned())?;
    let error_key =
        v8::String::new(scope, "error").ok_or_else(|| "invalid provider result".to_owned())?;
    if object.has_own_property(scope, error_key.into()) == Some(true)
        && let Some(error) =
            optional_text_property(scope, object, "error").map_err(|error| error.to_string())?
    {
        return Err(error);
    }
    let values = property(scope, object, "items", "provider result is missing items")
        .map_err(|error| error.to_string())?;
    let values = v8::Local::<v8::Array>::try_from(values)
        .map_err(|_| "semantic provider items must be an array".to_owned())?;
    let mut items = Vec::with_capacity(values.length() as usize);
    for index in 0..values.length() {
        let value = values
            .get_index(scope, index)
            .ok_or_else(|| "invalid provider item".to_owned())?;
        items.push(convert(scope, value).map_err(|error| error.to_string())?);
    }
    Ok(items)
}

fn tree_item_from_v8<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: v8::Local<'s, v8::Value>,
) -> Result<TreeItem, BoundaryError> {
    let object =
        v8::Local::<v8::Object>::try_from(value).map_err(|_| "tree item must be an object")?;
    let collapsible_state = match string_property(scope, object, "collapsibleState")?.as_str() {
        "none" => TreeCollapsibleState::None,
        "collapsed" => TreeCollapsibleState::Collapsed,
        "expanded" => TreeCollapsibleState::Expanded,
        _ => return Err(BoundaryError::Type("invalid tree collapsible state")),
    };
    let icon = optional_text_property(scope, object, "icon")?
        .map(|icon| match icon.as_str() {
            "file" => Ok(TreeIcon::File),
            "folder" => Ok(TreeIcon::Folder),
            "symbol" => Ok(TreeIcon::Symbol),
            _ => Err(BoundaryError::Type("invalid tree icon")),
        })
        .transpose()?;
    Ok(TreeItem {
        id: string_property(scope, object, "id")?,
        label: string_property(scope, object, "label")?,
        description: optional_text_property(scope, object, "description")?,
        collapsible_state,
        icon,
        command: optional_text_property(scope, object, "command")?,
    })
}

fn tree_response_from_v8<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    request: TreeChildrenRequest,
    value: v8::Local<'s, v8::Value>,
) -> TreeChildrenResponse {
    TreeChildrenResponse {
        registration: request.registration,
        window: request.window,
        view: request.view,
        parent_id: request.parent_id,
        generation: request.generation,
        result: provider_items_from_v8(scope, value, tree_item_from_v8)
            .map_err(|message| TreeProviderError { message }),
    }
}

fn completion_response_from_v8<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    request: CompletionRequest,
    value: v8::Local<'s, v8::Value>,
) -> CompletionResponse {
    CompletionResponse {
        registration: request.registration,
        revision: request.revision,
        generation: request.generation,
        result: provider_items_from_v8(scope, value, |scope, value| {
            let object = v8::Local::<v8::Object>::try_from(value)
                .map_err(|_| "completion item must be an object")?;
            Ok(CompletionResultItem {
                label: string_property(scope, object, "label")?,
                insert_text: string_property(scope, object, "insertText")?,
            })
        })
        .map_err(|message| CompletionProviderError { message }),
    }
}

const MAX_JS_INTEGER: u64 = 9_007_199_254_740_991;

fn exact_number<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: u64,
) -> Result<v8::Local<'s, v8::Value>, &'static str> {
    if value > MAX_JS_INTEGER {
        return Err("native integer cannot be represented exactly");
    }
    Ok(v8::Number::new(scope, value as f64).into())
}

fn application_document_is_exact(info: &super::protocol::DocumentInfo) -> bool {
    info.revision <= MAX_JS_INTEGER && info.buffer.value() <= MAX_JS_INTEGER
}
fn application_window_is_exact(info: &super::protocol::WindowInfo) -> bool {
    info.workspace.generation <= MAX_JS_INTEGER
        && info.panes.iter().flat_map(|pane| &pane.tabs).all(|tab| {
            tab.buffer
                .is_none_or(|buffer| buffer.value() <= MAX_JS_INTEGER)
        })
}
fn application_event_to_v8<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    event: &ApplicationEvent,
) -> Result<v8::Local<'s, v8::Value>, &'static str> {
    let exact = event.sequence <= MAX_JS_INTEGER
        && match &event.change {
            ApplicationChange::DocumentOpened { document }
            | ApplicationChange::DocumentChanged { document } => {
                application_document_is_exact(document)
            }
            ApplicationChange::WindowOpened { window }
            | ApplicationChange::PresentationsChanged { window } => {
                application_window_is_exact(window)
            }
            ApplicationChange::WorkspaceChanged { workspace } => {
                workspace.generation <= MAX_JS_INTEGER
            }
            _ => true,
        };
    if !exact {
        return Err("native application integer cannot be represented exactly");
    }
    let json = serde_json::to_string(event).map_err(|_| "UnsupportedOperation")?;
    let json = v8::String::new(scope, &json).ok_or("UnsupportedOperation")?;
    v8::json::parse(scope, json).ok_or("UnsupportedOperation")
}

fn host_failure_exception<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    error: &super::declarations::HostFailure,
) -> v8::Local<'s, v8::Value> {
    if let Err(error) = super::declarations::encode_json(&error.details) {
        return boundary_exception(scope, error.into());
    }

    let message = v8::String::new(scope, &error.message).unwrap();
    let exception = v8::Exception::error(scope, message);
    let object = v8::Local::<v8::Object>::try_from(exception).unwrap();
    let name = v8::String::new(scope, "KnotHostError").unwrap();
    let code = v8::String::new(scope, error.code).unwrap();
    set_property(scope, object, "name", name.into()).expect("error name");
    set_property(scope, object, "code", code.into()).expect("error code");
    let json = serde_json::to_string(&error.details).unwrap();
    let json = v8::String::new(scope, &json).unwrap();
    let details = v8::json::parse(scope, json).unwrap();
    set_property(scope, object, "details", details).expect("error details");
    exception
}

fn declared_response_to_v8<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    response: &super::declarations::DeclaredResponse,
) -> Result<v8::Local<'s, v8::Value>, v8::Local<'s, v8::Value>> {
    use super::declarations::{EncodedValue, encode_json};
    let json_value = |scope: &mut v8::PinScope<'s, '_>, value: &serde_json::Value| {
        encode_json(value).map_err(|error| boundary_exception(scope, error.into()))?;
        let json = serde_json::to_string(value).expect("encoded host data");
        let json = v8::String::new(scope, &json).expect("host data string");
        v8::json::parse(scope, json).ok_or_else(|| {
            boundary_exception(
                scope,
                BoundaryError::Host(TransportError::UnsupportedOperation),
            )
        })
    };
    match &response.result {
        Ok(Ok(EncodedValue::Undefined)) => Ok(v8::undefined(scope).into()),
        Ok(Ok(EncodedValue::Snapshot(snapshot))) => snapshot_to_v8(scope, snapshot.clone())
            .map_err(|_| {
                boundary_exception(
                    scope,
                    BoundaryError::Host(TransportError::UnsupportedOperation),
                )
            }),
        Ok(Ok(EncodedValue::Json(value))) => json_value(scope, value),
        Ok(Err(error)) => Err(boundary_exception(scope, (*error).into())),
        Err(error) => Err(host_failure_exception(scope, error)),
    }
}

fn set_number_property<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<'s, v8::Object>,
    name: &str,
    number: u64,
) -> Result<(), &'static str> {
    let number = exact_number(scope, number)?;
    set_property(scope, object, name, number)
}

fn snapshot_to_v8<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    snapshot: TextSnapshot,
) -> Result<v8::Local<'s, v8::Value>, &'static str> {
    let value = v8::Object::new(scope);
    let text = snapshot_text_to_v8(scope, snapshot.text)?;
    set_property(scope, value, "text", text.into())?;

    let range = v8::Object::new(scope);
    set_number_property(
        scope,
        range,
        "startByteOffset",
        snapshot.range.start_byte_offset as u64,
    )?;
    set_number_property(
        scope,
        range,
        "endByteOffset",
        snapshot.range.end_byte_offset as u64,
    )?;
    set_property(scope, value, "range", range.into())?;
    set_number_property(scope, value, "revision", snapshot.revision)?;
    Ok(value.into())
}

fn buffer_change_to_v8<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    change: BufferChange,
) -> Result<v8::Local<'s, v8::Value>, &'static str> {
    let value = v8::Object::new(scope);
    set_number_property(scope, value, "beforeRevision", change.before_revision)?;
    set_number_property(scope, value, "revision", change.revision)?;
    let edits = v8::Array::new(
        scope,
        i32::try_from(change.edits.len()).map_err(|_| "InvalidEditBatch")?,
    );
    for (index, edit) in change.edits.into_iter().enumerate() {
        let item = v8::Object::new(scope);
        let range = v8::Object::new(scope);
        set_number_property(
            scope,
            range,
            "startByteOffset",
            edit.range.start_byte_offset as u64,
        )?;
        set_number_property(
            scope,
            range,
            "endByteOffset",
            edit.range.end_byte_offset as u64,
        )?;
        set_property(scope, item, "range", range.into())?;
        let text = v8::String::new(scope, &edit.text).ok_or("InvalidEditBatch")?;
        set_property(scope, item, "text", text.into())?;
        if edits.set_index(
            scope,
            u32::try_from(index).map_err(|_| "InvalidEditBatch")?,
            item.into(),
        ) != Some(true)
        {
            return Err("InvalidEditBatch");
        }
    }
    set_property(scope, value, "edits", edits.into())?;
    Ok(value.into())
}

fn snapshot_text_to_v8<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    text: SnapshotText,
) -> Result<v8::Local<'s, v8::String>, &'static str> {
    match text.0 {
        text if text.is_empty() => Ok(v8::String::empty(scope)),
        text => {
            let length = text.len();
            let raw = Arc::into_raw(text);
            let buffer = raw.cast::<u16>() as *mut u16;
            // SAFETY: `raw` transfers one Arc strong reference to V8. The
            // callback reconstructs that exact slice using V8's retained
            // length. If V8 rejects externalization, we reclaim it below.
            match unsafe {
                v8::String::new_external_twobyte_raw(scope, buffer, length, drop_external_utf16)
            } {
                Some(value) => Ok(value),
                None => {
                    // SAFETY: V8 did not accept ownership, so `raw` is still
                    // the unique transferred Arc reference.
                    drop(unsafe { Arc::<[u16]>::from_raw(raw) });
                    Err("UnsupportedOperation")
                }
            }
        }
    }
}

fn set_property<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<'s, v8::Object>,
    name: &str,
    value: v8::Local<'s, v8::Value>,
) -> Result<(), &'static str> {
    let key = v8::String::new(scope, name).ok_or("UnsupportedOperation")?;
    match object.set(scope, key.into(), value) {
        Some(true) => Ok(()),
        _ => Err("UnsupportedOperation"),
    }
}

fn fatal_or_completed(error: &RuntimeError) -> TurnOutcome {
    if error.is_fatal() {
        TurnOutcome::Fatal(Failure::new(error.to_string()))
    } else {
        TurnOutcome::Completed
    }
}

fn execute_engine_turn(turn: Turn<EngineTurn>, finalizer: &LifecycleFinalizer) -> TurnOutcome {
    let key = turn.key;
    let root = turn.root;
    let work = turn.work;
    let capsule = Arc::clone(&work.capsule);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match work.work {
        EngineWork::Script {
            source_name,
            source,
            result,
        } => capsule.start_script_turn(root, source_name, source, result),
        EngineWork::FixtureModule {
            specifier,
            source,
            result,
        } => capsule.start_module_turn(root, specifier, source, result),
        EngineWork::PackageModule { entry, result } => {
            capsule.start_package_turn(root, entry, result)
        }
        EngineWork::HostResponse(response) => capsule.resume_response_turn(root, response),
        EngineWork::BufferChange {
            subscription,
            change,
            enqueued_at,
        } => capsule.start_notification_turn(
            root,
            subscription.value(),
            ListenerNotification::Buffer(change),
            enqueued_at,
        ),
        EngineWork::ApplicationChange {
            subscription,
            event,
        } => capsule.start_notification_turn(
            root,
            subscription.value(),
            ListenerNotification::Application(event),
            Instant::now(),
        ),
        EngineWork::CommandInvocation {
            invocation,
            active_buffer,
            result,
        } => capsule.start_command_turn(root, invocation, active_buffer, result),
        EngineWork::CancelCommand(invocation) => capsule.cancel_command_turn(root, invocation),
        EngineWork::Provider(call) => capsule.start_provider_turn(root, call),
        EngineWork::CancelProvider => capsule.cancel_provider_turn(root),
        EngineWork::Termination => capsule.finish_termination_turn(root),
        #[cfg(test)]
        EngineWork::Panic { result } => {
            let _result = result;
            panic!("injected extension worker panic")
        }
    }))
    .unwrap_or_else(|_| {
        TurnOutcome::Fatal(Failure::new(
            "extension engine panicked while executing a turn",
        ))
    });
    if matches!(outcome, TurnOutcome::Completed | TurnOutcome::Fatal(_)) {
        capsule
            .local_state
            .keybinding_mutations
            .lock()
            .unwrap()
            .clear();
        *capsule.local_state.keybinding_batch_pending.lock().unwrap() = None;
        capsule
            .local_state
            .deferred_responses
            .lock()
            .unwrap()
            .clear();
    }
    match &outcome {
        TurnOutcome::Fatal(failure) => {
            finalizer.finalize(
                key,
                LifecycleEnd::Failed(failure.clone()),
                RuntimeError::fatal(
                    RuntimeErrorKind::LifecycleFailed,
                    failure.message().to_owned(),
                ),
                false,
            );
            outcome
        }
        _ => outcome,
    }
}

extern "C" fn near_heap_limit_callback(
    data: *mut c_void,
    current_heap_limit: usize,
    _initial_heap_limit: usize,
) -> usize {
    // SAFETY: RuntimeCapsule keeps this boxed state alive until after the
    // callback is removed and the isolate is disposed.
    let state = unsafe { &*(data.cast::<HeapLimitState>()) };
    state.status.store(HEAP_LIMIT_EXCEEDED, Ordering::Release);
    state.isolate.terminate_execution();
    current_heap_limit.saturating_mul(2)
}

extern "C" fn promise_reject_callback(message: v8::PromiseRejectMessage) {
    v8::callback_scope!(unsafe scope, &message);
    let Some(state) = scope.get_slot::<Arc<RuntimeLocalState>>().cloned() else {
        return;
    };
    match message.get_event() {
        v8::PromiseRejectEvent::PromiseRejectWithNoHandler => {
            let promise = v8::Global::new(scope, message.get_promise());
            let value = message
                .get_value()
                .unwrap_or_else(|| v8::undefined(scope).into());
            let report = report_from_value(scope, value, promise);
            state
                .rejections
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(report);
        }
        v8::PromiseRejectEvent::PromiseHandlerAddedAfterReject => {
            let promise = message.get_promise();
            let mut rejections = state
                .rejections
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(index) = rejections.iter().position(|report| {
                let retained = v8::Local::new(scope, &report.promise);
                retained == promise
            }) {
                rejections.remove(index);
            }
        }
        v8::PromiseRejectEvent::PromiseRejectAfterResolved
        | v8::PromiseRejectEvent::PromiseResolveAfterResolved => {}
    }
}

fn report_from_value(
    scope: &mut v8::PinScope,
    value: v8::Local<v8::Value>,
    promise: v8::Global<v8::Promise>,
) -> RejectionReport {
    let message = v8::Exception::create_message(scope, value);
    let source = message.get_script_resource_name(scope).and_then(|value| {
        value
            .to_string(scope)
            .map(|value| value.to_rust_string_lossy(scope))
    });
    RejectionReport {
        promise,
        message: message.get(scope).to_rust_string_lossy(scope),
        source,
        line: message
            .get_line_number(scope)
            .and_then(|line| u32::try_from(line).ok()),
        column: one_based_coordinate(message.get_start_column()),
        stack: None,
    }
}

fn runtime_error_from_try_catch(
    try_catch: &mut v8::PinnedRef<'_, v8::TryCatch<v8::HandleScope>>,
    kind: RuntimeErrorKind,
    fallback_source: &str,
) -> RuntimeError {
    let message_text = try_catch
        .exception()
        .map(|value| value.to_rust_string_lossy(try_catch))
        .unwrap_or_else(|| "JavaScript execution failed".to_owned());
    let message = try_catch.message();
    let source = message
        .and_then(|message| message.get_script_resource_name(try_catch))
        .and_then(|value| value.to_string(try_catch))
        .map(|value| value.to_rust_string_lossy(try_catch))
        .or_else(|| Some(fallback_source.to_owned()));
    let line = message.and_then(|message| {
        message
            .get_line_number(try_catch)
            .and_then(|line| u32::try_from(line).ok())
    });
    let column = message.and_then(|message| one_based_coordinate(message.get_start_column()));
    let stack = try_catch
        .stack_trace()
        .and_then(|value| value.to_string(try_catch))
        .map(|value| value.to_rust_string_lossy(try_catch));
    RuntimeError {
        kind,
        message: message_text.into(),
        source: source.map(Into::into),
        line,
        column,
        stack: stack.map(Into::into),
    }
}

fn one_based_coordinate(value: usize) -> Option<u32> {
    u32::try_from(value).ok()?.checked_add(1)
}

unsafe extern "C" fn drop_external_utf16(buffer: *mut u16, length: usize) {
    let slice = ptr::slice_from_raw_parts(buffer.cast_const(), length);
    // SAFETY: this exactly rebuilds the Arc slice converted in
    // `hold_external_utf16`, consuming its one transferred strong reference.
    drop(unsafe { Arc::<[u16]>::from_raw(slice) });
}

#[cfg(test)]
fn test_barrier_callback(
    scope: &mut v8::PinnedRef<'_, v8::HandleScope>,
    _arguments: v8::FunctionCallbackArguments,
    mut result: v8::ReturnValue,
) {
    let state = scope.get_slot::<Arc<RuntimeLocalState>>().cloned();
    let barrier = state
        .as_ref()
        .and_then(|state| state.barrier.lock().ok()?.clone());
    if let Some(barrier) = barrier {
        barrier.wait();
    }
    result.set(v8::undefined(scope).into());
}

#[cfg(test)]
fn test_entered_callback(
    scope: &mut v8::PinnedRef<'_, v8::HandleScope>,
    _arguments: v8::FunctionCallbackArguments,
    mut result: v8::ReturnValue,
) {
    let entered = scope
        .get_slot::<Arc<RuntimeLocalState>>()
        .and_then(|state| state.entered.lock().ok()?.clone());
    if let Some(entered) = entered {
        let _ = entered.send(());
    }
    result.set(v8::undefined(scope).into());
}

#[cfg(test)]
mod tests {
    fn test_failure(code: &'static str) -> HostFailure {
        HostFailure {
            code,
            message: code.into(),
            details: serde_json::Value::Null,
        }
    }

    use crate::app::declared_host as declared;
    use crate::host::declarations::{DeclaredRequest, Handler, Responder};
    fn operation<H: Handler>(args: H::Arguments) -> DeclaredRequest {
        DeclaredRequest::new::<H>(args)
    }
    fn success<H: Handler>(value: H::Success) -> super::super::declarations::DeclaredResponse {
        Responder::<H>::new().respond(Ok(value))
    }

    fn keybindings_applied(error: Option<String>) -> super::super::declarations::DeclaredResponse {
        Responder::<declared::mutate_keybindings>::new().respond(error.map_or(Ok(()), |message| {
            Err(declared::KeymapError::invalid(message))
        }))
    }

    fn clipboard_operation(text: &str) -> DeclaredRequest {
        struct Reader(String);
        impl super::super::declarations::ArgumentReader for Reader {
            fn optional_integer(
                &mut self,
                _: usize,
            ) -> Result<Option<u64>, super::super::declarations::DecodeError> {
                unreachable!()
            }
            fn command_arguments(
                &mut self,
                _: usize,
            ) -> Result<CommandArgumentValue, super::super::declarations::DecodeError> {
                unreachable!()
            }
            fn outcome(
                &mut self,
                _: usize,
            ) -> Result<CommandOutcome, super::super::declarations::DecodeError> {
                unreachable!()
            }

            fn range(
                &mut self,
                _: usize,
            ) -> Result<Option<ByteRange>, super::super::declarations::DecodeError> {
                unreachable!()
            }
            fn edits(
                &mut self,
                _: usize,
            ) -> Result<Vec<TextEdit>, super::super::declarations::DecodeError> {
                unreachable!()
            }
            fn contributions(
                &mut self,
                _: usize,
            ) -> Result<Vec<EditorContribution>, super::super::declarations::DecodeError>
            {
                unreachable!()
            }

            fn optional_text(
                &mut self,
                _: usize,
                _: &'static str,
            ) -> Result<Option<String>, super::super::declarations::DecodeError> {
                unreachable!()
            }
            fn text_list(
                &mut self,
                _: usize,
            ) -> Result<Vec<String>, super::super::declarations::DecodeError> {
                unreachable!()
            }

            fn integer(
                &mut self,
                _: usize,
            ) -> Result<u64, super::super::declarations::DecodeError> {
                unreachable!()
            }

            fn text(
                &mut self,
                _: usize,
                _: &'static str,
            ) -> Result<String, super::super::declarations::DecodeError> {
                Ok(self.0.clone())
            }
            fn destination(
                &mut self,
                _: usize,
            ) -> Result<
                Option<super::super::protocol::DocumentDestination>,
                super::super::declarations::DecodeError,
            > {
                unreachable!()
            }
        }
        super::super::declarations::registry()
            .get("writeClipboardText")
            .unwrap()
            .decode(&mut Reader(text.into()))
            .unwrap()
    }

    fn clipboard_written() -> super::super::declarations::DeclaredResponse {
        super::super::declarations::DeclaredResponse {
            operation: "writeClipboardText",
            result: Ok(Ok(super::super::declarations::EncodedValue::Undefined)),
        }
    }

    use super::*;
    use std::{
        collections::{BTreeMap, HashSet},
        num::NonZeroUsize,
        process::Command,
        sync::mpsc,
        time::{Duration, Instant},
    };

    use crate::host::lifecycle::ExtensionState;
    use crate::host::protocol::{
        BufferHandle, ExtensionId, ExtensionLifecycleId, HostResponse, RequestId,
    };

    fn key(extension: u64) -> ExtensionKey {
        ExtensionKey::new(ExtensionId::new(extension), ExtensionLifecycleId::new(1))
    }

    fn package_graph(root: &str, entry: &str, files: &[(&str, &str)]) -> ModuleGraph {
        ModuleGraph::new(
            root,
            entry,
            files
                .iter()
                .map(|(path, source)| ((*path).into(), Arc::from(*source)))
                .collect(),
        )
        .unwrap()
    }

    fn config_graph(entries: &[&str], files: &[(&str, &str)]) -> ModuleGraph {
        ModuleGraph::with_entries(
            "file:///config/knot/",
            entries,
            files
                .iter()
                .map(|(path, source)| ((*path).into(), Arc::from(*source)))
                .collect(),
        )
        .unwrap()
    }

    #[test]
    fn keybinding_calls_are_void_and_one_turn_sends_one_ordered_batch() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(180);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let execution = pool
            .execute_fixture_module(
                runtime,
                "file:///fixtures/keybindings.js",
                r#"
                    import * as knot from "knot";
                    if (knot.keybinding("Cmd-C", "copy") !== undefined) throw new Error("set returned a value");
                    if (knot.keybinding("Cmd-C", { name: "copy", arguments: { format: "plain" } }, { view: "editor" }) !== undefined) throw new Error("set returned a value");
                    if (knot.removeKeybinding("Cmd-C", { view: "editor" }) !== undefined) throw new Error("remove returned a value");
                "#,
            )
            .unwrap();
        let request = pool
            .receive_request_timeout(Duration::from_secs(1))
            .unwrap();
        let mutations = &request
            .operation
            .arguments::<declared::mutate_keybindings>()
            .expect("keybinding batch")
            .mutations;
        assert_eq!(mutations.len(), 3);
        assert!(
            matches!(&mutations[0], KeybindingMutation::Set { key, view: None, command: Some(command) } if key == "Cmd-C" && command.name.as_ref() == "copy")
        );
        assert!(
            matches!(&mutations[1], KeybindingMutation::Set { view: Some(view), command: Some(command), .. } if view == "editor" && command.arguments != CommandArgumentValue::Null)
        );
        assert!(
            matches!(&mutations[2], KeybindingMutation::Remove { view: Some(view), .. } if view == "editor")
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(keybindings_applied(None)),
        })
        .unwrap();
        execution.wait().unwrap();
        assert!(
            pool.receive_request_timeout(Duration::from_millis(20))
                .is_none()
        );
        pool.shutdown();
    }

    #[test]
    fn keybinding_batch_settles_before_await_continuation_and_rejects_turn_errors() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(181);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let execution = pool
            .execute_fixture_module(
                runtime,
                "file:///fixtures/keybinding-await.js",
                r#"
                    import * as knot from "knot";
                    knot.keybinding("cmd-a", "file.new");
                    await knot.workbench.writeClipboardText("wait");
                    knot.keybinding("cmd-b", "file.open");
                "#,
            )
            .unwrap();
        let first = pool
            .receive_request_timeout(Duration::from_secs(1))
            .unwrap();
        let second = pool
            .receive_request_timeout(Duration::from_secs(1))
            .unwrap();
        let (host_request, batch) = if first.operation.descriptor.name == "writeClipboardText" {
            (first, second)
        } else {
            (second, first)
        };
        assert!(
            batch
                .operation
                .arguments::<declared::mutate_keybindings>()
                .is_some()
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: host_request.id,
            result: Ok(clipboard_written()),
        })
        .unwrap();
        assert!(
            pool.receive_request_timeout(Duration::from_millis(20))
                .is_none()
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: batch.id,
            result: Ok(keybindings_applied(None)),
        })
        .unwrap();
        let next = pool
            .receive_request_timeout(Duration::from_secs(1))
            .unwrap();
        assert!(
            matches!(next.operation.arguments::<declared::mutate_keybindings>(), Some(declared::KeybindingArguments { mutations }) if matches!(&mutations[..], [KeybindingMutation::Set { key, .. }] if key == "cmd-b"))
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: next.id,
            result: Ok(keybindings_applied(Some("invalid view kind".into()))),
        })
        .unwrap();
        let error = execution.wait().unwrap_err();
        assert!(error.message().contains("invalid view kind"));
        pool.shutdown();
    }

    #[test]
    fn invalid_keybinding_input_is_queued_for_batch_diagnostics() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(182);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let execution = pool
            .execute_fixture_module(
                runtime,
                "file:///fixtures/invalid-keybinding.js",
                r#"
                    import * as knot from "knot";
                    knot.keybinding("cmd-r", "file.open");
                    if (knot.keybinding("cmd-n", 42) !== undefined) throw new Error("invalid call returned a value");
                "#,
            )
            .unwrap();
        let request = pool
            .receive_request_timeout(Duration::from_secs(1))
            .unwrap();
        assert!(
            matches!(request.operation.arguments::<declared::mutate_keybindings>(), Some(declared::KeybindingArguments { mutations })
            if matches!(&mutations[..], [KeybindingMutation::Set { .. }, KeybindingMutation::Invalid(message)]
                if message.contains("command must be a string or object")))
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(keybindings_applied(Some(
                "keybinding operation 2: command must be a string or object".into(),
            ))),
        })
        .unwrap();
        let error = execution.wait().unwrap_err();
        assert_eq!(
            error.source(),
            Some("file:///fixtures/invalid-keybinding.js")
        );
        assert!(error.message().contains("operation 2"));
        let execution = pool
            .execute_fixture_module(
                runtime,
                "file:///fixtures/invalid-binding-arguments.js",
                r#"
                    import * as knot from "knot";
                    knot.keybinding("cmd-r", { name: "file.open", arguments: { marker: Infinity } });
                "#,
            )
            .unwrap();
        let request = pool
            .receive_request_timeout(Duration::from_secs(1))
            .unwrap();
        assert!(
            matches!(request.operation.arguments::<declared::mutate_keybindings>(), Some(declared::KeybindingArguments { mutations })
            if matches!(&mutations[..], [KeybindingMutation::Invalid(message)]
                if message.contains("finite numbers")))
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(keybindings_applied(Some(
                "keybinding operation 1: command arguments require finite numbers".into(),
            ))),
        })
        .unwrap();
        assert!(
            execution
                .wait()
                .unwrap_err()
                .message()
                .contains("finite numbers")
        );
        pool.shutdown();
    }

    #[test]
    fn personal_config_phases_share_one_awaited_lifecycle() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let mut inbox = pool.take_event_inbox().unwrap();
        let runtime = key(90);
        let graph = config_graph(
            &["pre-init.js", "post-init.js"],
            &[
                (
                    "counter.mjs",
                    "export let count = 0; export function increment() { count++; }",
                ),
                (
                    "pre-init.js",
                    "import * as knot from 'knot'; const { commands } = knot; import { increment } from './counter.mjs'; globalThis.phaseOrder = ['pre']; increment(); await Promise.resolve(); globalThis.configCommand = await commands.register('knot.config.test', () => {});",
                ),
                (
                    "post-init.js",
                    "import { count, increment } from './counter.mjs'; if (count !== 1 || !globalThis.configCommand) throw new Error('lost pre-init state'); increment(); await Promise.resolve(); globalThis.phaseOrder.push('post');",
                ),
            ],
        );
        pool.load_personal_config(runtime, IsolateConfig::default(), graph)
            .unwrap();
        let pre = pool
            .evaluate_personal_config_phase(runtime, ConfigPhase::PreInit)
            .unwrap();
        let Some(EngineEvent::Request(request)) = inbox.receive_timeout(Duration::from_secs(1))
        else {
            panic!("personal config registration request was not received");
        };
        assert_eq!(
            request.operation,
            operation::<declared::register_command>(declared::RegisterCommandArguments {
                name: "knot.config.test".into(),
                title: "knot.config.test".into(),
            })
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(success::<declared::register_command>(
                CommandRegistrationId::new(100),
            )),
        })
        .unwrap();
        pre.wait().unwrap();
        let extension = key(94);
        pool.load_package(
            extension,
            IsolateConfig::default(),
            package_graph(
                "file:///extensions/%40example/during-config/",
                "main.js",
                &[("main.js", "globalThis.extensionStarted = true;")],
            ),
        )
        .unwrap()
        .wait()
        .unwrap();
        pool.evaluate_personal_config_phase(runtime, ConfigPhase::PostInit)
            .unwrap()
            .wait()
            .unwrap();
        assert_eq!(
            pool.execute(runtime, "verify.js", "phaseOrder.join(',')")
                .unwrap()
                .wait()
                .unwrap()
                .value
                .as_ref(),
            "pre,post"
        );
        pool.unload(runtime).unwrap();
        assert!(matches!(
            inbox.receive_timeout(Duration::from_secs(1)),
            Some(EngineEvent::LifecycleEnded { key: ended, reason: LifecycleEnd::Unloaded }) if ended == runtime
        ));
        assert!(
            pool.evaluate_personal_config_phase(runtime, ConfigPhase::PostInit)
                .is_err()
        );
        pool.unload(extension).unwrap();
        pool.shutdown();
    }

    #[test]
    fn personal_config_runtime_blocks_early_post_init_import() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(95);
        let graph = config_graph(
            &["pre-init.js", "post-init.js"],
            &[
                ("pre-init.js", "import './helper.js';"),
                ("helper.js", "import './post-init.js';"),
                ("post-init.js", "globalThis.startedEarly = true;"),
            ],
        );
        pool.load_personal_config(runtime, IsolateConfig::default(), graph)
            .unwrap();
        let error = pool
            .evaluate_personal_config_phase(runtime, ConfigPhase::PreInit)
            .unwrap()
            .wait()
            .unwrap_err();
        assert_eq!(error.kind(), RuntimeErrorKind::ModuleResolution);
        assert_eq!(error.source(), Some("file:///config/knot/helper.js"));
        assert_eq!(error.line(), Some(1));
        pool.unload(runtime).unwrap();
        pool.shutdown();
    }

    #[test]
    fn personal_config_phase_errors_keep_their_source_and_can_be_unloaded() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        for (index, failing_phase) in [ConfigPhase::PreInit, ConfigPhase::PostInit]
            .into_iter()
            .enumerate()
        {
            let runtime = key(91 + index as u64);
            let graph = config_graph(
                &["pre-init.js", "post-init.js"],
                &[
                    (
                        "pre-init.js",
                        if failing_phase == ConfigPhase::PreInit {
                            "\nthrow new Error('pre boom');"
                        } else {
                            "globalThis.ready = true;"
                        },
                    ),
                    ("post-init.js", "\nthrow new Error('post boom');"),
                ],
            );
            pool.load_personal_config(runtime, IsolateConfig::default(), graph)
                .unwrap();
            let pre = pool
                .evaluate_personal_config_phase(runtime, ConfigPhase::PreInit)
                .unwrap()
                .wait();
            let error = if failing_phase == ConfigPhase::PreInit {
                pre.unwrap_err()
            } else {
                pre.unwrap();
                pool.evaluate_personal_config_phase(runtime, ConfigPhase::PostInit)
                    .unwrap()
                    .wait()
                    .unwrap_err()
            };
            assert_eq!(error.kind(), RuntimeErrorKind::Rejection);
            assert_eq!(error.line(), Some(2));
            assert_eq!(
                error.source(),
                Some(if failing_phase == ConfigPhase::PreInit {
                    "file:///config/knot/pre-init.js"
                } else {
                    "file:///config/knot/post-init.js"
                })
            );
            pool.unload(runtime).unwrap();
        }
        pool.shutdown();
    }

    #[test]
    fn personal_config_can_have_only_post_init() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(93);
        let graph = config_graph(
            &["post-init.js"],
            &[(
                "post-init.js",
                "await Promise.resolve(); globalThis.postOnly = true;",
            )],
        );
        pool.load_personal_config(runtime, IsolateConfig::default(), graph)
            .unwrap();
        assert!(
            pool.evaluate_personal_config_phase(runtime, ConfigPhase::PreInit)
                .is_err()
        );
        pool.evaluate_personal_config_phase(runtime, ConfigPhase::PostInit)
            .unwrap()
            .wait()
            .unwrap();
        assert_eq!(
            pool.execute(runtime, "verify.js", "postOnly")
                .unwrap()
                .wait()
                .unwrap()
                .value
                .as_ref(),
            "true"
        );
        pool.unload(runtime).unwrap();
        pool.shutdown();
    }

    #[test]
    fn package_modules_support_local_imports_and_top_level_await() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(50);
        let graph = package_graph(
            "file:///extensions/%40example/tools/",
            "dist/main.js",
            &[
                (
                    "dist/main.js",
                    "import * as knot from 'knot'; const { editor } = knot; import { value } from './helper.js'; import { suffix } from '../suffix.mjs'; await Promise.resolve(); globalThis.packageValue = value + suffix + ':' + typeof editor.activeBuffer;",
                ),
                ("dist/helper.js", "export const value = 'ready';"),
                ("suffix.mjs", "export const suffix = '!';"),
            ],
        );
        pool.load_package(runtime, IsolateConfig::default(), graph)
            .unwrap()
            .wait()
            .unwrap();
        assert_eq!(
            &*pool
                .execute(runtime, "verify.js", "packageValue")
                .unwrap()
                .wait()
                .unwrap()
                .value,
            "ready!:function"
        );
        let changed = pool
            .execute_fixture_module(
                runtime,
                "file:///extensions/%40example/tools/other.js",
                "export {};",
            )
            .unwrap()
            .wait()
            .unwrap_err();
        assert!(changed.message().contains("cannot change after load"));
        pool.unload(runtime).unwrap();
        pool.shutdown();
    }

    #[test]
    fn package_module_sources_are_isolated_per_lifecycle() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let first = key(51);
        let second = key(52);
        for (runtime, root, value) in [
            (first, "file:///extensions/%40example/first/", "first"),
            (second, "file:///extensions/%40example/second/", "second"),
        ] {
            let graph = package_graph(
                root,
                "main.js",
                &[
                    (
                        "main.js",
                        "import { value } from './shared.js'; globalThis.packageValue = value;",
                    ),
                    (
                        "shared.js",
                        if value == "first" {
                            "export const value = 'first';"
                        } else {
                            "export const value = 'second';"
                        },
                    ),
                ],
            );
            pool.load_package(runtime, IsolateConfig::default(), graph)
                .unwrap()
                .wait()
                .unwrap();
        }
        assert_eq!(
            &*pool
                .execute(first, "check.js", "packageValue")
                .unwrap()
                .wait()
                .unwrap()
                .value,
            "first"
        );
        assert_eq!(
            &*pool
                .execute(second, "check.js", "packageValue")
                .unwrap()
                .wait()
                .unwrap()
                .value,
            "second"
        );
        pool.unload(first).unwrap();
        pool.unload(second).unwrap();
        pool.shutdown();
    }

    #[test]
    fn package_module_imports_stay_within_the_supplied_graph() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        for (index, (source, expected)) in [
            ("import 'bare-package';", "relative or knot"),
            ("import '../other/secret.js';", "escapes package"),
            (
                "import 'file:///extensions/%40example/other/secret.js';",
                "relative or knot",
            ),
            ("import './missing.js';", "package module not found"),
            ("import 'knot:bootstrap';", "not importable"),
            ("import 'knot:editor';", "relative or knot"),
        ]
        .into_iter()
        .enumerate()
        {
            let runtime = key(60 + index as u64);
            let graph = package_graph(
                "file:///extensions/%40example/tools/",
                "main.js",
                &[("main.js", source)],
            );
            let failure = pool
                .load_package(runtime, IsolateConfig::default(), graph)
                .unwrap()
                .wait()
                .unwrap_err();
            assert_eq!(failure.kind(), RuntimeErrorKind::ModuleResolution);
            assert!(failure.message().contains(expected), "{failure}");
            assert_eq!(
                failure.source(),
                Some("file:///extensions/%40example/tools/main.js")
            );
            assert_eq!(failure.line(), Some(1));
            assert!(failure.column().is_some());
            pool.unload(runtime).unwrap();
        }
        pool.shutdown();
    }

    #[test]
    fn package_module_errors_report_generated_locations_and_dynamic_import_rejection() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let root = "file:///extensions/%40example/tools/";
        let runtime = key(70);
        let graph = package_graph(
            root,
            "main.js",
            &[
                ("main.js", "import './bad.js';"),
                ("bad.js", "export const = 1;"),
            ],
        );
        let syntax = pool
            .load_package(runtime, IsolateConfig::default(), graph)
            .unwrap()
            .wait()
            .unwrap_err();
        assert_eq!(syntax.kind(), RuntimeErrorKind::Compilation);
        assert_eq!(
            syntax.source(),
            Some("file:///extensions/%40example/tools/bad.js")
        );
        assert_eq!(syntax.line(), Some(1));
        assert!(syntax.column().is_some());
        pool.unload(runtime).unwrap();

        let runtime = key(71);
        let graph = package_graph(
            root,
            "main.js",
            &[("main.js", "\nthrow new Error('package boom');")],
        );
        let thrown = pool
            .load_package(runtime, IsolateConfig::default(), graph)
            .unwrap()
            .wait()
            .unwrap_err();
        assert_eq!(thrown.kind(), RuntimeErrorKind::Rejection);
        assert_eq!(
            thrown.source(),
            Some("file:///extensions/%40example/tools/main.js")
        );
        assert_eq!(thrown.line(), Some(2));
        assert!(thrown.column().is_some());
        pool.unload(runtime).unwrap();

        let runtime = key(72);
        let graph = package_graph(
            root,
            "main.js",
            &[
                ("main.js", "await import('./other.js');"),
                ("other.js", "export {};"),
            ],
        );
        let dynamic = pool
            .load_package(runtime, IsolateConfig::default(), graph)
            .unwrap()
            .wait()
            .unwrap_err();
        assert!(
            dynamic
                .message()
                .contains("dynamic imports are unavailable"),
            "{dynamic}"
        );
        pool.unload(runtime).unwrap();

        let runtime = key(73);
        let graph = package_graph(
            root,
            "main.js",
            &[
                ("main.js", "import './child.js';"),
                ("child.js", "\nthrow new Error('child boom');"),
            ],
        );
        let child = pool
            .load_package(runtime, IsolateConfig::default(), graph)
            .unwrap()
            .wait()
            .unwrap_err();
        assert_eq!(
            child.source(),
            Some("file:///extensions/%40example/tools/child.js")
        );
        assert_eq!(child.line(), Some(2));
        assert!(child.column().is_some());
        pool.unload(runtime).unwrap();
        pool.shutdown();
    }

    #[test]
    fn failed_package_entry_can_be_rolled_back_and_pending_entry_unloaded() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let mut inbox = pool.take_event_inbox().unwrap();
        let root = "file:///extensions/%40example/tools/";
        let runtime = key(80);
        let graph = package_graph(
            root,
            "main.js",
            &[("main.js", "throw new Error('failed entry');")],
        );
        let failed = pool
            .load_package(runtime, IsolateConfig::default(), graph)
            .unwrap()
            .wait()
            .unwrap_err();
        assert!(failed.message().contains("failed entry"));
        pool.unload(runtime).unwrap();
        assert!(
            matches!(inbox.receive_timeout(Duration::from_secs(2)), Some(EngineEvent::LifecycleEnded {
            key: ended,
            reason: LifecycleEnd::Unloaded,
        }) if ended == runtime)
        );

        let runtime = key(81);
        let graph = package_graph(
            root,
            "main.js",
            &[("main.js", "await new Promise(() => {});")],
        );
        let pending = pool
            .load_package(runtime, IsolateConfig::default(), graph)
            .unwrap();
        wait_for_state(&pool, runtime, ExtensionState::AwaitingHostWork);
        pool.unload(runtime).unwrap();
        assert_eq!(
            pending.wait().unwrap_err().kind(),
            RuntimeErrorKind::Cancelled
        );
        assert!(
            matches!(inbox.receive_timeout(Duration::from_secs(2)), Some(EngineEvent::LifecycleEnded {
            key: ended,
            reason: LifecycleEnd::Unloaded,
        }) if ended == runtime)
        );
        pool.shutdown();
    }

    fn wait_for_state(pool: &RuntimePool, key: ExtensionKey, expected: ExtensionState) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let state = pool.scheduler_handle.state(key).unwrap();
            if state == expected {
                return;
            }
            assert!(Instant::now() < deadline, "state remained {state:?}");
            std::thread::yield_now();
        }
    }

    fn execute_captured_fixture(
        pool: &RuntimePool,
        runtime: ExtensionKey,
        module: impl AsRef<str>,
        source: &str,
    ) -> RuntimeCommandExecution {
        let source = format!(
            "import * as knot from 'knot'; const {{ editor, commands }} = knot; await commands.register('fixture.setup', async () => {{\n{source}\n}});"
        );
        let setup = pool
            .execute_fixture_module(runtime, module.as_ref(), source.as_str())
            .unwrap();
        let registration_request = pool.receive_request().unwrap();
        let registration = CommandRegistrationId::new(900);
        assert!(
            registration_request
                .operation
                .arguments::<declared::register_command>()
                .is_some()
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: registration_request.id,
            result: Ok(success::<declared::register_command>(registration)),
        })
        .unwrap();
        setup.wait().unwrap();
        pool.invoke_command(
            runtime,
            CommandInvocation {
                id: CommandInvocationId::new(900),
                registration,
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                arguments: CommandArgumentValue::Null,
                capture: Some(crate::host::protocol::CommandCapture {
                    authority: CommandInvocationId::new(900),
                    window: "fixture-window".into(),
                    view: None,
                    tool: None,
                }),
            },
            Some(BufferHandle::new(3)),
        )
        .unwrap()
    }

    fn install_buffer_listener(
        pool: &RuntimePool,
        runtime: ExtensionKey,
        subscription: BufferSubscriptionId,
        module: &str,
    ) {
        let execution = execute_captured_fixture(
            pool,
            runtime,
            module,
            r#"
                const buffer = await editor.activeBuffer();
                globalThis.events = [];
                await buffer.onDidChange(globalThis.listener);
            "#,
        );
        let active = pool.receive_request().unwrap();
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: active.id,
            result: Ok(success::<declared::active_buffer>(Some(BufferHandle::new(
                3,
            )))),
        })
        .unwrap();
        let subscribe = pool.receive_request().unwrap();
        assert_eq!(
            subscribe.operation,
            operation::<declared::subscribe_buffer_changes>(declared::BufferArguments {
                buffer: BufferHandle::new(3)
            })
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: subscribe.id,
            result: Ok(success::<declared::subscribe_buffer_changes>(subscription)),
        })
        .unwrap();
        assert_eq!(execution.wait().unwrap(), CommandOutcome::Completed);
    }

    #[test]
    fn facade_rejects_malformed_coordinates_revisions_and_json_before_transport() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(190);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let execution = execute_captured_fixture(
            &pool,
            runtime,
            "file:///fixtures/validation.js",
            r#"
          const buffer = await editor.activeBuffer();
          async function expect(operation, name) {
            try { await operation(); } catch (error) { if (error.name === name) return; throw error; }
            throw new Error(`expected ${name}`);
          }
          for (const value of [undefined, null, "1", true, {}, 1n]) {
            await expect(() => buffer.applyEdits([], { ifRevision: value }), "TypeError");
            await expect(() => buffer.contributions.replace([], { ifRevision: value }), "TypeError");
          }
          for (const value of [-1, 1.5, Infinity, NaN, Number.MAX_SAFE_INTEGER + 1]) {
            await expect(() => buffer.applyEdits([], { ifRevision: value }), "RangeError");
            await expect(() => buffer.snapshot({ startByteOffset: 0, endByteOffset: value }), "RangeError");
          }
          await expect(() => buffer.applyEdits([]), "TypeError");
          await expect(() => buffer.contributions.replace([]), "TypeError");
          await expect(() => buffer.snapshot({ startByteOffset: 2, endByteOffset: 1 }), "RangeError");
          await expect(() => buffer.applyEdits([{ range: { startByteOffset: 0, endByteOffset: 0 }, text: 4 }], { ifRevision: 0 }), "TypeError");
          await expect(() => buffer.contributions.replace([{ range: { startByteOffset: 0, endByteOffset: 0 }, decoration: "other" }], { ifRevision: 0 }), "TypeError");
          const cyclic = {}; cyclic.self = cyclic;
          const custom = { toJSON() { return {}; } };
          const sparse = new Array(2);
          const symbolKey = { [Symbol()]: 1 };
          const extra = []; extra.hidden = () => {};
          for (const value of [undefined, () => {}, Symbol(), 1n, NaN, Infinity, new Date(), cyclic, custom, sparse, symbolKey, extra]) {
            // Undefined at the root defaults to null, so use it in a record.
            await expect(() => commands.invoke("never", { value }), "TypeError");
          }
          await expect(() => commands.invalidArguments(4), "TypeError");
          const snapshot = await buffer.snapshot();
          for (const value of ["0", {}, true, undefined]) {
            await expect(() => snapshot.byteOffsetAtUtf16(value), "TypeError");
            await expect(() => snapshot.utf16OffsetAtByte(value), "TypeError");
          }
          for (const value of [-1, 0.5, Infinity, Number.MAX_SAFE_INTEGER + 1]) {
            await expect(() => snapshot.byteOffsetAtUtf16(value), "RangeError");
            await expect(() => snapshot.utf16OffsetAtByte(value), "RangeError");
          }
          Array.prototype[1] = 99;
          try { await expect(() => snapshot.byteOffsetAtUtf16(1), "RangeError"); }
          finally { delete Array.prototype[1]; }
          await expect(() => snapshot.utf16OffsetAtByte(1), "RangeError");
        "#,
        );
        let active = pool.receive_request().unwrap();
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: active.id,
            result: Ok(success::<declared::active_buffer>(Some(BufferHandle::new(
                3,
            )))),
        })
        .unwrap();
        let snapshot = pool.receive_request().unwrap();
        assert!(
            snapshot
                .operation
                .arguments::<declared::snapshot>()
                .is_some()
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: snapshot.id,
            result: Ok(success::<declared::snapshot>(TextSnapshot {
                text: SnapshotText::from_utf8("😀"),
                range: ByteRange {
                    start_byte_offset: 0,
                    end_byte_offset: 4,
                },
                revision: 0,
            })),
        })
        .unwrap();
        assert_eq!(execution.wait().unwrap(), CommandOutcome::Completed);
        assert!(
            pool.receive_request_timeout(Duration::from_millis(20))
                .is_none()
        );
        pool.shutdown();
    }

    #[test]
    fn every_disposer_shares_native_acknowledgement_and_retains_failure() {
        for failed in [false, true] {
            let pool = RuntimePool::new(PoolConfig::single_worker());
            let runtime = key(191);
            pool.load(runtime, IsolateConfig::default()).unwrap();
            let setup = execute_captured_fixture(
                &pool,
                runtime,
                "file:///fixtures/owned.js",
                r#"
              const { workbench } = knot;
              globalThis.buffer = await editor.activeBuffer();
              globalThis.owned = [
                await commands.register("owned", () => { throw new Error("disposed command ran"); }),
                await buffer.onDidChange(() => { throw new Error("disposed listener ran"); }),
                buffer.contributions,
                await workbench.registerView({ id: "example.outline", title: "Outline", content: { kind: "tree", provider: { getChildren() { throw new Error("disposed tree ran"); } } } }),
                await editor.registerCompletionProvider("owned", { provideCompletions() { throw new Error("disposed completion ran"); } }),
              ];
            "#,
            );
            let responses = [
                success::<declared::active_buffer>(Some(BufferHandle::new(3))),
                success::<declared::register_command>(CommandRegistrationId::new(901)),
                success::<declared::subscribe_buffer_changes>(BufferSubscriptionId::new(902)),
                success::<declared::register_tree_provider>(TreeProviderRegistrationId::new(903)),
                success::<declared::register_completion_provider>(
                    CompletionProviderRegistrationId::new(904),
                ),
            ];
            for value in responses {
                let request = pool.receive_request().unwrap();
                pool.respond(HostResponse {
                    extension: runtime.extension,
                    lifecycle: runtime.lifecycle,
                    id: request.id,
                    result: Ok(value),
                })
                .unwrap();
            }
            assert_eq!(setup.wait().unwrap(), CommandOutcome::Completed);
            let disposal = pool.execute(runtime, "dispose.js", r#"
              (async () => {
                globalThis.cleanup = owned.map((owner) => {
                  const promise = owner.dispose();
                  if (promise !== owner.dispose()) throw new Error("cleanup promise changed");
                  return promise;
                });
                for (const mutation of [() => buffer.contributions.replace([], { ifRevision: 0 }), () => owned[3].invalidate()]) {
                  let inactive = false;
                  try { await mutation(); } catch (error) { if (error.code !== "OwnershipDisposed") throw error; inactive = true; }
                  if (!inactive) throw new Error("ownership still active");
                }
                globalThis.cleanupResults = await Promise.allSettled(cleanup);
                for (let index = 0; index < owned.length; index++) {
                  if (owned[index].dispose() !== cleanup[index]) throw new Error("settled cleanup promise changed");
                }
              })()
            "#).unwrap();
            let operations = [
                (
                    success::<declared::unregister_command>(()),
                    test_failure("CommandNotFound"),
                ),
                (
                    success::<declared::unsubscribe_buffer_changes>(()),
                    test_failure("OwnershipDisposed"),
                ),
                (
                    success::<declared::dispose_editor_contributions>(()),
                    test_failure("BufferClosed"),
                ),
                (
                    success::<declared::unregister_tree_provider>(()),
                    test_failure("TreeProviderNotFound"),
                ),
                (
                    success::<declared::unregister_completion_provider>(()),
                    test_failure("CompletionProviderNotFound"),
                ),
            ];
            for (value, error) in operations {
                let request = pool.receive_request().unwrap();
                pool.respond(HostResponse {
                    extension: runtime.extension,
                    lifecycle: runtime.lifecycle,
                    id: request.id,
                    result: if failed { Err(error) } else { Ok(value) },
                })
                .unwrap();
            }
            disposal.wait().unwrap();
            let expected = if failed {
                "rejected,rejected,rejected,rejected,rejected"
            } else {
                "fulfilled,fulfilled,fulfilled,fulfilled,fulfilled"
            };
            assert_eq!(
                &*pool
                    .execute(
                        runtime,
                        "verify-cleanup.js",
                        "cleanupResults.map(result => result.status).join(',')"
                    )
                    .unwrap()
                    .wait()
                    .unwrap()
                    .value,
                expected
            );
            assert!(
                pool.receive_request_timeout(Duration::from_millis(20))
                    .is_none()
            );
            pool.dispatch_buffer_change(
                runtime,
                BufferSubscriptionId::new(902),
                BufferChange {
                    buffer: BufferHandle::new(3),
                    before_revision: 0,
                    revision: 1,
                    edits: vec![],
                },
            )
            .unwrap();
            pool.execute(runtime, "listener-drained.js", "42")
                .unwrap()
                .wait()
                .unwrap();
            let tree = pool
                .request_tree_children(
                    runtime,
                    TreeChildrenRequest {
                        registration: TreeProviderRegistrationId::new(903),
                        window: "1".into(),
                        view: crate::host::protocol::ViewId::new(1),
                        parent_id: None,
                        generation: 1,
                    },
                )
                .unwrap()
                .wait()
                .unwrap();
            assert!(tree.result.unwrap_err().message.contains("disposed"));
            let completion = pool
                .request_completions(
                    runtime,
                    CompletionRequest {
                        registration: CompletionProviderRegistrationId::new(904),
                        buffer: BufferHandle::new(3),
                        revision: 0,
                        cursor_byte_offset: 0,
                        prefix: "".into(),
                        generation: 1,
                    },
                )
                .unwrap()
                .wait()
                .unwrap();
            assert!(completion.result.unwrap_err().message.contains("disposed"));
            pool.shutdown();
        }
    }

    #[test]
    fn invalidation_cleanup_waits_for_earlier_native_acknowledgements() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(192);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let setup = pool.execute_fixture_module(runtime, "file:///fixtures/ordering.js", r#"
          import { workbench } from "knot";
          globalThis.api = workbench;
          globalThis.tree = await workbench.registerView({ id: "example.outline", title: "Outline", content: { kind: "tree", provider: { getChildren() { return []; } } } });
        "#).unwrap();
        let request = pool.receive_request().unwrap();
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(success::<declared::register_tree_provider>(
                TreeProviderRegistrationId::new(905),
            )),
        })
        .unwrap();
        setup.wait().unwrap();
        let work = pool
            .execute(
                runtime,
                "ordered-disposal.js",
                r#"
          (async () => {
            const mutation = tree.invalidate("child");
            const cleanup = tree.dispose();
            const acknowledged = cleanup.then(() => api.writeClipboardText("cleanup acknowledged"));
            await Promise.all([mutation, cleanup, acknowledged]);
          })()
        "#,
            )
            .unwrap();
        let invalidate = pool.receive_request().unwrap();
        let dispose = pool.receive_request().unwrap();
        assert!(
            invalidate
                .operation
                .arguments::<declared::invalidate_tree_provider>()
                .is_some()
        );
        assert!(
            dispose
                .operation
                .arguments::<declared::unregister_tree_provider>()
                .is_some()
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: dispose.id,
            result: Ok(success::<declared::unregister_tree_provider>(())),
        })
        .unwrap();
        assert!(
            pool.receive_request_timeout(Duration::from_millis(20))
                .is_none()
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: invalidate.id,
            result: Ok(success::<declared::invalidate_tree_provider>(())),
        })
        .unwrap();
        let acknowledgement = pool.receive_request().unwrap();
        assert_eq!(
            acknowledgement.operation,
            clipboard_operation("cleanup acknowledged")
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: acknowledgement.id,
            result: Ok(clipboard_written()),
        })
        .unwrap();
        work.wait().unwrap();
        pool.shutdown();
    }

    #[test]
    fn native_integers_are_checked_before_v8_conversion() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(193);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let setup = execute_captured_fixture(
            &pool,
            runtime,
            "file:///fixtures/exact-number.js",
            r#"
          const buffer = await editor.activeBuffer();
          let rejected = false;
          try { await buffer.snapshot(); } catch (error) { if (error.code !== "UnsupportedOperation") throw error; rejected = true; }
          if (!rejected) throw new Error("rounded revision accepted");
        "#,
        );
        let request = pool.receive_request().unwrap();
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(success::<declared::active_buffer>(Some(BufferHandle::new(
                3,
            )))),
        })
        .unwrap();
        let request = pool.receive_request().unwrap();
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(success::<declared::snapshot>(TextSnapshot {
                text: SnapshotText::from_utf8(""),
                range: ByteRange {
                    start_byte_offset: 0,
                    end_byte_offset: 0,
                },
                revision: MAX_JS_INTEGER + 1,
            })),
        })
        .unwrap();
        assert_eq!(setup.wait().unwrap(), CommandOutcome::Completed);
        pool.shutdown();
    }

    #[test]
    fn provider_self_disposal_acknowledges_without_waiting_for_its_callback() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(194);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let setup = pool.execute_fixture_module(runtime, "file:///fixtures/self-disposal.js", r#"
          import { editor, workbench } from "knot";
          globalThis.treeCalls = 0;
          globalThis.completionCalls = 0;
          globalThis.tree = await workbench.registerView({ id: "example.outline", title: "Outline", content: { kind: "tree", provider: {
            async getChildren() { treeCalls++; await tree.dispose(); globalThis.treeReleased = true; return []; },
          } } });
          globalThis.completion = await editor.registerCompletionProvider("self", {
            async provideCompletions() { completionCalls++; await completion.dispose(); globalThis.completionReleased = true; return []; },
          });
        "#).unwrap();
        for value in [
            success::<declared::register_tree_provider>(TreeProviderRegistrationId::new(906)),
            success::<declared::register_completion_provider>(
                CompletionProviderRegistrationId::new(907),
            ),
        ] {
            let request = pool.receive_request().unwrap();
            pool.respond(HostResponse {
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                id: request.id,
                result: Ok(value),
            })
            .unwrap();
        }
        setup.wait().unwrap();
        let tree = pool
            .request_tree_children(
                runtime,
                TreeChildrenRequest {
                    registration: TreeProviderRegistrationId::new(906),
                    window: "1".into(),
                    view: crate::host::protocol::ViewId::new(1),
                    parent_id: None,
                    generation: 1,
                },
            )
            .unwrap();
        let release = pool.receive_request().unwrap();
        assert!(
            release
                .operation
                .arguments::<declared::unregister_tree_provider>()
                .is_some()
        );
        let queued = pool
            .request_tree_children(
                runtime,
                TreeChildrenRequest {
                    registration: TreeProviderRegistrationId::new(906),
                    window: "1".into(),
                    view: crate::host::protocol::ViewId::new(1),
                    parent_id: None,
                    generation: 2,
                },
            )
            .unwrap();
        drop(queued);
        // Native unregistration drops its receiver before replying to self-disposal.
        drop(tree);
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: release.id,
            result: Ok(success::<declared::unregister_tree_provider>(())),
        })
        .unwrap();
        assert_eq!(
            &*pool
                .execute(
                    runtime,
                    "verify-tree-self-disposal.js",
                    "`${treeReleased}:${treeCalls}`"
                )
                .unwrap()
                .wait()
                .unwrap()
                .value,
            "true:1"
        );
        let completion = pool
            .request_completions(
                runtime,
                CompletionRequest {
                    registration: CompletionProviderRegistrationId::new(907),
                    buffer: BufferHandle::new(3),
                    revision: 0,
                    cursor_byte_offset: 0,
                    prefix: "".into(),
                    generation: 1,
                },
            )
            .unwrap();
        let release = pool.receive_request().unwrap();
        assert!(
            release
                .operation
                .arguments::<declared::unregister_completion_provider>()
                .is_some()
        );
        drop(completion);
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: release.id,
            result: Ok(success::<declared::unregister_completion_provider>(())),
        })
        .unwrap();
        assert_eq!(
            &*pool
                .execute(
                    runtime,
                    "verify-completion-self-disposal.js",
                    "`${completionReleased}:${completionCalls}`"
                )
                .unwrap()
                .wait()
                .unwrap()
                .value,
            "true:1"
        );
        assert!(
            pool.capsule(runtime)
                .unwrap()
                .local_state
                .pending
                .lock()
                .unwrap()
                .is_empty()
        );
        pool.shutdown();
    }

    #[test]
    fn provider_results_require_iterables_and_validate_only_recognized_item_fields() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(195);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let setup = pool.execute_fixture_module(runtime, "file:///fixtures/provider-data.js", r#"
          import { editor, workbench } from "knot";
          await workbench.registerView({ id: "example.outline", title: "Outline", content: { kind: "tree", provider: {
            getChildren(parent) {
              if (parent === "prototype") { Object.prototype.error = "ambient error"; Object.prototype.toJSON = () => { throw new Error("ambient toJSON evaluated"); }; }
              if (parent === "arraylike") return { 0: {}, length: 1 };
              if (parent === "string") return "children";
              if (parent === "missing") return [{ id: "id", label: "label" }];
              if (parent === "null-state") return [{ id: "id", label: "label", collapsibleState: null }];
              if (parent === "wrong-description") return [{ id: "id", label: "label", collapsibleState: "none", description: 4 }];
              if (parent === "bad-token") return [{ id: "id", label: "label", collapsibleState: "none", icon: "other" }];
              return new Set([{ id: "id", label: "label", collapsibleState: "none", unknown: () => {}, toJSON() { throw new Error("unknown item field evaluated"); } }]);
            },
          } } });
          await editor.registerCompletionProvider("data", {
            provideCompletions(context) {
              if (context.prefix === "arraylike") return { length: 1, 0: {} };
              if (context.prefix === "string") return "candidates";
              if (context.prefix === "missing") return [{ label: "label" }];
              return new Set([{ label: "label", insertText: "label", unknown: () => {}, toJSON() { throw new Error("unknown item field evaluated"); } }]);
            },
          });
        "#).unwrap();
        for value in [
            success::<declared::register_tree_provider>(TreeProviderRegistrationId::new(908)),
            success::<declared::register_completion_provider>(
                CompletionProviderRegistrationId::new(909),
            ),
        ] {
            let request = pool.receive_request().unwrap();
            pool.respond(HostResponse {
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                id: request.id,
                result: Ok(value),
            })
            .unwrap();
        }
        setup.wait().unwrap();
        for parent in [
            "arraylike",
            "string",
            "missing",
            "null-state",
            "wrong-description",
            "bad-token",
            "valid",
            "prototype",
        ] {
            let result = pool
                .request_tree_children(
                    runtime,
                    TreeChildrenRequest {
                        registration: TreeProviderRegistrationId::new(908),
                        window: "1".into(),
                        view: crate::host::protocol::ViewId::new(1),
                        parent_id: Some(parent.into()),
                        generation: 1,
                    },
                )
                .unwrap()
                .wait()
                .unwrap()
                .result;
            assert_eq!(
                result.is_ok(),
                matches!(parent, "valid" | "prototype"),
                "{parent}: {result:?}"
            );
        }
        for prefix in ["arraylike", "string", "missing", "valid"] {
            let result = pool
                .request_completions(
                    runtime,
                    CompletionRequest {
                        registration: CompletionProviderRegistrationId::new(909),
                        buffer: BufferHandle::new(3),
                        revision: 0,
                        cursor_byte_offset: 0,
                        prefix: prefix.into(),
                        generation: 1,
                    },
                )
                .unwrap()
                .wait()
                .unwrap()
                .result;
            assert_eq!(result.is_ok(), prefix == "valid", "{prefix}: {result:?}");
        }
        pool.execute(
            runtime,
            "restore-prototype.js",
            "delete Object.prototype.toJSON; delete Object.prototype.error",
        )
        .unwrap()
        .wait()
        .unwrap();
        pool.shutdown();
    }

    #[test]
    fn registration_response_racing_unload_cannot_publish_late_ownership() {
        for operation in [
            "commands.register('late', () => {})",
            "editor.registerCompletionProvider('late', { provideCompletions() { return []; } })",
            "workbench.registerView({ id: 'example.outline', title: 'Outline', content: { kind: 'tree', provider: { getChildren() { return []; } } } })",
        ] {
            let pool = RuntimePool::new(PoolConfig::single_worker());
            let runtime = key(196);
            pool.load(runtime, IsolateConfig::default()).unwrap();
            let source = format!(
                "import {{commands, editor, workbench}} from 'knot'; await {operation}; globalThis.published = true;"
            );
            let registration = pool
                .execute_fixture_module(runtime, "file:///fixtures/late-registration.js", source)
                .unwrap();
            let request = pool.receive_request().unwrap();
            // Request delivery precedes the worker yielding; unload cancels a
            // parked root but can terminate JavaScript that is still running.
            wait_for_state(&pool, runtime, ExtensionState::AwaitingHostWork);
            pool.unload(runtime).unwrap();
            assert_eq!(
                registration.wait().unwrap_err().kind(),
                RuntimeErrorKind::Cancelled
            );
            let value = match request.operation {
                operation
                    if operation
                        .arguments::<declared::register_command>()
                        .is_some() =>
                {
                    success::<declared::register_command>(CommandRegistrationId::new(1))
                }
                operation
                    if operation
                        .arguments::<declared::register_completion_provider>()
                        .is_some() =>
                {
                    success::<declared::register_completion_provider>(
                        CompletionProviderRegistrationId::new(1),
                    )
                }
                operation
                    if operation
                        .arguments::<declared::register_tree_provider>()
                        .is_some() =>
                {
                    success::<declared::register_tree_provider>(TreeProviderRegistrationId::new(1))
                }
                _ => unreachable!(),
            };
            assert_eq!(
                pool.respond(HostResponse {
                    extension: runtime.extension,
                    lifecycle: runtime.lifecycle,
                    id: request.id,
                    result: Ok(value)
                }),
                Err(RuntimeResponseError::WrongExtension)
            );
            pool.shutdown();
        }
    }

    #[test]
    fn application_observation_installs_before_queued_delivery_and_self_disposes() {
        use super::super::protocol::{ApplicationSnapshot, WorkbenchObservation};
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(209);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let setup = pool.execute_fixture_module(runtime, "file:///fixtures/application-admission.js", r#"
import { documents, workbench } from "knot";
globalThis.calls = 0;
const observation = await workbench.observe(async (event) => {
  if (!globalThis.activationFinished || event.sequence <= observation.watermark) throw new Error("startup observation gap");
  globalThis.calls++;
  await documents.list();
  await observation.dispose();
  globalThis.disposed = true;
});
globalThis.activationFinished = true;
"#).unwrap();
        let admission = pool.receive_request().unwrap();
        assert_eq!(
            admission.operation,
            operation::<declared::observe_workbench>(())
        );
        let subscription = WorkbenchSubscriptionId::new(911);
        let event = |sequence| ApplicationEvent {
            sequence,
            change: ApplicationChange::DocumentClosed {
                document: "document".into(),
            },
        };
        pool.dispatch_application_change(runtime, subscription, event(8))
            .unwrap();
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: admission.id,
            result: Ok(success::<declared::observe_workbench>(
                WorkbenchObservation {
                    subscription,
                    snapshot: ApplicationSnapshot::default(),
                    watermark: 7,
                },
            )),
        })
        .unwrap();
        setup.wait().unwrap();
        let query = pool.receive_request().unwrap();
        assert_eq!(query.operation, operation::<declared::list_documents>(()));
        pool.dispatch_application_change(runtime, subscription, event(9))
            .unwrap();
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: query.id,
            result: Ok(success::<declared::list_documents>(vec![])),
        })
        .unwrap();
        let disposal = pool.receive_request().unwrap();
        assert_eq!(
            disposal.operation,
            operation::<declared::unobserve_workbench>(declared::WorkbenchSubscription {
                subscription
            })
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: disposal.id,
            result: Ok(success::<declared::unobserve_workbench>(())),
        })
        .unwrap();
        assert_eq!(
            &*pool
                .execute(
                    runtime,
                    "verify-observation.js",
                    "`${activationFinished}:${disposed}:${calls}`"
                )
                .unwrap()
                .wait()
                .unwrap()
                .value,
            "true:true:1"
        );
        pool.shutdown();
    }

    #[test]
    fn buffer_listener_self_disposal_rejects_queued_deliveries_without_deadlock() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(197);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let setup = execute_captured_fixture(
            &pool,
            runtime,
            "file:///fixtures/listener-self-disposal.js",
            r#"
          const buffer = await editor.activeBuffer();
          globalThis.listenerCalls = 0;
          globalThis.subscription = await buffer.onDidChange(async () => {
            listenerCalls++;
            await subscription.dispose();
            globalThis.listenerReleased = true;
          });
        "#,
        );
        for value in [
            success::<declared::active_buffer>(Some(BufferHandle::new(3))),
            success::<declared::subscribe_buffer_changes>(BufferSubscriptionId::new(910)),
        ] {
            let request = pool.receive_request().unwrap();
            pool.respond(HostResponse {
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                id: request.id,
                result: Ok(value),
            })
            .unwrap();
        }
        assert_eq!(setup.wait().unwrap(), CommandOutcome::Completed);
        for revision in 1..=2 {
            pool.dispatch_buffer_change(
                runtime,
                BufferSubscriptionId::new(910),
                BufferChange {
                    buffer: BufferHandle::new(3),
                    before_revision: revision - 1,
                    revision,
                    edits: vec![],
                },
            )
            .unwrap();
        }
        let release = pool.receive_request().unwrap();
        assert!(
            release
                .operation
                .arguments::<declared::unsubscribe_buffer_changes>()
                .is_some()
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: release.id,
            result: Ok(success::<declared::unsubscribe_buffer_changes>(())),
        })
        .unwrap();
        assert_eq!(
            &*pool
                .execute(
                    runtime,
                    "verify-listener-disposal.js",
                    "`${listenerReleased}:${listenerCalls}`"
                )
                .unwrap()
                .wait()
                .unwrap()
                .value,
            "true:1"
        );
        assert!(
            pool.capsule(runtime)
                .unwrap()
                .local_state
                .pending
                .lock()
                .unwrap()
                .is_empty()
        );
        pool.shutdown();
    }

    #[test]
    fn contribution_cleanup_waits_for_its_accepted_replacement_acknowledgement() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(198);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let setup = execute_captured_fixture(
            &pool,
            runtime,
            "file:///fixtures/contribution-order.js",
            r#"
          globalThis.buffer = await editor.activeBuffer();
          globalThis.api = knot.workbench;
        "#,
        );
        let request = pool.receive_request().unwrap();
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(success::<declared::active_buffer>(Some(BufferHandle::new(
                3,
            )))),
        })
        .unwrap();
        assert_eq!(setup.wait().unwrap(), CommandOutcome::Completed);
        let work = pool
            .execute(
                runtime,
                "ordered-contribution-disposal.js",
                r#"
          (async () => {
            const replacement = buffer.contributions.replace([], { ifRevision: 0 });
            const cleanup = buffer.contributions.dispose();
            const acknowledged = cleanup.then(() => api.writeClipboardText("contribution cleanup"));
            await Promise.all([replacement, cleanup, acknowledged]);
          })()
        "#,
            )
            .unwrap();
        let replace = pool.receive_request().unwrap();
        let dispose = pool.receive_request().unwrap();
        assert!(
            replace
                .operation
                .arguments::<declared::replace_editor_contributions>()
                .is_some()
        );
        assert!(
            dispose
                .operation
                .arguments::<declared::dispose_editor_contributions>()
                .is_some()
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: dispose.id,
            result: Ok(success::<declared::dispose_editor_contributions>(())),
        })
        .unwrap();
        assert!(
            pool.receive_request_timeout(Duration::from_millis(20))
                .is_none()
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: replace.id,
            result: Ok(success::<declared::replace_editor_contributions>(())),
        })
        .unwrap();
        let acknowledged = pool.receive_request().unwrap();
        assert_eq!(
            acknowledged.operation,
            clipboard_operation("contribution cleanup")
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: acknowledged.id,
            result: Ok(clipboard_written()),
        })
        .unwrap();
        work.wait().unwrap();
        pool.shutdown();
    }

    #[test]
    fn native_json_transport_preserves_deep_plain_data_without_custom_conversion() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(199);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let execution = execute_captured_fixture(
            &pool,
            runtime,
            "file:///fixtures/plain-json.js",
            r#"
          const items = [42];
          Object.defineProperty(items, "0", { value: 42, enumerable: false });
          let data = { toJSON: "literal field", items, plain: Object.assign(Object.create(null), { enabled: true }) };
          for (let index = 0; index < 300; index++) data = { next: data };
          if ((await commands.invoke("plain-child", data)).kind !== "completed") throw new Error("plain JSON failed");
        "#,
        );
        let request = pool.receive_request().unwrap();
        let command = &request
            .operation
            .arguments::<declared::invoke_command>()
            .expect("command invocation")
            .command;
        let mut data = &command.arguments;
        for _ in 0..300 {
            let CommandArgumentValue::Object(object) = data else {
                panic!("lost JSON depth");
            };
            data = object.get("next").unwrap();
        }
        let CommandArgumentValue::Object(object) = data else {
            panic!("lost JSON leaf");
        };
        assert_eq!(
            object.get("toJSON"),
            Some(&CommandArgumentValue::String("literal field".into()))
        );
        assert_eq!(
            object.get("items"),
            Some(&CommandArgumentValue::Array(vec![
                CommandArgumentValue::Number(42.0)
            ]))
        );
        assert_eq!(
            object.get("plain"),
            Some(&CommandArgumentValue::Object(
                std::collections::BTreeMap::from([(
                    "enabled".into(),
                    CommandArgumentValue::Boolean(true)
                )])
            ))
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(success::<declared::invoke_command>(
                CommandInvokeDispatch::Outcome {
                    outcome: CommandOutcome::Completed,
                },
            )),
        })
        .unwrap();
        assert_eq!(execution.wait().unwrap(), CommandOutcome::Completed);
        pool.shutdown();
    }

    #[test]
    fn native_numeric_decoder_rejects_coercion_and_precision_loss() {
        let capsule = RuntimeCapsule::new(key(200), IsolateConfig::default()).unwrap();
        let data = capsule.data.lock().unwrap();
        let data = data.as_ref().unwrap();
        let mut locker = data.isolate.lock();
        let scope = pin!(v8::HandleScope::new(&mut *locker));
        let mut scope = scope.init();
        let context = v8::Local::new(&scope, &data.context);
        let scope = &mut v8::ContextScope::new(&mut scope, context);
        let values = [
            v8::String::new(scope, "1").unwrap().into(),
            v8::Boolean::new(scope, true).into(),
            v8::null(scope).into(),
            v8::undefined(scope).into(),
        ];
        for value in values {
            assert!(matches!(
                value_u64(scope, value, "coordinate"),
                Err(BoundaryError::Type(_))
            ));
        }
        for number in [
            -1.0,
            0.5,
            f64::NAN,
            f64::INFINITY,
            MAX_JS_INTEGER as f64 + 1.0,
        ] {
            let value = v8::Number::new(scope, number);
            assert!(matches!(
                value_u64(scope, value.into(), "coordinate"),
                Err(BoundaryError::Range(_))
            ));
        }
        let maximum = v8::Number::new(scope, MAX_JS_INTEGER as f64);
        assert_eq!(
            value_u64(scope, maximum.into(), "coordinate").unwrap(),
            MAX_JS_INTEGER
        );
    }

    #[test]
    fn native_dispatch_callbacks_are_not_available_to_extension_code() {
        let capsule = RuntimeCapsule::new(key(201), IsolateConfig::default()).unwrap();
        let report = capsule
            .execute(
                "private-dispatch.js",
                r#"
            ["__knotNativeBindings", "__knotDispatchBufferChange", "__knotInvokeCommand",
             "__knotCancelCommand", "__knotRequestTreeChildren", "__knotRequestCompletions"]
                .every((name) => !Object.hasOwn(globalThis, name))
        "#,
            )
            .unwrap();
        assert_eq!(&*report.value, "true");
    }

    #[test]
    fn private_dispatch_handles_ignore_author_globals_and_teardown_rejects_new_requests() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(202);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let first = execute_captured_fixture(
            &pool,
            runtime,
            "file:///fixtures/dispatch-handles.js",
            r#"
            globalThis.commandCalls = (globalThis.commandCalls ?? 0) + 1;
            globalThis.__knotInvokeCommand = () => { throw new Error("forged dispatch"); };
        "#,
        );
        assert_eq!(first.wait().unwrap(), CommandOutcome::Completed);
        let second = pool
            .invoke_command(
                runtime,
                CommandInvocation {
                    id: CommandInvocationId::new(901),
                    registration: CommandRegistrationId::new(900),
                    extension: runtime.extension,
                    lifecycle: runtime.lifecycle,
                    arguments: CommandArgumentValue::Null,
                    capture: None,
                },
                None,
            )
            .unwrap();
        assert_eq!(second.wait().unwrap(), CommandOutcome::Completed);
        assert_eq!(
            &*pool
                .execute(runtime, "verify-private-dispatch.js", "commandCalls")
                .unwrap()
                .wait()
                .unwrap()
                .value,
            "2"
        );
        pool.shutdown();

        let capsule = RuntimeCapsule::new(key(203), IsolateConfig::default()).unwrap();
        {
            let data = capsule.data.lock().unwrap();
            let data = data.as_ref().unwrap();
            let mut locker = data.isolate.lock();
            let scope = pin!(v8::HandleScope::new(&mut *locker));
            let mut scope = scope.init();
            let context = v8::Local::new(&scope, &data.context);
            let scope = &mut v8::ContextScope::new(&mut scope, context);
            capsule.status.store(DISPOSED, Ordering::Release);
            let request = v8::Function::new(scope, host_request_callback).unwrap();
            let operation = v8::String::new(scope, "writeClipboardText").unwrap();
            let text = v8::String::new(scope, "cleanup").unwrap();
            let result = request
                .call(
                    scope,
                    v8::undefined(scope).into(),
                    &[v8::null(scope).into(), operation.into(), text.into()],
                )
                .unwrap();
            let promise = v8::Local::<v8::Promise>::try_from(result).unwrap();
            promise.mark_as_handled();
            assert_eq!(promise.state(), v8::PromiseState::Rejected);
            let error = v8::Local::<v8::Object>::try_from(promise.result(scope)).unwrap();
            assert_eq!(string_property(scope, error, "code").unwrap(), "Cancelled");
            assert!(capsule.local_state.pending.lock().unwrap().is_empty());
        }
        capsule.dispose();
    }

    #[test]
    fn process_initialization_is_idempotent() {
        initialize();
        initialize();
    }

    #[test]
    fn capsule_preserves_globals_and_drains_microtasks() {
        let capsule = RuntimeCapsule::new(key(1), IsolateConfig::default()).unwrap();
        capsule
            .execute(
                "first.js",
                "globalThis.count = 41; Promise.resolve().then(() => count++);",
            )
            .unwrap();
        let report = capsule.execute("second.js", "count").unwrap();
        assert_eq!(&*report.value, "42");
    }

    #[test]
    fn scheduled_fixture_modules_resolve_static_imports_and_preserve_state() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(1);
        pool.load(runtime, IsolateConfig::default()).unwrap();

        pool.execute_fixture_module(
            runtime,
            "file:///fixtures/counter.js",
            "export let count = 0; export function increment() { count++; }",
        )
        .unwrap()
        .wait()
        .unwrap();
        pool.execute_fixture_module(
            runtime,
            "file:///fixtures/first.js",
            r#"
                import { count, increment } from "./counter.js";
                increment();
                globalThis.firstModuleCount = count;
            "#,
        )
        .unwrap()
        .wait()
        .unwrap();
        pool.execute_fixture_module(
            runtime,
            "file:///fixtures/second.js",
            r#"
                import { count, increment } from "./counter.js";
                increment();
                globalThis.secondModuleCount = count;
            "#,
        )
        .unwrap()
        .wait()
        .unwrap();

        assert_eq!(
            &*pool
                .execute(
                    runtime,
                    "verify.js",
                    "`${firstModuleCount}:${secondModuleCount}`"
                )
                .unwrap()
                .wait()
                .unwrap()
                .value,
            "1:2"
        );
        pool.shutdown();
    }

    #[test]
    fn public_facade_is_semantic_and_private_bindings_are_hidden() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(1);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        pool.execute_fixture_module(
            runtime,
            "file:///fixtures/facade.js",
            r#"
                import * as knot from "knot";
                if (typeof Deno !== "undefined") throw new Error("Deno is exposed");
                if (typeof globalThis.__knotNativeBindings !== "undefined") {
                    throw new Error("private native bindings are exposed");
                }
                if (Object.keys(knot).sort().join(",") !== "commands,documents,editor,keybinding,removeKeybinding,storage,windows,workbench,workspaces") {
                    throw new Error("unexpected public exports");
                }
                if (!Object.isFrozen(knot.editor) || !Object.isFrozen(knot.commands) || !Object.isFrozen(knot.workbench) || !Object.isFrozen(knot.storage)) {
                    throw new Error("facade objects are mutable");
                }
                if (Object.keys(knot.storage).sort().join(",") !== "delete,list,read,write") {
                    throw new Error("storage facade is incomplete");
                }
                if (typeof knot.editor.activeBuffer !== "function"
                    || typeof knot.keybinding !== "function"
                    || typeof knot.removeKeybinding !== "function"
                    || typeof knot.editor.registerCompletionProvider !== "function"
                    || typeof knot.commands.invalidArguments !== "function"
                    || typeof knot.commands.invoke !== "function"
                    || typeof knot.commands.register !== "function"
                    || typeof knot.workbench.registerView !== "function") {
                    throw new Error("facade shape is incomplete");
                }
            "#,
        )
        .unwrap()
        .wait()
        .unwrap();

        let unsupported = pool
            .execute_fixture_module(
                runtime,
                "file:///fixtures/unsupported.js",
                r#"
                    import * as knot from "knot"; const { workbench } = knot;
                    await workbench.writeClipboardText("wait");
                "#,
            )
            .unwrap();
        let request = pool.receive_request().unwrap();
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Err(test_failure("UnsupportedOperation")),
        })
        .unwrap();
        let unsupported = unsupported.wait().unwrap_err();
        assert_eq!(unsupported.kind(), RuntimeErrorKind::Rejection);
        assert!(unsupported.message().contains("UnsupportedOperation"));
        pool.shutdown();
    }

    #[test]
    fn buffer_requests_and_typed_responses_round_trip() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(40);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let execution = execute_captured_fixture(
            &pool,
            runtime,
            "file:///fixtures/buffers.js",
            r#"
                        const buffer = await editor.activeBuffer();
                    globalThis.snapshot = await buffer.snapshot({ startByteOffset: 1, endByteOffset: 7 });
                    globalThis.edit = await buffer.applyEdits([{
                      range: { startByteOffset: 2, endByteOffset: 5 }, text: "λ",
                    }], { ifRevision: 7 });
                    await buffer.contributions.replace([{
                      range: { startByteOffset: 0, endByteOffset: 2 },
                      decoration: "warning", gutter: "error", command: "fixture.command",
                    }], { ifRevision: 8 });
                    const disposable = await buffer.onDidChange(() => {});
                    await disposable.dispose();
                    await buffer.contributions.dispose();
                "#,
        );

        let active = pool.receive_request().unwrap();
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: active.id,
            result: Ok(success::<declared::active_buffer>(Some(BufferHandle::new(
                3,
            )))),
        })
        .unwrap();

        let request = pool.receive_request().unwrap();
        assert_eq!(
            request.operation,
            operation::<declared::snapshot>(declared::SnapshotArguments {
                buffer: BufferHandle::new(3),
                range: Some(ByteRange {
                    start_byte_offset: 1,
                    end_byte_offset: 7
                }),
            })
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(success::<declared::snapshot>(TextSnapshot {
                text: SnapshotText::from_utf8("é中z"),
                range: ByteRange {
                    start_byte_offset: 1,
                    end_byte_offset: 7,
                },
                revision: 7,
            })),
        })
        .unwrap();

        let request = pool.receive_request().unwrap();
        assert_eq!(
            request.operation,
            operation::<declared::apply_edits>(declared::EditArguments {
                buffer: BufferHandle::new(3),
                edits: vec![TextEdit {
                    range: ByteRange {
                        start_byte_offset: 2,
                        end_byte_offset: 5
                    },
                    text: "λ".into(),
                }],
                if_revision: 7,
            })
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(success::<declared::apply_edits>(8)),
        })
        .unwrap();

        let request = pool.receive_request().unwrap();
        assert_eq!(
            request.operation,
            operation::<declared::replace_editor_contributions>(declared::ContributionArguments {
                buffer: BufferHandle::new(3),
                contributions: vec![EditorContribution {
                    range: ByteRange {
                        start_byte_offset: 0,
                        end_byte_offset: 2
                    },
                    decoration: Some(DecorationToken::Warning),
                    gutter: Some(GutterToken::Error),
                    command: Some("fixture.command".into()),
                }],
                if_revision: 8,
            })
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(success::<declared::replace_editor_contributions>(())),
        })
        .unwrap();

        let request = pool.receive_request().unwrap();
        assert_eq!(
            request.operation,
            operation::<declared::subscribe_buffer_changes>(declared::BufferArguments {
                buffer: BufferHandle::new(3)
            })
        );
        let subscription = BufferSubscriptionId::new(12);
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(success::<declared::subscribe_buffer_changes>(subscription)),
        })
        .unwrap();
        let request = pool.receive_request().unwrap();
        assert_eq!(
            request.operation,
            operation::<declared::unsubscribe_buffer_changes>(declared::SubscriptionArguments {
                subscription
            })
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(success::<declared::unsubscribe_buffer_changes>(())),
        })
        .unwrap();
        let request = pool.receive_request().unwrap();
        assert_eq!(
            request.operation,
            operation::<declared::dispose_editor_contributions>(declared::BufferArguments {
                buffer: BufferHandle::new(3)
            })
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(success::<declared::dispose_editor_contributions>(())),
        })
        .unwrap();
        assert_eq!(execution.wait().unwrap(), CommandOutcome::Completed);
        pool.execute(runtime, "verify-buffers.js", r#"
            if (snapshot.text !== "é中z" || snapshot.revision !== 7) throw new Error("snapshot");
            if (snapshot.byteOffsetAtUtf16(1) !== 2 || snapshot.utf16OffsetAtByte(5) !== 2) throw new Error("unicode");
            if (edit.revision !== 8) throw new Error("edit");
        "#).unwrap().wait().unwrap();
        pool.shutdown();
    }

    #[test]
    fn external_utf16_snapshot_storage_is_shared_and_released_on_unload() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtimes = [key(41), key(42)];
        let text: Arc<[u16]> = "héllo".encode_utf16().collect::<Vec<_>>().into();
        let weak = Arc::downgrade(&text);
        for runtime in runtimes {
            pool.load(runtime, IsolateConfig::default()).unwrap();
            let execution = execute_captured_fixture(
                &pool,
                runtime,
                format!("file:///fixtures/external-{}.js", runtime.extension.value()),
                r#"
                                const buffer = await editor.activeBuffer();
                        globalThis.heldSnapshot = await buffer.snapshot();
                    "#,
            );
            let active = pool.receive_request().unwrap();
            pool.respond(HostResponse {
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                id: active.id,
                result: Ok(success::<declared::active_buffer>(Some(BufferHandle::new(
                    1,
                )))),
            })
            .unwrap();
            let request = pool.receive_request().unwrap();
            pool.respond(HostResponse {
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                id: request.id,
                result: Ok(success::<declared::snapshot>(TextSnapshot {
                    text: SnapshotText(Arc::clone(&text)),
                    range: ByteRange {
                        start_byte_offset: 0,
                        end_byte_offset: 6,
                    },
                    revision: 0,
                })),
            })
            .unwrap();
            assert_eq!(execution.wait().unwrap(), CommandOutcome::Completed);
        }
        drop(text);
        assert_eq!(weak.strong_count(), 2);
        pool.unload(runtimes[0]).unwrap();
        assert_eq!(weak.strong_count(), 1);
        pool.unload(runtimes[1]).unwrap();
        assert_eq!(weak.strong_count(), 0);
        pool.shutdown();
    }

    #[test]
    fn buffer_change_callbacks_are_serial_and_survive_listener_failure() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(44);
        let subscription = BufferSubscriptionId::new(9);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        pool.execute(
            runtime,
            "listener.js",
            r#"
            globalThis.listener = async (event) => {
              events.push(event.revision);
              if (event.revision === 1) throw new Error("expected listener failure");
            };
        "#,
        )
        .unwrap()
        .wait()
        .unwrap();
        install_buffer_listener(&pool, runtime, subscription, "file:///fixtures/listener.js");
        for revision in 1..=2 {
            pool.dispatch_buffer_change(
                runtime,
                subscription,
                BufferChange {
                    buffer: BufferHandle::new(3),
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
        pool.execute(
            runtime,
            "verify-events.js",
            r#"
            if (events.join(",") !== "1,2") throw new Error("events reordered");
        "#,
        )
        .unwrap()
        .wait()
        .unwrap();
        let metrics = pool.buffer_change_queue_metrics(runtime).unwrap();
        assert!(metrics.max_depth >= 1);
        assert!(metrics.max_enqueue_to_start_lag > Duration::ZERO);
        pool.shutdown();
    }

    #[test]
    fn finite_slow_buffer_change_burst_is_lossless_and_measured() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(45);
        let subscription = BufferSubscriptionId::new(10);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        pool.execute(
            runtime,
            "slow-listener.js",
            r#"
            globalThis.listener = async (event) => {
              if (event.revision === 1) await event.buffer.snapshot();
              events.push(event.revision);
            };
        "#,
        )
        .unwrap()
        .wait()
        .unwrap();
        install_buffer_listener(
            &pool,
            runtime,
            subscription,
            "file:///fixtures/slow-listener.js",
        );
        for revision in 1..=8 {
            pool.dispatch_buffer_change(
                runtime,
                subscription,
                BufferChange {
                    buffer: BufferHandle::new(3),
                    before_revision: revision - 1,
                    revision,
                    edits: vec![],
                },
            )
            .unwrap();
        }
        let request = pool
            .receive_request_timeout(Duration::from_secs(1))
            .unwrap();
        wait_for_state(&pool, runtime, ExtensionState::AwaitingHostWork);
        let queued = pool.buffer_change_queue_metrics(runtime).unwrap();
        assert!(queued.max_depth >= 7, "metrics: {queued:?}");
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(success::<declared::snapshot>(TextSnapshot {
                text: SnapshotText::from_utf8(""),
                range: ByteRange {
                    start_byte_offset: 0,
                    end_byte_offset: 0,
                },
                revision: 1,
            })),
        })
        .unwrap();
        pool.execute(
            runtime,
            "verify-burst.js",
            r#"
            if (events.join(",") !== "1,2,3,4,5,6,7,8") throw new Error("events lost");
        "#,
        )
        .unwrap()
        .wait()
        .unwrap();
        let metrics = pool.buffer_change_queue_metrics(runtime).unwrap();
        assert!(metrics.max_depth >= 7, "metrics: {metrics:?}");
        assert!(metrics.max_enqueue_to_start_lag > Duration::ZERO);
        pool.shutdown();
    }

    #[test]
    fn awaiting_buffer_listener_yields_single_worker_to_another_extension() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let waiting = key(46);
        let neighbor = key(47);
        let subscription = BufferSubscriptionId::new(11);
        pool.load(waiting, IsolateConfig::default()).unwrap();
        pool.load(neighbor, IsolateConfig::default()).unwrap();
        pool.execute(
            waiting,
            "async-listener.js",
            r#"
            globalThis.listener = async (event) => {
              await event.buffer.snapshot();
              events.push(event.revision);
            };
        "#,
        )
        .unwrap()
        .wait()
        .unwrap();
        install_buffer_listener(
            &pool,
            waiting,
            subscription,
            "file:///fixtures/async-listener.js",
        );
        pool.dispatch_buffer_change(
            waiting,
            subscription,
            BufferChange {
                buffer: BufferHandle::new(3),
                before_revision: 0,
                revision: 1,
                edits: vec![],
            },
        )
        .unwrap();
        let request = pool.receive_request().unwrap();
        wait_for_state(&pool, waiting, ExtensionState::AwaitingHostWork);
        assert_eq!(
            &*pool
                .execute(neighbor, "neighbor.js", "6 * 7")
                .unwrap()
                .wait()
                .unwrap()
                .value,
            "42"
        );
        pool.respond(HostResponse {
            extension: waiting.extension,
            lifecycle: waiting.lifecycle,
            id: request.id,
            result: Ok(success::<declared::snapshot>(TextSnapshot {
                text: SnapshotText::from_utf8(""),
                range: ByteRange {
                    start_byte_offset: 0,
                    end_byte_offset: 0,
                },
                revision: 1,
            })),
        })
        .unwrap();
        pool.execute(
            waiting,
            "verify-async-event.js",
            r#"
            if (events.join(",") !== "1") throw new Error("listener did not resume");
        "#,
        )
        .unwrap()
        .wait()
        .unwrap();
        pool.shutdown();
    }

    #[test]
    fn unloading_a_buffer_listener_awaiting_host_work_rejects_its_late_response() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(49);
        let subscription = BufferSubscriptionId::new(12);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        pool.execute(
            runtime,
            "unload-listener.js",
            "globalThis.listener = async (event) => { await event.buffer.snapshot(); };",
        )
        .unwrap()
        .wait()
        .unwrap();
        install_buffer_listener(
            &pool,
            runtime,
            subscription,
            "file:///fixtures/unload-listener.js",
        );
        pool.dispatch_buffer_change(
            runtime,
            subscription,
            BufferChange {
                buffer: BufferHandle::new(3),
                before_revision: 0,
                revision: 1,
                edits: vec![],
            },
        )
        .unwrap();
        let request = pool.receive_request().unwrap();
        wait_for_state(&pool, runtime, ExtensionState::AwaitingHostWork);

        pool.unload(runtime).unwrap();
        assert_eq!(
            pool.respond(HostResponse {
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                id: request.id,
                result: Ok(success::<declared::snapshot>(TextSnapshot {
                    text: SnapshotText::from_utf8(""),
                    range: ByteRange {
                        start_byte_offset: 0,
                        end_byte_offset: 0
                    },
                    revision: 1
                })),
            }),
            Err(RuntimeResponseError::WrongExtension)
        );
        pool.shutdown();
    }

    #[test]
    fn stale_lifecycle_cannot_dispatch_a_buffer_change() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let stale = key(48);
        pool.load(stale, IsolateConfig::default()).unwrap();
        pool.unload(stale).unwrap();
        let replacement = ExtensionKey::new(stale.extension, ExtensionLifecycleId::new(2));
        pool.load(replacement, IsolateConfig::default()).unwrap();
        assert!(matches!(
            pool.dispatch_buffer_change(
                stale,
                BufferSubscriptionId::new(1),
                BufferChange {
                    buffer: BufferHandle::new(1),
                    before_revision: 0,
                    revision: 1,
                    edits: vec![],
                },
            ),
            Err(RuntimePoolError::Scheduler(
                SchedulerError::StaleLifecycle { .. }
            ))
        ));
        pool.shutdown();
    }

    #[test]
    fn host_response_resumes_a_pending_root_and_drains_microtasks() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(1);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let execution = pool
            .execute_fixture_module(
                runtime,
                "file:///fixtures/request.js",
                r#"
                    import * as knot from "knot"; const { workbench } = knot;
                    const buffer = await workbench.writeClipboardText("wait");
                    await Promise.resolve();
                    globalThis.responseValue = buffer === undefined;
                "#,
            )
            .unwrap();
        let request = pool.receive_request().unwrap();
        assert_eq!(request.extension, runtime.extension);
        assert_eq!(request.lifecycle, runtime.lifecycle);
        assert_eq!(request.id, RequestId::new(1));
        assert_eq!(request.operation, clipboard_operation("wait"));
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(clipboard_written()),
        })
        .unwrap();
        execution.wait().unwrap();
        assert_eq!(
            &*pool
                .execute(runtime, "verify-response.js", "responseValue")
                .unwrap()
                .wait()
                .unwrap()
                .value,
            "true"
        );
        pool.shutdown();
    }

    #[test]
    fn declared_responses_preserve_structured_errors_precision_and_operation_pairing() {
        use crate::host::declarations::{DeclaredResponse, EncodedValue, HostError};
        #[derive(knot_handler_macros::HostError)]
        enum ProbeError {
            #[error(code = "ProbeFailed", message = "probe diagnostic")]
            RenamableVariant { revision: u64 },
        }
        let cases = [
            (
                "structured",
                DeclaredResponse {
                    operation: "writeClipboardText",
                    result: Err(ProbeError::RenamableVariant { revision: 7 }.into_failure()),
                },
                r#"if (error.name !== "KnotHostError" || error.code !== "ProbeFailed" || error.message !== "probe diagnostic" || error.details.revision !== 7) throw error;"#,
            ),
            (
                "precision",
                DeclaredResponse {
                    operation: "writeClipboardText",
                    result: Ok(Ok(EncodedValue::Json(
                        serde_json::json!({"revision": MAX_JS_INTEGER + 1}),
                    ))),
                },
                r#"if (error.name !== "KnotHostError" || error.code !== "UnsupportedOperation") throw error;"#,
            ),
            (
                "error-detail-precision",
                DeclaredResponse {
                    operation: "writeClipboardText",
                    result: Err(crate::host::declarations::HostFailure {
                        code: "ProbeFailed",
                        message: "probe diagnostic".into(),
                        details: serde_json::json!({"revision": MAX_JS_INTEGER + 1}),
                    }),
                },
                r#"if (error.code !== "UnsupportedOperation") throw error;"#,
            ),
            (
                "pairing",
                DeclaredResponse {
                    operation: "openDocument",
                    result: Ok(Ok(EncodedValue::Undefined)),
                },
                r#"if (error.code !== "UnsupportedOperation") throw error;"#,
            ),
        ];
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(719);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        for (name, response, check) in cases {
            let execution = pool
                .execute_fixture_module(
                    runtime,
                    format!("file:///fixtures/declared-{name}.js"),
                    format!(
                        r#"
                import {{ workbench }} from "knot";
                let rejected = false;
                try {{ await workbench.writeClipboardText("probe"); }}
                catch (error) {{ rejected = true; {check} }}
                if (!rejected) throw new Error("invalid native response accepted");
            "#
                    ),
                )
                .unwrap();
            let request = pool.receive_request().unwrap();
            pool.respond(HostResponse {
                extension: request.extension,
                lifecycle: request.lifecycle,
                id: request.id,
                result: Ok(response),
            })
            .unwrap();
            execution.wait().unwrap();
        }
        pool.shutdown();
    }

    #[test]
    fn host_error_response_rejects_the_request_and_propagates_from_the_root() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(1);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let execution = pool
            .execute_fixture_module(
                runtime,
                "file:///fixtures/request-error.js",
                "import * as knot from 'knot'; const { workbench } = knot; await workbench.writeClipboardText('wait');",
            )
            .unwrap();
        let request = pool.receive_request().unwrap();
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Err(test_failure("RevisionConflict")),
        })
        .unwrap();
        let error = execution.wait().unwrap_err();
        assert_eq!(error.kind(), RuntimeErrorKind::Rejection);
        assert!(error.message().contains("RevisionConflict"));
        assert_eq!(error.source(), Some("file:///fixtures/request-error.js"));
        assert_eq!(
            &*pool
                .execute(runtime, "recover-after-host-error.js", "6 * 7")
                .unwrap()
                .wait()
                .unwrap()
                .value,
            "42"
        );
        pool.shutdown();
    }

    #[test]
    fn multiple_host_requests_settle_out_of_order_exactly_once() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(1);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let execution = pool
            .execute_fixture_module(
                runtime,
                "file:///fixtures/concurrent-requests.js",
                r#"
                    import * as knot from "knot"; const { workbench } = knot;
                    globalThis.responseOrder = [];
                    await Promise.all([
                      workbench.writeClipboardText("wait").then(() => responseOrder.push(1)),
                      workbench.writeClipboardText("wait").then(() => responseOrder.push(2)),
                    ]);
                    globalThis.responsesAreDistinct = responseOrder.join(",") === "2,1";
                "#,
            )
            .unwrap();
        let first = pool.receive_request().unwrap();
        let second = pool.receive_request().unwrap();
        assert_eq!(first.id, RequestId::new(1));
        assert_eq!(second.id, RequestId::new(2));
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: second.id,
            result: Ok(clipboard_written()),
        })
        .unwrap();
        assert_eq!(
            pool.respond(HostResponse {
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                id: second.id,
                result: Ok(clipboard_written()),
            }),
            Err(RuntimeResponseError::UnknownRequest)
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: first.id,
            result: Ok(clipboard_written()),
        })
        .unwrap();
        execution.wait().unwrap();
        assert_eq!(
            &*pool
                .execute(runtime, "verify-order.js", "responsesAreDistinct")
                .unwrap()
                .wait()
                .unwrap()
                .value,
            "true"
        );
        assert_eq!(
            pool.respond(HostResponse {
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                id: first.id,
                result: Ok(clipboard_written()),
            }),
            Err(RuntimeResponseError::UnknownRequest)
        );
        pool.shutdown();
    }

    #[test]
    fn root_rejection_rejects_and_removes_sibling_request_resolvers() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(1);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let execution = pool
            .execute_fixture_module(
                runtime,
                "file:///fixtures/rejected-siblings.js",
                r#"
                    import * as knot from "knot"; const { workbench } = knot;
                    await Promise.all([workbench.writeClipboardText("wait"), workbench.writeClipboardText("wait")]);
                "#,
            )
            .unwrap();
        let first = pool.receive_request().unwrap();
        let second = pool.receive_request().unwrap();
        wait_for_state(&pool, runtime, ExtensionState::AwaitingHostWork);
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: first.id,
            result: Err(test_failure("BufferClosed")),
        })
        .unwrap();
        let error = execution.wait().unwrap_err();
        assert_eq!(error.kind(), RuntimeErrorKind::Rejection);
        assert!(error.message().contains("BufferClosed"));
        assert_eq!(
            pool.respond(HostResponse {
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                id: second.id,
                result: Ok(clipboard_written()),
            }),
            Err(RuntimeResponseError::UnknownRequest)
        );
        pool.shutdown();
    }

    #[test]
    fn response_identity_is_validated_without_consuming_the_request() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(1);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let execution = pool
            .execute_fixture_module(
                runtime,
                "file:///fixtures/identity.js",
                "import * as knot from 'knot'; const { workbench } = knot; await workbench.writeClipboardText('wait');",
            )
            .unwrap();
        let request = pool.receive_request().unwrap();
        for response in [
            HostResponse {
                extension: ExtensionId::new(99),
                lifecycle: runtime.lifecycle,
                id: request.id,
                result: Ok(clipboard_written()),
            },
            HostResponse {
                extension: runtime.extension,
                lifecycle: ExtensionLifecycleId::new(99),
                id: request.id,
                result: Ok(clipboard_written()),
            },
            HostResponse {
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                id: RequestId::new(99),
                result: Ok(clipboard_written()),
            },
        ] {
            assert!(pool.respond(response).is_err());
        }
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(clipboard_written()),
        })
        .unwrap();
        execution.wait().unwrap();
        pool.shutdown();
    }

    #[test]
    fn single_worker_progresses_while_another_extension_awaits_host_work() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let waiting = key(1);
        let neighbor = key(2);
        pool.load(waiting, IsolateConfig::default()).unwrap();
        pool.load(neighbor, IsolateConfig::default()).unwrap();
        let delayed = pool
            .execute_fixture_module(
                waiting,
                "file:///fixtures/delayed.js",
                "import * as knot from 'knot'; const { workbench } = knot; await workbench.writeClipboardText('wait');",
            )
            .unwrap();
        let request = pool.receive_request().unwrap();
        assert_eq!(
            &*pool
                .execute(neighbor, "neighbor.js", "6 * 7")
                .unwrap()
                .wait()
                .unwrap()
                .value,
            "42"
        );
        pool.respond(HostResponse {
            extension: waiting.extension,
            lifecycle: waiting.lifecycle,
            id: request.id,
            result: Ok(clipboard_written()),
        })
        .unwrap();
        delayed.wait().unwrap();
        pool.shutdown();
    }

    #[test]
    fn sequential_host_requests_keep_the_root_active_until_the_last_response() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(1);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let execution = pool
            .execute_fixture_module(
                runtime,
                "file:///fixtures/sequential.js",
                r#"
                    import * as knot from "knot"; const { workbench } = knot;
                    await Promise.resolve();
                    await workbench.writeClipboardText("wait");
                    globalThis.firstResponse = true;
                    await workbench.writeClipboardText("wait");
                    globalThis.secondResponse = true;
                "#,
            )
            .unwrap();
        let first = pool.receive_request().unwrap();
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: first.id,
            result: Ok(clipboard_written()),
        })
        .unwrap();
        let second = pool.receive_request().unwrap();
        assert_eq!(second.id, RequestId::new(2));
        wait_for_state(&pool, runtime, ExtensionState::AwaitingHostWork);
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: second.id,
            result: Ok(clipboard_written()),
        })
        .unwrap();
        execution.wait().unwrap();
        assert_eq!(
            &*pool
                .execute(
                    runtime,
                    "verify-sequential.js",
                    "`${firstResponse}:${secondResponse}`"
                )
                .unwrap()
                .wait()
                .unwrap()
                .value,
            "true:true"
        );
        pool.shutdown();
    }

    #[test]
    fn mismatched_typed_response_rejects_the_javascript_request() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(1);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let execution = pool
            .execute_fixture_module(
                runtime,
                "file:///fixtures/mismatched-response.js",
                "import * as knot from 'knot'; const { workbench } = knot; await workbench.writeClipboardText('wait');",
            )
            .unwrap();
        let request = pool.receive_request().unwrap();
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(success::<declared::dispose_editor_contributions>(())),
        })
        .unwrap();
        let error = execution.wait().unwrap_err();
        assert_eq!(error.kind(), RuntimeErrorKind::Rejection);
        assert!(error.message().contains("UnsupportedOperation"));
        pool.shutdown();
    }

    #[test]
    fn unload_settles_waiting_execution_and_rejects_stale_responses() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let first = key(1);
        pool.load(first, IsolateConfig::default()).unwrap();
        let execution = pool
            .execute_fixture_module(
                first,
                "file:///fixtures/unload-pending.js",
                r#"
                    import * as knot from "knot"; const { workbench } = knot;
                    await Promise.all([workbench.writeClipboardText("wait"), workbench.writeClipboardText("wait")]);
                "#,
            )
            .unwrap();
        let first_request = pool.receive_request().unwrap();
        let _second_request = pool.receive_request().unwrap();
        wait_for_state(&pool, first, ExtensionState::AwaitingHostWork);
        pool.unload(first).unwrap();
        let error = execution.wait().unwrap_err();
        assert_eq!(error.kind(), RuntimeErrorKind::Cancelled);
        assert_eq!(
            pool.respond(HostResponse {
                extension: first.extension,
                lifecycle: first.lifecycle,
                id: first_request.id,
                result: Ok(clipboard_written()),
            }),
            Err(RuntimeResponseError::WrongExtension)
        );

        let replacement = ExtensionKey::new(first.extension, ExtensionLifecycleId::new(2));
        pool.load(replacement, IsolateConfig::default()).unwrap();
        let replacement_execution = pool
            .execute_fixture_module(
                replacement,
                "file:///fixtures/replacement.js",
                "import * as knot from 'knot'; const { workbench } = knot; await workbench.writeClipboardText('wait');",
            )
            .unwrap();
        let replacement_request = pool.receive_request().unwrap();
        assert_eq!(replacement_request.id, RequestId::new(1));
        assert_eq!(
            pool.respond(HostResponse {
                extension: first.extension,
                lifecycle: first.lifecycle,
                id: replacement_request.id,
                result: Ok(clipboard_written()),
            }),
            Err(RuntimeResponseError::WrongExtension)
        );
        pool.respond(HostResponse {
            extension: replacement.extension,
            lifecycle: replacement.lifecycle,
            id: replacement_request.id,
            result: Ok(clipboard_written()),
        })
        .unwrap();
        replacement_execution.wait().unwrap();
        pool.shutdown();
    }

    #[test]
    fn shutdown_and_forced_termination_settle_pending_executions() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(1);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let execution = pool
            .execute_fixture_module(
                runtime,
                "file:///fixtures/shutdown-pending.js",
                "import * as knot from 'knot'; const { workbench } = knot; await workbench.writeClipboardText('wait');",
            )
            .unwrap();
        pool.receive_request().unwrap();
        wait_for_state(&pool, runtime, ExtensionState::AwaitingHostWork);
        pool.shutdown();
        assert_eq!(
            execution.wait().unwrap_err().kind(),
            RuntimeErrorKind::Cancelled
        );

        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(2);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let execution = pool
            .execute_fixture_module(
                runtime,
                "file:///fixtures/terminated-pending.js",
                "import * as knot from 'knot'; const { workbench } = knot; await workbench.writeClipboardText('wait');",
            )
            .unwrap();
        let request = pool.receive_request().unwrap();
        wait_for_state(&pool, runtime, ExtensionState::AwaitingHostWork);
        assert!(pool.termination_handle(runtime).unwrap().terminate());
        assert_eq!(
            execution.wait().unwrap_err().kind(),
            RuntimeErrorKind::Terminated
        );
        assert_eq!(
            pool.respond(HostResponse {
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                id: request.id,
                result: Ok(clipboard_written()),
            }),
            Err(RuntimeResponseError::WrongExtension)
        );
        pool.shutdown();
    }

    #[test]
    fn awaited_queued_executions_report_unload_cancellation() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(1);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let waiting = pool
            .execute_fixture_module(
                runtime,
                "file:///fixtures/awaited-queued.js",
                "import * as knot from 'knot'; const { workbench } = knot; await workbench.writeClipboardText('wait');",
            )
            .unwrap();
        pool.receive_request().unwrap();
        wait_for_state(&pool, runtime, ExtensionState::AwaitingHostWork);

        let script = pool.execute(runtime, "queued.js", "42").unwrap();
        let command = pool
            .invoke_command(
                runtime,
                CommandInvocation {
                    id: CommandInvocationId::new(1),
                    registration: CommandRegistrationId::new(1),
                    extension: runtime.extension,
                    lifecycle: runtime.lifecycle,
                    arguments: CommandArgumentValue::Null,
                    capture: None,
                },
                None,
            )
            .unwrap();
        let provider = pool
            .request_tree_children(
                runtime,
                TreeChildrenRequest {
                    registration: TreeProviderRegistrationId::new(1),
                    window: "1".into(),
                    view: crate::host::protocol::ViewId::new(1),
                    parent_id: None,
                    generation: 1,
                },
            )
            .unwrap();

        pool.unload(runtime).unwrap();
        assert_eq!(
            pollster::block_on(waiting).unwrap_err().kind(),
            RuntimeErrorKind::Cancelled
        );
        assert_eq!(
            pollster::block_on(script).unwrap_err().kind(),
            RuntimeErrorKind::Cancelled
        );
        assert_eq!(
            pollster::block_on(command).unwrap(),
            CommandOutcome::Cancelled
        );
        assert_eq!(
            pollster::block_on(provider).unwrap_err().kind(),
            RuntimeErrorKind::Cancelled
        );
        pool.shutdown();
    }

    #[test]
    fn awaited_queued_executions_report_shutdown_and_fatal_failure() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(1);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let waiting = pool
            .execute_fixture_module(
                runtime,
                "file:///fixtures/awaited-shutdown.js",
                "import * as knot from 'knot'; const { workbench } = knot; await workbench.writeClipboardText('wait');",
            )
            .unwrap();
        pool.receive_request().unwrap();
        wait_for_state(&pool, runtime, ExtensionState::AwaitingHostWork);
        let queued = pool.execute(runtime, "queued-shutdown.js", "42").unwrap();
        pool.shutdown();
        assert_eq!(
            pollster::block_on(waiting).unwrap_err().kind(),
            RuntimeErrorKind::Cancelled
        );
        assert_eq!(
            pollster::block_on(queued).unwrap_err().kind(),
            RuntimeErrorKind::Cancelled
        );

        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(2);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let waiting = pool
            .execute_fixture_module(
                runtime,
                "file:///fixtures/awaited-fatal.js",
                "import * as knot from 'knot'; const { workbench } = knot; await workbench.writeClipboardText('wait');",
            )
            .unwrap();
        pool.receive_request().unwrap();
        wait_for_state(&pool, runtime, ExtensionState::AwaitingHostWork);
        let queued = pool.execute(runtime, "queued-fatal.js", "42").unwrap();
        assert!(pool.termination_handle(runtime).unwrap().terminate());
        assert_eq!(
            pollster::block_on(waiting).unwrap_err().kind(),
            RuntimeErrorKind::Terminated
        );
        assert_eq!(
            pollster::block_on(queued).unwrap_err().kind(),
            RuntimeErrorKind::LifecycleFailed
        );
        pool.shutdown();
    }

    #[test]
    fn facade_maps_every_host_error_to_a_stable_javascript_name() {
        let capsule = RuntimeCapsule::new(key(1), IsolateConfig::default()).unwrap();
        for (index, wire_name) in super::super::declarations::registry()
            .error_codes()
            .into_iter()
            .enumerate()
        {
            let expected_name = "KnotHostError";
            capsule.set_test_host_error(test_failure(wire_name));
            let report = capsule
                .execute_fixture_module(
                    &format!("file:///fixtures/host-error-{index}.js"),
                    r#"
                        import * as knot from "knot"; const { workbench } = knot;
                        try {
                            await workbench.writeClipboardText("wait");
                        } catch (error) {
                            globalThis.hostErrorName = error.name;
                            globalThis.hostErrorCode = error.code;
                        }
                    "#,
                )
                .unwrap();
            assert_eq!(&*report.value, "undefined");
            assert_eq!(
                &*capsule
                    .execute("host-error-name.js", "hostErrorName")
                    .unwrap()
                    .value,
                expected_name
            );
            assert_eq!(
                &*capsule
                    .execute("host-error-code.js", "hostErrorCode")
                    .unwrap()
                    .value,
                wire_name
            );
        }

        capsule
            .execute_fixture_module(
                "file:///fixtures/invalid-arguments.js",
                r#"
                    import * as knot from "knot"; const { commands } = knot;
                    try { commands.invalidArguments("expected"); }
                    catch (error) { globalThis.hostErrorName = error.name; }
                "#,
            )
            .unwrap();
        assert_eq!(
            &*capsule
                .execute("invalid-arguments-name.js", "hostErrorName")
                .unwrap()
                .value,
            "InvalidCommandArgumentsError"
        );
    }

    #[test]
    fn fixture_module_failures_are_scoped_and_classified() {
        let capsule = RuntimeCapsule::new(key(1), IsolateConfig::default()).unwrap();

        let invalid = capsule.execute_fixture_module("not a URL", "").unwrap_err();
        assert_eq!(invalid.kind(), RuntimeErrorKind::InvalidModuleSpecifier);

        let invalid_import = capsule
            .execute_fixture_module(
                "file:///fixtures/invalid-import.js",
                "import 'bare-specifier';",
            )
            .unwrap_err();
        assert_eq!(invalid_import.kind(), RuntimeErrorKind::ModuleResolution);
        assert!(
            invalid_import
                .message()
                .contains("invalid fixture module specifier")
        );

        let missing = capsule
            .execute_fixture_module(
                "file:///fixtures/missing-import.js",
                "import './missing.js';",
            )
            .unwrap_err();
        assert_eq!(missing.kind(), RuntimeErrorKind::ModuleResolution);
        assert!(missing.message().contains("fixture module not found"));

        let private = capsule
            .execute_fixture_module(
                "file:///fixtures/private-import.js",
                "import { activeBuffer } from 'knot:bootstrap'; void activeBuffer;",
            )
            .unwrap_err();
        assert_eq!(private.kind(), RuntimeErrorKind::ModuleResolution);
        assert!(private.message().contains("not importable by extensions"));

        let removed = capsule
            .execute_fixture_module(
                "file:///fixtures/removed-import.js",
                "import 'knot:editor';",
            )
            .unwrap_err();
        assert_eq!(removed.kind(), RuntimeErrorKind::ModuleResolution);
        assert!(removed.message().contains("module not found"));

        let syntax = capsule
            .execute_fixture_module("file:///fixtures/syntax.js", "export const = 1;")
            .unwrap_err();
        assert_eq!(syntax.kind(), RuntimeErrorKind::Compilation);
        assert_eq!(syntax.source(), Some("file:///fixtures/syntax.js"));

        let thrown = capsule
            .execute_fixture_module(
                "file:///fixtures/thrown.js",
                "throw new Error('expected module throw');",
            )
            .unwrap_err();
        assert_eq!(thrown.kind(), RuntimeErrorKind::Rejection);
        assert!(thrown.message().contains("expected module throw"));

        let rejected = capsule
            .execute_fixture_module(
                "file:///fixtures/rejected.js",
                "await Promise.reject(new Error('expected module rejection'));",
            )
            .unwrap_err();
        assert_eq!(rejected.kind(), RuntimeErrorKind::Rejection);
        assert!(rejected.message().contains("expected module rejection"));

        assert_eq!(
            &*capsule.execute("recover.js", "6 * 7").unwrap().value,
            "42"
        );
    }

    #[test]
    fn exceptions_and_rejections_are_scoped_to_a_turn() {
        let capsule = RuntimeCapsule::new(key(1), IsolateConfig::default()).unwrap();
        let compilation = capsule.execute("syntax.js", "const =").unwrap_err();
        assert_eq!(compilation.kind(), RuntimeErrorKind::Compilation);
        assert_eq!(compilation.source(), Some("syntax.js"));

        let thrown = capsule
            .execute(
                "throw.js",
                "globalThis.afterThrow = 0; Promise.resolve().then(() => afterThrow = 1); throw new Error('expected throw')",
            )
            .unwrap_err();
        assert_eq!(thrown.kind(), RuntimeErrorKind::Exception);
        assert!(thrown.message().contains("expected throw"));
        assert_eq!(thrown.source(), Some("throw.js"));
        assert_eq!(thrown.line(), Some(1));
        assert_eq!(thrown.column(), Some(74));
        assert!(
            thrown
                .stack()
                .is_some_and(|stack| stack.contains("throw.js"))
        );
        assert_eq!(
            &*capsule
                .execute("after-throw.js", "afterThrow")
                .unwrap()
                .value,
            "1"
        );

        let rejected = capsule
            .execute(
                "reject.js",
                "Promise.reject(new Error('expected rejection one')); Promise.reject(new Error('expected rejection two'))",
            )
            .unwrap_err();
        assert_eq!(rejected.kind(), RuntimeErrorKind::Rejection);
        assert!(rejected.message().contains("expected rejection one"));
        assert!(rejected.message().contains("expected rejection two"));
        assert_eq!(rejected.source(), Some("reject.js"));
        assert_eq!(rejected.line(), Some(1));

        assert_eq!(
            &*capsule.execute("recover.js", "6 * 7").unwrap().value,
            "42"
        );
    }

    #[test]
    fn external_utf16_storage_is_retained_until_isolate_disposal() {
        let text: Arc<[u16]> = "héllo".encode_utf16().collect::<Vec<_>>().into();
        let weak = Arc::downgrade(&text);
        let first = RuntimeCapsule::new(key(1), IsolateConfig::default()).unwrap();
        let second = RuntimeCapsule::new(key(2), IsolateConfig::default()).unwrap();
        first
            .hold_external_utf16("held", Arc::clone(&text))
            .unwrap();
        second
            .hold_external_utf16("held", Arc::clone(&text))
            .unwrap();
        drop(text);
        assert_eq!(weak.strong_count(), 2);
        assert_eq!(&*first.execute("read.js", "held").unwrap().value, "héllo");
        first.dispose();
        assert_eq!(weak.strong_count(), 1);
        second.dispose();
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn pooled_capsule_moves_between_workers_and_retains_state() {
        enum Work {
            Execute {
                capsule: Arc<RuntimeCapsule>,
                source: &'static str,
                result: mpsc::Sender<ExecutionReport>,
            },
            Block {
                started: mpsc::Sender<ThreadId>,
                release: mpsc::Receiver<()>,
            },
        }

        initialize();
        let pool = SchedulerPool::new(
            PoolConfig::new(NonZeroUsize::new(2).unwrap()),
            |turn: Turn<Work>| match turn.work {
                Work::Execute {
                    capsule,
                    source,
                    result,
                } => {
                    result
                        .send(capsule.execute("movement.js", source).unwrap())
                        .unwrap();
                    TurnOutcome::Completed
                }
                Work::Block { started, release } => {
                    started.send(std::thread::current().id()).unwrap();
                    release.recv().unwrap();
                    TurnOutcome::Completed
                }
            },
        );
        let handle = pool.handle();
        let capsule = Arc::new(RuntimeCapsule::new(key(1), IsolateConfig::default()).unwrap());
        for admitted in [key(1), key(2), key(3)] {
            handle.admit(admitted).unwrap();
            handle.finish_loading(admitted, Ok(())).unwrap();
        }

        let (first_sender, first_result) = mpsc::channel();
        let (_, first_completion) = handle
            .enqueue_root(
                key(1),
                Work::Execute {
                    capsule: Arc::clone(&capsule),
                    source: "globalThis.count = 1; count",
                    result: first_sender,
                },
            )
            .unwrap();
        let first_worker = first_result.recv().unwrap().worker;
        assert_eq!(
            first_completion.recv().unwrap(),
            CompletionOutcome::Completed
        );

        let blockers: Vec<_> = [key(2), key(3)]
            .into_iter()
            .map(|key| {
                let (started_sender, started) = mpsc::channel();
                let (release, release_receiver) = mpsc::channel();
                let (_, completion) = handle
                    .enqueue_root(
                        key,
                        Work::Block {
                            started: started_sender,
                            release: release_receiver,
                        },
                    )
                    .unwrap();
                (started, release, completion)
            })
            .collect();
        let blocker_workers: Vec<_> = blockers
            .iter()
            .map(|(started, _, _)| started.recv().unwrap())
            .collect();
        assert_eq!(
            blocker_workers
                .iter()
                .copied()
                .collect::<HashSet<_>>()
                .len(),
            2
        );
        let free_index = blocker_workers
            .iter()
            .position(|worker| *worker != first_worker)
            .unwrap();
        blockers[free_index].1.send(()).unwrap();
        assert_eq!(
            blockers[free_index].2.recv().unwrap(),
            CompletionOutcome::Completed
        );

        let (second_sender, second_result) = mpsc::channel();
        let (_, second_completion) = handle
            .enqueue_root(
                key(1),
                Work::Execute {
                    capsule: Arc::clone(&capsule),
                    source: "++count",
                    result: second_sender,
                },
            )
            .unwrap();
        let second = second_result.recv().unwrap();
        assert_ne!(second.worker, first_worker);
        assert_eq!(&*second.value, "2");
        assert_eq!(
            second_completion.recv().unwrap(),
            CompletionOutcome::Completed
        );

        blockers[1 - free_index].1.send(()).unwrap();
        assert_eq!(
            blockers[1 - free_index].2.recv().unwrap(),
            CompletionOutcome::Completed
        );
        pool.shutdown();
    }

    #[test]
    fn movement_stress_alternates_workers_before_disposal() {
        type Command = Option<(&'static str, mpsc::Sender<ExecutionReport>)>;

        let capsule = Arc::new(RuntimeCapsule::new(key(1), IsolateConfig::default()).unwrap());
        capsule.execute("init.js", "globalThis.count = 0").unwrap();
        let mut workers = Vec::new();
        for index in 0..2 {
            let (sender, receiver) = mpsc::channel::<Command>();
            let capsule = Arc::clone(&capsule);
            let worker = std::thread::Builder::new()
                .name(format!("knot-movement-stress-{index}"))
                .spawn(move || {
                    while let Some((source, result)) = receiver.recv().unwrap() {
                        result
                            .send(capsule.execute("movement-stress.js", source).unwrap())
                            .unwrap();
                    }
                })
                .unwrap();
            workers.push((sender, worker));
        }

        let mut worker_ids = HashSet::new();
        for index in 0..500 {
            let (result_sender, result) = mpsc::channel();
            workers[index % 2]
                .0
                .send(Some(("++count", result_sender)))
                .unwrap();
            worker_ids.insert(result.recv().unwrap().worker);
        }
        assert_eq!(worker_ids.len(), 2);
        assert_eq!(
            &*capsule.execute("result.js", "count").unwrap().value,
            "500"
        );
        for (sender, worker) in workers {
            sender.send(None).unwrap();
            worker.join().unwrap();
        }
        capsule.dispose();
        assert_eq!(
            capsule.execute("disposed.js", "count").unwrap_err().kind(),
            RuntimeErrorKind::Disposed
        );
    }

    #[test]
    fn independent_isolates_execute_javascript_concurrently() {
        let pool = RuntimePool::new(PoolConfig::new(NonZeroUsize::new(2).unwrap()));
        let first = key(1);
        let second = key(2);
        pool.load(first, IsolateConfig::default()).unwrap();
        pool.load(second, IsolateConfig::default()).unwrap();
        let barrier = Arc::new(TestGate::default());
        pool.capsule(first)
            .unwrap()
            .install_test_barrier(Arc::clone(&barrier));
        pool.capsule(second)
            .unwrap()
            .install_test_barrier(Arc::clone(&barrier));

        let first_execution = pool
            .execute(first, "first.js", "__knotTestBarrier(); 1")
            .unwrap();
        let second_execution = pool
            .execute(second, "second.js", "__knotTestBarrier(); 2")
            .unwrap();
        let first_report = first_execution.wait().unwrap();
        let second_report = second_execution.wait().unwrap();
        assert_ne!(first_report.worker, second_report.worker);
        assert_eq!((&*first_report.value, &*second_report.value), ("1", "2"));
        assert!(!barrier.timed_out.load(Ordering::Acquire));
        pool.shutdown();
    }

    fn install_semantic_providers(
        pool: &RuntimePool,
        runtime: ExtensionKey,
        tree_registration: TreeProviderRegistrationId,
        completion_registration: CompletionProviderRegistrationId,
    ) {
        let setup = pool
            .execute_fixture_module(
                runtime,
                "file:///fixtures/semantic-providers.js",
                r#"
                    import * as knot from "knot"; const { editor, workbench } = knot;
                    for (const name of ["__knotRequestTreeChildren", "__knotRequestCompletions"]) {
                      if (Object.hasOwn(globalThis, name)) throw new Error("private provider dispatch exposed");
                      globalThis[name] = () => { throw new Error("untrusted provider dispatch replacement"); };
                    }
                    globalThis.providerEvents = [];
                    globalThis.treeProvider = await workbench.registerView({
                      id: "example.outline",
                      title: "Outline",
                      content: { kind: "tree", provider: {
                        async getChildren(parentId) {
                          globalThis.providerEvents.push(`tree-${parentId ?? "root"}-start`);
                          if (parentId === "await") await workbench.writeClipboardText("tree wait");
                          if (parentId === "failure") throw new Error("expected tree failure");
                          globalThis.providerEvents.push(`tree-${parentId ?? "root"}-end`);
                          return [{
                            id: parentId === null ? "root" : `${parentId}.child`,
                            label: "Tree item",
                            description: "fixture",
                            icon: "symbol",
                            collapsibleState: parentId === null ? "expanded" : "none",
                          }];
                        },
                      } },
                    });
                    globalThis.completionProvider = await editor.registerCompletionProvider(
                      "fixture",
                      {
                        async provideCompletions(context) {
                          globalThis.providerEvents.push(`completion-${context.prefix}`);
                          if (context.prefix === "await") await context.buffer.snapshot();
                          if (context.prefix === "failure") {
                            throw new Error("expected completion failure");
                          }
                          return [{ label: `${context.prefix}Item`, insertText: "inserted" }];
                        },
                      },
                    );
                "#,
            )
            .unwrap();
        let tree = pool
            .receive_request_timeout(Duration::from_secs(1))
            .unwrap_or_else(|| panic!("no registerView request: {:?}", pool.diagnostics()));
        assert_eq!(
            tree.operation,
            operation::<declared::register_tree_provider>(declared::TreeArguments {
                id: "example.outline".into(),
                title: "Outline".into(),
                location: crate::app::tool_layout::StoredLocation::Left,
                order: None,
                visible: true,
            })
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: tree.id,
            result: Ok(success::<declared::register_tree_provider>(
                tree_registration,
            )),
        })
        .unwrap();
        let completion = pool
            .receive_request_timeout(Duration::from_secs(1))
            .unwrap();
        assert_eq!(
            completion.operation,
            operation::<declared::register_completion_provider>(declared::CompletionArguments {
                label: "fixture".into()
            })
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: completion.id,
            result: Ok(success::<declared::register_completion_provider>(
                completion_registration,
            )),
        })
        .unwrap();
        setup.wait().unwrap();
    }

    #[test]
    fn semantic_provider_callbacks_preserve_typed_generations_and_recover_from_failures() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(61);
        let tree_registration = TreeProviderRegistrationId::new(91);
        let completion_registration = CompletionProviderRegistrationId::new(92);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        install_semantic_providers(&pool, runtime, tree_registration, completion_registration);

        let tree = pool
            .request_tree_children(
                runtime,
                TreeChildrenRequest {
                    registration: tree_registration,
                    window: "1".into(),
                    view: crate::host::protocol::ViewId::new(1),
                    parent_id: None,
                    generation: 8,
                },
            )
            .unwrap()
            .wait()
            .unwrap();
        assert_eq!(tree.registration, tree_registration);
        assert_eq!(tree.generation, 8);
        let item = &tree.result.unwrap()[0];
        assert_eq!(item.id, "root");
        assert_eq!(item.icon, Some(crate::host::protocol::TreeIcon::Symbol));
        assert_eq!(
            item.collapsible_state,
            crate::host::protocol::TreeCollapsibleState::Expanded
        );

        let tree_failure = pool
            .request_tree_children(
                runtime,
                TreeChildrenRequest {
                    registration: tree_registration,
                    window: "1".into(),
                    view: crate::host::protocol::ViewId::new(1),
                    parent_id: Some("failure".into()),
                    generation: 9,
                },
            )
            .unwrap()
            .wait()
            .unwrap();
        assert!(
            tree_failure
                .result
                .unwrap_err()
                .message
                .contains("expected tree failure")
        );

        let completion = pool
            .request_completions(
                runtime,
                CompletionRequest {
                    registration: completion_registration,
                    buffer: BufferHandle::new(3),
                    revision: 11,
                    cursor_byte_offset: 4,
                    prefix: "pre".into(),
                    generation: 12,
                },
            )
            .unwrap()
            .wait()
            .unwrap();
        assert_eq!(completion.registration, completion_registration);
        assert_eq!(completion.revision, 11);
        assert_eq!(completion.generation, 12);
        assert_eq!(completion.result.unwrap()[0].label, "preItem");

        let completion_failure = pool
            .request_completions(
                runtime,
                CompletionRequest {
                    registration: completion_registration,
                    buffer: BufferHandle::new(3),
                    revision: 13,
                    cursor_byte_offset: 7,
                    prefix: "failure".into(),
                    generation: 14,
                },
            )
            .unwrap()
            .wait()
            .unwrap();
        assert!(
            completion_failure
                .result
                .unwrap_err()
                .message
                .contains("expected completion failure")
        );

        let invalidate = pool
            .execute(
                runtime,
                "invalidate-tree.js",
                "globalThis.treeProvider.invalidate('root')",
            )
            .unwrap();
        let request = pool
            .receive_request_timeout(Duration::from_secs(1))
            .unwrap();
        assert_eq!(
            request.operation,
            operation::<declared::invalidate_tree_provider>(declared::TreeInvalidationArguments {
                registration: tree_registration,
                parent_id: Some("root".into()),
                window: None,
            })
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(success::<declared::invalidate_tree_provider>(())),
        })
        .unwrap();
        invalidate.wait().unwrap();
        pool.shutdown();
    }

    #[test]
    fn provider_roots_queue_serially_and_a_waiting_callback_releases_the_worker() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(62);
        let neighbor = key(63);
        let tree_registration = TreeProviderRegistrationId::new(93);
        let completion_registration = CompletionProviderRegistrationId::new(94);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        pool.load(neighbor, IsolateConfig::default()).unwrap();
        install_semantic_providers(&pool, runtime, tree_registration, completion_registration);

        let tree = pool
            .request_tree_children(
                runtime,
                TreeChildrenRequest {
                    registration: tree_registration,
                    window: "1".into(),
                    view: crate::host::protocol::ViewId::new(1),
                    parent_id: Some("await".into()),
                    generation: 1,
                },
            )
            .unwrap();
        let suspended = pool
            .receive_request_timeout(Duration::from_secs(1))
            .unwrap();
        assert_eq!(suspended.operation, clipboard_operation("tree wait"));
        let completion = pool
            .request_completions(
                runtime,
                CompletionRequest {
                    registration: completion_registration,
                    buffer: BufferHandle::new(4),
                    revision: 2,
                    cursor_byte_offset: 3,
                    prefix: "queued".into(),
                    generation: 2,
                },
            )
            .unwrap();
        assert_eq!(
            &*pool
                .execute(neighbor, "neighbor.js", "6 * 7")
                .unwrap()
                .wait()
                .unwrap()
                .value,
            "42"
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: suspended.id,
            result: Ok(clipboard_written()),
        })
        .unwrap();
        tree.wait().unwrap();
        completion.wait().unwrap();
        assert_eq!(
            &*pool
                .execute(
                    runtime,
                    "provider-order.js",
                    "globalThis.providerEvents.join(',')"
                )
                .unwrap()
                .wait()
                .unwrap()
                .value,
            "tree-await-start,tree-await-end,completion-queued"
        );
        pool.shutdown();
    }

    #[test]
    fn dropped_removed_and_unloaded_provider_requests_release_runtime_state() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(64);
        let tree_registration = TreeProviderRegistrationId::new(95);
        let completion_registration = CompletionProviderRegistrationId::new(96);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        install_semantic_providers(&pool, runtime, tree_registration, completion_registration);

        let blocker = pool
            .request_tree_children(
                runtime,
                TreeChildrenRequest {
                    registration: tree_registration,
                    window: "1".into(),
                    view: crate::host::protocol::ViewId::new(1),
                    parent_id: Some("await".into()),
                    generation: 2,
                },
            )
            .unwrap();
        let blocker_request = pool
            .receive_request_timeout(Duration::from_secs(1))
            .unwrap();
        let queued = pool
            .request_completions(
                runtime,
                CompletionRequest {
                    registration: completion_registration,
                    buffer: BufferHandle::new(1),
                    revision: 1,
                    cursor_byte_offset: 0,
                    prefix: "queued-drop".into(),
                    generation: 3,
                },
            )
            .unwrap();
        drop(queued);
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: blocker_request.id,
            result: Ok(clipboard_written()),
        })
        .unwrap();
        blocker.wait().unwrap();
        wait_for_state(&pool, runtime, ExtensionState::Idle);
        assert!(
            !pool
                .execute(
                    runtime,
                    "queued-provider-cancelled.js",
                    "globalThis.providerEvents.join(',')",
                )
                .unwrap()
                .wait()
                .unwrap()
                .value
                .contains("completion-queued-drop")
        );

        let dropped = pool
            .request_tree_children(
                runtime,
                TreeChildrenRequest {
                    registration: tree_registration,
                    window: "1".into(),
                    view: crate::host::protocol::ViewId::new(1),
                    parent_id: Some("await".into()),
                    generation: 4,
                },
            )
            .unwrap();
        let pending = pool
            .receive_request_timeout(Duration::from_secs(1))
            .unwrap();
        drop(dropped);
        wait_for_state(&pool, runtime, ExtensionState::Idle);
        assert_eq!(
            pool.respond(HostResponse {
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                id: pending.id,
                result: Ok(clipboard_written()),
            }),
            Err(RuntimeResponseError::UnknownRequest)
        );

        let disposal = pool
            .execute(
                runtime,
                "dispose-providers.js",
                "Promise.all([globalThis.treeProvider.dispose(), globalThis.completionProvider.dispose()])",
            )
            .unwrap();
        for expected in [
            operation::<declared::unregister_tree_provider>(declared::TreeRegistrationArguments {
                registration: tree_registration,
            }),
            operation::<declared::unregister_completion_provider>(
                declared::CompletionRegistrationArguments {
                    registration: completion_registration,
                },
            ),
        ] {
            let request = pool
                .receive_request_timeout(Duration::from_secs(1))
                .unwrap();
            assert_eq!(request.operation, expected);
            let result = match request.operation {
                operation
                    if operation
                        .arguments::<declared::unregister_tree_provider>()
                        .is_some() =>
                {
                    success::<declared::unregister_tree_provider>(())
                }
                operation
                    if operation
                        .arguments::<declared::unregister_completion_provider>()
                        .is_some() =>
                {
                    success::<declared::unregister_completion_provider>(())
                }
                _ => unreachable!(),
            };
            pool.respond(HostResponse {
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                id: request.id,
                result: Ok(result),
            })
            .unwrap();
        }
        disposal.wait().unwrap();
        let removed = pool
            .request_tree_children(
                runtime,
                TreeChildrenRequest {
                    registration: tree_registration,
                    window: "1".into(),
                    view: crate::host::protocol::ViewId::new(1),
                    parent_id: None,
                    generation: 5,
                },
            )
            .unwrap()
            .wait()
            .unwrap();
        assert!(removed.result.unwrap_err().message.contains("disposed"));

        let removed_completion = pool
            .request_completions(
                runtime,
                CompletionRequest {
                    registration: completion_registration,
                    buffer: BufferHandle::new(1),
                    revision: 1,
                    cursor_byte_offset: 0,
                    prefix: "removed".into(),
                    generation: 6,
                },
            )
            .unwrap()
            .wait()
            .unwrap();
        assert!(
            removed_completion
                .result
                .unwrap_err()
                .message
                .contains("disposed")
        );

        let unload_runtime = key(65);
        let unload_tree = TreeProviderRegistrationId::new(97);
        let unload_completion = CompletionProviderRegistrationId::new(98);
        pool.load(unload_runtime, IsolateConfig::default()).unwrap();
        install_semantic_providers(&pool, unload_runtime, unload_tree, unload_completion);
        let pending_unload = pool
            .request_completions(
                unload_runtime,
                CompletionRequest {
                    registration: unload_completion,
                    buffer: BufferHandle::new(1),
                    revision: 1,
                    cursor_byte_offset: 0,
                    prefix: "await".into(),
                    generation: 7,
                },
            )
            .unwrap();
        let unload_request = pool
            .receive_request_timeout(Duration::from_secs(1))
            .unwrap();
        pool.unload(unload_runtime).unwrap();
        assert_eq!(
            pending_unload.wait().unwrap_err().kind(),
            RuntimeErrorKind::Cancelled
        );
        assert_eq!(
            pool.respond(HostResponse {
                extension: unload_runtime.extension,
                lifecycle: unload_runtime.lifecycle,
                id: unload_request.id,
                result: Ok(success::<declared::snapshot>(TextSnapshot {
                    text: SnapshotText::from_utf8(""),
                    range: ByteRange {
                        start_byte_offset: 0,
                        end_byte_offset: 0
                    },
                    revision: 1
                })),
            }),
            Err(RuntimeResponseError::WrongExtension)
        );
        pool.shutdown();
    }

    #[test]
    fn command_registration_delivers_arguments_and_captured_buffer() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(51);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let registration = CommandRegistrationId::new(81);
        let setup = pool
            .execute_fixture_module(
                runtime,
                "file:///fixtures/command-registration.js",
                r#"
                    import * as knot from "knot"; const { commands } = knot;
                    globalThis.commandRegistration = await commands.register(
                      "knot.fixture.arguments",
                      async (context) => {
                        globalThis.commandArguments = context.arguments;
                        globalThis.commandSnapshot = await context.buffer.snapshot();
                      },
                    );
                "#,
            )
            .unwrap();
        let request = pool
            .receive_request_timeout(Duration::from_secs(1))
            .unwrap();
        assert_eq!(request.invocation, None);
        assert_eq!(
            request.operation,
            operation::<declared::register_command>(declared::RegisterCommandArguments {
                name: "knot.fixture.arguments".into(),
                title: "knot.fixture.arguments".into(),
            })
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(success::<declared::register_command>(registration)),
        })
        .unwrap();
        setup.wait().unwrap();

        let arguments = CommandArgumentValue::Object(BTreeMap::from([
            ("enabled".into(), CommandArgumentValue::Boolean(true)),
            (
                "values".into(),
                CommandArgumentValue::Array(vec![
                    CommandArgumentValue::Null,
                    CommandArgumentValue::Number(3.5),
                    CommandArgumentValue::String("lambda-λ".into()),
                ]),
            ),
        ]));
        let invocation = CommandInvocationId::new(901);
        let execution = pool
            .invoke_command(
                runtime,
                CommandInvocation {
                    id: invocation,
                    registration,
                    extension: runtime.extension,
                    lifecycle: runtime.lifecycle,
                    arguments,
                    capture: None,
                },
                Some(BufferHandle::new(17)),
            )
            .unwrap();
        let snapshot = pool
            .receive_request_timeout(Duration::from_secs(1))
            .unwrap();
        assert_eq!(snapshot.invocation, Some(invocation));
        assert_eq!(
            snapshot.operation,
            operation::<declared::snapshot>(declared::SnapshotArguments {
                buffer: BufferHandle::new(17),
                range: None,
            })
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: snapshot.id,
            result: Ok(success::<declared::snapshot>(TextSnapshot {
                text: SnapshotText::from_utf8("captured"),
                range: ByteRange {
                    start_byte_offset: 0,
                    end_byte_offset: 8,
                },
                revision: 4,
            })),
        })
        .unwrap();
        assert_eq!(execution.wait().unwrap(), CommandOutcome::Completed);
        pool.execute(
            runtime,
            "verify-command-registration.js",
            r#"
                if (!globalThis.commandArguments.enabled
                    || globalThis.commandArguments.values[1] !== 3.5
                    || globalThis.commandArguments.values[2] !== "lambda-λ") {
                  throw new Error("command arguments were not preserved");
                }
                if (globalThis.commandSnapshot.text !== "captured"
                    || globalThis.commandSnapshot.revision !== 4) {
                  throw new Error("captured buffer was not used");
                }
            "#,
        )
        .unwrap()
        .wait()
        .unwrap();
        pool.shutdown();
    }

    #[test]
    fn command_registration_errors_are_stable_and_disposal_removes_the_handler() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(52);
        pool.load(runtime, IsolateConfig::default()).unwrap();

        let duplicate = pool
            .execute_fixture_module(
                runtime,
                "file:///fixtures/duplicate-command.js",
                r#"
                    import * as knot from "knot"; const { commands } = knot;
                    await commands.register("fixture.copy", () => {});
                "#,
            )
            .unwrap();
        let request = pool
            .receive_request_timeout(Duration::from_secs(1))
            .unwrap();
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Err(test_failure("CommandNameInUse")),
        })
        .unwrap();
        let error = duplicate.wait().unwrap_err();
        assert_eq!(error.kind(), RuntimeErrorKind::Rejection);
        assert!(error.message().contains("CommandNameInUse"), "{error}");

        let registration = CommandRegistrationId::new(82);
        let setup = pool
            .execute_fixture_module(
                runtime,
                "file:///fixtures/disposable-command.js",
                r#"
                    import * as knot from "knot"; const { commands } = knot;
                    globalThis.disposableCommand = await commands.register(
                      "knot.fixture.disposable",
                      () => { globalThis.disposedHandlerRan = true; },
                    );
                "#,
            )
            .unwrap();
        let request = pool
            .receive_request_timeout(Duration::from_secs(1))
            .unwrap();
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(success::<declared::register_command>(registration)),
        })
        .unwrap();
        setup.wait().unwrap();

        let disposal = pool
            .execute(
                runtime,
                "dispose-command.js",
                "globalThis.disposableCommand.dispose()",
            )
            .unwrap();
        let request = pool
            .receive_request_timeout(Duration::from_secs(1))
            .unwrap();
        assert_eq!(
            request.operation,
            operation::<declared::unregister_command>(declared::CommandRegistrationArguments {
                registration
            })
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(success::<declared::unregister_command>(())),
        })
        .unwrap();
        disposal.wait().unwrap();

        let outcome = pool
            .invoke_command(
                runtime,
                CommandInvocation {
                    id: CommandInvocationId::new(902),
                    registration,
                    extension: runtime.extension,
                    lifecycle: runtime.lifecycle,
                    arguments: CommandArgumentValue::Null,
                    capture: None,
                },
                None,
            )
            .unwrap()
            .wait()
            .unwrap();
        assert!(matches!(outcome, CommandOutcome::HandlerFailure { .. }));
        pool.execute(
            runtime,
            "verify-disposed-command.js",
            "if (globalThis.disposedHandlerRan) throw new Error('disposed handler ran')",
        )
        .unwrap()
        .wait()
        .unwrap();
        pool.shutdown();
    }

    #[test]
    fn command_same_runtime_nesting_preserves_order_and_frame_attribution() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(53);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let outer_registration = CommandRegistrationId::new(83);
        let inner_registration = CommandRegistrationId::new(84);
        let setup = pool
            .execute_fixture_module(
                runtime,
                "file:///fixtures/nested-commands.js",
                r#"
                    import * as knot from "knot"; const { commands, editor } = knot;
                    globalThis.commandEvents = [];
                    await commands.register("knot.fixture.outer", async () => {
                      globalThis.commandEvents.push("outer-before");
                      const outcome = await commands.invoke("knot.fixture.inner", null);
                      globalThis.commandEvents.push(`inner-${outcome.kind}`);
                      await editor.activeBuffer();
                      globalThis.commandEvents.push("outer-after");
                    });
                    await commands.register("knot.fixture.inner", async (context) => {
                      globalThis.commandEvents.push("inner-before");
                      await context.buffer.snapshot();
                      globalThis.commandEvents.push("inner-after");
                    });
                "#,
            )
            .unwrap();
        for registration in [outer_registration, inner_registration] {
            let request = pool
                .receive_request_timeout(Duration::from_secs(1))
                .unwrap();
            pool.respond(HostResponse {
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                id: request.id,
                result: Ok(success::<declared::register_command>(registration)),
            })
            .unwrap();
        }
        setup.wait().unwrap();

        let outer = CommandInvocationId::new(903);
        let inner = CommandInvocationId::new(904);
        let execution = pool
            .invoke_command(
                runtime,
                CommandInvocation {
                    id: outer,
                    registration: outer_registration,
                    extension: runtime.extension,
                    lifecycle: runtime.lifecycle,
                    arguments: CommandArgumentValue::Null,
                    capture: None,
                },
                Some(BufferHandle::new(23)),
            )
            .unwrap();
        let invoke = pool
            .receive_request_timeout(Duration::from_secs(1))
            .unwrap();
        assert_eq!(invoke.invocation, Some(outer));
        assert!(
            invoke
                .operation
                .arguments::<declared::invoke_command>()
                .is_some()
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: invoke.id,
            result: Ok(success::<declared::invoke_command>(
                CommandInvokeDispatch::Inline {
                    invocation: inner,
                    registration: inner_registration,
                },
            )),
        })
        .unwrap();

        let snapshot = pool
            .receive_request_timeout(Duration::from_secs(1))
            .unwrap();
        assert_eq!(snapshot.invocation, Some(inner));
        assert_eq!(
            snapshot.operation,
            operation::<declared::snapshot>(declared::SnapshotArguments {
                buffer: BufferHandle::new(23),
                range: None,
            })
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: snapshot.id,
            result: Ok(success::<declared::snapshot>(TextSnapshot {
                text: SnapshotText::from_utf8("nested"),
                range: ByteRange {
                    start_byte_offset: 0,
                    end_byte_offset: 6,
                },
                revision: 1,
            })),
        })
        .unwrap();

        let completion = pool
            .receive_request_timeout(Duration::from_secs(1))
            .unwrap();
        assert_eq!(completion.invocation, Some(inner));
        assert_eq!(
            completion.operation,
            operation::<declared::complete_inline_command>(declared::InlineArguments {
                invocation: inner,
                outcome: CommandOutcome::Completed,
            })
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: completion.id,
            result: Ok(success::<declared::complete_inline_command>(())),
        })
        .unwrap();

        let active = pool
            .receive_request_timeout(Duration::from_secs(1))
            .unwrap();
        assert_eq!(active.invocation, Some(outer));
        assert_eq!(
            active.operation,
            operation::<declared::active_buffer>(declared::ContextArguments { target: None })
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: active.id,
            result: Ok(success::<declared::active_buffer>(Some(BufferHandle::new(
                23,
            )))),
        })
        .unwrap();
        assert_eq!(execution.wait().unwrap(), CommandOutcome::Completed);
        assert_eq!(
            &*pool
                .execute(
                    runtime,
                    "nested-order.js",
                    "globalThis.commandEvents.join(',')"
                )
                .unwrap()
                .wait()
                .unwrap()
                .value,
            "outer-before,inner-before,inner-after,inner-completed,outer-after"
        );
        pool.shutdown();
    }

    #[test]
    fn command_cancellation_rejects_suspended_host_work_and_prevents_late_resume() {
        assert_command_cancellation(false);
    }

    #[test]
    fn command_cancellation_before_active_publication_is_not_lost() {
        assert_command_cancellation(true);
    }

    fn assert_command_cancellation(cancel_before_publication: bool) {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(54);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let registration = CommandRegistrationId::new(85);
        let setup = pool
            .execute_fixture_module(
                runtime,
                "file:///fixtures/cancelled-command.js",
                r#"
                    import * as knot from "knot"; const { commands } = knot;
                    await commands.register("knot.fixture.cancelled", async (context) => {
                      if (!Object.isFrozen(context.signal) || "abort" in context.signal) throw new Error("unsupported signal interface");
                      let rejected = false;
                      try { context.signal.addEventListener("abort", null); } catch (error) { if (!(error instanceof TypeError)) throw error; rejected = true; }
                      if (!rejected) throw new Error("invalid listener accepted");
                      context.signal.addEventListener("abort", () => {
                        context.signal.addEventListener("abort", () => { globalThis.abortReplay = true; });
                        throw new Error("isolated abort listener failure");
                      });
                      context.signal.addEventListener("abort", (event) => {
                        if (!Object.isFrozen(event) || event.target !== context.signal || event.type !== "abort") throw new Error("invalid abort event");
                        globalThis.abortListenerRan = true;
                      });
                      await context.buffer.snapshot();
                      globalThis.cancelledCommandResumed = true;
                    });
                    globalThis.__knotCancelCommand = () => { throw new Error("untrusted dispatch replacement"); };
                "#,
            )
            .unwrap();
        let request = pool
            .receive_request_timeout(Duration::from_secs(1))
            .unwrap();
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(success::<declared::register_command>(registration)),
        })
        .unwrap();
        setup.wait().unwrap();

        let gate = Arc::new(std::sync::Barrier::new(2));
        if cancel_before_publication {
            *pool
                .capsule(runtime)
                .unwrap()
                .local_state
                .command_start_gate
                .lock()
                .unwrap() = Some(gate.clone());
        }
        let invocation = CommandInvocationId::new(905);
        let execution = pool
            .invoke_command(
                runtime,
                CommandInvocation {
                    id: invocation,
                    registration,
                    extension: runtime.extension,
                    lifecycle: runtime.lifecycle,
                    arguments: CommandArgumentValue::Null,
                    capture: None,
                },
                Some(BufferHandle::new(29)),
            )
            .unwrap();
        let suspended = pool
            .receive_request_timeout(Duration::from_secs(1))
            .unwrap();
        assert_eq!(suspended.invocation, Some(invocation));
        if cancel_before_publication {
            gate.wait();
        }
        pool.cancel_command(runtime, invocation).unwrap();
        if cancel_before_publication {
            gate.wait();
        }
        assert_eq!(execution.wait().unwrap(), CommandOutcome::Cancelled);
        assert_eq!(
            pool.respond(HostResponse {
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                id: suspended.id,
                result: Ok(success::<declared::snapshot>(TextSnapshot {
                    text: SnapshotText::from_utf8("late"),
                    range: ByteRange {
                        start_byte_offset: 0,
                        end_byte_offset: 4,
                    },
                    revision: 1,
                })),
            }),
            Err(RuntimeResponseError::UnknownRequest)
        );
        pool.execute(
            runtime,
            "verify-command-cancellation.js",
            "if (globalThis.cancelledCommandResumed || !globalThis.abortListenerRan || globalThis.abortReplay) throw new Error('cancelled command or listener replay')",
        )
        .unwrap()
        .wait()
        .unwrap();
        pool.shutdown();
    }

    #[test]
    fn command_cancellation_consumes_an_already_queued_response_without_ending_the_lifecycle() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(213);
        let blocker = key(214);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        pool.load(blocker, IsolateConfig::default()).unwrap();
        let registration = CommandRegistrationId::new(85);
        let setup = pool
            .execute_fixture_module(
                runtime,
                "file:///fixtures/cancellation-response.js",
                r#"
import { commands, documents, workbench } from "knot";
await commands.register("cancel-response", async () => {
  try { await workbench.writeClipboardText("pending"); }
  catch (error) { globalThis.cancelledCode = error.code; await documents.list(); }
});
"#,
            )
            .unwrap();
        let request = pool.receive_request().unwrap();
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(success::<declared::register_command>(registration)),
        })
        .unwrap();
        setup.wait().unwrap();
        let invocation = CommandInvocationId::new(905);
        let execution = pool
            .invoke_command(
                runtime,
                CommandInvocation {
                    id: invocation,
                    registration,
                    extension: runtime.extension,
                    lifecycle: runtime.lifecycle,
                    arguments: CommandArgumentValue::Null,
                    capture: None,
                },
                None,
            )
            .unwrap();
        let pending = pool.receive_request().unwrap();
        wait_for_state(&pool, runtime, ExtensionState::AwaitingHostWork);
        let gate = Arc::new(TestGate::default());
        pool.capsule(blocker)
            .unwrap()
            .install_test_barrier(gate.clone());
        let blocked = pool
            .execute(blocker, "block-worker.js", "__knotTestBarrier()")
            .unwrap();
        {
            let entered = gate.entered.lock().unwrap();
            let (_entered, timeout) = gate
                .ready
                .wait_timeout_while(entered, Duration::from_secs(1), |entered| *entered == 0)
                .unwrap();
            assert!(!timeout.timed_out());
        }
        pool.cancel_command(runtime, invocation).unwrap();
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: pending.id,
            result: Ok(clipboard_written()),
        })
        .unwrap();
        gate.wait();
        blocked.wait().unwrap();
        let followup = pool.receive_request().unwrap();
        assert_eq!(
            followup.operation,
            operation::<declared::list_documents>(())
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: followup.id,
            result: Ok(success::<declared::list_documents>(vec![])),
        })
        .unwrap();
        assert_eq!(execution.wait().unwrap(), CommandOutcome::Cancelled);
        assert_eq!(
            &*pool
                .execute(runtime, "check-lifecycle.js", "cancelledCode")
                .unwrap()
                .wait()
                .unwrap()
                .value,
            "Cancelled"
        );
        pool.shutdown();
    }

    #[test]
    fn command_handlers_classify_invalid_arguments_and_failures() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(55);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let invalid_registration = CommandRegistrationId::new(86);
        let failing_registration = CommandRegistrationId::new(87);
        let setup = pool
            .execute_fixture_module(
                runtime,
                "file:///fixtures/command-failures.js",
                r#"
                    import * as knot from "knot"; const { commands } = knot;
                    await commands.register("knot.fixture.invalid", () => {
                      commands.invalidArguments("expected object argument");
                    });
                    await commands.register("knot.fixture.failure", () => {
                      throw new Error("expected handler failure");
                    });
                "#,
            )
            .unwrap();
        for registration in [invalid_registration, failing_registration] {
            let request = pool
                .receive_request_timeout(Duration::from_secs(1))
                .unwrap();
            pool.respond(HostResponse {
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                id: request.id,
                result: Ok(success::<declared::register_command>(registration)),
            })
            .unwrap();
        }
        setup.wait().unwrap();

        let invalid = pool
            .invoke_command(
                runtime,
                CommandInvocation {
                    id: CommandInvocationId::new(906),
                    registration: invalid_registration,
                    extension: runtime.extension,
                    lifecycle: runtime.lifecycle,
                    arguments: CommandArgumentValue::String("wrong".into()),
                    capture: None,
                },
                None,
            )
            .unwrap()
            .wait()
            .unwrap();
        assert!(matches!(
            invalid,
            CommandOutcome::InvalidArgument { ref message }
                if message.contains("expected object argument")
        ));

        let failure = pool
            .invoke_command(
                runtime,
                CommandInvocation {
                    id: CommandInvocationId::new(907),
                    registration: failing_registration,
                    extension: runtime.extension,
                    lifecycle: runtime.lifecycle,
                    arguments: CommandArgumentValue::Null,
                    capture: None,
                },
                None,
            )
            .unwrap()
            .wait()
            .unwrap();
        assert!(matches!(
            failure,
            CommandOutcome::HandlerFailure { ref message }
                if message.contains("expected handler failure")
        ));
        pool.shutdown();
    }

    #[test]
    fn command_arguments_reject_non_json_values_before_dispatch() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(58);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        pool.execute_fixture_module(
            runtime,
            "file:///fixtures/invalid-command-arguments.js",
            r#"
                import * as knot from "knot"; const { commands } = knot;
                const cyclic = {};
                cyclic.self = cyclic;
                for (const value of [NaN, () => {}, cyclic]) {
                  let rejected = false;
                  try {
                    await commands.invoke("knot.fixture.missing", value);
                  } catch (error) {
                    rejected = error instanceof TypeError;
                  }
                  if (!rejected) throw new Error("invalid command arguments were accepted");
                }
            "#,
        )
        .unwrap()
        .wait()
        .unwrap();
        assert!(
            pool.receive_request_timeout(Duration::from_millis(20))
                .is_none()
        );
        pool.shutdown();
    }

    #[test]
    fn command_forced_termination_is_fatal_and_does_not_poison_a_neighbor() {
        let pool = RuntimePool::new(PoolConfig::new(NonZeroUsize::new(2).unwrap()));
        let runaway = key(56);
        let neighbor = key(57);
        pool.load(runaway, IsolateConfig::default()).unwrap();
        pool.load(neighbor, IsolateConfig::default()).unwrap();
        let registration = CommandRegistrationId::new(88);
        let setup = pool
            .execute_fixture_module(
                runaway,
                "file:///fixtures/runaway-command.js",
                r#"
                    import * as knot from "knot"; const { commands } = knot;
                    await commands.register("knot.fixture.runaway", () => {
                      __knotTestEntered();
                      while (true) {}
                    });
                "#,
            )
            .unwrap();
        let request = pool
            .receive_request_timeout(Duration::from_secs(1))
            .unwrap();
        pool.respond(HostResponse {
            extension: runaway.extension,
            lifecycle: runaway.lifecycle,
            id: request.id,
            result: Ok(success::<declared::register_command>(registration)),
        })
        .unwrap();
        setup.wait().unwrap();

        let (entered_sender, entered) = mpsc::channel();
        pool.capsule(runaway)
            .unwrap()
            .install_test_entered_signal(entered_sender);
        let termination = pool.termination_handle(runaway).unwrap();
        let execution = pool
            .invoke_command(
                runaway,
                CommandInvocation {
                    id: CommandInvocationId::new(908),
                    registration,
                    extension: runaway.extension,
                    lifecycle: runaway.lifecycle,
                    arguments: CommandArgumentValue::Null,
                    capture: None,
                },
                None,
            )
            .unwrap();
        entered.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(termination.terminate());
        assert_eq!(
            execution.wait().unwrap_err().kind(),
            RuntimeErrorKind::Terminated
        );
        assert_eq!(
            &*pool
                .execute(neighbor, "neighbor-after-command.js", "6 * 7")
                .unwrap()
                .wait()
                .unwrap()
                .value,
            "42"
        );
        assert!(pool.execute(runaway, "late-command.js", "1").is_err());
        pool.shutdown();
    }

    #[test]
    fn forced_termination_is_fatal_and_does_not_poison_a_neighbor() {
        let pool = RuntimePool::new(PoolConfig::new(NonZeroUsize::new(2).unwrap()));
        let runaway = key(1);
        let neighbor = key(2);
        pool.load(runaway, IsolateConfig::default()).unwrap();
        pool.load(neighbor, IsolateConfig::default()).unwrap();
        let (entered_sender, entered) = mpsc::channel();
        pool.capsule(runaway)
            .unwrap()
            .install_test_entered_signal(entered_sender);
        let retained: Arc<[u16]> = "worker-owned".encode_utf16().collect::<Vec<_>>().into();
        let retained_weak = Arc::downgrade(&retained);
        pool.capsule(runaway)
            .unwrap()
            .hold_external_utf16("held", retained)
            .unwrap();
        let termination = pool.termination_handle(runaway).unwrap();
        let execution = pool
            .execute(
                runaway,
                "runaway.js",
                "__knotTestEntered(); while (true) {}",
            )
            .unwrap();
        entered.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(termination.terminate());
        let error = execution.wait().unwrap_err();
        assert_eq!(error.kind(), RuntimeErrorKind::Terminated);
        assert!(retained_weak.upgrade().is_none());
        assert_eq!(
            &*pool
                .execute(neighbor, "neighbor.js", "6 * 7")
                .unwrap()
                .wait()
                .unwrap()
                .value,
            "42"
        );
        assert!(pool.execute(runaway, "late.js", "1").is_err());
        pool.shutdown();
    }

    #[test]
    fn unload_terminates_running_javascript_before_disposal() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(1);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let (entered_sender, entered) = mpsc::channel();
        pool.capsule(runtime)
            .unwrap()
            .install_test_entered_signal(entered_sender);
        let execution = pool
            .execute(
                runtime,
                "runaway.js",
                "__knotTestEntered(); while (true) {}",
            )
            .unwrap();
        let queued = pool.execute(runtime, "queued.js", "1").unwrap();
        entered.recv_timeout(Duration::from_secs(1)).unwrap();
        let started = Instant::now();
        pool.unload(runtime).unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
        let error = execution.wait().unwrap_err();
        assert_eq!(error.kind(), RuntimeErrorKind::Terminated);
        assert_eq!(
            queued.wait().unwrap_err().kind(),
            RuntimeErrorKind::Cancelled
        );
        assert!(pool.execute(runtime, "late.js", "1").is_err());
        pool.shutdown();
    }

    #[test]
    fn unload_releases_external_state_before_returning() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(1);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let text: Arc<[u16]> = "retained".encode_utf16().collect::<Vec<_>>().into();
        let weak = Arc::downgrade(&text);
        pool.capsule(runtime)
            .unwrap()
            .hold_external_utf16("held", Arc::clone(&text))
            .unwrap();
        drop(text);
        assert_eq!(weak.strong_count(), 1);
        pool.unload(runtime).unwrap();
        assert!(weak.upgrade().is_none());
        pool.shutdown();
    }

    #[test]
    fn stale_unload_cannot_dispose_a_replacement_lifecycle() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let old = key(1);
        let replacement = ExtensionKey::new(old.extension, ExtensionLifecycleId::new(2));
        pool.load(old, IsolateConfig::default()).unwrap();
        pool.unload(old).unwrap();
        pool.load(replacement, IsolateConfig::default()).unwrap();

        assert!(matches!(
            pool.unload(old),
            Err(RuntimePoolError::Scheduler(
                SchedulerError::StaleLifecycle { .. }
            ))
        ));
        assert_eq!(
            &*pool
                .execute(replacement, "replacement.js", "6 * 7")
                .unwrap()
                .wait()
                .unwrap()
                .value,
            "42"
        );
        pool.shutdown();
    }

    #[test]
    fn heap_exhaustion_probe_runs_in_a_sacrificial_process() {
        let status = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("host::engine::tests::heap_exhaustion_probe_child")
            .arg("--nocapture")
            .env("KNOT_RUN_HEAP_EXHAUSTION_PROBE", "1")
            .status()
            .expect("heap-exhaustion probe process must start");
        assert!(status.success(), "heap limit must fail only its isolate");
    }

    #[test]
    fn heap_exhaustion_probe_child() {
        if std::env::var_os("KNOT_RUN_HEAP_EXHAUSTION_PROBE").is_none() {
            return;
        }
        let pool = RuntimePool::new(PoolConfig::new(NonZeroUsize::new(2).unwrap()));
        let exhausted = key(1);
        let neighbor = key(2);
        pool.load(exhausted, IsolateConfig::with_heap_limit(5 * 1024 * 1024))
            .unwrap();
        pool.load(neighbor, IsolateConfig::default()).unwrap();
        let error = pool
            .execute(
                exhausted,
                "heap.js",
                "let value = ''; while (true) { value += '0123456789abcdef'; }",
            )
            .unwrap()
            .wait()
            .unwrap_err();
        assert_eq!(error.kind(), RuntimeErrorKind::HeapLimitExceeded);
        assert!(pool.execute(exhausted, "late.js", "1").is_err());
        assert_eq!(
            &*pool
                .execute(neighbor, "neighbor.js", "6 * 7")
                .unwrap()
                .wait()
                .unwrap()
                .value,
            "42"
        );
        pool.shutdown();
    }
}
