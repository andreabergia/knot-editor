//! V8 process initialization and engine-owned isolate state.

use std::{
    collections::HashMap,
    ffi::c_void,
    fmt,
    pin::pin,
    ptr,
    sync::{
        Arc, Mutex, Once,
        atomic::{AtomicU8, Ordering},
        mpsc,
    },
    thread::ThreadId,
};

#[cfg(test)]
use std::sync::atomic::AtomicBool;

use super::{
    lifecycle::{ExtensionKey, Failure},
    protocol::ExtensionId,
    scheduler::{CompletionOutcome, PoolConfig, SchedulerError, SchedulerPool, Turn, TurnOutcome},
};

static V8_INITIALIZATION: Once = Once::new();
const DEFAULT_HEAP_LIMIT_BYTES: usize = 32 * 1024 * 1024;
const ACTIVE: u8 = 0;
const TERMINATED: u8 = 1;
const HEAP_LIMIT_EXCEEDED: u8 = 2;
const DISPOSED: u8 = 3;

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

#[derive(Default)]
struct RuntimeLocalState {
    rejections: Mutex<Vec<RejectionReport>>,
    #[cfg(test)]
    barrier: Mutex<Option<Arc<TestGate>>>,
    #[cfg(test)]
    entered: Mutex<Option<mpsc::Sender<()>>>,
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
        let local_state = Arc::new(RuntimeLocalState::default());
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
            let scope = scope.init();
            let context = v8::Context::new(&scope, Default::default());
            v8::Global::new(&scope, context)
        };
        // SAFETY: the context, callback data, and isolate slot above contain
        // only owned Send state. No scoped handles or references survive.
        let isolate = match unsafe { isolate.try_into_shared() } {
            Ok(isolate) => isolate,
            Err(error) => {
                let message = format!("V8 isolate could not become shared: {error}");
                let mut isolate = error.into_isolate();
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
            locker.remove_near_heap_limit_callback(near_heap_limit_callback, 0);
            self.local_state
                .rejections
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clear();
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
        self.isolate.terminate_execution()
    }
}

struct EngineTurn {
    capsule: Arc<RuntimeCapsule>,
    source_name: Arc<str>,
    source: Arc<str>,
    result: mpsc::Sender<Result<ExecutionReport, RuntimeError>>,
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

/// Engine-owned composition of persistent capsules and the bounded scheduler.
pub(crate) struct RuntimePool {
    scheduler: Option<SchedulerPool<EngineTurn>>,
    runtimes: Arc<Mutex<HashMap<ExtensionId, Arc<RuntimeCapsule>>>>,
}

impl RuntimePool {
    pub(crate) fn new(config: PoolConfig) -> Self {
        initialize();
        let runtimes = Arc::new(Mutex::new(HashMap::new()));
        let executor_runtimes = Arc::clone(&runtimes);
        let scheduler = SchedulerPool::new(config, move |turn| {
            execute_engine_turn(turn, &executor_runtimes)
        });
        Self {
            scheduler: Some(scheduler),
            runtimes,
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
            Ok(capsule) => Arc::new(capsule),
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
                    source_name: source_name.into(),
                    source: source.into(),
                    result: result_sender,
                },
            )?;
        Ok(RuntimeExecution { result, completion })
    }

    pub(crate) fn termination_handle(
        &self,
        key: ExtensionKey,
    ) -> Result<TerminationHandle, RuntimePoolError> {
        Ok(self.capsule(key)?.termination_handle())
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

fn execute_engine_turn(
    turn: Turn<EngineTurn>,
    runtimes: &Mutex<HashMap<ExtensionId, Arc<RuntimeCapsule>>>,
) -> TurnOutcome {
    let work = turn.work;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        work.capsule.execute(&work.source_name, &work.source)
    }))
    .unwrap_or_else(|_| {
        Err(RuntimeError::fatal(
            RuntimeErrorKind::Engine,
            "extension engine panicked while executing a turn",
        ))
    });
    let outcome = match &result {
        Ok(_) => TurnOutcome::Completed,
        Err(error) if error.is_fatal() => {
            {
                let mut runtimes = runtimes.lock().unwrap();
                if runtimes
                    .get(&work.capsule.key().extension)
                    .is_some_and(|capsule| capsule.key() == work.capsule.key())
                {
                    runtimes.remove(&work.capsule.key().extension);
                }
            }
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                work.capsule.dispose();
            }));
            TurnOutcome::Fatal(Failure::new(error.to_string()))
        }
        Err(_) => TurnOutcome::Completed,
    };
    let _ = work.result.send(result);
    outcome
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
        collections::HashSet,
        num::NonZeroUsize,
        process::Command,
        sync::mpsc,
        time::{Duration, Instant},
    };

    use crate::host::protocol::{ExtensionId, ExtensionLifecycleId};

    fn key(extension: u64) -> ExtensionKey {
        ExtensionKey::new(ExtensionId::new(extension), ExtensionLifecycleId::new(1))
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
