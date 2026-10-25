//! Native session replacement: staging, revalidation, and the single commit.

use std::rc::Rc;

use super::*;
use crate::app::extension_host::handlers::{AccessError, OpenError, ReplaceError};
use crate::app::product::sessions::{EntryOutcome, ReplaceWindowsResult};
use crate::app::recovery::{ApplicationRecovery, SnapshotKind};
use crate::app::workbench::TerminalSessionId;
use crate::host::lifecycle::ExtensionKey;
use serde_json::json;

const OWNER: &str = "@example/sessions";

struct Harness {
    documents: Entity<DocumentCollection>,
    host: Entity<crate::app::extension_host::ProductExtensionHost>,
    key: ExtensionKey,
    provider: Arc<MemoryFileSystemProvider>,
    read_gate: Arc<GatedFileSystemProvider>,
}

fn uri(path: &str) -> ResourceUri {
    ResourceUri::parse(&format!("mem://product/{path}")).unwrap()
}

fn harness(cx: &mut TestAppContext) -> Harness {
    let documents = install_globals(cx);
    let (_, provider) = memory_filesystems_with_provider();
    for directory in ["a", "b"] {
        provider.seed_directory(uri(directory)).unwrap();
    }
    provider.seed_file(uri("a/one.txt"), b"one").unwrap();
    provider.seed_file(uri("b/two.txt"), b"two").unwrap();
    let read_gate = Arc::new(GatedFileSystemProvider {
        inner: provider.clone(),
        read_release: Mutex::new(None),
        replace_release: Mutex::new(None),
    });
    let mut providers = FileSystemProviderRegistry::new();
    providers.register("mem", read_gate.clone()).unwrap();
    cx.set_global(ApplicationFileSystems(Arc::new(providers)));
    let host = cx.update(crate::app::extension_host::install);
    let key = ExtensionKey::new(ExtensionId::new(901), ExtensionLifecycleId::new(1));
    host.update(cx, |host, _| {
        host.load(key, crate::host::pool::ExtensionConfig::default())
            .unwrap()
    });
    Harness {
        documents,
        host,
        key,
        provider,
        read_gate,
    }
}

impl Harness {
    fn caller(&self) -> super::super::operations::OperationCaller {
        super::super::operations::OperationCaller {
            host: self.host.downgrade(),
            key: self.key,
            invocation: None,
        }
    }

    fn start(
        &self,
        request: serde_json::Value,
        cx: &mut TestAppContext,
    ) -> Rc<RefCell<Option<Result<ReplaceWindowsResult, ReplaceError>>>> {
        let settled = Rc::new(RefCell::new(None));
        let sink = settled.clone();
        let caller = self.caller();
        let admitted = cx.update(|cx| {
            super::super::sessions::replace_windows(
                serde_json::from_value(request).unwrap(),
                OWNER.into(),
                caller,
                move |result| *sink.borrow_mut() = Some(result),
                cx,
            )
        });
        if let Err(error) = admitted {
            *settled.borrow_mut() = Some(Err(error));
        }
        settled
    }

    fn replace(
        &self,
        request: serde_json::Value,
        cx: &mut TestAppContext,
    ) -> Result<ReplaceWindowsResult, ReplaceError> {
        let settled = self.start(request, cx);
        settle(&settled, cx)
    }

    /// A persisted file opened in a new window.
    fn file_window(
        &self,
        path: &str,
        cx: &mut TestAppContext,
    ) -> (
        crate::app::documents::DocumentId,
        Entity<ProductShell>,
        gpui::AnyWindowHandle,
    ) {
        let document = self.open_file(path, cx);
        let model = self.model(document, cx);
        let (shell, window) = product_window(document, model, cx);
        (document, shell, window)
    }

    fn open_file(&self, path: &str, cx: &mut TestAppContext) -> crate::app::documents::DocumentId {
        let file = pollster::block_on(self.provider.read(uri(path))).unwrap();
        let text = String::from_utf8(file.bytes).unwrap();
        let version = file.version;
        let model = cx.new(|_| BufferModel::from_text(text));
        self.documents.update(cx, |documents, cx| {
            documents.create_persisted(path.to_owned(), model, uri(path), 0, version, cx)
        })
    }

    fn untitled(&self, text: &str, cx: &mut TestAppContext) -> crate::app::documents::DocumentId {
        let model = cx.new(|_| BufferModel::from_text(""));
        let document = self.documents.update(cx, |documents, cx| {
            documents.create_untitled("Untitled", model.clone(), cx)
        });
        if !text.is_empty() {
            model.update(cx, |model, cx| {
                model.replace(0..0, text).unwrap();
                cx.notify();
            });
        }
        document
    }

    fn destination(
        &self,
        path: &str,
        cx: &mut TestAppContext,
    ) -> crate::app::documents::DocumentId {
        let model = cx.new(|_| BufferModel::from_text(""));
        let document = self.documents.update(cx, |documents, cx| {
            documents.create_destination(path.to_owned(), model.clone(), uri(path), cx)
        });
        model.update(cx, |model, cx| {
            model.replace(0..0, "drafted").unwrap();
            cx.notify();
        });
        document
    }

    fn model(
        &self,
        document: crate::app::documents::DocumentId,
        cx: &TestAppContext,
    ) -> Entity<BufferModel> {
        cx.read(|cx| {
            self.documents
                .read(cx)
                .get(document)
                .unwrap()
                .model()
                .clone()
        })
    }

    fn identity(&self, document: crate::app::documents::DocumentId, cx: &TestAppContext) -> String {
        cx.read(|cx| {
            self.documents
                .read(cx)
                .get(document)
                .unwrap()
                .identity()
                .to_string()
        })
    }

    fn edit(
        &self,
        document: crate::app::documents::DocumentId,
        text: &str,
        cx: &mut TestAppContext,
    ) {
        self.model(document, cx).update(cx, |model, cx| {
            let len = model.text().len();
            model.replace(len..len, text).unwrap();
            cx.notify();
        });
    }

    fn retain(
        &self,
        document: crate::app::documents::DocumentId,
        cx: &mut TestAppContext,
    ) -> String {
        let identity = uuid::Uuid::parse_str(&self.identity(document, cx)).unwrap();
        let task = cx
            .update(|cx| crate::app::recovery::retain(OWNER.into(), identity, cx))
            .unwrap();
        wait(task, cx).unwrap().capture_key
    }

    /// Document identities in a window's tabs, in layout order.
    fn tab_identities(&self, shell: &Entity<ProductShell>, cx: &TestAppContext) -> Vec<String> {
        cx.read(|cx| {
            let workbench = shell.read(cx).workbench.read(cx);
            workbench
                .tab_snapshots()
                .into_iter()
                .filter_map(|tab| match tab.surface {
                    TabSurfaceId::Document(document) => self
                        .documents
                        .read(cx)
                        .get(document)
                        .map(|document| document.identity().to_string()),
                    _ => None,
                })
                .collect()
        })
    }

    fn texts(&self, shell: &Entity<ProductShell>, cx: &TestAppContext) -> Vec<String> {
        cx.read(|cx| {
            let workbench = shell.read(cx).workbench.read(cx);
            workbench
                .tab_snapshots()
                .into_iter()
                .filter_map(|tab| match tab.surface {
                    TabSurfaceId::Document(document) => self
                        .documents
                        .read(cx)
                        .get(document)
                        .map(|document| document.model().read(cx).text()),
                    _ => None,
                })
                .collect()
        })
    }

    fn present(
        &self,
        shell: &Entity<ProductShell>,
        document: crate::app::documents::DocumentId,
        cx: &mut TestAppContext,
    ) {
        let model = self.model(document, cx);
        shell.update(cx, |shell, cx| {
            let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
            shell.workbench.update(cx, |workbench, cx| {
                workbench.open_tab_for_document(pane, document, model, cx);
            });
        });
    }
}

fn wait<T: 'static>(task: gpui::Task<T>, cx: &mut TestAppContext) -> T {
    let result = Rc::new(RefCell::new(None));
    let sink = result.clone();
    cx.spawn(async move |_| *sink.borrow_mut() = Some(task.await))
        .detach();
    cx.run_until_parked();
    result.borrow_mut().take().expect("task settled")
}

fn settle(
    settled: &Rc<RefCell<Option<Result<ReplaceWindowsResult, ReplaceError>>>>,
    cx: &mut TestAppContext,
) -> Result<ReplaceWindowsResult, ReplaceError> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        cx.run_until_parked();
        if let Some(result) = settled.borrow_mut().take() {
            return result;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "replacement did not settle"
        );
        std::thread::yield_now();
    }
}

fn window_id(window: gpui::AnyWindowHandle) -> String {
    window.window_id().as_u64().to_string()
}

fn root(shell: &Entity<ProductShell>, cx: &TestAppContext) -> Option<String> {
    cx.read(|cx| {
        shell
            .read(cx)
            .workspace
            .snapshot()
            .root()
            .map(ToString::to_string)
    })
}

fn product_windows(cx: &TestAppContext) -> usize {
    cx.read(|cx| {
        cx.windows()
            .into_iter()
            .filter(|window| window.downcast::<ProductShell>().is_some())
            .count()
    })
}

fn automatic_text(identity: &str, cx: &mut TestAppContext) -> Option<String> {
    let read = cx.read(|cx| {
        cx.global::<ApplicationRecovery>()
            .0
            .store()
            .snapshot(SnapshotKind::Automatic, identity.to_owned())
    });
    wait(read, cx).unwrap().map(|snapshot| snapshot.text)
}

fn conflict(result: Result<ReplaceWindowsResult, ReplaceError>) -> bool {
    matches!(result, Err(ReplaceError::Conflict { .. }))
}

#[gpui::test]
fn round_trip_suspends_and_restores_dirty_destination_and_untitled_documents(
    cx: &mut TestAppContext,
) {
    let h = harness(cx);
    let (one, shell, window) = h.file_window("a/one.txt", cx);
    let new = h.destination("a/new.txt", cx);
    let untitled = h.untitled("scratch", cx);
    h.present(&shell, new, cx);
    h.present(&shell, untitled, cx);
    h.edit(one, " edited", cx);
    let ids = [one, new, untitled].map(|document| h.identity(document, cx));
    let captures = [one, new, untitled].map(|document| h.retain(document, cx));

    // A → B: every final-view text is retained, so the tabs go without prompts.
    let result = h
        .replace(
            json!({
                "suspend": ids.iter().zip(&captures)
                    .map(|(document, capture)| json!({ "document": document, "capture": capture }))
                    .collect::<Vec<_>>(),
                "windows": [{ "target": window_id(window), "roots": ["mem://product/b/"],
                              "entries": [{ "uri": "mem://product/b/two.txt" }] }],
                "focus": 0,
            }),
            cx,
        )
        .unwrap();
    assert!(!cx.has_pending_prompt());
    assert!(matches!(
        result.windows[0].entries[..],
        [EntryOutcome::Opened { .. }]
    ));
    assert_eq!(root(&shell, cx).as_deref(), Some("mem://product/b"));
    assert_eq!(h.texts(&shell, cx), ["two"]);
    cx.read(|cx| {
        let documents = h.documents.read(cx);
        assert_eq!(documents.documents().count(), 1);
    });
    for id in &ids {
        assert_eq!(automatic_text(id, cx), None);
    }

    // B → A: captures restore with their text and dirty state; the clean
    // file leaves without a capture.
    let two = h.tab_identities(&shell, cx).remove(0);
    let result = h
        .replace(
            json!({
                "suspend": [{ "document": two, "capture": null }],
                "windows": [{ "target": window_id(window), "roots": ["mem://product/a/"],
                              "selected": 2,
                              "entries": [
                                  { "uri": "mem://product/a/one.txt", "document": ids[0], "capture": captures[0] },
                                  { "uri": "mem://product/a/new.txt", "document": ids[1], "capture": captures[1] },
                                  { "document": ids[2], "capture": captures[2] },
                              ] }],
            }),
            cx,
        )
        .unwrap();
    assert!(result.protected);
    let resources = result.windows[0]
        .entries
        .iter()
        .map(|entry| match entry {
            EntryOutcome::Restored { resource, .. } => *resource,
            other => panic!("unexpected outcome {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(resources, ["unchanged", "unchanged", "untitled"]);
    assert_eq!(root(&shell, cx).as_deref(), Some("mem://product/a"));
    assert_eq!(h.texts(&shell, cx), ["one edited", "drafted", "scratch"]);
    cx.read(|cx| {
        let shell = shell.read(cx);
        let workbench = shell.workbench.read(cx);
        let active = workbench.focused_pane().unwrap().active_tab();
        let documents = h.documents.read(cx);
        let active = documents.get(active.document_id().unwrap()).unwrap();
        assert_eq!(active.model().read(cx).text(), "scratch");
        assert_eq!(documents.documents().count(), 3);
        assert!(documents.documents().all(|document| document.is_dirty(cx)));
    });
    // Restored documents are durably protected when the operation resolves.
    for (index, entry) in result.windows[0].entries.iter().enumerate() {
        let EntryOutcome::Restored { document, .. } = entry else {
            unreachable!()
        };
        assert_eq!(
            automatic_text(document, cx).as_deref(),
            Some(["one edited", "drafted", "scratch"][index])
        );
    }
}

#[gpui::test]
fn live_documents_are_presented_instead_of_their_captures(cx: &mut TestAppContext) {
    let h = harness(cx);
    let (one, shell, window) = h.file_window("a/one.txt", cx);
    let (_, other_shell, _) = h.file_window("b/two.txt", cx);
    h.present(&other_shell, one, cx);
    h.edit(one, " shared", cx);
    let id = h.identity(one, cx);

    // The unbound window keeps the shared document live, so it needs no capture.
    h.replace(
        json!({
            "suspend": [{ "document": id, "capture": null }],
            "windows": [{ "target": window_id(window), "roots": ["mem://product/b/"], "entries": [] }],
        }),
        cx,
    )
    .unwrap();
    assert_eq!(h.identity(one, cx), id);
    let stale = h.retain(one, cx);
    h.edit(one, " newer", cx);

    // The live model is presented and the stale capture ignored; no copy exists.
    let placeholder = h.tab_identities(&shell, cx);
    let result = h
        .replace(
            json!({
                "suspend": placeholder.iter().map(|document| json!({ "document": document, "capture": null })).collect::<Vec<_>>(),
                "windows": [{ "target": window_id(window), "roots": ["mem://product/a/"],
                              "entries": [{ "uri": "mem://product/a/one.txt", "document": id, "capture": stale }] }],
            }),
            cx,
        )
        .unwrap();
    assert_eq!(
        result.windows[0].entries,
        [EntryOutcome::Live {
            document: id.clone()
        }]
    );
    assert_eq!(h.texts(&shell, cx), ["one shared newer"]);

    // A file reopened under a new identity is matched by its resource.
    let reopened = h
        .replace(
            json!({
                "suspend": [{ "document": id, "capture": null }],
                "windows": [{ "target": window_id(window), "roots": ["mem://product/a/"],
                              "entries": [{ "uri": "mem://product/a/one.txt",
                                            "document": uuid::Uuid::new_v4().to_string(),
                                            "capture": stale }] }],
            }),
            cx,
        )
        .unwrap();
    assert_eq!(
        reopened.windows[0].entries,
        [EntryOutcome::Live { document: id }]
    );
    cx.read(|cx| {
        let documents = h.documents.read(cx);
        assert_eq!(
            documents
                .documents()
                .filter(|document| document.model().read(cx).text().starts_with("one"))
                .count(),
            1
        );
    });
}

#[gpui::test]
fn multi_window_sessions_reuse_create_and_release_windows(cx: &mut TestAppContext) {
    let h = harness(cx);
    let (one, first_shell, first) = h.file_window("a/one.txt", cx);
    let untitled = h.untitled("", cx);
    let model = h.model(untitled, cx);
    let (second_shell, second) = product_window(untitled, model, cx);
    assert_eq!(product_windows(cx), 2);

    // Two source windows to one destination: the first is reused, the surplus
    // window has no terminal and closes.
    let suspend = json!([
        { "document": h.identity(one, cx), "capture": null },
        { "document": h.identity(untitled, cx), "capture": null },
    ]);
    h.replace(
        json!({
            "suspend": suspend,
            "windows": [{ "target": window_id(first), "roots": ["mem://product/b/"],
                          "entries": [{ "uri": "mem://product/b/two.txt" }] }],
            "release": [window_id(second)],
        }),
        cx,
    )
    .unwrap();
    assert_eq!(product_windows(cx), 1);
    assert!(!cx.read(|cx| cx.windows().contains(&second)));
    let _ = second_shell;

    // One source window to two destinations: the extra window is created
    // with its root and documents.
    let two = h.tab_identities(&first_shell, cx);
    let result = h
        .replace(
            json!({
                "suspend": [{ "document": two[0], "capture": null }],
                "windows": [
                    { "target": window_id(first), "roots": ["mem://product/a/"],
                      "entries": [{ "uri": "mem://product/a/one.txt" }] },
                    { "roots": ["mem://product/a/"], "entries": [{ "document": uuid::Uuid::new_v4().to_string() }] },
                ],
                "focus": 1,
            }),
            cx,
        )
        .unwrap();
    assert_eq!(product_windows(cx), 2);
    assert_eq!(result.windows[1].entries, [EntryOutcome::Missing]);
    let created = cx.read(|cx| {
        cx.windows()
            .into_iter()
            .find(|window| *window != first && window.downcast::<ProductShell>().is_some())
            .unwrap()
    });
    assert_eq!(result.windows[1].window.window, window_id(created));
    let created_shell = cx
        .update_window(created, |root, _, _| {
            root.downcast::<ProductShell>().unwrap()
        })
        .unwrap();
    assert_eq!(root(&created_shell, cx).as_deref(), Some("mem://product/a"));
    // A destination without documents gets an untitled document, so its
    // root's explorer dock never fills the window.
    assert_eq!(h.texts(&created_shell, cx), [""]);
    cx.read(|cx| {
        assert!(
            created_shell
                .read(cx)
                .workbench
                .read(cx)
                .normal_pane()
                .is_some()
        )
    });
    assert_eq!(h.texts(&first_shell, cx), ["one"]);
}

#[gpui::test]
fn released_window_with_a_terminal_is_cleared_and_the_terminal_keeps_running(
    cx: &mut TestAppContext,
) {
    let h = harness(cx);
    let (one, _, first) = h.file_window("a/one.txt", cx);
    let (two, second_shell, second) = h.file_window("a/one.txt", cx);
    assert_eq!(one, two);
    second_shell.update(cx, |shell, cx| {
        let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
        shell.workbench.update(cx, |workbench, _| {
            workbench.open_terminal_tab_test_double(pane, TerminalSessionId(77))
        });
        let _ = shell.replace_workspace_roots(vec![uri("a")], cx);
    });
    h.replace(
        json!({
            "suspend": [{ "document": h.identity(one, cx), "capture": null }],
            "windows": [{ "target": window_id(first), "roots": ["mem://product/b/"], "entries": [] }],
            "release": [window_id(second)],
        }),
        cx,
    )
    .unwrap();
    assert!(cx.read(|cx| cx.windows().contains(&second)));
    assert_eq!(root(&second_shell, cx), None);
    cx.read(|cx| {
        let tabs = second_shell.read(cx).workbench.read(cx).tab_snapshots();
        assert!(
            !tabs
                .iter()
                .any(|tab| matches!(tab.surface, TabSurfaceId::Document(_)))
        );
        assert!(
            tabs.iter()
                .any(|tab| tab.surface == TabSurfaceId::Terminal(TerminalSessionId(77)))
        );
    });
}

#[gpui::test]
fn stale_sources_reject_and_leave_everything_untouched(cx: &mut TestAppContext) {
    let h = harness(cx);
    let (one, shell, window) = h.file_window("a/one.txt", cx);
    h.edit(one, " dirty", cx);
    let id = h.identity(one, cx);
    let capture = h.retain(one, cx);
    let request = |capture: &str| {
        json!({
            "suspend": [{ "document": id, "capture": capture }],
            "windows": [{ "target": window_id(window), "roots": ["mem://product/b/"],
                          "entries": [{ "uri": "mem://product/b/two.txt" }] }],
        })
    };

    // An edit racing between retain and commit.
    h.edit(one, " racing", cx);
    assert!(conflict(h.replace(request(&capture), cx)));
    // A dirty final view without any capture.
    assert!(conflict(h.replace(
        json!({
            "suspend": [{ "document": id, "capture": null }],
            "windows": [{ "target": window_id(window), "roots": [], "entries": [] }],
        }),
        cx
    )));
    // An unexpected new tab in a source window.
    let capture = h.retain(one, cx);
    let extra = h.untitled("", cx);
    h.present(&shell, extra, cx);
    assert!(conflict(h.replace(request(&capture), cx)));
    assert_eq!(h.texts(&shell, cx), ["one dirty racing", ""]);
    assert_eq!(root(&shell, cx), None);

    // A closed target.
    let (_, _, closed) = h.file_window("b/two.txt", cx);
    cx.update_window(closed, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    assert!(matches!(
        h.replace(
            json!({ "suspend": [], "windows": [{ "target": window_id(closed), "roots": [], "entries": [] }] }),
            cx
        ),
        Err(ReplaceError::Resource(OpenError::Access(
            AccessError::InvalidTarget
        )))
    ));
    assert_eq!(h.texts(&shell, cx), ["one dirty racing", ""]);
}

#[gpui::test]
fn concurrent_operations_and_caller_unload_during_staging_leave_the_source(
    cx: &mut TestAppContext,
) {
    let h = harness(cx);
    let (one, shell, window) = h.file_window("a/one.txt", cx);
    let request = json!({
        "suspend": [{ "document": h.identity(one, cx), "capture": null }],
        "windows": [{ "target": window_id(window), "roots": ["mem://product/b/"],
                      "entries": [{ "uri": "mem://product/b/two.txt" }] }],
    });
    let (release, gate) = tokio::sync::oneshot::channel();
    *h.read_gate.read_release.lock().unwrap() = Some(gate);
    h.host
        .update(cx, |host, _| host.take_test_application_events());
    let pending = h.start(request.clone(), cx);
    cx.run_until_parked();
    assert!(pending.borrow().is_none());
    // Staging publishes no observation events.
    assert!(
        h.host
            .update(cx, |host, _| host.take_test_application_events())
            .is_empty()
    );

    // Another replacement or closure cannot interleave with the staged one.
    assert!(matches!(
        h.replace(request.clone(), cx),
        Err(ReplaceError::OperationInProgress)
    ));
    let target = cx
        .update_window(window, |_, native, cx| {
            shell.update(cx, |shell, cx| {
                shell.focus_active_surface(native, cx);
                shell.capture_command_target(native, cx).unwrap()
            })
        })
        .unwrap();
    let (completion, mut closed) = crate::app::CommandCompletion::new();
    cx.update(|cx| {
        ProductShell::begin_protected_close(
            super::super::ProtectedCloseKind::Window,
            target,
            Some(completion),
            cx,
        )
    });
    cx.run_until_parked();
    assert_eq!(closed.try_recv().unwrap(), CommandOutcome::Unavailable);
    assert!(cx.read(|cx| cx.windows().contains(&window)));

    // Unloading the caller before the commit cancels it.
    h.host
        .update(cx, |host, cx| host.unload(h.key, cx).unwrap());
    release.send(()).unwrap();
    assert!(matches!(
        settle(&pending, cx),
        Err(ReplaceError::Resource(OpenError::Access(
            AccessError::Cancelled
        )))
    ));
    assert_eq!(h.texts(&shell, cx), ["one"]);
    assert_eq!(root(&shell, cx), None);
    // The protected slot is free again.
    assert!(!cx.read(ProductShell::protected_closure_is_active));
}

#[gpui::test]
fn committed_replacement_publishes_one_set_of_events(cx: &mut TestAppContext) {
    let h = harness(cx);
    let (one, _, window) = h.file_window("a/one.txt", cx);
    cx.run_until_parked();
    h.host
        .update(cx, |host, _| host.take_test_application_events());
    h.replace(
        json!({
            "suspend": [{ "document": h.identity(one, cx), "capture": null }],
            "windows": [{ "target": window_id(window), "roots": ["mem://product/b/"],
                          "entries": [{ "uri": "mem://product/b/two.txt" }] }],
        }),
        cx,
    )
    .unwrap();
    let events = h
        .host
        .update(cx, |host, _| host.take_test_application_events());
    let kinds = events
        .iter()
        .map(|event| serde_json::to_value(&event.change).unwrap()["type"].clone())
        .collect::<Vec<_>>();
    // The removed document, the opened one, and the window's new roots and
    // tabs arrive together; no intermediate empty window is observed.
    assert!(kinds.contains(&json!("documentClosed")));
    assert!(kinds.contains(&json!("documentOpened")));
    assert!(kinds.contains(&json!("workspaceChanged")));
    let sequences = events
        .iter()
        .map(|event| event.sequence)
        .collect::<Vec<_>>();
    assert!(sequences.windows(2).all(|pair| pair[1] == pair[0] + 1));
    assert_eq!(
        events
            .iter()
            .filter(|event| serde_json::to_value(&event.change).unwrap()["type"]
                == "presentationsChanged")
            .count(),
        1
    );
}

#[gpui::test]
fn missing_changed_and_failed_entries_report_while_the_rest_commits(cx: &mut TestAppContext) {
    let h = harness(cx);
    let (one, shell, window) = h.file_window("a/one.txt", cx);
    h.edit(one, " captured", cx);
    let id = h.identity(one, cx);
    let capture = h.retain(one, cx);
    h.provider.seed_file(uri("b/bad.txt"), [0xff]).unwrap();
    h.replace(
        json!({
            "suspend": [{ "document": id, "capture": capture }],
            "windows": [{ "target": window_id(window), "roots": ["mem://product/b/"],
                          "entries": [{ "uri": "mem://product/b/two.txt" }] }],
        }),
        cx,
    )
    .unwrap();
    // The file changes on disk while its session is suspended.
    h.provider
        .seed_file(uri("a/one.txt"), b"changed on disk")
        .unwrap();
    let two = h.tab_identities(&shell, cx);
    let result = h
        .replace(
            json!({
                "suspend": [{ "document": two[0], "capture": null }],
                "windows": [{ "target": window_id(window), "roots": ["mem://product/a/"],
                              "entries": [
                                  { "uri": "mem://product/a/one.txt", "document": id, "capture": capture },
                                  { "uri": "mem://product/a/gone.txt" },
                                  { "uri": "mem://product/b/bad.txt" },
                              ] }],
            }),
            cx,
        )
        .unwrap();
    let entries = &result.windows[0].entries;
    assert!(matches!(
        &entries[0],
        EntryOutcome::Restored {
            resource: "changed",
            ..
        }
    ));
    assert_eq!(entries[1], EntryOutcome::Missing);
    assert!(matches!(
        &entries[2],
        EntryOutcome::Failed {
            code: "InvalidUtf8",
            ..
        }
    ));
    assert_eq!(h.texts(&shell, cx), ["one captured"]);
    // The capture-time version is kept, so Save reaches the conflict prompt.
    cx.read(|cx| {
        let documents = h.documents.read(cx);
        let restored = documents.documents().next().unwrap();
        assert!(restored.is_dirty(cx));
        assert!(matches!(restored.state(), DocumentState::Persisted { .. }));
    });

    // A destination root that no longer exists rejects before any change.
    let restored = h.tab_identities(&shell, cx);
    let restored_capture = h.retain(
        cx.read(|cx| h.documents.read(cx).documents().next().unwrap().id()),
        cx,
    );
    assert!(matches!(
        h.replace(
            json!({
                "suspend": [{ "document": restored[0], "capture": restored_capture }],
                "windows": [{ "target": window_id(window), "roots": ["mem://product/missing/"], "entries": [] }],
            }),
            cx,
        ),
        Err(ReplaceError::Resource(OpenError::ResourceNotFound))
    ));
    assert_eq!(h.texts(&shell, cx), ["one captured"]);
}

#[gpui::test]
fn unavailable_recovery_during_retain_removes_nothing(cx: &mut TestAppContext) {
    let h = harness(cx);
    install_failing_recovery(cx);
    let (one, shell, window) = h.file_window("a/one.txt", cx);
    h.edit(one, " unsaved", cx);
    let identity = uuid::Uuid::parse_str(&h.identity(one, cx)).unwrap();
    let retain = cx
        .update(|cx| crate::app::recovery::retain(OWNER.into(), identity, cx))
        .unwrap();
    assert!(wait(retain, cx).is_err());
    assert!(conflict(h.replace(
        json!({
            "suspend": [{ "document": identity.to_string(), "capture": null }],
            "windows": [{ "target": window_id(window), "roots": ["mem://product/b/"], "entries": [] }],
        }),
        cx
    )));
    assert_eq!(h.texts(&shell, cx), ["one unsaved"]);
}

#[gpui::test]
fn interrupted_switch_restores_every_text_exactly_once(cx: &mut TestAppContext) {
    let h = harness(cx);
    let (one, shell, window) = h.file_window("a/one.txt", cx);
    let untitled = h.untitled("loose", cx);
    h.present(&shell, untitled, cx);
    h.edit(one, " unsaved", cx);
    let ids = [one, untitled].map(|document| h.identity(document, cx));
    let captures = [one, untitled].map(|document| h.retain(document, cx));
    let flush = cx.read(|cx| cx.global::<ApplicationRecovery>().0.store().flush());
    wait(flush, cx).unwrap();

    // The process stops after the suspended record is durable and before the
    // commit: a fresh application restores automatic snapshots by hot exit.
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    let fresh = cx.new(|_| DocumentCollection::new());
    cx.set_global(ApplicationDocuments(fresh.clone()));
    let restore =
        cx.update(|cx| crate::app::recovery::restore_at_startup(ProductShell::filesystems(cx), cx));
    let restored = wait(restore, cx).restored;
    assert_eq!(restored.len(), 2);
    let placeholder = cx.update(|cx| create_untitled_document(&fresh, cx));
    let model = cx.read(|cx| fresh.read(cx).get(placeholder).unwrap().model().clone());
    let (shell, window) = product_window(placeholder, model, cx);
    let placeholder = cx.read(|cx| {
        fresh
            .read(cx)
            .get(placeholder)
            .unwrap()
            .identity()
            .to_string()
    });

    // Selecting the suspended project presents the hot-exit models.
    let result = h
        .replace(
            json!({
                "suspend": [{ "document": placeholder, "capture": null }],
                "windows": [{ "target": window_id(window), "roots": ["mem://product/a/"],
                              "entries": [
                                  { "uri": "mem://product/a/one.txt", "document": ids[0], "capture": captures[0] },
                                  { "document": ids[1], "capture": captures[1] },
                              ] }],
            }),
            cx,
        )
        .unwrap();
    assert!(
        result.windows[0]
            .entries
            .iter()
            .all(|entry| matches!(entry, EntryOutcome::Live { .. }))
    );
    let texts = cx.read(|cx| {
        fresh
            .read(cx)
            .documents()
            .map(|document| document.model().read(cx).text())
            .collect::<Vec<_>>()
    });
    assert_eq!(texts.len(), 2);
    assert!(texts.contains(&"one unsaved".to_owned()));
    assert!(texts.contains(&"loose".to_owned()));
    let _ = shell;
}
