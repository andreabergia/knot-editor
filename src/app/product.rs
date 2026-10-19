//! Native product windows backed by application documents and workbenches.

use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
    future::Future,
    path::Path,
    pin::Pin,
    sync::{Arc, OnceLock},
};

use gpui::{prelude::FluentBuilder, *};

pub(super) mod commands;
pub(super) mod operations;
mod relocate;
mod tab_drag;
#[cfg(test)]
use super::product_commands::SHOW_COMPLETIONS_COMMAND;

use super::{
    CommandCompletion, CommandPalette, CommandPaletteEntry, CommandPaletteEvent,
    documents::{ApplicationDocuments, DocumentCollection, DocumentId, DocumentState},
    entry::OpenRequest,
    extension_host::handlers::TransientError,
    filesystem::{
        FileSystemProviderRegistry, LocalFileSystemProvider, ResourceError, ResourceKind,
        ResourceStat, ResourceVersion,
    },
    keymaps::{ApplicationKeymaps, BindingResolution, KeymapAction},
    model::BufferModel,
    open::{OpenResource, enumerate_directory, load_resource},
    product_commands::{
        ApplicationProductCommands, CLOSE_TAB_COMMAND, CLOSE_WINDOW_COMMAND, COPY_COMMAND,
        CUT_COMMAND, FIND_COMMAND, FIND_NEXT_COMMAND, FIND_PREVIOUS_COMMAND,
        MOVE_TERMINAL_TO_NEW_WINDOW_COMMAND, NEW_COMMAND, NEW_TERMINAL_COMMAND, NEW_WINDOW_COMMAND,
        OPEN_COMMAND, PASTE_COMMAND, ProductCommandDispatcher, ProductCommandSource,
        ProductCommandTarget, QUIT_COMMAND, REDO_COMMAND, SAVE_AS_COMMAND, SAVE_COMMAND,
        SELECT_ALL_COMMAND, SHOW_COMMAND_PALETTE_COMMAND, SHOW_EXTENSION_REPORT_COMMAND,
        SPLIT_HORIZONTAL_COMMAND, SPLIT_VERTICAL_COMMAND, UNDO_COMMAND,
        dispatch_close_to_captured_target, dispatch_open_to_captured_target,
        dispatch_save_to_captured_target,
    },
    terminal_session::TerminalSession,
    terminal_view::TerminalView,
    transient::{
        InputEvent, InputView, PickerEvent, PickerView, PromptText, TransientOutcome,
        TransientOwner, TransientSurface,
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

    fn complete(&mut self) -> Vec<Option<OpenRequest>> {
        match std::mem::replace(self, Self::Ready) {
            Self::Pending(mut requests) => {
                if requests.is_empty() {
                    requests.push(None);
                }
                requests
            }
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
    Workspace(super::workspace::WorkspaceSnapshot),
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
    status: SharedString,
    tab_drag: std::rc::Rc<RefCell<tab_drag::TabDragState>>,
    dock_resize: Option<DockResizeDrag>,
    command_palette: Option<Entity<CommandPalette<ProductCommandTarget>>>,
    command_palette_subscription: Option<Subscription>,
    transient: Option<ActiveTransient>,
    workspace: WorkspaceState,
    application_observer: Option<Subscription>,
    application_activation: Option<Subscription>,
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
}

/// One live extension transient interaction owned by this window.
struct ActiveTransient {
    owner: TransientOwner,
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

impl ProductShell {
    fn new(workbench: Entity<Workbench>) -> Self {
        Self {
            workbench,
            window: None,
            status: "ready".into(),
            tab_drag: Default::default(),
            dock_resize: None,
            command_palette: None,
            command_palette_subscription: None,
            transient: None,
            workspace: WorkspaceState::default(),
            application_observer: None,
            application_activation: None,
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
        }
    }

    fn observe_extension_host(&mut self, native_window: &mut Window, cx: &mut Context<Self>) {
        self.window = Some(native_window.window_handle());
        self.application_observer = Some(cx.observe(&cx.entity(), |_, _, cx| {
            super::extension_host::application_changed(cx)
        }));
        self.application_activation =
            Some(cx.observe_window_activation(native_window, |_, _, cx| {
                super::extension_host::application_changed(cx)
            }));
        super::extension_host::application_changed(cx);
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
        if let Some(host) = super::extension_host::entity(cx) {
            self.extension_report_subscription = Some(cx.observe(&host, |this, _, cx| {
                this.sync_contributed_views(cx);
                cx.notify();
            }));
            self._contributed_view_release = Some(cx.on_release(|this, cx| {
                for (_, instance) in std::mem::take(&mut this.contributed_views) {
                    super::extension_host::detach_view_instance(instance.instance_id(cx), cx);
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

    /// Reconcile this window's contributed frames with the application-owned
    /// definitions. Attaching applies remembered placement or registration
    /// defaults once; later host notifications never override a window-local
    /// programmatic hide.
    fn sync_contributed_views(&mut self, cx: &mut Context<Self>) {
        let definitions = super::extension_host::view_definitions(cx);
        let live = definitions
            .iter()
            .map(|definition| definition.id.as_str())
            .collect::<HashSet<_>>();
        let stale = self
            .contributed_views
            .keys()
            .filter(|id| !live.contains(id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        for id in stale {
            self.detach_contributed_view(&id, cx);
        }
        for definition in definitions {
            if !self.contributed_views.contains_key(&definition.id) {
                self.attach_contributed_view(definition, cx);
            }
        }
    }

    fn attach_contributed_view(
        &mut self,
        definition: super::extension_semantics::ViewDefinition,
        cx: &mut Context<Self>,
    ) {
        let Some(window) = self.window else {
            return;
        };
        let window_id = window.window_id().as_u64().to_string();
        let (content, event, observer) = match definition.content {
            crate::host::protocol::ViewContentKind::Tree => {
                let view = cx.new(|cx| {
                    super::tree_view::TreeView::new(definition.id.clone(), window_id.clone(), cx)
                });
                let event = cx.subscribe(&view, |_, _, event, cx| {
                    let event = event.clone();
                    cx.defer(move |cx| super::extension_host::route_tree_event(&event, cx));
                    cx.notify();
                });
                let observer = cx.observe(&view, |_, _, cx| cx.notify());
                (
                    super::workbench::ExtensionView::Tree(view),
                    Some(event),
                    observer,
                )
            }
            crate::host::protocol::ViewContentKind::Text => {
                let view = cx.new(|cx| {
                    super::text_view::TextView::new(definition.id.clone(), window_id.clone(), cx)
                });
                let observer = cx.observe(&view, |_, _, cx| cx.notify());
                (super::workbench::ExtensionView::Text(view), None, observer)
            }
        };
        if !super::extension_host::attach_view_instance(
            definition.id.clone(),
            window_id,
            content.clone(),
            cx,
        ) {
            return;
        }
        let stored = Self::stored_tool_layout(cx);
        let record = stored
            .as_ref()
            .and_then(|snapshot| snapshot.tool(&definition.id))
            .cloned();
        let visible = record
            .as_ref()
            .map_or(definition.visible, |record| record.visible);
        let placement = record
            .as_ref()
            .map_or(definition.location.placement(), |record| {
                record.location.placement()
            });
        let index = record
            .as_ref()
            .map_or(definition.order, |record| record.index);
        let workbench_view = content.clone();
        self.workbench.update(cx, |workbench, _| {
            let previous = workbench.focused_pane_id();
            let id = workbench.register_tool(
                definition.id.clone(),
                definition.title.clone(),
                ToolContent::Extension(workbench_view),
                DockLocation::Left,
            );
            workbench.remember_tool_placement(id, placement, index);
            if visible {
                workbench.show_tool(id);
            }
            if let Some(pane) = previous {
                workbench.focus_pane(pane);
            }
        });
        self.contributed_views.insert(
            definition.id,
            ContributedView {
                content,
                _event: event,
                _observer: observer,
            },
        );
    }

    fn detach_contributed_view(&mut self, stable_id: &str, cx: &mut Context<Self>) {
        let Some(instance) = self.contributed_views.remove(stable_id) else {
            return;
        };
        let view_id = instance.instance_id(cx);
        self.workbench.update(cx, |workbench, _| {
            if let Some(frame) = workbench.tool_by_stable_id(stable_id) {
                workbench.unregister_tool(frame);
            }
        });
        super::extension_host::detach_view_instance(view_id, cx);
    }

    /// Show or hide a contributed frame without publishing a fresh-window
    /// default. Returns false when this window has no live instance.
    pub(crate) fn set_contributed_view_visible(
        &mut self,
        stable_id: &str,
        visible: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(content) = self
            .contributed_views
            .get(stable_id)
            .map(|instance| instance.content.clone())
        else {
            return false;
        };
        let Some(frame) = self.workbench.read(cx).tool_by_stable_id(stable_id) else {
            return false;
        };
        let visible_now = self.workbench.read(cx).tool_tab(frame).is_some();
        if visible_now == visible {
            return true;
        }
        if visible {
            self.workbench.update(cx, |workbench, _| {
                workbench.show_tool(frame);
            });
            if let super::workbench::ExtensionView::Tree(tree) = content {
                tree.update(cx, |tree, cx| tree.refresh_root(cx));
            }
        } else {
            self.workbench.update(cx, |workbench, _| {
                workbench.hide_tool(frame);
            });
        }
        cx.notify();
        true
    }

    /// Show and focus a contributed frame in this window.
    pub(crate) fn focus_contributed_view(
        &mut self,
        stable_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.set_contributed_view_visible(stable_id, true, cx) {
            return false;
        }
        let Some(frame) = self.workbench.read(cx).tool_by_stable_id(stable_id) else {
            return false;
        };
        self.workbench.update(cx, |workbench, _| {
            workbench.show_tool(frame);
        });
        let Some(view) = self
            .contributed_views
            .get(stable_id)
            .map(|instance| instance.content.clone())
        else {
            return false;
        };
        view.focus_handle(cx).focus(window);
        cx.notify();
        true
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
            self.application_focus.push((
                handle.clone(),
                cx.on_focus(&handle, window, |_, _, cx| {
                    super::extension_host::application_changed(cx)
                }),
                cx.on_blur(&handle, window, |_, _, cx| {
                    super::extension_host::application_changed(cx)
                }),
            ));
        }
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
        let id = cx
            .global::<ApplicationTerminalSessions>()
            .0
            .borrow_mut()
            .allocate_id();
        let session = cx.new(TerminalSession::new);
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
            self.status = "command target is no longer available".into();
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
        // Resolve the view kind from the focused tab; contributed frames and
        // native tools share this path instead of a shell-level special case.
        let view_kind = target.command_view(cx).and_then(|view| {
            view.matches_focus(&target.focus, cx).then(|| match view {
                super::workbench::CommandView::Editor(_) => "editor".to_owned(),
                super::workbench::CommandView::Terminal(_) => "terminal".to_owned(),
                super::workbench::CommandView::Workspace(_) => "workspace-tree".to_owned(),
                super::workbench::CommandView::Extension(view) => view.kind(cx),
            })
        });
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

    fn open_command_palette_from(
        &mut self,
        origin: ProductCommandTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entries = Self::command_dispatcher(cx)
            .read(cx)
            .definitions()
            .map(|definition| {
                CommandPaletteEntry::new(
                    definition,
                    crate::host::protocol::CommandArgumentValue::Null,
                )
            })
            .collect::<Vec<_>>();
        let palette = cx.new(|cx| CommandPalette::new(entries, origin, cx));
        let subscription = cx.subscribe(
            &palette,
            |this, _palette, event: &CommandPaletteEvent<ProductCommandTarget>, cx| {
                this.command_palette = None;
                this.command_palette_subscription = None;
                if let CommandPaletteEvent::Confirmed { command, origin } = event {
                    this.dispatch_command(command.clone(), origin.clone(), cx);
                }
                this.refocus_active_transient(cx);
                cx.notify();
            },
        );
        window.focus(&palette.focus_handle(cx));
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
        let matches = self.transient.as_ref().is_some_and(|active| {
            active.owner.extension == owner.extension
                && active.owner.lifecycle == owner.lifecycle
                && (owner.invocation.is_none() || active.owner.invocation == owner.invocation)
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<tokio::sync::oneshot::Receiver<TransientOutcome>, TransientError> {
        self.transient_slot_free()?;
        let restore_focus = self.transient_restore_focus(window, cx);
        let view = cx.new(|cx| PickerView::new(title, items, cx));
        let focus = view.read(cx).focus_handle(cx);
        let subscription =
            cx.subscribe(&view, |this: &mut Self, _view, event: &PickerEvent, cx| {
                this.finish_transient(
                    match event {
                        PickerEvent::Selected(id) => TransientOutcome::Picker(id.clone()),
                        PickerEvent::Dismissed => TransientOutcome::Dismissed,
                    },
                    cx,
                );
            });
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
            owner,
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
            owner,
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
            owner,
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
        owner: TransientOwner,
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

    fn protected_closure_is_active(cx: &App) -> bool {
        *cx.global::<ApplicationProtectedClosure>().0.borrow()
    }

    fn set_protected_closure_active(active: bool, cx: &App) {
        *cx.global::<ApplicationProtectedClosure>().0.borrow_mut() = active;
    }

    fn collect_product_views(cx: &mut App) -> Vec<ProtectedView> {
        let mut views = Vec::new();
        for window_handle in cx.windows() {
            let window_views = cx
                .update_window(window_handle, |_, window, cx| {
                    let shell = window.root::<ProductShell>().flatten()?;
                    let workbench = shell.read(cx).workbench.clone();
                    let snapshots = workbench.read(cx).tab_snapshots();
                    Some(
                        snapshots
                            .into_iter()
                            .map(|snapshot| {
                                let editor = {
                                    let workbench = workbench.read(cx);
                                    workbench
                                        .pane(snapshot.pane_id)
                                        .and_then(|pane| {
                                            pane.tabs()
                                                .iter()
                                                .find(|tab| tab.id() == snapshot.tab_id)
                                        })
                                        .and_then(|tab| tab.editor().cloned())
                                };
                                ProtectedView {
                                    key: ProtectedViewKey {
                                        window: window_handle,
                                        workbench: workbench.entity_id(),
                                        presentation: workbench
                                            .read(cx)
                                            .capture_tab_move(snapshot.pane_id, snapshot.tab_id)
                                            .unwrap()
                                            .presentation,
                                        pane: snapshot.pane_id,
                                        tab: snapshot.tab_id,
                                        surface: snapshot.surface,
                                    },
                                    target: editor.map(|editor| ProductCommandTarget {
                                        window: window_handle,
                                        shell: shell.downgrade(),
                                        workbench: workbench.downgrade(),
                                        pane: snapshot.pane_id,
                                        tab: snapshot.tab_id,
                                        surface: snapshot.surface,
                                        presentation: Some(editor.entity_id()),
                                        focus: editor.focus_handle(cx).downgrade(),
                                        view: None,
                                    }),
                                }
                            })
                            .collect::<Vec<_>>(),
                    )
                })
                .ok()
                .flatten()
                .unwrap_or_default();
            views.extend(window_views);
        }
        views
    }

    fn build_protected_close_plan(
        request: ProtectedCloseRequest,
        cx: &mut App,
    ) -> Result<ProtectedClosePlan, ProtectedCloseFailure> {
        request
            .origin
            .validate_tab(cx)
            .map_err(|_| ProtectedCloseFailure::InvalidTarget)?;
        let views = Self::collect_product_views(cx);
        let origin_key = ProtectedViewKey {
            window: request.origin.window,
            workbench: request.origin.workbench.entity_id(),
            presentation: request.origin.presentation,
            pane: request.origin.pane,
            tab: request.origin.tab,
            surface: request.origin.surface,
        };
        if !views.iter().any(|view| view.key == origin_key) {
            return Err(ProtectedCloseFailure::InvalidTarget);
        }
        let scoped = views
            .iter()
            .filter(|view| match request.kind {
                ProtectedCloseKind::Tab => view.key == origin_key,
                ProtectedCloseKind::Window => view.key.window == request.origin.window,
                ProtectedCloseKind::Quit => true,
            })
            .cloned()
            .collect::<Vec<_>>();
        let scope = scoped.iter().map(|view| view.key).collect::<Vec<_>>();
        let total_counts = views.iter().fold(HashMap::new(), |mut counts, view| {
            if let TabSurfaceId::Document(document) = view.key.surface {
                *counts.entry(document).or_insert(0usize) += 1;
            }
            counts
        });
        let scoped_counts = scoped.iter().fold(HashMap::new(), |mut counts, view| {
            if let TabSurfaceId::Document(document) = view.key.surface {
                *counts.entry(document).or_insert(0usize) += 1;
            }
            counts
        });
        let documents = Self::documents(cx);
        let mut prompted = HashSet::new();
        let mut prompts = Vec::new();
        for view in &scoped {
            let TabSurfaceId::Document(document) = view.key.surface else {
                continue;
            };
            let loses_final_view = request.kind == ProtectedCloseKind::Quit
                || scoped_counts.get(&document) == total_counts.get(&document);
            if loses_final_view
                && prompted.insert(document)
                && documents
                    .read(cx)
                    .get(document)
                    .is_some_and(|document| document.is_dirty(cx))
            {
                prompts.push((
                    document,
                    view.target.clone().expect("document tab has editor focus"),
                ));
            }
        }
        if request.kind == ProtectedCloseKind::Quit
            && documents
                .read(cx)
                .documents()
                .any(|document| document.is_dirty(cx) && !prompted.contains(&document.id()))
        {
            return Err(ProtectedCloseFailure::InvalidTarget);
        }
        Ok(ProtectedClosePlan {
            request,
            scope,
            prompts,
        })
    }

    fn next_open_generation(&mut self) -> u64 {
        self.open_generation = self
            .open_generation
            .checked_add(1)
            .expect("product open generation overflowed");
        self.open_generation
    }

    fn finish_open_command(
        &mut self,
        completion: CommandCompletion,
        outcome: crate::host::protocol::CommandOutcome,
        cx: &mut Context<Self>,
    ) {
        completion.complete(outcome.clone());
        Self::command_dispatcher(cx).update(cx, |dispatcher, cx| {
            dispatcher.record_outcome(outcome, cx);
        });
    }

    fn finish_save_command(
        &mut self,
        completion: CommandCompletion,
        outcome: crate::host::protocol::CommandOutcome,
        cx: &mut Context<Self>,
    ) {
        completion.complete(outcome.clone());
        Self::command_dispatcher(cx).update(cx, |dispatcher, cx| {
            dispatcher.record_outcome(outcome, cx);
        });
    }

    fn target_is_live(&self, target: &ProductCommandTarget, cx: &App) -> bool {
        cx.windows().contains(&target.window)
            && target.workbench.upgrade() == Some(self.workbench.clone())
            && target.validate_tab(cx).is_ok()
            && target.validate_view(cx).is_ok()
    }

    pub(crate) fn start_save_command(
        &mut self,
        save_as: bool,
        target: ProductCommandTarget,
        completion: CommandCompletion,
        cx: &mut Context<Self>,
    ) {
        if !self.target_is_live(&target, cx) {
            self.finish_save_command(
                completion,
                crate::host::protocol::CommandOutcome::InvalidTarget,
                cx,
            );
            return;
        }
        let documents = Self::documents(cx);
        let Some(document_id) = target_document(&target) else {
            self.finish_save_command(
                completion,
                crate::host::protocol::CommandOutcome::Unavailable,
                cx,
            );
            return;
        };
        let Some(document) = documents.read(cx).get(document_id) else {
            self.finish_save_command(
                completion,
                crate::host::protocol::CommandOutcome::InvalidTarget,
                cx,
            );
            return;
        };
        if matches!(document.state(), DocumentState::Generated) {
            let message = "generated documents cannot be saved".to_owned();
            self.status = message.clone().into();
            self.finish_save_command(
                completion,
                crate::host::protocol::CommandOutcome::HandlerFailure { message },
                cx,
            );
            cx.notify();
            return;
        }
        if save_as || matches!(document.state(), DocumentState::Untitled { .. }) {
            self.start_save_dialog(target, completion, cx);
        } else {
            let uri = document.resource_uri().unwrap().clone();
            self.start_save_to_uri(target, uri, false, completion, cx);
        }
    }

    fn start_save_dialog(
        &mut self,
        target: ProductCommandTarget,
        completion: CommandCompletion,
        cx: &mut Context<Self>,
    ) {
        let suggested_name = Self::documents(cx)
            .read(cx)
            .get(target_document(&target).expect("save target is a document"))
            .map(|document| document.title().clone())
            .unwrap_or_else(|| "Untitled".into());
        let selection = if let Some(dialog) = cx.try_global::<ApplicationSaveDialog>() {
            dialog.0.select(suggested_name)
        } else {
            let directory =
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"));
            let selection = cx.prompt_for_new_path(&directory, Some(&suggested_name));
            Box::pin(async move {
                match selection.await {
                    Ok(Ok(Some(path))) => match OpenRequest::from_path(&path, Path::new("/")) {
                        Ok(request) => SaveDialogOutcome::Selected(request.uri().clone()),
                        Err(message) => SaveDialogOutcome::Failed(message),
                    },
                    Ok(Ok(None)) => SaveDialogOutcome::Cancelled,
                    Ok(Err(error)) => SaveDialogOutcome::Failed(error.to_string()),
                    Err(error) => SaveDialogOutcome::Failed(error.to_string()),
                }
            }) as SaveDialogFuture
        };
        self.status = "choosing a save destination".into();
        cx.notify();
        let task = cx.spawn(async move |this, cx| {
            let outcome = selection.await;
            let fallback = completion.clone();
            if this
                .update(cx, |this, cx| match outcome {
                    SaveDialogOutcome::Selected(uri) => {
                        this.start_save_to_uri(target, uri, true, completion, cx);
                    }
                    SaveDialogOutcome::Cancelled => {
                        this.status = "save cancelled".into();
                        this.finish_save_command(
                            completion,
                            crate::host::protocol::CommandOutcome::Cancelled,
                            cx,
                        );
                        cx.notify();
                    }
                    SaveDialogOutcome::Failed(message) => {
                        this.status = format!("save failed: {message}").into();
                        this.finish_save_command(
                            completion,
                            crate::host::protocol::CommandOutcome::HandlerFailure { message },
                            cx,
                        );
                        cx.notify();
                    }
                })
                .is_err()
            {
                fallback.complete(crate::host::protocol::CommandOutcome::InvalidTarget);
            }
        });
        self.tasks.push(task);
    }

    fn start_save_to_uri(
        &mut self,
        target: ProductCommandTarget,
        uri: super::resource::ResourceUri,
        explicit_overwrite: bool,
        completion: CommandCompletion,
        cx: &mut Context<Self>,
    ) {
        if !self.target_is_live(&target, cx) {
            self.finish_save_command(
                completion,
                crate::host::protocol::CommandOutcome::InvalidTarget,
                cx,
            );
            return;
        }
        let documents = Self::documents(cx);
        let (model, title, state) = {
            let documents = documents.read(cx);
            let Some(document) =
                documents.get(target_document(&target).expect("save target is a document"))
            else {
                self.finish_save_command(
                    completion,
                    crate::host::protocol::CommandOutcome::InvalidTarget,
                    cx,
                );
                return;
            };
            (
                document.model().clone(),
                resource_title(&uri),
                document.state().clone(),
            )
        };
        if documents
            .read(cx)
            .document_for_resource(&uri)
            .is_some_and(|document| Some(document) != target_document(&target))
        {
            let message = format!("another open document already owns {uri}");
            self.status = format!("save failed: {message}").into();
            self.finish_save_command(
                completion,
                crate::host::protocol::CommandOutcome::HandlerFailure { message },
                cx,
            );
            cx.notify();
            return;
        }
        let (text, revision) = model.read_with(cx, |model, _| (model.text(), model.revision()));
        let Some(capture) = documents.update(cx, |documents, _| {
            documents.begin_persistence(
                target_document(&target).expect("save target is a document"),
                &model,
            )
        }) else {
            self.finish_save_command(
                completion,
                crate::host::protocol::CommandOutcome::InvalidTarget,
                cx,
            );
            return;
        };
        self.status = format!("saving {uri}").into();
        cx.notify();
        let providers = Self::filesystems(cx);
        let task = cx.spawn(async move |this, cx| {
            let result: Result<(super::resource::ResourceUri, ResourceVersion), ResourceError> =
                async {
                    let uri = providers.normalize(uri).await?;
                    let provider = providers.provider(&uri)?;
                    let version = if explicit_overwrite {
                        match provider.stat(uri.clone()).await? {
                            ResourceStat::Missing => {
                                provider.create(uri.clone(), text.into_bytes()).await?
                            }
                            ResourceStat::File => {
                                let existing = provider.read(uri.clone()).await?;
                                provider
                                    .replace(uri.clone(), existing.version, text.into_bytes())
                                    .await?
                            }
                            ResourceStat::Directory => {
                                return Err(ResourceError::WrongKind {
                                    uri,
                                    expected: ResourceKind::File,
                                    actual: ResourceKind::Directory,
                                });
                            }
                        }
                    } else {
                        match state {
                            DocumentState::Destination { .. } => {
                                provider.create(uri.clone(), text.into_bytes()).await?
                            }
                            DocumentState::Persisted { version, .. } => {
                                provider
                                    .replace(uri.clone(), version, text.into_bytes())
                                    .await?
                            }
                            DocumentState::Untitled { .. } | DocumentState::Generated => {
                                return Err(ResourceError::InvalidUri {
                                    uri: uri.to_string(),
                                    reason: "document has no direct save destination".into(),
                                });
                            }
                        }
                    };
                    Ok((uri, version))
                }
                .await;
            let fallback = completion.clone();
            if this
                .update(cx, |this, cx| match result {
                    Ok((uri, version)) if this.target_is_live(&target, cx) => {
                        let committed = Self::documents(cx).update(cx, |documents, cx| {
                            documents.finish_persistence(
                                target_document(&target).expect("save target is a document"),
                                &model,
                                &capture,
                                (title, uri.clone()),
                                (version, revision),
                                cx,
                            )
                        });
                        let outcome = if committed {
                            this.status = format!("saved {uri}").into();
                            crate::host::protocol::CommandOutcome::Completed
                        } else {
                            this.status = "save completion became stale".into();
                            crate::host::protocol::CommandOutcome::InvalidTarget
                        };
                        this.finish_save_command(completion, outcome, cx);
                        cx.notify();
                    }
                    Ok(_) => {
                        this.finish_save_command(
                            completion,
                            crate::host::protocol::CommandOutcome::InvalidTarget,
                            cx,
                        );
                    }
                    Err(ResourceError::Conflict { uri }) => {
                        this.prompt_for_save_conflict(target, uri, completion, cx);
                    }
                    Err(ResourceError::AlreadyExists { uri }) => {
                        this.prompt_for_save_conflict(target, uri, completion, cx);
                    }
                    Err(error) => {
                        let message = error.to_string();
                        this.status = format!("save failed: {message}").into();
                        this.finish_save_command(
                            completion,
                            crate::host::protocol::CommandOutcome::HandlerFailure { message },
                            cx,
                        );
                        cx.notify();
                    }
                })
                .is_err()
            {
                fallback.complete(crate::host::protocol::CommandOutcome::InvalidTarget);
            }
        });
        self.tasks.push(task);
    }

    fn prompt_for_save_conflict(
        &mut self,
        target: ProductCommandTarget,
        uri: super::resource::ResourceUri,
        completion: CommandCompletion,
        cx: &mut Context<Self>,
    ) {
        if !self.target_is_live(&target, cx) {
            self.finish_save_command(
                completion,
                crate::host::protocol::CommandOutcome::InvalidTarget,
                cx,
            );
            return;
        }
        let prompt = cx.update_window(target.window, |_, window, cx| {
            window.prompt(
                PromptLevel::Warning,
                "The file changed outside Knot",
                Some(
                    "Reload the external version, choose another destination, or cancel the save.",
                ),
                &[
                    PromptButton::new("Reload"),
                    PromptButton::new("Save As…"),
                    PromptButton::cancel("Cancel"),
                ],
                cx,
            )
        });
        let Ok(prompt) = prompt else {
            self.finish_save_command(
                completion,
                crate::host::protocol::CommandOutcome::InvalidTarget,
                cx,
            );
            return;
        };
        self.status = format!("save conflict at {uri}").into();
        cx.notify();
        let task = cx.spawn(async move |this, cx| {
            let answer = prompt.await.unwrap_or(2);
            let fallback = completion.clone();
            if this
                .update(cx, |this, cx| match answer {
                    0 => this.start_conflict_reload(target, uri, completion, cx),
                    1 => this.start_save_dialog(target, completion, cx),
                    _ => {
                        this.status = "save conflict cancelled".into();
                        this.finish_save_command(
                            completion,
                            crate::host::protocol::CommandOutcome::Cancelled,
                            cx,
                        );
                        cx.notify();
                    }
                })
                .is_err()
            {
                fallback.complete(crate::host::protocol::CommandOutcome::InvalidTarget);
            }
        });
        self.tasks.push(task);
    }

    fn start_conflict_reload(
        &mut self,
        target: ProductCommandTarget,
        uri: super::resource::ResourceUri,
        completion: CommandCompletion,
        cx: &mut Context<Self>,
    ) {
        if !self.target_is_live(&target, cx) {
            self.finish_save_command(
                completion,
                crate::host::protocol::CommandOutcome::InvalidTarget,
                cx,
            );
            return;
        }
        let documents = Self::documents(cx);
        let Some((model, title)) = documents
            .read(cx)
            .get(target_document(&target).expect("save target is a document"))
            .map(|document| (document.model().clone(), document.title().clone()))
        else {
            self.finish_save_command(
                completion,
                crate::host::protocol::CommandOutcome::InvalidTarget,
                cx,
            );
            return;
        };
        let revision = model.read(cx).revision();
        let Some(capture) = documents.update(cx, |documents, _| {
            documents.begin_persistence(
                target_document(&target).expect("save target is a document"),
                &model,
            )
        }) else {
            self.finish_save_command(
                completion,
                crate::host::protocol::CommandOutcome::InvalidTarget,
                cx,
            );
            return;
        };
        self.status = format!("reloading {uri}").into();
        cx.notify();
        let providers = Self::filesystems(cx);
        let task = cx.spawn(async move |this, cx| {
            let result = load_resource(providers, uri.clone()).await;
            let fallback = completion.clone();
            if this
                .update(cx, |this, cx| {
                    let outcome = match result {
                        Ok(OpenResource::File { uri, text, version })
                            if this.target_is_live(&target, cx)
                                && model.read(cx).revision() == revision =>
                        {
                            let replaced = model.update(cx, |model, _| {
                                let len = model.text().len();
                                model.replace(0..len, &text)
                            });
                            match replaced {
                                Ok(_) => {
                                    let loaded_revision = model.read(cx).revision();
                                    if Self::documents(cx).update(cx, |documents, cx| {
                                        documents.finish_persistence(
                                            target_document(&target)
                                                .expect("save target is a document"),
                                            &model,
                                            &capture,
                                            (title, uri.clone()),
                                            (version, loaded_revision),
                                            cx,
                                        )
                                    }) {
                                        this.status = format!("reloaded {uri}").into();
                                        crate::host::protocol::CommandOutcome::Completed
                                    } else {
                                        crate::host::protocol::CommandOutcome::InvalidTarget
                                    }
                                }
                                Err(error) => {
                                    crate::host::protocol::CommandOutcome::HandlerFailure {
                                        message: format!("{error:?}"),
                                    }
                                }
                            }
                        }
                        Ok(_) => crate::host::protocol::CommandOutcome::InvalidTarget,
                        Err(error) => crate::host::protocol::CommandOutcome::HandlerFailure {
                            message: error.to_string(),
                        },
                    };
                    this.finish_save_command(completion, outcome, cx);
                    cx.notify();
                })
                .is_err()
            {
                fallback.complete(crate::host::protocol::CommandOutcome::InvalidTarget);
            }
        });
        self.tasks.push(task);
    }

    pub(crate) fn start_open_dialog(
        &mut self,
        target: ProductCommandTarget,
        completion: CommandCompletion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.target_is_live(&target, cx) {
            self.finish_open_command(
                completion,
                crate::host::protocol::CommandOutcome::InvalidTarget,
                cx,
            );
            return;
        }
        if cx.try_global::<ApplicationOpenDialog>().is_none()
            && !cx.can_select_mixed_files_and_dirs()
        {
            let prompt = window.prompt(
                PromptLevel::Info,
                "What would you like to open?",
                None,
                &[
                    PromptButton::new("File"),
                    PromptButton::new("Folder"),
                    PromptButton::cancel("Cancel"),
                ],
                cx,
            );
            self.status = "choosing what to open".into();
            cx.notify();
            let task = cx.spawn(async move |this, cx| {
                let answer = prompt.await.unwrap_or(2);
                let fallback = completion.clone();
                if this
                    .update(cx, |this, cx| match answer {
                        0 | 1 => this.start_open_dialog_with_options(
                            target,
                            completion,
                            PathPromptOptions {
                                files: answer == 0,
                                directories: answer == 1,
                                multiple: false,
                                prompt: Some(
                                    if answer == 0 {
                                        "Open File"
                                    } else {
                                        "Open Folder"
                                    }
                                    .into(),
                                ),
                            },
                            cx,
                        ),
                        _ => {
                            this.status = "open cancelled".into();
                            this.finish_open_command(
                                completion,
                                crate::host::protocol::CommandOutcome::Cancelled,
                                cx,
                            );
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    fallback.complete(crate::host::protocol::CommandOutcome::InvalidTarget);
                }
            });
            self.tasks.push(task);
            return;
        }
        self.start_open_dialog_with_options(
            target,
            completion,
            PathPromptOptions {
                files: true,
                directories: true,
                multiple: false,
                prompt: Some("Open".into()),
            },
            cx,
        );
    }

    fn start_open_dialog_with_options(
        &mut self,
        target: ProductCommandTarget,
        completion: CommandCompletion,
        options: PathPromptOptions,
        cx: &mut Context<Self>,
    ) {
        if !self.target_is_live(&target, cx) {
            self.finish_open_command(
                completion,
                crate::host::protocol::CommandOutcome::InvalidTarget,
                cx,
            );
            return;
        }
        let open_target = OpenTarget::from_command(&target);
        let selection: OpenDialogFuture =
            if let Some(dialog) = cx.try_global::<ApplicationOpenDialog>() {
                dialog.0.select()
            } else {
                let selection = cx.prompt_for_paths(options);
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
        self.status = "choosing a file or folder".into();
        cx.notify();
        let task = cx.spawn(async move |this, cx| {
            let path = match selection.await {
                OpenDialogOutcome::Selected(path) => path,
                OpenDialogOutcome::Cancelled => {
                    let delivered = completion.clone();
                    if this
                        .update(cx, |this, cx| {
                            this.status = "open cancelled".into();
                            this.finish_open_command(
                                delivered,
                                crate::host::protocol::CommandOutcome::Cancelled,
                                cx,
                            );
                            cx.notify();
                        })
                        .is_err()
                    {
                        completion.complete(crate::host::protocol::CommandOutcome::InvalidTarget);
                    }
                    return;
                }
                OpenDialogOutcome::Failed(message) => {
                    let delivered = completion.clone();
                    if this
                        .update(cx, |this, cx| {
                            this.status = format!("open failed: {message}").into();
                            this.finish_open_command(
                                delivered,
                                crate::host::protocol::CommandOutcome::HandlerFailure { message },
                                cx,
                            );
                            cx.notify();
                        })
                        .is_err()
                    {
                        completion.complete(crate::host::protocol::CommandOutcome::InvalidTarget);
                    }
                    return;
                }
            };
            let request = match OpenRequest::from_path(&path, Path::new("/")) {
                Ok(request) => request,
                Err(message) => {
                    let delivered = completion.clone();
                    if this
                        .update(cx, |this, cx| {
                            this.status = format!("open failed: {message}").into();
                            this.finish_open_command(
                                delivered,
                                crate::host::protocol::CommandOutcome::HandlerFailure { message },
                                cx,
                            );
                            cx.notify();
                        })
                        .is_err()
                    {
                        completion.complete(crate::host::protocol::CommandOutcome::InvalidTarget);
                    }
                    return;
                }
            };
            let delivered = completion.clone();
            if this
                .update(cx, |this, cx| {
                    this.start_open_request(request, open_target, false, Some(delivered), cx);
                })
                .is_err()
            {
                completion.complete(crate::host::protocol::CommandOutcome::InvalidTarget);
            }
        });
        self.tasks.push(task);
    }

    fn start_open_request(
        &mut self,
        request: OpenRequest,
        target: OpenTarget,
        replace_target: bool,
        completion: Option<CommandCompletion>,
        cx: &mut Context<Self>,
    ) {
        let result = self.start_open_operation(request, target, replace_target, cx);
        let task = cx.spawn(async move |this, cx| {
            let outcome = match result.await {
                Ok(Ok(_)) => crate::host::protocol::CommandOutcome::Completed,
                Ok(Err(error)) => error.command_outcome(),
                Err(_) => crate::host::protocol::CommandOutcome::InvalidTarget,
            };
            if let Some(completion) = completion {
                let fallback = completion.clone();
                if this
                    .update(cx, |this, cx| {
                        this.finish_open_command(completion, outcome, cx)
                    })
                    .is_err()
                {
                    fallback.complete(crate::host::protocol::CommandOutcome::InvalidTarget);
                }
            }
        });
        self.tasks.push(task);
    }

    fn start_open_operation(
        &mut self,
        request: OpenRequest,
        target: OpenTarget,
        replace_target: bool,
        cx: &mut Context<Self>,
    ) -> tokio::sync::oneshot::Receiver<Result<OpenCommit, OpenFailure>> {
        let (completion, result) = tokio::sync::oneshot::channel();
        self.start_resource_operation(
            request,
            target,
            operations::OpenOptions {
                replace_target,
                ..Default::default()
            },
            move |outcome, _| {
                let _ = completion.send(outcome);
            },
            cx,
        );
        result
    }

    fn start_resource_operation(
        &mut self,
        request: OpenRequest,
        target: OpenTarget,
        options: operations::OpenOptions,
        completion: impl FnOnce(Result<OpenCommit, OpenFailure>, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) {
        if !target.matches_presentation(&self.workbench, cx) {
            completion(Err(OpenFailure::InvalidTarget), cx);
            return;
        }
        let generation = self.next_open_generation();
        let uri = request.uri().clone();
        self.status = format!("opening {uri}").into();
        let providers = Self::filesystems(cx);
        let task = cx.spawn(async move |this, cx| {
            let loaded = super::open::load_resource_kind(providers, uri, options.kind).await;
            let mut completion = Some(completion);
            if this
                .update(cx, |this, cx| {
                    let outcome = if options
                        .caller
                        .as_ref()
                        .is_some_and(|caller| !caller.is_live(cx))
                    {
                        Err(OpenFailure::Cancelled)
                    } else if this.open_generation != generation
                        || !workbench_is_live(target.window, &this.workbench, cx)
                        || !target.matches_presentation(&this.workbench, cx)
                    {
                        Err(OpenFailure::InvalidTarget)
                    } else {
                        this.apply_open_result(loaded, &target, options.replace_target, cx)
                            .map_err(OpenFailure::Resource)
                    };
                    completion.take().unwrap()(outcome, cx);
                })
                .is_err()
            {
                let _ =
                    cx.update(|cx| completion.take().unwrap()(Err(OpenFailure::InvalidTarget), cx));
            }
        });
        self.tasks.push(task);
        cx.notify();
    }

    fn apply_open_result(
        &mut self,
        result: Result<OpenResource, super::open::OpenResourceError>,
        target: &OpenTarget,
        replace_target: bool,
        cx: &mut Context<Self>,
    ) -> Result<OpenCommit, super::open::OpenResourceError> {
        let committed = match result {
            Ok(OpenResource::Directory(root)) => {
                OpenCommit::Workspace(self.install_workspace(root, cx))
            }
            Ok(OpenResource::Missing(uri)) => {
                let documents = Self::documents(cx);
                let model = cx.new(|_| BufferModel::from_text(""));
                let title = resource_title(&uri);
                let document = documents.update(cx, |documents, cx| {
                    documents.create_destination(title, model, uri.clone(), cx)
                });
                let target = self.present_open_document(document, target, replace_target, cx);
                OpenCommit::Document { document, target }
            }
            Ok(OpenResource::File { uri, text, version }) => {
                let documents = Self::documents(cx);
                let document =
                    if let Some(document) = documents.read(cx).document_for_resource(&uri) {
                        document
                    } else {
                        let model = cx.new(|_| BufferModel::from_text(text));
                        documents.update(cx, |documents, cx| {
                            documents.create_persisted(
                                resource_title(&uri),
                                model,
                                uri.clone(),
                                0,
                                version,
                                cx,
                            )
                        })
                    };
                let target = self.present_open_document(document, target, replace_target, cx);
                OpenCommit::Document { document, target }
            }
            Err(error) => {
                self.status = format!("open failed: {error}").into();
                cx.notify();
                return Err(error);
            }
        };
        match &committed {
            OpenCommit::Document { document, target } => {
                let documents = Self::documents(cx);
                let document = documents.read(cx).get(*document).unwrap();
                self.status = format!(
                    "{} {}",
                    if matches!(document.state(), DocumentState::Destination { .. }) {
                        "new file"
                    } else {
                        "opened"
                    },
                    document.resource_uri().unwrap()
                )
                .into();
                let _ = cx.update_window(target.window, |_, window, cx| {
                    self.focus_active_surface(window, cx)
                });
            }
            OpenCommit::Workspace(workspace) => {
                self.status = format!("workspace opened {}", workspace.root().unwrap()).into();
            }
        }
        cx.notify();
        Ok(committed)
    }

    fn present_open_document(
        &mut self,
        document: DocumentId,
        target: &OpenTarget,
        replace_target: bool,
        cx: &mut Context<Self>,
    ) -> OpenTarget {
        let existing = self.workbench.read(cx).panes().iter().find_map(|pane| {
            pane.tabs()
                .iter()
                .find(|tab| tab.document_id() == Some(document))
                .map(|tab| (pane.id(), tab.id()))
        });
        let (pane, tab) = if let Some((pane, tab)) = existing {
            self.workbench.update(cx, |workbench, _| {
                workbench.activate_tab(pane, tab);
            });
            (pane, tab)
        } else {
            let documents = Self::documents(cx);
            let model = documents.read(cx).get(document).unwrap().model().clone();
            if replace_target && let TabSurfaceId::Document(previous) = target.surface {
                let replaced = self.workbench.update(cx, |workbench, cx| {
                    workbench.replace_tab_document(target.pane, target.tab, document, model, cx)
                });
                if replaced
                    && previous != document
                    && cx
                        .global::<ApplicationWorkbenches>()
                        .view_count(previous, cx)
                        == 0
                {
                    documents.update(cx, |documents, cx| {
                        documents.remove(previous, cx);
                    });
                }
                (target.pane, target.tab)
            } else {
                self.workbench.update(cx, |workbench, cx| {
                    if workbench.dock_location(target.pane).is_some() {
                        workbench.open_document_in_normal_group(document, model, cx)
                    } else {
                        let tab = workbench
                            .open_tab_for_document(target.pane, document, model, cx)
                            .expect("validated destination pane");
                        (target.pane, tab)
                    }
                })
            }
        };
        let capture = self.workbench.read(cx).capture_tab_move(pane, tab).unwrap();
        OpenTarget {
            window: target.window,
            workbench: self.workbench.downgrade(),
            pane,
            tab,
            surface: capture.tab.surface,
            presentation: capture.presentation,
        }
    }

    fn explorer_frame(&self, cx: &App) -> Option<ToolFrameId> {
        let presentation = self.workspace_tree.as_ref()?.entity_id();
        self.workbench
            .read(cx)
            .tools()
            .iter()
            .find(|tool| tool.content.presentation() == presentation)
            .map(|tool| tool.id)
    }

    fn toggle_explorer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace_tree.is_none() {
            self.ensure_explorer(true, cx);
        } else {
            let id = self.explorer_frame(cx).unwrap();
            self.workbench.update(cx, |workbench, _| {
                if workbench.tool_tab(id).is_some() {
                    workbench.hide_tool(id);
                } else {
                    workbench.show_tool(id);
                }
            });
        }
        self.publish_tool_layout(cx);
        self.focus_active_surface(window, cx);
        cx.notify();
    }

    fn install_workspace(
        &mut self,
        root: super::resource::ResourceUri,
        cx: &mut Context<Self>,
    ) -> super::workspace::WorkspaceSnapshot {
        self.replace_workspace_roots(vec![root], cx)
            .expect("one workspace root is supported")
    }

    fn replace_workspace_roots(
        &mut self,
        roots: Vec<super::resource::ResourceUri>,
        cx: &mut Context<Self>,
    ) -> Result<super::workspace::WorkspaceSnapshot, super::workspace::TooManyWorkspaceRoots> {
        let workspace = self.workspace.replace_roots(roots)?;
        if let Some(tree) = &self.workspace_tree {
            tree.update(cx, |tree, cx| tree.set_workspace(workspace.clone(), cx));
        } else if !workspace.roots().is_empty() {
            self.ensure_explorer(false, cx);
        }
        self.status = if workspace.roots().is_empty() {
            "workspace cleared"
        } else {
            "workspace opened"
        }
        .into();
        cx.notify();
        Ok(workspace)
    }

    /// Register the native explorer frame and apply the stored tool arrangement.
    /// `show` forces visibility for explicit user requests; automatic presentation
    /// (workspace open, fresh-window restore) honors stored visibility instead and
    /// never publishes a new default.
    fn ensure_explorer(&mut self, show: bool, cx: &mut Context<Self>) {
        if self.workspace_tree.is_some() {
            return;
        }
        let tree = cx.new(|cx| WorkspaceTree::new(self.workspace.snapshot(), cx));
        self.workspace_tree_subscription = Some(cx.subscribe(
            &tree,
            |this, _tree, event: &WorkspaceTreeEvent, cx| {
                this.handle_workspace_tree_event(event.clone(), cx);
            },
        ));
        let load = tree.clone();
        cx.defer(move |cx| load.update(cx, |tree, cx| tree.load(cx)));
        let stored = Self::stored_tool_layout(cx);
        self.workbench.update(cx, |workbench, _| {
            let previous = workbench.focused_pane_id();
            let id = workbench.register_tool(
                super::tool_layout::EXPLORER_TOOL_ID,
                "Explorer",
                ToolContent::Workspace(tree.clone()),
                DockLocation::Left,
            );
            let visible = show
                || stored
                    .as_ref()
                    .and_then(|snapshot| snapshot.tool(super::tool_layout::EXPLORER_TOOL_ID))
                    .is_none_or(|record| record.visible);
            if let Some(snapshot) = &stored {
                workbench.set_dock_sizes(snapshot.dock_sizes);
                if let Some(record) = snapshot.tool(super::tool_layout::EXPLORER_TOOL_ID) {
                    workbench.remember_tool_placement(
                        id,
                        record.location.placement(),
                        record.index,
                    );
                }
            }
            if visible {
                workbench.show_tool(id);
            }
            if let Some(pane) = previous {
                workbench.focus_pane(pane);
            }
        });
        self.workspace_tree = Some(tree);
    }

    /// The last user-customized tool arrangement, or product defaults when no
    /// store is installed. Fresh windows restore from this; live windows never
    /// read it again after creation.
    fn stored_tool_layout(cx: &App) -> Option<super::tool_layout::ToolLayoutSnapshot> {
        cx.try_global::<super::tool_layout::ApplicationToolLayout>()
            .map(|store| store.0.current())
    }

    /// Publish this window's live arrangement as the fresh-window default.
    /// Call only for explicit user moves, resizes, and show/hide changes:
    /// restore, activation, and automatic presentation must not publish.
    fn publish_tool_layout(&self, cx: &App) {
        let Some(store) = cx.try_global::<super::tool_layout::ApplicationToolLayout>() else {
            return;
        };
        store
            .0
            .publish(self.workbench.read(cx).tool_layout_snapshot());
    }

    /// Reset this window's tools to product defaults and publish the reset.
    fn reset_tool_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace_tree.is_none() {
            self.ensure_explorer(false, cx);
        }
        let defaults = super::tool_layout::ToolLayoutSnapshot::product_default();
        self.restore_tool_layout(&defaults, cx);
        let definitions = super::extension_host::view_definitions(cx);
        self.workbench.update(cx, |workbench, _| {
            for definition in definitions {
                let Some(id) = workbench.tool_by_stable_id(&definition.id) else {
                    continue;
                };
                workbench.hide_tool(id);
                workbench.remember_tool_placement(
                    id,
                    definition.location.placement(),
                    definition.order,
                );
                if definition.visible {
                    workbench.show_tool(id);
                }
            }
        });
        self.publish_tool_layout(cx);
        self.focus_active_surface(window, cx);
        cx.notify();
    }

    /// Inherit the last customized arrangement on a fresh window. Only runs
    /// at window creation: already-open windows never re-read the default.
    /// Uncustomized stores keep first-run behavior (no eager explorer before
    /// any folder or toggle); hidden tools stay hidden. Never publishes.
    fn restore_fresh_window_layout(&mut self, cx: &mut Context<Self>) {
        let Some(snapshot) = Self::stored_tool_layout(cx) else {
            return;
        };
        self.workbench.update(cx, |workbench, _| {
            workbench.set_dock_sizes(snapshot.dock_sizes);
        });
        if !snapshot.customized {
            return;
        }
        let visible = snapshot
            .tool(super::tool_layout::EXPLORER_TOOL_ID)
            .is_some_and(|record| record.visible);
        if visible {
            self.ensure_explorer(false, cx);
        }
    }

    /// Restore the full stored arrangement onto this window's registered tools.
    /// Unknown stored tools are skipped without blank frames; registered tools
    /// without a stored record keep their live placement. Never publishes.
    fn restore_tool_layout(
        &mut self,
        snapshot: &super::tool_layout::ToolLayoutSnapshot,
        cx: &mut Context<Self>,
    ) {
        use super::tool_layout::StoredLocation;
        self.workbench.update(cx, |workbench, _| {
            workbench.set_dock_sizes(snapshot.dock_sizes);
            let known: Vec<(ToolFrameId, String)> = workbench
                .tools()
                .iter()
                .map(|tool| (tool.id, tool.stable_id.to_owned()))
                .collect();
            let live_location = |workbench: &Workbench, id: ToolFrameId| {
                workbench
                    .tool_tab(id)
                    .map(|(pane, _)| match workbench.dock_location(pane) {
                        Some(dock) => StoredLocation::from(dock),
                        None => StoredLocation::DocumentGroup,
                    })
            };
            // Visible dock tools restore in stored order so tab order matches.
            // Docks already in stored order are left untouched so live command
            // targets survive.
            for dock in DockLocation::ALL {
                let mut ordered: Vec<(usize, ToolFrameId)> = known
                    .iter()
                    .filter_map(|(id, stable)| {
                        snapshot
                            .tool(stable)
                            .filter(|record| {
                                record.visible && record.location == StoredLocation::from(dock)
                            })
                            .map(|record| (record.index, *id))
                    })
                    .collect();
                ordered.sort_by_key(|(order, _)| *order);
                let want: Vec<ToolFrameId> = ordered.iter().map(|(_, id)| *id).collect();
                let live: Vec<ToolFrameId> = workbench
                    .dock_pane(dock)
                    .and_then(|pane| workbench.pane(pane))
                    .map(|pane| {
                        pane.tabs()
                            .iter()
                            .filter_map(|tab| match tab.surface_id() {
                                TabSurfaceId::Tool(id) => Some(id),
                                _ => None,
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if live == want {
                    for (index, id) in &ordered {
                        workbench.remember_tool_placement(
                            *id,
                            StoredLocation::from(dock).placement(),
                            *index,
                        );
                    }
                    continue;
                }
                for (index, id) in &ordered {
                    workbench.hide_tool(*id);
                    workbench.remember_tool_placement(
                        *id,
                        StoredLocation::from(dock).placement(),
                        *index,
                    );
                    workbench.show_tool(*id);
                }
            }
            // Visible document-group tools and hidden tools restore next.
            for (id, stable) in &known {
                let Some(record) = snapshot.tool(stable) else {
                    continue;
                };
                if record.visible && record.location != StoredLocation::DocumentGroup {
                    continue;
                }
                if record.visible && live_location(workbench, *id) == Some(record.location) {
                    workbench.remember_tool_placement(
                        *id,
                        record.location.placement(),
                        record.index,
                    );
                    continue;
                }
                workbench.hide_tool(*id);
                workbench.remember_tool_placement(*id, record.location.placement(), record.index);
                if record.visible {
                    workbench.show_tool(*id);
                }
            }
            for dock in DockLocation::ALL {
                if let Some(selected) = &snapshot.dock_selected[dock.index()]
                    && let Some(id) = workbench.tool_by_stable_id(selected)
                    && workbench.tool_tab(id).is_some()
                {
                    workbench.show_tool(id);
                }
            }
        });
    }

    fn handle_workspace_tree_event(&mut self, event: WorkspaceTreeEvent, cx: &mut Context<Self>) {
        match event {
            WorkspaceTreeEvent::RequestChildren(request) => {
                self.enumerate_workspace(request, cx);
            }
            WorkspaceTreeEvent::OpenFile { workspace, uri } => {
                if !self.workspace.is_current(&workspace) {
                    return;
                }
                let workbench = self.workbench.read(cx);
                let Some(id) = self.explorer_frame(cx) else {
                    return;
                };
                let Some((pane, tab)) = workbench.tool_tab(id) else {
                    return;
                };
                let capture = workbench.capture_tab_move(pane, tab).unwrap();
                let target = OpenTarget {
                    window: cx
                        .global::<ApplicationWorkbenches>()
                        .window_for(&self.workbench)
                        .unwrap(),
                    workbench: self.workbench.downgrade(),
                    pane,
                    tab,
                    surface: capture.tab.surface,
                    presentation: capture.presentation,
                };
                self.start_open_request(
                    OpenRequest::from_uri_for_product(uri),
                    target,
                    false,
                    None,
                    cx,
                );
            }
        }
    }

    fn enumerate_workspace(&mut self, request: WorkspaceTreeRequest, cx: &mut Context<Self>) {
        if !self.workspace.is_current(&request.workspace) {
            return;
        }
        let providers = Self::filesystems(cx);
        let task = cx.spawn(async move |this, cx| {
            let result = enumerate_directory(providers, request.parent.clone()).await;
            let _ = this.update(cx, |this, cx| {
                if !this.workspace.is_current(&request.workspace) {
                    return;
                }
                if let Some(tree) = &this.workspace_tree {
                    tree.update(cx, |tree, cx| {
                        tree.apply_response(
                            WorkspaceTreeResponse {
                                workspace: request.workspace,
                                parent: request.parent,
                                generation: request.generation,
                                result: result.map_err(|error| error.to_string().into()),
                            },
                            cx,
                        );
                    });
                }
            });
        });
        self.tasks.push(task);
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
        self.status = "new untitled document".into();
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

    fn new_terminal_in_pane(
        &mut self,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.workbench.read(cx).pane(pane).is_none() {
            return false;
        }
        let (id, view) = self.create_terminal(cx);
        if self
            .workbench
            .update(cx, |workbench, _| {
                if workbench.dock_location(pane).is_some() {
                    workbench.open_terminal_in_normal_group(id, view)
                } else {
                    workbench.open_terminal_tab(pane, id, view)
                }
            })
            .is_none()
        {
            self.close_terminal(id, cx);
            return false;
        }
        self.focus_active_surface(window, cx);
        self.status = "new terminal".into();
        cx.notify();
        true
    }

    #[cfg(test)]
    fn split(&mut self, direction: SplitDirection, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_focused_pane(window, cx);
        let Some(pane) = self.workbench.read(cx).focused_pane_id() else {
            return;
        };
        self.split_pane(pane, direction, window, cx);
    }

    fn split_pane(
        &mut self,
        pane: PaneId,
        direction: SplitDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let terminal = matches!(
            self.workbench
                .read(cx)
                .pane(pane)
                .map(|pane| pane.active_tab().surface_id()),
            Some(TabSurfaceId::Terminal(_))
        );
        let new_session = terminal.then(|| self.create_terminal(cx));
        let split = self
            .workbench
            .update(cx, |workbench, cx| match new_session.clone() {
                Some((id, view)) => workbench.split_pane_with_terminal(
                    pane,
                    direction,
                    SplitPlacement::After,
                    id,
                    view,
                    cx,
                ),
                None => workbench.split_pane(pane, direction, SplitPlacement::After, cx),
            });
        if split.is_some() {
            self.focus_active_surface(window, cx);
            self.status = match direction {
                SplitDirection::Horizontal => "split horizontally",
                SplitDirection::Vertical => "split vertically",
            }
            .into();
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
    fn close_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_focused_pane(window, cx);
        let Some((pane, tab, document)) = self.workbench.read(cx).focused_pane().and_then(|pane| {
            Some((
                pane.id(),
                pane.active_tab_id(),
                pane.active_tab().document_id()?,
            ))
        }) else {
            return;
        };
        self.close_tab(pane, tab, document, window, cx);
    }

    pub(crate) fn begin_protected_close(
        kind: ProtectedCloseKind,
        origin: ProductCommandTarget,
        completion: Option<CommandCompletion>,
        cx: &mut App,
    ) {
        Self::begin_protected_operation(
            kind,
            origin,
            None,
            Box::new(move |result, cx| {
                use crate::host::protocol::{CommandOutcome, PresentationCloseOutcome};
                let outcome = match result {
                    Ok(PresentationCloseOutcome::Closed) => CommandOutcome::Completed,
                    Ok(PresentationCloseOutcome::Cancelled) => CommandOutcome::Cancelled,
                    Err(error) => error.command_outcome(),
                };
                Self::finish_protected_close_completion(completion, outcome, cx);
            }),
            cx,
        );
    }

    fn begin_protected_operation(
        kind: ProtectedCloseKind,
        origin: ProductCommandTarget,
        caller: Option<operations::OperationCaller>,
        completion: ProtectedCloseCompletion,
        cx: &mut App,
    ) {
        if Self::protected_closure_is_active(cx) {
            completion(Err(ProtectedCloseFailure::InProgress), cx);
            return;
        }
        let plan = match Self::build_protected_close_plan(
            ProtectedCloseRequest {
                kind,
                origin,
                caller,
            },
            cx,
        ) {
            Ok(plan) => plan,
            Err(error) => {
                completion(Err(error), cx);
                return;
            }
        };
        let Some(shell) = plan.request.origin.shell.upgrade() else {
            completion(Err(ProtectedCloseFailure::InvalidTarget), cx);
            return;
        };
        Self::set_protected_closure_active(true, cx);
        shell.update(cx, |shell, cx| {
            shell.start_protected_close_plan(plan, completion, cx)
        });
    }

    fn start_protected_close_plan(
        &mut self,
        plan: ProtectedClosePlan,
        completion: ProtectedCloseCompletion,
        cx: &mut Context<Self>,
    ) {
        use crate::host::protocol::{CommandOutcome, PresentationCloseOutcome};

        cx.spawn(async move |this, cx| {
            let mut approvals = Vec::new();
            let mut outcome = Ok(PresentationCloseOutcome::Closed);
            for (document, target) in &plan.prompts {
                let prompt = cx.update(|cx| {
                    if plan
                        .request
                        .caller
                        .as_ref()
                        .is_some_and(|caller| !caller.is_live(cx))
                    {
                        return Err(ProtectedCloseFailure::Cancelled);
                    }
                    let documents = Self::documents(cx);
                    let Some(document_state) = documents.read(cx).get(*document) else {
                        return Err(ProtectedCloseFailure::InvalidTarget);
                    };
                    if !document_state.is_dirty(cx) {
                        return Ok(None);
                    }
                    let title = document_state.title().clone();
                    let prompted_revision = document_state.model().read(cx).revision();
                    cx.update_window(target.window, |_, window, cx| {
                        Ok(Some((
                            window.prompt(
                                PromptLevel::Warning,
                                &format!("Do you want to save the changes to “{title}”?"),
                                Some("Your changes will be lost if you don't save them."),
                                &[
                                    PromptButton::new("Save"),
                                    PromptButton::new("Don't Save"),
                                    PromptButton::cancel("Cancel"),
                                ],
                                cx,
                            ),
                            prompted_revision,
                        )))
                    })
                    .unwrap_or(Err(ProtectedCloseFailure::InvalidTarget))
                });
                let (prompt, prompted_revision) = match prompt {
                    Ok(Ok(Some(prompt))) => prompt,
                    Ok(Ok(None)) => continue,
                    Ok(Err(error)) => {
                        outcome = Err(error);
                        break;
                    }
                    Err(_) => {
                        outcome = Err(ProtectedCloseFailure::InvalidTarget);
                        break;
                    }
                };
                match prompt.await.unwrap_or(2) {
                    0 => {
                        let save = cx.update(|cx| {
                            if plan
                                .request
                                .caller
                                .as_ref()
                                .is_some_and(|caller| !caller.is_live(cx))
                            {
                                return Err(ProtectedCloseFailure::Cancelled);
                            }
                            let Some(shell) = target.shell.upgrade() else {
                                return Err(ProtectedCloseFailure::InvalidTarget);
                            };
                            let (save_completion, receiver) = CommandCompletion::new();
                            let result = cx.update_window(target.window, |_, window, cx| {
                                if window.root::<ProductShell>().flatten().as_ref() != Some(&shell)
                                {
                                    return Err(ProtectedCloseFailure::InvalidTarget);
                                }
                                shell.update(cx, |shell, cx| {
                                    shell.start_save_command(
                                        false,
                                        target.clone(),
                                        save_completion,
                                        cx,
                                    );
                                });
                                Ok(receiver)
                            });
                            result.unwrap_or(Err(ProtectedCloseFailure::InvalidTarget))
                        });
                        let save_outcome = match save {
                            Ok(Ok(receiver)) => receiver.await.unwrap_or(CommandOutcome::Cancelled),
                            Ok(Err(error)) => {
                                outcome = Err(error);
                                break;
                            }
                            Err(_) => {
                                outcome = Err(ProtectedCloseFailure::InvalidTarget);
                                break;
                            }
                        };
                        if save_outcome != CommandOutcome::Completed {
                            outcome = match save_outcome {
                                CommandOutcome::Cancelled => {
                                    Ok(PresentationCloseOutcome::Cancelled)
                                }
                                CommandOutcome::InvalidTarget => {
                                    Err(ProtectedCloseFailure::InvalidTarget)
                                }
                                CommandOutcome::HandlerFailure { message } => {
                                    Err(ProtectedCloseFailure::PersistenceFailed { message })
                                }
                                _ => Err(ProtectedCloseFailure::PersistenceFailed {
                                    message: "save is unavailable".into(),
                                }),
                            };
                            break;
                        }
                        let approval = cx.update(|cx| {
                            let documents = Self::documents(cx);
                            let Some(document_state) = documents.read(cx).get(*document) else {
                                return Err(ProtectedCloseFailure::InvalidTarget);
                            };
                            if document_state.is_dirty(cx) {
                                return Err(ProtectedCloseFailure::PersistenceFailed {
                                    message: "document changed while it was being saved".into(),
                                });
                            }
                            Ok(CloseApproval {
                                document: *document,
                                revision: document_state.model().read(cx).revision(),
                                kind: CloseApprovalKind::Saved,
                            })
                        });
                        match approval {
                            Ok(Ok(approval)) => approvals.push(approval),
                            Ok(Err(error)) => {
                                outcome = Err(error);
                                break;
                            }
                            Err(_) => {
                                outcome = Err(ProtectedCloseFailure::InvalidTarget);
                                break;
                            }
                        }
                    }
                    1 => {
                        let approval = cx.update(|cx| {
                            let documents = Self::documents(cx);
                            let Some(document_state) = documents.read(cx).get(*document) else {
                                return Err(ProtectedCloseFailure::InvalidTarget);
                            };
                            if document_state.model().read(cx).revision() != prompted_revision {
                                return Err(ProtectedCloseFailure::InvalidTarget);
                            }
                            Ok(CloseApproval {
                                document: *document,
                                revision: prompted_revision,
                                kind: CloseApprovalKind::Discarded,
                            })
                        });
                        match approval {
                            Ok(Ok(approval)) => approvals.push(approval),
                            Ok(Err(error)) => {
                                outcome = Err(error);
                                break;
                            }
                            Err(_) => {
                                outcome = Err(ProtectedCloseFailure::InvalidTarget);
                                break;
                            }
                        }
                    }
                    _ => {
                        outcome = Ok(PresentationCloseOutcome::Cancelled);
                        break;
                    }
                }
            }
            let current_views = if matches!(outcome, Ok(PresentationCloseOutcome::Closed)) {
                cx.update(Self::collect_product_views).ok()
            } else {
                None
            };
            let mut completion = Some(completion);
            if this
                .update(cx, |this, cx| {
                    if plan
                        .request
                        .caller
                        .as_ref()
                        .is_some_and(|caller| !caller.is_live(cx))
                    {
                        outcome = Err(ProtectedCloseFailure::Cancelled);
                    } else if matches!(outcome, Ok(PresentationCloseOutcome::Closed)) {
                        outcome = match current_views {
                            Some(ref views) => this
                                .commit_protected_close(&plan, &approvals, views, cx)
                                .map(|()| PresentationCloseOutcome::Closed),
                            None => Err(ProtectedCloseFailure::InvalidTarget),
                        };
                    }
                    Self::set_protected_closure_active(false, cx);
                    completion.take().unwrap()(outcome, cx);
                })
                .is_err()
            {
                let _ = cx.update(|cx| {
                    Self::set_protected_closure_active(false, cx);
                    completion.take().unwrap()(Err(ProtectedCloseFailure::InvalidTarget), cx);
                });
            }
        })
        .detach();
    }

    fn finish_protected_close_completion(
        completion: Option<CommandCompletion>,
        outcome: crate::host::protocol::CommandOutcome,
        cx: &mut App,
    ) {
        let Some(completion) = completion else {
            return;
        };
        completion.complete(outcome.clone());
        Self::command_dispatcher(cx).update(cx, |dispatcher, cx| {
            dispatcher.record_outcome(outcome, cx);
        });
    }

    fn commit_protected_close(
        &mut self,
        plan: &ProtectedClosePlan,
        approvals: &[CloseApproval],
        views: &[ProtectedView],
        cx: &mut Context<Self>,
    ) -> Result<(), ProtectedCloseFailure> {
        let current_scope = views
            .iter()
            .filter(|view| match plan.request.kind {
                ProtectedCloseKind::Tab => {
                    view.key.window == plan.request.origin.window
                        && view.key.pane == plan.request.origin.pane
                        && view.key.tab == plan.request.origin.tab
                        && view.key.surface == plan.request.origin.surface
                }
                ProtectedCloseKind::Window => view.key.window == plan.request.origin.window,
                ProtectedCloseKind::Quit => true,
            })
            .map(|view| view.key)
            .collect::<Vec<_>>();
        if current_scope.len() != plan.scope.len()
            || current_scope
                .iter()
                .any(|key| !plan.scope.iter().any(|planned| planned == key))
        {
            return Err(ProtectedCloseFailure::InvalidTarget);
        }
        let documents = Self::documents(cx);
        for approval in approvals {
            let Some(document) = documents.read(cx).get(approval.document) else {
                return Err(ProtectedCloseFailure::InvalidTarget);
            };
            if document.model().read(cx).revision() != approval.revision
                || (approval.kind == CloseApprovalKind::Saved && document.is_dirty(cx))
            {
                return Err(ProtectedCloseFailure::InvalidTarget);
            }
        }
        let total_counts = views.iter().fold(HashMap::new(), |mut counts, view| {
            if let TabSurfaceId::Document(document) = view.key.surface {
                *counts.entry(document).or_insert(0usize) += 1;
            }
            counts
        });
        let scoped_counts = current_scope
            .iter()
            .fold(HashMap::new(), |mut counts, view| {
                if let TabSurfaceId::Document(document) = view.surface {
                    *counts.entry(document).or_insert(0usize) += 1;
                }
                counts
            });
        for document in documents.read(cx).documents() {
            let loses_final_view = plan.request.kind == ProtectedCloseKind::Quit
                || scoped_counts.get(&document.id()) == total_counts.get(&document.id());
            if loses_final_view && document.is_dirty(cx) {
                let approved = approvals.iter().any(|approval| {
                    approval.document == document.id()
                        && approval.kind == CloseApprovalKind::Discarded
                });
                if !approved {
                    return Err(ProtectedCloseFailure::InvalidTarget);
                }
            }
        }

        match plan.request.kind {
            ProtectedCloseKind::Tab => self.close_tab_after_approval(
                plan.request.origin.window,
                plan.request.origin.pane,
                plan.request.origin.tab,
                plan.request.origin.surface,
                cx,
            ),
            ProtectedCloseKind::Window => {
                let final_documents = scoped_counts
                    .iter()
                    .filter(|(document, count)| total_counts.get(document) == Some(count))
                    .map(|(document, _)| *document)
                    .collect::<Vec<_>>();
                match cx.update_window(plan.request.origin.window, |_, window, cx| {
                    tab_drag::cancel_source_window(window, cx);
                    window.remove_window();
                }) {
                    Ok(()) => {
                        documents.update(cx, |documents, cx| {
                            for document in final_documents {
                                documents.remove(document, cx);
                            }
                        });
                        Self::close_terminal_ids(
                            current_scope.iter().filter_map(|key| match key.surface {
                                TabSurfaceId::Terminal(id) => Some(id),
                                TabSurfaceId::Document(_) | TabSurfaceId::Tool(_) => None,
                            }),
                            cx,
                        );
                        Ok(())
                    }
                    Err(_) => Err(ProtectedCloseFailure::InvalidTarget),
                }
            }
            ProtectedCloseKind::Quit => {
                Self::close_terminal_ids(
                    current_scope.iter().filter_map(|key| match key.surface {
                        TabSurfaceId::Terminal(id) => Some(id),
                        TabSurfaceId::Document(_) | TabSurfaceId::Tool(_) => None,
                    }),
                    cx,
                );
                super::extension_host::quit_after_storage(cx);
                Ok(())
            }
        }
    }

    #[cfg(test)]
    fn close_tab(
        &mut self,
        pane: PaneId,
        tab: TabId,
        document: DocumentId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> crate::host::protocol::CommandOutcome {
        use crate::host::protocol::CommandOutcome;

        if !self.workbench.read(cx).contains_tab(pane, tab, document) {
            return CommandOutcome::InvalidTarget;
        }
        let open_view_count = cx
            .global::<ApplicationWorkbenches>()
            .view_count(document, cx);
        let documents = Self::documents(cx);
        let document_is_dirty = documents.read(cx).get(document).unwrap().is_dirty(cx);
        let outcome = self.workbench.update(cx, |workbench, _cx| {
            workbench.request_close_tab_with_state(
                pane,
                tab,
                document,
                document_is_dirty,
                open_view_count,
            )
        });
        let Some(outcome) = outcome else {
            return CommandOutcome::InvalidTarget;
        };
        let transition = match outcome {
            CloseRequestOutcome::Pending(_) => {
                self.status = "save confirmation will be added in checkpoint 7".into();
                cx.notify();
                return CommandOutcome::Unavailable;
            }
            CloseRequestOutcome::Closed(transition) => transition,
        };

        if transition.document == Some(DocumentCloseDisposition::CloseRequested) {
            documents.update(cx, |documents, cx| {
                documents.remove(document, cx);
            });
        }
        if transition.workbench_is_empty() {
            let replacement = create_untitled_document(&documents, cx);
            let model = documents.read(cx).get(replacement).unwrap().model().clone();
            let workbench = cx.new(|cx| Workbench::new_for_document(replacement, model, cx));
            cx.global::<ApplicationWorkbenches>()
                .register(&workbench, window.window_handle());
            self.workbench = workbench;
            self.status = "created replacement untitled document".into();
        } else {
            self.status = "closed tab".into();
        }
        self.focus_active_editor(window, cx);
        cx.notify();
        CommandOutcome::Completed
    }

    fn close_tab_after_approval(
        &mut self,
        window_handle: AnyWindowHandle,
        pane: PaneId,
        tab: TabId,
        surface: TabSurfaceId,
        cx: &mut Context<Self>,
    ) -> Result<(), ProtectedCloseFailure> {
        use crate::app::workbench::CloseConfirmation;

        if !self.workbench.read(cx).contains_surface(pane, tab, surface) {
            return Err(ProtectedCloseFailure::InvalidTarget);
        }
        if let TabSurfaceId::Tool(id) = surface {
            self.workbench.update(cx, |workbench, _| {
                workbench.hide_tool(id);
            });
            let _ = cx.update_window(window_handle, |_, window, cx| {
                self.focus_active_surface(window, cx)
            });
            cx.notify();
            return Ok(());
        }
        let TabSurfaceId::Document(document) = surface else {
            let TabSurfaceId::Terminal(session_id) = surface else {
                unreachable!()
            };
            let outcome = self.workbench.update(cx, |workbench, _| {
                workbench.request_close_terminal_tab(pane, tab, session_id)
            });
            let Some(transition) = outcome else {
                return Err(ProtectedCloseFailure::InvalidTarget);
            };
            self.close_terminal(session_id, cx);
            if transition.workbench_is_empty() {
                self.replace_empty_workbench(window_handle, cx);
            }
            let _ = cx.update_window(window_handle, |_, window, cx| {
                self.focus_active_surface(window, cx)
            });
            cx.notify();
            return Ok(());
        };
        let open_view_count = cx
            .global::<ApplicationWorkbenches>()
            .view_count(document, cx);
        let documents = Self::documents(cx);
        let document_is_dirty = documents
            .read(cx)
            .get(document)
            .is_some_and(|document| document.is_dirty(cx));
        let outcome = self.workbench.update(cx, |workbench, _| {
            workbench.request_close_tab_with_state(
                pane,
                tab,
                document,
                document_is_dirty,
                open_view_count,
            )
        });
        let transition = match outcome {
            Some(CloseRequestOutcome::Closed(transition)) => transition,
            Some(CloseRequestOutcome::Pending(pending)) => self
                .workbench
                .update(cx, |workbench, _| {
                    workbench.resolve_pending_close(
                        pending,
                        CloseConfirmation::Close,
                        open_view_count,
                    )
                })
                .unwrap(),
            None => return Err(ProtectedCloseFailure::InvalidTarget),
        };
        if transition.document == Some(DocumentCloseDisposition::CloseRequested) {
            documents.update(cx, |documents, cx| {
                documents.remove(document, cx);
            });
        }
        if transition.workbench_is_empty() {
            self.replace_empty_workbench(window_handle, cx);
        } else {
            self.status = "closed tab".into();
        }
        let _ = cx.update_window(window_handle, |_, window, cx| {
            self.focus_active_surface(window, cx)
        });
        cx.notify();
        Ok(())
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
            self.status = "created replacement untitled document".into();
            return;
        }
        let workbench = cx.new(|cx| Workbench::new_for_document(replacement, model, cx));
        cx.global::<ApplicationWorkbenches>()
            .register(&workbench, window);
        self.workbench = workbench;
        self.status = "created replacement untitled document".into();
    }

    fn move_terminal_to_new_window_with(
        &mut self,
        target: &ProductCommandTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
        create_destination: impl FnOnce(&mut App) -> anyhow::Result<WindowHandle<ProductShell>>,
    ) -> crate::host::protocol::CommandOutcome {
        use crate::host::protocol::CommandOutcome;
        if !matches!(target.surface, TabSurfaceId::Terminal(_)) {
            return CommandOutcome::Unavailable;
        }
        if !self.target_is_live(target, cx) {
            return CommandOutcome::InvalidTarget;
        }
        let Some(source) = self.capture_tab_relocation(target.pane, target.tab, cx) else {
            return CommandOutcome::InvalidTarget;
        };
        self.relocate_tab_to_new_window_with(&source, window, cx, create_destination)
    }

    pub(crate) fn toggle_extension_report(&mut self, cx: &mut Context<Self>) {
        self.extension_report_open = !self.extension_report_open;
        cx.notify();
    }

    fn render_layout(&self, layout: &WorkbenchLayout, cx: &mut Context<Self>) -> AnyElement {
        match layout {
            WorkbenchLayout::Pane(pane) => self.render_pane(*pane, cx),
            WorkbenchLayout::Split {
                direction,
                first,
                second,
            } => {
                let direction = *direction;
                // Only dock-adjacent dividers resize. Document/document splits
                // stay equal; docks keep window-local sizes with usable limits.
                let dock_extent = |layout: &WorkbenchLayout| match layout {
                    WorkbenchLayout::Pane(pane) => self
                        .workbench
                        .read(cx)
                        .dock_location(*pane)
                        .filter(|dock| {
                            (direction == SplitDirection::Vertical)
                                == (*dock == DockLocation::Bottom)
                        })
                        .map(|dock| (dock, self.workbench.read(cx).dock_size(dock))),
                    _ => None,
                };
                let first_extent = dock_extent(first);
                let second_extent = dock_extent(second);
                let divider_dock = first_extent
                    .map(|(dock, _)| dock)
                    .or_else(|| second_extent.map(|(dock, _)| dock));
                div()
                    .flex()
                    .when(direction == SplitDirection::Horizontal, |element| {
                        element.flex_row()
                    })
                    .when(direction == SplitDirection::Vertical, |element| {
                        element.flex_col()
                    })
                    .size_full()
                    .min_w_0()
                    .min_h_0()
                    .child(
                        div()
                            .when(first_extent.is_none(), |element| element.flex_1())
                            .min_w_0()
                            .min_h_0()
                            .when_some(first_extent, |element, (_, extent)| {
                                element
                                    .flex_none()
                                    .when(direction == SplitDirection::Horizontal, |element| {
                                        element.w(px(extent))
                                    })
                                    .when(direction == SplitDirection::Vertical, |element| {
                                        element.h(px(extent))
                                    })
                            })
                            .child(self.render_layout(first, cx)),
                    )
                    .child(self.render_divider(direction, divider_dock, cx))
                    .child(
                        div()
                            .when(second_extent.is_none(), |element| element.flex_1())
                            .min_w_0()
                            .min_h_0()
                            .when_some(second_extent, |element, (_, extent)| {
                                element
                                    .flex_none()
                                    .when(direction == SplitDirection::Horizontal, |element| {
                                        element.w(px(extent))
                                    })
                                    .when(direction == SplitDirection::Vertical, |element| {
                                        element.h(px(extent))
                                    })
                            })
                            .child(self.render_layout(second, cx)),
                    )
                    .into_any_element()
            }
        }
    }

    /// Divider between two split children. Dock-adjacent dividers get a wider
    /// hit area and drag to resize; other dividers stay a fixed 1px rule.
    fn render_divider(
        &self,
        direction: SplitDirection,
        dock: Option<DockLocation>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(dock) = dock else {
            return div()
                .when(direction == SplitDirection::Horizontal, |element| {
                    element.w(px(1.)).h_full()
                })
                .when(direction == SplitDirection::Vertical, |element| {
                    element.h(px(1.)).w_full()
                })
                .bg(rgb(0x454545))
                .into_any_element();
        };
        let horizontal = direction == SplitDirection::Horizontal;
        div()
            .id(("dock-divider", dock.index()))
            .debug_selector(move || format!("dock-divider-{}", dock.index()))
            .flex()
            .flex_none()
            .when(horizontal, |element| {
                element.flex_row().w(px(9.)).h_full().justify_center()
            })
            .when(!horizontal, |element| {
                element.flex_col().h(px(9.)).w_full().items_center()
            })
            .child(
                div()
                    .when(horizontal, |element| element.w(px(1.)).h_full())
                    .when(!horizontal, |element| element.h(px(1.)).w_full())
                    .bg(rgb(0x454545))
                    .hover(|style| style.bg(rgb(0x7fb5ff))),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    let size = this.workbench.read(cx).dock_size(dock);
                    this.dock_resize = Some(DockResizeDrag {
                        dock,
                        start_pointer: event.position,
                        start_size: size,
                    });
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .into_any_element()
    }

    fn render_pane(&self, pane_id: PaneId, cx: &mut Context<Self>) -> AnyElement {
        let workbench = self.workbench.read(cx);
        let pane = workbench.pane(pane_id).unwrap();
        let active_tab = pane.active_tab_id();
        let active_view: Option<AnyElement> = pane
            .active_tab()
            .editor()
            .cloned()
            .map(|editor| editor.into_any_element())
            .or_else(|| {
                pane.active_tab()
                    .terminal_view()
                    .cloned()
                    .map(|view| view.into_any_element())
            });
        let active_view = active_view.or_else(|| {
            pane.active_tab()
                .tool_content()
                .map(|content| match content {
                    ToolContent::Workspace(view) => view.clone().into_any_element(),
                    ToolContent::Extension(super::workbench::ExtensionView::Tree(view)) => {
                        view.clone().into_any_element()
                    }
                    ToolContent::Extension(super::workbench::ExtensionView::Text(view)) => {
                        view.clone().into_any_element()
                    }
                })
        });
        let documents = Self::documents(cx);
        let tabs = pane
            .tabs()
            .iter()
            .map(|tab| {
                let tab_id = tab.id();
                let documents = documents.read(cx);
                let title = match tab.surface_id() {
                    TabSurfaceId::Document(document_id) => {
                        let document = documents.get(document_id).unwrap();
                        format!(
                            "{}{}",
                            document.title(),
                            if document.is_dirty(cx) { " •" } else { "" }
                        )
                    }
                    TabSurfaceId::Terminal(_) => "Terminal".to_owned(),
                    TabSurfaceId::Tool(id) => workbench.tool(id).unwrap().title.to_owned(),
                };
                let capture = workbench.capture_tab_move(pane_id, tab_id).unwrap();
                let source = self.capture_tab_relocation(pane_id, tab_id, cx).unwrap();
                let drag_label = self.tab_drag_label(title.clone().into(), cx);
                div()
                    .id(("tab", tab_id.value()))
                    .debug_selector(move || format!("tab-{}", tab_id.value()))
                    .relative()
                    .on_drag(source, drag_label)
                    .child(self.tab_drag_geometry(pane_id, capture))
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .text_sm()
                    .text_color(if tab_id == active_tab {
                        rgb(0xf0f0f0)
                    } else {
                        rgb(0xaaaaaa)
                    })
                    .bg(if tab_id == active_tab {
                        rgb(0x1e1e1e)
                    } else {
                        rgb(0x2d2d2d)
                    })
                    .child(title)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.activate_tab(pane_id, tab_id, window, cx);
                    }))
            })
            .collect::<Vec<_>>();

        div()
            .id(("pane", pane_id.value()))
            .debug_selector(move || format!("pane-{}", pane_id.value()))
            .relative()
            .child(self.pane_drag_geometry(pane_id))
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .min_h_0()
            .bg(rgb(0x1e1e1e))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_none()
                    .h(px(30.))
                    .relative()
                    .debug_selector(move || format!("tab-bar-{}", pane_id.value()))
                    .child(self.tab_bar_drag_geometry(pane_id))
                    .overflow_hidden()
                    .bg(rgb(0x2d2d2d))
                    .children(tabs)
                    .when(
                        matches!(pane.active_tab().surface_id(), TabSurfaceId::Tool(_)),
                        |element| {
                            element.child(div().flex_1()).child(
                                div()
                                    .id(("hide-tool", pane_id.value()))
                                    .px_2()
                                    .cursor_pointer()
                                    .child("×")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        let id = this.workbench.read(cx).pane(pane_id).and_then(
                                            |pane| match pane.active_tab().surface_id() {
                                                TabSurfaceId::Tool(id) => Some(id),
                                                _ => None,
                                            },
                                        );
                                        if let Some(id) = id {
                                            this.workbench.update(cx, |workbench, _| {
                                                workbench.hide_tool(id);
                                            });
                                            this.publish_tool_layout(cx);
                                            this.focus_active_surface(window, cx);
                                            cx.notify();
                                        }
                                    })),
                            )
                        },
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .when_some(active_view, |element, view| element.child(view)),
            )
            .into_any_element()
    }
}

impl Render for ProductShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.observe_application_focus(window, cx);
        tab_drag::coordinator(cx);
        self.tab_drag.borrow_mut().begin_frame();
        let layout = self.workbench.read(cx).layout().cloned();
        let entity = cx.entity();
        let command_palette = self.command_palette.clone();
        let transient = self.render_transient();
        let dismiss_transient =
            cx.listener(|this: &mut Self, _: &MouseDownEvent, _: &mut Window, cx| {
                this.finish_transient(TransientOutcome::Dismissed, cx);
            });
        let startup_report = super::extension_host::startup_report(cx);
        let extension_button_label = match &startup_report {
            Some(report) if report.state == super::extension_host::StartupState::Complete => {
                let failures = report
                    .report
                    .entries
                    .iter()
                    .filter(|entry| {
                        matches!(entry.result, super::extension_load::LoadResult::Failed(_))
                    })
                    .count();
                if failures > 0 {
                    format!("extensions ({failures} failed)")
                } else {
                    "extensions".to_owned()
                }
            }
            Some(_) => "extensions (loading)".to_owned(),
            None => "extensions".to_owned(),
        };
        let extension_report = self.extension_report_open.then_some(startup_report);
        window.set_window_title("Knot");

        div()
            .relative()
            .key_context("product")
            .capture_key_down(cx.listener(Self::cancel_tab_drag))
            .on_action(cx.listener(Self::dispatch_source))
            .on_action(cx.listener(Self::dispatch_keybinding))
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(0x1e1e1e))
            .text_color(rgb(0xe0e0e0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_3()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .bg(rgb(0x252526))
                    .child(command_button("new tab", "new-tab", &entity, NEW_COMMAND))
                    .child(command_button(
                        "terminal",
                        "new-terminal",
                        &entity,
                        NEW_TERMINAL_COMMAND,
                    ))
                    .child(command_button(
                        "move terminal",
                        "move-terminal",
                        &entity,
                        MOVE_TERMINAL_TO_NEW_WINDOW_COMMAND,
                    ))
                    .child(command_button(
                        "split →",
                        "split-horizontal",
                        &entity,
                        SPLIT_HORIZONTAL_COMMAND,
                    ))
                    .child(command_button(
                        "split ↓",
                        "split-vertical",
                        &entity,
                        SPLIT_VERTICAL_COMMAND,
                    ))
                    .child(command_button(
                        "close tab",
                        "close-tab",
                        &entity,
                        CLOSE_TAB_COMMAND,
                    ))
                    .child(command_button(
                        "commands",
                        "product-command-palette",
                        &entity,
                        SHOW_COMMAND_PALETTE_COMMAND,
                    ))
                    .child(command_button(
                        extension_button_label,
                        "extension-startup-report",
                        &entity,
                        SHOW_EXTENSION_REPORT_COMMAND,
                    ))
                    .child(
                        div()
                            .id("explorer-toggle")
                            .px_2()
                            .cursor_pointer()
                            .child("Explorer")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_explorer(window, cx);
                            })),
                    )
                    .children(DockLocation::ALL.into_iter().map(|dock| {
                        div()
                            .id(("dock-toggle", dock.index()))
                            .px_2()
                            .cursor_pointer()
                            .child(match dock {
                                DockLocation::Left => "Left",
                                DockLocation::Right => "Right",
                                DockLocation::Bottom => "Bottom",
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.workbench
                                    .update(cx, |workbench, _| workbench.toggle_dock(dock));
                                this.publish_tool_layout(cx);
                                this.focus_active_surface(window, cx);
                                cx.notify();
                            }))
                    }))
                    .child(div().flex_1())
                    .child(self.status.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_row()
                    .min_h_0()
                    .min_w_0()
                    .when_some(layout, |element, layout| {
                        element.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .min_h_0()
                                .relative()
                                .child(self.render_layout(&layout, cx))
                                .child(self.workbench_drag_geometry()),
                        )
                    }),
            )
            .child(self.drag_listener(cx))
            .when_some(transient, |root, transient| {
                root.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_start()
                        .justify_center()
                        .pt(px(80.))
                        .bg(rgba(0x00000080))
                        .on_mouse_down(MouseButton::Left, dismiss_transient)
                        .child(transient),
                )
            })
            .when_some(command_palette, |root, palette| {
                root.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_start()
                        .justify_center()
                        .pt(px(80.))
                        .bg(rgba(0x00000080))
                        .child(palette),
                )
            })
            .when_some(extension_report, |root, report| {
                root.child(render_extension_report(report, &entity))
            })
            .into_any_element()
    }
}

fn render_extension_report(
    snapshot: Option<super::extension_host::StartupReportSnapshot>,
    shell: &Entity<ProductShell>,
) -> AnyElement {
    use super::extension_host::StartupState;
    use super::extension_load::LoadResult;

    let (root, state, entries) = match snapshot {
        Some(snapshot) => (
            snapshot.root.display().to_string(),
            snapshot.state,
            snapshot.report.entries,
        ),
        None => (
            "No installed extension scan".to_owned(),
            StartupState::Complete,
            Vec::new(),
        ),
    };
    let status = match state {
        StartupState::Scanning => "Scanning installed extensions",
        StartupState::Loading => "Loading installed extensions",
        StartupState::Complete => "Extension startup report",
    };
    let rows = entries
        .into_iter()
        .map(|entry| {
            let name = entry.name.unwrap_or_else(|| "<invalid package>".into());
            let (label, color, cause) = match entry.result {
                LoadResult::Loaded => ("Loaded", 0x8fd18f, None),
                LoadResult::Failed(cause) => ("Failed", 0xf19a8e, Some(cause)),
            };
            div()
                .flex()
                .flex_col()
                .gap_1()
                .py_2()
                .border_b_1()
                .border_color(rgb(0x454545))
                .child(
                    div()
                        .text_color(rgb(color))
                        .child(format!("{label}: {name}")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(0xaaaaaa))
                        .child(entry.directory.display().to_string()),
                )
                .when_some(cause, |row, cause| {
                    row.child(div().text_color(rgb(0xf19a8e)).child(cause))
                })
        })
        .collect::<Vec<_>>();
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_start()
        .justify_center()
        .pt(px(60.))
        .bg(rgba(0x000000a0))
        .child(
            div()
                .w(px(760.))
                .h(px(520.))
                .flex()
                .flex_col()
                .bg(rgb(0x252526))
                .border_1()
                .border_color(rgb(0x555555))
                .p_4()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .child(div().flex_1().text_lg().child(status))
                        .child(command_button(
                            "close",
                            "close-extension-report",
                            shell,
                            SHOW_EXTENSION_REPORT_COMMAND,
                        )),
                )
                .child(div().text_xs().text_color(rgb(0xaaaaaa)).child(root))
                .child(
                    div()
                        .id("extension-report-rows")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .when(rows.is_empty() && state == StartupState::Complete, |body| {
                            body.child("No installed extensions")
                        })
                        .children(rows),
                ),
        )
        .into_any_element()
}

fn command_button(
    label: impl Into<SharedString>,
    id: &'static str,
    shell: &Entity<ProductShell>,
    command: &'static str,
) -> Stateful<Div> {
    let shell = shell.clone();
    let label = label.into();
    div()
        .id(id)
        .cursor_pointer()
        .text_color(rgb(0x80c0ff))
        .child(label)
        .on_click(move |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.dispatch_source(&ProductCommandSource::new(command), window, cx);
            })
        })
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

fn product_filesystems() -> Arc<FileSystemProviderRegistry> {
    static FILESYSTEMS: OnceLock<Arc<FileSystemProviderRegistry>> = OnceLock::new();
    FILESYSTEMS
        .get_or_init(|| {
            let runtime = Arc::new(
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(1)
                    .enable_all()
                    .thread_name("knot-product-filesystem")
                    .build()
                    .expect("product filesystem runtime starts"),
            );
            let mut providers = FileSystemProviderRegistry::new();
            providers
                .register(
                    "file",
                    Arc::new(LocalFileSystemProvider::unrestricted(runtime)),
                )
                .expect("the product file provider is unique");
            Arc::new(providers)
        })
        .clone()
}

fn install_protected_window_close(shell: &Entity<ProductShell>, window: &Window, cx: &App) {
    let shell = shell.downgrade();
    window.on_window_should_close(cx, move |window, cx| {
        let Some(shell) = shell.upgrade() else {
            return true;
        };
        if shell.read(cx).workbench.read(cx).panes().is_empty() {
            return true;
        }
        let target = shell.update(cx, |shell, cx| shell.window_close_target(window, cx));
        if let Some(target) = target {
            cx.defer(move |cx| {
                ProductShell::begin_protected_close(ProtectedCloseKind::Window, target, None, cx);
            });
        }
        false
    });
}

pub(crate) fn open_product_window(request: Option<OpenRequest>, cx: &mut App) {
    let documents = cx.global::<ApplicationDocuments>().0.clone();
    let document = create_untitled_document(&documents, cx);
    let model = documents.read(cx).get(document).unwrap().model().clone();
    let bounds = Bounds::centered(None, size(px(1000.), px(720.)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(480.), px(320.))),
            ..Default::default()
        },
        move |window, cx| {
            let workbench = cx.new(|cx| Workbench::new_for_document(document, model, cx));
            cx.global::<ApplicationWorkbenches>()
                .register(&workbench, window.window_handle());
            let shell = cx.new(|_| ProductShell::new(workbench));
            shell.update(cx, |shell, cx| shell.observe_extension_host(window, cx));
            install_protected_window_close(&shell, window, cx);
            shell.update(cx, |shell, cx| shell.restore_fresh_window_layout(cx));
            shell.read(cx).focus_active_surface(window, cx);
            if let Some(request) = request {
                let pane = shell.read(cx).workbench.read(cx).focused_pane().unwrap();
                let tab = pane.active_tab();
                let target = OpenTarget {
                    window: window.window_handle(),
                    workbench: shell.read(cx).workbench.downgrade(),
                    pane: pane.id(),
                    tab: tab.id(),
                    surface: tab.surface_id(),
                    presentation: tab.presentation(),
                };
                shell.update(cx, |shell, cx| {
                    shell.start_open_request(request, target, true, None, cx);
                });
            }
            shell
        },
    )
    .expect("product window must open");
}

fn open_tab_transfer_window(cx: &mut App) -> anyhow::Result<WindowHandle<ProductShell>> {
    let bounds = Bounds::centered(None, size(px(1000.), px(720.)), cx);
    open_tab_transfer_window_with_bounds(bounds, cx)
}

fn open_tab_tear_out_window(
    pointer: Point<Pixels>,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<ProductShell>> {
    let mut bounds = Bounds::new(
        pointer - point(px(140.), px(40.)),
        size(px(1000.), px(720.)),
    );
    if let Some(display) = cx
        .displays()
        .into_iter()
        .find(|display| display.bounds().contains(&pointer))
    {
        let display = display.bounds();
        bounds.size.width = bounds.size.width.min(display.size.width).max(px(480.));
        bounds.size.height = bounds.size.height.min(display.size.height).max(px(320.));
        bounds.origin.x = bounds
            .origin
            .x
            .max(display.left())
            .min((display.right() - bounds.size.width).max(display.left()));
        bounds.origin.y = bounds
            .origin
            .y
            .max(display.top())
            .min((display.bottom() - bounds.size.height).max(display.top()));
    }
    open_tab_transfer_window_with_bounds(bounds, cx)
}

fn open_tab_transfer_window_with_bounds(
    bounds: Bounds<Pixels>,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<ProductShell>> {
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(480.), px(320.))),
            focus: !cfg!(target_os = "macos"),
            show: !cfg!(target_os = "macos"),
            ..Default::default()
        },
        |window, cx| {
            let workbench = cx.new(|_| Workbench::empty());
            let shell = cx.new(|_| ProductShell::new(workbench));
            shell.update(cx, |shell, cx| shell.observe_extension_host(window, cx));
            install_protected_window_close(&shell, window, cx);
            shell
        },
    )
}

fn dispatch_launch_request(request: Option<OpenRequest>, cx: &mut App) {
    let ready_request = cx
        .global::<ApplicationLaunchGate>()
        .0
        .borrow_mut()
        .request(request);
    if let Some(request) = ready_request {
        open_product_window(request, cx);
        cx.activate(true);
    }
}

pub(super) fn finish_product_startup(
    result: Result<(), super::personal_config::ConfigDiagnostic>,
    cx: &mut App,
) {
    match result {
        Ok(()) => {
            let requests = cx
                .global::<ApplicationLaunchGate>()
                .0
                .borrow_mut()
                .complete();
            for request in requests {
                open_product_window(request, cx);
            }
            cx.activate(true);
        }
        Err(diagnostic) => {
            cx.global::<ApplicationLaunchGate>().0.borrow_mut().fail();
            eprintln!("[knot] {}", diagnostic.clipboard_text().replace('\n', ": "));
            super::personal_config_error::open(diagnostic, cx);
        }
    }
}

pub(crate) fn run(initial_request: Option<OpenRequest>) {
    let application = Application::new();
    let (open_requests, mut incoming_requests) = tokio::sync::mpsc::unbounded_channel();
    application.on_open_urls(move |urls| {
        for url in urls {
            match OpenRequest::from_url(&url) {
                Ok(request) => {
                    let _ = open_requests.send(request);
                }
                Err(error) => eprintln!("[knot] cannot open {url}: {error}"),
            }
        }
    });
    application.on_reopen(|cx| {
        dispatch_launch_request(None, cx);
    });
    application.run(move |cx| {
        let documents = cx.new(|_| DocumentCollection::new());
        cx.set_global(ApplicationDocuments(documents));
        cx.set_global(ApplicationWorkbenches(RefCell::new(Vec::new())));
        cx.set_global(ApplicationProtectedClosure(RefCell::new(false)));
        cx.set_global(ApplicationTerminalSessions(RefCell::new(
            TerminalSessions::default(),
        )));
        // Load the persisted tool arrangement before any window opens so the
        // first window restores it; a missing or unreadable record keeps
        // product defaults without delaying startup.
        let layout_path = super::tool_layout::ToolLayoutStore::user_path()
            .unwrap_or_else(|_| std::env::temp_dir().join("knot-tool-layout.sqlite3"));
        cx.set_global(super::tool_layout::ApplicationToolLayout(
            super::tool_layout::ToolLayoutStore::open(
                layout_path,
                cx.background_executor().clone(),
            ),
        ));
        cx.set_global(ApplicationLaunchGate(RefCell::new(LaunchGate::Pending(
            Vec::new(),
        ))));
        cx.set_global(ApplicationFileSystems(product_filesystems()));
        let commands = cx.new(ProductCommandDispatcher::new);
        cx.set_global(ApplicationProductCommands(commands));
        let extension_host = super::extension_host::install(cx);
        bind_product_keys(cx);
        cx.set_menus(vec![
            Menu {
                name: "Knot".into(),
                items: vec![MenuItem::action(
                    "Quit Knot",
                    ProductCommandSource::new(QUIT_COMMAND),
                )],
            },
            Menu {
                name: "File".into(),
                items: vec![
                    MenuItem::action("New", ProductCommandSource::new(NEW_COMMAND)),
                    MenuItem::action(
                        "New Terminal",
                        ProductCommandSource::new(NEW_TERMINAL_COMMAND),
                    ),
                    MenuItem::action(
                        "Move Terminal to New Window",
                        ProductCommandSource::new(MOVE_TERMINAL_TO_NEW_WINDOW_COMMAND),
                    ),
                    MenuItem::action("New Window", ProductCommandSource::new(NEW_WINDOW_COMMAND)),
                    MenuItem::action("Open…", ProductCommandSource::new(OPEN_COMMAND)),
                    MenuItem::action("Save", ProductCommandSource::new(SAVE_COMMAND)),
                    MenuItem::action("Save As…", ProductCommandSource::new(SAVE_AS_COMMAND)),
                    MenuItem::action("Close Tab", ProductCommandSource::new(CLOSE_TAB_COMMAND)),
                    MenuItem::action(
                        "Close Window",
                        ProductCommandSource::new(CLOSE_WINDOW_COMMAND),
                    ),
                ],
            },
            Menu {
                name: "Edit".into(),
                items: vec![
                    MenuItem::action("Undo", ProductCommandSource::new(UNDO_COMMAND)),
                    MenuItem::action("Redo", ProductCommandSource::new(REDO_COMMAND)),
                    MenuItem::action("Cut", ProductCommandSource::new(CUT_COMMAND)),
                    MenuItem::action("Copy", ProductCommandSource::new(COPY_COMMAND)),
                    MenuItem::action("Paste", ProductCommandSource::new(PASTE_COMMAND)),
                    MenuItem::action("Select All", ProductCommandSource::new(SELECT_ALL_COMMAND)),
                    MenuItem::action("Find", ProductCommandSource::new(FIND_COMMAND)),
                    MenuItem::action("Find Next", ProductCommandSource::new(FIND_NEXT_COMMAND)),
                    MenuItem::action(
                        "Find Previous",
                        ProductCommandSource::new(FIND_PREVIOUS_COMMAND),
                    ),
                ],
            },
            Menu {
                name: "View".into(),
                items: vec![
                    MenuItem::action(
                        "Split Right",
                        ProductCommandSource::new(SPLIT_HORIZONTAL_COMMAND),
                    ),
                    MenuItem::action(
                        "Split Down",
                        ProductCommandSource::new(SPLIT_VERTICAL_COMMAND),
                    ),
                ],
            },
        ]);
        let first_request = initial_request.or_else(|| incoming_requests.try_recv().ok());
        if let Some(request) = first_request {
            dispatch_launch_request(Some(request), cx);
        }
        cx.spawn(async move |cx| {
            while let Some(request) = incoming_requests.recv().await {
                let _ = cx.update(|cx| dispatch_launch_request(Some(request), cx));
            }
        })
        .detach();
        let extensions_root = super::extension_package::user_extensions_root();
        extension_host.update(cx, |host, cx| {
            host.start_product_startup(None, extensions_root, cx)
        });
    });
}

pub(super) fn bind_product_keys(cx: &mut App) {
    super::keymaps::install(cx);
}

#[cfg(test)]
mod tests {
    mod closure;
    mod git_status;
    mod observation;
    mod operations;
    mod projects;
    mod tool_layout;
    mod tools;
    mod transient;
    mod workspaces;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use gpui::{
        AppContext, Focusable, Modifiers, MouseButton, TestAppContext, VisualTestContext, point, px,
    };

    use super::super::keymaps::{ApplicationKeymaps, BindingOwner, BindingResolution};
    use crate::host::protocol::{
        Command, CommandArgumentValue, CommandOutcome, ExtensionId, ExtensionLifecycleId,
    };

    use super::{
        ApplicationDocuments, ApplicationFileSystems, ApplicationLaunchGate, ApplicationOpenDialog,
        ApplicationProductCommands, ApplicationProtectedClosure, ApplicationSaveDialog,
        ApplicationTerminalSessions, ApplicationWorkbenches, BufferModel, CLOSE_TAB_COMMAND,
        CLOSE_WINDOW_COMMAND, DocumentCollection, Entity, LaunchGate,
        MOVE_TERMINAL_TO_NEW_WINDOW_COMMAND, NEW_COMMAND, NEW_TERMINAL_COMMAND, OpenDialogFuture,
        OpenDialogOutcome, OpenRequest, OpenTarget, ProductCommandDispatcher, ProductCommandSource,
        ProductOpenDialog, ProductSaveDialog, ProductShell, QUIT_COMMAND, REDO_COMMAND, RefCell,
        SAVE_AS_COMMAND, SAVE_COMMAND, SHOW_EXTENSION_REPORT_COMMAND, SPLIT_HORIZONTAL_COMMAND,
        SaveDialogFuture, SaveDialogOutcome, SplitDirection, TerminalSessions, UNDO_COMMAND,
        Workbench, WorkbenchLayout, create_untitled_document, install_protected_window_close,
        open_tab_transfer_window, product_filesystems,
    };
    use crate::app::{
        documents::DocumentState,
        filesystem::{
            FileSystemProvider, FileSystemProviderRegistry, MemoryFileSystemProvider,
            ProviderFuture, ResourceEntry, ResourceFile, ResourceStat, ResourceVersion,
        },
        resource::ResourceUri,
        workbench::TabSurfaceId,
    };

    struct GatedFileSystemProvider {
        inner: Arc<MemoryFileSystemProvider>,
        read_release: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
        replace_release: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    }

    impl FileSystemProvider for GatedFileSystemProvider {
        fn normalize(&self, uri: ResourceUri) -> ProviderFuture<'_, ResourceUri> {
            self.inner.normalize(uri)
        }

        fn enumerate(&self, uri: ResourceUri) -> ProviderFuture<'_, Vec<ResourceEntry>> {
            self.inner.enumerate(uri)
        }

        fn read(&self, uri: ResourceUri) -> ProviderFuture<'_, ResourceFile> {
            let release = self.read_release.lock().unwrap().take();
            Box::pin(async move {
                if let Some(release) = release {
                    let _ = release.await;
                }
                self.inner.read(uri).await
            })
        }

        fn create(&self, uri: ResourceUri, bytes: Vec<u8>) -> ProviderFuture<'_, ResourceVersion> {
            self.inner.create(uri, bytes)
        }

        fn replace(
            &self,
            uri: ResourceUri,
            expected: ResourceVersion,
            bytes: Vec<u8>,
        ) -> ProviderFuture<'_, ResourceVersion> {
            let release = self.replace_release.lock().unwrap().take();
            Box::pin(async move {
                if let Some(release) = release {
                    let _ = release.await;
                }
                self.inner.replace(uri, expected, bytes).await
            })
        }

        fn stat(&self, uri: ResourceUri) -> ProviderFuture<'_, ResourceStat> {
            self.inner.stat(uri)
        }
    }

    pub(super) fn install_globals(cx: &mut TestAppContext) -> Entity<DocumentCollection> {
        let documents = cx.new(|_| DocumentCollection::new());
        cx.set_global(ApplicationDocuments(documents.clone()));
        cx.set_global(ApplicationWorkbenches(RefCell::new(Vec::new())));
        cx.set_global(ApplicationProtectedClosure(RefCell::new(false)));
        cx.set_global(ApplicationTerminalSessions(RefCell::new(
            TerminalSessions::default(),
        )));
        cx.set_global(ApplicationFileSystems(product_filesystems()));
        let commands = cx.new(ProductCommandDispatcher::new);
        cx.set_global(ApplicationProductCommands(commands));
        // Each test gets an isolated tool-layout database; the directory is
        // retained as a global so the database path stays alive with the test.
        let layout_dir = tempfile::tempdir().unwrap();
        cx.set_global(crate::app::tool_layout::ApplicationToolLayout(
            crate::app::tool_layout::ToolLayoutStore::open(
                layout_dir.path().join("tool-layout.sqlite3"),
                cx.background_executor.clone(),
            ),
        ));
        cx.set_global(TestToolLayoutDir(layout_dir));
        documents
    }

    #[allow(dead_code, reason = "ownership keeps the test database path alive")]
    struct TestToolLayoutDir(tempfile::TempDir);

    impl gpui::Global for TestToolLayoutDir {}

    fn keymap_command(cx: &gpui::App, key: &str, view: Option<&str>) -> Option<String> {
        match cx
            .global::<ApplicationKeymaps>()
            .0
            .resolve(&gpui::Keystroke::parse(key).unwrap().unparse(), view)
        {
            Some(BindingResolution::Command(command)) => Some(command.name.to_string()),
            Some(BindingResolution::Unbound) => Some("<unbound>".into()),
            None => None,
        }
    }

    fn install_launch_gate(cx: &mut TestAppContext) {
        cx.set_global(ApplicationLaunchGate(RefCell::new(LaunchGate::Pending(
            Vec::new(),
        ))));
    }

    fn wait_for_product_startup(cx: &mut TestAppContext) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            cx.run_until_parked();
            let state = cx.read(
                |cx| match &*cx.global::<ApplicationLaunchGate>().0.borrow() {
                    LaunchGate::Pending(_) => None,
                    LaunchGate::Ready => Some(true),
                    LaunchGate::Failed => Some(false),
                },
            );
            if let Some(ready) = state {
                return ready;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "product startup did not finish"
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn launch_gate_keeps_requests_ordered_and_discards_them_on_failure() {
        let first = OpenRequest::from_uri_for_product(
            ResourceUri::parse("file:///queued-first.txt").unwrap(),
        );
        let second = OpenRequest::from_uri_for_product(
            ResourceUri::parse("file:///queued-second.txt").unwrap(),
        );
        let mut gate = LaunchGate::Pending(Vec::new());
        assert!(gate.request(Some(first.clone())).is_none());
        assert!(gate.request(Some(second.clone())).is_none());
        assert_eq!(gate.complete(), vec![Some(first), Some(second.clone())]);
        assert_eq!(gate.request(None), Some(None));

        let mut failed = LaunchGate::Pending(Vec::new());
        failed.request(None);
        failed.fail();
        assert!(failed.complete().is_empty());
        assert!(failed.request(Some(second)).is_none());

        let mut empty = LaunchGate::Pending(Vec::new());
        assert_eq!(empty.complete(), vec![None]);
    }

    pub(super) fn product_window(
        document: super::DocumentId,
        model: Entity<super::BufferModel>,
        cx: &mut TestAppContext,
    ) -> (Entity<ProductShell>, gpui::AnyWindowHandle) {
        let (shell, _) = cx.add_window_view(|window, cx| {
            let workbench = cx.new(|cx| Workbench::new_for_document(document, model, cx));
            cx.global::<ApplicationWorkbenches>()
                .register(&workbench, window.window_handle());
            ProductShell::new(workbench)
        });
        let window = cx.read(|cx| {
            cx.global::<ApplicationWorkbenches>()
                .window_for(&shell.read(cx).workbench)
                .unwrap()
        });
        cx.update_window(window, |_, native_window, cx| {
            shell.update(cx, |shell, cx| {
                shell.observe_extension_host(native_window, cx)
            });
            install_protected_window_close(&shell, native_window, cx);
        })
        .unwrap();
        (shell, window)
    }

    fn two_dirty_untitled_documents_in_one_window(
        documents: &Entity<DocumentCollection>,
        cx: &mut TestAppContext,
    ) -> (
        super::DocumentId,
        super::DocumentId,
        Entity<ProductShell>,
        gpui::AnyWindowHandle,
    ) {
        let first = cx.update(|cx| create_untitled_document(documents, cx));
        let first_model = cx.read(|cx| documents.read(cx).get(first).unwrap().model().clone());
        first_model.update(cx, |model, _| model.replace(0..0, "first").unwrap());
        let second_model = cx.new(|_| BufferModel::from_text("second"));
        let second = documents.update(cx, |documents, cx| {
            documents.create_untitled("Second", second_model.clone(), cx)
        });
        second_model.update(cx, |model, _| model.replace(0..0, "changed ").unwrap());
        let (shell, window) = product_window(first, first_model, cx);
        shell.update(cx, |shell, cx| {
            let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
            shell.workbench.update(cx, |workbench, cx| {
                workbench
                    .open_tab_for_document(pane, second, second_model, cx)
                    .unwrap();
            });
        });
        (first, second, shell, window)
    }

    fn memory_filesystems() -> Arc<FileSystemProviderRegistry> {
        memory_filesystems_with_provider().0
    }

    fn memory_filesystems_with_provider() -> (
        Arc<FileSystemProviderRegistry>,
        Arc<MemoryFileSystemProvider>,
    ) {
        let root = ResourceUri::parse("mem://product/").unwrap();
        let provider = Arc::new(MemoryFileSystemProvider::new(root).unwrap());
        provider
            .seed_file(
                ResourceUri::parse("mem://product/notes.txt").unwrap(),
                b"loaded text",
            )
            .unwrap();
        provider
            .seed_file(
                ResourceUri::parse("mem://product/invalid.txt").unwrap(),
                [0xff],
            )
            .unwrap();
        provider
            .seed_directory(ResourceUri::parse("mem://product/src").unwrap())
            .unwrap();
        let mut providers = FileSystemProviderRegistry::new();
        providers.register("mem", provider.clone()).unwrap();
        (Arc::new(providers), provider)
    }

    fn current_open_target(shell: &Entity<ProductShell>, cx: &gpui::App) -> OpenTarget {
        let shell = shell.read(cx);
        let pane = shell.workbench.read(cx).focused_pane().unwrap();
        let tab = pane.active_tab();
        OpenTarget {
            window: cx
                .global::<ApplicationWorkbenches>()
                .window_for(&shell.workbench)
                .unwrap(),
            workbench: shell.workbench.downgrade(),
            pane: pane.id(),
            tab: tab.id(),
            surface: tab.surface_id(),
            presentation: tab.presentation(),
        }
    }

    fn dispatch_product_command(
        shell: &Entity<ProductShell>,
        window_handle: gpui::AnyWindowHandle,
        name: &'static str,
        cx: &mut TestAppContext,
    ) -> super::super::CommandExecution {
        let target = cx
            .update_window(window_handle, |_, window, cx| {
                shell.update(cx, |shell, cx| {
                    shell.focus_active_surface(window, cx);
                    shell.capture_command_target(window, cx).unwrap()
                })
            })
            .unwrap();
        cx.update(|cx| {
            let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
            dispatcher.update(cx, |dispatcher, cx| {
                dispatcher.dispatch(
                    Command {
                        name: name.into(),
                        arguments: CommandArgumentValue::Null,
                    },
                    target,
                    cx,
                )
            })
        })
    }

    fn install_extension(root: &Path, name: &str, requires: &[&str], files: &[(&str, &str)]) {
        let directory = root.join(name);
        std::fs::create_dir_all(&directory).unwrap();
        let main = files[0].0;
        std::fs::write(
            directory.join("knot.jsonc"),
            serde_json::json!({
                "name": name,
                "apiVersion": 1, "version": "1.0.0",
                "main": main,
                "requires": requires,
            })
            .to_string(),
        )
        .unwrap();
        for (path, source) in files {
            let destination = directory.join(path);
            std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
            std::fs::write(destination, source).unwrap();
        }
    }

    fn wait_for_extension_report(
        _host: &Entity<super::super::extension_host::ProductExtensionHost>,
        cx: &mut TestAppContext,
    ) -> super::super::extension_host::StartupReportSnapshot {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            cx.run_until_parked();
            if let Some(report) = cx.read(super::super::extension_host::startup_report)
                && report.state == super::super::extension_host::StartupState::Complete
            {
                return report;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "extension startup did not finish"
            );
            std::thread::yield_now();
        }
    }

    #[gpui::test]
    fn personal_config_gates_product_windows_and_keeps_extension_failures_independent(
        cx: &mut TestAppContext,
    ) {
        let config = tempfile::tempdir().unwrap();
        let extensions = tempfile::tempdir().unwrap();
        std::fs::write(
            config.path().join("pre-init.js"),
            "import * as knot from 'knot'; const { commands } = knot; await commands.register('config.pre', () => {});",
        )
        .unwrap();
        std::fs::write(
            config.path().join("post-init.js"),
            "import * as knot from 'knot'; const { commands } = knot; await commands.register('config.post', () => {});",
        )
        .unwrap();
        install_extension(
            extensions.path(),
            "@example/good",
            &[],
            &[(
                "main.js",
                "import * as knot from 'knot'; const { commands } = knot; await commands.register('extension.good', () => {});",
            )],
        );
        install_extension(
            extensions.path(),
            "@example/bad",
            &[],
            &[("main.js", "throw new Error('package failed');")],
        );
        install_globals(cx);
        install_launch_gate(cx);
        let host = cx.update(super::super::extension_host::install);
        cx.update(|cx| super::dispatch_launch_request(None, cx));
        assert!(cx.windows().is_empty());
        host.update(cx, |host, cx| {
            host.start_product_startup(
                Some(config.path().to_path_buf()),
                Ok(extensions.path().to_path_buf()),
                cx,
            )
        });
        assert!(wait_for_product_startup(cx));
        assert_eq!(cx.windows().len(), 1);
        let report = cx
            .read(super::super::extension_host::startup_report)
            .unwrap();
        assert!(
            report
                .report
                .entries
                .iter()
                .any(|entry| entry.name.as_deref() == Some("@example/good")
                    && entry.result == super::super::extension_load::LoadResult::Loaded)
        );
        assert!(report.report.entries.iter().any(|entry| entry.name.as_deref() == Some("@example/bad") && matches!(&entry.result, super::super::extension_load::LoadResult::Failed(cause) if cause.contains("package failed"))));
        let names = cx.read(|cx| {
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .definitions()
                .map(|definition| definition.name.to_string())
                .collect::<Vec<_>>()
        });
        for name in ["config.pre", "extension.good", "config.post"] {
            assert!(names.contains(&name.to_owned()), "missing {name}");
        }
        cx.update(|cx| super::dispatch_launch_request(None, cx));
        assert_eq!(cx.windows().len(), 2);
    }

    #[gpui::test]
    fn script_keybindings_follow_config_phases_and_runtime_replacement(cx: &mut TestAppContext) {
        let config = tempfile::tempdir().unwrap();
        let extensions = tempfile::tempdir().unwrap();
        std::fs::write(
            config.path().join("pre-init.js"),
            "import * as knot from 'knot'; knot.keybinding('cmd-n', 'file.open');",
        )
        .unwrap();
        std::fs::write(
            config.path().join("post-init.js"),
            "import * as knot from 'knot'; knot.keybinding('cmd-n', 'file.save-as'); knot.keybinding('cmd-x', null); knot.keybinding('cmd-j', { name: 'test.args', arguments: { marker: 42 } });",
        )
        .unwrap();
        install_extension(
            extensions.path(),
            "@example/good",
            &[],
            &[(
                "main.js",
                r#"
                    import * as knot from 'knot';
                    knot.keybinding('cmd-n', 'file.save');
                    knot.keybinding('cmd-r', 'file.open');
                    await knot.commands.register('test.remap', () => {
                        knot.keybinding('cmd-r', 'file.new');
                        knot.removeKeybinding('cmd-n');
                    });
                    await knot.commands.register('test.unbind', () => knot.keybinding('cmd-r', null, { view: 'editor' }));
                    await knot.commands.register('test.restore', () => knot.removeKeybinding('cmd-r', { view: 'editor' }));
                    await knot.commands.register('test.remove', () => knot.removeKeybinding('cmd-r'));
                    await knot.commands.register('test.args', ({ arguments: args }) => {
                        if (args.marker !== 42) throw new Error('wrong binding arguments');
                    });
                    await knot.commands.register('test.invalid', () => {
                        knot.keybinding('cmd-r', 'file.save');
                        knot.keybinding('bad-modifier-key', 'file.new');
                    });
                "#,
            )],
        );
        install_extension(
            extensions.path(),
            "@example/bad",
            &[],
            &[(
                "main.js",
                "import * as knot from 'knot'; knot.keybinding('cmd-l', 'file.new'); await knot.workbench.writeClipboardText('startup'); throw new Error('failed after binding');",
            )],
        );
        install_globals(cx);
        cx.update(super::bind_product_keys);
        install_launch_gate(cx);
        let host = cx.update(super::super::extension_host::install);
        host.update(cx, |host, cx| {
            host.start_product_startup(
                Some(config.path().to_path_buf()),
                Ok(extensions.path().to_path_buf()),
                cx,
            )
        });
        assert!(wait_for_product_startup(cx));
        cx.read(|cx| {
            assert_eq!(keymap_command(cx, "cmd-n", Some("editor")).as_deref(), Some("file.save-as"));
            assert_eq!(keymap_command(cx, "cmd-r", Some("editor")).as_deref(), Some("file.open"));
            assert_eq!(keymap_command(cx, "cmd-x", Some("editor")).as_deref(), Some("<unbound>"));
            assert_eq!(keymap_command(cx, "cmd-l", Some("editor")), None);
            assert!(matches!(
                cx.global::<ApplicationKeymaps>().0.resolve(&gpui::Keystroke::parse("cmd-j").unwrap().unparse(), Some("editor")),
                Some(BindingResolution::Command(Command { arguments: CommandArgumentValue::Object(arguments), .. }))
                    if arguments.get("marker") == Some(&CommandArgumentValue::Number(42.0))
            ));
        });
        let report = cx
            .read(super::super::extension_host::startup_report)
            .unwrap();
        assert!(report.report.entries.iter().any(|entry| {
            entry.name.as_deref() == Some("@example/bad")
                && matches!(&entry.result, super::super::extension_load::LoadResult::Failed(cause) if cause.contains("failed after binding"))
        }));
        assert!(cx.read(|cx| {
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .definitions()
                .any(|definition| definition.name.as_ref() == "test.args")
        }));
        let documents = cx.read(|cx| cx.global::<ApplicationDocuments>().0.clone());
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window) = product_window(document, model, cx);
        let target = cx
            .update_window(window, |_, window, cx| {
                shell.update(cx, |shell, cx| {
                    shell.focus_active_surface(window, cx);
                    shell.capture_command_target(window, cx).unwrap()
                })
            })
            .unwrap();
        let command = cx.read(|cx| {
            let Some(BindingResolution::Command(command)) =
                cx.global::<ApplicationKeymaps>().0.resolve(
                    &gpui::Keystroke::parse("cmd-j").unwrap().unparse(),
                    Some("editor"),
                )
            else {
                panic!("expected command binding");
            };
            command
        });
        let execution = cx.update(|cx| {
            let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
            dispatcher.update(cx, |dispatcher, cx| {
                dispatcher.dispatch(command, target, cx)
            })
        });
        assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
        let execution = dispatch_product_command(&shell, window, "test.remap", cx);
        assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
        cx.read(|cx| {
            assert_eq!(
                keymap_command(cx, "cmd-r", Some("editor")).as_deref(),
                Some("file.new")
            );
            assert_eq!(
                keymap_command(cx, "cmd-n", Some("editor")).as_deref(),
                Some("file.save-as")
            );
        });
        let invalid = wait_for_command(
            dispatch_product_command(&shell, window, "test.invalid", cx),
            cx,
        );
        assert!(
            matches!(invalid, CommandOutcome::HandlerFailure { message } if message.contains("keybinding operation 2"))
        );
        cx.read(|cx| {
            assert_eq!(
                keymap_command(cx, "cmd-r", Some("editor")).as_deref(),
                Some("file.new")
            )
        });
        assert_eq!(
            wait_for_command(
                dispatch_product_command(&shell, window, "test.unbind", cx),
                cx
            ),
            CommandOutcome::Completed
        );
        cx.read(|cx| {
            assert_eq!(
                keymap_command(cx, "cmd-r", Some("editor")).as_deref(),
                Some("<unbound>")
            );
            assert_eq!(
                keymap_command(cx, "cmd-r", Some("terminal")).as_deref(),
                Some("file.new")
            );
        });
        assert_eq!(
            wait_for_command(
                dispatch_product_command(&shell, window, "test.restore", cx),
                cx
            ),
            CommandOutcome::Completed
        );
        cx.read(|cx| {
            assert_eq!(
                keymap_command(cx, "cmd-r", Some("editor")).as_deref(),
                Some("file.new")
            )
        });
        assert_eq!(
            wait_for_command(
                dispatch_product_command(&shell, window, "test.remove", cx),
                cx
            ),
            CommandOutcome::Completed
        );
        cx.read(|cx| assert_eq!(keymap_command(cx, "cmd-r", Some("editor")), None));
    }

    #[gpui::test]
    fn config_remaps_product_keys_without_changing_menu_or_palette_dispatch(
        cx: &mut TestAppContext,
    ) {
        let config = tempfile::tempdir().unwrap();
        let extensions = tempfile::tempdir().unwrap();
        std::fs::write(
            config.path().join("pre-init.js"),
            "import * as knot from 'knot'; knot.keybinding('cmd-n', null); knot.keybinding('cmd-g', 'file.new');",
        )
        .unwrap();
        std::fs::write(
            config.path().join("post-init.js"),
            "import * as knot from 'knot'; knot.keybinding('cmd-r', 'test.mark');",
        )
        .unwrap();
        install_extension(
            extensions.path(),
            "@example/mark",
            &[],
            &[(
                "main.js",
                "import * as knot from 'knot'; await knot.commands.register('test.mark', () => knot.keybinding('cmd-y', 'file.new'));",
            )],
        );
        install_globals(cx);
        cx.update(super::bind_product_keys);
        install_launch_gate(cx);
        let host = cx.update(super::super::extension_host::install);
        host.update(cx, |host, cx| {
            host.start_product_startup(
                Some(config.path().to_path_buf()),
                Ok(extensions.path().to_path_buf()),
                cx,
            )
        });
        assert!(wait_for_product_startup(cx));
        let window = cx.windows()[0];
        let shell = cx
            .update_window(window, |_, window, _| {
                window.root::<ProductShell>().flatten()
            })
            .unwrap()
            .unwrap();
        cx.update_window(window, |_, window, cx| {
            shell.read(cx).focus_active_editor(window, cx)
        })
        .unwrap();
        cx.refresh().unwrap();

        cx.simulate_keystrokes(window, "cmd-n");
        cx.run_until_parked();
        cx.read(|cx| {
            assert_eq!(
                shell
                    .read(cx)
                    .workbench
                    .read(cx)
                    .focused_pane()
                    .unwrap()
                    .tabs()
                    .len(),
                1
            )
        });
        cx.simulate_keystrokes(window, "cmd-g");
        cx.run_until_parked();
        cx.read(|cx| {
            assert_eq!(
                shell
                    .read(cx)
                    .workbench
                    .read(cx)
                    .focused_pane()
                    .unwrap()
                    .tabs()
                    .len(),
                2
            )
        });

        cx.simulate_keystrokes(window, "cmd-r");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            cx.run_until_parked();
            if cx.read(|cx| {
                keymap_command(cx, "cmd-y", Some("editor")).as_deref() == Some("file.new")
            }) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "config binding did not invoke the extension command"
            );
            std::thread::yield_now();
        }

        cx.dispatch_action(window, ProductCommandSource::new(NEW_COMMAND));
        cx.read(|cx| {
            assert_eq!(
                shell
                    .read(cx)
                    .workbench
                    .read(cx)
                    .focused_pane()
                    .unwrap()
                    .tabs()
                    .len(),
                3
            )
        });
        cx.update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| shell.open_command_palette(window, cx));
        })
        .unwrap();
        cx.refresh().unwrap();
        cx.simulate_keystrokes(window, "f i l e . n e w enter");
        cx.run_until_parked();
        cx.read(|cx| {
            assert_eq!(
                shell
                    .read(cx)
                    .workbench
                    .read(cx)
                    .focused_pane()
                    .unwrap()
                    .tabs()
                    .len(),
                4
            )
        });
    }

    #[gpui::test]
    fn config_global_copy_binding_routes_to_editor_and_extension_view(cx: &mut TestAppContext) {
        use gpui::EntityInputHandler;

        let config = tempfile::tempdir().unwrap();
        let extensions = tempfile::tempdir().unwrap();
        std::fs::write(
            config.path().join("post-init.js"),
            "import * as knot from 'knot'; knot.keybinding('cmd-j', 'copy');",
        )
        .unwrap();
        install_extension(
            extensions.path(),
            "@example/outline-copy",
            &[],
            &[(
                "main.js",
                r#"
                    import * as knot from 'knot';
                    await knot.workbench.registerView({
                        id: 'example.outline',
                        title: 'Outline',
                        content: { kind: 'tree', provider: {
                            getChildren(parentId) {
                                return parentId === null
                                    ? [{ id: 'first', label: 'Tree item', collapsibleState: 'none' }]
                                    : [];
                            },
                        } },
                    });
                    await knot.commands.registerForView('example.outline', 'copy', async () => {
                        const text = await knot.workbench.selectedText();
                        await knot.workbench.writeClipboardText(text);
                    });
                    knot.keybinding('cmd-j', 'file.new', { view: 'example.outline' });
                "#,
            )],
        );
        let documents = install_globals(cx);
        cx.update(super::bind_product_keys);
        install_launch_gate(cx);
        let host = cx.update(super::super::extension_host::install);
        host.update(cx, |host, cx| {
            host.start_product_startup(
                Some(config.path().to_path_buf()),
                Ok(extensions.path().to_path_buf()),
                cx,
            )
        });
        assert!(wait_for_product_startup(cx));
        cx.read(|cx| {
            assert_eq!(
                keymap_command(cx, "cmd-j", Some("editor")).as_deref(),
                Some("copy")
            );
            assert_eq!(
                keymap_command(cx, "cmd-j", Some("example.outline")).as_deref(),
                Some("copy")
            );
        });
        let window = cx.windows()[0];
        let shell = cx
            .update_window(window, |_, window, _| {
                window.root::<ProductShell>().flatten()
            })
            .unwrap()
            .unwrap();
        let tree = wait_for_contributed_view(&shell, "example.outline", cx);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            cx.run_until_parked();
            if cx.read(|cx| tree.read(cx).root_labels() == ["Tree item"]) {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "outline did not load");
            std::thread::yield_now();
        }

        cx.update_window(window, |_, window, cx| {
            let editor = shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .active_tab()
                .editor()
                .unwrap()
                .clone();
            editor.update(cx, |editor, cx| {
                editor.replace_text_in_range(None, "Editor selection", window, cx);
                editor.execute_editing_command("editor.select-all", window, cx);
            });
            editor.focus_handle(cx).focus(window);
        })
        .unwrap();
        cx.refresh().unwrap();
        cx.simulate_keystrokes(window, "cmd-j");
        cx.run_until_parked();
        cx.read(|cx| {
            assert_eq!(
                cx.read_from_clipboard()
                    .and_then(|item| item.text())
                    .as_deref(),
                Some("Editor selection")
            );
        });

        cx.update_window(window, |_, window, cx| {
            tree.update(cx, |tree, cx| assert!(tree.select_item("first", cx)));
            tree.focus_handle(cx).focus(window);
        })
        .unwrap();
        cx.update(|cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string("untouched".into())));
        cx.refresh().unwrap();
        cx.simulate_keystrokes(window, "cmd-j");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            cx.run_until_parked();
            if cx.read(|cx| {
                cx.read_from_clipboard()
                    .and_then(|item| item.text())
                    .as_deref()
                    == Some("Tree item")
            }) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "global copy binding did not route to the outline"
            );
            std::thread::yield_now();
        }
        cx.read(|cx| assert_eq!(documents.read(cx).documents().count(), 1));
    }

    #[gpui::test]
    fn invalid_config_keybinding_batch_rolls_back_before_product_readiness(
        cx: &mut TestAppContext,
    ) {
        let config = tempfile::tempdir().unwrap();
        let extensions = tempfile::tempdir().unwrap();
        std::fs::write(
            config.path().join("pre-init.js"),
            "import * as knot from 'knot'; knot.keybinding('cmd-r', 'file.open'); knot.keybinding('bad-modifier-key', 'file.new');",
        )
        .unwrap();
        install_globals(cx);
        cx.update(super::bind_product_keys);
        install_launch_gate(cx);
        let host = cx.update(super::super::extension_host::install);
        host.update(cx, |host, cx| {
            host.start_product_startup(
                Some(config.path().to_path_buf()),
                Ok(extensions.path().to_path_buf()),
                cx,
            )
        });
        assert!(!wait_for_product_startup(cx));
        cx.read(|cx| {
            assert_eq!(keymap_command(cx, "cmd-r", Some("editor")), None);
            assert_eq!(host.read(cx).lifecycle_count(), 0);
            assert!(cx.global::<ApplicationWorkbenches>().0.borrow().is_empty());
        });
        assert_eq!(cx.windows().len(), 1);
    }

    #[gpui::test]
    fn invalid_extension_keybinding_batch_does_not_block_other_extensions(cx: &mut TestAppContext) {
        let extensions = tempfile::tempdir().unwrap();
        install_extension(
            extensions.path(),
            "@example/good",
            &[],
            &[(
                "main.js",
                "import * as knot from 'knot'; knot.keybinding('cmd-r', 'file.new');",
            )],
        );
        install_extension(
            extensions.path(),
            "@example/bad",
            &[],
            &[(
                "main.js",
                "import * as knot from 'knot'; knot.keybinding('cmd-l', 'file.new'); knot.keybinding('bad-modifier-key', 'file.open');",
            )],
        );
        install_globals(cx);
        cx.update(super::bind_product_keys);
        install_launch_gate(cx);
        let host = cx.update(super::super::extension_host::install);
        host.update(cx, |host, cx| {
            host.start_product_startup(None, Ok(extensions.path().to_path_buf()), cx)
        });
        assert!(wait_for_product_startup(cx));
        let report = cx
            .read(super::super::extension_host::startup_report)
            .unwrap();
        assert!(report.report.entries.iter().any(|entry| {
            entry.name.as_deref() == Some("@example/good")
                && entry.result == super::super::extension_load::LoadResult::Loaded
        }));
        assert!(report.report.entries.iter().any(|entry| {
            entry.name.as_deref() == Some("@example/bad")
                && matches!(&entry.result, super::super::extension_load::LoadResult::Failed(cause) if cause.contains("keybinding operation 2"))
        }));
        cx.read(|cx| {
            assert_eq!(
                keymap_command(cx, "cmd-r", Some("editor")).as_deref(),
                Some("file.new")
            );
            assert_eq!(keymap_command(cx, "cmd-l", Some("editor")), None);
        });
    }

    #[gpui::test]
    fn unloading_a_script_lifecycle_removes_its_keybindings(cx: &mut TestAppContext) {
        use crate::host::lifecycle::ExtensionKey;

        install_globals(cx);
        cx.update(super::bind_product_keys);
        let host = cx.update(super::super::extension_host::install);
        let key = ExtensionKey::new(ExtensionId::new(91), ExtensionLifecycleId::new(1));
        let loaded = Arc::new(Mutex::new(false));
        let done = loaded.clone();
        host.update(cx, |host, cx| {
            let execution = host
                .load_test_source(
                    key,
                    "import * as knot from 'knot'; knot.keybinding('cmd-r', 'file.open');",
                )
                .unwrap();
            cx.spawn(async move |_, _| {
                execution.await.unwrap();
                *done.lock().unwrap() = true;
            })
            .detach();
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !*loaded.lock().unwrap() {
            cx.run_until_parked();
            assert!(
                std::time::Instant::now() < deadline,
                "extension keybinding did not load"
            );
            std::thread::yield_now();
        }
        cx.read(|cx| {
            assert_eq!(
                keymap_command(cx, "cmd-r", None).as_deref(),
                Some("file.open")
            )
        });
        host.update(cx, |host, cx| host.unload(key, cx).unwrap());
        cx.read(|cx| assert_eq!(keymap_command(cx, "cmd-r", None), None));
    }

    #[gpui::test]
    fn queued_file_open_runs_only_after_configured_startup(cx: &mut TestAppContext) {
        let config = tempfile::tempdir().unwrap();
        let extensions = tempfile::tempdir().unwrap();
        let file = config.path().join("queued.txt");
        std::fs::write(&file, "queued contents").unwrap();
        let request = OpenRequest::from_path(&file, config.path()).unwrap();
        let uri = request.uri().clone();
        let documents = install_globals(cx);
        install_launch_gate(cx);
        let host = cx.update(super::super::extension_host::install);
        cx.update(|cx| super::dispatch_launch_request(Some(request), cx));
        assert!(cx.windows().is_empty());
        host.update(cx, |host, cx| {
            host.start_product_startup(
                Some(config.path().to_path_buf()),
                Ok(extensions.path().to_path_buf()),
                cx,
            )
        });
        assert!(wait_for_product_startup(cx));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            cx.run_until_parked();
            let text = cx.read(|cx| {
                let documents = documents.read(cx);
                documents
                    .document_for_resource(&uri)
                    .and_then(|id| documents.get(id))
                    .map(|document| document.model().read(cx).text().to_owned())
            });
            if let Some(text) = text {
                assert_eq!(text, "queued contents");
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "queued file did not open"
            );
            std::thread::yield_now();
        }
    }

    #[gpui::test]
    fn pre_init_failure_skips_extensions_and_opens_only_the_error_window(cx: &mut TestAppContext) {
        let config = tempfile::tempdir().unwrap();
        let extensions = tempfile::tempdir().unwrap();
        std::fs::write(config.path().join("pre-init.js"), "import * as knot from 'knot'; const { commands } = knot; await commands.register('config.before-failure', () => {}); throw new Error('pre failed');").unwrap();
        install_extension(
            extensions.path(),
            "@example/skipped",
            &[],
            &[("main.js", "throw new Error('extension ran');")],
        );
        install_globals(cx);
        install_launch_gate(cx);
        let host = cx.update(super::super::extension_host::install);
        let queued = OpenRequest::from_uri_for_product(
            ResourceUri::parse("file:///never-open-on-failure.txt").unwrap(),
        );
        cx.update(|cx| super::dispatch_launch_request(Some(queued.clone()), cx));
        assert!(cx.windows().is_empty());
        host.update(cx, |host, cx| {
            host.start_product_startup(
                Some(config.path().to_path_buf()),
                Ok(extensions.path().to_path_buf()),
                cx,
            )
        });
        assert!(!wait_for_product_startup(cx));
        assert_eq!(cx.windows().len(), 1);
        assert!(
            cx.read(super::super::extension_host::startup_report)
                .is_none()
        );
        assert!(cx.read(|cx| cx.global::<ApplicationWorkbenches>().0.borrow().is_empty()));
        assert_eq!(cx.read(|cx| host.read(cx).lifecycle_count()), 0);
        let names = cx.read(|cx| {
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .definitions()
                .map(|definition| definition.name.to_string())
                .collect::<Vec<_>>()
        });
        assert!(!names.contains(&"config.before-failure".to_owned()));
        cx.update(|cx| super::dispatch_launch_request(Some(queued), cx));
        assert_eq!(cx.windows().len(), 1);
    }

    #[gpui::test]
    fn invalid_config_import_prevents_extension_discovery(cx: &mut TestAppContext) {
        let config = tempfile::tempdir().unwrap();
        let extensions = tempfile::tempdir().unwrap();
        std::fs::write(config.path().join("pre-init.js"), "import './missing.js';").unwrap();
        install_extension(
            extensions.path(),
            "@example/skipped",
            &[],
            &[("main.js", "throw new Error('extension ran');")],
        );
        install_globals(cx);
        install_launch_gate(cx);
        let host = cx.update(super::super::extension_host::install);
        host.update(cx, |host, cx| {
            host.start_product_startup(
                Some(config.path().to_path_buf()),
                Ok(extensions.path().to_path_buf()),
                cx,
            )
        });
        assert!(!wait_for_product_startup(cx));
        assert_eq!(cx.windows().len(), 1);
        assert!(
            cx.read(super::super::extension_host::startup_report)
                .is_none()
        );
        assert_eq!(cx.read(|cx| host.read(cx).lifecycle_count()), 0);
    }

    #[gpui::test]
    fn post_init_failure_unloads_config_and_installed_extensions(cx: &mut TestAppContext) {
        let config = tempfile::tempdir().unwrap();
        let extensions = tempfile::tempdir().unwrap();
        std::fs::write(
            config.path().join("pre-init.js"),
            "import * as knot from 'knot'; const { commands, editor } = knot; await commands.register('config.before', () => {}); await editor.registerCompletionProvider('config-provider', { provideCompletions() { return []; } });",
        )
        .unwrap();
        std::fs::write(
            config.path().join("post-init.js"),
            "throw new Error('post failed');",
        )
        .unwrap();
        install_extension(
            extensions.path(),
            "@example/loaded",
            &[],
            &[(
                "main.js",
                "import * as knot from 'knot'; const { commands } = knot; await commands.register('extension.before', () => {});",
            )],
        );
        install_globals(cx);
        install_launch_gate(cx);
        let host = cx.update(super::super::extension_host::install);
        host.update(cx, |host, cx| {
            host.start_product_startup(
                Some(config.path().to_path_buf()),
                Ok(extensions.path().to_path_buf()),
                cx,
            )
        });
        assert!(!wait_for_product_startup(cx));
        assert_eq!(cx.windows().len(), 1);
        assert!(cx.read(|cx| cx.global::<ApplicationWorkbenches>().0.borrow().is_empty()));
        let report = cx
            .read(super::super::extension_host::startup_report)
            .unwrap();
        assert_eq!(
            report.report.entries[0].result,
            super::super::extension_load::LoadResult::Loaded
        );
        assert_eq!(cx.read(|cx| host.read(cx).lifecycle_count()), 0);
        let names = cx.read(|cx| {
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .definitions()
                .map(|definition| definition.name.to_string())
                .collect::<Vec<_>>()
        });
        assert!(!names.contains(&"config.before".to_owned()));
        assert!(!names.contains(&"extension.before".to_owned()));
        assert_eq!(
            cx.read(super::super::extension_host::completion_provider_count),
            0
        );
    }

    #[gpui::test]
    fn post_init_validation_failure_waits_for_installed_extensions(cx: &mut TestAppContext) {
        let config = tempfile::tempdir().unwrap();
        let extensions = tempfile::tempdir().unwrap();
        std::fs::write(
            config.path().join("pre-init.js"),
            "globalThis.preRan = true;",
        )
        .unwrap();
        std::fs::write(config.path().join("post-init.js"), "import './missing.js';").unwrap();
        install_extension(
            extensions.path(),
            "@example/loaded",
            &[],
            &[("main.js", "export {};")],
        );
        install_globals(cx);
        install_launch_gate(cx);
        let host = cx.update(super::super::extension_host::install);
        host.update(cx, |host, cx| {
            host.start_product_startup(
                Some(config.path().to_path_buf()),
                Ok(extensions.path().to_path_buf()),
                cx,
            )
        });
        assert!(!wait_for_product_startup(cx));
        let report = cx
            .read(super::super::extension_host::startup_report)
            .unwrap();
        assert_eq!(
            report.report.entries[0].result,
            super::super::extension_load::LoadResult::Loaded
        );
        assert_eq!(cx.read(|cx| host.read(cx).lifecycle_count()), 0);
        assert!(cx.read(|cx| cx.global::<ApplicationWorkbenches>().0.borrow().is_empty()));
    }

    #[gpui::test]
    fn installed_api_version_rejection_prevents_evaluation_and_preserves_dependency_diagnostics(
        cx: &mut TestAppContext,
    ) {
        let root = tempfile::tempdir().unwrap();
        const PROBE: &str = "import {commands, workbench} from 'knot'; await workbench.writeClipboardText('rejected entry evaluated'); await commands.register('rejected-entry', () => {});";
        for (suffix, version) in [
            ("missing", None),
            ("null", Some(serde_json::Value::Null)),
            ("string", Some(serde_json::json!("1"))),
            ("boolean", Some(serde_json::json!(true))),
            ("zero", Some(serde_json::json!(0))),
            ("fraction", Some(serde_json::json!(1.5))),
            ("unsupported", Some(serde_json::json!(2))),
        ] {
            let name = format!("@example/api-{suffix}");
            install_extension(root.path(), &name, &[], &[("main.js", PROBE)]);
            let manifest_path = root.path().join(&name).join("knot.jsonc");
            let mut manifest: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
            match version {
                Some(version) => manifest["apiVersion"] = version,
                None => {
                    manifest.as_object_mut().unwrap().remove("apiVersion");
                }
            }
            std::fs::write(manifest_path, manifest.to_string()).unwrap();
            install_extension(
                root.path(),
                &format!("@example/dependent-{suffix}"),
                &[&name],
                &[("main.js", PROBE)],
            );
        }
        install_extension(
            root.path(),
            "@example/duplicate",
            &[],
            &[("main.js", PROBE)],
        );
        install_extension(
            root.path(),
            "@example/duplicate-peer",
            &[],
            &[("main.js", PROBE)],
        );
        let duplicate_path = root.path().join("@example/duplicate-peer/knot.jsonc");
        std::fs::write(
            duplicate_path,
            r#"{"name":"@example/duplicate","apiVersion":2,"version":"1.0.0","main":"main.js"}"#,
        )
        .unwrap();
        install_extension(
            root.path(),
            "@example/independent",
            &[],
            &[(
                "main.js",
                "import {commands} from 'knot'; await commands.register('admission.independent', () => {});",
            )],
        );
        let documents = install_globals(cx);
        let host = cx.update(super::super::extension_host::install);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (_shell, _window) = product_window(document, model, cx);
        cx.update(|cx| {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string("admission sentinel".into()))
        });
        host.update(cx, |host, cx| {
            host.start_installed_extensions(root.path().to_path_buf(), cx)
        });
        let report = wait_for_extension_report(&host, cx);
        assert_eq!(report.report.entries.len(), 17);
        for suffix in [
            "missing",
            "null",
            "string",
            "boolean",
            "zero",
            "fraction",
            "unsupported",
        ] {
            let rejected = format!("@example/api-{suffix}");
            let dependent = format!("@example/dependent-{suffix}");
            for name in [&rejected, &dependent] {
                let entry = report
                    .report
                    .entries
                    .iter()
                    .find(|entry| entry.name.as_ref() == Some(name))
                    .unwrap();
                assert!(
                    matches!(&entry.result, super::super::extension_load::LoadResult::Failed(cause) if cause.contains("apiVersion") && cause.contains("expected") && cause.contains("declared")),
                    "{entry:?}"
                );
            }
        }
        let duplicates = report
            .report
            .entries
            .iter()
            .filter(|entry| entry.name.as_deref() == Some("@example/duplicate"))
            .collect::<Vec<_>>();
        assert_eq!(duplicates.len(), 2);
        assert!(duplicates.iter().all(|entry| matches!(&entry.result, super::super::extension_load::LoadResult::Failed(cause) if cause.contains("duplicate package ID"))));
        assert!(duplicates.iter().any(|entry| matches!(&entry.result, super::super::extension_load::LoadResult::Failed(cause) if cause.contains("unsupported apiVersion"))));
        cx.read(|cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "admission sentinel"
            );
            assert!(
                cx.global::<ApplicationProductCommands>()
                    .0
                    .read(cx)
                    .catalog()
                    .borrow()
                    .resolve("rejected-entry")
                    .is_err()
            );
            assert!(
                cx.global::<ApplicationProductCommands>()
                    .0
                    .read(cx)
                    .catalog()
                    .borrow()
                    .resolve("admission.independent")
                    .is_ok()
            );
            assert_eq!(host.read(cx).lifecycle_count(), 1);
        });
    }

    #[gpui::test]
    fn checked_authoring_examples_run_as_an_installed_version_one_package(cx: &mut TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        install_extension(
            root.path(),
            "@example/guide",
            &[],
            &[
                (
                    "main.js",
                    "import {deactivateFeature} from './activation.js'; import './errors.js'; import {commands} from 'knot'; await commands.register('guide.test-deactivate', deactivateFeature);",
                ),
                (
                    "activation.js",
                    include_str!("../../scripts/extension-api/examples/activation.js"),
                ),
                (
                    "errors.js",
                    include_str!("../../scripts/extension-api/examples/errors.js"),
                ),
            ],
        );
        let documents = install_globals(cx);
        cx.update(super::bind_product_keys);
        let original_binding = cx.read(|cx| keymap_command(cx, "cmd-g", Some("editor")));
        let host = cx.update(super::super::extension_host::install);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        model.update(cx, |model, _| model.replace(0..0, "text").unwrap());
        let (shell, window) = product_window(document, model.clone(), cx);
        host.update(cx, |host, cx| {
            host.start_installed_extensions(root.path().to_path_buf(), cx)
        });
        let report = wait_for_extension_report(&host, cx);
        assert_eq!(
            report.report.entries[0].result,
            super::super::extension_load::LoadResult::Loaded
        );
        cx.read(|cx| {
            assert_eq!(
                keymap_command(cx, "cmd-g", Some("editor")).as_deref(),
                Some("guide.compose")
            )
        });
        let annotate = dispatch_product_command(&shell, window, "guide.compose", cx);
        assert_eq!(wait_for_command(annotate, cx), CommandOutcome::Completed);
        cx.read(|cx| assert_eq!(model.read(cx).resolved_contributions().len(), 1));
        let invalid = dispatch_product_command(&shell, window, "guide.insert", cx);
        assert!(matches!(
            wait_for_command(invalid, cx),
            CommandOutcome::InvalidArgument { .. }
        ));
        let target = cx
            .update_window(window, |_, window, cx| {
                shell.update(cx, |shell, cx| {
                    shell.focus_active_surface(window, cx);
                    shell.capture_command_target(window, cx).unwrap()
                })
            })
            .unwrap();
        let dispatcher = cx.read(|cx| cx.global::<ApplicationProductCommands>().0.clone());
        let insertion = dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: "guide.insert".into(),
                    arguments: CommandArgumentValue::String("!".into()),
                },
                target,
                cx,
            )
        });
        assert_eq!(wait_for_command(insertion, cx), CommandOutcome::Completed);
        cx.read(|cx| assert_eq!(model.read(cx).text(), "!text"));
        let native_child = dispatch_product_command(&shell, window, "guide.select-all", cx);
        assert_eq!(
            wait_for_command(native_child, cx),
            CommandOutcome::Completed
        );
        let native_copy = dispatch_product_command(&shell, window, super::COPY_COMMAND, cx);
        assert_eq!(wait_for_command(native_copy, cx), CommandOutcome::Completed);
        cx.read(|cx| assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), "!text"));
        let tree = wait_for_contributed_view(&shell, "guide.outline", cx);
        wait_for_api_condition(cx, |cx| tree.read(cx).root_labels() == ["Guide"]);
        let copy_editor = cx.read(|cx| {
            shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .and_then(|pane| pane.active_tab().editor().cloned())
                .expect("document editor is focused before the tree")
        });
        wait_for_api_condition(cx, |cx| {
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .catalog()
                .borrow()
                .resolve_view(tree.read(cx).instance_id(), "copy")
                .is_some()
        });
        cx.update_window(window, |_, window, cx| {
            tree.update(cx, |tree, cx| assert!(tree.select_item("root", cx)));
            tree.focus_handle(cx).focus(window);
        })
        .unwrap();
        let copy = dispatch_tree_copy(&shell, window, cx);
        assert_eq!(wait_for_command(copy, cx), CommandOutcome::Completed);
        cx.read(|cx| assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), "Guide"));
        wait_for_api_condition(cx, |cx| {
            super::super::extension_host::completion_provider_count(cx) == 1
        });
        cx.update_window(window, |_, window, cx| {
            copy_editor.read(cx).focus_handle(cx).focus(window);
            shell.update(cx, |shell, cx| shell.sync_focused_pane(window, cx));
        })
        .unwrap();
        let completion =
            dispatch_product_command(&shell, window, super::SHOW_COMPLETIONS_COMMAND, cx);
        assert_eq!(wait_for_command(completion, cx), CommandOutcome::Completed);
        let editor = cx.read(|cx| {
            shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .active_tab()
                .editor()
                .unwrap()
                .clone()
        });
        wait_for_api_condition(cx, |cx| {
            editor
                .read(cx)
                .completion_state()
                .is_some_and(|(items, pending, failures, _, _)| {
                    items == 1 && pending == 0 && failures == 0
                })
        });
        let cleanup = dispatch_product_command(&shell, window, "guide.test-deactivate", cx);
        assert_eq!(wait_for_command(cleanup, cx), CommandOutcome::Completed);
        cx.read(|cx| {
            assert!(!tree.read(cx).lifecycle_state().0);
            assert!(model.read(cx).resolved_contributions().is_empty());
            assert_eq!(
                keymap_command(cx, "cmd-g", Some("editor")),
                original_binding
            );
            assert_eq!(
                super::super::extension_host::completion_provider_count(cx),
                0
            );
            assert!(
                cx.global::<ApplicationProductCommands>()
                    .0
                    .read(cx)
                    .catalog()
                    .borrow()
                    .resolve("guide.compose")
                    .is_err()
            );
        });
    }

    #[gpui::test]
    fn on_disk_extensions_load_in_order_invoke_dependencies_and_isolate_failures(
        cx: &mut TestAppContext,
    ) {
        let root = tempfile::tempdir().unwrap();
        install_extension(
            root.path(),
            "@example/base",
            &[],
            &[
                (
                    "dist/main.js",
                    r#"
                import * as knot from 'knot'; const { commands, editor, workbench } = knot;
                import { prefix } from './words.js';
                await commands.register('example.base', async ({ buffer }) => {
                    const snapshot = await buffer.snapshot();
                    await buffer.applyEdits([{
                        range: { startByteOffset: 0, endByteOffset: 0 },
                        text: prefix,
                    }], { ifRevision: snapshot.revision });
                });
                await editor.registerCompletionProvider('example-provider', {
                    provideCompletions() { return [{ label: 'Installed', insertText: 'Installed' }]; }
                });
            "#,
                ),
                ("dist/words.js", "export const prefix = 'base ';"),
            ],
        );
        install_extension(
            root.path(),
            "@example/dependent",
            &["@example/base"],
            &[(
                "main.js",
                r#"
                import * as knot from 'knot'; const { commands } = knot;
                await commands.register('example.dependent', async () => {
                    await commands.invoke('example.base', null);
                });
            "#,
            )],
        );
        install_extension(
            root.path(),
            "@example/broken",
            &[],
            &[(
                "main.js",
                r#"
                import * as knot from 'knot'; const { commands, editor, workbench } = knot;
                await commands.register('example.shared', async () => {});
                await editor.registerCompletionProvider('broken-provider', {
                    provideCompletions() { return []; }
                });
                await workbench.registerView({
                    id: 'example.broken.outline',
                    title: 'Broken outline',
                    content: { kind: 'tree', provider: { getChildren() { return []; } } },
                });
                await commands.registerForView('example.broken.outline', 'copy', async () => {});
                throw new Error('broken entry');
            "#,
            )],
        );
        install_extension(
            root.path(),
            "@example/blocked",
            &["@example/broken"],
            &[("main.js", "throw new Error('must not run');")],
        );
        install_extension(
            root.path(),
            "@example/independent",
            &[],
            &[(
                "main.js",
                r#"
                import * as knot from 'knot'; const { commands } = knot;
                await commands.register('example.shared', async () => {});
            "#,
            )],
        );
        let invalid = root.path().join("@example/invalid");
        std::fs::create_dir_all(&invalid).unwrap();
        std::fs::write(invalid.join("knot.jsonc"), "{ invalid").unwrap();

        let documents = install_globals(cx);
        let host = cx.update(super::super::extension_host::install);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window) = product_window(document, model.clone(), cx);
        host.update(cx, |host, cx| {
            host.start_installed_extensions(root.path().to_path_buf(), cx)
        });
        let report = wait_for_extension_report(&host, cx);
        cx.read(|cx| assert!(shell.read(cx).contributed_views.is_empty()));
        let result = |name| {
            report
                .report
                .entries
                .iter()
                .find(|entry| entry.name.as_deref() == Some(name))
                .unwrap()
                .result
                .clone()
        };
        assert_eq!(
            result("@example/base"),
            super::super::extension_load::LoadResult::Loaded
        );
        assert_eq!(
            result("@example/dependent"),
            super::super::extension_load::LoadResult::Loaded
        );
        assert_eq!(
            result("@example/independent"),
            super::super::extension_load::LoadResult::Loaded
        );
        assert!(
            matches!(result("@example/broken"), super::super::extension_load::LoadResult::Failed(cause) if cause.contains("broken entry"))
        );
        assert!(
            matches!(result("@example/blocked"), super::super::extension_load::LoadResult::Failed(cause) if cause.contains("@example/broken"))
        );
        assert!(report.report.entries.iter().any(|entry| entry.directory == invalid && matches!(&entry.result, super::super::extension_load::LoadResult::Failed(cause) if cause.contains("invalid knot.jsonc"))));
        let definitions = cx.read(|cx| {
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .definitions()
                .map(|definition| definition.name.to_string())
                .collect::<Vec<_>>()
        });
        assert!(definitions.contains(&"example.base".to_owned()));
        assert!(definitions.contains(&"example.dependent".to_owned()));
        assert!(definitions.contains(&"example.shared".to_owned()));
        assert_eq!(
            definitions
                .iter()
                .filter(|name| name.as_str() == "example.shared")
                .count(),
            1
        );
        cx.read(|cx| assert!(shell.read(cx).contributed_views.is_empty()));
        let mut execution = dispatch_product_command(&shell, window, "example.dependent", cx);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            cx.run_until_parked();
            match execution.completion.try_recv() {
                Ok(outcome) => {
                    assert_eq!(outcome, CommandOutcome::Completed);
                    break;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "dependent command did not finish"
                    );
                    std::thread::yield_now();
                }
                Err(error) => panic!("dependent command completion closed: {error}"),
            }
        }
        assert_eq!(cx.read(|cx| model.read(cx).text()), "base ");
        assert_eq!(
            cx.read(super::super::extension_host::completion_provider_count),
            1
        );

        let mut report_command =
            dispatch_product_command(&shell, window, SHOW_EXTENSION_REPORT_COMMAND, cx);
        cx.run_until_parked();
        assert_eq!(
            report_command.completion.try_recv().unwrap(),
            CommandOutcome::Completed
        );
        assert!(cx.read(|cx| shell.read(cx).extension_report_open));
    }

    #[gpui::test]
    fn extension_startup_shows_known_outcomes_while_a_later_entry_is_pending(
        cx: &mut TestAppContext,
    ) {
        let root = tempfile::tempdir().unwrap();
        install_extension(
            root.path(),
            "@example/a-ready",
            &[],
            &[("main.js", "export {};")],
        );
        install_extension(
            root.path(),
            "@example/z-pending",
            &[],
            &[("main.js", "await new Promise(() => {});")],
        );
        let invalid = root.path().join("@example/invalid");
        std::fs::create_dir_all(&invalid).unwrap();
        std::fs::write(invalid.join("knot.jsonc"), "{ invalid").unwrap();

        install_globals(cx);
        let host = cx.update(super::super::extension_host::install);
        host.update(cx, |host, cx| {
            host.start_installed_extensions(root.path().to_path_buf(), cx)
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            cx.run_until_parked();
            let report = cx
                .read(super::super::extension_host::startup_report)
                .unwrap();
            if report.report.entries.len() == 2 {
                assert_eq!(
                    report.state,
                    super::super::extension_host::StartupState::Loading
                );
                assert_eq!(report.report.entries[0].directory, invalid);
                assert!(matches!(
                    &report.report.entries[0].result,
                    super::super::extension_load::LoadResult::Failed(cause)
                        if cause.contains("invalid knot.jsonc")
                ));
                assert_eq!(
                    report.report.entries[1].name.as_deref(),
                    Some("@example/a-ready")
                );
                assert_eq!(
                    report.report.entries[1].result,
                    super::super::extension_load::LoadResult::Loaded
                );
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "startup outcomes were not published"
            );
            std::thread::yield_now();
        }
    }

    #[gpui::test]
    fn extension_startup_rescans_changed_directories_and_cleans_old_registrations(
        cx: &mut TestAppContext,
    ) {
        let root = tempfile::tempdir().unwrap();
        install_extension(
            root.path(),
            "@example/old",
            &[],
            &[(
                "main.js",
                r#"
                import * as knot from 'knot'; const { commands, editor } = knot;
                await commands.register('example.old', async () => {});
                await editor.registerCompletionProvider('old-provider', {
                    provideCompletions() { return []; }
                });
            "#,
            )],
        );
        install_globals(cx);
        let host = cx.update(super::super::extension_host::install);
        host.update(cx, |host, cx| {
            host.start_installed_extensions(root.path().to_path_buf(), cx)
        });
        let first = wait_for_extension_report(&host, cx);
        assert_eq!(first.report.entries.len(), 1);
        assert_eq!(
            first.report.entries[0].name.as_deref(),
            Some("@example/old")
        );
        assert_eq!(
            cx.read(super::super::extension_host::completion_provider_count),
            1
        );

        std::fs::remove_dir_all(root.path().join("@example/old")).unwrap();
        install_extension(
            root.path(),
            "@example/new",
            &[],
            &[(
                "main.js",
                r#"
                import * as knot from 'knot'; const { commands } = knot;
                await commands.register('example.new', async () => {});
            "#,
            )],
        );
        host.update(cx, |host, cx| {
            host.start_installed_extensions(root.path().to_path_buf(), cx)
        });
        let second = wait_for_extension_report(&host, cx);
        assert_eq!(second.report.entries.len(), 1);
        assert_eq!(
            second.report.entries[0].name.as_deref(),
            Some("@example/new")
        );
        assert_eq!(
            second.report.entries[0].result,
            super::super::extension_load::LoadResult::Loaded
        );
        assert_eq!(
            cx.read(super::super::extension_host::completion_provider_count),
            0
        );
        let definitions = cx.read(|cx| {
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .definitions()
                .map(|definition| definition.name.to_string())
                .collect::<Vec<_>>()
        });
        assert!(definitions.contains(&"example.new".to_owned()));
        assert!(!definitions.contains(&"example.old".to_owned()));
    }

    #[gpui::test]
    fn checked_in_extension_example_runs_through_the_product_command_bridge(
        cx: &mut TestAppContext,
    ) {
        let storage = tempfile::tempdir().unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/extensions");
        let documents = install_globals(cx);
        let host = cx.update(super::super::extension_host::install);
        host.update(cx, |host, cx| {
            host.set_test_storage_root(storage.path().to_owned(), cx)
        });
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window) = product_window(document, model.clone(), cx);
        host.update(cx, |host, cx| {
            host.start_installed_extensions(root.clone(), cx)
        });
        let report = wait_for_extension_report(&host, cx);
        assert_eq!(report.report.entries.len(), 5);
        assert!(
            report
                .report
                .entries
                .iter()
                .all(|entry| entry.result == super::super::extension_load::LoadResult::Loaded)
        );
        let mut execution = dispatch_product_command(&shell, window, "example.greet", cx);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            cx.run_until_parked();
            match execution.completion.try_recv() {
                Ok(outcome) => {
                    assert_eq!(outcome, CommandOutcome::Completed);
                    break;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "example command did not finish"
                    );
                    std::thread::yield_now();
                }
                Err(error) => panic!("example command completion closed: {error}"),
            }
        }
        assert_eq!(cx.read(|cx| model.read(cx).text()), "Hello ");
        for activations in 1..=2 {
            if activations == 2 {
                host.update(cx, |host, cx| {
                    host.start_installed_extensions(root.clone(), cx)
                });
                let report = wait_for_extension_report(&host, cx);
                assert!(
                    report
                        .report
                        .entries
                        .iter()
                        .all(|entry| entry.result
                            == super::super::extension_load::LoadResult::Loaded)
                );
            }
            let execution = dispatch_product_command(&shell, window, "example.storage-report", cx);
            assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
            let report: serde_json::Value = cx.read(|cx| {
                serde_json::from_str(&cx.read_from_clipboard().unwrap().text().unwrap()).unwrap()
            });
            assert_eq!(report["activations"]["found"], true);
            assert_eq!(
                report["activations"]["value"].as_f64(),
                Some(f64::from(activations))
            );
            assert_eq!(
                report["example"]["value"]["optional"],
                serde_json::Value::Null
            );
            assert_eq!(report["missing"], serde_json::json!({"found": false}));
            assert_eq!(
                report["keys"],
                serde_json::json!(["activations", "example"])
            );
        }
    }

    struct FixedOpenDialog(OpenDialogOutcome);

    impl ProductOpenDialog for FixedOpenDialog {
        fn select(&self) -> OpenDialogFuture {
            let outcome = self.0.clone();
            Box::pin(async move { outcome })
        }
    }

    struct FixedSaveDialog(SaveDialogOutcome);

    impl ProductSaveDialog for FixedSaveDialog {
        fn select(&self, _: gpui::SharedString) -> SaveDialogFuture {
            let outcome = self.0.clone();
            Box::pin(async move { outcome })
        }
    }

    #[gpui::test]
    fn product_catalog_registers_every_slice_command(cx: &mut TestAppContext) {
        install_globals(cx);
        cx.read(|cx| {
            let dispatcher = cx.global::<ApplicationProductCommands>().0.read(cx);
            let mut actual = dispatcher
                .definitions()
                .map(|definition| definition.name.to_string())
                .collect::<Vec<_>>();
            let mut expected = super::super::product_commands::product_command_names()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            actual.sort();
            expected.sort();
            assert_eq!(actual, expected);
        });
    }

    #[gpui::test]
    fn pooled_fixture_commands_and_completions_use_the_product_target(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window) = product_window(document, model.clone(), cx);
        let host = cx.update(|cx| {
            let host = super::super::extension_host::install(cx);
            host.update(cx, |host, cx| host.start_test_fixture("product", cx));
            host
        });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            cx.run_until_parked();
            let registered = cx.read(|cx| {
                cx.global::<ApplicationProductCommands>()
                    .0
                    .read(cx)
                    .definitions()
                    .any(|definition| definition.name.as_ref() == "knot.fixture.primary")
            });
            if registered {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "fixture command did not enter the product catalog"
            );
            std::thread::yield_now();
        }

        let mut execution = dispatch_product_command(&shell, window, "knot.fixture.primary", cx);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let outcome = loop {
            cx.run_until_parked();
            match execution.completion.try_recv() {
                Ok(outcome) => break outcome,
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    assert!(std::time::Instant::now() < deadline);
                    std::thread::yield_now();
                }
                Err(error) => panic!("fixture command completion closed: {error}"),
            }
        };
        assert_eq!(outcome, CommandOutcome::Completed);
        assert_eq!(cx.read(|cx| model.read(cx).text()), "fixture ");

        let mut completion = dispatch_product_command(
            &shell,
            window,
            super::super::product_commands::SHOW_COMPLETIONS_COMMAND,
            cx,
        );
        cx.run_until_parked();
        assert_eq!(
            completion.completion.try_recv().unwrap(),
            CommandOutcome::Completed
        );

        let editor = cx.read(|cx| {
            shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .active_tab()
                .editor()
                .unwrap()
                .clone()
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            cx.run_until_parked();
            if cx.read(|cx| {
                editor.read(cx).completion_state().is_some_and(
                    |(items, pending, failures, _, _)| items == 2 && pending == 0 && failures == 0,
                )
            }) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "fixture completion items did not reach the product editor"
            );
            std::thread::yield_now();
        }

        drop(host);
        cx.update(|cx| {
            drop(cx.remove_global::<super::super::extension_host::ApplicationExtensionHost>())
        });
        cx.run_until_parked();
    }

    fn wait_for_command(
        mut execution: crate::app::CommandExecution,
        cx: &mut TestAppContext,
    ) -> CommandOutcome {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            cx.run_until_parked();
            match execution.completion.try_recv() {
                Ok(outcome) => return outcome,
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "command did not settle"
                    );
                    std::thread::yield_now();
                }
                Err(error) => panic!("command completion closed: {error}"),
            }
        }
    }

    fn wait_for_contributed_view(
        shell: &Entity<ProductShell>,
        id: &str,
        cx: &mut TestAppContext,
    ) -> Entity<crate::app::tree_view::TreeView> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            cx.run_until_parked();
            if let Some(view) = cx.read(|cx| {
                shell
                    .read(cx)
                    .contributed_views
                    .get(id)
                    .map(|view| view.tree().clone())
            }) {
                return view;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "contributed view did not attach"
            );
            std::thread::yield_now();
        }
    }

    fn dispatch_tree_command(
        shell: &Entity<ProductShell>,
        window: gpui::AnyWindowHandle,
        name: &'static str,
        cx: &mut TestAppContext,
    ) -> crate::app::CommandExecution {
        let target = cx
            .update_window(window, |_, window, cx| {
                shell.update(cx, |shell, cx| {
                    shell.capture_command_target(window, cx).unwrap()
                })
            })
            .unwrap();
        let dispatcher = cx.read(|cx| cx.global::<ApplicationProductCommands>().0.clone());
        dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: name.into(),
                    arguments: CommandArgumentValue::Null,
                },
                target,
                cx,
            )
        })
    }

    fn dispatch_tree_copy(
        shell: &Entity<ProductShell>,
        window: gpui::AnyWindowHandle,
        cx: &mut TestAppContext,
    ) -> crate::app::CommandExecution {
        dispatch_tree_command(shell, window, super::COPY_COMMAND, cx)
    }

    fn wait_for_api_condition(cx: &mut TestAppContext, mut ready: impl FnMut(&gpui::App) -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            cx.run_until_parked();
            if cx.read(&mut ready) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "public API fixture did not settle"
            );
            std::thread::yield_now();
        }
    }

    #[gpui::test]
    fn public_api_capture_survives_focus_and_nested_handlers_without_leaking_to_callbacks(
        cx: &mut TestAppContext,
    ) {
        use crate::host::lifecycle::ExtensionKey;
        const SOURCE: &str = r#"
import { commands, editor, workbench } from "knot";
async function expectCode(operation, code) {
  try { await operation(); } catch (error) { if (error.code === code) return; throw error; }
  throw new Error(`expected ${code}`);
}
await expectCode(() => editor.activeBuffer(), "ContextUnavailable");
await expectCode(() => workbench.selectedText(), "ContextUnavailable");
await expectCode(() => commands.invoke("file.new"), "ContextUnavailable");
await workbench.writeClipboardText("activation");
let origin;
let previous;
await workbench.registerView({
  id: "example.outline",
  title: "Context-free",
  content: { kind: "tree", provider: {
    async getChildren(parentId) {
      await expectCode(() => editor.activeBuffer(), "ContextUnavailable");
      await expectCode(() => workbench.selectedText(), "ContextUnavailable");
      return parentId === null ? [{ id: "provider", label: "context-free", collapsibleState: "none" }] : [];
    },
  } },
});
await editor.registerCompletionProvider("context-free", {
  async provideCompletions(context) {
    await expectCode(() => editor.activeBuffer(), "ContextUnavailable");
    await expectCode(() => workbench.selectedText(), "ContextUnavailable");
    await expectCode(() => editor.activeBuffer(origin.target), "InvalidTarget");
    await context.buffer.snapshot();
    return [{ label: "context-free", insertText: "context-free" }];
  },
});
await commands.register("api.inline", async (context) => {
  if (context.buffer !== origin.buffer || context.target.window !== origin.target.window) throw new Error("inline capture");
  if (await editor.activeBuffer(origin.target) !== origin.buffer) throw new Error("inherited authority");
});
await commands.register("api.capture", async (context) => {
  if (!Object.isFrozen(context.target) || typeof context.target.window !== "string") throw new Error("capture shape");
  if (previous) await expectCode(() => editor.activeBuffer(previous), "InvalidTarget");
  previous = context.target;
  origin = context;
  await expectCode(() => editor.activeBuffer({ ...context.target }), "InvalidTarget");
  await expectCode(() => workbench.selectedText(context.target), "UnsupportedOperation");
  await workbench.writeClipboardText("awaiting");
  if (await editor.activeBuffer() !== context.buffer || await editor.activeBuffer(context.target) !== context.buffer) throw new Error("focus redirected capture");
  const snapshot = await context.buffer.snapshot();
  if (snapshot.text !== "first") throw new Error(`wrong buffer: ${snapshot.text}`);
  if ((await commands.invoke("api.inline")).kind !== "completed") throw new Error("inline failed");
  if ((await commands.invoke("api.cross", { window: context.target.window })).kind !== "completed") throw new Error("cross failed");
  await context.buffer.onDidChange(async (change) => {
    await expectCode(() => editor.activeBuffer(), "ContextUnavailable");
    await expectCode(() => editor.activeBuffer(context.target), "InvalidTarget");
    await expectCode(() => workbench.selectedText(), "ContextUnavailable");
    await workbench.writeClipboardText((await change.buffer.snapshot()).text);
  });
});
"#;
        const CROSS: &str = r#"
import { commands, editor, workbench } from "knot";
await commands.register("api.cross", async (context) => {
  if (context.target.window !== context.arguments.window || await editor.activeBuffer(context.target) !== context.buffer) throw new Error("cross capture");
  await workbench.writeClipboardText((await context.buffer.snapshot()).text);
});
"#;
        let documents = install_globals(cx);
        let host = cx.update(super::super::extension_host::install);
        let first_document = cx.update(|cx| create_untitled_document(&documents, cx));
        let first_model = cx.read(|cx| {
            documents
                .read(cx)
                .get(first_document)
                .unwrap()
                .model()
                .clone()
        });
        first_model.update(cx, |model, _| model.replace(0..0, "first").unwrap());
        let (first_shell, first_window) = product_window(first_document, first_model.clone(), cx);
        let second_document = cx.update(|cx| create_untitled_document(&documents, cx));
        let second_model = cx.read(|cx| {
            documents
                .read(cx)
                .get(second_document)
                .unwrap()
                .model()
                .clone()
        });
        second_model.update(cx, |model, _| model.replace(0..0, "second").unwrap());
        let (second_shell, second_window) = product_window(second_document, second_model, cx);
        for (id, source) in [(701, SOURCE), (702, CROSS)] {
            host.update(cx, |host, cx| {
                let execution = host
                    .load_test_source(
                        ExtensionKey::new(ExtensionId::new(id), ExtensionLifecycleId::new(1)),
                        source,
                    )
                    .unwrap();
                cx.spawn(async move |_, _| {
                    execution.await.unwrap();
                })
                .detach();
            });
        }
        wait_for_api_condition(cx, |cx| {
            let catalog = cx
                .global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .catalog();
            let catalog = catalog.borrow();
            catalog.resolve("api.capture").is_ok() && catalog.resolve("api.cross").is_ok()
        });
        wait_for_api_condition(cx, |cx| {
            first_shell
                .read(cx)
                .contributed_views
                .get("example.outline")
                .is_some_and(|view| view.tree().read(cx).root_labels() == ["context-free"])
        });
        // Capture before the palette takes focus, then admit the saved origin.
        let origin = cx
            .update_window(first_window, |_, window, cx| {
                first_shell.update(cx, |shell, cx| {
                    shell.focus_active_surface(window, cx);
                    shell.open_command_palette(window, cx);
                    shell.command_palette.as_ref().unwrap().read(cx).origin()
                })
            })
            .unwrap();
        for _ in 0..2 {
            host.update(cx, |host, _| host.hold_clipboard_writes());
            let dispatcher = cx.read(|cx| cx.global::<ApplicationProductCommands>().0.clone());
            let execution = dispatcher.update(cx, |dispatcher, cx| {
                dispatcher.dispatch(
                    Command {
                        name: "api.capture".into(),
                        arguments: CommandArgumentValue::Null,
                    },
                    origin.clone(),
                    cx,
                )
            });
            wait_for_api_condition(cx, |cx| host.read(cx).pending_view_request_count() == 1);
            cx.update_window(second_window, |_, window, cx| {
                second_shell.read(cx).focus_active_surface(window, cx)
            })
            .unwrap();
            host.update(cx, |host, cx| host.release_view_requests(cx));
            assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
            cx.read(|cx| assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), "first"));
        }
        let editor = cx.read(|cx| {
            first_shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .active_tab()
                .editor()
                .unwrap()
                .clone()
        });
        let completion = dispatch_product_command(
            &first_shell,
            first_window,
            super::SHOW_COMPLETIONS_COMMAND,
            cx,
        );
        assert_eq!(wait_for_command(completion, cx), CommandOutcome::Completed);
        wait_for_api_condition(cx, |cx| {
            editor
                .read(cx)
                .completion_state()
                .is_some_and(|(items, pending, failures, _, _)| {
                    items == 1 && pending == 0 && failures == 0
                })
        });
        let (_shared_shell, _shared_window) =
            product_window(first_document, first_model.clone(), cx);
        cx.update_window(first_window, |_, window, _| window.remove_window())
            .unwrap();
        drop(first_shell);
        first_model.update(cx, |model, cx| {
            model.replace(0..5, "shared callback").unwrap();
            cx.notify();
        });
        wait_for_api_condition(cx, |cx| {
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .as_deref()
                == Some("shared callback")
        });
    }

    #[gpui::test]
    fn public_api_context_rejects_replaced_closed_and_transferred_presentations(
        cx: &mut TestAppContext,
    ) {
        use crate::host::lifecycle::ExtensionKey;
        const SOURCE: &str = r#"
import { commands, editor, workbench } from "knot";
await commands.register("api.stale", async (context) => {
  await workbench.writeClipboardText("waiting");
  for (const operation of [() => editor.activeBuffer(), () => editor.activeBuffer(context.target), () => workbench.selectedText(context.target)]) {
    let rejected = false;
    try { await operation(); } catch (error) { if (error.code !== "InvalidTarget") throw error; rejected = true; }
    if (!rejected) throw new Error("stale presentation accepted");
  }
  // An explicit live buffer remains usable after its presentation changes.
  await context.buffer.snapshot();
  await workbench.writeClipboardText("InvalidTarget");
});
await commands.register("api.nontext", async (context) => {
  if (context.buffer !== null || await editor.activeBuffer() !== null || await editor.activeBuffer(context.target) !== null) throw new Error("nontext buffer");
  try { await workbench.selectedText(); } catch (error) { if (error.code === "UnsupportedOperation") return; throw error; }
  throw new Error("unsupported selection accepted");
});
"#;
        let documents = install_globals(cx);
        let host = cx.update(super::super::extension_host::install);
        host.update(cx, |host, cx| {
            let execution = host
                .load_test_source(
                    ExtensionKey::new(ExtensionId::new(703), ExtensionLifecycleId::new(1)),
                    SOURCE,
                )
                .unwrap();
            cx.spawn(async move |_, _| {
                execution.await.unwrap();
            })
            .detach();
        });
        wait_for_api_condition(cx, |cx| {
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .catalog()
                .borrow()
                .resolve("api.nontext")
                .is_ok()
        });
        for change in ["replace", "close", "transfer"] {
            let document = cx.update(|cx| create_untitled_document(&documents, cx));
            let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
            let (shell, window) = product_window(document, model.clone(), cx);
            host.update(cx, |host, _| host.hold_clipboard_writes());
            let execution = dispatch_product_command(&shell, window, "api.stale", cx);
            wait_for_api_condition(cx, |cx| host.read(cx).pending_view_request_count() == 1);
            let target = cx
                .update_window(window, |_, window, cx| {
                    shell.update(cx, |shell, cx| {
                        shell.capture_command_target(window, cx).unwrap()
                    })
                })
                .unwrap();
            match change {
                "replace" => {
                    let workbench = cx.read(|cx| shell.read(cx).workbench.clone());
                    workbench.update(cx, |workbench, cx| {
                        assert!(workbench.replace_tab_document(
                            target.pane,
                            target.tab,
                            document,
                            model.clone(),
                            cx
                        ))
                    });
                }
                "close" => {
                    cx.update_window(window, |_, window, _| window.remove_window())
                        .unwrap();
                }
                "transfer" => {
                    let destination_document =
                        cx.update(|cx| create_untitled_document(&documents, cx));
                    let destination_model = cx.read(|cx| {
                        documents
                            .read(cx)
                            .get(destination_document)
                            .unwrap()
                            .model()
                            .clone()
                    });
                    let (destination_shell, destination_window) =
                        product_window(destination_document, destination_model, cx);
                    let source = cx.read(|cx| {
                        shell
                            .read(cx)
                            .capture_tab_relocation(target.pane, target.tab, cx)
                            .unwrap()
                    });
                    let destination = cx.read(|cx| {
                        let workbench = &destination_shell.read(cx).workbench;
                        let pane = workbench.read(cx).focused_pane().unwrap();
                        super::relocate::TabRelocationDestination {
                            window: destination_window,
                            workbench: workbench.downgrade(),
                            tab: crate::app::workbench::TabMoveDestination::At {
                                anchor: workbench
                                    .read(cx)
                                    .capture_tab_move(pane.id(), pane.active_tab().id())
                                    .unwrap(),
                                placement: crate::app::workbench::TabMovePlacement::End,
                            },
                        }
                    });
                    let outcome = cx
                        .update_window(window, |_, window, cx| {
                            shell.update(cx, |shell, cx| {
                                shell.relocate_tab_to(&source, &destination, window, cx)
                            })
                        })
                        .unwrap();
                    assert_eq!(outcome, CommandOutcome::Completed);
                }
                _ => unreachable!(),
            }
            host.update(cx, |host, cx| host.release_view_requests(cx));
            assert_eq!(
                wait_for_command(execution, cx),
                CommandOutcome::InvalidTarget
            );
            cx.read(|cx| {
                assert_eq!(
                    cx.read_from_clipboard().unwrap().text().unwrap(),
                    "InvalidTarget"
                )
            });
        }
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window) = product_window(document, model, cx);
        cx.update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                let (session, view) = shell.create_terminal(cx);
                let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
                let tab = shell.workbench.update(cx, |workbench, _| {
                    workbench.open_terminal_tab(pane, session, view).unwrap()
                });
                shell.activate_tab(pane, tab, window, cx);
            })
        })
        .unwrap();
        let execution = dispatch_product_command(&shell, window, "api.nontext", cx);
        assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
    }

    #[gpui::test]
    fn installed_application_observer_tracks_zero_window_activation_and_native_commits(
        cx: &mut TestAppContext,
    ) {
        fn report(
            shell: &Entity<ProductShell>,
            window: gpui::AnyWindowHandle,
            cx: &mut TestAppContext,
        ) -> serde_json::Value {
            let execution =
                dispatch_product_command(shell, window, "example.application-state", cx);
            assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
            cx.read(|cx| {
                serde_json::from_str(&cx.read_from_clipboard().unwrap().text().unwrap()).unwrap()
            })
        }
        let root = tempfile::tempdir().unwrap();
        install_extension(
            root.path(),
            "@example/application-state",
            &[],
            &[(
                "main.js",
                include_str!("../../examples/extensions/@example/application-state/dist/main.js"),
            )],
        );
        install_extension(
            root.path(),
            "@example/broken-observer",
            &[],
            &[(
                "main.js",
                "import {workbench} from 'knot'; await workbench.observe(() => { throw new Error('failed activation listener ran'); }); throw new Error('activation failed');",
            )],
        );
        let documents = install_globals(cx);
        let host = cx.update(super::super::extension_host::install);
        host.update(cx, |host, cx| {
            host.start_installed_extensions(root.path().to_path_buf(), cx)
        });
        let startup = wait_for_extension_report(&host, cx);
        assert_eq!(
            startup.report.entries[0].result,
            super::super::extension_load::LoadResult::Loaded
        );
        assert!(matches!(
            startup.report.entries[1].result,
            super::super::extension_load::LoadResult::Failed(_)
        ));
        cx.read(|cx| assert_eq!(host.read(cx).application_observer_count(), 1));
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let id = cx.read(|cx| {
            documents
                .read(cx)
                .get(document)
                .unwrap()
                .identity()
                .to_string()
        });
        let (first, first_window) = product_window(document, model.clone(), cx);
        cx.update_window(first_window, |_, window, cx| {
            window.activate_window();
            first.read(cx).focus_active_editor(window, cx);
        })
        .unwrap();
        let initial = report(&first, first_window, cx);
        assert_eq!(initial["initial"]["documents"], 0);
        assert_eq!(initial["initial"]["windows"], 0);
        assert_eq!(initial["documents"].as_array().unwrap().len(), 1);
        assert!(
            initial["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|event| event["type"] == "documentOpened" && event["document"] == id)
        );
        model.update(cx, |model, cx| {
            model.replace(0..0, "edit").unwrap();
            cx.notify();
        });
        let edited = report(&first, first_window, cx);
        assert_eq!(edited["documents"][0]["dirty"], true);
        assert_eq!(edited["documents"][0]["revision"], 1);
        model.update(cx, |model, cx| {
            model.undo().unwrap();
            cx.notify();
        });
        let split =
            dispatch_product_command(&first, first_window, super::SPLIT_HORIZONTAL_COMMAND, cx);
        assert_eq!(wait_for_command(split, cx), CommandOutcome::Completed);
        let (second, second_window) = product_window(document, model.clone(), cx);
        first.update(cx, |shell, cx| {
            shell.install_workspace(ResourceUri::parse("mem://observer/").unwrap(), cx)
        });
        let source = cx.read(|cx| {
            let shell = first.read(cx);
            let pane = shell.workbench.read(cx).focused_pane().unwrap();
            shell
                .capture_tab_relocation(pane.id(), pane.active_tab_id(), cx)
                .unwrap()
        });
        let destination = cx.read(|cx| {
            let workbench = &second.read(cx).workbench;
            let pane = workbench.read(cx).focused_pane().unwrap();
            super::relocate::TabRelocationDestination {
                window: second_window,
                workbench: workbench.downgrade(),
                tab: crate::app::workbench::TabMoveDestination::At {
                    anchor: workbench
                        .read(cx)
                        .capture_tab_move(pane.id(), pane.active_tab_id())
                        .unwrap(),
                    placement: crate::app::workbench::TabMovePlacement::End,
                },
            }
        });
        assert_eq!(
            cx.update_window(first_window, |_, window, cx| first.update(
                cx,
                |shell, cx| shell.relocate_tab_to(&source, &destination, window, cx)
            ))
            .unwrap(),
            CommandOutcome::Completed
        );
        let updated = report(&first, first_window, cx);
        let mut tabs = updated["windows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|window| {
                window["panes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|pane| pane["tabs"].as_array().unwrap().len())
                    .sum::<usize>()
            })
            .collect::<Vec<_>>();
        tabs.sort();
        assert_eq!(tabs, [2, 2]);
        assert_eq!(updated["documents"][0]["dirty"], false);
        assert_eq!(updated["documents"][0]["revision"], 2);
        assert_eq!(updated["windows"].as_array().unwrap().len(), 2);
        let events = updated["events"].as_array().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event["type"] == "documentOpened" && event["document"] == id)
                .count(),
            1
        );
        for kind in [
            "documentChanged",
            "windowOpened",
            "presentationsChanged",
            "workspaceChanged",
            "activeTargetChanged",
        ] {
            assert!(
                events.iter().any(|event| event["type"] == kind),
                "missing {kind}: {events:?}"
            );
        }
        let mut previous = updated["initial"]["watermark"].as_u64().unwrap();
        for event in events {
            let sequence = event["sequence"].as_u64().unwrap();
            assert!(sequence > previous);
            previous = sequence;
        }
        let installed_root = updated["windows"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|window| window["roots"].as_array().unwrap().first())
            .unwrap()["id"]
            .clone();
        first.update(cx, |shell, cx| {
            shell.install_workspace(ResourceUri::parse("mem://observer/").unwrap(), cx)
        });
        let retained = report(&first, first_window, cx);
        assert!(
            retained["windows"]
                .as_array()
                .unwrap()
                .iter()
                .any(|window| window["generation"] == 2
                    && window["roots"][0]["id"] == installed_root)
        );
        first.update(cx, |shell, cx| {
            shell.replace_workspace_roots(Vec::new(), cx).unwrap()
        });
        let cleared = report(&first, first_window, cx);
        assert!(
            cleared["windows"]
                .as_array()
                .unwrap()
                .iter()
                .all(|window| window["roots"].as_array().unwrap().is_empty())
        );
        let before_rejection = cleared["events"].as_array().unwrap().len();
        first.update(cx, |shell, cx| {
            assert!(
                shell
                    .replace_workspace_roots(
                        vec![
                            ResourceUri::parse("mem://one/").unwrap(),
                            ResourceUri::parse("mem://two/").unwrap()
                        ],
                        cx
                    )
                    .is_err()
            )
        });
        assert_eq!(
            report(&first, first_window, cx)["events"]
                .as_array()
                .unwrap()
                .len(),
            before_rejection
        );
        first.update(cx, |shell, cx| {
            shell.install_workspace(ResourceUri::parse("mem://observer/").unwrap(), cx)
        });
        let reinstalled = report(&first, first_window, cx);
        assert!(reinstalled["windows"].as_array().unwrap().iter().any(
            |window| window["generation"] == 4
                && window["roots"][0]["id"] != installed_root
                && !window["roots"].as_array().unwrap().is_empty()
        ));
        for (shell, window) in [(&second, second_window), (&first, first_window)] {
            let close = dispatch_product_command(shell, window, super::CLOSE_WINDOW_COMMAND, cx);
            assert_eq!(wait_for_command(close, cx), CommandOutcome::Completed);
        }
        let replacement = cx.update(|cx| create_untitled_document(&documents, cx));
        let replacement_model =
            cx.read(|cx| documents.read(cx).get(replacement).unwrap().model().clone());
        let (replacement_shell, replacement_window) =
            product_window(replacement, replacement_model.clone(), cx);
        let closed = report(&replacement_shell, replacement_window, cx);
        assert!(!cx.read(|cx| model.read(cx).is_open()));
        let events = closed["events"].as_array().unwrap();
        assert!(
            events
                .iter()
                .any(|event| event["type"] == "documentClosed" && event["document"] == id)
        );
        assert!(events.iter().any(|event| event["type"] == "windowClosed"));
        let stopped = dispatch_product_command(
            &replacement_shell,
            replacement_window,
            "example.stop-application-observer",
            cx,
        );
        assert_eq!(wait_for_command(stopped, cx), CommandOutcome::Completed);
        cx.read(|cx| assert_eq!(host.read(cx).application_observer_count(), 0));
        let stopped_count = report(&replacement_shell, replacement_window, cx)["events"]
            .as_array()
            .unwrap()
            .len();
        replacement_model.update(cx, |model, cx| {
            model.replace(0..0, "after disposal").unwrap();
            cx.notify();
        });
        assert_eq!(
            report(&replacement_shell, replacement_window, cx)["events"]
                .as_array()
                .unwrap()
                .len(),
            stopped_count
        );
    }

    #[gpui::test]
    fn application_listener_can_query_edit_fail_and_dispose_without_blocking_queued_roots(
        cx: &mut TestAppContext,
    ) {
        use crate::host::lifecycle::ExtensionKey;
        const SOURCE: &str = r#"
import { commands, documents, workbench } from "knot";
let count = 0, last = 0;
const failing = await workbench.observe(() => { throw new Error("isolated observer failure"); });
const observation = await workbench.observe(async (event) => {
  if (event.sequence <= observation.watermark || event.sequence <= last) throw new Error("observation order");
  last = event.sequence;
  if (event.type !== "documentChanged") return;
  count++;
  const current = await documents.get(event.document.id);
  const snapshot = await current.buffer.snapshot();
  await current.buffer.applyEdits([{ range: { startByteOffset: 0, endByteOffset: 0 }, text: "listener " }], { ifRevision: snapshot.revision });
  const disposal = observation.dispose();
  if (disposal !== observation.dispose()) throw new Error("disposal promise changed");
  await disposal;
});
await commands.register("observation.verify", async () => {
  if (count !== 1) throw new Error(`listener count ${count}`);
  if (!(await (await documents.list())[0].buffer.snapshot()).text.startsWith("listener ")) throw new Error("listener mutation missing");
  await failing.dispose();
});
await commands.register("observation.live", () => {});
await commands.register("observation.install-for-unload", async () => { await workbench.observe(() => {}); });
"#;
        let documents = install_globals(cx);
        let host = cx.update(super::super::extension_host::install);
        let key = ExtensionKey::new(ExtensionId::new(708), ExtensionLifecycleId::new(1));
        host.update(cx, |host, cx| {
            let execution = host.load_test_source(key, SOURCE).unwrap();
            cx.spawn(async move |_, _| {
                execution.await.unwrap();
            })
            .detach();
        });
        wait_for_api_condition(cx, |cx| {
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .catalog()
                .borrow()
                .resolve("observation.install-for-unload")
                .is_ok()
        });
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window) = product_window(document, model.clone(), cx);
        model.update(cx, |model, cx| {
            model.replace(0..0, "edit").unwrap();
            cx.notify();
        });
        let verify = dispatch_product_command(&shell, window, "observation.verify", cx);
        assert_eq!(wait_for_command(verify, cx), CommandOutcome::Completed);
        cx.read(|cx| assert_eq!(host.read(cx).application_observer_count(), 0));
        let install =
            dispatch_product_command(&shell, window, "observation.install-for-unload", cx);
        assert_eq!(wait_for_command(install, cx), CommandOutcome::Completed);
        cx.read(|cx| assert_eq!(host.read(cx).application_observer_count(), 1));
        host.update(cx, |host, cx| host.unload(key, cx).unwrap());
        cx.read(|cx| assert_eq!(host.read(cx).application_observer_count(), 0));
    }

    #[gpui::test]
    fn application_queries_use_live_product_state_and_distinct_target_authority(
        cx: &mut TestAppContext,
    ) {
        use crate::host::lifecycle::ExtensionKey;
        const SOURCE: &str = r#"
import { commands, documents, windows, workspaces, editor } from "knot";
if ((await documents.list()).length || (await windows.list()).length) throw new Error("startup catalog not empty");
let savedId, savedWindows;
await commands.register("queries.catalog", async ({ target }) => {
  const docs = await documents.list();
  const wins = await windows.list();
  if (docs.length !== 1 || wins.length !== 2) throw new Error("catalog scope/dedup");
  const doc = docs[0];
  if (doc.kind !== "untitled" || doc.title !== "Clean" || doc.dirty || doc.resourceUri !== null || doc.revision !== 0) throw new Error("document metadata");
  if ((await doc.buffer.snapshot()).text !== "owned") throw new Error("document buffer");
  if ((await documents.get(doc.id)).buffer !== doc.buffer) throw new Error("document lookup");
  if (await documents.get("missing") !== null) throw new Error("missing identity");
  for (const win of wins) {
    const pane = win.panes[0], tab = pane.tabs[0];
    if (tab.document !== doc.id || tab.buffer !== doc.buffer || tab.kind !== "document") throw new Error("presentation association");
    if (pane.selected !== tab.target || tab.target.pane !== pane.target || tab.target.window !== win.target) throw new Error("target identity");
    const again = await windows.get(win.target);
    if (again.panes[0].tabs[0].target !== tab.target) throw new Error("unstable target");
    const workspace = await workspaces.get(win.target);
    if (workspace.window !== win.target || workspace.roots.length !== 0 || workspace.generation !== 0) throw new Error("empty workspace");
    for (const value of [docs, doc, wins, win, pane, pane.tabs, tab, tab.target, workspace, workspace.roots]) {
      if (!Object.isFrozen(value)) throw new Error("mutable snapshot");
    }
  }
  for (const invalid of [target, {}, wins[0].panes[0].target, wins[0].panes[0].tabs[0].target]) {
    try { await windows.get(invalid); } catch (error) {
      if (error.code === "InvalidTarget") continue;
      throw error;
    }
    throw new Error("invalid live window accepted");
  }
  try { await editor.activeBuffer(wins[0].panes[0].tabs[0].target); throw new Error("live target gained invocation authority"); }
  catch (error) { if (error.code !== "InvalidTarget") throw error; }
  savedId = doc.id; savedWindows = wins.map(win => win.target);
});
await commands.register("queries.closed", async () => {
  if (await documents.get(savedId) !== null) throw new Error("closed document catalogued");
  const values = await Promise.all(savedWindows.map(target => windows.get(target)));
  if (values.filter(value => value === null).length !== 1) throw new Error("closed window catalogued");
  const closed = savedWindows[values.findIndex(value => value === null)];
  if (await workspaces.get(closed) !== null) throw new Error("closed workspace catalogued");
});
await commands.register("queries.terminal", async () => {
  const wins = await windows.list();
  const terminal = wins.flatMap(win => win.panes.flatMap(pane => pane.tabs)).find(tab => tab.kind === "terminal");
  if (!terminal || terminal.buffer !== null || terminal.document !== null) throw new Error("terminal pretends to be a document");
  if (!wins.some(win => win.currentTarget === terminal.target)) throw new Error("terminal focus missing");
});
"#;
        let documents = install_globals(cx);
        let host = cx.update(super::super::extension_host::install);
        let key = ExtensionKey::new(ExtensionId::new(707), ExtensionLifecycleId::new(1));
        host.update(cx, |host, cx| {
            let execution = host.load_test_source(key, SOURCE).unwrap();
            cx.spawn(async move |_, _| {
                execution.await.unwrap();
            })
            .detach();
        });
        wait_for_api_condition(cx, |cx| {
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .catalog()
                .borrow()
                .resolve("queries.terminal")
                .is_ok()
        });
        let model = cx.new(|_| BufferModel::from_text("owned"));
        let document = documents.update(cx, |documents, cx| {
            documents.create_untitled("Clean", model.clone(), cx)
        });
        let (first, first_window) = product_window(document, model.clone(), cx);
        let (second, second_window) = product_window(document, model, cx);
        let (_diagnostic, _) =
            cx.add_window_view(|_, cx| crate::app::tree_view::TreeView::new("diagnostic", "1", cx));
        for (shell, window, command) in [
            (&first, first_window, "queries.catalog"),
            (&first, first_window, CLOSE_TAB_COMMAND),
            (&second, second_window, super::CLOSE_WINDOW_COMMAND),
            (&first, first_window, "queries.closed"),
            (&first, first_window, NEW_TERMINAL_COMMAND),
            (&first, first_window, "queries.terminal"),
        ] {
            let execution = dispatch_product_command(shell, window, command, cx);
            assert_eq!(
                wait_for_command(execution, cx),
                CommandOutcome::Completed,
                "{command}"
            );
        }
        host.update(cx, |host, cx| host.unload(key, cx).unwrap());
    }

    #[gpui::test]
    fn final_document_close_invalidates_js_buffer_even_with_retained_editor(
        cx: &mut TestAppContext,
    ) {
        use crate::host::lifecycle::ExtensionKey;
        const SOURCE: &str = r#"
import { commands } from "knot";
let saved;
await commands.register("closure.setup", async ({ buffer }) => {
  saved = buffer;
  const snapshot = await buffer.snapshot();
  await buffer.contributions.replace([{ range: { startByteOffset: 0, endByteOffset: 1 }, decoration: "info" }], { ifRevision: snapshot.revision });
  await buffer.onDidChange(() => {});
});
await commands.register("closure.live", async () => {
  if ((await saved.snapshot()).text !== "owned") throw new Error("shared buffer lost");
});
await commands.register("closure.closed", async () => {
  for (const operation of [() => saved.snapshot(), () => saved.onDidChange(() => {}), () => saved.applyEdits([], { ifRevision: 0 }), () => saved.contributions.replace([], { ifRevision: 0 })]) {
    try { await operation(); } catch (error) {
      if (error.code === "BufferClosed") continue;
      throw error;
    }
    throw new Error("closed buffer accepted operation");
  }
});
"#;
        let documents = install_globals(cx);
        let host = cx.update(super::super::extension_host::install);
        let model = cx.new(|_| BufferModel::from_text("owned"));
        let document = documents.update(cx, |documents, cx| {
            documents.create_untitled("Clean", model.clone(), cx)
        });
        let (first, first_window) = product_window(document, model.clone(), cx);
        let (second, second_window) = product_window(document, model.clone(), cx);
        let retained_editor = cx.read(|cx| {
            first
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .active_tab()
                .editor()
                .unwrap()
                .clone()
        });
        let key = ExtensionKey::new(ExtensionId::new(706), ExtensionLifecycleId::new(1));
        host.update(cx, |host, cx| {
            let execution = host.load_test_source(key, SOURCE).unwrap();
            cx.spawn(async move |_, _| {
                execution.await.unwrap();
            })
            .detach();
        });
        wait_for_api_condition(cx, |cx| {
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .catalog()
                .borrow()
                .resolve("closure.closed")
                .is_ok()
        });
        let setup = dispatch_product_command(&first, first_window, "closure.setup", cx);
        assert_eq!(wait_for_command(setup, cx), CommandOutcome::Completed);
        let close = dispatch_product_command(&first, first_window, CLOSE_TAB_COMMAND, cx);
        assert_eq!(wait_for_command(close, cx), CommandOutcome::Completed);
        cx.read(|cx| {
            assert!(documents.read(cx).get(document).is_some());
            assert!(model.read(cx).is_open());
            assert_eq!(model.read(cx).resolved_contributions().len(), 1);
        });
        let live = dispatch_product_command(&second, second_window, "closure.live", cx);
        assert_eq!(wait_for_command(live, cx), CommandOutcome::Completed);
        let close = dispatch_product_command(&second, second_window, CLOSE_TAB_COMMAND, cx);
        assert_eq!(wait_for_command(close, cx), CommandOutcome::Completed);
        cx.read(|cx| {
            assert!(documents.read(cx).get(document).is_none());
            assert!(!model.read(cx).is_open());
            assert!(model.read(cx).resolved_contributions().is_empty());
            assert_eq!(retained_editor.read(cx).model(), &model);
        });
        let closed = dispatch_product_command(&second, second_window, "closure.closed", cx);
        assert_eq!(wait_for_command(closed, cx), CommandOutcome::Completed);
        host.update(cx, |host, cx| host.unload(key, cx).unwrap());
    }

    #[gpui::test]
    fn public_api_disposal_and_unload_release_foreground_ownership(cx: &mut TestAppContext) {
        use crate::host::lifecycle::ExtensionKey;
        const SOURCE: &str = r#"
import { commands, editor, workbench, keybinding } from "knot";
let owned;
keybinding("cmd-alt-o", "api.owned");
await commands.register("api.owned-setup", async ({ buffer }) => {
  const snapshot = await buffer.snapshot();
  await buffer.contributions.replace([{ range: { startByteOffset: 0, endByteOffset: 1 }, decoration: "info" }], { ifRevision: snapshot.revision });
  owned = [
    buffer.contributions,
    await buffer.onDidChange(() => workbench.writeClipboardText("leaked listener")),
    await commands.register("api.owned", () => {}),
    await workbench.registerView({
      id: "example.owned",
      title: "Owned",
      content: { kind: "tree", provider: {
        getChildren() { return [{ id: "owned", label: "owned", collapsibleState: "none" }]; },
      } },
    }),
    await editor.registerCompletionProvider("owned", { provideCompletions() { return [{ label: "owned", insertText: "owned" }]; } }),
  ];
});
await commands.register("api.owned-cleanup", async () => {
  const cleanup = owned.map(owner => owner.dispose());
  for (let index = 0; index < owned.length; index++) if (cleanup[index] !== owned[index].dispose()) throw new Error("cleanup promise changed");
  await Promise.all(cleanup);
});
"#;
        let documents = install_globals(cx);
        cx.update(super::bind_product_keys);
        let host = cx.update(super::super::extension_host::install);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        model.update(cx, |model, _| model.replace(0..0, "owned").unwrap());
        let (shell, window) = product_window(document, model.clone(), cx);
        let editor = cx.read(|cx| {
            shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .active_tab()
                .editor()
                .unwrap()
                .clone()
        });
        for explicit_cleanup in [true, false] {
            let key = ExtensionKey::new(
                ExtensionId::new(if explicit_cleanup { 704 } else { 705 }),
                ExtensionLifecycleId::new(1),
            );
            host.update(cx, |host, cx| {
                let execution = host.load_test_source(key, SOURCE).unwrap();
                cx.spawn(async move |_, _| {
                    execution.await.unwrap();
                })
                .detach();
            });
            wait_for_api_condition(cx, |cx| {
                cx.global::<ApplicationProductCommands>()
                    .0
                    .read(cx)
                    .catalog()
                    .borrow()
                    .resolve("api.owned-cleanup")
                    .is_ok()
            });
            let setup = dispatch_product_command(&shell, window, "api.owned-setup", cx);
            assert_eq!(wait_for_command(setup, cx), CommandOutcome::Completed);
            wait_for_api_condition(cx, |cx| {
                shell
                    .read(cx)
                    .contributed_views
                    .get("example.owned")
                    .is_some_and(|view| view.tree().read(cx).root_labels() == ["owned"])
            });
            cx.read(|cx| assert_eq!(model.read(cx).resolved_contributions().len(), 1));
            let completion =
                dispatch_product_command(&shell, window, super::SHOW_COMPLETIONS_COMMAND, cx);
            assert_eq!(wait_for_command(completion, cx), CommandOutcome::Completed);
            wait_for_api_condition(cx, |cx| {
                editor
                    .read(cx)
                    .completion_state()
                    .is_some_and(|(items, pending, _, _, _)| items == 1 && pending == 0)
            });
            if explicit_cleanup {
                let cleanup = dispatch_product_command(&shell, window, "api.owned-cleanup", cx);
                assert_eq!(wait_for_command(cleanup, cx), CommandOutcome::Completed);
            } else {
                host.update(cx, |host, cx| host.unload(key, cx).unwrap());
            }
            cx.read(|cx| {
                assert!(model.read(cx).resolved_contributions().is_empty());
                assert!(shell.read(cx).contributed_views.is_empty());
                assert!(
                    cx.global::<ApplicationProductCommands>()
                        .0
                        .read(cx)
                        .catalog()
                        .borrow()
                        .resolve("api.owned")
                        .is_err()
                );
                let (items, pending, _, _, _) = editor.read(cx).completion_state().unwrap();
                assert_eq!((items, pending), (0, 0));
            });
            if explicit_cleanup {
                host.update(cx, |host, cx| host.unload(key, cx).unwrap());
            }
            cx.read(|cx| {
                assert!(keymap_command(cx, "cmd-alt-o", Some("editor")).is_none());
                assert!(
                    cx.global::<ApplicationProductCommands>()
                        .0
                        .read(cx)
                        .catalog()
                        .borrow()
                        .resolve("api.owned-setup")
                        .is_err()
                );
            });
        }
        let key = ExtensionKey::new(ExtensionId::new(706), ExtensionLifecycleId::new(1));
        host.update(cx, |host, cx| {
            let execution = host.load_test_source(key, SOURCE).unwrap();
            cx.spawn(async move |_, _| {
                execution.await.unwrap();
            })
            .detach();
        });
        wait_for_api_condition(cx, |cx| {
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .catalog()
                .borrow()
                .resolve("api.owned-cleanup")
                .is_ok()
        });
        let setup = dispatch_product_command(&shell, window, "api.owned-setup", cx);
        assert_eq!(wait_for_command(setup, cx), CommandOutcome::Completed);
        cx.read(|cx| assert_eq!(model.read(cx).resolved_contributions().len(), 1));
        cx.update(|cx| {
            drop(cx.remove_global::<super::super::extension_host::ApplicationExtensionHost>())
        });
        cx.update(move |_| drop(host));
        cx.run_until_parked();
        cx.read(|cx| {
            assert!(model.read(cx).resolved_contributions().is_empty());
            assert!(shell.read(cx).contributed_views.is_empty());
            assert!(
                cx.global::<ApplicationProductCommands>()
                    .0
                    .read(cx)
                    .catalog()
                    .borrow()
                    .resolve("api.owned")
                    .is_err()
            );
            assert!(keymap_command(cx, "cmd-alt-o", Some("editor")).is_none());
        });
    }

    #[gpui::test]
    fn contributed_view_registered_before_the_window_attaches_on_open(cx: &mut TestAppContext) {
        use crate::host::lifecycle::ExtensionKey;

        const SOURCE: &str = r#"
import { workbench } from "knot";
await workbench.registerView({
  id: "example.deferred",
  title: "Deferred",
  content: { kind: "tree", provider: {
    getChildren() { return [{ id: "root", label: "Deferred", collapsibleState: "none" }]; },
  } },
});
"#;

        let documents = install_globals(cx);
        let host = cx.update(super::super::extension_host::install);
        let key = ExtensionKey::new(ExtensionId::new(90), ExtensionLifecycleId::new(1));
        host.update(cx, |host, cx| {
            let execution = host.load_test_source(key, SOURCE).unwrap();
            cx.spawn(async move |_, _| {
                execution.await.unwrap();
            })
            .detach();
        });
        wait_for_api_condition(cx, |cx| {
            super::super::extension_host::view_definitions(cx)
                .iter()
                .any(|definition| definition.id == "example.deferred")
        });

        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, _window) = product_window(document, model, cx);
        let tree = wait_for_contributed_view(&shell, "example.deferred", cx);
        wait_for_api_condition(cx, |cx| tree.read(cx).root_labels() == ["Deferred"]);
    }

    #[gpui::test]
    fn focused_extension_tree_claims_copy_and_unload_removes_its_handler(cx: &mut TestAppContext) {
        use crate::host::lifecycle::ExtensionKey;

        const SOURCE: &str = r#"
import * as knot from "knot"; const { commands, workbench } = knot;
await workbench.registerView({
  id: "example.outline",
  title: "Outline",
  content: { kind: "tree", provider: {
    getChildren(parentId) {
      return parentId === null ? [
        { id: "first", label: "Selected tree item", collapsibleState: "none", command: "test.view-first" },
        { id: "failure", label: "Fail", collapsibleState: "none" },
      ] : [];
    },
  } },
});
const viewCopy = await commands.registerForView("example.outline", "copy", async (context) => {
  if (context.buffer !== null || await knot.editor.activeBuffer(context.target) !== null) throw new Error("outline has a text buffer");
  const text = await workbench.selectedText(context.target);
  if (text === "Fail") throw new Error("copy failed");
  await workbench.writeClipboardText(text);
});
await commands.register("test.view-first", async () => {
  throw new Error("global handler ran");
});
await commands.registerForView("example.outline", "test.view-first", async () => {
  await workbench.writeClipboardText("tree item");
});
await commands.register("test.view-null", () => { throw new Error("global null handler"); });
await commands.registerForView("example.outline", "test.view-null", async (context) => {
  if (context.target.view === null || await workbench.selectedText(context.target) !== null) throw new Error("missing selection is not null");
});
await commands.register("test.nested-view-copy", async () => {
  const outcome = await commands.invoke("copy");
  if (outcome.kind !== "completed") throw new Error(`nested copy: ${outcome.kind}`);
});
await commands.register("test.nested-new", async () => {
  const outcome = await commands.invoke("file.new");
  if (outcome.kind !== "completed") throw new Error(`nested new: ${outcome.kind}`);
});
await commands.register("test.dispose-view-copy", async () => viewCopy.dispose());
"#;

        let documents = install_globals(cx);
        cx.update(super::bind_product_keys);
        let host = cx.update(super::super::extension_host::install);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window) = product_window(document, model.clone(), cx);
        let key = ExtensionKey::new(ExtensionId::new(87), ExtensionLifecycleId::new(1));
        host.update(cx, |host, cx| {
            let execution = host.load_test_source(key, SOURCE).unwrap();
            cx.spawn(async move |_, _| {
                execution.await.unwrap();
            })
            .detach();
        });

        let tree = wait_for_contributed_view(&shell, "example.outline", cx);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            cx.run_until_parked();
            if cx.read(|cx| {
                tree.read(cx).root_labels().len() == 2
                    && cx
                        .global::<ApplicationProductCommands>()
                        .0
                        .read(cx)
                        .catalog()
                        .borrow()
                        .resolve_view(tree.read(cx).instance_id(), "copy")
                        .is_some()
            }) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "tree provider did not settle"
            );
            std::thread::yield_now();
        }

        cx.update_window(window, |_, window, cx| {
            tree.focus_handle(cx).focus(window);
            let target = shell.update(cx, |shell, cx| {
                shell.capture_command_target(window, cx).unwrap()
            });
            assert_eq!(target.view, Some(tree.read(cx).instance_id()));
        })
        .unwrap();
        let declined = dispatch_tree_copy(&shell, window, cx);
        assert_eq!(wait_for_command(declined, cx), CommandOutcome::Unavailable);
        let empty_selection = dispatch_tree_command(&shell, window, "test.view-null", cx);
        assert_eq!(
            wait_for_command(empty_selection, cx),
            CommandOutcome::Completed
        );
        let view_first = dispatch_tree_command(&shell, window, "test.view-first", cx);
        assert_eq!(wait_for_command(view_first, cx), CommandOutcome::Completed);
        cx.update(|cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string("untouched".into())));
        cx.refresh().unwrap();
        cx.simulate_keystrokes(window, "enter");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            cx.run_until_parked();
            if cx.read(|cx| {
                cx.read_from_clipboard()
                    .is_some_and(|item| item.text().as_deref() == Some("tree item"))
            }) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "tree item command did not run"
            );
            std::thread::yield_now();
        }
        cx.update_window(window, |_, window, cx| {
            shell.read(cx).focus_active_editor(window, cx)
        })
        .unwrap();
        cx.update(|cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string("untouched".into())));
        cx.refresh().unwrap();
        {
            let mut window_cx = VisualTestContext::from_window(window, cx);
            let row = window_cx
                .debug_bounds("extension-tree-row-0")
                .expect("contributed tree row is laid out")
                .center();
            window_cx.simulate_mouse_down(row, MouseButton::Left, Modifiers::default());
            window_cx.simulate_mouse_up(row, MouseButton::Left, Modifiers::default());
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            cx.run_until_parked();
            if cx.read(|cx| {
                cx.read_from_clipboard()
                    .is_some_and(|item| item.text().as_deref() == Some("tree item"))
            }) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "tree click command did not run"
            );
            std::thread::yield_now();
        }
        cx.update(|cx| tree.update(cx, |tree, cx| assert!(tree.select_item("first", cx))));
        let copy = dispatch_tree_copy(&shell, window, cx);
        assert_eq!(wait_for_command(copy, cx), CommandOutcome::Completed);
        cx.update(|cx| {
            assert!(
                cx.global_mut::<ApplicationKeymaps>()
                    .0
                    .set(
                        BindingOwner::Extension(key.extension, key.lifecycle),
                        "cmd-j",
                        Some("example.outline"),
                        Some(Command {
                            name: super::COPY_COMMAND.into(),
                            arguments: CommandArgumentValue::Null,
                        }),
                    )
                    .unwrap()
            );
            super::super::keymaps::rebuild(cx);
            cx.write_to_clipboard(gpui::ClipboardItem::new_string("untouched".into()));
        });
        cx.refresh().unwrap();
        cx.simulate_keystrokes(window, "cmd-j");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            cx.run_until_parked();
            if cx.read(|cx| {
                cx.read_from_clipboard()
                    .is_some_and(|item| item.text().as_deref() == Some("Selected tree item"))
            }) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "view keybinding did not dispatch"
            );
            std::thread::yield_now();
        }
        let nested = dispatch_tree_command(&shell, window, "test.nested-view-copy", cx);
        assert_eq!(wait_for_command(nested, cx), CommandOutcome::Completed);
        let nested_native = dispatch_tree_command(&shell, window, "test.nested-new", cx);
        assert_eq!(
            wait_for_command(nested_native, cx),
            CommandOutcome::Completed
        );
        cx.read(|cx| {
            assert_eq!(
                shell
                    .read(cx)
                    .workbench
                    .read(cx)
                    .focused_pane()
                    .unwrap()
                    .tabs()
                    .len(),
                2
            );
        });
        cx.update_window(window, |_, window, cx| tree.focus_handle(cx).focus(window))
            .unwrap();
        cx.read(|cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "Selected tree item"
            )
        });

        let (second_shell, second_window) = product_window(document, model.clone(), cx);
        let second_tree = wait_for_contributed_view(&second_shell, "example.outline", cx);
        assert_ne!(
            cx.read(|cx| tree.read(cx).instance_id()),
            cx.read(|cx| second_tree.read(cx).instance_id())
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            cx.run_until_parked();
            if cx.read(|cx| second_tree.read(cx).root_labels().len() == 2) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "second tree did not receive provider data"
            );
            std::thread::yield_now();
        }
        cx.update_window(second_window, |_, window, cx| {
            second_tree.update(cx, |tree, cx| assert!(tree.select_item("first", cx)));
            second_tree.focus_handle(cx).focus(window);
        })
        .unwrap();
        assert_eq!(
            wait_for_command(dispatch_tree_copy(&second_shell, second_window, cx), cx),
            CommandOutcome::Completed
        );

        cx.update(|cx| tree.update(cx, |tree, cx| assert!(tree.select_item("failure", cx))));
        let failure = dispatch_tree_copy(&shell, window, cx);
        assert!(matches!(
            wait_for_command(failure, cx),
            CommandOutcome::HandlerFailure { .. }
        ));
        cx.update(|cx| tree.update(cx, |tree, cx| assert!(tree.select_item("first", cx))));
        let recovery = dispatch_tree_copy(&shell, window, cx);
        assert_eq!(wait_for_command(recovery, cx), CommandOutcome::Completed);

        let dispose = dispatch_product_command(&shell, window, "test.dispose-view-copy", cx);
        assert_eq!(wait_for_command(dispose, cx), CommandOutcome::Completed);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            cx.run_until_parked();
            if cx.read(|cx| {
                cx.global::<ApplicationProductCommands>()
                    .0
                    .read(cx)
                    .catalog()
                    .borrow()
                    .resolve_view(tree.read(cx).instance_id(), "copy")
                    .is_none()
            }) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "disposed view handler remained registered"
            );
            std::thread::yield_now();
        }
        let disposed = dispatch_tree_copy(&second_shell, second_window, cx);
        assert_eq!(wait_for_command(disposed, cx), CommandOutcome::Unavailable);

        host.update(cx, |host, cx| host.unload(key, cx).unwrap());
        cx.read(|cx| {
            assert!(
                cx.global::<ApplicationKeymaps>()
                    .0
                    .resolve(
                        &gpui::Keystroke::parse("cmd-j").unwrap().unparse(),
                        Some("example.outline")
                    )
                    .is_none()
            );
        });
        cx.read(|cx| {
            assert!(shell.read(cx).contributed_views.is_empty());
            assert!(second_shell.read(cx).contributed_views.is_empty());
            assert!(!tree.read(cx).lifecycle_state().0);
            assert!(!second_tree.read(cx).lifecycle_state().0);
        });
    }

    #[gpui::test]
    fn startup_script_command_cannot_borrow_the_focused_product_window(cx: &mut TestAppContext) {
        use crate::host::lifecycle::ExtensionKey;

        let documents = install_globals(cx);
        let host = cx.update(super::super::extension_host::install);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window) = product_window(document, model, cx);
        cx.update_window(window, |_, window, cx| {
            shell.read(cx).focus_active_editor(window, cx);
        })
        .unwrap();

        let key = ExtensionKey::new(ExtensionId::new(89), ExtensionLifecycleId::new(1));
        let (done, mut result) = tokio::sync::oneshot::channel();
        host.update(cx, |host, cx| {
            let execution = host
                .load_test_source(
                    key,
                    r#"
import * as knot from "knot"; const { commands } = knot;
let rejected = false;
try { await commands.invoke("file.new"); }
catch (error) { if (error.code !== "ContextUnavailable") throw error; rejected = true; }
if (!rejected) throw new Error("startup borrowed focus");
"#,
                )
                .unwrap();
            cx.spawn(async move |_, _| {
                let _ = done.send(execution.await);
            })
            .detach();
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            cx.run_until_parked();
            match result.try_recv() {
                Ok(outcome) => {
                    outcome.unwrap();
                    break;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "script command did not settle"
                    );
                    std::thread::yield_now();
                }
                Err(error) => panic!("script completion closed: {error}"),
            }
        }
        cx.read(|cx| {
            assert_eq!(
                shell
                    .read(cx)
                    .workbench
                    .read(cx)
                    .focused_pane()
                    .unwrap()
                    .tabs()
                    .len(),
                1
            );
        });
    }

    #[gpui::test]
    fn extension_command_with_editor_prefix_keeps_its_arguments(cx: &mut TestAppContext) {
        use crate::host::lifecycle::ExtensionKey;

        let documents = install_globals(cx);
        let host = cx.update(super::super::extension_host::install);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window) = product_window(document, model, cx);
        let key = ExtensionKey::new(ExtensionId::new(91), ExtensionLifecycleId::new(1));
        host.update(cx, |host, cx| {
            let execution = host
                .load_test_source(
                    key,
                    r#"
import * as knot from "knot"; const { commands } = knot;
await commands.register("editor.custom", async ({ arguments: args }) => {
  if (args?.value !== 42) throw new Error("missing command arguments");
});
"#,
                )
                .unwrap();
            cx.spawn(async move |_, _| {
                execution.await.unwrap();
            })
            .detach();
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            cx.run_until_parked();
            if cx.read(|cx| {
                cx.global::<ApplicationProductCommands>()
                    .0
                    .read(cx)
                    .catalog()
                    .borrow()
                    .resolve("editor.custom")
                    .is_ok()
            }) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "command did not register"
            );
            std::thread::yield_now();
        }
        let target = cx
            .update_window(window, |_, window, cx| {
                shell.read(cx).focus_active_editor(window, cx);
                shell.update(cx, |shell, cx| {
                    shell.capture_command_target(window, cx).unwrap()
                })
            })
            .unwrap();
        let dispatcher = cx.read(|cx| cx.global::<ApplicationProductCommands>().0.clone());
        let execution = dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: "editor.custom".into(),
                    arguments: CommandArgumentValue::Object(std::collections::BTreeMap::from([(
                        "value".into(),
                        CommandArgumentValue::Number(42.),
                    )])),
                },
                target,
                cx,
            )
        });
        assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
    }

    #[gpui::test]
    fn nested_script_waits_for_a_pending_native_command(cx: &mut TestAppContext) {
        use crate::host::lifecycle::ExtensionKey;

        let documents = install_globals(cx);
        cx.set_global(ApplicationOpenDialog(Arc::new(FixedOpenDialog(
            OpenDialogOutcome::Cancelled,
        ))));
        let host = cx.update(super::super::extension_host::install);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window) = product_window(document, model, cx);
        let key = ExtensionKey::new(ExtensionId::new(90), ExtensionLifecycleId::new(1));
        host.update(cx, |host, cx| {
            let execution = host
                .load_test_source(
                    key,
                    r#"
import * as knot from "knot"; const { commands } = knot;
await commands.register("test.nested-open", async () => {
  const outcome = await commands.invoke("file.open");
  if (outcome.kind !== "cancelled") throw new Error(`open: ${outcome.kind}`);
});
"#,
                )
                .unwrap();
            cx.spawn(async move |_, _| {
                execution.await.unwrap();
            })
            .detach();
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            cx.run_until_parked();
            if cx.read(|cx| {
                cx.global::<ApplicationProductCommands>()
                    .0
                    .read(cx)
                    .catalog()
                    .borrow()
                    .resolve("test.nested-open")
                    .is_ok()
            }) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "nested command did not register"
            );
            std::thread::yield_now();
        }
        let execution = dispatch_product_command(&shell, window, "test.nested-open", cx);
        assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
        cx.read(|cx| assert_eq!(documents.read(cx).documents().count(), 1));
    }

    #[gpui::test]
    fn delayed_clipboard_write_survives_presentation_closure_but_not_unload(
        cx: &mut TestAppContext,
    ) {
        use crate::host::lifecycle::ExtensionKey;

        const SOURCE: &str = r#"
import * as knot from "knot"; const { commands, workbench } = knot;
await workbench.registerView({
  id: "example.outline",
  title: "Outline",
  content: { kind: "tree", provider: {
    getChildren(parentId) {
      return parentId === null
        ? [{ id: "held", label: "Held selection", collapsibleState: "none" }]
        : [];
    },
  } },
});
await commands.registerForView("example.outline", "copy", async () => {
  const text = await workbench.selectedText();
  await workbench.writeClipboardText(text);
});
"#;

        let documents = install_globals(cx);
        let host = cx.update(super::super::extension_host::install);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window) = product_window(document, model.clone(), cx);
        let key = ExtensionKey::new(ExtensionId::new(88), ExtensionLifecycleId::new(1));
        host.update(cx, |host, cx| {
            let execution = host.load_test_source(key, SOURCE).unwrap();
            cx.spawn(async move |_, _| {
                execution.await.unwrap();
            })
            .detach();
        });
        let tree = wait_for_contributed_view(&shell, "example.outline", cx);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            cx.run_until_parked();
            if cx.read(|cx| {
                tree.read(cx).root_labels().len() == 1
                    && cx
                        .global::<ApplicationProductCommands>()
                        .0
                        .read(cx)
                        .catalog()
                        .borrow()
                        .resolve_view(tree.read(cx).instance_id(), "copy")
                        .is_some()
            }) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "view handler did not register"
            );
            std::thread::yield_now();
        }
        cx.update_window(window, |_, window, cx| {
            tree.update(cx, |tree, cx| assert!(tree.select_item("held", cx)));
            tree.focus_handle(cx).focus(window);
        })
        .unwrap();
        host.update(cx, |host, _| host.hold_clipboard_writes());
        cx.update(|cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string("untouched".into())));
        let execution = dispatch_tree_copy(&shell, window, cx);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            cx.run_until_parked();
            if cx.read(|cx| host.read(cx).pending_view_request_count() == 1) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "view request was not held"
            );
            std::thread::yield_now();
        }
        cx.update_window(window, |_, window, _| window.remove_window())
            .unwrap();
        host.update(cx, |host, cx| host.release_view_requests(cx));
        assert_eq!(
            wait_for_command(execution, cx),
            CommandOutcome::InvalidTarget
        );
        cx.read(|cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "Held selection"
            )
        });
        let closed_view = cx.read(|cx| tree.read(cx).instance_id());
        drop(shell);
        cx.run_until_parked();
        cx.read(|cx| {
            assert!(
                cx.global::<ApplicationProductCommands>()
                    .0
                    .read(cx)
                    .catalog()
                    .borrow()
                    .resolve_view(closed_view, "copy")
                    .is_none()
            )
        });

        let (second_shell, second_window) = product_window(document, model, cx);
        let second_tree = wait_for_contributed_view(&second_shell, "example.outline", cx);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            cx.run_until_parked();
            if cx.read(|cx| second_tree.read(cx).root_labels().len() == 1) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "second tree did not load"
            );
            std::thread::yield_now();
        }
        cx.update_window(second_window, |_, window, cx| {
            second_tree.update(cx, |tree, cx| assert!(tree.select_item("held", cx)));
            second_tree.focus_handle(cx).focus(window);
        })
        .unwrap();
        host.update(cx, |host, _| host.hold_clipboard_writes());
        let interrupted = dispatch_tree_copy(&second_shell, second_window, cx);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            cx.run_until_parked();
            if cx.read(|cx| host.read(cx).pending_view_request_count() == 1) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "second clipboard write was not held"
            );
            std::thread::yield_now();
        }
        host.update(cx, |host, cx| host.unload(key, cx).unwrap());
        host.update(cx, |host, cx| host.release_view_requests(cx));
        assert_eq!(wait_for_command(interrupted, cx), CommandOutcome::Cancelled);
        cx.read(|cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "Held selection"
            );
            assert!(!second_tree.read(cx).lifecycle_state().0);
        });
    }

    #[gpui::test]
    async fn dispatch_completion_preserves_the_captured_pane(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window_handle) = product_window(document, model, cx);

        cx.update_window(window_handle, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.split(SplitDirection::Horizontal, window, cx)
            });
        })
        .unwrap();
        cx.run_until_parked();
        cx.refresh().unwrap();

        let (first_pane, second_pane, first_editor, second_editor) = cx.read(|cx| {
            let workbench = shell.read(cx).workbench.read(cx);
            let first = &workbench.panes()[0];
            let second = &workbench.panes()[1];
            (
                first.id(),
                second.id(),
                first.active_tab().editor().unwrap().clone(),
                second.active_tab().editor().unwrap().clone(),
            )
        });
        let target = cx
            .update_window(window_handle, |_, window, cx| {
                first_editor.focus_handle(cx).focus(window);
                shell.update(cx, |shell, cx| {
                    shell.capture_command_target(window, cx).unwrap()
                })
            })
            .unwrap();
        let execution = cx.update(|cx| {
            let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
            dispatcher.update(cx, |dispatcher, cx| {
                dispatcher.dispatch(
                    Command {
                        name: NEW_COMMAND.into(),
                        arguments: CommandArgumentValue::Null,
                    },
                    target,
                    cx,
                )
            })
        });
        cx.update_window(window_handle, |_, window, cx| {
            second_editor.focus_handle(cx).focus(window)
        })
        .unwrap();

        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::Completed
        );
        cx.read(|cx| {
            let workbench = shell.read(cx).workbench.read(cx);
            assert_eq!(workbench.pane(first_pane).unwrap().tabs().len(), 2);
            assert_eq!(workbench.pane(second_pane).unwrap().tabs().len(), 1);
        });
    }

    #[gpui::test]
    async fn destroyed_product_targets_are_not_retargeted(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window_handle) = product_window(document, model, cx);
        cx.refresh().unwrap();

        let target = cx
            .update_window(window_handle, |_, window, cx| {
                shell.update(cx, |shell, cx| {
                    shell.focus_active_editor(window, cx);
                    shell.capture_command_target(window, cx).unwrap()
                })
            })
            .unwrap();
        cx.update_window(window_handle, |_, window, cx| {
            shell.update(cx, |shell, cx| shell.close_active_tab(window, cx));
        })
        .unwrap();
        let execution = cx.update(|cx| {
            let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
            dispatcher.update(cx, |dispatcher, cx| {
                dispatcher.dispatch(
                    Command {
                        name: SPLIT_HORIZONTAL_COMMAND.into(),
                        arguments: CommandArgumentValue::Null,
                    },
                    target,
                    cx,
                )
            })
        });

        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::InvalidTarget
        );
        cx.read(|cx| assert_eq!(shell.read(cx).workbench.read(cx).panes().len(), 1));
    }

    #[gpui::test]
    async fn keybinding_adapter_enters_the_product_dispatcher(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        cx.update(super::bind_product_keys);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window_handle) = product_window(document, model, cx);
        cx.update_window(window_handle, |_, window, cx| {
            shell.read(cx).focus_active_editor(window, cx)
        })
        .unwrap();

        cx.simulate_keystrokes(
            window_handle,
            if cfg!(windows) { "ctrl-n" } else { "cmd-n" },
        );
        cx.run_until_parked();

        cx.read(|cx| {
            assert_eq!(
                shell
                    .read(cx)
                    .workbench
                    .read(cx)
                    .focused_pane()
                    .unwrap()
                    .tabs()
                    .len(),
                2
            );
            assert_eq!(
                cx.global::<ApplicationProductCommands>()
                    .0
                    .read(cx)
                    .last_outcome(),
                Some(&CommandOutcome::Completed)
            );
        });
    }

    #[gpui::test]
    fn personal_global_binding_overrides_editor_default_in_every_window(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        cx.update(super::bind_product_keys);
        let mut windows = Vec::new();
        for _ in 0..2 {
            let document = cx.update(|cx| create_untitled_document(&documents, cx));
            let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
            let (shell, window) = product_window(document, model.clone(), cx);
            cx.update_window(window, |_, window, cx| {
                shell.read(cx).focus_active_editor(window, cx)
            })
            .unwrap();
            windows.push((window, model));
        }
        cx.update(|cx| {
            let changed = cx
                .global_mut::<ApplicationKeymaps>()
                .0
                .set(
                    BindingOwner::Personal(ExtensionId::new(0), ExtensionLifecycleId::new(1)),
                    "Enter",
                    None,
                    Some(Command {
                        name: super::super::product_commands::INSERT_TAB_COMMAND.into(),
                        arguments: CommandArgumentValue::Null,
                    }),
                )
                .unwrap();
            assert!(changed);
            super::super::keymaps::rebuild(cx);
        });
        cx.refresh().unwrap();
        for (window, _) in &windows {
            cx.simulate_keystrokes(*window, "enter");
        }
        cx.run_until_parked();
        cx.read(|cx| {
            for (_, model) in &windows {
                assert_eq!(model.read(cx).text(), "\t");
            }
        });
    }

    #[gpui::test]
    fn unbinding_an_unmapped_letter_preserves_editor_input(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        cx.update(super::bind_product_keys);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window) = product_window(document, model.clone(), cx);
        cx.update_window(window, |_, window, cx| {
            shell.read(cx).focus_active_editor(window, cx)
        })
        .unwrap();
        cx.update(|cx| {
            let owner = BindingOwner::Personal(ExtensionId::new(0), ExtensionLifecycleId::new(1));
            assert!(
                cx.global_mut::<ApplicationKeymaps>()
                    .0
                    .set(owner, "a", Some("editor"), None)
                    .unwrap()
            );
            super::super::keymaps::rebuild(cx);
        });
        cx.refresh().unwrap();
        cx.simulate_keystrokes(window, "a");
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(model.read(cx).text(), "a"));
    }

    #[gpui::test]
    async fn editor_unbind_leaves_global_binding_available_in_terminal(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        cx.update(super::bind_product_keys);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window) = product_window(document, model, cx);
        cx.update_window(window, |_, window, cx| {
            shell.read(cx).focus_active_editor(window, cx)
        })
        .unwrap();
        cx.update(|cx| {
            assert!(
                cx.global_mut::<ApplicationKeymaps>()
                    .0
                    .set(
                        BindingOwner::Personal(ExtensionId::new(0), ExtensionLifecycleId::new(1)),
                        if cfg!(windows) { "ctrl-n" } else { "cmd-n" },
                        Some("editor"),
                        None,
                    )
                    .unwrap()
            );
            super::super::keymaps::rebuild(cx);
        });
        cx.refresh().unwrap();
        cx.simulate_keystrokes(window, if cfg!(windows) { "ctrl-n" } else { "cmd-n" });
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(shell.read(cx).workbench.read(cx).panes()[0].tabs().len(), 1));

        let terminal = dispatch_product_command(&shell, window, NEW_TERMINAL_COMMAND, cx);
        assert_eq!(
            terminal.completion.await.unwrap(),
            CommandOutcome::Completed
        );
        cx.refresh().unwrap();
        cx.simulate_keystrokes(window, if cfg!(windows) { "ctrl-n" } else { "cmd-n" });
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(shell.read(cx).workbench.read(cx).panes()[0].tabs().len(), 3));
    }

    #[gpui::test]
    fn sequence_prefix_dispatches_the_product_command(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        cx.update(super::bind_product_keys);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window) = product_window(document, model, cx);
        cx.update_window(window, |_, window, cx| {
            shell.read(cx).focus_active_editor(window, cx)
        })
        .unwrap();
        cx.refresh().unwrap();
        cx.simulate_keystrokes(
            window,
            if cfg!(windows) {
                "ctrl-k right"
            } else {
                "cmd-k right"
            },
        );
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(shell.read(cx).workbench.read(cx).panes().len(), 2));
    }

    #[gpui::test]
    fn unavailable_high_priority_binding_does_not_invoke_native_default(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        cx.update(super::bind_product_keys);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window) = product_window(document, model, cx);
        cx.update_window(window, |_, window, cx| {
            shell.read(cx).focus_active_editor(window, cx)
        })
        .unwrap();
        cx.update(|cx| {
            assert!(
                cx.global_mut::<ApplicationKeymaps>()
                    .0
                    .set(
                        BindingOwner::Personal(ExtensionId::new(0), ExtensionLifecycleId::new(1)),
                        if cfg!(windows) { "ctrl-n" } else { "cmd-n" },
                        None,
                        Some(Command {
                            name: "missing.command".into(),
                            arguments: CommandArgumentValue::Null,
                        }),
                    )
                    .unwrap()
            );
            super::super::keymaps::rebuild(cx);
        });
        cx.refresh().unwrap();
        cx.simulate_keystrokes(window, if cfg!(windows) { "ctrl-n" } else { "cmd-n" });
        cx.run_until_parked();
        cx.read(|cx| {
            assert_eq!(shell.read(cx).workbench.read(cx).panes()[0].tabs().len(), 1);
            assert_eq!(
                cx.global::<ApplicationProductCommands>()
                    .0
                    .read(cx)
                    .last_outcome(),
                Some(&CommandOutcome::Unavailable),
            );
        });
    }

    #[gpui::test]
    fn menu_action_reaches_the_captured_window_in_its_dispatch_turn(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window) = product_window(document, model, cx);
        cx.update_window(window, |_, window, cx| {
            shell.read(cx).focus_active_editor(window, cx)
        })
        .unwrap();
        cx.refresh().unwrap();

        cx.dispatch_action(window, ProductCommandSource::new(NEW_COMMAND));

        cx.read(|cx| {
            assert_eq!(
                shell
                    .read(cx)
                    .workbench
                    .read(cx)
                    .focused_pane()
                    .unwrap()
                    .tabs()
                    .len(),
                2
            );
            assert_eq!(
                cx.global::<ApplicationProductCommands>()
                    .0
                    .read(cx)
                    .last_outcome(),
                Some(&CommandOutcome::Completed),
            );
        });
    }

    #[cfg(windows)]
    #[gpui::test]
    fn windows_native_ctrl_shift_p_opens_command_palette(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        cx.update(super::bind_product_keys);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window) = product_window(document, model, cx);
        cx.update_window(window, |_, window, cx| {
            shell.read(cx).focus_active_editor(window, cx);
        })
        .unwrap();
        cx.refresh().unwrap();
        cx.simulate_keystrokes(window, "ctrl-shift-p");
        cx.run_until_parked();
        cx.read(|cx| assert!(shell.read(cx).command_palette.is_some()));
    }

    #[gpui::test]
    fn personal_config_ctrl_shift_p_opens_command_palette(cx: &mut TestAppContext) {
        let config = tempfile::tempdir().unwrap();
        let extensions = tempfile::tempdir().unwrap();
        std::fs::write(
            config.path().join("post-init.js"),
            "import * as knot from 'knot'; knot.keybinding('ctrl-shift-p', 'workbench.show-command-palette');",
        )
        .unwrap();
        install_globals(cx);
        cx.update(super::bind_product_keys);
        install_launch_gate(cx);
        let host = cx.update(super::super::extension_host::install);
        host.update(cx, |host, cx| {
            host.start_product_startup(
                Some(config.path().to_path_buf()),
                Ok(extensions.path().to_path_buf()),
                cx,
            )
        });
        assert!(wait_for_product_startup(cx));
        let window = cx.windows()[0];
        let shell = cx
            .update_window(window, |_, window, _| {
                window.root::<ProductShell>().flatten()
            })
            .unwrap()
            .unwrap();
        cx.update_window(window, |_, window, cx| {
            shell.read(cx).focus_active_editor(window, cx);
        })
        .unwrap();
        cx.refresh().unwrap();
        cx.simulate_keystrokes(window, "ctrl-shift-p");
        cx.run_until_parked();
        cx.read(|cx| assert!(shell.read(cx).command_palette.is_some()));
    }

    #[gpui::test]
    async fn palette_discovers_commands_and_keeps_its_opening_target(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        cx.update(super::bind_product_keys);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window_handle) = product_window(document, model, cx);
        cx.update_window(window_handle, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.split(SplitDirection::Horizontal, window, cx)
            });
        })
        .unwrap();
        cx.refresh().unwrap();

        let (first_pane, second_pane, first_editor) = cx.read(|cx| {
            let workbench = shell.read(cx).workbench.read(cx);
            let first = &workbench.panes()[0];
            (
                first.id(),
                workbench.panes()[1].id(),
                first.active_tab().editor().unwrap().clone(),
            )
        });
        cx.update_window(window_handle, |_, window, cx| {
            first_editor.focus_handle(cx).focus(window);
        })
        .unwrap();
        cx.simulate_keystrokes(
            window_handle,
            if cfg!(windows) {
                "ctrl-shift-p"
            } else {
                "cmd-shift-p"
            },
        );
        cx.run_until_parked();
        cx.update_window(window_handle, |_, _, cx| {
            assert!(shell.read(cx).command_palette.is_some());
            let workbench = shell.read(cx).workbench.clone();
            workbench.update(cx, |workbench, _| {
                workbench.focus_pane(second_pane);
            });
        })
        .unwrap();
        cx.refresh().unwrap();

        cx.simulate_keystrokes(window_handle, "f i l e . n e w enter");

        cx.read(|cx| {
            let workbench = shell.read(cx).workbench.read(cx);
            assert_eq!(workbench.pane(first_pane).unwrap().tabs().len(), 2);
            assert_eq!(workbench.pane(second_pane).unwrap().tabs().len(), 1);
        });
    }

    #[gpui::test]
    async fn editing_commands_keep_the_captured_view_and_native_clipboard(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window_handle) = product_window(document, model.clone(), cx);
        let (target, dispatcher) = cx
            .update_window(window_handle, |_, window, cx| {
                shell.read(cx).focus_active_editor(window, cx);
                let target = shell.update(cx, |shell, cx| {
                    shell.capture_command_target(window, cx).unwrap()
                });
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                    "fn café() {\n    👩‍💻\n}\n".into(),
                ));
                shell.update(cx, |shell, cx| shell.new_tab(window, cx));
                (target, cx.global::<ApplicationProductCommands>().0.clone())
            })
            .unwrap();
        for command in [
            super::PASTE_COMMAND,
            super::SELECT_ALL_COMMAND,
            super::COPY_COMMAND,
        ] {
            let execution = dispatcher.update(cx, |dispatcher, cx| {
                dispatcher.dispatch(
                    Command {
                        name: command.into(),
                        arguments: CommandArgumentValue::Null,
                    },
                    target.clone(),
                    cx,
                )
            });
            assert_eq!(
                execution.completion.await.unwrap(),
                CommandOutcome::Completed
            );
        }
        cx.read(|cx| {
            assert_eq!(model.read(cx).text(), "fn café() {\n    👩‍💻\n}\n");
            let active = shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .active_tab()
                .editor()
                .unwrap();
            assert_eq!(active.read(cx).model().read(cx).text(), "");
        });
        let execution = dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: super::CUT_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                target,
                cx,
            )
        });
        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::Completed
        );
        cx.update(|cx| {
            assert_eq!(model.read(cx).text(), "");
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "fn café() {\n    👩‍💻\n}\n"
            );
        });
    }

    #[cfg(unix)]
    #[gpui::test]
    async fn terminal_mouse_reporting_toggle_routes_to_the_focused_session_and_plain_drag_copies(
        cx: &mut TestAppContext,
    ) {
        use crate::app::{
            terminal_session::TerminalSession,
            terminal_view::TOGGLE_TERMINAL_MOUSE_REPORTING_COMMAND, terminal_view::TerminalView,
        };
        use alacritty_terminal::{tty, vte::ansi::Processor};
        let documents = install_globals(cx);
        cx.update(super::bind_product_keys);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window_handle) = product_window(document, model, cx);
        assert_eq!(
            dispatch_product_command(
                &shell,
                window_handle,
                TOGGLE_TERMINAL_MOUSE_REPORTING_COMMAND,
                cx
            )
            .completion
            .await
            .unwrap(),
            CommandOutcome::Unavailable
        );
        let session = cx.new(|cx| {
            TerminalSession::new_with_options(
                tty::Options {
                    shell: Some(tty::Shell::new(
                        "/bin/sh".into(),
                        vec!["-c".into(), "read line".into()],
                    )),
                    drain_on_exit: true,
                    ..Default::default()
                },
                cx,
            )
        });
        let view = cx
            .update_window(window_handle, |_, window, cx| {
                let id = cx
                    .global::<ApplicationTerminalSessions>()
                    .0
                    .borrow_mut()
                    .allocate_id();
                cx.global::<ApplicationTerminalSessions>()
                    .0
                    .borrow_mut()
                    .sessions
                    .insert(id, session.clone());
                let view = cx.new(|cx| TerminalView::new(session.clone(), cx));
                shell.update(cx, |shell, cx| {
                    let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
                    let tab = shell.workbench.update(cx, |workbench, _| {
                        workbench.open_terminal_tab(pane, id, view.clone()).unwrap()
                    });
                    shell.activate_tab(pane, tab, window, cx);
                });
                view
            })
            .unwrap();
        cx.refresh().unwrap();
        assert_eq!(
            dispatch_product_command(
                &shell,
                window_handle,
                TOGGLE_TERMINAL_MOUSE_REPORTING_COMMAND,
                cx
            )
            .completion
            .await
            .unwrap(),
            CommandOutcome::Completed
        );
        cx.read(|cx| assert!(!session.read(cx).mouse_reporting_enabled()));
        let (start, end) = cx
            .update_window(window_handle, |_, window, cx| {
                let mut parser: Processor = Processor::new();
                parser.advance(
                    &mut *session.read(cx).terminal().unwrap().lock(),
                    b"\x1b[?1002h\x1b[?1006h\x1b[2J\x1b[3J\x1b[Hcopy me",
                );
                let bounds = view.read(cx).interaction_bounds();
                let width = px(session.read(cx).size().cell_width as f32 / window.scale_factor());
                (
                    bounds.origin + point(px(0.1), px(0.1)),
                    bounds.origin + point(width * 3 + px(0.1), px(0.1)),
                )
            })
            .unwrap();
        let mut window_cx = VisualTestContext::from_window(window_handle, cx);
        window_cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        window_cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
        window_cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
        cx.simulate_keystrokes(
            window_handle,
            if cfg!(windows) { "ctrl-c" } else { "cmd-c" },
        );
        cx.run_until_parked();
        cx.update(|cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some("copy")
            );
            assert!(!session.read(cx).mouse_reporting_enabled());
        });
        assert_eq!(
            dispatch_product_command(
                &shell,
                window_handle,
                TOGGLE_TERMINAL_MOUSE_REPORTING_COMMAND,
                cx
            )
            .completion
            .await
            .unwrap(),
            CommandOutcome::Completed
        );
        cx.read(|cx| assert!(session.read(cx).mouse_reporting_enabled()));
        session.update(cx, |session, cx| session.close(cx));
    }

    #[gpui::test]
    async fn copy_requires_focused_selection_and_declines_on_terminal(cx: &mut TestAppContext) {
        use alacritty_terminal::tty::{Options, Shell};
        use alacritty_terminal::{
            index::{Column, Line, Point as TermPoint, Side},
            selection::{Selection, SelectionType},
            vte::ansi::Handler,
        };
        use gpui::EntityInputHandler;

        #[cfg(windows)]
        let quiet_shell = Shell::new(
            "cmd.exe".into(),
            vec!["/C".into(), "ping -n 3 127.0.0.1 >nul".into()],
        );
        #[cfg(not(windows))]
        let quiet_shell = Shell::new("/bin/sh".into(), vec!["-c".into(), "sleep 2".into()]);

        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window) = product_window(document, model.clone(), cx);
        cx.update_window(window, |_, window, cx| {
            let editor = shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .active_tab()
                .editor()
                .unwrap()
                .clone();
            editor.update(cx, |editor, cx| {
                editor.replace_text_in_range(None, "selected text", window, cx);
            });
        })
        .unwrap();
        let no_selection = dispatch_product_command(&shell, window, super::COPY_COMMAND, cx);
        assert_eq!(
            no_selection.completion.await.unwrap(),
            CommandOutcome::Unavailable
        );
        cx.update_window(window, |_, window, cx| {
            let editor = shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .active_tab()
                .editor()
                .unwrap()
                .clone();
            editor.update(cx, |editor, cx| {
                editor.execute_editing_command("editor.select-all", window, cx);
            });
        })
        .unwrap();
        let editor_target = cx
            .update_window(window, |_, window, cx| {
                shell.update(cx, |shell, cx| {
                    shell.capture_command_target(window, cx).unwrap()
                })
            })
            .unwrap();
        let editor_copy = dispatch_product_command(&shell, window, super::COPY_COMMAND, cx);
        assert_eq!(
            editor_copy.completion.await.unwrap(),
            CommandOutcome::Completed
        );
        cx.update(|cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "selected text"
            )
        });
        let legacy_copy = dispatch_product_command(&shell, window, "editor.copy", cx);
        assert_eq!(
            legacy_copy.completion.await.unwrap(),
            CommandOutcome::Unavailable
        );

        cx.update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| shell.open_command_palette(window, cx));
        })
        .unwrap();
        cx.refresh().unwrap();
        cx.simulate_keystrokes(window, "c o p y enter");
        cx.run_until_parked();
        cx.update(|cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "selected text"
            );
            assert_eq!(
                cx.global::<ApplicationProductCommands>()
                    .0
                    .read(cx)
                    .last_outcome(),
                Some(&CommandOutcome::Completed)
            );
        });

        let terminal_session = cx.update(|cx| {
            cx.new(|cx| {
                crate::app::terminal_session::TerminalSession::new_with_options(
                    Options {
                        shell: Some(quiet_shell),
                        drain_on_exit: true,
                        ..Default::default()
                    },
                    cx,
                )
            })
        });
        let target = cx
            .update_window(window, |_, window, cx| {
                shell.update(cx, |shell, cx| {
                    let session = cx
                        .global::<ApplicationTerminalSessions>()
                        .0
                        .borrow_mut()
                        .allocate_id();
                    cx.global::<ApplicationTerminalSessions>()
                        .0
                        .borrow_mut()
                        .sessions
                        .insert(session, terminal_session.clone());
                    let view = cx.new(|cx| {
                        crate::app::terminal_view::TerminalView::new(terminal_session.clone(), cx)
                    });
                    let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
                    let tab = shell.workbench.update(cx, |workbench, _| {
                        workbench.open_terminal_tab(pane, session, view).unwrap()
                    });
                    shell.activate_tab(pane, tab, window, cx);
                    shell.capture_command_target(window, cx).unwrap()
                })
            })
            .unwrap();
        let dispatcher = cx.read(|cx| cx.global::<ApplicationProductCommands>().0.clone());
        let captured_editor = dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: super::COPY_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                editor_target,
                cx,
            )
        });
        assert_eq!(
            captured_editor.completion.await.unwrap(),
            CommandOutcome::Completed
        );
        cx.read(|cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "selected text"
            )
        });
        let execution = dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: super::COPY_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                target.clone(),
                cx,
            )
        });
        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::Unavailable
        );
        cx.read(|cx| {
            let session_id = match target.surface {
                TabSurfaceId::Terminal(id) => id,
                _ => unreachable!(),
            };
            let session = cx
                .global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions[&session_id]
                .clone();
            let terminal = session.read(cx).terminal().unwrap().clone();
            let mut terminal = terminal.lock();
            terminal.goto(0, 0);
            for ch in "selected terminal text".chars() {
                terminal.input(ch);
            }
            let mut selection = Selection::new(
                SelectionType::Simple,
                TermPoint::new(Line(0), Column(0)),
                Side::Left,
            );
            selection.update(TermPoint::new(Line(0), Column(21)), Side::Right);
            terminal.selection = Some(selection);
        });
        let selected_text = cx.read(|cx| {
            let session_id = match target.surface {
                TabSurfaceId::Terminal(id) => id,
                _ => unreachable!(),
            };
            let session = cx
                .global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions[&session_id]
                .clone();
            session
                .read(cx)
                .terminal()
                .unwrap()
                .lock()
                .selection_to_string()
                .unwrap()
        });
        assert!(!selected_text.is_empty());
        let selected_terminal = dispatch_product_command(&shell, window, super::COPY_COMMAND, cx);
        assert_eq!(
            selected_terminal.completion.await.unwrap(),
            CommandOutcome::Completed
        );
        cx.read(|cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                selected_text
            );
        });
        let application =
            dispatch_product_command(&shell, window, super::SHOW_EXTENSION_REPORT_COMMAND, cx);
        assert_eq!(
            application.completion.await.unwrap(),
            CommandOutcome::Completed
        );
        cx.read(|cx| assert!(shell.read(cx).extension_report_open));

        let scripted = dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: super::COPY_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                target.clone(),
                cx,
            )
        });
        assert_eq!(
            scripted.completion.await.unwrap(),
            CommandOutcome::Completed
        );

        let unavailable = dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: "missing.command".into(),
                    arguments: CommandArgumentValue::Null,
                },
                target.clone(),
                cx,
            )
        });
        assert_eq!(
            unavailable.completion.await.unwrap(),
            CommandOutcome::Unavailable
        );

        let mut stale = target;
        stale.surface = TabSurfaceId::Document(document);
        let execution = dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: super::COPY_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                stale,
                cx,
            )
        });
        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::InvalidTarget
        );
    }

    #[gpui::test]
    fn product_editor_accepts_utf16_composition_and_grapheme_deletion(cx: &mut TestAppContext) {
        use gpui::EntityInputHandler;
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window_handle) = product_window(document, model.clone(), cx);
        cx.update_window(window_handle, |_, window, cx| {
            shell.read(cx).focus_active_editor(window, cx);
            let editor = shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .active_tab()
                .editor()
                .unwrap()
                .clone();
            editor.update(cx, |editor, cx| {
                editor.replace_text_in_range(None, "# Notes\n👩‍💻 é ", window, cx);
                let start = "# Notes\n👩‍💻 é ".encode_utf16().count();
                editor.replace_and_mark_text_in_range(None, "に", Some(1..1), window, cx);
                assert_eq!(editor.marked_text_range(window, cx), Some(start..start + 1));
                editor.replace_and_mark_text_in_range(None, "日本", Some(2..2), window, cx);
                assert_eq!(editor.marked_text_range(window, cx), Some(start..start + 2));
                editor.replace_text_in_range(None, "日本語", window, cx);
                assert_eq!(editor.marked_text_range(window, cx), None);
                assert!(editor.execute_editing_command("editor.move-document-start", window, cx));
                assert!(editor.execute_editing_command("editor.move-down", window, cx));
                assert!(editor.execute_editing_command("editor.delete-forward", window, cx));
            });
        })
        .unwrap();
        cx.read(|cx| assert_eq!(model.read(cx).text(), "# Notes\n é 日本語"));
    }

    #[gpui::test]
    fn editing_keybindings_use_semantic_commands(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        cx.update(super::bind_product_keys);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window_handle) = product_window(document, model.clone(), cx);
        cx.update_window(window_handle, |_, window, cx| {
            shell.read(cx).focus_active_editor(window, cx)
        })
        .unwrap();
        cx.simulate_keystrokes(window_handle, "enter");
        cx.run_until_parked();
        cx.read(|cx| {
            assert_eq!(model.read(cx).text(), "\n");
            assert_eq!(
                cx.global::<ApplicationProductCommands>()
                    .0
                    .read(cx)
                    .last_outcome(),
                Some(&CommandOutcome::Completed)
            );
        });
        cx.simulate_keystrokes(window_handle, "shift-up backspace tab");
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(model.read(cx).text(), "\t"));
        cx.simulate_input(window_handle, "café👩‍💻");
        cx.simulate_keystrokes(window_handle, "enter");
        cx.simulate_input(window_handle, "tail");
        cx.simulate_keystrokes(window_handle, "home");
        cx.simulate_input(window_handle, ">");
        cx.simulate_keystrokes(window_handle, "end");
        cx.simulate_input(window_handle, "<");
        cx.simulate_keystrokes(
            window_handle,
            if cfg!(windows) {
                "shift-home ctrl-c"
            } else {
                "shift-home cmd-c"
            },
        );
        cx.run_until_parked();
        cx.update(|cx| assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), ">tail<"));
        cx.simulate_keystrokes(
            window_handle,
            if cfg!(windows) {
                "ctrl-a ctrl-c"
            } else {
                "cmd-a cmd-c"
            },
        );
        cx.run_until_parked();
        cx.update(|cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "\tcafé👩‍💻\n>tail<"
            )
        });
        cx.simulate_keystrokes(
            window_handle,
            if cfg!(windows) {
                "ctrl-x ctrl-v"
            } else {
                "cmd-x cmd-v"
            },
        );
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(model.read(cx).text(), "\tcafé👩‍💻\n>tail<"));
    }

    #[gpui::test]
    fn product_editor_captures_mouse_selection_outside_its_pane(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let text = "first line\nsecond line";
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        model.update(cx, |model, _| {
            model.replace(0..0, text).unwrap();
        });
        let (shell, window_handle) = product_window(document, model, cx);
        cx.refresh().unwrap();
        cx.run_until_parked();

        let editor = cx.read(|cx| {
            shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .active_tab()
                .editor()
                .unwrap()
                .clone()
        });
        let bounds = cx.read(|cx| editor.read(cx).interaction_bounds());
        let start = point(bounds.origin.x + px(1.), bounds.origin.y + px(5.));
        let outside = point(
            bounds.origin.x + bounds.size.width + px(20.),
            bounds.origin.y + bounds.size.height + px(20.),
        );
        let mut window_cx = VisualTestContext::from_window(window_handle, cx);
        window_cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        window_cx.simulate_mouse_move(outside, MouseButton::Left, Modifiers::default());
        window_cx.simulate_mouse_up(outside, MouseButton::Left, Modifiers::default());

        window_cx.read(|cx| {
            assert_eq!(editor.read(cx).selected_byte_range(), Some(0..text.len()));
        });
    }

    #[gpui::test]
    async fn undo_command_routes_to_the_captured_editor(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window_handle) = product_window(document, model, cx);
        cx.update(|cx| {
            let model = documents.read(cx).get(document).unwrap().model().clone();
            model.update(cx, |model, _| model.replace(0..0, "edit").unwrap());
        });
        let (target, dispatcher) = cx
            .update_window(window_handle, |_, window, cx| {
                shell.read(cx).focus_active_editor(window, cx);
                let target = shell.update(cx, |shell, cx| {
                    shell.capture_command_target(window, cx).unwrap()
                });
                (target, cx.global::<ApplicationProductCommands>().0.clone())
            })
            .unwrap();
        let execution = dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: UNDO_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                target.clone(),
                cx,
            )
        });

        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::Completed
        );
        cx.read(|cx| {
            assert_eq!(
                documents
                    .read(cx)
                    .get(document)
                    .unwrap()
                    .model()
                    .read(cx)
                    .text(),
                ""
            );
        });
        let execution = dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: REDO_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                target,
                cx,
            )
        });
        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::Completed
        );
        cx.read(|cx| {
            assert_eq!(
                documents
                    .read(cx)
                    .get(document)
                    .unwrap()
                    .model()
                    .read(cx)
                    .text(),
                "edit"
            );
        });
    }

    #[gpui::test]
    async fn every_editor_motion_command_reaches_the_focused_view(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window) = product_window(document, model, cx);
        let target = cx
            .update_window(window, |_, window, cx| {
                shell.read(cx).focus_active_editor(window, cx);
                shell.update(cx, |shell, cx| {
                    shell.capture_command_target(window, cx).unwrap()
                })
            })
            .unwrap();
        let dispatcher = cx.read(|cx| cx.global::<ApplicationProductCommands>().0.clone());
        for name in super::super::product_commands::product_command_names().filter(|name| {
            name.starts_with(super::super::product_commands::EDITOR_MOVE_PREFIX)
                || name.starts_with(super::super::product_commands::EDITOR_SELECT_PREFIX)
        }) {
            let execution = dispatcher.update(cx, |dispatcher, cx| {
                dispatcher.dispatch(
                    Command {
                        name: name.into(),
                        arguments: CommandArgumentValue::Null,
                    },
                    target.clone(),
                    cx,
                )
            });
            assert_eq!(
                execution.completion.await.unwrap(),
                CommandOutcome::Completed,
                "{name}"
            );
        }
    }

    #[gpui::test]
    async fn native_participants_reject_arguments_before_mutation(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window) = product_window(document, model.clone(), cx);
        let target = cx
            .update_window(window, |_, window, cx| {
                shell.read(cx).focus_active_editor(window, cx);
                shell.update(cx, |shell, cx| {
                    shell.capture_command_target(window, cx).unwrap()
                })
            })
            .unwrap();
        cx.update(|cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string("ignored".into())));
        let dispatcher = cx.read(|cx| cx.global::<ApplicationProductCommands>().0.clone());
        for name in [
            super::COPY_COMMAND,
            super::PASTE_COMMAND,
            NEW_COMMAND,
            super::NEW_WINDOW_COMMAND,
            SHOW_EXTENSION_REPORT_COMMAND,
        ] {
            let execution = dispatcher.update(cx, |dispatcher, cx| {
                dispatcher.dispatch(
                    Command {
                        name: name.into(),
                        arguments: CommandArgumentValue::String("unexpected".into()),
                    },
                    target.clone(),
                    cx,
                )
            });
            assert!(
                matches!(
                    execution.completion.await.unwrap(),
                    CommandOutcome::InvalidArgument { .. }
                ),
                "{name}"
            );
        }
        cx.read(|cx| {
            assert_eq!(model.read(cx).text(), "");
            assert_eq!(
                shell
                    .read(cx)
                    .workbench
                    .read(cx)
                    .focused_pane()
                    .unwrap()
                    .tabs()
                    .len(),
                1
            );
            assert_eq!(cx.windows().len(), 1);
        });
    }

    #[gpui::test]
    async fn undo_keeps_the_document_captured_before_focus_changes(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let first_document = cx.update(|cx| create_untitled_document(&documents, cx));
        let first_model = cx.read(|cx| {
            documents
                .read(cx)
                .get(first_document)
                .unwrap()
                .model()
                .clone()
        });
        let (shell, window_handle) = product_window(first_document, first_model.clone(), cx);
        cx.update_window(window_handle, |_, window, cx| {
            shell.update(cx, |shell, cx| shell.new_tab(window, cx));
        })
        .unwrap();

        let (pane, first_tab, second_tab, second_document, second_model) = cx.read(|cx| {
            let workbench = shell.read(cx).workbench.read(cx);
            let pane = workbench.focused_pane().unwrap();
            let first = &pane.tabs()[0];
            let second = &pane.tabs()[1];
            let second_model = documents
                .read(cx)
                .get(second.document_id().unwrap())
                .unwrap()
                .model()
                .clone();
            (
                pane.id(),
                first.id(),
                second.id(),
                second.document_id().unwrap(),
                second_model,
            )
        });
        first_model.update(cx, |model, _| model.replace(0..0, "first").unwrap());
        second_model.update(cx, |model, _| model.replace(0..0, "second").unwrap());

        let target = cx
            .update_window(window_handle, |_, window, cx| {
                shell.update(cx, |shell, cx| {
                    shell.activate_tab(pane, first_tab, window, cx);
                    shell.capture_command_target(window, cx).unwrap()
                })
            })
            .unwrap();
        let dispatcher = cx.read(|cx| cx.global::<ApplicationProductCommands>().0.clone());
        let execution = dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: UNDO_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                target,
                cx,
            )
        });
        cx.update_window(window_handle, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.activate_tab(pane, second_tab, window, cx)
            });
        })
        .unwrap();

        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::Completed
        );
        cx.read(|cx| {
            assert_eq!(first_model.read(cx).text(), "");
            assert_eq!(second_model.read(cx).text(), "second");
            assert_eq!(
                shell
                    .read(cx)
                    .workbench
                    .read(cx)
                    .focused_pane()
                    .unwrap()
                    .active_tab()
                    .document_id()
                    .unwrap(),
                second_document
            );
        });
    }

    #[gpui::test]
    fn focus_transfer_and_distinct_views_break_typing_groups(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        cx.update(super::bind_product_keys);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window_handle) = product_window(document, model.clone(), cx);
        cx.update_window(window_handle, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.split(SplitDirection::Horizontal, window, cx)
            });
        })
        .unwrap();
        cx.refresh().unwrap();
        cx.run_until_parked();
        let (first, second) = cx.read(|cx| {
            let workbench = shell.read(cx).workbench.read(cx);
            (
                workbench.panes()[0].active_tab().editor().unwrap().clone(),
                workbench.panes()[1].active_tab().editor().unwrap().clone(),
            )
        });
        cx.update_window(window_handle, |_, window, cx| {
            first.focus_handle(cx).focus(window)
        })
        .unwrap();
        cx.refresh().unwrap();
        cx.simulate_input(window_handle, "a");
        cx.update_window(window_handle, |_, window, cx| {
            second.focus_handle(cx).focus(window);
        })
        .unwrap();
        cx.refresh().unwrap();
        cx.run_until_parked();
        cx.update_window(window_handle, |_, window, cx| {
            first.focus_handle(cx).focus(window);
        })
        .unwrap();
        cx.refresh().unwrap();
        cx.run_until_parked();
        cx.simulate_input(window_handle, "b");
        cx.refresh().unwrap();
        cx.simulate_keystrokes(
            window_handle,
            if cfg!(windows) { "ctrl-z" } else { "cmd-z" },
        );
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(model.read(cx).text(), "a"));

        cx.simulate_keystrokes(
            window_handle,
            if cfg!(windows) {
                "ctrl-shift-z"
            } else {
                "cmd-shift-z"
            },
        );
        cx.run_until_parked();
        cx.update_window(window_handle, |_, window, cx| {
            second.focus_handle(cx).focus(window)
        })
        .unwrap();
        cx.refresh().unwrap();
        cx.run_until_parked();
        cx.simulate_input(window_handle, "X");
        cx.update_window(window_handle, |_, window, cx| {
            first.focus_handle(cx).focus(window)
        })
        .unwrap();
        cx.refresh().unwrap();
        cx.simulate_keystrokes(
            window_handle,
            if cfg!(windows) { "ctrl-z" } else { "cmd-z" },
        );
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(model.read(cx).text(), "ab"));
    }

    #[gpui::test]
    fn shell_actions_render_tabs_and_both_split_directions(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, cx) = cx.add_window_view(|window, cx| {
            let workbench = cx.new(|cx| Workbench::new_for_document(document, model, cx));
            cx.global::<ApplicationWorkbenches>()
                .register(&workbench, window.window_handle());
            ProductShell::new(workbench)
        });

        cx.update(|window, cx| {
            shell.update(cx, |shell, cx| shell.new_tab(window, cx));
            shell.update(cx, |shell, cx| {
                shell.split(SplitDirection::Horizontal, window, cx)
            });
            shell.update(cx, |shell, cx| {
                shell.split(SplitDirection::Vertical, window, cx)
            });
        });
        cx.run_until_parked();

        cx.read(|cx| {
            let workbench = shell.read(cx).workbench.read(cx);
            assert_eq!(workbench.panes().len(), 3);
            assert!(matches!(
                workbench.layout(),
                Some(WorkbenchLayout::Split {
                    direction: SplitDirection::Horizontal,
                    second,
                    ..
                }) if matches!(
                    second.as_ref(),
                    WorkbenchLayout::Split {
                        direction: SplitDirection::Vertical,
                        ..
                    }
                )
            ));
            assert_eq!(
                workbench
                    .panes()
                    .iter()
                    .map(|pane| pane.tabs().len())
                    .sum::<usize>(),
                4
            );
        });
    }

    #[gpui::test]
    fn new_tab_targets_the_pane_whose_editor_has_focus(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, cx) = cx.add_window_view(|window, cx| {
            let workbench = cx.new(|cx| Workbench::new_for_document(document, model, cx));
            cx.global::<ApplicationWorkbenches>()
                .register(&workbench, window.window_handle());
            ProductShell::new(workbench)
        });

        cx.update(|window, cx| {
            shell.update(cx, |shell, cx| {
                shell.split(SplitDirection::Horizontal, window, cx)
            });
        });
        cx.run_until_parked();
        cx.refresh().unwrap();

        let (first_pane, first_editor, second_editor) = cx.read(|cx| {
            let shell = shell.read(cx);
            let workbench = shell.workbench.read(cx);
            let first_pane = workbench.panes()[0].id();
            (
                first_pane,
                workbench
                    .pane(first_pane)
                    .unwrap()
                    .active_tab()
                    .editor()
                    .unwrap()
                    .clone(),
                workbench.panes()[1].active_tab().editor().unwrap().clone(),
            )
        });
        cx.update(|window, cx| second_editor.focus_handle(cx).focus(window));
        cx.run_until_parked();
        cx.refresh().unwrap();
        assert!(cx.update(|window, cx| second_editor.focus_handle(cx).is_focused(window)));
        cx.update(|window, cx| first_editor.focus_handle(cx).focus(window));
        cx.run_until_parked();
        cx.refresh().unwrap();
        assert!(cx.update(|window, cx| first_editor.focus_handle(cx).is_focused(window)));

        cx.update(|window, cx| {
            shell.update(cx, |shell, cx| shell.new_tab(window, cx));
        });
        cx.run_until_parked();

        cx.read(|cx| {
            let workbench = shell.read(cx).workbench.read(cx);
            assert_eq!(workbench.pane(first_pane).unwrap().tabs().len(), 2);
            assert_eq!(
                workbench
                    .panes()
                    .iter()
                    .find(|pane| pane.id() != first_pane)
                    .unwrap()
                    .tabs()
                    .len(),
                1
            );
        });
    }

    #[gpui::test]
    fn closing_the_final_clean_tab_creates_a_replacement(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let original = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(original).unwrap().model().clone());
        let (shell, cx) = cx.add_window_view(|window, cx| {
            let workbench = cx.new(|cx| Workbench::new_for_document(original, model, cx));
            cx.global::<ApplicationWorkbenches>()
                .register(&workbench, window.window_handle());
            ProductShell::new(workbench)
        });

        cx.update(|window, cx| {
            shell.update(cx, |shell, cx| shell.close_active_tab(window, cx));
        });
        cx.run_until_parked();

        cx.read(|cx| {
            let replacement = shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .active_tab()
                .document_id()
                .unwrap();
            assert_ne!(replacement, original);
            assert!(documents.read(cx).get(original).is_none());
            assert!(documents.read(cx).get(replacement).is_some());
            assert_eq!(
                shell.read(cx).status,
                "created replacement untitled document"
            );
        });
    }

    #[gpui::test]
    fn replacing_a_shared_blank_tab_preserves_the_other_windows_document(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let blank = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(blank).unwrap().model().clone());
        let (first, first_window) = product_window(blank, model.clone(), cx);
        let (second, _) = product_window(blank, model.clone(), cx);
        let replacement = cx.update(|cx| create_untitled_document(&documents, cx));
        cx.update_window(first_window, |_, _window, cx| {
            first.update(cx, |shell, cx| {
                let pane = shell.workbench.read(cx).focused_pane().unwrap();
                let target = super::OpenTarget {
                    window: _window.window_handle(),
                    workbench: shell.workbench.downgrade(),
                    pane: pane.id(),
                    tab: pane.active_tab_id(),
                    surface: pane.active_tab().surface_id(),
                    presentation: pane
                        .active_tab()
                        .editor()
                        .map(Entity::entity_id)
                        .or_else(|| pane.active_tab().terminal_view().map(Entity::entity_id)),
                };
                shell.present_open_document(replacement, &target, true, cx);
            });
        })
        .unwrap();
        cx.read(|cx| {
            assert!(documents.read(cx).get(blank).is_some());
            assert!(model.read(cx).is_open());
            assert_eq!(first.read(cx).workbench.read(cx).view_count(blank), 0);
            assert_eq!(second.read(cx).workbench.read(cx).view_count(blank), 1);
        });
    }

    #[gpui::test]
    fn workbench_registry_counts_views_across_windows(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (_first, _) = product_window(document, model.clone(), cx);
        let (_second, _) = product_window(document, model, cx);
        cx.read(|cx| {
            assert_eq!(
                cx.global::<ApplicationWorkbenches>()
                    .view_count(document, cx),
                2
            )
        });
    }

    #[gpui::test]
    async fn retained_closed_shell_does_not_keep_the_last_live_document_open(
        cx: &mut TestAppContext,
    ) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (closed_shell, closed_window) = product_window(document, model.clone(), cx);
        let (live_shell, live_window) = product_window(document, model.clone(), cx);
        assert_eq!(
            dispatch_product_command(
                &closed_shell,
                closed_window,
                super::CLOSE_WINDOW_COMMAND,
                cx
            )
            .completion
            .await
            .unwrap(),
            CommandOutcome::Completed
        );
        cx.read(|cx| {
            assert_eq!(
                closed_shell
                    .read(cx)
                    .workbench
                    .read(cx)
                    .view_count(document),
                1
            );
            assert!(documents.read(cx).get(document).is_some());
        });
        assert_eq!(
            dispatch_product_command(&live_shell, live_window, CLOSE_TAB_COMMAND, cx)
                .completion
                .await
                .unwrap(),
            CommandOutcome::Completed
        );
        cx.read(|cx| {
            assert!(documents.read(cx).get(document).is_none());
            assert!(!model.read(cx).is_open());
        });
    }

    #[gpui::test]
    async fn dirty_final_tab_close_cancel_preserves_the_document_and_view(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        model.update(cx, |model, _| model.replace(0..0, "unsaved").unwrap());
        let (shell, window) = product_window(document, model, cx);

        let execution = dispatch_product_command(&shell, window, CLOSE_TAB_COMMAND, cx);
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Cancel");

        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::Cancelled
        );
        cx.read(|cx| {
            assert!(documents.read(cx).get(document).is_some());
            assert_eq!(shell.read(cx).workbench.read(cx).view_count(document), 1);
        });
    }

    #[gpui::test]
    async fn discarding_a_dirty_final_tab_closes_it_and_installs_a_replacement(
        cx: &mut TestAppContext,
    ) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        model.update(cx, |model, _| model.replace(0..0, "unsaved").unwrap());
        let (shell, window) = product_window(document, model, cx);

        let execution = dispatch_product_command(&shell, window, CLOSE_TAB_COMMAND, cx);
        cx.run_until_parked();
        cx.simulate_prompt_answer("Don't Save");

        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::Completed
        );
        cx.read(|cx| {
            assert!(documents.read(cx).get(document).is_none());
            let replacement = shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .active_tab()
                .document_id()
                .unwrap();
            assert_ne!(replacement, document);
            assert!(documents.read(cx).get(replacement).is_some());
        });
    }

    #[gpui::test]
    async fn saving_a_dirty_tab_uses_existing_persistence_before_closing(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let (filesystems, provider) = memory_filesystems_with_provider();
        cx.set_global(ApplicationFileSystems(filesystems));
        let uri = ResourceUri::parse("mem://product/notes.txt").unwrap();
        let opened = provider.read(uri.clone()).await.unwrap();
        let model = cx.new(|_| BufferModel::from_text("loaded text"));
        let document = documents.update(cx, |documents, cx| {
            documents.create_persisted(
                "notes.txt",
                model.clone(),
                uri.clone(),
                0,
                opened.version,
                cx,
            )
        });
        model.update(cx, |model, _| model.replace(0..0, "changed ").unwrap());
        let (shell, window) = product_window(document, model, cx);

        let execution = dispatch_product_command(&shell, window, CLOSE_TAB_COMMAND, cx);
        cx.run_until_parked();
        cx.simulate_prompt_answer("Save");

        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::Completed
        );
        assert_eq!(
            provider.read(uri).await.unwrap().bytes,
            b"changed loaded text"
        );
        cx.read(|cx| assert!(documents.read(cx).get(document).is_none()));
    }

    #[gpui::test]
    async fn edit_racing_protected_close_save_preserves_the_document_and_every_view(
        cx: &mut TestAppContext,
    ) {
        let documents = install_globals(cx);
        let root = ResourceUri::parse("mem://product/").unwrap();
        let provider = Arc::new(MemoryFileSystemProvider::new(root).unwrap());
        let uri = ResourceUri::parse("mem://product/notes.txt").unwrap();
        provider.seed_file(uri.clone(), b"loaded text").unwrap();
        let opened = provider.read(uri.clone()).await.unwrap();
        let (replace_release, gated_replace) = tokio::sync::oneshot::channel();
        let gated_provider = Arc::new(GatedFileSystemProvider {
            inner: provider.clone(),
            read_release: Mutex::new(None),
            replace_release: Mutex::new(Some(gated_replace)),
        });
        let mut filesystems = FileSystemProviderRegistry::new();
        filesystems.register("mem", gated_provider).unwrap();
        cx.set_global(ApplicationFileSystems(Arc::new(filesystems)));

        let model = cx.new(|_| BufferModel::from_text("loaded text"));
        let document = documents.update(cx, |documents, cx| {
            documents.create_persisted(
                "notes.txt",
                model.clone(),
                uri.clone(),
                0,
                opened.version,
                cx,
            )
        });
        model.update(cx, |model, _| model.replace(0..0, "captured ").unwrap());
        let (shell, window) = product_window(document, model.clone(), cx);
        cx.update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.split(SplitDirection::Horizontal, window, cx)
            });
        })
        .unwrap();

        let execution = dispatch_product_command(&shell, window, CLOSE_WINDOW_COMMAND, cx);
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Save");
        cx.run_until_parked();

        model.update(cx, |model, _| model.replace(0..0, "newer ").unwrap());
        replace_release.send(()).unwrap();

        assert!(matches!(
            execution.completion.await.unwrap(),
            CommandOutcome::HandlerFailure { .. }
        ));
        assert_eq!(
            provider.read(uri.clone()).await.unwrap().bytes,
            b"captured loaded text"
        );
        cx.read(|cx| {
            assert!(cx.windows().contains(&window));
            assert_eq!(model.read(cx).text(), "newer captured loaded text");
            assert!(documents.read(cx).get(document).unwrap().is_dirty(cx));
            assert_eq!(shell.read(cx).workbench.read(cx).view_count(document), 2);
        });
    }

    #[gpui::test]
    async fn closing_one_of_multiple_views_of_a_dirty_document_does_not_prompt(
        cx: &mut TestAppContext,
    ) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        model.update(cx, |model, _| model.replace(0..0, "unsaved").unwrap());
        let (shell, window) = product_window(document, model, cx);
        cx.update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.split(SplitDirection::Horizontal, window, cx)
            });
        })
        .unwrap();

        let execution = dispatch_product_command(&shell, window, CLOSE_TAB_COMMAND, cx);
        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::Completed
        );
        assert!(!cx.has_pending_prompt());
        cx.read(|cx| {
            assert!(documents.read(cx).get(document).is_some());
            assert_eq!(shell.read(cx).workbench.read(cx).view_count(document), 1);
        });
    }

    #[gpui::test]
    async fn window_close_prompts_once_for_a_multiply_viewed_dirty_document(
        cx: &mut TestAppContext,
    ) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        model.update(cx, |model, _| model.replace(0..0, "unsaved").unwrap());
        let (shell, window) = product_window(document, model, cx);
        cx.update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.split(SplitDirection::Horizontal, window, cx)
            });
        })
        .unwrap();

        let execution = dispatch_product_command(&shell, window, CLOSE_WINDOW_COMMAND, cx);
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Don't Save");

        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::Completed
        );
        assert!(!cx.has_pending_prompt());
        cx.read(|cx| {
            assert!(!cx.windows().contains(&window));
            assert!(documents.read(cx).get(document).is_none());
        });
    }

    #[gpui::test]
    async fn window_close_keeps_a_dirty_document_visible_in_another_window_without_prompting(
        cx: &mut TestAppContext,
    ) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        model.update(cx, |model, _| model.replace(0..0, "unsaved").unwrap());
        let (first_shell, first_window) = product_window(document, model.clone(), cx);
        let (_second_shell, second_window) = product_window(document, model, cx);

        let execution =
            dispatch_product_command(&first_shell, first_window, CLOSE_WINDOW_COMMAND, cx);
        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::Completed
        );
        assert!(!cx.has_pending_prompt());
        cx.read(|cx| {
            assert!(!cx.windows().contains(&first_window));
            assert!(cx.windows().contains(&second_window));
            assert!(documents.read(cx).get(document).unwrap().is_dirty(cx));
        });
    }

    #[gpui::test]
    async fn quit_prompts_once_for_a_document_shared_across_windows(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        model.update(cx, |model, _| model.replace(0..0, "unsaved").unwrap());
        let (first_shell, first_window) = product_window(document, model.clone(), cx);
        let _second = product_window(document, model, cx);

        let execution = dispatch_product_command(&first_shell, first_window, QUIT_COMMAND, cx);
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Don't Save");

        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::Completed
        );
        assert!(!cx.has_pending_prompt());
    }

    #[gpui::test]
    async fn multi_document_quit_applies_serial_save_and_discard_decisions(
        cx: &mut TestAppContext,
    ) {
        let documents = install_globals(cx);
        let (filesystems, provider) = memory_filesystems_with_provider();
        cx.set_global(ApplicationFileSystems(filesystems));
        let uri = ResourceUri::parse("mem://product/notes.txt").unwrap();
        let opened = provider.read(uri.clone()).await.unwrap();
        let first_model = cx.new(|_| BufferModel::from_text("loaded text"));
        let first = documents.update(cx, |documents, cx| {
            documents.create_persisted(
                "notes.txt",
                first_model.clone(),
                uri.clone(),
                0,
                opened.version,
                cx,
            )
        });
        first_model.update(cx, |model, _| model.replace(0..0, "saved ").unwrap());
        let second_model = cx.new(|_| BufferModel::from_text("second"));
        let second = documents.update(cx, |documents, cx| {
            documents.create_untitled("Second", second_model.clone(), cx)
        });
        second_model.update(cx, |model, _| model.replace(0..0, "discarded ").unwrap());
        let (shell, window) = product_window(first, first_model, cx);
        shell.update(cx, |shell, cx| {
            let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
            shell.workbench.update(cx, |workbench, cx| {
                workbench
                    .open_tab_for_document(pane, second, second_model, cx)
                    .unwrap();
            });
        });

        let execution = dispatch_product_command(&shell, window, QUIT_COMMAND, cx);
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Save");
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Don't Save");

        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::Completed
        );
        assert_eq!(
            provider.read(uri).await.unwrap().bytes,
            b"saved loaded text"
        );
        assert!(!cx.has_pending_prompt());
    }

    #[gpui::test]
    async fn later_quit_cancellation_preserves_every_document_and_view(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let (first, second, shell, window) =
            two_dirty_untitled_documents_in_one_window(&documents, cx);

        let execution = dispatch_product_command(&shell, window, QUIT_COMMAND, cx);
        cx.run_until_parked();
        cx.simulate_prompt_answer("Don't Save");
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Cancel");

        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::Cancelled
        );
        cx.read(|cx| {
            assert!(cx.windows().contains(&window));
            assert_eq!(shell.read(cx).workbench.read(cx).tab_snapshots().len(), 2);
            assert!(documents.read(cx).get(first).unwrap().is_dirty(cx));
            assert!(documents.read(cx).get(second).unwrap().is_dirty(cx));
        });
    }

    #[gpui::test]
    async fn later_quit_save_failure_preserves_every_document_and_view(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        cx.set_global(ApplicationFileSystems(memory_filesystems()));
        cx.set_global(ApplicationSaveDialog(Arc::new(FixedSaveDialog(
            SaveDialogOutcome::Selected(ResourceUri::parse("mem://product/src").unwrap()),
        ))));
        let (first, second, shell, window) =
            two_dirty_untitled_documents_in_one_window(&documents, cx);

        let execution = dispatch_product_command(&shell, window, QUIT_COMMAND, cx);
        cx.run_until_parked();
        cx.simulate_prompt_answer("Don't Save");
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Save");

        assert!(matches!(
            execution.completion.await.unwrap(),
            CommandOutcome::HandlerFailure { .. }
        ));
        cx.read(|cx| {
            assert!(cx.windows().contains(&window));
            assert_eq!(shell.read(cx).workbench.read(cx).tab_snapshots().len(), 2);
            assert!(documents.read(cx).get(first).unwrap().is_dirty(cx));
            assert!(documents.read(cx).get(second).unwrap().is_dirty(cx));
        });
    }

    #[gpui::test]
    async fn later_window_close_cancellation_preserves_every_tab(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let first = cx.update(|cx| create_untitled_document(&documents, cx));
        let first_model = cx.read(|cx| documents.read(cx).get(first).unwrap().model().clone());
        first_model.update(cx, |model, _| model.replace(0..0, "first").unwrap());
        let second_model = cx.new(|_| BufferModel::from_text("second"));
        let second = cx.update(|cx| {
            documents.update(cx, |documents, cx| {
                documents.create_untitled("Second", second_model.clone(), cx)
            })
        });
        second_model.update(cx, |model, _| model.replace(0..0, "changed ").unwrap());
        let (shell, window) = product_window(first, first_model, cx);
        shell.update(cx, |shell, cx| {
            let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
            shell.workbench.update(cx, |workbench, cx| {
                workbench
                    .open_tab_for_document(pane, second, second_model, cx)
                    .unwrap();
            });
        });

        let execution = dispatch_product_command(&shell, window, CLOSE_WINDOW_COMMAND, cx);
        cx.run_until_parked();
        cx.simulate_prompt_answer("Don't Save");
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Cancel");

        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::Cancelled
        );
        cx.read(|cx| {
            assert!(cx.windows().contains(&window));
            assert_eq!(shell.read(cx).workbench.read(cx).tab_snapshots().len(), 2);
            assert!(documents.read(cx).get(first).is_some());
            assert!(documents.read(cx).get(second).is_some());
        });
    }

    #[gpui::test]
    async fn close_save_failure_preserves_the_dirty_tab(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        cx.set_global(ApplicationFileSystems(memory_filesystems()));
        cx.set_global(ApplicationSaveDialog(Arc::new(FixedSaveDialog(
            SaveDialogOutcome::Selected(ResourceUri::parse("mem://product/src").unwrap()),
        ))));
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        model.update(cx, |model, _| model.replace(0..0, "unsaved").unwrap());
        let (shell, window) = product_window(document, model, cx);

        let execution = dispatch_product_command(&shell, window, CLOSE_TAB_COMMAND, cx);
        cx.run_until_parked();
        cx.simulate_prompt_answer("Save");

        assert!(matches!(
            execution.completion.await.unwrap(),
            CommandOutcome::HandlerFailure { .. }
        ));
        cx.read(|cx| {
            assert!(documents.read(cx).get(document).unwrap().is_dirty(cx));
            assert_eq!(shell.read(cx).workbench.read(cx).view_count(document), 1);
        });
    }

    #[gpui::test]
    async fn edit_while_discard_prompt_is_open_rejects_the_close(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        model.update(cx, |model, _| model.replace(0..0, "first edit").unwrap());
        let (shell, window) = product_window(document, model.clone(), cx);

        let execution = dispatch_product_command(&shell, window, CLOSE_TAB_COMMAND, cx);
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
        model.update(cx, |model, _| model.replace(0..0, "racing edit ").unwrap());
        cx.simulate_prompt_answer("Don't Save");

        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::InvalidTarget
        );
        cx.read(|cx| {
            assert!(documents.read(cx).get(document).unwrap().is_dirty(cx));
            assert_eq!(shell.read(cx).workbench.read(cx).view_count(document), 1);
        });
    }

    #[gpui::test]
    async fn scope_change_while_prompting_rejects_window_close(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        model.update(cx, |model, _| model.replace(0..0, "unsaved").unwrap());
        let (shell, window) = product_window(document, model, cx);
        let execution = dispatch_product_command(&shell, window, CLOSE_WINDOW_COMMAND, cx);
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());

        let added = cx.update(|cx| create_untitled_document(&documents, cx));
        let added_model = cx.read(|cx| documents.read(cx).get(added).unwrap().model().clone());
        shell.update(cx, |shell, cx| {
            let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
            shell.workbench.update(cx, |workbench, cx| {
                workbench
                    .open_tab_for_document(pane, added, added_model, cx)
                    .unwrap();
            });
        });
        cx.simulate_prompt_answer("Don't Save");

        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::InvalidTarget
        );
        cx.read(|cx| {
            assert!(cx.windows().contains(&window));
            assert_eq!(shell.read(cx).workbench.read(cx).tab_snapshots().len(), 2);
            assert!(documents.read(cx).get(document).is_some());
            assert!(documents.read(cx).get(added).is_some());
        });
    }

    #[gpui::test]
    async fn concurrent_close_is_rejected_without_a_second_prompt(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        model.update(cx, |model, _| model.replace(0..0, "unsaved").unwrap());
        let (shell, window) = product_window(document, model, cx);
        let target = cx
            .update_window(window, |_, window, cx| {
                shell.update(cx, |shell, cx| {
                    shell.focus_active_editor(window, cx);
                    shell.capture_command_target(window, cx).unwrap()
                })
            })
            .unwrap();
        let dispatcher = cx.read(|cx| cx.global::<ApplicationProductCommands>().0.clone());
        let first = dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: CLOSE_TAB_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                target.clone(),
                cx,
            )
        });
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
        let second = dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: CLOSE_TAB_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                target,
                cx,
            )
        });

        assert_eq!(
            second.completion.await.unwrap(),
            CommandOutcome::Unavailable
        );
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Cancel");
        assert_eq!(first.completion.await.unwrap(), CommandOutcome::Cancelled);
        cx.read(|cx| assert_eq!(shell.read(cx).workbench.read(cx).view_count(document), 1));
    }

    #[gpui::test]
    async fn native_window_close_is_vetoed_until_protected_closure_finishes(
        cx: &mut TestAppContext,
    ) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        model.update(cx, |model, _| model.replace(0..0, "unsaved").unwrap());
        let (shell, window_cx) = cx.add_window_view(|window, cx| {
            let workbench = cx.new(|cx| Workbench::new_for_document(document, model, cx));
            cx.global::<ApplicationWorkbenches>()
                .register(&workbench, window.window_handle());
            ProductShell::new(workbench)
        });
        window_cx.update(|window, cx| {
            install_protected_window_close(&shell, window, cx);
            shell.read(cx).focus_active_editor(window, cx);
        });

        assert!(!window_cx.simulate_close());
        window_cx.run_until_parked();
        assert!(window_cx.has_pending_prompt());
        window_cx.simulate_prompt_answer("Don't Save");
        window_cx.run_until_parked();

        window_cx.read(|cx| {
            assert!(cx.windows().is_empty());
            assert!(documents.read(cx).get(document).is_none());
        });
    }

    #[gpui::test]
    async fn cancelling_the_native_open_selection_settles_the_command(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        cx.set_global(ApplicationOpenDialog(Arc::new(FixedOpenDialog(
            OpenDialogOutcome::Cancelled,
        ))));
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window_handle) = product_window(document, model, cx);
        let target = cx
            .update_window(window_handle, |_, window, cx| {
                shell.update(cx, |shell, cx| {
                    shell.focus_active_editor(window, cx);
                    shell.capture_command_target(window, cx).unwrap()
                })
            })
            .unwrap();
        let execution = cx.update(|cx| {
            let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
            dispatcher.update(cx, |dispatcher, cx| {
                dispatcher.dispatch(
                    Command {
                        name: super::OPEN_COMMAND.into(),
                        arguments: CommandArgumentValue::Null,
                    },
                    target,
                    cx,
                )
            })
        });

        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::Cancelled
        );
        cx.read(|cx| assert_eq!(documents.read(cx).documents().count(), 1));
    }

    #[gpui::test]
    async fn save_creates_a_destination_before_binding_it_as_persisted(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let (filesystems, provider) = memory_filesystems_with_provider();
        cx.set_global(ApplicationFileSystems(filesystems));
        let uri = ResourceUri::parse("mem://product/created.txt").unwrap();
        let model = cx.new(|_| BufferModel::from_text("created text"));
        let document = documents.update(cx, |documents, cx| {
            documents.create_destination("created.txt", model.clone(), uri.clone(), cx)
        });
        let (shell, window_handle) = product_window(document, model, cx);
        let target = cx
            .update_window(window_handle, |_, window, cx| {
                shell.update(cx, |shell, cx| {
                    shell.focus_active_editor(window, cx);
                    shell.capture_command_target(window, cx).unwrap()
                })
            })
            .unwrap();
        let execution = cx.update(|cx| {
            let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
            dispatcher.update(cx, |dispatcher, cx| {
                dispatcher.dispatch(
                    Command {
                        name: SAVE_COMMAND.into(),
                        arguments: CommandArgumentValue::Null,
                    },
                    target,
                    cx,
                )
            })
        });

        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::Completed
        );
        assert_eq!(
            provider.read(uri.clone()).await.unwrap().bytes,
            b"created text"
        );
        cx.read(|cx| {
            let document = documents.read(cx).get(document).unwrap();
            assert_eq!(document.resource_uri(), Some(&uri));
            assert!(matches!(document.state(), DocumentState::Persisted { .. }));
            assert!(!document.is_dirty(cx));
        });
    }

    #[gpui::test]
    async fn edit_racing_save_keeps_the_newer_revision_dirty(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let (filesystems, provider) = memory_filesystems_with_provider();
        cx.set_global(ApplicationFileSystems(filesystems));
        let uri = ResourceUri::parse("mem://product/notes.txt").unwrap();
        let opened = provider.read(uri.clone()).await.unwrap();
        let model = cx.new(|_| BufferModel::from_text("loaded text"));
        let document = documents.update(cx, |documents, cx| {
            documents.create_persisted(
                "notes.txt",
                model.clone(),
                uri.clone(),
                0,
                opened.version,
                cx,
            )
        });
        model.update(cx, |model, _| {
            let len = model.text().len();
            model.replace(0..len, "captured text").unwrap();
        });
        let captured_revision = model.read_with(cx, |model, _| model.revision());
        let (shell, window_handle) = product_window(document, model.clone(), cx);
        let (completion, receiver) = super::CommandCompletion::new();
        cx.update_window(window_handle, |_, window, cx| {
            let target = shell.update(cx, |shell, cx| {
                shell.focus_active_editor(window, cx);
                shell.capture_command_target(window, cx).unwrap()
            });
            shell.update(cx, |shell, cx| {
                shell.start_save_command(false, target, completion, cx)
            });
            model.update(cx, |model, _| {
                let len = model.text().len();
                model.replace(0..len, "newer text").unwrap();
            });
        })
        .unwrap();

        assert_eq!(receiver.await.unwrap(), CommandOutcome::Completed);
        assert_eq!(provider.read(uri).await.unwrap().bytes, b"captured text");
        cx.read(|cx| {
            let document = documents.read(cx).get(document).unwrap();
            assert_eq!(
                document.state().persisted_revision(),
                Some(captured_revision)
            );
            assert!(document.is_dirty(cx));
        });
    }

    #[gpui::test]
    async fn save_completion_rejects_a_closed_captured_tab(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let (filesystems, provider) = memory_filesystems_with_provider();
        cx.set_global(ApplicationFileSystems(filesystems));
        let uri = ResourceUri::parse("mem://product/notes.txt").unwrap();
        let opened = provider.read(uri.clone()).await.unwrap();
        let model = cx.new(|_| BufferModel::from_text("loaded text"));
        let document = documents.update(cx, |documents, cx| {
            documents.create_persisted(
                "notes.txt",
                model.clone(),
                uri.clone(),
                0,
                opened.version,
                cx,
            )
        });
        model.update(cx, |model, _| model.replace(0..0, "changed ").unwrap());
        let (shell, window_handle) = product_window(document, model.clone(), cx);
        let (completion, receiver) = super::CommandCompletion::new();
        cx.update_window(window_handle, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                assert!(shell.split_pane(
                    shell.workbench.read(cx).focused_pane_id().unwrap(),
                    SplitDirection::Horizontal,
                    window,
                    cx,
                ));
            });
            let target = shell.update(cx, |shell, cx| {
                shell.capture_command_target(window, cx).unwrap()
            });
            shell.update(cx, |shell, cx| {
                shell.start_save_command(false, target.clone(), completion, cx);
                assert_eq!(
                    shell.close_tab(
                        target.pane,
                        target.tab,
                        super::target_document(&target).unwrap(),
                        window,
                        cx
                    ),
                    CommandOutcome::Completed
                );
            });
        })
        .unwrap();

        assert_eq!(receiver.await.unwrap(), CommandOutcome::InvalidTarget);
        cx.read(|cx| {
            let document = documents.read(cx).get(document).unwrap();
            assert_eq!(document.state().persisted_revision(), Some(0));
            assert!(document.is_dirty(cx));
        });
    }

    #[gpui::test]
    async fn save_as_overwrites_the_selected_file_and_retargets_only_after_success(
        cx: &mut TestAppContext,
    ) {
        let documents = install_globals(cx);
        let (filesystems, provider) = memory_filesystems_with_provider();
        cx.set_global(ApplicationFileSystems(filesystems));
        let uri = ResourceUri::parse("mem://product/notes.txt").unwrap();
        cx.set_global(ApplicationSaveDialog(Arc::new(FixedSaveDialog(
            SaveDialogOutcome::Selected(uri.clone()),
        ))));
        let model = cx.new(|_| BufferModel::from_text("replacement"));
        let document = cx.update(|cx| {
            documents.update(cx, |documents, cx| {
                documents.create_untitled("Untitled", model.clone(), cx)
            })
        });
        let (shell, window_handle) = product_window(document, model, cx);
        let target = cx
            .update_window(window_handle, |_, window, cx| {
                shell.update(cx, |shell, cx| {
                    shell.focus_active_editor(window, cx);
                    shell.capture_command_target(window, cx).unwrap()
                })
            })
            .unwrap();
        let execution = cx.update(|cx| {
            let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
            dispatcher.update(cx, |dispatcher, cx| {
                dispatcher.dispatch(
                    Command {
                        name: SAVE_AS_COMMAND.into(),
                        arguments: CommandArgumentValue::Null,
                    },
                    target,
                    cx,
                )
            })
        });

        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::Completed
        );
        assert_eq!(
            provider.read(uri.clone()).await.unwrap().bytes,
            b"replacement"
        );
        cx.read(|cx| {
            let document = documents.read(cx).get(document).unwrap();
            assert_eq!(document.resource_uri(), Some(&uri));
            assert_eq!(document.title(), "notes.txt");
        });
    }

    #[gpui::test]
    async fn save_as_cancellation_preserves_untitled_identity(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        cx.set_global(ApplicationSaveDialog(Arc::new(FixedSaveDialog(
            SaveDialogOutcome::Cancelled,
        ))));
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window_handle) = product_window(document, model, cx);
        let target = cx
            .update_window(window_handle, |_, window, cx| {
                shell.update(cx, |shell, cx| {
                    shell.focus_active_editor(window, cx);
                    shell.capture_command_target(window, cx).unwrap()
                })
            })
            .unwrap();
        let execution = cx.update(|cx| {
            let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
            dispatcher.update(cx, |dispatcher, cx| {
                dispatcher.dispatch(
                    Command {
                        name: SAVE_AS_COMMAND.into(),
                        arguments: CommandArgumentValue::Null,
                    },
                    target,
                    cx,
                )
            })
        });

        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::Cancelled
        );
        cx.read(|cx| {
            assert!(matches!(
                documents.read(cx).get(document).unwrap().state(),
                DocumentState::Untitled { .. }
            ));
        });
    }

    #[gpui::test]
    async fn save_as_failure_preserves_text_and_document_identity(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        cx.set_global(ApplicationFileSystems(memory_filesystems()));
        cx.set_global(ApplicationSaveDialog(Arc::new(FixedSaveDialog(
            SaveDialogOutcome::Selected(ResourceUri::parse("mem://product/src").unwrap()),
        ))));
        let model = cx.new(|_| BufferModel::from_text("unsaved text"));
        let document = cx.update(|cx| {
            documents.update(cx, |documents, cx| {
                documents.create_untitled("Untitled", model.clone(), cx)
            })
        });
        let (shell, window_handle) = product_window(document, model.clone(), cx);
        let target = cx
            .update_window(window_handle, |_, window, cx| {
                shell.update(cx, |shell, cx| {
                    shell.focus_active_editor(window, cx);
                    shell.capture_command_target(window, cx).unwrap()
                })
            })
            .unwrap();
        let execution = cx.update(|cx| {
            let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
            dispatcher.update(cx, |dispatcher, cx| {
                dispatcher.dispatch(
                    Command {
                        name: SAVE_AS_COMMAND.into(),
                        arguments: CommandArgumentValue::Null,
                    },
                    target,
                    cx,
                )
            })
        });

        assert!(matches!(
            execution.completion.await.unwrap(),
            CommandOutcome::HandlerFailure { .. }
        ));
        cx.read(|cx| {
            let document = documents.read(cx).get(document).unwrap();
            assert!(matches!(document.state(), DocumentState::Untitled { .. }));
            assert_eq!(document.model(), &model);
            assert_eq!(model.read(cx).text(), "unsaved text");
        });
    }

    #[gpui::test]
    async fn external_save_conflict_offers_reload_and_installs_the_external_version(
        cx: &mut TestAppContext,
    ) {
        let documents = install_globals(cx);
        let (filesystems, provider) = memory_filesystems_with_provider();
        cx.set_global(ApplicationFileSystems(filesystems));
        let uri = ResourceUri::parse("mem://product/notes.txt").unwrap();
        let opened = provider.read(uri.clone()).await.unwrap();
        let model = cx.new(|_| BufferModel::from_text("loaded text"));
        let document = documents.update(cx, |documents, cx| {
            documents.create_persisted(
                "notes.txt",
                model.clone(),
                uri.clone(),
                0,
                opened.version,
                cx,
            )
        });
        model.update(cx, |model, _| model.replace(0..0, "local ").unwrap());
        provider.seed_file(uri.clone(), b"external text").unwrap();
        let (shell, window_handle) = product_window(document, model.clone(), cx);
        let target = cx
            .update_window(window_handle, |_, window, cx| {
                shell.update(cx, |shell, cx| {
                    shell.focus_active_editor(window, cx);
                    shell.capture_command_target(window, cx).unwrap()
                })
            })
            .unwrap();
        let execution = cx.update(|cx| {
            let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
            dispatcher.update(cx, |dispatcher, cx| {
                dispatcher.dispatch(
                    Command {
                        name: SAVE_COMMAND.into(),
                        arguments: CommandArgumentValue::Null,
                    },
                    target,
                    cx,
                )
            })
        });
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Reload");

        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::Completed
        );
        cx.read(|cx| {
            assert_eq!(model.read(cx).text(), "external text");
            assert!(!documents.read(cx).get(document).unwrap().is_dirty(cx));
        });
    }

    #[gpui::test]
    async fn external_conflict_reload_rejects_an_edit_racing_its_read(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let root = ResourceUri::parse("mem://product/").unwrap();
        let provider = Arc::new(MemoryFileSystemProvider::new(root).unwrap());
        let uri = ResourceUri::parse("mem://product/notes.txt").unwrap();
        provider.seed_file(uri.clone(), b"loaded text").unwrap();
        let opened = provider.read(uri.clone()).await.unwrap();
        provider.seed_file(uri.clone(), b"external text").unwrap();
        let (read_release, gated_read) = tokio::sync::oneshot::channel();
        let gated_provider = Arc::new(GatedFileSystemProvider {
            inner: provider.clone(),
            read_release: Mutex::new(Some(gated_read)),
            replace_release: Mutex::new(None),
        });
        let mut filesystems = FileSystemProviderRegistry::new();
        filesystems.register("mem", gated_provider).unwrap();
        cx.set_global(ApplicationFileSystems(Arc::new(filesystems)));

        let model = cx.new(|_| BufferModel::from_text("loaded text"));
        let document = documents.update(cx, |documents, cx| {
            documents.create_persisted(
                "notes.txt",
                model.clone(),
                uri.clone(),
                0,
                opened.version,
                cx,
            )
        });
        model.update(cx, |model, _| model.replace(0..0, "local ").unwrap());
        let (shell, window_handle) = product_window(document, model.clone(), cx);
        let execution = dispatch_product_command(&shell, window_handle, SAVE_COMMAND, cx);
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Reload");
        cx.run_until_parked();

        model.update(cx, |model, _| model.replace(0..0, "newer ").unwrap());
        read_release.send(()).unwrap();

        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::InvalidTarget
        );
        assert_eq!(
            provider.read(uri.clone()).await.unwrap().bytes,
            b"external text"
        );
        cx.read(|cx| {
            let document = documents.read(cx).get(document).unwrap();
            assert_eq!(model.read(cx).text(), "newer local loaded text");
            assert_eq!(document.resource_uri(), Some(&uri));
            assert_eq!(document.state().persisted_revision(), Some(0));
            assert!(document.is_dirty(cx));
        });
    }

    #[gpui::test]
    async fn cancelling_an_external_save_conflict_preserves_the_dirty_document(
        cx: &mut TestAppContext,
    ) {
        let documents = install_globals(cx);
        let (filesystems, provider) = memory_filesystems_with_provider();
        cx.set_global(ApplicationFileSystems(filesystems));
        let uri = ResourceUri::parse("mem://product/notes.txt").unwrap();
        let opened = provider.read(uri.clone()).await.unwrap();
        let model = cx.new(|_| BufferModel::from_text("loaded text"));
        let document = documents.update(cx, |documents, cx| {
            documents.create_persisted(
                "notes.txt",
                model.clone(),
                uri.clone(),
                0,
                opened.version,
                cx,
            )
        });
        model.update(cx, |model, _| model.replace(0..0, "local ").unwrap());
        provider.seed_file(uri.clone(), b"external text").unwrap();
        let (shell, window_handle) = product_window(document, model.clone(), cx);
        let target = cx
            .update_window(window_handle, |_, window, cx| {
                shell.update(cx, |shell, cx| {
                    shell.focus_active_editor(window, cx);
                    shell.capture_command_target(window, cx).unwrap()
                })
            })
            .unwrap();
        let execution = cx.update(|cx| {
            let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
            dispatcher.update(cx, |dispatcher, cx| {
                dispatcher.dispatch(
                    Command {
                        name: SAVE_COMMAND.into(),
                        arguments: CommandArgumentValue::Null,
                    },
                    target,
                    cx,
                )
            })
        });
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Cancel");

        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::Cancelled
        );
        assert_eq!(
            provider.read(uri.clone()).await.unwrap().bytes,
            b"external text"
        );
        cx.read(|cx| {
            let document = documents.read(cx).get(document).unwrap();
            assert_eq!(model.read(cx).text(), "local loaded text");
            assert_eq!(document.resource_uri(), Some(&uri));
            assert!(document.is_dirty(cx));
        });
    }

    #[gpui::test]
    async fn save_as_after_an_external_conflict_retargets_only_after_saving(
        cx: &mut TestAppContext,
    ) {
        let documents = install_globals(cx);
        let (filesystems, provider) = memory_filesystems_with_provider();
        cx.set_global(ApplicationFileSystems(filesystems));
        let original_uri = ResourceUri::parse("mem://product/notes.txt").unwrap();
        let alternate_uri = ResourceUri::parse("mem://product/recovered.txt").unwrap();
        cx.set_global(ApplicationSaveDialog(Arc::new(FixedSaveDialog(
            SaveDialogOutcome::Selected(alternate_uri.clone()),
        ))));
        let opened = provider.read(original_uri.clone()).await.unwrap();
        let model = cx.new(|_| BufferModel::from_text("loaded text"));
        let document = documents.update(cx, |documents, cx| {
            documents.create_persisted(
                "notes.txt",
                model.clone(),
                original_uri.clone(),
                0,
                opened.version,
                cx,
            )
        });
        model.update(cx, |model, _| model.replace(0..0, "local ").unwrap());
        provider
            .seed_file(original_uri.clone(), b"external text")
            .unwrap();
        let (shell, window_handle) = product_window(document, model.clone(), cx);
        let target = cx
            .update_window(window_handle, |_, window, cx| {
                shell.update(cx, |shell, cx| {
                    shell.focus_active_editor(window, cx);
                    shell.capture_command_target(window, cx).unwrap()
                })
            })
            .unwrap();
        let execution = cx.update(|cx| {
            let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
            dispatcher.update(cx, |dispatcher, cx| {
                dispatcher.dispatch(
                    Command {
                        name: SAVE_COMMAND.into(),
                        arguments: CommandArgumentValue::Null,
                    },
                    target,
                    cx,
                )
            })
        });
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
        cx.read(|cx| {
            let document = documents.read(cx).get(document).unwrap();
            assert_eq!(document.resource_uri(), Some(&original_uri));
            assert!(document.is_dirty(cx));
        });
        cx.simulate_prompt_answer("Save As…");

        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::Completed
        );
        assert_eq!(
            provider.read(original_uri).await.unwrap().bytes,
            b"external text"
        );
        assert_eq!(
            provider.read(alternate_uri.clone()).await.unwrap().bytes,
            b"local loaded text"
        );
        cx.read(|cx| {
            let document = documents.read(cx).get(document).unwrap();
            assert_eq!(document.resource_uri(), Some(&alternate_uri));
            assert_eq!(document.title(), "recovered.txt");
            assert!(!document.is_dirty(cx));
        });
    }

    #[gpui::test]
    async fn file_and_missing_file_open_replace_the_launch_placeholder(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        cx.set_global(ApplicationFileSystems(memory_filesystems()));
        let placeholder = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(placeholder).unwrap().model().clone());
        let (shell, _) = product_window(placeholder, model, cx);

        let target = cx.read(|cx| current_open_target(&shell, cx));
        shell.update(cx, |shell, cx| {
            shell.start_open_request(
                OpenRequest::from_uri_for_product(
                    ResourceUri::parse("mem://product/notes.txt").unwrap(),
                ),
                target,
                true,
                None,
                cx,
            );
        });
        cx.run_until_parked();

        cx.read(|cx| {
            let documents = documents.read(cx);
            assert_eq!(documents.documents().count(), 1);
            let document = documents.documents().next().unwrap();
            assert_eq!(document.model().read(cx).text(), "loaded text");
            assert!(matches!(document.state(), DocumentState::Persisted { .. }));
        });

        let target = cx.read(|cx| current_open_target(&shell, cx));
        shell.update(cx, |shell, cx| {
            shell.start_open_request(
                OpenRequest::from_uri_for_product(
                    ResourceUri::parse("mem://product/new.txt").unwrap(),
                ),
                target,
                false,
                None,
                cx,
            );
        });
        cx.run_until_parked();
        cx.read(|cx| {
            let documents = documents.read(cx);
            let id = documents
                .document_for_resource(&ResourceUri::parse("mem://product/new.txt").unwrap())
                .unwrap();
            assert!(matches!(
                documents.get(id).unwrap().state(),
                DocumentState::Destination { .. }
            ));
        });
    }

    #[gpui::test]
    async fn open_deduplicates_resources_and_rejects_stale_completions(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        cx.set_global(ApplicationFileSystems(memory_filesystems()));
        let placeholder = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(placeholder).unwrap().model().clone());
        let (shell, _) = product_window(placeholder, model, cx);
        let target = cx.read(|cx| current_open_target(&shell, cx));

        shell.update(cx, |shell, cx| {
            shell.start_open_request(
                OpenRequest::from_uri_for_product(
                    ResourceUri::parse("mem://product/notes.txt").unwrap(),
                ),
                target.clone(),
                true,
                None,
                cx,
            );
            shell.start_open_request(
                OpenRequest::from_uri_for_product(
                    ResourceUri::parse("mem://product/new.txt").unwrap(),
                ),
                target,
                true,
                None,
                cx,
            );
        });
        cx.run_until_parked();

        cx.read(|cx| {
            let documents = documents.read(cx);
            assert_eq!(documents.documents().count(), 1);
            assert!(
                documents
                    .document_for_resource(&ResourceUri::parse("mem://product/notes.txt").unwrap())
                    .is_none()
            );
            assert!(
                documents
                    .document_for_resource(&ResourceUri::parse("mem://product/new.txt").unwrap())
                    .is_some()
            );
        });

        let target = cx.read(|cx| current_open_target(&shell, cx));
        shell.update(cx, |shell, cx| {
            shell.start_open_request(
                OpenRequest::from_uri_for_product(
                    ResourceUri::parse("mem://product/new.txt").unwrap(),
                ),
                target,
                false,
                None,
                cx,
            );
        });
        cx.run_until_parked();
        cx.read(|cx| {
            assert_eq!(documents.read(cx).documents().count(), 1);
            assert_eq!(
                shell
                    .read(cx)
                    .workbench
                    .read(cx)
                    .focused_pane()
                    .unwrap()
                    .tabs()
                    .len(),
                1
            );
        });
    }

    #[gpui::test]
    async fn folder_open_installs_a_workspace_and_failures_preserve_the_document(
        cx: &mut TestAppContext,
    ) {
        let documents = install_globals(cx);
        cx.set_global(ApplicationFileSystems(memory_filesystems()));
        let placeholder = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(placeholder).unwrap().model().clone());
        let (shell, _) = product_window(placeholder, model, cx);
        let target = cx.read(|cx| current_open_target(&shell, cx));

        shell.update(cx, |shell, cx| {
            shell.start_open_request(
                OpenRequest::from_uri_for_product(
                    ResourceUri::parse("mem://product/invalid.txt").unwrap(),
                ),
                target,
                true,
                None,
                cx,
            );
        });
        cx.run_until_parked();
        cx.read(|cx| {
            assert_eq!(documents.read(cx).documents().count(), 1);
            assert_eq!(
                shell
                    .read(cx)
                    .workbench
                    .read(cx)
                    .focused_pane()
                    .unwrap()
                    .active_tab()
                    .document_id()
                    .unwrap(),
                placeholder
            );
            assert!(shell.read(cx).status.contains("not valid UTF-8"));
        });

        let target = cx.read(|cx| current_open_target(&shell, cx));
        shell.update(cx, |shell, cx| {
            shell.start_open_request(
                OpenRequest::from_uri_for_product(ResourceUri::parse("mem://product/src").unwrap()),
                target,
                false,
                None,
                cx,
            );
        });
        cx.run_until_parked();
        cx.read(|cx| {
            let shell = shell.read(cx);
            assert_eq!(
                shell.workspace.snapshot().root().unwrap().to_string(),
                "mem://product/src"
            );
            assert!(shell.workspace_tree.is_some());
            assert_eq!(documents.read(cx).documents().count(), 1);
        });
    }

    #[gpui::test]
    fn open_from_terminal_requests_picker_and_preserves_terminal(cx: &mut TestAppContext) {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), "opened from terminal").unwrap();
        let documents = install_globals(cx);
        cx.set_global(ApplicationOpenDialog(Arc::new(FixedOpenDialog(
            OpenDialogOutcome::Selected(file.path().to_path_buf()),
        ))));
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window) = product_window(document, model, cx);
        let terminal_tab = shell.update(cx, |shell, cx| {
            let (session, view) = shell.create_terminal(cx);
            let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
            shell.workbench.update(cx, |workbench, _| {
                workbench.open_terminal_tab(pane, session, view).unwrap()
            })
        });
        cx.update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| shell.focus_active_surface(window, cx));
        })
        .unwrap();
        let execution = dispatch_product_command(&shell, window, super::OPEN_COMMAND, cx);
        assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
        cx.read(|cx| {
            let pane = shell.read(cx).workbench.read(cx).focused_pane().unwrap();
            assert!(
                pane.tabs()
                    .iter()
                    .any(|tab| tab.id() == terminal_tab && tab.terminal_view().is_some())
            );
            assert_eq!(
                pane.active_tab()
                    .editor()
                    .unwrap()
                    .read(cx)
                    .model()
                    .read(cx)
                    .text(),
                "opened from terminal"
            );
        });
    }

    #[gpui::test]
    async fn terminal_tab_has_a_captured_surface_and_document_commands_are_unavailable(
        cx: &mut TestAppContext,
    ) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window) = product_window(document, model, cx);
        let (pane, tab) = shell.update(cx, |shell, cx| {
            let (session, view) = shell.create_terminal(cx);
            let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
            let tab = shell.workbench.update(cx, |workbench, _| {
                workbench.open_terminal_tab(pane, session, view).unwrap()
            });
            (pane, tab)
        });
        let session = cx.read(|cx| {
            shell
                .read(cx)
                .workbench
                .read(cx)
                .pane(pane)
                .unwrap()
                .active_tab()
                .surface_id()
        });
        let target = cx
            .update_window(window, |_, window, cx| {
                shell.update(cx, |shell, cx| {
                    shell.activate_tab(pane, tab, window, cx);
                    shell.capture_command_target(window, cx).unwrap()
                })
            })
            .unwrap();
        assert_eq!(target.surface, session);
        cx.read(|cx| {
            assert_eq!(shell.read(cx).workbench.read(cx).view_count(document), 1);
            assert_eq!(
                cx.global::<ApplicationWorkbenches>()
                    .view_count(document, cx),
                1
            );
        });
        let (completion, receiver) = super::CommandCompletion::new();
        shell.update(cx, |shell, cx| {
            shell.start_save_command(false, target.clone(), completion, cx)
        });
        assert_eq!(receiver.await.unwrap(), CommandOutcome::Unavailable);
        let dispatcher = cx.read(|cx| cx.global::<ApplicationProductCommands>().0.clone());
        let result = dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: "editor.move-left".into(),
                    arguments: CommandArgumentValue::Null,
                },
                target.clone(),
                cx,
            )
        });
        assert_eq!(
            result.completion.await.unwrap(),
            CommandOutcome::Unavailable
        );
        let mut wrong_surface = target;
        wrong_surface.surface = TabSurfaceId::Document(document);
        let result = dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: NEW_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                wrong_surface,
                cx,
            )
        });
        assert_eq!(
            result.completion.await.unwrap(),
            CommandOutcome::InvalidTarget
        );
    }

    #[gpui::test]
    async fn closing_terminal_tab_preserves_dirty_document_and_invalidates_its_target(
        cx: &mut TestAppContext,
    ) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        model.update(cx, |model, _| model.replace(0..0, "unsaved").unwrap());
        let (shell, window) = product_window(document, model, cx);
        let (pane, tab) = shell.update(cx, |shell, cx| {
            let (session, view) = shell.create_terminal(cx);
            let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
            let tab = shell.workbench.update(cx, |workbench, _| {
                workbench.open_terminal_tab(pane, session, view).unwrap()
            });
            (pane, tab)
        });
        let stale = cx
            .update_window(window, |_, window, cx| {
                shell.update(cx, |shell, cx| {
                    shell.activate_tab(pane, tab, window, cx);
                    shell.capture_command_target(window, cx).unwrap()
                })
            })
            .unwrap();
        let execution = dispatch_product_command(&shell, window, CLOSE_TAB_COMMAND, cx);
        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::Completed
        );
        assert!(!cx.has_pending_prompt());
        cx.read(|cx| {
            let workbench = shell.read(cx).workbench.clone();
            assert_eq!(workbench.read(cx).tab_snapshots().len(), 1);
            assert_eq!(workbench.read(cx).view_count(document), 1);
            assert!(documents.read(cx).get(document).unwrap().is_dirty(cx));
        });
        let dispatcher = cx.read(|cx| cx.global::<ApplicationProductCommands>().0.clone());
        let result = dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: NEW_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                stale,
                cx,
            )
        });
        assert_eq!(
            result.completion.await.unwrap(),
            CommandOutcome::InvalidTarget
        );
    }

    #[gpui::test]
    async fn mixed_window_and_quit_cancellation_preserve_every_surface(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        model.update(cx, |model, _| model.replace(0..0, "unsaved").unwrap());
        let (shell, window) = product_window(document, model, cx);
        let (pane, tab) = shell.update(cx, |shell, cx| {
            let (session, view) = shell.create_terminal(cx);
            let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
            let tab = shell.workbench.update(cx, |workbench, _| {
                workbench.open_terminal_tab(pane, session, view).unwrap()
            });
            (pane, tab)
        });
        let session = cx.read(|cx| {
            shell
                .read(cx)
                .workbench
                .read(cx)
                .pane(pane)
                .unwrap()
                .active_tab()
                .surface_id()
        });
        cx.update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| shell.activate_tab(pane, tab, window, cx));
        })
        .unwrap();
        for command in [CLOSE_WINDOW_COMMAND, QUIT_COMMAND] {
            let execution = dispatch_product_command(&shell, window, command, cx);
            cx.run_until_parked();
            assert!(cx.has_pending_prompt());
            cx.simulate_prompt_answer("Cancel");
            assert_eq!(
                execution.completion.await.unwrap(),
                CommandOutcome::Cancelled
            );
            cx.read(|cx| {
                assert!(cx.windows().contains(&window));
                let snapshots = shell.read(cx).workbench.read(cx).tab_snapshots();
                assert_eq!(snapshots.len(), 2);
                assert!(
                    snapshots
                        .iter()
                        .any(|tab| tab.surface == TabSurfaceId::Document(document))
                );
                assert!(snapshots.iter().any(|tab| tab.surface == session));
                let TabSurfaceId::Terminal(id) = session else {
                    panic!()
                };
                let registry = cx.global::<ApplicationTerminalSessions>().0.borrow();
                assert_ne!(
                    registry.sessions[&id].read(cx).status(),
                    crate::app::terminal_session::TerminalStatus::Closed
                );
                assert!(documents.read(cx).get(document).unwrap().is_dirty(cx));
            });
        }
    }

    #[gpui::test]
    async fn terminal_command_focuses_a_live_view_and_split_creates_an_independent_session(
        cx: &mut TestAppContext,
    ) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window) = product_window(document, model, cx);

        let command = dispatch_product_command(&shell, window, NEW_TERMINAL_COMMAND, cx);
        assert_eq!(command.completion.await.unwrap(), CommandOutcome::Completed);
        let (pane, first, document_tab) = cx.read(|cx| {
            let shell = shell.read(cx);
            let pane = shell.workbench.read(cx).focused_pane().unwrap();
            (
                pane.id(),
                pane.active_tab().surface_id(),
                pane.tabs()[0].id(),
            )
        });
        let TabSurfaceId::Terminal(first_id) = first else {
            panic!("terminal command opened a document")
        };
        cx.update_window(window, |_, window, cx| {
            let shell = shell.read(cx);
            let pane = shell.workbench.read(cx).pane(pane).unwrap();
            assert!(
                pane.active_tab()
                    .terminal_view()
                    .unwrap()
                    .focus_handle(cx)
                    .contains_focused(window, cx)
            );
        })
        .unwrap();
        cx.read(|cx| {
            assert!(
                cx.global::<ApplicationTerminalSessions>()
                    .0
                    .borrow()
                    .sessions
                    .contains_key(&first_id)
            );
        });

        cx.update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.activate_tab(pane, document_tab, window, cx)
            });
        })
        .unwrap();
        cx.update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                let terminal_tab = shell.workbench.read(cx).pane(pane).unwrap().tabs()[1].id();
                shell.activate_tab(pane, terminal_tab, window, cx);
            });
        })
        .unwrap();
        cx.read(|cx| {
            assert_eq!(
                shell
                    .read(cx)
                    .workbench
                    .read(cx)
                    .pane(pane)
                    .unwrap()
                    .active_tab()
                    .surface_id(),
                first
            );
            assert!(
                cx.global::<ApplicationTerminalSessions>()
                    .0
                    .borrow()
                    .sessions
                    .contains_key(&first_id)
            );
        });

        let split = dispatch_product_command(&shell, window, SPLIT_HORIZONTAL_COMMAND, cx);
        assert_eq!(split.completion.await.unwrap(), CommandOutcome::Completed);
        let second_id = cx.read(|cx| {
            let shell = shell.read(cx);
            let workbench = shell.workbench.read(cx);
            assert_eq!(workbench.panes().len(), 2);
            match workbench.focused_pane().unwrap().active_tab().surface_id() {
                TabSurfaceId::Terminal(id) => id,
                TabSurfaceId::Document(_) | TabSurfaceId::Tool(_) => {
                    panic!("split did not create terminal")
                }
            }
        });
        assert_ne!(first_id, second_id);
        cx.read(|cx| {
            let registry = cx.global::<ApplicationTerminalSessions>().0.borrow();
            assert_eq!(registry.sessions.len(), 2);
            assert!(registry.sessions.contains_key(&first_id));
            assert!(registry.sessions.contains_key(&second_id));
        });
    }

    #[gpui::test]
    async fn terminal_close_and_reopen_settle_the_exact_session(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window) = product_window(document, model, cx);
        assert_eq!(
            dispatch_product_command(&shell, window, NEW_TERMINAL_COMMAND, cx)
                .completion
                .await
                .unwrap(),
            CommandOutcome::Completed
        );
        let (first_id, first_session) = cx.read(|cx| {
            let TabSurfaceId::Terminal(id) = shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .active_tab()
                .surface_id()
            else {
                panic!()
            };
            (
                id,
                cx.global::<ApplicationTerminalSessions>()
                    .0
                    .borrow()
                    .sessions[&id]
                    .clone(),
            )
        });
        assert_eq!(
            dispatch_product_command(&shell, window, CLOSE_TAB_COMMAND, cx)
                .completion
                .await
                .unwrap(),
            CommandOutcome::Completed
        );
        cx.read(|cx| {
            assert_eq!(
                first_session.read(cx).status(),
                crate::app::terminal_session::TerminalStatus::Closed
            );
            assert!(
                !cx.global::<ApplicationTerminalSessions>()
                    .0
                    .borrow()
                    .sessions
                    .contains_key(&first_id)
            );
        });
        assert_eq!(
            dispatch_product_command(&shell, window, NEW_TERMINAL_COMMAND, cx)
                .completion
                .await
                .unwrap(),
            CommandOutcome::Completed
        );
        cx.read(|cx| {
            let TabSurfaceId::Terminal(second_id) = shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .active_tab()
                .surface_id()
            else {
                panic!()
            };
            assert_ne!(first_id, second_id);
            assert!(
                cx.global::<ApplicationTerminalSessions>()
                    .0
                    .borrow()
                    .sessions
                    .contains_key(&second_id)
            );
        });
    }

    #[gpui::test]
    async fn closing_the_final_terminal_tab_installs_an_untitled_workbench(
        cx: &mut TestAppContext,
    ) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window) = product_window(document, model, cx);
        assert_eq!(
            dispatch_product_command(&shell, window, NEW_TERMINAL_COMMAND, cx)
                .completion
                .await
                .unwrap(),
            CommandOutcome::Completed
        );
        let (pane, document_tab, session) = cx.read(|cx| {
            let shell = shell.read(cx);
            let pane = shell.workbench.read(cx).focused_pane().unwrap();
            let TabSurfaceId::Terminal(id) = pane.active_tab().surface_id() else {
                panic!()
            };
            (
                pane.id(),
                pane.tabs()[0].id(),
                cx.global::<ApplicationTerminalSessions>()
                    .0
                    .borrow()
                    .sessions[&id]
                    .clone(),
            )
        });
        cx.update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.activate_tab(pane, document_tab, window, cx)
            });
        })
        .unwrap();
        assert_eq!(
            dispatch_product_command(&shell, window, CLOSE_TAB_COMMAND, cx)
                .completion
                .await
                .unwrap(),
            CommandOutcome::Completed
        );
        cx.read(|cx| {
            let shell = shell.read(cx);
            let tabs = shell.workbench.read(cx).tab_snapshots();
            assert_eq!(tabs.len(), 1);
            assert!(matches!(tabs[0].surface, TabSurfaceId::Terminal(_)));
            assert!(documents.read(cx).get(document).is_none());
        });
        assert_eq!(
            dispatch_product_command(&shell, window, CLOSE_TAB_COMMAND, cx)
                .completion
                .await
                .unwrap(),
            CommandOutcome::Completed
        );
        cx.read(|cx| {
            let shell = shell.read(cx);
            let tabs = shell.workbench.read(cx).tab_snapshots();
            assert_eq!(tabs.len(), 1);
            assert!(matches!(tabs[0].surface, TabSurfaceId::Document(_)));
            assert_eq!(
                session.read(cx).status(),
                crate::app::terminal_session::TerminalStatus::Closed
            );
            assert!(
                cx.global::<ApplicationTerminalSessions>()
                    .0
                    .borrow()
                    .sessions
                    .is_empty()
            );
        });
    }

    #[gpui::test]
    async fn window_close_shuts_down_terminal_after_protected_decisions(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window) = product_window(document, model, cx);
        assert_eq!(
            dispatch_product_command(&shell, window, NEW_TERMINAL_COMMAND, cx)
                .completion
                .await
                .unwrap(),
            CommandOutcome::Completed
        );
        let session = cx.read(|cx| {
            let TabSurfaceId::Terminal(id) = shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .active_tab()
                .surface_id()
            else {
                panic!()
            };
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions[&id]
                .clone()
        });
        assert_eq!(
            dispatch_product_command(&shell, window, CLOSE_WINDOW_COMMAND, cx)
                .completion
                .await
                .unwrap(),
            CommandOutcome::Completed
        );
        cx.read(|cx| {
            assert!(!cx.windows().contains(&window));
            assert_eq!(
                session.read(cx).status(),
                crate::app::terminal_session::TerminalStatus::Closed
            );
            assert!(
                cx.global::<ApplicationTerminalSessions>()
                    .0
                    .borrow()
                    .sessions
                    .is_empty()
            );
        });
    }

    #[gpui::test]
    async fn mixed_window_save_failure_keeps_terminal_running(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        cx.set_global(ApplicationFileSystems(memory_filesystems()));
        cx.set_global(ApplicationSaveDialog(Arc::new(FixedSaveDialog(
            SaveDialogOutcome::Selected(ResourceUri::parse("mem://product/src").unwrap()),
        ))));
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        model.update(cx, |model, _| model.replace(0..0, "unsaved").unwrap());
        let (shell, window) = product_window(document, model, cx);
        assert_eq!(
            dispatch_product_command(&shell, window, NEW_TERMINAL_COMMAND, cx)
                .completion
                .await
                .unwrap(),
            CommandOutcome::Completed
        );
        let (id, session) = cx.read(|cx| {
            let TabSurfaceId::Terminal(id) = shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .active_tab()
                .surface_id()
            else {
                panic!()
            };
            (
                id,
                cx.global::<ApplicationTerminalSessions>()
                    .0
                    .borrow()
                    .sessions[&id]
                    .clone(),
            )
        });
        let execution = dispatch_product_command(&shell, window, CLOSE_WINDOW_COMMAND, cx);
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Save");
        assert!(matches!(
            execution.completion.await.unwrap(),
            CommandOutcome::HandlerFailure { .. }
        ));
        cx.read(|cx| {
            assert!(cx.windows().contains(&window));
            assert_eq!(shell.read(cx).workbench.read(cx).tab_snapshots().len(), 2);
            assert!(
                cx.global::<ApplicationTerminalSessions>()
                    .0
                    .borrow()
                    .sessions
                    .contains_key(&id)
            );
            assert_ne!(
                session.read(cx).status(),
                crate::app::terminal_session::TerminalStatus::Closed
            );
            assert!(documents.read(cx).get(document).unwrap().is_dirty(cx));
        });
    }

    #[gpui::test]
    async fn quit_shuts_down_terminal_sessions_in_every_window(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let first = cx.update(|cx| create_untitled_document(&documents, cx));
        let first_model = cx.read(|cx| documents.read(cx).get(first).unwrap().model().clone());
        let (first_shell, first_window) = product_window(first, first_model, cx);
        let second = cx.update(|cx| create_untitled_document(&documents, cx));
        let second_model = cx.read(|cx| documents.read(cx).get(second).unwrap().model().clone());
        let (second_shell, second_window) = product_window(second, second_model, cx);
        for (shell, window) in [(&first_shell, first_window), (&second_shell, second_window)] {
            assert_eq!(
                dispatch_product_command(shell, window, NEW_TERMINAL_COMMAND, cx)
                    .completion
                    .await
                    .unwrap(),
                CommandOutcome::Completed
            );
        }
        let sessions = cx.read(|cx| {
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions
                .values()
                .cloned()
                .collect::<Vec<_>>()
        });
        assert_eq!(sessions.len(), 2);
        assert_eq!(
            dispatch_product_command(&first_shell, first_window, QUIT_COMMAND, cx)
                .completion
                .await
                .unwrap(),
            CommandOutcome::Completed
        );
        cx.read(|cx| {
            assert!(
                cx.global::<ApplicationTerminalSessions>()
                    .0
                    .borrow()
                    .sessions
                    .is_empty()
            );
            for session in &sessions {
                assert_eq!(
                    session.read(cx).status(),
                    crate::app::terminal_session::TerminalStatus::Closed
                );
            }
        });
    }

    #[gpui::test]
    async fn moving_terminal_keeps_its_session_and_source_window_close_does_not_stop_it(
        cx: &mut TestAppContext,
    ) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (source, source_window) = product_window(document, model, cx);
        assert_eq!(
            dispatch_product_command(&source, source_window, NEW_TERMINAL_COMMAND, cx)
                .completion
                .await
                .unwrap(),
            CommandOutcome::Completed
        );
        let (id, session, grid, source_view) = cx.read(|cx| {
            let shell = source.read(cx);
            let tab = shell
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .active_tab();
            let TabSurfaceId::Terminal(id) = tab.surface_id() else {
                panic!()
            };
            let session = cx
                .global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions[&id]
                .clone();
            let grid = session.read(cx).terminal().unwrap().clone();
            (id, session, grid, tab.terminal_view().unwrap().clone())
        });
        assert_eq!(
            dispatch_product_command(
                &source,
                source_window,
                MOVE_TERMINAL_TO_NEW_WINDOW_COMMAND,
                cx
            )
            .completion
            .await
            .unwrap(),
            CommandOutcome::Completed
        );
        let destination_window = cx.read(|cx| {
            *cx.windows()
                .iter()
                .find(|handle| **handle != source_window)
                .unwrap()
        });
        let destination = cx
            .update_window(destination_window, |_, window, _| {
                window.root::<ProductShell>().flatten().unwrap()
            })
            .unwrap();
        let destination_view = cx.read(|cx| {
            destination
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .active_tab()
                .terminal_view()
                .unwrap()
                .clone()
        });
        cx.update_window(destination_window, |_, window, cx| {
            assert!(
                destination_view
                    .focus_handle(cx)
                    .contains_focused(window, cx)
            );
        })
        .unwrap();
        let destination_size = crate::app::terminal_session::TerminalSize {
            columns: 80,
            lines: 20,
            cell_width: 9,
            cell_height: 18,
        };
        destination_view.update(cx, |view, cx| view.resize(destination_size, cx));
        cx.read(|cx| assert_eq!(session.read(cx).size(), destination_size));
        let stale_size = crate::app::terminal_session::TerminalSize {
            columns: 40,
            lines: 10,
            cell_width: 9,
            cell_height: 18,
        };
        source_view.update(cx, |view, cx| view.resize(stale_size, cx));
        cx.read(|cx| assert_eq!(session.read(cx).size(), destination_size));
        cx.read(|cx| {
            assert!(
                !source
                    .read(cx)
                    .workbench
                    .read(cx)
                    .tab_snapshots()
                    .iter()
                    .any(|tab| tab.surface == TabSurfaceId::Terminal(id))
            );
            let destination_shell = destination.read(cx);
            let tab = destination_shell
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .active_tab();
            assert_eq!(tab.surface_id(), TabSurfaceId::Terminal(id));
            assert_ne!(tab.terminal_view(), Some(&source_view));
            assert!(std::sync::Arc::ptr_eq(
                session.read(cx).terminal().unwrap(),
                &grid
            ));
            assert_eq!(
                cx.global::<ApplicationTerminalSessions>()
                    .0
                    .borrow()
                    .sessions[&id],
                session
            );
        });
        assert_eq!(
            dispatch_product_command(&source, source_window, CLOSE_WINDOW_COMMAND, cx)
                .completion
                .await
                .unwrap(),
            CommandOutcome::Completed
        );
        cx.read(|cx| {
            assert_eq!(
                session.read(cx).status(),
                crate::app::terminal_session::TerminalStatus::Running
            );
            assert!(cx.windows().contains(&destination_window));
        });
        assert_eq!(
            dispatch_product_command(&destination, destination_window, CLOSE_TAB_COMMAND, cx)
                .completion
                .await
                .unwrap(),
            CommandOutcome::Completed
        );
        cx.read(|cx| {
            assert_eq!(
                session.read(cx).status(),
                crate::app::terminal_session::TerminalStatus::Closed
            );
            assert!(
                !cx.global::<ApplicationTerminalSessions>()
                    .0
                    .borrow()
                    .sessions
                    .contains_key(&id)
            );
        });
    }

    #[gpui::test]
    async fn failed_terminal_destination_leaves_source_attached(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window) = product_window(document, model, cx);
        assert_eq!(
            dispatch_product_command(&shell, window, NEW_TERMINAL_COMMAND, cx)
                .completion
                .await
                .unwrap(),
            CommandOutcome::Completed
        );
        let outcome = cx
            .update_window(window, |_, window, cx| {
                shell.update(cx, |shell, cx| {
                    let target = shell.capture_command_target(window, cx).unwrap();
                    shell.move_terminal_to_new_window_with(&target, window, cx, |_| {
                        anyhow::bail!("simulated window creation failure")
                    })
                })
            })
            .unwrap();
        assert!(matches!(outcome, CommandOutcome::HandlerFailure { .. }));
        cx.read(|cx| {
            let shell = shell.read(cx);
            let tab = shell
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .active_tab();
            let TabSurfaceId::Terminal(id) = tab.surface_id() else {
                panic!()
            };
            assert!(tab.terminal_view().is_some());
            assert!(
                cx.global::<ApplicationTerminalSessions>()
                    .0
                    .borrow()
                    .sessions
                    .contains_key(&id)
            );
            assert_eq!(cx.windows().len(), 1);
        });
    }

    #[gpui::test]
    async fn vanished_terminal_destination_leaves_source_attached(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window) = product_window(document, model, cx);
        assert_eq!(
            dispatch_product_command(&shell, window, MOVE_TERMINAL_TO_NEW_WINDOW_COMMAND, cx)
                .completion
                .await
                .unwrap(),
            CommandOutcome::Unavailable
        );
        assert_eq!(
            dispatch_product_command(&shell, window, NEW_TERMINAL_COMMAND, cx)
                .completion
                .await
                .unwrap(),
            CommandOutcome::Completed
        );
        let outcome = cx
            .update_window(window, |_, window, cx| {
                shell.update(cx, |shell, cx| {
                    let target = shell.capture_command_target(window, cx).unwrap();
                    shell.move_terminal_to_new_window_with(&target, window, cx, |cx| {
                        let destination = open_tab_transfer_window(cx)?;
                        cx.update_window(destination.into(), |_, window, _| {
                            window.remove_window()
                        })?;
                        Ok(destination)
                    })
                })
            })
            .unwrap();
        assert_eq!(outcome, CommandOutcome::InvalidTarget);
        cx.read(|cx| {
            let shell = shell.read(cx);
            let tab = shell
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .active_tab();
            let TabSurfaceId::Terminal(id) = tab.surface_id() else {
                panic!()
            };
            assert!(tab.terminal_view().is_some());
            assert!(
                cx.global::<ApplicationTerminalSessions>()
                    .0
                    .borrow()
                    .sessions
                    .contains_key(&id)
            );
            assert_eq!(cx.windows().len(), 1);
        });
    }

    #[gpui::test]
    async fn stale_terminal_move_target_is_rejected(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window) = product_window(document, model, cx);
        assert_eq!(
            dispatch_product_command(&shell, window, NEW_TERMINAL_COMMAND, cx)
                .completion
                .await
                .unwrap(),
            CommandOutcome::Completed
        );
        let target = cx
            .update_window(window, |_, window, cx| {
                shell.update(cx, |shell, cx| {
                    shell.capture_command_target(window, cx).unwrap()
                })
            })
            .unwrap();
        assert_eq!(
            dispatch_product_command(&shell, window, CLOSE_TAB_COMMAND, cx)
                .completion
                .await
                .unwrap(),
            CommandOutcome::Completed
        );
        let command = crate::host::protocol::Command {
            name: MOVE_TERMINAL_TO_NEW_WINDOW_COMMAND.into(),
            arguments: crate::host::protocol::CommandArgumentValue::Null,
        };
        let execution = cx.update(|cx| {
            let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
            dispatcher.update(cx, |dispatcher, cx| {
                dispatcher.dispatch(command, target, cx)
            })
        });
        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::InvalidTarget
        );
        cx.read(|cx| assert_eq!(cx.windows().len(), 1));
    }

    #[gpui::test]
    async fn process_exit_during_terminal_handoff_keeps_final_grid(cx: &mut TestAppContext) {
        use alacritty_terminal::tty::{Options, Shell};
        use std::{
            thread,
            time::{Duration, Instant},
        };

        #[cfg(windows)]
        let shell = Shell::new(
            "cmd.exe".into(),
            vec!["/C".into(), "echo handoff-output & exit /b 7".into()],
        );
        #[cfg(not(windows))]
        let shell = Shell::new(
            "/bin/sh".into(),
            vec![
                "-c".into(),
                "printf 'handoff-output\\n'; sleep 0.05; exit 7".into(),
            ],
        );

        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (source, source_window) = product_window(document, model, cx);
        let session = cx.update(|cx| {
            cx.new(|cx| {
                crate::app::terminal_session::TerminalSession::new_with_options(
                    Options {
                        shell: Some(shell),
                        drain_on_exit: true,
                        ..Default::default()
                    },
                    cx,
                )
            })
        });
        let id = source.update(cx, |shell, cx| {
            let id = cx
                .global::<ApplicationTerminalSessions>()
                .0
                .borrow_mut()
                .allocate_id();
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow_mut()
                .sessions
                .insert(id, session.clone());
            let view =
                cx.new(|cx| crate::app::terminal_view::TerminalView::new(session.clone(), cx));
            let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
            shell.workbench.update(cx, |workbench, _| {
                workbench.open_terminal_tab(pane, id, view).unwrap()
            });
            id
        });
        let outcome = cx
            .update_window(source_window, |_, window, cx| {
                source.update(cx, |shell, cx| {
                    shell.focus_active_surface(window, cx);
                    let target = shell.capture_command_target(window, cx).unwrap();
                    shell.move_terminal_to_new_window_with(&target, window, cx, |cx| {
                        thread::sleep(Duration::from_millis(100));
                        open_tab_transfer_window(cx)
                    })
                })
            })
            .unwrap();
        assert_eq!(outcome, CommandOutcome::Completed);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            cx.run_until_parked();
            if cx.read(|cx| {
                matches!(
                    session.read(cx).status(),
                    crate::app::terminal_session::TerminalStatus::Exited(_)
                )
            }) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "terminal did not exit after handoff"
            );
            thread::sleep(Duration::from_millis(5));
        }
        cx.read(|cx| {
            assert!(matches!(
                session.read(cx).status(),
                crate::app::terminal_session::TerminalStatus::Exited(_)
            ));
            let terminal = session.read(cx).terminal().unwrap().lock();
            let output: String = terminal
                .renderable_content()
                .display_iter
                .map(|cell| cell.cell.c)
                .collect();
            assert!(output.contains("handoff-output"));
            assert!(
                cx.global::<ApplicationTerminalSessions>()
                    .0
                    .borrow()
                    .sessions
                    .contains_key(&id)
            );
        });
    }
}
