//! Open dialogs, open requests, and presentation of opened documents.

use super::*;

impl ProductShell {
    pub(super) fn next_open_generation(&mut self) -> u64 {
        self.open_generation = self
            .open_generation
            .checked_add(1)
            .expect("product open generation overflowed");
        self.open_generation
    }

    pub(super) fn finish_open_command(
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

    pub(super) fn start_open_dialog_with_options(
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
        cx.notify();
        let task = cx.spawn(async move |this, cx| {
            let path = match selection.await {
                OpenDialogOutcome::Selected(path) => path,
                OpenDialogOutcome::Cancelled => {
                    let delivered = completion.clone();
                    if this
                        .update(cx, |this, cx| {
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
                            this.show_notice(
                                PromptLevel::Warning,
                                "Open failed",
                                Some(message.clone()),
                                cx,
                            );
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
                            this.show_notice(
                                PromptLevel::Warning,
                                "Open failed",
                                Some(message.clone()),
                                cx,
                            );
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

    pub(super) fn start_open_request(
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

    pub(super) fn start_open_operation(
        &mut self,
        request: OpenRequest,
        target: OpenTarget,
        replace_target: bool,
        cx: &mut Context<Self>,
    ) -> tokio::sync::oneshot::Receiver<Result<OpenCommit, OpenFailure>> {
        self.start_open_operation_with(
            request,
            target,
            operations::OpenOptions {
                replace_target,
                ..Default::default()
            },
            cx,
        )
    }

    /// Open a resource, reporting a resource failure in the target window.
    pub(super) fn start_open_operation_with(
        &mut self,
        request: OpenRequest,
        target: OpenTarget,
        options: operations::OpenOptions,
        cx: &mut Context<Self>,
    ) -> tokio::sync::oneshot::Receiver<Result<OpenCommit, OpenFailure>> {
        let (completion, result) = tokio::sync::oneshot::channel();
        let window = target.window;
        self.start_resource_operation(
            request,
            target,
            options,
            move |outcome, cx| {
                if let Err(OpenFailure::Resource(error)) = &outcome {
                    show_notice(
                        window,
                        PromptLevel::Warning,
                        "Open failed".into(),
                        Some(error.to_string()),
                        cx,
                    );
                }
                let _ = completion.send(outcome);
            },
            cx,
        );
        result
    }

    pub(super) fn start_resource_operation(
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
        let providers = Self::filesystems(cx);
        let task = cx.spawn(async move |this, cx| {
            let loaded = crate::app::open::load_resource_kind(providers, uri, options.kind).await;
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
                        this.apply_open_result(
                            loaded,
                            &target,
                            options.replace_target,
                            options.preview,
                            cx,
                        )
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

    pub(super) fn apply_open_result(
        &mut self,
        result: Result<OpenResource, crate::app::open::OpenResourceError>,
        target: &OpenTarget,
        replace_target: bool,
        preview: bool,
        cx: &mut Context<Self>,
    ) -> Result<OpenCommit, crate::app::open::OpenResourceError> {
        let committed = match result {
            Ok(OpenResource::Directory(root)) => {
                OpenCommit::Workspace(self.install_workspace(root, cx))
            }
            Ok(resource) => {
                let document = Self::resource_document(resource, cx)
                    .expect("files and missing resources are documents");
                let target =
                    self.present_open_document(document, target, replace_target, preview, cx);
                OpenCommit::Document { document, target }
            }
            Err(error) => return Err(error),
        };
        if let OpenCommit::Document { target, .. } = &committed {
            let _ = cx.update_window(target.window, |_, window, cx| {
                self.focus_active_surface(window, cx)
            });
        }
        cx.notify();
        Ok(committed)
    }

    /// The document for a loaded file or missing destination: the live
    /// document of its URI, else a new one. A directory has none.
    pub(super) fn resource_document(resource: OpenResource, cx: &mut App) -> Option<DocumentId> {
        let documents = Self::documents(cx);
        match resource {
            OpenResource::Directory(_) => None,
            OpenResource::Missing(uri) => {
                let model = cx.new(|_| BufferModel::from_text(""));
                let title = resource_title(&uri);
                Some(documents.update(cx, |documents, cx| {
                    documents.create_destination(title, model, uri.clone(), cx)
                }))
            }
            OpenResource::File { uri, text, version } => {
                if let Some(document) = documents.read(cx).document_for_resource(&uri) {
                    return Some(document);
                }
                let model = cx.new(|_| BufferModel::from_text(text));
                Some(documents.update(cx, |documents, cx| {
                    documents.create_persisted(
                        resource_title(&uri),
                        model,
                        uri.clone(),
                        0,
                        version,
                        cx,
                    )
                }))
            }
        }
    }

    pub(super) fn present_open_document(
        &mut self,
        document: DocumentId,
        target: &OpenTarget,
        replace_target: bool,
        preview: bool,
        cx: &mut Context<Self>,
    ) -> OpenTarget {
        let existing = self.workbench.read(cx).panes().iter().find_map(|pane| {
            pane.tabs()
                .iter()
                .find(|tab| tab.document_id() == Some(document))
                .map(|tab| (pane.id(), tab.id()))
        });
        let preview = preview
            && crate::app::settings::boolean(crate::app::settings::WORKBENCH_PREVIEW_TABS, cx);
        let (pane, tab) = if let Some((pane, tab)) = existing {
            // Opening a document again without a preview keeps its tab.
            self.workbench.update(cx, |workbench, _| {
                workbench.activate_tab(pane, tab);
                if !preview {
                    workbench.promote_preview(pane, tab);
                }
            });
            (pane, tab)
        } else if preview {
            let documents = Self::documents(cx);
            let model = documents.read(cx).get(document).unwrap().model().clone();
            let (pane, tab, replaced) = self.workbench.update(cx, |workbench, cx| {
                workbench
                    .open_preview_for_document(target.pane, document, model, cx)
                    .expect("validated destination pane")
            });
            if let Some(replaced) = replaced.filter(|replaced| *replaced != document)
                && cx
                    .global::<ApplicationWorkbenches>()
                    .view_count(replaced, cx)
                    == 0
            {
                documents.update(cx, |documents, cx| {
                    documents.remove(replaced, cx);
                });
            }
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
}
