//! Persistent isolate capsules and the turns that run inside them.

use super::*;

/// One persistent, movable V8 isolate and context for an extension lifetime.
///
/// All state attached to the shared isolate is `Send`; this is the safety
/// condition required by `v8::Isolate::try_into_shared`.
pub(crate) struct RuntimeCapsule {
    key: ExtensionKey,
    pub(super) data: Mutex<Option<IsolateData>>,
    pub(super) local_state: Arc<RuntimeLocalState>,
    pub(super) heap_state: Box<HeapLimitState>,
    pub(super) status: Arc<AtomicU8>,
}

impl RuntimeCapsule {
    #[cfg(test)]
    pub(crate) fn new(key: ExtensionKey, config: IsolateConfig) -> Result<Self, RuntimeError> {
        Self::new_with_graph(key, config, None)
    }

    pub(super) fn new_with_graph(
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

    #[cfg(test)]
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

    #[cfg(test)]
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

    #[cfg(test)]
    fn start_script_turn(
        &self,
        root: RootId,
        source_name: Arc<str>,
        source: Arc<str>,
        result: ResultSender<ExecutionReport>,
    ) -> TurnOutcome {
        self.run_root_turn(root, source_name, result, RootEvaluation::Script { source })
    }

    #[cfg(test)]
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
            #[cfg(test)]
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
                    Ok(Ok(crate::host::declarations::EncodedValue::Undefined)) => None,
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
        let operation = crate::host::declarations::registry()
            .get("mutateKeybindings")
            .expect("native keymap handler")
            .request(crate::host::protocol::KeybindingArguments { mutations });
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

    #[cfg(test)]
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

    pub(super) fn dispose_with_error(&self, error: RuntimeError) {
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
    pub(super) fn install_test_barrier(&self, barrier: Arc<TestGate>) {
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
    pub(super) fn install_test_entered_signal(&self, entered: mpsc::Sender<()>) {
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
    pub(super) fn set_test_host_error(&self, error: HostFailure) {
        *self.local_state.host_error.lock().unwrap() = Some(error);
    }
}

impl Drop for RuntimeCapsule {
    fn drop(&mut self) {
        self.dispose();
    }
}

pub(super) fn fatal_or_completed(error: &RuntimeError) -> TurnOutcome {
    if error.is_fatal() {
        TurnOutcome::Fatal(Failure::new(error.to_string()))
    } else {
        TurnOutcome::Completed
    }
}

pub(super) fn execute_engine_turn(
    turn: Turn<EngineTurn>,
    finalizer: &LifecycleFinalizer,
) -> TurnOutcome {
    let key = turn.key;
    let root = turn.root;
    let work = turn.work;
    let capsule = Arc::clone(&work.capsule);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match work.work {
        #[cfg(test)]
        EngineWork::Script {
            source_name,
            source,
            result,
        } => capsule.start_script_turn(root, source_name, source, result),
        #[cfg(test)]
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

pub(super) extern "C" fn near_heap_limit_callback(
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

pub(super) extern "C" fn promise_reject_callback(message: v8::PromiseRejectMessage) {
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

pub(super) fn report_from_value(
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

pub(super) fn runtime_error_from_try_catch(
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

pub(super) fn one_based_coordinate(value: usize) -> Option<u32> {
    u32::try_from(value).ok()?.checked_add(1)
}

pub(super) unsafe extern "C" fn drop_external_utf16(buffer: *mut u16, length: usize) {
    let slice = ptr::slice_from_raw_parts(buffer.cast_const(), length);
    // SAFETY: this exactly rebuilds the Arc slice converted in
    // `hold_external_utf16`, consuming its one transferred strong reference.
    drop(unsafe { Arc::<[u16]>::from_raw(slice) });
}

#[cfg(test)]
pub(super) fn test_barrier_callback(
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
pub(super) fn test_entered_callback(
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
