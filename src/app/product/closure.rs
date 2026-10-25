//! Protected closure planning, approval, and commit for tabs, windows, and quit.

use super::*;

impl ProductShell {
    pub(super) fn protected_closure_is_active(cx: &App) -> bool {
        *cx.global::<ApplicationProtectedClosure>().0.borrow()
    }

    pub(super) fn set_protected_closure_active(active: bool, cx: &App) {
        *cx.global::<ApplicationProtectedClosure>().0.borrow_mut() = active;
    }

    pub(super) fn collect_product_views(cx: &mut App) -> Vec<ProtectedView> {
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

    pub(super) fn build_protected_close_plan(
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
            hot_exit: false,
        })
    }

    #[cfg(test)]
    pub(super) fn close_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    /// Close a tab chosen by pointer, such as a middle-clicked tab, whether or
    /// not it is active. Documents and terminals go through protected closure;
    /// a tool tab hides like its hide button.
    pub(super) fn close_tab_from_pointer(
        &mut self,
        pane: PaneId,
        tab: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self
            .workbench
            .read(cx)
            .pane(pane)
            .and_then(|pane| pane.tabs().iter().find(|candidate| candidate.id() == tab))
        else {
            return;
        };
        if let TabSurfaceId::Tool(id) = tab.surface_id() {
            self.hide_tool_from_pointer(id, window, cx);
            return;
        }
        let Some(focus) = tab.command_view().map(|view| view.focus_handle(cx)) else {
            return;
        };
        let target = ProductCommandTarget {
            window: window.window_handle(),
            shell: cx.entity().downgrade(),
            workbench: self.workbench.downgrade(),
            pane,
            tab: tab.id(),
            surface: tab.surface_id(),
            presentation: tab.presentation(),
            focus: focus.downgrade(),
            view: None,
        };
        // Protected closure updates this shell, so it starts outside the
        // current update.
        cx.defer(move |cx| {
            super::super::product_commands::break_captured_history_group(&target, cx);
            Self::begin_protected_close(ProtectedCloseKind::Tab, target, None, cx);
        });
    }

    pub(super) fn hide_tool_from_pointer(
        &mut self,
        id: crate::app::workbench::ToolFrameId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workbench.update(cx, |workbench, _| {
            workbench.hide_tool(id);
        });
        self.publish_tool_layout(cx);
        self.focus_active_surface(window, cx);
        cx.notify();
    }

    pub(super) fn begin_protected_operation(
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
        let mut plan = match Self::build_protected_close_plan(
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
        if kind != ProtectedCloseKind::Quit || plan.prompts.is_empty() {
            shell.update(cx, |shell, cx| {
                shell.start_protected_close_plan(plan, completion, cx)
            });
            return;
        }
        // Quit is a hot exit when recovery commits every dirty document; a
        // failing store falls back to the ordinary prompts.
        let protected = crate::app::recovery::protect_for_exit(cx);
        cx.spawn(async move |cx| {
            if protected.await {
                plan.prompts.clear();
                plan.hot_exit = true;
            }
            let _ = cx.update(|cx| {
                shell.update(cx, |shell, cx| {
                    shell.start_protected_close_plan(plan, completion, cx)
                })
            });
        })
        .detach();
    }

    pub(super) fn start_protected_close_plan(
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

    pub(super) fn finish_protected_close_completion(
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

    pub(super) fn commit_protected_close(
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
        // A discarded document retires its automatic protection even when the
        // quit transition keeps the document entity alive until process exit.
        let discarded: Vec<uuid::Uuid> = documents
            .read(cx)
            .documents()
            .filter(|document| {
                approvals.iter().any(|approval| {
                    approval.document == document.id()
                        && approval.kind == CloseApprovalKind::Discarded
                })
            })
            .map(|document| document.identity())
            .collect();
        for identity in discarded {
            crate::app::recovery::discard(identity, cx);
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
            if loses_final_view && document.is_dirty(cx) && !plan.hot_exit {
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
                crate::app::extension_host::quit_after_storage(cx);
                Ok(())
            }
        }
    }

    #[cfg(test)]
    pub(super) fn close_tab(
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
                self.status = "this tab needs a save decision before it can close".into();
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

    pub(super) fn close_tab_after_approval(
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
}

pub(super) fn install_protected_window_close(
    shell: &Entity<ProductShell>,
    window: &Window,
    cx: &App,
) {
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
