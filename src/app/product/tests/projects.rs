//! Bundled project registry behavior through the product host.

use super::operations::empty_product_window;
use super::*;

fn wait_for_prompt(cx: &mut TestAppContext) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !cx.has_pending_prompt() {
        cx.run_until_parked();
        assert!(
            std::time::Instant::now() < deadline,
            "native prompt never appeared"
        );
        std::thread::yield_now();
    }
}

/// Focuses the normal pane's active document so tool commands target it.
fn focus_document_tab(
    shell: &Entity<ProductShell>,
    window: gpui::AnyWindowHandle,
    cx: &mut TestAppContext,
) {
    let located = cx.read(|cx| {
        let workbench = shell.read(cx).workbench.read(cx);
        let pane = workbench.normal_pane()?;
        let tab = workbench.pane(pane)?.active_tab_id();
        Some((pane, tab))
    });
    let Some((pane, tab)) = located else {
        return;
    };
    cx.update_window(window, |_, window, cx| {
        shell.update(cx, |shell, cx| shell.activate_tab(pane, tab, window, cx));
    })
    .unwrap();
    cx.run_until_parked();
}

fn tree_tab_attached(
    shell: &Entity<ProductShell>,
    tree: &Entity<crate::app::tree_view::TreeView>,
    cx: &gpui::App,
) -> bool {
    let focus = tree.read(cx).focus_handle(cx).downgrade();
    shell
        .read(cx)
        .workbench
        .read(cx)
        .panes()
        .iter()
        .any(|pane| {
            pane.tabs().iter().any(|tab| {
                tab.command_view()
                    .is_some_and(|view| view.matches_focus(&focus, cx))
            })
        })
}

fn tree_tab_focused(
    shell: &Entity<ProductShell>,
    tree: &Entity<crate::app::tree_view::TreeView>,
    cx: &gpui::App,
) -> bool {
    let focus = tree.read(cx).focus_handle(cx).downgrade();
    let workbench = shell.read(cx).workbench.read(cx);
    workbench.focused_pane().is_some_and(|pane| {
        pane.active_tab()
            .command_view()
            .is_some_and(|view| view.matches_focus(&focus, cx))
    })
}

/// Makes the contributed tree the active tool tab so row events resolve against it.
fn activate_tree_tab(
    shell: &Entity<ProductShell>,
    tree: &Entity<crate::app::tree_view::TreeView>,
    window: gpui::AnyWindowHandle,
    cx: &mut TestAppContext,
) {
    let focus = cx.read(|cx| tree.read(cx).focus_handle(cx).downgrade());
    let located = cx.read(|cx| {
        let workbench = shell.read(cx).workbench.read(cx);
        workbench.panes().iter().find_map(|pane| {
            pane.tabs()
                .iter()
                .find(|tab| {
                    tab.command_view()
                        .is_some_and(|view| view.matches_focus(&focus, cx))
                })
                .map(|tab| (pane.id(), tab.id()))
        })
    });
    let (pane, tab) = located.expect("contributed tree tab is attached");
    cx.update_window(window, |_, window, cx| {
        shell.update(cx, |shell, cx| shell.activate_tab(pane, tab, window, cx));
    })
    .unwrap();
    cx.run_until_parked();
}

fn invoke_tree_action(
    shell: &Entity<ProductShell>,
    tree: &Entity<crate::app::tree_view::TreeView>,
    action: crate::host::protocol::TreeAction,
    window: gpui::AnyWindowHandle,
    cx: &mut TestAppContext,
) {
    activate_tree_tab(shell, tree, window, cx);
    let _ = cx.update_window(window, |_, window, cx| {
        tree.read(cx).focus_handle(cx).focus(window);
    });
    cx.run_until_parked();
    let _ = cx.update_window(window, |_, window, cx| {
        tree.update(cx, |tree, cx| {
            tree.invoke_action(action, window.window_handle(), cx)
        });
    });
}

fn clipboard_json(cx: &TestAppContext) -> serde_json::Value {
    cx.read(|cx| serde_json::from_str(&cx.read_from_clipboard().unwrap().text().unwrap()).unwrap())
}

fn wait_for_input_transient(shell: &Entity<ProductShell>, cx: &mut TestAppContext) {
    use crate::app::transient::TransientSurface;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        cx.run_until_parked();
        let waiting_for_input = cx.read(|cx| {
            matches!(
                shell
                    .read(cx)
                    .transient
                    .as_ref()
                    .and_then(|active| active.surface.as_ref()),
                Some(TransientSurface::Input(_))
            )
        });
        if waiting_for_input {
            return;
        }
        assert!(
            !cx.has_pending_prompt(),
            "command reported a native prompt instead of opening the input"
        );
        assert!(
            std::time::Instant::now() < deadline,
            "input transient never appeared"
        );
        std::thread::yield_now();
    }
}

fn listed_projects(
    shell: &Entity<ProductShell>,
    window: gpui::AnyWindowHandle,
    cx: &mut TestAppContext,
) -> serde_json::Value {
    assert_eq!(
        wait_for_command(
            dispatch_product_command(shell, window, "projects.list", cx),
            cx
        ),
        CommandOutcome::Completed
    );
    clipboard_json(cx)
}

struct Fixture {
    documents: Entity<DocumentCollection>,
    host: Entity<crate::app::extension_host::ProductExtensionHost>,
    shell: Entity<ProductShell>,
    window: gpui::AnyWindowHandle,
    _storage: tempfile::TempDir,
    extensions: tempfile::TempDir,
}

fn start_registry(cx: &mut TestAppContext) -> Fixture {
    let documents = install_globals(cx);
    let host = cx.update(crate::app::extension_host::install);
    let (shell, window) = empty_product_window(&documents, cx);
    let storage = tempfile::tempdir().unwrap();
    let extensions = tempfile::tempdir().unwrap();
    host.update(cx, |host, cx| {
        host.set_test_storage_root(storage.path().to_owned(), cx)
    });
    restart_registry(&host, &extensions, cx);
    show_registry(&shell, window, cx);
    Fixture {
        documents,
        host,
        shell,
        window,
        _storage: storage,
        extensions,
    }
}

fn show_registry(
    shell: &Entity<ProductShell>,
    window: gpui::AnyWindowHandle,
    cx: &mut TestAppContext,
) {
    assert_eq!(
        wait_for_command(
            dispatch_product_command(shell, window, "projects.show", cx),
            cx
        ),
        CommandOutcome::Completed
    );
    cx.run_until_parked();
}

fn restart_registry(
    host: &Entity<crate::app::extension_host::ProductExtensionHost>,
    extensions: &tempfile::TempDir,
    cx: &mut TestAppContext,
) {
    host.update(cx, |host, cx| {
        host.start_product_extensions(extensions.path().to_path_buf(), cx)
    });
    let report = wait_for_extension_report(host, cx);
    assert!(report.report.entries.iter().any(|entry| {
        entry.name.as_deref() == Some("@knot/projects")
            && entry.result == crate::app::extension_load::LoadResult::Loaded
    }));
}

fn create_project(
    shell: &Entity<ProductShell>,
    window: gpui::AnyWindowHandle,
    directory: &std::path::Path,
    name: &str,
    cx: &mut TestAppContext,
) {
    cx.set_global(ApplicationOpenDialog(Arc::new(FixedOpenDialog(
        OpenDialogOutcome::Selected(directory.to_path_buf()),
    ))));
    let create = dispatch_product_command(shell, window, "projects.create", cx);
    wait_for_input_transient(shell, cx);
    cx.simulate_keystrokes(window, "cmd-a");
    cx.simulate_input(window, name);
    cx.simulate_keystrokes(window, "enter");
    assert_eq!(wait_for_command(create, cx), CommandOutcome::Completed);
}

#[gpui::test]
fn bundled_projects_registry_manages_roots_and_persists(cx: &mut TestAppContext) {
    let fixture = start_registry(cx);
    let tree = wait_for_contributed_view(&fixture.shell, "builtin.projects", cx);

    let project_dir = tempfile::tempdir().unwrap();
    let uri = url::Url::from_file_path(project_dir.path())
        .unwrap()
        .to_string();
    create_project(
        &fixture.shell,
        fixture.window,
        project_dir.path(),
        "Knot Editor",
        cx,
    );

    wait_for_api_condition(cx, |cx| {
        tree.read(cx)
            .visible_labels()
            .iter()
            .any(|label| label == "Knot Editor")
    });
    cx.read(|cx| {
        assert_eq!(
            fixture
                .shell
                .read(cx)
                .workspace
                .snapshot()
                .root()
                .map(ToString::to_string),
            Some(uri.clone())
        );
    });

    let listed = listed_projects(&fixture.shell, fixture.window, cx);
    let id = listed["projects"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(listed["projects"][0]["name"], "Knot Editor");
    assert_eq!(listed["projects"][0]["roots"][0], uri);
    assert_eq!(listed["recentProjectId"], id);
    wait_for_api_condition(cx, |cx| {
        tree.read(cx)
            .status_for_test(&id)
            .is_some_and(|(status, _)| status.as_deref() == Some("active"))
    });

    let rename = cx.read(|cx| {
        tree.read(cx)
            .actions_for_row_for_test(&id)
            .into_iter()
            .find(|action| action.command == "projects.rename")
            .expect("rows expose rename")
    });
    invoke_tree_action(&fixture.shell, &tree, rename, fixture.window, cx);
    wait_for_input_transient(&fixture.shell, cx);
    cx.simulate_keystrokes(fixture.window, "cmd-a");
    cx.simulate_input(fixture.window, "Renamed");
    cx.simulate_keystrokes(fixture.window, "enter");
    wait_for_api_condition(cx, |cx| {
        tree.read(cx)
            .visible_labels()
            .iter()
            .any(|label| label == "Renamed")
    });

    // A reload keeps the record and re-registers the contributed commands.
    restart_registry(&fixture.host, &fixture.extensions, cx);
    let tree = wait_for_contributed_view(&fixture.shell, "builtin.projects", cx);
    wait_for_api_condition(cx, |cx| {
        tree_tab_attached(&fixture.shell, &tree, cx)
            && tree
                .read(cx)
                .visible_labels()
                .iter()
                .any(|label| label == "Renamed")
    });
    let listed = listed_projects(&fixture.shell, fixture.window, cx);
    assert_eq!(listed["projects"][0]["id"], id);
    assert_eq!(listed["projects"][0]["name"], "Renamed");

    let remove = cx.read(|cx| {
        tree.read(cx)
            .actions_for_row_for_test(&id)
            .into_iter()
            .find(|action| action.command == "projects.remove")
            .expect("rows expose remove")
    });
    invoke_tree_action(&fixture.shell, &tree, remove, fixture.window, cx);
    wait_for_prompt(cx);
    cx.simulate_prompt_answer("Remove");
    wait_for_api_condition(cx, |cx| {
        !tree
            .read(cx)
            .visible_labels()
            .iter()
            .any(|label| label == "Renamed")
    });
    assert_eq!(
        listed_projects(&fixture.shell, fixture.window, cx)["projects"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[gpui::test]
fn opening_a_project_with_a_missing_root_is_actionable(cx: &mut TestAppContext) {
    let fixture = start_registry(cx);
    let tree = wait_for_contributed_view(&fixture.shell, "builtin.projects", cx);

    let project_dir = tempfile::tempdir().unwrap();
    let path = project_dir.path().to_path_buf();
    create_project(&fixture.shell, fixture.window, &path, "Gone", cx);
    let listed = listed_projects(&fixture.shell, fixture.window, cx);
    let id = listed["projects"][0]["id"].as_str().unwrap().to_string();
    wait_for_api_condition(cx, |cx| {
        tree.read(cx).visible_labels().iter().any(|l| l == "Gone")
    });

    std::fs::remove_dir_all(&path).unwrap();
    activate_tree_tab(&fixture.shell, &tree, fixture.window, cx);
    cx.update_window(fixture.window, |_, window, cx| {
        tree.update(cx, |tree, cx| {
            tree.activate_row_for_test(&id, window.window_handle(), cx)
        });
    })
    .unwrap();
    wait_for_prompt(cx);
    cx.simulate_prompt_answer("OK");

    // The record is retained even though opening its root failed.
    let listed = listed_projects(&fixture.shell, fixture.window, cx);
    assert_eq!(listed["projects"][0]["name"], "Gone");
    assert_eq!(
        listed["projects"][0]["roots"][0],
        url::Url::from_file_path(&path).unwrap().to_string()
    );
}

#[gpui::test]
fn project_selection_stays_in_the_capturing_window(cx: &mut TestAppContext) {
    let fixture = start_registry(cx);
    let first_dir = tempfile::tempdir().unwrap();
    let first_uri = url::Url::from_file_path(first_dir.path())
        .unwrap()
        .to_string();
    create_project(
        &fixture.shell,
        fixture.window,
        first_dir.path(),
        "First",
        cx,
    );

    let (second_shell, second_window) = empty_product_window(&fixture.documents, cx);
    show_registry(&second_shell, second_window, cx);
    let second_tree = wait_for_contributed_view(&second_shell, "builtin.projects", cx);
    let second_dir = tempfile::tempdir().unwrap();
    let second_uri = url::Url::from_file_path(second_dir.path())
        .unwrap()
        .to_string();
    create_project(
        &second_shell,
        second_window,
        second_dir.path(),
        "Second",
        cx,
    );

    let listed = listed_projects(&second_shell, second_window, cx);
    let second_id = listed["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|project| project["name"] == "Second")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Row activation in one window opens that project's root there only.
    activate_tree_tab(&second_shell, &second_tree, second_window, cx);
    cx.update_window(second_window, |_, window, cx| {
        second_tree.update(cx, |tree, cx| {
            tree.activate_row_for_test(&second_id, window.window_handle(), cx)
        });
    })
    .unwrap();
    cx.run_until_parked();
    cx.read(|cx| {
        assert_eq!(
            second_shell
                .read(cx)
                .workspace
                .snapshot()
                .root()
                .map(ToString::to_string),
            Some(second_uri.clone())
        );
        assert_eq!(
            fixture
                .shell
                .read(cx)
                .workspace
                .snapshot()
                .root()
                .map(ToString::to_string),
            Some(first_uri.clone())
        );
    });
    wait_for_api_condition(cx, |cx| {
        second_tree
            .read(cx)
            .visible_labels()
            .iter()
            .any(|label| label == "Second")
    });
}

#[gpui::test]
fn projects_tool_visibility_survives_reload(cx: &mut TestAppContext) {
    let fixture = start_registry(cx);
    let tree = wait_for_contributed_view(&fixture.shell, "builtin.projects", cx);
    assert!(cx.read(|cx| tree_tab_attached(&fixture.shell, &tree, cx)));
    activate_tree_tab(&fixture.shell, &tree, fixture.window, cx);

    focus_document_tab(&fixture.shell, fixture.window, cx);
    let hide = dispatch_product_command(&fixture.shell, fixture.window, "projects.hide", cx);
    assert_eq!(wait_for_command(hide, cx), CommandOutcome::Completed);
    cx.run_until_parked();
    assert!(!cx.read(|cx| tree_tab_attached(&fixture.shell, &tree, cx)));

    // A hidden choice stays hidden across a reload.
    restart_registry(&fixture.host, &fixture.extensions, cx);
    let tree = wait_for_contributed_view(&fixture.shell, "builtin.projects", cx);
    cx.run_until_parked();
    assert!(!cx.read(|cx| tree_tab_attached(&fixture.shell, &tree, cx)));

    focus_document_tab(&fixture.shell, fixture.window, cx);
    let show = dispatch_product_command(&fixture.shell, fixture.window, "projects.show", cx);
    assert_eq!(wait_for_command(show, cx), CommandOutcome::Completed);
    wait_for_api_condition(cx, |cx| tree_tab_focused(&fixture.shell, &tree, cx));

    // A shown choice is reapplied and selected in the window after another reload.
    restart_registry(&fixture.host, &fixture.extensions, cx);
    let tree = wait_for_contributed_view(&fixture.shell, "builtin.projects", cx);
    wait_for_api_condition(cx, |cx| tree_tab_focused(&fixture.shell, &tree, cx));
}
