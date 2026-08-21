//! Knot's gpui application shell.
//!
//! The shell contains resizable explorer, editor, and outline panes plus a
//! status area for the active buffer and extension runtime.

use gpui::{prelude::FluentBuilder, *};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::Duration,
};

use crate::host::{
    ExtensionRequestInbox, ExtensionRuntimeControl, ExtensionRuntimeParts, ExtensionRuntimeThread,
    V8Host,
    protocol::{
        BufferChange, BufferHandle, ByteRange, Command, CommandArgumentValue, CommandInvocation,
        CommandInvocationId, CommandOutcome, ExtensionId, HostOperation, HostRequest,
        HostRequestError, HostResponse, HostResponseValue, TreeChildrenResponse, TreeProviderError,
        TreeProviderRegistrationId,
    },
};

mod command_palette;
mod editor;
pub mod model;
mod open_buffers;
mod search_results;
mod terminal_view;
mod tree_view;

use command_palette::{CommandPalette, CommandPaletteEntry, CommandPaletteEvent};
use editor::{
    EditorContributionAction, EditorRenderingOptions, EditorView, seed_fixture_contributions,
};
use model::{
    BufferAccessError, BufferModel, BufferRegistry, BufferSubscriptionRegistry, CommandCatalog,
    ContributionError, ContributionSource,
};
use open_buffers::{OpenBufferCollection, OpenBufferId};
use search_results::{ACTIVATE_SEARCH_RESULT_COMMAND, SearchResultsController};
use terminal_view::TerminalView;
use tree_view::{TreeProviderIdentity, TreeView, TreeViewEvent, TreeViewRegistrationError};

actions!(
    knot,
    [
        Quit,
        ToggleActiveKeymap,
        ActivateTransientKeymap,
        CancelTransientKeymap
    ]
);

const MIN_PANE: f32 = 120.;
const MIN_TERMINAL_HEIGHT: f32 = 96.;
const MIN_EDITOR_HEIGHT: f32 = 160.;
const DEFAULT_FIXTURE_NAME: &str = "rust_sample";
const DIAGNOSTIC_COMMAND: &str = "knot.diagnostic.command-context";
const FIXTURE_EDIT_COMMAND: &str = "knot.fixture.edit";
const FIXTURE_EDIT_ARGUMENT: &str = "// palette command\n";
const DIAGNOSTIC_BINDING_ARGUMENT: &str = "keybinding.diagnostic";
const MULTI_KEY_DIAGNOSTIC_ARGUMENT: &str = "keybinding.multi-keystroke-diagnostic";
const EDITOR_KEY_CONTEXT: &str = "editor";
const TREE_KEY_CONTEXT: &str = "tree";
const TERMINAL_KEY_CONTEXT: &str = "terminal";
const PALETTE_KEY_CONTEXT: &str = "palette";
const BASE_KEYMAP_CONTEXT: &str = "keymap";
const ACTIVE_KEYMAP_CONTEXT: &str = "active_keymap";
const TRANSIENT_KEYMAP_CONTEXT: &str = "transient_keymap";
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
  const text = typeof context.arguments === "string"
    ? context.arguments
    : "// command\n";
  await context.buffer.applyEdits(
    [{ range: { startByteOffset: 0, endByteOffset: 0 }, text }],
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
        BufferAccessError::ReadOnly => HostRequestError::UnsupportedOperation,
        BufferAccessError::InvalidRange => HostRequestError::InvalidRange,
        BufferAccessError::InvalidEditBatch => HostRequestError::InvalidEditBatch,
        BufferAccessError::RevisionConflict => HostRequestError::RevisionConflict,
    }
}

fn map_command_error(error: model::CommandCatalogError) -> HostRequestError {
    match error {
        model::CommandCatalogError::NameInUse => HostRequestError::CommandNameInUse,
        model::CommandCatalogError::NotFound => HostRequestError::CommandNotFound,
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

#[derive(Clone, Copy)]
struct TerminalDividerDrag;

#[derive(Clone, Eq, PartialEq)]
struct CommandOrigin {
    window: AnyWindowHandle,
    focus: WeakFocusHandle,
    buffer: Option<BufferHandle>,
}

#[derive(Clone, Eq, PartialEq)]
struct CapturedInvocationContext {
    window: AnyWindowHandle,
    workspace: WeakEntity<Shell>,
    focus: WeakFocusHandle,
    buffer: Option<BufferHandle>,
}

struct CommandDispatchCompletionState {
    claimed: bool,
    sender: Option<tokio::sync::oneshot::Sender<CommandOutcome>>,
}

#[derive(Clone)]
struct CommandDispatchCompletion {
    caller: (ExtensionId, crate::host::protocol::ExtensionLifecycleId),
    state: Arc<Mutex<CommandDispatchCompletionState>>,
}

impl PartialEq for CommandDispatchCompletion {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }
}

impl CommandDispatchCompletion {
    fn new(
        caller: (ExtensionId, crate::host::protocol::ExtensionLifecycleId),
    ) -> (Self, tokio::sync::oneshot::Receiver<CommandOutcome>) {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        (
            Self {
                caller,
                state: Arc::new(Mutex::new(CommandDispatchCompletionState {
                    claimed: false,
                    sender: Some(sender),
                })),
            },
            receiver,
        )
    }

    fn claim(&self) {
        self.state
            .lock()
            .expect("command completion lock poisoned")
            .claimed = true;
    }

    fn complete(&self, outcome: CommandOutcome) {
        let sender = self
            .state
            .lock()
            .expect("command completion lock poisoned")
            .sender
            .take();
        if let Some(sender) = sender {
            let _ = sender.send(outcome);
        }
    }

    fn complete_if_unclaimed(&self) {
        let mut state = self.state.lock().expect("command completion lock poisoned");
        if !state.claimed {
            if let Some(sender) = state.sender.take() {
                let _ = sender.send(CommandOutcome::Unavailable);
            }
        }
    }
}

#[derive(Clone, PartialEq, Action)]
#[action(namespace = knot, no_json)]
#[allow(
    dead_code,
    reason = "command dispatch uses this adapter when native focus routing is connected"
)]
struct CommandAction {
    command: Command,
    context: CapturedInvocationContext,
    completion: Option<CommandDispatchCompletion>,
}

#[derive(Clone, PartialEq, Action)]
#[action(namespace = knot, no_json)]
struct KeybindingCommand {
    command: Command,
}

fn fixed_command_bindings() -> [KeyBinding; 3] {
    [
        KeyBinding::new(
            "ctrl-alt-d",
            KeybindingCommand {
                command: Command {
                    name: DIAGNOSTIC_COMMAND.into(),
                    arguments: CommandArgumentValue::String(DIAGNOSTIC_BINDING_ARGUMENT.into()),
                },
            },
            Some(BASE_KEYMAP_CONTEXT),
        ),
        KeyBinding::new(
            "ctrl-alt-e",
            KeybindingCommand {
                command: Command {
                    name: FIXTURE_EDIT_COMMAND.into(),
                    arguments: CommandArgumentValue::String(FIXTURE_EDIT_ARGUMENT.into()),
                },
            },
            Some(BASE_KEYMAP_CONTEXT),
        ),
        KeyBinding::new(
            "ctrl-alt-k ctrl-alt-d",
            KeybindingCommand {
                command: Command {
                    name: DIAGNOSTIC_COMMAND.into(),
                    arguments: CommandArgumentValue::String(MULTI_KEY_DIAGNOSTIC_ARGUMENT.into()),
                },
            },
            Some(BASE_KEYMAP_CONTEXT),
        ),
    ]
}

fn keymap_control_bindings() -> [KeyBinding; 3] {
    [
        KeyBinding::new("ctrl-alt-a", ToggleActiveKeymap, None),
        KeyBinding::new("ctrl-alt-t", ActivateTransientKeymap, None),
        KeyBinding::new(
            "ctrl-alt-c",
            CancelTransientKeymap,
            Some(TRANSIENT_KEYMAP_CONTEXT),
        ),
    ]
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CommandSurfaceKind {
    Editor,
    Tree,
    Terminal,
    Shell,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CommandDiagnostic {
    invocation: CommandInvocationId,
    surface: CommandSurfaceKind,
    has_buffer: bool,
}

impl CommandAction {
    #[allow(
        dead_code,
        reason = "command sources dispatch prepared actions when native commands are registered"
    )]
    fn dispatch(&self, window: &mut Window, cx: &mut App) -> Result<(), CommandOutcome> {
        let Some(focus) = self.context.focus.upgrade() else {
            self.complete(CommandOutcome::InvalidTarget);
            return Err(CommandOutcome::InvalidTarget);
        };
        if self.context.window != window.window_handle() {
            self.complete(CommandOutcome::InvalidTarget);
            return Err(CommandOutcome::InvalidTarget);
        }
        focus.dispatch_action(self, window, cx);
        if let Some(completion) = &self.completion {
            completion.complete_if_unclaimed();
        }
        Ok(())
    }

    fn claim(&self) {
        if let Some(completion) = &self.completion {
            completion.claim();
        }
    }

    fn complete(&self, outcome: CommandOutcome) {
        if let Some(completion) = &self.completion {
            completion.complete(outcome);
        }
    }

    fn record_diagnostic(&self, surface: CommandSurfaceKind, cx: &mut App) {
        self.claim();
        let Some(workspace) = self.context.workspace.upgrade() else {
            self.complete(CommandOutcome::InvalidTarget);
            return;
        };
        let has_buffer = self.context.buffer.is_some();
        let _ = workspace.update(cx, |shell, cx| {
            shell.record_command_diagnostic(surface, has_buffer, cx);
        });
        self.complete(CommandOutcome::Completed);
    }
}

#[derive(Clone, PartialEq)]
struct InFlightExtensionCommand {
    id: CommandInvocationId,
    command: Command,
    target: model::CommandTarget,
    context: CapturedInvocationContext,
}

#[allow(
    dead_code,
    reason = "command sources may observe the identity and await the structured result"
)]
struct CommandExecution {
    id: CommandInvocationId,
    completion: tokio::sync::oneshot::Receiver<CommandOutcome>,
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
    open_buffers: OpenBufferCollection,
    source_buffer: OpenBufferId,
    outline: Entity<TreeView>,
    left_width: f32,
    right_width: f32,
    /// `(which, start_x, start_width)` captured on the first drag-move event
    /// of an in-progress divider drag; cleared on drop.
    drag_origin: Option<(usize, Pixels, f32)>,
    terminal_height: f32,
    /// `(start_y, start_height)` captured on the first terminal-divider move.
    terminal_drag_origin: Option<(Pixels, f32)>,
    /// Each view is an independent presentation over the active shared model.
    editor: Entity<EditorView>,
    secondary_editor: Entity<EditorView>,
    terminal: Entity<TerminalView>,
    buffer_registry: BufferRegistry,
    buffer_subscriptions: BufferSubscriptionRegistry,
    command_catalog: CommandCatalog,
    next_tree_registration: u64,
    next_command_invocation: u64,
    active_command: Option<InFlightExtensionCommand>,
    cancelled_commands: HashSet<CommandInvocationId>,
    command_state: SharedString,
    command_outcome: Option<CommandOutcome>,
    command_diagnostic: Option<CommandDiagnostic>,
    command_palette: Option<Entity<CommandPalette>>,
    command_palette_subscription: Option<Subscription>,
    active_keymap: bool,
    transient_keymap: bool,
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
        let mut open_buffers = OpenBufferCollection::new();
        let source_buffer = open_buffers.add(format!("{fixture_name}.kfx"), model.clone());
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
        let terminal = cx.new(TerminalView::new);
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
        let mut command_catalog = CommandCatalog::new();
        command_catalog
            .register_native(
                DIAGNOSTIC_COMMAND.into(),
                "Record resolved command context".into(),
            )
            .expect("diagnostic command name is unique");
        Shell {
            open_buffers,
            source_buffer,
            outline,
            left_width: 260.,
            right_width: 220.,
            drag_origin: None,
            terminal_height: 180.,
            terminal_drag_origin: None,
            editor,
            secondary_editor,
            terminal,
            buffer_registry,
            buffer_subscriptions: BufferSubscriptionRegistry::new(),
            command_catalog,
            next_tree_registration: 1,
            next_command_invocation: 1,
            active_command: None,
            cancelled_commands: HashSet::new(),
            command_state: "idle".into(),
            command_outcome: None,
            command_diagnostic: None,
            command_palette: None,
            command_palette_subscription: None,
            active_keymap: false,
            transient_keymap: false,
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
                let response = if let HostOperation::InvokeCommand { command } =
                    request.operation.clone()
                {
                    let caller = (request.extension, request.lifecycle);
                    let id = request.id;
                    let outcome = cx
                        .update(|cx| {
                            let Some(shell) = this.upgrade() else {
                                return None;
                            };
                            let windows = cx.window_stack().unwrap_or_else(|| cx.windows());
                            for window_handle in windows {
                                let command = command.clone();
                                let receiver = cx
                                    .update_window(window_handle, |_, window, cx| {
                                        if window.root::<Shell>().flatten().as_ref() != Some(&shell)
                                        {
                                            return None;
                                        }
                                        let (completion, receiver) =
                                            CommandDispatchCompletion::new(caller);
                                        let action = shell.update(cx, |shell, cx| {
                                            let Some(origin) = shell.command_origin(window, cx)
                                            else {
                                                completion.complete(CommandOutcome::InvalidTarget);
                                                return None;
                                            };
                                            match shell.prepare_command_action_with_completion(
                                                command,
                                                origin,
                                                Some(completion.clone()),
                                                window,
                                                cx,
                                            ) {
                                                Ok(action) => Some(action),
                                                Err(outcome) => {
                                                    completion.complete(outcome);
                                                    None
                                                }
                                            }
                                        });
                                        if let Some(action) = action {
                                            let _ = action.dispatch(window, cx);
                                        }
                                        Some(receiver)
                                    })
                                    .ok()
                                    .flatten();
                                if receiver.is_some() {
                                    return receiver;
                                }
                            }
                            None
                        })
                        .ok()
                        .flatten();
                    let outcome = match outcome {
                        Some(receiver) => receiver.await.unwrap_or(CommandOutcome::Unavailable),
                        None => CommandOutcome::InvalidTarget,
                    };
                    HostResponse {
                        extension: caller.0,
                        lifecycle: caller.1,
                        id,
                        result: Ok(HostResponseValue::CommandInvoked { outcome }),
                    }
                } else {
                    match this.update(cx, |this, cx| this.dispatch_host_request(request, cx)) {
                        Ok(response) => response,
                        Err(_) => return,
                    }
                };
                if let Err(error) = control.respond(response) {
                    failure = Some(format!("runtime response failed: {error:?}"));
                    break;
                }
            }
            let (extension, lifecycle) = control.identity();
            let _ = this.update(cx, |this, cx| {
                this.extension_controls.remove(&(extension, lifecycle));
                this.command_catalog.remove_lifecycle(extension, lifecycle);
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
        let command_mutation_error = self.validate_command_mutation(&request, cx).err();
        let result = match request.operation {
            _ if cancelled || !lifecycle_live => Err(HostRequestError::Cancelled),
            _ if command_mutation_error.is_some() => Err(command_mutation_error.unwrap()),
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
            HostOperation::RegisterCommand { name, title } => self
                .command_catalog
                .register_extension(name, title, request.extension, request.lifecycle)
                .map(|registration| HostResponseValue::CommandRegistered { registration })
                .map_err(map_command_error),
            HostOperation::UnregisterCommand { registration } => self
                .command_catalog
                .unregister(registration, request.extension, request.lifecycle)
                .map(|()| HostResponseValue::CommandUnregistered { registration })
                .map_err(map_command_error),
            HostOperation::InvokeCommand { .. } => unreachable!(
                "programmatic command invocation is completed by the asynchronous bridge"
            ),
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

    fn validate_command_mutation(
        &self,
        request: &HostRequest,
        cx: &Context<Self>,
    ) -> Result<(), HostRequestError> {
        let buffer = match &request.operation {
            HostOperation::ApplyEdits { buffer, .. }
            | HostOperation::ReplaceEditorContributions { buffer, .. }
            | HostOperation::DisposeEditorContributions { buffer } => *buffer,
            _ => return Ok(()),
        };
        let Some(invocation) = request.invocation else {
            return Ok(());
        };
        let Some(command) = self.active_command.as_ref() else {
            return Err(HostRequestError::Cancelled);
        };
        if command.id != invocation
            || command.target.extension != request.extension
            || command.target.lifecycle != request.lifecycle
            || self.cancelled_commands.contains(&invocation)
        {
            return Err(HostRequestError::Cancelled);
        }

        let workspace = cx.entity();
        if command.context.workspace.upgrade() != Some(workspace.clone())
            || !cx.windows().contains(&command.context.window)
            || command.context.focus.upgrade().is_none()
            || command.context.buffer != Some(buffer)
        {
            return Err(HostRequestError::Cancelled);
        }

        self.buffer_registry
            .resolve(buffer)
            .map(|_| ())
            .map_err(|_| HostRequestError::BufferClosed)
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
            TreeViewEvent::InvokeCommand {
                provider,
                command,
                window,
                focus,
            } => {
                let target = match self.command_catalog.resolve_extension(&command) {
                    Ok(target) => target,
                    Err(_) => {
                        self.record_command_outcome(CommandOutcome::Unavailable, cx);
                        return;
                    }
                };
                if target.extension == provider.extension && target.lifecycle == provider.lifecycle
                {
                    let _ = self.invoke_command_target(
                        target,
                        Command {
                            name: command.into(),
                            arguments: CommandArgumentValue::Null,
                        },
                        CommandOrigin {
                            window,
                            focus,
                            buffer: None,
                        },
                        cx,
                    );
                } else {
                    self.record_command_outcome(CommandOutcome::Unavailable, cx);
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

    fn invoke_fixture_command(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Result<CommandExecution, CommandOutcome> {
        if self.active_command.is_some() {
            return Err(CommandOutcome::Unavailable);
        }
        let Some(focus) = window.focused(cx) else {
            let outcome = CommandOutcome::InvalidTarget;
            self.record_command_outcome(outcome.clone(), cx);
            return Err(outcome);
        };
        let target = match self.command_catalog.resolve_extension("knot.fixture.edit") {
            Ok(target) => target,
            Err(_) => {
                let outcome = CommandOutcome::Unavailable;
                self.record_command_outcome(outcome.clone(), cx);
                return Err(outcome);
            }
        };
        self.invoke_command_target(
            target,
            Command {
                name: "knot.fixture.edit".into(),
                arguments: CommandArgumentValue::Null,
            },
            CommandOrigin {
                window: window.window_handle(),
                focus: focus.downgrade(),
                buffer: self.buffer_registry.active_handle(),
            },
            cx,
        )
    }

    fn invoke_contribution_action(
        &mut self,
        action: &EditorContributionAction,
        cx: &mut Context<Self>,
    ) {
        if action.source == ContributionSource::BuiltIn
            && action.command == ACTIVATE_SEARCH_RESULT_COMMAND
        {
            self.activate_search_result(action.range, Some(action.window), cx);
            return;
        }
        let ContributionSource::Extension {
            extension,
            lifecycle,
        } = action.source
        else {
            return;
        };
        let Some(buffer) = self.buffer_registry.handle_for(&action.model) else {
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
        let Ok(target) = self.command_catalog.resolve_extension(&action.command) else {
            return;
        };
        if target.extension != extension || target.lifecycle != lifecycle {
            return;
        }
        let _ = self.invoke_command_target(
            target,
            Command {
                name: action.command.as_str().into(),
                arguments: CommandArgumentValue::Null,
            },
            CommandOrigin {
                window: action.window,
                focus: action.focus.clone(),
                buffer: Some(buffer),
            },
            cx,
        );
    }

    fn activate_search_result(
        &mut self,
        result_range: ByteRange,
        window_handle: Option<AnyWindowHandle>,
        cx: &mut Context<Self>,
    ) {
        let Some(controller) = self
            .open_buffers
            .selected()
            .and_then(|entry| entry.search_results())
        else {
            return;
        };
        let Ok(target) = controller.resolve_target(result_range, cx) else {
            return;
        };
        if target.source != self.source_buffer
            || target.source_model != *self.editor.read(cx).model()
        {
            return;
        }
        let Some(window_handle) = window_handle else {
            return;
        };
        let editor = self.editor.clone();
        cx.defer(move |cx| {
            let _ = cx.update_window(window_handle, move |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    editor.select_reveal_and_focus(target.source_range, window, cx);
                });
            });
        });
    }

    fn invoke_command_target(
        &mut self,
        target: model::CommandTarget,
        value: Command,
        origin: CommandOrigin,
        cx: &mut Context<Self>,
    ) -> Result<CommandExecution, CommandOutcome> {
        if self.active_command.is_some() {
            return Err(CommandOutcome::Unavailable);
        }
        if self.command_catalog.resolve_extension(value.name.as_ref()) != Ok(target) {
            let outcome = CommandOutcome::Unavailable;
            self.record_command_outcome(outcome.clone(), cx);
            return Err(outcome);
        }
        if origin.focus.upgrade().is_none()
            || !cx.windows().contains(&origin.window)
            || origin
                .buffer
                .is_some_and(|buffer| self.buffer_registry.resolve(buffer).is_err())
        {
            let outcome = CommandOutcome::InvalidTarget;
            self.record_command_outcome(outcome.clone(), cx);
            return Err(outcome);
        }
        let Some(control) = self
            .extension_controls
            .get(&(target.extension, target.lifecycle))
            .cloned()
        else {
            let outcome = CommandOutcome::Unavailable;
            self.record_command_outcome(outcome.clone(), cx);
            return Err(outcome);
        };
        let command = InFlightExtensionCommand {
            id: self.allocate_command_invocation(),
            command: value,
            target,
            context: CapturedInvocationContext {
                window: origin.window,
                workspace: cx.entity().downgrade(),
                focus: origin.focus,
                buffer: origin.buffer,
            },
        };
        let id = command.id;
        let buffer = command.context.buffer;
        let arguments = command.command.arguments.clone();
        self.active_command = Some(command);
        self.command_state = "running".into();
        self.command_outcome = None;
        let execution = control.invoke_command(
            CommandInvocation {
                id,
                registration: target.registration,
                extension: target.extension,
                lifecycle: target.lifecycle,
                arguments,
            },
            buffer,
        );
        let (completion_sender, completion) = tokio::sync::oneshot::channel();
        cx.spawn(async move |this, cx| {
            let result = execution.await;
            let outcome = this
                .update(cx, |this, cx| this.finish_command(id, result, cx))
                .ok()
                .flatten()
                .unwrap_or(CommandOutcome::Unavailable);
            let _ = completion_sender.send(outcome);
        })
        .detach();
        cx.notify();
        Ok(CommandExecution { id, completion })
    }

    #[allow(
        dead_code,
        reason = "command sources use focus dispatch when native handlers are connected"
    )]
    fn prepare_command_action(
        &self,
        command: Command,
        origin: CommandOrigin,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Result<CommandAction, CommandOutcome> {
        self.prepare_command_action_with_completion(command, origin, None, window, cx)
    }

    fn prepare_command_action_with_completion(
        &self,
        command: Command,
        origin: CommandOrigin,
        completion: Option<CommandDispatchCompletion>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Result<CommandAction, CommandOutcome> {
        if origin.focus.upgrade().is_none() {
            return Err(CommandOutcome::InvalidTarget);
        }
        if origin.window != window.window_handle()
            || !cx.windows().contains(&origin.window)
            || origin
                .buffer
                .is_some_and(|buffer| self.buffer_registry.resolve(buffer).is_err())
        {
            return Err(CommandOutcome::InvalidTarget);
        }
        Ok(CommandAction {
            command,
            context: CapturedInvocationContext {
                window: origin.window,
                workspace: cx.entity().downgrade(),
                focus: origin.focus,
                buffer: origin.buffer,
            },
            completion,
        })
    }

    fn on_command_action(
        &mut self,
        action: &CommandAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.handle_native_command(action, window, cx) {
            return;
        }
        let Ok(target) = self
            .command_catalog
            .resolve_extension(action.command.name.as_ref())
        else {
            cx.propagate();
            return;
        };
        if action
            .completion
            .as_ref()
            .is_some_and(|completion| completion.caller == (target.extension, target.lifecycle))
        {
            action.claim();
            action.complete(CommandOutcome::Unavailable);
            return;
        }
        action.claim();
        let result = self.invoke_command_target(
            target,
            action.command.clone(),
            CommandOrigin {
                window: action.context.window,
                focus: action.context.focus.clone(),
                buffer: action.context.buffer,
            },
            cx,
        );
        match result {
            Ok(execution) => {
                if let Some(completion) = action.completion.clone() {
                    cx.spawn(async move |_, _| {
                        let outcome = execution
                            .completion
                            .await
                            .unwrap_or(CommandOutcome::Unavailable);
                        completion.complete(outcome);
                    })
                    .detach();
                }
            }
            Err(outcome) => action.complete(outcome),
        }
    }

    fn handle_native_command(
        &mut self,
        action: &CommandAction,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if action.command.name.as_ref() != DIAGNOSTIC_COMMAND {
            return false;
        }
        action.claim();
        self.record_command_diagnostic(
            CommandSurfaceKind::Shell,
            action.context.buffer.is_some(),
            cx,
        );
        action.complete(CommandOutcome::Completed);
        true
    }

    fn record_command_diagnostic(
        &mut self,
        surface: CommandSurfaceKind,
        has_buffer: bool,
        cx: &mut Context<Self>,
    ) {
        let invocation = self.allocate_command_invocation();
        self.command_diagnostic = Some(CommandDiagnostic {
            invocation,
            surface,
            has_buffer,
        });
        self.record_command_outcome(CommandOutcome::Completed, cx);
    }

    fn open_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(origin) = self.command_origin(window, cx) else {
            return;
        };
        let entries = self
            .command_catalog
            .definitions()
            .map(|definition| {
                let arguments = if definition.name.as_ref() == FIXTURE_EDIT_COMMAND {
                    CommandArgumentValue::String(FIXTURE_EDIT_ARGUMENT.into())
                } else {
                    CommandArgumentValue::Null
                };
                CommandPaletteEntry::new(definition.clone(), arguments)
            })
            .collect::<Vec<_>>();
        let palette = cx.new(|cx| CommandPalette::new(entries, origin, cx));
        let subscription = cx.subscribe(
            &palette,
            |this, _palette, event: &CommandPaletteEvent, cx| {
                this.command_palette = None;
                this.command_palette_subscription = None;
                if let CommandPaletteEvent::Confirmed { command, origin } = event {
                    this.dispatch_command(command.clone(), origin.clone(), cx);
                }
                cx.notify();
            },
        );
        window.focus(&palette.focus_handle(cx));
        self.command_palette = Some(palette);
        self.command_palette_subscription = Some(subscription);
        cx.notify();
    }

    fn on_keybinding_command(
        &mut self,
        action: &KeybindingCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(origin) = self.command_origin(window, cx) else {
            self.record_command_outcome(CommandOutcome::InvalidTarget, cx);
            return;
        };
        self.dispatch_command(action.command.clone(), origin, cx);
    }

    fn toggle_active_keymap(
        &mut self,
        _: &ToggleActiveKeymap,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.active_keymap = !self.active_keymap;
        cx.notify();
    }

    fn activate_transient_keymap(
        &mut self,
        _: &ActivateTransientKeymap,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.transient_keymap = true;
        cx.notify();
    }

    fn cancel_transient_keymap(
        &mut self,
        _: &CancelTransientKeymap,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clear_transient_keymap(cx);
    }

    fn clear_transient_keymap(&mut self, cx: &mut Context<Self>) {
        if self.transient_keymap {
            self.transient_keymap = false;
            cx.notify();
        }
    }

    fn dispatch_command(
        &mut self,
        command: Command,
        origin: CommandOrigin,
        cx: &mut Context<Self>,
    ) {
        self.clear_transient_keymap(cx);
        let shell = cx.entity();
        let window_handle = origin.window;
        cx.defer(move |cx| {
            let shell_for_window = shell.clone();
            let result = cx.update_window(window_handle, move |_, window, cx| {
                let prepared = shell_for_window.update(cx, |shell, cx| {
                    shell.prepare_command_action(command, origin, window, cx)
                });
                match prepared {
                    Ok(action) => {
                        if let Err(outcome) = action.dispatch(window, cx) {
                            shell_for_window.update(cx, |shell, cx| {
                                shell.record_command_outcome(outcome, cx);
                            });
                        }
                    }
                    Err(outcome) => {
                        shell_for_window.update(cx, |shell, cx| {
                            shell.record_command_outcome(outcome, cx);
                        });
                    }
                }
            });
            if result.is_err() {
                shell.update(cx, |shell, cx| {
                    shell.record_command_outcome(CommandOutcome::InvalidTarget, cx);
                });
            }
        });
    }

    fn command_origin(&self, window: &Window, cx: &App) -> Option<CommandOrigin> {
        let focus = window.focused(cx)?;
        let buffer = [&self.editor, &self.secondary_editor]
            .into_iter()
            .find(|editor| editor.focus_handle(cx).is_focused(window))
            .and_then(|editor| self.buffer_registry.handle_for(editor.read(cx).model()));
        Some(CommandOrigin {
            window: window.window_handle(),
            focus: focus.downgrade(),
            buffer,
        })
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
        result: Result<(), crate::host::ExtensionRuntimeExecutionError>,
        cx: &mut Context<Self>,
    ) -> Option<CommandOutcome> {
        if self.active_command.as_ref().map(|command| command.id) != Some(invocation) {
            return None;
        }
        self.active_command = None;
        let outcome = if self.cancelled_commands.remove(&invocation) {
            CommandOutcome::Cancelled
        } else {
            match result {
                Ok(()) => CommandOutcome::Completed,
                Err(crate::host::ExtensionRuntimeExecutionError::InvalidCommandArguments {
                    report,
                }) => CommandOutcome::InvalidArgument { message: report },
                Err(
                    crate::host::ExtensionRuntimeExecutionError::Closed
                    | crate::host::ExtensionRuntimeExecutionError::Terminated,
                ) => CommandOutcome::Cancelled,
                Err(error) => CommandOutcome::HandlerFailure {
                    message: format!("{error:?}"),
                },
            }
        };
        self.record_command_outcome(outcome.clone(), cx);
        Some(outcome)
    }

    fn record_command_outcome(&mut self, outcome: CommandOutcome, cx: &mut Context<Self>) {
        self.command_state = match &outcome {
            CommandOutcome::Completed => "completed",
            CommandOutcome::Unavailable => "unavailable",
            CommandOutcome::InvalidTarget => "invalid-target",
            CommandOutcome::InvalidArgument { .. } => "invalid-argument",
            CommandOutcome::Cancelled => "cancelled",
            CommandOutcome::HandlerFailure { .. } => "handler-failure",
        }
        .into();
        self.command_outcome = Some(outcome);
        cx.notify();
    }

    fn cancel_active_command(&mut self, cx: &mut Context<Self>) {
        if let Some(command) = &self.active_command {
            self.cancelled_commands.insert(command.id);
            self.command_state = "cancelling".into();
            cx.notify();
        }
    }

    /// Build a single selectable row: a div wrapping `label`; clicking selects it,
    /// the selected row gets a highlight bg, all rows get a hover bg.
    fn buffer_row(
        id: OpenBufferId,
        label: SharedString,
        selected: Option<OpenBufferId>,
        entity: Entity<Shell>,
    ) -> AnyElement {
        let is_selected = selected == Some(id);
        div()
            .id(("buffer", id.value()))
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
                    s.select_open_buffer(id, cx);
                });
            })
            .into_any_element()
    }

    fn select_open_buffer(&mut self, id: OpenBufferId, cx: &mut Context<Self>) {
        if self.open_buffers.selected_id() == Some(id) || !self.open_buffers.select(id) {
            return;
        }

        let model = self
            .open_buffers
            .selected()
            .expect("selected open buffer exists")
            .model()
            .clone();
        let secondary_editor = cx.new(|cx| {
            EditorView::new_with_options(
                model,
                1,
                EditorRenderingOptions {
                    show_gutter_markers: false,
                },
                cx,
            )
        });
        let action_subscription = cx.subscribe(
            &secondary_editor,
            |this, _editor, action: &EditorContributionAction, cx| {
                this.invoke_contribution_action(action, cx);
            },
        );

        self.secondary_editor = secondary_editor;
        self._editor_action_subscriptions[1] = action_subscription;
        cx.notify();
    }

    fn search_fixture(&mut self, query: &'static str, cx: &mut Context<Self>) {
        let source = self
            .open_buffers
            .entries()
            .find(|entry| entry.id() == self.source_buffer)
            .expect("source buffer remains open");
        let controller = SearchResultsController::search(
            query,
            self.source_buffer,
            source.title(),
            source.model().clone(),
            cx,
        );
        let result_buffer = self.open_buffers.add_search_results(controller);
        self.select_open_buffer(result_buffer, cx);
    }

    /// A pane that hosts a selectable single-row list.
    fn buffer_pane(&self, entity: Entity<Shell>) -> impl IntoElement {
        let entries: Vec<_> = self
            .open_buffers
            .entries()
            .map(|entry| (entry.id(), entry.title().clone()))
            .collect();
        let selected = self.open_buffers.selected_id();
        uniform_list("buffers", entries.len(), move |range, _window, _cx| {
            range
                .map(|ix| {
                    let (id, title) = entries[ix].clone();
                    Self::buffer_row(id, title, selected, entity.clone())
                })
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

    fn terminal_divider() -> impl IntoElement {
        div()
            .id("terminal-divider")
            .w_full()
            .h(px(6.))
            .flex_none()
            .bg(rgb(0x333333))
            .cursor_ns_resize()
            .on_drag(TerminalDividerDrag, |_value, _offset, _window, cx| {
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
        let revision = self
            .open_buffers
            .selected()
            .map(|entry| entry.model().read(cx).revision().to_string())
            .unwrap_or_else(|| "closed".into());
        let runtime_error = self
            .latest_runtime_error
            .clone()
            .unwrap_or_else(|| "none".into());
        let command_palette = self.command_palette.clone();
        let mut keymap_context = KeyContext::default();
        keymap_context.add(BASE_KEYMAP_CONTEXT);
        if self.active_keymap {
            keymap_context.add(ACTIVE_KEYMAP_CONTEXT);
        }
        if self.transient_keymap {
            keymap_context.add(TRANSIENT_KEYMAP_CONTEXT);
        }

        div()
            .flex()
            .flex_row()
            .size_full()
            .bg(rgb(0x1e1e1e))
            .text_color(rgb(0xd4d4d4))
            .key_context(keymap_context)
            .on_action(cx.listener(Self::toggle_active_keymap))
            .on_action(cx.listener(Self::activate_transient_keymap))
            .on_action(cx.listener(Self::cancel_transient_keymap))
            .on_action(cx.listener(Self::on_keybinding_command))
            .on_action(cx.listener(Self::on_command_action))
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
            .on_drag_move::<TerminalDividerDrag>(cx.listener(
                |this, event: &DragMoveEvent<TerminalDividerDrag>, window, cx| {
                    let pos_y = event.event.position.y;
                    let (start_y, start_height) = match this.terminal_drag_origin {
                        Some(origin) => origin,
                        None => {
                            let origin = (pos_y, this.terminal_height);
                            this.terminal_drag_origin = Some(origin);
                            origin
                        }
                    };
                    let max_height = (f32::from(window.viewport_size().height) - MIN_EDITOR_HEIGHT)
                        .max(MIN_TERMINAL_HEIGHT);
                    this.terminal_height = (start_height - f32::from(pos_y - start_y))
                        .clamp(MIN_TERMINAL_HEIGHT, max_height);
                    cx.notify();
                },
            ))
            .on_drop::<TerminalDividerDrag>(cx.listener(
                |this, _value: &TerminalDividerDrag, _window, cx| {
                    this.terminal_drag_origin = None;
                    cx.notify();
                },
            ))
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
                            .child("BUFFERS"),
                    )
                    .child(self.buffer_pane(entity.clone())),
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
                            .min_h_0()
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
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_h_0()
                                            .overflow_hidden()
                                            .child(self.editor.clone()),
                                    ),
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
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_h_0()
                                            .overflow_hidden()
                                            .child(self.secondary_editor.clone()),
                                    ),
                            ),
                    )
                    .child(Self::terminal_divider())
                    .child(
                        div()
                            .h(px(self.terminal_height))
                            .flex_none()
                            .child(self.terminal.clone()),
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
                                    .id("open-command-palette")
                                    .cursor_pointer()
                                    .text_color(rgb(0x80c0ff))
                                    .child("commands")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_command_palette(window, cx);
                                    })),
                            )
                            .child(
                                div()
                                    .id("run-fixture-command")
                                    .cursor_pointer()
                                    .text_color(rgb(0x80c0ff))
                                    .child("run command")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        let _ = this.invoke_fixture_command(window, cx);
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
                            .child(
                                div()
                                    .id("search-fixture-node")
                                    .cursor_pointer()
                                    .text_color(rgb(0x80c0ff))
                                    .child("search Node")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.search_fixture("Node", cx);
                                    })),
                            )
                            .child(
                                div()
                                    .id("search-fixture-rope")
                                    .cursor_pointer()
                                    .text_color(rgb(0x80c0ff))
                                    .child("search Rope")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.search_fixture("Rope", cx);
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
            .when_some(command_palette, |root, palette| {
                root.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .flex()
                        .items_start()
                        .justify_center()
                        .pt(px(72.))
                        .bg(rgba(0x00000088))
                        .child(palette),
                )
            })
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

        app.bind_keys(
            fixed_command_bindings()
                .into_iter()
                .chain(keymap_control_bindings())
                .chain([KeyBinding::new("cmd-q", Quit, None)]),
        );

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
    use std::{cell::RefCell, rc::Rc};

    use gpui::{
        AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
        KeyBinding, Modifiers, MouseButton, ParentElement, Render, TestAppContext, VisualContext,
        VisualTestContext, WeakEntity, Window, div, point, px,
    };

    use super::{
        BufferModel, ContributionSource, FIXTURE_EDIT_ARGUMENT, Shell,
        editor::EditorContributionAction, model::BufferAccessError,
    };
    use crate::host::{
        ExtensionRuntimeControl, ExtensionRuntimeExecutionError, V8Host,
        protocol::{
            ByteRange, Command, CommandArgumentValue, CommandInvocation, CommandInvocationId,
            CommandOutcome, DecorationToken, ExtensionId, GutterToken, HostOperation, HostRequest,
            HostRequestError, RequestId, TextEdit,
        },
    };

    gpui::actions!(step11_probe, [ClaimedProbe, FallbackProbe]);
    gpui::actions!(
        surface_context_probe,
        [
            EditorContext,
            TreeContext,
            TerminalContext,
            PaletteContext,
            BaseMapContext,
            ActiveMapContext,
            TransientMapContext
        ]
    );

    struct FocusDispatchProbe {
        target: FocusHandle,
        other: FocusHandle,
        visits: Rc<RefCell<Vec<&'static str>>>,
    }

    impl Render for FocusDispatchProbe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let child_claimed_visits = self.visits.clone();
            let child_fallback_visits = self.visits.clone();
            let parent_claimed_visits = self.visits.clone();
            let parent_fallback_visits = self.visits.clone();

            div()
                .on_action(move |_: &ClaimedProbe, _, _| {
                    parent_claimed_visits.borrow_mut().push("parent-claimed");
                })
                .on_action(move |_: &FallbackProbe, _, _| {
                    parent_fallback_visits.borrow_mut().push("parent-fallback");
                })
                .child(
                    div()
                        .track_focus(&self.target)
                        .on_action(move |_: &ClaimedProbe, _, _| {
                            child_claimed_visits.borrow_mut().push("child-claimed");
                        })
                        .on_action(move |_: &FallbackProbe, _, cx| {
                            child_fallback_visits.borrow_mut().push("child-fallback");
                            cx.propagate();
                        }),
                )
                .child(div().track_focus(&self.other))
        }
    }

    #[gpui::test]
    fn focus_handle_dispatch_preserves_focus_and_uses_normal_bubbling(cx: &mut TestAppContext) {
        let visits = Rc::new(RefCell::new(Vec::new()));
        let fixture_visits = visits.clone();
        let (probe, cx) = cx.add_window_view(move |_, cx| FocusDispatchProbe {
            target: cx.focus_handle(),
            other: cx.focus_handle(),
            visits: fixture_visits,
        });

        cx.update(|window, cx| {
            let (target, other) = {
                let probe = probe.read(cx);
                (probe.target.clone(), probe.other.clone())
            };
            window.focus(&other);

            target.dispatch_action(&ClaimedProbe, window, cx);
            target.dispatch_action(&FallbackProbe, window, cx);

            assert!(other.is_focused(window));
        });

        assert_eq!(
            visits.borrow().as_slice(),
            ["child-claimed", "child-fallback", "parent-fallback"]
        );
    }

    #[gpui::test]
    fn unhandled_command_action_reaches_app_scope_with_captured_context(cx: &mut TestAppContext) {
        let observed = Rc::new(RefCell::new(None));
        let observed_action = observed.clone();
        cx.update(move |cx| {
            cx.on_action(move |action: &super::CommandAction, _| {
                *observed_action.borrow_mut() = Some(action.clone());
            });
        });

        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(42))
            .into_parts();
        let (shell, cx) = cx.add_window_view(|_, cx| Shell::new(runtime, cx));
        let command = Command {
            name: "knot.fixture.dispatch".into(),
            arguments: CommandArgumentValue::String("argument".into()),
        };

        let action = shell.update_in(cx, |shell, window, cx| {
            let focus = shell.editor.focus_handle(cx);
            let origin = super::CommandOrigin {
                window: window.window_handle(),
                focus: focus.downgrade(),
                buffer: shell.buffer_registry.active_handle(),
            };
            let action = shell
                .prepare_command_action(command.clone(), origin.clone(), window, cx)
                .unwrap();
            (action, origin)
        });
        let (action, origin) = action;
        cx.update(|window, cx| action.dispatch(window, cx).unwrap());

        let action = observed.borrow();
        let action = action.as_ref().expect("unhandled action reaches app scope");
        assert_eq!(action.command, command);
        assert!(action.context.window == origin.window);
        assert_eq!(action.context.focus, origin.focus);
        assert_eq!(action.context.buffer, origin.buffer);
        assert_eq!(action.context.workspace.upgrade(), Some(shell.clone()));
    }

    #[gpui::test]
    fn command_action_rejects_a_focus_target_destroyed_after_capture(cx: &mut TestAppContext) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(44))
            .into_parts();
        let (shell, cx) = cx.add_window_view(|_, cx| Shell::new(runtime, cx));

        let (action, focus) = shell.update_in(cx, |shell, window, cx| {
            let focus = cx.focus_handle();
            let action = shell
                .prepare_command_action(
                    Command {
                        name: super::DIAGNOSTIC_COMMAND.into(),
                        arguments: CommandArgumentValue::Null,
                    },
                    super::CommandOrigin {
                        window: window.window_handle(),
                        focus: focus.downgrade(),
                        buffer: None,
                    },
                    window,
                    cx,
                )
                .unwrap();
            (action, focus)
        });
        drop(focus);

        assert_eq!(
            cx.update(|window, cx| action.dispatch(window, cx)),
            Err(CommandOutcome::InvalidTarget)
        );
        assert_eq!(
            cx.read(|cx| shell.read(cx).command_diagnostic),
            None,
            "a destroyed target must not fall back to the shell"
        );
    }

    #[gpui::test]
    fn diagnostic_command_resolves_each_surface_and_its_buffer_context(cx: &mut TestAppContext) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(43))
            .into_parts();
        let (shell, cx) = cx.add_window_view(|_, cx| Shell::new(runtime, cx));
        cx.refresh().unwrap();

        let surfaces = shell.update_in(cx, |shell, window, cx| {
            let buffer = shell.buffer_registry.active_handle().unwrap();
            [
                (
                    shell.editor.focus_handle(cx),
                    Some(buffer),
                    super::CommandSurfaceKind::Editor,
                    true,
                ),
                (
                    shell.outline.focus_handle(cx),
                    None,
                    super::CommandSurfaceKind::Tree,
                    false,
                ),
                (
                    shell.terminal.focus_handle(cx),
                    None,
                    super::CommandSurfaceKind::Terminal,
                    false,
                ),
            ]
            .map(|(focus, buffer, expected_surface, expected_buffer)| {
                let action = shell
                    .prepare_command_action(
                        Command {
                            name: super::DIAGNOSTIC_COMMAND.into(),
                            arguments: CommandArgumentValue::Null,
                        },
                        super::CommandOrigin {
                            window: window.window_handle(),
                            focus: focus.downgrade(),
                            buffer,
                        },
                        window,
                        cx,
                    )
                    .unwrap();
                (action, expected_surface, expected_buffer)
            })
        });

        for (index, (action, expected_surface, expected_buffer)) in surfaces.into_iter().enumerate()
        {
            cx.update(|window, cx| action.dispatch(window, cx).unwrap());

            let diagnostic = cx.read(|cx| shell.read(cx).command_diagnostic.unwrap());
            assert_eq!(
                diagnostic.invocation,
                CommandInvocationId::new(index as u64 + 1)
            );
            assert_eq!(diagnostic.surface, expected_surface);
            assert_eq!(diagnostic.has_buffer, expected_buffer);
        }
    }

    #[gpui::test]
    async fn command_palette_uses_the_live_catalog_and_dismisses_cleanly(cx: &mut TestAppContext) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(45))
            .into_parts();
        let (shell, cx) = cx.add_window_view(|_, cx| Shell::new(runtime, cx));
        wait_for_runtime_state(&shell, "running", cx).await;

        let expected = cx.read(|cx| {
            let mut names = shell
                .read(cx)
                .command_catalog
                .definitions()
                .map(|definition| definition.name.clone())
                .collect::<Vec<_>>();
            names.sort();
            names
        });
        let expected_origin = shell.update_in(cx, |shell, window, cx| {
            let editor_focus = shell.editor.focus_handle(cx);
            window.focus(&editor_focus);
            let origin = shell.command_origin(window, cx).unwrap();
            shell.open_command_palette(window, cx);
            origin
        });

        let palette = cx.read(|cx| shell.read(cx).command_palette.clone().unwrap());
        assert!(cx.update(|window, cx| palette.focus_handle(cx).is_focused(window)));
        assert!(cx.read(|cx| palette.read(cx).origin() == expected_origin));
        assert_eq!(
            expected_origin.buffer,
            cx.read(|cx| shell.read(cx).buffer_registry.active_handle())
        );
        assert_eq!(
            cx.read(|cx| {
                palette
                    .read(cx)
                    .visible_definitions()
                    .iter()
                    .map(|definition| definition.name.clone())
                    .collect::<Vec<_>>()
            }),
            expected
        );

        cx.update(|window, cx| {
            window.focus(&shell.read(cx).terminal.focus_handle(cx));
        });
        assert!(
            cx.read(|cx| palette.read(cx).origin() == expected_origin),
            "later focus changes must not mutate the palette origin"
        );

        palette.update(cx, |_palette, cx| {
            cx.emit(super::CommandPaletteEvent::Dismissed);
        });
        assert!(cx.read(|cx| shell.read(cx).command_palette.is_none()));
        assert!(cx.read(|cx| shell.read(cx).command_palette_subscription.is_none()));
    }

    #[gpui::test]
    async fn command_palette_controls_remain_local_to_the_preserved_origin(
        cx: &mut TestAppContext,
    ) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(46))
            .into_parts();
        let (shell, cx) = cx.add_window_view(|_, cx| Shell::new(runtime, cx));
        wait_for_runtime_state(&shell, "running", cx).await;

        let origin = shell.update_in(cx, |shell, window, cx| {
            let editor_focus = shell.editor.focus_handle(cx);
            window.focus(&editor_focus);
            shell.open_command_palette(window, cx);
            shell.command_palette.as_ref().unwrap().read(cx).origin()
        });
        let palette = cx.read(|cx| shell.read(cx).command_palette.clone().unwrap());
        cx.refresh().unwrap();

        let initially_selected =
            cx.read(|cx| palette.read(cx).selected_definition().unwrap().name.clone());
        cx.simulate_keystrokes("down");
        assert_ne!(
            cx.read(|cx| { palette.read(cx).selected_definition().unwrap().name.clone() }),
            initially_selected
        );

        cx.simulate_keystrokes("d i a g n o s t i c");
        assert_eq!(
            cx.read(|cx| {
                palette
                    .read(cx)
                    .visible_definitions()
                    .iter()
                    .map(|definition| definition.name.clone())
                    .collect::<Vec<_>>()
            }),
            vec![super::DIAGNOSTIC_COMMAND.into()]
        );
        assert!(cx.read(|cx| palette.read(cx).origin() == origin));
        assert!(origin.focus.upgrade().is_some());
        assert_eq!(
            origin.buffer,
            cx.read(|cx| shell.read(cx).buffer_registry.active_handle())
        );

        cx.simulate_keystrokes("backspace escape");
        assert!(cx.read(|cx| shell.read(cx).command_palette.is_none()));
        assert_eq!(cx.read(|cx| shell.read(cx).command_outcome.clone()), None);
    }

    #[gpui::test]
    async fn command_palette_dispatches_native_command_at_the_unfocused_origin(
        cx: &mut TestAppContext,
    ) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(47))
            .into_parts();
        let (shell, cx) = cx.add_window_view(|_, cx| Shell::new(runtime, cx));
        wait_for_runtime_state(&shell, "running", cx).await;

        shell.update_in(cx, |shell, window, cx| {
            window.focus(&shell.editor.focus_handle(cx));
            shell.open_command_palette(window, cx);
        });
        cx.refresh().unwrap();
        cx.simulate_keystrokes("d i a g n o s t i c enter");

        assert_eq!(
            cx.read(|cx| shell.read(cx).command_diagnostic),
            Some(super::CommandDiagnostic {
                invocation: CommandInvocationId::new(1),
                surface: super::CommandSurfaceKind::Editor,
                has_buffer: true,
            })
        );
        assert!(cx.read(|cx| shell.read(cx).command_palette.is_none()));
        assert!(!cx.update(|window, cx| shell.read(cx).editor.focus_handle(cx).is_focused(window)));
    }

    #[gpui::test]
    async fn command_palette_dispatches_extension_command_at_the_unfocused_origin(
        cx: &mut TestAppContext,
    ) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(48))
            .into_parts();
        let (shell, cx) = cx.add_window_view(|_, cx| Shell::new(runtime, cx));
        wait_for_runtime_state(&shell, "running", cx).await;
        let initial_text = cx.read(|cx| shell.read(cx).editor.read(cx).model().read(cx).text());

        shell.update_in(cx, |shell, window, cx| {
            window.focus(&shell.editor.focus_handle(cx));
            shell.open_command_palette(window, cx);
        });
        cx.refresh().unwrap();
        cx.simulate_keystrokes("f i x t u r e . e d i t enter");
        wait_for_command_state(&shell, "completed", cx).await;

        assert_eq!(
            cx.read(|cx| shell.read(cx).command_outcome.clone()),
            Some(CommandOutcome::Completed)
        );
        assert_eq!(
            cx.read(|cx| shell.read(cx).editor.read(cx).model().read(cx).text()),
            format!("{FIXTURE_EDIT_ARGUMENT}{initial_text}")
        );
        assert!(!cx.update(|window, cx| shell.read(cx).editor.focus_handle(cx).is_focused(window)));
    }

    #[gpui::test]
    async fn command_palette_rejects_an_origin_that_disappears_before_confirmation(
        cx: &mut TestAppContext,
    ) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(49))
            .into_parts();
        let (shell, cx) = cx.add_window_view(|_, cx| Shell::new(runtime, cx));
        wait_for_runtime_state(&shell, "running", cx).await;

        let origin = shell.update_in(cx, |shell, window, cx| {
            window.focus(&shell.secondary_editor.focus_handle(cx));
            shell.open_command_palette(window, cx);
            shell.command_palette.as_ref().unwrap().read(cx).origin()
        });
        cx.refresh().unwrap();
        cx.simulate_keystrokes("d i a g n o s t i c");

        shell.update(cx, |shell, cx| shell.search_fixture("Node", cx));
        cx.refresh().unwrap();
        assert!(origin.focus.upgrade().is_none());

        cx.simulate_keystrokes("enter");

        assert_eq!(
            cx.read(|cx| shell.read(cx).command_outcome.clone()),
            Some(CommandOutcome::InvalidTarget)
        );
        assert_eq!(cx.read(|cx| shell.read(cx).command_diagnostic), None);
        assert!(cx.read(|cx| shell.read(cx).command_palette.is_none()));
    }

    #[gpui::test]
    async fn fixed_keybindings_dispatch_commands_with_explicit_arguments(cx: &mut TestAppContext) {
        let bindings = super::fixed_command_bindings();
        let diagnostic = bindings[0]
            .action()
            .as_any()
            .downcast_ref::<super::KeybindingCommand>()
            .unwrap();
        assert_eq!(
            diagnostic.command.arguments,
            CommandArgumentValue::String(super::DIAGNOSTIC_BINDING_ARGUMENT.into())
        );
        let edit = bindings[1]
            .action()
            .as_any()
            .downcast_ref::<super::KeybindingCommand>()
            .unwrap();
        assert_eq!(
            edit.command.arguments,
            CommandArgumentValue::String(FIXTURE_EDIT_ARGUMENT.into())
        );
        let multi_key = bindings[2]
            .action()
            .as_any()
            .downcast_ref::<super::KeybindingCommand>()
            .unwrap();
        assert_eq!(
            multi_key.command.arguments,
            CommandArgumentValue::String(super::MULTI_KEY_DIAGNOSTIC_ARGUMENT.into())
        );
        cx.update(|cx| cx.bind_keys(bindings));

        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(50))
            .into_parts();
        let (shell, cx) = cx.add_window_view(|_, cx| Shell::new(runtime, cx));
        wait_for_runtime_state(&shell, "running", cx).await;
        let initial_text = cx.read(|cx| shell.read(cx).editor.read(cx).model().read(cx).text());

        shell.update_in(cx, |shell, window, cx| {
            window.focus(&shell.terminal.focus_handle(cx));
        });
        cx.refresh().unwrap();
        cx.simulate_keystrokes("ctrl-alt-d");
        assert_eq!(
            cx.read(|cx| shell.read(cx).command_diagnostic),
            Some(super::CommandDiagnostic {
                invocation: CommandInvocationId::new(1),
                surface: super::CommandSurfaceKind::Terminal,
                has_buffer: false,
            })
        );

        shell.update_in(cx, |shell, window, cx| {
            window.focus(&shell.editor.focus_handle(cx));
        });
        cx.simulate_keystrokes("ctrl-alt-e");
        wait_for_command_state(&shell, "completed", cx).await;
        assert_eq!(
            cx.read(|cx| shell.read(cx).editor.read(cx).model().read(cx).text()),
            format!("{FIXTURE_EDIT_ARGUMENT}{initial_text}")
        );
    }

    #[gpui::test]
    async fn multi_keystroke_binding_uses_gpui_pending_input(cx: &mut TestAppContext) {
        cx.update(|cx| cx.bind_keys(super::fixed_command_bindings()));

        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(53))
            .into_parts();
        let (shell, cx) = cx.add_window_view(|_, cx| Shell::new(runtime, cx));
        wait_for_runtime_state(&shell, "running", cx).await;
        shell.update_in(cx, |shell, window, cx| {
            window.focus(&shell.editor.focus_handle(cx));
        });
        cx.refresh().unwrap();

        cx.simulate_keystrokes("ctrl-alt-k");
        assert!(shell.update_in(cx, |_, window, _| window.has_pending_keystrokes()));
        assert_eq!(cx.read(|cx| shell.read(cx).command_diagnostic), None);

        cx.simulate_keystrokes("ctrl-alt-d");
        assert!(!shell.update_in(cx, |_, window, _| window.has_pending_keystrokes()));
        assert_eq!(
            cx.read(|cx| shell.read(cx).command_diagnostic),
            Some(super::CommandDiagnostic {
                invocation: CommandInvocationId::new(1),
                surface: super::CommandSurfaceKind::Editor,
                has_buffer: true,
            })
        );
    }

    #[gpui::test]
    async fn focused_surfaces_publish_distinct_key_contexts(cx: &mut TestAppContext) {
        let visits = Rc::new(RefCell::new(Vec::new()));
        cx.update(|cx| {
            let observed = visits.clone();
            cx.on_action(move |_: &EditorContext, _| observed.borrow_mut().push("editor"));
            let observed = visits.clone();
            cx.on_action(move |_: &TreeContext, _| observed.borrow_mut().push("tree"));
            let observed = visits.clone();
            cx.on_action(move |_: &TerminalContext, _| observed.borrow_mut().push("terminal"));
            let observed = visits.clone();
            cx.on_action(move |_: &PaletteContext, _| observed.borrow_mut().push("palette"));
            cx.bind_keys([
                KeyBinding::new("ctrl-alt-x", EditorContext, Some(super::EDITOR_KEY_CONTEXT)),
                KeyBinding::new("ctrl-alt-x", TreeContext, Some(super::TREE_KEY_CONTEXT)),
                KeyBinding::new(
                    "ctrl-alt-x",
                    TerminalContext,
                    Some(super::TERMINAL_KEY_CONTEXT),
                ),
                KeyBinding::new(
                    "ctrl-alt-x",
                    PaletteContext,
                    Some(super::PALETTE_KEY_CONTEXT),
                ),
            ]);
        });

        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(51))
            .into_parts();
        let (shell, cx) = cx.add_window_view(|_, cx| Shell::new(runtime, cx));
        wait_for_runtime_state(&shell, "running", cx).await;
        cx.refresh().unwrap();

        shell.update_in(cx, |shell, window, cx| {
            window.focus(&shell.editor.focus_handle(cx));
        });
        cx.simulate_keystrokes("ctrl-alt-x");

        shell.update_in(cx, |shell, window, cx| {
            window.focus(&shell.outline.focus_handle(cx));
        });
        cx.simulate_keystrokes("ctrl-alt-x");

        shell.update_in(cx, |shell, window, cx| {
            window.focus(&shell.terminal.focus_handle(cx));
        });
        cx.simulate_keystrokes("ctrl-alt-x");

        shell.update_in(cx, |shell, window, cx| {
            window.focus(&shell.editor.focus_handle(cx));
            shell.open_command_palette(window, cx);
        });
        cx.refresh().unwrap();
        cx.simulate_keystrokes("ctrl-alt-x");

        assert_eq!(
            visits.borrow().as_slice(),
            ["editor", "tree", "terminal", "palette"]
        );
    }

    #[gpui::test]
    async fn active_and_transient_keymap_contexts_follow_their_lifetimes(cx: &mut TestAppContext) {
        let visits = Rc::new(RefCell::new(Vec::new()));
        cx.update(|cx| {
            let observed = visits.clone();
            cx.on_action(move |_: &ActiveMapContext, _| observed.borrow_mut().push("active"));
            let observed = visits.clone();
            cx.on_action(move |_: &TransientMapContext, _| observed.borrow_mut().push("transient"));
            cx.bind_keys(
                super::fixed_command_bindings()
                    .into_iter()
                    .chain(super::keymap_control_bindings())
                    .chain([
                        KeyBinding::new(
                            "ctrl-alt-y",
                            ActiveMapContext,
                            Some(super::ACTIVE_KEYMAP_CONTEXT),
                        ),
                        KeyBinding::new(
                            "ctrl-alt-u",
                            TransientMapContext,
                            Some(super::TRANSIENT_KEYMAP_CONTEXT),
                        ),
                    ]),
            );
        });

        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(52))
            .into_parts();
        let (shell, cx) = cx.add_window_view(|_, cx| Shell::new(runtime, cx));
        wait_for_runtime_state(&shell, "running", cx).await;
        shell.update_in(cx, |shell, window, cx| {
            window.focus(&shell.editor.focus_handle(cx));
        });
        cx.refresh().unwrap();

        cx.simulate_keystrokes("ctrl-alt-a");
        assert!(cx.read(|cx| shell.read(cx).active_keymap));
        cx.refresh().unwrap();
        cx.simulate_keystrokes("ctrl-alt-y");

        cx.simulate_keystrokes("ctrl-alt-t");
        assert!(cx.read(|cx| shell.read(cx).transient_keymap));
        cx.refresh().unwrap();
        cx.simulate_keystrokes("ctrl-alt-u");
        assert_eq!(visits.borrow().as_slice(), ["active", "transient"]);

        cx.simulate_keystrokes("ctrl-alt-d");
        assert!(cx.read(|cx| shell.read(cx).active_keymap));
        assert!(!cx.read(|cx| shell.read(cx).transient_keymap));
        assert_eq!(
            cx.read(|cx| shell.read(cx).command_diagnostic),
            Some(super::CommandDiagnostic {
                invocation: CommandInvocationId::new(1),
                surface: super::CommandSurfaceKind::Editor,
                has_buffer: true,
            })
        );
        cx.refresh().unwrap();
        cx.simulate_keystrokes("ctrl-alt-u");
        assert_eq!(visits.borrow().as_slice(), ["active", "transient"]);

        cx.simulate_keystrokes("ctrl-alt-t");
        cx.refresh().unwrap();
        cx.simulate_keystrokes("ctrl-alt-c");
        assert!(cx.read(|cx| shell.read(cx).active_keymap));
        assert!(!cx.read(|cx| shell.read(cx).transient_keymap));
    }

    #[gpui::test]
    async fn keymap_precedence_follows_surface_then_transient_active_and_base(
        cx: &mut TestAppContext,
    ) {
        let visits = Rc::new(RefCell::new(Vec::new()));
        cx.update(|cx| {
            let observed = visits.clone();
            cx.on_action(move |_: &BaseMapContext, _| observed.borrow_mut().push("base"));
            let observed = visits.clone();
            cx.on_action(move |_: &ActiveMapContext, _| observed.borrow_mut().push("active"));
            let observed = visits.clone();
            cx.on_action(move |_: &TransientMapContext, _| observed.borrow_mut().push("transient"));
            let observed = visits.clone();
            cx.on_action(move |_: &EditorContext, _| observed.borrow_mut().push("editor"));
            cx.bind_keys(super::keymap_control_bindings().into_iter().chain([
                KeyBinding::new(
                    "ctrl-alt-p",
                    BaseMapContext,
                    Some(super::BASE_KEYMAP_CONTEXT),
                ),
                KeyBinding::new(
                    "ctrl-alt-p",
                    ActiveMapContext,
                    Some(super::ACTIVE_KEYMAP_CONTEXT),
                ),
                KeyBinding::new(
                    "ctrl-alt-p",
                    TransientMapContext,
                    Some(super::TRANSIENT_KEYMAP_CONTEXT),
                ),
                KeyBinding::new("ctrl-alt-p", EditorContext, Some(super::EDITOR_KEY_CONTEXT)),
            ]));
        });

        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(54))
            .into_parts();
        let (shell, cx) = cx.add_window_view(|_, cx| Shell::new(runtime, cx));
        wait_for_runtime_state(&shell, "running", cx).await;
        shell.update_in(cx, |shell, window, cx| {
            window.focus(&shell.outline.focus_handle(cx));
        });
        cx.refresh().unwrap();

        cx.simulate_keystrokes("ctrl-alt-p");
        cx.simulate_keystrokes("ctrl-alt-a");
        cx.refresh().unwrap();
        cx.simulate_keystrokes("ctrl-alt-p");
        cx.simulate_keystrokes("ctrl-alt-t");
        cx.refresh().unwrap();
        cx.simulate_keystrokes("ctrl-alt-p");

        shell.update_in(cx, |shell, window, cx| {
            window.focus(&shell.editor.focus_handle(cx));
        });
        cx.simulate_keystrokes("ctrl-alt-p");

        assert_eq!(
            visits.borrow().as_slice(),
            ["base", "active", "transient", "editor"]
        );
    }

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

    fn invoke_fixture_command_in_window(shell: &Entity<Shell>, cx: &mut VisualTestContext) {
        shell.update_in(cx, |shell, window, cx| {
            let focus = shell.editor.focus_handle(cx);
            window.focus(&focus);
            let _ = shell.invoke_fixture_command(window, cx);
        });
    }

    fn invoke_extension_target_in_window(
        shell: &Entity<Shell>,
        target: super::model::CommandTarget,
        name: &str,
        cx: &mut VisualTestContext,
    ) {
        shell.update_in(cx, |shell, window, cx| {
            let focus = shell.editor.focus_handle(cx);
            window.focus(&focus);
            let _ = shell.invoke_command_target(
                target,
                Command {
                    name: name.into(),
                    arguments: CommandArgumentValue::Null,
                },
                super::CommandOrigin {
                    window: window.window_handle(),
                    focus: focus.downgrade(),
                    buffer: shell.buffer_registry.active_handle(),
                },
                cx,
            );
        });
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
            let source = shell.open_buffers.selected().unwrap();
            assert_eq!(source.title(), "rust_sample.kfx");
            assert_eq!(source.model(), &displayed);
            (resolved, displayed)
        });
        assert_eq!(resolved, displayed);
    }

    #[gpui::test]
    async fn top_level_script_invokes_a_command_through_the_captured_editor_target(
        cx: &mut TestAppContext,
    ) {
        let host = V8Host::new();
        let (shell, cx) = cx.add_window_view(|_, cx| {
            Shell::new_with_runtimes(
                [ExtensionId::new(55), ExtensionId::new(56)]
                    .map(|extension| host.spawn_extension(extension).into_parts())
                    .into(),
                cx,
            )
        });
        wait_for_runtime_state(&shell, "running", cx).await;
        shell.update_in(cx, |shell, window, cx| {
            window.focus(&shell.editor.focus_handle(cx));
        });
        let caller = runtime_control(&shell, ExtensionId::new(56), cx);

        caller
            .execute_fixture_module(
                "file:///fixtures/top-level-command.js",
                r#"
                    import { commands } from "knot:editor";
                    const outcome = await commands.invoke(
                      "knot.fixture.edit",
                      "// script command\n",
                    );
                    if (outcome.kind !== "completed") {
                      throw new Error(`unexpected command outcome: ${outcome.kind}`);
                    }
                "#,
            )
            .await
            .unwrap();

        let text = cx.read(|cx| {
            let shell = shell.read(cx);
            shell
                .editor
                .read(cx)
                .model()
                .read_with(cx, |model, _| model.text())
        });
        assert!(text.starts_with("// script command\n"));
    }

    #[gpui::test]
    fn selecting_an_open_buffer_reconstructs_only_the_secondary_view(cx: &mut TestAppContext) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(8))
            .into_parts();
        let shell = cx.new(|cx| Shell::new(runtime, cx));
        let generated = cx.new(|_| BufferModel::from_text("generated\nresult"));

        let (source_editor, old_secondary, generated_id) = shell.update(cx, |shell, _| {
            let generated_id = shell.open_buffers.add("Generated", generated.clone());
            (
                shell.editor.clone(),
                shell.secondary_editor.downgrade(),
                generated_id,
            )
        });
        shell.update(cx, |shell, cx| {
            shell.select_open_buffer(generated_id, cx);
        });
        cx.run_until_parked();

        cx.read(|cx| {
            let shell = shell.read(cx);
            assert_eq!(shell.editor, source_editor);
            assert_eq!(shell.secondary_editor.read(cx).model(), &generated);
            assert_eq!(
                shell.secondary_editor.read(cx).responsiveness_state(),
                (0, 0., 0)
            );
            assert!(old_secondary.upgrade().is_none());
        });
    }

    #[gpui::test]
    fn fixture_searches_create_retained_read_only_buffers(cx: &mut TestAppContext) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(81))
            .into_parts();
        let shell = cx.new(|cx| Shell::new(runtime, cx));

        shell.update(cx, |shell, cx| {
            shell.search_fixture("Node", cx);
            shell.search_fixture("Rope", cx);
        });
        cx.run_until_parked();

        let first_result = shell.read_with(cx, |shell, cx| {
            let entries = shell.open_buffers.entries().collect::<Vec<_>>();
            assert_eq!(entries.len(), 3);
            assert_eq!(entries[1].title(), "Search: \"Node\"");
            assert_eq!(entries[2].title(), "Search: \"Rope\"");
            assert!(entries[1].search_results().is_some());
            assert!(entries[2].search_results().is_some());
            assert_eq!(shell.open_buffers.selected_id(), Some(entries[2].id()));
            assert!(
                !entries[1]
                    .model()
                    .read(cx)
                    .resolved_contributions()
                    .is_empty()
            );
            assert!(
                !entries[2]
                    .model()
                    .read(cx)
                    .resolved_contributions()
                    .is_empty()
            );
            entries[1].model().clone()
        });
        assert_eq!(
            first_result.update(cx, |model, _| model.replace(0..0, "edit")),
            Err(BufferAccessError::ReadOnly)
        );
    }

    #[gpui::test]
    fn retained_search_result_activates_after_repeated_buffer_switching(cx: &mut TestAppContext) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(82))
            .into_parts();
        let (shell, cx) = cx.add_window_view(|_, cx| Shell::new(runtime, cx));
        shell.update(cx, |shell, cx| {
            shell.search_fixture("Node", cx);
            shell.search_fixture("Rope", cx);
            let ids = shell
                .open_buffers
                .entries()
                .map(|entry| entry.id())
                .collect::<Vec<_>>();
            for id in [ids[0], ids[1], ids[2], ids[0], ids[2], ids[1]] {
                shell.select_open_buffer(id, cx);
            }
        });
        cx.refresh().unwrap();

        let (click, expected_range) = cx.read(|cx| {
            let shell = shell.read(cx);
            let bounds = shell.secondary_editor.read(cx).interaction_bounds();
            let controller = shell
                .open_buffers
                .selected()
                .unwrap()
                .search_results()
                .unwrap();
            (
                point(bounds.origin.x + px(20.), bounds.origin.y + px(10.)),
                controller.matches()[0].source_range,
            )
        });
        cx.simulate_mouse_down(click, MouseButton::Left, Modifiers::default());
        cx.run_until_parked();

        cx.update(|window, cx| {
            let editor = shell.read(cx).editor.clone();
            let editor = editor.read(cx);
            assert_eq!(
                editor.selected_byte_range(),
                Some(expected_range.start_byte_offset..expected_range.end_byte_offset)
            );
            assert!(editor.focus_handle(cx).is_focused(window));
        });
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
        let (shell, cx) = cx.add_window_view(|_, cx| Shell::new(runtime, cx));
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

        shell.update_in(cx, |shell, window, cx| {
            let focus = shell.editor.focus_handle(cx).downgrade();
            let model = shell.editor.read(cx).model().clone();
            shell.invoke_contribution_action(
                &EditorContributionAction {
                    command: "knot.fixture.test-contribution".into(),
                    source: ContributionSource::Extension {
                        extension: identity.0,
                        lifecycle: identity.1,
                    },
                    range: ByteRange {
                        start_byte_offset: 0,
                        end_byte_offset: 0,
                    },
                    model,
                    window: window.window_handle(),
                    focus,
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
        shell.update_in(cx, |shell, window, cx| {
            let focus = shell.editor.focus_handle(cx).downgrade();
            let model = shell.editor.read(cx).model().clone();
            shell.invoke_contribution_action(
                &EditorContributionAction {
                    command: "knot.fixture.test-contribution".into(),
                    source: ContributionSource::Extension {
                        extension: identity.0,
                        lifecycle: identity.1,
                    },
                    range: ByteRange {
                        start_byte_offset: 0,
                        end_byte_offset: 0,
                    },
                    model,
                    window: window.window_handle(),
                    focus,
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
                assert!(model.replace(0..1, "C").unwrap());
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
        let (shell, cx) = cx.add_window_view(|_, cx| Shell::new(runtime, cx));
        wait_for_runtime_state(&shell, "running", cx).await;
        let heartbeat = cx.read(|cx| shell.read(cx).heartbeat);
        invoke_fixture_command_in_window(&shell, cx);
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
        let (shell, cx) = cx.add_window_view(|_, cx| {
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
                .command_catalog
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

        invoke_fixture_command_in_window(&shell, cx);
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
    async fn commands_retain_context_with_monotonic_identities_and_reject_overlap(
        cx: &mut TestAppContext,
    ) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(19))
            .into_parts();
        let (shell, cx) = cx.add_window_view(|_, cx| Shell::new(runtime, cx));
        wait_for_runtime_state(&shell, "running", cx).await;
        let control = only_runtime_control(&shell, cx);
        control
            .execute_fixture_module(
                "file:///fixtures/slow-command.js",
                r#"
                    import { commands } from "knot:editor";
                    await commands.register("knot.fixture.slow", async () => {
                      globalThis.slowCommandRuns =
                        (globalThis.slowCommandRuns ?? 0) + 1;
                      await globalThis.__knotFixtureDelay(25);
                    });
                "#,
            )
            .await
            .unwrap();

        let (target, buffer) = cx.read(|cx| {
            let shell = shell.read(cx);
            (
                shell
                    .command_catalog
                    .resolve_extension("knot.fixture.slow")
                    .unwrap(),
                shell.buffer_registry.active_handle(),
            )
        });
        invoke_extension_target_in_window(&shell, target, "knot.fixture.slow", cx);
        let window = cx.window_handle();
        cx.read(|cx| {
            let command = shell.read(cx).active_command.as_ref().unwrap();
            assert_eq!(command.id, CommandInvocationId::new(1));
            assert_eq!(command.command.name, "knot.fixture.slow".into());
            assert_eq!(command.command.arguments, CommandArgumentValue::Null);
            assert_eq!(command.target, target);
            assert!(command.context.window == window);
            assert_eq!(command.context.buffer, buffer);
            assert_eq!(command.context.workspace.upgrade(), Some(shell.clone()));
            assert!(command.context.focus.upgrade().is_some());
        });

        invoke_extension_target_in_window(&shell, target, "knot.fixture.slow", cx);
        cx.read(|cx| {
            let shell = shell.read(cx);
            assert_eq!(
                shell.active_command.as_ref().map(|command| command.id),
                Some(CommandInvocationId::new(1))
            );
            assert_eq!(shell.next_command_invocation, 2);
        });
        wait_for_command_state(&shell, "completed", cx).await;
        control
            .execute_fixture_script(
                "verify-single-command.js",
                "if (globalThis.slowCommandRuns !== 1) throw new Error('overlapping command ran')",
            )
            .await
            .unwrap();

        invoke_extension_target_in_window(&shell, target, "knot.fixture.slow", cx);
        cx.read(|cx| {
            let command = shell.read(cx).active_command.as_ref().unwrap();
            assert_eq!(command.id, CommandInvocationId::new(2));
            assert_eq!(command.target, target);
            assert_eq!(command.context.buffer, buffer);
        });
        wait_for_command_state(&shell, "completed", cx).await;
        control
            .execute_fixture_script(
                "verify-second-command.js",
                "if (globalThis.slowCommandRuns !== 2) throw new Error('second command did not run')",
            )
            .await
            .unwrap();
    }

    #[gpui::test]
    async fn command_keeps_its_captured_buffer_after_focus_and_active_buffer_change(
        cx: &mut TestAppContext,
    ) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(20))
            .into_parts();
        let (shell, cx) = cx.add_window_view(|_, cx| Shell::new(runtime, cx));
        wait_for_runtime_state(&shell, "running", cx).await;
        let control = only_runtime_control(&shell, cx);
        control
            .execute_fixture_module(
                "file:///fixtures/captured-buffer-command.js",
                r#"
                    import { commands } from "knot:editor";
                    await commands.register("knot.fixture.captured-buffer", async (context) => {
                      await globalThis.__knotFixtureDelay(25);
                      const snapshot = await context.buffer.snapshot();
                      await context.buffer.applyEdits(
                        [{ range: { startByteOffset: 0, endByteOffset: 0 }, text: "captured\n" }],
                        { ifRevision: snapshot.revision },
                      );
                    });
                "#,
            )
            .await
            .unwrap();

        let replacement = cx.new(|_| BufferModel::from_text("replacement"));
        let (target, captured_model, captured_handle) = cx.read(|cx| {
            let shell = shell.read(cx);
            let captured_handle = shell.buffer_registry.active_handle().unwrap();
            (
                shell
                    .command_catalog
                    .resolve_extension("knot.fixture.captured-buffer")
                    .unwrap(),
                shell.buffer_registry.resolve(captured_handle).unwrap(),
                captured_handle,
            )
        });

        invoke_extension_target_in_window(&shell, target, "knot.fixture.captured-buffer", cx);
        let replacement_handle = shell.update_in(cx, |shell, window, cx| {
            let replacement_handle = shell.buffer_registry.open(&replacement);
            shell.buffer_registry.set_active(Some(replacement_handle));
            let terminal_focus = shell.terminal.focus_handle(cx);
            window.focus(&terminal_focus);
            replacement_handle
        });

        cx.read(|cx| {
            let shell = shell.read(cx);
            let command = shell.active_command.as_ref().unwrap();
            assert_eq!(command.context.buffer, Some(captured_handle));
            assert_eq!(
                shell.buffer_registry.active_handle(),
                Some(replacement_handle)
            );
        });
        cx.update(|window, cx| {
            assert!(shell.read(cx).terminal.focus_handle(cx).is_focused(window));
        });
        wait_for_command_state(&shell, "completed", cx).await;

        assert!(
            captured_model
                .read_with(cx, |model, _| model.text())
                .starts_with("captured\n")
        );
        assert_eq!(
            replacement.read_with(cx, |model, _| model.text()),
            "replacement"
        );
    }

    #[gpui::test]
    async fn command_mutations_revalidate_the_captured_context(cx: &mut TestAppContext) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(21))
            .into_parts();
        let (shell, cx) = cx.add_window_view(|_, cx| Shell::new(runtime, cx));
        wait_for_runtime_state(&shell, "running", cx).await;
        let control = only_runtime_control(&shell, cx);
        control
            .execute_fixture_module(
                "file:///fixtures/context-validation-command.js",
                r#"
                    import { commands } from "knot:editor";
                    await commands.register("knot.fixture.context-validation", async () => {
                      await globalThis.__knotFixtureDelay(25);
                    });
                "#,
            )
            .await
            .unwrap();
        let target = cx.read(|cx| {
            shell
                .read(cx)
                .command_catalog
                .resolve_extension("knot.fixture.context-validation")
                .unwrap()
        });
        invoke_extension_target_in_window(&shell, target, "knot.fixture.context-validation", cx);

        shell.update_in(cx, |shell, _window, cx| {
            let command = shell.active_command.as_ref().unwrap().clone();
            let buffer = command.context.buffer.unwrap();
            let revision = shell
                .buffer_registry
                .resolve(buffer)
                .unwrap()
                .read_with(cx, |model, _| model.revision());
            let request = HostRequest {
                extension: target.extension,
                lifecycle: target.lifecycle,
                id: RequestId::new(1),
                invocation: Some(command.id),
                operation: HostOperation::ApplyEdits {
                    buffer,
                    edits: Vec::new(),
                    if_revision: revision,
                },
            };
            assert_eq!(shell.validate_command_mutation(&request, cx), Ok(()));

            let mut stale = request.clone();
            stale.invocation = Some(CommandInvocationId::new(999));
            assert_eq!(
                shell.validate_command_mutation(&stale, cx),
                Err(HostRequestError::Cancelled)
            );

            stale = request.clone();
            stale.lifecycle = crate::host::protocol::ExtensionLifecycleId::new(999);
            assert_eq!(
                shell.validate_command_mutation(&stale, cx),
                Err(HostRequestError::Cancelled)
            );

            shell.cancelled_commands.insert(command.id);
            assert_eq!(
                shell.validate_command_mutation(&request, cx),
                Err(HostRequestError::Cancelled)
            );
            shell.cancelled_commands.remove(&command.id);

            let live_focus = command.context.focus.clone();
            let dead_focus = {
                let focus = cx.focus_handle();
                focus.downgrade()
            };
            shell.active_command.as_mut().unwrap().context.focus = dead_focus;
            assert_eq!(
                shell.validate_command_mutation(&request, cx),
                Err(HostRequestError::Cancelled)
            );
            shell.active_command.as_mut().unwrap().context.focus = live_focus;

            let live_workspace = command.context.workspace.clone();
            shell.active_command.as_mut().unwrap().context.workspace = WeakEntity::new_invalid();
            assert_eq!(
                shell.validate_command_mutation(&request, cx),
                Err(HostRequestError::Cancelled)
            );
            shell.active_command.as_mut().unwrap().context.workspace = live_workspace;

            shell.close_buffer(buffer, cx);
            assert_eq!(
                shell.validate_command_mutation(&request, cx),
                Err(HostRequestError::BufferClosed)
            );
        });
        wait_for_command_state(&shell, "completed", cx).await;
    }

    #[gpui::test]
    async fn command_failures_have_structured_outcomes(cx: &mut TestAppContext) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(22))
            .into_parts();
        let (shell, cx) = cx.add_window_view(|_, cx| Shell::new(runtime, cx));
        wait_for_runtime_state(&shell, "running", cx).await;
        let control = only_runtime_control(&shell, cx);
        control
            .execute_fixture_module(
                "file:///fixtures/command-outcomes.js",
                r#"
                    import { commands } from "knot:editor";
                    await commands.register("knot.fixture.invalid-argument", (context) => {
                      if (typeof context.arguments !== "string") {
                        commands.invalidArguments("expected a string argument");
                      }
                    });
                    await commands.register("knot.fixture.handler-failure", () => {
                      throw new Error("expected handler failure");
                    });
                "#,
            )
            .await
            .unwrap();

        for (index, (name, expected_state)) in [
            ("knot.fixture.invalid-argument", "invalid-argument"),
            ("knot.fixture.handler-failure", "handler-failure"),
        ]
        .into_iter()
        .enumerate()
        {
            let target = cx.read(|cx| {
                shell
                    .read(cx)
                    .command_catalog
                    .resolve_extension(name)
                    .unwrap()
            });
            let execution = shell.update_in(cx, |shell, window, cx| {
                let focus = shell.editor.focus_handle(cx);
                window.focus(&focus);
                shell
                    .invoke_command_target(
                        target,
                        Command {
                            name: name.into(),
                            arguments: CommandArgumentValue::Null,
                        },
                        super::CommandOrigin {
                            window: window.window_handle(),
                            focus: focus.downgrade(),
                            buffer: shell.buffer_registry.active_handle(),
                        },
                        cx,
                    )
                    .unwrap()
            });
            assert_eq!(execution.id, CommandInvocationId::new(index as u64 + 1));
            wait_for_command_state(&shell, expected_state, cx).await;
            let recorded = cx.read(|cx| shell.read(cx).command_outcome.clone().unwrap());
            assert_eq!(execution.completion.await.unwrap(), recorded);
            match &recorded {
                CommandOutcome::InvalidArgument { message } => {
                    assert!(message.contains("expected a string argument"));
                }
                CommandOutcome::HandlerFailure { message } => {
                    assert!(message.contains("expected handler failure"));
                }
                outcome => panic!("unexpected command outcome: {outcome:?}"),
            }
        }
    }

    #[gpui::test]
    async fn rejected_commands_return_structured_outcomes(cx: &mut TestAppContext) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(23))
            .into_parts();
        let (shell, cx) = cx.add_window_view(|_, cx| Shell::new(runtime, cx));
        wait_for_runtime_state(&shell, "running", cx).await;

        shell.update_in(cx, |shell, window, cx| {
            window.blur();
            assert!(matches!(
                shell.invoke_fixture_command(window, cx),
                Err(CommandOutcome::InvalidTarget)
            ));

            let target = shell
                .command_catalog
                .resolve_extension("knot.fixture.edit")
                .unwrap();
            shell
                .command_catalog
                .unregister(target.registration, target.extension, target.lifecycle)
                .unwrap();
            let focus = shell.editor.focus_handle(cx);
            window.focus(&focus);
            assert!(matches!(
                shell.invoke_fixture_command(window, cx),
                Err(CommandOutcome::Unavailable)
            ));
        });
    }

    #[gpui::test]
    async fn cancelling_before_the_awaited_command_request_prevents_its_edit(
        cx: &mut TestAppContext,
    ) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(11))
            .into_parts();
        let (shell, cx) = cx.add_window_view(|_, cx| Shell::new(runtime, cx));
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

        let execution = shell.update_in(cx, |shell, window, cx| {
            let focus = shell.editor.focus_handle(cx);
            window.focus(&focus);
            let execution = shell.invoke_fixture_command(window, cx).unwrap();
            shell.cancel_active_command(cx);
            execution
        });
        wait_for_command_state(&shell, "cancelled", cx).await;
        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::Cancelled
        );
        assert_eq!(
            cx.read(|cx| shell.read(cx).command_outcome.clone()),
            Some(CommandOutcome::Cancelled)
        );

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
        let (shell, cx) = cx.add_window_view(|_, cx| {
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
                    shell.command_catalog.resolve_extension(name).unwrap(),
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
                        arguments: CommandArgumentValue::Null,
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
                .command_catalog
                .resolve_extension("knot.fixture.disposed")
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
        shell.update_in(cx, |shell, window, cx| {
            let focus = shell.editor.focus_handle(cx);
            window.focus(&focus);
            let _ = shell.invoke_fixture_command(window, cx);
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
