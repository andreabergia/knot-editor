//! Protected presentation closure through the public host.

use super::operations::{empty_product_window, install_operation_fixture};
use super::*;
use crate::app::extension_host;

const SOURCE: &str = r#"
import { commands, documents, windows, workbench } from "knot";
let created;
await commands.register("close.prepare", async () => {
  created = await documents.createUntitled();
  await workbench.writeClipboardText(created.document.id);
});
await commands.register("close.prepare-file", async () => {
  created = await documents.open("mem://product/notes.txt");
  await workbench.writeClipboardText(created.document.id);
});
await commands.register("close.edit", async () => {
  await created.document.buffer.applyEdits([{ range: { startByteOffset: 0, endByteOffset: 0 }, text: "unsaved" }], { ifRevision: 0 });
});
await commands.register("close.share", async () => {
  await documents.present(created.document.id);
});
await commands.register("close.run", async () => {
  try {
    const result = await workbench.close(created.presentation.target);
    if (!Object.isFrozen(result)) throw new Error("mutable close result");
    const current = await documents.get(created.document.id);
    let buffer = "live";
    try { await created.document.buffer.snapshot(); }
    catch (error) { if (error.code !== "BufferClosed") throw error; buffer = error.code; }
    await workbench.writeClipboardText(`${result.kind}:${current === null ? "gone" : "live"}:${buffer}`);
  } catch (error) {
    if (!["InvalidTarget", "PersistenceFailed", "Cancelled"].includes(error.code)) throw error;
    await workbench.writeClipboardText(error.code);
  }
});
await commands.register("close.concurrent", async () => {
  const first = workbench.close(created.presentation.target);
  try { await workbench.close(created.presentation.target); throw new Error("concurrent close accepted"); }
  catch (error) { if (error.code !== "OperationInProgress") throw error; }
  const result = await first;
  await workbench.writeClipboardText(`${result.kind}:OperationInProgress`);
});
await commands.register("close.invalid", async ({ target }) => {
  const win = (await windows.list())[0];
  for (const invalid of [target, win.target, win.panes[0].target, {}, { ...created.presentation.target }]) {
    try { await workbench.close(invalid); throw new Error("non-view target accepted"); }
    catch (error) { if (error.code !== "InvalidTarget") throw error; }
  }
});
"#;

fn wait_prompt(cx: &mut TestAppContext) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        cx.run_until_parked();
        if cx.has_pending_prompt() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "protected close did not prompt"
        );
        std::thread::yield_now();
    }
}

fn clipboard(cx: &TestAppContext) -> String {
    cx.read(|cx| cx.read_from_clipboard().unwrap().text().unwrap())
}

fn created_document(
    documents: &Entity<DocumentCollection>,
    cx: &TestAppContext,
) -> (super::super::DocumentId, Entity<BufferModel>) {
    let id = clipboard(cx);
    cx.read(|cx| {
        let document = documents
            .read(cx)
            .documents()
            .find(|document| document.identity().to_string() == id)
            .unwrap();
        (document.id(), document.model().clone())
    })
}

#[gpui::test]
fn public_close_preserves_cancel_and_invalidates_the_final_document_after_save_or_discard(
    cx: &mut TestAppContext,
) {
    for answer in ["Cancel", "Don't Save", "Save"] {
        let documents = install_globals(cx);
        let (filesystems, provider) = memory_filesystems_with_provider();
        cx.set_global(ApplicationFileSystems(filesystems));
        let destination = ResourceUri::parse("mem://product/closed.txt").unwrap();
        cx.set_global(ApplicationSaveDialog(Arc::new(FixedSaveDialog(
            SaveDialogOutcome::Selected(destination.clone()),
        ))));
        let host = cx.update(extension_host::install);
        let key = install_operation_fixture(&host, SOURCE, "close.invalid", cx);
        let (first, first_window) = empty_product_window(&documents, cx);
        let (control, control_window) = empty_product_window(&documents, cx);
        assert_eq!(
            wait_for_command(
                dispatch_product_command(&first, first_window, "close.prepare", cx),
                cx
            ),
            CommandOutcome::Completed
        );
        let (document, model) = created_document(&documents, cx);
        assert_eq!(
            wait_for_command(
                dispatch_product_command(&control, control_window, "close.invalid", cx),
                cx
            ),
            CommandOutcome::Completed
        );
        assert_eq!(
            wait_for_command(
                dispatch_product_command(&control, control_window, "close.edit", cx),
                cx
            ),
            CommandOutcome::Completed
        );
        let pending = dispatch_product_command(&control, control_window, "close.run", cx);
        wait_prompt(cx);
        assert!(cx.read(|cx| model.read(cx).is_open()));
        cx.simulate_prompt_answer(answer);
        assert_eq!(wait_for_command(pending, cx), CommandOutcome::Completed);
        assert_eq!(
            clipboard(cx),
            if answer == "Cancel" {
                "cancelled:live:live"
            } else {
                "closed:gone:BufferClosed"
            }
        );
        cx.read(|cx| {
            assert_eq!(
                documents.read(cx).get(document).is_some(),
                answer == "Cancel"
            );
            assert_eq!(model.read(cx).is_open(), answer == "Cancel");
            assert_eq!(
                first.read(cx).workbench.read(cx).view_count(document),
                usize::from(answer == "Cancel")
            );
        });
        if answer == "Save" {
            assert_eq!(
                pollster::block_on(provider.read(destination))
                    .unwrap()
                    .bytes,
                b"unsaved"
            );
        }
        host.update(cx, |host, cx| host.unload(key, cx).unwrap());
        for window in [first_window, control_window] {
            cx.update_window(window, |_, window, _| window.remove_window())
                .unwrap();
        }
    }
}

#[gpui::test]
fn public_close_keeps_a_shared_dirty_document_live_without_prompting(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let host = cx.update(extension_host::install);
    let key = install_operation_fixture(&host, SOURCE, "close.invalid", cx);
    let (first, first_window) = empty_product_window(&documents, cx);
    let (control, control_window) = empty_product_window(&documents, cx);
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&first, first_window, "close.prepare", cx),
            cx
        ),
        CommandOutcome::Completed
    );
    let (document, model) = created_document(&documents, cx);
    for command in ["close.edit", "close.share", "close.run"] {
        assert_eq!(
            wait_for_command(
                dispatch_product_command(&control, control_window, command, cx),
                cx
            ),
            CommandOutcome::Completed
        );
    }
    assert!(!cx.has_pending_prompt());
    assert_eq!(clipboard(cx), "closed:live:live");
    cx.read(|cx| {
        assert_eq!(first.read(cx).workbench.read(cx).view_count(document), 0);
        assert_eq!(control.read(cx).workbench.read(cx).view_count(document), 1);
        assert!(documents.read(cx).get(document).unwrap().is_dirty(cx));
        assert_eq!(model.read(cx).text(), "unsaved");
    });
    host.update(cx, |host, cx| host.unload(key, cx).unwrap());
}

#[gpui::test]
fn public_close_rejects_concurrent_closure_and_save_failure_without_removing_the_view(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    cx.set_global(ApplicationFileSystems(memory_filesystems()));
    cx.set_global(ApplicationSaveDialog(Arc::new(FixedSaveDialog(
        SaveDialogOutcome::Selected(ResourceUri::parse("mem://product/src").unwrap()),
    ))));
    let host = cx.update(extension_host::install);
    let key = install_operation_fixture(&host, SOURCE, "close.invalid", cx);
    let (first, first_window) = empty_product_window(&documents, cx);
    let (control, control_window) = empty_product_window(&documents, cx);
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&first, first_window, "close.prepare", cx),
            cx
        ),
        CommandOutcome::Completed
    );
    let (document, model) = created_document(&documents, cx);
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&control, control_window, "close.edit", cx),
            cx
        ),
        CommandOutcome::Completed
    );
    let pending = dispatch_product_command(&control, control_window, "close.concurrent", cx);
    wait_prompt(cx);
    cx.simulate_prompt_answer("Cancel");
    assert_eq!(wait_for_command(pending, cx), CommandOutcome::Completed);
    assert_eq!(clipboard(cx), "cancelled:OperationInProgress");
    let failed = dispatch_product_command(&control, control_window, "close.run", cx);
    wait_prompt(cx);
    cx.simulate_prompt_answer("Save");
    assert_eq!(wait_for_command(failed, cx), CommandOutcome::Completed);
    assert_eq!(clipboard(cx), "PersistenceFailed");
    assert!(cx.read(|cx| documents.read(cx).get(document).unwrap().is_dirty(cx)));
    assert_eq!(
        cx.read(|cx| first.read(cx).workbench.read(cx).view_count(document)),
        1
    );
    assert_eq!(cx.read(|cx| model.read(cx).text()), "unsaved");
    host.update(cx, |host, cx| host.unload(key, cx).unwrap());
}

#[gpui::test]
fn replacing_a_prompted_editor_cannot_close_its_replacement(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let host = cx.update(extension_host::install);
    let key = install_operation_fixture(&host, SOURCE, "close.invalid", cx);
    let (first, first_window) = empty_product_window(&documents, cx);
    let (control, control_window) = empty_product_window(&documents, cx);
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&first, first_window, "close.prepare", cx),
            cx
        ),
        CommandOutcome::Completed
    );
    let (document, model) = created_document(&documents, cx);
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&control, control_window, "close.edit", cx),
            cx
        ),
        CommandOutcome::Completed
    );
    let pending = dispatch_product_command(&control, control_window, "close.run", cx);
    wait_prompt(cx);
    let target = cx.read(|cx| current_open_target(&first, cx));
    first.update(cx, |shell, cx| {
        shell.workbench.update(cx, |workbench, cx| {
            assert!(workbench.replace_tab_document(
                target.pane,
                target.tab,
                document,
                model.clone(),
                cx
            ));
        })
    });
    cx.simulate_prompt_answer("Don't Save");
    assert_eq!(wait_for_command(pending, cx), CommandOutcome::Completed);
    assert_eq!(clipboard(cx), "InvalidTarget");
    assert_eq!(
        cx.read(|cx| first.read(cx).workbench.read(cx).view_count(document)),
        1
    );
    assert!(cx.read(|cx| model.read(cx).is_open()));
    host.update(cx, |host, cx| host.unload(key, cx).unwrap());
}

#[gpui::test]
fn public_close_cancellation_before_commit_keeps_the_presentation(cx: &mut TestAppContext) {
    for (unload, answer) in [(false, "Don't Save"), (true, "Save")] {
        let documents = install_globals(cx);
        let host = cx.update(extension_host::install);
        let key = install_operation_fixture(&host, SOURCE, "close.invalid", cx);
        let (first, first_window) = empty_product_window(&documents, cx);
        let (control, control_window) = empty_product_window(&documents, cx);
        assert_eq!(
            wait_for_command(
                dispatch_product_command(&first, first_window, "close.prepare", cx),
                cx
            ),
            CommandOutcome::Completed
        );
        let (document, model) = created_document(&documents, cx);
        assert_eq!(
            wait_for_command(
                dispatch_product_command(&control, control_window, "close.edit", cx),
                cx
            ),
            CommandOutcome::Completed
        );
        let pending = dispatch_product_command(&control, control_window, "close.run", cx);
        wait_prompt(cx);
        host.update(cx, |host, cx| {
            if unload {
                host.unload(key, cx).unwrap();
            } else {
                host.cancel_active_test_commands(cx);
            }
        });
        cx.simulate_prompt_answer(answer);
        assert_eq!(wait_for_command(pending, cx), CommandOutcome::Cancelled);
        wait_for_api_condition(cx, |cx| !ProductShell::protected_closure_is_active(cx));
        cx.read(|cx| {
            assert!(model.read(cx).is_open());
            assert!(documents.read(cx).get(document).unwrap().is_dirty(cx));
            assert_eq!(first.read(cx).workbench.read(cx).view_count(document), 1);
        });
        if !unload {
            host.update(cx, |host, cx| host.unload(key, cx).unwrap());
        }
        for window in [first_window, control_window] {
            cx.update_window(window, |_, window, _| window.remove_window())
                .unwrap();
        }
    }
}

#[gpui::test]
fn an_edit_racing_a_public_close_save_preserves_the_live_dirty_document(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (_, provider) = memory_filesystems_with_provider();
    let (release, gate) = tokio::sync::oneshot::channel();
    let gated = Arc::new(GatedFileSystemProvider {
        inner: provider.clone(),
        read_release: Mutex::new(None),
        replace_release: Mutex::new(Some(gate)),
    });
    let mut filesystems = FileSystemProviderRegistry::new();
    filesystems.register("mem", gated.clone()).unwrap();
    cx.set_global(ApplicationFileSystems(Arc::new(filesystems)));
    let host = cx.update(extension_host::install);
    let key = install_operation_fixture(&host, SOURCE, "close.invalid", cx);
    let (first, first_window) = empty_product_window(&documents, cx);
    let (control, control_window) = empty_product_window(&documents, cx);
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&first, first_window, "close.prepare-file", cx),
            cx
        ),
        CommandOutcome::Completed
    );
    let (document, model) = created_document(&documents, cx);
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&control, control_window, "close.edit", cx),
            cx
        ),
        CommandOutcome::Completed
    );
    let pending = dispatch_product_command(&control, control_window, "close.run", cx);
    wait_prompt(cx);
    cx.simulate_prompt_answer("Save");
    wait_for_api_condition(cx, |_| gated.replace_release.lock().unwrap().is_none());
    model.update(cx, |model, cx| {
        model.replace(0..0, "racing ").unwrap();
        cx.notify();
    });
    release.send(()).unwrap();
    assert_eq!(wait_for_command(pending, cx), CommandOutcome::Completed);
    assert_eq!(clipboard(cx), "PersistenceFailed");
    cx.read(|cx| {
        assert!(documents.read(cx).get(document).unwrap().is_dirty(cx));
        assert_eq!(model.read(cx).text(), "racing unsavedloaded text");
        assert_eq!(first.read(cx).workbench.read(cx).view_count(document), 1);
    });
    assert_eq!(
        pollster::block_on(provider.read(ResourceUri::parse("mem://product/notes.txt").unwrap()))
            .unwrap()
            .bytes,
        b"unsavedloaded text"
    );
    host.update(cx, |host, cx| host.unload(key, cx).unwrap());
}

#[gpui::test]
fn activation_can_close_the_final_tab_and_observe_its_replacement(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (shell, _) = empty_product_window(&documents, cx);
    let old_workbench = cx.read(|cx| shell.read(cx).workbench.clone());
    let host = cx.update(extension_host::install);
    let key = install_operation_fixture(
        &host,
        r#"
import { commands, documents, windows, workbench } from "knot";
const window = (await windows.list())[0], old = window.panes[0].tabs[0];
const closed = await workbench.close(old.target);
if (closed.kind !== "closed" || await documents.get(old.document) !== null) throw new Error("final tab not closed");
try { await old.buffer.snapshot(); throw new Error("retained buffer usable"); }
catch (error) { if (error.code !== "BufferClosed") throw error; }
const fresh = await windows.get(window.target);
if (fresh.panes.length !== 1 || fresh.panes[0].tabs.length !== 1 || fresh.panes[0].selected === old.target) throw new Error("missing replacement");
try { await workbench.close(old.target); throw new Error("closed view accepted"); }
catch (error) { if (error.code !== "InvalidTarget") throw error; }
await commands.register("close.activation-ready", () => {});
"#,
        "close.activation-ready",
        cx,
    );
    cx.read(|cx| {
        assert_ne!(old_workbench, shell.read(cx).workbench);
        assert_eq!(documents.read(cx).documents().count(), 1);
    });
    host.update(cx, |host, cx| host.unload(key, cx).unwrap());
}

#[gpui::test]
fn an_observation_listener_can_close_and_dispose_without_deadlocking(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let host = cx.update(extension_host::install);
    let key = install_operation_fixture(
        &host,
        r#"
import { commands, documents, windows, workbench } from "knot";
let id;
const observer = await workbench.observe(async event => {
  if (event.type !== "documentOpened" || event.document.id !== id) return;
  const tab = (await windows.list()).flatMap(win => win.panes.flatMap(pane => pane.tabs)).find(tab => tab.document === id);
  const result = await workbench.close(tab.target);
  if (result.kind !== "closed" || await documents.get(id) !== null) throw new Error("listener close");
  await observer.dispose();
  await commands.register("close.listener-ready", () => {});
});
await commands.register("close.listener-trigger", async () => { id = (await documents.createUntitled()).document.id; });
"#,
        "close.listener-trigger",
        cx,
    );
    let (shell, window) = empty_product_window(&documents, cx);
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&shell, window, "close.listener-trigger", cx),
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
            .resolve("close.listener-ready")
            .is_ok()
    });
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&shell, window, "close.listener-ready", cx),
            cx
        ),
        CommandOutcome::Completed
    );
    assert_eq!(cx.read(|cx| documents.read(cx).documents().count()), 1);
    host.update(cx, |host, cx| host.unload(key, cx).unwrap());
}

#[gpui::test]
fn a_transferred_presentation_cannot_be_closed_through_its_old_target(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let host = cx.update(extension_host::install);
    let key = install_operation_fixture(&host, SOURCE, "close.invalid", cx);
    let (first, first_window) = empty_product_window(&documents, cx);
    let (control, control_window) = empty_product_window(&documents, cx);
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&first, first_window, "close.prepare", cx),
            cx
        ),
        CommandOutcome::Completed
    );
    let (document, model) = created_document(&documents, cx);
    let source = cx.read(|cx| {
        let target = current_open_target(&first, cx);
        first
            .read(cx)
            .capture_tab_relocation(target.pane, target.tab, cx)
            .unwrap()
    });
    let destination = cx.read(|cx| {
        let target = current_open_target(&control, cx);
        let workbench = control.read(cx).workbench.clone();
        super::super::relocate::TabRelocationDestination {
            window: control_window,
            workbench: workbench.downgrade(),
            tab: crate::app::workbench::TabMoveDestination::At {
                anchor: workbench
                    .read(cx)
                    .capture_tab_move(target.pane, target.tab)
                    .unwrap(),
                placement: crate::app::workbench::TabMovePlacement::End,
            },
        }
    });
    assert_eq!(
        cx.update_window(first_window, |_, window, cx| first.update(
            cx,
            |shell, cx| shell.relocate_tab_to(&source, &destination, window, cx)
        ))
        .unwrap(),
        CommandOutcome::Completed
    );
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&first, first_window, "close.run", cx),
            cx
        ),
        CommandOutcome::Completed
    );
    assert_eq!(clipboard(cx), "InvalidTarget");
    cx.read(|cx| {
        assert!(model.read(cx).is_open());
        assert_eq!(first.read(cx).workbench.read(cx).view_count(document), 0);
        assert_eq!(control.read(cx).workbench.read(cx).view_count(document), 1);
    });
    host.update(cx, |host, cx| host.unload(key, cx).unwrap());
}

#[gpui::test]
fn public_close_removes_a_terminal_without_closing_document_models(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (shell, window) = empty_product_window(&documents, cx);
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&shell, window, NEW_TERMINAL_COMMAND, cx),
            cx
        ),
        CommandOutcome::Completed
    );
    let host = cx.update(extension_host::install);
    let key = install_operation_fixture(
        &host,
        r#"
import { commands, documents, windows, workbench } from "knot";
const before = await documents.list();
const win = (await windows.list())[0], terminal = win.panes[0].tabs.find(tab => tab.kind === "terminal");
if ((await workbench.close(terminal.target)).kind !== "closed") throw new Error("terminal close");
const after = await windows.get(win.target);
if (after.panes[0].tabs.some(tab => tab.target === terminal.target)) throw new Error("terminal retained");
if ((await documents.list()).length !== before.length || (await before[0].buffer.snapshot()).text !== "") throw new Error("document removed with terminal");
await commands.register("close.terminal-ready", () => {});
"#,
        "close.terminal-ready",
        cx,
    );
    cx.read(|cx| {
        assert!(
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions
                .is_empty()
        );
        assert_eq!(shell.read(cx).workbench.read(cx).panes()[0].tabs().len(), 1);
        assert_eq!(documents.read(cx).documents().count(), 1);
    });
    host.update(cx, |host, cx| host.unload(key, cx).unwrap());
}
