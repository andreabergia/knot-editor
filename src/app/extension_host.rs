//! Product ownership and foreground routing for the pooled extension host.

pub(crate) mod handlers;

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

use gpui::{App, AppContext, Context, Entity, Global, Subscription, Task};

use crate::host::{
    lifecycle::ExtensionKey,
    module_graph::{ConfigPhase as HostConfigPhase, ModuleGraph},
    pool::{
        ExtensionConfig, ExtensionEvent, ExtensionEventInbox, ExtensionExecution, ExtensionPool,
    },
    protocol::{
        Command, CommandInvocationId, ExtensionId, ExtensionLifecycleId, HostRequest, ViewId,
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
    transient::TransientOwner,
    tree_view::TreeViewEvent,
    workbench::ExtensionView,
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
await workbench.registerView({
  id: "@fixture/primary.outline",
  title: "Outline",
  content: { kind: "tree", provider: { getChildren(parentId) {
    return parentId === null
      ? [{ id: "fixture", label: "Pooled fixture", collapsibleState: "none" }]
      : [];
  } } },
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

/// The durable storage and recovery owner of a discovered package.
fn package_owner(
    package: &InstalledPackage,
) -> Result<super::extension_storage::StorageOwner, String> {
    match package.origin {
        extension_package::PackageOrigin::Installed => {
            super::extension_storage::StorageOwner::package(&package.manifest.name)
        }
        extension_package::PackageOrigin::Bundled => {
            super::extension_storage::StorageOwner::bundled(&package.manifest.name)
        }
    }
}

fn graph_for_package(package: &InstalledPackage) -> Result<ModuleGraph, String> {
    extension_package::validate_package_api(&package.manifest)?;
    ModuleGraph::new(
        &package.module_root()?,
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

/// Publish a window's settled view-state change after committing any pending
/// metadata changes, so the event follows the window's structural events.
pub(crate) fn view_state_changed(window: crate::host::protocol::WindowTarget, cx: &mut App) {
    if let Some(host) = entity(cx) {
        host.update(cx, |host, cx| {
            host.publish_application_change(
                crate::host::protocol::ApplicationChange::ViewStateChanged { window },
                cx,
            )
        });
    }
}

struct ApplicationLaunched;

impl Global for ApplicationLaunched {}

/// Record that startup committed its windows; observers see `launched` once.
pub(crate) fn launched(cx: &mut App) {
    cx.set_global(ApplicationLaunched);
    application_changed(cx);
}

#[cfg(test)]
pub(crate) fn reset_launched_for_test(cx: &mut App) {
    if cx.has_global::<ApplicationLaunched>() {
        cx.remove_global::<ApplicationLaunched>();
    }
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

pub(crate) fn quit_after_storage(cx: &mut App) {
    let Some(host) = entity(cx) else {
        cx.quit();
        return;
    };
    let Some(drain) = host.update(cx, |host, _| host.shutdown_storage()) else {
        return;
    };
    let window_flush = cx.spawn(async move |cx| {
        // Windows are recorded once the shell that started quit is released,
        // and before quitting closes them.
        match cx.update(super::window_sessions::record_for_quit) {
            Ok(Some(flush)) => flush.await.is_ok(),
            _ => true,
        }
    });
    // A debounced tool-layout write may still be pending; force it out before
    // exiting so the last customization survives the restart.
    let layout_flush = cx
        .try_global::<super::tool_layout::ApplicationToolLayout>()
        .map(|store| store.0.flush());
    // A recovery capture inside its debounce window is drained so the last
    // edits reach the durable store before exit.
    let recovery_flush = cx
        .try_global::<super::recovery::ApplicationRecovery>()
        .map(|recovery| recovery.0.store().flush());
    cx.spawn(async move |cx| {
        drain.await;
        if let Some(flush) = recovery_flush {
            let _ = flush.await;
        }
        if let Some(flush) = layout_flush {
            let _ = flush.await;
        }
        window_flush.await;
        let _ = cx.update(|cx| cx.quit());
    })
    .detach();
}

pub(crate) fn install(cx: &mut App) -> Entity<ProductExtensionHost> {
    crate::host::declarations::registry()
        .validate_context::<handlers::HostContext>()
        .expect("valid application handler contexts");
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

pub(crate) fn view_definitions(cx: &App) -> Vec<super::extension_semantics::ViewDefinition> {
    entity(cx)
        .map(|host| host.read(cx).semantics.definitions())
        .unwrap_or_default()
}

/// Attach one native view instance to its definition and command catalog.
/// Returns false when the definition disappeared before attachment.
pub(crate) fn attach_view_instance(
    stable_id: String,
    window_id: String,
    view: ExtensionView,
    cx: &mut App,
) -> bool {
    let Some(host) = entity(cx) else { return false };
    host.update(cx, |host, cx| {
        if !host.semantics.has_definition(&stable_id) {
            return false;
        }
        let view_id = view.instance_id(cx);
        if host
            .commands
            .catalog()
            .borrow_mut()
            .bind_view(&stable_id, view_id)
            .is_err()
        {
            return false;
        }
        let attached = match &view {
            ExtensionView::Tree(tree) => host
                .semantics
                .add_tree_view(stable_id, window_id, tree.clone(), cx)
                .is_ok(),
            ExtensionView::Text(text) => {
                host.semantics
                    .add_text_view(stable_id, window_id, text.clone(), cx);
                true
            }
        };
        if !attached {
            host.commands.catalog().borrow_mut().remove_view(view_id);
            return false;
        }
        true
    })
}

pub(crate) fn detach_view_instance(view: ViewId, cx: &mut App) {
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

fn route_nested_command(
    request: crate::host::protocol::RequestContext,
    command: crate::host::protocol::Command,
    sender: super::extension_commands::CommandReply,
    cx: &mut App,
) {
    use crate::host::protocol::{CommandInvokeDispatch, CommandOutcome};
    let mut reply = Some(sender);

    let Some(host) = entity(cx) else { return };
    let parent = request.invocation.expect("nested invocation");
    if let Err(outcome) = host
        .read(cx)
        .commands
        .admit_child(parent, (request.extension, request.lifecycle))
    {
        let _ = reply
            .take()
            .unwrap()
            .send(Ok(CommandInvokeDispatch::Outcome { outcome }));
        return;
    }
    let target = host
        .read(cx)
        .commands
        .root_of(parent)
        .and_then(|root| host.read(cx).command_targets.get(&root).cloned());
    let Some(target) = target else {
        let _ = reply
            .take()
            .unwrap()
            .send(Ok(CommandInvokeDispatch::Outcome {
                outcome: CommandOutcome::Cancelled,
            }));
        return;
    };
    let (completion, receiver) = super::CommandCompletion::new();
    let claim = dispatch_to_captured_target(&command, &target, completion, cx);
    let (response, child) = host.update(cx, |host, cx| {
        let routed = match &claim {
            CommandClaim::Extension { handler, .. } => RoutedCommandTarget::Extension(*handler),
            _ => RoutedCommandTarget::Window,
        };
        let (response, child) =
            host.commands
                .handle_routed_child(&request, command.clone(), routed, &mut reply);
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
        let _ = reply.take().unwrap().send(response);
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

/// Test hosts isolate extension storage so bundled packages never read the
/// developer's real records. Production uses the platform application-data root.
#[cfg(test)]
fn test_storage(
    cx: &mut Context<ProductExtensionHost>,
) -> Result<super::extension_storage::ExtensionStorage, String> {
    #[allow(
        dead_code,
        reason = "keeps the test storage directory alive with the app"
    )]
    struct TestStorageDir(tempfile::TempDir);
    impl Global for TestStorageDir {}

    let directory = tempfile::tempdir().expect("test extension storage root");
    let root = directory.path().to_owned();
    cx.set_global(TestStorageDir(directory));
    Ok(super::extension_storage::ExtensionStorage::new(
        root,
        cx.background_executor().clone(),
    ))
}

#[cfg(not(test))]
fn test_storage(
    cx: &mut Context<ProductExtensionHost>,
) -> Result<super::extension_storage::ExtensionStorage, String> {
    super::extension_storage::ExtensionStorage::user_root().map(|root| {
        super::extension_storage::ExtensionStorage::new(root, cx.background_executor().clone())
    })
}

/// Owns the one process-wide extension pool and all foreground protocol state.
pub(crate) struct ProductExtensionHost {
    pool: Arc<ExtensionPool>,
    lifecycles: HashSet<ExtensionKey>,
    storage: Result<super::extension_storage::ExtensionStorage, String>,
    storage_owners: HashMap<ExtensionKey, super::extension_storage::StorageOwner>,
    storage_stopping: bool,
    storage_callers: Vec<(
        crate::host::protocol::RequestContext,
        std::sync::Weak<std::sync::atomic::AtomicBool>,
    )>,
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
    _event_task: Task<()>,
    _release_subscription: Subscription,
    startup_report: Option<StartupReportSnapshot>,
    _startup_task: Option<Task<()>>,
    _personal_startup_task: Option<Task<()>>,
    personal_config_key: Option<ExtensionKey>,
    /// The live lifecycle of each admitted package, by canonical name.
    package_keys: HashMap<String, ExtensionKey>,
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
        let commands = ExtensionCommandBridge::with_catalog(catalog);
        let semantics = ExtensionSemanticBridge::new();
        let release_subscription = cx.on_release(|host, cx| {
            if let Some(drain) = host.shutdown_storage() {
                drain.detach();
            }
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
            storage: test_storage(cx),
            storage_owners: HashMap::new(),
            storage_stopping: false,
            storage_callers: Vec::new(),
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
            _event_task: event_task,
            _release_subscription: release_subscription,
            startup_report: None,
            _startup_task: None,
            _personal_startup_task: None,
            personal_config_key: None,
            package_keys: HashMap::new(),
            next_extension_id: Arc::new(AtomicU64::new(1)),
        }
    }

    pub(crate) fn shutdown_storage(&mut self) -> Option<Task<()>> {
        if self.storage_stopping {
            return None;
        }
        self.storage_stopping = true;
        Some(
            match std::mem::replace(
                &mut self.storage,
                Err("application is shutting down".into()),
            ) {
                Ok(storage) => storage.shutdown(),
                Err(_) => Task::ready(()),
            },
        )
    }

    fn refresh_storage_callers(&mut self) {
        let callers = std::mem::take(&mut self.storage_callers);
        self.storage_callers = callers
            .into_iter()
            .filter(|(request, caller)| {
                let Some(caller) = caller.upgrade() else {
                    return false;
                };
                if !self.operation_is_live(
                    ExtensionKey::new(request.extension, request.lifecycle),
                    request.invocation,
                ) {
                    caller.store(false, Ordering::Release);
                    return false;
                }
                true
            })
            .collect();
    }

    #[cfg(test)]
    pub(crate) fn set_test_storage_root(&mut self, root: PathBuf, cx: &Context<Self>) {
        self.storage = Ok(super::extension_storage::ExtensionStorage::new(
            root,
            cx.background_executor().clone(),
        ));
    }

    #[cfg(test)]
    pub(crate) fn start_installed_extensions(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let _ = self.start_installed_extensions_with_completion(root, false, cx);
    }

    #[cfg(test)]
    pub(crate) fn start_product_extensions(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let _ = self.start_installed_extensions_with_completion(root, true, cx);
    }

    fn start_installed_extensions_with_completion(
        &mut self,
        root: PathBuf,
        include_bundled: bool,
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
        let discovery = cx.background_executor().spawn(async move {
            if include_bundled {
                extension_package::discover_all(&root)
            } else {
                extension_package::discover(&root)
            }
        });
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
                            this.update(cx, |host, _| {
                                let owner = package_owner(package)?;
                                host.package_keys.insert(package.manifest.name.clone(), key);
                                host.load_package(key, graph, Some(owner))
                            })
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

    /// Stop one package with a terminating unload, rediscover it (bundled
    /// sources, or its installed directory re-read from disk), and load a fresh
    /// lifecycle. The outcome replaces the package's startup report entry.
    /// Dependents keep running; they share only the global command namespace.
    pub(crate) fn restart_package(
        &mut self,
        name: String,
        cx: &mut Context<Self>,
    ) -> Task<LoadEntry> {
        let root = self
            .startup_report
            .as_ref()
            .filter(|snapshot| snapshot.state == StartupState::Complete)
            .map(|snapshot| snapshot.root.clone());
        let Some(root) = root else {
            return Task::ready(LoadEntry {
                directory: PathBuf::new(),
                name: Some(name),
                result: LoadResult::Failed("extension startup is still running".into()),
            });
        };
        if let Some(key) = self.package_keys.get(&name).copied() {
            let _ = self.pool.unload(key);
            self.remove_lifecycle(key, cx);
        }
        let discovery = cx
            .background_executor()
            .spawn(async move { extension_package::discover_all(&root) });
        let pool = self.pool.clone();
        let next_extension_id = self.next_extension_id.clone();
        cx.spawn(async move |this, cx| {
            let discovered = discovery.await;
            let entry = match discovered
                .packages
                .into_iter()
                .find(|package| package.manifest.name == name)
            {
                Some(package) => {
                    let id = next_extension_id.fetch_add(1, Ordering::Relaxed);
                    assert_ne!(id, u64::MAX, "extension identity exhausted");
                    let key = ExtensionKey::new(ExtensionId::new(id), ExtensionLifecycleId::new(1));
                    let execution = this
                        .update(cx, |host, _| {
                            if let Some(missing) = package
                                .manifest
                                .requires
                                .iter()
                                .find(|dependency| !host.package_keys.contains_key(*dependency))
                            {
                                return Err(format!("missing required extension {missing}"));
                            }
                            let graph = graph_for_package(&package)?;
                            let owner = package_owner(&package)?;
                            host.package_keys.insert(name.clone(), key);
                            host.load_package(key, graph, Some(owner))
                        })
                        .map_err(|error| error.to_string())
                        .and_then(|loaded| loaded);
                    let result = match execution {
                        Ok(execution) => execution
                            .await
                            .map(|_| ())
                            .map_err(|error| error.to_string()),
                        Err(cause) => Err(cause),
                    };
                    if result.is_err() {
                        let _ = pool.unload(key);
                        let _ = this.update(cx, |host, cx| host.remove_lifecycle(key, cx));
                    }
                    LoadEntry {
                        directory: package.directory,
                        name: Some(name),
                        result: match result {
                            Ok(()) => LoadResult::Loaded,
                            Err(cause) => LoadResult::Failed(cause),
                        },
                    }
                }
                None => {
                    let rejected = discovered
                        .diagnostics
                        .into_iter()
                        .find(|diagnostic| diagnostic.name.as_deref() == Some(name.as_str()));
                    LoadEntry {
                        directory: rejected
                            .as_ref()
                            .map(|diagnostic| diagnostic.directory.clone())
                            .unwrap_or_default(),
                        name: Some(name),
                        result: LoadResult::Failed(
                            rejected
                                .map(|diagnostic| diagnostic.cause)
                                .unwrap_or_else(|| "the package is no longer installed".into()),
                        ),
                    }
                }
            };
            match &entry.result {
                LoadResult::Loaded => {
                    eprintln!(
                        "[knot] extension restarted: {}",
                        entry.name.as_deref().unwrap_or_default()
                    )
                }
                LoadResult::Failed(cause) => eprintln!(
                    "[knot] extension restart failed: {}: {cause}",
                    entry.name.as_deref().unwrap_or_default()
                ),
            }
            let _ = this.update(cx, |host, cx| {
                if let Some(snapshot) = &mut host.startup_report {
                    let entries = &mut snapshot.report.entries;
                    match entries
                        .iter_mut()
                        .find(|existing| existing.name == entry.name)
                    {
                        Some(existing) => *existing = entry.clone(),
                        None => entries.push(entry.clone()),
                    }
                }
                cx.notify();
            });
            entry
        })
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
                                host.start_installed_extensions_with_completion(root, true, cx)
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
        self.storage_owners.insert(
            key,
            super::extension_storage::StorageOwner::personal_config(),
        );
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
        owner: Option<super::extension_storage::StorageOwner>,
    ) -> Result<ExtensionExecution, String> {
        let execution = self
            .pool
            .load_package(key, ExtensionConfig::default(), graph)
            .map_err(|error| error.to_string())?;
        self.admit_lifecycle(key);
        if let Some(owner) = owner {
            self.storage_owners.insert(key, owner);
        }
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
        self.load_package(key, graph, None)
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
            let execution = self.load_package(key, graph, None);
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
        #[cfg(test)]
        if self.hold_clipboard_writes && request.operation.descriptor.name == "writeClipboardText" {
            self.deferred_view_requests.push(request);
            return;
        }
        let operation = request.operation.clone();
        let context = handlers::HostContext::new(request.context(), cx);
        let pool = self.pool.clone();
        cx.spawn(async move |_, _| {
            let result = operation.run(context).await;
            let _ = pool.respond(crate::host::protocol::HostResponse {
                extension: request.extension,
                lifecycle: request.lifecycle,
                id: request.id,
                result: Ok(result),
            });
        })
        .detach();
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
            self.handle_request(request, cx);
        }
    }

    fn context_target(
        &self,
        request: &crate::host::protocol::RequestContext,
        explicit: Option<CommandInvocationId>,
        cx: &App,
    ) -> Result<&ProductCommandTarget, handlers::AccessError> {
        use handlers::AccessError;
        let root = self.commands.authorize_context(request, explicit)?;
        let target = self
            .command_targets
            .get(&root)
            .ok_or(AccessError::InvalidTarget)?;
        target
            .validate_tab(cx)
            .and_then(|()| target.validate_view(cx))
            .map_err(|_| AccessError::InvalidTarget)?;
        Ok(target)
    }

    fn contextual_buffer(
        &self,
        request: &crate::host::protocol::RequestContext,
        explicit: Option<CommandInvocationId>,
        cx: &App,
    ) -> Result<Option<crate::host::protocol::BufferHandle>, handlers::AccessError> {
        self.context_target(request, explicit, cx)?;
        Ok(self.commands.captured_buffer(request.invocation.unwrap()))
    }

    fn contextual_selected_text(
        &self,
        request: &crate::host::protocol::RequestContext,
        explicit: Option<CommandInvocationId>,
        cx: &App,
    ) -> Result<Option<String>, handlers::ContextError> {
        use handlers::{AccessError, ContextError};
        let target = self.context_target(request, explicit, cx)?;
        let (handler, name) = self
            .view_command_handlers
            .get(&request.invocation.unwrap())
            .ok_or(ContextError::UnsupportedOperation)?;
        let view = target.view.ok_or(ContextError::UnsupportedOperation)?;
        if self
            .commands
            .catalog()
            .borrow()
            .resolve_view(view, name.as_ref())
            != Some(*handler)
        {
            return Err(AccessError::InvalidTarget.into());
        }
        let Some(super::workbench::CommandView::Extension(extension)) = target.command_view(cx)
        else {
            return Err(AccessError::InvalidTarget.into());
        };
        if extension.instance_id(cx) != view {
            return Err(AccessError::InvalidTarget.into());
        }
        Ok(extension.selected_text(cx))
    }

    fn drain_command_events(&mut self, cx: &mut Context<Self>) {
        loop {
            let events = self.commands.drain_events().collect::<Vec<_>>();
            if events.is_empty() {
                break;
            }
            for event in events {
                match event {
                    ExtensionCommandEvent::CancelExtension {
                        extension,
                        lifecycle,
                        invocation,
                    } => {
                        self.cancel_transients(extension, lifecycle, Some(invocation), cx);
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
                                window: target.window.window_id().as_u64().to_string(),
                                view: target.view.map(|view| format!("{view:?}")),
                                tool: target.extension_tool_id(cx),
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
        self.refresh_storage_callers();
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
                                    window: fallback.window.clone(),
                                    view: fallback.view,
                                    parent_id: fallback.parent_id.clone(),
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
                                window: fallback.window.clone(),
                                view: fallback.view,
                                parent_id: fallback.parent_id.clone(),
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
                arguments,
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
                            arguments: arguments.clone(),
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
                    let Some(host) = this.upgrade() else {
                        return crate::host::protocol::CommandOutcome::Cancelled;
                    };
                    // A dead lifecycle cancels the command even when its frame
                    // was detached first; a live lifecycle with an invalid
                    // target is a distinct InvalidTarget result.
                    let host = host.read(cx);
                    if !host
                        .lifecycles
                        .contains(&ExtensionKey::new(handler.extension, handler.lifecycle))
                    {
                        return crate::host::protocol::CommandOutcome::Cancelled;
                    }
                    if let Err(invalid) = target.validate_view(cx) {
                        return invalid;
                    }
                    if target.view.is_none_or(|view| {
                        host.commands
                            .catalog()
                            .borrow()
                            .resolve_view(view, name.as_ref())
                            != Some(handler)
                    }) {
                        return crate::host::protocol::CommandOutcome::Cancelled;
                    }
                    outcome
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
            launched: cx.has_global::<ApplicationLaunched>(),
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

    fn publish_application_change(
        &mut self,
        change: crate::host::protocol::ApplicationChange,
        cx: &mut Context<Self>,
    ) {
        self.refresh_application_observation(cx);
        let event = self.application_observation.push(change);
        #[cfg(test)]
        self.application_events.push(event.clone());
        for (subscription, key) in self.application_observation.subscriptions() {
            let _ = self
                .pool
                .dispatch_application_change(key, subscription, event.clone());
        }
    }

    pub(crate) fn operation_is_live(
        &self,
        key: ExtensionKey,
        invocation: Option<CommandInvocationId>,
    ) -> bool {
        self.lifecycles.contains(&key) && !self.commands.is_cancelled(invocation)
    }

    fn document_destination(
        &self,
        request: &crate::host::protocol::RequestContext,
        target: Option<crate::host::protocol::DocumentDestination>,
        cx: &App,
    ) -> Result<crate::host::protocol::DocumentDestination, handlers::AccessError> {
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
        request: &crate::host::protocol::RequestContext,
        cx: &App,
    ) -> Result<crate::host::protocol::WindowTarget, handlers::AccessError> {
        use crate::host::protocol::WindowTarget;
        use handlers::AccessError;
        let window = if let Some(invocation) = request.invocation {
            let root = self
                .commands
                .root_of(invocation)
                .ok_or(AccessError::InvalidTarget)?;
            self.command_targets
                .get(&root)
                .ok_or(AccessError::InvalidTarget)?
                .window
        } else {
            cx.active_window().ok_or(AccessError::ContextUnavailable)?
        };
        if window.downcast::<ProductShell>().is_none() {
            return Err(AccessError::ContextUnavailable);
        }
        Ok(WindowTarget {
            window: window.window_id().as_u64().to_string(),
        })
    }

    #[cfg(test)]
    pub(crate) fn cancel_active_test_commands(&mut self, cx: &mut Context<Self>) {
        for invocation in self.command_targets.keys().copied().collect::<Vec<_>>() {
            self.commands.cancel(invocation);
        }
        self.drain_command_events(cx);
    }

    fn document_presentation(
        &mut self,
        id: super::documents::DocumentId,
        target: crate::host::protocol::PresentationTarget,
        cx: &mut Context<Self>,
    ) -> Result<crate::host::protocol::DocumentPresentation, handlers::OpenError> {
        use super::documents::ApplicationDocuments;
        use crate::host::protocol::{DocumentPresentation, PresentationInfo, SurfaceKind};
        let documents = cx.global::<ApplicationDocuments>().0.clone();
        let model = documents
            .read(cx)
            .get(id)
            .ok_or(handlers::OpenError::BufferClosed)?
            .model()
            .clone();
        let buffer = self.track_buffer(&model, cx);
        let document = documents
            .read(cx)
            .get(id)
            .ok_or(handlers::OpenError::BufferClosed)?;
        let info = application_document_info(document, buffer, cx);
        Ok(DocumentPresentation {
            presentation: PresentationInfo {
                target,
                kind: SurfaceKind::Document,
                document: Some(info.id.clone()),
                buffer: Some(buffer),
                tool: None,
            },
            document: info,
        })
    }

    pub(crate) fn register_view(
        &mut self,
        request: &crate::host::protocol::RequestContext,
        arguments: handlers::TreeArguments,
        cx: &mut Context<Self>,
    ) -> Result<
        crate::host::protocol::TreeProviderRegistrationId,
        super::extension_semantics::SemanticError,
    > {
        let id = arguments.id.clone();
        let registration = self
            .semantics
            .register_tree_provider(request, arguments, cx)?;
        self.commands.catalog().borrow_mut().declare_view_kind(&id);
        if cx.has_global::<super::keymaps::ApplicationKeymaps>() {
            let _ = cx
                .global_mut::<super::keymaps::ApplicationKeymaps>()
                .0
                .declare_extension_kind(&id);
        }
        cx.notify();
        Ok(registration)
    }

    pub(crate) fn unregister_view(
        &mut self,
        request: &crate::host::protocol::RequestContext,
        arguments: handlers::TreeRegistrationArguments,
        cx: &mut Context<Self>,
    ) -> Result<(), super::extension_semantics::SemanticError> {
        let id = self.semantics.owned_kind(arguments.registration, request)?;
        self.semantics
            .unregister_tree_provider(request, arguments, cx)?;
        self.remove_view_kind(&id, cx);
        cx.notify();
        Ok(())
    }

    fn remove_view_kind(&mut self, id: &str, cx: &mut App) {
        self.commands.catalog().borrow_mut().remove_view_kind(id);
        if cx.has_global::<super::keymaps::ApplicationKeymaps>()
            && cx
                .global_mut::<super::keymaps::ApplicationKeymaps>()
                .0
                .remove_extension_kind(id)
        {
            super::keymaps::rebuild(cx);
        }
    }

    fn transient_window(
        &self,
        request: &crate::host::protocol::RequestContext,
        explicit: Option<&str>,
        cx: &App,
    ) -> Result<gpui::AnyWindowHandle, handlers::AccessError> {
        let window = match (explicit, request.invocation) {
            (Some(window), _) => window.to_owned(),
            // An explicit target or an admitted command capture is required;
            // activation and context-free callbacks cannot borrow active focus.
            (None, Some(_)) => self.operation_window(request, cx)?.window,
            (None, None) => return Err(handlers::AccessError::ContextUnavailable),
        };
        product_window_handle(&window, cx).ok_or(handlers::AccessError::InvalidTarget)
    }

    /// Run one transient interaction install against the resolved product window.
    fn with_transient_window<R>(
        &self,
        request: &crate::host::protocol::RequestContext,
        explicit: Option<&str>,
        access: impl FnOnce(
            &mut ProductShell,
            &mut gpui::Window,
            &mut gpui::Context<ProductShell>,
        ) -> Result<R, handlers::TransientError>,
        cx: &mut Context<Self>,
    ) -> Result<R, handlers::TransientError> {
        let handle = self.transient_window(request, explicit, cx)?;
        cx.update_window(handle, |root, window, cx| {
            let Ok(shell) = root.downcast::<ProductShell>() else {
                return Err(handlers::TransientError::InvalidTarget);
            };
            shell.update(cx, |shell, cx| access(shell, window, cx))
        })
        .map_err(|_| handlers::TransientError::InvalidTarget)?
    }

    /// Settle any live transient interaction owned by the given extension
    /// lifetime, optionally narrowed to one command invocation.
    fn cancel_transients(
        &self,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
        invocation: Option<CommandInvocationId>,
        cx: &mut App,
    ) {
        let owner = TransientOwner {
            extension,
            lifecycle,
            invocation,
        };
        let handles = cx
            .windows()
            .into_iter()
            .filter(|handle| handle.downcast::<ProductShell>().is_some())
            .collect::<Vec<_>>();
        for handle in handles {
            let _ = cx.update_window(handle, |root, _window, cx| {
                if let Ok(shell) = root.downcast::<ProductShell>() {
                    shell.update(cx, |shell, cx| shell.cancel_transient(owner, cx));
                }
            });
        }
    }

    /// Show, hide, or focus a live registration in all product windows or in
    /// one explicit window. Programmatic presentation never publishes defaults.
    pub(crate) fn present_view(
        &mut self,
        request: &crate::host::protocol::RequestContext,
        registration: crate::host::protocol::TreeProviderRegistrationId,
        window: Option<&str>,
        action: super::extension_semantics::ViewPresentation,
        cx: &mut Context<Self>,
    ) -> Result<(), super::extension_semantics::SemanticError> {
        use handlers::AccessError;
        if !self
            .lifecycles
            .contains(&ExtensionKey::new(request.extension, request.lifecycle))
        {
            return Err(AccessError::Cancelled.into());
        }
        let id = self.semantics.owned_kind(registration, request)?;
        let handles = match window {
            Some(window) => {
                vec![product_window_handle(window, cx).ok_or(AccessError::InvalidTarget)?]
            }
            None => cx
                .windows()
                .into_iter()
                .filter(|handle| handle.downcast::<ProductShell>().is_some())
                .collect(),
        };
        for handle in handles {
            let _ = cx.update_window(handle, |root, window, cx| {
                let Ok(shell) = root.downcast::<ProductShell>() else {
                    return;
                };
                shell.update(cx, |shell, cx| match action {
                    super::extension_semantics::ViewPresentation::Show => {
                        shell.set_contributed_view_visible(&id, true, cx);
                    }
                    super::extension_semantics::ViewPresentation::Hide => {
                        shell.set_contributed_view_visible(&id, false, cx);
                    }
                    super::extension_semantics::ViewPresentation::Focus => {
                        shell.focus_contributed_view(&id, window, cx);
                    }
                });
            });
        }
        // A command-invoked presentation change is an explicit user arrangement,
        // so it becomes the fresh-window default. Activation and callbacks never
        // publish.
        if request.invocation.is_some() {
            self.publish_command_layout(request, cx);
        }
        Ok(())
    }

    /// Publish the invoking window's arrangement after a command-invoked show,
    /// hide, or focus.
    fn publish_command_layout(
        &self,
        request: &crate::host::protocol::RequestContext,
        cx: &mut Context<Self>,
    ) {
        let Ok(target) = self.operation_window(request, cx) else {
            return;
        };
        let Some(handle) = product_window_handle(&target.window, cx) else {
            return;
        };
        let _ = cx.update_window(handle, |root, _, cx| {
            if let Ok(shell) = root.downcast::<ProductShell>() {
                shell.update(cx, |shell, cx| shell.publish_tool_layout(cx));
            }
        });
    }

    fn remove_lifecycle(&mut self, key: ExtensionKey, cx: &mut Context<Self>) {
        self.remove_foreground_lifecycle(key, cx);
        self.drain_command_events(cx);
        cx.notify();
    }

    fn remove_foreground_lifecycle(&mut self, key: ExtensionKey, cx: &mut App) {
        if !self.lifecycles.remove(&key) {
            return;
        }
        self.package_keys.retain(|_, live| *live != key);
        self.cancel_transients(key.extension, key.lifecycle, None, cx);
        self.refresh_storage_callers();
        self.storage_owners.remove(&key);
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
        let removed_views = self
            .semantics
            .remove_lifecycle(key.extension, key.lifecycle, cx);
        for id in removed_views {
            self.remove_view_kind(&id, cx);
        }
    }
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
        },
        resource_uri: document.resource_uri().map(ToString::to_string),
        dirty: document.is_dirty(cx),
        revision: document.model().read(cx).revision(),
        buffer,
    }
}

fn product_window_handle(window: &str, cx: &App) -> Option<gpui::AnyWindowHandle> {
    cx.windows().into_iter().find(|handle| {
        handle.window_id().as_u64().to_string() == window
            && handle.downcast::<ProductShell>().is_some()
    })
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
    use gpui::TestAppContext;

    #[gpui::test]
    fn application_context_failures_have_distinct_results(cx: &mut TestAppContext) {
        use crate::host::protocol::{RequestContext, RequestId};
        cx.update(|cx| {
            let commands = cx.new(super::super::product_commands::ProductCommandDispatcher::new);
            cx.set_global(ApplicationProductCommands(commands));
            let host = install(cx);
            let key = ExtensionKey::new(ExtensionId::new(701), ExtensionLifecycleId::new(1));
            host.update(cx, |host, cx| {
                host.load(key, ExtensionConfig::default()).unwrap();
                let request = RequestContext {
                    extension: key.extension,
                    lifecycle: key.lifecycle,
                    id: RequestId::new(1),
                    invocation: None,
                };
                assert_eq!(
                    host.contextual_buffer(&request, None, cx),
                    Err(handlers::AccessError::ContextUnavailable)
                );
                assert!(matches!(
                    host.contextual_selected_text(&request, None, cx),
                    Err(handlers::ContextError::Access(
                        handlers::AccessError::ContextUnavailable
                    ))
                ));
                assert_eq!(
                    host.contextual_buffer(&request, Some(CommandInvocationId::new(1)), cx),
                    Err(handlers::AccessError::InvalidTarget)
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
                origin: extension_package::PackageOrigin::Installed,
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
                    && host.semantics.has_definition("@fixture/primary.outline")
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
            let definitions = host.semantics.definitions();
            assert_eq!(definitions.len(), 1);
            assert_eq!(definitions[0].id, "@fixture/primary.outline");
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
                assert!(host.semantics.definitions().is_empty());
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

    #[gpui::test]
    fn bundled_packages_load_with_product_discovery(cx: &mut TestAppContext) {
        use std::time::{Duration, Instant};

        let root = tempfile::tempdir().unwrap();
        let host = cx.update(|cx| {
            let commands = cx.new(super::super::product_commands::ProductCommandDispatcher::new);
            cx.set_global(ApplicationProductCommands(commands));
            let host = install(cx);
            host.update(cx, |host, cx| {
                host.set_test_storage_root(root.path().join("storage"), cx)
            });
            host
        });
        let completed = host.update(cx, |host, cx| {
            host.start_installed_extensions_with_completion(
                root.path().join("extensions"),
                true,
                cx,
            )
            .unwrap()
        });
        let (sender, receiver) = std::sync::mpsc::channel();
        cx.spawn(async move |_| {
            let _ = sender.send(completed.await);
        })
        .detach();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            cx.run_until_parked();
            if receiver.try_recv().is_ok() {
                break;
            }
            assert!(Instant::now() < deadline, "bundled startup did not finish");
            std::thread::yield_now();
        }
        cx.read(|cx| {
            let report = host.read(cx).startup_report.as_ref().unwrap();
            assert!(report.report.entries.iter().any(|entry| {
                entry.name.as_deref() == Some("@knot/projects")
                    && entry.result == LoadResult::Loaded
            }));
        });
    }
}

#[cfg(test)]
mod storage_tests;
