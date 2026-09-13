//! Bounded extension scheduling, independent of V8 mechanics.

use std::{
    collections::{HashMap, VecDeque},
    num::NonZeroUsize,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::JoinHandle,
};

use super::{
    lifecycle::{ExtensionKey, ExtensionState, Failure},
    protocol::{ExtensionId, ExtensionLifecycleId},
};

/// Proof that process-wide engine initialization preceded worker creation.
#[derive(Clone, Copy, Debug)]
pub struct WorkerThreadPermit {
    _private: (),
}

impl WorkerThreadPermit {
    pub(crate) const fn new() -> Self {
        Self { _private: () }
    }
}

/// Fixed worker count for one scheduler pool lifetime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PoolConfig {
    workers: NonZeroUsize,
}

impl PoolConfig {
    pub const fn new(workers: NonZeroUsize) -> Self {
        Self { workers }
    }

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

/// Whether a dispatched payload starts a root or resumes its command tree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TurnKind {
    Root,
    Continuation,
}

/// One unit of opaque work selected for a worker.
#[derive(Debug)]
pub struct Turn<W> {
    pub key: ExtensionKey,
    pub root: RootId,
    pub id: TurnId,
    pub kind: TurnKind,
    pub cancellation: CancellationToken,
    pub work: W,
}

/// Cooperative cancellation signal shared with the active turn executor.
#[derive(Clone, Debug)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
}

/// The scheduler-relevant result of executing one turn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TurnOutcome {
    Completed,
    AwaitingHostWork,
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

/// Final disposition delivered exactly once for each accepted continuation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContinuationOutcome {
    Applied,
    Discarded,
    Cancelled,
    Failed(Failure),
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

struct CompletionSlot<T>(Option<mpsc::Sender<T>>);

impl<T> CompletionSlot<T> {
    fn settle(&mut self, outcome: T) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(outcome);
        }
    }
}

struct Root<W> {
    id: RootId,
    work: W,
    completion: CompletionSlot<CompletionOutcome>,
    cancellation: CancellationToken,
}

struct Continuation<W> {
    work: W,
    completion: CompletionSlot<ContinuationOutcome>,
}

struct ActiveRoot<W> {
    id: RootId,
    completion: CompletionSlot<CompletionOutcome>,
    cancellation: CancellationToken,
    continuations: VecDeque<Continuation<W>>,
}

struct RunningTurn {
    id: TurnId,
    continuation: Option<CompletionSlot<ContinuationOutcome>>,
}

struct Extension<W> {
    key: ExtensionKey,
    state: ExtensionState,
    roots: VecDeque<Root<W>>,
    active: Option<ActiveRoot<W>>,
    running: Option<RunningTurn>,
    stop_waiters: Vec<mpsc::Sender<()>>,
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
}

impl<W> Default for StateMachine<W> {
    fn default() -> Self {
        Self {
            extensions: HashMap::new(),
            ready: VecDeque::new(),
            next_root: 1,
            next_turn: 1,
            shutting_down: false,
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
                stop_waiters: Vec::new(),
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
    ) -> Result<(RootId, mpsc::Receiver<CompletionOutcome>), SchedulerError> {
        if self.shutting_down {
            return Err(SchedulerError::ShuttingDown);
        }
        let id = RootId::new(self.next_root);
        self.next_root += 1;
        let (sender, receiver) = mpsc::channel();
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
            work,
            completion: CompletionSlot(Some(sender)),
            cancellation: CancellationToken::new(),
        });
        if extension.state == ExtensionState::Idle {
            self.make_ready(key);
        }
        Ok((id, receiver))
    }

    pub fn enqueue_continuation(
        &mut self,
        key: ExtensionKey,
        root: RootId,
        work: W,
    ) -> Result<mpsc::Receiver<ContinuationOutcome>, SchedulerError> {
        if self.shutting_down {
            return Err(SchedulerError::ShuttingDown);
        }
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
        let (sender, receiver) = mpsc::channel();
        active.continuations.push_back(Continuation {
            work,
            completion: CompletionSlot(Some(sender)),
        });
        if extension.state == ExtensionState::AwaitingHostWork {
            self.make_ready(key);
        }
        Ok(receiver)
    }

    pub fn next_turn(&mut self) -> Option<Turn<W>> {
        while let Some(key) = self.ready.pop_front() {
            let extension = self.extensions.get_mut(&key.extension)?;
            if extension.key != key || extension.state != ExtensionState::Queued {
                continue;
            }
            let (root, kind, cancellation, work, continuation) =
                if let Some(active) = extension.active.as_mut() {
                    let continuation = active.continuations.pop_front()?;
                    (
                        active.id,
                        TurnKind::Continuation,
                        active.cancellation.clone(),
                        continuation.work,
                        Some(continuation.completion),
                    )
                } else {
                    let pending = extension.roots.pop_front()?;
                    let root = pending.id;
                    let work = pending.work;
                    let cancellation = pending.cancellation;
                    extension.active = Some(ActiveRoot {
                        id: root,
                        completion: pending.completion,
                        cancellation: cancellation.clone(),
                        continuations: VecDeque::new(),
                    });
                    (root, TurnKind::Root, cancellation, work, None)
                };
            let id = TurnId(self.next_turn);
            self.next_turn += 1;
            extension.state = ExtensionState::Running;
            extension.running = Some(RunningTurn { id, continuation });
            return Some(Turn {
                key,
                root,
                id,
                kind,
                cancellation,
                work,
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
        if running.id != turn {
            extension.running = Some(running);
            return Err(SchedulerError::DuplicateOrStaleTurn(turn));
        }
        if extension.state == ExtensionState::Stopping {
            settle_continuation(running.continuation, ContinuationOutcome::Cancelled);
            self.settle_active(key, CompletionOutcome::Cancelled);
            self.finish_stopping(key);
            return Ok(());
        }
        if extension.state != ExtensionState::Running {
            return Err(SchedulerError::NotRunning);
        }

        let continuation_failure = match &outcome {
            TurnOutcome::Fatal(failure) => Some(failure.clone()),
            _ => None,
        };
        settle_continuation(
            running.continuation,
            continuation_failure.map_or(ContinuationOutcome::Applied, ContinuationOutcome::Failed),
        );

        match outcome {
            TurnOutcome::Completed => {
                self.settle_queued_continuations(key, ContinuationOutcome::Discarded);
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

    pub fn stop(&mut self, key: ExtensionKey) -> Result<mpsc::Receiver<()>, SchedulerError> {
        self.extension(key)?;
        self.remove_ready(key);
        let (sender, receiver) = mpsc::channel();
        let extension = self.extension_mut(key)?;
        let was_running = extension.running.is_some();
        extension.state = ExtensionState::Stopping;
        extension.stop_waiters.push(sender);
        settle_roots(&mut extension.roots, CompletionOutcome::Cancelled);
        if let Some(active) = extension.active.as_mut() {
            active.cancellation.cancel();
            settle_continuations(&mut active.continuations, ContinuationOutcome::Cancelled);
        }
        if !was_running {
            if let Some(active) = extension.active.as_mut() {
                active.completion.settle(CompletionOutcome::Cancelled);
            }
            extension.active = None;
            notify_stopped(extension);
        }
        Ok(receiver)
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
            if let Some(active) = extension.active.as_mut() {
                active.cancellation.cancel();
                settle_continuations(&mut active.continuations, ContinuationOutcome::Cancelled);
            }
            if !was_running {
                if let Some(active) = extension.active.as_mut() {
                    active.completion.settle(CompletionOutcome::Cancelled);
                }
                extension.active = None;
                notify_stopped(extension);
            }
        }
    }

    pub fn state(&self, key: ExtensionKey) -> Result<&ExtensionState, SchedulerError> {
        Ok(&self.extension(key)?.state)
    }

    pub fn is_shutting_down(&self) -> bool {
        self.shutting_down
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

    fn settle_queued_continuations(&mut self, key: ExtensionKey, outcome: ContinuationOutcome) {
        if let Some(active) = self
            .extensions
            .get_mut(&key.extension)
            .and_then(|extension| extension.active.as_mut())
        {
            settle_continuations(&mut active.continuations, outcome);
        }
    }

    fn finish_stopping(&mut self, key: ExtensionKey) {
        let extension = self
            .extensions
            .get_mut(&key.extension)
            .expect("admitted extension");
        notify_stopped(extension);
    }

    fn fail(&mut self, key: ExtensionKey, failure: Failure) -> Result<(), SchedulerError> {
        self.remove_ready(key);
        let extension = self.extension_mut(key)?;
        extension.running = None;
        if let Some(active) = extension.active.as_mut() {
            active.cancellation.cancel();
            settle_continuations(
                &mut active.continuations,
                ContinuationOutcome::Failed(failure.clone()),
            );
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

fn settle_roots<W>(roots: &mut VecDeque<Root<W>>, outcome: CompletionOutcome) {
    for root in roots.iter_mut() {
        root.cancellation.cancel();
        root.completion.settle(outcome.clone());
    }
    roots.clear();
}

fn settle_continuation(
    continuation: Option<CompletionSlot<ContinuationOutcome>>,
    outcome: ContinuationOutcome,
) {
    if let Some(mut continuation) = continuation {
        continuation.settle(outcome);
    }
}

fn settle_continuations<W>(
    continuations: &mut VecDeque<Continuation<W>>,
    outcome: ContinuationOutcome,
) {
    for continuation in continuations.iter_mut() {
        continuation.completion.settle(outcome.clone());
    }
    continuations.clear();
}

fn notify_stopped<W>(extension: &mut Extension<W>) {
    for waiter in extension.stop_waiters.drain(..) {
        let _ = waiter.send(());
    }
}

struct Shared<W> {
    state: Mutex<StateMachine<W>>,
    ready: Condvar,
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
    ) -> Result<(RootId, mpsc::Receiver<CompletionOutcome>), SchedulerError> {
        let result = self.shared.state.lock().unwrap().enqueue_root(key, work);
        self.shared.ready.notify_one();
        result
    }

    pub fn enqueue_continuation(
        &self,
        key: ExtensionKey,
        root: RootId,
        work: W,
    ) -> Result<mpsc::Receiver<ContinuationOutcome>, SchedulerError> {
        let result = self
            .shared
            .state
            .lock()
            .unwrap()
            .enqueue_continuation(key, root, work);
        self.shared.ready.notify_one();
        result
    }

    pub fn stop(&self, key: ExtensionKey) -> Result<mpsc::Receiver<()>, SchedulerError> {
        self.shared.state.lock().unwrap().stop(key)
    }

    pub fn state(&self, key: ExtensionKey) -> Result<ExtensionState, SchedulerError> {
        self.shared.state.lock().unwrap().state(key).cloned()
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
        _permit: WorkerThreadPermit,
        config: PoolConfig,
        executor: impl Fn(Turn<W>) -> TurnOutcome + Send + Sync + 'static,
    ) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(StateMachine::default()),
            ready: Condvar::new(),
        });
        let executor = Arc::new(executor);
        let mut workers = Vec::with_capacity(config.workers().get());
        for index in 0..config.workers().get() {
            let shared = Arc::clone(&shared);
            let executor = Arc::clone(&executor);
            workers.push(
                std::thread::Builder::new()
                    .name(format!("knot-extension-{index}"))
                    .spawn(move || worker_loop(shared, executor))
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

    pub fn worker_count(&self) -> usize {
        self.workers.lock().unwrap().len()
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
    shared: Arc<Shared<W>>,
    executor: Arc<impl Fn(Turn<W>) -> TurnOutcome + Send + Sync + 'static>,
) {
    loop {
        let turn = {
            let mut state = shared.state.lock().unwrap();
            loop {
                if let Some(turn) = state.next_turn() {
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
mod tests {
    use super::*;
    use std::time::Duration;

    fn key(extension: u64, lifecycle: u64) -> ExtensionKey {
        ExtensionKey::new(
            ExtensionId::new(extension),
            ExtensionLifecycleId::new(lifecycle),
        )
    }

    fn admitted() -> (StateMachine<&'static str>, ExtensionKey) {
        let key = key(1, 1);
        let mut scheduler = StateMachine::default();
        scheduler.admit(key).unwrap();
        scheduler.finish_loading(key, Ok(())).unwrap();
        (scheduler, key)
    }

    #[test]
    fn transitions_through_root_and_host_continuation() {
        let (mut scheduler, key) = admitted();
        assert_eq!(scheduler.state(key), Ok(&ExtensionState::Idle));
        let (root, completion) = scheduler.enqueue_root(key, "root").unwrap();
        assert_eq!(scheduler.state(key), Ok(&ExtensionState::Queued));

        let turn = scheduler.next_turn().unwrap();
        assert_eq!((turn.kind, turn.work), (TurnKind::Root, "root"));
        scheduler
            .finish_turn(key, turn.id, TurnOutcome::AwaitingHostWork)
            .unwrap();
        assert_eq!(scheduler.state(key), Ok(&ExtensionState::AwaitingHostWork));

        scheduler
            .enqueue_continuation(key, root, "response")
            .unwrap();
        let turn = scheduler.next_turn().unwrap();
        assert_eq!((turn.kind, turn.work), (TurnKind::Continuation, "response"));
        scheduler
            .finish_turn(key, turn.id, TurnOutcome::Completed)
            .unwrap();
        assert_eq!(completion.recv().unwrap(), CompletionOutcome::Completed);
        assert_eq!(scheduler.state(key), Ok(&ExtensionState::Idle));
    }

    #[test]
    fn preserves_global_fifo_and_requeues_after_one_turn() {
        let first = key(1, 1);
        let second = key(2, 1);
        let mut scheduler = StateMachine::default();
        for key in [first, second] {
            scheduler.admit(key).unwrap();
            scheduler.finish_loading(key, Ok(())).unwrap();
        }
        let (first_root, _) = scheduler.enqueue_root(first, "first root").unwrap();
        scheduler.enqueue_root(second, "second root").unwrap();

        let turn = scheduler.next_turn().unwrap();
        assert_eq!(turn.key, first);
        scheduler
            .enqueue_continuation(first, first_root, "first continuation")
            .unwrap();
        scheduler
            .finish_turn(first, turn.id, TurnOutcome::ReadyForContinuation)
            .unwrap();

        assert_eq!(scheduler.next_turn().unwrap().key, second);
        assert_eq!(scheduler.next_turn().unwrap().key, first);
    }

    #[test]
    fn unrelated_roots_wait_behind_an_awaiting_tree() {
        let (mut scheduler, key) = admitted();
        let (active, _) = scheduler.enqueue_root(key, "active").unwrap();
        scheduler.enqueue_root(key, "unrelated").unwrap();
        let turn = scheduler.next_turn().unwrap();
        scheduler
            .finish_turn(key, turn.id, TurnOutcome::AwaitingHostWork)
            .unwrap();
        assert!(scheduler.next_turn().is_none());

        scheduler
            .enqueue_continuation(key, active, "response")
            .unwrap();
        let turn = scheduler.next_turn().unwrap();
        scheduler
            .finish_turn(key, turn.id, TurnOutcome::Completed)
            .unwrap();
        assert_eq!(scheduler.next_turn().unwrap().work, "unrelated");
    }

    #[test]
    fn rejects_wrong_tree_stale_lifecycle_and_duplicate_turns() {
        let (mut scheduler, key) = admitted();
        let (root, _) = scheduler.enqueue_root(key, "root").unwrap();
        let turn = scheduler.next_turn().unwrap();
        assert!(matches!(
            scheduler.enqueue_continuation(key, RootId::new(999), "wrong"),
            Err(SchedulerError::WrongRoot { active, .. }) if active == root
        ));
        let stale = ExtensionKey::new(key.extension, ExtensionLifecycleId::new(2));
        assert!(matches!(
            scheduler.enqueue_root(stale, "stale"),
            Err(SchedulerError::StaleLifecycle { .. })
        ));
        scheduler
            .finish_turn(key, turn.id, TurnOutcome::Completed)
            .unwrap();
        assert_eq!(
            scheduler.finish_turn(key, turn.id, TurnOutcome::Completed),
            Err(SchedulerError::DuplicateOrStaleTurn(turn.id))
        );
    }

    #[test]
    fn terminal_paths_settle_every_completion_once() {
        let (mut scheduler, key) = admitted();
        let (_, active) = scheduler.enqueue_root(key, "active").unwrap();
        let (_, queued) = scheduler.enqueue_root(key, "queued").unwrap();
        let turn = scheduler.next_turn().unwrap();
        let continuation = scheduler
            .enqueue_continuation(key, turn.root, "continuation")
            .unwrap();
        let failure = Failure::new("boom");
        scheduler
            .finish_turn(key, turn.id, TurnOutcome::Fatal(failure.clone()))
            .unwrap();
        assert_eq!(
            active.recv().unwrap(),
            CompletionOutcome::Failed(failure.clone())
        );
        assert_eq!(queued.recv().unwrap(), CompletionOutcome::Failed(failure));
        assert!(matches!(
            continuation.recv().unwrap(),
            ContinuationOutcome::Failed(failure) if failure.message() == "boom"
        ));
        assert!(active.try_recv().is_err());
        assert!(queued.try_recv().is_err());
    }

    #[test]
    fn stop_and_shutdown_cancel_all_work() {
        let (mut scheduler, first) = admitted();
        let second = key(2, 1);
        scheduler.admit(second).unwrap();
        scheduler.finish_loading(second, Ok(())).unwrap();
        let (_, running) = scheduler.enqueue_root(first, "running").unwrap();
        let (_, queued) = scheduler.enqueue_root(first, "queued").unwrap();
        let (_, other) = scheduler.enqueue_root(second, "other").unwrap();
        let turn = scheduler.next_turn().unwrap();
        let continuation = scheduler
            .enqueue_continuation(first, turn.root, "continuation")
            .unwrap();
        scheduler.stop(first).unwrap();
        assert_eq!(queued.recv().unwrap(), CompletionOutcome::Cancelled);
        assert_eq!(continuation.recv().unwrap(), ContinuationOutcome::Cancelled);
        assert!(running.try_recv().is_err());
        scheduler
            .finish_turn(first, turn.id, TurnOutcome::Completed)
            .unwrap();
        assert_eq!(running.recv().unwrap(), CompletionOutcome::Cancelled);

        scheduler.shutdown();
        assert_eq!(other.recv().unwrap(), CompletionOutcome::Cancelled);
        assert!(matches!(
            scheduler.enqueue_root(second, "late"),
            Err(SchedulerError::ShuttingDown)
        ));
    }

    #[test]
    fn load_failure_settles_work_queued_during_loading() {
        let key = key(1, 1);
        let mut scheduler = StateMachine::default();
        scheduler.admit(key).unwrap();
        let (_, completion) = scheduler.enqueue_root(key, "root").unwrap();
        let failure = Failure::new("load failed");
        scheduler.finish_loading(key, Err(failure.clone())).unwrap();
        assert_eq!(
            completion.recv().unwrap(),
            CompletionOutcome::Failed(failure.clone())
        );
        assert_eq!(scheduler.state(key), Ok(&ExtensionState::Failed(failure)));
    }

    #[test]
    fn replacement_lifecycle_rejects_stale_work() {
        let (mut scheduler, old) = admitted();
        scheduler.stop(old).unwrap();
        let replacement = key(1, 2);
        scheduler.admit(replacement).unwrap();
        scheduler.finish_loading(replacement, Ok(())).unwrap();

        assert!(matches!(
            scheduler.enqueue_root(old, "stale"),
            Err(SchedulerError::StaleLifecycle { current, received })
                if current == replacement.lifecycle && received == old.lifecycle
        ));
        assert_eq!(scheduler.state(replacement), Ok(&ExtensionState::Idle));
    }

    #[test]
    fn roots_queued_during_loading_preserve_fifo_order() {
        let key = key(1, 1);
        let mut scheduler = StateMachine::default();
        scheduler.admit(key).unwrap();
        scheduler.enqueue_root(key, "first").unwrap();
        scheduler.enqueue_root(key, "second").unwrap();
        assert!(scheduler.next_turn().is_none());

        scheduler.finish_loading(key, Ok(())).unwrap();
        let turn = scheduler.next_turn().unwrap();
        assert_eq!(turn.work, "first");
        scheduler
            .finish_turn(key, turn.id, TurnOutcome::Completed)
            .unwrap();
        assert_eq!(scheduler.next_turn().unwrap().work, "second");
    }

    #[test]
    fn reports_a_ready_without_continuation_invariant_violation() {
        let (mut scheduler, key) = admitted();
        scheduler.enqueue_root(key, "root").unwrap();
        let turn = scheduler.next_turn().unwrap();
        assert_eq!(
            scheduler.finish_turn(key, turn.id, TurnOutcome::ReadyForContinuation),
            Err(SchedulerError::NoContinuation)
        );
        assert_eq!(scheduler.state(key), Ok(&ExtensionState::AwaitingHostWork));
    }

    #[test]
    fn multiple_continuation_wakes_enqueue_the_extension_once() {
        let (mut scheduler, key) = admitted();
        let (root, _) = scheduler.enqueue_root(key, "root").unwrap();
        let turn = scheduler.next_turn().unwrap();
        scheduler
            .finish_turn(key, turn.id, TurnOutcome::AwaitingHostWork)
            .unwrap();
        let first = scheduler.enqueue_continuation(key, root, "first").unwrap();
        let second = scheduler.enqueue_continuation(key, root, "second").unwrap();

        let turn = scheduler.next_turn().unwrap();
        assert_eq!(turn.work, "first");
        assert!(scheduler.next_turn().is_none());
        scheduler
            .finish_turn(key, turn.id, TurnOutcome::ReadyForContinuation)
            .unwrap();
        assert_eq!(first.recv().unwrap(), ContinuationOutcome::Applied);
        assert_eq!(scheduler.next_turn().unwrap().work, "second");
        assert_eq!(second.try_recv(), Err(mpsc::TryRecvError::Empty));
    }

    #[test]
    fn continuation_racing_with_root_completion_is_settled_as_discarded() {
        let (mut scheduler, key) = admitted();
        let (root, root_completion) = scheduler.enqueue_root(key, "root").unwrap();
        let turn = scheduler.next_turn().unwrap();
        let continuation = scheduler
            .enqueue_continuation(key, root, "late response")
            .unwrap();

        scheduler
            .finish_turn(key, turn.id, TurnOutcome::Completed)
            .unwrap();
        assert_eq!(continuation.recv().unwrap(), ContinuationOutcome::Discarded);
        assert_eq!(
            root_completion.recv().unwrap(),
            CompletionOutcome::Completed
        );
    }

    #[test]
    fn stop_running_signals_cancellation_and_lifecycle_retirement() {
        let (mut scheduler, old) = admitted();
        let (_, completion) = scheduler.enqueue_root(old, "root").unwrap();
        let turn = scheduler.next_turn().unwrap();
        let cancellation = turn.cancellation.clone();
        let stopped = scheduler.stop(old).unwrap();

        assert!(cancellation.is_cancelled());
        assert_eq!(stopped.try_recv(), Err(mpsc::TryRecvError::Empty));
        scheduler
            .finish_turn(old, turn.id, TurnOutcome::Completed)
            .unwrap();
        stopped.recv().unwrap();
        assert_eq!(completion.recv().unwrap(), CompletionOutcome::Cancelled);

        let replacement = key(1, 2);
        scheduler.admit(replacement).unwrap();
    }

    struct BlockingWork {
        label: &'static str,
        started: mpsc::Sender<&'static str>,
        release: mpsc::Receiver<()>,
    }

    fn blocking_work(
        label: &'static str,
        started: &mpsc::Sender<&'static str>,
    ) -> (BlockingWork, mpsc::Sender<()>) {
        let (release, released) = mpsc::channel();
        (
            BlockingWork {
                label,
                started: started.clone(),
                release: released,
            },
            release,
        )
    }

    fn blocking_executor(work: Turn<BlockingWork>) -> TurnOutcome {
        work.work.started.send(work.work.label).unwrap();
        work.work.release.recv().unwrap();
        TurnOutcome::Completed
    }

    #[test]
    fn one_worker_serializes_independent_extensions() {
        let pool = SchedulerPool::new(
            super::super::engine::initialize(),
            PoolConfig::single_worker(),
            blocking_executor,
        );
        assert_eq!(pool.worker_count(), 1);
        let handle = pool.handle();
        let first = key(1, 1);
        let second = key(2, 1);
        for key in [first, second] {
            handle.admit(key).unwrap();
            handle.finish_loading(key, Ok(())).unwrap();
        }
        let (started, starts) = mpsc::channel();
        let (first_work, release_first) = blocking_work("first", &started);
        let (second_work, release_second) = blocking_work("second", &started);
        let (_, first_done) = handle.enqueue_root(first, first_work).unwrap();
        let (_, second_done) = handle.enqueue_root(second, second_work).unwrap();

        assert_eq!(
            starts.recv_timeout(Duration::from_secs(2)).unwrap(),
            "first"
        );
        assert!(starts.try_recv().is_err());
        release_first.send(()).unwrap();
        assert_eq!(
            starts.recv_timeout(Duration::from_secs(2)).unwrap(),
            "second"
        );
        release_second.send(()).unwrap();
        assert_eq!(first_done.recv().unwrap(), CompletionOutcome::Completed);
        assert_eq!(second_done.recv().unwrap(), CompletionOutcome::Completed);
        pool.shutdown();
    }

    #[test]
    fn two_workers_run_independent_extensions_concurrently() {
        let config = PoolConfig::new(NonZeroUsize::new(2).unwrap());
        let pool = SchedulerPool::new(
            super::super::engine::initialize(),
            config,
            blocking_executor,
        );
        assert_eq!(pool.worker_count(), 2);
        let handle = pool.handle();
        let first = key(1, 1);
        let second = key(2, 1);
        for key in [first, second] {
            handle.admit(key).unwrap();
            handle.finish_loading(key, Ok(())).unwrap();
        }
        let (started, starts) = mpsc::channel();
        let (first_work, release_first) = blocking_work("first", &started);
        let (second_work, release_second) = blocking_work("second", &started);
        let (_, first_done) = handle.enqueue_root(first, first_work).unwrap();
        let (_, second_done) = handle.enqueue_root(second, second_work).unwrap();

        let mut labels = [
            starts.recv_timeout(Duration::from_secs(2)).unwrap(),
            starts.recv_timeout(Duration::from_secs(2)).unwrap(),
        ];
        labels.sort_unstable();
        assert_eq!(labels, ["first", "second"]);
        release_first.send(()).unwrap();
        release_second.send(()).unwrap();
        assert_eq!(first_done.recv().unwrap(), CompletionOutcome::Completed);
        assert_eq!(second_done.recv().unwrap(), CompletionOutcome::Completed);
        pool.shutdown();
    }

    #[test]
    fn two_workers_never_run_one_extension_concurrently() {
        let pool = SchedulerPool::new(
            super::super::engine::initialize(),
            PoolConfig::new(NonZeroUsize::new(2).unwrap()),
            |turn: Turn<BlockingWork>| {
                let outcome = if turn.work.label == "root" {
                    TurnOutcome::ReadyForContinuation
                } else {
                    TurnOutcome::Completed
                };
                turn.work.started.send(turn.work.label).unwrap();
                turn.work.release.recv().unwrap();
                outcome
            },
        );
        let handle = pool.handle();
        let key = key(1, 1);
        handle.admit(key).unwrap();
        handle.finish_loading(key, Ok(())).unwrap();
        let (started, starts) = mpsc::channel();
        let (root_work, release_root) = blocking_work("root", &started);
        let (root, completed) = handle.enqueue_root(key, root_work).unwrap();
        assert_eq!(starts.recv_timeout(Duration::from_secs(2)).unwrap(), "root");
        let (continuation_work, release_continuation) = blocking_work("continuation", &started);
        let applied = handle
            .enqueue_continuation(key, root, continuation_work)
            .unwrap();

        assert_eq!(starts.try_recv(), Err(mpsc::TryRecvError::Empty));
        release_root.send(()).unwrap();
        assert_eq!(
            starts.recv_timeout(Duration::from_secs(2)).unwrap(),
            "continuation"
        );
        release_continuation.send(()).unwrap();
        assert_eq!(applied.recv().unwrap(), ContinuationOutcome::Applied);
        assert_eq!(completed.recv().unwrap(), CompletionOutcome::Completed);
        pool.shutdown();
    }

    #[test]
    fn pool_shutdown_cancels_a_cooperative_running_turn_and_joins() {
        let (started, starts) = mpsc::channel();
        let pool = SchedulerPool::new(
            super::super::engine::initialize(),
            PoolConfig::single_worker(),
            move |turn: Turn<&'static str>| {
                started.send(()).unwrap();
                while !turn.cancellation.is_cancelled() {
                    std::thread::yield_now();
                }
                TurnOutcome::Completed
            },
        );
        let handle = pool.handle();
        let key = key(1, 1);
        handle.admit(key).unwrap();
        handle.finish_loading(key, Ok(())).unwrap();
        let (_, completion) = handle.enqueue_root(key, "root").unwrap();
        starts.recv_timeout(Duration::from_secs(2)).unwrap();

        pool.shutdown();
        assert_eq!(completion.recv().unwrap(), CompletionOutcome::Cancelled);
    }

    #[test]
    fn executor_panic_fails_only_its_extension() {
        let pool = SchedulerPool::new(
            super::super::engine::initialize(),
            PoolConfig::single_worker(),
            |turn: Turn<&'static str>| {
                assert_ne!(turn.work, "panic");
                TurnOutcome::Completed
            },
        );
        let handle = pool.handle();
        let failed_key = key(1, 1);
        let neighbor = key(2, 1);
        for key in [failed_key, neighbor] {
            handle.admit(key).unwrap();
            handle.finish_loading(key, Ok(())).unwrap();
        }
        let (_, failed) = handle.enqueue_root(failed_key, "panic").unwrap();
        let (_, completed) = handle.enqueue_root(neighbor, "complete").unwrap();

        assert!(matches!(
            failed.recv().unwrap(),
            CompletionOutcome::Failed(failure)
                if failure.message() == "turn executor panicked"
        ));
        assert_eq!(completed.recv().unwrap(), CompletionOutcome::Completed);
        pool.shutdown();
    }
}
