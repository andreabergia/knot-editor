//! Product ownership and foreground routing for the pooled extension host.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

#[cfg(test)]
use std::collections::BTreeMap;

use gpui::{App, AppContext, ClipboardItem, Context, Entity, Global, Subscription, Task};

use crate::host::{
    lifecycle::ExtensionKey,
    module_graph::{ConfigPhase as HostConfigPhase, ModuleGraph},
    pool::{
        ExtensionConfig, ExtensionEvent, ExtensionEventInbox, ExtensionExecution, ExtensionPool,
    },
    protocol::{
        Command, CommandInvocationId, ExtensionId, ExtensionLifecycleId, HostOperation,
        HostRequest, ViewId,
    },
    scheduler::PoolConfig,
};

use super::{
    extension_buffers::ExtensionBufferBridge,
    extension_commands::{ExtensionCommandBridge, ExtensionCommandEvent, RoutedCommandTarget},
    extension_load::{DependencyPlan, LoadEntry, LoadReport, LoadResult, StartupAttempt},
    extension_package::{self, InstalledPackage},
    extension_semantics::ExtensionSemanticBridge,
    model::{CommandTarget, CommandTargetKind},
    personal_config::{self, ConfigDiagnostic, ConfigPhase},
    product::{self, ProductShell},
    product_commands::{
        ApplicationProductCommands, CommandClaim, ProductCommandTarget, dispatch_to_captured_target,
    },
    tree_view::{TreeView, TreeViewEvent},
};

#[cfg(test)]
const PRIMARY_FIXTURE: &str = r#"
import * as knot from "knot"; const { commands, editor, workbench } = knot;

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

#[cfg(test)]
const SECONDARY_FIXTURE: &str = r#"
import * as knot from "knot"; const { commands, editor } = knot;

await commands.register("knot.fixture.secondary", async () => ({ status: "handled" }));
await editor.registerCompletionProvider("fixture-secondary", {
  provideCompletions(context) {
    return [{ label: `${context.prefix}Secondary`, insertText: `${context.prefix}Secondary` }];
  },
});
"#;

fn graph_for_package(package: &InstalledPackage) -> Result<ModuleGraph, String> {
    extension_package::validate_package_api(&package.manifest)?;
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

pub(crate) fn application_changed(cx: &mut App) {
    super::application_observation::changed(cx);
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
    application_changed(cx);
    host
}

pub(crate) fn start_completion(
    target: ProductCommandTarget,
    cx: &mut App,
) -> crate::host::protocol::CommandOutcome {
    let host = cx.global::<ApplicationExtensionHost>()._host.clone();
    host.update(cx, |host, cx| host.start_completion(target, cx))
}

pub(crate) fn attach_tree_view(view: Entity<TreeView>, cx: &mut App) {
    let Some(host) = entity(cx) else { return };
    host.update(cx, |host, cx| {
        let id = view.read(cx).instance_id();
        host.commands
            .catalog()
            .borrow_mut()
            .bind_view("outline", id)
            .expect("outline is a declared view kind");
        host.semantics
            .add_tree_view("outline", view, cx)
            .expect("new outline view accepts its provider");
    });
}

pub(crate) fn detach_tree_view(view: ViewId, cx: &mut App) {
    let Some(host) = entity(cx) else { return };
    host.update(cx, |host, cx| {
        host.semantics.remove_tree_view(view, cx);
        host.commands.catalog().borrow_mut().remove_view(view);
    });
}

pub(crate) fn route_tree_event(event: &TreeViewEvent, cx: &mut App) {
    let Some(host) = entity(cx) else { return };
    host.update(cx, |host, cx| host.handle_tree_event(event, cx));
}

pub(crate) fn start_view_command(
    command: Command,
    target: ProductCommandTarget,
    handler: CommandTarget,
    completion: super::CommandCompletion,
    cx: &mut App,
) -> Result<(), crate::host::protocol::CommandOutcome> {
    target.validate_tab(cx)?;
    target.validate_view(cx)?;
    let host = entity(cx).ok_or(crate::host::protocol::CommandOutcome::InvalidTarget)?;
    host.update(cx, |host, cx| {
        host.start_view_command(command, target, handler, completion, cx)
    })
}

pub(crate) fn start_global_command(
    command: Command,
    target: ProductCommandTarget,
    handler: CommandTarget,
    completion: super::CommandCompletion,
    cx: &mut App,
) -> Result<(), crate::host::protocol::CommandOutcome> {
    let host = entity(cx).ok_or(crate::host::protocol::CommandOutcome::InvalidTarget)?;
    host.update(cx, |host, cx| {
        host.start_global_command(command, target, handler, completion, cx)
    })
}

fn nested_command_response(
    request: &HostRequest,
    outcome: crate::host::protocol::CommandOutcome,
) -> crate::host::protocol::HostResponse {
    use crate::host::protocol::{CommandInvokeDispatch, HostResponse, HostResponseValue};
    HostResponse {
        extension: request.extension,
        lifecycle: request.lifecycle,
        id: request.id,
        result: Ok(HostResponseValue::CommandInvoked {
            dispatch: CommandInvokeDispatch::Outcome { outcome },
        }),
    }
}

fn complete_nested_command(
    host: &Entity<ProductExtensionHost>,
    child: CommandInvocationId,
    outcome: crate::host::protocol::CommandOutcome,
    cx: &mut App,
) {
    host.update(cx, |host, cx| {
        host.commands.complete(child, outcome);
        host.drain_command_events(cx);
    });
}

fn route_nested_command(request: HostRequest, cx: &mut App) {
    use crate::host::protocol::{CommandOutcome, HostOperation};

    let Some(host) = entity(cx) else { return };
    let HostOperation::InvokeCommand { command } = request.operation.clone() else {
        return;
    };
    let parent = request.invocation.expect("nested invocation");
    if let Err(outcome) = host
        .read(cx)
        .commands
        .admit_child(parent, (request.extension, request.lifecycle))
    {
        let _ = host
            .read(cx)
            .pool
            .respond(nested_command_response(&request, outcome));
        return;
    }
    let target = host
        .read(cx)
        .commands
        .root_of(parent)
        .and_then(|root| host.read(cx).command_targets.get(&root).cloned());
    let Some(target) = target else {
        let _ = host
            .read(cx)
            .pool
            .respond(nested_command_response(&request, CommandOutcome::Cancelled));
        return;
    };
    let (completion, receiver) = super::CommandCompletion::new();
    let claim = dispatch_to_captured_target(&command, &target, completion, cx);
    let (response, child) = host.update(cx, |host, cx| {
        let routed = match &claim {
            CommandClaim::Extension { handler, .. } => RoutedCommandTarget::Extension(*handler),
            _ => RoutedCommandTarget::Window,
        };
        let (response, child) = host.commands.handle_routed_child(&request, routed);
        if let (
            CommandClaim::Extension {
                handler,
                view: Some(_),
            },
            Some(child),
        ) = (&claim, child)
        {
            host.view_command_handlers
                .insert(child, (*handler, command.name.clone()));
        }
        host.drain_command_events(cx);
        (response, child)
    });
    if let Some(response) = response {
        let _ = host.read(cx).pool.respond(response);
    }
    let Some(child) = child else { return };
    match claim {
        CommandClaim::Declined => {
            complete_nested_command(&host, child, CommandOutcome::Unavailable, cx)
        }
        CommandClaim::Finished(outcome) => complete_nested_command(&host, child, outcome, cx),
        CommandClaim::Pending(start) => match start(cx) {
            Err(outcome) => complete_nested_command(&host, child, outcome, cx),
            Ok(()) => {
                cx.spawn(async move |cx| {
                    let outcome = receiver.await.unwrap_or(CommandOutcome::Cancelled);
                    let _ = host.update(cx, |host, cx| {
                        host.commands.complete(child, outcome);
                        host.drain_command_events(cx);
                    });
                })
                .detach();
            }
        },
        CommandClaim::Extension { .. } => {}
    }
}

/// Owns the one process-wide extension pool and all foreground protocol state.
pub(crate) struct ProductExtensionHost {
    pool: Arc<ExtensionPool>,
    lifecycles: HashSet<ExtensionKey>,
    buffers: ExtensionBufferBridge,
    application_observation: super::application_observation::ApplicationObservation,
    #[cfg(test)]
    application_events: Vec<crate::host::protocol::ApplicationEvent>,
    _application_window_closed: Subscription,
    _application_signal: Subscription,
    buffer_observers: HashMap<crate::host::protocol::BufferHandle, Subscription>,
    commands: ExtensionCommandBridge,
    completion_editors: Vec<gpui::WeakEntity<super::editor::EditorView>>,
    command_targets: HashMap<CommandInvocationId, ProductCommandTarget>,
    view_command_handlers:
        HashMap<CommandInvocationId, (CommandTarget, crate::host::protocol::CommandName)>,
    #[cfg(test)]
    hold_clipboard_writes: bool,
    #[cfg(test)]
    deferred_view_requests: Vec<HostRequest>,
    semantics: ExtensionSemanticBridge,
    #[cfg(test)]
    _outline_tree: Entity<TreeView>,
    #[cfg(test)]
    _outline_tree_subscription: Subscription,
    _event_task: Task<()>,
    _release_subscription: Subscription,
    startup_report: Option<StartupReportSnapshot>,
    _startup_task: Option<Task<()>>,
    _personal_startup_task: Option<Task<()>>,
    personal_config_key: Option<ExtensionKey>,
    next_extension_id: Arc<AtomicU64>,
}

impl ProductExtensionHost {
    fn new(
        pool: Arc<ExtensionPool>,
        mut inbox: ExtensionEventInbox,
        cx: &mut Context<Self>,
    ) -> Self {
        let application_signal = super::application_observation::signal(cx);
        let product_commands = cx.global::<ApplicationProductCommands>().0.clone();
        let catalog = product_commands.read(cx).catalog();
        catalog.borrow_mut().declare_view_kind("outline");
        let commands = ExtensionCommandBridge::with_catalog(catalog);
        #[cfg(test)]
        let outline_tree = cx.new(|cx| TreeView::new("outline", cx));
        let mut semantics = ExtensionSemanticBridge::new();
        semantics.declare_tree_kind("outline");
        #[cfg(test)]
        semantics
            .add_tree_view("outline", outline_tree.clone(), cx)
            .unwrap();
        #[cfg(test)]
        let outline_tree_subscription = cx.subscribe(&outline_tree, |this, _, event, cx| {
            this.handle_tree_event(event, cx)
        });
        let release_subscription = cx.on_release(|host, cx| {
            let lifecycles = host.lifecycles.iter().copied().collect::<Vec<_>>();
            for key in lifecycles {
                let _ = host.pool.unload(key);
                host.remove_foreground_lifecycle(key, cx);
            }
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
            application_observation: Default::default(),
            #[cfg(test)]
            application_events: Vec::new(),
            _application_window_closed: cx.on_window_closed(application_changed),
            _application_signal: cx.observe(&application_signal, |host, _, cx| {
                host.refresh_application_observation(cx)
            }),
            buffer_observers: HashMap::new(),
            commands,
            completion_editors: Vec::new(),
            command_targets: HashMap::new(),
            view_command_handlers: HashMap::new(),
            #[cfg(test)]
            hold_clipboard_writes: false,
            #[cfg(test)]
            deferred_view_requests: Vec::new(),
            semantics,
            #[cfg(test)]
            _outline_tree: outline_tree,
            #[cfg(test)]
            _outline_tree_subscription: outline_tree_subscription,
            _event_task: event_task,
            _release_subscription: release_subscription,
            startup_report: None,
            _startup_task: None,
            _personal_startup_task: None,
            personal_config_key: None,
            next_extension_id: Arc::new(AtomicU64::new(1)),
        }
    }

    #[cfg(test)]
    pub(crate) fn start_installed_extensions(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let _ = self.start_installed_extensions_with_completion(root, cx);
    }

    fn start_installed_extensions_with_completion(
        &mut self,
        root: PathBuf,
        cx: &mut Context<Self>,
    ) -> Option<tokio::sync::oneshot::Receiver<()>> {
        if self
            .startup_report
            .as_ref()
            .is_some_and(|snapshot| snapshot.state != StartupState::Complete)
        {
            return None;
        }
        let (done_sender, done_receiver) = tokio::sync::oneshot::channel();
        self._startup_task.take();
        let previous = self
            .lifecycles
            .iter()
            .copied()
            .filter(|key| Some(*key) != self.personal_config_key)
            .collect::<Vec<_>>();
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
            let _ = done_sender.send(());
        }));
        Some(done_receiver)
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

    pub(crate) fn start_product_startup(
        &mut self,
        config_root: Option<PathBuf>,
        extensions_root: Result<PathBuf, String>,
        cx: &mut Context<Self>,
    ) {
        let capture = cx.background_executor().spawn(async move {
            let root = match config_root {
                Some(root) => root,
                None => personal_config::user_config_root().map_err(|cause| {
                    ConfigDiagnostic::new(None, &PathBuf::from("<user configuration>"), cause)
                })?,
            };
            personal_config::capture(&root).map(|sources| (root, sources))
        });
        self._personal_startup_task = Some(cx.spawn(async move |this, cx| {
            let result: Result<(), ConfigDiagnostic> = async {
                let (config_root, captured) = capture.await?;
                let canonical_config_root = captured.canonical_directory().to_path_buf();
                let pre_init = captured.pre_init;
                let post_init = captured.post_init;
                let post_init_error = captured.post_init_error.clone();
                let config_key = if let Some(graph) = captured.into_graph() {
                    let key = ExtensionKey::new(ExtensionId::new(0), ExtensionLifecycleId::new(1));
                    let loaded = this
                        .update(cx, |host, _| host.load_personal_config(key, graph))
                        .map_err(|error| {
                            ConfigDiagnostic::new(None, &config_root, error.to_string())
                        })?;
                    loaded.map_err(|cause| ConfigDiagnostic::new(None, &config_root, cause))?;
                    Some(key)
                } else {
                    None
                };
                if pre_init {
                    let key = config_key.expect("pre-init has a loaded config graph");
                    let execution = this
                        .update(cx, |host, _| {
                            host.evaluate_personal_config_phase(key, HostConfigPhase::PreInit)
                        })
                        .map_err(|error| {
                            ConfigDiagnostic::new(
                                Some(ConfigPhase::PreInit),
                                &config_root.join(ConfigPhase::PreInit.entry()),
                                error.to_string(),
                            )
                        })?
                        .map_err(|cause| {
                            ConfigDiagnostic::new(
                                Some(ConfigPhase::PreInit),
                                &config_root.join(ConfigPhase::PreInit.entry()),
                                cause,
                            )
                        })?;
                    execution.await.map_err(|error| {
                        ConfigDiagnostic::from_runtime(
                            ConfigPhase::PreInit,
                            &config_root,
                            &canonical_config_root,
                            &error,
                        )
                    })?;
                }
                match extensions_root {
                    Ok(root) => {
                        let done = this
                            .update(cx, |host, cx| {
                                host.start_installed_extensions_with_completion(root, cx)
                            })
                            .map_err(|error| {
                                ConfigDiagnostic::new(None, &config_root, error.to_string())
                            })?
                            .ok_or_else(|| {
                                ConfigDiagnostic::new(
                                    None,
                                    &config_root,
                                    "installed extension startup is already running",
                                )
                            })?;
                        done.await.map_err(|error| {
                            ConfigDiagnostic::new(None, &config_root, error.to_string())
                        })?;
                    }
                    Err(cause) => {
                        let _ = this
                            .update(cx, |host, cx| host.report_extensions_root_error(cause, cx));
                    }
                }
                if let Some(error) = post_init_error {
                    return Err(error);
                }
                if post_init {
                    let key = config_key.expect("post-init has a loaded config graph");
                    let execution = this
                        .update(cx, |host, _| {
                            host.evaluate_personal_config_phase(key, HostConfigPhase::PostInit)
                        })
                        .map_err(|error| {
                            ConfigDiagnostic::new(
                                Some(ConfigPhase::PostInit),
                                &config_root.join(ConfigPhase::PostInit.entry()),
                                error.to_string(),
                            )
                        })?
                        .map_err(|cause| {
                            ConfigDiagnostic::new(
                                Some(ConfigPhase::PostInit),
                                &config_root.join(ConfigPhase::PostInit.entry()),
                                cause,
                            )
                        })?;
                    execution.await.map_err(|error| {
                        ConfigDiagnostic::from_runtime(
                            ConfigPhase::PostInit,
                            &config_root,
                            &canonical_config_root,
                            &error,
                        )
                    })?;
                }
                Ok(())
            }
            .await;
            if result.is_err() {
                let _ = this.update(cx, |host, cx| host.abort_product_startup(cx));
            }
            let _ = cx.update(|cx| product::finish_product_startup(result, cx));
        }));
    }

    fn load_personal_config(
        &mut self,
        key: ExtensionKey,
        graph: ModuleGraph,
    ) -> Result<(), String> {
        self.pool
            .load_personal_config(key, ExtensionConfig::default(), graph)
            .map_err(|error| error.to_string())?;
        self.personal_config_key = Some(key);
        self.admit_lifecycle(key);
        Ok(())
    }

    fn evaluate_personal_config_phase(
        &self,
        key: ExtensionKey,
        phase: HostConfigPhase,
    ) -> Result<ExtensionExecution, String> {
        self.pool
            .evaluate_personal_config_phase(key, phase)
            .map_err(|error| error.to_string())
    }

    fn abort_product_startup(&mut self, cx: &mut Context<Self>) {
        let lifecycles = self.lifecycles.iter().copied().collect::<Vec<_>>();
        for key in lifecycles {
            let _ = self.pool.unload(key);
            self.remove_lifecycle(key, cx);
        }
        self.personal_config_key = None;
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

    #[cfg(test)]
    pub(crate) fn load_test_source(
        &mut self,
        key: ExtensionKey,
        source: &str,
    ) -> Result<ExtensionExecution, String> {
        let graph = ModuleGraph::new(
            "file:///fixtures/view-command/",
            "main.js",
            BTreeMap::from([("main.js".into(), Arc::from(source))]),
        )
        .map_err(|error| error.to_string())?;
        self.load_package(key, graph)
    }

    fn admit_lifecycle(&mut self, key: ExtensionKey) {
        if self.lifecycles.insert(key) {
            self.buffers.admit_lifecycle(key.extension, key.lifecycle);
            self.commands.admit_lifecycle(key.extension, key.lifecycle);
            self.semantics.admit_lifecycle(key.extension, key.lifecycle);
        }
    }

    #[cfg(test)]
    pub(crate) fn take_test_application_events(
        &mut self,
    ) -> Vec<crate::host::protocol::ApplicationEvent> {
        std::mem::take(&mut self.application_events)
    }

    #[cfg(test)]
    pub(crate) fn application_observer_count(&self) -> usize {
        self.application_observation.subscriptions().count()
    }

    #[cfg(test)]
    pub(crate) fn lifecycle_count(&self) -> usize {
        self.lifecycles.len()
    }

    #[cfg(test)]
    pub(crate) fn start_test_fixture(&mut self, name: &str, cx: &mut Context<Self>) {
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
            .expect("test fixture graph is valid");
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

    #[cfg(test)]
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
        let response = match request_route(&request.operation) {
            RequestRoute::Buffer => {
                let invocation = request.invocation;
                let commands = &self.commands;
                Some(
                    self.buffers
                        .dispatch(request, || commands.is_cancelled(invocation), cx),
                )
            }
            RequestRoute::Command => {
                if matches!(&request.operation, HostOperation::InvokeCommand { .. })
                    && request.invocation.is_none()
                {
                    self.dispatch_script_command(request)
                } else if matches!(&request.operation, HostOperation::InvokeCommand { .. }) {
                    cx.defer(move |cx| route_nested_command(request, cx));
                    None
                } else {
                    Some(self.commands.handle_host_request(request))
                }
            }
            RequestRoute::View => self.dispatch_view_request(request, cx),
            RequestRoute::Application => Some(self.dispatch_application_request(request, cx)),
            RequestRoute::ApplicationMutation => {
                if matches!(
                    &request.operation,
                    HostOperation::OpenFolder { .. } | HostOperation::ReplaceWorkspaceRoots { .. }
                ) {
                    self.dispatch_workspace_operation(request, cx)
                } else {
                    self.dispatch_document_operation(request, cx)
                }
            }
            RequestRoute::Semantic => {
                let removed = match &request.operation {
                    HostOperation::UnregisterCompletionProvider { registration } => {
                        Some(*registration)
                    }
                    _ => None,
                };
                let response = self.semantics.dispatch(request, cx);
                if response.result.is_ok()
                    && let Some(registration) = removed
                {
                    self.cancel_completion_provider(registration, cx);
                }
                Some(response)
            }
            RequestRoute::Keymap => Some(self.apply_keybinding_batch(request, cx)),
        };
        if let Some(response) = response {
            let _ = self.pool.respond(response);
        }
        self.drain_command_events(cx);
    }

    fn apply_keybinding_batch(
        &mut self,
        request: HostRequest,
        cx: &mut Context<Self>,
    ) -> crate::host::protocol::HostResponse {
        use crate::host::protocol::{HostRequestError, HostResponse, HostResponseValue};

        let key = ExtensionKey::new(request.extension, request.lifecycle);
        let result = if !self.lifecycles.contains(&key) {
            Err(HostRequestError::Cancelled)
        } else if !cx.has_global::<super::keymaps::ApplicationKeymaps>() {
            Ok(HostResponseValue::KeybindingsApplied {
                error: Some("application keymap is unavailable".into()),
            })
        } else {
            let owner = if self.personal_config_key == Some(key) {
                super::keymaps::BindingOwner::Personal(key.extension, key.lifecycle)
            } else {
                super::keymaps::BindingOwner::Extension(key.extension, key.lifecycle)
            };
            let HostOperation::MutateKeybindings { mutations } = &request.operation else {
                unreachable!("only keymap mutations enter this route");
            };
            let result = cx
                .global_mut::<super::keymaps::ApplicationKeymaps>()
                .0
                .apply_batch(owner, mutations);
            if matches!(result, Ok(true)) {
                super::keymaps::rebuild(cx);
            }
            Ok(HostResponseValue::KeybindingsApplied {
                error: result.err(),
            })
        };
        HostResponse {
            extension: request.extension,
            lifecycle: request.lifecycle,
            id: request.id,
            result,
        }
    }

    fn dispatch_script_command(
        &self,
        request: HostRequest,
    ) -> Option<crate::host::protocol::HostResponse> {
        use crate::host::protocol::{HostRequestError, HostResponse};
        let error = if self
            .lifecycles
            .contains(&ExtensionKey::new(request.extension, request.lifecycle))
        {
            HostRequestError::ContextUnavailable
        } else {
            HostRequestError::Cancelled
        };
        Some(HostResponse {
            extension: request.extension,
            lifecycle: request.lifecycle,
            id: request.id,
            result: Err(error),
        })
    }

    fn dispatch_view_request(
        &mut self,
        request: HostRequest,
        cx: &mut Context<Self>,
    ) -> Option<crate::host::protocol::HostResponse> {
        #[cfg(test)]
        if self.hold_clipboard_writes
            && matches!(&request.operation, HostOperation::WriteClipboardText { .. })
        {
            self.deferred_view_requests.push(request);
            return None;
        }
        Some(self.handle_view_request(request, cx))
    }

    #[cfg(test)]
    pub(crate) fn hold_clipboard_writes(&mut self) {
        self.hold_clipboard_writes = true;
    }

    #[cfg(test)]
    pub(crate) fn pending_view_request_count(&self) -> usize {
        self.deferred_view_requests.len()
    }

    #[cfg(test)]
    pub(crate) fn release_view_requests(&mut self, cx: &mut Context<Self>) {
        self.hold_clipboard_writes = false;
        for request in std::mem::take(&mut self.deferred_view_requests) {
            let response = self.handle_view_request(request, cx);
            let _ = self.pool.respond(response);
        }
    }

    fn handle_view_request(
        &self,
        request: HostRequest,
        cx: &mut App,
    ) -> crate::host::protocol::HostResponse {
        use crate::host::protocol::{HostRequestError, HostResponseValue};
        let result = (|| {
            if !self
                .lifecycles
                .contains(&ExtensionKey::new(request.extension, request.lifecycle))
            {
                return Err(HostRequestError::Cancelled);
            }
            if let HostOperation::WriteClipboardText { text } = &request.operation {
                if self.commands.is_cancelled(request.invocation) {
                    return Err(HostRequestError::Cancelled);
                }
                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                return Ok(HostResponseValue::ClipboardTextWritten);
            }
            let explicit = match request.operation {
                HostOperation::ActiveBuffer { target }
                | HostOperation::SelectedViewText { target } => target,
                _ => return Err(HostRequestError::UnsupportedOperation),
            };
            let root = self.commands.authorize_context(&request, explicit)?;
            let target = self
                .command_targets
                .get(&root)
                .ok_or(HostRequestError::InvalidTarget)?;
            target
                .validate_tab(cx)
                .and_then(|()| target.validate_view(cx))
                .map_err(|_| HostRequestError::InvalidTarget)?;
            match &request.operation {
                HostOperation::ActiveBuffer { .. } => Ok(HostResponseValue::ActiveBuffer(
                    self.commands.captured_buffer(request.invocation.unwrap()),
                )),
                HostOperation::SelectedViewText { .. } => {
                    let invocation = request.invocation.unwrap();
                    let (handler, name) = self
                        .view_command_handlers
                        .get(&invocation)
                        .ok_or(HostRequestError::UnsupportedOperation)?;
                    let view = target.view.ok_or(HostRequestError::UnsupportedOperation)?;
                    if self
                        .commands
                        .catalog()
                        .borrow()
                        .resolve_view(view, name.as_ref())
                        != Some(*handler)
                    {
                        return Err(HostRequestError::InvalidTarget);
                    }
                    let shell = target
                        .shell
                        .upgrade()
                        .ok_or(HostRequestError::InvalidTarget)?;
                    let tree = shell
                        .read(cx)
                        .extension_command_view(view, cx)
                        .ok_or(HostRequestError::InvalidTarget)?;
                    Ok(HostResponseValue::SelectedViewText(
                        tree.read(cx).selected_text(),
                    ))
                }
                _ => Err(HostRequestError::UnsupportedOperation),
            }
        })();
        crate::host::protocol::HostResponse {
            extension: request.extension,
            lifecycle: request.lifecycle,
            id: request.id,
            result,
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
                    ExtensionCommandEvent::DispatchExtension {
                        root,
                        mut invocation,
                        buffer,
                    } => {
                        invocation.capture = self.command_targets.get(&root).map(|target| {
                            crate::host::protocol::CommandCapture {
                                authority: root,
                                window: format!("{:?}", target.window.window_id()),
                                view: target.view.map(|view| format!("{view:?}")),
                            }
                        });
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
                                let outcome =
                                    this.validate_view_command_outcome(id, root, outcome, cx);
                                this.commands.complete(id, outcome);
                                if id == root {
                                    this.command_targets.remove(&root);
                                    this.view_command_handlers.remove(&root);
                                }
                                this.drain_command_events(cx);
                            });
                        })
                        .detach();
                    }
                }
            }
        }
        self.view_command_handlers
            .retain(|invocation, _| self.commands.contains_invocation(*invocation));
    }

    fn validate_view_command_outcome(
        &self,
        invocation: CommandInvocationId,
        root: CommandInvocationId,
        outcome: crate::host::protocol::CommandOutcome,
        cx: &App,
    ) -> crate::host::protocol::CommandOutcome {
        use crate::host::protocol::CommandOutcome;

        if invocation == root {
            return outcome;
        }
        let Some((handler, name)) = self.view_command_handlers.get(&invocation) else {
            return outcome;
        };
        let Some(target) = self.command_targets.get(&root) else {
            return CommandOutcome::InvalidTarget;
        };
        if let Err(invalid) = target
            .validate_tab(cx)
            .and_then(|()| target.validate_view(cx))
        {
            return invalid;
        }
        if !self
            .lifecycles
            .contains(&ExtensionKey::new(handler.extension, handler.lifecycle))
            || target.view.is_none_or(|view| {
                self.commands
                    .catalog()
                    .borrow()
                    .resolve_view(view, name.as_ref())
                    != Some(*handler)
            })
        {
            CommandOutcome::Cancelled
        } else {
            outcome
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
                if target.focus != *focus {
                    return;
                }
                let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
                dispatcher.update(cx, |dispatcher, cx| {
                    dispatcher.dispatch(
                        Command {
                            name: command.clone().into(),
                            arguments: crate::host::protocol::CommandArgumentValue::Null,
                        },
                        target,
                        cx,
                    );
                });
            }
        }
    }

    fn start_global_command(
        &mut self,
        command: Command,
        target: ProductCommandTarget,
        handler: CommandTarget,
        completion: super::CommandCompletion,
        cx: &mut Context<Self>,
    ) -> Result<(), crate::host::protocol::CommandOutcome> {
        use crate::host::protocol::CommandOutcome;

        target.validate_tab(cx)?;
        target.validate_view(cx)?;
        if !cx.windows().contains(&target.window) || target.shell.upgrade().is_none() {
            return Err(CommandOutcome::InvalidTarget);
        }
        let name = command.name.clone();
        if self.commands.catalog().borrow().resolve(name.as_ref())
            != Ok(CommandTargetKind::Extension(handler))
            || !self
                .lifecycles
                .contains(&ExtensionKey::new(handler.extension, handler.lifecycle))
        {
            return Err(CommandOutcome::Cancelled);
        }
        let buffer = self.open_target_buffer(&target, cx);
        let execution = self
            .commands
            .enqueue_extension_root(command, handler, buffer);
        self.command_targets.insert(execution.id, target.clone());
        self.drain_command_events(cx);
        cx.spawn(async move |this, cx| {
            let outcome = execution
                .completion
                .await
                .unwrap_or(CommandOutcome::Cancelled);
            let outcome = cx
                .update(|cx| {
                    if !cx.windows().contains(&target.window) {
                        return CommandOutcome::InvalidTarget;
                    }
                    if let Err(invalid) = target
                        .validate_tab(cx)
                        .and_then(|()| target.validate_view(cx))
                    {
                        return invalid;
                    }
                    let Some(host) = this.upgrade() else {
                        return CommandOutcome::Cancelled;
                    };
                    let host = host.read(cx);
                    if !host
                        .lifecycles
                        .contains(&ExtensionKey::new(handler.extension, handler.lifecycle))
                        || host.commands.catalog().borrow().resolve(name.as_ref())
                            != Ok(CommandTargetKind::Extension(handler))
                    {
                        CommandOutcome::Cancelled
                    } else {
                        outcome
                    }
                })
                .unwrap_or(CommandOutcome::InvalidTarget);
            completion.complete(outcome);
        })
        .detach();
        Ok(())
    }

    fn start_view_command(
        &mut self,
        command: Command,
        target: ProductCommandTarget,
        handler: CommandTarget,
        completion: super::CommandCompletion,
        cx: &mut Context<Self>,
    ) -> Result<(), crate::host::protocol::CommandOutcome> {
        let view = target
            .view
            .ok_or(crate::host::protocol::CommandOutcome::InvalidTarget)?;
        let active = self
            .commands
            .catalog()
            .borrow()
            .resolve_view(view, command.name.as_ref());
        if active != Some(handler)
            || !self
                .lifecycles
                .contains(&ExtensionKey::new(handler.extension, handler.lifecycle))
        {
            return Err(crate::host::protocol::CommandOutcome::InvalidTarget);
        }
        let buffer = self.open_target_buffer(&target, cx);
        let name = command.name.clone();
        let execution = self
            .commands
            .enqueue_extension_root(command, handler, buffer);
        self.command_targets.insert(execution.id, target.clone());
        self.view_command_handlers
            .insert(execution.id, (handler, name.clone()));
        self.drain_command_events(cx);
        cx.spawn(async move |this, cx| {
            let outcome = execution
                .completion
                .await
                .unwrap_or(crate::host::protocol::CommandOutcome::Cancelled);
            let outcome = cx
                .update(|cx| {
                    if let Err(invalid) = target.validate_view(cx) {
                        return invalid;
                    }
                    let Some(host) = this.upgrade() else {
                        return crate::host::protocol::CommandOutcome::Cancelled;
                    };
                    let host = host.read(cx);
                    if !host
                        .lifecycles
                        .contains(&ExtensionKey::new(handler.extension, handler.lifecycle))
                        || target.view.is_none_or(|view| {
                            host.commands
                                .catalog()
                                .borrow()
                                .resolve_view(view, name.as_ref())
                                != Some(handler)
                        })
                    {
                        crate::host::protocol::CommandOutcome::Cancelled
                    } else {
                        outcome
                    }
                })
                .unwrap_or(crate::host::protocol::CommandOutcome::InvalidTarget);
            completion.complete(outcome);
        })
        .detach();
        Ok(())
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
        self.completion_editors
            .retain(|editor| editor.upgrade().is_some());
        if !self
            .completion_editors
            .iter()
            .any(|candidate| candidate.upgrade().as_ref() == Some(&editor))
        {
            self.completion_editors.push(editor.downgrade());
        }
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

    fn cancel_completion_provider(
        &mut self,
        registration: crate::host::protocol::CompletionProviderRegistrationId,
        cx: &mut App,
    ) {
        self.completion_editors.retain(|editor| {
            let Some(editor) = editor.upgrade() else {
                return false;
            };
            editor.update(cx, |editor, cx| {
                editor.cancel_completion_provider(registration, cx)
            });
            true
        });
    }

    fn open_target_buffer(
        &mut self,
        target: &ProductCommandTarget,
        cx: &mut Context<Self>,
    ) -> Option<crate::host::protocol::BufferHandle> {
        if target.view.is_some() {
            return None;
        }
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
        Some(self.track_buffer(&model, cx))
    }

    fn track_buffer(
        &mut self,
        model: &Entity<super::model::BufferModel>,
        cx: &mut Context<Self>,
    ) -> crate::host::protocol::BufferHandle {
        let buffer = self.buffers.open_buffer(model);
        self.buffer_observers.entry(buffer).or_insert_with(|| {
            let _ = self.buffers.drain_model_changes(model, cx);
            cx.observe(model, move |this, model, cx| {
                this.route_model_changes(&model, cx);
                if !model.read(cx).is_open() {
                    this.buffer_observers.remove(&buffer);
                }
                application_changed(cx);
            })
        });
        buffer
    }

    fn route_model_changes(&mut self, model: &Entity<super::model::BufferModel>, cx: &mut App) {
        for dispatch in self.buffers.drain_model_changes(model, cx) {
            let _ = self.pool.dispatch_buffer_change(
                ExtensionKey::new(dispatch.extension, dispatch.lifecycle),
                dispatch.subscription,
                dispatch.change,
            );
        }
    }

    fn application_snapshot(
        &mut self,
        cx: &mut Context<Self>,
    ) -> crate::host::protocol::ApplicationSnapshot {
        use super::documents::ApplicationDocuments;
        use crate::host::protocol::ApplicationSnapshot;
        let Some(documents) = cx
            .try_global::<ApplicationDocuments>()
            .map(|global| global.0.clone())
        else {
            return ApplicationSnapshot::default();
        };
        let models = documents
            .read(cx)
            .documents()
            .map(|document| (document.id(), document.model().clone()))
            .collect::<Vec<_>>();
        let buffers = models
            .iter()
            .map(|(id, model)| (*id, self.track_buffer(model, cx)))
            .collect::<HashMap<_, _>>();
        for (_, model) in &models {
            self.route_model_changes(model, cx);
        }
        let document_infos = documents
            .read(cx)
            .documents()
            .map(|document| application_document_info(document, buffers[&document.id()], cx))
            .collect();
        let active = cx.active_window();
        let mut snapshot = ApplicationSnapshot {
            documents: document_infos,
            ..Default::default()
        };
        for handle in cx.windows() {
            if handle.downcast::<ProductShell>().is_none() {
                continue;
            }
            if let Ok(info) = cx.update_window(handle, |root, window, cx| {
                root.downcast::<ProductShell>()
                    .unwrap()
                    .read(cx)
                    .application_window(window, &buffers, cx)
            }) {
                if active == Some(handle) {
                    snapshot.active_window = Some(info.target.clone());
                    snapshot.active_target = info.current_target.clone();
                }
                snapshot.windows.push(info);
            }
        }
        snapshot
            .windows
            .sort_by(|a, b| a.target.window.cmp(&b.target.window));
        snapshot
    }

    fn refresh_application_observation(&mut self, cx: &mut Context<Self>) {
        let snapshot = self.application_snapshot(cx);
        let events = self.application_observation.commit(snapshot);
        #[cfg(test)]
        self.application_events.extend(events.iter().cloned());
        for event in events {
            for (subscription, key) in self.application_observation.subscriptions() {
                let _ = self
                    .pool
                    .dispatch_application_change(key, subscription, event.clone());
            }
        }
    }

    pub(crate) fn operation_is_live(
        &self,
        key: ExtensionKey,
        invocation: Option<CommandInvocationId>,
    ) -> bool {
        self.lifecycles.contains(&key) && !self.commands.is_cancelled(invocation)
    }

    fn dispatch_document_operation(
        &mut self,
        request: HostRequest,
        cx: &mut Context<Self>,
    ) -> Option<crate::host::protocol::HostResponse> {
        use crate::host::protocol::{HostRequestError, HostResponse, HostResponseValue};
        let key = ExtensionKey::new(request.extension, request.lifecycle);
        let result = (|| {
            if !self.operation_is_live(key, request.invocation) {
                return Err(HostRequestError::Cancelled);
            }
            let target = match &request.operation {
                HostOperation::OpenDocument { target, .. }
                | HostOperation::CreateUntitled { target }
                | HostOperation::PresentDocument { target, .. } => target.clone(),
                _ => return Err(HostRequestError::UnsupportedOperation),
            };
            let destination = self.document_destination(&request, target, cx)?;
            let uri = match request.operation.clone() {
                HostOperation::CreateUntitled { .. } => {
                    let (document, target) = product::operations::create_untitled(destination, cx)?;
                    let value = self.document_presentation(document, target, cx)?;
                    return Ok(Some(HostResponseValue::DocumentPresented(value)));
                }
                HostOperation::PresentDocument { id, .. } => {
                    let (document, target) =
                        product::operations::present_document(id, destination, cx)?;
                    let value = self.document_presentation(document, target, cx)?;
                    return Ok(Some(HostResponseValue::DocumentPresented(value)));
                }
                HostOperation::OpenDocument { uri, .. } => uri,
                _ => unreachable!(),
            };
            let host = cx.entity().downgrade();
            let caller = product::operations::OperationCaller {
                host: host.clone(),
                key,
                invocation: request.invocation,
            };
            let delivered = request.clone();
            let pool = self.pool.clone();
            product::operations::open_document(
                uri,
                destination,
                caller,
                move |result, cx| {
                    let result = result
                        .and_then(|(document, target)| {
                            let host = host.upgrade().ok_or(HostRequestError::Cancelled)?;
                            host.update(cx, |host, cx| {
                                host.document_presentation(document, target, cx)
                            })
                        })
                        .map(HostResponseValue::DocumentPresented);
                    let _ = pool.respond(HostResponse {
                        extension: delivered.extension,
                        lifecycle: delivered.lifecycle,
                        id: delivered.id,
                        result,
                    });
                },
                cx,
            )?;
            Ok(None)
        })();
        result.transpose().map(|result| HostResponse {
            extension: request.extension,
            lifecycle: request.lifecycle,
            id: request.id,
            result,
        })
    }

    fn document_destination(
        &self,
        request: &HostRequest,
        target: Option<crate::host::protocol::DocumentDestination>,
        cx: &App,
    ) -> Result<crate::host::protocol::DocumentDestination, crate::host::protocol::HostRequestError>
    {
        use crate::host::protocol::DocumentDestination;
        if let Some(target) = target {
            return Ok(target);
        }
        Ok(DocumentDestination::Window(
            self.operation_window(request, cx)?,
        ))
    }

    fn operation_window(
        &self,
        request: &HostRequest,
        cx: &App,
    ) -> Result<crate::host::protocol::WindowTarget, crate::host::protocol::HostRequestError> {
        use crate::host::protocol::{HostRequestError, WindowTarget};
        let window = if let Some(invocation) = request.invocation {
            let root = self
                .commands
                .root_of(invocation)
                .ok_or(HostRequestError::InvalidTarget)?;
            self.command_targets
                .get(&root)
                .ok_or(HostRequestError::InvalidTarget)?
                .window
        } else {
            cx.active_window()
                .ok_or(HostRequestError::ContextUnavailable)?
        };
        if window.downcast::<ProductShell>().is_none() {
            return Err(HostRequestError::ContextUnavailable);
        }
        Ok(WindowTarget {
            window: window.window_id().as_u64().to_string(),
        })
    }

    fn dispatch_workspace_operation(
        &mut self,
        request: HostRequest,
        cx: &mut Context<Self>,
    ) -> Option<crate::host::protocol::HostResponse> {
        use crate::host::protocol::{HostRequestError, HostResponse, HostResponseValue};
        let key = ExtensionKey::new(request.extension, request.lifecycle);
        let result = (|| {
            if !self.operation_is_live(key, request.invocation) {
                return Err(HostRequestError::Cancelled);
            }
            let (roots, target) = match request.operation.clone() {
                HostOperation::OpenFolder { uri, target } => (vec![uri], target),
                HostOperation::ReplaceWorkspaceRoots { roots, target } => (roots, target),
                _ => return Err(HostRequestError::UnsupportedOperation),
            };
            let target = match target {
                Some(target) => target,
                None => self.operation_window(&request, cx)?,
            };
            let caller = product::operations::OperationCaller {
                host: cx.entity().downgrade(),
                key,
                invocation: request.invocation,
            };
            let delivered = request.clone();
            let pool = self.pool.clone();
            product::operations::replace_roots(
                roots,
                target,
                caller,
                move |result| {
                    let _ = pool.respond(HostResponse {
                        extension: delivered.extension,
                        lifecycle: delivered.lifecycle,
                        id: delivered.id,
                        result: result.map(HostResponseValue::WorkspaceUpdated),
                    });
                },
                cx,
            )
        })();
        result.err().map(|error| HostResponse {
            extension: request.extension,
            lifecycle: request.lifecycle,
            id: request.id,
            result: Err(error),
        })
    }

    fn document_presentation(
        &mut self,
        id: super::documents::DocumentId,
        target: crate::host::protocol::PresentationTarget,
        cx: &mut Context<Self>,
    ) -> Result<crate::host::protocol::DocumentPresentation, crate::host::protocol::HostRequestError>
    {
        use super::documents::ApplicationDocuments;
        use crate::host::protocol::{
            DocumentPresentation, HostRequestError, PresentationInfo, SurfaceKind,
        };
        let documents = cx.global::<ApplicationDocuments>().0.clone();
        let model = documents
            .read(cx)
            .get(id)
            .ok_or(HostRequestError::BufferClosed)?
            .model()
            .clone();
        let buffer = self.track_buffer(&model, cx);
        let document = documents
            .read(cx)
            .get(id)
            .ok_or(HostRequestError::BufferClosed)?;
        let info = application_document_info(document, buffer, cx);
        Ok(DocumentPresentation {
            presentation: PresentationInfo {
                target,
                kind: SurfaceKind::Document,
                document: Some(info.id.clone()),
                buffer: Some(buffer),
            },
            document: info,
        })
    }

    fn dispatch_application_request(
        &mut self,
        request: HostRequest,
        cx: &mut Context<Self>,
    ) -> crate::host::protocol::HostResponse {
        use crate::host::protocol::{
            ApplicationQuery, ApplicationQueryResult, HostRequestError, HostResponse,
            HostResponseValue,
        };
        let result = if !self
            .lifecycles
            .contains(&ExtensionKey::new(request.extension, request.lifecycle))
            || self.commands.is_cancelled(request.invocation)
        {
            Err(HostRequestError::Cancelled)
        } else if matches!(request.operation, HostOperation::ObserveWorkbench) {
            self.refresh_application_observation(cx);
            Ok(HostResponseValue::WorkbenchObserved(
                self.application_observation
                    .subscribe(ExtensionKey::new(request.extension, request.lifecycle)),
            ))
        } else if let HostOperation::UnobserveWorkbench { subscription } = request.operation {
            if self.application_observation.unsubscribe(
                subscription,
                ExtensionKey::new(request.extension, request.lifecycle),
            ) {
                Ok(HostResponseValue::WorkbenchUnobserved)
            } else {
                Err(HostRequestError::OwnershipDisposed)
            }
        } else if let HostOperation::ApplicationQuery(query) = request.operation {
            let snapshot = self.application_snapshot(cx);
            let value = match query {
                ApplicationQuery::Documents => {
                    ApplicationQueryResult::Documents(snapshot.documents)
                }
                ApplicationQuery::Document { id } => ApplicationQueryResult::Document(
                    snapshot
                        .documents
                        .into_iter()
                        .find(|document| document.id == id),
                ),
                ApplicationQuery::Windows => ApplicationQueryResult::Windows(snapshot.windows),
                ApplicationQuery::Window { target } => ApplicationQueryResult::Window(
                    snapshot
                        .windows
                        .into_iter()
                        .find(|window| window.target == target),
                ),
                ApplicationQuery::Workspace { target } => ApplicationQueryResult::Workspace(
                    snapshot
                        .windows
                        .into_iter()
                        .find(|window| window.target == target)
                        .map(|window| window.workspace),
                ),
            };
            Ok(HostResponseValue::ApplicationQuery(value))
        } else {
            Err(HostRequestError::UnsupportedOperation)
        };
        HostResponse {
            extension: request.extension,
            lifecycle: request.lifecycle,
            id: request.id,
            result,
        }
    }

    fn remove_lifecycle(&mut self, key: ExtensionKey, cx: &mut Context<Self>) {
        self.remove_foreground_lifecycle(key, cx);
        self.drain_command_events(cx);
    }

    fn remove_foreground_lifecycle(&mut self, key: ExtensionKey, cx: &mut App) {
        if !self.lifecycles.remove(&key) {
            return;
        }
        self.application_observation.remove_lifecycle(key);
        let personal = self.personal_config_key == Some(key);
        if cx.has_global::<super::keymaps::ApplicationKeymaps>() {
            let owner = if personal {
                super::keymaps::BindingOwner::Personal(key.extension, key.lifecycle)
            } else {
                super::keymaps::BindingOwner::Extension(key.extension, key.lifecycle)
            };
            if cx
                .global_mut::<super::keymaps::ApplicationKeymaps>()
                .0
                .remove_owner(owner)
            {
                super::keymaps::rebuild(cx);
            }
        }
        if self.personal_config_key == Some(key) {
            self.personal_config_key = None;
        }
        self.commands.remove_lifecycle(key.extension, key.lifecycle);
        self.command_targets
            .retain(|root, _| self.commands.contains_invocation(*root));
        self.view_command_handlers
            .retain(|root, _| self.commands.contains_invocation(*root));
        self.buffers
            .remove_lifecycle(key.extension, key.lifecycle, cx);
        for provider in self.semantics.completion_snapshot() {
            if (provider.extension, provider.lifecycle) == (key.extension, key.lifecycle) {
                self.cancel_completion_provider(provider.id, cx);
            }
        }
        self.semantics
            .remove_lifecycle(key.extension, key.lifecycle, cx);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequestRoute {
    Application,
    Buffer,
    Command,
    View,
    Semantic,
    ApplicationMutation,
    Keymap,
}

fn application_document_info(
    document: &super::documents::Document,
    buffer: crate::host::protocol::BufferHandle,
    cx: &App,
) -> crate::host::protocol::DocumentInfo {
    use super::documents::DocumentState;
    use crate::host::protocol::{DocumentInfo, DocumentKind};
    DocumentInfo {
        id: document.identity().to_string(),
        title: document.title().to_string(),
        kind: match document.state() {
            DocumentState::Untitled { .. } => DocumentKind::Untitled,
            DocumentState::Destination { .. } => DocumentKind::Destination,
            DocumentState::Persisted { .. } => DocumentKind::Persisted,
            DocumentState::Generated => DocumentKind::Generated,
        },
        resource_uri: document.resource_uri().map(ToString::to_string),
        dirty: document.is_dirty(cx),
        revision: document.model().read(cx).revision(),
        buffer,
    }
}

fn request_route(operation: &HostOperation) -> RequestRoute {
    match operation {
        HostOperation::OpenDocument { .. }
        | HostOperation::CreateUntitled { .. }
        | HostOperation::PresentDocument { .. }
        | HostOperation::OpenFolder { .. }
        | HostOperation::ReplaceWorkspaceRoots { .. } => RequestRoute::ApplicationMutation,
        HostOperation::ApplicationQuery(_)
        | HostOperation::ObserveWorkbench
        | HostOperation::UnobserveWorkbench { .. } => RequestRoute::Application,
        HostOperation::MutateKeybindings { .. } => RequestRoute::Keymap,
        HostOperation::Snapshot { .. }
        | HostOperation::ApplyEdits { .. }
        | HostOperation::SubscribeBufferChanges { .. }
        | HostOperation::UnsubscribeBufferChanges { .. }
        | HostOperation::ReplaceEditorContributions { .. }
        | HostOperation::DisposeEditorContributions { .. } => RequestRoute::Buffer,
        HostOperation::RegisterCommand { .. }
        | HostOperation::RegisterViewCommand { .. }
        | HostOperation::UnregisterCommand { .. }
        | HostOperation::InvokeCommand { .. }
        | HostOperation::CompleteInlineCommand { .. } => RequestRoute::Command,
        HostOperation::ActiveBuffer { .. }
        | HostOperation::SelectedViewText { .. }
        | HostOperation::WriteClipboardText { .. } => RequestRoute::View,
        HostOperation::RegisterTreeProvider { .. }
        | HostOperation::InvalidateTreeProvider { .. }
        | HostOperation::UnregisterTreeProvider { .. }
        | HostOperation::RegisterCompletionProvider { .. }
        | HostOperation::UnregisterCompletionProvider { .. } => RequestRoute::Semantic,
    }
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

    #[gpui::test]
    fn application_clipboard_and_context_failures_have_distinct_results(cx: &mut TestAppContext) {
        use crate::host::protocol::{HostRequestError, HostResponseValue, RequestId};
        cx.update(|cx| {
            let commands = cx.new(super::super::product_commands::ProductCommandDispatcher::new);
            cx.set_global(ApplicationProductCommands(commands));
            let host = install(cx);
            let key = ExtensionKey::new(ExtensionId::new(701), ExtensionLifecycleId::new(1));
            host.update(cx, |host, cx| {
                host.load(key, ExtensionConfig::default()).unwrap();
                let request = |operation| HostRequest {
                    extension: key.extension,
                    lifecycle: key.lifecycle,
                    id: RequestId::new(1),
                    invocation: None,
                    operation,
                };
                assert_eq!(
                    host.handle_view_request(
                        request(HostOperation::ActiveBuffer { target: None }),
                        cx
                    )
                    .result,
                    Err(HostRequestError::ContextUnavailable)
                );
                assert_eq!(
                    host.handle_view_request(
                        request(HostOperation::SelectedViewText { target: None }),
                        cx
                    )
                    .result,
                    Err(HostRequestError::ContextUnavailable)
                );
                assert_eq!(
                    host.handle_view_request(
                        request(HostOperation::ActiveBuffer {
                            target: Some(CommandInvocationId::new(1))
                        }),
                        cx
                    )
                    .result,
                    Err(HostRequestError::InvalidTarget)
                );
                assert_eq!(
                    host.handle_view_request(
                        request(HostOperation::WriteClipboardText {
                            text: "application".into()
                        }),
                        cx
                    )
                    .result,
                    Ok(HostResponseValue::ClipboardTextWritten)
                );
                assert_eq!(
                    cx.read_from_clipboard().unwrap().text().unwrap(),
                    "application"
                );
                host.unload(key, cx).unwrap();
                assert_eq!(
                    host.handle_view_request(
                        request(HostOperation::WriteClipboardText {
                            text: "late".into()
                        }),
                        cx
                    )
                    .result,
                    Err(HostRequestError::Cancelled)
                );
                assert_eq!(
                    cx.read_from_clipboard().unwrap().text().unwrap(),
                    "application"
                );
            });
        });
    }

    #[test]
    fn installed_graph_admission_checks_constructed_manifest_versions() {
        let mut manifest: extension_package::Manifest = serde_json::from_str(
            r#"{"name":"@example/tools","apiVersion":1,"version":"1.0.0","main":"main.js"}"#,
        )
        .unwrap();
        for version in [0, 2] {
            manifest.api_version = version;
            let package = InstalledPackage {
                directory: PathBuf::from("/fixtures/tools"),
                manifest: manifest.clone(),
                sources: BTreeMap::new(),
            };
            let cause = graph_for_package(&package).unwrap_err();
            assert!(
                cause.contains("apiVersion")
                    && cause.contains("expected")
                    && cause.contains(&format!("declared {version}")),
                "{cause}"
            );
        }
    }

    #[test]
    fn every_host_operation_has_one_foreground_owner() {
        assert_eq!(
            request_route(&HostOperation::ActiveBuffer { target: None }),
            RequestRoute::View
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
    fn nested_view_result_rejects_a_detached_captured_target(cx: &mut TestAppContext) {
        use crate::host::protocol::{CommandOutcome, CommandRegistrationId};

        cx.update(|cx| {
            let commands = cx.new(super::super::product_commands::ProductCommandDispatcher::new);
            cx.set_global(ApplicationProductCommands(commands));
            let host = install(cx);
            let root = CommandInvocationId::new(1);
            let child = CommandInvocationId::new(2);
            host.update(cx, |host, cx| {
                host.view_command_handlers.insert(
                    child,
                    (
                        CommandTarget {
                            registration: CommandRegistrationId::new(1),
                            extension: ExtensionId::new(1),
                            lifecycle: ExtensionLifecycleId::new(1),
                        },
                        "copy".into(),
                    ),
                );
                assert_eq!(
                    host.validate_view_command_outcome(child, root, CommandOutcome::Completed, cx),
                    CommandOutcome::InvalidTarget
                );
            });
        });
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
            host.update(cx, |host, cx| host.start_test_fixture("integration", cx));
            host
        });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            cx.run_until_parked();
            if cx.update(|cx| {
                let host = host.read(cx);
                host.semantics.completion_snapshot().len() == 2
                    && host._outline_tree.read(cx).lifecycle_state() == (true, 1, false, 0)
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
                host._outline_tree.read(cx).root_labels(),
                ["Pooled fixture"]
            );
            let definitions = host
                .commands
                .definitions()
                .map(|definition| definition.name.to_string())
                .collect::<Vec<_>>();
            assert!(definitions.contains(&"knot.fixture.primary".to_owned()));
            assert!(definitions.contains(&"knot.fixture.secondary".to_owned()));
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
                    host._outline_tree.read(cx).lifecycle_state(),
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
