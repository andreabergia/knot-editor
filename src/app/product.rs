//! Native product windows backed by application documents and workbenches.

use std::{
    cell::RefCell,
    future::Future,
    path::Path,
    pin::Pin,
    sync::{Arc, OnceLock},
};

use gpui::{prelude::FluentBuilder, *};

use super::{
    CommandCompletion, CommandPalette, CommandPaletteEntry, CommandPaletteEvent,
    documents::{ApplicationDocuments, DocumentCollection, DocumentId},
    entry::OpenRequest,
    filesystem::{FileSystemProviderRegistry, LocalFileSystemProvider},
    model::BufferModel,
    open::{OpenResource, enumerate_directory, load_resource},
    product_commands::{
        ApplicationProductCommands, CLOSE_TAB_COMMAND, CLOSE_WINDOW_COMMAND, COPY_COMMAND,
        CUT_COMMAND, FIND_COMMAND, FIND_NEXT_COMMAND, FIND_PREVIOUS_COMMAND, NEW_COMMAND,
        NEW_WINDOW_COMMAND, OPEN_COMMAND, PASTE_COMMAND, ProductCommandDispatcher,
        ProductCommandSource, ProductCommandTarget, QUIT_COMMAND, REDO_COMMAND, SAVE_AS_COMMAND,
        SAVE_COMMAND, SELECT_ALL_COMMAND, SPLIT_HORIZONTAL_COMMAND, SPLIT_VERTICAL_COMMAND,
        ShowProductCommandPalette, UNDO_COMMAND,
    },
    workbench::{
        CloseRequestOutcome, DocumentCloseDisposition, PaneId, SplitDirection, SplitPlacement,
        TabId, Workbench, WorkbenchLayout,
    },
    workspace::WorkspaceState,
    workspace_tree::{
        WorkspaceTree, WorkspaceTreeEvent, WorkspaceTreeRequest, WorkspaceTreeResponse,
    },
};

struct ApplicationWorkbenches(RefCell<Vec<WeakEntity<Workbench>>>);

impl Global for ApplicationWorkbenches {}

struct ApplicationFileSystems(Arc<FileSystemProviderRegistry>);

impl Global for ApplicationFileSystems {}

type OpenDialogFuture =
    Pin<Box<dyn Future<Output = Result<Option<Vec<std::path::PathBuf>>, String>> + Send + 'static>>;

trait ProductOpenDialog: Send + Sync {
    fn select(&self) -> OpenDialogFuture;
}

struct ApplicationOpenDialog(Arc<dyn ProductOpenDialog>);

impl Global for ApplicationOpenDialog {}

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

impl From<&ProductCommandTarget> for OpenTarget {
    fn from(target: &ProductCommandTarget) -> Self {
        Self {
            workbench: target.workbench.clone(),
            pane: target.pane,
            tab: target.tab,
            document: target.document,
        }
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
    open_generation: u64,
    tasks: Vec<Task<()>>,
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
            open_generation: 0,
            tasks: Vec::new(),
        }
    }

    fn command_dispatcher(cx: &App) -> Entity<ProductCommandDispatcher> {
        cx.global::<ApplicationProductCommands>().0.clone()
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
        Some(ProductCommandTarget {
            window: window.window_handle(),
            shell: cx.entity().downgrade(),
            workbench: self.workbench.downgrade(),
            pane: pane.id(),
            tab: tab.id(),
            document: tab.document_id(),
            focus: focus.downgrade(),
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

    fn open_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(origin) = self.capture_command_target(window, cx) else {
            return;
        };
        let entries = Self::command_dispatcher(cx)
            .read(cx)
            .definitions()
            .cloned()
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
        let focused_pane = self.workbench.read(cx).panes().iter().find_map(|pane| {
            pane.tabs()
                .iter()
                .any(|tab| tab.editor().focus_handle(cx).contains_focused(window, cx))
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

    pub(crate) fn start_open_dialog(
        &mut self,
        target: ProductCommandTarget,
        completion: CommandCompletion,
        cx: &mut Context<Self>,
    ) {
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
                    selection
                        .await
                        .map_err(|error| error.to_string())?
                        .map_err(|error| error.to_string())
                })
            };
        self.status = "choosing a file or folder".into();
        cx.notify();
        let task = cx.spawn(async move |this, cx| {
            let paths = match selection.await {
                Ok(paths) => paths,
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
            let request = match open_request_from_dialog_paths(paths) {
                Ok(Some(request)) => request,
                Ok(None) => {
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
                    this.start_open_request(
                        request,
                        OpenTarget::from(&target),
                        false,
                        Some(delivered),
                        cx,
                    );
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
            {
                if let Some(completion) = fallback {
                    completion.complete(crate::host::protocol::CommandOutcome::InvalidTarget);
                }
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
            Ok(OpenResource::File { uri, text }) => {
                let documents = Self::documents(cx);
                let document =
                    if let Some(document) = documents.read(cx).document_for_resource(&uri) {
                        document
                    } else {
                        let model = cx.new(|_| BufferModel::from_text(text));
                        documents.update(cx, |documents, _| {
                            documents.create_persisted(resource_title(&uri), model, uri.clone(), 0)
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
                .find(|tab| tab.document_id() == document)
                .map(|tab| (pane.id(), tab.id(), tab.editor().clone()))
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
                let target = OpenTarget {
                    workbench: self.workbench.downgrade(),
                    pane: pane.id(),
                    tab: tab.id(),
                    document: tab.document_id(),
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
            workbench.pane(pane).unwrap().active_tab().editor().clone()
        });
        editor.focus_handle(cx).focus(window);
        self.status = "new untitled document".into();
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
        let editor = self.workbench.update(cx, |workbench, cx| {
            workbench.split_pane(pane, direction, SplitPlacement::After, cx)?;
            Some(workbench.focused_pane()?.active_tab().editor().clone())
        });
        if let Some(editor) = editor {
            editor.focus_handle(cx).focus(window);
            self.status = match direction {
                SplitDirection::Horizontal => "split horizontally",
                SplitDirection::Vertical => "split vertically",
            }
            .into();
            cx.notify();
            true
        } else {
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
        let editor = self.workbench.update(cx, |workbench, _| {
            workbench.activate_tab(pane, tab);
            Some(workbench.pane(pane)?.active_tab().editor().clone())
        });
        if let Some(editor) = editor {
            editor.focus_handle(cx).focus(window);
            cx.notify();
        }
    }

    #[cfg(test)]
    fn close_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_focused_pane(window, cx);
        let Some((pane, tab, document)) = self.workbench.read(cx).focused_pane().map(|pane| {
            (
                pane.id(),
                pane.active_tab_id(),
                pane.active_tab().document_id(),
            )
        }) else {
            return;
        };
        self.close_tab(pane, tab, document, window, cx);
    }

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

        if transition.document == DocumentCloseDisposition::CloseRequested {
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

    fn focus_active_editor(&self, window: &mut Window, cx: &App) {
        if let Some(editor) = self
            .workbench
            .read(cx)
            .focused_pane()
            .map(|pane| pane.active_tab().editor().clone())
        {
            editor.focus_handle(cx).focus(window);
        }
    }

    pub(crate) fn execute_product_command(
        &mut self,
        name: &str,
        target: &ProductCommandTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> crate::host::protocol::CommandOutcome {
        use crate::host::protocol::CommandOutcome;

        let target_is_live = target.workbench.upgrade() == Some(self.workbench.clone())
            && self
                .workbench
                .read(cx)
                .contains_tab(target.pane, target.tab, target.document);
        if !target_is_live {
            return CommandOutcome::InvalidTarget;
        }

        match name {
            NEW_COMMAND => self
                .new_document_in_pane(target.pane, window, cx)
                .then_some(CommandOutcome::Completed)
                .unwrap_or(CommandOutcome::InvalidTarget),
            CLOSE_TAB_COMMAND => {
                self.close_tab(target.pane, target.tab, target.document, window, cx)
            }
            SPLIT_HORIZONTAL_COMMAND => self
                .split_pane(target.pane, SplitDirection::Horizontal, window, cx)
                .then_some(CommandOutcome::Completed)
                .unwrap_or(CommandOutcome::InvalidTarget),
            SPLIT_VERTICAL_COMMAND => self
                .split_pane(target.pane, SplitDirection::Vertical, window, cx)
                .then_some(CommandOutcome::Completed)
                .unwrap_or(CommandOutcome::InvalidTarget),
            CLOSE_WINDOW_COMMAND => {
                window.remove_window();
                CommandOutcome::Completed
            }
            NEW_WINDOW_COMMAND => {
                open_product_window(None, cx);
                CommandOutcome::Completed
            }
            QUIT_COMMAND => {
                cx.quit();
                CommandOutcome::Completed
            }
            OPEN_COMMAND
            | SAVE_COMMAND
            | SAVE_AS_COMMAND
            | UNDO_COMMAND
            | REDO_COMMAND
            | CUT_COMMAND
            | COPY_COMMAND
            | PASTE_COMMAND
            | SELECT_ALL_COMMAND
            | FIND_COMMAND
            | FIND_NEXT_COMMAND
            | FIND_PREVIOUS_COMMAND => {
                self.status = "command is not implemented yet".into();
                cx.notify();
                CommandOutcome::Unavailable
            }
            _ => CommandOutcome::Unavailable,
        }
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
        let editor = pane.active_tab().editor().clone();
        let documents = Self::documents(cx);
        let tabs = pane
            .tabs()
            .iter()
            .map(|tab| {
                let tab_id = tab.id();
                let documents = documents.read(cx);
                let document = documents.get(tab.document_id()).unwrap();
                let title = format!(
                    "{}{}",
                    document.title(),
                    if document.is_dirty(cx) { " •" } else { "" }
                );
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
            .child(div().flex_1().min_h_0().overflow_hidden().child(editor))
            .into_any_element()
    }
}

impl Render for ProductShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let layout = self.workbench.read(cx).layout().cloned();
        let entity = cx.entity();
        let command_palette = self.command_palette.clone();
        let workspace_tree = self.workspace_tree.clone();
        window.set_window_title("Knot");

        div()
            .key_context("product")
            .on_action(cx.listener(Self::dispatch_source))
            .on_action(
                cx.listener(|this, _: &ShowProductCommandPalette, window, cx| {
                    this.open_command_palette(window, cx)
                }),
            )
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
                        "",
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
            .into_any_element()
    }
}

fn command_button(
    label: &'static str,
    id: &'static str,
    shell: &Entity<ProductShell>,
    command: &'static str,
) -> Stateful<Div> {
    let shell = shell.clone();
    div()
        .id(id)
        .cursor_pointer()
        .text_color(rgb(0x80c0ff))
        .child(label)
        .on_click(move |_, window, cx| {
            shell.update(cx, |shell, cx| {
                if command.is_empty() {
                    shell.open_command_palette(window, cx);
                } else {
                    shell.dispatch_source(&ProductCommandSource::new(command), window, cx);
                }
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
        .and_then(|segments| segments.filter(|segment| !segment.is_empty()).next_back())
        .map(percent_encoding::percent_decode_str)
        .and_then(|title| title.decode_utf8().ok())
        .filter(|title| !title.is_empty())
        .map(|title| title.into_owned())
        .unwrap_or_else(|| uri.to_string())
}

fn open_request_from_dialog_paths(
    paths: Option<Vec<std::path::PathBuf>>,
) -> Result<Option<OpenRequest>, String> {
    paths
        .and_then(|paths| paths.into_iter().next())
        .map(|path| OpenRequest::from_path(&path, Path::new("/")))
        .transpose()
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
            shell.read(cx).focus_active_editor(window, cx);
            if let Some(request) = request {
                let pane = shell.read(cx).workbench.read(cx).focused_pane().unwrap();
                let tab = pane.active_tab();
                let target = OpenTarget {
                    workbench: shell.read(cx).workbench.downgrade(),
                    pane: pane.id(),
                    tab: tab.id(),
                    document: tab.document_id(),
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
    application.run(move |cx| {
        let documents = cx.new(|_| DocumentCollection::new());
        cx.set_global(ApplicationDocuments(documents));
        cx.set_global(ApplicationWorkbenches(RefCell::new(Vec::new())));
        cx.set_global(ApplicationFileSystems(product_filesystems()));
        let commands = cx.new(ProductCommandDispatcher::new);
        cx.set_global(ApplicationProductCommands(commands));
        cx.bind_keys([
            KeyBinding::new(
                "cmd-n",
                ProductCommandSource::new(NEW_COMMAND),
                Some("product"),
            ),
            KeyBinding::new(
                "cmd-t",
                ProductCommandSource::new(NEW_COMMAND),
                Some("product"),
            ),
            KeyBinding::new(
                "cmd-shift-n",
                ProductCommandSource::new(NEW_WINDOW_COMMAND),
                Some("product"),
            ),
            KeyBinding::new(
                "cmd-o",
                ProductCommandSource::new(OPEN_COMMAND),
                Some("product"),
            ),
            KeyBinding::new(
                "cmd-s",
                ProductCommandSource::new(SAVE_COMMAND),
                Some("product"),
            ),
            KeyBinding::new(
                "cmd-shift-s",
                ProductCommandSource::new(SAVE_AS_COMMAND),
                Some("product"),
            ),
            KeyBinding::new(
                "cmd-w",
                ProductCommandSource::new(CLOSE_TAB_COMMAND),
                Some("product"),
            ),
            KeyBinding::new(
                "cmd-shift-w",
                ProductCommandSource::new(CLOSE_WINDOW_COMMAND),
                Some("product"),
            ),
            KeyBinding::new(
                "cmd-k right",
                ProductCommandSource::new(SPLIT_HORIZONTAL_COMMAND),
                Some("product"),
            ),
            KeyBinding::new(
                "cmd-k down",
                ProductCommandSource::new(SPLIT_VERTICAL_COMMAND),
                Some("product"),
            ),
            KeyBinding::new("cmd-shift-p", ShowProductCommandPalette, Some("product")),
            KeyBinding::new(
                "cmd-z",
                ProductCommandSource::new(UNDO_COMMAND),
                Some("product"),
            ),
            KeyBinding::new(
                "cmd-shift-z",
                ProductCommandSource::new(REDO_COMMAND),
                Some("product"),
            ),
            KeyBinding::new(
                "cmd-x",
                ProductCommandSource::new(CUT_COMMAND),
                Some("product"),
            ),
            KeyBinding::new(
                "cmd-c",
                ProductCommandSource::new(COPY_COMMAND),
                Some("product"),
            ),
            KeyBinding::new(
                "cmd-v",
                ProductCommandSource::new(PASTE_COMMAND),
                Some("product"),
            ),
            KeyBinding::new(
                "cmd-a",
                ProductCommandSource::new(SELECT_ALL_COMMAND),
                Some("product"),
            ),
            KeyBinding::new(
                "cmd-f",
                ProductCommandSource::new(FIND_COMMAND),
                Some("product"),
            ),
            KeyBinding::new(
                "cmd-g",
                ProductCommandSource::new(FIND_NEXT_COMMAND),
                Some("product"),
            ),
            KeyBinding::new(
                "cmd-shift-g",
                ProductCommandSource::new(FIND_PREVIOUS_COMMAND),
                Some("product"),
            ),
            KeyBinding::new(
                "cmd-q",
                ProductCommandSource::new(QUIT_COMMAND),
                Some("product"),
            ),
        ]);
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
        cx.spawn(async move |cx| {
            while let Some(request) = incoming_requests.recv().await {
                let _ = cx.update(|cx| open_product_window(Some(request), cx));
            }
        })
        .detach();
        open_product_window(initial_request, cx);
    });
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui::{AppContext, Focusable, KeyBinding, TestAppContext};

    use crate::host::protocol::{Command, CommandArgumentValue, CommandOutcome};

    use super::{
        ApplicationDocuments, ApplicationFileSystems, ApplicationOpenDialog,
        ApplicationProductCommands, ApplicationWorkbenches, DocumentCollection, Entity,
        NEW_COMMAND, OpenDialogFuture, OpenRequest, OpenTarget, ProductCommandDispatcher,
        ProductCommandSource, ProductOpenDialog, ProductShell, RefCell, SAVE_COMMAND,
        SPLIT_HORIZONTAL_COMMAND, SplitDirection, Workbench, WorkbenchLayout,
        create_untitled_document, product_filesystems,
    };
    use crate::app::{
        documents::DocumentState,
        filesystem::{FileSystemProviderRegistry, MemoryFileSystemProvider},
        resource::ResourceUri,
    };

    fn install_globals(cx: &mut TestAppContext) -> Entity<DocumentCollection> {
        let documents = cx.new(|_| DocumentCollection::new());
        cx.set_global(ApplicationDocuments(documents.clone()));
        cx.set_global(ApplicationWorkbenches(RefCell::new(Vec::new())));
        cx.set_global(ApplicationFileSystems(product_filesystems()));
        let commands = cx.new(ProductCommandDispatcher::new);
        cx.set_global(ApplicationProductCommands(commands));
        documents
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
        (shell, window)
    }

    fn memory_filesystems() -> Arc<FileSystemProviderRegistry> {
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
        providers.register("mem", provider).unwrap();
        Arc::new(providers)
    }

    fn current_open_target(shell: &Entity<ProductShell>, cx: &gpui::App) -> OpenTarget {
        let shell = shell.read(cx);
        let pane = shell.workbench.read(cx).focused_pane().unwrap();
        let tab = pane.active_tab();
        OpenTarget {
            workbench: shell.workbench.downgrade(),
            pane: pane.id(),
            tab: tab.id(),
            document: tab.document_id(),
        }
    }

    struct FixedOpenDialog(Option<Vec<std::path::PathBuf>>);

    impl ProductOpenDialog for FixedOpenDialog {
        fn select(&self) -> OpenDialogFuture {
            let selection = self.0.clone();
            Box::pin(async move { Ok(selection) })
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
                first.active_tab().editor().clone(),
                second.active_tab().editor().clone(),
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
        cx.update(|cx| {
            cx.bind_keys([KeyBinding::new(
                "cmd-n",
                ProductCommandSource::new(NEW_COMMAND),
                Some("product"),
            )]);
        });
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
    async fn palette_discovers_commands_and_keeps_its_opening_target(cx: &mut TestAppContext) {
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
        cx.refresh().unwrap();

        let (first_pane, second_pane, first_editor) = cx.read(|cx| {
            let workbench = shell.read(cx).workbench.read(cx);
            let first = &workbench.panes()[0];
            (
                first.id(),
                workbench.panes()[1].id(),
                first.active_tab().editor().clone(),
            )
        });
        cx.update_window(window_handle, |_, window, cx| {
            first_editor.focus_handle(cx).focus(window);
            shell.update(cx, |shell, cx| shell.open_command_palette(window, cx));
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
    async fn javascript_awaits_the_same_product_command_outcome(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window_handle) = product_window(document, model, cx);
        cx.update_window(window_handle, |_, window, cx| {
            shell.read(cx).focus_active_editor(window, cx)
        })
        .unwrap();
        let runtime = cx.read(|cx| {
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .runtime_control()
        });

        runtime
            .execute_fixture_module(
                "file:///fixtures/product-command.js",
                format!(
                    r#"
                        import {{ commands }} from "knot:editor";
                        const outcome = await commands.invoke("{NEW_COMMAND}", null);
                        if (outcome.kind !== "completed") {{
                          throw new Error(`unexpected outcome: ${{outcome.kind}}`);
                        }}
                    "#
                ),
            )
            .await
            .unwrap();

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
    async fn registered_but_deferred_commands_complete_as_unavailable(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window_handle) = product_window(document, model, cx);
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
                    name: SAVE_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                target,
                cx,
            )
        });

        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::Unavailable
        );
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
                    .clone(),
                workbench.panes()[1].active_tab().editor().clone(),
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
                .document_id();
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
    async fn cancelling_the_native_open_selection_settles_the_command(cx: &mut TestAppContext) {
        let documents = install_globals(cx);
        cx.set_global(ApplicationOpenDialog(Arc::new(FixedOpenDialog(None))));
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
                    .document_id(),
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
}
