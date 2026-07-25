//! Knot's gpui application shell.
//!
//! The shell contains resizable explorer, editor, and outline panes plus a
//! status area for the active buffer and extension runtime.

use gpui::{prelude::FluentBuilder, *};
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use crate::host::{
    ExtensionRequestInbox, ExtensionRuntimeControl, ExtensionRuntimeParts, ExtensionRuntimeThread,
    V8Host,
    protocol::{
        BufferChange, CommandInvocation, CommandInvocationId, ExtensionId, HostOperation,
        HostRequest, HostRequestError, HostResponse, HostResponseValue, TreeChildrenResponse,
        TreeProviderError, TreeProviderRegistrationId,
    },
};

mod editor;
pub mod model;
mod tree_view;

use editor::{
    EditorContributionAction, EditorRenderingOptions, EditorView, seed_fixture_contributions,
};
use model::{
    BufferAccessError, BufferModel, BufferRegistry, BufferSubscriptionRegistry, CommandRegistry,
    ContributionError, ContributionSource,
};
use tree_view::{TreeProviderIdentity, TreeView, TreeViewEvent, TreeViewRegistrationError};

actions!(knot, [Quit]);

const MIN_PANE: f32 = 120.;
const DEFAULT_FIXTURE_NAME: &str = "rust_sample";
const RUNTIME_PROBE_SOURCE: &str = r#"
import { commands, editor, workbench } from "knot:editor";

globalThis.knotActiveBuffer = await editor.activeBuffer();
if (!globalThis.knotActiveBuffer) {
  throw new Error("Knot has no active editor buffer");
}
globalThis.knotFixtureEvents = [];
await globalThis.knotActiveBuffer.onDidChange((event) => {
  globalThis.knotFixtureEvents.push({
    beforeRevision: event.beforeRevision,
    revision: event.revision,
    edits: event.edits.length,
  });
});
await commands.register("knot.fixture.edit", async (context) => {
  if (!context.buffer) throw new Error("Knot has no active editor buffer");
  context.signal.addEventListener("abort", () => {
    globalThis.knotFixtureCommandAborted = true;
  });
  const snapshot = await context.buffer.snapshot();
  await context.buffer.applyEdits(
    [{ range: { startByteOffset: 0, endByteOffset: 0 }, text: "// command\n" }],
    { ifRevision: snapshot.revision },
  );
});
await commands.register("knot.fixture.contribution", async () => {
  globalThis.knotFixtureContributionActions =
    (globalThis.knotFixtureContributionActions ?? 0) + 1;
  await globalThis.knotActiveBuffer.contributions.dispose();
});
const contributionSnapshot = await globalThis.knotActiveBuffer.snapshot();
const contributionStartUtf16 = contributionSnapshot.text.indexOf("LEAF_MAX");
if (contributionStartUtf16 >= 0) {
  const contributionRange = {
    startByteOffset: contributionSnapshot.byteOffsetAtUtf16(contributionStartUtf16),
    endByteOffset: contributionSnapshot.byteOffsetAtUtf16(
      contributionStartUtf16 + "LEAF_MAX".length,
    ),
  };
  await globalThis.knotActiveBuffer.contributions.replace([{
    range: contributionRange,
    decoration: "warning",
    gutter: "warning",
    command: "knot.fixture.contribution",
  }], { ifRevision: contributionSnapshot.revision });
  await globalThis.knotActiveBuffer.contributions.replace([{
    range: contributionRange,
    decoration: "error",
    gutter: "error",
    command: "knot.fixture.contribution",
  }], { ifRevision: contributionSnapshot.revision });
}
globalThis.knotFixtureTreeRegistration =
  await workbench.registerTreeDataProvider("outline", {
  async getChildren(parentId) {
    if (parentId === null) {
      await globalThis.__knotFixtureDelay(150);
    }
    if (parentId === "failure") {
      throw new Error("expected fixture tree failure");
    }
    if (parentId === null) return [
      { id: "shell", label: "struct Shell", icon: "symbol", collapsibleState: "expanded" },
      { id: "render", label: "impl Render", icon: "symbol", collapsibleState: "collapsed" },
      { id: "main", label: "fn main()", icon: "symbol", collapsibleState: "none",
        command: "knot.fixture.edit" },
    ];
    if (parentId === "shell") return [
      { id: "shell.editor", label: "editor", description: "EditorView", collapsibleState: "none" },
      { id: "shell.buffers", label: "buffer_registry", description: "BufferRegistry", collapsibleState: "none" },
      { id: "shell.extensions", label: "extension_controls", description: "runtime owners", collapsibleState: "none" },
    ];
    if (parentId === "render") return [
      { id: "render.render", label: "render()", collapsibleState: "none" },
    ];
    return [];
  },
  });
"#;

fn map_buffer_error(error: BufferAccessError) -> HostRequestError {
    match error {
        BufferAccessError::Closed => HostRequestError::BufferClosed,
        BufferAccessError::InvalidRange => HostRequestError::InvalidRange,
        BufferAccessError::InvalidEditBatch => HostRequestError::InvalidEditBatch,
        BufferAccessError::RevisionConflict => HostRequestError::RevisionConflict,
    }
}

fn map_command_error(error: model::CommandRegistryError) -> HostRequestError {
    match error {
        model::CommandRegistryError::NameInUse => HostRequestError::CommandNameInUse,
        model::CommandRegistryError::NotFound => HostRequestError::CommandNotFound,
    }
}

fn map_contribution_error(error: ContributionError) -> HostRequestError {
    match error {
        ContributionError::Closed => HostRequestError::BufferClosed,
        ContributionError::InvalidRange => HostRequestError::InvalidRange,
        ContributionError::RevisionConflict => HostRequestError::RevisionConflict,
        ContributionError::NotFound => HostRequestError::ContributionSetNotFound,
    }
}

fn map_tree_error(error: TreeViewRegistrationError) -> HostRequestError {
    match error {
        TreeViewRegistrationError::WrongView => HostRequestError::TreeViewNotFound,
        TreeViewRegistrationError::ProviderInUse => HostRequestError::TreeProviderInUse,
        TreeViewRegistrationError::ProviderNotFound => HostRequestError::TreeProviderNotFound,
    }
}

/// Drag value carried by the active drag while a pane divider is being dragged.
/// `which` identifies which divider (0 = left/center, 1 = center/right).
#[derive(Clone, Copy)]
struct DividerDrag {
    which: usize,
}

/// Invisible drag-ghost view gpui renders while the drag is in progress.
/// Required by `on_drag`'s constructor; we don't want a visible ghost.
struct DragGhost;

impl Render for DragGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

struct Shell {
    left_files: Vec<SharedString>,
    left_selected: Option<usize>,
    outline: Entity<TreeView>,
    left_width: f32,
    right_width: f32,
    /// `(which, start_x, start_width)` captured on the first drag-move event
    /// of an in-progress divider drag; cleared on drop.
    drag_origin: Option<(usize, Pixels, f32)>,
    /// Each view is an independent presentation over the active shared model.
    editor: Entity<EditorView>,
    secondary_editor: Entity<EditorView>,
    buffer_registry: BufferRegistry,
    buffer_subscriptions: BufferSubscriptionRegistry,
    command_registry: CommandRegistry,
    next_tree_registration: u64,
    next_command_invocation: u64,
    active_command: Option<CommandInvocationId>,
    cancelled_commands: HashSet<CommandInvocationId>,
    command_state: SharedString,
    heartbeat: u64,
    runtime_state: SharedString,
    latest_runtime_error: Option<SharedString>,
    extension_controls: HashMap<
        (ExtensionId, crate::host::protocol::ExtensionLifecycleId),
        ExtensionRuntimeControl,
    >,
    runtime_threads: Vec<ExtensionRuntimeThread>,
    background_executor: BackgroundExecutor,
    _runtime_bridge_tasks: Vec<Task<()>>,
    _runtime_execution_task: Task<()>,
    _model_subscription: Subscription,
    _editor_action_subscriptions: Vec<Subscription>,
    _tree_view_subscription: Subscription,
    _heartbeat_task: Task<()>,
    /// Name of the fixture currently loaded (shown in a thin status header
    /// above the editor so it's visible at a glance which fixture is running).
    fixture_name: String,
}

impl Shell {
    /// Construct the shell with the default fixture.
    fn new(runtime: ExtensionRuntimeParts, cx: &mut Context<Self>) -> Self {
        Self::new_with_runtimes(vec![runtime], cx)
    }

    fn new_with_runtimes(runtimes: Vec<ExtensionRuntimeParts>, cx: &mut Context<Self>) -> Self {
        Self::new_with_runtimes_and_fixture(runtimes, DEFAULT_FIXTURE_NAME.into(), cx)
    }

    /// Construct the shell with an explicitly selected fixture. Fixture
    /// resolution is relative to the crate root so the binary runs from any
    /// current working directory.
    fn new_with_runtimes_and_fixture(
        runtimes: Vec<ExtensionRuntimeParts>,
        fixture_name: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let fixture_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("bench/fixtures")
            .join(format!("{fixture_name}.kfx"));
        let fixture = crate::view::fixture::Fixture::load(&fixture_path).unwrap_or_else(|e| {
            eprintln!("[knot] failed to load fixture {fixture_path:?}: {e}");
            crate::view::fixture::Fixture::from_lines(vec![format!(
                "(no fixture at {fixture_path:?}: {e})"
            )])
        });
        let text = fixture.lines.join("\n");
        let (extension, lifecycle) = runtimes
            .first()
            .expect("Knot shell requires one extension runtime")
            .control
            .identity();
        let model = cx.new(|_| {
            let mut model = BufferModel::from_text(text);
            seed_fixture_contributions(
                &mut model,
                ContributionSource::Extension {
                    extension,
                    lifecycle,
                },
            );
            model
        });
        let mut buffer_registry = BufferRegistry::new();
        let handle = buffer_registry.open(&model);
        buffer_registry.set_active(Some(handle));
        let editor = cx.new(|cx| EditorView::from_fixture(&fixture, model.clone(), cx));
        let secondary_editor = cx.new(|cx| {
            EditorView::from_fixture_with_options(
                &fixture,
                model.clone(),
                1,
                EditorRenderingOptions {
                    show_gutter_markers: false,
                },
                cx,
            )
        });
        let editor_action_subscription = cx.subscribe(
            &editor,
            |this, _editor, action: &EditorContributionAction, cx| {
                this.invoke_contribution_action(action, cx);
            },
        );
        let secondary_editor_action_subscription = cx.subscribe(
            &secondary_editor,
            |this, _editor, action: &EditorContributionAction, cx| {
                this.invoke_contribution_action(action, cx);
            },
        );
        let outline = cx.new(|cx| TreeView::new("outline", cx));
        let tree_view_subscription =
            cx.subscribe(&outline, |this, _tree, event: &TreeViewEvent, cx| {
                this.dispatch_tree_view_event(event.clone(), cx);
            });
        let model_subscription = cx.observe(&model, |this, model, cx| {
            this.publish_model_change(model, cx);
            cx.notify();
        });
        let mut runtimes = runtimes.into_iter();
        let ExtensionRuntimeParts {
            control: runtime_control,
            requests,
            thread: runtime_thread,
        } = runtimes
            .next()
            .expect("Knot shell requires one extension runtime");
        let mut extension_controls = HashMap::new();
        extension_controls.insert(runtime_control.identity(), runtime_control.clone());
        let mut runtime_bridge_tasks = vec![Self::spawn_runtime_bridge(
            requests,
            runtime_control.clone(),
            cx,
        )];
        let mut runtime_threads = vec![runtime_thread];
        for ExtensionRuntimeParts {
            control,
            requests,
            thread,
        } in runtimes
        {
            extension_controls.insert(control.identity(), control.clone());
            runtime_bridge_tasks.push(Self::spawn_runtime_bridge(requests, control, cx));
            runtime_threads.push(thread);
        }
        let runtime_execution = runtime_control.execute_fixture_module(
            "file:///fixtures/gpui-runtime-probe.js",
            RUNTIME_PROBE_SOURCE,
        );
        let runtime_execution_task = cx.spawn(async move |this, cx| {
            let result = runtime_execution.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.runtime_state = if this.outline.read(cx).is_loading() {
                            "tree-loading".into()
                        } else {
                            "running".into()
                        };
                        this.latest_runtime_error = None;
                    }
                    Err(error) => {
                        this.runtime_state = "failed".into();
                        this.latest_runtime_error = Some(format!("{error:?}").into());
                    }
                }
                cx.notify();
            });
        });
        let heartbeat_task = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                if this
                    .update(cx, |this, cx| {
                        this.heartbeat = this.heartbeat.wrapping_add(1);
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Shell {
            left_files: vec![
                "src/lib.rs".into(),
                "src/main.rs".into(),
                "src/app/mod.rs".into(),
                "src/app/editor.rs".into(),
                "src/app/model.rs".into(),
                "docs/roadmap.md".into(),
                "docs/design.md".into(),
                "docs/step3-framework-comparison.md".into(),
                "Cargo.toml".into(),
                "AGENTS.md".into(),
            ],
            left_selected: Some(2),
            outline,
            left_width: 260.,
            right_width: 220.,
            drag_origin: None,
            editor,
            secondary_editor,
            buffer_registry,
            buffer_subscriptions: BufferSubscriptionRegistry::new(),
            command_registry: CommandRegistry::new(),
            next_tree_registration: 1,
            next_command_invocation: 1,
            active_command: None,
            cancelled_commands: HashSet::new(),
            command_state: "idle".into(),
            heartbeat: 0,
            runtime_state: "starting".into(),
            latest_runtime_error: None,
            extension_controls,
            runtime_threads,
            background_executor: cx.background_executor().clone(),
            _runtime_bridge_tasks: runtime_bridge_tasks,
            _runtime_execution_task: runtime_execution_task,
            _model_subscription: model_subscription,
            _editor_action_subscriptions: vec![
                editor_action_subscription,
                secondary_editor_action_subscription,
            ],
            _tree_view_subscription: tree_view_subscription,
            _heartbeat_task: heartbeat_task,
            fixture_name,
        }
    }

    fn spawn_runtime_bridge(
        mut requests: ExtensionRequestInbox,
        control: ExtensionRuntimeControl,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let mut failure = None;
            while let Some(request) = requests.receive().await {
                let response =
                    match this.update(cx, |this, cx| this.dispatch_host_request(request, cx)) {
                        Ok(response) => response,
                        Err(_) => return,
                    };
                if let Err(error) = control.respond(response) {
                    failure = Some(format!("runtime response failed: {error:?}"));
                    break;
                }
            }
            let (extension, lifecycle) = control.identity();
            let _ = this.update(cx, |this, cx| {
                this.extension_controls.remove(&(extension, lifecycle));
                this.command_registry.remove_lifecycle(extension, lifecycle);
                this.buffer_subscriptions
                    .remove_lifecycle(extension, lifecycle);
                this.buffer_registry
                    .remove_contribution_lifecycle(extension, lifecycle, cx);
                this.outline.update(cx, |tree, cx| {
                    tree.remove_lifecycle(extension, lifecycle, cx);
                });
                this.runtime_state = "closed".into();
                if let Some(error) = failure {
                    this.latest_runtime_error = Some(error.into());
                }
                cx.notify();
            });
        })
    }

    fn dispatch_host_request(
        &mut self,
        request: HostRequest,
        cx: &mut Context<Self>,
    ) -> HostResponse {
        let cancelled = request
            .invocation
            .is_some_and(|invocation| self.cancelled_commands.contains(&invocation));
        let lifecycle_live = self
            .extension_controls
            .contains_key(&(request.extension, request.lifecycle));
        let result = match request.operation {
            _ if cancelled || !lifecycle_live => Err(HostRequestError::Cancelled),
            HostOperation::ActiveBuffer => Ok(HostResponseValue::ActiveBuffer(
                self.buffer_registry.active_handle(),
            )),
            HostOperation::Snapshot { buffer, range } => self
                .buffer_registry
                .resolve(buffer)
                .map_err(|_| HostRequestError::BufferClosed)
                .and_then(|model| {
                    model
                        .read_with(cx, |model, _| model.snapshot(range))
                        .map_err(map_buffer_error)
                })
                .map(HostResponseValue::Snapshot),
            HostOperation::ApplyEdits {
                buffer,
                edits,
                if_revision,
            } => self
                .buffer_registry
                .resolve(buffer)
                .map_err(|_| HostRequestError::BufferClosed)
                .and_then(|model| {
                    model
                        .update(cx, |model, cx| {
                            if model.apply_edits(&edits, if_revision)? {
                                cx.notify();
                            }
                            Ok(())
                        })
                        .map_err(map_buffer_error)
                })
                .map(|()| {
                    let revision = self
                        .buffer_registry
                        .resolve(buffer)
                        .expect("buffer remains open after its own edit")
                        .read_with(cx, |model, _| model.revision());
                    HostResponseValue::AppliedEdits { revision }
                }),
            HostOperation::RegisterCommand { name } => self
                .command_registry
                .register(name, request.extension, request.lifecycle)
                .map(|registration| HostResponseValue::CommandRegistered { registration })
                .map_err(map_command_error),
            HostOperation::UnregisterCommand { registration } => self
                .command_registry
                .unregister(registration, request.extension, request.lifecycle)
                .map(|()| HostResponseValue::CommandUnregistered { registration })
                .map_err(map_command_error),
            HostOperation::SubscribeBufferChanges { buffer } => self
                .buffer_registry
                .resolve(buffer)
                .map_err(|_| HostRequestError::BufferClosed)
                .map(|_| HostResponseValue::BufferChangesSubscribed {
                    subscription: self.buffer_subscriptions.subscribe(
                        buffer,
                        request.extension,
                        request.lifecycle,
                    ),
                }),
            HostOperation::UnsubscribeBufferChanges { subscription } => {
                if self.buffer_subscriptions.unsubscribe(
                    subscription,
                    request.extension,
                    request.lifecycle,
                ) {
                    Ok(HostResponseValue::BufferChangesUnsubscribed { subscription })
                } else {
                    Err(HostRequestError::BufferClosed)
                }
            }
            HostOperation::ReplaceEditorContributions {
                buffer,
                contributions,
                if_revision,
            } => self
                .buffer_registry
                .resolve(buffer)
                .map_err(|_| HostRequestError::BufferClosed)
                .and_then(|model| {
                    model
                        .update(cx, |model, cx| {
                            model.replace_contributions(
                                ContributionSource::Extension {
                                    extension: request.extension,
                                    lifecycle: request.lifecycle,
                                },
                                &contributions,
                                if_revision,
                            )?;
                            cx.notify();
                            Ok(())
                        })
                        .map_err(map_contribution_error)
                })
                .map(|()| HostResponseValue::EditorContributionsReplaced),
            HostOperation::DisposeEditorContributions { buffer } => self
                .buffer_registry
                .resolve(buffer)
                .map_err(|_| HostRequestError::BufferClosed)
                .and_then(|model| {
                    model
                        .update(cx, |model, cx| {
                            let result =
                                model.dispose_contributions(ContributionSource::Extension {
                                    extension: request.extension,
                                    lifecycle: request.lifecycle,
                                });
                            match result {
                                Ok(()) => cx.notify(),
                                Err(ContributionError::NotFound) => {}
                                Err(error) => return Err(error),
                            }
                            Ok(())
                        })
                        .map_err(map_contribution_error)
                })
                .map(|()| HostResponseValue::EditorContributionsDisposed),
            HostOperation::RegisterTreeProvider { view_id } => {
                let registration = TreeProviderRegistrationId::new(self.next_tree_registration);
                let result = self.outline.update(cx, |tree, cx| {
                    tree.register_provider(
                        &view_id,
                        TreeProviderIdentity {
                            extension: request.extension,
                            lifecycle: request.lifecycle,
                            registration,
                        },
                        cx,
                    )
                });
                result
                    .map(|()| {
                        self.next_tree_registration = self
                            .next_tree_registration
                            .checked_add(1)
                            .expect("tree registration space exhausted");
                        HostResponseValue::TreeProviderRegistered { registration }
                    })
                    .map_err(map_tree_error)
            }
            HostOperation::InvalidateTreeProvider {
                registration,
                parent_id,
            } => {
                if !self.outline.read(cx).owns_provider(
                    registration,
                    request.extension,
                    request.lifecycle,
                ) {
                    Err(HostRequestError::TreeProviderNotFound)
                } else {
                    self.outline
                        .update(cx, |tree, cx| tree.invalidate(registration, parent_id, cx))
                        .map(|()| HostResponseValue::TreeProviderInvalidated)
                        .map_err(map_tree_error)
                }
            }
            HostOperation::UnregisterTreeProvider { registration } => {
                if !self.outline.read(cx).owns_provider(
                    registration,
                    request.extension,
                    request.lifecycle,
                ) {
                    Err(HostRequestError::TreeProviderNotFound)
                } else {
                    self.outline
                        .update(cx, |tree, cx| tree.unregister_provider(registration, cx))
                        .map(|()| HostResponseValue::TreeProviderUnregistered { registration })
                        .map_err(map_tree_error)
                }
            }
        };

        HostResponse {
            extension: request.extension,
            lifecycle: request.lifecycle,
            id: request.id,
            result,
        }
    }

    fn publish_buffer_change(&self, change: BufferChange) {
        for subscription in self.buffer_subscriptions.for_buffer(change.buffer) {
            if let Some(control) = self
                .extension_controls
                .get(&(subscription.extension, subscription.lifecycle))
            {
                let _ = control.dispatch_buffer_change(subscription.id, change.clone());
            }
        }
    }

    fn dispatch_tree_view_event(&mut self, event: TreeViewEvent, cx: &mut Context<Self>) {
        match event {
            TreeViewEvent::RequestChildren { provider, request } => {
                let Some(control) = self
                    .extension_controls
                    .get(&(provider.extension, provider.lifecycle))
                    .cloned()
                else {
                    return;
                };
                let outline = self.outline.clone();
                let failed_request = request.clone();
                let callback = control.request_tree_children(request);
                cx.spawn(async move |this, cx| {
                    let response = callback.await.unwrap_or_else(|error| TreeChildrenResponse {
                        registration: failed_request.registration,
                        parent_id: failed_request.parent_id,
                        generation: failed_request.generation,
                        result: Err(TreeProviderError {
                            message: format!("{error:?}"),
                        }),
                    });
                    let done_loading = outline
                        .update(cx, |tree, cx| {
                            tree.apply_response(response, cx);
                            !tree.is_loading()
                        })
                        .unwrap_or(false);
                    if done_loading {
                        let _ = this.update(cx, |this, cx| {
                            if this.runtime_state == "tree-loading" {
                                this.runtime_state = "running".into();
                                cx.notify();
                            }
                        });
                    }
                })
                .detach();
            }
            TreeViewEvent::InvokeCommand { provider, command } => {
                let Ok(target) = self.command_registry.resolve(&command) else {
                    return;
                };
                if target.extension == provider.extension && target.lifecycle == provider.lifecycle
                {
                    self.invoke_command_target(target, cx);
                }
            }
        }
    }

    #[allow(
        dead_code,
        reason = "buffer closing is not exposed by the prototype shell yet"
    )]
    fn close_buffer(
        &mut self,
        buffer: crate::host::protocol::BufferHandle,
        cx: &mut Context<Self>,
    ) {
        self.buffer_registry
            .close(buffer, cx)
            .expect("buffer is open");
        self.buffer_subscriptions.remove_buffer(buffer);
    }

    fn publish_model_change(&mut self, model: Entity<BufferModel>, cx: &mut Context<Self>) {
        let Some(buffer) = self.buffer_registry.active_handle() else {
            return;
        };
        let active = match self.buffer_registry.resolve(buffer) {
            Ok(active) => active,
            Err(_) => return,
        };
        if active != model {
            return;
        }
        while let Some(change) = model.update(cx, |model, _| model.take_pending_change()) {
            self.publish_buffer_change(BufferChange {
                buffer,
                before_revision: change.before_revision,
                revision: change.revision,
                edits: change.edits,
            });
        }
    }

    fn invoke_fixture_command(&mut self, cx: &mut Context<Self>) {
        if self.active_command.is_some() {
            return;
        }
        let target = match self.command_registry.resolve("knot.fixture.edit") {
            Ok(target) => target,
            Err(_) => {
                self.command_state = "unavailable".into();
                cx.notify();
                return;
            }
        };
        self.invoke_command_target(target, cx);
    }

    fn invoke_contribution_action(
        &mut self,
        action: &EditorContributionAction,
        cx: &mut Context<Self>,
    ) {
        let ContributionSource::Extension {
            extension,
            lifecycle,
        } = action.source
        else {
            return;
        };
        let Some(buffer) = self.buffer_registry.active_handle() else {
            return;
        };
        let Ok(model) = self.buffer_registry.resolve(buffer) else {
            return;
        };
        if !model.read_with(cx, |model, _| {
            model.has_contribution_action(action.source, &action.command)
        }) {
            return;
        }
        let Ok(target) = self.command_registry.resolve(&action.command) else {
            return;
        };
        if target.extension != extension || target.lifecycle != lifecycle {
            return;
        }
        self.invoke_command_target(target, cx);
    }

    fn invoke_command_target(&mut self, target: model::CommandTarget, cx: &mut Context<Self>) {
        if self.active_command.is_some() {
            return;
        }
        let Some(control) = self
            .extension_controls
            .get(&(target.extension, target.lifecycle))
            .cloned()
        else {
            self.command_state = "unavailable".into();
            cx.notify();
            return;
        };
        let id = self.allocate_command_invocation();
        self.active_command = Some(id);
        self.command_state = "running".into();
        let execution = control.invoke_command(
            CommandInvocation {
                id,
                registration: target.registration,
                extension: target.extension,
                lifecycle: target.lifecycle,
            },
            self.buffer_registry.active_handle(),
        );
        cx.spawn(async move |this, cx| {
            let result = execution.await;
            let _ = this.update(cx, |this, cx| {
                this.finish_command(id, result.is_ok(), cx);
            });
        })
        .detach();
        cx.notify();
    }

    fn allocate_command_invocation(&mut self) -> CommandInvocationId {
        let id = CommandInvocationId::new(self.next_command_invocation);
        self.next_command_invocation = self
            .next_command_invocation
            .checked_add(1)
            .expect("command invocation space exhausted");
        id
    }

    fn finish_command(
        &mut self,
        invocation: CommandInvocationId,
        succeeded: bool,
        cx: &mut Context<Self>,
    ) {
        if self.active_command != Some(invocation) {
            return;
        }
        self.active_command = None;
        self.command_state = if self.cancelled_commands.remove(&invocation) {
            "cancelled".into()
        } else if succeeded {
            "completed".into()
        } else {
            "failed".into()
        };
        cx.notify();
    }

    fn cancel_active_command(&mut self, cx: &mut Context<Self>) {
        if let Some(invocation) = self.active_command {
            self.cancelled_commands.insert(invocation);
            self.command_state = "cancelling".into();
            cx.notify();
        }
    }

    /// Build a single selectable row: a div wrapping `label`; clicking selects it,
    /// the selected row gets a highlight bg, all rows get a hover bg.
    fn row(
        id_prefix: &'static str,
        ix: usize,
        label: SharedString,
        selected: Option<usize>,
        entity: Entity<Shell>,
    ) -> AnyElement {
        let is_selected = selected == Some(ix);
        div()
            .id((id_prefix, ix))
            .w_full()
            .px_2()
            .py_1()
            .text_sm()
            .child(label)
            .when(is_selected, |d| {
                d.bg(rgb(0x2a4a7a)).text_color(rgb(0xffffff))
            })
            .hover(|s| s.bg(rgb(0x222222)))
            .on_click(move |_ev, _window, cx| {
                entity.update(cx, |s, cx| {
                    s.left_selected = Some(ix);
                    cx.notify();
                });
            })
            .into_any_element()
    }

    /// A pane that hosts a selectable single-row list.
    fn pane(
        &self,
        id_prefix: &'static str,
        items: &[SharedString],
        selected: Option<usize>,
        entity: Entity<Shell>,
    ) -> impl IntoElement {
        let items: Vec<SharedString> = items.to_vec();
        uniform_list(id_prefix, items.len(), move |range, _window, _cx| {
            range
                .map(|ix| Self::row(id_prefix, ix, items[ix].clone(), selected, entity.clone()))
                .collect()
        })
        .h_full()
    }

    /// A draggable pane-divider handle. The handle itself does no resize math;
    /// the root listens for `DividerDrag` drag-move/drop events.
    fn divider(which: usize) -> impl IntoElement {
        div()
            .id(("divider", which))
            .w(px(6.))
            .h_full()
            .bg(rgb(0x333333))
            .cursor_ew_resize()
            .on_drag(DividerDrag { which }, |_value, _offset, _window, cx| {
                cx.new(|_| DragGhost)
            })
    }
}

impl Drop for Shell {
    fn drop(&mut self) {
        for control in self.extension_controls.values() {
            control.request_shutdown();
        }
        for thread in self.runtime_threads.drain(..) {
            self.background_executor
                .spawn(async move { thread.shutdown() })
                .detach();
        }
    }
}

impl Render for Shell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let left_selected = self.left_selected;
        let left_files = self.left_files.clone();
        let revision = self
            .buffer_registry
            .active_handle()
            .and_then(|handle| self.buffer_registry.resolve(handle).ok())
            .map(|model| model.read(cx).revision().to_string())
            .unwrap_or_else(|| "closed".into());
        let runtime_error = self
            .latest_runtime_error
            .clone()
            .unwrap_or_else(|| "none".into());

        div()
            .flex()
            .flex_row()
            .size_full()
            .bg(rgb(0x1e1e1e))
            .text_color(rgb(0xd4d4d4))
            // Drag resize listeners on the root: on_drag_move fires (capture phase)
            // for every move while a DividerDrag is active, regardless of mouse
            // position, so the root — not the handle — owns the resize math.
            .on_drag_move::<DividerDrag>(cx.listener(
                |this, event: &DragMoveEvent<DividerDrag>, _window, cx| {
                    let which = event.drag(cx).which;
                    let pos_x = event.event.position.x;
                    let (_, start_x, start_w) = match this.drag_origin {
                        Some(o) => o,
                        None => {
                            let sw = if which == 0 {
                                this.left_width
                            } else {
                                this.right_width
                            };
                            let o = (which, pos_x, sw);
                            this.drag_origin = Some(o);
                            o
                        }
                    };
                    let delta = f32::from(pos_x - start_x);
                    let new_w = if which == 0 {
                        start_w + delta
                    } else {
                        start_w - delta
                    }
                    .max(MIN_PANE);
                    if which == 0 {
                        this.left_width = new_w;
                    } else {
                        this.right_width = new_w;
                    }
                    cx.notify();
                },
            ))
            .on_drop::<DividerDrag>(cx.listener(|this, _value: &DividerDrag, _window, cx| {
                this.drag_origin = None;
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .h_full()
                    .w(px(self.left_width))
                    .bg(rgb(0x252526))
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .text_xs()
                            .text_color(rgb(0x888888))
                            .child("EXPLORER"),
                    )
                    .child(self.pane("left", &left_files, left_selected, entity.clone())),
            )
            .child(Self::divider(0))
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .flex()
                    .flex_col()
                    .bg(rgb(0x1e1e1e))
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .text_xs()
                            .text_color(rgb(0x888888))
                            .bg(rgb(0x252526))
                            .child(self.fixture_name.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_row()
                            .child(
                                div()
                                    .flex_1()
                                    .h_full()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .px_2()
                                            .py_1()
                                            .text_xs()
                                            .text_color(rgb(0x777777))
                                            .child("VIEW A · GUTTER ON"),
                                    )
                                    .child(div().flex_1().child(self.editor.clone())),
                            )
                            .child(div().w(px(1.)).h_full().bg(rgb(0x3a3a3a)))
                            .child(
                                div()
                                    .flex_1()
                                    .h_full()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .px_2()
                                            .py_1()
                                            .text_xs()
                                            .text_color(rgb(0x777777))
                                            .child("VIEW B · GUTTER OFF"),
                                    )
                                    .child(div().flex_1().child(self.secondary_editor.clone())),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_4()
                            .px_2()
                            .py_1()
                            .text_xs()
                            .text_color(rgb(0x9d9d9d))
                            .bg(rgb(0x252526))
                            .child(format!("revision {revision}"))
                            .child(format!("runtime {}", self.runtime_state))
                            .child(format!("command {}", self.command_state))
                            .child(
                                div()
                                    .id("run-fixture-command")
                                    .cursor_pointer()
                                    .text_color(rgb(0x80c0ff))
                                    .child("run command")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.invoke_fixture_command(cx);
                                    })),
                            )
                            .child(
                                div()
                                    .id("cancel-fixture-command")
                                    .cursor_pointer()
                                    .text_color(rgb(0xffb080))
                                    .child("cancel command")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.cancel_active_command(cx);
                                    })),
                            )
                            .child(format!("error {runtime_error}"))
                            .child(format!("heartbeat {}", self.heartbeat)),
                    ),
            )
            .child(Self::divider(1))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .h_full()
                    .w(px(self.right_width))
                    .bg(rgb(0x252526))
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .text_xs()
                            .text_color(rgb(0x888888))
                            .child("OUTLINE"),
                    )
                    .child(self.outline.clone()),
            )
    }
}

pub fn run() {
    let fixture_name = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_FIXTURE_NAME.into());
    let runtime = V8Host::new()
        .spawn_extension(ExtensionId::new(1))
        .into_parts();

    Application::new().run(move |app: &mut App| {
        app.on_action(|_action: &Quit, app: &mut App| app.quit());

        app.key_bindings()
            .borrow_mut()
            .add_bindings([KeyBinding::new("cmd-q", Quit, None)]);

        let bounds = Bounds::centered(None, size(px(1200.), px(800.)), app);
        app.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_window, cx| {
                cx.new(|cx| Shell::new_with_runtimes_and_fixture(vec![runtime], fixture_name, cx))
            },
        )
        .unwrap();

        app.set_menus(vec![Menu {
            name: "Knot".into(),
            items: vec![MenuItem::action("Quit Knot", Quit)],
        }]);
    });
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use gpui::{AppContext, Entity, Focusable, TestAppContext};

    use super::{ContributionSource, Shell, editor::EditorContributionAction};
    use crate::host::{
        ExtensionRuntimeControl, ExtensionRuntimeExecutionError, V8Host,
        protocol::{
            ByteRange, CommandInvocation, CommandInvocationId, DecorationToken, ExtensionId,
            GutterToken, HostOperation, HostRequest, HostRequestError, RequestId, TextEdit,
        },
    };

    async fn wait_for_runtime_state(
        shell: &Entity<Shell>,
        expected: &str,
        cx: &mut TestAppContext,
    ) {
        while cx.read(|cx| shell.read(cx).runtime_state != expected) {
            shell.next_notification(Duration::ZERO, cx).await;
        }
    }

    async fn wait_for_command_state(
        shell: &Entity<Shell>,
        expected: &str,
        cx: &mut TestAppContext,
    ) {
        while cx.read(|cx| shell.read(cx).command_state != expected) {
            shell.next_notification(Duration::ZERO, cx).await;
        }
    }

    async fn wait_for_tree_error(shell: &Entity<Shell>, cx: &mut TestAppContext) {
        let outline = cx.read(|cx| shell.read(cx).outline.clone());
        while cx.read(|cx| outline.read(cx).lifecycle_state().3 == 0) {
            outline.next_notification(Duration::ZERO, cx).await;
        }
    }

    fn only_runtime_control(
        shell: &Entity<Shell>,
        cx: &mut TestAppContext,
    ) -> ExtensionRuntimeControl {
        cx.read(|cx| {
            shell
                .read(cx)
                .extension_controls
                .values()
                .next()
                .cloned()
                .expect("single-runtime shell has its control")
        })
    }

    fn runtime_control(
        shell: &Entity<Shell>,
        extension: ExtensionId,
        cx: &mut TestAppContext,
    ) -> ExtensionRuntimeControl {
        cx.read(|cx| {
            shell
                .read(cx)
                .extension_controls
                .values()
                .find(|control| control.identity().0 == extension)
                .cloned()
                .expect("shell has the requested runtime control")
        })
    }

    #[gpui::test]
    async fn foreground_bridge_resolves_the_displayed_active_buffer(cx: &mut TestAppContext) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(7))
            .into_parts();
        let shell = cx.new(|cx| Shell::new(runtime, cx));
        wait_for_runtime_state(&shell, "running", cx).await;
        let control = only_runtime_control(&shell, cx);
        let execution = control.execute_fixture_script(
            "verify-gpui-active-buffer.js",
            r#"
                if (!globalThis.knotActiveBuffer) {
                    throw new Error("gpui did not return its active buffer");
                }
            "#,
        );

        execution.await.unwrap();

        let (resolved, displayed) = cx.read(|cx| {
            let shell = shell.read(cx);
            let handle = shell.buffer_registry.active_handle().unwrap();
            let resolved = shell.buffer_registry.resolve(handle).unwrap();
            let displayed = shell.editor.read(cx).model().clone();
            (resolved, displayed)
        });
        assert_eq!(resolved, displayed);
    }

    #[gpui::test]
    async fn extension_buffer_proxy_reads_and_edits_the_displayed_model(cx: &mut TestAppContext) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(9))
            .into_parts();
        let shell = cx.new(|cx| Shell::new(runtime, cx));
        wait_for_runtime_state(&shell, "running", cx).await;
        let control = only_runtime_control(&shell, cx);

        control
            .execute_fixture_module(
                "file:///fixtures/buffer-proxy.js",
                r#"
                    import { editor } from "knot:editor";

                    const buffer = await editor.activeBuffer();
                    const snapshot = await buffer.snapshot();
                    const start = snapshot.byteOffsetAtUtf16(0);
                    await buffer.applyEdits(
                        [{ range: { startByteOffset: start, endByteOffset: start }, text: "// extension\n" }],
                        { ifRevision: snapshot.revision },
                    );
                    const afterComment = await buffer.snapshot();
                    await buffer.applyEdits(
                        [{ range: { startByteOffset: 0, endByteOffset: 0 }, text: "😀中e\u0301" }],
                        { ifRevision: afterComment.revision },
                    );
                    const unicode = await buffer.snapshot({ startByteOffset: 0, endByteOffset: 10 });
                    if (unicode.byteOffsetAtUtf16(2) !== 4 || unicode.byteOffsetAtUtf16(3) !== 7) {
                        throw new Error("unexpected UTF-16 to byte conversion");
                    }
                    if (unicode.utf16OffsetAtByte(7) !== 3) {
                        throw new Error("unexpected byte to UTF-16 conversion");
                    }
                    for (const invalid of [
                        () => unicode.byteOffsetAtUtf16(1),
                        () => unicode.utf16OffsetAtByte(1),
                    ]) {
                        try { invalid(); throw new Error("accepted split boundary"); } catch (error) {
                            if (!(error instanceof RangeError)) throw error;
                        }
                    }
                    globalThis.extensionRevision = unicode.revision;
                "#,
            )
            .await
            .unwrap();

        let (text, revision) = cx.read(|cx| {
            let shell = shell.read(cx);
            let handle = shell.buffer_registry.active_handle().unwrap();
            let model = shell.buffer_registry.resolve(handle).unwrap();
            model.read_with(cx, |model, _| (model.text(), model.revision()))
        });
        assert!(text.starts_with("😀中é// extension\n"));
        assert_eq!(revision, 2);
    }

    #[gpui::test]
    async fn extension_contributions_replace_invoke_and_dispose(cx: &mut TestAppContext) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(10))
            .into_parts();
        let identity = runtime.control.identity();
        let shell = cx.new(|cx| Shell::new(runtime, cx));
        wait_for_runtime_state(&shell, "running", cx).await;
        let control = only_runtime_control(&shell, cx);

        control
            .execute_fixture_module(
                "file:///fixtures/editor-contributions.js",
                r#"
                    import { commands, editor } from "knot:editor";

                    const buffer = await editor.activeBuffer();
                    const snapshot = await buffer.snapshot();
                    await commands.register("knot.fixture.test-contribution", async () => {
                      globalThis.contributionActions = (globalThis.contributionActions ?? 0) + 1;
                      await buffer.contributions.dispose();
                    });
                    await buffer.contributions.replace([{
                      range: { startByteOffset: 0, endByteOffset: 2 },
                      decoration: "warning",
                      gutter: "warning",
                      command: "knot.fixture.test-contribution",
                    }], { ifRevision: snapshot.revision });
                    await buffer.contributions.replace([{
                      range: { startByteOffset: 0, endByteOffset: 2 },
                      decoration: "error",
                      gutter: "error",
                      command: "knot.fixture.test-contribution",
                    }], { ifRevision: snapshot.revision });
                    for (const [replacement, revision, expected] of [
                      [{ range: { startByteOffset: 0, endByteOffset: 2 }, decoration: "info" },
                       snapshot.revision + 1, "RevisionConflictError"],
                      [{ range: { startByteOffset: 0, endByteOffset: 999999 }, decoration: "info" },
                       snapshot.revision, "RangeError"],
                    ]) {
                      try {
                        await buffer.contributions.replace([replacement], { ifRevision: revision });
                        throw new Error(`accepted invalid replacement for ${expected}`);
                      } catch (error) {
                        if (error.name !== expected) throw error;
                      }
                    }
                "#,
            )
            .await
            .unwrap();

        cx.read(|cx| {
            let shell = shell.read(cx);
            let model = shell.editor.read(cx).model().read(cx);
            let contributions = model.resolved_contributions();
            assert!(
                contributions
                    .iter()
                    .any(|contribution| contribution.decoration == Some(DecorationToken::Error))
            );
            assert!(
                contributions
                    .iter()
                    .any(|contribution| contribution.gutter == Some(GutterToken::Error))
            );
            assert!(contributions.iter().any(|contribution| {
                contribution.command.as_deref() == Some("knot.fixture.test-contribution")
            }));
        });

        shell.update(cx, |shell, cx| {
            shell.invoke_contribution_action(
                &EditorContributionAction {
                    command: "knot.fixture.test-contribution".into(),
                    source: ContributionSource::Extension {
                        extension: identity.0,
                        lifecycle: identity.1,
                    },
                },
                cx,
            );
        });
        wait_for_command_state(&shell, "completed", cx).await;

        cx.read(|cx| {
            let shell = shell.read(cx);
            let model = shell.editor.read(cx).model().read(cx);
            let contributions = model.resolved_contributions();
            assert!(
                contributions
                    .iter()
                    .all(|contribution| contribution.gutter.is_none())
            );
            assert!(
                contributions
                    .iter()
                    .all(|contribution| contribution.command.is_none())
            );
        });
        shell.update(cx, |shell, cx| {
            shell.invoke_contribution_action(
                &EditorContributionAction {
                    command: "knot.fixture.test-contribution".into(),
                    source: ContributionSource::Extension {
                        extension: identity.0,
                        lifecycle: identity.1,
                    },
                },
                cx,
            );
        });
        control
            .execute_fixture_script(
                "verify-contribution-action.js",
                "if (globalThis.contributionActions !== 1) throw new Error('action not received')",
            )
            .await
            .unwrap();
    }

    #[gpui::test]
    async fn buffer_changes_fan_out_in_order_and_survive_listener_failures(
        cx: &mut TestAppContext,
    ) {
        let host = V8Host::new();
        let shell = cx.new(|cx| {
            Shell::new_with_runtimes(
                vec![
                    host.spawn_extension(ExtensionId::new(12)).into_parts(),
                    host.spawn_extension(ExtensionId::new(13)).into_parts(),
                ],
                cx,
            )
        });
        wait_for_runtime_state(&shell, "running", cx).await;
        let controls = cx.read(|cx| {
            shell
                .read(cx)
                .extension_controls
                .values()
                .cloned()
                .collect::<Vec<_>>()
        });
        assert_eq!(controls.len(), 2);

        for control in &controls {
            control
                .execute_fixture_module(
                    format!("file:///fixtures/subscriber-{}.js", control.identity().0.value()),
                    r#"
                        import { editor } from "knot:editor";
                        const buffer = await editor.activeBuffer();
                        globalThis.disposable = await buffer.onDidChange((event) => {
                          globalThis.events = [...(globalThis.events ?? []), `${event.revision}:${event.edits.length}`];
                          if (event.revision === 1 && globalThis.failFirst) {
                            throw new Error("expected listener failure");
                          }
                        });
                    "#,
                )
                .await
                .unwrap();
        }
        controls[1]
            .execute_fixture_script("fail-first-listener.js", "globalThis.failFirst = true")
            .await
            .unwrap();

        shell.update(cx, |shell, cx| {
            let handle = shell.buffer_registry.active_handle().unwrap();
            let model = shell.buffer_registry.resolve(handle).unwrap();
            assert!(model.update(cx, |model, cx| {
                let changed = model
                    .apply_edits(
                        &[
                            TextEdit {
                                range: ByteRange {
                                    start_byte_offset: 0,
                                    end_byte_offset: 0,
                                },
                                text: "A".into(),
                            },
                            TextEdit {
                                range: ByteRange {
                                    start_byte_offset: 1,
                                    end_byte_offset: 1,
                                },
                                text: "B".into(),
                            },
                        ],
                        model.revision(),
                    )
                    .unwrap();
                cx.notify();
                changed
            }));
            model.update(cx, |model, cx| {
                assert!(model.replace(0..1, "C"));
                cx.notify();
            });
        });
        cx.run_until_parked();

        for control in &controls {
            control
                .execute_fixture_script(
                    "verify-buffer-change-events.js",
                    "if (globalThis.events.join(',') !== '1:2,2:1') throw new Error(`missing or unordered buffer changes: ${globalThis.events}`)",
                )
                .await
                .unwrap();
        }

        controls[0]
            .execute_fixture_script(
                "dispose-buffer-listener.js",
                "globalThis.disposable.dispose()",
            )
            .await
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            cx.read(|cx| {
                shell
                    .read(cx)
                    .buffer_subscriptions
                    .for_buffer(shell.read(cx).buffer_registry.active_handle().unwrap())
                    .count()
            }),
            // The first extension owns the shell fixture subscription; the
            // remaining explicit listener belongs to the second extension.
            2
        );

        let handle = cx.read(|cx| shell.read(cx).buffer_registry.active_handle().unwrap());
        shell.update(cx, |shell, cx| shell.close_buffer(handle, cx));
        assert!(cx.read(|cx| {
            shell
                .read(cx)
                .buffer_subscriptions
                .for_buffer(handle)
                .next()
                .is_none()
        }));
    }

    #[gpui::test]
    async fn fixture_command_edits_the_displayed_buffer_without_blocking_the_bridge(
        cx: &mut TestAppContext,
    ) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(10))
            .into_parts();
        let shell = cx.new(|cx| Shell::new(runtime, cx));
        wait_for_runtime_state(&shell, "running", cx).await;
        let heartbeat = cx.read(|cx| shell.read(cx).heartbeat);
        shell.update(cx, |shell, cx| shell.invoke_fixture_command(cx));
        wait_for_command_state(&shell, "completed", cx).await;

        let text = cx.read(|cx| {
            let shell = shell.read(cx);
            let handle = shell.buffer_registry.active_handle().unwrap();
            shell
                .buffer_registry
                .resolve(handle)
                .unwrap()
                .read_with(cx, |model, _| model.text())
        });
        assert!(text.starts_with("// command\n"));

        only_runtime_control(&shell, cx)
            .execute_fixture_script(
                "verify-fixture-command-event.js",
                "if (globalThis.knotFixtureEvents.length !== 1 || globalThis.knotFixtureEvents[0].beforeRevision !== 0 || globalThis.knotFixtureEvents[0].revision !== 1 || globalThis.knotFixtureEvents[0].edits !== 1) throw new Error(`unexpected fixture event: ${JSON.stringify(globalThis.knotFixtureEvents)}`)",
            )
            .await
            .unwrap();
        cx.executor().advance_clock(Duration::from_millis(500));
        cx.run_until_parked();
        assert!(cx.read(|cx| shell.read(cx).heartbeat) > heartbeat);
    }

    #[gpui::test]
    async fn slow_tree_provider_does_not_block_foreground_work(cx: &mut TestAppContext) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(11))
            .into_parts();
        let (shell, cx) = cx.add_window_view(|_, cx| Shell::new(runtime, cx));
        wait_for_runtime_state(&shell, "tree-loading", cx).await;

        let (heartbeat, paint_count) = cx.read(|cx| {
            let shell = shell.read(cx);
            (
                shell.heartbeat,
                shell.editor.read(cx).responsiveness_state().2,
            )
        });
        cx.update(|window, cx| {
            shell
                .read(cx)
                .editor
                .read(cx)
                .focus_handle(cx)
                .focus(window);
        });
        cx.run_until_parked();
        cx.simulate_input("x");
        cx.simulate_keystrokes("pagedown");
        cx.executor().advance_clock(Duration::from_millis(500));
        cx.refresh().unwrap();
        cx.run_until_parked();

        cx.read(|cx| {
            let shell = shell.read(cx);
            let editor = shell.editor.read(cx);
            let (cursor_line, scroll, paints) = editor.responsiveness_state();
            assert!(shell.outline.read(cx).is_loading());
            assert!(shell.heartbeat > heartbeat);
            assert!(editor.model().read(cx).text().starts_with('x'));
            assert!(cursor_line > 0);
            assert!(scroll > 0.);
            assert!(paints > paint_count);
        });

        wait_for_runtime_state(&shell, "running", cx).await;
    }

    #[gpui::test]
    async fn fixture_command_runs_on_its_registered_extension(cx: &mut TestAppContext) {
        let host = V8Host::new();
        let shell = cx.new(|cx| {
            Shell::new_with_runtimes(
                vec![
                    host.spawn_extension(ExtensionId::new(14)).into_parts(),
                    host.spawn_extension(ExtensionId::new(15)).into_parts(),
                ],
                cx,
            )
        });
        wait_for_runtime_state(&shell, "running", cx).await;
        let (first, second) = cx.read(|cx| {
            let shell = shell.read(cx);
            let mut controls = shell.extension_controls.values().cloned();
            let first = controls
                .find(|control| control.identity().0 == ExtensionId::new(14))
                .unwrap();
            let second = shell
                .extension_controls
                .values()
                .find(|control| control.identity().0 == ExtensionId::new(15))
                .cloned()
                .unwrap();
            (first, second)
        });
        let (first_extension, first_lifecycle) = first.identity();
        shell.update(cx, |shell, _| {
            shell
                .command_registry
                .remove_lifecycle(first_extension, first_lifecycle);
        });
        second
            .execute_fixture_module(
                "file:///fixtures/second-owner-command.js",
                r#"
                    import { commands } from "knot:editor";
                    await commands.register("knot.fixture.edit", () => {
                      globalThis.ranOnSecondExtension = true;
                    });
                "#,
            )
            .await
            .unwrap();

        shell.update(cx, |shell, cx| shell.invoke_fixture_command(cx));
        wait_for_command_state(&shell, "completed", cx).await;
        second
            .execute_fixture_script(
                "verify-second-owner-command.js",
                "if (!globalThis.ranOnSecondExtension) throw new Error('command ran on the wrong extension')",
            )
            .await
            .unwrap();
    }

    #[gpui::test]
    async fn cancelling_before_the_awaited_command_request_prevents_its_edit(
        cx: &mut TestAppContext,
    ) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(11))
            .into_parts();
        let shell = cx.new(|cx| Shell::new(runtime, cx));
        wait_for_runtime_state(&shell, "running", cx).await;
        let initial_text = cx.read(|cx| {
            let shell = shell.read(cx);
            let handle = shell.buffer_registry.active_handle().unwrap();
            shell
                .buffer_registry
                .resolve(handle)
                .unwrap()
                .read_with(cx, |model, _| model.text())
        });

        shell.update(cx, |shell, cx| {
            shell.invoke_fixture_command(cx);
            shell.cancel_active_command(cx);
        });
        wait_for_command_state(&shell, "cancelled", cx).await;

        only_runtime_control(&shell, cx)
            .execute_fixture_script(
                "verify-command-abort.js",
                "if (!globalThis.knotFixtureCommandAborted) throw new Error('command signal was not aborted')",
            )
            .await
            .unwrap();

        let text = cx.read(|cx| {
            let shell = shell.read(cx);
            let handle = shell.buffer_registry.active_handle().unwrap();
            shell
                .buffer_registry
                .resolve(handle)
                .unwrap()
                .read_with(cx, |model, _| model.text())
        });
        assert_eq!(text, initial_text);
    }

    #[gpui::test]
    async fn cancelled_late_edit_request_cannot_mutate_the_displayed_buffer(
        cx: &mut TestAppContext,
    ) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(16))
            .into_parts();
        let shell = cx.new(|cx| Shell::new(runtime, cx));
        wait_for_runtime_state(&shell, "running", cx).await;
        let control = only_runtime_control(&shell, cx);
        let (extension, lifecycle) = control.identity();
        let invocation = CommandInvocationId::new(99);

        let response = shell.update(cx, |shell, cx| {
            let buffer = shell.buffer_registry.active_handle().unwrap();
            let revision = shell
                .buffer_registry
                .resolve(buffer)
                .unwrap()
                .read_with(cx, |model, _| model.revision());
            shell.cancelled_commands.insert(invocation);
            shell.dispatch_host_request(
                HostRequest {
                    extension,
                    lifecycle,
                    id: RequestId::new(1),
                    invocation: Some(invocation),
                    operation: HostOperation::ApplyEdits {
                        buffer,
                        edits: vec![TextEdit {
                            range: ByteRange {
                                start_byte_offset: 0,
                                end_byte_offset: 0,
                            },
                            text: "// late command\n".into(),
                        }],
                        if_revision: revision,
                    },
                },
                cx,
            )
        });
        assert_eq!(response.result, Err(HostRequestError::Cancelled));
        let text = cx.read(|cx| {
            let shell = shell.read(cx);
            let buffer = shell.buffer_registry.active_handle().unwrap();
            shell
                .buffer_registry
                .resolve(buffer)
                .unwrap()
                .read_with(cx, |model, _| model.text())
        });
        assert!(!text.starts_with("// late command\n"));
    }

    #[gpui::test]
    async fn failed_tree_owner_is_cleaned_up_without_affecting_its_neighbor(
        cx: &mut TestAppContext,
    ) {
        let host = V8Host::new();
        let shell = cx.new(|cx| {
            Shell::new_with_runtimes(
                vec![
                    host.spawn_extension(ExtensionId::new(17)).into_parts(),
                    host.spawn_extension(ExtensionId::new(18)).into_parts(),
                ],
                cx,
            )
        });
        wait_for_runtime_state(&shell, "running", cx).await;
        let (tree_owner, neighbor) = cx.read(|cx| {
            let shell = shell.read(cx);
            let controls = shell.extension_controls.values();
            let tree_owner = controls
                .clone()
                .find(|control| control.identity().0 == ExtensionId::new(17))
                .cloned()
                .unwrap();
            let neighbor = controls
                .clone()
                .find(|control| control.identity().0 == ExtensionId::new(18))
                .cloned()
                .unwrap();
            (tree_owner, neighbor)
        });
        let owner = tree_owner.identity();

        tree_owner
            .execute_fixture_script(
                "fail-tree-callback.js",
                "globalThis.knotFixtureTreeRegistration.invalidate('failure')",
            )
            .await
            .unwrap();
        wait_for_tree_error(&shell, cx).await;
        assert_eq!(
            cx.read(|cx| shell.read(cx).outline.read(cx).lifecycle_state()),
            (true, 3, false, 1)
        );

        let execution = tree_owner.execute_fixture_script("runaway.js", "while (true) {}");
        tree_owner.watchdog().terminate().unwrap();
        assert_eq!(
            execution.await,
            Err(ExtensionRuntimeExecutionError::Terminated)
        );
        wait_for_runtime_state(&shell, "closed", cx).await;

        cx.read(|cx| {
            let shell = shell.read(cx);
            assert_eq!(
                shell.outline.read(cx).lifecycle_state(),
                (false, 0, false, 0)
            );
            let model = shell.editor.read(cx).model().read(cx);
            assert!(model.resolved_contributions().iter().all(|contribution| {
                contribution.source
                    != ContributionSource::Extension {
                        extension: owner.0,
                        lifecycle: owner.1,
                    }
            }));
        });

        neighbor
            .execute_fixture_script(
                "neighbor-remains-usable.js",
                "globalThis.neighborAlive = true",
            )
            .await
            .unwrap();
        neighbor
            .execute_fixture_script(
                "verify-neighbor-remains-usable.js",
                "if (!globalThis.neighborAlive) throw new Error('neighbor did not run')",
            )
            .await
            .unwrap();

        let heartbeat = cx.read(|cx| shell.read(cx).heartbeat);
        cx.executor().advance_clock(Duration::from_millis(500));
        cx.run_until_parked();
        assert!(cx.read(|cx| shell.read(cx).heartbeat) > heartbeat);
    }

    #[gpui::test]
    async fn extension_failures_are_isolated_together_through_gpui(cx: &mut TestAppContext) {
        let host = V8Host::new();
        let shell = cx.new(|cx| {
            Shell::new_with_runtimes(
                (20..24)
                    .map(|id| host.spawn_extension(ExtensionId::new(id)).into_parts())
                    .collect(),
                cx,
            )
        });
        wait_for_runtime_state(&shell, "running", cx).await;
        let neighbor = runtime_control(&shell, ExtensionId::new(20), cx);
        let command_runtime = runtime_control(&shell, ExtensionId::new(21), cx);
        let failed_startup = runtime_control(&shell, ExtensionId::new(22), cx);
        let runaway = runtime_control(&shell, ExtensionId::new(23), cx);

        let startup_error = failed_startup
            .execute_fixture_module(
                "file:///fixtures/failing-startup.js",
                r#"
                    import { editor } from "knot:editor";
                    await editor.activeBuffer();
                    throw new Error("expected startup failure");
                "#,
            )
            .await
            .unwrap_err();
        assert!(matches!(
            startup_error,
            ExtensionRuntimeExecutionError::JavaScriptException { ref report }
                if report.contains("expected startup failure")
        ));
        failed_startup.request_shutdown();

        command_runtime
            .execute_fixture_module(
                "file:///fixtures/failing-commands.js",
                r#"
                    import { commands, editor } from "knot:editor";
                    globalThis.closedBuffer = await editor.activeBuffer();
                    await commands.register("knot.fixture.throw", () => {
                      throw new Error("expected thrown handler");
                    });
                    await commands.register("knot.fixture.reject", async () => {
                      await Promise.resolve();
                      throw new Error("expected rejected handler");
                    });
                    globalThis.disposedCommand = await commands.register(
                      "knot.fixture.disposed",
                      () => {},
                    );
                "#,
            )
            .await
            .unwrap();

        for (name, expected) in [
            ("knot.fixture.throw", "expected thrown handler"),
            ("knot.fixture.reject", "expected rejected handler"),
        ] {
            let (target, active_buffer) = cx.read(|cx| {
                let shell = shell.read(cx);
                (
                    shell.command_registry.resolve(name).unwrap(),
                    shell.buffer_registry.active_handle(),
                )
            });
            let error = command_runtime
                .invoke_command(
                    CommandInvocation {
                        id: CommandInvocationId::new(if name.ends_with("throw") {
                            100
                        } else {
                            101
                        }),
                        registration: target.registration,
                        extension: target.extension,
                        lifecycle: target.lifecycle,
                    },
                    active_buffer,
                )
                .await
                .unwrap_err();
            assert!(matches!(
                error,
                ExtensionRuntimeExecutionError::JavaScriptException { ref report }
                    if report.contains(expected)
            ));
        }

        command_runtime
            .execute_fixture_script("dispose-command.js", "globalThis.disposedCommand.dispose()")
            .await
            .unwrap();
        cx.run_until_parked();
        assert!(cx.read(|cx| {
            shell
                .read(cx)
                .command_registry
                .resolve("knot.fixture.disposed")
                .is_err()
        }));

        let initial_text = cx.read(|cx| {
            let shell = shell.read(cx);
            let buffer = shell.buffer_registry.active_handle().unwrap();
            shell
                .buffer_registry
                .resolve(buffer)
                .unwrap()
                .read_with(cx, |model, _| model.text())
        });
        shell.update(cx, |shell, cx| {
            shell.invoke_fixture_command(cx);
            shell.cancel_active_command(cx);
        });
        wait_for_command_state(&shell, "cancelled", cx).await;
        assert_eq!(
            cx.read(|cx| {
                let shell = shell.read(cx);
                let buffer = shell.buffer_registry.active_handle().unwrap();
                shell
                    .buffer_registry
                    .resolve(buffer)
                    .unwrap()
                    .read_with(cx, |model, _| model.text())
            }),
            initial_text
        );

        let buffer = cx.read(|cx| shell.read(cx).buffer_registry.active_handle().unwrap());
        shell.update(cx, |shell, cx| shell.close_buffer(buffer, cx));
        command_runtime
            .execute_fixture_script(
                "closed-buffer.js",
                r#"
                    (async () => {
                      try {
                        await globalThis.closedBuffer.snapshot();
                        throw new Error("closed buffer operation succeeded");
                      } catch (error) {
                        if (error.name !== "BufferClosedError") throw error;
                      }
                    })()
                "#,
            )
            .await
            .unwrap();

        // A queued script does not establish that the runtime thread has
        // attached its isolate, which the watchdog requires.
        runaway
            .execute_fixture_script("integrated-runaway-ready.js", "void 0")
            .await
            .unwrap();
        let runaway_execution =
            runaway.execute_fixture_script("integrated-runaway.js", "while (true) {}");
        runaway.watchdog().terminate().unwrap();
        assert_eq!(
            runaway_execution.await,
            Err(ExtensionRuntimeExecutionError::Terminated)
        );

        neighbor
            .execute_fixture_script(
                "integrated-neighbor.js",
                "globalThis.integratedNeighborAlive = true",
            )
            .await
            .unwrap();
        neighbor
            .execute_fixture_script(
                "verify-integrated-neighbor.js",
                "if (!globalThis.integratedNeighborAlive) throw new Error('neighbor stopped')",
            )
            .await
            .unwrap();
        let heartbeat = cx.read(|cx| shell.read(cx).heartbeat);
        cx.executor().advance_clock(Duration::from_millis(500));
        cx.run_until_parked();
        assert!(cx.read(|cx| shell.read(cx).heartbeat) > heartbeat);
    }

    #[gpui::test]
    async fn heartbeat_progresses_before_and_after_the_runtime_bridge_closes(
        cx: &mut TestAppContext,
    ) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(8))
            .into_parts();
        let shell = cx.new(|cx| Shell::new(runtime, cx));

        wait_for_runtime_state(&shell, "running", cx).await;
        let first = cx.read(|cx| shell.read(cx).heartbeat);
        cx.executor().advance_clock(Duration::from_millis(500));
        cx.run_until_parked();
        let second = cx.read(|cx| shell.read(cx).heartbeat);
        assert!(second > first);

        let control = only_runtime_control(&shell, cx);
        control.request_shutdown();
        wait_for_runtime_state(&shell, "closed", cx).await;

        cx.executor().advance_clock(Duration::from_millis(500));
        cx.run_until_parked();
        let third = cx.read(|cx| shell.read(cx).heartbeat);
        assert!(third > second);
    }
}
