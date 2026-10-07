//! Native product windows backed by application documents and workbenches.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    future::Future,
    path::Path,
    pin::Pin,
    sync::{Arc, OnceLock},
};

use gpui::{prelude::FluentBuilder, *};

use super::{
    CommandCompletion, CommandPalette, CommandPaletteEntry, CommandPaletteEvent,
    documents::{ApplicationDocuments, DocumentCollection, DocumentId, DocumentState},
    entry::OpenRequest,
    filesystem::{
        FileSystemProviderRegistry, LocalFileSystemProvider, ResourceError, ResourceKind,
        ResourceStat, ResourceVersion,
    },
    keymaps::{ApplicationKeymaps, BindingResolution, KeymapAction},
    model::BufferModel,
    open::{OpenResource, enumerate_directory, load_resource},
    product_commands::{
        ApplicationProductCommands, CLOSE_TAB_COMMAND, CLOSE_WINDOW_COMMAND, COPY_COMMAND,
        CUT_COMMAND, CommandClaim, FIND_COMMAND, FIND_NEXT_COMMAND, FIND_PREVIOUS_COMMAND,
        MOVE_TERMINAL_TO_NEW_WINDOW_COMMAND, NEW_COMMAND, NEW_TERMINAL_COMMAND, NEW_WINDOW_COMMAND,
        OPEN_COMMAND, PASTE_COMMAND, ProductCommandDispatcher, ProductCommandSource,
        ProductCommandTarget, QUIT_COMMAND, REDO_COMMAND, SAVE_AS_COMMAND, SAVE_COMMAND,
        SELECT_ALL_COMMAND, SHOW_COMMAND_PALETTE_COMMAND, SHOW_COMPLETIONS_COMMAND,
        SHOW_EXTENSION_REPORT_COMMAND, SPLIT_HORIZONTAL_COMMAND, SPLIT_VERTICAL_COMMAND,
        UNDO_COMMAND, dispatch_close_to_captured_target, dispatch_open_to_captured_target,
        dispatch_save_to_captured_target,
    },
    terminal_session::TerminalSession,
    terminal_view::TerminalView,
    workbench::{
        CloseRequestOutcome, DocumentCloseDisposition, PaneId, SplitDirection, SplitPlacement,
        TabId, TabSurfaceId, TerminalSessionId, Workbench, WorkbenchLayout,
    },
    workspace::WorkspaceState,
    workspace_tree::{
        WorkspaceTree, WorkspaceTreeEvent, WorkspaceTreeRequest, WorkspaceTreeResponse,
    },
};

struct ApplicationWorkbenches(RefCell<Vec<WeakEntity<Workbench>>>);

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
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct ProtectedViewKey {
    window: AnyWindowHandle,
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
    fn register(&self, workbench: &Entity<Workbench>) {
        self.0.borrow_mut().push(workbench.downgrade());
    }

    fn view_count(&self, document: DocumentId, cx: &App) -> usize {
        let mut workbenches = self.0.borrow_mut();
        workbenches.retain(|workbench| workbench.upgrade().is_some());
        workbenches
            .iter()
            .filter_map(WeakEntity::upgrade)
            .map(|workbench| workbench.read(cx).view_count(document))
            .sum()
    }
}

#[derive(Clone)]
struct OpenTarget {
    workbench: WeakEntity<Workbench>,
    pane: PaneId,
    tab: TabId,
    document: DocumentId,
}

impl OpenTarget {
    fn from_command(target: &ProductCommandTarget) -> Option<Self> {
        let TabSurfaceId::Document(document) = target.surface else {
            return None;
        };
        Some(Self {
            workbench: target.workbench.clone(),
            pane: target.pane,
            tab: target.tab,
            document,
        })
    }
}

fn target_document(target: &ProductCommandTarget) -> Option<DocumentId> {
    match target.surface {
        TabSurfaceId::Document(document) => Some(document),
        TabSurfaceId::Terminal(_) => None,
    }
}

pub(crate) struct ProductShell {
    workbench: Entity<Workbench>,
    status: SharedString,
    command_palette: Option<Entity<CommandPalette<ProductCommandTarget>>>,
    command_palette_subscription: Option<Subscription>,
    workspace: Option<WorkspaceState>,
    workspace_tree: Option<Entity<WorkspaceTree>>,
    workspace_tree_subscription: Option<Subscription>,
    extension_tree: Option<Entity<super::tree_view::TreeView>>,
    extension_tree_subscription: Option<Subscription>,
    extension_tree_observer: Option<Subscription>,
    extension_tree_release: Option<Subscription>,
    extension_tree_window_closed: Option<Subscription>,
    open_generation: u64,
    tasks: Vec<Task<()>>,
    extension_report_open: bool,
    extension_report_subscription: Option<Subscription>,
}

impl ProductShell {
    fn new(workbench: Entity<Workbench>) -> Self {
        Self {
            workbench,
            status: "ready".into(),
            command_palette: None,
            command_palette_subscription: None,
            workspace: None,
            workspace_tree: None,
            workspace_tree_subscription: None,
            extension_tree: None,
            extension_tree_subscription: None,
            extension_tree_observer: None,
            extension_tree_release: None,
            extension_tree_window_closed: None,
            open_generation: 0,
            tasks: Vec::new(),
            extension_report_open: false,
            extension_report_subscription: None,
        }
    }

    fn observe_extension_host(&mut self, window: gpui::AnyWindowHandle, cx: &mut Context<Self>) {
        if let Some(host) = super::extension_host::entity(cx) {
            self.extension_report_subscription = Some(cx.observe(&host, |_, _, cx| cx.notify()));
            let tree = cx.new(|cx| super::tree_view::TreeView::new("outline", cx));
            let view = tree.read(cx).instance_id();
            self.extension_tree_subscription = Some(cx.subscribe(&tree, |_, _, event, cx| {
                let event = event.clone();
                cx.defer(move |cx| super::extension_host::route_tree_event(&event, cx));
                cx.notify();
            }));
            self.extension_tree_observer = Some(cx.observe(&tree, |_, _, cx| cx.notify()));
            self.extension_tree_release = Some(cx.on_release(move |_, cx| {
                super::extension_host::detach_tree_view(view, cx);
            }));
            self.extension_tree_window_closed = Some(cx.on_window_closed(move |cx| {
                if !cx.windows().contains(&window) {
                    super::extension_host::detach_tree_view(view, cx);
                }
            }));
            super::extension_host::attach_tree_view(tree.clone(), cx);
            self.extension_tree = Some(tree);
        }
    }

    pub(crate) fn extension_command_view(
        &self,
        id: crate::host::protocol::ViewId,
        cx: &App,
    ) -> Option<Entity<super::tree_view::TreeView>> {
        self.extension_tree
            .as_ref()
            .filter(|view| view.read(cx).instance_id() == id)
            .cloned()
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
        let Some(tab) = self
            .workbench
            .read(cx)
            .focused_pane()
            .map(|pane| pane.active_tab())
        else {
            return;
        };
        if let Some(editor) = tab.editor() {
            editor.focus_handle(cx).focus(window);
        }
        if let Some(view) = tab.terminal_view() {
            view.focus_handle(cx).focus(window);
        }
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
        let view = self
            .extension_tree
            .as_ref()
            .and_then(|tree| (tree.focus_handle(cx) == focus).then(|| tree.read(cx).instance_id()));
        Some(ProductCommandTarget {
            window: window.window_handle(),
            shell: cx.entity().downgrade(),
            workbench: self.workbench.downgrade(),
            pane: pane.id(),
            tab: tab.id(),
            surface: tab.surface_id(),
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
            focus: tab
                .editor()
                .map(|editor| editor.focus_handle(cx).downgrade())
                .or_else(|| {
                    tab.terminal_view()
                        .map(|view| view.focus_handle(cx).downgrade())
                })
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
        let view_kind = if self
            .workspace_tree
            .as_ref()
            .is_some_and(|tree| Some(tree.focus_handle(cx)) == target.focus.upgrade())
        {
            Some("workspace-tree".to_owned())
        } else if target.view.is_some() {
            self.extension_tree.as_ref().and_then(|tree| {
                (Some(tree.read(cx).instance_id()) == target.view
                    && Some(tree.focus_handle(cx)) == target.focus.upgrade())
                .then(|| tree.read(cx).kind().to_owned())
            })
        } else {
            target.command_view(cx).and_then(|view| {
                view.matches_focus(&target.focus, cx).then(|| match view {
                    super::workbench::CommandView::Editor(_) => "editor".to_owned(),
                    super::workbench::CommandView::Terminal(_) => "terminal".to_owned(),
                    super::workbench::CommandView::Extension(view) => {
                        view.read(cx).kind().to_owned()
                    }
                })
            })
        };
        let resolution = cx
            .global::<ApplicationKeymaps>()
            .0
            .resolve(&action.key, view_kind.as_deref());
        match resolution {
            Some(BindingResolution::Command(command)) => self.dispatch_command(command, target, cx),
            Some(BindingResolution::Unbound) => {}
            None => cx.propagate(),
        }
    }

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
                cx.notify();
            },
        );
        window.focus(&palette.focus_handle(cx));
        self.command_palette = Some(palette);
        self.command_palette_subscription = Some(subscription);
        cx.notify();
    }

    fn sync_focused_pane(&self, window: &Window, cx: &mut Context<Self>) {
        if let Some(pane_id) = self.workbench.read(cx).panes().iter().find_map(|pane| {
            pane.tabs().iter().find_map(|tab| {
                tab.terminal_view()
                    .filter(|view| view.focus_handle(cx).contains_focused(window, cx))
                    .map(|_| pane.id())
            })
        }) {
            self.workbench.update(cx, |workbench, _| {
                workbench.focus_pane(pane_id);
            });
            return;
        }
        let focused_pane = self.workbench.read(cx).panes().iter().find_map(|pane| {
            pane.tabs()
                .iter()
                .any(|tab| {
                    tab.editor()
                        .is_some_and(|editor| editor.focus_handle(cx).contains_focused(window, cx))
                })
                .then_some(pane.id())
        });
        if let Some(focused_pane) = focused_pane {
            self.workbench.update(cx, |workbench, _| {
                workbench.focus_pane(focused_pane);
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
    ) -> Result<ProtectedClosePlan, crate::host::protocol::CommandOutcome> {
        use crate::host::protocol::CommandOutcome;

        let views = Self::collect_product_views(cx);
        let origin_key = ProtectedViewKey {
            window: request.origin.window,
            pane: request.origin.pane,
            tab: request.origin.tab,
            surface: request.origin.surface,
        };
        if !views.iter().any(|view| view.key == origin_key) {
            return Err(CommandOutcome::InvalidTarget);
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
            return Err(CommandOutcome::InvalidTarget);
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
        target.workbench.upgrade() == Some(self.workbench.clone())
            && self
                .workbench
                .read(cx)
                .contains_surface(target.pane, target.tab, target.surface)
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
                        let committed = Self::documents(cx).update(cx, |documents, _| {
                            documents.finish_persistence(
                                target_document(&target).expect("save target is a document"),
                                &model,
                                &capture,
                                title,
                                uri.clone(),
                                (version, revision),
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
                                    if Self::documents(cx).update(cx, |documents, _| {
                                        documents.finish_persistence(
                                            target_document(&target)
                                                .expect("save target is a document"),
                                            &model,
                                            &capture,
                                            title,
                                            uri.clone(),
                                            (version, loaded_revision),
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
        let Some(open_target) = OpenTarget::from_command(&target) else {
            self.finish_open_command(
                completion,
                crate::host::protocol::CommandOutcome::Unavailable,
                cx,
            );
            return;
        };
        let selection: OpenDialogFuture =
            if let Some(dialog) = cx.try_global::<ApplicationOpenDialog>() {
                dialog.0.select()
            } else {
                let selection = cx.prompt_for_paths(PathPromptOptions {
                    files: true,
                    directories: true,
                    multiple: false,
                    prompt: Some("Open".into()),
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
        if target.workbench.upgrade() != Some(self.workbench.clone())
            || !self
                .workbench
                .read(cx)
                .contains_tab(target.pane, target.tab, target.document)
        {
            if let Some(completion) = completion {
                self.finish_open_command(
                    completion,
                    crate::host::protocol::CommandOutcome::InvalidTarget,
                    cx,
                );
            }
            return;
        }
        let generation = self.next_open_generation();
        let uri = request.uri().clone();
        self.status = format!("opening {uri}").into();
        let providers = Self::filesystems(cx);
        let task = cx.spawn(async move |this, cx| {
            let result = load_resource(providers, uri).await;
            let fallback = completion.clone();
            if this
                .update(cx, |this, cx| {
                    let outcome = if this.open_generation != generation
                        || target.workbench.upgrade() != Some(this.workbench.clone())
                        || !this.workbench.read(cx).contains_tab(
                            target.pane,
                            target.tab,
                            target.document,
                        ) {
                        crate::host::protocol::CommandOutcome::InvalidTarget
                    } else {
                        this.apply_open_result(result, &target, replace_target, cx)
                    };
                    if let Some(completion) = completion {
                        this.finish_open_command(completion, outcome, cx);
                    }
                })
                .is_err()
                && let Some(completion) = fallback
            {
                completion.complete(crate::host::protocol::CommandOutcome::InvalidTarget);
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
    ) -> crate::host::protocol::CommandOutcome {
        use crate::host::protocol::CommandOutcome;

        match result {
            Ok(OpenResource::Directory(root)) => {
                self.install_workspace(root, cx);
                CommandOutcome::Completed
            }
            Ok(OpenResource::Missing(uri)) => {
                let documents = Self::documents(cx);
                let model = cx.new(|_| BufferModel::from_text(""));
                let title = resource_title(&uri);
                let document = documents.update(cx, |documents, _| {
                    documents.create_destination(title, model, uri.clone())
                });
                self.present_open_document(document, target, replace_target, cx);
                self.status = format!("new file {uri}").into();
                cx.notify();
                CommandOutcome::Completed
            }
            Ok(OpenResource::File { uri, text, version }) => {
                let documents = Self::documents(cx);
                let document =
                    if let Some(document) = documents.read(cx).document_for_resource(&uri) {
                        document
                    } else {
                        let model = cx.new(|_| BufferModel::from_text(text));
                        documents.update(cx, |documents, _| {
                            documents.create_persisted(
                                resource_title(&uri),
                                model,
                                uri.clone(),
                                0,
                                version,
                            )
                        })
                    };
                self.present_open_document(document, target, replace_target, cx);
                self.status = format!("opened {uri}").into();
                cx.notify();
                CommandOutcome::Completed
            }
            Err(error) => {
                self.status = format!("open failed: {error}").into();
                cx.notify();
                CommandOutcome::HandlerFailure {
                    message: error.to_string(),
                }
            }
        }
    }

    fn present_open_document(
        &mut self,
        document: DocumentId,
        target: &OpenTarget,
        replace_target: bool,
        cx: &mut Context<Self>,
    ) {
        let existing = self.workbench.read(cx).panes().iter().find_map(|pane| {
            pane.tabs()
                .iter()
                .find(|tab| tab.document_id() == Some(document))
                .map(|tab| (pane.id(), tab.id(), tab.editor().unwrap().clone()))
        });
        if let Some((pane, tab, _)) = existing {
            self.workbench.update(cx, |workbench, _| {
                workbench.activate_tab(pane, tab);
            });
            return;
        }

        let documents = Self::documents(cx);
        let model = documents.read(cx).get(document).unwrap().model().clone();
        if replace_target {
            let replaced = self.workbench.update(cx, |workbench, cx| {
                workbench.replace_tab_document(target.pane, target.tab, document, model, cx)
            });
            if replaced && target.document != document {
                documents.update(cx, |documents, _| {
                    documents.remove(target.document);
                });
            }
        } else {
            self.workbench.update(cx, |workbench, cx| {
                workbench.open_tab_for_document(target.pane, document, model, cx);
            });
        }
    }

    fn install_workspace(&mut self, root: super::resource::ResourceUri, cx: &mut Context<Self>) {
        let workspace = match &mut self.workspace {
            Some(workspace) => workspace.replace_root(root),
            None => {
                let workspace = WorkspaceState::new(root);
                let snapshot = workspace.snapshot();
                self.workspace = Some(workspace);
                snapshot
            }
        };
        if let Some(tree) = &self.workspace_tree {
            tree.update(cx, |tree, cx| tree.set_workspace(workspace, cx));
        } else {
            let tree = cx.new(|cx| WorkspaceTree::new(workspace, cx));
            self.workspace_tree_subscription = Some(cx.subscribe(
                &tree,
                |this, _tree, event: &WorkspaceTreeEvent, cx| {
                    this.handle_workspace_tree_event(event.clone(), cx);
                },
            ));
            let load = tree.clone();
            cx.defer(move |cx| load.update(cx, |tree, cx| tree.load(cx)));
            self.workspace_tree = Some(tree);
        }
        self.status = "workspace opened".into();
        cx.notify();
    }

    fn handle_workspace_tree_event(&mut self, event: WorkspaceTreeEvent, cx: &mut Context<Self>) {
        match event {
            WorkspaceTreeEvent::RequestChildren(request) => {
                self.enumerate_workspace(request, cx);
            }
            WorkspaceTreeEvent::OpenFile { workspace, uri } => {
                if !self
                    .workspace
                    .as_ref()
                    .is_some_and(|current| current.is_current(&workspace))
                {
                    return;
                }
                let Some(pane) = self.workbench.read(cx).focused_pane() else {
                    return;
                };
                let tab = pane.active_tab();
                let Some(document) = tab.document_id() else {
                    return;
                };
                let target = OpenTarget {
                    workbench: self.workbench.downgrade(),
                    pane: pane.id(),
                    tab: tab.id(),
                    document,
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
        if !self
            .workspace
            .as_ref()
            .is_some_and(|workspace| workspace.is_current(&request.workspace))
        {
            return;
        }
        let providers = Self::filesystems(cx);
        let task = cx.spawn(async move |this, cx| {
            let result = enumerate_directory(providers, request.parent.clone()).await;
            let _ = this.update(cx, |this, cx| {
                if !this
                    .workspace
                    .as_ref()
                    .is_some_and(|workspace| workspace.is_current(&request.workspace))
                {
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
    ) -> bool {
        self.next_open_generation();
        if self.workbench.read(cx).pane(pane).is_none() {
            return false;
        }
        let documents = Self::documents(cx);
        let model = cx.new(|_| BufferModel::from_text(""));
        let document = documents.update(cx, |documents, cx| {
            documents.create_untitled("Untitled", model, cx)
        });
        let model = documents.read(cx).get(document).unwrap().model().clone();
        let editor = self.workbench.update(cx, |workbench, cx| {
            workbench.open_tab_for_document(pane, document, model, cx);
            workbench
                .pane(pane)
                .unwrap()
                .active_tab()
                .editor()
                .unwrap()
                .clone()
        });
        editor.focus_handle(cx).focus(window);
        self.status = "new untitled document".into();
        cx.notify();
        true
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
                workbench.open_terminal_tab(pane, id, view)
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
        use crate::host::protocol::CommandOutcome;

        if Self::protected_closure_is_active(cx) {
            Self::finish_protected_close_completion(completion, CommandOutcome::Unavailable, cx);
            return;
        }
        let plan =
            match Self::build_protected_close_plan(ProtectedCloseRequest { kind, origin }, cx) {
                Ok(plan) => plan,
                Err(outcome) => {
                    Self::finish_protected_close_completion(completion, outcome, cx);
                    return;
                }
            };
        let Some(shell) = plan.request.origin.shell.upgrade() else {
            Self::finish_protected_close_completion(completion, CommandOutcome::InvalidTarget, cx);
            return;
        };
        Self::set_protected_closure_active(true, cx);
        shell.update(cx, |shell, cx| {
            shell.start_protected_close_plan(plan, completion, cx);
        });
    }

    fn start_protected_close_plan(
        &mut self,
        plan: ProtectedClosePlan,
        completion: Option<CommandCompletion>,
        cx: &mut Context<Self>,
    ) {
        use crate::host::protocol::CommandOutcome;

        cx.spawn(async move |this, cx| {
            let mut approvals = Vec::new();
            let mut outcome = CommandOutcome::Completed;
            for (document, target) in &plan.prompts {
                let prompt = cx.update(|cx| {
                    let documents = Self::documents(cx);
                    let Some(document_state) = documents.read(cx).get(*document) else {
                        return Err(CommandOutcome::InvalidTarget);
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
                    .unwrap_or(Err(CommandOutcome::InvalidTarget))
                });
                let (prompt, prompted_revision) = match prompt {
                    Ok(Ok(Some(prompt))) => prompt,
                    Ok(Ok(None)) => continue,
                    Ok(Err(error)) => {
                        outcome = error;
                        break;
                    }
                    Err(_) => {
                        outcome = CommandOutcome::InvalidTarget;
                        break;
                    }
                };
                match prompt.await.unwrap_or(2) {
                    0 => {
                        let save = cx.update(|cx| {
                            let Some(shell) = target.shell.upgrade() else {
                                return Err(CommandOutcome::InvalidTarget);
                            };
                            let (save_completion, receiver) = CommandCompletion::new();
                            let result = cx.update_window(target.window, |_, window, cx| {
                                if window.root::<ProductShell>().flatten().as_ref() != Some(&shell)
                                {
                                    return Err(CommandOutcome::InvalidTarget);
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
                            result.unwrap_or(Err(CommandOutcome::InvalidTarget))
                        });
                        let save_outcome = match save {
                            Ok(Ok(receiver)) => receiver.await.unwrap_or(CommandOutcome::Cancelled),
                            Ok(Err(error)) => error,
                            Err(_) => CommandOutcome::InvalidTarget,
                        };
                        if save_outcome != CommandOutcome::Completed {
                            outcome = save_outcome;
                            break;
                        }
                        let approval = cx.update(|cx| {
                            let documents = Self::documents(cx);
                            let Some(document_state) = documents.read(cx).get(*document) else {
                                return Err(CommandOutcome::InvalidTarget);
                            };
                            if document_state.is_dirty(cx) {
                                return Err(CommandOutcome::HandlerFailure {
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
                                outcome = error;
                                break;
                            }
                            Err(_) => {
                                outcome = CommandOutcome::InvalidTarget;
                                break;
                            }
                        }
                    }
                    1 => {
                        let approval = cx.update(|cx| {
                            let documents = Self::documents(cx);
                            let Some(document_state) = documents.read(cx).get(*document) else {
                                return Err(CommandOutcome::InvalidTarget);
                            };
                            if document_state.model().read(cx).revision() != prompted_revision {
                                return Err(CommandOutcome::InvalidTarget);
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
                                outcome = error;
                                break;
                            }
                            Err(_) => {
                                outcome = CommandOutcome::InvalidTarget;
                                break;
                            }
                        }
                    }
                    _ => {
                        outcome = CommandOutcome::Cancelled;
                        break;
                    }
                }
            }
            let current_views = if outcome == CommandOutcome::Completed {
                cx.update(Self::collect_product_views).ok()
            } else {
                None
            };
            let fallback = completion.clone();
            if this
                .update(cx, |this, cx| {
                    if outcome == CommandOutcome::Completed {
                        outcome = match current_views {
                            Some(ref views) => {
                                this.commit_protected_close(&plan, &approvals, views, cx)
                            }
                            None => CommandOutcome::InvalidTarget,
                        };
                    }
                    Self::set_protected_closure_active(false, cx);
                    this.finish_protected_close(completion, outcome, cx);
                })
                .is_err()
            {
                let _ = cx.update(|cx| {
                    Self::set_protected_closure_active(false, cx);
                    Self::finish_protected_close_completion(
                        fallback,
                        CommandOutcome::InvalidTarget,
                        cx,
                    );
                });
            }
        })
        .detach();
    }

    fn finish_protected_close(
        &mut self,
        completion: Option<CommandCompletion>,
        outcome: crate::host::protocol::CommandOutcome,
        cx: &mut Context<Self>,
    ) {
        Self::finish_protected_close_completion(completion, outcome, cx);
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
    ) -> crate::host::protocol::CommandOutcome {
        use crate::host::protocol::CommandOutcome;

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
            return CommandOutcome::InvalidTarget;
        }
        let documents = Self::documents(cx);
        for approval in approvals {
            let Some(document) = documents.read(cx).get(approval.document) else {
                return CommandOutcome::InvalidTarget;
            };
            if document.model().read(cx).revision() != approval.revision
                || (approval.kind == CloseApprovalKind::Saved && document.is_dirty(cx))
            {
                return CommandOutcome::InvalidTarget;
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
                    return CommandOutcome::InvalidTarget;
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
                match cx.update_window(plan.request.origin.window, |_, window, _| {
                    window.remove_window();
                }) {
                    Ok(()) => {
                        documents.update(cx, |documents, _| {
                            for document in final_documents {
                                documents.remove(document);
                            }
                        });
                        Self::close_terminal_ids(
                            current_scope.iter().filter_map(|key| match key.surface {
                                TabSurfaceId::Terminal(id) => Some(id),
                                TabSurfaceId::Document(_) => None,
                            }),
                            cx,
                        );
                        CommandOutcome::Completed
                    }
                    Err(_) => CommandOutcome::InvalidTarget,
                }
            }
            ProtectedCloseKind::Quit => {
                Self::close_terminal_ids(
                    current_scope.iter().filter_map(|key| match key.surface {
                        TabSurfaceId::Terminal(id) => Some(id),
                        TabSurfaceId::Document(_) => None,
                    }),
                    cx,
                );
                cx.quit();
                CommandOutcome::Completed
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
            documents.update(cx, |documents, _| {
                documents.remove(document);
            });
        }
        if transition.workbench_is_empty() {
            let replacement = create_untitled_document(&documents, cx);
            let model = documents.read(cx).get(replacement).unwrap().model().clone();
            let workbench = cx.new(|cx| Workbench::new_for_document(replacement, model, cx));
            cx.global::<ApplicationWorkbenches>().register(&workbench);
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
    ) -> crate::host::protocol::CommandOutcome {
        use crate::app::workbench::CloseConfirmation;
        use crate::host::protocol::CommandOutcome;

        if !self.workbench.read(cx).contains_surface(pane, tab, surface) {
            return CommandOutcome::InvalidTarget;
        }
        let TabSurfaceId::Document(document) = surface else {
            let TabSurfaceId::Terminal(session_id) = surface else {
                unreachable!()
            };
            let outcome = self.workbench.update(cx, |workbench, _| {
                workbench.request_close_terminal_tab(pane, tab, session_id)
            });
            let Some(transition) = outcome else {
                return CommandOutcome::InvalidTarget;
            };
            self.close_terminal(session_id, cx);
            if transition.workbench_is_empty() {
                self.replace_empty_workbench(cx);
            }
            let _ = cx.update_window(window_handle, |_, window, cx| {
                self.focus_active_surface(window, cx)
            });
            cx.notify();
            return CommandOutcome::Completed;
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
            None => return CommandOutcome::InvalidTarget,
        };
        if transition.document == Some(DocumentCloseDisposition::CloseRequested) {
            documents.update(cx, |documents, _| {
                documents.remove(document);
            });
        }
        if transition.workbench_is_empty() {
            self.replace_empty_workbench(cx);
        } else {
            self.status = "closed tab".into();
        }
        let editor = self
            .workbench
            .read(cx)
            .focused_pane()
            .and_then(|pane| pane.active_tab().editor().cloned());
        if let Some(editor) = editor {
            let _ = cx.update_window(window_handle, move |_, window, cx| {
                editor.focus_handle(cx).focus(window);
            });
        }
        cx.notify();
        CommandOutcome::Completed
    }

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

    fn replace_empty_workbench(&mut self, cx: &mut App) {
        let documents = Self::documents(cx);
        let replacement = create_untitled_document(&documents, cx);
        let model = documents.read(cx).get(replacement).unwrap().model().clone();
        let workbench = cx.new(|cx| Workbench::new_for_document(replacement, model, cx));
        cx.global::<ApplicationWorkbenches>().register(&workbench);
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

        let TabSurfaceId::Terminal(session_id) = target.surface else {
            return CommandOutcome::Unavailable;
        };
        let source_view = self
            .workbench
            .read(cx)
            .pane(target.pane)
            .and_then(|pane| pane.tabs().iter().find(|tab| tab.id() == target.tab))
            .and_then(|tab| tab.terminal_view().cloned());
        let Some(source_view) = source_view else {
            return CommandOutcome::InvalidTarget;
        };
        let session = cx
            .global::<ApplicationTerminalSessions>()
            .0
            .borrow()
            .sessions
            .get(&session_id)
            .cloned();
        let Some(session) = session else {
            return CommandOutcome::InvalidTarget;
        };

        let Ok(destination) = create_destination(cx) else {
            self.status = "could not create terminal window".into();
            cx.notify();
            return CommandOutcome::HandlerFailure {
                message: "could not create terminal window".into(),
            };
        };
        let destination_handle = destination.into();
        let result = cx.update_window(destination_handle, |_, destination_window, cx| {
            let Some(destination_shell) = destination_window.root::<ProductShell>().flatten()
            else {
                return CommandOutcome::InvalidTarget;
            };
            let still_live = self.target_is_live(target, cx)
                && cx
                    .global::<ApplicationTerminalSessions>()
                    .0
                    .borrow()
                    .sessions
                    .get(&session_id)
                    == Some(&session)
                && self
                    .workbench
                    .read(cx)
                    .pane(target.pane)
                    .and_then(|pane| pane.tabs().iter().find(|tab| tab.id() == target.tab))
                    .and_then(|tab| tab.terminal_view())
                    == Some(&source_view);
            if !still_live {
                return CommandOutcome::InvalidTarget;
            }

            source_view.update(cx, |view, cx| view.detach(cx));
            let destination_view = cx.new(|cx| TerminalView::new(session, cx));
            let destination_workbench =
                cx.new(|_| Workbench::new_for_terminal(session_id, destination_view));
            cx.global::<ApplicationWorkbenches>()
                .register(&destination_workbench);
            let transition = self
                .workbench
                .update(cx, |workbench, _| {
                    workbench.request_close_terminal_tab(target.pane, target.tab, session_id)
                })
                .expect("validated source terminal remains until transfer");
            if transition.workbench_is_empty() {
                self.replace_empty_workbench(cx);
            } else {
                self.status = "moved terminal to new window".into();
            }
            destination_shell.update(cx, |shell, cx| {
                shell.workbench = destination_workbench;
                shell.status = "terminal moved here".into();
                shell.focus_active_surface(destination_window, cx);
                cx.notify();
            });
            #[cfg(target_os = "macos")]
            {
                let clear = destination_window.draw(cx);
                clear.clear();
                destination_window.activate_window();
            }
            CommandOutcome::Completed
        });
        if !matches!(result, Ok(CommandOutcome::Completed)) {
            let _ = cx.update_window(destination_handle, |_, window, _| window.remove_window());
            return CommandOutcome::InvalidTarget;
        }
        self.focus_active_surface(window, cx);
        cx.notify();
        CommandOutcome::Completed
    }

    pub(crate) fn handle_workbench_command(
        &mut self,
        command: &crate::host::protocol::Command,
        target: &ProductCommandTarget,
        completion: CommandCompletion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> CommandClaim {
        use crate::host::protocol::CommandOutcome;

        let target_is_live = target.workbench.upgrade() == Some(self.workbench.clone())
            && self
                .workbench
                .read(cx)
                .contains_surface(target.pane, target.tab, target.surface);
        if !target_is_live {
            return CommandClaim::Finished(CommandOutcome::InvalidTarget);
        }
        let name = command.name.as_ref();
        if matches!(
            name,
            NEW_COMMAND
                | NEW_TERMINAL_COMMAND
                | MOVE_TERMINAL_TO_NEW_WINDOW_COMMAND
                | CLOSE_TAB_COMMAND
                | SPLIT_HORIZONTAL_COMMAND
                | SPLIT_VERTICAL_COMMAND
                | OPEN_COMMAND
                | SAVE_COMMAND
                | SAVE_AS_COMMAND
                | SHOW_COMPLETIONS_COMMAND
                | SHOW_COMMAND_PALETTE_COMMAND
        ) && let Err(claim) = super::product_commands::validate_native_arguments(command)
        {
            return claim;
        }
        let result = match name {
            SHOW_COMMAND_PALETTE_COMMAND => {
                self.open_command_palette_from(target.clone(), window, cx);
                CommandClaim::Finished(CommandOutcome::Completed)
            }
            NEW_COMMAND => {
                if self.new_document_in_pane(target.pane, window, cx) {
                    CommandClaim::Finished(CommandOutcome::Completed)
                } else {
                    CommandClaim::Finished(CommandOutcome::InvalidTarget)
                }
            }
            NEW_TERMINAL_COMMAND => {
                if self.new_terminal_in_pane(target.pane, window, cx) {
                    CommandClaim::Finished(CommandOutcome::Completed)
                } else {
                    CommandClaim::Finished(CommandOutcome::InvalidTarget)
                }
            }
            MOVE_TERMINAL_TO_NEW_WINDOW_COMMAND => {
                CommandClaim::Finished(self.move_terminal_to_new_window_with(
                    target,
                    window,
                    cx,
                    open_terminal_transfer_window,
                ))
            }
            CLOSE_TAB_COMMAND => {
                let target = target.clone();
                CommandClaim::Pending(Box::new(move |cx| {
                    dispatch_close_to_captured_target(
                        &target,
                        ProtectedCloseKind::Tab,
                        completion,
                        cx,
                    )
                }))
            }
            SPLIT_HORIZONTAL_COMMAND => {
                if self.split_pane(target.pane, SplitDirection::Horizontal, window, cx) {
                    CommandClaim::Finished(CommandOutcome::Completed)
                } else {
                    CommandClaim::Finished(CommandOutcome::InvalidTarget)
                }
            }
            SPLIT_VERTICAL_COMMAND => {
                if self.split_pane(target.pane, SplitDirection::Vertical, window, cx) {
                    CommandClaim::Finished(CommandOutcome::Completed)
                } else {
                    CommandClaim::Finished(CommandOutcome::InvalidTarget)
                }
            }
            OPEN_COMMAND => {
                let target = target.clone();
                CommandClaim::Pending(Box::new(move |cx| {
                    dispatch_open_to_captured_target(&target, completion, cx)
                }))
            }
            SAVE_COMMAND | SAVE_AS_COMMAND => {
                let target = target.clone();
                let save_as = name == SAVE_AS_COMMAND;
                CommandClaim::Pending(Box::new(move |cx| {
                    dispatch_save_to_captured_target(&target, save_as, completion, cx)
                }))
            }
            SHOW_COMPLETIONS_COMMAND => {
                CommandClaim::Finished(super::extension_host::start_completion(target.clone(), cx))
            }
            _ => CommandClaim::Declined,
        };
        if !matches!(result, CommandClaim::Declined) {
            super::product_commands::break_captured_history_group(target, cx);
        }
        result
    }

    pub(crate) fn handle_application_command(
        &mut self,
        command: &crate::host::protocol::Command,
        target: &ProductCommandTarget,
        completion: CommandCompletion,
        cx: &mut Context<Self>,
    ) -> CommandClaim {
        use crate::host::protocol::CommandOutcome;

        if matches!(
            command.name.as_ref(),
            SHOW_EXTENSION_REPORT_COMMAND
                | NEW_WINDOW_COMMAND
                | CLOSE_WINDOW_COMMAND
                | QUIT_COMMAND
        ) && let Err(claim) = super::product_commands::validate_native_arguments(command)
        {
            return claim;
        }
        match command.name.as_ref() {
            SHOW_EXTENSION_REPORT_COMMAND => {
                super::product_commands::break_captured_history_group(target, cx);
                self.toggle_extension_report(cx);
                CommandClaim::Finished(CommandOutcome::Completed)
            }
            NEW_WINDOW_COMMAND => {
                super::product_commands::break_captured_history_group(target, cx);
                open_product_window(None, cx);
                CommandClaim::Finished(CommandOutcome::Completed)
            }
            CLOSE_WINDOW_COMMAND | QUIT_COMMAND => {
                super::product_commands::break_captured_history_group(target, cx);
                let kind = if command.name.as_ref() == CLOSE_WINDOW_COMMAND {
                    ProtectedCloseKind::Window
                } else {
                    ProtectedCloseKind::Quit
                };
                let target = target.clone();
                CommandClaim::Pending(Box::new(move |cx| {
                    dispatch_close_to_captured_target(&target, kind, completion, cx)
                }))
            }
            _ => {
                let catalog = Self::command_dispatcher(cx).read(cx).catalog();
                let handler = match catalog.borrow().resolve(command.name.as_ref()) {
                    Ok(super::model::CommandTargetKind::Extension(handler)) => handler,
                    _ => return CommandClaim::Declined,
                };
                CommandClaim::Extension {
                    handler,
                    view: None,
                }
            }
        }
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
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .child(self.render_layout(first, cx)),
                    )
                    .child(
                        div()
                            .when(direction == SplitDirection::Horizontal, |element| {
                                element.w(px(1.)).h_full()
                            })
                            .when(direction == SplitDirection::Vertical, |element| {
                                element.h(px(1.)).w_full()
                            })
                            .bg(rgb(0x454545)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .child(self.render_layout(second, cx)),
                    )
                    .into_any_element()
            }
        }
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
                };
                div()
                    .id(("tab", tab_id.value()))
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
                    .overflow_hidden()
                    .bg(rgb(0x2d2d2d))
                    .children(tabs),
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
        let layout = self.workbench.read(cx).layout().cloned();
        let entity = cx.entity();
        let command_palette = self.command_palette.clone();
        let workspace_tree = self.workspace_tree.clone();
        let extension_tree = self
            .extension_tree
            .as_ref()
            .filter(|tree| tree.read(cx).has_provider())
            .cloned();
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
            .key_context("product")
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
                    .when_some(workspace_tree, |element, tree| {
                        element.child(
                            div()
                                .w(px(240.))
                                .h_full()
                                .flex_none()
                                .border_r_1()
                                .border_color(rgb(0x454545))
                                .child(tree),
                        )
                    })
                    .when_some(extension_tree, |element, tree| {
                        element.child(
                            div()
                                .w(px(240.))
                                .h_full()
                                .flex_none()
                                .border_r_1()
                                .border_color(rgb(0x454545))
                                .child(tree),
                        )
                    })
                    .when_some(layout, |element, layout| {
                        element.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .min_h_0()
                                .child(self.render_layout(&layout, cx)),
                        )
                    }),
            )
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
            cx.global::<ApplicationWorkbenches>().register(&workbench);
            let shell = cx.new(|_| ProductShell::new(workbench));
            shell.update(cx, |shell, cx| {
                shell.observe_extension_host(window.window_handle(), cx)
            });
            install_protected_window_close(&shell, window, cx);
            shell.read(cx).focus_active_editor(window, cx);
            if let Some(request) = request {
                let pane = shell.read(cx).workbench.read(cx).focused_pane().unwrap();
                let tab = pane.active_tab();
                let target = OpenTarget {
                    workbench: shell.read(cx).workbench.downgrade(),
                    pane: pane.id(),
                    tab: tab.id(),
                    document: tab
                        .document_id()
                        .expect("initial product tab is a document"),
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

fn open_terminal_transfer_window(cx: &mut App) -> anyhow::Result<WindowHandle<ProductShell>> {
    let bounds = Bounds::centered(None, size(px(1000.), px(720.)), cx);
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
            shell.update(cx, |shell, cx| {
                shell.observe_extension_host(window.window_handle(), cx)
            });
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
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use gpui::{
        AppContext, Focusable, Modifiers, MouseButton, TestAppContext, VisualTestContext, point, px,
    };

    use super::super::keymaps::{ApplicationKeymaps, BindingOwner};
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
        open_terminal_transfer_window, product_filesystems,
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

    fn install_globals(cx: &mut TestAppContext) -> Entity<DocumentCollection> {
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
        documents
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

    fn product_window(
        document: super::DocumentId,
        model: Entity<super::BufferModel>,
        cx: &mut TestAppContext,
    ) -> (Entity<ProductShell>, gpui::AnyWindowHandle) {
        let (shell, _) = cx.add_window_view(|_, cx| {
            let workbench = cx.new(|cx| Workbench::new_for_document(document, model, cx));
            cx.global::<ApplicationWorkbenches>().register(&workbench);
            ProductShell::new(workbench)
        });
        let window = *cx.windows().last().unwrap();
        cx.update_window(window, |_, native_window, cx| {
            shell.update(cx, |shell, cx| shell.observe_extension_host(window, cx));
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
            workbench: shell.workbench.downgrade(),
            pane: pane.id(),
            tab: tab.id(),
            document: tab.document_id().unwrap(),
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
                "version": "1.0.0",
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
                import * as knot from 'knot'; const { commands, editor } = knot;
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
                import * as knot from 'knot'; const { commands, editor } = knot;
                await commands.register('example.shared', async () => {});
                await commands.registerForView('outline', 'copy', async () => {});
                await editor.registerCompletionProvider('broken-provider', {
                    provideCompletions() { return []; }
                });
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
        cx.read(|cx| {
            let tree = shell.read(cx).extension_tree.as_ref().unwrap().clone();
            assert!(
                cx.global::<ApplicationProductCommands>()
                    .0
                    .read(cx)
                    .catalog()
                    .borrow()
                    .resolve_view(tree.read(cx).instance_id(), "copy")
                    .is_none()
            );
        });
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
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/extensions");
        let documents = install_globals(cx);
        let host = cx.update(super::super::extension_host::install);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window) = product_window(document, model.clone(), cx);
        host.update(cx, |host, cx| host.start_installed_extensions(root, cx));
        let report = wait_for_extension_report(&host, cx);
        assert_eq!(report.report.entries.len(), 2);
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

    #[gpui::test]
    fn focused_extension_tree_claims_copy_and_unload_removes_its_handler(cx: &mut TestAppContext) {
        use crate::host::lifecycle::ExtensionKey;

        const SOURCE: &str = r#"
import * as knot from "knot"; const { commands, workbench } = knot;
await workbench.registerTreeDataProvider("outline", {
  getChildren(parentId) {
    return parentId === null ? [
      { id: "first", label: "Selected tree item", collapsibleState: "none", command: "test.view-first" },
      { id: "failure", label: "Fail", collapsibleState: "none" },
    ] : [];
  },
});
const viewCopy = await commands.registerForView("outline", "copy", async () => {
  const text = await workbench.selectedText();
  if (text === "Fail") throw new Error("copy failed");
  await workbench.writeClipboardText(text);
});
await commands.register("test.view-first", async () => {
  throw new Error("global handler ran");
});
await commands.registerForView("outline", "test.view-first", async () => {
  await workbench.writeClipboardText("tree item");
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

        let tree = cx.read(|cx| shell.read(cx).extension_tree.clone().unwrap());
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
            let row = point(px(50.), px(42.));
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
                        Some("outline"),
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
        let second_tree = cx.read(|cx| second_shell.read(cx).extension_tree.clone().unwrap());
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
                    .resolve("cmd-j", Some("outline"))
                    .is_none()
            );
        });
        let unavailable = dispatch_tree_copy(&second_shell, second_window, cx);
        assert_eq!(
            wait_for_command(unavailable, cx),
            CommandOutcome::Unavailable
        );
        cx.read(|cx| {
            assert!(!tree.read(cx).has_provider());
            assert!(!second_tree.read(cx).has_provider());
        });
    }

    #[gpui::test]
    fn startup_script_command_uses_the_focused_product_window(cx: &mut TestAppContext) {
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
const outcome = await commands.invoke("file.new");
if (outcome.kind !== "completed") throw new Error(`file.new: ${outcome.kind}`);
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
                2
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
    fn delayed_extension_copy_rejects_a_closed_captured_view(cx: &mut TestAppContext) {
        use crate::host::lifecycle::ExtensionKey;

        const SOURCE: &str = r#"
import * as knot from "knot"; const { commands, workbench } = knot;
await workbench.registerTreeDataProvider("outline", {
  getChildren(parentId) {
    return parentId === null
      ? [{ id: "held", label: "Held selection", collapsibleState: "none" }]
      : [];
  },
});
await commands.registerForView("outline", "copy", async () => {
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
        let tree = cx.read(|cx| shell.read(cx).extension_tree.clone().unwrap());
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
                "untouched"
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
        let second_tree = cx.read(|cx| second_shell.read(cx).extension_tree.clone().unwrap());
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
                "untouched"
            );
            assert!(!second_tree.read(cx).has_provider());
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

        cx.simulate_keystrokes(window_handle, "cmd-n");
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
                        "cmd-n",
                        Some("editor"),
                        None,
                    )
                    .unwrap()
            );
            super::super::keymaps::rebuild(cx);
        });
        cx.refresh().unwrap();
        cx.simulate_keystrokes(window, "cmd-n");
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(shell.read(cx).workbench.read(cx).panes()[0].tabs().len(), 1));

        let terminal = dispatch_product_command(&shell, window, NEW_TERMINAL_COMMAND, cx);
        assert_eq!(
            terminal.completion.await.unwrap(),
            CommandOutcome::Completed
        );
        cx.refresh().unwrap();
        cx.simulate_keystrokes(window, "cmd-n");
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
        cx.simulate_keystrokes(window, "cmd-k right");
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
                        "cmd-n",
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
        cx.simulate_keystrokes(window, "cmd-n");
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
        cx.simulate_keystrokes(window_handle, "cmd-shift-p");
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

    #[gpui::test]
    async fn copy_requires_focused_selection_and_declines_on_terminal(cx: &mut TestAppContext) {
        use alacritty_terminal::vte::ansi::Handler;
        use gpui::EntityInputHandler;

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

        let target = cx
            .update_window(window, |_, window, cx| {
                shell.update(cx, |shell, cx| {
                    let (session, view) = shell.create_terminal(cx);
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
        });
        cx.refresh().unwrap();
        let bounds = cx.read(|cx| {
            shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .active_tab()
                .terminal_view()
                .unwrap()
                .read(cx)
                .interaction_bounds()
        });
        let start = point(bounds.origin.x + px(1.), bounds.origin.y + px(5.));
        let end = point(
            bounds.origin.x + px(21. * 8. + 1.),
            bounds.origin.y + px(5.),
        );
        {
            let mut window_cx = VisualTestContext::from_window(window, cx);
            window_cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
            window_cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
            window_cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
        }
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
        cx.simulate_keystrokes(window_handle, "shift-home cmd-c");
        cx.run_until_parked();
        cx.update(|cx| assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), ">tail<"));
        cx.simulate_keystrokes(window_handle, "cmd-a cmd-c");
        cx.run_until_parked();
        cx.update(|cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "\tcafé👩‍💻\n>tail<"
            )
        });
        cx.simulate_keystrokes(window_handle, "cmd-x cmd-v");
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
        for name in [super::PASTE_COMMAND, NEW_COMMAND, super::NEW_WINDOW_COMMAND] {
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
        cx.simulate_keystrokes(window_handle, "cmd-z");
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(model.read(cx).text(), "a"));

        cx.simulate_keystrokes(window_handle, "cmd-shift-z");
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
        cx.simulate_keystrokes(window_handle, "cmd-z");
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(model.read(cx).text(), "ab"));
    }

    #[gpui::test]
    fn shell_actions_render_tabs_and_both_split_directions(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, cx) = cx.add_window_view(|_, cx| {
            let workbench = cx.new(|cx| Workbench::new_for_document(document, model, cx));
            cx.global::<ApplicationWorkbenches>().register(&workbench);
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
        let (shell, cx) = cx.add_window_view(|_, cx| {
            let workbench = cx.new(|cx| Workbench::new_for_document(document, model, cx));
            cx.global::<ApplicationWorkbenches>().register(&workbench);
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
        let (shell, cx) = cx.add_window_view(|_, cx| {
            let workbench = cx.new(|cx| Workbench::new_for_document(original, model, cx));
            cx.global::<ApplicationWorkbenches>().register(&workbench);
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
    fn workbench_registry_counts_views_across_windows(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let first = cx.new(|cx| Workbench::new_for_document(document, model.clone(), cx));
        let second = cx.new(|cx| Workbench::new_for_document(document, model, cx));
        cx.read(|cx| {
            let registry = cx.global::<ApplicationWorkbenches>();
            registry.register(&first);
            registry.register(&second);
            assert_eq!(registry.view_count(document, cx), 2);
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
        let document = documents.update(cx, |documents, _| {
            documents.create_persisted("notes.txt", model.clone(), uri.clone(), 0, opened.version)
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
        let document = documents.update(cx, |documents, _| {
            documents.create_persisted("notes.txt", model.clone(), uri.clone(), 0, opened.version)
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
        let first = documents.update(cx, |documents, _| {
            documents.create_persisted(
                "notes.txt",
                first_model.clone(),
                uri.clone(),
                0,
                opened.version,
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
        let (shell, window_cx) = cx.add_window_view(|_, cx| {
            let workbench = cx.new(|cx| Workbench::new_for_document(document, model, cx));
            cx.global::<ApplicationWorkbenches>().register(&workbench);
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
        let document = documents.update(cx, |documents, _| {
            documents.create_destination("created.txt", model.clone(), uri.clone())
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
        let document = documents.update(cx, |documents, _| {
            documents.create_persisted("notes.txt", model.clone(), uri.clone(), 0, opened.version)
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
        let document = documents.update(cx, |documents, _| {
            documents.create_persisted("notes.txt", model.clone(), uri.clone(), 0, opened.version)
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
        let document = documents.update(cx, |documents, _| {
            documents.create_persisted("notes.txt", model.clone(), uri.clone(), 0, opened.version)
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
        let document = documents.update(cx, |documents, _| {
            documents.create_persisted("notes.txt", model.clone(), uri.clone(), 0, opened.version)
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
        let document = documents.update(cx, |documents, _| {
            documents.create_persisted("notes.txt", model.clone(), uri.clone(), 0, opened.version)
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
        let document = documents.update(cx, |documents, _| {
            documents.create_persisted(
                "notes.txt",
                model.clone(),
                original_uri.clone(),
                0,
                opened.version,
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
                shell.workspace.as_ref().unwrap().root().to_string(),
                "mem://product/src"
            );
            assert!(shell.workspace_tree.is_some());
            assert_eq!(documents.read(cx).documents().count(), 1);
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
                TabSurfaceId::Document(_) => panic!("split did not create terminal"),
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
        let destination_size =
            crate::app::terminal_session::TerminalSize::from_pixels(640., 320., 1.);
        destination_view.update(cx, |view, cx| view.resize(destination_size, cx));
        cx.read(|cx| assert_eq!(session.read(cx).size(), destination_size));
        let stale_size = crate::app::terminal_session::TerminalSize::from_pixels(320., 160., 1.);
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
                        let destination = open_terminal_transfer_window(cx)?;
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
        use std::{thread, time::Duration};

        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (source, source_window) = product_window(document, model, cx);
        let session = cx.update(|cx| {
            cx.new(|cx| {
                crate::app::terminal_session::TerminalSession::new_with_options(
                    Options {
                        shell: Some(Shell::new(
                            "/bin/sh".into(),
                            vec![
                                "-c".into(),
                                "printf 'handoff-output\\n'; sleep 0.05; exit 7".into(),
                            ],
                        )),
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
                        open_terminal_transfer_window(cx)
                    })
                })
            })
            .unwrap();
        assert_eq!(outcome, CommandOutcome::Completed);
        cx.run_until_parked();
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
