//! Rendering of the product shell's panes, split layout, dividers, and extension report.

use super::*;
use crate::app::theme::{Theme, color, theme};

impl ProductShell {
    pub(super) fn render_layout(
        &self,
        layout: &WorkbenchLayout,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match layout {
            WorkbenchLayout::Pane(pane) => self.render_pane(*pane, cx),
            WorkbenchLayout::Split {
                direction,
                ratio,
                first,
                second,
            } => {
                let direction = *direction;
                let workbench = self.workbench.read(cx);
                // A dock keeps its absolute size and its sibling takes the
                // rest; an ordinary split divides its extent by ratio.
                let dock = workbench.split_dock(layout);
                let extent = |child: &WorkbenchLayout| match (child, dock) {
                    (WorkbenchLayout::Pane(pane), Some(dock))
                        if workbench.dock_location(*pane) == Some(dock) =>
                    {
                        Some(workbench.dock_size(dock))
                    }
                    _ => None,
                };
                let first_extent = extent(first);
                let second_extent = extent(second);
                let boundary = layout.boundary().expect("splits have a boundary");
                let horizontal = direction == SplitDirection::Horizontal;
                let child = |extent: Option<f32>, grow: f32| {
                    let mut element = div().min_w_0().min_h_0();
                    match extent {
                        Some(extent) => {
                            element = element
                                .flex_none()
                                .when(horizontal, |element| element.w(px(extent)))
                                .when(!horizontal, |element| element.h(px(extent)));
                        }
                        None if dock.is_some() => element = element.flex_1(),
                        None => {
                            element = element.flex_basis(px(0.)).flex_shrink();
                            element.style().flex_grow = Some(grow);
                        }
                    }
                    element
                };
                let first_child = child(first_extent, *ratio);
                let second_child = child(second_extent, 1. - *ratio);
                div()
                    .flex()
                    .when(horizontal, |element| element.flex_row())
                    .when(!horizontal, |element| element.flex_col())
                    .size_full()
                    .min_w_0()
                    .min_h_0()
                    .relative()
                    .when(dock.is_none(), |element| {
                        element.child(self.split_drag_geometry(boundary))
                    })
                    .child(first_child.child(self.render_layout(first, cx)))
                    .child(self.render_divider(direction, dock, boundary, cx))
                    .child(second_child.child(self.render_layout(second, cx)))
                    .into_any_element()
            }
        }
    }

    /// Divider between two split children, with a wider hit area that drags
    /// to resize: a dock's divider changes its size, an ordinary divider its
    /// split's ratio.
    pub(super) fn render_divider(
        &self,
        direction: SplitDirection,
        dock: Option<DockLocation>,
        boundary: PaneId,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let horizontal = direction == SplitDirection::Horizontal;
        let element = match dock {
            Some(dock) => div()
                .id(("dock-divider", dock.index()))
                .debug_selector(move || format!("dock-divider-{}", dock.index())),
            None => div()
                .id(("split-divider", boundary.value()))
                .debug_selector(move || format!("split-divider-{}", boundary.value())),
        };
        element
            .flex()
            .flex_none()
            .when(horizontal, |element| {
                element
                    .flex_row()
                    .w(px(9.))
                    .h_full()
                    .justify_center()
                    .cursor(CursorStyle::ResizeLeftRight)
            })
            .when(!horizontal, |element| {
                element
                    .flex_col()
                    .h(px(9.))
                    .w_full()
                    .items_center()
                    .cursor(CursorStyle::ResizeUpDown)
            })
            .child(
                div()
                    .when(horizontal, |element| element.w(px(1.)).h_full())
                    .when(!horizontal, |element| element.h(px(1.)).w_full())
                    .bg(theme(cx).color(color::BORDER))
                    .hover({
                        let hover = theme(cx).color(color::BORDER_HOVER);
                        move |style| style.bg(hover)
                    }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    match dock {
                        Some(dock) => {
                            let size = this.workbench.read(cx).dock_size(dock);
                            this.dock_resize = Some(DockResizeDrag {
                                dock,
                                start_pointer: event.position,
                                start_size: size,
                            });
                        }
                        None => {
                            let ratio = this.workbench.read(cx).split_ratio(boundary);
                            let bounds = this.tab_drag.borrow().split_bounds(boundary);
                            if let (Some(ratio), Some(bounds)) = (ratio, bounds) {
                                this.split_resize = Some(SplitResizeDrag {
                                    boundary,
                                    horizontal,
                                    start_pointer: event.position,
                                    start_ratio: ratio,
                                    extent: if horizontal {
                                        f32::from(bounds.size.width)
                                    } else {
                                        f32::from(bounds.size.height)
                                    },
                                });
                            }
                        }
                    }
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .into_any_element()
    }

    pub(super) fn render_pane(&self, pane_id: PaneId, cx: &mut Context<Self>) -> AnyElement {
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
                    ToolContent::Extension(crate::app::workbench::ExtensionView::Tree(view)) => {
                        view.clone().into_any_element()
                    }
                    ToolContent::Extension(crate::app::workbench::ExtensionView::Text(view)) => {
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
                    .text_color(theme(cx).color(if tab_id == active_tab {
                        color::TAB_ACTIVE_FOREGROUND
                    } else {
                        color::TAB_INACTIVE_FOREGROUND
                    }))
                    .bg(theme(cx).color(if tab_id == active_tab {
                        color::TAB_ACTIVE_BACKGROUND
                    } else {
                        color::TAB_INACTIVE_BACKGROUND
                    }))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .when(tab.is_preview(), |element| element.italic())
                    .child(title)
                    .when(tab.is_pinned(), |element| {
                        element.child(
                            div()
                                .debug_selector(move || format!("pin-{}", tab_id.value()))
                                .text_xs()
                                .child("📌"),
                        )
                    })
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        if event.click_count() > 1 {
                            this.workbench.update(cx, |workbench, _| {
                                workbench.promote_preview(pane_id, tab_id)
                            });
                        }
                        this.activate_tab(pane_id, tab_id, window, cx);
                    }))
                    .on_mouse_up(
                        MouseButton::Middle,
                        cx.listener(move |this, _, window, cx| {
                            this.close_tab_from_pointer(pane_id, tab_id, window, cx);
                        }),
                    )
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
            .bg(theme(cx).color(color::BACKGROUND))
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
                    .bg(theme(cx).color(color::TAB_BAR_BACKGROUND))
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
                                            this.hide_tool_from_pointer(id, window, cx);
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
        let layout = match self.maximized_pane(cx) {
            Some(pane) => Some(WorkbenchLayout::Pane(pane)),
            None => self.workbench.read(cx).layout().cloned(),
        };
        let entity = cx.entity();
        let command_palette = self
            .command_palette
            .as_ref()
            .map(|palette| palette.picker().clone());
        let transient = self.render_transient();
        let dismiss_transient =
            cx.listener(|this: &mut Self, _: &MouseDownEvent, _: &mut Window, cx| {
                this.finish_transient(TransientOutcome::Dismissed, cx);
            });
        let scrim = theme(cx).color(color::SCRIM);
        let startup_report = crate::app::extensions::host::startup_report(cx);
        let extension_report = self.extension_report_open.then(|| {
            (
                startup_report,
                crate::app::extensions::host::setting_warnings(cx),
            )
        });
        window.set_window_title("Knot");

        div()
            .relative()
            .key_context("product")
            .capture_key_down(cx.listener(Self::cancel_tab_drag))
            .on_modifiers_changed(cx.listener(Self::modifiers_changed))
            .on_action(cx.listener(Self::dispatch_source))
            .on_action(cx.listener(Self::dispatch_keybinding))
            .flex()
            .flex_col()
            .size_full()
            .bg(theme(cx).color(color::BACKGROUND))
            .text_color(theme(cx).color(color::TEXT))
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
                        .bg(scrim)
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
                        .bg(scrim)
                        .child(palette),
                )
            })
            .when_some(extension_report, |root, (report, setting_warnings)| {
                root.child(render_extension_report(
                    report,
                    setting_warnings,
                    &entity,
                    theme(cx),
                ))
            })
            .into_any_element()
    }
}

pub(super) fn render_extension_report(
    snapshot: Option<crate::app::extensions::host::StartupReportSnapshot>,
    setting_warnings: Vec<String>,
    shell: &Entity<ProductShell>,
    theme: &Theme,
) -> AnyElement {
    use crate::app::extensions::host::StartupState;
    use crate::app::extensions::load::LoadResult;

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
            let (label, status_color, cause) = match entry.result {
                LoadResult::Loaded => ("Loaded", color::SUCCESS, None),
                LoadResult::Failed(cause) => ("Failed", color::ERROR, Some(cause)),
            };
            div()
                .flex()
                .flex_col()
                .gap_1()
                .py_2()
                .border_b_1()
                .border_color(theme.color(color::BORDER))
                .child(
                    div()
                        .text_color(theme.color(status_color))
                        .child(format!("{label}: {name}")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.color(color::TEXT_MUTED))
                        .child(entry.directory.display().to_string()),
                )
                .when_some(cause, |row, cause| {
                    row.child(div().text_color(theme.color(color::ERROR)).child(cause))
                })
        })
        .chain(setting_warnings.into_iter().map(|warning| {
            div()
                .py_2()
                .border_b_1()
                .border_color(theme.color(color::BORDER))
                .text_color(theme.color(color::WARNING))
                .child(warning)
        }))
        .collect::<Vec<_>>();
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_start()
        .justify_center()
        .pt(px(60.))
        .bg(theme.color(color::SCRIM))
        .child(
            div()
                .w(px(760.))
                .h(px(520.))
                .flex()
                .flex_col()
                .bg(theme.color(color::POPOVER_BACKGROUND))
                .border_1()
                .border_color(theme.color(color::POPOVER_BORDER))
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
                            theme.color(color::TEXT_ACCENT),
                        )),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.color(color::TEXT_MUTED))
                        .child(root),
                )
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

pub(super) fn command_button(
    label: impl Into<SharedString>,
    id: &'static str,
    shell: &Entity<ProductShell>,
    command: &'static str,
    accent: Hsla,
) -> Stateful<Div> {
    let shell = shell.clone();
    let label = label.into();
    div()
        .id(id)
        .cursor_pointer()
        .text_color(accent)
        .child(label)
        .on_click(move |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.dispatch_source(&ProductCommandSource::new(command), window, cx);
            })
        })
}
