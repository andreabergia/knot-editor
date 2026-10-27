//! `@knot/projects` sessions: switching, tracking, suspension, and removal.

use super::operations::empty_product_window;
use super::projects::{
    Fixture, activate_tree_tab, listed_projects, restart_registry, start_registry, wait_for_prompt,
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
        shell.replace_workspace_roots(roots, cx);
        cx.notify();
    });
    cx.run_until_parked();
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

impl Fixture {
    /// Wait until the focused window shows exactly these roots, then follow it.
    fn follow_roots(&mut self, expected: &[&str], cx: &mut TestAppContext) {
        wait_for_api_condition(cx, |cx| {
            cx.active_window()
                .and_then(|window| window.downcast::<ProductShell>())
                .and_then(|window| window.read(cx).ok())
                .is_some_and(|shell| {
                    shell
                        .workspace
                        .snapshot()
                        .roots()
                        .iter()
                        .map(|root| root.uri.to_string())
                        .eq(expected.iter().map(|uri| uri.to_string()))
                })
        });
        cx.run_until_parked();
        self.follow(cx);
    }
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
    let mut fixture = start_registry(cx);
    let a = project_dir("one.txt", "one");
    std::fs::write(a.path().join("two.txt"), "two").unwrap();
    let b = project_dir("b.txt", "bee");
    fixture.create_project(a.path(), "A", cx);
    assert_eq!(root(&fixture.shell, cx), Some(file_uri(a.path())));
    open_path(&fixture.shell, &a.path().join("one.txt"), cx);
    type_text(&fixture.shell, " edited", cx);
    new_untitled(&fixture.shell, fixture.window, "scratch", cx);
    // A second window on the same root joins the active project.
    let (second, second_window) = empty_product_window(&fixture.documents, cx);
    set_root(&second, Some(&file_uri(a.path())), cx);
    open_path(&second, &a.path().join("two.txt"), cx);

    fixture.create_project(b.path(), "B", cx);
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
    fixture.follow_roots(&[&file_uri(a.path())], cx);
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
    let mut fixture = start_registry(cx);
    let a = project_dir("one.txt", "one");
    let b = project_dir("b.txt", "bee");
    fixture.create_project(a.path(), "A", cx);
    open_path(&fixture.shell, &a.path().join("one.txt"), cx);
    type_text(&fixture.shell, " first", cx);
    fixture.create_project(b.path(), "B", cx);
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
    fixture.follow_roots(&[&file_uri(a.path())], cx);
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
    let mut fixture = start_registry(cx);
    let a = project_dir("one.txt", "one");
    let b = project_dir("b.txt", "bee");
    fixture.create_project(a.path(), "A", cx);
    open_path(&fixture.shell, &a.path().join("one.txt"), cx);
    type_text(&fixture.shell, " kept", cx);
    fixture.create_project(b.path(), "B", cx);
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
    let mut fixture = start_registry(cx);
    let a = project_dir("one.txt", "one");
    std::fs::write(a.path().join("two.txt"), "two").unwrap();
    fixture.create_project(a.path(), "A", cx);
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
    let windows = &listed["sessions"][&a_id]["windows"];
    assert_eq!(windows.as_array().unwrap().len(), 1);
    assert_eq!(
        windows[0]["entries"],
        serde_json::json!([{ "uri": file_uri(&a.path().join("one.txt")) }])
    );
    assert_eq!(windows[0]["selected"], 0);
    // Each session window keeps its captured layout.
    assert_eq!(windows[0]["layout"]["version"], 1);

    // Closing the last one suspends the project with its last membership,
    // which records the dirty document's identity once tracking sees it.
    let (other, other_window) = empty_product_window(&fixture.documents, cx);
    type_text(&fixture.shell, " dirty", cx);
    let one = file_uri(&a.path().join("one.txt"));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let dirty = loop {
        let listed = listed_projects(&other, other_window, cx);
        let entry = listed["sessions"][&a_id]["windows"][0]["entries"][0].clone();
        if entry["document"].is_string() {
            break entry;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "tracking never recorded the edit"
        );
    };
    assert_eq!(dirty["uri"], one);
    let close = dispatch_product_command(&fixture.shell, fixture.window, CLOSE_WINDOW_COMMAND, cx);
    wait_for_prompt(cx);
    cx.simulate_prompt_answer("Don't Save");
    assert_eq!(wait_for_command(close, cx), CommandOutcome::Completed);
    let listed = listed_projects(&other, other_window, cx);
    let session = &listed["sessions"][&a_id];
    assert_eq!(session["state"], "suspended");
    assert_eq!(session["windows"][0]["entries"], serde_json::json!([dirty]));
    assert!(captures(session).is_empty());
    assert!(retained(cx).is_empty());
}

#[gpui::test]
fn startup_suspends_active_sessions_without_a_window(cx: &mut TestAppContext) {
    let mut fixture = start_registry(cx);
    let a = project_dir("one.txt", "one");
    fixture.create_project(a.path(), "A", cx);
    open_path(&fixture.shell, &a.path().join("one.txt"), cx);
    let listed = listed_projects(&fixture.shell, fixture.window, cx);
    let a_id = project_id(&listed, "A");
    assert_eq!(listed["sessions"][&a_id]["state"], "active");

    // The root changes while the package is not running, as across a restart.
    cx.update(|cx| {
        fixture.shell.update(cx, |shell, cx| {
            shell.replace_workspace_roots(Vec::new(), cx);
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
fn quit_records_no_window_closure(cx: &mut TestAppContext) {
    let mut fixture = start_registry(cx);
    let a = project_dir("one.txt", "one");
    fixture.create_project(a.path(), "A", cx);
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
    let mut fixture = start_registry(cx);
    let a = project_dir("one.txt", "one");
    std::fs::write(a.path().join("gone.txt"), "soon gone").unwrap();
    let b = project_dir("b.txt", "bee");
    fixture.create_project(a.path(), "A", cx);
    open_path(&fixture.shell, &a.path().join("one.txt"), cx);
    open_path(&fixture.shell, &a.path().join("gone.txt"), cx);
    fixture.create_project(b.path(), "B", cx);
    let a_id = project_id(&listed_projects(&fixture.shell, fixture.window, cx), "A");
    std::fs::remove_file(a.path().join("gone.txt")).unwrap();

    select_project(&fixture, &a_id, cx);
    wait_for_prompt(cx);
    let (_, message) = cx.pending_prompt().unwrap();
    assert_eq!(message, "Opened A. gone.txt no longer exists.");
    cx.simulate_prompt_answer("OK");
    fixture.follow_roots(&[&file_uri(a.path())], cx);
    assert_eq!(texts(&fixture.shell, cx), ["one"]);
}

#[gpui::test]
fn restarting_the_registry_keeps_bound_sessions_active(cx: &mut TestAppContext) {
    let mut fixture = start_registry(cx);
    let a = project_dir("one.txt", "one");
    fixture.create_project(a.path(), "A", cx);
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

fn set_roots(shell: &Entity<ProductShell>, uris: &[&str], cx: &mut TestAppContext) {
    shell.update(cx, |shell, cx| {
        let roots = uris
            .iter()
            .map(|uri| ResourceUri::parse(uri).unwrap())
            .collect();
        shell.replace_workspace_roots(roots, cx);
        cx.notify();
    });
    cx.run_until_parked();
}

fn wait_for_roots(shell: &Entity<ProductShell>, expected: &[&str], cx: &mut TestAppContext) {
    wait_for_api_condition(cx, |cx| {
        shell
            .read(cx)
            .workspace
            .snapshot()
            .roots()
            .iter()
            .map(|root| root.uri.to_string())
            .eq(expected.iter().map(|uri| uri.to_string()))
    });
    cx.run_until_parked();
}

fn session_windows(fixture: &Fixture, id: &str, cx: &mut TestAppContext) -> usize {
    listed_projects(&fixture.shell, fixture.window, cx)["sessions"][id]["windows"]
        .as_array()
        .unwrap()
        .len()
}

fn project_roots(listed: &Value, id: &str) -> Vec<String> {
    listed["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|project| project["id"] == id)
        .unwrap()["roots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|root| root.as_str().unwrap().to_owned())
        .collect()
}

#[gpui::test]
fn root_edits_in_a_bound_window_move_the_project_and_its_other_windows(cx: &mut TestAppContext) {
    let mut fixture = start_registry(cx);
    let a = project_dir("one.txt", "one");
    let c = project_dir("c.txt", "see");
    let (a_uri, c_uri) = (file_uri(a.path()), file_uri(c.path()));
    fixture.create_project(a.path(), "A", cx);
    let a_id = project_id(&listed_projects(&fixture.shell, fixture.window, cx), "A");
    let (second, _) = empty_product_window(&fixture.documents, cx);
    set_root(&second, Some(&a_uri), cx);
    assert_eq!(session_windows(&fixture, &a_id, cx), 2);

    // Adding a folder in one window updates the record and the other window.
    set_roots(&fixture.shell, &[&a_uri, &c_uri], cx);
    wait_for_roots(&second, &[&a_uri, &c_uri], cx);
    let listed = listed_projects(&fixture.shell, fixture.window, cx);
    assert_eq!(
        project_roots(&listed, &a_id),
        [a_uri.clone(), c_uri.clone()]
    );
    let tree = wait_for_contributed_view(&fixture.shell, "builtin.projects", cx);
    wait_for_api_condition(cx, |cx| {
        tree.read(cx).description_for_test(&a_id).as_deref()
            == Some(&*format!(
                "{} +1",
                a.path().file_name().unwrap().to_str().unwrap()
            ))
    });

    // Membership ignores order: a window with the roots reversed joins.
    let (third, _) = empty_product_window(&fixture.documents, cx);
    set_roots(&third, &[&c_uri, &a_uri], cx);
    assert_eq!(session_windows(&fixture, &a_id, cx), 3);
    let listed = listed_projects(&fixture.shell, fixture.window, cx);
    assert_eq!(
        project_roots(&listed, &a_id),
        [a_uri.clone(), c_uri.clone()]
    );

    // Removing a folder in another window follows back the same way.
    set_roots(&second, &[&c_uri], cx);
    fixture.follow_roots(&[&c_uri], cx);
    wait_for_roots(&third, &[&c_uri], cx);
    let listed = listed_projects(&fixture.shell, fixture.window, cx);
    assert_eq!(project_roots(&listed, &a_id), std::slice::from_ref(&c_uri));
    assert_eq!(listed["sessions"][&a_id]["state"], "active");
    assert!(!cx.has_pending_prompt());
}

#[gpui::test]
fn a_multi_root_project_restores_every_root_across_switches_and_restarts(cx: &mut TestAppContext) {
    let mut fixture = start_registry(cx);
    let a = project_dir("one.txt", "one");
    let c = project_dir("c.txt", "see");
    let b = project_dir("b.txt", "bee");
    let (a_uri, c_uri) = (file_uri(a.path()), file_uri(c.path()));
    fixture.create_project(a.path(), "A", cx);
    set_roots(&fixture.shell, &[&a_uri, &c_uri], cx);
    open_path(&fixture.shell, &c.path().join("c.txt"), cx);
    type_text(&fixture.shell, " dirty", cx);
    let a_id = project_id(&listed_projects(&fixture.shell, fixture.window, cx), "A");

    fixture.create_project(b.path(), "B", cx);
    select_project(&fixture, &a_id, cx);
    fixture.follow_roots(&[&a_uri, &c_uri], cx);
    assert_eq!(texts(&fixture.shell, cx), ["see dirty"]);

    // Suspended with its text, A survives a restart and restores both roots.
    let b_id = project_id(&listed_projects(&fixture.shell, fixture.window, cx), "B");
    select_project(&fixture, &b_id, cx);
    fixture.follow_roots(&[&file_uri(b.path())], cx);
    // Listing runs after the switch finishes recording, so the restart cannot
    // interrupt it.
    assert_eq!(
        listed_projects(&fixture.shell, fixture.window, cx)["sessions"][&b_id]["state"],
        "active"
    );
    restart_registry(&fixture.host, &fixture.extensions, cx);
    let listed = listed_projects(&fixture.shell, fixture.window, cx);
    assert_eq!(listed["sessions"][&a_id]["state"], "suspended");
    assert_eq!(captures(&listed["sessions"][&a_id]).len(), 1);
    assert_eq!(retained(cx).len(), 1);
    select_project(&fixture, &a_id, cx);
    fixture.follow_roots(&[&a_uri, &c_uri], cx);
    assert_eq!(texts(&fixture.shell, cx), ["see dirty"]);
    assert!(!cx.has_pending_prompt());
}

#[gpui::test]
fn a_new_project_window_opens_with_the_project_roots_and_joins_it(cx: &mut TestAppContext) {
    let mut fixture = start_registry(cx);
    let a = project_dir("one.txt", "one");
    let c = project_dir("c.txt", "see");
    let (a_uri, c_uri) = (file_uri(a.path()), file_uri(c.path()));
    fixture.create_project(a.path(), "A", cx);
    set_roots(&fixture.shell, &[&a_uri, &c_uri], cx);
    let a_id = project_id(&listed_projects(&fixture.shell, fixture.window, cx), "A");

    let opened =
        dispatch_product_command(&fixture.shell, fixture.window, "projects.new-window", cx);
    assert_eq!(wait_for_command(opened, cx), CommandOutcome::Completed);
    let shells = product_shells(cx);
    assert_eq!(shells.len(), 2);
    let (_, created) = shells
        .iter()
        .find(|(window, _)| *window != fixture.window)
        .unwrap();
    wait_for_roots(created, &[&a_uri, &c_uri], cx);
    assert_eq!(session_windows(&fixture, &a_id, cx), 2);

    // A window outside every project has no project to open.
    let (blank, blank_window) = empty_product_window(&fixture.documents, cx);
    let refused = dispatch_product_command(&blank, blank_window, "projects.new-window", cx);
    wait_for_prompt(cx);
    cx.simulate_prompt_answer("OK");
    assert_eq!(wait_for_command(refused, cx), CommandOutcome::Completed);
    assert_eq!(product_shells(cx).len(), 3);
}

fn package_storage(fixture: &Fixture) -> crate::app::extension_storage::ExtensionStorage {
    crate::app::extension_storage::ExtensionStorage::new(
        fixture.storage.path().to_owned(),
        crate::app::extension_storage::test_executor(),
    )
}

fn write_record(fixture: &Fixture, key: &str, value: Value) {
    let storage = package_storage(fixture);
    pollster::block_on(
        storage
            .for_owner(crate::app::extension_storage::StorageOwner::bundled(OWNER).unwrap())
            .write(key.to_owned(), value),
    )
    .unwrap();
    pollster::block_on(storage.shutdown());
}

/// A window's arrangement without document identities, which restored
/// captures and reopened files replace.
fn arrangement(window: gpui::AnyWindowHandle, cx: &mut TestAppContext) -> Value {
    fn forget(node: &mut Value) {
        for tab in node["tabs"].as_array_mut().into_iter().flatten() {
            tab["document"] = Value::Null;
        }
        for child in ["first", "second"] {
            if node.get(child).is_some() {
                forget(&mut node[child]);
            }
        }
    }
    let target = crate::host::protocol::WindowTarget {
        window: window.window_id().as_u64().to_string(),
    };
    let layout = cx
        .update(|cx| super::super::sessions::capture_layout(&target, cx))
        .unwrap();
    let mut layout = serde_json::to_value(layout).unwrap();
    forget(&mut layout["root"]);
    serde_json::json!({ "root": layout["root"], "focused": layout["focused"], "dockSizes": layout["dockSizes"] })
}

fn split_with_view_state(
    shell: &Entity<ProductShell>,
    state: crate::app::editor::EditorViewState,
    cx: &mut TestAppContext,
) {
    shell.update(cx, |shell, cx| {
        let pane = shell.workbench.read(cx).normal_pane().unwrap();
        shell.workbench.update(cx, |workbench, cx| {
            workbench.split_pane(
                pane,
                crate::app::workbench::SplitDirection::Horizontal,
                crate::app::workbench::SplitPlacement::After,
                cx,
            );
            workbench.set_dock_size(crate::app::workbench::DockLocation::Left, 300.);
        });
    });
    let editor = cx.read(|cx| {
        shell
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab()
            .editor()
            .unwrap()
            .clone()
    });
    editor.update(cx, |editor, cx| editor.restore_view_state(state, cx));
    cx.run_until_parked();
}

fn document_count(fixture: &Fixture, cx: &TestAppContext) -> usize {
    cx.read(|cx| fixture.documents.read(cx).documents().count())
}

#[gpui::test]
fn a_multi_root_layout_with_shared_views_survives_repeated_switches(cx: &mut TestAppContext) {
    let mut fixture = start_registry(cx);
    let a = project_dir("one.txt", &"line\n".repeat(200));
    let c = project_dir("c.txt", "see");
    let b = project_dir("b.txt", "bee");
    let (a_uri, c_uri) = (file_uri(a.path()), file_uri(c.path()));
    fixture.create_project(a.path(), "A", cx);
    set_roots(&fixture.shell, &[&a_uri, &c_uri], cx);
    open_path(&fixture.shell, &a.path().join("one.txt"), cx);
    type_text(&fixture.shell, "dirty", cx);
    let state = crate::app::editor::EditorViewState {
        anchor: 10,
        caret: 25,
        top: 20,
        scroll_x: 0.,
    };
    split_with_view_state(&fixture.shell, state, cx);
    let a_id = project_id(&listed_projects(&fixture.shell, fixture.window, cx), "A");
    fixture.create_project(b.path(), "B", cx);
    let b_id = project_id(&listed_projects(&fixture.shell, fixture.window, cx), "B");

    // The empty placeholder tab is pruned on the first return; every later
    // return and restart reproduces that arrangement exactly.
    let mut before = None;
    for round in 0..3 {
        select_project(&fixture, &a_id, cx);
        fixture.follow_roots(&[&a_uri, &c_uri], cx);
        // Selecting through the tree focuses the Projects tool before each
        // switch, so focus and the dock's selection are not compared.
        let mut current = arrangement(fixture.window, cx);
        current["focused"] = Value::Null;
        current["root"]["first"]["selected"] = Value::Null;
        assert_eq!(current["root"]["second"]["kind"], "split");
        assert_eq!(current["dockSizes"]["left"], 300.);
        assert_eq!(
            before.get_or_insert(current.clone()),
            &current,
            "round {round}"
        );
        let views = cx.read(|cx| {
            fixture
                .shell
                .read(cx)
                .workbench
                .read(cx)
                .panes()
                .iter()
                .flat_map(|pane| pane.tabs())
                .filter_map(|tab| tab.editor().cloned())
                .collect::<Vec<_>>()
        });
        cx.read(|cx| {
            let models = views
                .iter()
                .map(|view| view.read(cx).model().clone())
                .collect::<Vec<_>>();
            assert_eq!(models[models.len() - 1], models[models.len() - 2]);
            assert_eq!(views.last().unwrap().read(cx).view_state(), state);
            assert!(models[models.len() - 1].read(cx).text().ends_with("dirty"));
        });
        select_project(&fixture, &b_id, cx);
        fixture.follow_roots(&[&file_uri(b.path())], cx);
        // Switching never duplicates windows or documents.
        assert_eq!(product_shells(cx).len(), 1);
        assert!(document_count(&fixture, cx) <= 2);
    }
    // Listing waits for the last switch to finish recording before restarting.
    assert_eq!(
        listed_projects(&fixture.shell, fixture.window, cx)["sessions"][&b_id]["state"],
        "active"
    );
    restart_registry(&fixture.host, &fixture.extensions, cx);
    select_project(&fixture, &a_id, cx);
    fixture.follow_roots(&[&a_uri, &c_uri], cx);
    let mut restarted = arrangement(fixture.window, cx);
    restarted["focused"] = Value::Null;
    restarted["root"]["first"]["selected"] = Value::Null;
    assert_eq!(Some(restarted), before);
    assert_eq!(product_shells(cx).len(), 1);
}

#[gpui::test]
fn schema_one_records_switch_without_a_layout(cx: &mut TestAppContext) {
    let mut fixture = start_registry(cx);
    let a = project_dir("one.txt", "one");
    let b = project_dir("b.txt", "bee");
    fixture.create_project(a.path(), "A", cx);
    fixture.create_project(b.path(), "B", cx);
    let a_id = project_id(&listed_projects(&fixture.shell, fixture.window, cx), "A");
    write_record(
        &fixture,
        &format!("session:{a_id}"),
        serde_json::json!({
            "schema": 1, "state": "suspended",
            "windows": [{ "entries": [{ "uri": file_uri(&a.path().join("one.txt")) }], "selected": 0 }],
        }),
    );
    restart_registry(&fixture.host, &fixture.extensions, cx);
    select_project(&fixture, &a_id, cx);
    fixture.follow_roots(&[&file_uri(a.path())], cx);
    assert_eq!(texts(&fixture.shell, cx), ["one"]);
    // The record is upgraded when it is next written.
    let listed = listed_projects(&fixture.shell, fixture.window, cx);
    assert_eq!(listed["sessions"][&a_id]["schema"], 2);
    assert!(listed["sessions"][&a_id]["windows"][0]["layout"].is_object());
}

#[gpui::test]
fn restored_windows_rebind_after_launch_and_unrestored_sessions_suspend(cx: &mut TestAppContext) {
    let mut fixture = start_registry(cx);
    let sessions = tempfile::tempdir().unwrap();
    let path = sessions.path().join("window-session.sqlite3");
    let executor = cx.background_executor.clone();
    cx.update(|cx| {
        crate::app::window_sessions::install(path.clone(), executor.clone(), cx);
        crate::app::window_sessions::start_recording(cx);
    });
    let a = project_dir("one.txt", "one");
    let c = project_dir("c.txt", "see");
    fixture.create_project(a.path(), "A", cx);
    open_path(&fixture.shell, &a.path().join("one.txt"), cx);
    type_text(&fixture.shell, " dirty", cx);
    let (second, second_window) = empty_product_window(&fixture.documents, cx);
    super::projects::create_project(&second, second_window, c.path(), "C", cx);
    let listed = listed_projects(&fixture.shell, fixture.window, cx);
    let (a_id, c_id) = (project_id(&listed, "A"), project_id(&listed, "C"));
    assert_eq!(listed["sessions"][&c_id]["state"], "active");

    // Quit records only A's window, as if C's window had not been recorded.
    let drain = cx.update(|cx| crate::app::recovery::drain(cx));
    let drained = std::rc::Rc::new(RefCell::new(None));
    let sink = drained.clone();
    cx.spawn(async move |_| *sink.borrow_mut() = Some(drain.await))
        .detach();
    cx.run_until_parked();
    assert!(drained.borrow_mut().take().unwrap().is_ok());
    cx.update(crate::app::window_sessions::record);
    let mut record = cx.read(crate::app::window_sessions::current).unwrap();
    record
        .windows
        .retain(|window| window.roots == [file_uri(a.path())]);
    record.active = Some(0);
    cx.update(|cx| {
        crate::app::window_sessions::stop_for_test(cx);
        cx.global::<crate::app::window_sessions::ApplicationWindowSessions>()
            .publish_for_test(record);
    });
    let flushed = cx.read(crate::app::window_sessions::flush);
    let sink = drained.clone();
    cx.spawn(async move |_| *sink.borrow_mut() = Some(flushed.await))
        .detach();
    cx.run_until_parked();
    assert!(drained.borrow_mut().take().unwrap().is_ok());

    // A fresh application: the registry activates before any window exists.
    // Quitting stops the registry before windows close, so it never records
    // their closure.
    fixture.host.update(cx, |host, cx| {
        host.unload_package_for_test("@knot/projects", cx)
    });
    cx.run_until_parked();
    for window in cx.read(|cx| cx.windows()) {
        let _ = cx.update_window(window, |_, window, _| window.remove_window());
    }
    cx.run_until_parked();
    let documents = cx.new(|_| DocumentCollection::new());
    cx.set_global(ApplicationDocuments(documents.clone()));
    let recovery_path = cx.read(|cx| {
        cx.global::<TestRecoveryDir>()
            .0
            .path()
            .join("recovery.sqlite3")
    });
    cx.set_global(ApplicationRecovery(
        crate::app::recovery::RecoveryService::new(crate::app::recovery::RecoveryStore::open(
            recovery_path,
            executor.clone(),
        )),
    ));
    cx.set_global(ApplicationWorkbenches(RefCell::new(Vec::new())));
    cx.update(|cx| {
        crate::app::extension_host::reset_launched_for_test(cx);
        crate::app::window_sessions::install(path, executor, cx);
    });
    restart_registry(&fixture.host, &fixture.extensions, cx);
    install_launch_gate(cx);
    cx.update(|cx| super::super::finish_product_startup(Ok(()), cx));
    assert!(wait_for_product_startup(cx));
    cx.run_until_parked();

    // The restored window rebinds to A with its text; no window or document
    // was added, and C, whose window did not return, is suspended.
    let shells = product_shells(cx);
    assert_eq!(shells.len(), 1);
    (fixture.window, fixture.shell) = shells[0].clone();
    fixture.documents = documents;
    assert_eq!(texts(&fixture.shell, cx), ["one dirty"]);
    let listed = listed_projects(&fixture.shell, fixture.window, cx);
    assert_eq!(listed["sessions"][&a_id]["state"], "active");
    assert_eq!(listed["sessions"][&c_id]["state"], "suspended");
    let tree = wait_for_contributed_view(&fixture.shell, "builtin.projects", cx);
    wait_for_api_condition(cx, |cx| {
        tree.read(cx)
            .status_for_test(&a_id)
            .is_some_and(|(status, _)| status.as_deref() == Some("active"))
    });
    assert_eq!(product_shells(cx).len(), 1);
    assert_eq!(document_count(&fixture, cx), 1);
}
