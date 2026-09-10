//! Knot's gpui application shell.
//!
//! The shell contains resizable explorer, editor, and outline panes plus a
//! status area for the active buffer and extension runtime.

use gpui::{prelude::FluentBuilder, *};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use crate::host::{
    ExtensionRequestInbox, ExtensionRuntimeControl, ExtensionRuntimeParts, ExtensionRuntimeThread,
    V8Host,
    protocol::{
        BufferChange, BufferHandle, ByteRange, Command, CommandArgumentValue, CommandInvocation,
        CommandInvocationId, CommandInvokeDispatch, CommandOutcome, CompletionProviderError,
        CompletionResponse, ExtensionId, HostOperation, HostRequest, HostRequestError,
        HostResponse, HostResponseValue, TreeChildrenResponse, TreeProviderError,
        TreeProviderRegistrationId,
    },
};

mod command_palette;
mod completion;
mod documents;
mod editor;
mod entry;
mod filesystem;
mod history;
pub mod model;
mod open;
mod product;
mod product_commands;
mod resource;
mod search_results;
mod terminal_view;
mod tree_view;
mod workbench;
mod workspace;
mod workspace_tree;

use command_palette::{CommandPalette, CommandPaletteEntry, CommandPaletteEvent};
use completion::CompletionProviderRegistry;
use documents::{ApplicationDocuments, DocumentCollection, DocumentId, DocumentState};
use editor::{
    EditorContributionAction, EditorRenderingOptions, EditorView, seed_fixture_contributions,
};
use filesystem::{
    FileSystemProviderRegistry, LocalFileSystemProvider, MemoryFileSystemProvider, ResourceError,
    ResourceKind, ResourceStat,
};
use model::{
    BufferAccessError, BufferModel, BufferRegistry, BufferSubscriptionRegistry, CommandCatalog,
    ContributionError, ContributionSource,
};
use resource::ResourceUri;
use search_results::{ACTIVATE_SEARCH_RESULT_COMMAND, SearchResultsController};
use terminal_view::TerminalView;
use tree_view::{TreeProviderIdentity, TreeView, TreeViewEvent, TreeViewRegistrationError};
use workspace::{WorkspaceSnapshot, WorkspaceState};
use workspace_tree::{
    WorkspaceTree, WorkspaceTreeEvent, WorkspaceTreeRequest, WorkspaceTreeResponse,
};

actions!(
    knot,
    [
        Quit,
        ToggleActiveKeymap,
        ActivateTransientKeymap,
        CancelTransientKeymap,
        CompletionPrevious,
        CompletionNext,
        CompletionAccept,
        CompletionDismiss
    ]
);

const MIN_PANE: f32 = 120.;
const MIN_TERMINAL_HEIGHT: f32 = 96.;
const MIN_EDITOR_HEIGHT: f32 = 160.;
const DEFAULT_FIXTURE_NAME: &str = "rust_sample";
const DIAGNOSTIC_COMMAND: &str = "knot.diagnostic.command-context";
const FIXTURE_SEARCH_COMMAND: &str = "knot.fixture.search";
const FIXTURE_EDIT_COMMAND: &str = "knot.fixture.edit";
const SHOW_COMPLETIONS_COMMAND: &str = "editor.show-completions";
const SWAP_COMPLETION_SURFACE_COMMAND: &str = "knot.fixture.swap-completion-surface";
const FIXTURE_EDIT_ARGUMENT: &str = "// fixture command\n";
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
globalThis.knotFixtureCommandInvocations = [];
await globalThis.knotActiveBuffer.onDidChange((event) => {
  globalThis.knotFixtureEvents.push({
    beforeRevision: event.beforeRevision,
    revision: event.revision,
    edits: event.edits.length,
  });
});
await commands.register("knot.fixture.edit", async (context) => {
  if (!context.buffer) throw new Error("Knot has no active editor buffer");
  globalThis.knotFixtureCommandInvocations.push({
    arguments: context.arguments,
    capturedActiveBuffer: context.buffer === globalThis.knotActiveBuffer,
  });
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
globalThis.knotFastCompletionRegistration =
  await editor.registerCompletionProvider("fast-provider", {
    async provideCompletions(context) {
      await globalThis.__knotFixtureDelay(120);
      const prefix = context.prefix;
      return [
        { label: `${prefix}Alpha`, insertText: `${prefix}Alpha` },
        { label: `${prefix}Shared`, insertText: `${prefix}Shared` },
        { label: `${prefix.toUpperCase()}Case`, insertText: `${prefix}Case` },
      ];
    },
  });
"#;

const SLOW_COMPLETION_FIXTURE_SOURCE: &str = r#"
import { commands, editor } from "knot:editor";

let failNextRequest = false;
await commands.register("knot.fixture.completion-fail-next", async () => {
  failNextRequest = true;
});
globalThis.knotSlowCompletionRegistration =
  await editor.registerCompletionProvider("slow-provider", {
    async provideCompletions(context) {
      await globalThis.__knotFixtureDelay(1500);
      if (failNextRequest) {
        failNextRequest = false;
        throw new Error("expected one-shot slow completion failure");
      }
      const prefix = context.prefix;
      return [
        { label: `${prefix}Aardvark`, insertText: `${prefix}Aardvark` },
        { label: `${prefix}Shared duplicate`, insertText: `${prefix}Shared` },
        { label: `${prefix}Zeta`, insertText: `${prefix}Zeta` },
      ];
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

struct CommandCompletionState {
    sender: Option<tokio::sync::oneshot::Sender<CommandOutcome>>,
}

#[derive(Clone)]
struct CommandCompletion {
    state: Arc<Mutex<CommandCompletionState>>,
}

impl PartialEq for CommandCompletion {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }
}

impl CommandCompletion {
    fn new() -> (Self, tokio::sync::oneshot::Receiver<CommandOutcome>) {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        (
            Self {
                state: Arc::new(Mutex::new(CommandCompletionState {
                    sender: Some(sender),
                })),
            },
            receiver,
        )
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
}

#[derive(Clone, PartialEq, Action)]
#[action(namespace = knot, no_json)]
#[allow(
    dead_code,
    reason = "command dispatch uses this adapter when native focus routing is connected"
)]
struct CommandAction {
    invocation: CommandInvocationId,
    command: Command,
    context: CapturedInvocationContext,
}

#[derive(Clone, PartialEq, Action)]
#[action(namespace = knot, no_json)]
struct KeybindingCommand {
    command: Command,
}

fn fixed_command_bindings() -> [KeyBinding; 4] {
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
            "ctrl-space",
            KeybindingCommand {
                command: Command {
                    name: SHOW_COMPLETIONS_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
            },
            Some(EDITOR_KEY_CONTEXT),
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

fn completion_bindings() -> [KeyBinding; 5] {
    [
        KeyBinding::new("up", CompletionPrevious, Some("editor_completion")),
        KeyBinding::new("down", CompletionNext, Some("editor_completion")),
        KeyBinding::new("enter", CompletionAccept, Some("editor_completion")),
        KeyBinding::new("escape", CompletionDismiss, Some("editor_completion")),
        KeyBinding::new(
            "ctrl-alt-s",
            KeybindingCommand {
                command: Command {
                    name: SWAP_COMPLETION_SURFACE_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
            },
            Some("editor_completion"),
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
            return Err(CommandOutcome::InvalidTarget);
        };
        if self.context.window != window.window_handle() {
            return Err(CommandOutcome::InvalidTarget);
        }
        focus.dispatch_action(self, window, cx);
        Ok(())
    }

    fn record_diagnostic(&self, surface: CommandSurfaceKind, cx: &mut App) {
        let Some(workspace) = self.context.workspace.upgrade() else {
            return;
        };
        let has_buffer = self.context.buffer.is_some();
        let _ = workspace.update(cx, |shell, cx| {
            shell.record_command_diagnostic(self.invocation, surface, has_buffer, cx);
        });
    }

    fn show_completions(&self, editor: Entity<EditorView>, cx: &mut App) {
        let Some(workspace) = self.context.workspace.upgrade() else {
            return;
        };
        let invocation = self.invocation;
        let buffer = self.context.buffer;
        cx.defer(move |cx| {
            let _ = workspace.update(cx, |shell, cx| {
                shell.start_completion(invocation, editor, buffer, cx);
            });
        });
    }

    fn complete_native(&self, outcome: CommandOutcome, cx: &mut App) {
        let Some(workspace) = self.context.workspace.upgrade() else {
            return;
        };
        let invocation = self.invocation;
        let _ = workspace.update(cx, |shell, cx| {
            shell.finish_command_invocation(invocation, outcome, cx);
        });
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InvocationTarget {
    Native,
    Extension(model::CommandTarget),
    Unavailable,
}

struct CommandInvocationNode {
    command: Command,
    target: InvocationTarget,
    context: CapturedInvocationContext,
    parent: Option<CommandInvocationId>,
    children: HashSet<CommandInvocationId>,
    completion: CommandCompletion,
    handler_outcome: Option<CommandOutcome>,
    started: bool,
    cancelled: bool,
}

#[allow(
    dead_code,
    reason = "command sources may observe the identity and await the structured result"
)]
struct CommandExecution {
    id: CommandInvocationId,
    completion: tokio::sync::oneshot::Receiver<CommandOutcome>,
}

enum ChildCommandAdmission {
    Await(CommandExecution),
    Inline {
        invocation: CommandInvocationId,
        registration: crate::host::protocol::CommandRegistrationId,
    },
}

/// Invisible drag-ghost view gpui renders while the drag is in progress.
/// Required by `on_drag`'s constructor; we don't want a visible ghost.
struct DragGhost;

impl Render for DragGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

#[derive(Debug)]
enum ResourceBufferError {
    Provider(ResourceError),
    InvalidUtf8 { uri: ResourceUri },
}

impl std::fmt::Display for ResourceBufferError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Provider(error) => error.fmt(formatter),
            Self::InvalidUtf8 { uri } => write!(formatter, "resource is not valid UTF-8: {uri}"),
        }
    }
}

impl From<ResourceError> for ResourceBufferError {
    fn from(error: ResourceError) -> Self {
        Self::Provider(error)
    }
}

type FilesystemFixture = (
    Arc<FileSystemProviderRegistry>,
    ResourceUri,
    ResourceUri,
    Arc<tokio::runtime::Runtime>,
);

fn build_filesystem_fixture() -> FilesystemFixture {
    static NATIVE_IO_RUNTIME: OnceLock<Arc<tokio::runtime::Runtime>> = OnceLock::new();
    let native_io_runtime = NATIVE_IO_RUNTIME
        .get_or_init(|| {
            Arc::new(
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(1)
                    .enable_all()
                    .thread_name("knot-filesystem")
                    .build()
                    .expect("native filesystem runtime starts"),
            )
        })
        .clone();

    let memory_root = ResourceUri::parse("mem://workspace/").expect("memory fixture root is valid");
    let memory_provider = Arc::new(
        MemoryFileSystemProvider::new(memory_root.clone())
            .expect("memory fixture provider accepts its root"),
    );
    let memory_root = memory_provider.root().clone();
    for directory in ["mem://workspace/src", "mem://workspace/notes"] {
        memory_provider
            .seed_directory(ResourceUri::parse(directory).expect("memory fixture URI is valid"))
            .expect("memory fixture directory can be seeded");
    }
    for (uri, bytes) in [
        (
            "mem://workspace/README.md",
            b"# Knot memory workspace\n".as_slice(),
        ),
        (
            "mem://workspace/src/main.rs",
            b"fn main() {\n    println!(\"memory workspace\");\n}\n".as_slice(),
        ),
        (
            "mem://workspace/notes/todo.txt",
            b"validate URI persistence\n".as_slice(),
        ),
        ("mem://workspace/invalid.bin", &[0xff, 0xfe][..]),
    ] {
        memory_provider
            .seed_file(
                ResourceUri::parse(uri).expect("memory fixture URI is valid"),
                bytes,
            )
            .expect("memory fixture file can be seeded");
    }

    static NEXT_LOCAL_FIXTURE: AtomicU64 = AtomicU64::new(1);
    let fixture_id = NEXT_LOCAL_FIXTURE.fetch_add(1, Ordering::Relaxed);
    let local_root_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(format!(
            "step13-local-workspace-{}-{fixture_id}",
            std::process::id()
        ));
    std::fs::create_dir_all(local_root_path.join("src"))
        .expect("local fixture directory can be created");
    std::fs::write(
        local_root_path.join("README.md"),
        b"# Knot local workspace\n",
    )
    .expect("local fixture can be seeded");
    std::fs::write(
        local_root_path.join("src/lib.rs"),
        b"pub fn fixture() -> &'static str { \"local workspace\" }\n",
    )
    .expect("nested local fixture can be seeded");
    let local_provider = Arc::new(
        LocalFileSystemProvider::new(&local_root_path, native_io_runtime.clone())
            .expect("local fixture provider accepts its root"),
    );
    let local_root = local_provider.root().clone();

    let mut providers = FileSystemProviderRegistry::new();
    providers
        .register("mem", memory_provider)
        .expect("memory scheme is unique");
    providers
        .register("file", local_provider)
        .expect("file scheme is unique");
    (
        Arc::new(providers),
        memory_root,
        local_root,
        native_io_runtime,
    )
}

#[cfg(test)]
fn shared_test_filesystem_fixture() -> FilesystemFixture {
    static FIXTURE: OnceLock<FilesystemFixture> = OnceLock::new();
    FIXTURE.get_or_init(build_filesystem_fixture).clone()
}

struct Shell {
    selected_document: DocumentId,
    source_document: DocumentId,
    filesystem_providers: Arc<FileSystemProviderRegistry>,
    workspace: WorkspaceState,
    workspace_tree: Entity<WorkspaceTree>,
    memory_workspace_root: ResourceUri,
    local_workspace_root: ResourceUri,
    resource_operation_generation: u64,
    resource_status: SharedString,
    latest_resource_error: Option<SharedString>,
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
    completion_providers: CompletionProviderRegistry,
    next_tree_registration: u64,
    next_command_invocation: u64,
    command_invocations: HashMap<CommandInvocationId, CommandInvocationNode>,
    queued_command_roots: VecDeque<CommandInvocationId>,
    active_command_root: Option<CommandInvocationId>,
    command_state: SharedString,
    command_outcome: Option<CommandOutcome>,
    command_diagnostic: Option<CommandDiagnostic>,
    command_palette: Option<Entity<CommandPalette<CommandOrigin>>>,
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
    _runtime_execution_tasks: Vec<Task<()>>,
    _resource_tasks: Vec<Task<()>>,
    _native_io_runtime: Arc<tokio::runtime::Runtime>,
    _model_subscriptions: Vec<Subscription>,
    _editor_action_subscriptions: Vec<Subscription>,
    _tree_view_subscription: Subscription,
    _workspace_tree_subscription: Subscription,
    _heartbeat_task: Task<()>,
    /// Name of the fixture currently loaded (shown in a thin status header
    /// above the editor so it's visible at a glance which fixture is running).
    fixture_name: String,
}

impl Shell {
    fn document_collection(cx: &App) -> Entity<DocumentCollection> {
        cx.global::<ApplicationDocuments>().0.clone()
    }

    fn documents(cx: &App) -> &DocumentCollection {
        cx.global::<ApplicationDocuments>().0.read(cx)
    }

    /// Construct the shell with the default fixture.
    #[cfg(test)]
    fn new(runtime: ExtensionRuntimeParts, cx: &mut Context<Self>) -> Self {
        Self::new_with_runtimes(vec![runtime], cx)
    }

    #[cfg(test)]
    fn new_with_runtimes(runtimes: Vec<ExtensionRuntimeParts>, cx: &mut Context<Self>) -> Self {
        Self::new_with_runtimes_fixture_and_filesystems(
            runtimes,
            DEFAULT_FIXTURE_NAME.into(),
            shared_test_filesystem_fixture(),
            cx,
        )
    }

    #[cfg(test)]
    fn new_with_resource_fixture(runtime: ExtensionRuntimeParts, cx: &mut Context<Self>) -> Self {
        Self::new_with_runtimes_fixture_and_filesystems(
            vec![runtime],
            DEFAULT_FIXTURE_NAME.into(),
            build_filesystem_fixture(),
            cx,
        )
    }

    /// Construct the shell with an explicitly selected fixture. Fixture
    /// resolution is relative to the crate root so the binary runs from any
    /// current working directory.
    fn new_with_runtimes_and_fixture(
        runtimes: Vec<ExtensionRuntimeParts>,
        fixture_name: String,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with_runtimes_fixture_and_filesystems(
            runtimes,
            fixture_name,
            build_filesystem_fixture(),
            cx,
        )
    }

    fn new_with_runtimes_fixture_and_filesystems(
        runtimes: Vec<ExtensionRuntimeParts>,
        fixture_name: String,
        filesystems: FilesystemFixture,
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
        let documents = if let Some(documents) = cx.try_global::<ApplicationDocuments>() {
            documents.0.clone()
        } else {
            let documents = cx.new(|_| DocumentCollection::new());
            cx.set_global(ApplicationDocuments(documents.clone()));
            documents
        };
        let source_document = documents.update(cx, |documents, _| {
            documents.create_generated(format!("{fixture_name}.kfx"), model.clone())
        });
        let (filesystem_providers, memory_workspace_root, local_workspace_root, native_io_runtime) =
            filesystems;
        let workspace = WorkspaceState::new(memory_workspace_root.clone());
        let workspace_tree = cx.new(|cx| WorkspaceTree::new(workspace.snapshot(), cx));
        let workspace_tree_subscription = cx.subscribe(
            &workspace_tree,
            |this, _tree, event: &WorkspaceTreeEvent, cx| {
                this.dispatch_workspace_tree_event(event.clone(), cx);
            },
        );
        #[cfg(not(test))]
        {
            let workspace_tree_to_load = workspace_tree.clone();
            cx.defer(move |cx| {
                workspace_tree_to_load.update(cx, |tree, cx| tree.load(cx));
            });
        }
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
        let mut extra_runtime_executions = Vec::new();
        for ExtensionRuntimeParts {
            control,
            requests,
            thread,
        } in runtimes
        {
            extra_runtime_executions.push(control.execute_fixture_module(
                "file:///fixtures/slow-completion-provider.js",
                SLOW_COMPLETION_FIXTURE_SOURCE,
            ));
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
        let mut runtime_execution_tasks = vec![runtime_execution_task];
        for execution in extra_runtime_executions {
            runtime_execution_tasks.push(cx.spawn(async move |this, cx| {
                let result = execution.await;
                let _ = this.update(cx, |this, cx| {
                    if let Err(error) = result {
                        this.runtime_state = "failed".into();
                        this.latest_runtime_error = Some(format!("{error:?}").into());
                    }
                    cx.notify();
                });
            }));
        }
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
        command_catalog
            .register_native(
                FIXTURE_SEARCH_COMMAND.into(),
                "Search captured buffer".into(),
            )
            .expect("fixture search command name is unique");
        command_catalog
            .register_native(
                SHOW_COMPLETIONS_COMMAND.into(),
                "Show editor completions".into(),
            )
            .expect("completion command name is unique");
        command_catalog
            .register_native(
                SWAP_COMPLETION_SURFACE_COMMAND.into(),
                "Swap completion fixture surface".into(),
            )
            .expect("completion surface fixture command name is unique");
        Shell {
            selected_document: source_document,
            source_document,
            filesystem_providers,
            workspace,
            workspace_tree,
            memory_workspace_root,
            local_workspace_root,
            resource_operation_generation: 0,
            resource_status: "ready".into(),
            latest_resource_error: None,
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
            completion_providers: CompletionProviderRegistry::new(),
            next_tree_registration: 1,
            next_command_invocation: 1,
            command_invocations: HashMap::new(),
            queued_command_roots: VecDeque::new(),
            active_command_root: None,
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
            _runtime_execution_tasks: runtime_execution_tasks,
            _resource_tasks: Vec::new(),
            _native_io_runtime: native_io_runtime,
            _model_subscriptions: vec![model_subscription],
            _editor_action_subscriptions: vec![
                editor_action_subscription,
                secondary_editor_action_subscription,
            ],
            _tree_view_subscription: tree_view_subscription,
            _workspace_tree_subscription: workspace_tree_subscription,
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
                if let HostOperation::InvokeCommand { command } = request.operation.clone() {
                    let caller = (request.extension, request.lifecycle);
                    let request_id = request.id;
                    let admission = if let Some(parent) = request.invocation {
                        this.update(cx, |this, cx| {
                            this.enqueue_command_child(parent, command, caller, cx)
                        })
                        .unwrap_or(Err(CommandOutcome::Unavailable))
                    } else {
                        cx.update(|cx| {
                            let Some(shell) = this.upgrade() else {
                                return Err(CommandOutcome::Unavailable);
                            };
                            let windows = cx.window_stack().unwrap_or_else(|| cx.windows());
                            for window_handle in windows {
                                let command = command.clone();
                                let execution = cx
                                    .update_window(window_handle, |_, window, cx| {
                                        if window.root::<Shell>().flatten().as_ref() != Some(&shell)
                                        {
                                            return None;
                                        }
                                        shell.update(cx, |shell, cx| {
                                            shell.command_origin(window, cx).map(|origin| {
                                                shell.enqueue_command_root(
                                                    command,
                                                    origin,
                                                    Some(caller),
                                                    cx,
                                                )
                                            })
                                        })
                                    })
                                    .ok()
                                    .flatten();
                                if let Some(execution) = execution {
                                    return Ok(ChildCommandAdmission::Await(execution));
                                }
                            }
                            Err(CommandOutcome::InvalidTarget)
                        })
                        .unwrap_or(Err(CommandOutcome::Unavailable))
                    };
                    match admission {
                        Ok(ChildCommandAdmission::Inline {
                            invocation,
                            registration,
                        }) => {
                            let response = HostResponse {
                                extension: caller.0,
                                lifecycle: caller.1,
                                id: request_id,
                                result: Ok(HostResponseValue::CommandInvoked {
                                    dispatch: CommandInvokeDispatch::Inline {
                                        invocation,
                                        registration,
                                    },
                                }),
                            };
                            if let Err(error) = control.respond(response) {
                                failure = Some(format!("runtime response failed: {error:?}"));
                                break;
                            }
                        }
                        Ok(ChildCommandAdmission::Await(execution)) => {
                            let response_control = control.clone();
                            cx.spawn(async move |_| {
                                let outcome = execution
                                    .completion
                                    .await
                                    .unwrap_or(CommandOutcome::Cancelled);
                                let _ = response_control.respond(HostResponse {
                                    extension: caller.0,
                                    lifecycle: caller.1,
                                    id: request_id,
                                    result: Ok(HostResponseValue::CommandInvoked {
                                        dispatch: CommandInvokeDispatch::Outcome { outcome },
                                    }),
                                });
                            })
                            .detach();
                        }
                        Err(outcome) => {
                            let response = HostResponse {
                                extension: caller.0,
                                lifecycle: caller.1,
                                id: request_id,
                                result: Ok(HostResponseValue::CommandInvoked {
                                    dispatch: CommandInvokeDispatch::Outcome { outcome },
                                }),
                            };
                            if let Err(error) = control.respond(response) {
                                failure = Some(format!("runtime response failed: {error:?}"));
                                break;
                            }
                        }
                    }
                    continue;
                }
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
                this.cancel_command_lifecycle(extension, lifecycle, cx);
                this.extension_controls.remove(&(extension, lifecycle));
                this.command_catalog.remove_lifecycle(extension, lifecycle);
                let removed_completion_providers = this
                    .completion_providers
                    .remove_lifecycle(extension, lifecycle);
                for registration in removed_completion_providers {
                    this.cancel_completion_provider(registration, cx);
                }
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
        if let HostOperation::CompleteInlineCommand {
            invocation,
            outcome,
        } = request.operation.clone()
        {
            let authorized = request.invocation == Some(invocation)
                && self
                    .command_invocations
                    .get(&invocation)
                    .is_some_and(|node| {
                        matches!(
                            node.target,
                            InvocationTarget::Extension(target)
                                if target.extension == request.extension
                                    && target.lifecycle == request.lifecycle
                        )
                    });
            let result = if authorized {
                self.finish_command_invocation(invocation, outcome, cx);
                Ok(HostResponseValue::InlineCommandCompleted { invocation })
            } else {
                Err(HostRequestError::Cancelled)
            };
            return HostResponse {
                extension: request.extension,
                lifecycle: request.lifecycle,
                id: request.id,
                result,
            };
        }
        let cancelled = request.invocation.is_some_and(|invocation| {
            self.command_invocations
                .get(&invocation)
                .is_none_or(|command| command.cancelled)
        });
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
            HostOperation::CompleteInlineCommand { .. } => unreachable!(),
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
            HostOperation::RegisterCompletionProvider { label } => {
                let registration =
                    self.completion_providers
                        .register(request.extension, request.lifecycle, label);
                Ok(HostResponseValue::CompletionProviderRegistered { registration })
            }
            HostOperation::UnregisterCompletionProvider { registration } => {
                if self.completion_providers.unregister(
                    registration,
                    request.extension,
                    request.lifecycle,
                ) {
                    self.cancel_completion_provider(registration, cx);
                    Ok(HostResponseValue::CompletionProviderUnregistered { registration })
                } else {
                    Err(HostRequestError::CompletionProviderNotFound)
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
        let Some(command) = self.command_invocations.get(&invocation) else {
            return Err(HostRequestError::Cancelled);
        };
        let InvocationTarget::Extension(target) = command.target else {
            return Err(HostRequestError::Cancelled);
        };
        if target.extension != request.extension
            || target.lifecycle != request.lifecycle
            || command.cancelled
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
                    let _ = self.enqueue_command_root(
                        Command {
                            name: command.into(),
                            arguments: CommandArgumentValue::Null,
                        },
                        CommandOrigin {
                            window,
                            focus,
                            buffer: None,
                        },
                        None,
                        cx,
                    );
                } else {
                    self.record_command_outcome(CommandOutcome::Unavailable, cx);
                }
            }
        }
    }

    fn start_completion(
        &mut self,
        invocation: CommandInvocationId,
        editor: Entity<EditorView>,
        buffer: Option<BufferHandle>,
        cx: &mut Context<Self>,
    ) {
        let Some(buffer) = buffer else {
            self.finish_command_invocation(invocation, CommandOutcome::InvalidTarget, cx);
            return;
        };
        let model_matches = self
            .buffer_registry
            .resolve(buffer)
            .is_ok_and(|model| model == *editor.read(cx).model());
        if !model_matches {
            self.finish_command_invocation(invocation, CommandOutcome::InvalidTarget, cx);
            return;
        }

        let requests = editor.update(cx, |editor, cx| {
            editor.start_completion(buffer, self.completion_providers.snapshot(), cx)
        });
        for (provider, request) in requests {
            let Some(control) = self
                .extension_controls
                .get(&(provider.extension, provider.lifecycle))
                .cloned()
            else {
                continue;
            };
            let callback = control.request_completions(request.clone());
            let weak_editor = editor.downgrade();
            let generation = request.generation;
            let registration = request.registration;
            let task = cx.spawn(async move |this, cx| {
                let response = callback.await.unwrap_or_else(|error| CompletionResponse {
                    registration: request.registration,
                    revision: request.revision,
                    generation: request.generation,
                    result: Err(CompletionProviderError {
                        message: format!("{error:?}"),
                    }),
                });
                let _ = this.update(cx, |this, cx| {
                    if !this.completion_providers.owns(
                        provider.id,
                        provider.extension,
                        provider.lifecycle,
                    ) || !this
                        .extension_controls
                        .contains_key(&(provider.extension, provider.lifecycle))
                    {
                        return;
                    }
                    let Some(editor) = weak_editor.upgrade() else {
                        return;
                    };
                    if this.buffer_registry.resolve(request.buffer).is_err() {
                        return;
                    }
                    editor.update(cx, |editor, cx| {
                        editor.apply_completion_response(request.buffer, response, cx);
                    });
                });
            });
            editor.update(cx, |editor, _| {
                editor.retain_completion_task(generation, registration, task);
            });
        }
        self.finish_command_invocation(invocation, CommandOutcome::Completed, cx);
    }

    fn cancel_completion_provider(
        &mut self,
        registration: crate::host::protocol::CompletionProviderRegistrationId,
        cx: &mut Context<Self>,
    ) {
        self.editor.update(cx, |editor, cx| {
            editor.cancel_completion_provider(registration, cx);
        });
        self.secondary_editor.update(cx, |editor, cx| {
            editor.cancel_completion_provider(registration, cx);
        });
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
        let Some(focus) = window.focused(cx) else {
            let outcome = CommandOutcome::InvalidTarget;
            self.record_command_outcome(outcome.clone(), cx);
            return Err(outcome);
        };
        if self
            .command_catalog
            .resolve_extension("knot.fixture.edit")
            .is_err()
        {
            let outcome = CommandOutcome::Unavailable;
            self.record_command_outcome(outcome.clone(), cx);
            return Err(outcome);
        }
        Ok(self.enqueue_command_root(
            Command {
                name: "knot.fixture.edit".into(),
                arguments: CommandArgumentValue::Null,
            },
            CommandOrigin {
                window: window.window_handle(),
                focus: focus.downgrade(),
                buffer: self.buffer_registry.active_handle(),
            },
            None,
            cx,
        ))
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
        let _ = self.enqueue_command_root(
            Command {
                name: action.command.as_str().into(),
                arguments: CommandArgumentValue::Null,
            },
            CommandOrigin {
                window: action.window,
                focus: action.focus.clone(),
                buffer: Some(buffer),
            },
            None,
            cx,
        );
    }

    fn activate_search_result(
        &mut self,
        result_range: ByteRange,
        window_handle: Option<AnyWindowHandle>,
        cx: &mut Context<Self>,
    ) {
        let documents = Self::documents(cx);
        let Some(controller) = documents
            .get(self.selected_document)
            .and_then(|document| document.search_results())
        else {
            return;
        };
        let Ok(target) = controller.resolve_target(result_range, cx) else {
            return;
        };
        if target.source != self.source_document
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

    fn enqueue_command_root(
        &mut self,
        command: Command,
        origin: CommandOrigin,
        caller: Option<(ExtensionId, crate::host::protocol::ExtensionLifecycleId)>,
        cx: &mut Context<Self>,
    ) -> CommandExecution {
        let id = self.allocate_command_invocation();
        let (completion, receiver) = CommandCompletion::new();
        let execution = CommandExecution {
            id,
            completion: receiver,
        };
        let target = match self.command_catalog.resolve(command.name.as_ref()) {
            Ok(model::CommandTargetKind::Native) => InvocationTarget::Native,
            Ok(model::CommandTargetKind::Extension(target)) => {
                if caller == Some((target.extension, target.lifecycle)) {
                    InvocationTarget::Unavailable
                } else {
                    InvocationTarget::Extension(target)
                }
            }
            Err(_) => InvocationTarget::Unavailable,
        };
        let context = CapturedInvocationContext {
            window: origin.window,
            workspace: cx.entity().downgrade(),
            focus: origin.focus,
            buffer: origin.buffer,
        };
        self.command_invocations.insert(
            id,
            CommandInvocationNode {
                command,
                target,
                context,
                parent: None,
                children: HashSet::new(),
                completion,
                handler_outcome: None,
                started: false,
                cancelled: false,
            },
        );
        self.queued_command_roots.push_back(id);
        self.start_next_command_root(cx);
        execution
    }

    #[cfg(test)]
    fn invoke_command_target(
        &mut self,
        target: model::CommandTarget,
        command: Command,
        origin: CommandOrigin,
        cx: &mut Context<Self>,
    ) -> Result<CommandExecution, CommandOutcome> {
        if self
            .command_catalog
            .resolve_extension(command.name.as_ref())
            != Ok(target)
        {
            return Err(CommandOutcome::Unavailable);
        }
        Ok(self.enqueue_command_root(command, origin, None, cx))
    }

    #[cfg(test)]
    fn prepare_command_action(
        &mut self,
        command: Command,
        origin: CommandOrigin,
        _window: &Window,
        cx: &mut Context<Self>,
    ) -> Result<CommandAction, CommandOutcome> {
        if origin.focus.upgrade().is_none()
            || !cx.windows().contains(&origin.window)
            || origin
                .buffer
                .is_some_and(|buffer| self.buffer_registry.resolve(buffer).is_err())
        {
            return Err(CommandOutcome::InvalidTarget);
        }
        Ok(CommandAction {
            invocation: self.allocate_command_invocation(),
            command,
            context: CapturedInvocationContext {
                window: origin.window,
                workspace: cx.entity().downgrade(),
                focus: origin.focus,
                buffer: origin.buffer,
            },
        })
    }

    fn enqueue_command_child(
        &mut self,
        parent: CommandInvocationId,
        command: Command,
        caller: (ExtensionId, crate::host::protocol::ExtensionLifecycleId),
        cx: &mut Context<Self>,
    ) -> Result<ChildCommandAdmission, CommandOutcome> {
        let Some(parent_node) = self.command_invocations.get(&parent) else {
            return Err(CommandOutcome::Unavailable);
        };
        if parent_node.cancelled
            || parent_node.handler_outcome.is_some()
            || !parent_node.children.is_empty()
            || !matches!(
                parent_node.target,
                InvocationTarget::Extension(target)
                    if (target.extension, target.lifecycle) == caller
            )
        {
            return Err(if parent_node.cancelled {
                CommandOutcome::Cancelled
            } else {
                CommandOutcome::Unavailable
            });
        }
        let target = match self.command_catalog.resolve(command.name.as_ref()) {
            Ok(model::CommandTargetKind::Native) => InvocationTarget::Native,
            Ok(model::CommandTargetKind::Extension(target)) => InvocationTarget::Extension(target),
            Err(_) => return Err(CommandOutcome::Unavailable),
        };
        let same_runtime = matches!(
            target,
            InvocationTarget::Extension(target)
                if (target.extension, target.lifecycle) == caller
        );
        let mut ancestor = Some(parent);
        while let Some(id) = ancestor {
            let node = self
                .command_invocations
                .get(&id)
                .expect("command ancestry remains active");
            if let InvocationTarget::Extension(ancestor_target) = node.target {
                if let InvocationTarget::Extension(child_target) = target {
                    let same_owner = (ancestor_target.extension, ancestor_target.lifecycle)
                        == (child_target.extension, child_target.lifecycle);
                    if same_owner
                        && (!same_runtime
                            || ancestor_target.registration == child_target.registration)
                    {
                        return Err(CommandOutcome::Unavailable);
                    }
                }
            }
            ancestor = node.parent;
        }
        let id = self.allocate_command_invocation();
        let (completion, receiver) = CommandCompletion::new();
        let context = self
            .command_invocations
            .get(&parent)
            .expect("validated parent remains active")
            .context
            .clone();
        self.command_invocations.insert(
            id,
            CommandInvocationNode {
                command,
                target,
                context,
                parent: Some(parent),
                children: HashSet::new(),
                completion,
                handler_outcome: None,
                started: same_runtime,
                cancelled: false,
            },
        );
        self.command_invocations
            .get_mut(&parent)
            .expect("validated parent remains active")
            .children
            .insert(id);
        if same_runtime {
            let InvocationTarget::Extension(target) = target else {
                unreachable!();
            };
            Ok(ChildCommandAdmission::Inline {
                invocation: id,
                registration: target.registration,
            })
        } else {
            self.start_command_invocation(id, cx);
            Ok(ChildCommandAdmission::Await(CommandExecution {
                id,
                completion: receiver,
            }))
        }
    }

    fn start_next_command_root(&mut self, cx: &mut Context<Self>) {
        if self.active_command_root.is_some() {
            return;
        }
        let Some(id) = self.queued_command_roots.pop_front() else {
            return;
        };
        self.active_command_root = Some(id);
        self.start_command_invocation(id, cx);
    }

    fn start_command_invocation(&mut self, id: CommandInvocationId, cx: &mut Context<Self>) {
        let Some(node) = self.command_invocations.get(&id) else {
            return;
        };
        if node.started {
            return;
        }
        let context = node.context.clone();
        let target_is_live = context.workspace.upgrade() == Some(cx.entity())
            && cx.windows().contains(&context.window)
            && context.focus.upgrade().is_some()
            && context
                .buffer
                .is_none_or(|buffer| self.buffer_registry.resolve(buffer).is_ok());
        if !target_is_live {
            self.finish_command_invocation(id, CommandOutcome::InvalidTarget, cx);
            return;
        }
        if node.target == InvocationTarget::Unavailable {
            self.finish_command_invocation(id, CommandOutcome::Unavailable, cx);
            return;
        }
        let node = self
            .command_invocations
            .get_mut(&id)
            .expect("validated invocation remains active");
        node.started = true;
        self.command_state = "running".into();
        self.command_outcome = None;
        let action = CommandAction {
            invocation: id,
            command: node.command.clone(),
            context: node.context.clone(),
        };
        let shell = cx.entity();
        let window_handle = node.context.window;
        cx.defer(move |cx| {
            let shell_for_window = shell.clone();
            let result = cx.update_window(window_handle, move |_, window, cx| {
                if let Err(outcome) = action.dispatch(window, cx) {
                    shell_for_window.update(cx, |shell, cx| {
                        shell.finish_command_invocation(id, outcome, cx);
                    });
                }
            });
            if result.is_err() {
                shell.update(cx, |shell, cx| {
                    shell.finish_command_invocation(id, CommandOutcome::InvalidTarget, cx);
                });
            }
        });
        cx.notify();
    }

    fn start_extension_command(
        &mut self,
        id: CommandInvocationId,
        target: model::CommandTarget,
        cx: &mut Context<Self>,
    ) {
        let Some(node) = self.command_invocations.get(&id) else {
            return;
        };
        let Some(control) = self
            .extension_controls
            .get(&(target.extension, target.lifecycle))
            .cloned()
        else {
            self.finish_command_invocation(id, CommandOutcome::Unavailable, cx);
            return;
        };
        let execution = control.invoke_command(
            CommandInvocation {
                id,
                registration: target.registration,
                extension: target.extension,
                lifecycle: target.lifecycle,
                arguments: node.command.arguments.clone(),
            },
            node.context.buffer,
        );
        cx.spawn(async move |this, cx| {
            let result = execution.await;
            let _ = this.update(cx, |this, cx| {
                let outcome = this.command_runtime_outcome(id, result);
                this.finish_command_invocation(id, outcome, cx);
            });
        })
        .detach();
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
        match self
            .command_invocations
            .get(&action.invocation)
            .map(|node| node.target)
        {
            Some(InvocationTarget::Extension(target)) => {
                self.start_extension_command(action.invocation, target, cx);
            }
            Some(InvocationTarget::Native) => {
                self.finish_command_invocation(action.invocation, CommandOutcome::Unavailable, cx);
            }
            Some(InvocationTarget::Unavailable) => {
                self.finish_command_invocation(action.invocation, CommandOutcome::Unavailable, cx);
            }
            None => cx.propagate(),
        }
    }

    fn handle_native_command(
        &mut self,
        action: &CommandAction,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match action.command.name.as_ref() {
            DIAGNOSTIC_COMMAND => {
                self.record_command_diagnostic(
                    action.invocation,
                    CommandSurfaceKind::Shell,
                    action.context.buffer.is_some(),
                    cx,
                );
            }
            FIXTURE_SEARCH_COMMAND => {
                let CommandArgumentValue::String(query) = &action.command.arguments else {
                    self.finish_command_invocation(
                        action.invocation,
                        CommandOutcome::InvalidArgument {
                            message: "fixture search requires a string query".into(),
                        },
                        cx,
                    );
                    return true;
                };
                let outcome = action
                    .context
                    .buffer
                    .and_then(|buffer| self.buffer_registry.resolve(buffer).ok())
                    .and_then(|model| {
                        Self::documents(cx)
                            .documents()
                            .find(|document| document.model() == &model)
                            .map(|document| (document.id(), document.title().clone(), model))
                    })
                    .map(|(source, title, model)| {
                        let controller = SearchResultsController::search(
                            query.clone(),
                            source,
                            &title,
                            model,
                            cx,
                        );
                        let result = Self::document_collection(cx).update(cx, |documents, _| {
                            documents.create_search_results(controller)
                        });
                        self.select_document(result, cx);
                        CommandOutcome::Completed
                    })
                    .unwrap_or(CommandOutcome::InvalidTarget);
                self.finish_command_invocation(action.invocation, outcome, cx);
            }
            _ => return false,
        }
        true
    }

    fn record_command_diagnostic(
        &mut self,
        invocation: CommandInvocationId,
        surface: CommandSurfaceKind,
        has_buffer: bool,
        cx: &mut Context<Self>,
    ) {
        self.command_diagnostic = Some(CommandDiagnostic {
            invocation,
            surface,
            has_buffer,
        });
        self.finish_command_invocation(invocation, CommandOutcome::Completed, cx);
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
            |this, _palette, event: &CommandPaletteEvent<CommandOrigin>, cx| {
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
        let _ = self.enqueue_command_root(command, origin, None, cx);
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

    fn command_runtime_outcome(
        &self,
        invocation: CommandInvocationId,
        result: Result<(), crate::host::ExtensionRuntimeExecutionError>,
    ) -> CommandOutcome {
        if self
            .command_invocations
            .get(&invocation)
            .is_none_or(|node| node.cancelled)
        {
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
        }
    }

    fn finish_command_invocation(
        &mut self,
        invocation: CommandInvocationId,
        outcome: CommandOutcome,
        cx: &mut Context<Self>,
    ) {
        let Some(node) = self.command_invocations.get_mut(&invocation) else {
            return;
        };
        if node.handler_outcome.is_some() {
            return;
        }
        node.handler_outcome = Some(if node.cancelled {
            CommandOutcome::Cancelled
        } else {
            outcome
        });
        let unfinished_children = node.children.iter().copied().collect::<Vec<_>>();
        if !unfinished_children.is_empty() {
            for child in unfinished_children {
                self.cancel_command_subtree(child);
            }
            return;
        }
        self.settle_command_invocation(invocation, cx);
    }

    fn settle_command_invocation(
        &mut self,
        invocation: CommandInvocationId,
        cx: &mut Context<Self>,
    ) {
        let Some(node) = self.command_invocations.remove(&invocation) else {
            return;
        };
        let outcome = node.handler_outcome.unwrap_or(if node.cancelled {
            CommandOutcome::Cancelled
        } else {
            CommandOutcome::Unavailable
        });
        node.completion.complete(outcome.clone());
        if let Some(parent) = node.parent {
            let parent_ready = self
                .command_invocations
                .get_mut(&parent)
                .is_some_and(|parent| {
                    parent.children.remove(&invocation);
                    parent.children.is_empty() && parent.handler_outcome.is_some()
                });
            if parent_ready {
                self.settle_command_invocation(parent, cx);
            }
        } else if self.active_command_root == Some(invocation) {
            self.active_command_root = None;
            self.record_command_outcome(outcome, cx);
            self.start_next_command_root(cx);
        } else {
            self.queued_command_roots
                .retain(|queued| *queued != invocation);
        }
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
        if let Some(root) = self.active_command_root {
            self.cancel_command_subtree(root);
            self.command_state = "cancelling".into();
            cx.notify();
        }
    }

    fn cancel_command_subtree(&mut self, invocation: CommandInvocationId) {
        let Some(node) = self.command_invocations.get_mut(&invocation) else {
            return;
        };
        if node.cancelled {
            return;
        }
        node.cancelled = true;
        let children = node.children.iter().copied().collect::<Vec<_>>();
        if let InvocationTarget::Extension(target) = node.target {
            if let Some(control) = self
                .extension_controls
                .get(&(target.extension, target.lifecycle))
            {
                let _ = control.cancel_command(invocation);
            }
        }
        for child in children {
            self.cancel_command_subtree(child);
        }
    }

    fn cancel_command_lifecycle(
        &mut self,
        extension: ExtensionId,
        lifecycle: crate::host::protocol::ExtensionLifecycleId,
        cx: &mut Context<Self>,
    ) {
        let affected = self
            .command_invocations
            .iter()
            .filter_map(|(id, node)| {
                matches!(
                    node.target,
                    InvocationTarget::Extension(target)
                        if target.extension == extension && target.lifecycle == lifecycle
                )
                .then_some(*id)
            })
            .collect::<Vec<_>>();
        for invocation in &affected {
            self.cancel_command_subtree(*invocation);
        }
        for invocation in affected {
            self.finish_command_invocation(invocation, CommandOutcome::Cancelled, cx);
        }
    }

    /// Build a single selectable row: a div wrapping `label`; clicking selects it,
    /// the selected row gets a highlight bg, all rows get a hover bg.
    fn buffer_row(
        id: DocumentId,
        label: SharedString,
        selected: Option<DocumentId>,
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
                    s.select_document(id, cx);
                });
            })
            .into_any_element()
    }

    fn dispatch_workspace_tree_event(&mut self, event: WorkspaceTreeEvent, cx: &mut Context<Self>) {
        match event {
            WorkspaceTreeEvent::RequestChildren(request) => {
                self.enumerate_workspace_directory(request, cx);
            }
            WorkspaceTreeEvent::OpenFile { workspace, uri } => {
                self.open_workspace_file(workspace, uri, cx);
            }
        }
    }

    fn enumerate_workspace_directory(
        &mut self,
        request: WorkspaceTreeRequest,
        cx: &mut Context<Self>,
    ) {
        if !self.workspace.is_current(&request.workspace) {
            return;
        }
        let providers = self.filesystem_providers.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = async {
                let parent = providers.normalize(request.parent.clone()).await?;
                Self::validate_workspace_target(&request.workspace, &parent)?;
                let provider = providers.provider(&parent)?;
                match provider.stat(parent.clone()).await? {
                    ResourceStat::Directory => provider.enumerate(parent).await,
                    ResourceStat::File => Err(ResourceError::WrongKind {
                        uri: parent,
                        expected: ResourceKind::Directory,
                        actual: ResourceKind::File,
                    }),
                    ResourceStat::Missing => Err(ResourceError::NotFound { uri: parent }),
                }
            }
            .await;
            let error = result.as_ref().err().map(|error| error.to_string());
            let response = WorkspaceTreeResponse {
                workspace: request.workspace.clone(),
                parent: request.parent,
                generation: request.generation,
                result: result.map_err(|error| error.to_string().into()),
            };
            let _ = this.update(cx, |this, cx| {
                if !this.workspace.is_current(&request.workspace) {
                    return;
                }
                let applied = this
                    .workspace_tree
                    .update(cx, |tree, cx| tree.apply_response(response, cx));
                if !applied {
                    return;
                }
                if this.resource_status.starts_with("workspace") || this.resource_status == "ready"
                {
                    if let Some(error) = error {
                        this.resource_status = "workspace enumeration failed".into();
                        this.latest_resource_error = Some(error.into());
                    } else {
                        this.resource_status = "workspace ready".into();
                        this.latest_resource_error = None;
                    }
                }
                cx.notify();
            });
        });
        self._resource_tasks.push(task);
    }

    fn open_workspace_file(
        &mut self,
        workspace: WorkspaceSnapshot,
        uri: ResourceUri,
        cx: &mut Context<Self>,
    ) {
        if !self.workspace.is_current(&workspace) {
            return;
        }
        self.resource_operation_generation = self
            .resource_operation_generation
            .checked_add(1)
            .expect("resource operation generation overflowed");
        let operation_generation = self.resource_operation_generation;
        self.resource_status = format!("loading {uri}").into();
        self.latest_resource_error = None;
        let providers = self.filesystem_providers.clone();
        let task = cx.spawn(async move |this, cx| {
            let result: Result<
                (ResourceUri, String, filesystem::ResourceVersion),
                ResourceBufferError,
            > = async {
                let normalized = providers.normalize(uri).await?;
                Self::validate_workspace_target(&workspace, &normalized)?;
                let provider = providers.provider(&normalized)?;
                match provider.stat(normalized.clone()).await? {
                    ResourceStat::File => {}
                    ResourceStat::Directory => {
                        return Err(ResourceError::WrongKind {
                            uri: normalized,
                            expected: ResourceKind::File,
                            actual: ResourceKind::Directory,
                        }
                        .into());
                    }
                    ResourceStat::Missing => {
                        return Err(ResourceError::NotFound { uri: normalized }.into());
                    }
                }
                let file = provider.read(normalized.clone()).await?;
                let text = String::from_utf8(file.bytes).map_err(|_| {
                    ResourceBufferError::InvalidUtf8 {
                        uri: normalized.clone(),
                    }
                })?;
                Ok((normalized, text, file.version))
            }
            .await;

            let _ = this.update(cx, |this, cx| {
                if !this.workspace.is_current(&workspace)
                    || this.resource_operation_generation != operation_generation
                {
                    return;
                }
                match result {
                    Ok((uri, text, version)) => {
                        let title = uri
                            .as_url()
                            .path_segments()
                            .and_then(|segments| {
                                segments.filter(|segment| !segment.is_empty()).last()
                            })
                            .unwrap_or(uri.as_url().as_str())
                            .to_owned();
                        let model = cx.new(|_| BufferModel::from_text(text));
                        let id = Self::document_collection(cx).update(cx, |documents, _| {
                            documents.create_persisted(title, model, uri.clone(), 0, version)
                        });
                        this.select_document(id, cx);
                        this.resource_status = format!("opened {uri}").into();
                        this.latest_resource_error = None;
                    }
                    Err(error) => {
                        let message: SharedString = error.to_string().into();
                        this.resource_status = "open failed".into();
                        this.latest_resource_error = Some(message);
                    }
                }
                cx.notify();
            });
        });
        self._resource_tasks.push(task);
        cx.notify();
    }

    fn save_selected_resource(&mut self, cx: &mut Context<Self>) {
        let Some((id, model, title, uri, expected_version)) = Self::documents(cx)
            .get(self.selected_document)
            .and_then(|document| {
                Some((
                    document.id(),
                    document.model().clone(),
                    document.title().clone(),
                    document.resource_uri()?.clone(),
                    document.state().persisted_version()?.clone(),
                ))
            })
        else {
            self.resource_status = "buffer is not resource-backed".into();
            self.latest_resource_error = Some("generated buffers cannot be saved".into());
            cx.notify();
            return;
        };
        let workspace = self.workspace.snapshot();
        if let Err(error) = Self::validate_workspace_target(&workspace, &uri) {
            self.resource_status = "save failed".into();
            self.latest_resource_error = Some(error.to_string().into());
            cx.notify();
            return;
        }
        let (text, revision) = model.read_with(cx, |model, _| (model.text(), model.revision()));
        let Some(capture) = Self::document_collection(cx)
            .update(cx, |documents, _| documents.begin_persistence(id, &model))
        else {
            return;
        };
        let providers = self.filesystem_providers.clone();
        self.resource_status = format!("saving {uri}").into();
        self.latest_resource_error = None;
        let task = cx.spawn(async move |this, cx| {
            let result = async {
                let normalized = providers.normalize(uri.clone()).await?;
                Self::validate_workspace_target(&workspace, &normalized)?;
                let provider = providers.provider(&normalized)?;
                provider
                    .replace(normalized, expected_version, text.into_bytes())
                    .await
            }
            .await;
            let _ = this.update(cx, |this, cx| {
                if !this.workspace.is_current(&workspace) {
                    return;
                }
                match result {
                    Ok(version) => {
                        if Self::document_collection(cx).update(cx, |documents, _| {
                            documents.finish_persistence(
                                id,
                                &model,
                                &capture,
                                title,
                                uri.clone(),
                                version,
                                revision,
                            )
                        }) {
                            this.resource_status =
                                format!("saved {uri} at revision {revision}").into();
                            this.latest_resource_error = None;
                        }
                    }
                    Err(error) => {
                        this.resource_status = "save failed".into();
                        this.latest_resource_error = Some(error.to_string().into());
                    }
                }
                cx.notify();
            });
        });
        self._resource_tasks.push(task);
        cx.notify();
    }

    fn switch_workspace(&mut self, root: ResourceUri, cx: &mut Context<Self>) {
        self.resource_operation_generation = self
            .resource_operation_generation
            .checked_add(1)
            .expect("resource operation generation overflowed");
        let snapshot = self.workspace.replace_root(root);
        self.workspace_tree
            .update(cx, |tree, cx| tree.set_workspace(snapshot, cx));
        self.resource_status = "workspace loading".into();
        self.latest_resource_error = None;
        cx.notify();
    }

    fn validate_workspace_target(
        workspace: &WorkspaceSnapshot,
        uri: &ResourceUri,
    ) -> Result<(), ResourceError> {
        if uri == workspace.root() || uri.is_descendant_of(workspace.root()) {
            Ok(())
        } else {
            Err(ResourceError::OutsideWorkspace {
                uri: uri.clone(),
                root: workspace.root().clone(),
            })
        }
    }

    fn select_document(&mut self, id: DocumentId, cx: &mut Context<Self>) {
        if self.selected_document == id {
            return;
        }
        let Some(model) = Self::documents(cx)
            .get(id)
            .map(|document| document.model().clone())
        else {
            return;
        };
        self.selected_document = id;
        let handle = if let Some(handle) = self.buffer_registry.handle_for(&model) {
            handle
        } else {
            let handle = self.buffer_registry.open(&model);
            let subscription = cx.observe(&model, |this, model, cx| {
                this.publish_model_change(model, cx);
                cx.notify();
            });
            self._model_subscriptions.push(subscription);
            handle
        };
        self.buffer_registry.set_active(Some(handle));
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

    #[cfg(test)]
    fn selected_document<'a>(&self, cx: &'a App) -> Option<&'a documents::Document> {
        Self::documents(cx).get(self.selected_document)
    }

    fn search_fixture(&mut self, query: &'static str, cx: &mut Context<Self>) {
        let (source_title, source_model) = {
            let documents = Self::documents(cx);
            let source = documents
                .get(self.source_document)
                .expect("source document remains open");
            (source.title().clone(), source.model().clone())
        };
        let controller = SearchResultsController::search(
            query,
            self.source_document,
            &source_title,
            source_model,
            cx,
        );
        let result_document = Self::document_collection(cx).update(cx, |documents, _| {
            documents.create_search_results(controller)
        });
        self.select_document(result_document, cx);
    }

    /// A pane that hosts a selectable single-row list.
    fn buffer_pane(&self, entity: Entity<Shell>, cx: &App) -> impl IntoElement {
        let entries: Vec<_> = Self::documents(cx)
            .documents()
            .map(|document| {
                let label: SharedString = if document.is_dirty(cx) {
                    format!("{} •", document.title()).into()
                } else {
                    document.title().clone()
                };
                (document.id(), label)
            })
            .collect();
        let selected = Some(self.selected_document);
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
        for (_, invocation) in self.command_invocations.drain() {
            invocation.completion.complete(CommandOutcome::Cancelled);
        }
        self.queued_command_roots.clear();
        self.active_command_root = None;
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
        let documents = Self::documents(cx);
        let selected_document = documents.get(self.selected_document);
        let revision = selected_document
            .map(|document| document.model().read(cx).revision().to_string())
            .unwrap_or_else(|| "closed".into());
        let runtime_error = self
            .latest_runtime_error
            .clone()
            .unwrap_or_else(|| "none".into());
        let resource_error = self
            .latest_resource_error
            .clone()
            .unwrap_or_else(|| "none".into());
        let (resource_uri, dirty) = selected_document
            .map(|document| {
                let identity = match document.state() {
                    DocumentState::Untitled { .. } => "untitled".into(),
                    DocumentState::Destination { uri } | DocumentState::Persisted { uri, .. } => {
                        uri.to_string()
                    }
                    DocumentState::Generated => "generated".into(),
                };
                (identity, document.is_dirty(cx))
            })
            .unwrap_or_else(|| ("closed".into(), false));
        let workspace_root = self.workspace.root().to_string();
        let workspace_provider = self.workspace.root().scheme().to_owned();
        let workspace_selection = self
            .workspace_tree
            .read(cx)
            .selected()
            .map(ToString::to_string)
            .unwrap_or_else(|| "none".into());
        let memory_workspace_root = self.memory_workspace_root.clone();
        let local_workspace_root = self.local_workspace_root.clone();
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
                            .flex()
                            .flex_row()
                            .justify_between()
                            .items_center()
                            .px_2()
                            .py_1()
                            .text_xs()
                            .text_color(rgb(0x888888))
                            .child("WORKSPACE")
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .gap_2()
                                    .child(
                                        div()
                                            .id("use-memory-workspace")
                                            .cursor_pointer()
                                            .text_color(rgb(0x80c0ff))
                                            .child("mem")
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.switch_workspace(
                                                    memory_workspace_root.clone(),
                                                    cx,
                                                );
                                            })),
                                    )
                                    .child(
                                        div()
                                            .id("use-local-workspace")
                                            .cursor_pointer()
                                            .text_color(rgb(0x80c0ff))
                                            .child("local")
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.switch_workspace(
                                                    local_workspace_root.clone(),
                                                    cx,
                                                );
                                            })),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .overflow_hidden()
                            .child(self.workspace_tree.clone()),
                    )
                    .child(div().w_full().h(px(1.)).bg(rgb(0x3a3a3a)))
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .text_xs()
                            .text_color(rgb(0x888888))
                            .child("BUFFERS"),
                    )
                    .child(
                        div()
                            .h(px(180.))
                            .flex_none()
                            .overflow_hidden()
                            .child(self.buffer_pane(entity.clone(), cx)),
                    ),
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
                            .child(format!("workspace {workspace_provider} {workspace_root}"))
                            .child(format!("selected {workspace_selection}"))
                            .child(format!("resource {resource_uri}"))
                            .child(if dirty { "dirty" } else { "clean" })
                            .child(format!("filesystem {}", self.resource_status))
                            .child(
                                div()
                                    .id("save-resource")
                                    .cursor_pointer()
                                    .text_color(rgb(0x80c0ff))
                                    .child("save")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.save_selected_resource(cx);
                                    })),
                            )
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
                            .child(format!("filesystem error {resource_error}"))
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

fn run_fixture(fixture_name: String) {
    let host = V8Host::new();
    let runtimes = vec![
        host.spawn_extension(ExtensionId::new(1)).into_parts(),
        host.spawn_extension(ExtensionId::new(2)).into_parts(),
    ];

    Application::new().run(move |app: &mut App| {
        app.on_action(|_action: &Quit, app: &mut App| app.quit());

        app.bind_keys(
            fixed_command_bindings()
                .into_iter()
                .chain(keymap_control_bindings())
                .chain(completion_bindings())
                .chain([KeyBinding::new("cmd-q", Quit, None)]),
        );

        let bounds = Bounds::centered(None, size(px(1200.), px(800.)), app);
        app.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_window, cx| {
                cx.new(|cx| Shell::new_with_runtimes_and_fixture(runtimes, fixture_name, cx))
            },
        )
        .unwrap();

        app.set_menus(vec![Menu {
            name: "Knot".into(),
            items: vec![MenuItem::action("Quit Knot", Quit)],
        }]);
    });
}

pub fn run() {
    let launch = entry::LaunchConfiguration::parse(
        std::env::args(),
        &std::env::current_dir().expect("Knot requires a current working directory"),
    )
    .unwrap_or_else(|error| {
        eprintln!("[knot] {error}");
        std::process::exit(2);
    });
    match launch {
        entry::LaunchConfiguration::Product(request) => product::run(request),
        entry::LaunchConfiguration::Fixture(fixture) => run_fixture(fixture),
    }
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
        editor::EditorContributionAction, model::BufferAccessError, resource::ResourceUri,
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
        let completion = bindings[1]
            .action()
            .as_any()
            .downcast_ref::<super::KeybindingCommand>()
            .unwrap();
        assert_eq!(completion.command.arguments, CommandArgumentValue::Null);
        let edit = bindings[2]
            .action()
            .as_any()
            .downcast_ref::<super::KeybindingCommand>()
            .unwrap();
        assert_eq!(
            edit.command.arguments,
            CommandArgumentValue::String(FIXTURE_EDIT_ARGUMENT.into())
        );
        let multi_key = bindings[3]
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

    async fn wait_for_editor_text(shell: &Entity<Shell>, expected: &str, cx: &mut TestAppContext) {
        while cx.read(|cx| shell.read(cx).editor.read(cx).model().read(cx).text() != expected) {
            shell.next_notification(Duration::ZERO, cx).await;
        }
    }

    fn top_level_fixture_command_source() -> String {
        r#"
            import { commands } from "knot:editor";
            const outcome = await commands.invoke(
              "knot.fixture.edit",
              __FIXTURE_EDIT_ARGUMENT__,
            );
            if (outcome.kind !== "completed") {
              throw new Error(`unexpected command outcome: ${outcome.kind}`);
            }
        "#
        .replace(
            "__FIXTURE_EDIT_ARGUMENT__",
            &serde_json::to_string(FIXTURE_EDIT_ARGUMENT).unwrap(),
        )
    }

    async fn wait_for_tree_error(shell: &Entity<Shell>, cx: &mut TestAppContext) {
        let outline = cx.read(|cx| shell.read(cx).outline.clone());
        while cx.read(|cx| outline.read(cx).lifecycle_state().3 == 0) {
            outline.next_notification(Duration::ZERO, cx).await;
        }
    }

    async fn wait_for_selected_resource(
        shell: &Entity<Shell>,
        expected: &ResourceUri,
        cx: &mut TestAppContext,
    ) {
        while !cx.read(|cx| {
            if let Some(error) = &shell.read(cx).latest_resource_error {
                panic!("resource open failed while waiting for {expected}: {error}");
            }
            shell
                .read(cx)
                .selected_document(cx)
                .and_then(|document| document.resource_uri())
                .is_some_and(|uri| uri == expected)
        }) {
            shell.next_notification(Duration::ZERO, cx).await;
        }
    }

    async fn wait_for_resource_error(shell: &Entity<Shell>, cx: &mut TestAppContext) {
        while cx.read(|cx| shell.read(cx).latest_resource_error.is_none()) {
            shell.next_notification(Duration::ZERO, cx).await;
        }
    }

    fn workspace_child(root: &ResourceUri, name: &str) -> ResourceUri {
        ResourceUri::parse(&format!(
            "{}/{}",
            root.to_string().trim_end_matches('/'),
            name
        ))
        .unwrap()
    }

    #[gpui::test]
    async fn workspace_resource_flow_covers_rejection_both_providers_and_a_save_race(
        cx: &mut TestAppContext,
    ) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(113))
            .into_parts();
        let shell = cx.new(|cx| Shell::new_with_resource_fixture(runtime, cx));
        let (memory_root, local_root, source) = cx.read(|cx| {
            let shell = shell.read(cx);
            (
                shell.memory_workspace_root.clone(),
                shell.local_workspace_root.clone(),
                shell.source_document,
            )
        });

        let stale_memory_file = workspace_child(&memory_root, "README.md");
        shell.update(cx, |shell, cx| {
            let captured = shell.workspace.snapshot();
            shell.open_workspace_file(captured, stale_memory_file, cx);
            shell.switch_workspace(local_root.clone(), cx);
        });
        let tree = cx.read(|cx| shell.read(cx).workspace_tree.clone());
        while cx.read(|cx| tree.read(cx).is_loading()) {
            tree.next_notification(Duration::ZERO, cx).await;
        }
        cx.read(|cx| {
            let shell = shell.read(cx);
            assert_eq!(Shell::documents(cx).documents().count(), 1);
            assert_eq!(shell.selected_document, source);
            assert!(shell.latest_resource_error.is_none());
        });

        let invalid = workspace_child(&memory_root, "invalid.bin");
        shell.update(cx, |shell, cx| {
            shell.switch_workspace(memory_root.clone(), cx);
            shell.open_workspace_file(shell.workspace.snapshot(), invalid, cx);
        });
        wait_for_resource_error(&shell, cx).await;
        cx.read(|cx| {
            let shell = shell.read(cx);
            assert_eq!(Shell::documents(cx).documents().count(), 1);
            assert_eq!(shell.selected_document, source);
            assert!(
                shell
                    .latest_resource_error
                    .as_deref()
                    .unwrap()
                    .contains("not valid UTF-8")
            );
        });

        let roots = [
            (memory_root, "memory saved\n"),
            (local_root, "local saved\n"),
        ];
        for (root, expected) in roots {
            shell.update(cx, |shell, cx| {
                if shell.workspace.root() != &root {
                    shell.switch_workspace(root.clone(), cx);
                }
            });
            let tree = cx.read(|cx| shell.read(cx).workspace_tree.clone());
            while cx.read(|cx| tree.read(cx).is_loading()) {
                tree.next_notification(Duration::ZERO, cx).await;
            }

            let directory = workspace_child(&root, "src");
            tree.update(cx, |tree, cx| {
                assert!(tree.activate_for_test(&directory, cx));
            });
            while cx.read(|cx| tree.read(cx).is_loading()) {
                tree.next_notification(Duration::ZERO, cx).await;
            }

            let uri = workspace_child(&root, "README.md");
            tree.update(cx, |tree, cx| {
                assert!(tree.activate_for_test(&uri, cx));
            });
            wait_for_selected_resource(&shell, &uri, cx).await;
            shell.update(cx, |shell, cx| {
                let model = shell.selected_document(cx).unwrap().model().clone();
                model.update(cx, |model, _| {
                    let len = model.text().len();
                    model.replace(0..len, expected).unwrap();
                });
                shell.save_selected_resource(cx);
            });
            while cx.read(|cx| shell.read(cx).selected_document(cx).unwrap().is_dirty(cx)) {
                shell.next_notification(Duration::ZERO, cx).await;
            }
            let provider =
                cx.read(|cx| shell.read(cx).filesystem_providers.provider(&uri).unwrap());
            assert_eq!(provider.read(uri).await.unwrap().bytes, expected.as_bytes());
        }

        let (uri, model, captured_text, captured_revision) = shell.update(cx, |shell, cx| {
            let document = shell.selected_document(cx).unwrap();
            let uri = document.resource_uri().unwrap().clone();
            let model = document.model().clone();
            model.update(cx, |model, _| {
                let len = model.text().len();
                model.replace(0..len, "captured save\n").unwrap();
            });
            let captured = model.read_with(cx, |model, _| (model.text(), model.revision()));
            shell.save_selected_resource(cx);
            model.update(cx, |model, _| {
                let len = model.text().len();
                model.replace(0..len, "newer edit\n").unwrap();
            });
            (uri, model, captured.0, captured.1)
        });
        while !cx.read(|cx| {
            shell
                .read(cx)
                .selected_document(cx)
                .unwrap()
                .state()
                .persisted_revision()
                == Some(captured_revision)
        }) {
            shell.next_notification(Duration::ZERO, cx).await;
        }

        let provider = cx.read(|cx| shell.read(cx).filesystem_providers.provider(&uri).unwrap());
        assert_eq!(
            provider.read(uri).await.unwrap().bytes,
            captured_text.as_bytes()
        );
        cx.read(|cx| {
            let shell = shell.read(cx);
            let document = shell.selected_document(cx).unwrap();
            assert_eq!(document.model(), &model);
            assert!(document.is_dirty(cx));
        });
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
            let source = shell.selected_document(cx).unwrap();
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
                top_level_fixture_command_source(),
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
        assert!(text.starts_with(FIXTURE_EDIT_ARGUMENT));
    }

    #[gpui::test]
    async fn command_sources_reach_one_handler_with_equal_arguments_and_buffer_context(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| cx.bind_keys(super::fixed_command_bindings()));
        let host = V8Host::new();
        let (shell, cx) = cx.add_window_view(|_, cx| {
            Shell::new_with_runtimes(
                [ExtensionId::new(59), ExtensionId::new(60)]
                    .map(|extension| host.spawn_extension(extension).into_parts())
                    .into(),
                cx,
            )
        });
        wait_for_runtime_state(&shell, "running", cx).await;
        let initial_text = cx.read(|cx| shell.read(cx).editor.read(cx).model().read(cx).text());

        shell.update_in(cx, |shell, window, cx| {
            window.focus(&shell.editor.focus_handle(cx));
        });
        cx.refresh().unwrap();
        cx.simulate_keystrokes("ctrl-alt-e");
        let one_edit = format!("{FIXTURE_EDIT_ARGUMENT}{initial_text}");
        wait_for_editor_text(&shell, &one_edit, cx).await;

        shell.update_in(cx, |shell, window, cx| {
            window.focus(&shell.editor.focus_handle(cx));
            shell.open_command_palette(window, cx);
        });
        cx.refresh().unwrap();
        cx.simulate_keystrokes("f i x t u r e . e d i t enter");
        let two_edits = format!("{FIXTURE_EDIT_ARGUMENT}{one_edit}");
        wait_for_editor_text(&shell, &two_edits, cx).await;

        shell.update_in(cx, |shell, window, cx| {
            window.focus(&shell.editor.focus_handle(cx));
        });
        let script = runtime_control(&shell, ExtensionId::new(60), cx);
        script
            .execute_fixture_module(
                "file:///fixtures/equivalent-command-source.js",
                top_level_fixture_command_source(),
            )
            .await
            .unwrap();

        let owner = runtime_control(&shell, ExtensionId::new(59), cx);
        let verification = r#"
                const expected = __FIXTURE_EDIT_ARGUMENT__;
                const invocations = globalThis.knotFixtureCommandInvocations;
                if (invocations.length !== 3) {
                  throw new Error(`expected three invocations, got ${invocations.length}`);
                }
                if (!invocations.every((entry) =>
                  entry.arguments === expected && entry.capturedActiveBuffer
                )) {
                  throw new Error("command sources did not preserve arguments and buffer context");
                }
            "#
        .replace(
            "__FIXTURE_EDIT_ARGUMENT__",
            &serde_json::to_string(FIXTURE_EDIT_ARGUMENT).unwrap(),
        );
        owner
            .execute_fixture_script("verify-equivalent-command-sources.js", verification)
            .await
            .unwrap();

        assert_eq!(
            cx.read(|cx| shell.read(cx).command_diagnostic),
            None,
            "the argument-bearing fixture command must not use the cross-surface diagnostic"
        );
        let final_text = cx.read(|cx| shell.read(cx).editor.read(cx).model().read(cx).text());
        assert_eq!(final_text, format!("{FIXTURE_EDIT_ARGUMENT}{two_edits}"));
    }

    #[gpui::test]
    async fn extension_command_awaits_cross_extension_edit_then_native_search(
        cx: &mut TestAppContext,
    ) {
        let host = V8Host::new();
        let (shell, cx) = cx.add_window_view(|_, cx| {
            Shell::new_with_runtimes(
                [ExtensionId::new(57), ExtensionId::new(58)]
                    .map(|extension| host.spawn_extension(extension).into_parts())
                    .into(),
                cx,
            )
        });
        wait_for_runtime_state(&shell, "running", cx).await;
        let caller = runtime_control(&shell, ExtensionId::new(58), cx);
        caller
            .execute_fixture_module(
                "file:///fixtures/composed-command.js",
                r#"
                    import { commands } from "knot:editor";
                    await commands.register("knot.fixture.composed", async () => {
                      const edit = await commands.invoke(
                        "knot.fixture.edit",
                        "KNOT_STEP_11B_MARKER\n",
                      );
                      if (edit.kind !== "completed") return;
                      const search = await commands.invoke(
                        "knot.fixture.search",
                        "KNOT_STEP_11B_MARKER",
                      );
                      if (search.kind !== "completed") {
                        throw new Error(`unexpected search outcome: ${search.kind}`);
                      }
                    });
                "#,
            )
            .await
            .unwrap();

        let (target, initial_text) = cx.read(|cx| {
            let shell = shell.read(cx);
            let target = shell
                .command_catalog
                .resolve_extension("knot.fixture.composed")
                .unwrap();
            let text = shell
                .editor
                .read(cx)
                .model()
                .read_with(cx, |model, _| model.text());
            (target, text)
        });
        invoke_extension_target_in_window(&shell, target, "knot.fixture.composed", cx);
        wait_for_command_state(&shell, "completed", cx).await;

        let (outcome, text, title, result_text) = cx.read(|cx| {
            let shell = shell.read(cx);
            let source_text = shell
                .editor
                .read(cx)
                .model()
                .read_with(cx, |model, _| model.text());
            let selected = shell.selected_document(cx).unwrap();
            let result_text = selected.model().read_with(cx, |model, _| model.text());
            (
                shell.command_outcome.clone(),
                source_text,
                selected.title().to_string(),
                result_text,
            )
        });
        assert_eq!(outcome, Some(CommandOutcome::Completed));
        assert!(text.starts_with("KNOT_STEP_11B_MARKER\n"));
        assert_ne!(text, initial_text);
        assert_eq!(title, "Search: \"KNOT_STEP_11B_MARKER\"");
        assert!(result_text.contains("KNOT_STEP_11B_MARKER"));
    }

    #[gpui::test]
    async fn same_extension_commands_execute_inline_and_reject_recursive_cycles(
        cx: &mut TestAppContext,
    ) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(82))
            .into_parts();
        let (shell, cx) = cx.add_window_view(|_, cx| Shell::new(runtime, cx));
        wait_for_runtime_state(&shell, "running", cx).await;
        let control = only_runtime_control(&shell, cx);
        control
            .execute_fixture_module(
                "file:///fixtures/inline-composition.js",
                r#"
                    import { commands } from "knot:editor";
                    globalThis.inlineCompositionEvents = [];
                    await commands.register("knot.fixture.inline-child", async () => {
                      globalThis.inlineCompositionEvents.push("child-before");
                      await globalThis.__knotFixtureDelay(25);
                      globalThis.inlineCompositionEvents.push("child-after");
                    });
                    await commands.register("knot.fixture.inline-recursive", async () => {
                      globalThis.recursiveCycle = await commands.invoke(
                        "knot.fixture.inline-recursive",
                        null,
                      );
                    });
                    await commands.register("knot.fixture.inline-wrapper", async () => {
                      globalThis.inlineCompositionEvents.push("parent-before");
                      const child = await commands.invoke("knot.fixture.inline-child", null);
                      globalThis.inlineCompositionEvents.push(`child-${child.kind}`);
                      const recursive = await commands.invoke(
                        "knot.fixture.inline-recursive",
                        null,
                      );
                      globalThis.inlineCompositionEvents.push(`recursive-${recursive.kind}`);
                      globalThis.inlineCompositionEvents.push("parent-after");
                    });
                "#,
            )
            .await
            .unwrap();
        let target = cx.read(|cx| {
            shell
                .read(cx)
                .command_catalog
                .resolve_extension("knot.fixture.inline-wrapper")
                .unwrap()
        });
        invoke_extension_target_in_window(&shell, target, "knot.fixture.inline-wrapper", cx);
        wait_for_command_state(&shell, "completed", cx).await;

        control
            .execute_fixture_script(
                "verify-inline-composition.js",
                r#"
                    const expected = [
                      "parent-before", "child-before", "child-after", "child-completed",
                      "recursive-completed", "parent-after",
                    ];
                    if (globalThis.inlineCompositionEvents.join(",") !== expected.join(",")) {
                      throw new Error(`unexpected inline events: ${globalThis.inlineCompositionEvents}`);
                    }
                    if (globalThis.recursiveCycle?.kind !== "unavailable") {
                      throw new Error(`unexpected recursive cycle: ${globalThis.recursiveCycle?.kind}`);
                    }
                "#,
            )
            .await
            .unwrap();
    }

    #[gpui::test]
    async fn cross_extension_cycle_is_unavailable_without_deadlocking_the_root(
        cx: &mut TestAppContext,
    ) {
        let host = V8Host::new();
        let (shell, cx) = cx.add_window_view(|_, cx| {
            Shell::new_with_runtimes(
                [ExtensionId::new(83), ExtensionId::new(84)]
                    .map(|extension| host.spawn_extension(extension).into_parts())
                    .into(),
                cx,
            )
        });
        wait_for_runtime_state(&shell, "running", cx).await;
        let second = runtime_control(&shell, ExtensionId::new(84), cx);
        second
            .execute_fixture_module(
                "file:///fixtures/cycle-child.js",
                r#"
                    import { commands } from "knot:editor";
                    await commands.register("knot.fixture.cycle-child", async () => {
                      globalThis.cycleOutcome = await commands.invoke("knot.fixture.edit", null);
                    });
                "#,
            )
            .await
            .unwrap();
        let first = runtime_control(&shell, ExtensionId::new(83), cx);
        first
            .execute_fixture_module(
                "file:///fixtures/cycle-root.js",
                r#"
                    import { commands } from "knot:editor";
                    await commands.register("knot.fixture.cycle-root", async () => {
                      const child = await commands.invoke("knot.fixture.cycle-child", null);
                      if (child.kind !== "completed") throw new Error(child.kind);
                    });
                "#,
            )
            .await
            .unwrap();
        let target = cx.read(|cx| {
            shell
                .read(cx)
                .command_catalog
                .resolve_extension("knot.fixture.cycle-root")
                .unwrap()
        });
        invoke_extension_target_in_window(&shell, target, "knot.fixture.cycle-root", cx);
        wait_for_command_state(&shell, "completed", cx).await;
        second
            .execute_fixture_script(
                "verify-cycle.js",
                r#"
                    if (globalThis.cycleOutcome?.kind !== "unavailable") {
                      throw new Error(`unexpected cycle outcome: ${globalThis.cycleOutcome?.kind}`);
                    }
                "#,
            )
            .await
            .unwrap();
    }

    #[gpui::test]
    async fn unsuccessful_child_only_controls_composition_when_javascript_branches_on_it(
        cx: &mut TestAppContext,
    ) {
        let host = V8Host::new();
        let (shell, cx) = cx.add_window_view(|_, cx| {
            Shell::new_with_runtimes(
                [ExtensionId::new(85), ExtensionId::new(86)]
                    .map(|extension| host.spawn_extension(extension).into_parts())
                    .into(),
                cx,
            )
        });
        wait_for_runtime_state(&shell, "running", cx).await;
        let owner = runtime_control(&shell, ExtensionId::new(85), cx);
        owner
            .execute_fixture_module(
                "file:///fixtures/failing-child.js",
                r#"
                    import { commands } from "knot:editor";
                    await commands.register("knot.fixture.failing-child", () => {
                      throw new Error("expected child failure");
                    });
                "#,
            )
            .await
            .unwrap();
        let caller = runtime_control(&shell, ExtensionId::new(86), cx);
        caller
            .execute_fixture_module(
                "file:///fixtures/failure-composition.js",
                r#"
                    import { commands } from "knot:editor";
                    await commands.register("knot.fixture.stop-after-failure", async () => {
                      const child = await commands.invoke("knot.fixture.failing-child", null);
                      if (child.kind !== "completed") return;
                      await commands.invoke("knot.fixture.search", "Node");
                    });
                    await commands.register("knot.fixture.continue-after-failure", async () => {
                      const child = await commands.invoke("knot.fixture.failing-child", null);
                      globalThis.observedFailure = child.kind;
                      await commands.invoke("knot.fixture.search", "Node");
                    });
                "#,
            )
            .await
            .unwrap();

        for (name, expected_buffers) in [
            ("knot.fixture.stop-after-failure", 1),
            ("knot.fixture.continue-after-failure", 2),
        ] {
            let target = cx.read(|cx| {
                shell
                    .read(cx)
                    .command_catalog
                    .resolve_extension(name)
                    .unwrap()
            });
            invoke_extension_target_in_window(&shell, target, name, cx);
            wait_for_command_state(&shell, "completed", cx).await;
            assert_eq!(
                cx.read(|cx| Shell::documents(cx).documents().count()),
                expected_buffers
            );
        }
        caller
            .execute_fixture_script(
                "verify-observed-failure.js",
                r#"
                    if (globalThis.observedFailure !== "handlerFailure") {
                      throw new Error(`unexpected child failure: ${globalThis.observedFailure}`);
                    }
                "#,
            )
            .await
            .unwrap();
    }

    #[gpui::test]
    async fn cancelling_root_aborts_child_skips_remaining_sequence_and_releases_queue(
        cx: &mut TestAppContext,
    ) {
        let host = V8Host::new();
        let (shell, cx) = cx.add_window_view(|_, cx| {
            Shell::new_with_runtimes(
                [ExtensionId::new(87), ExtensionId::new(88)]
                    .map(|extension| host.spawn_extension(extension).into_parts())
                    .into(),
                cx,
            )
        });
        wait_for_runtime_state(&shell, "running", cx).await;
        let child_owner = runtime_control(&shell, ExtensionId::new(87), cx);
        child_owner
            .execute_fixture_module(
                "file:///fixtures/cancellable-child.js",
                r#"
                    import { commands } from "knot:editor";
                    await commands.register("knot.fixture.cancellable-child", async (context) => {
                      await new Promise((resolve) => context.signal.addEventListener("abort", resolve));
                      try {
                        const snapshot = await context.buffer.snapshot();
                        await context.buffer.applyEdits(
                          [{ range: { startByteOffset: 0, endByteOffset: 0 }, text: "late-child\n" }],
                          { ifRevision: snapshot.revision },
                        );
                      } catch (error) {
                        globalThis.cancelledChildMutation = error.name;
                      }
                    });
                "#,
            )
            .await
            .unwrap();
        let parent_owner = runtime_control(&shell, ExtensionId::new(88), cx);
        parent_owner
            .execute_fixture_module(
                "file:///fixtures/cancellable-root.js",
                r#"
                    import { commands } from "knot:editor";
                    await commands.register("knot.fixture.cancellable-root", async () => {
                      const child = await commands.invoke("knot.fixture.cancellable-child", null);
                      if (child.kind === "completed") {
                        await commands.invoke("knot.fixture.search", "late-child");
                      }
                    });
                    await commands.register("knot.fixture.after-cancel", () => {
                      globalThis.afterCancellationRan = true;
                    });
                "#,
            )
            .await
            .unwrap();
        let initial_text = cx.read(|cx| shell.read(cx).editor.read(cx).model().read(cx).text());
        let (root, queued) = shell.update_in(cx, |shell, window, cx| {
            let focus = shell.editor.focus_handle(cx);
            window.focus(&focus);
            let origin = super::CommandOrigin {
                window: window.window_handle(),
                focus: focus.downgrade(),
                buffer: shell.buffer_registry.active_handle(),
            };
            let root = shell.enqueue_command_root(
                Command {
                    name: "knot.fixture.cancellable-root".into(),
                    arguments: CommandArgumentValue::Null,
                },
                origin.clone(),
                None,
                cx,
            );
            let queued = shell.enqueue_command_root(
                Command {
                    name: "knot.fixture.after-cancel".into(),
                    arguments: CommandArgumentValue::Null,
                },
                origin,
                None,
                cx,
            );
            (root, queued)
        });
        while cx.read(|cx| shell.read(cx).command_invocations.len() < 3) {
            shell.next_notification(Duration::ZERO, cx).await;
        }
        shell.update(cx, |shell, cx| shell.cancel_active_command(cx));

        assert_eq!(root.completion.await.unwrap(), CommandOutcome::Cancelled);
        assert_eq!(queued.completion.await.unwrap(), CommandOutcome::Completed);
        assert_eq!(
            cx.read(|cx| shell.read(cx).editor.read(cx).model().read(cx).text()),
            initial_text
        );
        assert_eq!(
            cx.read(|cx| Shell::documents(cx).documents().count()),
            1,
            "the cancelled parent must not run its remaining native child"
        );
        parent_owner
            .execute_fixture_script(
                "verify-root-queue-after-cancel.js",
                r#"
                    if (!globalThis.afterCancellationRan) {
                      throw new Error("queued root did not run after cancellation");
                    }
                "#,
            )
            .await
            .unwrap();
    }

    #[gpui::test]
    async fn cancelling_only_a_child_leaves_parent_in_control_of_composition(
        cx: &mut TestAppContext,
    ) {
        let host = V8Host::new();
        let (shell, cx) = cx.add_window_view(|_, cx| {
            Shell::new_with_runtimes(
                [ExtensionId::new(94), ExtensionId::new(95)]
                    .map(|extension| host.spawn_extension(extension).into_parts())
                    .into(),
                cx,
            )
        });
        wait_for_runtime_state(&shell, "running", cx).await;
        let child_owner = runtime_control(&shell, ExtensionId::new(94), cx);
        child_owner
            .execute_fixture_module(
                "file:///fixtures/child-only-cancellation.js",
                r#"
                    import { commands } from "knot:editor";
                    await commands.register("knot.fixture.child-only-cancellation", async ({ signal }) => {
                      await new Promise((resolve) => signal.addEventListener("abort", resolve));
                    });
                "#,
            )
            .await
            .unwrap();
        let parent_owner = runtime_control(&shell, ExtensionId::new(95), cx);
        parent_owner
            .execute_fixture_module(
                "file:///fixtures/parent-after-child-cancellation.js",
                r#"
                    import { commands } from "knot:editor";
                    await commands.register("knot.fixture.parent-after-child-cancellation", async () => {
                      const child = await commands.invoke(
                        "knot.fixture.child-only-cancellation",
                        null,
                      );
                      globalThis.childOnlyCancellationOutcome = child.kind;
                      await commands.invoke("knot.fixture.search", "Node");
                    });
                "#,
            )
            .await
            .unwrap();
        let execution = shell.update_in(cx, |shell, window, cx| {
            let focus = shell.editor.focus_handle(cx);
            window.focus(&focus);
            shell.enqueue_command_root(
                Command {
                    name: "knot.fixture.parent-after-child-cancellation".into(),
                    arguments: CommandArgumentValue::Null,
                },
                super::CommandOrigin {
                    window: window.window_handle(),
                    focus: focus.downgrade(),
                    buffer: shell.buffer_registry.active_handle(),
                },
                None,
                cx,
            )
        });
        while cx.read(|cx| shell.read(cx).command_invocations.len() < 2) {
            shell.next_notification(Duration::ZERO, cx).await;
        }
        shell.update(cx, |shell, _| {
            let root = shell.active_command_root.unwrap();
            let child = *shell
                .command_invocations
                .get(&root)
                .unwrap()
                .children
                .iter()
                .next()
                .unwrap();
            shell.cancel_command_subtree(child);
            assert!(!shell.command_invocations.get(&root).unwrap().cancelled);
        });

        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::Completed
        );
        assert_eq!(cx.read(|cx| Shell::documents(cx).documents().count()), 2);
        parent_owner
            .execute_fixture_script(
                "verify-child-only-cancellation.js",
                r#"
                    if (globalThis.childOnlyCancellationOutcome !== "cancelled") {
                      throw new Error(
                        `unexpected child outcome: ${globalThis.childOnlyCancellationOutcome}`,
                      );
                    }
                "#,
            )
            .await
            .unwrap();
    }

    #[gpui::test]
    async fn disposing_suspended_child_settles_parent_and_releases_root_queue(
        cx: &mut TestAppContext,
    ) {
        let host = V8Host::new();
        let (shell, cx) = cx.add_window_view(|_, cx| {
            Shell::new_with_runtimes(
                [ExtensionId::new(89), ExtensionId::new(90)]
                    .map(|extension| host.spawn_extension(extension).into_parts())
                    .into(),
                cx,
            )
        });
        wait_for_runtime_state(&shell, "running", cx).await;
        let child_owner = runtime_control(&shell, ExtensionId::new(89), cx);
        child_owner
            .execute_fixture_module(
                "file:///fixtures/disposable-child.js",
                r#"
                    import { commands } from "knot:editor";
                    await commands.register("knot.fixture.disposable-child", async (context) => {
                      await new Promise((resolve) => context.signal.addEventListener("abort", resolve));
                    });
                "#,
            )
            .await
            .unwrap();
        let parent_owner = runtime_control(&shell, ExtensionId::new(90), cx);
        parent_owner
            .execute_fixture_module(
                "file:///fixtures/parent-after-child-disposal.js",
                r#"
                    import { commands } from "knot:editor";
                    await commands.register("knot.fixture.parent-awaiting-disposal", async () => {
                      const child = await commands.invoke("knot.fixture.disposable-child", null);
                      globalThis.disposedChildOutcome = child.kind;
                      globalThis.parentContinuedAfterChildDisposal = true;
                    });
                    await commands.register("knot.fixture.queued-after-child-disposal", () => {
                      globalThis.queueContinuedAfterChildDisposal = true;
                    });
                "#,
            )
            .await
            .unwrap();

        let (root, queued) = shell.update_in(cx, |shell, window, cx| {
            let focus = shell.editor.focus_handle(cx);
            window.focus(&focus);
            let origin = super::CommandOrigin {
                window: window.window_handle(),
                focus: focus.downgrade(),
                buffer: shell.buffer_registry.active_handle(),
            };
            let root = shell.enqueue_command_root(
                Command {
                    name: "knot.fixture.parent-awaiting-disposal".into(),
                    arguments: CommandArgumentValue::Null,
                },
                origin.clone(),
                None,
                cx,
            );
            let queued = shell.enqueue_command_root(
                Command {
                    name: "knot.fixture.queued-after-child-disposal".into(),
                    arguments: CommandArgumentValue::Null,
                },
                origin,
                None,
                cx,
            );
            (root, queued)
        });
        while cx.read(|cx| shell.read(cx).command_invocations.len() < 3) {
            shell.next_notification(Duration::ZERO, cx).await;
        }
        child_owner.request_shutdown();

        assert_eq!(root.completion.await.unwrap(), CommandOutcome::Completed);
        assert_eq!(queued.completion.await.unwrap(), CommandOutcome::Completed);
        parent_owner
            .execute_fixture_script(
                "verify-child-disposal-continuation.js",
                r#"
                    if (globalThis.disposedChildOutcome !== "cancelled") {
                      throw new Error(`unexpected child outcome: ${globalThis.disposedChildOutcome}`);
                    }
                    if (!globalThis.parentContinuedAfterChildDisposal) {
                      throw new Error("parent did not continue after child disposal");
                    }
                    if (!globalThis.queueContinuedAfterChildDisposal) {
                      throw new Error("root queue did not continue after child disposal");
                    }
                "#,
            )
            .await
            .unwrap();
    }

    #[gpui::test]
    async fn disposing_parent_cancels_cross_extension_child_and_releases_queued_root(
        cx: &mut TestAppContext,
    ) {
        let host = V8Host::new();
        let (shell, cx) = cx.add_window_view(|_, cx| {
            Shell::new_with_runtimes(
                [ExtensionId::new(91), ExtensionId::new(92)]
                    .map(|extension| host.spawn_extension(extension).into_parts())
                    .into(),
                cx,
            )
        });
        wait_for_runtime_state(&shell, "running", cx).await;
        let parent_owner = runtime_control(&shell, ExtensionId::new(91), cx);
        parent_owner
            .execute_fixture_module(
                "file:///fixtures/disposable-parent.js",
                r#"
                    import { commands } from "knot:editor";
                    await commands.register("knot.fixture.disposable-parent", async () => {
                      await commands.invoke("knot.fixture.child-of-disposed-parent", null);
                    });
                "#,
            )
            .await
            .unwrap();
        let child_owner = runtime_control(&shell, ExtensionId::new(92), cx);
        child_owner
            .execute_fixture_module(
                "file:///fixtures/child-after-parent-disposal.js",
                r#"
                    import { commands } from "knot:editor";
                    await commands.register("knot.fixture.child-of-disposed-parent", async (context) => {
                      await new Promise((resolve) => context.signal.addEventListener("abort", resolve));
                      globalThis.childObservedParentDisposal = true;
                    });
                    await commands.register("knot.fixture.queued-after-parent-disposal", () => {
                      globalThis.queueContinuedAfterParentDisposal = true;
                    });
                "#,
            )
            .await
            .unwrap();

        let (root, queued) = shell.update_in(cx, |shell, window, cx| {
            let focus = shell.editor.focus_handle(cx);
            window.focus(&focus);
            let origin = super::CommandOrigin {
                window: window.window_handle(),
                focus: focus.downgrade(),
                buffer: shell.buffer_registry.active_handle(),
            };
            let root = shell.enqueue_command_root(
                Command {
                    name: "knot.fixture.disposable-parent".into(),
                    arguments: CommandArgumentValue::Null,
                },
                origin.clone(),
                None,
                cx,
            );
            let queued = shell.enqueue_command_root(
                Command {
                    name: "knot.fixture.queued-after-parent-disposal".into(),
                    arguments: CommandArgumentValue::Null,
                },
                origin,
                None,
                cx,
            );
            (root, queued)
        });
        while cx.read(|cx| shell.read(cx).command_invocations.len() < 3) {
            shell.next_notification(Duration::ZERO, cx).await;
        }
        parent_owner.request_shutdown();

        assert_eq!(root.completion.await.unwrap(), CommandOutcome::Cancelled);
        assert_eq!(queued.completion.await.unwrap(), CommandOutcome::Completed);
        child_owner
            .execute_fixture_script(
                "verify-parent-disposal-cancellation.js",
                r#"
                    if (!globalThis.childObservedParentDisposal) {
                      throw new Error("child did not observe parent disposal");
                    }
                    if (!globalThis.queueContinuedAfterParentDisposal) {
                      throw new Error("queued root did not run after parent disposal");
                    }
                "#,
            )
            .await
            .unwrap();
    }

    #[gpui::test]
    async fn queued_root_revalidates_its_captured_buffer_before_start(cx: &mut TestAppContext) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(93))
            .into_parts();
        let (shell, cx) = cx.add_window_view(|_, cx| Shell::new(runtime, cx));
        wait_for_runtime_state(&shell, "running", cx).await;
        let owner = only_runtime_control(&shell, cx);
        owner
            .execute_fixture_module(
                "file:///fixtures/queued-target-loss.js",
                r#"
                    import { commands } from "knot:editor";
                    await commands.register("knot.fixture.blocking-root", async (context) => {
                      await new Promise((resolve) => context.signal.addEventListener("abort", resolve));
                    });
                    await commands.register("knot.fixture.queued-lost-target", () => {
                      globalThis.lostTargetCommandRan = true;
                    });
                "#,
            )
            .await
            .unwrap();

        let (active, queued, buffer) = shell.update_in(cx, |shell, window, cx| {
            let focus = shell.editor.focus_handle(cx);
            window.focus(&focus);
            let buffer = shell.buffer_registry.active_handle().unwrap();
            let origin = super::CommandOrigin {
                window: window.window_handle(),
                focus: focus.downgrade(),
                buffer: Some(buffer),
            };
            let active = shell.enqueue_command_root(
                Command {
                    name: "knot.fixture.blocking-root".into(),
                    arguments: CommandArgumentValue::Null,
                },
                origin.clone(),
                None,
                cx,
            );
            let queued = shell.enqueue_command_root(
                Command {
                    name: "knot.fixture.queued-lost-target".into(),
                    arguments: CommandArgumentValue::Null,
                },
                origin,
                None,
                cx,
            );
            (active, queued, buffer)
        });
        shell.update(cx, |shell, cx| {
            shell.close_buffer(buffer, cx);
            shell.cancel_active_command(cx);
        });

        assert_eq!(active.completion.await.unwrap(), CommandOutcome::Cancelled);
        assert_eq!(
            queued.completion.await.unwrap(),
            CommandOutcome::InvalidTarget
        );
        owner
            .execute_fixture_script(
                "verify-queued-target-loss.js",
                r#"
                    if (globalThis.lostTargetCommandRan) {
                      throw new Error("queued command ran after its target was lost");
                    }
                "#,
            )
            .await
            .unwrap();
    }

    #[gpui::test]
    async fn selecting_a_document_reconstructs_only_the_secondary_view(cx: &mut TestAppContext) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(8))
            .into_parts();
        let shell = cx.new(|cx| Shell::new(runtime, cx));
        wait_for_runtime_state(&shell, "running", cx).await;
        let generated = cx.new(|_| BufferModel::from_text("generated\nresult"));

        let (source_editor, old_secondary, generated_id) = shell.update(cx, |shell, cx| {
            let generated_id = Shell::document_collection(cx).update(cx, |documents, _| {
                documents.create_generated("Generated", generated.clone())
            });
            (
                shell.editor.clone(),
                shell.secondary_editor.downgrade(),
                generated_id,
            )
        });
        shell.update(cx, |shell, cx| {
            shell.select_document(generated_id, cx);
        });

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
            let entries = Shell::documents(cx).documents().collect::<Vec<_>>();
            assert_eq!(entries.len(), 3);
            assert_eq!(entries[1].title(), "Search: \"Node\"");
            assert_eq!(entries[2].title(), "Search: \"Rope\"");
            assert!(entries[1].search_results().is_some());
            assert!(entries[2].search_results().is_some());
            assert_eq!(shell.selected_document, entries[2].id());
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
            let ids = Shell::documents(cx)
                .documents()
                .map(|document| document.id())
                .collect::<Vec<_>>();
            for id in [ids[0], ids[1], ids[2], ids[0], ids[2], ids[1]] {
                shell.select_document(id, cx);
            }
        });
        cx.refresh().unwrap();

        let (click, expected_range) = cx.read(|cx| {
            let shell = shell.read(cx);
            let bounds = shell.secondary_editor.read(cx).interaction_bounds();
            let controller = shell
                .selected_document(cx)
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
            assert!(shell.heartbeat > heartbeat);
            assert!(editor.model().read(cx).text().starts_with('x'));
            assert!(cursor_line > 0);
            assert!(scroll > 0.);
            assert!(paints > paint_count);
        });

        wait_for_runtime_state(&shell, "running", cx).await;
    }

    #[gpui::test]
    async fn completion_surface_replacement_keeps_in_flight_provider_work_responsive(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| cx.bind_keys(super::completion_bindings()));
        let host = V8Host::new();
        let (shell, cx) = cx.add_window_view(|_, cx| {
            Shell::new_with_runtimes(
                vec![
                    host.spawn_extension(ExtensionId::new(81)).into_parts(),
                    host.spawn_extension(ExtensionId::new(82)).into_parts(),
                ],
                cx,
            )
        });
        while cx.read(|cx| shell.read(cx).completion_providers.snapshot().len()) != 2 {
            shell.next_notification(Duration::ZERO, cx).await;
        }

        shell.update_in(cx, |shell, window, cx| {
            window.focus(&shell.editor.focus_handle(cx));
            let origin = shell.command_origin(window, cx).unwrap();
            shell.dispatch_command(
                Command {
                    name: super::SHOW_COMPLETIONS_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                origin,
                cx,
            );
        });
        while !cx.read(|cx| {
            shell
                .read(cx)
                .editor
                .read(cx)
                .completion_state()
                .is_some_and(|state| state.0 > 0 && state.1 == 1)
        }) {
            shell.next_notification(Duration::ZERO, cx).await;
        }
        let (heartbeat, generation) = cx.read(|cx| {
            let shell = shell.read(cx);
            (
                shell.heartbeat,
                shell.editor.read(cx).completion_state().unwrap().4,
            )
        });
        cx.simulate_keystrokes("down");

        shell.update_in(cx, |shell, window, cx| {
            let origin = shell.command_origin(window, cx).unwrap();
            shell.dispatch_command(
                Command {
                    name: super::SWAP_COMPLETION_SURFACE_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                origin,
                cx,
            );
        });
        cx.run_until_parked();
        cx.read(|cx| {
            let state = shell.read(cx).editor.read(cx).completion_state().unwrap();
            assert_eq!(state.3, super::completion::CompletionSurfaceKind::Compact);
            assert_eq!(state.4, generation);
            assert_eq!(state.1, 1);
        });

        shell.update_in(cx, |shell, window, cx| {
            let origin = shell.command_origin(window, cx).unwrap();
            shell.dispatch_command(
                Command {
                    name: super::SWAP_COMPLETION_SURFACE_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                origin,
                cx,
            );
        });
        cx.run_until_parked();
        assert_eq!(
            cx.read(|cx| { shell.read(cx).editor.read(cx).completion_state().unwrap().3 }),
            super::completion::CompletionSurfaceKind::List
        );
        shell.update_in(cx, |shell, window, cx| {
            let origin = shell.command_origin(window, cx).unwrap();
            shell.dispatch_command(
                Command {
                    name: super::SWAP_COMPLETION_SURFACE_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                origin,
                cx,
            );
        });
        cx.run_until_parked();

        cx.executor().advance_clock(Duration::from_millis(500));
        cx.run_until_parked();
        assert!(cx.read(|cx| shell.read(cx).heartbeat) > heartbeat);

        while cx.read(|cx| {
            shell
                .read(cx)
                .editor
                .read(cx)
                .completion_state()
                .is_some_and(|state| state.1 > 0)
        }) {
            shell.next_notification(Duration::ZERO, cx).await;
        }
        cx.read(|cx| {
            let state = shell.read(cx).editor.read(cx).completion_state().unwrap();
            assert_eq!(state.0, 5);
            assert_eq!(state.2, 0);
            assert_eq!(state.3, super::completion::CompletionSurfaceKind::Compact);
        });

        shell.update_in(cx, |shell, window, cx| {
            let origin = shell.command_origin(window, cx).unwrap();
            shell.dispatch_command(
                Command {
                    name: "knot.fixture.completion-fail-next".into(),
                    arguments: CommandArgumentValue::Null,
                },
                origin,
                cx,
            );
        });
        wait_for_command_state(&shell, "completed", cx).await;
        let prior_generation = generation;
        shell.update_in(cx, |shell, window, cx| {
            let origin = shell.command_origin(window, cx).unwrap();
            shell.dispatch_command(
                Command {
                    name: super::SHOW_COMPLETIONS_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                origin,
                cx,
            );
        });
        while !cx.read(|cx| {
            shell
                .read(cx)
                .editor
                .read(cx)
                .completion_state()
                .is_some_and(|state| state.4 > prior_generation && state.1 == 0)
        }) {
            shell.next_notification(Duration::ZERO, cx).await;
        }
        let failed_generation = cx.read(|cx| {
            let state = shell.read(cx).editor.read(cx).completion_state().unwrap();
            assert_eq!(state.0, 3);
            assert_eq!(state.2, 1);
            state.4
        });

        shell.update_in(cx, |shell, window, cx| {
            let origin = shell.command_origin(window, cx).unwrap();
            shell.dispatch_command(
                Command {
                    name: super::SHOW_COMPLETIONS_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                origin,
                cx,
            );
        });
        while !cx.read(|cx| {
            shell
                .read(cx)
                .editor
                .read(cx)
                .completion_state()
                .is_some_and(|state| state.4 > failed_generation && state.1 == 0)
        }) {
            shell.next_notification(Duration::ZERO, cx).await;
        }
        cx.read(|cx| {
            let state = shell.read(cx).editor.read(cx).completion_state().unwrap();
            assert_eq!(state.0, 5);
            assert_eq!(state.2, 0);
        });

        cx.simulate_keystrokes("escape");
        assert!(cx.read(|cx| { shell.read(cx).editor.read(cx).completion_state().is_none() }));

        shell.update_in(cx, |shell, window, cx| {
            let origin = shell.command_origin(window, cx).unwrap();
            shell.dispatch_command(
                Command {
                    name: super::SHOW_COMPLETIONS_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                origin,
                cx,
            );
        });
        while !cx.read(|cx| {
            shell
                .read(cx)
                .editor
                .read(cx)
                .completion_state()
                .is_some_and(|state| state.0 > 0)
        }) {
            shell.next_notification(Duration::ZERO, cx).await;
        }
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        cx.read(|cx| {
            let editor = shell.read(cx).editor.read(cx);
            assert!(editor.completion_state().is_none());
            assert!(editor.model().read(cx).text().starts_with("Alpha"));
        });
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
    async fn commands_retain_context_with_monotonic_identities_and_queue_roots_fifo(
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
            let shell_state = shell.read(cx);
            let command = shell_state
                .command_invocations
                .get(&shell_state.active_command_root.unwrap())
                .unwrap();
            assert_eq!(command.command.name, "knot.fixture.slow".into());
            assert_eq!(command.command.arguments, CommandArgumentValue::Null);
            assert_eq!(command.target, super::InvocationTarget::Extension(target));
            assert!(command.context.window == window);
            assert_eq!(command.context.buffer, buffer);
            assert_eq!(command.context.workspace.upgrade(), Some(shell.clone()));
            assert!(command.context.focus.upgrade().is_some());
        });

        invoke_extension_target_in_window(&shell, target, "knot.fixture.slow", cx);
        cx.read(|cx| {
            let shell = shell.read(cx);
            assert_eq!(shell.active_command_root, Some(CommandInvocationId::new(1)));
            assert_eq!(
                shell
                    .queued_command_roots
                    .iter()
                    .copied()
                    .collect::<Vec<_>>(),
                vec![CommandInvocationId::new(2)]
            );
            assert_eq!(shell.next_command_invocation, 3);
        });
        wait_for_command_state(&shell, "completed", cx).await;
        control
            .execute_fixture_script(
                "verify-queued-commands.js",
                "if (globalThis.slowCommandRuns !== 2) throw new Error('queued roots did not both run')",
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
            let command = shell
                .command_invocations
                .get(&shell.active_command_root.unwrap())
                .unwrap();
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
    async fn suspended_command_rejects_a_late_mutation_after_its_target_closes(
        cx: &mut TestAppContext,
    ) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(61))
            .into_parts();
        let (shell, cx) = cx.add_window_view(|_, cx| Shell::new(runtime, cx));
        wait_for_runtime_state(&shell, "running", cx).await;
        let control = only_runtime_control(&shell, cx);
        control
            .execute_fixture_module(
                "file:///fixtures/suspended-target-command.js",
                r#"
                    import { commands } from "knot:editor";
                    await commands.register("knot.fixture.suspended-target", async (context) => {
                      await globalThis.__knotFixtureDelay(150);
                      try {
                        await context.buffer.applyEdits(
                          [{ range: { startByteOffset: 0, endByteOffset: 0 }, text: "late\n" }],
                          { ifRevision: 0 },
                        );
                        globalThis.suspendedTargetMutation = "applied";
                      } catch (error) {
                        globalThis.suspendedTargetMutation = error.name;
                      }
                    });
                "#,
            )
            .await
            .unwrap();

        let replacement = cx.new(|_| BufferModel::from_text("replacement"));
        let (target, captured_model, captured_handle, initial_text, heartbeat) = cx.read(|cx| {
            let shell = shell.read(cx);
            let captured_handle = shell.buffer_registry.active_handle().unwrap();
            let captured_model = shell.buffer_registry.resolve(captured_handle).unwrap();
            let initial_text = captured_model.read_with(cx, |model, _| model.text());
            (
                shell
                    .command_catalog
                    .resolve_extension("knot.fixture.suspended-target")
                    .unwrap(),
                captured_model,
                captured_handle,
                initial_text,
                shell.heartbeat,
            )
        });

        invoke_extension_target_in_window(&shell, target, "knot.fixture.suspended-target", cx);
        shell.update_in(cx, |shell, window, cx| {
            shell.close_buffer(captured_handle, cx);
            let replacement_handle = shell.buffer_registry.open(&replacement);
            shell.buffer_registry.set_active(Some(replacement_handle));
            window.focus(&shell.terminal.focus_handle(cx));
        });

        cx.executor().advance_clock(Duration::from_millis(500));
        cx.run_until_parked();
        cx.read(|cx| {
            let shell = shell.read(cx);
            assert_eq!(shell.command_state.as_ref(), "running");
            assert!(shell.heartbeat > heartbeat);
        });

        wait_for_command_state(&shell, "completed", cx).await;
        control
            .execute_fixture_script(
                "verify-suspended-target-command.js",
                r#"
                    if (globalThis.suspendedTargetMutation !== "BufferClosedError") {
                      throw new Error(`unexpected late mutation result: ${globalThis.suspendedTargetMutation}`);
                    }
                "#,
            )
            .await
            .unwrap();

        assert_eq!(
            captured_model.read_with(cx, |model, _| model.text()),
            initial_text
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
            let command_id = shell.active_command_root.unwrap();
            let command = shell.command_invocations.get(&command_id).unwrap();
            let buffer = command.context.buffer.unwrap();
            let live_focus = command.context.focus.clone();
            let live_workspace = command.context.workspace.clone();
            let revision = shell
                .buffer_registry
                .resolve(buffer)
                .unwrap()
                .read_with(cx, |model, _| model.revision());
            let request = HostRequest {
                extension: target.extension,
                lifecycle: target.lifecycle,
                id: RequestId::new(1),
                invocation: Some(command_id),
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

            shell
                .command_invocations
                .get_mut(&command_id)
                .unwrap()
                .cancelled = true;
            assert_eq!(
                shell.validate_command_mutation(&request, cx),
                Err(HostRequestError::Cancelled)
            );
            shell
                .command_invocations
                .get_mut(&command_id)
                .unwrap()
                .cancelled = false;

            let dead_focus = {
                let focus = cx.focus_handle();
                focus.downgrade()
            };
            shell
                .command_invocations
                .get_mut(&command_id)
                .unwrap()
                .context
                .focus = dead_focus;
            assert_eq!(
                shell.validate_command_mutation(&request, cx),
                Err(HostRequestError::Cancelled)
            );
            shell
                .command_invocations
                .get_mut(&command_id)
                .unwrap()
                .context
                .focus = live_focus;

            shell
                .command_invocations
                .get_mut(&command_id)
                .unwrap()
                .context
                .workspace = WeakEntity::new_invalid();
            assert_eq!(
                shell.validate_command_mutation(&request, cx),
                Err(HostRequestError::Cancelled)
            );
            shell
                .command_invocations
                .get_mut(&command_id)
                .unwrap()
                .context
                .workspace = live_workspace;

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
