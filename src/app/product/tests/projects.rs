//! Bundled project registry behavior through the product host.

use super::operations::empty_product_window;
use super::*;

pub(super) fn wait_for_prompt(cx: &mut TestAppContext) {
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
pub(super) fn activate_tree_tab(
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

pub(super) fn wait_for_input_transient(shell: &Entity<ProductShell>, cx: &mut TestAppContext) {
    use crate::app::transient::TransientSurface;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
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

pub(super) fn listed_projects(
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

pub(super) struct Fixture {
    pub(super) documents: Entity<DocumentCollection>,
    pub(super) host: Entity<crate::app::extension_host::ProductExtensionHost>,
    pub(super) shell: Entity<ProductShell>,
    pub(super) window: gpui::AnyWindowHandle,
    pub(super) storage: tempfile::TempDir,
    pub(super) extensions: tempfile::TempDir,
}

/// The focused product window; a switch closes its source windows and
/// focuses the first destination.
pub(super) fn active_shell(cx: &TestAppContext) -> (Entity<ProductShell>, gpui::AnyWindowHandle) {
    let window = cx.read(|cx| cx.active_window()).expect("an active window");
    let shell = cx.read(|cx| {
        window
            .downcast::<ProductShell>()
            .unwrap()
            .entity(cx)
            .unwrap()
    });
    (shell, window)
}

impl Fixture {
    /// Follow the window a switch focused.
    pub(super) fn follow(&mut self, cx: &TestAppContext) {
        (self.shell, self.window) = active_shell(cx);
    }

    pub(super) fn create_project(
        &mut self,
        directory: &std::path::Path,
        name: &str,
        cx: &mut TestAppContext,
    ) {
        let (shell, window) = (self.shell.clone(), self.window);
        create_project(&shell, window, directory, name, cx);
        self.follow(cx);
    }
}

pub(super) fn start_registry(cx: &mut TestAppContext) -> Fixture {
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
        storage,
        extensions,
    }
}

pub(super) fn show_registry(
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

pub(super) fn restart_registry(
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

pub(super) fn create_project(
    shell: &Entity<ProductShell>,
    window: gpui::AnyWindowHandle,
    directory: &std::path::Path,
    name: &str,
    cx: &mut TestAppContext,
) -> (Entity<ProductShell>, gpui::AnyWindowHandle) {
    cx.set_global(ApplicationOpenDialog(Arc::new(FixedOpenDialog(
        OpenDialogOutcome::Selected(directory.to_path_buf()),
    ))));
    let create = dispatch_product_command(shell, window, "projects.create", cx);
    wait_for_input_transient(shell, cx);
    cx.simulate_keystrokes(window, "cmd-a");
    cx.simulate_input(window, name);
    cx.simulate_keystrokes(window, "enter");
    // Creating a project switches the window to it; when the command was
    // captured on a document tab that the switch replaced, its completion
    // reports the stale capture rather than a failure.
    assert!(matches!(
        wait_for_command(create, cx),
        CommandOutcome::Completed | CommandOutcome::InvalidTarget
    ));
    active_shell(cx)
}

#[gpui::test]
fn bundled_projects_registry_manages_roots_and_persists(cx: &mut TestAppContext) {
    let mut fixture = start_registry(cx);

    let project_dir = tempfile::tempdir().unwrap();
    let uri = url::Url::from_file_path(project_dir.path())
        .unwrap()
        .to_string();
    fixture.create_project(project_dir.path(), "Knot Editor", cx);
    // Creating the project replaced the blank window with the project's.
    let tree = wait_for_contributed_view(&fixture.shell, "builtin.projects", cx);

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
                .first_root()
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
fn switching_to_a_project_with_a_missing_root_is_actionable_and_keeps_the_source(
    cx: &mut TestAppContext,
) {
    let mut fixture = start_registry(cx);

    let gone_dir = tempfile::tempdir().unwrap();
    let path = gone_dir.path().to_path_buf();
    fixture.create_project(&path, "Gone", cx);
    let other_dir = tempfile::tempdir().unwrap();
    let other_uri = url::Url::from_file_path(other_dir.path())
        .unwrap()
        .to_string();
    fixture.create_project(other_dir.path(), "Other", cx);
    let listed = listed_projects(&fixture.shell, fixture.window, cx);
    let id = |name: &str| {
        listed["projects"]
            .as_array()
            .unwrap()
            .iter()
            .find(|project| project["name"] == name)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let (gone, other) = (id("Gone"), id("Other"));
    let tree = wait_for_contributed_view(&fixture.shell, "builtin.projects", cx);
    assert_eq!(listed["sessions"][&gone]["state"], "suspended");
    assert_eq!(listed["sessions"][&other]["state"], "active");

    wait_for_api_condition(cx, |cx| {
        let labels = tree.read(cx).visible_labels();
        labels.iter().any(|label| label == "Gone") && labels.iter().any(|label| label == "Other")
    });
    std::fs::remove_dir_all(&path).unwrap();
    activate_tree_tab(&fixture.shell, &tree, fixture.window, cx);
    cx.update_window(fixture.window, |_, window, cx| {
        tree.update(cx, |tree, cx| {
            tree.activate_row_for_test(&gone, window.window_handle(), cx)
        });
    })
    .unwrap();
    wait_for_prompt(cx);
    assert!(
        cx.pending_prompt()
            .is_some_and(|(_, message)| message.contains("ResourceNotFound"))
    );
    cx.simulate_prompt_answer("OK");

    // The record is retained and the source window stays on its project.
    let listed = listed_projects(&fixture.shell, fixture.window, cx);
    assert_eq!(
        listed["projects"]
            .as_array()
            .unwrap()
            .iter()
            .find(|project| project["id"] == gone.as_str())
            .unwrap()["roots"][0],
        url::Url::from_file_path(&path).unwrap().to_string()
    );
    assert_eq!(listed["sessions"][&other]["state"], "active");
    cx.read(|cx| {
        assert_eq!(
            fixture
                .shell
                .read(cx)
                .workspace
                .snapshot()
                .first_root()
                .map(ToString::to_string),
            Some(other_uri.clone())
        );
    });
}

#[gpui::test]
fn project_selection_stays_in_the_capturing_window(cx: &mut TestAppContext) {
    let mut fixture = start_registry(cx);
    let first_dir = tempfile::tempdir().unwrap();
    let first_uri = url::Url::from_file_path(first_dir.path())
        .unwrap()
        .to_string();
    fixture.create_project(first_dir.path(), "First", cx);

    let (second_shell, second_window) = empty_product_window(&fixture.documents, cx);
    show_registry(&second_shell, second_window, cx);
    let second_dir = tempfile::tempdir().unwrap();
    let second_uri = url::Url::from_file_path(second_dir.path())
        .unwrap()
        .to_string();
    // The blank second window is replaced by the new project's window.
    let (second_shell, second_window) = create_project(
        &second_shell,
        second_window,
        second_dir.path(),
        "Second",
        cx,
    );
    let second_tree = wait_for_contributed_view(&second_shell, "builtin.projects", cx);

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
                .first_root()
                .map(ToString::to_string),
            Some(second_uri.clone())
        );
        assert_eq!(
            fixture
                .shell
                .read(cx)
                .workspace
                .snapshot()
                .first_root()
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
fn command_invoked_project_tool_changes_publish_the_window_default(cx: &mut TestAppContext) {
    // start_registry shows the tool through a command, which is an explicit user
    // arrangement and therefore becomes the fresh-window default.
    let fixture = start_registry(cx);
    let tree = wait_for_contributed_view(&fixture.shell, "builtin.projects", cx);
    wait_for_api_condition(cx, |cx| tree_tab_focused(&fixture.shell, &tree, cx));
    cx.read(|cx| {
        let snapshot = cx
            .global::<crate::app::tool_layout::ApplicationToolLayout>()
            .0
            .current();
        assert!(
            snapshot
                .tool("builtin.projects")
                .is_some_and(|tool| tool.visible)
        );
        // Left dock is index 0 and now selects the shown tool.
        assert_eq!(
            snapshot.dock_selected[0].as_deref(),
            Some("builtin.projects")
        );
    });

    // A late native explorer registration must not steal the stored selection.
    fixture
        .shell
        .update(cx, |shell, cx| shell.restore_fresh_window_layout(cx));
    wait_for_api_condition(cx, |cx| tree_tab_focused(&fixture.shell, &tree, cx));

    focus_document_tab(&fixture.shell, fixture.window, cx);
    let hide = dispatch_product_command(&fixture.shell, fixture.window, "projects.hide", cx);
    assert_eq!(wait_for_command(hide, cx), CommandOutcome::Completed);
    cx.run_until_parked();
    assert!(!cx.read(|cx| tree_tab_attached(&fixture.shell, &tree, cx)));
    cx.read(|cx| {
        let snapshot = cx
            .global::<crate::app::tool_layout::ApplicationToolLayout>()
            .0
            .current();
        assert!(
            !snapshot
                .tool("builtin.projects")
                .is_some_and(|tool| tool.visible)
        );
    });
}
