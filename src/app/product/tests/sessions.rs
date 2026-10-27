//! Native session replacement: staging, revalidation, and the single commit.

use std::rc::Rc;

use super::*;
use crate::app::extension_host::handlers::{AccessError, OpenError, ReplaceError};
use crate::app::product::sessions::{EntryOutcome, ReplaceWindowsResult};
use crate::app::recovery::{ApplicationRecovery, SnapshotKind};
use crate::app::workbench::{DockLocation, SplitPlacement, ToolPlacement};
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
                Some(caller),
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

/// The shell of a window the replacement created.
fn created(
    result: &ReplaceWindowsResult,
    index: usize,
    cx: &TestAppContext,
) -> (Entity<ProductShell>, gpui::AnyWindowHandle) {
    let id = &result.windows[index].window.window;
    cx.read(|cx| {
        let window = cx
            .windows()
            .into_iter()
            .find(|window| window_id(*window) == *id)
            .expect("created window is open");
        (
            window
                .downcast::<ProductShell>()
                .unwrap()
                .entity(cx)
                .unwrap(),
            window,
        )
    })
}

fn root(shell: &Entity<ProductShell>, cx: &TestAppContext) -> Option<String> {
    cx.read(|cx| {
        shell
            .read(cx)
            .workspace
            .snapshot()
            .first_root()
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
                "windows": [{ "roots": ["mem://product/b/"],
                              "entries": [{ "uri": "mem://product/b/two.txt" }] }],
                "release": [window_id(window)],
                "focus": 0,
            }),
            cx,
        )
        .unwrap();
    assert!(!cx.has_pending_prompt());
    assert!(!cx.read(|cx| cx.windows().contains(&window)));
    let (shell, window) = created(&result, 0, cx);
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
                "windows": [{ "roots": ["mem://product/a/"],
                              "selected": 2,
                              "entries": [
                                  { "uri": "mem://product/a/one.txt", "document": ids[0], "capture": captures[0] },
                                  { "uri": "mem://product/a/new.txt", "document": ids[1], "capture": captures[1] },
                                  { "document": ids[2], "capture": captures[2] },
                              ] }],
                "release": [window_id(window)],
            }),
            cx,
        )
        .unwrap();
    assert!(result.protected);
    let (shell, _) = created(&result, 0, cx);
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
    let (one, _, window) = h.file_window("a/one.txt", cx);
    let (_, other_shell, _) = h.file_window("b/two.txt", cx);
    h.present(&other_shell, one, cx);
    h.edit(one, " shared", cx);
    let id = h.identity(one, cx);

    // The unbound window keeps the shared document live, so it needs no capture.
    let result = h
        .replace(
            json!({
                "suspend": [{ "document": id, "capture": null }],
                "windows": [{ "roots": ["mem://product/b/"], "entries": [] }],
                "release": [window_id(window)],
            }),
            cx,
        )
        .unwrap();
    let (shell, window) = created(&result, 0, cx);
    assert_eq!(h.identity(one, cx), id);
    let stale = h.retain(one, cx);
    h.edit(one, " newer", cx);

    // The live model is presented and the stale capture ignored; no copy exists.
    let placeholder = h.tab_identities(&shell, cx);
    let result = h
        .replace(
            json!({
                "suspend": placeholder.iter().map(|document| json!({ "document": document, "capture": null })).collect::<Vec<_>>(),
                "windows": [{ "roots": ["mem://product/a/"],
                              "entries": [{ "uri": "mem://product/a/one.txt", "document": id, "capture": stale }] }],
                "release": [window_id(window)],
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
    let (shell, window) = created(&result, 0, cx);
    assert_eq!(h.texts(&shell, cx), ["one shared newer"]);

    // A file reopened under a new identity is matched by its resource.
    let reopened = h
        .replace(
            json!({
                "suspend": [{ "document": id, "capture": null }],
                "windows": [{ "roots": ["mem://product/a/"],
                              "entries": [{ "uri": "mem://product/a/one.txt",
                                            "document": uuid::Uuid::new_v4().to_string(),
                                            "capture": stale }] }],
                "release": [window_id(window)],
            }),
            cx,
        )
        .unwrap();
    assert_eq!(
        reopened.windows[0].entries,
        [EntryOutcome::Live { document: id }]
    );
    let _ = shell;
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
fn multi_window_sessions_close_every_source_and_create_every_destination(cx: &mut TestAppContext) {
    let h = harness(cx);
    let (one, _, first) = h.file_window("a/one.txt", cx);
    let untitled = h.untitled("", cx);
    let model = h.model(untitled, cx);
    let (_, second) = product_window(untitled, model, cx);
    assert_eq!(product_windows(cx), 2);

    // Two source windows to one destination: both sources close.
    let suspend = json!([
        { "document": h.identity(one, cx), "capture": null },
        { "document": h.identity(untitled, cx), "capture": null },
    ]);
    let result = h
        .replace(
            json!({
                "suspend": suspend,
                "windows": [{ "roots": ["mem://product/b/"],
                              "entries": [{ "uri": "mem://product/b/two.txt" }] }],
                "release": [window_id(first), window_id(second)],
            }),
            cx,
        )
        .unwrap();
    assert_eq!(product_windows(cx), 1);
    assert!(!cx.read(|cx| cx.windows().contains(&first) || cx.windows().contains(&second)));
    let (shell, window) = created(&result, 0, cx);
    assert_eq!(h.texts(&shell, cx), ["two"]);

    // One source window to two destinations, each created with its roots.
    let two = h.tab_identities(&shell, cx);
    let result = h
        .replace(
            json!({
                "suspend": [{ "document": two[0], "capture": null }],
                "windows": [
                    { "roots": ["mem://product/a/"],
                      "entries": [{ "uri": "mem://product/a/one.txt" }] },
                    { "roots": ["mem://product/a/", "mem://product/b/", "mem://product/./a/"],
                      "entries": [{ "document": uuid::Uuid::new_v4().to_string() }] },
                ],
                "release": [window_id(window)],
                "focus": 1,
            }),
            cx,
        )
        .unwrap();
    assert_eq!(product_windows(cx), 2);
    assert_eq!(result.windows[1].entries, [EntryOutcome::Missing]);
    assert_eq!(result.windows[1].layout, None);
    let (first_shell, _) = created(&result, 0, cx);
    let (created_shell, created_window) = created(&result, 1, cx);
    assert!(cx.read(|cx| cx.active_window()) == Some(created_window));
    // Multiple roots install in order, deduplicated after normalization.
    let created_roots = cx.read(|cx| {
        created_shell
            .read(cx)
            .workspace
            .snapshot()
            .roots()
            .iter()
            .map(|root| root.uri.to_string())
            .collect::<Vec<_>>()
    });
    assert_eq!(created_roots, ["mem://product/a", "mem://product/b"]);
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
fn released_windows_close_and_stop_their_terminals(cx: &mut TestAppContext) {
    let h = harness(cx);
    let (one, shell, window) = h.file_window("a/one.txt", cx);
    let session = shell.update(cx, |shell, cx| {
        let (id, view) = shell.create_terminal(cx);
        let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
        shell.workbench.update(cx, |workbench, _| {
            workbench.open_terminal_tab(pane, id, view)
        });
        id
    });
    let live = |cx: &TestAppContext| {
        cx.read(|cx| {
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions
                .contains_key(&session)
        })
    };
    assert!(live(cx));
    h.replace(
        json!({
            "suspend": [{ "document": h.identity(one, cx), "capture": null }],
            "windows": [{ "roots": ["mem://product/b/"], "entries": [] }],
            "release": [window_id(window)],
        }),
        cx,
    )
    .unwrap();
    assert!(!cx.read(|cx| cx.windows().contains(&window)));
    assert!(!live(cx));
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
            "release": [window_id(window)], "windows": [{ "roots": ["mem://product/b/"],
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
            "release": [window_id(window)], "windows": [{ "roots": [], "entries": [] }],
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
            json!({ "suspend": [], "release": [window_id(closed)], "windows": [{ "roots": [], "entries": [] }] }),
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
        "release": [window_id(window)], "windows": [{ "roots": ["mem://product/b/"],
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
            "release": [window_id(window)], "windows": [{ "roots": ["mem://product/b/"],
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
    // The removed document, the opened one, the closed source window, and
    // the created window with its roots and tabs arrive together; no
    // intermediate empty window is observed.
    for kind in [
        "documentClosed",
        "documentOpened",
        "windowClosed",
        "windowOpened",
    ] {
        assert!(kinds.contains(&json!(kind)), "missing {kind} in {kinds:?}");
    }
    let opened = events
        .iter()
        .find_map(|event| match &event.change {
            crate::host::protocol::ApplicationChange::WindowOpened { window } => Some(window),
            _ => None,
        })
        .unwrap();
    assert_eq!(opened.workspace.roots[0].uri, "mem://product/b");
    assert_eq!(opened.panes[0].tabs.len(), 1);
    let sequences = events
        .iter()
        .map(|event| event.sequence)
        .collect::<Vec<_>>();
    assert!(sequences.windows(2).all(|pair| pair[1] == pair[0] + 1));
    assert!(!kinds.contains(&json!("presentationsChanged")));
}

#[gpui::test]
fn missing_changed_and_failed_entries_report_while_the_rest_commits(cx: &mut TestAppContext) {
    let h = harness(cx);
    let (one, _, window) = h.file_window("a/one.txt", cx);
    h.edit(one, " captured", cx);
    let id = h.identity(one, cx);
    let capture = h.retain(one, cx);
    h.provider.seed_file(uri("b/bad.txt"), [0xff]).unwrap();
    let suspended = h
        .replace(
            json!({
                "suspend": [{ "document": id, "capture": capture }],
                "release": [window_id(window)], "windows": [{ "roots": ["mem://product/b/"],
                              "entries": [{ "uri": "mem://product/b/two.txt" }] }],
            }),
            cx,
        )
        .unwrap();
    let (shell, window) = created(&suspended, 0, cx);
    // The file changes on disk while its session is suspended.
    h.provider
        .seed_file(uri("a/one.txt"), b"changed on disk")
        .unwrap();
    let two = h.tab_identities(&shell, cx);
    let result = h
        .replace(
            json!({
                "suspend": [{ "document": two[0], "capture": null }],
                "release": [window_id(window)], "windows": [{ "roots": ["mem://product/a/"],
                              "entries": [
                                  { "uri": "mem://product/a/one.txt", "document": id, "capture": capture },
                                  { "uri": "mem://product/a/gone.txt" },
                                  { "uri": "mem://product/b/bad.txt" },
                              ] }],
            }),
            cx,
        )
        .unwrap();
    let (shell, window) = created(&result, 0, cx);
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
                "release": [window_id(window)], "windows": [{ "roots": ["mem://product/missing/"], "entries": [] }],
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
            "release": [window_id(window)], "windows": [{ "roots": ["mem://product/b/"], "entries": [] }],
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
                "release": [window_id(window)], "windows": [{ "roots": ["mem://product/a/"],
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

fn capture_layout(window: gpui::AnyWindowHandle, cx: &mut TestAppContext) -> serde_json::Value {
    let target = crate::host::protocol::WindowTarget {
        window: window_id(window),
    };
    let layout = cx
        .update(|cx| super::super::sessions::capture_layout(&target, cx))
        .unwrap();
    serde_json::to_value(layout).unwrap()
}

/// The parts of a layout restoration reproduces exactly; geometry is fitted
/// to the displays instead.
fn arrangement(layout: &serde_json::Value) -> serde_json::Value {
    json!({ "root": layout["root"], "focused": layout["focused"], "dockSizes": layout["dockSizes"] })
}

fn editors(
    shell: &Entity<ProductShell>,
    cx: &TestAppContext,
) -> Vec<Entity<crate::app::editor::EditorView>> {
    cx.read(|cx| {
        let workbench = shell.read(cx).workbench.read(cx);
        workbench
            .panes()
            .iter()
            .flat_map(|pane| pane.tabs())
            .filter_map(|tab| tab.editor().cloned())
            .collect()
    })
}

fn terminal_sessions(shell: &Entity<ProductShell>, cx: &TestAppContext) -> Vec<TabSurfaceId> {
    cx.read(|cx| {
        shell
            .read(cx)
            .workbench
            .read(cx)
            .tab_snapshots()
            .into_iter()
            .map(|tab| tab.surface)
            .filter(|surface| matches!(surface, TabSurfaceId::Terminal(_)))
            .collect()
    })
}

#[gpui::test]
fn layouts_restore_splits_shared_views_docks_and_fresh_terminals(cx: &mut TestAppContext) {
    use crate::app::editor::EditorViewState;
    let h = harness(cx);
    // Long enough that restored scroll positions are not clamped by paint.
    let text = format!("first\nsecond\nthird\nfourth\n{}", "line\n".repeat(400));
    h.provider
        .seed_file(uri("a/long.txt"), text.as_bytes())
        .unwrap();
    let (long, shell, window) = h.file_window("a/long.txt", cx);
    let stored = cx.read(|cx| {
        cx.global::<crate::app::tool_layout::ApplicationToolLayout>()
            .0
            .current()
    });
    let states = [
        EditorViewState {
            anchor: 6,
            caret: 12,
            top: 6,
            scroll_x: 4.,
        },
        EditorViewState {
            anchor: 20,
            caret: 20,
            top: 13,
            scroll_x: 0.,
        },
    ];
    shell.update(cx, |shell, cx| {
        shell.replace_workspace_roots(vec![uri("a")], cx);
        let pane = shell.workbench.read(cx).normal_pane().unwrap();
        let (terminal, view) = shell.create_terminal(cx);
        shell.workbench.update(cx, |workbench, cx| {
            let split = workbench
                .split_pane(pane, SplitDirection::Horizontal, SplitPlacement::After, cx)
                .unwrap();
            workbench.open_terminal_tab(split, terminal, view);
            workbench.set_dock_size(DockLocation::Left, 310.);
            workbench.focus_pane(split);
        });
    });
    for (editor, state) in editors(&shell, cx).iter().zip(states) {
        editor.update(cx, |editor, cx| editor.restore_view_state(state, cx));
    }
    let source_terminals = terminal_sessions(&shell, cx);
    let layout = capture_layout(window, cx);
    assert_eq!(layout["root"]["kind"], "split");
    assert_eq!(layout["root"]["first"]["dock"], "left");

    let result = h
        .replace(
            json!({
                "suspend": [{ "document": h.identity(long, cx), "capture": null }],
                "windows": [{ "roots": ["mem://product/a/"], "layout": layout,
                              "entries": [{ "uri": "mem://product/a/long.txt" }] }],
                "release": [window_id(window)],
            }),
            cx,
        )
        .unwrap();
    assert_eq!(
        result.windows[0].layout,
        Some(super::super::sessions::LayoutOutcome::Applied)
    );
    let (restored, restored_window) = created(&result, 0, cx);
    // The same arrangement, focus, dock size, and per-view state come back.
    assert_eq!(
        arrangement(&capture_layout(restored_window, cx)),
        arrangement(&layout)
    );
    let views = editors(&restored, cx);
    assert_eq!(views.len(), 2);
    assert_ne!(views[0], views[1]);
    cx.read(|cx| {
        assert_eq!(views[0].read(cx).model(), views[1].read(cx).model());
        assert_eq!(views[0].read(cx).view_state(cx), states[0]);
        assert_eq!(views[1].read(cx).view_state(cx), states[1]);
    });
    // The terminal restores in place as a fresh shell; the old one stopped.
    let terminals = terminal_sessions(&restored, cx);
    assert_eq!(terminals.len(), 1);
    assert_ne!(terminals, source_terminals);
    cx.read(|cx| {
        let sessions = &cx.global::<ApplicationTerminalSessions>().0;
        let TabSurfaceId::Terminal(fresh) = terminals[0] else {
            unreachable!()
        };
        let TabSurfaceId::Terminal(stopped) = source_terminals[0] else {
            unreachable!()
        };
        assert!(sessions.borrow().sessions.contains_key(&fresh));
        assert!(!sessions.borrow().sessions.contains_key(&stopped));
    });

    // A tool sharing a document group is placed exactly too.
    let tab = restored.update(cx, |shell, cx| {
        let explorer = shell.explorer_frame(cx).unwrap();
        let pane = shell.workbench.read(cx).normal_pane().unwrap();
        shell.workbench.update(cx, |workbench, _| {
            workbench.place_tool(explorer, ToolPlacement::Group(pane));
            workbench.focus_pane(pane);
        });
        pane
    });
    let shared = capture_layout(restored_window, cx);
    assert_eq!(shared["root"]["kind"], "split");
    let result = h
        .replace(
            json!({
                "suspend": [{ "document": h.identity(long, cx), "capture": null }],
                "windows": [{ "roots": ["mem://product/a/"], "layout": shared,
                              "entries": [{ "uri": "mem://product/a/long.txt" }] }],
                "release": [window_id(restored_window)],
            }),
            cx,
        )
        .unwrap();
    let (again, again_window) = created(&result, 0, cx);
    assert_eq!(
        arrangement(&capture_layout(again_window, cx)),
        arrangement(&shared)
    );
    let _ = (tab, again);
    // Restoration never publishes the fresh-window tool default.
    assert_eq!(
        cx.read(|cx| {
            cx.global::<crate::app::tool_layout::ApplicationToolLayout>()
                .0
                .current()
        }),
        stored
    );
}

#[gpui::test]
fn layouts_prune_unknown_tabs_hide_unnamed_tools_and_ignore_unsupported_versions(
    cx: &mut TestAppContext,
) {
    let h = harness(cx);
    let (one, _, window) = h.file_window("a/one.txt", cx);
    let untitled = h.untitled("loose", cx);
    let view = json!({ "anchor": 0, "caret": 2, "top": 0, "scrollX": 0 });
    // The explorer is registered by the roots but not named, the second
    // pane holds only an unknown tool and an unmatched document, and the
    // first pane repeats its document.
    let layout = json!({
        "version": 1, "geometry": null, "focused": 1,
        "dockSizes": { "left": 200, "right": 240, "bottom": 220 },
        "root": { "kind": "split", "direction": "vertical",
            "first": { "kind": "pane", "dock": null, "selected": 5, "tabs": [
                { "kind": "document", "document": null, "uri": "mem://product/a/one.txt", "view": view },
                { "kind": "document", "document": null, "uri": "mem://product/a/./one.txt", "view": view },
            ] },
            "second": { "kind": "pane", "dock": "bottom", "selected": 0, "tabs": [
                { "kind": "tool", "tool": "@example/gone.view" },
                { "kind": "document", "document": "not-an-entry", "uri": null, "view": view },
            ] } },
    });
    let result = h
        .replace(
            json!({
                "suspend": [
                    { "document": h.identity(one, cx), "capture": null },
                ],
                "windows": [
                    { "roots": ["mem://product/a/"], "layout": layout,
                      "entries": [{ "uri": "mem://product/a/one.txt" },
                                  { "document": h.identity(untitled, cx) }] },
                    { "roots": [], "layout": { "version": 99 },
                      "entries": [{ "uri": "mem://product/b/two.txt" }] },
                ],
                "release": [window_id(window)],
            }),
            cx,
        )
        .unwrap();
    use super::super::sessions::LayoutOutcome;
    assert_eq!(result.windows[0].layout, Some(LayoutOutcome::Applied));
    assert_eq!(result.windows[1].layout, Some(LayoutOutcome::Ignored));
    let (pruned, pruned_window) = created(&result, 0, cx);
    let captured = capture_layout(pruned_window, cx);
    // One pane remains with the document once; the unplaced entry follows in
    // the normal group, so its text is never hidden.
    assert_eq!(captured["root"]["kind"], "pane");
    assert_eq!(captured["root"]["dock"], serde_json::Value::Null);
    assert_eq!(captured["root"]["tabs"][0]["view"]["caret"], 2);
    assert_eq!(h.texts(&pruned, cx), ["one", "loose"]);
    assert_eq!(captured["dockSizes"]["left"], 200.);
    cx.read(|cx| {
        let shell = pruned.read(cx);
        let explorer = shell.explorer_frame(cx).unwrap();
        assert!(shell.workbench.read(cx).tool_tab(explorer).is_none());
    });
    // The ignored layout falls back to the entries-only arrangement.
    let (fallback, _) = created(&result, 1, cx);
    assert_eq!(h.texts(&fallback, cx), ["two"]);
}
