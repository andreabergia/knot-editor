//! Futures and handles for work submitted to a capsule: executions, provider
//! calls, cancellation, and termination.

use super::*;

/// Thread-safe, V8-free control surface for forced termination.
pub(crate) struct TerminationHandle {
    pub(super) isolate: v8::IsolateHandle,
    pub(super) status: Arc<AtomicU8>,
    pub(super) wake: Option<TerminationWake>,
}

pub(super) struct TerminationWake {
    pub(super) key: ExtensionKey,
    pub(super) capsule: Arc<RuntimeCapsule>,
    pub(super) scheduler: SchedulerHandle<EngineTurn>,
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

pub(super) struct EngineTurn {
    pub(super) capsule: Arc<RuntimeCapsule>,
    pub(super) work: EngineWork,
}

pub(super) enum ListenerNotification {
    Buffer(BufferChange),
    Application(ApplicationEvent),
    /// The key of a setting written or reset in any scope.
    Setting(String),
}

pub(super) enum EngineWork {
    ApplicationChange {
        subscription: WorkbenchSubscriptionId,
        event: ApplicationEvent,
    },
    SettingChange {
        subscription: SettingsSubscriptionId,
        key: String,
    },
    #[cfg(test)]
    Script {
        source_name: Arc<str>,
        source: Arc<str>,
        result: ResultSender<ExecutionReport>,
    },
    #[cfg(test)]
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
    pub(super) result: ResultReceiver<ExecutionReport>,
    pub(super) completion: CompletionReceiver,
}

impl RuntimeExecution {
    #[cfg(test)]
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
    pub(super) result: ResultReceiver<CommandOutcome>,
    pub(super) completion: CompletionReceiver,
}

impl RuntimeCommandExecution {
    #[cfg(test)]
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
    pub(super) result: Option<ResultReceiver<T>>,
    pub(super) completion: CompletionReceiver,
    pub(super) cancellation: Option<ProviderCancellation>,
}

impl<T> Unpin for RuntimeProviderExecution<T> {}

pub(super) struct ProviderCancellation {
    pub(super) provider: ProviderIdentity,
    pub(super) key: ExtensionKey,
    pub(super) root: RootId,
    pub(super) capsule: Arc<RuntimeCapsule>,
    pub(super) scheduler: SchedulerHandle<EngineTurn>,
    pub(super) cancelled: Arc<AtomicBool>,
    pub(super) completed: Arc<AtomicBool>,
}

impl<T> RuntimeProviderExecution<T> {
    #[cfg(test)]
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
