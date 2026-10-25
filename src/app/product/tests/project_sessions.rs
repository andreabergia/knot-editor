//! `@knot/projects` sessions: switching, tracking, suspension, and removal.

use super::operations::empty_product_window;
use super::projects::{
    Fixture, activate_tree_tab, create_project, listed_projects, restart_registry, start_registry,
    wait_for_prompt,
};
use super::*;
use crate::app::recovery::{ApplicationRecovery, StoredSnapshot};
use serde_json::Value;

const OWNER: &str = "@knot/projects";

fn file_uri(path: &Path) -> String {
    url::Url::from_file_path(path).unwrap().to_string()
}

/// A project directory with one file.
fn project_dir(file: &str, text: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join(file), text).unwrap();
    directory
}

fn open_path(shell: &Entity<ProductShell>, path: &Path, cx: &mut TestAppContext) {
    let request = OpenRequest::from_path(path, Path::new("/")).unwrap();
    let target = cx.read(|cx| current_open_target(shell, cx));
    let mut opened = shell.update(cx, |shell, cx| {
        shell.start_open_operation(request, target, false, cx)
    });
    // Local file I/O completes on the provider's own runtime.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        cx.run_until_parked();
        if let Ok(result) = opened.try_recv() {
            assert!(result.is_ok(), "open failed");
            return;
        }
        assert!(std::time::Instant::now() < deadline, "open did not settle");
        std::thread::yield_now();
    }
}

/// Stored JSON numbers are doubles; compare records independently of that.
fn normalized(value: Value) -> Value {
    match value {
        Value::Number(number) => serde_json::json!(number.as_f64().unwrap()),
        Value::Array(items) => Value::Array(items.into_iter().map(normalized).collect()),
        Value::Object(fields) => Value::Object(
            fields
                .into_iter()
                .map(|(key, value)| (key, normalized(value)))
                .collect(),
        ),
        other => other,
    }
}

fn set_root(shell: &Entity<ProductShell>, uri: Option<&str>, cx: &mut TestAppContext) {
    shell.update(cx, |shell, cx| {
        let roots = uri
            .map(|uri| vec![ResourceUri::parse(uri).unwrap()])
            .unwrap_or_default();
        shell.replace_workspace_roots(roots, cx).unwrap();
        cx.notify();
    });
    cx.run_until_parked();
}

/// Waits until the window shows a root; switches finish on the provider runtime.
fn wait_for_root(shell: &Entity<ProductShell>, uri: &str, cx: &mut TestAppContext) {
    wait_for_api_condition(cx, |cx| {
        shell
            .read(cx)
            .workspace
            .snapshot()
            .root()
            .is_some_and(|root| root.to_string() == uri)
    });
    cx.run_until_parked();
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

/// Text of the document tabs in a window, in layout order.
fn texts(shell: &Entity<ProductShell>, cx: &TestAppContext) -> Vec<String> {
    cx.read(|cx| {
        let documents = cx.global::<ApplicationDocuments>().0.read(cx);
        shell
            .read(cx)
            .workbench
            .read(cx)
            .tab_snapshots()
            .into_iter()
            .filter_map(|tab| match tab.surface {
                TabSurfaceId::Document(document) => documents
                    .get(document)
                    .map(|document| document.model().read(cx).text()),
                _ => None,
            })
            .collect()
    })
}

/// Append to the active document of a window.
fn type_text(shell: &Entity<ProductShell>, text: &str, cx: &mut TestAppContext) {
    let model = cx.read(|cx| {
        let workbench = shell.read(cx).workbench.read(cx);
        let pane = workbench.normal_pane().unwrap();
        let tab = workbench.pane(pane).unwrap().active_tab();
        tab.editor().unwrap().read(cx).model().clone()
    });
    model.update(cx, |model, cx| {
        let len = model.text().len();
        model.replace(len..len, text).unwrap();
        cx.notify();
    });
    cx.run_until_parked();
}

fn new_untitled(
    shell: &Entity<ProductShell>,
    window: gpui::AnyWindowHandle,
    text: &str,
    cx: &mut TestAppContext,
) {
    cx.update_window(window, |_, native, cx| {
        shell.update(cx, |shell, cx| {
            let pane = shell.workbench.read(cx).normal_pane().unwrap();
            shell.new_document_in_pane(pane, native, cx);
        });
    })
    .unwrap();
    type_text(shell, text, cx);
}

fn retained(cx: &mut TestAppContext) -> Vec<StoredSnapshot> {
    let listing = cx.read(|cx| {
        cx.global::<ApplicationRecovery>()
            .0
            .store()
            .retained(OWNER.into())
    });
    let result = std::rc::Rc::new(RefCell::new(None));
    let sink = result.clone();
    cx.spawn(async move |_| *sink.borrow_mut() = Some(listing.await))
        .detach();
    cx.run_until_parked();
    result.borrow_mut().take().unwrap().unwrap()
}

fn project_id(listed: &Value, name: &str) -> String {
    listed["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|project| project["name"] == name)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn captures(session: &Value) -> Vec<String> {
    session["windows"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|window| window["entries"].as_array().unwrap())
        .filter_map(|entry| entry["capture"].as_str().map(str::to_owned))
        .collect()
}

fn select_project(fixture: &Fixture, id: &str, cx: &mut TestAppContext) {
    let tree = wait_for_contributed_view(&fixture.shell, "builtin.projects", cx);
    wait_for_api_condition(cx, |cx| tree.read(cx).status_for_test(id).is_some());
    activate_tree_tab(&fixture.shell, &tree, fixture.window, cx);
    cx.update_window(fixture.window, |_, window, cx| {
        tree.update(cx, |tree, cx| {
            tree.activate_row_for_test(id, window.window_handle(), cx)
        });
    })
    .unwrap();
    cx.run_until_parked();
}

fn product_shells(cx: &mut TestAppContext) -> Vec<(gpui::AnyWindowHandle, Entity<ProductShell>)> {
    let windows = cx.read(|cx| cx.windows());
    windows
        .into_iter()
        .filter_map(|window| {
            cx.update_window(window, |root, _, _| root.downcast::<ProductShell>().ok())
                .ok()
                .flatten()
                .map(|shell| (window, shell))
        })
        .collect()
}

#[gpui::test]
fn switching_suspends_and_restores_unsaved_documents_across_windows(cx: &mut TestAppContext) {
    let fixture = start_registry(cx);
    let a = project_dir("one.txt", "one");
    std::fs::write(a.path().join("two.txt"), "two").unwrap();
    let b = project_dir("b.txt", "bee");
    create_project(&fixture.shell, fixture.window, a.path(), "A", cx);
    assert_eq!(root(&fixture.shell, cx), Some(file_uri(a.path())));
    open_path(&fixture.shell, &a.path().join("one.txt"), cx);
    type_text(&fixture.shell, " edited", cx);
    new_untitled(&fixture.shell, fixture.window, "scratch", cx);
    // A second window on the same root joins the active project.
    let (second, second_window) = empty_product_window(&fixture.documents, cx);
    set_root(&second, Some(&file_uri(a.path())), cx);
    open_path(&second, &a.path().join("two.txt"), cx);

    create_project(&fixture.shell, fixture.window, b.path(), "B", cx);
    assert_eq!(root(&fixture.shell, cx), Some(file_uri(b.path())));
    assert!(!cx.read(|cx| cx.windows().contains(&second_window)));
    // A project without a session opens with an untitled document in the
    // normal group, never only its docks.
    assert_eq!(texts(&fixture.shell, cx), [""]);
    cx.read(|cx| {
        assert!(
            fixture
                .shell
                .read(cx)
                .workbench
                .read(cx)
                .normal_pane()
                .is_some()
        )
    });
    let listed = listed_projects(&fixture.shell, fixture.window, cx);
    let (a_id, b_id) = (project_id(&listed, "A"), project_id(&listed, "B"));
    let suspended = &listed["sessions"][&a_id];
    assert_eq!(suspended["state"], "suspended");
    assert_eq!(suspended["windows"].as_array().unwrap().len(), 2);
    assert_eq!(
        suspended["windows"][1]["entries"],
        serde_json::json!([{ "uri": file_uri(&a.path().join("two.txt")) }])
    );
    let kept = captures(suspended);
    assert_eq!(kept.len(), 2);
    let mut texts_kept = retained(cx)
        .into_iter()
        .map(|snapshot| snapshot.text)
        .collect::<Vec<_>>();
    texts_kept.sort();
    assert_eq!(texts_kept, ["one edited", "scratch"]);
    assert_eq!(listed["sessions"][&b_id]["state"], "active");

    // Back to A: both windows return with their documents and dirty text.
    select_project(&fixture, &a_id, cx);
    wait_for_root(&fixture.shell, &file_uri(a.path()), cx);
    assert_eq!(root(&fixture.shell, cx), Some(file_uri(a.path())));
    assert_eq!(texts(&fixture.shell, cx), ["one edited", "scratch"]);
    let shells = product_shells(cx);
    assert_eq!(shells.len(), 2);
    let created = shells
        .iter()
        .find(|(window, _)| *window != fixture.window)
        .unwrap();
    assert_eq!(root(&created.1, cx), Some(file_uri(a.path())));
    assert_eq!(texts(&created.1, cx), ["two"]);
    let listed = listed_projects(&fixture.shell, fixture.window, cx);
    assert_eq!(listed["sessions"][&a_id]["state"], "active");
    assert!(captures(&listed["sessions"][&a_id]).is_empty());
    assert_eq!(listed["sessions"][&b_id]["state"], "suspended");
    // Superseded captures are released once A's active record is durable.
    assert!(retained(cx).is_empty());
    assert!(!cx.has_pending_prompt());
}

#[gpui::test]
fn a_switch_racing_an_edit_retries_with_fresh_captures(cx: &mut TestAppContext) {
    let fixture = start_registry(cx);
    let a = project_dir("one.txt", "one");
    let b = project_dir("b.txt", "bee");
    create_project(&fixture.shell, fixture.window, a.path(), "A", cx);
    open_path(&fixture.shell, &a.path().join("one.txt"), cx);
    type_text(&fixture.shell, " first", cx);
    create_project(&fixture.shell, fixture.window, b.path(), "B", cx);
    open_path(&fixture.shell, &b.path().join("b.txt"), cx);
    type_text(&fixture.shell, " typed", cx);
    let listed = listed_projects(&fixture.shell, fixture.window, cx);
    let a_id = project_id(&listed, "A");

    // Hold the destination's file read so an edit lands between retain and commit.
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap(),
    );
    let (release, gate) = tokio::sync::oneshot::channel();
    let mut providers = FileSystemProviderRegistry::new();
    providers
        .register(
            "file",
            Arc::new(GatedFileSystemProvider {
                inner: Arc::new(
                    crate::app::filesystem::LocalFileSystemProvider::unrestricted(runtime),
                ),
                read_release: Mutex::new(Some(gate)),
                replace_release: Mutex::new(None),
            }),
        )
        .unwrap();
    cx.set_global(ApplicationFileSystems(Arc::new(providers)));
    select_project(&fixture, &a_id, cx);
    wait_for_api_condition(cx, |_| true);
    let mut first_attempt = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while !first_attempt
        .iter()
        .any(|snapshot: &StoredSnapshot| snapshot.text == "bee typed")
    {
        assert!(std::time::Instant::now() < deadline, "B was never retained");
        std::thread::yield_now();
        first_attempt = retained(cx);
    }
    type_text(&fixture.shell, " racing", cx);
    release.send(()).unwrap();
    wait_for_root(&fixture.shell, &file_uri(a.path()), cx);
    assert!(!cx.has_pending_prompt());
    assert_eq!(texts(&fixture.shell, cx), ["one first"]);
    let listed = listed_projects(&fixture.shell, fixture.window, cx);
    let b_id = project_id(&listed, "B");
    let kept = captures(&listed["sessions"][&b_id]);
    assert_eq!(kept.len(), 1);
    // The racing text is retained; the first attempt's capture was released.
    let snapshots = retained(cx);
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].capture_key, kept[0]);
    assert_eq!(snapshots[0].text, "bee typed racing");
}

#[gpui::test]
fn a_suspended_root_opened_manually_is_never_bound_or_overwritten(cx: &mut TestAppContext) {
    let fixture = start_registry(cx);
    let a = project_dir("one.txt", "one");
    let b = project_dir("b.txt", "bee");
    create_project(&fixture.shell, fixture.window, a.path(), "A", cx);
    open_path(&fixture.shell, &a.path().join("one.txt"), cx);
    type_text(&fixture.shell, " kept", cx);
    create_project(&fixture.shell, fixture.window, b.path(), "B", cx);
    let listed = listed_projects(&fixture.shell, fixture.window, cx);
    let a_id = project_id(&listed, "A");
    let suspended = listed["sessions"][&a_id].clone();

    // Opening the suspended root by hand does not bind the window, and edits
    // there never reach A's record.
    let (manual, manual_window) = empty_product_window(&fixture.documents, cx);
    set_root(&manual, Some(&file_uri(a.path())), cx);
    open_path(&manual, &a.path().join("one.txt"), cx);
    type_text(&manual, " elsewhere", cx);
    cx.update_window(manual_window, |_, native, cx| {
        manual.update(cx, |shell, cx| {
            shell.new_document_in_pane(shell.workbench.read(cx).normal_pane().unwrap(), native, cx)
        });
    })
    .unwrap();
    cx.run_until_parked();
    let listed = listed_projects(&fixture.shell, fixture.window, cx);
    assert_eq!(listed["sessions"][&a_id], suspended);
    assert_eq!(captures(&suspended).len(), 1);
    assert_eq!(retained(cx).len(), 1);
}

#[gpui::test]
fn closing_the_last_window_suspends_without_captures(cx: &mut TestAppContext) {
    let fixture = start_registry(cx);
    let a = project_dir("one.txt", "one");
    std::fs::write(a.path().join("two.txt"), "two").unwrap();
    create_project(&fixture.shell, fixture.window, a.path(), "A", cx);
    let (second, second_window) = empty_product_window(&fixture.documents, cx);
    set_root(&second, Some(&file_uri(a.path())), cx);
    open_path(&fixture.shell, &a.path().join("one.txt"), cx);
    open_path(&second, &a.path().join("two.txt"), cx);
    let listed = listed_projects(&fixture.shell, fixture.window, cx);
    let a_id = project_id(&listed, "A");
    assert_eq!(
        listed["sessions"][&a_id]["windows"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    // Closing a window that is not the last removes it from the session.
    let close = dispatch_product_command(&second, second_window, CLOSE_WINDOW_COMMAND, cx);
    assert_eq!(wait_for_command(close, cx), CommandOutcome::Completed);
    let listed = listed_projects(&fixture.shell, fixture.window, cx);
    assert_eq!(listed["sessions"][&a_id]["state"], "active");
    assert_eq!(
        listed["sessions"][&a_id]["windows"],
        serde_json::json!([{ "entries": [{ "uri": file_uri(&a.path().join("one.txt")) }], "selected": 0 }])
    );

    // Closing the last one suspends the project with its last membership.
    let (other, other_window) = empty_product_window(&fixture.documents, cx);
    type_text(&fixture.shell, " dirty", cx);
    let close = dispatch_product_command(&fixture.shell, fixture.window, CLOSE_WINDOW_COMMAND, cx);
    wait_for_prompt(cx);
    cx.simulate_prompt_answer("Don't Save");
    assert_eq!(wait_for_command(close, cx), CommandOutcome::Completed);
    let listed = listed_projects(&other, other_window, cx);
    let session = &listed["sessions"][&a_id];
    assert_eq!(session["state"], "suspended");
    assert_eq!(
        session["windows"][0]["entries"],
        serde_json::json!([{ "uri": file_uri(&a.path().join("one.txt")) }])
    );
    assert!(captures(session).is_empty());
    assert!(retained(cx).is_empty());
}

#[gpui::test]
fn startup_suspends_active_sessions_without_a_window(cx: &mut TestAppContext) {
    let fixture = start_registry(cx);
    let a = project_dir("one.txt", "one");
    create_project(&fixture.shell, fixture.window, a.path(), "A", cx);
    open_path(&fixture.shell, &a.path().join("one.txt"), cx);
    let listed = listed_projects(&fixture.shell, fixture.window, cx);
    let a_id = project_id(&listed, "A");
    assert_eq!(listed["sessions"][&a_id]["state"], "active");

    // The root changes while the package is not running, as across a restart.
    cx.update(|cx| {
        fixture.shell.update(cx, |shell, cx| {
            shell.replace_workspace_roots(Vec::new(), cx).unwrap();
        });
        fixture.host.update(cx, |host, cx| {
            host.start_product_extensions(fixture.extensions.path().to_path_buf(), cx)
        });
    });
    let _ = wait_for_extension_report(&fixture.host, cx);
    let listed = listed_projects(&fixture.shell, fixture.window, cx);
    let session = &listed["sessions"][&a_id];
    assert_eq!(session["state"], "suspended");
    assert_eq!(
        session["windows"][0]["entries"],
        serde_json::json!([{ "uri": file_uri(&a.path().join("one.txt")) }])
    );
}

#[gpui::test]
fn removal_reports_and_releases_suspended_text_only_after_the_deletes(cx: &mut TestAppContext) {
    let fixture = start_registry(cx);
    let a = project_dir("one.txt", "one");
    let b = project_dir("b.txt", "bee");
    create_project(&fixture.shell, fixture.window, a.path(), "A", cx);
    open_path(&fixture.shell, &a.path().join("one.txt"), cx);
    type_text(&fixture.shell, " unsaved", cx);
    new_untitled(&fixture.shell, fixture.window, "scratch", cx);
    create_project(&fixture.shell, fixture.window, b.path(), "B", cx);
    let listed = listed_projects(&fixture.shell, fixture.window, cx);
    let (a_id, b_id) = (project_id(&listed, "A"), project_id(&listed, "B"));
    assert_eq!(retained(cx).len(), 2);

    let remove = |id: &str, cx: &mut TestAppContext| {
        dispatch_product_command_with(
            &fixture.shell,
            fixture.window,
            "projects.remove",
            CommandArgumentValue::Object(
                [("id".to_owned(), CommandArgumentValue::String(id.into()))].into(),
            ),
            cx,
        )
    };
    let removal = remove(&a_id, cx);
    wait_for_prompt(cx);
    let (_, message) = cx.pending_prompt().unwrap();
    assert!(message.contains("2 unsaved documents in its suspended session will be discarded"));
    // Cancelling keeps the record and its captures.
    cx.simulate_prompt_answer("Cancel");
    assert_eq!(wait_for_command(removal, cx), CommandOutcome::Completed);
    assert_eq!(retained(cx).len(), 2);

    let removal = remove(&a_id, cx);
    wait_for_prompt(cx);
    cx.simulate_prompt_answer("Remove");
    assert_eq!(wait_for_command(removal, cx), CommandOutcome::Completed);
    assert!(retained(cx).is_empty());
    let listed = listed_projects(&fixture.shell, fixture.window, cx);
    assert!(listed["sessions"].get(&a_id).is_none());

    // Removing the active project says nothing about unsaved text and leaves
    // its window as it is.
    let removal = remove(&b_id, cx);
    wait_for_prompt(cx);
    let (_, message) = cx.pending_prompt().unwrap();
    assert_eq!(message, "Open documents and folders stay open.");
    cx.simulate_prompt_answer("Remove");
    assert_eq!(wait_for_command(removal, cx), CommandOutcome::Completed);
    assert_eq!(root(&fixture.shell, cx), Some(file_uri(b.path())));
    let listed = listed_projects(&fixture.shell, fixture.window, cx);
    assert_eq!(listed["projects"].as_array().unwrap().len(), 0);
}

#[gpui::test]
fn activation_releases_captures_no_session_references(cx: &mut TestAppContext) {
    let fixture = start_registry(cx);
    let document = cx.read(|cx| {
        fixture
            .documents
            .read(cx)
            .documents()
            .next()
            .unwrap()
            .identity()
    });
    let orphan = cx
        .update(|cx| crate::app::recovery::retain(OWNER.into(), document, cx))
        .unwrap();
    let result = std::rc::Rc::new(RefCell::new(None));
    let sink = result.clone();
    cx.spawn(async move |_| *sink.borrow_mut() = Some(orphan.await))
        .detach();
    cx.run_until_parked();
    assert!(result.borrow_mut().take().unwrap().is_ok());
    assert_eq!(retained(cx).len(), 1);
    restart_registry(&fixture.host, &fixture.extensions, cx);
    assert!(retained(cx).is_empty());
}

#[gpui::test]
fn quit_records_no_window_closure(cx: &mut TestAppContext) {
    let fixture = start_registry(cx);
    let a = project_dir("one.txt", "one");
    create_project(&fixture.shell, fixture.window, a.path(), "A", cx);
    open_path(&fixture.shell, &a.path().join("one.txt"), cx);
    let listed = listed_projects(&fixture.shell, fixture.window, cx);
    let a_id = project_id(&listed, "A");
    let before = listed["sessions"][&a_id].clone();
    assert_eq!(before["state"], "active");

    fixture
        .host
        .update(cx, |host, _| host.take_test_application_events());
    let quit = dispatch_product_command(&fixture.shell, fixture.window, QUIT_COMMAND, cx);
    assert_eq!(wait_for_command(quit, cx), CommandOutcome::Completed);
    cx.run_until_parked();
    let events = fixture
        .host
        .update(cx, |host, _| host.take_test_application_events());
    assert!(
        !events
            .iter()
            .any(|event| serde_json::to_value(&event.change).unwrap()["type"] == "windowClosed")
    );
    // The durable record still describes the window as it was at quit.
    let storage = crate::app::extension_storage::ExtensionStorage::new(
        fixture.storage.path().to_owned(),
        crate::app::extension_storage::test_executor(),
    );
    let stored = pollster::block_on(
        storage
            .for_owner(crate::app::extension_storage::StorageOwner::bundled(OWNER).unwrap())
            .read(format!("session:{a_id}")),
    )
    .unwrap();
    assert_eq!(stored.map(normalized), Some(normalized(before)));
    pollster::block_on(storage.shutdown());
}

#[gpui::test]
fn a_partial_restore_commits_and_reports_what_was_skipped(cx: &mut TestAppContext) {
    let fixture = start_registry(cx);
    let a = project_dir("one.txt", "one");
    std::fs::write(a.path().join("gone.txt"), "soon gone").unwrap();
    let b = project_dir("b.txt", "bee");
    create_project(&fixture.shell, fixture.window, a.path(), "A", cx);
    open_path(&fixture.shell, &a.path().join("one.txt"), cx);
    open_path(&fixture.shell, &a.path().join("gone.txt"), cx);
    create_project(&fixture.shell, fixture.window, b.path(), "B", cx);
    let a_id = project_id(&listed_projects(&fixture.shell, fixture.window, cx), "A");
    std::fs::remove_file(a.path().join("gone.txt")).unwrap();

    select_project(&fixture, &a_id, cx);
    wait_for_prompt(cx);
    let (_, message) = cx.pending_prompt().unwrap();
    assert_eq!(message, "Opened A. gone.txt no longer exists.");
    cx.simulate_prompt_answer("OK");
    cx.run_until_parked();
    assert_eq!(root(&fixture.shell, cx), Some(file_uri(a.path())));
    assert_eq!(texts(&fixture.shell, cx), ["one"]);
}

#[gpui::test]
fn restarting_the_registry_keeps_bound_sessions_active(cx: &mut TestAppContext) {
    let fixture = start_registry(cx);
    let a = project_dir("one.txt", "one");
    create_project(&fixture.shell, fixture.window, a.path(), "A", cx);
    let a_id = project_id(&listed_projects(&fixture.shell, fixture.window, cx), "A");

    let restart = dispatch_product_command(
        &fixture.shell,
        fixture.window,
        super::super::commands::RESTART_EXTENSION_COMMAND,
        cx,
    );
    assert_eq!(wait_for_command(restart, cx), CommandOutcome::Completed);
    wait_for_api_condition(cx, |cx| fixture.shell.read(cx).transient.is_some());
    cx.simulate_input(fixture.window, OWNER);
    cx.simulate_keystrokes(fixture.window, "enter");
    wait_for_prompt(cx);
    assert_eq!(cx.pending_prompt().unwrap().0, "Restarted @knot/projects");
    cx.simulate_prompt_answer("OK");
    cx.run_until_parked();

    let listed = listed_projects(&fixture.shell, fixture.window, cx);
    assert_eq!(listed["sessions"][&a_id]["state"], "active");
    assert_eq!(root(&fixture.shell, cx), Some(file_uri(a.path())));
}
