//! Git-status fixture acceptance through the public extension API.
use super::*;
use crate::app::workbench::ExtensionView;

fn install_git_status(extensions: &Path) {
    install_extension(
        extensions,
        "@example/git-status",
        &[],
        &[(
            "dist/main.js",
            include_str!("../../../../examples/extensions/@example/git-status/dist/main.js"),
        )],
    );
}

fn log_text(shell: &Entity<ProductShell>, cx: &gpui::App) -> Option<String> {
    let content = &shell
        .read(cx)
        .contributed_views
        .get("example.git-log")?
        .content;
    match content {
        ExtensionView::Text(view) => Some(view.read(cx).text(cx)),
        ExtensionView::Tree(_) => None,
    }
}

#[gpui::test]
fn git_status_frame_groups_actions_and_log_through_public_apis(cx: &mut TestAppContext) {
    let extensions = tempfile::tempdir().unwrap();
    install_git_status(extensions.path());
    let storage = tempfile::tempdir().unwrap();
    let documents = install_globals(cx);
    let host = cx.update(crate::app::extension_host::install);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    host.update(cx, |host, cx| {
        host.set_test_storage_root(storage.path().to_owned(), cx)
    });
    host.update(cx, |host, cx| {
        host.start_installed_extensions(extensions.path().to_path_buf(), cx)
    });
    let report = wait_for_extension_report(&host, cx);
    assert!(
        report
            .report
            .entries
            .iter()
            .all(|entry| entry.result == crate::app::extension_load::LoadResult::Loaded),
    );
    assert_eq!(report.report.entries.len(), 1);

    let tree = wait_for_contributed_view(&shell, "example.git-status", cx);
    wait_for_api_condition(cx, |cx| {
        tree.read(cx)
            .visible_labels()
            .iter()
            .any(|label| label == "notes.txt")
    });
    assert_eq!(
        cx.read(|cx| tree.read(cx).visible_labels()),
        [
            "Staged",
            "src/main.rs",
            "src/app/tree_view.rs",
            "Changes",
            "docs/architecture/semantic-ui.md",
            "notes.txt",
        ]
    );
    assert_eq!(
        cx.read(|cx| tree.read(cx).status_for_test("notes.txt")),
        Some((
            Some("U".to_owned()),
            Some(crate::host::protocol::TreeBadge::Untracked)
        ))
    );
    let stage = cx.read(|cx| {
        tree.read(cx)
            .actions_for_row_for_test("notes.txt")
            .into_iter()
            .next()
            .expect("changed rows expose a stage action")
    });
    assert_eq!(stage.command, "example.git.stage");
    assert_eq!(
        stage.arguments,
        crate::host::protocol::CommandArgumentValue::Object(
            [(
                "path".to_owned(),
                crate::host::protocol::CommandArgumentValue::String("notes.txt".into())
            )]
            .into()
        )
    );

    wait_for_api_condition(cx, |cx| {
        log_text(&shell, cx).is_some_and(|text| text.contains("U  notes.txt"))
    });

    // Arrow navigation follows the rendered rows across groups and files.
    let _ = cx.update_window(window, |_, window, cx| {
        tree.read(cx).focus_handle(cx).focus(window);
    });
    cx.refresh().unwrap();
    for expected in ["Staged", "src/main.rs", "src/app/tree_view.rs", "Changes"] {
        cx.simulate_keystrokes(window, "down");
        cx.run_until_parked();
        assert_eq!(
            cx.read(|cx| tree.read(cx).selected_text()).as_deref(),
            Some(expected)
        );
    }

    // Route the row action through the native tree event path with the tree
    // focused, so the captured window and item arguments are exercised.
    let _ = cx.update_window(window, |_, window, cx| {
        tree.read(cx).focus_handle(cx).focus(window);
    });
    cx.run_until_parked();
    let _ = cx.update_window(window, |_, window, cx| {
        tree.update(cx, |tree, cx| {
            tree.invoke_action(stage.clone(), window.window_handle(), cx)
        });
    });
    wait_for_api_condition(cx, |cx| {
        let labels = tree.read(cx).visible_labels();
        let notes = labels.iter().position(|label| label == "notes.txt");
        let changes = labels.iter().position(|label| label == "Changes");
        notes.is_some_and(|notes| changes.is_some_and(|changes| notes < changes))
            && log_text(&shell, cx).is_some_and(|text| text.contains("U  notes.txt"))
    });

    // Activation routes through the same captured-window path and reads the
    // selected item text from the live tree.
    let _ = cx.update_window(window, |_, window, cx| {
        tree.update(cx, |tree, cx| {
            tree.activate_row_for_test("src/main.rs", window.window_handle(), cx)
        });
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        cx.run_until_parked();
        let clipboard = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .is_some_and(|text| text == "src/main.rs");
        if clipboard
            && cx.read(|cx| {
                log_text(&shell, cx).is_some_and(|text| text.contains("open src/main.rs"))
            })
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "activation did not route through the captured tree"
        );
        std::thread::yield_now();
    }
}
