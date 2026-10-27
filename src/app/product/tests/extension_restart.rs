//! Native Restart Extension: terminating a stuck package and loading it again.

use super::operations::empty_product_window;
use super::*;
use crate::app::extension_load::LoadResult;

const LOOP_V1: &str = r#"
import { commands, workbench } from "knot";
await commands.register("loop.spin", async () => { while (true) {} });
await commands.register("loop.ping", async () => { await workbench.writeClipboardText("v1"); });
await workbench.registerView({ id: "@example/loop.view", title: "Loop", content: { kind: "text" } });
"#;

const LOOP_V2: &str = r#"
import { commands, workbench } from "knot";
await commands.register("loop.ping", async () => { await workbench.writeClipboardText("v2"); });
await commands.register("loop.added", async () => {});
await workbench.registerView({ id: "@example/loop.view", title: "Loop", content: { kind: "text" } });
"#;

const DEPENDENT: &str = r#"
import { commands, workbench } from "knot";
await commands.register("dependent.ping", async () => { await workbench.writeClipboardText("dependent"); });
"#;

const SWITCHER: &str = r#"
import { commands, documents, recovery, windows, workbench } from "knot";
async function source() {
  const [window] = await windows.list();
  const [document] = await documents.list();
  return { window, document, capture: await recovery.retain(document.id) };
}
await commands.register("switch.hang", async () => {
  await source();
  while (true) {}
});
await commands.register("switch.go", async () => {
  const { window, document, capture } = await source();
  await workbench.replaceWindows({
    suspend: [{ document: document.id, capture: capture.id }],
    windows: [{ roots: [], entries: [] }],
    release: [window.target],
  });
});
"#;

struct Fixture {
    host: Entity<crate::app::extension_host::ProductExtensionHost>,
    documents: Entity<DocumentCollection>,
    shell: Entity<ProductShell>,
    window: gpui::AnyWindowHandle,
    extensions: tempfile::TempDir,
    _storage: tempfile::TempDir,
}

fn start(packages: &[(&str, &[&str], &str)], cx: &mut TestAppContext) -> Fixture {
    let extensions = tempfile::tempdir().unwrap();
    for (name, requires, source) in packages {
        install_extension(extensions.path(), name, requires, &[("main.js", source)]);
    }
    let documents = install_globals(cx);
    let host = cx.update(crate::app::extension_host::install);
    let storage = tempfile::tempdir().unwrap();
    host.update(cx, |host, cx| {
        host.set_test_storage_root(storage.path().to_owned(), cx)
    });
    let (shell, window) = empty_product_window(&documents, cx);
    host.update(cx, |host, cx| {
        host.start_installed_extensions(extensions.path().to_owned(), cx)
    });
    let report = wait_for_extension_report(&host, cx);
    assert!(
        report
            .report
            .entries
            .iter()
            .all(|entry| entry.result == LoadResult::Loaded)
    );
    Fixture {
        host,
        documents,
        shell,
        window,
        extensions,
        _storage: storage,
    }
}

impl Fixture {
    fn rewrite(&self, name: &str, source: &str) {
        std::fs::write(self.extensions.path().join(name).join("main.js"), source).unwrap();
    }

    fn command(&self, name: &'static str, cx: &mut TestAppContext) -> crate::app::CommandExecution {
        dispatch_product_command(&self.shell, self.window, name, cx)
    }

    fn clipboard_after(&self, name: &'static str, cx: &mut TestAppContext) -> String {
        let execution = self.command(name, cx);
        assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
        cx.read(|cx| cx.read_from_clipboard().unwrap().text().unwrap())
    }

    /// Restart one package through the native picker and return the prompt.
    fn restart(&self, name: &str, cx: &mut TestAppContext) -> (String, String) {
        let execution = self.command(super::super::commands::RESTART_EXTENSION_COMMAND, cx);
        assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
        wait_for_api_condition(cx, |cx| self.shell.read(cx).transient.is_some());
        cx.simulate_input(self.window, name);
        cx.simulate_keystrokes(self.window, "enter");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !cx.has_pending_prompt() {
            assert!(
                std::time::Instant::now() < deadline,
                "restart did not report"
            );
            cx.run_until_parked();
            std::thread::yield_now();
        }
        let prompt = cx.pending_prompt().unwrap();
        cx.simulate_prompt_answer("OK");
        cx.run_until_parked();
        prompt
    }

    fn report_entry(&self, name: &str, cx: &TestAppContext) -> LoadResult {
        cx.read(|cx| {
            crate::app::extension_host::startup_report(cx)
                .unwrap()
                .report
                .entries
                .into_iter()
                .find(|entry| entry.name.as_deref() == Some(name))
                .unwrap()
                .result
        })
    }

    fn texts(&self, cx: &TestAppContext) -> Vec<String> {
        cx.read(|cx| {
            let documents = self.documents.read(cx);
            self.shell
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
}

fn resolves(name: &str, cx: &TestAppContext) -> bool {
    cx.read(|cx| {
        cx.global::<ApplicationProductCommands>()
            .0
            .read(cx)
            .catalog()
            .borrow()
            .resolve(name)
            .is_ok()
    })
}

#[gpui::test]
fn restart_terminates_a_stuck_turn_and_rereads_the_installed_package(cx: &mut TestAppContext) {
    let fixture = start(
        &[
            ("@example/loop", &[], LOOP_V1),
            ("@example/dependent", &["@example/loop"], DEPENDENT),
        ],
        cx,
    );
    let spin = fixture.command("loop.spin", cx);
    cx.run_until_parked();
    // The stuck root holds the package's queue.
    let mut queued = fixture.command("loop.ping", cx);
    cx.run_until_parked();
    assert!(queued.completion.try_recv().is_err());
    let lifecycles = fixture.host.read_with(cx, |host, _| host.lifecycle_count());

    fixture.rewrite("@example/loop", LOOP_V2);
    let (title, _) = fixture.restart("@example/loop", cx);
    assert_eq!(title, "Restarted @example/loop");
    assert_eq!(wait_for_command(spin, cx), CommandOutcome::Cancelled);
    assert_eq!(wait_for_command(queued, cx), CommandOutcome::Cancelled);
    assert_eq!(
        fixture.host.read_with(cx, |host, _| host.lifecycle_count()),
        lifecycles
    );

    // The fresh lifecycle runs the re-read source with its registrations.
    assert_eq!(fixture.clipboard_after("loop.ping", cx), "v2");
    assert!(resolves("loop.added", cx));
    assert!(!resolves("loop.spin", cx));
    assert!(cx.read(|cx| {
        crate::app::extension_host::view_definitions(cx)
            .iter()
            .any(|definition| definition.id == "@example/loop.view")
    }));
    assert_eq!(
        fixture.report_entry("@example/loop", cx),
        LoadResult::Loaded
    );
    // Dependents were never stopped.
    assert_eq!(fixture.clipboard_after("dependent.ping", cx), "dependent");
}

#[gpui::test]
fn a_failed_restart_is_reported_while_dependents_keep_running(cx: &mut TestAppContext) {
    let fixture = start(
        &[
            ("@example/loop", &[], LOOP_V1),
            ("@example/dependent", &["@example/loop"], DEPENDENT),
        ],
        cx,
    );
    fixture.rewrite("@example/loop", "throw new Error('broken on purpose');");
    let (title, message) = fixture.restart("@example/loop", cx);
    assert_eq!(title, "@example/loop failed to restart");
    assert!(message.contains("broken on purpose"), "{message}");
    assert!(matches!(
        fixture.report_entry("@example/loop", cx),
        LoadResult::Failed(_)
    ));
    assert!(!resolves("loop.ping", cx));
    assert_eq!(fixture.clipboard_after("dependent.ping", cx), "dependent");

    // Restarting the dependent now reports its missing requirement.
    let (title, message) = fixture.restart("@example/dependent", cx);
    assert_eq!(title, "@example/dependent failed to restart");
    assert!(message.contains("missing required extension @example/loop"));

    // Fixing the package and restarting it again loads it.
    fixture.rewrite("@example/loop", LOOP_V2);
    assert_eq!(
        fixture.restart("@example/loop", cx).0,
        "Restarted @example/loop"
    );
    assert_eq!(fixture.clipboard_after("loop.ping", cx), "v2");
}

#[gpui::test]
fn a_switch_hung_before_its_commit_leaves_the_source_and_recovers_after_restart(
    cx: &mut TestAppContext,
) {
    let fixture = start(&[("@example/switcher", &[], SWITCHER)], cx);
    let model = cx.read(|cx| {
        let documents = fixture.documents.read(cx);
        documents.documents().next().unwrap().model().clone()
    });
    model.update(cx, |model, cx| {
        model.replace(0..0, "unsaved").unwrap();
        cx.notify();
    });
    let hung = fixture.command("switch.hang", cx);
    cx.run_until_parked();
    assert_eq!(fixture.texts(cx), ["unsaved"]);

    let (title, _) = fixture.restart("@example/switcher", cx);
    assert_eq!(title, "Restarted @example/switcher");
    assert_eq!(wait_for_command(hung, cx), CommandOutcome::Cancelled);
    assert_eq!(fixture.texts(cx), ["unsaved"]);
    assert!(!cx.read(ProductShell::protected_closure_is_active));

    // The restarted package can complete a replacement.
    let go = fixture.command("switch.go", cx);
    assert!(matches!(
        wait_for_command(go, cx),
        CommandOutcome::Completed | CommandOutcome::InvalidTarget
    ));
    assert!(!fixture.texts(cx).contains(&"unsaved".to_owned()));
}
