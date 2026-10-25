//! Semantic resource completion through the product owner.

use super::super::{OpenCommit, OpenFailure};
use super::*;
use crate::app::open::OpenResourceError;

fn begin_open(
    shell: &Entity<ProductShell>,
    uri: &str,
    cx: &mut TestAppContext,
) -> tokio::sync::oneshot::Receiver<Result<OpenCommit, OpenFailure>> {
    let target = cx.read(|cx| current_open_target(shell, cx));
    shell.update(cx, |shell, cx| {
        shell.start_open_operation(
            OpenRequest::from_uri_for_product(ResourceUri::parse(uri).unwrap()),
            target,
            false,
            cx,
        )
    })
}

#[gpui::test]
fn semantic_open_returns_the_actual_created_or_reused_presentation(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (filesystems, _) = memory_filesystems_with_provider();
    cx.set_global(ApplicationFileSystems(filesystems));
    let placeholder = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(placeholder).unwrap().model().clone());
    let (shell, _) = product_window(placeholder, model, cx);
    let original = cx.read(|cx| current_open_target(&shell, cx));
    let mut pending = begin_open(&shell, "mem://product/notes.txt", cx);
    cx.run_until_parked();
    let OpenCommit::Document { document, target } = pending.try_recv().unwrap().unwrap() else {
        panic!("file Open returned a workspace");
    };
    assert_ne!(document, placeholder);
    assert_ne!(target.tab, original.tab);
    cx.read(|cx| {
        assert!(target.matches_presentation(&shell.read(cx).workbench, cx));
        assert_eq!(target.surface, TabSurfaceId::Document(document));
        assert_eq!(
            documents
                .read(cx)
                .get(document)
                .unwrap()
                .model()
                .read(cx)
                .text(),
            "loaded text"
        );
    });
    // Admission from another tab must still return the reused file presentation.
    shell.update(cx, |shell, cx| {
        shell.workbench.update(cx, |workbench, _| {
            assert!(workbench.activate_tab(original.pane, original.tab));
        })
    });
    let mut reopened = begin_open(&shell, "mem://product/notes.txt", cx);
    cx.run_until_parked();
    let OpenCommit::Document {
        document: shared,
        target: selected,
    } = reopened.try_recv().unwrap().unwrap()
    else {
        panic!("file Open returned a workspace");
    };
    assert_eq!(document, shared);
    assert_eq!(target.pane, selected.pane);
    assert_eq!(target.tab, selected.tab);
    assert_eq!(target.presentation, selected.presentation);
    assert_eq!(cx.read(|cx| documents.read(cx).documents().count()), 2);
}

#[gpui::test]
fn semantic_open_returns_committed_roots_and_preserves_resource_errors(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (filesystems, _) = memory_filesystems_with_provider();
    cx.set_global(ApplicationFileSystems(filesystems));
    let placeholder = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(placeholder).unwrap().model().clone());
    let (shell, _) = product_window(placeholder, model, cx);
    let mut pending = begin_open(&shell, "mem://product/src", cx);
    cx.run_until_parked();
    let OpenCommit::Workspace(workspace) = pending.try_recv().unwrap().unwrap() else {
        panic!("folder Open returned a document");
    };
    assert_eq!(workspace.generation(), 1);
    assert_eq!(
        workspace.first_root().unwrap().to_string(),
        "mem://product/src"
    );
    assert!(cx.read(|cx| shell.read(cx).workspace.is_current(&workspace)));
    let mut invalid = begin_open(&shell, "mem://product/invalid.txt", cx);
    cx.run_until_parked();
    assert!(matches!(
        invalid.try_recv().unwrap(),
        Err(OpenFailure::Resource(OpenResourceError::InvalidUtf8 { .. }))
    ));
    let mut unsupported = begin_open(&shell, "other://product/notes.txt", cx);
    cx.run_until_parked();
    assert!(matches!(
        unsupported.try_recv().unwrap(),
        Err(OpenFailure::Resource(OpenResourceError::Provider(_)))
    ));
    assert_eq!(cx.read(|cx| documents.read(cx).documents().count()), 1);
    assert!(cx.read(|cx| shell.read(cx).workspace.is_current(&workspace)));
}

#[gpui::test]
async fn semantic_open_rejects_a_replaced_presentation_without_creating_a_document(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    let (_, provider) = memory_filesystems_with_provider();
    let (release, gate) = tokio::sync::oneshot::channel();
    let provider = Arc::new(GatedFileSystemProvider {
        inner: provider,
        read_release: Mutex::new(Some(gate)),
        replace_release: Mutex::new(None),
    });
    let mut filesystems = FileSystemProviderRegistry::new();
    filesystems.register("mem", provider.clone()).unwrap();
    cx.set_global(ApplicationFileSystems(Arc::new(filesystems)));
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, _) = product_window(document, model.clone(), cx);
    let pending = begin_open(&shell, "mem://product/notes.txt", cx);
    wait_for_api_condition(cx, |_| provider.read_release.lock().unwrap().is_none());
    let target = cx.read(|cx| current_open_target(&shell, cx));
    shell.update(cx, |shell, cx| {
        shell.workbench.update(cx, |workbench, cx| {
            assert!(workbench.replace_tab_document(target.pane, target.tab, document, model, cx));
        })
    });
    release.send(()).unwrap();
    assert!(matches!(
        pending.await.unwrap(),
        Err(OpenFailure::InvalidTarget)
    ));
    assert_eq!(cx.read(|cx| documents.read(cx).documents().count()), 1);
}

use crate::app::extension_host::{self, ProductExtensionHost};
use crate::host::lifecycle::ExtensionKey;

pub(super) fn install_operation_fixture(
    host: &Entity<ProductExtensionHost>,
    source: &str,
    ready: &str,
    cx: &mut TestAppContext,
) -> ExtensionKey {
    let key = ExtensionKey::new(ExtensionId::new(711), ExtensionLifecycleId::new(1));
    let activated = std::rc::Rc::new(std::cell::Cell::new(false));
    let finished = activated.clone();
    host.update(cx, |host, cx| {
        let execution = host.load_test_source(key, source).unwrap();
        cx.spawn(async move |_, _| {
            execution.await.unwrap();
            finished.set(true);
        })
        .detach();
    });
    wait_for_api_condition(cx, |cx| {
        activated.get()
            && cx
                .global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .catalog()
                .borrow()
                .resolve(ready)
                .is_ok()
    });
    key
}

pub(super) fn empty_product_window(
    documents: &Entity<DocumentCollection>,
    cx: &mut TestAppContext,
) -> (Entity<ProductShell>, gpui::AnyWindowHandle) {
    let document = cx.update(|cx| create_untitled_document(documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    product_window(document, model, cx)
}

#[gpui::test]
fn public_open_captures_destinations_and_returns_shared_document_metadata(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (filesystems, _) = memory_filesystems_with_provider();
    cx.set_global(ApplicationFileSystems(filesystems));
    let host = cx.update(extension_host::install);
    let key = install_operation_fixture(
        &host,
        r#"
import { commands, documents, windows } from "knot";
async function rejects(action, code) {
  try { await action(); } catch (error) { if (error.code === code) return; throw error; }
  throw new Error(`expected ${code}`);
}
await rejects(() => documents.open("mem://product/notes.txt"), "ContextUnavailable");
await commands.register("operations.open", async ({ target: command }) => {
  const wins = await windows.list();
  const pane = wins[1].panes[0].target;
  const first = await documents.open("mem://product/notes.txt", { target: pane, ignored: true });
  if (first.document.title !== "notes.txt" || first.document.dirty || first.document.revision !== 0) throw new Error("metadata");
  if (first.presentation.target.pane !== pane || first.presentation.document !== first.document.id) throw new Error("destination");
  if (first.document.buffer !== first.presentation.buffer || (await first.document.buffer.snapshot()).text !== "loaded text") throw new Error("buffer");
  const again = await documents.open("mem://product/notes.txt", { target: first.presentation.target });
  if (again.document.id !== first.document.id || again.presentation.target !== first.presentation.target) throw new Error("reuse");
  const shared = await documents.open("mem://product/notes.txt", { target: wins[0].target });
  if (shared.document.id !== first.document.id || shared.document.buffer !== first.document.buffer || shared.presentation.target === first.presentation.target) throw new Error("sharing");
  const destination = await documents.open("mem://product/new.txt", { target: wins[0].target });
  if (destination.document.kind !== "destination" || !destination.document.dirty || destination.document.revision !== 0) throw new Error("missing destination");
  const duplicate = await documents.open("mem://product/new.txt", { target: wins[1].target });
  if (destination.document.id !== duplicate.document.id) throw new Error("missing destination not deduplicated");
  for (const value of [first, first.document, first.presentation, first.presentation.target]) if (!Object.isFrozen(value)) throw new Error("mutable result");
  await rejects(() => documents.open("mem://product/src"), "ResourceWrongKind");
  await rejects(() => documents.open("mem://product/invalid.txt"), "InvalidUtf8");
  await rejects(() => documents.open("other://product/file"), "ProviderFailure");
  await rejects(() => documents.open("not a URI"), "InvalidUri");
  for (const target of [{}, command, { ...pane }]) await rejects(() => documents.open("mem://product/notes.txt", { target }), "InvalidTarget");
  try { await documents.open(42); throw new Error("nontext URI accepted"); } catch (error) { if (!(error instanceof TypeError)) throw error; }
  if ((await documents.list()).length !== 4) throw new Error("failed opens leaked documents");
});
"#,
        "operations.open",
        cx,
    );
    let (first, window) = empty_product_window(&documents, cx);
    let (_second, _) = empty_product_window(&documents, cx);
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&first, window, "operations.open", cx),
            cx
        ),
        CommandOutcome::Completed
    );
    host.update(cx, |host, cx| host.unload(key, cx).unwrap());
}

#[gpui::test]
fn public_open_keeps_the_admitted_window_when_focus_changes_during_read(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (_, inner) = memory_filesystems_with_provider();
    let (release, gate) = tokio::sync::oneshot::channel();
    let provider = Arc::new(GatedFileSystemProvider {
        inner,
        read_release: Mutex::new(Some(gate)),
        replace_release: Mutex::new(None),
    });
    let mut filesystems = FileSystemProviderRegistry::new();
    filesystems.register("mem", provider.clone()).unwrap();
    cx.set_global(ApplicationFileSystems(Arc::new(filesystems)));
    let host = cx.update(extension_host::install);
    let (first, first_window) = empty_product_window(&documents, cx);
    let (second, second_window) = empty_product_window(&documents, cx);
    let key = install_operation_fixture(
        &host,
        r#"
import { commands, documents, windows } from "knot";
await commands.register("operations.delayed", async () => {
  const result = await documents.open("mem://product/notes.txt");
  const wins = await windows.list();
  const owner = wins.find(win => win.panes.some(pane => pane.tabs.some(tab => tab.target === result.presentation.target)));
  if (!owner || result.document.title !== "notes.txt") throw new Error("missing committed presentation");
});
"#,
        "operations.delayed",
        cx,
    );
    let execution = dispatch_product_command(&first, first_window, "operations.delayed", cx);
    wait_for_api_condition(cx, |_| provider.read_release.lock().unwrap().is_none());
    cx.update_window(second_window, |_, window, cx| {
        window.activate_window();
        second.read(cx).focus_active_surface(window, cx);
    })
    .unwrap();
    release.send(()).unwrap();
    assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
    cx.read(|cx| {
        let opened = documents
            .read(cx)
            .document_for_resource(&ResourceUri::parse("mem://product/notes.txt").unwrap())
            .unwrap();
        assert_eq!(first.read(cx).workbench.read(cx).view_count(opened), 1);
        assert_eq!(second.read(cx).workbench.read(cx).view_count(opened), 0);
    });
    host.update(cx, |host, cx| host.unload(key, cx).unwrap());
}

#[gpui::test]
fn public_open_rejects_replaced_targets_and_unloaded_callers_after_io(cx: &mut TestAppContext) {
    for unload in [false, true] {
        let documents = install_globals(cx);
        let (_, inner) = memory_filesystems_with_provider();
        let (release, gate) = tokio::sync::oneshot::channel();
        let provider = Arc::new(GatedFileSystemProvider {
            inner,
            read_release: Mutex::new(Some(gate)),
            replace_release: Mutex::new(None),
        });
        let mut filesystems = FileSystemProviderRegistry::new();
        filesystems.register("mem", provider.clone()).unwrap();
        cx.set_global(ApplicationFileSystems(Arc::new(filesystems)));
        let host = cx.update(extension_host::install);
        let (shell, window) = empty_product_window(&documents, cx);
        let key = install_operation_fixture(
            &host,
            r#"
import { commands, documents, windows, workbench } from "knot";
await commands.register("operations.stale", async () => {
  const view = (await windows.list())[0].panes[0].selected;
  try { await documents.open("mem://product/notes.txt", { target: view }); }
  catch (error) { if (error.code === "InvalidTarget") { await workbench.writeClipboardText("stale target rejected"); return; } throw error; }
  throw new Error("stale target accepted");
});
"#,
            "operations.stale",
            cx,
        );
        let execution = dispatch_product_command(&shell, window, "operations.stale", cx);
        wait_for_api_condition(cx, |_| provider.read_release.lock().unwrap().is_none());
        if unload {
            host.update(cx, |host, cx| host.unload(key, cx).unwrap());
        } else {
            let target = cx.read(|cx| current_open_target(&shell, cx));
            let document = cx.read(|cx| {
                shell
                    .read(cx)
                    .workbench
                    .read(cx)
                    .focused_pane()
                    .unwrap()
                    .active_tab()
                    .document_id()
                    .unwrap()
            });
            let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
            shell.update(cx, |shell, cx| {
                shell.workbench.update(cx, |workbench, cx| {
                    assert!(workbench.replace_tab_document(
                        target.pane,
                        target.tab,
                        document,
                        model,
                        cx
                    ));
                })
            });
        }
        release.send(()).unwrap();
        assert_eq!(
            wait_for_command(execution, cx),
            if unload {
                CommandOutcome::Cancelled
            } else {
                CommandOutcome::InvalidTarget
            }
        );
        cx.run_until_parked();
        assert_eq!(cx.read(|cx| documents.read(cx).documents().count()), 1);
        if !unload {
            assert_eq!(
                cx.read(|cx| cx.read_from_clipboard().unwrap().text().unwrap()),
                "stale target rejected"
            );
            host.update(cx, |host, cx| host.unload(key, cx).unwrap());
        }
        // Finish this window before installing another application fixture.
        cx.update_window(window, |_, window, _| window.remove_window())
            .unwrap();
    }
}

#[gpui::test]
fn activation_and_listeners_can_open_with_explicit_targets(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (filesystems, _) = memory_filesystems_with_provider();
    cx.set_global(ApplicationFileSystems(filesystems));
    let host = cx.update(extension_host::install);
    let (shell, window) = empty_product_window(&documents, cx);
    let key = install_operation_fixture(
        &host,
        r#"
import { commands, documents, windows, workbench } from "knot";
const destination = (await windows.list())[0].panes[0].selected;
const initial = await documents.open("mem://product/notes.txt", { target: destination });
let listenerResult;
const observer = await workbench.observe(async event => {
  if (event.type === "documentOpened" && event.document.resourceUri === "mem://product/trigger.txt") {
    listenerResult = await documents.open("mem://product/from-listener.txt", { target: initial.presentation.target });
    await observer.dispose();
    await commands.register("operations.listener-ready", () => {
      if (listenerResult.document.kind !== "destination") throw new Error("listener result");
    });
  }
});
await commands.register("operations.trigger", async () => {
  await documents.open("mem://product/trigger.txt", { target: initial.presentation.target });
});
"#,
        "operations.trigger",
        cx,
    );
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&shell, window, "operations.trigger", cx),
            cx
        ),
        CommandOutcome::Completed
    );
    wait_for_api_condition(cx, |cx| {
        cx.global::<ApplicationProductCommands>()
            .0
            .read(cx)
            .catalog()
            .borrow()
            .resolve("operations.listener-ready")
            .is_ok()
    });
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&shell, window, "operations.listener-ready", cx),
            cx
        ),
        CommandOutcome::Completed
    );
    assert_eq!(cx.read(|cx| documents.read(cx).documents().count()), 4);
    host.update(cx, |host, cx| host.unload(key, cx).unwrap());
}

#[gpui::test]
fn installed_open_command_consumes_the_committed_result_in_its_originating_window(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    let (filesystems, _) = memory_filesystems_with_provider();
    cx.set_global(ApplicationFileSystems(filesystems));
    let root = tempfile::tempdir().unwrap();
    install_extension(
        root.path(),
        "@example/application-state",
        &[],
        &[(
            "main.js",
            include_str!("../../../../examples/extensions/@example/application-state/dist/main.js"),
        )],
    );
    let host = cx.update(extension_host::install);
    host.update(cx, |host, cx| {
        host.start_installed_extensions(root.path().to_path_buf(), cx)
    });
    assert_eq!(
        wait_for_extension_report(&host, cx).report.entries[0].result,
        crate::app::extension_load::LoadResult::Loaded
    );
    let (first, first_window) = empty_product_window(&documents, cx);
    let mut opened = begin_open(&first, "mem://product/notes.txt", cx);
    cx.run_until_parked();
    let OpenCommit::Document { document, .. } = opened.try_recv().unwrap().unwrap() else {
        panic!("file open returned roots");
    };
    let (second, second_window) = empty_product_window(&documents, cx);
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&second, second_window, "example.open-document", cx),
            cx
        ),
        CommandOutcome::Completed
    );
    let report: serde_json::Value = cx.read(|cx| {
        serde_json::from_str(&cx.read_from_clipboard().unwrap().text().unwrap()).unwrap()
    });
    assert_eq!(report["title"], "notes.txt");
    assert_eq!(report["resourceUri"], "mem://product/notes.txt");
    cx.read(|cx| {
        assert_eq!(
            report["document"],
            documents
                .read(cx)
                .get(document)
                .unwrap()
                .identity()
                .to_string()
        );
        assert_eq!(first.read(cx).workbench.read(cx).view_count(document), 1);
        assert_eq!(second.read(cx).workbench.read(cx).view_count(document), 1);
    });
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&first, first_window, "example.application-state", cx),
            cx
        ),
        CommandOutcome::Completed
    );
    for (command, generation, root_count) in
        [("example.open-folder", 1, 1), ("example.clear-roots", 2, 0)]
    {
        assert_eq!(
            wait_for_command(
                dispatch_product_command(&second, second_window, command, cx),
                cx
            ),
            CommandOutcome::Completed
        );
        let report: serde_json::Value = cx.read(|cx| {
            serde_json::from_str(&cx.read_from_clipboard().unwrap().text().unwrap()).unwrap()
        });
        assert_eq!(report["generation"], generation);
        assert_eq!(report["roots"].as_array().unwrap().len(), root_count);
        assert_eq!(
            cx.read(|cx| second.read(cx).workbench.read(cx).view_count(document)),
            1
        );
    }
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&second, second_window, "example.create-untitled", cx),
            cx
        ),
        CommandOutcome::Completed
    );
    let created: serde_json::Value = cx.read(|cx| {
        serde_json::from_str(&cx.read_from_clipboard().unwrap().text().unwrap()).unwrap()
    });
    assert_eq!(created["kind"], "untitled");
    assert_eq!(created["dirty"], false);
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&first, first_window, "example.close-created-document", cx),
            cx
        ),
        CommandOutcome::Completed
    );
    let closed: serde_json::Value = cx.read(|cx| {
        serde_json::from_str(&cx.read_from_clipboard().unwrap().text().unwrap()).unwrap()
    });
    assert_eq!(closed["kind"], "closed");
    assert!(!cx.read(|cx| {
        documents.read(cx).documents().any(|document| {
            document.identity().to_string() == created["document"].as_str().unwrap()
        })
    }));
}

#[gpui::test]
fn public_untitled_creation_and_present_share_native_tabs_and_dirty_state(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (filesystems, _) = memory_filesystems_with_provider();
    cx.set_global(ApplicationFileSystems(filesystems));
    let host = cx.update(extension_host::install);
    let key = install_operation_fixture(
        &host,
        r#"
import { commands, documents, windows, workbench } from "knot";
try { await documents.createUntitled(); throw new Error("missing window accepted"); }
catch (error) { if (error.code !== "ContextUnavailable") throw error; }
let created;
await commands.register("operations.create", async () => {
  const wins = await windows.list();
  const pane = wins[1].panes[0];
  const terminal = pane.tabs.find(tab => tab.kind === "terminal");
  created = await documents.createUntitled({ target: terminal.target });
  if (created.document.kind !== "untitled" || created.document.dirty || created.document.revision !== 0 || created.document.resourceUri !== null) throw new Error("untitled metadata");
  if ((await created.document.buffer.snapshot()).text !== "" || created.presentation.target.pane !== pane.target) throw new Error("untitled presentation");
  const changed = await windows.get(wins[1].target);
  if (!changed.panes[0].tabs.some(tab => tab.target === terminal.target)) throw new Error("terminal replaced");
  await created.document.buffer.applyEdits([{ range: { startByteOffset: 0, endByteOffset: 0 }, text: "edit" }], { ifRevision: 0 });
  if (!(await documents.get(created.document.id)).dirty) throw new Error("untitled edit clean");
  const shared = await documents.present(created.document.id, { target: wins[0].target });
  if (shared.document.id !== created.document.id || shared.document.buffer !== created.document.buffer || !shared.document.dirty) throw new Error("present metadata");
  const again = await documents.present(created.document.id, { target: shared.presentation.target });
  if (again.presentation.target !== shared.presentation.target) throw new Error("present did not reuse tab");
  try { await documents.present("closed-document", { target: pane.target }); throw new Error("missing document accepted"); }
  catch (error) { if (error.code !== "DocumentNotFound") throw error; }
  const fromTerminal = await documents.open("mem://product/notes.txt", { target: terminal.target });
  if (fromTerminal.presentation.target.pane !== pane.target || !(await windows.get(wins[1].target)).panes[0].tabs.some(tab => tab.target === terminal.target)) throw new Error("Open replaced terminal");
  if ((await documents.list()).length !== 4) throw new Error("headless document leaked");
  await workbench.writeClipboardText(created.document.id);
});
await commands.register("operations.clean", async () => {
  const current = await documents.get(created.document.id);
  if (current.dirty || current.revision !== 2 || (await current.buffer.snapshot()).text !== "") throw new Error("undo did not restore clean state");
});
"#,
        "operations.clean",
        cx,
    );
    let (first, window) = empty_product_window(&documents, cx);
    let (second, second_window) = empty_product_window(&documents, cx);
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&second, second_window, NEW_TERMINAL_COMMAND, cx),
            cx
        ),
        CommandOutcome::Completed
    );
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&first, window, "operations.create", cx),
            cx
        ),
        CommandOutcome::Completed
    );
    let identity = cx.read(|cx| cx.read_from_clipboard().unwrap().text().unwrap());
    let (document, model) = cx.read(|cx| {
        let document = documents
            .read(cx)
            .documents()
            .find(|document| document.identity().to_string() == identity)
            .unwrap();
        assert_eq!(
            first.read(cx).workbench.read(cx).view_count(document.id()),
            1
        );
        assert_eq!(
            second.read(cx).workbench.read(cx).view_count(document.id()),
            1
        );
        (document.id(), document.model().clone())
    });
    model.update(cx, |model, cx| {
        model.undo().unwrap();
        cx.notify();
    });
    assert!(!cx.read(|cx| documents.read(cx).get(document).unwrap().is_dirty(cx)));
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&first, window, "operations.clean", cx),
            cx
        ),
        CommandOutcome::Completed
    );
    host.update(cx, |host, cx| host.unload(key, cx).unwrap());
}

#[gpui::test]
fn public_open_rejects_pane_or_window_loss_and_command_cancellation_without_leaks(
    cx: &mut TestAppContext,
) {
    for scenario in ["pane", "window", "cancel"] {
        let documents = install_globals(cx);
        let (_, inner) = memory_filesystems_with_provider();
        let (release, gate) = tokio::sync::oneshot::channel();
        let provider = Arc::new(GatedFileSystemProvider {
            inner,
            read_release: Mutex::new(Some(gate)),
            replace_release: Mutex::new(None),
        });
        let mut filesystems = FileSystemProviderRegistry::new();
        filesystems.register("mem", provider.clone()).unwrap();
        cx.set_global(ApplicationFileSystems(Arc::new(filesystems)));
        let host = cx.update(extension_host::install);
        let (first, first_window) = empty_product_window(&documents, cx);
        assert_eq!(
            wait_for_command(
                dispatch_product_command(&first, first_window, SPLIT_HORIZONTAL_COMMAND, cx),
                cx
            ),
            CommandOutcome::Completed
        );
        let (control, control_window) = empty_product_window(&documents, cx);
        let index = usize::from(
            first_window.window_id().as_u64().to_string()
                > control_window.window_id().as_u64().to_string(),
        );
        let source = r#"
import { commands, documents, windows, workbench } from "knot";
await commands.register("operations.target-loss", async () => {
  const target = (await windows.list())[DESTINATION].panes[0].selected;
  try { await documents.open("mem://product/notes.txt", { target }); throw new Error("lost target accepted"); }
  catch (error) {
    if (error.code === "Cancelled") return;
    if (error.code !== "InvalidTarget") throw error;
    await workbench.writeClipboardText(error.code);
  }
});
"#.replace("DESTINATION", &index.to_string());
        let key = install_operation_fixture(&host, &source, "operations.target-loss", cx);
        let pending =
            dispatch_product_command(&control, control_window, "operations.target-loss", cx);
        wait_for_api_condition(cx, |_| provider.read_release.lock().unwrap().is_none());
        match scenario {
            "pane" => {
                cx.update_window(first_window, |_, window, cx| {
                    first.update(cx, |shell, cx| {
                        let pane = &shell.workbench.read(cx).panes()[0];
                        let (pane, tab) = (pane.id(), pane.active_tab_id());
                        shell.activate_tab(pane, tab, window, cx);
                    })
                })
                .unwrap();
                assert_eq!(
                    wait_for_command(
                        dispatch_product_command(&first, first_window, CLOSE_TAB_COMMAND, cx),
                        cx
                    ),
                    CommandOutcome::Completed
                );
            }
            "window" => assert_eq!(
                wait_for_command(
                    dispatch_product_command(&first, first_window, CLOSE_WINDOW_COMMAND, cx),
                    cx
                ),
                CommandOutcome::Completed
            ),
            "cancel" => host.update(cx, |host, cx| host.cancel_active_test_commands(cx)),
            _ => unreachable!(),
        }
        release.send(()).unwrap();
        assert_eq!(
            wait_for_command(pending, cx),
            if scenario == "cancel" {
                CommandOutcome::Cancelled
            } else {
                CommandOutcome::Completed
            },
            "{scenario}"
        );
        cx.run_until_parked();
        cx.read(|cx| {
            assert!(
                documents
                    .read(cx)
                    .document_for_resource(&ResourceUri::parse("mem://product/notes.txt").unwrap())
                    .is_none()
            );
            assert_eq!(
                documents.read(cx).documents().count(),
                if scenario == "window" { 1 } else { 2 }
            );
            if scenario == "pane" {
                assert_eq!(first.read(cx).workbench.read(cx).panes().len(), 1);
            }
        });
        if scenario != "cancel" {
            assert_eq!(
                cx.read(|cx| cx.read_from_clipboard().unwrap().text().unwrap()),
                "InvalidTarget"
            );
        }
        host.update(cx, |host, cx| host.unload(key, cx).unwrap());
        if scenario != "window" {
            cx.update_window(first_window, |_, window, _| window.remove_window())
                .unwrap();
        }
        cx.update_window(control_window, |_, window, _| window.remove_window())
            .unwrap();
    }
}

const FOLDER_PICKER_MAIN: &str = r#"
import { commands, workbench, workspaces } from "knot";
await commands.register("picker.pick", async () => {
  try {
    const result = await workspaces.pickFolder();
    await workbench.writeClipboardText(JSON.stringify(result));
  } catch (error) {
    await workbench.writeClipboardText(JSON.stringify({ error: error.code }));
  }
});
"#;

fn clipboard_json(cx: &TestAppContext) -> serde_json::Value {
    cx.read(|cx| serde_json::from_str(&cx.read_from_clipboard().unwrap().text().unwrap()).unwrap())
}

#[gpui::test]
fn pick_folder_returns_a_normalized_directory_and_reports_path_failures(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let root = tempfile::tempdir().unwrap();
    install_extension(
        root.path(),
        "@example/picker",
        &[],
        &[("main.js", FOLDER_PICKER_MAIN)],
    );
    let host = cx.update(extension_host::install);
    host.update(cx, |host, cx| {
        host.start_installed_extensions(root.path().to_path_buf(), cx)
    });
    assert_eq!(
        wait_for_extension_report(&host, cx).report.entries[0].result,
        crate::app::extension_load::LoadResult::Loaded
    );
    let (shell, window) = empty_product_window(&documents, cx);

    let directory = tempfile::tempdir().unwrap();
    cx.set_global(ApplicationOpenDialog(Arc::new(FixedOpenDialog(
        OpenDialogOutcome::Selected(directory.path().to_path_buf()),
    ))));
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&shell, window, "picker.pick", cx),
            cx
        ),
        CommandOutcome::Completed
    );
    let result = clipboard_json(cx);
    assert_eq!(result["kind"], "selected");
    assert_eq!(
        result["uri"],
        url::Url::from_file_path(directory.path())
            .unwrap()
            .to_string()
    );

    let file = directory.path().join("notes.txt");
    std::fs::write(&file, "text").unwrap();
    cx.set_global(ApplicationOpenDialog(Arc::new(FixedOpenDialog(
        OpenDialogOutcome::Selected(file),
    ))));
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&shell, window, "picker.pick", cx),
            cx
        ),
        CommandOutcome::Completed
    );
    assert_eq!(clipboard_json(cx)["error"], "ResourceWrongKind");

    cx.set_global(ApplicationOpenDialog(Arc::new(FixedOpenDialog(
        OpenDialogOutcome::Selected(directory.path().join("missing")),
    ))));
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&shell, window, "picker.pick", cx),
            cx
        ),
        CommandOutcome::Completed
    );
    assert_eq!(clipboard_json(cx)["error"], "ResourceNotFound");
}
