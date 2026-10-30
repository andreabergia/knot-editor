//! The runtime pool: capsule lifetimes, loading, finalization, and engine events.

use super::*;

/// Engine-owned composition of persistent capsules and the bounded scheduler.
pub(crate) struct RuntimePool {
    scheduler: Option<SchedulerPool<EngineTurn>>,
    pub(super) scheduler_handle: SchedulerHandle<EngineTurn>,
    finalizer: LifecycleFinalizer,
    events: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<EngineEvent>>>,
}

#[derive(Clone)]
pub(super) struct LifecycleFinalizer {
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

    pub(super) fn finalize(
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

    #[cfg(test)]
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

    #[cfg(test)]
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

    #[cfg(test)]
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
    pub(super) fn receive_request_timeout(
        &self,
        timeout: std::time::Duration,
    ) -> Option<HostRequest> {
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

    pub(crate) fn dispatch_setting_change(
        &self,
        key: ExtensionKey,
        subscription: SettingsSubscriptionId,
        setting: String,
    ) -> Result<(), RuntimePoolError> {
        let capsule = self.capsule(key)?;
        self.scheduler_handle.enqueue_root(
            key,
            EngineTurn {
                capsule,
                work: EngineWork::SettingChange {
                    subscription,
                    key: setting,
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

    #[cfg(test)]
    pub(crate) fn buffer_change_queue_metrics(
        &self,
        key: ExtensionKey,
    ) -> Result<BufferChangeQueueMetrics, RuntimePoolError> {
        Ok(self.capsule(key)?.local_state.buffer_change_queue_metrics())
    }

    #[cfg(test)]
    pub(crate) fn diagnostics(&self) -> crate::host::scheduler::SchedulerDiagnostics {
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

    #[cfg(test)]
    pub(crate) fn shutdown(mut self) {
        self.shutdown_inner();
    }

    pub(super) fn capsule(
        &self,
        key: ExtensionKey,
    ) -> Result<Arc<RuntimeCapsule>, RuntimePoolError> {
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

pub(super) fn initialize_extension_context(
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
    let platform_name = v8::String::new(scope, "platform").ok_or_else(RuntimeError::disposed)?;
    let platform = v8::String::new(
        scope,
        if cfg!(target_os = "macos") {
            "macos"
        } else if cfg!(windows) {
            "windows"
        } else {
            "linux"
        },
    )
    .ok_or_else(RuntimeError::disposed)?;
    let listener_failure_name = v8::String::new(scope, "reportCancellationListenerFailure")
        .ok_or_else(RuntimeError::disposed)?;
    let listener_failure = v8::Function::new(scope, cancellation_listener_failure_callback)
        .ok_or_else(RuntimeError::disposed)?;
    if bindings.set(scope, request_name.into(), request.into()) != Some(true)
        || bindings.set(scope, keybinding_name.into(), keybinding.into()) != Some(true)
        || bindings.set(scope, platform_name.into(), platform.into()) != Some(true)
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
        (
            RuntimeCallback::SettingChange,
            "__knotDispatchSettingChange",
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
