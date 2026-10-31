//! Native product windows backed by application documents and workbenches.
//!
//! This module owns shell state, focus, command routing, terminals, and
//! transient interactions. Save, open, protected closure, tools, rendering, and
//! launch live in submodules.

use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
    future::Future,
    path::Path,
    pin::Pin,
    sync::{Arc, OnceLock},
};

use gpui::{prelude::FluentBuilder, *};

mod closure;
pub(super) mod commands;
mod extension_restart;
mod language_mode;
mod launch;
mod move_tool;
mod navigation;
mod open;
pub(super) mod operations;
mod recovery_listing;
mod relocate;
mod render;
mod reopen;
mod roots;
mod save;
pub(super) mod sessions;
mod show_keybindings;
mod tab_drag;
mod tools;
mod view_state;

#[cfg(test)]
use super::commands::dispatch::SHOW_COMPLETIONS_COMMAND;
use closure::install_protected_window_close;
#[cfg(test)]
pub(super) use launch::bind_product_keys;
pub(super) use launch::finish_product_startup;
use launch::open_tab_tear_out_window;
#[cfg(test)]
use launch::{dispatch_launch_request, open_tab_transfer_window, product_filesystems};
pub(crate) use launch::{open_product_window, run};

use super::{
    CommandCompletion, CommandPalette,
    command_history::ApplicationCommandHistory,
    commands::dispatch::{
        ApplicationProductCommands, CLOSE_TAB_COMMAND, CLOSE_WINDOW_COMMAND, COPY_COMMAND,
        CUT_COMMAND, NEW_COMMAND, NEW_TERMINAL_COMMAND, NEW_WINDOW_COMMAND, OPEN_COMMAND,
        PASTE_COMMAND, ProductCommandDispatcher, ProductCommandSource, ProductCommandTarget,
        QUIT_COMMAND, REDO_COMMAND, SAVE_AS_COMMAND, SAVE_COMMAND, SELECT_ALL_COMMAND,
        SHOW_EXTENSION_REPORT_COMMAND, SPLIT_HORIZONTAL_COMMAND, SPLIT_VERTICAL_COMMAND,
        UNDO_COMMAND, dispatch_close_to_captured_target, dispatch_open_to_captured_target,
        dispatch_save_to_captured_target,
    },
    commands::keymaps::{ApplicationKeymaps, BindingResolution, KeymapAction},
    documents::{ApplicationDocuments, DocumentCollection, DocumentId, DocumentState},
    entry::OpenRequest,
    extensions::host::handlers::TransientError,
    filesystem::{
        FileSystemProviderRegistry, LocalFileSystemProvider, ResourceError, ResourceKind,
        ResourceStat, ResourceVersion,
    },
    model::BufferModel,
    open::{OpenResource, enumerate_directory, load_resource},
    picker::{Picker, PickerEntry, PickerEvent},
    terminal::session::TerminalSession,
    terminal::view::TerminalView,
    transient::{
        InputEvent, InputView, PromptText, TransientOutcome, TransientOwner, TransientSurface,
    },
    workbench::{
        CloseRequestOutcome, DockLocation, DocumentCloseDisposition, PaneId, SplitDirection,
        SplitPlacement, TabId, TabSurfaceId, TerminalSessionId, ToolContent, ToolFrameId,
        ToolPlacement, Workbench, WorkbenchLayout,
    },
    workspace::WorkspaceState,
    workspace_tree::{
        WorkspaceTree, WorkspaceTreeEvent, WorkspaceTreeRequest, WorkspaceTreeResponse,
    },
};

struct ApplicationWorkbenches(RefCell<Vec<(AnyWindowHandle, WeakEntity<Workbench>)>>);

impl Global for ApplicationWorkbenches {}

struct ApplicationProtectedClosure(RefCell<bool>);

impl Global for ApplicationProtectedClosure {}

struct ApplicationTerminalSessions(RefCell<TerminalSessions>);

impl Global for ApplicationTerminalSessions {}

struct ApplicationLaunchGate(RefCell<LaunchGate>);

impl Global for ApplicationLaunchGate {}

enum LaunchGate {
    Pending(Vec<Option<OpenRequest>>),
    Ready,
    Failed,
}

impl LaunchGate {
    fn request(&mut self, request: Option<OpenRequest>) -> Option<Option<OpenRequest>> {
        match self {
            Self::Pending(requests) => {
                requests.push(request);
                None
            }
            Self::Ready => Some(request),
            Self::Failed => None,
        }
    }

    /// The queued requests; `None` entries are reopen requests without a
    /// resource.
    fn complete(&mut self) -> Vec<Option<OpenRequest>> {
        match std::mem::replace(self, Self::Ready) {
            Self::Pending(requests) => requests,
            Self::Ready => Vec::new(),
            Self::Failed => {
                *self = Self::Failed;
                Vec::new()
            }
        }
    }

    fn fail(&mut self) {
        *self = Self::Failed;
    }
}

#[derive(Default)]
struct TerminalSessions {
    next_id: u64,
    sessions: HashMap<TerminalSessionId, Entity<TerminalSession>>,
}

impl TerminalSessions {
    fn allocate_id(&mut self) -> TerminalSessionId {
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("terminal identity exhausted");
        TerminalSessionId(self.next_id)
    }

    fn remove(&mut self, id: TerminalSessionId) -> Option<Entity<TerminalSession>> {
        self.sessions.remove(&id)
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum ProtectedCloseKind {
    Tab,
    /// Every unpinned tab of the origin's pane except the origin.
    OtherTabs,
    /// Every tab of the origin's pane.
    Pane,
    Window,
    Quit,
}

#[derive(Clone)]
struct ProtectedCloseRequest {
    kind: ProtectedCloseKind,
    origin: ProductCommandTarget,
    caller: Option<operations::OperationCaller>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct ProtectedViewKey {
    window: AnyWindowHandle,
    workbench: EntityId,
    presentation: Option<EntityId>,
    pane: PaneId,
    tab: TabId,
    surface: TabSurfaceId,
}

#[derive(Clone)]
struct ProtectedView {
    key: ProtectedViewKey,
    pinned: bool,
    target: Option<ProductCommandTarget>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CloseApprovalKind {
    Saved,
    Discarded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CloseApproval {
    document: DocumentId,
    revision: u64,
    kind: CloseApprovalKind,
}

struct ProtectedClosePlan {
    request: ProtectedCloseRequest,
    scope: Vec<ProtectedViewKey>,
    prompts: Vec<(DocumentId, ProductCommandTarget)>,
    /// Quit keeps dirty documents in durable recovery instead of asking about them.
    hot_exit: bool,
}

#[derive(Debug)]
enum ProtectedCloseFailure {
    InvalidTarget,
    InProgress,
    Cancelled,
    PersistenceFailed { message: String },
}

impl ProtectedCloseFailure {
    fn command_outcome(self) -> crate::host::protocol::CommandOutcome {
        use crate::host::protocol::CommandOutcome;
        match self {
            Self::InvalidTarget => CommandOutcome::InvalidTarget,
            Self::InProgress => CommandOutcome::Unavailable,
            Self::Cancelled => CommandOutcome::Cancelled,
            Self::PersistenceFailed { message } => CommandOutcome::HandlerFailure { message },
        }
    }
}

type ProtectedCloseCompletion = Box<
    dyn FnOnce(
        Result<crate::host::protocol::PresentationCloseOutcome, ProtectedCloseFailure>,
        &mut App,
    ),
>;

struct ApplicationFileSystems(Arc<FileSystemProviderRegistry>);

impl Global for ApplicationFileSystems {}

#[derive(Clone)]
enum OpenDialogOutcome {
    Selected(std::path::PathBuf),
    Cancelled,
    Failed(String),
}

type OpenDialogFuture = Pin<Box<dyn Future<Output = OpenDialogOutcome> + Send + 'static>>;

trait ProductOpenDialog: Send + Sync {
    fn select(&self) -> OpenDialogFuture;
}

struct ApplicationOpenDialog(Arc<dyn ProductOpenDialog>);

impl Global for ApplicationOpenDialog {}

#[derive(Clone)]
enum SaveDialogOutcome {
    Selected(super::resource::ResourceUri),
    Cancelled,
    Failed(String),
}

type SaveDialogFuture = Pin<Box<dyn Future<Output = SaveDialogOutcome> + Send + 'static>>;

trait ProductSaveDialog: Send + Sync {
    fn select(&self, suggested_name: SharedString) -> SaveDialogFuture;
}

struct ApplicationSaveDialog(Arc<dyn ProductSaveDialog>);

impl Global for ApplicationSaveDialog {}

impl ApplicationWorkbenches {
    fn register(&self, workbench: &Entity<Workbench>, window: AnyWindowHandle) {
        let mut workbenches = self.0.borrow_mut();
        workbenches.retain(|(registered, _)| *registered != window);
        workbenches.push((window, workbench.downgrade()));
    }

    fn window_for(&self, workbench: &Entity<Workbench>) -> Option<AnyWindowHandle> {
        self.0.borrow().iter().find_map(|(window, registered)| {
            (registered.upgrade().as_ref() == Some(workbench)).then_some(*window)
        })
    }

    fn view_count(&self, document: DocumentId, cx: &App) -> usize {
        let mut workbenches = self.0.borrow_mut();
        let windows = cx.windows();
        workbenches.retain(|(window, workbench)| {
            windows.contains(window) && workbench.upgrade().is_some()
        });
        workbenches
            .iter()
            .filter_map(|(_, workbench)| workbench.upgrade())
            .map(|workbench| workbench.read(cx).view_count(document))
            .sum()
    }
}

pub(super) fn workbench_is_live(
    window: AnyWindowHandle,
    workbench: &Entity<Workbench>,
    cx: &App,
) -> bool {
    cx.windows().contains(&window)
        && cx.global::<ApplicationWorkbenches>().window_for(workbench) == Some(window)
}

#[derive(Clone)]
struct OpenTarget {
    window: AnyWindowHandle,
    workbench: WeakEntity<Workbench>,
    pane: PaneId,
    tab: TabId,
    surface: TabSurfaceId,
    presentation: Option<EntityId>,
}

impl OpenTarget {
    fn from_command(target: &ProductCommandTarget) -> Self {
        Self {
            window: target.window,
            workbench: target.workbench.clone(),
            pane: target.pane,
            tab: target.tab,
            surface: target.surface,
            presentation: target.presentation,
        }
    }

    fn matches_presentation(&self, workbench: &Entity<Workbench>, cx: &App) -> bool {
        self.workbench.upgrade().as_ref() == Some(workbench)
            && workbench
                .read(cx)
                .capture_tab_move(self.pane, self.tab)
                .is_some_and(|capture| {
                    capture.tab.surface == self.surface && capture.presentation == self.presentation
                })
    }
}

/// State committed by resource Open, independent of command dispatch outcomes.
#[derive(Clone)]
enum OpenCommit {
    Document {
        document: DocumentId,
        target: OpenTarget,
    },
    Workspace(
        #[cfg_attr(
            not(test),
            allow(dead_code, reason = "tests inspect the installed workspace")
        )]
        super::workspace::WorkspaceSnapshot,
    ),
}

#[derive(Debug)]
enum OpenFailure {
    InvalidTarget,
    Cancelled,
    Resource(super::open::OpenResourceError),
}

impl OpenFailure {
    fn command_outcome(self) -> crate::host::protocol::CommandOutcome {
        use crate::host::protocol::CommandOutcome;
        match self {
            Self::InvalidTarget => CommandOutcome::InvalidTarget,
            Self::Cancelled => CommandOutcome::Cancelled,
            Self::Resource(error) => CommandOutcome::HandlerFailure {
                message: error.to_string(),
            },
        }
    }
}

fn target_document(target: &ProductCommandTarget) -> Option<DocumentId> {
    match target.surface {
        TabSurfaceId::Document(document) => Some(document),
        TabSurfaceId::Terminal(_) | TabSurfaceId::Tool(_) => None,
    }
}

pub(crate) struct ProductShell {
    workbench: Entity<Workbench>,
    window: Option<AnyWindowHandle>,
    tab_drag: std::rc::Rc<RefCell<tab_drag::TabDragState>>,
    dock_resize: Option<DockResizeDrag>,
    split_resize: Option<SplitResizeDrag>,
    recent_walk: Option<navigation::RecentWalk>,
    /// The pane rendered over the whole workbench, while it stays focused.
    maximized: Option<PaneId>,
    /// Closed tabs Reopen Closed Tab recreates, most recent last.
    closed_tabs: Vec<reopen::ClosedTab>,
    command_palette: Option<CommandPalette<ProductCommandTarget>>,
    command_palette_subscription: Option<Subscription>,
    transient: Option<ActiveTransient>,
    workspace: WorkspaceState,
    /// The document of the last focused document tab, which new terminals
    /// follow while a terminal or tool has focus.
    last_focused_document: Option<DocumentId>,
    application_observer: Option<Subscription>,
    application_activation: Option<Subscription>,
    _appearance_observer: Option<Subscription>,
    application_focus: Vec<(FocusHandle, Subscription, Subscription)>,
    workspace_tree: Option<Entity<WorkspaceTree>>,
    workspace_tree_subscription: Option<Subscription>,
    contributed_views: BTreeMap<String, ContributedView>,
    _contributed_view_release: Option<Subscription>,
    _contributed_view_window_closed: Option<Subscription>,
    _extension_host_release: Option<Subscription>,
    open_generation: u64,
    tasks: Vec<Task<()>>,
    extension_report_open: bool,
    extension_report_subscription: Option<Subscription>,
    view_state: view_state::ViewStateTracking,
    /// Creation order across product windows; window identities are reused.
    created: u64,
}

/// One live transient interaction owned by this window. Native interactions
/// have no extension owner and are never cancelled by extension lifecycles.
struct ActiveTransient {
    owner: Option<TransientOwner>,
    surface: Option<TransientSurface>,
    focus: Option<FocusHandle>,
    completion: Option<tokio::sync::oneshot::Sender<TransientOutcome>>,
    restore_focus: Option<WeakFocusHandle>,
    _subscription: Option<Subscription>,
}

impl ActiveTransient {
    fn render(&self) -> Option<AnyElement> {
        self.surface.as_ref().map(|surface| match surface {
            TransientSurface::Picker(view) => view.clone().into_any_element(),
            TransientSurface::Input(view) => view.clone().into_any_element(),
        })
    }

    fn settle(&mut self, outcome: TransientOutcome) {
        if let Some(completion) = self.completion.take() {
            let _ = completion.send(outcome);
        }
    }
}

struct ContributedView {
    content: super::workbench::ExtensionView,
    _event: Option<Subscription>,
    _observer: Subscription,
}

impl ContributedView {
    fn instance_id(&self, cx: &App) -> crate::host::protocol::ViewId {
        self.content.instance_id(cx)
    }

    #[cfg(test)]
    fn tree(&self) -> &Entity<super::tree_view::TreeView> {
        match &self.content {
            super::workbench::ExtensionView::Tree(view) => view,
            super::workbench::ExtensionView::Text(_) => {
                panic!("contributed view is not a tree")
            }
        }
    }
}

#[derive(Clone, Copy)]
struct DockResizeDrag {
    dock: DockLocation,
    start_pointer: Point<Pixels>,
    start_size: f32,
}

/// An ordinary divider being dragged: its split, identified by its boundary
/// pane, and the split's painted extent along its direction.
#[derive(Clone, Copy)]
struct SplitResizeDrag {
    boundary: PaneId,
    horizontal: bool,
    start_pointer: Point<Pixels>,
    start_ratio: f32,
    extent: f32,
}

/// The smallest extent, in logical pixels, a divider drag leaves either side.
const MIN_DRAGGED_PANE_SIZE: f32 = 120.;

impl ProductShell {
    fn new(workbench: Entity<Workbench>) -> Self {
        Self {
            workbench,
            window: None,
            tab_drag: Default::default(),
            dock_resize: None,
            split_resize: None,
            recent_walk: None,
            maximized: None,
            closed_tabs: Vec::new(),
            command_palette: None,
            command_palette_subscription: None,
            transient: None,
            workspace: WorkspaceState::default(),
            last_focused_document: None,
            application_observer: None,
            application_activation: None,
            _appearance_observer: None,
            application_focus: Vec::new(),
            workspace_tree: None,
            workspace_tree_subscription: None,
            contributed_views: BTreeMap::new(),
            _contributed_view_release: None,
            _contributed_view_window_closed: None,
            _extension_host_release: None,
            open_generation: 0,
            tasks: Vec::new(),
            extension_report_open: false,
            extension_report_subscription: None,
            view_state: Default::default(),
            created: {
                static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            },
        }
    }

    /// Orders product windows by creation.
    pub(crate) fn creation_order(&self) -> u64 {
        self.created
    }

    fn observe_extension_host(&mut self, native_window: &mut Window, cx: &mut Context<Self>) {
        self.window = Some(native_window.window_handle());
        self.application_observer = Some(cx.observe(&cx.entity(), |this, _, cx| {
            super::extensions::host::application_changed(cx);
            this.schedule_view_state(cx);
        }));
        self.observe_window_bounds(native_window, cx);
        self.application_activation =
            Some(cx.observe_window_activation(native_window, |_, _, cx| {
                super::extensions::host::application_changed(cx)
            }));
        self._appearance_observer = Some(
            native_window.observe_window_appearance(|_, cx| super::themes::selection_changed(cx)),
        );
        super::extensions::host::application_changed(cx);
        // A closed window drops its live frames and any pending transient
        // interaction; other windows keep theirs.
        let shell = cx.weak_entity();
        self._contributed_view_window_closed = Some(cx.on_window_closed(move |cx| {
            let shell = shell.clone();
            cx.defer(move |cx| {
                let Some(shell) = shell.upgrade() else { return };
                shell.update(cx, |this, cx| {
                    let Some(window) = this.window else { return };
                    if cx.windows().contains(&window) {
                        return;
                    }
                    let ids = this.contributed_views.keys().cloned().collect::<Vec<_>>();
                    for id in ids {
                        this.detach_contributed_view(&id, cx);
                    }
                    this.finish_transient(TransientOutcome::Cancelled, cx);
                });
            });
        }));
        if let Some(host) = super::extensions::host::entity(cx) {
            self.extension_report_subscription = Some(cx.observe(&host, |this, _, cx| {
                this.sync_contributed_views(cx);
                cx.notify();
            }));
            self._contributed_view_release = Some(cx.on_release(|this, cx| {
                for (_, instance) in std::mem::take(&mut this.contributed_views) {
                    super::extensions::host::detach_view_instance(instance.instance_id(cx), cx);
                }
            }));
            // Host notifications reconcile definitions while it lives; release
            // removes every remaining frame without waiting for another event.
            self._extension_host_release = Some(cx.observe_release(&host, |this, _, cx| {
                // The host is leased and losing ownership, so drop the frames
                // without calling back into it.
                let frames = std::mem::take(&mut this.contributed_views);
                for (id, _instance) in frames {
                    if let Some(frame) = this.workbench.read(cx).tool_by_stable_id(&id) {
                        this.workbench
                            .update(cx, |workbench, _| workbench.unregister_tool(frame));
                    }
                }
            }));
            self.sync_contributed_views(cx);
        }
    }

    fn observe_application_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handles = self
            .workbench
            .read(cx)
            .panes()
            .iter()
            .flat_map(|pane| pane.tabs())
            .filter_map(|tab| tab.command_view().map(|view| view.focus_handle(cx)))
            .collect::<Vec<_>>();
        self.application_focus
            .retain(|(handle, _, _)| handles.contains(handle));
        for handle in handles {
            if self
                .application_focus
                .iter()
                .any(|(current, _, _)| current == &handle)
            {
                continue;
            }
            // A view focused before its subscription exists sends no event.
            if handle.is_focused(window) {
                self.remember_focused_document(&handle, cx);
            }
            self.application_focus.push((
                handle.clone(),
                cx.on_focus(&handle, window, {
                    let handle = handle.clone();
                    move |this, _, cx| {
                        this.remember_focused_document(&handle, cx);
                        super::extensions::host::application_changed(cx);
                        this.schedule_view_state(cx);
                    }
                }),
                cx.on_blur(&handle, window, |_, _, cx| {
                    super::extensions::host::application_changed(cx)
                }),
            ));
        }
        self.observe_editor_view_state(cx);
    }

    fn remember_focused_document(&mut self, handle: &FocusHandle, cx: &App) {
        let document = self
            .workbench
            .read(cx)
            .panes()
            .iter()
            .flat_map(|pane| pane.tabs())
            .find(|tab| {
                tab.command_view()
                    .is_some_and(|view| &view.focus_handle(cx) == handle)
            })
            .and_then(|tab| tab.document_id());
        if document.is_some() {
            self.last_focused_document = document;
        }
    }

    /// The directory a new terminal in this window starts in: the focused
    /// terminal's current directory, otherwise the window's default.
    fn terminal_directory(&self, cx: &App) -> Option<std::path::PathBuf> {
        let focused = self
            .workbench
            .read(cx)
            .focused_pane()
            .map(|pane| pane.active_tab().surface_id());
        match focused {
            Some(TabSurfaceId::Terminal(id)) => Self::terminal_session_directory(id, cx),
            _ => None,
        }
        .or_else(|| self.default_terminal_directory(cx))
    }

    fn terminal_session_directory(id: TerminalSessionId, cx: &App) -> Option<std::path::PathBuf> {
        let sessions = cx.global::<ApplicationTerminalSessions>().0.borrow();
        sessions
            .sessions
            .get(&id)
            .and_then(|session| session.read(cx).current_directory())
    }

    /// The window's default terminal directory: the root of its most recently
    /// focused document, otherwise its first root, otherwise home.
    fn default_terminal_directory(&self, cx: &App) -> Option<std::path::PathBuf> {
        let document = self
            .workbench
            .read(cx)
            .focused_pane()
            .and_then(|pane| pane.active_tab().document_id())
            .or(self.last_focused_document);
        let documents = Self::documents(cx);
        let uri = document
            .and_then(|document| documents.read(cx).get(document))
            .and_then(|document| document.resource_uri());
        let roots = self.workspace.snapshot();
        let roots = roots
            .roots()
            .iter()
            .map(|root| root.uri.clone())
            .collect::<Vec<_>>();
        super::terminal::directory::working_directory(
            &roots,
            uri,
            directories::BaseDirs::new().map(|base| base.home_dir().to_owned()),
        )
    }

    pub(crate) fn application_window(
        &self,
        window: &Window,
        buffers: &std::collections::HashMap<DocumentId, crate::host::protocol::BufferHandle>,
        cx: &App,
    ) -> crate::host::protocol::WindowInfo {
        use crate::host::protocol::{
            PaneInfo, PaneTarget, PresentationInfo, PresentationTarget, SurfaceKind, WindowInfo,
            WindowTarget,
        };
        let target = WindowTarget {
            window: window.window_handle().window_id().as_u64().to_string(),
        };
        let workbench = self.workbench.read(cx);
        let documents = cx.global::<ApplicationDocuments>().0.read(cx);
        let mut current_target = None;
        let panes = workbench
            .panes()
            .iter()
            .map(|pane| {
                let pane_target = PaneTarget {
                    window: target.clone(),
                    workbench: self.workbench.entity_id().as_u64().to_string(),
                    pane: pane.id().value().to_string(),
                };
                let tabs = pane
                    .tabs()
                    .iter()
                    .map(|tab| {
                        let presentation = tab
                            .presentation()
                            .map(|id| id.as_u64().to_string())
                            .unwrap_or_default();
                        let view_target = PresentationTarget {
                            pane: pane_target.clone(),
                            tab: tab.id().value().to_string(),
                            presentation,
                        };
                        if let Some(focus) = window.focused(cx)
                            && tab
                                .command_view()
                                .is_some_and(|view| view.matches_focus(&focus.downgrade(), cx))
                        {
                            current_target = Some(view_target.clone());
                        }
                        let document = tab.document_id();
                        PresentationInfo {
                            target: view_target,
                            kind: match tab.surface_id() {
                                TabSurfaceId::Document(_) => SurfaceKind::Document,
                                TabSurfaceId::Terminal(_) => SurfaceKind::Terminal,
                                TabSurfaceId::Tool(_) => SurfaceKind::Tool,
                            },
                            document: document
                                .and_then(|id| documents.get(id))
                                .map(|document| document.identity().to_string()),
                            buffer: document.and_then(|id| buffers.get(&id).copied()),
                            tool: match tab.surface_id() {
                                TabSurfaceId::Tool(id) => {
                                    workbench.tool(id).map(|tool| tool.stable_id.clone())
                                }
                                TabSurfaceId::Document(_) | TabSurfaceId::Terminal(_) => None,
                            },
                        }
                    })
                    .collect::<Vec<_>>();
                let selected = tabs
                    .iter()
                    .find(|tab| tab.target.tab == pane.active_tab_id().value().to_string())
                    .unwrap()
                    .target
                    .clone();
                PaneInfo {
                    target: pane_target,
                    tabs,
                    selected,
                }
            })
            .collect();
        let workspace = self.workspace.snapshot();
        WindowInfo {
            target: target.clone(),
            panes,
            current_target,
            workspace: operations::workspace_info(target, &workspace),
        }
    }

    fn command_dispatcher(cx: &App) -> Entity<ProductCommandDispatcher> {
        cx.global::<ApplicationProductCommands>().0.clone()
    }

    fn create_terminal(
        &mut self,
        cx: &mut Context<Self>,
    ) -> (TerminalSessionId, Entity<TerminalView>) {
        let directory = self.default_terminal_directory(cx);
        self.create_terminal_in(directory, cx)
    }

    fn create_terminal_in(
        &mut self,
        directory: Option<std::path::PathBuf>,
        cx: &mut Context<Self>,
    ) -> (TerminalSessionId, Entity<TerminalView>) {
        let id = cx
            .global::<ApplicationTerminalSessions>()
            .0
            .borrow_mut()
            .allocate_id();
        let session = cx.new(|cx| TerminalSession::new(directory, cx));
        let view = cx.new(|cx| TerminalView::new(session.clone(), cx));
        cx.global::<ApplicationTerminalSessions>()
            .0
            .borrow_mut()
            .sessions
            .insert(id, session);
        (id, view)
    }

    fn close_terminal(&mut self, id: TerminalSessionId, cx: &mut Context<Self>) {
        let session = cx
            .global::<ApplicationTerminalSessions>()
            .0
            .borrow_mut()
            .remove(id);
        if let Some(session) = session {
            session.update(cx, |session, cx| session.close(cx));
        }
    }

    fn close_terminal_ids(ids: impl IntoIterator<Item = TerminalSessionId>, cx: &mut App) {
        for id in ids {
            let session = cx
                .global::<ApplicationTerminalSessions>()
                .0
                .borrow_mut()
                .remove(id);
            if let Some(session) = session {
                session.update(cx, |session, cx| session.close(cx));
            }
        }
    }

    fn focus_active_surface(&self, window: &mut Window, cx: &App) {
        let Some(view) = self
            .workbench
            .read(cx)
            .focused_pane()
            .and_then(|pane| pane.active_tab().command_view())
        else {
            return;
        };
        view.focus_handle(cx).focus(window);
    }

    pub(crate) fn capture_command_target(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<ProductCommandTarget> {
        self.sync_focused_pane(window, cx);
        let focus = window.focused(cx)?;
        let workbench = self.workbench.read(cx);
        let pane = workbench.focused_pane()?;
        let tab = pane.active_tab();
        let view = match tab.command_view() {
            Some(super::workbench::CommandView::Extension(extension))
                if extension.focus_handle(cx) == focus =>
            {
                Some(extension.instance_id(cx))
            }
            _ => None,
        };
        Some(ProductCommandTarget {
            window: window.window_handle(),
            shell: cx.entity().downgrade(),
            workbench: self.workbench.downgrade(),
            pane: pane.id(),
            tab: tab.id(),
            surface: tab.surface_id(),
            presentation: tab.presentation(),
            focus: focus.downgrade(),
            view,
        })
    }

    fn window_close_target(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<ProductCommandTarget> {
        if let Some(target) = self.capture_command_target(window, cx) {
            return Some(target);
        }
        let workbench = self.workbench.read(cx);
        let pane = workbench.panes().first()?;
        let tab = pane.tabs().first()?;
        Some(ProductCommandTarget {
            window: window.window_handle(),
            shell: cx.entity().downgrade(),
            workbench: self.workbench.downgrade(),
            pane: pane.id(),
            tab: tab.id(),
            surface: tab.surface_id(),
            presentation: tab.presentation(),
            focus: tab
                .command_view()
                .map(|view| view.focus_handle(cx).downgrade())
                .or_else(|| window.focused(cx).map(|focus| focus.downgrade()))?,
            view: None,
        })
    }

    fn dispatch_command(
        &mut self,
        command: crate::host::protocol::Command,
        target: ProductCommandTarget,
        cx: &mut Context<Self>,
    ) {
        Self::command_dispatcher(cx).update(cx, |dispatcher, cx| {
            dispatcher.dispatch(command, target, cx);
        });
    }

    fn dispatch_source(
        &mut self,
        source: &ProductCommandSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.capture_command_target(window, cx) else {
            cx.notify();
            return;
        };
        self.dispatch_command(source.command(), target, cx);
    }

    fn dispatch_keybinding(
        &mut self,
        action: &KeymapAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.command_palette.is_some() {
            cx.propagate();
            return;
        }
        let Some(target) = self.capture_command_target(window, cx) else {
            cx.propagate();
            return;
        };
        let view_kind = Self::target_view_kind(&target, cx);
        let resolution = cx
            .global::<ApplicationKeymaps>()
            .0
            .resolve(&action.key, view_kind.as_deref());
        match resolution {
            Some(BindingResolution::Command(command)) => self.dispatch_command(command, target, cx),
            Some(BindingResolution::Unbound) => cx.propagate(),
            None => cx.propagate(),
        }
    }

    #[cfg(test)]
    fn open_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(origin) = self.capture_command_target(window, cx) else {
            return;
        };
        self.open_command_palette_from(origin, window, cx);
    }

    fn record_palette_command(name: &str, cx: &App) {
        let Some(history) = cx.try_global::<ApplicationCommandHistory>() else {
            return;
        };
        let catalog = Self::command_dispatcher(cx).read(cx).catalog();
        let catalog = catalog.borrow();
        history
            .0
            .record(name, |recent| catalog.resolve(recent).is_ok());
    }

    /// The keymap view kind of a captured target's focused tab; contributed
    /// frames and native tools share this path instead of a shell-level
    /// special case.
    fn target_view_kind(target: &ProductCommandTarget, cx: &App) -> Option<String> {
        target.command_view(cx).and_then(|view| {
            view.matches_focus(&target.focus, cx).then(|| match view {
                super::workbench::CommandView::Editor(_) => "editor".to_owned(),
                super::workbench::CommandView::Terminal(_) => "terminal".to_owned(),
                super::workbench::CommandView::Workspace(_) => "workspace-tree".to_owned(),
                super::workbench::CommandView::Extension(view) => view.kind(cx),
            })
        })
    }

    fn open_command_palette_from(
        &mut self,
        origin: ProductCommandTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let definitions = Self::command_dispatcher(cx)
            .read(cx)
            .definitions()
            .collect::<Vec<_>>();
        let bindings = cx
            .try_global::<ApplicationKeymaps>()
            .map(|keymaps| {
                keymaps
                    .0
                    .shown_bindings(Self::target_view_kind(&origin, cx).as_deref())
            })
            .unwrap_or_default();
        let recent = cx
            .try_global::<ApplicationCommandHistory>()
            .map(|history| history.0.recent())
            .unwrap_or_default();
        let palette = CommandPalette::new(definitions, &bindings, &recent, origin, cx);
        let picker = palette.picker().clone();
        let subscription = cx.subscribe(&picker, |this, _picker, event: &PickerEvent, cx| {
            if matches!(event, PickerEvent::Highlighted(_)) {
                return;
            }
            let Some(palette) = this.command_palette.take() else {
                return;
            };
            this.command_palette_subscription = None;
            // Give focus back before dispatch, so a command that moves focus
            // or opens a transient takes it from the origin.
            this.restore_palette_origin_focus(&palette.origin(), cx);
            if let PickerEvent::Confirmed(entry) = event {
                let command = palette.command(*entry);
                Self::record_palette_command(command.name.as_ref(), cx);
                this.dispatch_command(command, palette.origin(), cx);
            }
            this.refocus_active_transient(cx);
            cx.notify();
        });
        window.focus(&picker.read(cx).focus_handle(cx));
        self.command_palette = Some(palette);
        self.command_palette_subscription = Some(subscription);
        cx.notify();
    }

    fn transient_slot_free(&self) -> Result<(), TransientError> {
        if self.transient.is_some() {
            Err(TransientError::OperationInProgress)
        } else {
            Ok(())
        }
    }

    fn transient_restore_focus(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<WeakFocusHandle> {
        window.focused(cx).map(|focus| focus.downgrade())
    }

    fn finish_transient(&mut self, outcome: TransientOutcome, cx: &mut Context<Self>) {
        let Some(mut active) = self.transient.take() else {
            return;
        };
        active.settle(outcome);
        self.restore_focus(active.restore_focus, cx);
        cx.notify();
    }

    fn restore_focus(&self, restore: Option<WeakFocusHandle>, cx: &mut Context<Self>) {
        let (Some(window), Some(focus)) = (self.window, restore.and_then(|focus| focus.upgrade()))
        else {
            return;
        };
        let _ = cx.update_window(window, |_, window, _| window.focus(&focus));
    }

    /// Focus the view the palette opened from, or the focused pane's active
    /// surface when that view is gone.
    fn restore_palette_origin_focus(&self, origin: &ProductCommandTarget, cx: &mut Context<Self>) {
        let Some(window) = self.window else {
            return;
        };
        let focus = origin.focus.upgrade();
        let _ = cx.update_window(window, |_, window, cx| match focus {
            Some(focus) => window.focus(&focus),
            None => self.focus_active_surface(window, cx),
        });
    }

    fn refocus_active_transient(&self, cx: &mut Context<Self>) {
        let Some(focus) = self
            .transient
            .as_ref()
            .and_then(|active| active.focus.clone())
        else {
            return;
        };
        let Some(window) = self.window else {
            return;
        };
        let _ = cx.update_window(window, |_, window, _| window.focus(&focus));
    }

    /// Cancel the live interaction when it belongs to the requested owner.
    /// A `None` invocation cancels any interaction of that extension lifetime.
    pub(crate) fn cancel_transient(&mut self, owner: TransientOwner, cx: &mut Context<Self>) {
        let matches = self
            .transient
            .as_ref()
            .and_then(|active| active.owner)
            .is_some_and(|active| {
                active.extension == owner.extension
                    && active.lifecycle == owner.lifecycle
                    && (owner.invocation.is_none() || active.invocation == owner.invocation)
            });
        if matches {
            self.finish_transient(TransientOutcome::Cancelled, cx);
        }
    }

    /// Present a lifecycle-authorized picker, or reject when another extension
    /// interaction already owns this window.
    pub(crate) fn begin_picker(
        &mut self,
        owner: TransientOwner,
        title: String,
        items: Vec<crate::host::protocol::PickerItem>,
        highlights: Option<tokio::sync::watch::Sender<Option<String>>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<tokio::sync::oneshot::Receiver<TransientOutcome>, TransientError> {
        self.begin_owned_picker(Some(owner), title, items, highlights, window, cx)
    }

    /// Present a picker in the transient slot. With `highlights`, the ID of
    /// the highlighted item is published as it changes, starting with the
    /// initial one.
    fn begin_owned_picker(
        &mut self,
        owner: Option<TransientOwner>,
        title: String,
        items: Vec<crate::host::protocol::PickerItem>,
        highlights: Option<tokio::sync::watch::Sender<Option<String>>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<tokio::sync::oneshot::Receiver<TransientOutcome>, TransientError> {
        self.transient_slot_free()?;
        let restore_focus = self.transient_restore_focus(window, cx);
        let (ids, entries): (Vec<_>, Vec<_>) = items
            .into_iter()
            .map(|item| {
                let entry = PickerEntry {
                    description: item.description,
                    detail: item.detail,
                    hint: item.hint,
                    ..PickerEntry::new(item.label)
                };
                (item.id, entry)
            })
            .unzip();
        let view = cx.new(|cx| Picker::new(Some(title), entries, super::TRANSIENT_KEY_CONTEXT, cx));
        let focus = view.read(cx).focus_handle(cx);
        let id_of = move |entry: Option<usize>| entry.map(|entry| ids[entry].clone());
        if let Some(highlights) = &highlights {
            highlights.send_replace(id_of(view.read(cx).selected_entry()));
        }
        let subscription = cx.subscribe(
            &view,
            move |this: &mut Self, _view, event: &PickerEvent, cx| {
                let outcome = match event {
                    PickerEvent::Highlighted(entry) => {
                        if let Some(highlights) = &highlights {
                            highlights.send_replace(id_of(*entry));
                        }
                        return;
                    }
                    PickerEvent::Confirmed(entry) => {
                        TransientOutcome::Picker(id_of(Some(*entry)).expect("confirmed entry"))
                    }
                    PickerEvent::Dismissed => TransientOutcome::Dismissed,
                };
                this.finish_transient(outcome, cx);
            },
        );
        let (completion, receiver) = tokio::sync::oneshot::channel();
        self.transient = Some(ActiveTransient {
            owner,
            surface: Some(TransientSurface::Picker(view)),
            focus: Some(focus.clone()),
            completion: Some(completion),
            restore_focus,
            _subscription: Some(subscription),
        });
        window.focus(&focus);
        cx.notify();
        Ok(receiver)
    }

    /// Present a lifecycle-authorized single-line input, or reject when another
    /// extension interaction already owns this window.
    pub(crate) fn begin_input(
        &mut self,
        owner: TransientOwner,
        title: String,
        initial: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<tokio::sync::oneshot::Receiver<TransientOutcome>, TransientError> {
        self.transient_slot_free()?;
        let restore_focus = self.transient_restore_focus(window, cx);
        let view = cx.new(|cx| InputView::new(title, initial, cx));
        let focus = view.read(cx).focus_handle(cx);
        let subscription = cx.subscribe(&view, |this: &mut Self, _view, event: &InputEvent, cx| {
            this.finish_transient(
                match event {
                    InputEvent::Accepted(value) => TransientOutcome::Input(value.clone()),
                    InputEvent::Dismissed => TransientOutcome::Dismissed,
                },
                cx,
            );
        });
        let (completion, receiver) = tokio::sync::oneshot::channel();
        self.transient = Some(ActiveTransient {
            owner: Some(owner),
            surface: Some(TransientSurface::Input(view)),
            focus: Some(focus.clone()),
            completion: Some(completion),
            restore_focus,
            _subscription: Some(subscription),
        });
        window.focus(&focus);
        cx.notify();
        Ok(receiver)
    }

    /// Present the native confirmation prompt, settling the shared slot when it
    /// is answered.
    pub(crate) fn begin_confirmation(
        &mut self,
        owner: TransientOwner,
        text: PromptText,
        confirm_label: String,
        cancel_label: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<tokio::sync::oneshot::Receiver<TransientOutcome>, TransientError> {
        self.transient_slot_free()?;
        let restore_focus = self.transient_restore_focus(window, cx);
        let prompt = window.prompt(
            PromptLevel::Warning,
            &text.title,
            text.message.as_deref(),
            &[
                PromptButton::new(confirm_label),
                PromptButton::cancel(cancel_label),
            ],
            cx,
        );
        let (completion, receiver) = tokio::sync::oneshot::channel();
        self.transient = Some(ActiveTransient {
            owner: Some(owner),
            surface: None,
            focus: None,
            completion: Some(completion),
            restore_focus,
            _subscription: None,
        });
        cx.spawn(async move |this, cx| {
            let outcome = match prompt.await {
                Ok(0) => TransientOutcome::Confirmed,
                _ => TransientOutcome::Dismissed,
            };
            let _ = this.update(cx, |this, cx| this.finish_transient(outcome, cx));
        })
        .detach();
        cx.notify();
        Ok(receiver)
    }

    /// Present the native message prompt. Resolution carries no result, but the
    /// interaction still occupies the shared slot while it is open.
    pub(crate) fn begin_message(
        &mut self,
        owner: TransientOwner,
        text: PromptText,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<tokio::sync::oneshot::Receiver<TransientOutcome>, TransientError> {
        self.transient_slot_free()?;
        let restore_focus = self.transient_restore_focus(window, cx);
        let prompt = window.prompt(
            PromptLevel::Info,
            &text.title,
            text.message.as_deref(),
            &[PromptButton::new("OK")],
            cx,
        );
        let (completion, receiver) = tokio::sync::oneshot::channel();
        self.transient = Some(ActiveTransient {
            owner: Some(owner),
            surface: None,
            focus: None,
            completion: Some(completion),
            restore_focus,
            _subscription: None,
        });
        cx.spawn(async move |this, cx| {
            let _ = prompt.await;
            let _ = this.update(cx, |this, cx| {
                this.finish_transient(TransientOutcome::Dismissed, cx)
            });
        })
        .detach();
        cx.notify();
        Ok(receiver)
    }

    /// Present the native directory prompt, settling the shared slot when it is
    /// answered. Path validation and normalization belong to the caller.
    pub(crate) fn begin_folder_picker(
        &mut self,
        owner: Option<TransientOwner>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<tokio::sync::oneshot::Receiver<TransientOutcome>, TransientError> {
        self.transient_slot_free()?;
        let restore_focus = self.transient_restore_focus(window, cx);
        let selection: OpenDialogFuture =
            if let Some(dialog) = cx.try_global::<ApplicationOpenDialog>() {
                dialog.0.select()
            } else {
                let selection = cx.prompt_for_paths(PathPromptOptions {
                    files: false,
                    directories: true,
                    multiple: false,
                    prompt: Some("Choose Folder".into()),
                });
                Box::pin(async move {
                    match selection.await {
                        Ok(Ok(Some(paths))) => paths
                            .into_iter()
                            .next()
                            .map(OpenDialogOutcome::Selected)
                            .unwrap_or(OpenDialogOutcome::Cancelled),
                        Ok(Ok(None)) => OpenDialogOutcome::Cancelled,
                        Ok(Err(error)) => OpenDialogOutcome::Failed(error.to_string()),
                        Err(error) => OpenDialogOutcome::Failed(error.to_string()),
                    }
                })
            };
        let (completion, receiver) = tokio::sync::oneshot::channel();
        self.transient = Some(ActiveTransient {
            owner,
            surface: None,
            focus: None,
            completion: Some(completion),
            restore_focus,
            _subscription: None,
        });
        cx.spawn(async move |this, cx| {
            let outcome = match selection.await {
                OpenDialogOutcome::Selected(path) => TransientOutcome::Folder(path),
                OpenDialogOutcome::Cancelled => TransientOutcome::Dismissed,
                OpenDialogOutcome::Failed(message) => TransientOutcome::Failed(message),
            };
            let _ = this.update(cx, |this, cx| this.finish_transient(outcome, cx));
        })
        .detach();
        cx.notify();
        Ok(receiver)
    }

    fn render_transient(&self) -> Option<AnyElement> {
        self.transient.as_ref().and_then(|active| active.render())
    }

    fn sync_focused_pane(&self, window: &Window, cx: &mut Context<Self>) {
        let Some(focus) = window.focused(cx) else {
            return;
        };
        let focused_pane = self.workbench.read(cx).panes().iter().find_map(|pane| {
            pane.tabs()
                .iter()
                .any(|tab| {
                    tab.command_view()
                        .is_some_and(|view| view.matches_focus(&focus.downgrade(), cx))
                })
                .then_some(pane.id())
        });
        if let Some(pane) = focused_pane {
            self.workbench.update(cx, |workbench, _| {
                workbench.focus_pane(pane);
            });
        }
    }

    fn documents(cx: &App) -> Entity<DocumentCollection> {
        cx.global::<ApplicationDocuments>().0.clone()
    }

    fn filesystems(cx: &App) -> Arc<FileSystemProviderRegistry> {
        cx.global::<ApplicationFileSystems>().0.clone()
    }

    #[cfg(test)]
    fn new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_focused_pane(window, cx);
        let Some(pane) = self.workbench.read(cx).focused_pane_id() else {
            return;
        };
        self.new_document_in_pane(pane, window, cx);
    }

    fn new_document_in_pane(
        &mut self,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<(DocumentId, OpenTarget)> {
        self.next_open_generation();
        self.workbench.read(cx).pane(pane)?;
        let documents = Self::documents(cx);
        let model = cx.new(|_| BufferModel::from_text(""));
        let document = documents.update(cx, |documents, cx| {
            documents.create_untitled("Untitled", model, cx)
        });
        let model = documents.read(cx).get(document).unwrap().model().clone();
        let (pane, tab) = self.workbench.update(cx, |workbench, cx| {
            if workbench.dock_location(pane).is_some() {
                workbench.open_document_in_normal_group(document, model, cx)
            } else {
                let tab = workbench
                    .open_tab_for_document(pane, document, model, cx)
                    .unwrap();
                (pane, tab)
            }
        });
        let editor = self
            .workbench
            .read(cx)
            .pane(pane)
            .unwrap()
            .active_tab()
            .editor()
            .unwrap()
            .clone();
        editor.focus_handle(cx).focus(window);
        cx.notify();
        Some((
            document,
            OpenTarget {
                window: window.window_handle(),
                workbench: self.workbench.downgrade(),
                pane,
                tab,
                surface: TabSurfaceId::Document(document),
                presentation: Some(editor.entity_id()),
            },
        ))
    }

    /// Opens a focused terminal starting in `directory`, in `pane` or, when
    /// `pane` is a dock, in the normal group.
    fn new_terminal_in_pane(
        &mut self,
        pane: PaneId,
        directory: Option<std::path::PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<OpenTarget> {
        self.workbench.read(cx).pane(pane)?;
        let (id, view) = self.create_terminal_in(directory, cx);
        let presentation = view.entity_id();
        let Some((pane, tab)) = self.workbench.update(cx, |workbench, _| {
            if workbench.dock_location(pane).is_some() {
                workbench.open_terminal_in_normal_group(id, view)
            } else {
                workbench
                    .open_terminal_tab(pane, id, view)
                    .map(|tab| (pane, tab))
            }
        }) else {
            self.close_terminal(id, cx);
            return None;
        };
        self.focus_active_surface(window, cx);
        cx.notify();
        Some(OpenTarget {
            window: window.window_handle(),
            workbench: self.workbench.downgrade(),
            pane,
            tab,
            surface: TabSurfaceId::Terminal(id),
            presentation: Some(presentation),
        })
    }

    #[cfg(test)]
    fn split(&mut self, direction: SplitDirection, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_focused_pane(window, cx);
        let Some(pane) = self.workbench.read(cx).focused_pane_id() else {
            return;
        };
        self.split_pane(pane, direction, SplitPlacement::After, window, cx);
    }

    fn split_pane(
        &mut self,
        pane: PaneId,
        direction: SplitDirection,
        placement: SplitPlacement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let terminal = match self
            .workbench
            .read(cx)
            .pane(pane)
            .map(|pane| pane.active_tab().surface_id())
        {
            Some(TabSurfaceId::Terminal(id)) => Some(id),
            _ => None,
        };
        let new_session = terminal.map(|id| {
            let directory = Self::terminal_session_directory(id, cx)
                .or_else(|| self.default_terminal_directory(cx));
            self.create_terminal_in(directory, cx)
        });
        let split = self
            .workbench
            .update(cx, |workbench, cx| match new_session.clone() {
                Some((id, view)) => {
                    workbench.split_pane_with_terminal(pane, direction, placement, id, view, cx)
                }
                None => workbench.split_pane(pane, direction, placement, cx),
            });
        if split.is_some() {
            self.focus_active_surface(window, cx);
            cx.notify();
            true
        } else {
            if let Some((id, _)) = new_session {
                self.close_terminal(id, cx);
            }
            false
        }
    }

    fn activate_tab(
        &mut self,
        pane: PaneId,
        tab: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let activated = self.workbench.update(cx, |workbench, _| {
            workbench.activate_tab(pane, tab);
            workbench
                .pane(pane)
                .is_some_and(|pane| pane.active_tab_id() == tab)
        });
        if activated {
            self.focus_active_surface(window, cx);
        }
        cx.notify();
    }

    #[cfg(test)]
    fn focus_active_editor(&self, window: &mut Window, cx: &App) {
        if let Some(editor) = self
            .workbench
            .read(cx)
            .focused_pane()
            .and_then(|pane| pane.active_tab().editor().cloned())
        {
            editor.focus_handle(cx).focus(window);
        }
    }

    fn replace_empty_workbench(&mut self, window: AnyWindowHandle, cx: &mut App) {
        let documents = Self::documents(cx);
        let replacement = create_untitled_document(&documents, cx);
        let model = documents.read(cx).get(replacement).unwrap().model().clone();
        if !self.workbench.read(cx).tools().is_empty() {
            self.workbench.update(cx, |workbench, cx| {
                workbench.open_document_in_normal_group(replacement, model, cx);
            });
            return;
        }
        let workbench = cx.new(|cx| Workbench::new_for_document(replacement, model, cx));
        cx.global::<ApplicationWorkbenches>()
            .register(&workbench, window);
        self.workbench = workbench;
    }

    /// Report an outcome that has no other visible effect in this window. The
    /// prompt needs no answer.
    pub(super) fn show_notice(
        &self,
        level: PromptLevel,
        title: impl Into<String>,
        detail: Option<String>,
        cx: &mut App,
    ) {
        if let Some(window) = self.window {
            show_notice(window, level, title.into(), detail, cx);
        }
    }

    pub(crate) fn toggle_extension_report(&mut self, cx: &mut Context<Self>) {
        self.extension_report_open = !self.extension_report_open;
        cx.notify();
    }
}

/// Present a native prompt after the current update, so callers may hold the
/// shell or the window.
pub(super) fn show_notice(
    window: AnyWindowHandle,
    level: PromptLevel,
    title: String,
    detail: Option<String>,
    cx: &mut App,
) {
    cx.defer(move |cx| {
        let _ = cx.update_window(window, |_, window, cx| {
            drop(window.prompt(
                level,
                &title,
                detail.as_deref(),
                &[PromptButton::new("OK")],
                cx,
            ));
        });
    });
}

fn create_untitled_document(documents: &Entity<DocumentCollection>, cx: &mut App) -> DocumentId {
    let model = cx.new(|_| BufferModel::from_text(""));
    documents.update(cx, |documents, cx| {
        documents.create_untitled("Untitled", model, cx)
    })
}

fn resource_title(uri: &super::resource::ResourceUri) -> String {
    uri.as_url()
        .path_segments()
        .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
        .map(percent_encoding::percent_decode_str)
        .and_then(|title| title.decode_utf8().ok())
        .filter(|title| !title.is_empty())
        .map(|title| title.into_owned())
        .unwrap_or_else(|| uri.to_string())
}

#[cfg(test)]
mod tests;
