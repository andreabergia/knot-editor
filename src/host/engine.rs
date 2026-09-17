//! V8 process initialization and engine-owned isolate state.

use std::{
    collections::{HashMap, HashSet},
    ffi::c_void,
    fmt,
    pin::pin,
    ptr,
    sync::{
        Arc, Mutex, Once,
        atomic::{AtomicU8, AtomicU64, Ordering},
        mpsc,
    },
    thread::ThreadId,
    time::{Duration, Instant},
};

use url::Url;

#[cfg(test)]
use std::sync::atomic::AtomicBool;

use super::{
    lifecycle::{ExtensionKey, Failure},
    protocol::{
        BufferChange, BufferHandle, BufferSubscriptionId, ByteRange, Command, CommandArgumentValue,
        CommandInvocation, CommandInvocationId, CommandInvokeDispatch, CommandOutcome,
        CommandRegistrationId, DecorationToken, EditorContribution, ExtensionId, GutterToken,
        HostOperation, HostRequest, HostResponse, HostResponseValue, RequestId, SnapshotText,
        TextEdit, TextSnapshot,
    },
    scheduler::{
        CompletionOutcome, PoolConfig, RootId, SchedulerError, SchedulerHandle, SchedulerPool,
        Turn, TurnOutcome,
    },
};

static V8_INITIALIZATION: Once = Once::new();
const DEFAULT_HEAP_LIMIT_BYTES: usize = 32 * 1024 * 1024;
const ACTIVE: u8 = 0;
const TERMINATED: u8 = 1;
const HEAP_LIMIT_EXCEEDED: u8 = 2;
const DISPOSED: u8 = 3;
const PRIVATE_BOOTSTRAP_SPECIFIER: &str = "knot:bootstrap";
const PUBLIC_FACADE_SPECIFIER: &str = "knot:editor";
const NATIVE_BINDINGS_GLOBAL: &str = "__knotNativeBindings";
const PRIVATE_BOOTSTRAP_SOURCE: &str = include_str!("js/bootstrap.js");
const PUBLIC_FACADE_SOURCE: &str = include_str!("js/editor.js");

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

struct RuntimeLocalState {
    key: ExtensionKey,
    next_request: AtomicU64,
    request_sender: Mutex<Option<mpsc::Sender<HostRequest>>>,
    current_root: Mutex<Option<RootId>>,
    pending: Mutex<HashMap<RequestId, PendingRequest>>,
    active: Mutex<Option<ActiveExecution>>,
    rejections: Mutex<Vec<RejectionReport>>,
    modules: Mutex<FixtureModuleRegistry>,
    buffer_changes: Mutex<BufferChangeQueueState>,
    cancelled_commands: Mutex<HashSet<CommandInvocationId>>,
    #[cfg(test)]
    host_error: Mutex<Option<String>>,
    #[cfg(test)]
    barrier: Mutex<Option<Arc<TestGate>>>,
    #[cfg(test)]
    entered: Mutex<Option<mpsc::Sender<()>>>,
}

impl RuntimeLocalState {
    fn new(key: ExtensionKey) -> Self {
        Self {
            key,
            next_request: AtomicU64::new(1),
            request_sender: Mutex::new(None),
            current_root: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            active: Mutex::new(None),
            rejections: Mutex::new(Vec::new()),
            modules: Mutex::new(FixtureModuleRegistry::default()),
            buffer_changes: Mutex::new(BufferChangeQueueState::default()),
            cancelled_commands: Mutex::new(HashSet::new()),
            #[cfg(test)]
            host_error: Mutex::new(None),
            #[cfg(test)]
            barrier: Mutex::new(None),
            #[cfg(test)]
            entered: Mutex::new(None),
        }
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
    operation: HostOperationKind,
    resolver: v8::Global<v8::PromiseResolver>,
    response_queued: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HostOperationKind {
    ActiveBuffer,
    Snapshot,
    ApplyEdits,
    SubscribeBufferChanges,
    UnsubscribeBufferChanges,
    ReplaceEditorContributions,
    DisposeEditorContributions,
    RegisterCommand,
    UnregisterCommand,
    InvokeCommand,
    CompleteInlineCommand,
}

struct ActiveExecution {
    root: RootId,
    promise: v8::Global<v8::Promise>,
    source: Arc<str>,
    completion: ActiveCompletion,
}

enum ActiveCompletion {
    Report(mpsc::Sender<Result<ExecutionReport, RuntimeError>>),
    Command(mpsc::Sender<Result<CommandOutcome, RuntimeError>>),
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
    Module { source: Arc<str> },
}

struct FixtureModuleRegistry {
    sources: HashMap<String, Arc<str>>,
    modules: Vec<CompiledModule>,
    resolutions: Vec<ModuleResolution>,
}

impl Default for FixtureModuleRegistry {
    fn default() -> Self {
        Self {
            sources: HashMap::from([
                (
                    PRIVATE_BOOTSTRAP_SPECIFIER.to_owned(),
                    Arc::from(PRIVATE_BOOTSTRAP_SOURCE),
                ),
                (
                    PUBLIC_FACADE_SPECIFIER.to_owned(),
                    Arc::from(PUBLIC_FACADE_SOURCE),
                ),
            ]),
            modules: Vec::new(),
            resolutions: Vec::new(),
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
        initialize();
        let status = Arc::new(AtomicU8::new(ACTIVE));
        let local_state = Arc::new(RuntimeLocalState::new(key));
        let mut isolate =
            v8::Isolate::new(v8::CreateParams::default().heap_limits(0, config.heap_limit_bytes));
        isolate.set_microtasks_policy(v8::MicrotasksPolicy::Explicit);
        isolate.set_promise_reject_callback(promise_reject_callback);
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
        result: mpsc::Sender<Result<ExecutionReport, RuntimeError>>,
    ) -> TurnOutcome {
        self.run_root_turn(root, source_name, result, RootEvaluation::Script { source })
    }

    fn start_module_turn(
        &self,
        root: RootId,
        specifier: Arc<str>,
        source: Arc<str>,
        result: mpsc::Sender<Result<ExecutionReport, RuntimeError>>,
    ) -> TurnOutcome {
        let canonical = match validate_root_module_specifier(&specifier) {
            Ok(specifier) => Arc::<str>::from(specifier),
            Err(error) => {
                let _ = result.send(Err(error));
                return TurnOutcome::Completed;
            }
        };
        self.run_root_turn(root, canonical, result, RootEvaluation::Module { source })
    }

    fn start_buffer_change_turn(
        &self,
        root: RootId,
        subscription: BufferSubscriptionId,
        change: BufferChange,
        enqueued_at: Instant,
    ) -> TurnOutcome {
        self.local_state.buffer_change_started(enqueued_at);
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
            let name = v8::String::new(try_catch, "__knotDispatchBufferChange")?;
            let function = context.global(try_catch).get(try_catch, name.into())?;
            let function = v8::Local::<v8::Function>::try_from(function).ok()?;
            let subscription = v8::Number::new(try_catch, subscription.value() as f64);
            let change = buffer_change_to_v8(try_catch, change).ok()?;
            function.call(
                try_catch,
                v8::undefined(try_catch).into(),
                &[subscription.into(), change],
            )
        })();
        let Some(value) = evaluation else {
            let error = self.execution_error(try_catch, "knot:buffer-change");
            *self.local_state.current_root.lock().unwrap() = None;
            if !error.is_fatal() {
                eprintln!("[knot] buffer-change listener failed: {error}");
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
            source: "knot:buffer-change".into(),
            completion: ActiveCompletion::Detached("buffer-change listener"),
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
        result: mpsc::Sender<Result<CommandOutcome, RuntimeError>>,
    ) -> TurnOutcome {
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
            let name = v8::String::new(try_catch, "__knotInvokeCommand")?;
            let function = context.global(try_catch).get(try_catch, name.into())?;
            let function = v8::Local::<v8::Function>::try_from(function).ok()?;
            let active_buffer = active_buffer.map_or_else(
                || v8::null(try_catch).into(),
                |buffer| v8::Number::new(try_catch, buffer.value() as f64).into(),
            );
            let arguments = command_argument_to_v8(try_catch, &invocation.arguments).ok()?;
            function.call(
                try_catch,
                v8::undefined(try_catch).into(),
                &[
                    v8::Number::new(try_catch, invocation.id.value() as f64).into(),
                    v8::Number::new(try_catch, invocation.registration.value() as f64).into(),
                    active_buffer,
                    arguments,
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
        *self.local_state.active.lock().unwrap() = Some(ActiveExecution {
            root,
            promise: v8::Global::new(try_catch, promise),
            source: "knot:command".into(),
            completion: ActiveCompletion::Command(result),
        });
        try_catch.perform_microtask_checkpoint();
        *self.local_state.current_root.lock().unwrap() = None;
        self.inspect_active(try_catch, root)
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
        let name = v8::String::new(scope, "__knotCancelCommand").unwrap();
        if let Some(function) = context.global(scope).get(scope, name.into())
            && let Ok(function) = v8::Local::<v8::Function>::try_from(function)
        {
            function.call(
                scope,
                v8::undefined(scope).into(),
                &[v8::Number::new(scope, invocation.value() as f64).into()],
            );
        }
        self.reject_pending_for_command(scope, root);
        scope.perform_microtask_checkpoint();
        *self.local_state.current_root.lock().unwrap() = None;
        self.inspect_active(scope, root)
    }

    fn run_root_turn(
        &self,
        root: RootId,
        source: Arc<str>,
        result: mpsc::Sender<Result<ExecutionReport, RuntimeError>>,
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
                {
                    let mut modules = self.local_state.modules.lock().unwrap();
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
                    compile_module_graph(try_catch, &mut modules, &source)
                };
                let compiled = match compiled {
                    Ok(compiled) => compiled,
                    Err(kind) => {
                        let error = runtime_error_from_try_catch(try_catch, kind, &source);
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
        let resolver = v8::Local::new(scope, &pending.resolver);
        match response.result {
            Ok(value) => match host_response_to_v8(scope, pending.operation, value) {
                Ok(value) => {
                    resolver.resolve(scope, value);
                }
                Err(error) => {
                    let value = v8::String::new(scope, error).unwrap();
                    resolver.reject(scope, value.into());
                }
            },
            Err(error) => {
                let wire = serde_json::to_value(error)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "UnsupportedOperation".to_owned());
                let value = v8::String::new(scope, &wire).unwrap();
                resolver.reject(scope, value.into());
            }
        }
        scope.perform_microtask_checkpoint();
        *self.local_state.current_root.lock().unwrap() = None;
        self.inspect_active(scope, root)
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
            let resolver = v8::Local::new(scope, &pending.resolver);
            let cancelled = v8::String::new(scope, "Cancelled").unwrap();
            resolver.reject(scope, cancelled.into());
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
                .filter_map(|id| requests.remove(&id))
                .collect::<Vec<_>>()
        };
        for pending in pending {
            let resolver = v8::Local::new(scope, &pending.resolver);
            let cancelled = v8::String::new(scope, "Cancelled").unwrap();
            resolver.reject(scope, cancelled.into());
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
        if let Some(active) = self.local_state.active.lock().unwrap().take() {
            match active.completion {
                ActiveCompletion::Report(result) => {
                    let _ = result.send(Err(error));
                }
                ActiveCompletion::Command(result) => {
                    let _ = result.send(Err(error));
                }
                ActiveCompletion::Detached(_) => {}
            }
        }
        self.local_state.pending.lock().unwrap().clear();
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
                    let resolver = v8::Local::new(scope, &pending.resolver);
                    let cancelled = v8::String::new(scope, "Cancelled").unwrap();
                    resolver.reject(scope, cancelled.into());
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
    fn set_test_host_error(&self, error: impl Into<String>) {
        *self.local_state.host_error.lock().unwrap() = Some(error.into());
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

enum EngineWork {
    Script {
        source_name: Arc<str>,
        source: Arc<str>,
        result: mpsc::Sender<Result<ExecutionReport, RuntimeError>>,
    },
    FixtureModule {
        specifier: Arc<str>,
        source: Arc<str>,
        result: mpsc::Sender<Result<ExecutionReport, RuntimeError>>,
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
        result: mpsc::Sender<Result<CommandOutcome, RuntimeError>>,
    },
    CancelCommand(CommandInvocationId),
    Termination,
}

/// Completion side of one scheduled JavaScript turn.
pub(crate) struct RuntimeExecution {
    result: mpsc::Receiver<Result<ExecutionReport, RuntimeError>>,
    completion: mpsc::Receiver<CompletionOutcome>,
}

impl RuntimeExecution {
    pub(crate) fn wait(self) -> Result<ExecutionReport, RuntimeError> {
        match self.result.recv() {
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

/// Completion side of one scheduled extension command handler.
pub(crate) struct RuntimeCommandExecution {
    result: mpsc::Receiver<Result<CommandOutcome, RuntimeError>>,
    completion: mpsc::Receiver<CompletionOutcome>,
}

impl RuntimeCommandExecution {
    pub(crate) fn wait(self) -> Result<CommandOutcome, RuntimeError> {
        match self.result.recv() {
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

/// Engine-owned composition of persistent capsules and the bounded scheduler.
pub(crate) struct RuntimePool {
    scheduler: Option<SchedulerPool<EngineTurn>>,
    scheduler_handle: SchedulerHandle<EngineTurn>,
    runtimes: Arc<Mutex<HashMap<ExtensionId, Arc<RuntimeCapsule>>>>,
    requests: Mutex<mpsc::Receiver<HostRequest>>,
    request_sender: mpsc::Sender<HostRequest>,
}

impl RuntimePool {
    pub(crate) fn new(config: PoolConfig) -> Self {
        initialize();
        let runtimes = Arc::new(Mutex::new(HashMap::new()));
        let (request_sender, requests) = mpsc::channel();
        let executor_runtimes = Arc::clone(&runtimes);
        let scheduler = SchedulerPool::new(config, move |turn| {
            execute_engine_turn(turn, &executor_runtimes)
        });
        let scheduler_handle = scheduler.handle();
        Self {
            scheduler: Some(scheduler),
            scheduler_handle,
            runtimes,
            requests: Mutex::new(requests),
            request_sender,
        }
    }

    pub(crate) fn load(
        &self,
        key: ExtensionKey,
        config: IsolateConfig,
    ) -> Result<(), RuntimePoolError> {
        let scheduler = self.scheduler.as_ref().expect("runtime pool is active");
        let handle = scheduler.handle();
        handle.admit(key)?;
        let capsule = match RuntimeCapsule::new(key, config) {
            Ok(capsule) => {
                *capsule.local_state.request_sender.lock().unwrap() =
                    Some(self.request_sender.clone());
                Arc::new(capsule)
            }
            Err(error) => {
                let _ = handle.finish_loading(key, Err(Failure::new(error.to_string())));
                return Err(error.into());
            }
        };
        let finish = {
            let mut runtimes = self.runtimes.lock().unwrap();
            runtimes.insert(key.extension, Arc::clone(&capsule));
            let finish = handle.finish_loading(key, Ok(()));
            if finish.is_err()
                && runtimes
                    .get(&key.extension)
                    .is_some_and(|current| current.key() == key)
            {
                runtimes.remove(&key.extension);
            }
            finish
        };
        if let Err(error) = finish {
            capsule.dispose();
            return Err(error.into());
        }
        Ok(())
    }

    pub(crate) fn execute(
        &self,
        key: ExtensionKey,
        source_name: impl Into<Arc<str>>,
        source: impl Into<Arc<str>>,
    ) -> Result<RuntimeExecution, RuntimePoolError> {
        let capsule = self.capsule(key)?;
        let (result_sender, result) = mpsc::channel();
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
        let (result_sender, result) = mpsc::channel();
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
        let (result_sender, result) = mpsc::channel();
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

    pub(crate) fn cancel_command(
        &self,
        key: ExtensionKey,
        invocation: CommandInvocationId,
    ) -> Result<(), RuntimePoolError> {
        let capsule = self.capsule(key)?;
        capsule
            .local_state
            .cancelled_commands
            .lock()
            .unwrap()
            .insert(invocation);
        let root = capsule
            .local_state
            .active
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|active| {
                matches!(&active.completion, ActiveCompletion::Command(_)).then_some(active.root)
            });
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

    pub(crate) fn receive_request(&self) -> Option<HostRequest> {
        self.requests.lock().unwrap().recv().ok()
    }

    #[cfg(test)]
    fn receive_request_timeout(&self, timeout: std::time::Duration) -> Option<HostRequest> {
        self.requests.lock().unwrap().recv_timeout(timeout).ok()
    }

    pub(crate) fn respond(&self, response: HostResponse) -> Result<(), RuntimeResponseError> {
        let request_id = response.id;
        let capsule = {
            let runtimes = self.runtimes.lock().unwrap();
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
        let capsule = {
            let mut runtimes = self.runtimes.lock().unwrap();
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
            self.scheduler
                .as_ref()
                .expect("runtime pool is active")
                .handle()
                .stop(key)?;
            runtimes.remove(&key.extension).unwrap()
        };
        let _ = capsule.termination_handle().terminate();
        capsule.dispose();
        Ok(())
    }

    pub(crate) fn shutdown(mut self) {
        self.shutdown_inner();
    }

    fn capsule(&self, key: ExtensionKey) -> Result<Arc<RuntimeCapsule>, RuntimePoolError> {
        let runtimes = self.runtimes.lock().unwrap();
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
        let capsules: Vec<_> = self.runtimes.lock().unwrap().values().cloned().collect();
        for capsule in &capsules {
            let _ = capsule.termination_handle().terminate();
        }
        if let Some(scheduler) = self.scheduler.take() {
            scheduler.shutdown();
        }
        self.runtimes.lock().unwrap().clear();
        for capsule in capsules {
            capsule.dispose();
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
    if bindings.set(scope, request_name.into(), request.into()) != Some(true)
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

    evaluate_fixture_module(scope, state, PUBLIC_FACADE_SPECIFIER, None, true).map(|_| ())
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
        compile_module_graph(try_catch, &mut modules, specifier)
    };
    let compiled = match compiled {
        Ok(compiled) => compiled,
        Err(kind) => {
            let error = runtime_error_from_try_catch(try_catch, kind, specifier);
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

fn compile_module_graph(
    scope: &mut v8::PinScope<'_, '_>,
    registry: &mut FixtureModuleRegistry,
    specifier: &str,
) -> Result<v8::Global<v8::Module>, RuntimeErrorKind> {
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
            &format!("Knot fixture module not found: {specifier}"),
        );
        return Err(RuntimeErrorKind::ModuleResolution);
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
        let request = request.get_specifier().to_rust_string_lossy(scope);
        let resolved = match resolve_fixture_specifier(&request, specifier) {
            Ok(resolved) => resolved,
            Err(message) => {
                throw_module_error(scope, &message);
                return Err(RuntimeErrorKind::ModuleResolution);
            }
        };
        registry.resolutions.push(ModuleResolution {
            referrer: global.clone(),
            request,
            resolved: resolved.clone(),
        });
        compile_module_graph(scope, registry, &resolved)?;
    }
    Ok(global)
}

fn resolve_fixture_specifier(request: &str, referrer: &str) -> Result<String, String> {
    if request == PRIVATE_BOOTSTRAP_SPECIFIER && referrer != PUBLIC_FACADE_SPECIFIER {
        return Err("Knot private bootstrap bindings are not importable by extensions".into());
    }
    if request == PRIVATE_BOOTSTRAP_SPECIFIER || request == PUBLIC_FACADE_SPECIFIER {
        return Ok(request.to_owned());
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
        throw_module_error(
            scope,
            &format!("unresolved fixture module import: {request}"),
        );
        return None;
    };
    registry
        .modules
        .iter()
        .find(|record| record.specifier == resolution.resolved)
        .map(|record| v8::Local::new(scope, &record.module))
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
    let configured_error: Option<String> = None;
    let resolver = v8::PromiseResolver::new(scope).unwrap();
    if let Some(error) = configured_error {
        let message = v8::String::new(scope, &error).unwrap();
        resolver.reject(scope, message.into());
        result.set(resolver.get_promise(scope).into());
        return;
    }
    let Some(state) = scope.get_slot::<Arc<RuntimeLocalState>>().cloned() else {
        let message = v8::String::new(scope, "UnsupportedOperation").unwrap();
        resolver.reject(scope, message.into());
        result.set(resolver.get_promise(scope).into());
        return;
    };
    let invocation = match optional_u64(scope, arguments.get(0), "UnsupportedOperation") {
        Ok(value) => value.map(CommandInvocationId::new),
        Err(error) => {
            let message = v8::String::new(scope, error).unwrap();
            resolver.reject(scope, message.into());
            result.set(resolver.get_promise(scope).into());
            return;
        }
    };
    let parsed = parse_host_operation(scope, &arguments);
    let sender = state.request_sender.lock().unwrap().clone();
    let root = *state.current_root.lock().unwrap();
    let (operation, kind) = match parsed {
        Ok(parsed) => parsed,
        Err(error) => {
            let message = v8::String::new(scope, error).unwrap();
            resolver.reject(scope, message.into());
            result.set(resolver.get_promise(scope).into());
            return;
        }
    };
    let (Some(sender), Some(root)) = (sender, root) else {
        let message = v8::String::new(scope, "UnsupportedOperation").unwrap();
        resolver.reject(scope, message.into());
        result.set(resolver.get_promise(scope).into());
        return;
    };
    let Ok(request_value) =
        state
            .next_request
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(1)
            })
    else {
        let message = v8::String::new(scope, "UnsupportedOperation").unwrap();
        resolver.reject(scope, message.into());
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
            resolver: v8::Global::new(scope, resolver),
            response_queued: false,
        },
    );
    if sender
        .send(HostRequest {
            extension: state.key.extension,
            lifecycle: state.key.lifecycle,
            id,
            invocation,
            operation,
        })
        .is_err()
    {
        state.pending.lock().unwrap().remove(&id);
        let message = v8::String::new(scope, "Cancelled").unwrap();
        resolver.reject(scope, message.into());
    }
    result.set(resolver.get_promise(scope).into());
}

fn parse_host_operation<'s, 'i>(
    scope: &mut v8::PinnedRef<'s, v8::HandleScope<'i>>,
    arguments: &v8::FunctionCallbackArguments<'s>,
) -> Result<(HostOperation, HostOperationKind), &'static str> {
    let operation = arguments
        .get(1)
        .to_string(scope)
        .ok_or("UnsupportedOperation")?
        .to_rust_string_lossy(scope);
    match operation.as_str() {
        "activeBuffer" => Ok((HostOperation::ActiveBuffer, HostOperationKind::ActiveBuffer)),
        "snapshot" => Ok((
            HostOperation::Snapshot {
                buffer: BufferHandle::new(argument_u64(scope, arguments, 2)?),
                range: optional_byte_range(scope, arguments.get(3))?,
            },
            HostOperationKind::Snapshot,
        )),
        "applyEdits" => Ok((
            HostOperation::ApplyEdits {
                buffer: BufferHandle::new(argument_u64(scope, arguments, 2)?),
                edits: text_edits(scope, arguments.get(3))?,
                if_revision: value_u64(scope, arguments.get(4), "InvalidEditBatch")?,
            },
            HostOperationKind::ApplyEdits,
        )),
        "subscribeBufferChanges" => Ok((
            HostOperation::SubscribeBufferChanges {
                buffer: BufferHandle::new(argument_u64(scope, arguments, 2)?),
            },
            HostOperationKind::SubscribeBufferChanges,
        )),
        "unsubscribeBufferChanges" => Ok((
            HostOperation::UnsubscribeBufferChanges {
                subscription: BufferSubscriptionId::new(argument_u64(scope, arguments, 2)?),
            },
            HostOperationKind::UnsubscribeBufferChanges,
        )),
        "replaceEditorContributions" => Ok((
            HostOperation::ReplaceEditorContributions {
                buffer: BufferHandle::new(argument_u64(scope, arguments, 2)?),
                contributions: editor_contributions(scope, arguments.get(3))?,
                if_revision: value_u64(scope, arguments.get(4), "UnsupportedOperation")?,
            },
            HostOperationKind::ReplaceEditorContributions,
        )),
        "disposeEditorContributions" => Ok((
            HostOperation::DisposeEditorContributions {
                buffer: BufferHandle::new(argument_u64(scope, arguments, 2)?),
            },
            HostOperationKind::DisposeEditorContributions,
        )),
        "registerCommand" => Ok((
            HostOperation::RegisterCommand {
                name: required_string(scope, arguments.get(2), "UnsupportedOperation")?.into(),
                title: required_string(scope, arguments.get(3), "UnsupportedOperation")?,
            },
            HostOperationKind::RegisterCommand,
        )),
        "unregisterCommand" => Ok((
            HostOperation::UnregisterCommand {
                registration: CommandRegistrationId::new(argument_u64(scope, arguments, 2)?),
            },
            HostOperationKind::UnregisterCommand,
        )),
        "invokeCommand" => Ok((
            HostOperation::InvokeCommand {
                command: Command {
                    name: required_string(scope, arguments.get(2), "UnsupportedOperation")?.into(),
                    arguments: command_argument_from_v8(scope, arguments.get(3))?,
                },
            },
            HostOperationKind::InvokeCommand,
        )),
        "completeInlineCommand" => Ok((
            HostOperation::CompleteInlineCommand {
                invocation: CommandInvocationId::new(argument_u64(scope, arguments, 2)?),
                outcome: command_outcome_from_v8(scope, arguments.get(3))
                    .map_err(|_| "UnsupportedOperation")?,
            },
            HostOperationKind::CompleteInlineCommand,
        )),
        _ => Err("UnsupportedOperation"),
    }
}

fn optional_u64(
    scope: &mut v8::PinnedRef<'_, v8::HandleScope>,
    value: v8::Local<'_, v8::Value>,
    error: &'static str,
) -> Result<Option<u64>, &'static str> {
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
) -> Result<u64, &'static str> {
    value_u64(scope, arguments.get(index), "UnsupportedOperation")
}

fn value_u64(
    scope: &mut v8::PinnedRef<'_, v8::HandleScope>,
    value: v8::Local<'_, v8::Value>,
    error: &'static str,
) -> Result<u64, &'static str> {
    let number = value.number_value(scope).ok_or(error)?;
    if !number.is_finite()
        || number < 0.0
        || number.fract() != 0.0
        || number > 9_007_199_254_740_991.0
    {
        return Err(error);
    }
    Ok(number as u64)
}

fn value_usize(
    scope: &mut v8::PinnedRef<'_, v8::HandleScope>,
    value: v8::Local<'_, v8::Value>,
    error: &'static str,
) -> Result<usize, &'static str> {
    usize::try_from(value_u64(scope, value, error)?).map_err(|_| error)
}

fn property<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<'s, v8::Object>,
    name: &str,
    error: &'static str,
) -> Result<v8::Local<'s, v8::Value>, &'static str> {
    let key = v8::String::new(scope, name).ok_or(error)?;
    object.get(scope, key.into()).ok_or(error)
}

fn required_string(
    scope: &mut v8::PinnedRef<'_, v8::HandleScope>,
    value: v8::Local<'_, v8::Value>,
    error: &'static str,
) -> Result<String, &'static str> {
    if !value.is_string() {
        return Err(error);
    }
    Ok(value.to_rust_string_lossy(scope))
}

fn optional_string(
    scope: &mut v8::PinnedRef<'_, v8::HandleScope>,
    value: v8::Local<'_, v8::Value>,
    error: &'static str,
) -> Result<Option<String>, &'static str> {
    if value.is_null_or_undefined() {
        Ok(None)
    } else {
        required_string(scope, value, error).map(Some)
    }
}

fn optional_byte_range<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: v8::Local<'s, v8::Value>,
) -> Result<Option<ByteRange>, &'static str> {
    if value.is_null_or_undefined() {
        return Ok(None);
    }
    byte_range(scope, value).map(Some)
}

fn byte_range<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: v8::Local<'s, v8::Value>,
) -> Result<ByteRange, &'static str> {
    let object = v8::Local::<v8::Object>::try_from(value).map_err(|_| "InvalidRange")?;
    let start = property(scope, object, "startByteOffset", "InvalidRange")?;
    let start_byte_offset = value_usize(scope, start, "InvalidRange")?;
    let end = property(scope, object, "endByteOffset", "InvalidRange")?;
    let end_byte_offset = value_usize(scope, end, "InvalidRange")?;
    Ok(ByteRange {
        start_byte_offset,
        end_byte_offset,
    })
}

fn text_edits(
    scope: &mut v8::PinnedRef<'_, v8::HandleScope>,
    value: v8::Local<'_, v8::Value>,
) -> Result<Vec<TextEdit>, &'static str> {
    let array = v8::Local::<v8::Array>::try_from(value).map_err(|_| "InvalidEditBatch")?;
    let mut edits = Vec::with_capacity(array.length() as usize);
    for index in 0..array.length() {
        let value = array.get_index(scope, index).ok_or("InvalidEditBatch")?;
        let object = v8::Local::<v8::Object>::try_from(value).map_err(|_| "InvalidEditBatch")?;
        let range = property(scope, object, "range", "InvalidEditBatch")?;
        let range = byte_range(scope, range).map_err(|_| "InvalidEditBatch")?;
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
) -> Result<Vec<EditorContribution>, &'static str> {
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
                _ => Err(error),
            })
            .transpose()?;
        let gutter_value = property(scope, object, "gutter", error)?;
        let gutter = optional_string(scope, gutter_value, error)?
            .map(|value| match value.as_str() {
                "info" => Ok(GutterToken::Info),
                "warning" => Ok(GutterToken::Warning),
                "error" => Ok(GutterToken::Error),
                _ => Err(error),
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

fn command_argument_from_v8(
    scope: &mut v8::PinScope<'_, '_>,
    value: v8::Local<'_, v8::Value>,
) -> Result<CommandArgumentValue, &'static str> {
    let json = v8::json::stringify(scope, value).ok_or("UnsupportedOperation")?;
    serde_json::from_str(&json.to_rust_string_lossy(scope)).map_err(|_| "UnsupportedOperation")
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

fn command_outcome_from_v8(
    scope: &mut v8::PinScope<'_, '_>,
    value: v8::Local<'_, v8::Value>,
) -> Result<CommandOutcome, RuntimeError> {
    let json = v8::json::stringify(scope, value).ok_or_else(|| {
        RuntimeError::fatal(
            RuntimeErrorKind::Engine,
            "invalid command outcome from JavaScript",
        )
    })?;
    serde_json::from_str(&json.to_rust_string_lossy(scope)).map_err(|error| {
        RuntimeError::fatal(
            RuntimeErrorKind::Engine,
            format!("invalid command outcome from JavaScript: {error}"),
        )
    })
}

fn command_dispatch_to_v8<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    dispatch: &CommandInvokeDispatch,
) -> Result<v8::Local<'s, v8::Value>, &'static str> {
    let json = serde_json::to_string(dispatch).map_err(|_| "UnsupportedOperation")?;
    let json = v8::String::new(scope, &json).ok_or("UnsupportedOperation")?;
    v8::json::parse(scope, json).ok_or("UnsupportedOperation")
}

fn host_response_to_v8<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    operation: HostOperationKind,
    response: HostResponseValue,
) -> Result<v8::Local<'s, v8::Value>, &'static str> {
    match (operation, response) {
        (HostOperationKind::ActiveBuffer, HostResponseValue::ActiveBuffer(None)) => {
            Ok(v8::null(scope).into())
        }
        (HostOperationKind::ActiveBuffer, HostResponseValue::ActiveBuffer(Some(buffer))) => {
            Ok(v8::Number::new(scope, buffer.value() as f64).into())
        }
        (HostOperationKind::Snapshot, HostResponseValue::Snapshot(snapshot)) => {
            snapshot_to_v8(scope, snapshot)
        }
        (HostOperationKind::ApplyEdits, HostResponseValue::AppliedEdits { revision }) => {
            Ok(v8::Number::new(scope, revision as f64).into())
        }
        (
            HostOperationKind::SubscribeBufferChanges,
            HostResponseValue::BufferChangesSubscribed { subscription },
        ) => Ok(v8::Number::new(scope, subscription.value() as f64).into()),
        (
            HostOperationKind::UnsubscribeBufferChanges,
            HostResponseValue::BufferChangesUnsubscribed { .. },
        )
        | (
            HostOperationKind::ReplaceEditorContributions,
            HostResponseValue::EditorContributionsReplaced,
        )
        | (
            HostOperationKind::DisposeEditorContributions,
            HostResponseValue::EditorContributionsDisposed,
        ) => Ok(v8::undefined(scope).into()),
        (
            HostOperationKind::RegisterCommand,
            HostResponseValue::CommandRegistered { registration },
        ) => Ok(v8::Number::new(scope, registration.value() as f64).into()),
        (HostOperationKind::UnregisterCommand, HostResponseValue::CommandUnregistered { .. })
        | (
            HostOperationKind::CompleteInlineCommand,
            HostResponseValue::InlineCommandCompleted { .. },
        ) => Ok(v8::undefined(scope).into()),
        (HostOperationKind::InvokeCommand, HostResponseValue::CommandInvoked { dispatch }) => {
            command_dispatch_to_v8(scope, &dispatch)
        }
        _ => Err("UnsupportedOperation"),
    }
}

fn snapshot_to_v8<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    snapshot: TextSnapshot,
) -> Result<v8::Local<'s, v8::Value>, &'static str> {
    let value = v8::Object::new(scope);
    let text = snapshot_text_to_v8(scope, snapshot.text)?;
    set_property(scope, value, "text", text.into())?;

    let range = v8::Object::new(scope);
    set_property(
        scope,
        range,
        "startByteOffset",
        v8::Number::new(scope, snapshot.range.start_byte_offset as f64).into(),
    )?;
    set_property(
        scope,
        range,
        "endByteOffset",
        v8::Number::new(scope, snapshot.range.end_byte_offset as f64).into(),
    )?;
    set_property(scope, value, "range", range.into())?;
    set_property(
        scope,
        value,
        "revision",
        v8::Number::new(scope, snapshot.revision as f64).into(),
    )?;
    Ok(value.into())
}

fn buffer_change_to_v8<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    change: BufferChange,
) -> Result<v8::Local<'s, v8::Value>, &'static str> {
    let value = v8::Object::new(scope);
    set_property(
        scope,
        value,
        "beforeRevision",
        v8::Number::new(scope, change.before_revision as f64).into(),
    )?;
    set_property(
        scope,
        value,
        "revision",
        v8::Number::new(scope, change.revision as f64).into(),
    )?;
    let edits = v8::Array::new(
        scope,
        i32::try_from(change.edits.len()).map_err(|_| "InvalidEditBatch")?,
    );
    for (index, edit) in change.edits.into_iter().enumerate() {
        let item = v8::Object::new(scope);
        let range = v8::Object::new(scope);
        set_property(
            scope,
            range,
            "startByteOffset",
            v8::Number::new(scope, edit.range.start_byte_offset as f64).into(),
        )?;
        set_property(
            scope,
            range,
            "endByteOffset",
            v8::Number::new(scope, edit.range.end_byte_offset as f64).into(),
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
    match text {
        SnapshotText::Utf8(text) => v8::String::new(scope, &text).ok_or("UnsupportedOperation"),
        SnapshotText::Utf16(text) if text.is_empty() => Ok(v8::String::empty(scope)),
        SnapshotText::Utf16(text) => {
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

fn execute_engine_turn(
    turn: Turn<EngineTurn>,
    runtimes: &Mutex<HashMap<ExtensionId, Arc<RuntimeCapsule>>>,
) -> TurnOutcome {
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
        EngineWork::HostResponse(response) => capsule.resume_response_turn(root, response),
        EngineWork::BufferChange {
            subscription,
            change,
            enqueued_at,
        } => capsule.start_buffer_change_turn(root, subscription, change, enqueued_at),
        EngineWork::CommandInvocation {
            invocation,
            active_buffer,
            result,
        } => capsule.start_command_turn(root, invocation, active_buffer, result),
        EngineWork::CancelCommand(invocation) => capsule.cancel_command_turn(root, invocation),
        EngineWork::Termination => capsule.finish_termination_turn(root),
    }))
    .unwrap_or_else(|_| {
        TurnOutcome::Fatal(Failure::new(
            "extension engine panicked while executing a turn",
        ))
    });
    match &outcome {
        TurnOutcome::Fatal(failure) => {
            {
                let mut runtimes = runtimes.lock().unwrap();
                if runtimes
                    .get(&key.extension)
                    .is_some_and(|current| current.key() == key)
                {
                    runtimes.remove(&key.extension);
                }
            }
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                capsule.dispose_with_error(RuntimeError::fatal(
                    RuntimeErrorKind::LifecycleFailed,
                    failure.message().to_owned(),
                ));
            }));
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
        BufferHandle, ExtensionId, ExtensionLifecycleId, HostRequestError, HostResponse,
        HostResponseValue, RequestId,
    };

    fn key(extension: u64) -> ExtensionKey {
        ExtensionKey::new(ExtensionId::new(extension), ExtensionLifecycleId::new(1))
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

    fn install_buffer_listener(
        pool: &RuntimePool,
        runtime: ExtensionKey,
        subscription: BufferSubscriptionId,
        module: &str,
    ) {
        let execution = pool
            .execute_fixture_module(
                runtime,
                module,
                r#"
                import { editor } from "knot:editor";
                const buffer = await editor.activeBuffer();
                globalThis.editorApi = editor;
                globalThis.events = [];
                await buffer.onDidChange(globalThis.listener);
            "#,
            )
            .unwrap();
        let active = pool.receive_request().unwrap();
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: active.id,
            result: Ok(HostResponseValue::ActiveBuffer(Some(BufferHandle::new(3)))),
        })
        .unwrap();
        let subscribe = pool.receive_request().unwrap();
        assert_eq!(
            subscribe.operation,
            HostOperation::SubscribeBufferChanges {
                buffer: BufferHandle::new(3)
            }
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: subscribe.id,
            result: Ok(HostResponseValue::BufferChangesSubscribed { subscription }),
        })
        .unwrap();
        execution.wait().unwrap();
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
                import { editor, commands, workbench } from "knot:editor";
                if (typeof Deno !== "undefined") throw new Error("Deno is exposed");
                if (typeof globalThis.__knotNativeBindings !== "undefined") {
                    throw new Error("private native bindings are exposed");
                }
                if (!Object.isFrozen(editor) || !Object.isFrozen(commands) || !Object.isFrozen(workbench)) {
                    throw new Error("facade objects are mutable");
                }
                if (typeof editor.activeBuffer !== "function"
                    || typeof editor.registerCompletionProvider !== "function"
                    || typeof commands.invalidArguments !== "function"
                    || typeof commands.invoke !== "function"
                    || typeof commands.register !== "function"
                    || typeof workbench.registerTreeDataProvider !== "function") {
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
                    import { editor } from "knot:editor";
                    await editor.activeBuffer();
                "#,
            )
            .unwrap();
        let request = pool.receive_request().unwrap();
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Err(HostRequestError::UnsupportedOperation),
        })
        .unwrap();
        let unsupported = unsupported.wait().unwrap_err();
        assert_eq!(unsupported.kind(), RuntimeErrorKind::Rejection);
        assert!(unsupported.message().contains("UnsupportedOperationError"));
        pool.shutdown();
    }

    #[test]
    fn buffer_requests_and_typed_responses_round_trip() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(40);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let execution = pool
            .execute_fixture_module(
                runtime,
                "file:///fixtures/buffers.js",
                r#"
                    import { editor } from "knot:editor";
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
                    disposable.dispose();
                    await buffer.contributions.dispose();
                "#,
            )
            .unwrap();

        let active = pool.receive_request().unwrap();
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: active.id,
            result: Ok(HostResponseValue::ActiveBuffer(Some(BufferHandle::new(3)))),
        })
        .unwrap();

        let request = pool.receive_request().unwrap();
        assert_eq!(
            request.operation,
            HostOperation::Snapshot {
                buffer: BufferHandle::new(3),
                range: Some(ByteRange {
                    start_byte_offset: 1,
                    end_byte_offset: 7
                }),
            }
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(HostResponseValue::Snapshot(TextSnapshot {
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
            HostOperation::ApplyEdits {
                buffer: BufferHandle::new(3),
                edits: vec![TextEdit {
                    range: ByteRange {
                        start_byte_offset: 2,
                        end_byte_offset: 5
                    },
                    text: "λ".into(),
                }],
                if_revision: 7,
            }
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(HostResponseValue::AppliedEdits { revision: 8 }),
        })
        .unwrap();

        let request = pool.receive_request().unwrap();
        assert_eq!(
            request.operation,
            HostOperation::ReplaceEditorContributions {
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
            }
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(HostResponseValue::EditorContributionsReplaced),
        })
        .unwrap();

        let request = pool.receive_request().unwrap();
        assert_eq!(
            request.operation,
            HostOperation::SubscribeBufferChanges {
                buffer: BufferHandle::new(3)
            }
        );
        let subscription = BufferSubscriptionId::new(12);
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(HostResponseValue::BufferChangesSubscribed { subscription }),
        })
        .unwrap();
        let request = pool.receive_request().unwrap();
        assert_eq!(
            request.operation,
            HostOperation::UnsubscribeBufferChanges { subscription }
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(HostResponseValue::BufferChangesUnsubscribed { subscription }),
        })
        .unwrap();
        let request = pool.receive_request().unwrap();
        assert_eq!(
            request.operation,
            HostOperation::DisposeEditorContributions {
                buffer: BufferHandle::new(3)
            }
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(HostResponseValue::EditorContributionsDisposed),
        })
        .unwrap();
        execution.wait().unwrap();
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
            let execution = pool
                .execute_fixture_module(
                    runtime,
                    format!("file:///fixtures/external-{}.js", runtime.extension.value()),
                    r#"
                        import { editor } from "knot:editor";
                        const buffer = await editor.activeBuffer();
                        globalThis.heldSnapshot = await buffer.snapshot();
                    "#,
                )
                .unwrap();
            let active = pool.receive_request().unwrap();
            pool.respond(HostResponse {
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                id: active.id,
                result: Ok(HostResponseValue::ActiveBuffer(Some(BufferHandle::new(1)))),
            })
            .unwrap();
            let request = pool.receive_request().unwrap();
            pool.respond(HostResponse {
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                id: request.id,
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
            execution.wait().unwrap();
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
            globalThis.listener = (event) => {
              const end = Date.now() + 30;
              while (Date.now() < end) {}
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
        assert!(metrics.max_depth >= 4, "metrics: {metrics:?}");
        assert!(
            metrics.max_enqueue_to_start_lag >= Duration::from_millis(50),
            "metrics: {metrics:?}"
        );
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
              await editorApi.activeBuffer();
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
            result: Ok(HostResponseValue::ActiveBuffer(Some(BufferHandle::new(3)))),
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
            "globalThis.listener = async () => { await editorApi.activeBuffer(); };",
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
                result: Ok(HostResponseValue::ActiveBuffer(None)),
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
                    import { editor } from "knot:editor";
                    const buffer = await editor.activeBuffer();
                    await Promise.resolve();
                    globalThis.responseValue = buffer !== null;
                "#,
            )
            .unwrap();
        let request = pool.receive_request().unwrap();
        assert_eq!(request.extension, runtime.extension);
        assert_eq!(request.lifecycle, runtime.lifecycle);
        assert_eq!(request.id, RequestId::new(1));
        assert_eq!(request.operation, HostOperation::ActiveBuffer);
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(HostResponseValue::ActiveBuffer(Some(BufferHandle::new(9)))),
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
    fn host_error_response_rejects_the_request_and_propagates_from_the_root() {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(1);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let execution = pool
            .execute_fixture_module(
                runtime,
                "file:///fixtures/request-error.js",
                "import { editor } from 'knot:editor'; await editor.activeBuffer();",
            )
            .unwrap();
        let request = pool.receive_request().unwrap();
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Err(HostRequestError::RevisionConflict),
        })
        .unwrap();
        let error = execution.wait().unwrap_err();
        assert_eq!(error.kind(), RuntimeErrorKind::Rejection);
        assert!(error.message().contains("RevisionConflictError"));
        assert_eq!(error.source(), Some(PRIVATE_BOOTSTRAP_SPECIFIER));
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
                    import { editor } from "knot:editor";
                    const [first, second] = await Promise.all([
                      editor.activeBuffer(),
                      editor.activeBuffer(),
                    ]);
                    globalThis.responsesAreDistinct = first !== second;
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
            result: Ok(HostResponseValue::ActiveBuffer(Some(BufferHandle::new(2)))),
        })
        .unwrap();
        assert_eq!(
            pool.respond(HostResponse {
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                id: second.id,
                result: Ok(HostResponseValue::ActiveBuffer(None)),
            }),
            Err(RuntimeResponseError::UnknownRequest)
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: first.id,
            result: Ok(HostResponseValue::ActiveBuffer(Some(BufferHandle::new(1)))),
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
                result: Ok(HostResponseValue::ActiveBuffer(None)),
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
                    import { editor } from "knot:editor";
                    await Promise.all([editor.activeBuffer(), editor.activeBuffer()]);
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
            result: Err(HostRequestError::BufferClosed),
        })
        .unwrap();
        let error = execution.wait().unwrap_err();
        assert_eq!(error.kind(), RuntimeErrorKind::Rejection);
        assert!(error.message().contains("BufferClosedError"));
        assert_eq!(
            pool.respond(HostResponse {
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                id: second.id,
                result: Ok(HostResponseValue::ActiveBuffer(None)),
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
                "import { editor } from 'knot:editor'; await editor.activeBuffer();",
            )
            .unwrap();
        let request = pool.receive_request().unwrap();
        for response in [
            HostResponse {
                extension: ExtensionId::new(99),
                lifecycle: runtime.lifecycle,
                id: request.id,
                result: Ok(HostResponseValue::ActiveBuffer(None)),
            },
            HostResponse {
                extension: runtime.extension,
                lifecycle: ExtensionLifecycleId::new(99),
                id: request.id,
                result: Ok(HostResponseValue::ActiveBuffer(None)),
            },
            HostResponse {
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                id: RequestId::new(99),
                result: Ok(HostResponseValue::ActiveBuffer(None)),
            },
        ] {
            assert!(pool.respond(response).is_err());
        }
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(HostResponseValue::ActiveBuffer(None)),
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
                "import { editor } from 'knot:editor'; await editor.activeBuffer();",
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
            result: Ok(HostResponseValue::ActiveBuffer(None)),
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
                    import { editor } from "knot:editor";
                    await Promise.resolve();
                    await editor.activeBuffer();
                    globalThis.firstResponse = true;
                    await editor.activeBuffer();
                    globalThis.secondResponse = true;
                "#,
            )
            .unwrap();
        let first = pool.receive_request().unwrap();
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: first.id,
            result: Ok(HostResponseValue::ActiveBuffer(None)),
        })
        .unwrap();
        let second = pool.receive_request().unwrap();
        assert_eq!(second.id, RequestId::new(2));
        wait_for_state(&pool, runtime, ExtensionState::AwaitingHostWork);
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: second.id,
            result: Ok(HostResponseValue::ActiveBuffer(None)),
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
                "import { editor } from 'knot:editor'; await editor.activeBuffer();",
            )
            .unwrap();
        let request = pool.receive_request().unwrap();
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(HostResponseValue::EditorContributionsDisposed),
        })
        .unwrap();
        let error = execution.wait().unwrap_err();
        assert_eq!(error.kind(), RuntimeErrorKind::Rejection);
        assert!(error.message().contains("UnsupportedOperationError"));
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
                    import { editor } from "knot:editor";
                    await Promise.all([editor.activeBuffer(), editor.activeBuffer()]);
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
                result: Ok(HostResponseValue::ActiveBuffer(None)),
            }),
            Err(RuntimeResponseError::WrongExtension)
        );

        let replacement = ExtensionKey::new(first.extension, ExtensionLifecycleId::new(2));
        pool.load(replacement, IsolateConfig::default()).unwrap();
        let replacement_execution = pool
            .execute_fixture_module(
                replacement,
                "file:///fixtures/replacement.js",
                "import { editor } from 'knot:editor'; await editor.activeBuffer();",
            )
            .unwrap();
        let replacement_request = pool.receive_request().unwrap();
        assert_eq!(replacement_request.id, RequestId::new(1));
        assert_eq!(
            pool.respond(HostResponse {
                extension: first.extension,
                lifecycle: first.lifecycle,
                id: replacement_request.id,
                result: Ok(HostResponseValue::ActiveBuffer(None)),
            }),
            Err(RuntimeResponseError::WrongExtension)
        );
        pool.respond(HostResponse {
            extension: replacement.extension,
            lifecycle: replacement.lifecycle,
            id: replacement_request.id,
            result: Ok(HostResponseValue::ActiveBuffer(None)),
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
                "import { editor } from 'knot:editor'; await editor.activeBuffer();",
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
                "import { editor } from 'knot:editor'; await editor.activeBuffer();",
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
                result: Ok(HostResponseValue::ActiveBuffer(None)),
            }),
            Err(RuntimeResponseError::WrongExtension)
        );
        pool.shutdown();
    }

    #[test]
    fn facade_maps_every_host_error_to_a_stable_javascript_name() {
        let capsule = RuntimeCapsule::new(key(1), IsolateConfig::default()).unwrap();
        let cases = [
            (
                HostRequestError::UnsupportedOperation,
                "UnsupportedOperationError",
            ),
            (HostRequestError::BufferClosed, "BufferClosedError"),
            (HostRequestError::InvalidRange, "RangeError"),
            (HostRequestError::InvalidEditBatch, "InvalidEditBatchError"),
            (HostRequestError::RevisionConflict, "RevisionConflictError"),
            (
                HostRequestError::ContributionSetNotFound,
                "ContributionSetNotFoundError",
            ),
            (HostRequestError::TreeViewNotFound, "TreeViewNotFoundError"),
            (
                HostRequestError::TreeProviderInUse,
                "TreeProviderInUseError",
            ),
            (
                HostRequestError::TreeProviderNotFound,
                "TreeProviderNotFoundError",
            ),
            (
                HostRequestError::CompletionProviderNotFound,
                "CompletionProviderNotFoundError",
            ),
            (HostRequestError::CommandNameInUse, "CommandNameInUseError"),
            (HostRequestError::CommandNotFound, "CommandNotFoundError"),
            (HostRequestError::Cancelled, "AbortError"),
        ];
        for (index, (error, expected_name)) in cases.into_iter().enumerate() {
            let wire_name = serde_json::to_value(error)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned();
            capsule.set_test_host_error(wire_name);
            let report = capsule
                .execute_fixture_module(
                    &format!("file:///fixtures/host-error-{index}.js"),
                    r#"
                        import { editor } from "knot:editor";
                        try {
                            await editor.activeBuffer();
                        } catch (error) {
                            globalThis.hostErrorName = error.name;
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
        }

        capsule.set_test_host_error("FutureHostFailure");
        capsule
            .execute_fixture_module(
                "file:///fixtures/unknown-host-error.js",
                r#"
                    import { editor } from "knot:editor";
                    try { await editor.activeBuffer(); }
                    catch (error) { globalThis.hostErrorName = error.name; }
                "#,
            )
            .unwrap();
        assert_eq!(
            &*capsule
                .execute("unknown-host-error-name.js", "hostErrorName")
                .unwrap()
                .value,
            "KnotHostError"
        );

        capsule
            .execute_fixture_module(
                "file:///fixtures/invalid-arguments.js",
                r#"
                    import { commands } from "knot:editor";
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
                    import { commands } from "knot:editor";
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
            HostOperation::RegisterCommand {
                name: "knot.fixture.arguments".into(),
                title: "knot.fixture.arguments".into(),
            }
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(HostResponseValue::CommandRegistered { registration }),
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
            HostOperation::Snapshot {
                buffer: BufferHandle::new(17),
                range: None,
            }
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: snapshot.id,
            result: Ok(HostResponseValue::Snapshot(TextSnapshot {
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
                    import { commands } from "knot:editor";
                    await commands.register("editor.copy", () => {});
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
            result: Err(HostRequestError::CommandNameInUse),
        })
        .unwrap();
        let error = duplicate.wait().unwrap_err();
        assert_eq!(error.kind(), RuntimeErrorKind::Rejection);
        assert!(error.message().contains("CommandNameInUseError"), "{error}");

        let registration = CommandRegistrationId::new(82);
        let setup = pool
            .execute_fixture_module(
                runtime,
                "file:///fixtures/disposable-command.js",
                r#"
                    import { commands } from "knot:editor";
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
            result: Ok(HostResponseValue::CommandRegistered { registration }),
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
            HostOperation::UnregisterCommand { registration }
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(HostResponseValue::CommandUnregistered { registration }),
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
                    import { commands, editor } from "knot:editor";
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
                result: Ok(HostResponseValue::CommandRegistered { registration }),
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
                },
                Some(BufferHandle::new(23)),
            )
            .unwrap();
        let invoke = pool
            .receive_request_timeout(Duration::from_secs(1))
            .unwrap();
        assert_eq!(invoke.invocation, Some(outer));
        assert!(matches!(
            invoke.operation,
            HostOperation::InvokeCommand { .. }
        ));
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: invoke.id,
            result: Ok(HostResponseValue::CommandInvoked {
                dispatch: CommandInvokeDispatch::Inline {
                    invocation: inner,
                    registration: inner_registration,
                },
            }),
        })
        .unwrap();

        let snapshot = pool
            .receive_request_timeout(Duration::from_secs(1))
            .unwrap();
        assert_eq!(snapshot.invocation, Some(inner));
        assert_eq!(
            snapshot.operation,
            HostOperation::Snapshot {
                buffer: BufferHandle::new(23),
                range: None,
            }
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: snapshot.id,
            result: Ok(HostResponseValue::Snapshot(TextSnapshot {
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
            HostOperation::CompleteInlineCommand {
                invocation: inner,
                outcome: CommandOutcome::Completed,
            }
        );
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: completion.id,
            result: Ok(HostResponseValue::InlineCommandCompleted { invocation: inner }),
        })
        .unwrap();

        let active = pool
            .receive_request_timeout(Duration::from_secs(1))
            .unwrap();
        assert_eq!(active.invocation, Some(outer));
        assert_eq!(active.operation, HostOperation::ActiveBuffer);
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: active.id,
            result: Ok(HostResponseValue::ActiveBuffer(Some(BufferHandle::new(23)))),
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
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(54);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let registration = CommandRegistrationId::new(85);
        let setup = pool
            .execute_fixture_module(
                runtime,
                "file:///fixtures/cancelled-command.js",
                r#"
                    import { commands } from "knot:editor";
                    await commands.register("knot.fixture.cancelled", async (context) => {
                      await context.buffer.snapshot();
                      globalThis.cancelledCommandResumed = true;
                    });
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
            result: Ok(HostResponseValue::CommandRegistered { registration }),
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
                },
                Some(BufferHandle::new(29)),
            )
            .unwrap();
        let suspended = pool
            .receive_request_timeout(Duration::from_secs(1))
            .unwrap();
        assert_eq!(suspended.invocation, Some(invocation));
        pool.cancel_command(runtime, invocation).unwrap();
        assert_eq!(execution.wait().unwrap(), CommandOutcome::Cancelled);
        assert_eq!(
            pool.respond(HostResponse {
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                id: suspended.id,
                result: Ok(HostResponseValue::Snapshot(TextSnapshot {
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
            "if (globalThis.cancelledCommandResumed) throw new Error('cancelled command resumed')",
        )
        .unwrap()
        .wait()
        .unwrap();
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
                    import { commands } from "knot:editor";
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
                result: Ok(HostResponseValue::CommandRegistered { registration }),
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
                import { commands } from "knot:editor";
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
                    import { commands } from "knot:editor";
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
            result: Ok(HostResponseValue::CommandRegistered { registration }),
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
