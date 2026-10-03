//! Product ownership and foreground routing for the pooled extension host.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use gpui::{App, AppContext, Context, Entity, Global, Subscription, Task};

use crate::host::{
    lifecycle::ExtensionKey,
    module_graph::ModuleGraph,
    pool::{
        ExtensionConfig, ExtensionEvent, ExtensionEventInbox, ExtensionExecution, ExtensionPool,
    },
    protocol::{
        Command, CommandInvocationId, ExtensionId, ExtensionLifecycleId, HostOperation, HostRequest,
    },
    scheduler::PoolConfig,
};

use super::{
    extension_buffers::ExtensionBufferBridge,
    extension_commands::{ExtensionCommandBridge, ExtensionCommandEvent},
    extension_load::{DependencyPlan, LoadEntry, LoadReport, LoadResult, StartupAttempt},
    extension_package::{self, InstalledPackage},
    extension_semantics::ExtensionSemanticBridge,
    product::ProductShell,
    product_commands::{ApplicationProductCommands, ProductCommandTarget},
    tree_view::{TreeView, TreeViewEvent},
};

const PRIMARY_FIXTURE: &str = r#"
import { commands, editor, workbench } from "knot:editor";

globalThis.knotFixtureBuffer = await editor.activeBuffer();
await commands.register("knot.fixture.primary", async ({ buffer }) => {
  const snapshot = await buffer.snapshot();
  await buffer.applyEdits([{
    range: { startByteOffset: 0, endByteOffset: 0 },
    text: "fixture ",
  }], { ifRevision: snapshot.revision });
});
await editor.registerCompletionProvider("fixture-primary", {
  provideCompletions(context) {
    return [{ label: `${context.prefix}Primary`, insertText: `${context.prefix}Primary` }];
  },
});
await workbench.registerTreeDataProvider("outline", {
  getChildren(parentId) {
    return parentId === null
      ? [{ id: "fixture", label: "Pooled fixture", collapsibleState: "none" }]
      : [];
  },
});
"#;

const SECONDARY_FIXTURE: &str = r#"
import { commands, editor } from "knot:editor";

globalThis.knotFixtureBuffer = await editor.activeBuffer();
await commands.register("knot.fixture.secondary", async () => ({ status: "handled" }));
await editor.registerCompletionProvider("fixture-secondary", {
  provideCompletions(context) {
    return [{ label: `${context.prefix}Secondary`, insertText: `${context.prefix}Secondary` }];
  },
});
"#;

fn graph_for_package(package: &InstalledPackage) -> Result<ModuleGraph, String> {
    let root = url::Url::from_directory_path(&package.directory)
        .map_err(|_| format!("invalid package directory: {}", package.directory.display()))?;
    ModuleGraph::new(
        root.as_str(),
        &package.manifest.main,
        package.sources.clone(),
    )
}

pub(crate) struct ApplicationExtensionHost {
    _host: Entity<ProductExtensionHost>,
}

impl Global for ApplicationExtensionHost {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StartupState {
    Scanning,
    Loading,
    Complete,
}

#[derive(Clone)]
pub(crate) struct StartupReportSnapshot {
    pub(crate) root: PathBuf,
    pub(crate) state: StartupState,
    pub(crate) report: LoadReport,
}

pub(crate) fn entity(cx: &App) -> Option<Entity<ProductExtensionHost>> {
    cx.try_global::<ApplicationExtensionHost>()
        .map(|global| global._host.clone())
}

pub(crate) fn startup_report(cx: &App) -> Option<StartupReportSnapshot> {
    let host = entity(cx)?;
    host.read(cx).startup_report.clone()
}

#[cfg(test)]
pub(crate) fn completion_provider_count(cx: &App) -> usize {
    entity(cx)
        .map(|host| host.read(cx).semantics.completion_snapshot().len())
        .unwrap_or_default()
}

pub(crate) fn install(cx: &mut App) -> Entity<ProductExtensionHost> {
    let (pool, inbox) = ExtensionPool::new(PoolConfig::default());
    let host = cx.new(|cx| ProductExtensionHost::new(pool, inbox, cx));
    cx.set_global(ApplicationExtensionHost {
        _host: host.clone(),
    });
    host
}

pub(crate) fn dispatch_product_command(
    command: Command,
    target: ProductCommandTarget,
    cx: &mut App,
) -> super::CommandExecution {
    let host = cx.global::<ApplicationExtensionHost>()._host.clone();
    host.update(cx, |host, cx| {
        host.dispatch_product_command(command, target, cx)
    })
}

pub(crate) fn start_completion(
    target: ProductCommandTarget,
    cx: &mut App,
) -> crate::host::protocol::CommandOutcome {
    let host = cx.global::<ApplicationExtensionHost>()._host.clone();
    host.update(cx, |host, cx| host.start_completion(target, cx))
}

/// Owns the one process-wide extension pool and all foreground protocol state.
pub(crate) struct ProductExtensionHost {
    pool: Arc<ExtensionPool>,
    lifecycles: HashSet<ExtensionKey>,
    buffers: ExtensionBufferBridge,
    buffer_observers: HashMap<crate::host::protocol::BufferHandle, Subscription>,
    commands: ExtensionCommandBridge,
    command_targets: HashMap<CommandInvocationId, ProductCommandTarget>,
    semantics: ExtensionSemanticBridge,
    _fixture_tree: Entity<TreeView>,
    _fixture_tree_subscription: Subscription,
    _event_task: Task<()>,
    startup_report: Option<StartupReportSnapshot>,
    _startup_task: Option<Task<()>>,
    next_extension_id: Arc<AtomicU64>,
}

impl ProductExtensionHost {
    fn new(
        pool: Arc<ExtensionPool>,
        mut inbox: ExtensionEventInbox,
        cx: &mut Context<Self>,
    ) -> Self {
        let product_commands = cx.global::<ApplicationProductCommands>().0.clone();
        let definitions = product_commands
            .read(cx)
            .definitions()
            .map(|definition| (definition.name.clone(), definition.title.clone()))
            .collect::<Vec<_>>();
        let mut commands = ExtensionCommandBridge::new();
        for (name, title) in definitions {
            commands
                .register_native(name, title)
                .expect("product command names are unique in the extension catalog");
        }
        let fixture_tree = cx.new(|cx| TreeView::new("outline", cx));
        let mut semantics = ExtensionSemanticBridge::new();
        semantics.add_tree_view("outline", fixture_tree.clone());
        let fixture_tree_subscription = cx.subscribe(&fixture_tree, |this, _, event, cx| {
            this.handle_tree_event(event, cx)
        });
        let event_task = cx.spawn(async move |this, cx| {
            while let Some(event) = inbox.receive().await {
                if this
                    .update(cx, |this, cx| this.handle_event(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            pool,
            lifecycles: HashSet::new(),
            buffers: ExtensionBufferBridge::new(),
            buffer_observers: HashMap::new(),
            commands,
            command_targets: HashMap::new(),
            semantics,
            _fixture_tree: fixture_tree,
            _fixture_tree_subscription: fixture_tree_subscription,
            _event_task: event_task,
            startup_report: None,
            _startup_task: None,
            next_extension_id: Arc::new(AtomicU64::new(1)),
        }
    }

    pub(crate) fn start_installed_extensions(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if self
            .startup_report
            .as_ref()
            .is_some_and(|snapshot| snapshot.state != StartupState::Complete)
        {
            return;
        }
        self._startup_task.take();
        let previous = self.lifecycles.iter().copied().collect::<Vec<_>>();
        for key in previous {
            let _ = self.pool.unload(key);
            self.remove_lifecycle(key, cx);
        }
        self.startup_report = Some(StartupReportSnapshot {
            root: root.clone(),
            state: StartupState::Scanning,
            report: LoadReport::default(),
        });
        cx.notify();
        let discovery = cx
            .background_executor()
            .spawn(async move { extension_package::discover(&root) });
        let pool = self.pool.clone();
        let next_extension_id = self.next_extension_id.clone();
        self._startup_task = Some(cx.spawn(async move |this, cx| {
            let discovered = discovery.await;
            let _ = this.update(cx, |host, cx| {
                if let Some(snapshot) = &mut host.startup_report {
                    snapshot.state = StartupState::Loading;
                }
                cx.notify();
            });
            let report_host = this.clone();
            let mut report_cx = cx.clone();
            let report = DependencyPlan::new(discovered)
                .execute_with_progress(
                    |package| {
                        let id = next_extension_id.fetch_add(1, Ordering::Relaxed);
                        assert_ne!(id, u64::MAX, "extension identity exhausted");
                        let key =
                            ExtensionKey::new(ExtensionId::new(id), ExtensionLifecycleId::new(1));
                        let graph = graph_for_package(package);
                        let execution = graph.and_then(|graph| {
                            this.update(cx, |host, _| host.load_package(key, graph))
                                .map_err(|error| error.to_string())?
                        });
                        let rollback_pool = pool.clone();
                        let rollback_host = this.clone();
                        let mut rollback_cx = cx.clone();
                        StartupAttempt::new(
                            async move {
                                execution?
                                    .await
                                    .map(|_| ())
                                    .map_err(|error| error.to_string())
                            },
                            move || {
                                let _ = rollback_pool.unload(key);
                                let _ = rollback_host.update(&mut rollback_cx, |host, cx| {
                                    host.remove_lifecycle(key, cx);
                                });
                            },
                        )
                    },
                    |entry| {
                        let name = entry.name.as_deref().unwrap_or("<invalid package>");
                        match &entry.result {
                            LoadResult::Loaded => eprintln!("[knot] extension loaded: {name}"),
                            LoadResult::Failed(cause) => eprintln!(
                                "[knot] extension failed: {name} ({}): {cause}",
                                entry.directory.display()
                            ),
                        }
                        let _ = report_host.update(&mut report_cx, |host, cx| {
                            if let Some(snapshot) = &mut host.startup_report {
                                snapshot.report.entries.push(entry.clone());
                            }
                            cx.notify();
                        });
                    },
                )
                .await;
            let _ = this.update(cx, |host, cx| {
                if let Some(snapshot) = &mut host.startup_report {
                    snapshot.state = StartupState::Complete;
                    snapshot.report = report;
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn report_extensions_root_error(&mut self, cause: String, cx: &mut Context<Self>) {
        self.startup_report = Some(StartupReportSnapshot {
            root: PathBuf::from("<user extensions>"),
            state: StartupState::Complete,
            report: LoadReport {
                entries: vec![LoadEntry {
                    directory: PathBuf::from("<user extensions>"),
                    name: None,
                    result: LoadResult::Failed(cause.clone()),
                }],
            },
        });
        eprintln!("[knot] cannot locate installed extensions: {cause}");
        cx.notify();
    }

    fn load_package(
        &mut self,
        key: ExtensionKey,
        graph: ModuleGraph,
    ) -> Result<ExtensionExecution, String> {
        let execution = self
            .pool
            .load_package(key, ExtensionConfig::default(), graph)
            .map_err(|error| error.to_string())?;
        self.admit_lifecycle(key);
        Ok(execution)
    }

    fn admit_lifecycle(&mut self, key: ExtensionKey) {
        if self.lifecycles.insert(key) {
            self.buffers.admit_lifecycle(key.extension, key.lifecycle);
            self.commands.admit_lifecycle(key.extension, key.lifecycle);
            self.semantics.admit_lifecycle(key.extension, key.lifecycle);
        }
    }

    pub(crate) fn start_diagnostic_fixture(&mut self, name: &str, cx: &mut Context<Self>) {
        for (index, source) in [PRIMARY_FIXTURE, SECONDARY_FIXTURE].into_iter().enumerate() {
            let id = self.next_extension_id.fetch_add(1, Ordering::Relaxed);
            assert_ne!(id, u64::MAX, "extension identity exhausted");
            let key = ExtensionKey::new(
                crate::host::protocol::ExtensionId::new(id),
                crate::host::protocol::ExtensionLifecycleId::new(1),
            );
            let graph = ModuleGraph::new(
                &format!("file:///fixtures/product-{index}/"),
                "main.js",
                BTreeMap::from([("main.js".into(), Arc::from(source))]),
            )
            .expect("diagnostic fixture graph is valid");
            let execution = self.load_package(key, graph);
            match execution {
                Ok(execution) => {
                    let name = name.to_owned();
                    let pool = self.pool.clone();
                    cx.spawn(async move |this, cx| {
                        if let Err(error) = execution.await {
                            let _ = pool.unload(key);
                            let _ = this.update(cx, |host, cx| host.remove_lifecycle(key, cx));
                            eprintln!("[knot] fixture {name:?} extension {index} failed: {error}");
                        }
                    })
                    .detach();
                }
                Err(error) => {
                    eprintln!("[knot] cannot start fixture {name:?} extension {index}: {error}");
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn load(
        &mut self,
        key: ExtensionKey,
        config: ExtensionConfig,
    ) -> Result<(), crate::host::pool::ExtensionPoolError> {
        self.pool.load(key, config)?;
        self.admit_lifecycle(key);
        Ok(())
    }

    #[allow(dead_code, reason = "explicit unload is used by diagnostic clients")]
    pub(crate) fn unload(
        &mut self,
        key: ExtensionKey,
        cx: &mut Context<Self>,
    ) -> Result<(), crate::host::pool::ExtensionPoolError> {
        self.pool.unload(key)?;
        self.remove_lifecycle(key, cx);
        Ok(())
    }

    fn handle_event(&mut self, event: ExtensionEvent, cx: &mut Context<Self>) {
        match event {
            ExtensionEvent::Request(request) => self.handle_request(request, cx),
            ExtensionEvent::LifecycleEnded { key, reason } => {
                let _ = reason;
                self.remove_lifecycle(key, cx);
            }
        }
        cx.notify();
    }

    fn handle_request(&mut self, request: HostRequest, cx: &mut Context<Self>) {
        let active_buffer = self.sync_active_buffer(cx).map(|(buffer, _)| buffer);
        if active_buffer.is_none() {
            self.buffers.set_active_buffer(None);
        }
        let command_operation = request.operation.clone();
        let response = match request_route(&request.operation) {
            RequestRoute::Buffer => {
                let invocation = request.invocation;
                let commands = &self.commands;
                Some(
                    self.buffers
                        .dispatch(request, || commands.is_cancelled(invocation), cx),
                )
            }
            RequestRoute::Command => self.commands.handle_host_request(request, active_buffer),
            RequestRoute::Semantic => Some(self.semantics.dispatch(request, cx)),
        };
        if let Some(response) = response {
            self.sync_product_command_catalog(&command_operation, &response, cx);
            let _ = self.pool.respond(response);
        }
        self.drain_command_events(cx);
    }

    fn sync_product_command_catalog(
        &self,
        operation: &HostOperation,
        response: &crate::host::protocol::HostResponse,
        cx: &mut Context<Self>,
    ) {
        let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
        match (operation, &response.result) {
            (
                HostOperation::RegisterCommand { name, title },
                Ok(crate::host::protocol::HostResponseValue::CommandRegistered { registration }),
            ) => {
                let mirrored = dispatcher.update(cx, |dispatcher, _| {
                    dispatcher.register_extension(
                        name.clone(),
                        title.clone(),
                        response.extension,
                        response.lifecycle,
                    )
                });
                assert_eq!(mirrored, Ok(*registration), "command catalogs diverged");
            }
            (
                HostOperation::UnregisterCommand { registration },
                Ok(crate::host::protocol::HostResponseValue::CommandUnregistered { .. }),
            ) => {
                let mirrored = dispatcher.update(cx, |dispatcher, _| {
                    dispatcher.unregister_extension(
                        *registration,
                        response.extension,
                        response.lifecycle,
                    )
                });
                assert_eq!(mirrored, Ok(()), "command catalogs diverged");
            }
            _ => {}
        }
    }

    fn drain_command_events(&mut self, cx: &mut Context<Self>) {
        loop {
            let events = self.commands.drain_events().collect::<Vec<_>>();
            if events.is_empty() {
                break;
            }
            for event in events {
                match event {
                    ExtensionCommandEvent::HostResponse(response) => {
                        let _ = self.pool.respond(response);
                    }
                    ExtensionCommandEvent::CancelExtension {
                        extension,
                        lifecycle,
                        invocation,
                    } => {
                        let _ = self
                            .pool
                            .cancel_command(ExtensionKey::new(extension, lifecycle), invocation);
                    }
                    ExtensionCommandEvent::DispatchNative {
                        root,
                        invocation,
                        command,
                        ..
                    } => {
                        let Some(target) = self.command_targets.get(&root).cloned() else {
                            self.commands.complete(
                                invocation,
                                crate::host::protocol::CommandOutcome::InvalidTarget,
                            );
                            continue;
                        };
                        let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
                        let execution = dispatcher.update(cx, |dispatcher, cx| {
                            dispatcher.dispatch(command, target, cx)
                        });
                        cx.spawn(async move |this, cx| {
                            let outcome = execution
                                .completion
                                .await
                                .unwrap_or(crate::host::protocol::CommandOutcome::Cancelled);
                            let _ = this.update(cx, |this, cx| {
                                this.commands.complete(invocation, outcome);
                                if invocation == root {
                                    this.command_targets.remove(&root);
                                }
                                this.drain_command_events(cx);
                            });
                        })
                        .detach();
                    }
                    ExtensionCommandEvent::DispatchExtension {
                        root,
                        invocation,
                        buffer,
                    } => {
                        let id = invocation.id;
                        let key = ExtensionKey::new(invocation.extension, invocation.lifecycle);
                        let Ok(execution) = self.pool.invoke_command(key, invocation, buffer)
                        else {
                            self.commands
                                .complete(id, crate::host::protocol::CommandOutcome::Unavailable);
                            continue;
                        };
                        cx.spawn(async move |this, cx| {
                            let outcome = execution
                                .await
                                .unwrap_or(crate::host::protocol::CommandOutcome::Cancelled);
                            let _ = this.update(cx, |this, cx| {
                                this.commands.complete(id, outcome);
                                if id == root {
                                    this.command_targets.remove(&root);
                                }
                                this.drain_command_events(cx);
                            });
                        })
                        .detach();
                    }
                }
            }
        }
    }

    fn handle_tree_event(&mut self, event: &TreeViewEvent, cx: &mut Context<Self>) {
        match event {
            TreeViewEvent::RequestChildren { provider, request } => {
                let key = ExtensionKey::new(provider.extension, provider.lifecycle);
                let fallback = request.clone();
                match self.pool.request_tree_children(key, request.clone()) {
                    Ok(execution) => {
                        cx.spawn(async move |this, cx| {
                            let response = execution.await.unwrap_or_else(|error| {
                                crate::host::protocol::TreeChildrenResponse {
                                    registration: fallback.registration,
                                    parent_id: fallback.parent_id,
                                    generation: fallback.generation,
                                    result: Err(crate::host::protocol::TreeProviderError {
                                        message: error.to_string(),
                                    }),
                                }
                            });
                            let _ = this.update(cx, |this, cx| {
                                this.semantics.apply_tree_response(response, cx);
                            });
                        })
                        .detach();
                    }
                    Err(error) => {
                        self.semantics.apply_tree_response(
                            crate::host::protocol::TreeChildrenResponse {
                                registration: fallback.registration,
                                parent_id: fallback.parent_id,
                                generation: fallback.generation,
                                result: Err(crate::host::protocol::TreeProviderError {
                                    message: error.to_string(),
                                }),
                            },
                            cx,
                        );
                    }
                }
            }
            TreeViewEvent::InvokeCommand {
                provider,
                command,
                window,
                focus,
            } => {
                if !self
                    .lifecycles
                    .contains(&ExtensionKey::new(provider.extension, provider.lifecycle))
                    || focus.upgrade().is_none()
                {
                    return;
                }
                let Some(target) = capture_product_target(*window, cx) else {
                    return;
                };
                self.dispatch_product_command(
                    Command {
                        name: command.clone().into(),
                        arguments: crate::host::protocol::CommandArgumentValue::Null,
                    },
                    target,
                    cx,
                );
            }
        }
    }

    fn sync_active_buffer(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<(crate::host::protocol::BufferHandle, ProductCommandTarget)> {
        let target = capture_active_product_target(cx)?;
        let buffer = self.open_target_buffer(&target, cx)?;
        self.buffers.set_active_buffer(Some(buffer));
        Some((buffer, target))
    }

    pub(crate) fn dispatch_product_command(
        &mut self,
        command: Command,
        target: ProductCommandTarget,
        cx: &mut Context<Self>,
    ) -> super::CommandExecution {
        let buffer = self.open_target_buffer(&target, cx);
        let execution = self.commands.enqueue_root(command, buffer);
        self.command_targets.insert(execution.id, target);
        self.drain_command_events(cx);
        execution
    }

    fn start_completion(
        &mut self,
        target: ProductCommandTarget,
        cx: &mut Context<Self>,
    ) -> crate::host::protocol::CommandOutcome {
        if let Err(outcome) = target.document_id(cx) {
            return outcome;
        }
        let Some(buffer) = self.open_target_buffer(&target, cx) else {
            return crate::host::protocol::CommandOutcome::InvalidTarget;
        };
        let Some(workbench) = target.workbench.upgrade() else {
            return crate::host::protocol::CommandOutcome::InvalidTarget;
        };
        let Some(editor) = workbench
            .read(cx)
            .pane(target.pane)
            .and_then(|pane| {
                pane.tabs()
                    .iter()
                    .find(|tab| tab.id() == target.tab && tab.surface_id() == target.surface)
            })
            .and_then(|tab| tab.editor().cloned())
        else {
            return crate::host::protocol::CommandOutcome::InvalidTarget;
        };
        let requests = editor.update(cx, |editor, cx| {
            editor.start_completion(buffer, self.semantics.completion_snapshot(), cx)
        });
        for (provider, request) in requests {
            let registration = request.registration;
            let generation = request.generation;
            let fallback = request.clone();
            let execution = self.pool.request_completions(
                ExtensionKey::new(provider.extension, provider.lifecycle),
                request,
            );
            let editor_for_task = editor.clone();
            let task = cx.spawn(async move |_, cx| {
                let response = match execution {
                    Ok(execution) => execution.await.unwrap_or_else(|error| {
                        crate::host::protocol::CompletionResponse {
                            registration: fallback.registration,
                            revision: fallback.revision,
                            generation: fallback.generation,
                            result: Err(crate::host::protocol::CompletionProviderError {
                                message: error.to_string(),
                            }),
                        }
                    }),
                    Err(error) => crate::host::protocol::CompletionResponse {
                        registration: fallback.registration,
                        revision: fallback.revision,
                        generation: fallback.generation,
                        result: Err(crate::host::protocol::CompletionProviderError {
                            message: error.to_string(),
                        }),
                    },
                };
                let _ = editor_for_task.update(cx, |editor, cx| {
                    editor.apply_completion_response(buffer, response, cx);
                });
            });
            editor.update(cx, |editor, _| {
                editor.retain_completion_task(generation, registration, task)
            });
        }
        crate::host::protocol::CommandOutcome::Completed
    }

    fn open_target_buffer(
        &mut self,
        target: &ProductCommandTarget,
        cx: &mut Context<Self>,
    ) -> Option<crate::host::protocol::BufferHandle> {
        target.document_id(cx).ok()?;
        let workbench = target.workbench.upgrade()?;
        let model = workbench
            .read(cx)
            .pane(target.pane)?
            .tabs()
            .iter()
            .find(|tab| tab.id() == target.tab && tab.surface_id() == target.surface)?
            .editor()?
            .read(cx)
            .model()
            .clone();
        let buffer = self.buffers.open_buffer(&model);
        self.buffer_observers.entry(buffer).or_insert_with(|| {
            let observer = cx.observe(&model, |this, model, cx| {
                for dispatch in this.buffers.drain_model_changes(&model, cx) {
                    let _ = this.pool.dispatch_buffer_change(
                        ExtensionKey::new(dispatch.extension, dispatch.lifecycle),
                        dispatch.subscription,
                        dispatch.change,
                    );
                }
            });
            observer
        });
        Some(buffer)
    }

    fn remove_lifecycle(&mut self, key: ExtensionKey, cx: &mut Context<Self>) {
        if !self.lifecycles.remove(&key) {
            return;
        }
        self.commands.remove_lifecycle(key.extension, key.lifecycle);
        let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
        dispatcher.update(cx, |dispatcher, _| {
            dispatcher.remove_extension_lifecycle(key.extension, key.lifecycle)
        });
        self.command_targets
            .retain(|root, _| self.commands.contains_invocation(*root));
        self.buffers
            .remove_lifecycle(key.extension, key.lifecycle, cx);
        self.semantics
            .remove_lifecycle(key.extension, key.lifecycle, cx);
        self.drain_command_events(cx);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequestRoute {
    Buffer,
    Command,
    Semantic,
}

fn request_route(operation: &HostOperation) -> RequestRoute {
    match operation {
        HostOperation::ActiveBuffer
        | HostOperation::Snapshot { .. }
        | HostOperation::ApplyEdits { .. }
        | HostOperation::SubscribeBufferChanges { .. }
        | HostOperation::UnsubscribeBufferChanges { .. }
        | HostOperation::ReplaceEditorContributions { .. }
        | HostOperation::DisposeEditorContributions { .. } => RequestRoute::Buffer,
        HostOperation::RegisterCommand { .. }
        | HostOperation::UnregisterCommand { .. }
        | HostOperation::InvokeCommand { .. }
        | HostOperation::CompleteInlineCommand { .. } => RequestRoute::Command,
        HostOperation::RegisterTreeProvider { .. }
        | HostOperation::InvalidateTreeProvider { .. }
        | HostOperation::UnregisterTreeProvider { .. }
        | HostOperation::RegisterCompletionProvider { .. }
        | HostOperation::UnregisterCompletionProvider { .. } => RequestRoute::Semantic,
    }
}

fn capture_active_product_target(cx: &mut App) -> Option<ProductCommandTarget> {
    let windows = cx.window_stack().unwrap_or_else(|| cx.windows());
    for window_handle in windows {
        let target = capture_product_target(window_handle, cx);
        if target.is_some() {
            return target;
        }
    }
    None
}

fn capture_product_target(
    window_handle: gpui::AnyWindowHandle,
    cx: &mut App,
) -> Option<ProductCommandTarget> {
    cx.update_window(window_handle, |_, window, cx| {
        let shell = window.root::<ProductShell>().flatten()?;
        shell.update(cx, |shell, cx| shell.capture_command_target(window, cx))
    })
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::protocol::{Command, CommandArgumentValue};
    use gpui::TestAppContext;

    #[test]
    fn every_host_operation_has_one_foreground_owner() {
        assert_eq!(
            request_route(&HostOperation::ActiveBuffer),
            RequestRoute::Buffer
        );
        assert_eq!(
            request_route(&HostOperation::InvokeCommand {
                command: Command {
                    name: "test.command".into(),
                    arguments: CommandArgumentValue::Null,
                },
            }),
            RequestRoute::Command
        );
        assert_eq!(
            request_route(&HostOperation::RegisterCompletionProvider {
                label: "test".into(),
            }),
            RequestRoute::Semantic
        );
    }

    #[gpui::test]
    fn product_owns_one_bounded_pool(cx: &mut TestAppContext) {
        let pool = cx.update(|cx| {
            let commands = cx.new(super::super::product_commands::ProductCommandDispatcher::new);
            cx.set_global(ApplicationProductCommands(commands));
            install(cx);

            let host = cx.global::<ApplicationExtensionHost>()._host.clone();
            let diagnostics = host.read(cx).pool.diagnostics();
            assert!(diagnostics.worker_count > 0);
            assert!(diagnostics.extensions.is_empty());
            let pool = Arc::downgrade(&host.read(cx).pool);

            drop(host);
            drop(cx.remove_global::<ApplicationExtensionHost>());
            pool
        });
        cx.run_until_parked();
        assert!(pool.upgrade().is_none());
    }

    #[gpui::test]
    fn product_host_owns_reload_identity_and_foreground_teardown(cx: &mut TestAppContext) {
        use crate::host::protocol::{ExtensionId, ExtensionLifecycleId};

        cx.update(|cx| {
            let commands = cx.new(super::super::product_commands::ProductCommandDispatcher::new);
            cx.set_global(ApplicationProductCommands(commands));
            install(cx);

            let host = cx.global::<ApplicationExtensionHost>()._host.clone();
            let first = ExtensionKey::new(ExtensionId::new(7), ExtensionLifecycleId::new(1));
            let neighbor = ExtensionKey::new(ExtensionId::new(8), ExtensionLifecycleId::new(1));
            host.update(cx, |host, _| {
                host.load(first, ExtensionConfig::default()).unwrap();
                host.load(neighbor, ExtensionConfig::default()).unwrap();
                assert_eq!(host.lifecycles.len(), 2);
                assert_eq!(
                    host.pool.diagnostics().worker_count,
                    PoolConfig::default().workers().get()
                );
            });

            host.update(cx, |host, cx| {
                host.unload(first, cx).unwrap();
                let replacement = ExtensionKey::new(first.extension, ExtensionLifecycleId::new(2));
                host.load(replacement, ExtensionConfig::default()).unwrap();
                assert!(!host.lifecycles.contains(&first));
                assert!(host.lifecycles.contains(&replacement));
                assert!(host.lifecycles.contains(&neighbor));
            });

            drop(host);
            drop(cx.remove_global::<ApplicationExtensionHost>());
        });
    }

    #[gpui::test]
    fn diagnostic_client_loads_multiple_extensions_through_product_transport(
        cx: &mut TestAppContext,
    ) {
        let host = cx.update(|cx| {
            let commands = cx.new(super::super::product_commands::ProductCommandDispatcher::new);
            cx.set_global(ApplicationProductCommands(commands));
            let host = install(cx);
            host.update(cx, |host, cx| {
                host.start_diagnostic_fixture("integration", cx)
            });
            host
        });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            cx.run_until_parked();
            if cx.update(|cx| {
                let host = host.read(cx);
                host.semantics.completion_snapshot().len() == 2
                    && host._fixture_tree.read(cx).lifecycle_state() == (true, 1, false, 0)
            }) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "fixture registrations did not cross the product transport"
            );
            std::thread::yield_now();
        }
        cx.update(|cx| {
            let host = host.read(cx);
            assert_eq!(host.lifecycles.len(), 2);
            assert_eq!(host.semantics.completion_snapshot().len(), 2);
            assert_eq!(
                host._fixture_tree.read(cx).root_labels(),
                ["Pooled fixture"]
            );
            let definitions = host
                .commands
                .definitions()
                .map(|definition| definition.name.as_ref())
                .collect::<Vec<_>>();
            assert!(definitions.contains(&"knot.fixture.primary"));
            assert!(definitions.contains(&"knot.fixture.secondary"));
        });
        cx.update(|cx| {
            let first = ExtensionKey::new(
                crate::host::protocol::ExtensionId::new(1),
                crate::host::protocol::ExtensionLifecycleId::new(1),
            );
            let second = ExtensionKey::new(
                crate::host::protocol::ExtensionId::new(2),
                crate::host::protocol::ExtensionLifecycleId::new(1),
            );
            host.update(cx, |host, cx| {
                host.unload(first, cx).unwrap();
                host.unload(second, cx).unwrap();
                assert!(host.semantics.completion_snapshot().is_empty());
                assert_eq!(
                    host._fixture_tree.read(cx).lifecycle_state(),
                    (false, 0, false, 0)
                );
                assert!(
                    !host.commands.definitions().any(|definition| {
                        definition.name.as_ref().starts_with("knot.fixture.")
                    })
                );
            });
            drop(cx.remove_global::<ApplicationExtensionHost>());
        });
        drop(host);
        cx.run_until_parked();
    }
}
