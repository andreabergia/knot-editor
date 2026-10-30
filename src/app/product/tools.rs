//! Contributed views, the native explorer, workspace roots, and tool-layout memory.

use super::*;

impl ProductShell {
    /// Reconcile this window's contributed frames with the application-owned
    /// definitions. Attaching applies remembered placement or registration
    /// defaults once; later host notifications never override a window-local
    /// programmatic hide.
    pub(super) fn sync_contributed_views(&mut self, cx: &mut Context<Self>) {
        let definitions = crate::app::extensions::host::view_definitions(cx);
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

    pub(super) fn attach_contributed_view(
        &mut self,
        definition: crate::app::extensions::semantics::ViewDefinition,
        cx: &mut Context<Self>,
    ) {
        let Some(window) = self.window else {
            return;
        };
        let window_id = window.window_id().as_u64().to_string();
        let (content, event, observer) = match definition.content {
            crate::host::protocol::ViewContentKind::Tree => {
                let view = cx.new(|cx| {
                    crate::app::tree_view::TreeView::new(
                        definition.id.clone(),
                        window_id.clone(),
                        cx,
                    )
                });
                let event = cx.subscribe(&view, |_, _, event, cx| {
                    let event = event.clone();
                    cx.defer(move |cx| crate::app::extensions::host::route_tree_event(&event, cx));
                    cx.notify();
                });
                let observer = cx.observe(&view, |_, _, cx| cx.notify());
                (
                    crate::app::workbench::ExtensionView::Tree(view),
                    Some(event),
                    observer,
                )
            }
            crate::host::protocol::ViewContentKind::Text => {
                let view = cx.new(|cx| {
                    crate::app::text_view::TextView::new(
                        definition.id.clone(),
                        window_id.clone(),
                        cx,
                    )
                });
                let observer = cx.observe(&view, |_, _, cx| cx.notify());
                (
                    crate::app::workbench::ExtensionView::Text(view),
                    None,
                    observer,
                )
            }
        };
        if !crate::app::extensions::host::attach_view_instance(
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

    pub(super) fn detach_contributed_view(&mut self, stable_id: &str, cx: &mut Context<Self>) {
        let Some(instance) = self.contributed_views.remove(stable_id) else {
            return;
        };
        let view_id = instance.instance_id(cx);
        self.workbench.update(cx, |workbench, _| {
            if let Some(frame) = workbench.tool_by_stable_id(stable_id) {
                workbench.unregister_tool(frame);
            }
        });
        crate::app::extensions::host::detach_view_instance(view_id, cx);
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
            if let crate::app::workbench::ExtensionView::Tree(tree) = content {
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

    pub(super) fn explorer_frame(&self, cx: &App) -> Option<ToolFrameId> {
        let presentation = self.workspace_tree.as_ref()?.entity_id();
        self.workbench
            .read(cx)
            .tools()
            .iter()
            .find(|tool| tool.content.presentation() == presentation)
            .map(|tool| tool.id)
    }

    /// Place a tool as an explicit user arrangement, publishing the
    /// fresh-window default.
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "no product command places a tool yet")
    )]
    pub(crate) fn place_tool(
        &mut self,
        id: ToolFrameId,
        placement: ToolPlacement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let placed = self
            .workbench
            .update(cx, |workbench, _| workbench.place_tool(id, placement));
        if placed {
            self.publish_tool_layout(cx);
        }
        self.focus_active_surface(window, cx);
        cx.notify();
        placed
    }

    pub(super) fn toggle_explorer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(super) fn install_workspace(
        &mut self,
        root: crate::app::resource::ResourceUri,
        cx: &mut Context<Self>,
    ) -> crate::app::workspace::WorkspaceSnapshot {
        self.replace_workspace_roots(vec![root], cx)
    }

    pub(super) fn replace_workspace_roots(
        &mut self,
        roots: Vec<crate::app::resource::ResourceUri>,
        cx: &mut Context<Self>,
    ) -> crate::app::workspace::WorkspaceSnapshot {
        let workspace = self.workspace.replace_roots(roots);
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
        workspace
    }

    /// Register the native explorer frame and apply the stored tool arrangement.
    /// `show` forces visibility for explicit user requests; automatic presentation
    /// (workspace open, fresh-window restore) honors stored visibility instead and
    /// never publishes a new default.
    pub(super) fn ensure_explorer(&mut self, show: bool, cx: &mut Context<Self>) {
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
                crate::app::tool_layout::EXPLORER_TOOL_ID,
                "Explorer",
                ToolContent::Workspace(tree.clone()),
                DockLocation::Left,
            );
            let visible = show
                || stored
                    .as_ref()
                    .and_then(|snapshot| snapshot.tool(crate::app::tool_layout::EXPLORER_TOOL_ID))
                    .is_none_or(|record| record.visible);
            if let Some(snapshot) = &stored {
                workbench.set_dock_sizes(snapshot.dock_sizes);
                if let Some(record) = snapshot.tool(crate::app::tool_layout::EXPLORER_TOOL_ID) {
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
    pub(super) fn stored_tool_layout(
        cx: &App,
    ) -> Option<crate::app::tool_layout::ToolLayoutSnapshot> {
        cx.try_global::<crate::app::tool_layout::ApplicationToolLayout>()
            .map(|store| store.0.current())
    }

    /// Publish this window's live arrangement as the fresh-window default.
    /// Call only for explicit user moves, resizes, and show/hide changes:
    /// restore, activation, and automatic presentation must not publish.
    pub(crate) fn publish_tool_layout(&self, cx: &App) {
        let Some(store) = cx.try_global::<crate::app::tool_layout::ApplicationToolLayout>() else {
            return;
        };
        store
            .0
            .publish(self.workbench.read(cx).tool_layout_snapshot());
    }

    /// Reset this window's tools to product defaults and publish the reset.
    pub(super) fn reset_tool_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace_tree.is_none() {
            self.ensure_explorer(false, cx);
        }
        let defaults = crate::app::tool_layout::ToolLayoutSnapshot::product_default();
        self.restore_tool_layout(&defaults, cx);
        let definitions = crate::app::extensions::host::view_definitions(cx);
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
    pub(super) fn restore_fresh_window_layout(&mut self, cx: &mut Context<Self>) {
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
            .tool(crate::app::tool_layout::EXPLORER_TOOL_ID)
            .is_some_and(|record| record.visible);
        if visible {
            self.ensure_explorer(false, cx);
        }
        // Explorer registration selects itself; restore the stored dock selection
        // so a contributed tool that was selected stays selected.
        self.apply_stored_selection(&snapshot, cx);
    }

    /// Select each dock's stored tool when it is already registered. Unknown or
    /// hidden tools are skipped without changing the live selection.
    pub(super) fn apply_stored_selection(
        &mut self,
        snapshot: &crate::app::tool_layout::ToolLayoutSnapshot,
        cx: &mut Context<Self>,
    ) {
        self.workbench.update(cx, |workbench, _| {
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

    /// Restore the full stored arrangement onto this window's registered tools.
    /// Unknown stored tools are skipped without blank frames; registered tools
    /// without a stored record keep their live placement. Never publishes.
    pub(super) fn restore_tool_layout(
        &mut self,
        snapshot: &crate::app::tool_layout::ToolLayoutSnapshot,
        cx: &mut Context<Self>,
    ) {
        use crate::app::tool_layout::StoredLocation;
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

    pub(super) fn handle_workspace_tree_event(
        &mut self,
        event: WorkspaceTreeEvent,
        cx: &mut Context<Self>,
    ) {
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

    pub(super) fn enumerate_workspace(
        &mut self,
        request: WorkspaceTreeRequest,
        cx: &mut Context<Self>,
    ) {
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
}
