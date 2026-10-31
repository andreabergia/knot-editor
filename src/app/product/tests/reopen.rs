//! Reopen Closed Tab after single and bulk closes.
use super::*;
use crate::app::commands::dispatch::{
    CLOSE_OTHER_TABS_COMMAND, REOPEN_CLOSED_TAB_COMMAND, SPLIT_HORIZONTAL_COMMAND,
};
use crate::app::documents::DocumentId;
use crate::app::workbench::PaneId;

/// A window whose pane holds an untitled document followed by three files.
fn files_window(
    cx: &mut TestAppContext,
) -> (
    Entity<ProductShell>,
    gpui::AnyWindowHandle,
    [ResourceUri; 3],
) {
    let documents = install_globals(cx);
    let (filesystems, provider) = memory_filesystems_with_provider();
    cx.set_global(ApplicationFileSystems(filesystems));
    let uris = ["a", "b", "c"].map(|name| {
        let uri = ResourceUri::parse(&format!("mem://product/{name}.txt")).unwrap();
        provider
            .seed_file(uri.clone(), format!("{name}\n{name}\n").as_bytes())
            .unwrap();
        uri
    });
    let untitled = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(untitled).unwrap().model().clone());
    let (shell, window) = product_window(untitled, model, cx);
    for uri in &uris {
        let file = pollster::block_on(provider.read(uri.clone())).unwrap();
        let model = cx.new(|_| BufferModel::from_text(String::from_utf8(file.bytes).unwrap()));
        let document = documents.update(cx, |documents, cx| {
            documents.create_persisted(
                uri.to_string(),
                model.clone(),
                uri.clone(),
                0,
                file.version,
                cx,
            )
        });
        shell.update(cx, |shell, cx| {
            shell.workbench.update(cx, |workbench, cx| {
                let pane = workbench.focused_pane_id().unwrap();
                workbench.open_tab_for_document(pane, document, model, cx);
            });
        });
    }
    cx.run_until_parked();
    (shell, window, uris)
}

/// The focused pane's tabs as resource URIs, `None` for other tabs.
fn tab_uris(shell: &Entity<ProductShell>, cx: &TestAppContext) -> Vec<Option<String>> {
    cx.read(|cx| {
        let documents = cx.global::<ApplicationDocuments>().0.read(cx);
        shell
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .tabs()
            .iter()
            .map(|tab| {
                tab.document_id()
                    .and_then(|document| documents.get(document))
                    .and_then(|document| document.resource_uri())
                    .map(ToString::to_string)
            })
            .collect()
    })
}

fn activate(shell: &Entity<ProductShell>, index: usize, cx: &mut TestAppContext) -> PaneId {
    shell.update(cx, |shell, cx| {
        shell.workbench.update(cx, |workbench, _| {
            let pane = workbench.focused_pane_id().unwrap();
            let tab = workbench.pane(pane).unwrap().tabs()[index].id();
            workbench.activate_tab(pane, tab);
            pane
        })
    })
}

fn run(
    shell: &Entity<ProductShell>,
    window: gpui::AnyWindowHandle,
    name: &'static str,
    cx: &mut TestAppContext,
) -> CommandOutcome {
    let execution = dispatch_product_command(shell, window, name, cx);
    let outcome = wait_for_command(execution, cx);
    cx.run_until_parked();
    outcome
}

fn active_document(shell: &Entity<ProductShell>, cx: &TestAppContext) -> Option<DocumentId> {
    cx.read(|cx| {
        shell
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab()
            .document_id()
    })
}

#[gpui::test]
fn a_closed_document_reopens_at_its_position_with_its_view(cx: &mut TestAppContext) {
    let (shell, window, [a, b, c]) = files_window(cx);
    activate(&shell, 2, cx);
    let state = crate::app::editor::EditorViewState {
        anchor: 2,
        caret: 3,
        top: 0,
        scroll_x: 0.,
    };
    shell.update(cx, |shell, cx| {
        let editor = shell
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab()
            .editor()
            .unwrap()
            .clone();
        editor.update(cx, |editor, cx| editor.restore_view_state(state, cx));
    });
    let closed = active_document(&shell, cx).unwrap();
    assert_eq!(
        run(&shell, window, CLOSE_TAB_COMMAND, cx),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert!(
            cx.global::<ApplicationDocuments>()
                .0
                .read(cx)
                .get(closed)
                .is_none()
        )
    });
    let others =
        |shell: &Entity<ProductShell>, cx: &TestAppContext| tab_uris(shell, cx)[1..].to_vec();
    assert_eq!(
        others(&shell, cx),
        [Some(a.to_string()), Some(c.to_string())]
    );

    assert_eq!(
        run(&shell, window, REOPEN_CLOSED_TAB_COMMAND, cx),
        CommandOutcome::Completed
    );
    assert_eq!(
        others(&shell, cx),
        [
            Some(a.to_string()),
            Some(b.to_string()),
            Some(c.to_string())
        ]
    );
    cx.read(|cx| {
        let pane = shell.read(cx).workbench.read(cx).focused_pane().unwrap();
        assert_eq!(pane.active_tab_id(), pane.tabs()[2].id());
        assert_eq!(
            pane.active_tab().editor().unwrap().read(cx).view_state(cx),
            state
        );
    });
    // The stack is empty again.
    assert_eq!(
        run(&shell, window, REOPEN_CLOSED_TAB_COMMAND, cx),
        CommandOutcome::Unavailable
    );
}

#[gpui::test]
fn untitled_documents_are_skipped_and_a_vanished_pane_falls_back_to_the_focused_one(
    cx: &mut TestAppContext,
) {
    let (shell, window, [a, _, _]) = files_window(cx);
    let main = activate(&shell, 0, cx);
    // Closing the untitled document leaves nothing to reopen.
    run(&shell, window, CLOSE_TAB_COMMAND, cx);
    assert_eq!(
        run(&shell, window, REOPEN_CLOSED_TAB_COMMAND, cx),
        CommandOutcome::Unavailable
    );

    // A split view closes with its pane; reopening lands in the focused pane.
    activate(&shell, 0, cx);
    run(&shell, window, SPLIT_HORIZONTAL_COMMAND, cx);
    let split = cx.read(|cx| shell.read(cx).workbench.read(cx).focused_pane_id().unwrap());
    assert_ne!(split, main);
    run(&shell, window, CLOSE_TAB_COMMAND, cx);
    cx.read(|cx| assert!(shell.read(cx).workbench.read(cx).pane(split).is_none()));
    let before = tab_uris(&shell, cx).len();
    assert_eq!(
        run(&shell, window, REOPEN_CLOSED_TAB_COMMAND, cx),
        CommandOutcome::Completed
    );
    let after = tab_uris(&shell, cx);
    assert_eq!(after.len(), before + 1);
    assert_eq!(after.last().unwrap(), &Some(a.to_string()));
    cx.read(|cx| {
        assert_eq!(
            shell.read(cx).workbench.read(cx).focused_pane_id(),
            Some(main)
        )
    });
}

#[gpui::test]
fn a_bulk_close_reopens_in_reverse_order(cx: &mut TestAppContext) {
    let (shell, window, [a, b, c]) = files_window(cx);
    activate(&shell, 3, cx);
    run(&shell, window, CLOSE_OTHER_TABS_COMMAND, cx);
    assert_eq!(tab_uris(&shell, cx), [Some(c.to_string())]);
    run(&shell, window, REOPEN_CLOSED_TAB_COMMAND, cx);
    assert_eq!(
        tab_uris(&shell, cx),
        [Some(b.to_string()), Some(c.to_string())]
    );
    run(&shell, window, REOPEN_CLOSED_TAB_COMMAND, cx);
    assert_eq!(
        tab_uris(&shell, cx),
        [
            Some(a.to_string()),
            Some(b.to_string()),
            Some(c.to_string())
        ]
    );
    // The untitled document was closed too but is not reopened.
    assert_eq!(
        run(&shell, window, REOPEN_CLOSED_TAB_COMMAND, cx),
        CommandOutcome::Unavailable
    );
}

#[gpui::test]
fn a_terminal_reopens_in_its_last_directory(cx: &mut TestAppContext) {
    let (shell, window, _) = files_window(cx);
    let directory = tempfile::tempdir().unwrap();
    shell.update(cx, |shell, cx| {
        let (id, view) = shell.create_terminal_in(Some(directory.path().to_owned()), cx);
        shell.workbench.update(cx, |workbench, _| {
            let pane = workbench.focused_pane_id().unwrap();
            workbench.open_terminal_tab(pane, id, view);
        });
    });
    cx.run_until_parked();
    assert_eq!(
        run(&shell, window, CLOSE_TAB_COMMAND, cx),
        CommandOutcome::Completed
    );
    assert!(active_document(&shell, cx).is_some());
    assert_eq!(
        run(&shell, window, REOPEN_CLOSED_TAB_COMMAND, cx),
        CommandOutcome::Completed
    );
    let started = cx.read(|cx| {
        let surface = shell
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab()
            .surface_id();
        let TabSurfaceId::Terminal(id) = surface else {
            panic!("the reopened terminal is not active")
        };
        cx.global::<ApplicationTerminalSessions>()
            .0
            .borrow()
            .sessions[&id]
            .read(cx)
            .working_directory()
            .map(Path::to_owned)
    });
    assert_eq!(
        started.map(|path| path.canonicalize().unwrap()),
        Some(directory.path().canonicalize().unwrap())
    );
}
