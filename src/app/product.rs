//! Native product windows backed by application documents and workbenches.

use std::cell::RefCell;

use gpui::{prelude::FluentBuilder, *};

use super::{
    documents::{ApplicationDocuments, DocumentCollection, DocumentId},
    entry::OpenRequest,
    model::BufferModel,
    workbench::{
        CloseRequestOutcome, DocumentCloseDisposition, PaneId, SplitDirection, SplitPlacement,
        TabId, Workbench, WorkbenchLayout,
    },
};

actions!(
    product,
    [
        NewWindow,
        CloseWindow,
        NewTab,
        CloseTab,
        SplitHorizontal,
        SplitVertical
    ]
);

struct ApplicationWorkbenches(RefCell<Vec<WeakEntity<Workbench>>>);

impl Global for ApplicationWorkbenches {}

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

pub(crate) struct ProductShell {
    workbench: Entity<Workbench>,
    status: SharedString,
}

impl ProductShell {
    fn new(workbench: Entity<Workbench>) -> Self {
        Self {
            workbench,
            status: "ready".into(),
        }
    }

    fn documents(cx: &App) -> Entity<DocumentCollection> {
        cx.global::<ApplicationDocuments>().0.clone()
    }

    fn new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let documents = Self::documents(cx);
        let model = cx.new(|_| BufferModel::from_text(""));
        let document = documents.update(cx, |documents, cx| {
            documents.create_untitled("Untitled", model, cx)
        });
        let model = documents.read(cx).get(document).unwrap().model().clone();
        let editor = self.workbench.update(cx, |workbench, cx| {
            let pane = workbench
                .focused_pane_id()
                .expect("a product workbench always has a visible pane");
            workbench.open_tab_for_document(pane, document, model, cx);
            workbench.focused_pane().unwrap().active_tab().editor().clone()
        });
        editor.focus_handle(cx).focus(window);
        self.status = "new untitled document".into();
        cx.notify();
    }

    fn split(&mut self, direction: SplitDirection, window: &mut Window, cx: &mut Context<Self>) {
        let editor = self.workbench.update(cx, |workbench, cx| {
            workbench.split_focused(direction, SplitPlacement::After, cx)?;
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

    fn close_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((pane, tab, document)) = self.workbench.read(cx).focused_pane().map(|pane| {
            (
                pane.id(),
                pane.active_tab_id(),
                pane.active_tab().document_id(),
            )
        }) else {
            return;
        };
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
            return;
        };
        let transition = match outcome {
            CloseRequestOutcome::Pending(_) => {
                self.status = "save confirmation will be added in checkpoint 7".into();
                cx.notify();
                return;
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
            let model = documents
                .read(cx)
                .get(replacement)
                .unwrap()
                .model()
                .clone();
            let workbench = cx.new(|cx| {
                Workbench::new_for_document(replacement, model, cx)
            });
            cx.global::<ApplicationWorkbenches>().register(&workbench);
            self.workbench = workbench;
            self.status = "created replacement untitled document".into();
        } else {
            self.status = "closed tab".into();
        }
        self.focus_active_editor(window, cx);
        cx.notify();
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

    fn render_layout(
        &self,
        layout: &WorkbenchLayout,
        cx: &mut Context<Self>,
    ) -> AnyElement {
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
        window.set_window_title("Knot");

        div()
            .key_context("product")
            .on_action(cx.listener(|this, _: &NewTab, window, cx| {
                this.new_tab(window, cx)
            }))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                this.close_active_tab(window, cx)
            }))
            .on_action(cx.listener(|this, _: &SplitHorizontal, window, cx| {
                this.split(SplitDirection::Horizontal, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SplitVertical, window, cx| {
                this.split(SplitDirection::Vertical, window, cx)
            }))
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
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
                    .child(action_button(
                        "new tab",
                        "new-tab",
                        &entity,
                        |this, window, cx| this.new_tab(window, cx),
                    ))
                    .child(action_button(
                        "split →",
                        "split-horizontal",
                        &entity,
                        |this, window, cx| {
                            this.split(SplitDirection::Horizontal, window, cx)
                        },
                    ))
                    .child(action_button(
                        "split ↓",
                        "split-vertical",
                        &entity,
                        |this, window, cx| this.split(SplitDirection::Vertical, window, cx),
                    ))
                    .child(action_button(
                        "close tab",
                        "close-tab",
                        &entity,
                        |this, window, cx| this.close_active_tab(window, cx),
                    ))
                    .child(div().flex_1())
                    .child(self.status.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .when_some(layout, |element, layout| {
                        element.child(self.render_layout(&layout, cx))
                    }),
            )
            .into_any_element()
    }
}

fn action_button(
    label: &'static str,
    id: &'static str,
    shell: &Entity<ProductShell>,
    action: impl Fn(&mut ProductShell, &mut Window, &mut Context<ProductShell>) + 'static,
) -> Stateful<Div> {
    let shell = shell.clone();
    div()
        .id(id)
        .cursor_pointer()
        .text_color(rgb(0x80c0ff))
        .child(label)
        .on_click(move |_, window, cx| {
            shell.update(cx, |shell, cx| action(shell, window, cx))
        })
}

fn create_untitled_document(
    documents: &Entity<DocumentCollection>,
    cx: &mut App,
) -> DocumentId {
    let model = cx.new(|_| BufferModel::from_text(""));
    documents.update(cx, |documents, cx| {
        documents.create_untitled("Untitled", model, cx)
    })
}

fn document_for_request(
    request: Option<OpenRequest>,
    documents: &Entity<DocumentCollection>,
    cx: &mut App,
) -> DocumentId {
    let Some(request) = request else {
        return create_untitled_document(documents, cx);
    };
    if let Some(existing) = documents.read(cx).document_for_resource(request.uri()) {
        return existing;
    }
    let model = cx.new(|_| BufferModel::from_text(""));
    documents.update(cx, |documents, _| {
        documents.create_destination(request.title(), model, request.uri().clone())
    })
}

fn open_product_window(request: Option<OpenRequest>, cx: &mut App) {
    let documents = cx.global::<ApplicationDocuments>().0.clone();
    let document = document_for_request(request, &documents, cx);
    let model = documents.read(cx).get(document).unwrap().model().clone();
    let bounds = Bounds::centered(None, size(px(1000.), px(720.)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(480.), px(320.))),
            ..Default::default()
        },
        move |window, cx| {
            let workbench =
                cx.new(|cx| Workbench::new_for_document(document, model, cx));
            cx.global::<ApplicationWorkbenches>().register(&workbench);
            let shell = cx.new(|_| ProductShell::new(workbench));
            shell.read(cx).focus_active_editor(window, cx);
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
        cx.bind_keys([
            KeyBinding::new("cmd-n", NewWindow, Some("product")),
            KeyBinding::new("cmd-t", NewTab, Some("product")),
            KeyBinding::new("cmd-w", CloseTab, Some("product")),
            KeyBinding::new("cmd-shift-w", CloseWindow, Some("product")),
            KeyBinding::new("cmd-k right", SplitHorizontal, Some("product")),
            KeyBinding::new("cmd-k down", SplitVertical, Some("product")),
            KeyBinding::new("cmd-q", super::Quit, None),
        ]);
        cx.on_action(|_: &NewWindow, cx| open_product_window(None, cx));
        cx.on_action(|_: &super::Quit, cx| cx.quit());
        cx.set_menus(vec![
            Menu {
                name: "Knot".into(),
                items: vec![MenuItem::action("Quit Knot", super::Quit)],
            },
            Menu {
                name: "File".into(),
                items: vec![
                    MenuItem::action("New Window", NewWindow),
                    MenuItem::action("New Tab", NewTab),
                    MenuItem::action("Close Tab", CloseTab),
                    MenuItem::action("Close Window", CloseWindow),
                ],
            },
            Menu {
                name: "View".into(),
                items: vec![
                    MenuItem::action("Split Right", SplitHorizontal),
                    MenuItem::action("Split Down", SplitVertical),
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
    use gpui::{AppContext, TestAppContext};

    use super::{
        ApplicationDocuments, ApplicationWorkbenches, DocumentCollection, Entity,
        OpenRequest, ProductShell, RefCell, SplitDirection, Workbench, WorkbenchLayout,
        create_untitled_document, document_for_request,
    };

    fn install_globals(cx: &mut TestAppContext) -> Entity<DocumentCollection> {
        let documents = cx.new(|_| DocumentCollection::new());
        cx.set_global(ApplicationDocuments(documents.clone()));
        cx.set_global(ApplicationWorkbenches(RefCell::new(Vec::new())));
        documents
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
            assert_eq!(shell.read(cx).status, "created replacement untitled document");
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
    fn open_requests_use_destination_documents_and_global_deduplication(
        cx: &mut TestAppContext,
    ) {
        let documents = install_globals(cx);
        let request = OpenRequest::from_url("file:///tmp/knot-entry.txt").unwrap();
        let first = cx.update(|cx| {
            document_for_request(Some(request.clone()), &documents, cx)
        });
        let second = cx.update(|cx| document_for_request(Some(request), &documents, cx));

        assert_eq!(first, second);
        cx.read(|cx| {
            assert_eq!(documents.read(cx).documents().count(), 1);
            assert!(matches!(
                documents.read(cx).get(first).unwrap().state(),
                super::super::documents::DocumentState::Destination { .. }
            ));
        });
    }
}
