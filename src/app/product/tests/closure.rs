//! Protected closure of tabs, windows, and quit: native prompts and the public host.

use super::operations::{empty_product_window, install_operation_fixture};
use super::*;
use crate::app::extensions;

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
        let host = cx.update(extensions::host::install);
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
    let host = cx.update(extensions::host::install);
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
    let host = cx.update(extensions::host::install);
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
    let host = cx.update(extensions::host::install);
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
        let host = cx.update(extensions::host::install);
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
    let host = cx.update(extensions::host::install);
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
    let host = cx.update(extensions::host::install);
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
    let host = cx.update(extensions::host::install);
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
    let host = cx.update(extensions::host::install);
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
    let host = cx.update(extensions::host::install);
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

#[gpui::test]
async fn dirty_final_tab_close_cancel_preserves_the_document_and_view(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    model.update(cx, |model, _| model.replace(0..0, "unsaved").unwrap());
    let (shell, window) = product_window(document, model, cx);

    let execution = dispatch_product_command(&shell, window, CLOSE_TAB_COMMAND, cx);
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Cancel");

    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Cancelled
    );
    cx.read(|cx| {
        assert!(documents.read(cx).get(document).is_some());
        assert_eq!(shell.read(cx).workbench.read(cx).view_count(document), 1);
    });
}

#[gpui::test]
async fn discarding_a_dirty_final_tab_closes_it_and_installs_a_replacement(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    model.update(cx, |model, _| model.replace(0..0, "unsaved").unwrap());
    let (shell, window) = product_window(document, model, cx);

    let execution = dispatch_product_command(&shell, window, CLOSE_TAB_COMMAND, cx);
    cx.run_until_parked();
    cx.simulate_prompt_answer("Don't Save");

    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert!(documents.read(cx).get(document).is_none());
        let replacement = shell
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab()
            .document_id()
            .unwrap();
        assert_ne!(replacement, document);
        assert!(documents.read(cx).get(replacement).is_some());
    });
}

#[gpui::test]
fn middle_clicking_an_inactive_dirty_tab_closes_it_after_its_save_decision(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    let (first, second, shell, window) = two_dirty_untitled_documents_in_one_window(&documents, cx);
    let first_tab = cx.read(|cx| {
        let workbench = shell.read(cx).workbench.read(cx);
        let pane = workbench.focused_pane().unwrap();
        assert_eq!(pane.active_tab().document_id(), Some(second));
        pane.tabs()
            .iter()
            .find(|tab| tab.document_id() == Some(first))
            .unwrap()
            .id()
    });
    let mut visual = VisualTestContext::from_window(window, cx);
    visual.run_until_parked();
    let tab = visual
        .debug_bounds(format!("tab-{}", first_tab.value()).leak())
        .unwrap()
        .center();
    visual.simulate_mouse_down(tab, MouseButton::Middle, Modifiers::default());
    visual.simulate_mouse_up(tab, MouseButton::Middle, Modifiers::default());
    wait_prompt(&mut visual);
    visual.simulate_prompt_answer("Don't Save");
    visual.run_until_parked();

    visual.read(|cx| {
        assert!(documents.read(cx).get(first).is_none());
        let workbench = shell.read(cx).workbench.read(cx);
        let pane = workbench.focused_pane().unwrap();
        assert_eq!(pane.tabs().len(), 1);
        assert_eq!(pane.active_tab().document_id(), Some(second));
        assert!(!*cx.global::<ApplicationProtectedClosure>().0.borrow());
    });
}

#[gpui::test]
async fn saving_a_dirty_tab_uses_existing_persistence_before_closing(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (filesystems, provider) = memory_filesystems_with_provider();
    cx.set_global(ApplicationFileSystems(filesystems));
    let uri = ResourceUri::parse("mem://product/notes.txt").unwrap();
    let opened = provider.read(uri.clone()).await.unwrap();
    let model = cx.new(|_| BufferModel::from_text("loaded text"));
    let document = documents.update(cx, |documents, cx| {
        documents.create_persisted(
            "notes.txt",
            model.clone(),
            uri.clone(),
            0,
            opened.version,
            cx,
        )
    });
    model.update(cx, |model, _| model.replace(0..0, "changed ").unwrap());
    let (shell, window) = product_window(document, model, cx);

    let execution = dispatch_product_command(&shell, window, CLOSE_TAB_COMMAND, cx);
    cx.run_until_parked();
    cx.simulate_prompt_answer("Save");

    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    assert_eq!(
        provider.read(uri).await.unwrap().bytes,
        b"changed loaded text"
    );
    cx.read(|cx| assert!(documents.read(cx).get(document).is_none()));
}

#[gpui::test]
async fn edit_racing_protected_close_save_preserves_the_document_and_every_view(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    let root = ResourceUri::parse("mem://product/").unwrap();
    let provider = Arc::new(MemoryFileSystemProvider::new(root).unwrap());
    let uri = ResourceUri::parse("mem://product/notes.txt").unwrap();
    provider.seed_file(uri.clone(), b"loaded text").unwrap();
    let opened = provider.read(uri.clone()).await.unwrap();
    let (replace_release, gated_replace) = tokio::sync::oneshot::channel();
    let gated_provider = Arc::new(GatedFileSystemProvider {
        inner: provider.clone(),
        read_release: Mutex::new(None),
        replace_release: Mutex::new(Some(gated_replace)),
    });
    let mut filesystems = FileSystemProviderRegistry::new();
    filesystems.register("mem", gated_provider).unwrap();
    cx.set_global(ApplicationFileSystems(Arc::new(filesystems)));

    let model = cx.new(|_| BufferModel::from_text("loaded text"));
    let document = documents.update(cx, |documents, cx| {
        documents.create_persisted(
            "notes.txt",
            model.clone(),
            uri.clone(),
            0,
            opened.version,
            cx,
        )
    });
    model.update(cx, |model, _| model.replace(0..0, "captured ").unwrap());
    let (shell, window) = product_window(document, model.clone(), cx);
    cx.update_window(window, |_, window, cx| {
        shell.update(cx, |shell, cx| {
            shell.split(SplitDirection::Horizontal, window, cx)
        });
    })
    .unwrap();

    let execution = dispatch_product_command(&shell, window, CLOSE_WINDOW_COMMAND, cx);
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Save");
    cx.run_until_parked();

    model.update(cx, |model, _| model.replace(0..0, "newer ").unwrap());
    replace_release.send(()).unwrap();

    assert!(matches!(
        execution.completion.await.unwrap(),
        CommandOutcome::HandlerFailure { .. }
    ));
    assert_eq!(
        provider.read(uri.clone()).await.unwrap().bytes,
        b"captured loaded text"
    );
    cx.read(|cx| {
        assert!(cx.windows().contains(&window));
        assert_eq!(model.read(cx).text(), "newer captured loaded text");
        assert!(documents.read(cx).get(document).unwrap().is_dirty(cx));
        assert_eq!(shell.read(cx).workbench.read(cx).view_count(document), 2);
    });
}

#[gpui::test]
async fn closing_one_of_multiple_views_of_a_dirty_document_does_not_prompt(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    model.update(cx, |model, _| model.replace(0..0, "unsaved").unwrap());
    let (shell, window) = product_window(document, model, cx);
    cx.update_window(window, |_, window, cx| {
        shell.update(cx, |shell, cx| {
            shell.split(SplitDirection::Horizontal, window, cx)
        });
    })
    .unwrap();

    let execution = dispatch_product_command(&shell, window, CLOSE_TAB_COMMAND, cx);
    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    assert!(!cx.has_pending_prompt());
    cx.read(|cx| {
        assert!(documents.read(cx).get(document).is_some());
        assert_eq!(shell.read(cx).workbench.read(cx).view_count(document), 1);
    });
}

#[gpui::test]
async fn window_close_prompts_once_for_a_multiply_viewed_dirty_document(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    model.update(cx, |model, _| model.replace(0..0, "unsaved").unwrap());
    let (shell, window) = product_window(document, model, cx);
    cx.update_window(window, |_, window, cx| {
        shell.update(cx, |shell, cx| {
            shell.split(SplitDirection::Horizontal, window, cx)
        });
    })
    .unwrap();

    let execution = dispatch_product_command(&shell, window, CLOSE_WINDOW_COMMAND, cx);
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Don't Save");

    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    assert!(!cx.has_pending_prompt());
    cx.read(|cx| {
        assert!(!cx.windows().contains(&window));
        assert!(documents.read(cx).get(document).is_none());
    });
}

#[gpui::test]
async fn window_close_keeps_a_dirty_document_visible_in_another_window_without_prompting(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    model.update(cx, |model, _| model.replace(0..0, "unsaved").unwrap());
    let (first_shell, first_window) = product_window(document, model.clone(), cx);
    let (_second_shell, second_window) = product_window(document, model, cx);

    let execution = dispatch_product_command(&first_shell, first_window, CLOSE_WINDOW_COMMAND, cx);
    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    assert!(!cx.has_pending_prompt());
    cx.read(|cx| {
        assert!(!cx.windows().contains(&first_window));
        assert!(cx.windows().contains(&second_window));
        assert!(documents.read(cx).get(document).unwrap().is_dirty(cx));
    });
}

#[gpui::test]
async fn quit_prompts_once_for_a_document_shared_across_windows(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    install_failing_recovery(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    model.update(cx, |model, _| model.replace(0..0, "unsaved").unwrap());
    let (first_shell, first_window) = product_window(document, model.clone(), cx);
    let _second = product_window(document, model, cx);

    let execution = dispatch_product_command(&first_shell, first_window, QUIT_COMMAND, cx);
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Don't Save");

    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    assert!(!cx.has_pending_prompt());
}

#[gpui::test]
async fn multi_document_quit_applies_serial_save_and_discard_decisions(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    install_failing_recovery(cx);
    let (filesystems, provider) = memory_filesystems_with_provider();
    cx.set_global(ApplicationFileSystems(filesystems));
    let uri = ResourceUri::parse("mem://product/notes.txt").unwrap();
    let opened = provider.read(uri.clone()).await.unwrap();
    let first_model = cx.new(|_| BufferModel::from_text("loaded text"));
    let first = documents.update(cx, |documents, cx| {
        documents.create_persisted(
            "notes.txt",
            first_model.clone(),
            uri.clone(),
            0,
            opened.version,
            cx,
        )
    });
    first_model.update(cx, |model, _| model.replace(0..0, "saved ").unwrap());
    let second_model = cx.new(|_| BufferModel::from_text("second"));
    let second = documents.update(cx, |documents, cx| {
        documents.create_untitled("Second", second_model.clone(), cx)
    });
    second_model.update(cx, |model, _| model.replace(0..0, "discarded ").unwrap());
    let (shell, window) = product_window(first, first_model, cx);
    shell.update(cx, |shell, cx| {
        let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
        shell.workbench.update(cx, |workbench, cx| {
            workbench
                .open_tab_for_document(pane, second, second_model, cx)
                .unwrap();
        });
    });

    let execution = dispatch_product_command(&shell, window, QUIT_COMMAND, cx);
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Save");
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Don't Save");

    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    assert_eq!(
        provider.read(uri).await.unwrap().bytes,
        b"saved loaded text"
    );
    assert!(!cx.has_pending_prompt());
}

#[gpui::test]
async fn later_quit_cancellation_preserves_every_document_and_view(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    install_failing_recovery(cx);
    let (first, second, shell, window) = two_dirty_untitled_documents_in_one_window(&documents, cx);

    let execution = dispatch_product_command(&shell, window, QUIT_COMMAND, cx);
    cx.run_until_parked();
    cx.simulate_prompt_answer("Don't Save");
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Cancel");

    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Cancelled
    );
    cx.read(|cx| {
        assert!(cx.windows().contains(&window));
        assert_eq!(shell.read(cx).workbench.read(cx).tab_snapshots().len(), 2);
        assert!(documents.read(cx).get(first).unwrap().is_dirty(cx));
        assert!(documents.read(cx).get(second).unwrap().is_dirty(cx));
    });
}

#[gpui::test]
async fn later_quit_save_failure_preserves_every_document_and_view(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    install_failing_recovery(cx);
    cx.set_global(ApplicationFileSystems(memory_filesystems()));
    cx.set_global(ApplicationSaveDialog(Arc::new(FixedSaveDialog(
        SaveDialogOutcome::Selected(ResourceUri::parse("mem://product/src").unwrap()),
    ))));
    let (first, second, shell, window) = two_dirty_untitled_documents_in_one_window(&documents, cx);

    let execution = dispatch_product_command(&shell, window, QUIT_COMMAND, cx);
    cx.run_until_parked();
    cx.simulate_prompt_answer("Don't Save");
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Save");

    assert!(matches!(
        execution.completion.await.unwrap(),
        CommandOutcome::HandlerFailure { .. }
    ));
    cx.read(|cx| {
        assert!(cx.windows().contains(&window));
        assert_eq!(shell.read(cx).workbench.read(cx).tab_snapshots().len(), 2);
        assert!(documents.read(cx).get(first).unwrap().is_dirty(cx));
        assert!(documents.read(cx).get(second).unwrap().is_dirty(cx));
    });
}

#[gpui::test]
async fn later_window_close_cancellation_preserves_every_tab(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let first = cx.update(|cx| create_untitled_document(&documents, cx));
    let first_model = cx.read(|cx| documents.read(cx).get(first).unwrap().model().clone());
    first_model.update(cx, |model, _| model.replace(0..0, "first").unwrap());
    let second_model = cx.new(|_| BufferModel::from_text("second"));
    let second = cx.update(|cx| {
        documents.update(cx, |documents, cx| {
            documents.create_untitled("Second", second_model.clone(), cx)
        })
    });
    second_model.update(cx, |model, _| model.replace(0..0, "changed ").unwrap());
    let (shell, window) = product_window(first, first_model, cx);
    shell.update(cx, |shell, cx| {
        let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
        shell.workbench.update(cx, |workbench, cx| {
            workbench
                .open_tab_for_document(pane, second, second_model, cx)
                .unwrap();
        });
    });

    let execution = dispatch_product_command(&shell, window, CLOSE_WINDOW_COMMAND, cx);
    cx.run_until_parked();
    cx.simulate_prompt_answer("Don't Save");
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Cancel");

    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Cancelled
    );
    cx.read(|cx| {
        assert!(cx.windows().contains(&window));
        assert_eq!(shell.read(cx).workbench.read(cx).tab_snapshots().len(), 2);
        assert!(documents.read(cx).get(first).is_some());
        assert!(documents.read(cx).get(second).is_some());
    });
}

#[gpui::test]
async fn close_save_failure_preserves_the_dirty_tab(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.set_global(ApplicationFileSystems(memory_filesystems()));
    cx.set_global(ApplicationSaveDialog(Arc::new(FixedSaveDialog(
        SaveDialogOutcome::Selected(ResourceUri::parse("mem://product/src").unwrap()),
    ))));
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    model.update(cx, |model, _| model.replace(0..0, "unsaved").unwrap());
    let (shell, window) = product_window(document, model, cx);

    let execution = dispatch_product_command(&shell, window, CLOSE_TAB_COMMAND, cx);
    cx.run_until_parked();
    cx.simulate_prompt_answer("Save");

    assert!(matches!(
        execution.completion.await.unwrap(),
        CommandOutcome::HandlerFailure { .. }
    ));
    cx.read(|cx| {
        assert!(documents.read(cx).get(document).unwrap().is_dirty(cx));
        assert_eq!(shell.read(cx).workbench.read(cx).view_count(document), 1);
    });
}

#[gpui::test]
async fn edit_while_discard_prompt_is_open_rejects_the_close(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    model.update(cx, |model, _| model.replace(0..0, "first edit").unwrap());
    let (shell, window) = product_window(document, model.clone(), cx);

    let execution = dispatch_product_command(&shell, window, CLOSE_TAB_COMMAND, cx);
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    model.update(cx, |model, _| model.replace(0..0, "racing edit ").unwrap());
    cx.simulate_prompt_answer("Don't Save");

    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::InvalidTarget
    );
    cx.read(|cx| {
        assert!(documents.read(cx).get(document).unwrap().is_dirty(cx));
        assert_eq!(shell.read(cx).workbench.read(cx).view_count(document), 1);
    });
}

#[gpui::test]
async fn scope_change_while_prompting_rejects_window_close(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    model.update(cx, |model, _| model.replace(0..0, "unsaved").unwrap());
    let (shell, window) = product_window(document, model, cx);
    let execution = dispatch_product_command(&shell, window, CLOSE_WINDOW_COMMAND, cx);
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());

    let added = cx.update(|cx| create_untitled_document(&documents, cx));
    let added_model = cx.read(|cx| documents.read(cx).get(added).unwrap().model().clone());
    shell.update(cx, |shell, cx| {
        let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
        shell.workbench.update(cx, |workbench, cx| {
            workbench
                .open_tab_for_document(pane, added, added_model, cx)
                .unwrap();
        });
    });
    cx.simulate_prompt_answer("Don't Save");

    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::InvalidTarget
    );
    cx.read(|cx| {
        assert!(cx.windows().contains(&window));
        assert_eq!(shell.read(cx).workbench.read(cx).tab_snapshots().len(), 2);
        assert!(documents.read(cx).get(document).is_some());
        assert!(documents.read(cx).get(added).is_some());
    });
}

#[gpui::test]
async fn concurrent_close_is_rejected_without_a_second_prompt(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    model.update(cx, |model, _| model.replace(0..0, "unsaved").unwrap());
    let (shell, window) = product_window(document, model, cx);
    let target = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.focus_active_editor(window, cx);
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let dispatcher = cx.read(|cx| cx.global::<ApplicationProductCommands>().0.clone());
    let first = dispatcher.update(cx, |dispatcher, cx| {
        dispatcher.dispatch(
            Command {
                name: CLOSE_TAB_COMMAND.into(),
                arguments: CommandArgumentValue::Null,
            },
            target.clone(),
            cx,
        )
    });
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    let second = dispatcher.update(cx, |dispatcher, cx| {
        dispatcher.dispatch(
            Command {
                name: CLOSE_TAB_COMMAND.into(),
                arguments: CommandArgumentValue::Null,
            },
            target,
            cx,
        )
    });

    assert_eq!(
        second.completion.await.unwrap(),
        CommandOutcome::Unavailable
    );
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Cancel");
    assert_eq!(first.completion.await.unwrap(), CommandOutcome::Cancelled);
    cx.read(|cx| assert_eq!(shell.read(cx).workbench.read(cx).view_count(document), 1));
}

#[gpui::test]
async fn native_window_close_is_vetoed_until_protected_closure_finishes(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    model.update(cx, |model, _| model.replace(0..0, "unsaved").unwrap());
    let (shell, window_cx) = cx.add_window_view(|window, cx| {
        let workbench = cx.new(|cx| Workbench::new_for_document(document, model, cx));
        cx.global::<ApplicationWorkbenches>()
            .register(&workbench, window.window_handle());
        ProductShell::new(workbench)
    });
    window_cx.update(|window, cx| {
        install_protected_window_close(&shell, window, cx);
        shell.read(cx).focus_active_editor(window, cx);
    });

    assert!(!window_cx.simulate_close());
    window_cx.run_until_parked();
    assert!(window_cx.has_pending_prompt());
    window_cx.simulate_prompt_answer("Don't Save");
    window_cx.run_until_parked();

    window_cx.read(|cx| {
        assert!(cx.windows().is_empty());
        assert!(documents.read(cx).get(document).is_none());
    });
}

/// One pane holding a clean pinned document, a dirty document twice, a second
/// dirty document, and the clean focused document, in that order.
fn bulk_close_fixture(
    documents: &Entity<DocumentCollection>,
    cx: &mut TestAppContext,
) -> (
    Entity<ProductShell>,
    gpui::AnyWindowHandle,
    [super::super::DocumentId; 4],
) {
    let created = ["shared", "other", "pinned", "focused"].map(|text| {
        let model = cx.new(|_| BufferModel::from_text(text));
        let document = documents.update(cx, |documents, cx| {
            documents.create_untitled(text, model.clone(), cx)
        });
        (document, model)
    });
    for (_, model) in &created[..2] {
        model.update(cx, |model, _| model.replace(0..0, "unsaved ").unwrap());
    }
    let (shell, window) = product_window(created[0].0, created[0].1.clone(), cx);
    shell.update(cx, |shell, cx| {
        shell.workbench.update(cx, |workbench, cx| {
            let pane = workbench.focused_pane_id().unwrap();
            for (document, model) in [&created[0], &created[1], &created[2], &created[3]] {
                workbench
                    .open_tab_for_document(pane, *document, model.clone(), cx)
                    .unwrap();
            }
            let pinned = workbench.focused_pane().unwrap().tabs()[3].id();
            workbench.toggle_pin(pane, pinned).unwrap();
        });
    });
    cx.run_until_parked();
    (shell, window, created.map(|(document, _)| document))
}

fn tab_documents(
    shell: &Entity<ProductShell>,
    cx: &TestAppContext,
) -> Vec<super::super::DocumentId> {
    cx.read(|cx| {
        shell
            .read(cx)
            .workbench
            .read(cx)
            .tab_snapshots()
            .into_iter()
            .filter_map(|tab| match tab.surface {
                TabSurfaceId::Document(document) => Some(document),
                _ => None,
            })
            .collect()
    })
}

#[gpui::test]
async fn cancelling_a_bulk_close_leaves_every_tab(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (shell, window, _) = bulk_close_fixture(&documents, cx);
    let before = tab_documents(&shell, cx);
    let execution = dispatch_product_command(
        &shell,
        window,
        crate::app::commands::dispatch::CLOSE_OTHER_TABS_COMMAND,
        cx,
    );
    wait_prompt(cx);
    cx.simulate_prompt_answer("Don't Save");
    wait_prompt(cx);
    cx.simulate_prompt_answer("Cancel");
    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Cancelled
    );
    assert_eq!(tab_documents(&shell, cx), before);
}

#[gpui::test]
async fn close_other_tabs_prompts_once_per_dirty_document_and_keeps_pinned_tabs(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    let (shell, window, [shared, other, pinned, focused]) = bulk_close_fixture(&documents, cx);
    let execution = dispatch_product_command(
        &shell,
        window,
        crate::app::commands::dispatch::CLOSE_OTHER_TABS_COMMAND,
        cx,
    );
    // The shared document loses both views but is asked about once.
    wait_prompt(cx);
    cx.simulate_prompt_answer("Don't Save");
    wait_prompt(cx);
    cx.simulate_prompt_answer("Don't Save");
    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    assert!(!cx.has_pending_prompt());
    assert_eq!(tab_documents(&shell, cx), [pinned, focused]);
    cx.read(|cx| {
        assert!(documents.read(cx).get(shared).is_none());
        assert!(documents.read(cx).get(other).is_none());
    });

    // Close Pane closes pinned tabs too, leaving a replacement document.
    let execution = dispatch_product_command(
        &shell,
        window,
        crate::app::commands::dispatch::CLOSE_PANE_COMMAND,
        cx,
    );
    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    let remaining = tab_documents(&shell, cx);
    assert_eq!(remaining.len(), 1);
    assert!(!remaining.contains(&pinned) && !remaining.contains(&focused));
}

#[gpui::test]
async fn close_pane_collapses_its_split_and_keeps_the_other_pane(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (shell, window, [_, _, pinned, focused]) = bulk_close_fixture(&documents, cx);
    let split = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
                shell.split_pane(
                    pane,
                    SplitDirection::Horizontal,
                    super::super::SplitPlacement::After,
                    window,
                    cx,
                );
                shell.workbench.read(cx).focused_pane_id().unwrap()
            })
        })
        .unwrap();
    let execution = dispatch_product_command(
        &shell,
        window,
        crate::app::commands::dispatch::CLOSE_PANE_COMMAND,
        cx,
    );
    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    assert!(!cx.has_pending_prompt());
    cx.read(|cx| {
        let workbench = shell.read(cx).workbench.read(cx);
        assert!(workbench.pane(split).is_none());
        assert_eq!(workbench.panes().len(), 1);
    });
    let remaining = tab_documents(&shell, cx);
    assert_eq!(remaining.len(), 5);
    assert_eq!(remaining.first(), Some(&pinned));
    assert_eq!(remaining.last(), Some(&focused));
}

#[gpui::test]
fn clicking_a_tab_close_button_closes_that_tab_after_its_save_decision(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (first, second, shell, window) = two_dirty_untitled_documents_in_one_window(&documents, cx);
    let first_tab = cx.read(|cx| {
        let workbench = shell.read(cx).workbench.read(cx);
        let pane = workbench.focused_pane().unwrap();
        pane.tabs()
            .iter()
            .find(|tab| tab.document_id() == Some(first))
            .unwrap()
            .id()
    });
    let mut visual = VisualTestContext::from_window(window, cx);
    visual.run_until_parked();
    let close = visual
        .debug_bounds(format!("close-tab-{}", first_tab.value()).leak())
        .unwrap()
        .center();
    visual.simulate_click(close, Modifiers::default());
    wait_prompt(&mut visual);
    visual.simulate_prompt_answer("Don't Save");
    visual.run_until_parked();

    visual.read(|cx| {
        assert!(documents.read(cx).get(first).is_none());
        let workbench = shell.read(cx).workbench.read(cx);
        let pane = workbench.focused_pane().unwrap();
        assert_eq!(pane.tabs().len(), 1);
        assert_eq!(pane.active_tab().document_id(), Some(second));
    });
}
