//! Bounded extension scheduling, independent of V8 mechanics.

use std::{
    collections::{HashMap, VecDeque},
    num::NonZeroUsize,
    pin::Pin,
    sync::{Arc, Condvar, Mutex},
    task::{Context, Poll},
    thread::JoinHandle,
    time::{Duration, Instant},
};

#[cfg(test)]
use std::sync::mpsc;

use super::{
    lifecycle::{ExtensionKey, ExtensionState, Failure},
    protocol::{ExtensionId, ExtensionLifecycleId},
};

/// Fixed worker count for one scheduler pool lifetime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PoolConfig {
    workers: NonZeroUsize,
}

impl PoolConfig {
    pub const fn new(workers: NonZeroUsize) -> Self {
        Self { workers }
    }

    #[cfg(test)]
    pub const fn single_worker() -> Self {
        Self::new(NonZeroUsize::MIN)
    }

    pub const fn workers(self) -> NonZeroUsize {
        self.workers
    }
}

impl Default for PoolConfig {
    fn default() -> Self {
        let workers = std::thread::available_parallelism()
            .unwrap_or(NonZeroUsize::MIN)
            .get()
            .min(4);
        Self::new(NonZeroUsize::new(workers).expect("the pool size is at least one"))
    }
}

/// Identifies one root callback and its logical command tree.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RootId(u64);

impl RootId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
}

/// Identifies one dispatched turn so late and duplicate results can be rejected.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TurnId(u64);

/// One unit of opaque work selected for a worker.
#[derive(Debug)]
pub struct Turn<W> {
    pub key: ExtensionKey,
    pub root: RootId,
    pub id: TurnId,
    pub work: W,
}

/// The scheduler-relevant result of executing one turn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TurnOutcome {
    Completed,
    AwaitingHostWork,
    #[cfg(test)]
    ReadyForContinuation,
    Fatal(Failure),
}

/// Final disposition delivered exactly once for each accepted root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompletionOutcome {
    Completed,
    Cancelled,
    Failed(Failure),
}

#[cfg(test)]
/// Point-in-time scheduler diagnostics for one extension lifetime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtensionDiagnostics {
    pub key: ExtensionKey,
    pub state: ExtensionState,
    pub queue_depth: usize,
    pub max_queue_depth: usize,
    pub turn_count: u64,
    pub worker_movements: u64,
    pub max_enqueue_to_start_lag: Duration,
}

#[cfg(test)]
/// Point-in-time diagnostics for one bounded scheduler pool.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchedulerDiagnostics {
    pub worker_count: usize,
    pub ready_queue_depth: usize,
    pub queue_depth: usize,
    pub max_queue_depth: usize,
    pub turn_count: u64,
    pub worker_movements: u64,
    pub max_enqueue_to_start_lag: Duration,
    pub shutting_down: bool,
    pub extensions: Vec<ExtensionDiagnostics>,
}

/// Errors returned before work is accepted or a state transition is applied.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SchedulerError {
    ShuttingDown,
    AlreadyAdmitted(ExtensionKey),
    UnknownExtension(ExtensionId),
    StaleLifecycle {
        current: ExtensionLifecycleId,
        received: ExtensionLifecycleId,
    },
    NotLoading,
    NotRunning,
    NoActiveRoot,
    WrongRoot {
        active: RootId,
        received: RootId,
    },
    DuplicateOrStaleTurn(TurnId),
    #[cfg(test)]
    NoContinuation,
    Stopping,
    Failed(Failure),
}

impl std::fmt::Display for SchedulerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for SchedulerError {}

struct CompletionSlot {
    #[cfg(test)]
    blocking: Option<mpsc::Sender<CompletionOutcome>>,
    asynchronous: Option<tokio::sync::oneshot::Sender<CompletionOutcome>>,
}

/// Awaitable view of one root's terminal disposition. Tests may also block on it.
pub struct CompletionReceiver {
    #[cfg(test)]
    blocking: mpsc::Receiver<CompletionOutcome>,
    asynchronous: tokio::sync::oneshot::Receiver<CompletionOutcome>,
}

impl CompletionReceiver {
    #[cfg(test)]
    pub fn recv(&self) -> Result<CompletionOutcome, mpsc::RecvError> {
        self.blocking.recv()
    }

    #[cfg(test)]
    fn try_recv(&self) -> Result<CompletionOutcome, mpsc::TryRecvError> {
        self.blocking.try_recv()
    }
}

impl Future for CompletionReceiver {
    type Output = Result<CompletionOutcome, tokio::sync::oneshot::error::RecvError>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.asynchronous).poll(context)
    }
}

impl CompletionSlot {
    fn settle(&mut self, outcome: CompletionOutcome) {
        #[cfg(test)]
        if let Some(blocking) = self.blocking.take() {
            let _ = blocking.send(outcome.clone());
        }
        if let Some(asynchronous) = self.asynchronous.take() {
            let _ = asynchronous.send(outcome);
        }
    }
}

struct Root<W> {
    id: RootId,
    work: QueuedWork<W>,
    completion: CompletionSlot,
}

struct QueuedWork<W> {
    work: W,
    enqueued_at: Instant,
}

struct ActiveRoot<W> {
    id: RootId,
    completion: CompletionSlot,
    continuations: VecDeque<QueuedWork<W>>,
}

struct Extension<W> {
    key: ExtensionKey,
    state: ExtensionState,
    roots: VecDeque<Root<W>>,
    active: Option<ActiveRoot<W>>,
    running: Option<TurnId>,
    max_queue_depth: usize,
    turn_count: u64,
    last_worker: Option<usize>,
    worker_movements: u64,
    max_enqueue_to_start_lag: Duration,
}

/// Deterministic scheduling policy used by the worker pool.
///
/// `ExtensionState::Queued` is also the ready-queue deduplication bit. All
/// transitions into the ready queue go through `make_ready`.
pub struct StateMachine<W> {
    extensions: HashMap<ExtensionId, Extension<W>>,
    ready: VecDeque<ExtensionKey>,
    next_root: u64,
    next_turn: u64,
    shutting_down: bool,
    max_queue_depth: usize,
    turn_count: u64,
    worker_movements: u64,
    max_enqueue_to_start_lag: Duration,
}

impl<W> Default for StateMachine<W> {
    fn default() -> Self {
        Self {
            extensions: HashMap::new(),
            ready: VecDeque::new(),
            next_root: 1,
            next_turn: 1,
            shutting_down: false,
            max_queue_depth: 0,
            turn_count: 0,
            worker_movements: 0,
            max_enqueue_to_start_lag: Duration::ZERO,
        }
    }
}

impl<W> StateMachine<W> {
    pub fn admit(&mut self, key: ExtensionKey) -> Result<(), SchedulerError> {
        if self.shutting_down {
            return Err(SchedulerError::ShuttingDown);
        }
        if let Some(existing) = self.extensions.get(&key.extension) {
            if existing.key == key {
                return Err(SchedulerError::AlreadyAdmitted(key));
            }
            if !matches!(
                existing.state,
                ExtensionState::Stopping | ExtensionState::Failed(_)
            ) || existing.running.is_some()
            {
                return Err(SchedulerError::AlreadyAdmitted(existing.key));
            }
        }
        self.extensions.insert(
            key.extension,
            Extension {
                key,
                state: ExtensionState::Loading,
                roots: VecDeque::new(),
                active: None,
                running: None,
                max_queue_depth: 0,
                turn_count: 0,
                last_worker: None,
                worker_movements: 0,
                max_enqueue_to_start_lag: Duration::ZERO,
            },
        );
        Ok(())
    }

    pub fn finish_loading(
        &mut self,
        key: ExtensionKey,
        result: Result<(), Failure>,
    ) -> Result<(), SchedulerError> {
        let extension = self.extension_mut(key)?;
        if extension.state != ExtensionState::Loading {
            return Err(SchedulerError::NotLoading);
        }
        match result {
            Ok(()) if extension.roots.is_empty() => extension.state = ExtensionState::Idle,
            Ok(()) => self.make_ready(key),
            Err(failure) => self.fail(key, failure)?,
        }
        Ok(())
    }

    pub fn enqueue_root(
        &mut self,
        key: ExtensionKey,
        work: W,
    ) -> Result<(RootId, CompletionReceiver), SchedulerError> {
        if self.shutting_down {
            return Err(SchedulerError::ShuttingDown);
        }
        let id = RootId::new(self.next_root);
        self.next_root += 1;
        #[cfg(test)]
        let (sender, receiver) = mpsc::channel();
        let (async_sender, async_receiver) = tokio::sync::oneshot::channel();
        let was_idle = {
            let extension = self.extension_mut(key)?;
            match &extension.state {
                ExtensionState::Stopping => return Err(SchedulerError::Stopping),
                ExtensionState::Failed(failure) => {
                    return Err(SchedulerError::Failed(failure.clone()));
                }
                _ => {}
            }
            extension.roots.push_back(Root {
                id,
                work: QueuedWork {
                    work,
                    enqueued_at: Instant::now(),
                },
                completion: CompletionSlot {
                    #[cfg(test)]
                    blocking: Some(sender),
                    asynchronous: Some(async_sender),
                },
            });
            extension.state == ExtensionState::Idle
        };
        self.record_queue_depth(key);
        if was_idle {
            self.make_ready(key);
        }
        Ok((
            id,
            CompletionReceiver {
                #[cfg(test)]
                blocking: receiver,
                asynchronous: async_receiver,
            },
        ))
    }

    pub fn enqueue_continuation(
        &mut self,
        key: ExtensionKey,
        root: RootId,
        work: W,
    ) -> Result<(), SchedulerError> {
        if self.shutting_down {
            return Err(SchedulerError::ShuttingDown);
        }
        let was_awaiting = {
            let extension = self.extension_mut(key)?;
            match &extension.state {
                ExtensionState::Stopping => return Err(SchedulerError::Stopping),
                ExtensionState::Failed(failure) => {
                    return Err(SchedulerError::Failed(failure.clone()));
                }
                _ => {}
            }
            let active = extension
                .active
                .as_mut()
                .ok_or(SchedulerError::NoActiveRoot)?;
            if active.id != root {
                return Err(SchedulerError::WrongRoot {
                    active: active.id,
                    received: root,
                });
            }
            active.continuations.push_back(QueuedWork {
                work,
                enqueued_at: Instant::now(),
            });
            extension.state == ExtensionState::AwaitingHostWork
        };
        self.record_queue_depth(key);
        if was_awaiting {
            self.make_ready(key);
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn next_turn(&mut self) -> Option<Turn<W>> {
        self.next_turn_on_worker(0)
    }

    fn next_turn_on_worker(&mut self, worker: usize) -> Option<Turn<W>> {
        while let Some(key) = self.ready.pop_front() {
            let extension = self.extensions.get_mut(&key.extension)?;
            if extension.key != key || extension.state != ExtensionState::Queued {
                continue;
            }
            let (root, queued) = if let Some(active) = extension.active.as_mut() {
                let work = active.continuations.pop_front()?;
                (active.id, work)
            } else {
                let pending = extension.roots.pop_front()?;
                let root = pending.id;
                let work = pending.work;
                extension.active = Some(ActiveRoot {
                    id: root,
                    completion: pending.completion,
                    continuations: VecDeque::new(),
                });
                (root, work)
            };
            let lag = queued.enqueued_at.elapsed();
            extension.max_enqueue_to_start_lag = extension.max_enqueue_to_start_lag.max(lag);
            extension.turn_count += 1;
            if extension
                .last_worker
                .is_some_and(|previous| previous != worker)
            {
                extension.worker_movements += 1;
                self.worker_movements += 1;
            }
            extension.last_worker = Some(worker);
            self.turn_count += 1;
            self.max_enqueue_to_start_lag = self.max_enqueue_to_start_lag.max(lag);
            let id = TurnId(self.next_turn);
            self.next_turn += 1;
            extension.state = ExtensionState::Running;
            extension.running = Some(id);
            return Some(Turn {
                key,
                root,
                id,
                work: queued.work,
            });
        }
        None
    }

    pub fn finish_turn(
        &mut self,
        key: ExtensionKey,
        turn: TurnId,
        outcome: TurnOutcome,
    ) -> Result<(), SchedulerError> {
        let extension = self.extension_mut(key)?;
        let Some(running) = extension.running.take() else {
            return Err(SchedulerError::DuplicateOrStaleTurn(turn));
        };
        if running != turn {
            extension.running = Some(running);
            return Err(SchedulerError::DuplicateOrStaleTurn(turn));
        }
        if extension.state == ExtensionState::Stopping {
            self.settle_active(key, CompletionOutcome::Cancelled);
            return Ok(());
        }
        if extension.state != ExtensionState::Running {
            return Err(SchedulerError::NotRunning);
        }

        match outcome {
            TurnOutcome::Completed => {
                self.settle_active(key, CompletionOutcome::Completed);
                if self.extension(key)?.roots.is_empty() {
                    self.extension_mut(key)?.state = ExtensionState::Idle;
                } else {
                    self.make_ready(key);
                }
            }
            TurnOutcome::AwaitingHostWork => {
                if self
                    .extension(key)?
                    .active
                    .as_ref()
                    .is_some_and(|active| !active.continuations.is_empty())
                {
                    self.make_ready(key);
                } else {
                    self.extension_mut(key)?.state = ExtensionState::AwaitingHostWork;
                }
            }
            #[cfg(test)]
            TurnOutcome::ReadyForContinuation => {
                if self
                    .extension(key)?
                    .active
                    .as_ref()
                    .is_some_and(|active| active.continuations.is_empty())
                {
                    self.extension_mut(key)?.state = ExtensionState::AwaitingHostWork;
                    return Err(SchedulerError::NoContinuation);
                }
                self.make_ready(key);
            }
            TurnOutcome::Fatal(failure) => self.fail(key, failure)?,
        }
        Ok(())
    }

    pub fn stop(&mut self, key: ExtensionKey) -> Result<(), SchedulerError> {
        self.extension(key)?;
        self.remove_ready(key);
        let extension = self.extension_mut(key)?;
        let was_running = extension.running.is_some();
        extension.state = ExtensionState::Stopping;
        settle_roots(&mut extension.roots, CompletionOutcome::Cancelled);
        if !was_running {
            if let Some(active) = extension.active.as_mut() {
                active.completion.settle(CompletionOutcome::Cancelled);
            }
            extension.active = None;
        }
        Ok(())
    }

    pub fn shutdown(&mut self) {
        if self.shutting_down {
            return;
        }
        self.shutting_down = true;
        self.ready.clear();
        for extension in self.extensions.values_mut() {
            let was_running = extension.running.is_some();
            extension.state = ExtensionState::Stopping;
            settle_roots(&mut extension.roots, CompletionOutcome::Cancelled);
            if !was_running {
                if let Some(active) = extension.active.as_mut() {
                    active.completion.settle(CompletionOutcome::Cancelled);
                }
                extension.active = None;
            }
        }
    }

    #[cfg(test)]
    pub fn state(&self, key: ExtensionKey) -> Result<&ExtensionState, SchedulerError> {
        Ok(&self.extension(key)?.state)
    }

    pub fn is_shutting_down(&self) -> bool {
        self.shutting_down
    }

    #[cfg(test)]
    fn diagnostics(&self, worker_count: usize) -> SchedulerDiagnostics {
        let mut extensions = self
            .extensions
            .values()
            .map(|extension| ExtensionDiagnostics {
                key: extension.key,
                state: extension.state.clone(),
                queue_depth: extension.queue_depth(),
                max_queue_depth: extension.max_queue_depth,
                turn_count: extension.turn_count,
                worker_movements: extension.worker_movements,
                max_enqueue_to_start_lag: extension.max_enqueue_to_start_lag,
            })
            .collect::<Vec<_>>();
        extensions.sort_by_key(|extension| extension.key.extension.value());
        SchedulerDiagnostics {
            worker_count,
            ready_queue_depth: self.ready.len(),
            queue_depth: self.queue_depth(),
            max_queue_depth: self.max_queue_depth,
            turn_count: self.turn_count,
            worker_movements: self.worker_movements,
            max_enqueue_to_start_lag: self.max_enqueue_to_start_lag,
            shutting_down: self.shutting_down,
            extensions,
        }
    }

    fn record_queue_depth(&mut self, key: ExtensionKey) {
        let extension_depth = self
            .extensions
            .get(&key.extension)
            .expect("admitted extension")
            .queue_depth();
        let extension = self
            .extensions
            .get_mut(&key.extension)
            .expect("admitted extension");
        extension.max_queue_depth = extension.max_queue_depth.max(extension_depth);
        self.max_queue_depth = self.max_queue_depth.max(self.queue_depth());
    }

    fn queue_depth(&self) -> usize {
        self.extensions.values().map(Extension::queue_depth).sum()
    }

    fn extension(&self, key: ExtensionKey) -> Result<&Extension<W>, SchedulerError> {
        let Some(extension) = self.extensions.get(&key.extension) else {
            return Err(SchedulerError::UnknownExtension(key.extension));
        };
        if extension.key.lifecycle != key.lifecycle {
            return Err(SchedulerError::StaleLifecycle {
                current: extension.key.lifecycle,
                received: key.lifecycle,
            });
        }
        Ok(extension)
    }

    fn extension_mut(&mut self, key: ExtensionKey) -> Result<&mut Extension<W>, SchedulerError> {
        let Some(extension) = self.extensions.get_mut(&key.extension) else {
            return Err(SchedulerError::UnknownExtension(key.extension));
        };
        if extension.key.lifecycle != key.lifecycle {
            return Err(SchedulerError::StaleLifecycle {
                current: extension.key.lifecycle,
                received: key.lifecycle,
            });
        }
        Ok(extension)
    }

    fn make_ready(&mut self, key: ExtensionKey) {
        let extension = self
            .extensions
            .get_mut(&key.extension)
            .expect("admitted extension");
        if extension.state == ExtensionState::Queued {
            return;
        }
        extension.state = ExtensionState::Queued;
        self.ready.push_back(key);
    }

    fn remove_ready(&mut self, key: ExtensionKey) {
        self.ready.retain(|queued| *queued != key);
    }

    fn settle_active(&mut self, key: ExtensionKey, outcome: CompletionOutcome) {
        let extension = self
            .extensions
            .get_mut(&key.extension)
            .expect("admitted extension");
        if let Some(mut active) = extension.active.take() {
            active.completion.settle(outcome);
        }
    }

    fn fail(&mut self, key: ExtensionKey, failure: Failure) -> Result<(), SchedulerError> {
        self.remove_ready(key);
        let extension = self.extension_mut(key)?;
        extension.running = None;
        if let Some(active) = extension.active.as_mut() {
            active
                .completion
                .settle(CompletionOutcome::Failed(failure.clone()));
        }
        extension.active = None;
        settle_roots(
            &mut extension.roots,
            CompletionOutcome::Failed(failure.clone()),
        );
        extension.state = ExtensionState::Failed(failure);
        Ok(())
    }
}

impl<W> Extension<W> {
    fn queue_depth(&self) -> usize {
        self.roots.len()
            + self
                .active
                .as_ref()
                .map_or(0, |active| active.continuations.len())
    }
}

fn settle_roots<W>(roots: &mut VecDeque<Root<W>>, outcome: CompletionOutcome) {
    for root in roots.iter_mut() {
        root.completion.settle(outcome.clone());
    }
    roots.clear();
}

struct Shared<W> {
    state: Mutex<StateMachine<W>>,
    ready: Condvar,
    #[cfg(test)]
    worker_count: usize,
}

/// Cloneable admission and wakeup side of a scheduler pool.
pub struct SchedulerHandle<W> {
    shared: Arc<Shared<W>>,
}

impl<W> Clone for SchedulerHandle<W> {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<W> SchedulerHandle<W> {
    pub fn admit(&self, key: ExtensionKey) -> Result<(), SchedulerError> {
        self.shared.state.lock().unwrap().admit(key)
    }

    pub fn finish_loading(
        &self,
        key: ExtensionKey,
        result: Result<(), Failure>,
    ) -> Result<(), SchedulerError> {
        let result = self
            .shared
            .state
            .lock()
            .unwrap()
            .finish_loading(key, result);
        self.shared.ready.notify_all();
        result
    }

    pub fn enqueue_root(
        &self,
        key: ExtensionKey,
        work: W,
    ) -> Result<(RootId, CompletionReceiver), SchedulerError> {
        let result = self.shared.state.lock().unwrap().enqueue_root(key, work);
        self.shared.ready.notify_one();
        result
    }

    pub fn enqueue_continuation(
        &self,
        key: ExtensionKey,
        root: RootId,
        work: W,
    ) -> Result<(), SchedulerError> {
        let result = self
            .shared
            .state
            .lock()
            .unwrap()
            .enqueue_continuation(key, root, work);
        self.shared.ready.notify_one();
        result
    }

    pub fn stop(&self, key: ExtensionKey) -> Result<(), SchedulerError> {
        self.shared.state.lock().unwrap().stop(key)
    }

    #[cfg(test)]
    pub fn state(&self, key: ExtensionKey) -> Result<ExtensionState, SchedulerError> {
        self.shared.state.lock().unwrap().state(key).cloned()
    }

    #[cfg(test)]
    pub fn diagnostics(&self) -> SchedulerDiagnostics {
        self.shared
            .state
            .lock()
            .unwrap()
            .diagnostics(self.shared.worker_count)
    }
}

/// Owns a fixed set of worker threads for its entire lifetime.
///
/// The executor receives opaque work and returns only scheduler state. V8
/// initialization and isolate entry remain the engine owner's responsibility.
pub struct SchedulerPool<W> {
    handle: SchedulerHandle<W>,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

impl<W: Send + 'static> SchedulerPool<W> {
    pub fn new(
        config: PoolConfig,
        executor: impl Fn(Turn<W>) -> TurnOutcome + Send + Sync + 'static,
    ) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(StateMachine::default()),
            ready: Condvar::new(),
            #[cfg(test)]
            worker_count: config.workers().get(),
        });
        let executor = Arc::new(executor);
        let mut workers = Vec::with_capacity(config.workers().get());
        for index in 0..config.workers().get() {
            let shared = Arc::clone(&shared);
            let executor = Arc::clone(&executor);
            workers.push(
                std::thread::Builder::new()
                    .name(format!("knot-pool-worker-{index}"))
                    .spawn(move || worker_loop(index, shared, executor))
                    .expect("extension worker thread creation must succeed"),
            );
        }
        Self {
            handle: SchedulerHandle { shared },
            workers: Mutex::new(workers),
        }
    }

    pub fn handle(&self) -> SchedulerHandle<W> {
        self.handle.clone()
    }

    #[cfg(test)]
    pub fn worker_count(&self) -> usize {
        self.handle.shared.worker_count
    }

    #[cfg(test)]
    pub fn diagnostics(&self) -> SchedulerDiagnostics {
        self.handle.diagnostics()
    }

    pub fn shutdown(self) {
        self.shutdown_workers();
    }

    fn shutdown_workers(&self) {
        self.handle.shared.state.lock().unwrap().shutdown();
        self.handle.shared.ready.notify_all();
        for worker in self.workers.lock().unwrap().drain(..) {
            if worker.thread().id() == std::thread::current().id() {
                continue;
            }
            let _ = worker.join();
        }
    }
}

impl<W> Drop for SchedulerPool<W> {
    fn drop(&mut self) {
        self.handle.shared.state.lock().unwrap().shutdown();
        self.handle.shared.ready.notify_all();
        for worker in self.workers.get_mut().unwrap().drain(..) {
            if worker.thread().id() != std::thread::current().id() {
                let _ = worker.join();
            }
        }
    }
}

fn worker_loop<W: Send + 'static>(
    worker: usize,
    shared: Arc<Shared<W>>,
    executor: Arc<impl Fn(Turn<W>) -> TurnOutcome + Send + Sync + 'static>,
) {
    loop {
        let turn = {
            let mut state = shared.state.lock().unwrap();
            loop {
                if let Some(turn) = state.next_turn_on_worker(worker) {
                    break turn;
                }
                if state.is_shutting_down() {
                    return;
                }
                state = shared.ready.wait(state).unwrap();
            }
        };
        let key = turn.key;
        let id = turn.id;
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| executor(turn)))
            .unwrap_or_else(|_| TurnOutcome::Fatal(Failure::new("turn executor panicked")));
        let mut state = shared.state.lock().unwrap();
        if let Err(error) = state.finish_turn(key, id, outcome) {
            let failure = Failure::new(format!("scheduler invariant violated: {error}"));
            let _ = state.fail(key, failure);
        }
        drop(state);
        shared.ready.notify_all();
    }
}

#[cfg(test)]
mod tests;
