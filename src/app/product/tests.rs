//! Product shell integration tests and their shared fixtures.

mod closure;
mod git_status;
mod observation;
mod operations;
mod projects;
mod recovery;
mod tool_layout;
mod tools;
mod transient;
mod workspaces;
use std::path::Path;
use std::sync::{Arc, Mutex};

use gpui::{
    AppContext, Focusable, Modifiers, MouseButton, TestAppContext, VisualTestContext, point, px,
};

use super::super::keymaps::{ApplicationKeymaps, BindingOwner, BindingResolution};
use crate::host::protocol::{
    Command, CommandArgumentValue, CommandOutcome, ExtensionId, ExtensionLifecycleId,
};

use super::{
    ApplicationDocuments, ApplicationFileSystems, ApplicationLaunchGate, ApplicationOpenDialog,
    ApplicationProductCommands, ApplicationProtectedClosure, ApplicationSaveDialog,
    ApplicationTerminalSessions, ApplicationWorkbenches, BufferModel, CLOSE_TAB_COMMAND,
    CLOSE_WINDOW_COMMAND, DocumentCollection, Entity, LaunchGate,
    MOVE_TERMINAL_TO_NEW_WINDOW_COMMAND, NEW_COMMAND, NEW_TERMINAL_COMMAND, OpenDialogFuture,
    OpenDialogOutcome, OpenRequest, OpenTarget, ProductCommandDispatcher, ProductCommandSource,
    ProductOpenDialog, ProductSaveDialog, ProductShell, QUIT_COMMAND, REDO_COMMAND, RefCell,
    SAVE_AS_COMMAND, SAVE_COMMAND, SHOW_EXTENSION_REPORT_COMMAND, SPLIT_HORIZONTAL_COMMAND,
    SaveDialogFuture, SaveDialogOutcome, SplitDirection, TerminalSessions, UNDO_COMMAND, Workbench,
    WorkbenchLayout, create_untitled_document, install_protected_window_close,
    open_tab_transfer_window, product_filesystems,
};
use crate::app::{
    documents::DocumentState,
    filesystem::{
        FileSystemProvider, FileSystemProviderRegistry, MemoryFileSystemProvider, ProviderFuture,
        ResourceEntry, ResourceFile, ResourceStat, ResourceVersion,
    },
    resource::ResourceUri,
    workbench::TabSurfaceId,
};

struct GatedFileSystemProvider {
    inner: Arc<MemoryFileSystemProvider>,
    read_release: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    replace_release: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}

impl FileSystemProvider for GatedFileSystemProvider {
    fn normalize(&self, uri: ResourceUri) -> ProviderFuture<'_, ResourceUri> {
        self.inner.normalize(uri)
    }

    fn enumerate(&self, uri: ResourceUri) -> ProviderFuture<'_, Vec<ResourceEntry>> {
        self.inner.enumerate(uri)
    }

    fn read(&self, uri: ResourceUri) -> ProviderFuture<'_, ResourceFile> {
        let release = self.read_release.lock().unwrap().take();
        Box::pin(async move {
            if let Some(release) = release {
                let _ = release.await;
            }
            self.inner.read(uri).await
        })
    }

    fn create(&self, uri: ResourceUri, bytes: Vec<u8>) -> ProviderFuture<'_, ResourceVersion> {
        self.inner.create(uri, bytes)
    }

    fn replace(
        &self,
        uri: ResourceUri,
        expected: ResourceVersion,
        bytes: Vec<u8>,
    ) -> ProviderFuture<'_, ResourceVersion> {
        let release = self.replace_release.lock().unwrap().take();
        Box::pin(async move {
            if let Some(release) = release {
                let _ = release.await;
            }
            self.inner.replace(uri, expected, bytes).await
        })
    }

    fn stat(&self, uri: ResourceUri) -> ProviderFuture<'_, ResourceStat> {
        self.inner.stat(uri)
    }
}

pub(super) fn install_globals(cx: &mut TestAppContext) -> Entity<DocumentCollection> {
    let documents = cx.new(|_| DocumentCollection::new());
    cx.set_global(ApplicationDocuments(documents.clone()));
    // Each test gets an isolated recovery database; the directory is kept
    // as a global so the database path stays alive with the test.
    let recovery_dir = tempfile::tempdir().unwrap();
    cx.set_global(crate::app::recovery::ApplicationRecovery(
        crate::app::recovery::RecoveryService::new(crate::app::recovery::RecoveryStore::open(
            recovery_dir.path().join("recovery.sqlite3"),
            cx.background_executor.clone(),
        )),
    ));
    cx.set_global(TestRecoveryDir(recovery_dir));
    cx.set_global(ApplicationWorkbenches(RefCell::new(Vec::new())));
    cx.set_global(ApplicationProtectedClosure(RefCell::new(false)));
    cx.set_global(ApplicationTerminalSessions(RefCell::new(
        TerminalSessions::default(),
    )));
    cx.set_global(ApplicationFileSystems(product_filesystems()));
    let commands = cx.new(ProductCommandDispatcher::new);
    cx.set_global(ApplicationProductCommands(commands));
    // Each test gets an isolated tool-layout database; the directory is
    // retained as a global so the database path stays alive with the test.
    let layout_dir = tempfile::tempdir().unwrap();
    cx.set_global(crate::app::tool_layout::ApplicationToolLayout(
        crate::app::tool_layout::ToolLayoutStore::open(
            layout_dir.path().join("tool-layout.sqlite3"),
            cx.background_executor.clone(),
        ),
    ));
    cx.set_global(TestToolLayoutDir(layout_dir));
    documents
}

/// Replace recovery with a store that cannot write, so quit falls back to
/// asking about each dirty document.
pub(super) fn install_failing_recovery(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, "not a directory").unwrap();
    cx.set_global(crate::app::recovery::ApplicationRecovery(
        crate::app::recovery::RecoveryService::new(crate::app::recovery::RecoveryStore::open(
            blocker.join("recovery.sqlite3"),
            cx.background_executor.clone(),
        )),
    ));
    cx.set_global(TestRecoveryDir(dir));
}

#[allow(dead_code, reason = "ownership keeps the test database path alive")]
struct TestToolLayoutDir(tempfile::TempDir);

impl gpui::Global for TestToolLayoutDir {}

#[allow(dead_code, reason = "ownership keeps the test database path alive")]
struct TestRecoveryDir(tempfile::TempDir);

impl gpui::Global for TestRecoveryDir {}

fn keymap_command(cx: &gpui::App, key: &str, view: Option<&str>) -> Option<String> {
    match cx
        .global::<ApplicationKeymaps>()
        .0
        .resolve(&gpui::Keystroke::parse(key).unwrap().unparse(), view)
    {
        Some(BindingResolution::Command(command)) => Some(command.name.to_string()),
        Some(BindingResolution::Unbound) => Some("<unbound>".into()),
        None => None,
    }
}

fn install_launch_gate(cx: &mut TestAppContext) {
    cx.set_global(ApplicationLaunchGate(RefCell::new(LaunchGate::Pending(
        Vec::new(),
    ))));
}

fn wait_for_product_startup(cx: &mut TestAppContext) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        cx.run_until_parked();
        let state = cx.read(
            |cx| match &*cx.global::<ApplicationLaunchGate>().0.borrow() {
                LaunchGate::Pending(_) => None,
                LaunchGate::Ready => Some(true),
                LaunchGate::Failed => Some(false),
            },
        );
        if let Some(ready) = state {
            return ready;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "product startup did not finish"
        );
        std::thread::yield_now();
    }
}

#[test]
fn launch_gate_keeps_requests_ordered_and_discards_them_on_failure() {
    let first =
        OpenRequest::from_uri_for_product(ResourceUri::parse("file:///queued-first.txt").unwrap());
    let second =
        OpenRequest::from_uri_for_product(ResourceUri::parse("file:///queued-second.txt").unwrap());
    let mut gate = LaunchGate::Pending(Vec::new());
    assert!(gate.request(Some(first.clone())).is_none());
    assert!(gate.request(Some(second.clone())).is_none());
    assert_eq!(gate.complete(), vec![Some(first), Some(second.clone())]);
    assert_eq!(gate.request(None), Some(None));

    let mut failed = LaunchGate::Pending(Vec::new());
    failed.request(None);
    failed.fail();
    assert!(failed.complete().is_empty());
    assert!(failed.request(Some(second)).is_none());

    let mut empty = LaunchGate::Pending(Vec::new());
    assert_eq!(empty.complete(), vec![None]);
}

pub(super) fn product_window(
    document: super::DocumentId,
    model: Entity<super::BufferModel>,
    cx: &mut TestAppContext,
) -> (Entity<ProductShell>, gpui::AnyWindowHandle) {
    let (shell, _) = cx.add_window_view(|window, cx| {
        let workbench = cx.new(|cx| Workbench::new_for_document(document, model, cx));
        cx.global::<ApplicationWorkbenches>()
            .register(&workbench, window.window_handle());
        ProductShell::new(workbench)
    });
    let window = cx.read(|cx| {
        cx.global::<ApplicationWorkbenches>()
            .window_for(&shell.read(cx).workbench)
            .unwrap()
    });
    cx.update_window(window, |_, native_window, cx| {
        shell.update(cx, |shell, cx| {
            shell.observe_extension_host(native_window, cx)
        });
        install_protected_window_close(&shell, native_window, cx);
    })
    .unwrap();
    (shell, window)
}

fn two_dirty_untitled_documents_in_one_window(
    documents: &Entity<DocumentCollection>,
    cx: &mut TestAppContext,
) -> (
    super::DocumentId,
    super::DocumentId,
    Entity<ProductShell>,
    gpui::AnyWindowHandle,
) {
    let first = cx.update(|cx| create_untitled_document(documents, cx));
    let first_model = cx.read(|cx| documents.read(cx).get(first).unwrap().model().clone());
    first_model.update(cx, |model, _| model.replace(0..0, "first").unwrap());
    let second_model = cx.new(|_| BufferModel::from_text("second"));
    let second = documents.update(cx, |documents, cx| {
        documents.create_untitled("Second", second_model.clone(), cx)
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
    (first, second, shell, window)
}

fn memory_filesystems() -> Arc<FileSystemProviderRegistry> {
    memory_filesystems_with_provider().0
}

fn memory_filesystems_with_provider() -> (
    Arc<FileSystemProviderRegistry>,
    Arc<MemoryFileSystemProvider>,
) {
    let root = ResourceUri::parse("mem://product/").unwrap();
    let provider = Arc::new(MemoryFileSystemProvider::new(root).unwrap());
    provider
        .seed_file(
            ResourceUri::parse("mem://product/notes.txt").unwrap(),
            b"loaded text",
        )
        .unwrap();
    provider
        .seed_file(
            ResourceUri::parse("mem://product/invalid.txt").unwrap(),
            [0xff],
        )
        .unwrap();
    provider
        .seed_directory(ResourceUri::parse("mem://product/src").unwrap())
        .unwrap();
    let mut providers = FileSystemProviderRegistry::new();
    providers.register("mem", provider.clone()).unwrap();
    (Arc::new(providers), provider)
}

fn current_open_target(shell: &Entity<ProductShell>, cx: &gpui::App) -> OpenTarget {
    let shell = shell.read(cx);
    let pane = shell.workbench.read(cx).focused_pane().unwrap();
    let tab = pane.active_tab();
    OpenTarget {
        window: cx
            .global::<ApplicationWorkbenches>()
            .window_for(&shell.workbench)
            .unwrap(),
        workbench: shell.workbench.downgrade(),
        pane: pane.id(),
        tab: tab.id(),
        surface: tab.surface_id(),
        presentation: tab.presentation(),
    }
}

fn dispatch_product_command(
    shell: &Entity<ProductShell>,
    window_handle: gpui::AnyWindowHandle,
    name: &'static str,
    cx: &mut TestAppContext,
) -> super::super::CommandExecution {
    dispatch_product_command_with(shell, window_handle, name, CommandArgumentValue::Null, cx)
}

fn dispatch_product_command_with(
    shell: &Entity<ProductShell>,
    window_handle: gpui::AnyWindowHandle,
    name: &'static str,
    arguments: CommandArgumentValue,
    cx: &mut TestAppContext,
) -> super::super::CommandExecution {
    let target = cx
        .update_window(window_handle, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.focus_active_surface(window, cx);
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    cx.update(|cx| {
        let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
        dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: name.into(),
                    arguments,
                },
                target,
                cx,
            )
        })
    })
}

fn install_extension(root: &Path, name: &str, requires: &[&str], files: &[(&str, &str)]) {
    let directory = root.join(name);
    std::fs::create_dir_all(&directory).unwrap();
    let main = files[0].0;
    std::fs::write(
        directory.join("knot.jsonc"),
        serde_json::json!({
            "name": name,
            "apiVersion": 1, "version": "1.0.0",
            "main": main,
            "requires": requires,
        })
        .to_string(),
    )
    .unwrap();
    for (path, source) in files {
        let destination = directory.join(path);
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::write(destination, source).unwrap();
    }
}

fn wait_for_extension_report(
    _host: &Entity<super::super::extension_host::ProductExtensionHost>,
    cx: &mut TestAppContext,
) -> super::super::extension_host::StartupReportSnapshot {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        cx.run_until_parked();
        if let Some(report) = cx.read(super::super::extension_host::startup_report)
            && report.state == super::super::extension_host::StartupState::Complete
        {
            return report;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "extension startup did not finish"
        );
        std::thread::yield_now();
    }
}

#[gpui::test]
fn personal_config_gates_product_windows_and_keeps_extension_failures_independent(
    cx: &mut TestAppContext,
) {
    let config = tempfile::tempdir().unwrap();
    let extensions = tempfile::tempdir().unwrap();
    std::fs::write(
        config.path().join("pre-init.js"),
        "import * as knot from 'knot'; const { commands } = knot; await commands.register('config.pre', () => {});",
    )
    .unwrap();
    std::fs::write(
        config.path().join("post-init.js"),
        "import * as knot from 'knot'; const { commands } = knot; await commands.register('config.post', () => {});",
    )
    .unwrap();
    install_extension(
        extensions.path(),
        "@example/good",
        &[],
        &[(
            "main.js",
            "import * as knot from 'knot'; const { commands } = knot; await commands.register('extension.good', () => {});",
        )],
    );
    install_extension(
        extensions.path(),
        "@example/bad",
        &[],
        &[("main.js", "throw new Error('package failed');")],
    );
    install_globals(cx);
    install_launch_gate(cx);
    let host = cx.update(super::super::extension_host::install);
    cx.update(|cx| super::dispatch_launch_request(None, cx));
    assert!(cx.windows().is_empty());
    host.update(cx, |host, cx| {
        host.start_product_startup(
            Some(config.path().to_path_buf()),
            Ok(extensions.path().to_path_buf()),
            cx,
        )
    });
    assert!(wait_for_product_startup(cx));
    assert_eq!(cx.windows().len(), 1);
    let report = cx
        .read(super::super::extension_host::startup_report)
        .unwrap();
    assert!(
        report
            .report
            .entries
            .iter()
            .any(|entry| entry.name.as_deref() == Some("@example/good")
                && entry.result == super::super::extension_load::LoadResult::Loaded)
    );
    assert!(report.report.entries.iter().any(|entry| entry.name.as_deref() == Some("@example/bad") && matches!(&entry.result, super::super::extension_load::LoadResult::Failed(cause) if cause.contains("package failed"))));
    let names = cx.read(|cx| {
        cx.global::<ApplicationProductCommands>()
            .0
            .read(cx)
            .definitions()
            .map(|definition| definition.name.to_string())
            .collect::<Vec<_>>()
    });
    for name in ["config.pre", "extension.good", "config.post"] {
        assert!(names.contains(&name.to_owned()), "missing {name}");
    }
    cx.update(|cx| super::dispatch_launch_request(None, cx));
    assert_eq!(cx.windows().len(), 2);
}

#[gpui::test]
fn script_keybindings_follow_config_phases_and_runtime_replacement(cx: &mut TestAppContext) {
    let config = tempfile::tempdir().unwrap();
    let extensions = tempfile::tempdir().unwrap();
    std::fs::write(
        config.path().join("pre-init.js"),
        "import * as knot from 'knot'; knot.keybinding('cmd-n', 'file.open');",
    )
    .unwrap();
    std::fs::write(
        config.path().join("post-init.js"),
        "import * as knot from 'knot'; knot.keybinding('cmd-n', 'file.save-as'); knot.keybinding('cmd-x', null); knot.keybinding('cmd-j', { name: 'test.args', arguments: { marker: 42 } });",
    )
    .unwrap();
    install_extension(
        extensions.path(),
        "@example/good",
        &[],
        &[(
            "main.js",
            r#"
                import * as knot from 'knot';
                knot.keybinding('cmd-n', 'file.save');
                knot.keybinding('cmd-r', 'file.open');
                await knot.commands.register('test.remap', () => {
                    knot.keybinding('cmd-r', 'file.new');
                    knot.removeKeybinding('cmd-n');
                });
                await knot.commands.register('test.unbind', () => knot.keybinding('cmd-r', null, { view: 'editor' }));
                await knot.commands.register('test.restore', () => knot.removeKeybinding('cmd-r', { view: 'editor' }));
                await knot.commands.register('test.remove', () => knot.removeKeybinding('cmd-r'));
                await knot.commands.register('test.args', ({ arguments: args }) => {
                    if (args.marker !== 42) throw new Error('wrong binding arguments');
                });
                await knot.commands.register('test.invalid', () => {
                    knot.keybinding('cmd-r', 'file.save');
                    knot.keybinding('bad-modifier-key', 'file.new');
                });
            "#,
        )],
    );
    install_extension(
        extensions.path(),
        "@example/bad",
        &[],
        &[(
            "main.js",
            "import * as knot from 'knot'; knot.keybinding('cmd-l', 'file.new'); await knot.workbench.writeClipboardText('startup'); throw new Error('failed after binding');",
        )],
    );
    install_globals(cx);
    cx.update(super::bind_product_keys);
    install_launch_gate(cx);
    let host = cx.update(super::super::extension_host::install);
    host.update(cx, |host, cx| {
        host.start_product_startup(
            Some(config.path().to_path_buf()),
            Ok(extensions.path().to_path_buf()),
            cx,
        )
    });
    assert!(wait_for_product_startup(cx));
    cx.read(|cx| {
        assert_eq!(keymap_command(cx, "cmd-n", Some("editor")).as_deref(), Some("file.save-as"));
        assert_eq!(keymap_command(cx, "cmd-r", Some("editor")).as_deref(), Some("file.open"));
        assert_eq!(keymap_command(cx, "cmd-x", Some("editor")).as_deref(), Some("<unbound>"));
        assert_eq!(keymap_command(cx, "cmd-l", Some("editor")), None);
        assert!(matches!(
            cx.global::<ApplicationKeymaps>().0.resolve(&gpui::Keystroke::parse("cmd-j").unwrap().unparse(), Some("editor")),
            Some(BindingResolution::Command(Command { arguments: CommandArgumentValue::Object(arguments), .. }))
                if arguments.get("marker") == Some(&CommandArgumentValue::Number(42.0))
        ));
    });
    let report = cx
        .read(super::super::extension_host::startup_report)
        .unwrap();
    assert!(report.report.entries.iter().any(|entry| {
        entry.name.as_deref() == Some("@example/bad")
            && matches!(&entry.result, super::super::extension_load::LoadResult::Failed(cause) if cause.contains("failed after binding"))
    }));
    assert!(cx.read(|cx| {
        cx.global::<ApplicationProductCommands>()
            .0
            .read(cx)
            .definitions()
            .any(|definition| definition.name.as_ref() == "test.args")
    }));
    let documents = cx.read(|cx| cx.global::<ApplicationDocuments>().0.clone());
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    let target = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.focus_active_surface(window, cx);
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let command = cx.read(|cx| {
        let Some(BindingResolution::Command(command)) =
            cx.global::<ApplicationKeymaps>().0.resolve(
                &gpui::Keystroke::parse("cmd-j").unwrap().unparse(),
                Some("editor"),
            )
        else {
            panic!("expected command binding");
        };
        command
    });
    let execution = cx.update(|cx| {
        let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
        dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(command, target, cx)
        })
    });
    assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
    let execution = dispatch_product_command(&shell, window, "test.remap", cx);
    assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
    cx.read(|cx| {
        assert_eq!(
            keymap_command(cx, "cmd-r", Some("editor")).as_deref(),
            Some("file.new")
        );
        assert_eq!(
            keymap_command(cx, "cmd-n", Some("editor")).as_deref(),
            Some("file.save-as")
        );
    });
    let invalid = wait_for_command(
        dispatch_product_command(&shell, window, "test.invalid", cx),
        cx,
    );
    assert!(
        matches!(invalid, CommandOutcome::HandlerFailure { message } if message.contains("keybinding operation 2"))
    );
    cx.read(|cx| {
        assert_eq!(
            keymap_command(cx, "cmd-r", Some("editor")).as_deref(),
            Some("file.new")
        )
    });
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&shell, window, "test.unbind", cx),
            cx
        ),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert_eq!(
            keymap_command(cx, "cmd-r", Some("editor")).as_deref(),
            Some("<unbound>")
        );
        assert_eq!(
            keymap_command(cx, "cmd-r", Some("terminal")).as_deref(),
            Some("file.new")
        );
    });
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&shell, window, "test.restore", cx),
            cx
        ),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert_eq!(
            keymap_command(cx, "cmd-r", Some("editor")).as_deref(),
            Some("file.new")
        )
    });
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&shell, window, "test.remove", cx),
            cx
        ),
        CommandOutcome::Completed
    );
    cx.read(|cx| assert_eq!(keymap_command(cx, "cmd-r", Some("editor")), None));
}

#[gpui::test]
fn config_remaps_product_keys_without_changing_menu_or_palette_dispatch(cx: &mut TestAppContext) {
    let config = tempfile::tempdir().unwrap();
    let extensions = tempfile::tempdir().unwrap();
    std::fs::write(
        config.path().join("pre-init.js"),
        "import * as knot from 'knot'; knot.keybinding('cmd-n', null); knot.keybinding('cmd-g', 'file.new');",
    )
    .unwrap();
    std::fs::write(
        config.path().join("post-init.js"),
        "import * as knot from 'knot'; knot.keybinding('cmd-r', 'test.mark');",
    )
    .unwrap();
    install_extension(
        extensions.path(),
        "@example/mark",
        &[],
        &[(
            "main.js",
            "import * as knot from 'knot'; await knot.commands.register('test.mark', () => knot.keybinding('cmd-y', 'file.new'));",
        )],
    );
    install_globals(cx);
    cx.update(super::bind_product_keys);
    install_launch_gate(cx);
    let host = cx.update(super::super::extension_host::install);
    host.update(cx, |host, cx| {
        host.start_product_startup(
            Some(config.path().to_path_buf()),
            Ok(extensions.path().to_path_buf()),
            cx,
        )
    });
    assert!(wait_for_product_startup(cx));
    let window = cx.windows()[0];
    let shell = cx
        .update_window(window, |_, window, _| {
            window.root::<ProductShell>().flatten()
        })
        .unwrap()
        .unwrap();
    cx.update_window(window, |_, window, cx| {
        shell.read(cx).focus_active_editor(window, cx)
    })
    .unwrap();
    cx.refresh().unwrap();

    cx.simulate_keystrokes(window, "cmd-n");
    cx.run_until_parked();
    cx.read(|cx| {
        assert_eq!(
            shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .tabs()
                .len(),
            1
        )
    });
    cx.simulate_keystrokes(window, "cmd-g");
    cx.run_until_parked();
    cx.read(|cx| {
        assert_eq!(
            shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .tabs()
                .len(),
            2
        )
    });

    cx.simulate_keystrokes(window, "cmd-r");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        cx.run_until_parked();
        if cx.read(|cx| keymap_command(cx, "cmd-y", Some("editor")).as_deref() == Some("file.new"))
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "config binding did not invoke the extension command"
        );
        std::thread::yield_now();
    }

    cx.dispatch_action(window, ProductCommandSource::new(NEW_COMMAND));
    cx.read(|cx| {
        assert_eq!(
            shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .tabs()
                .len(),
            3
        )
    });
    cx.update_window(window, |_, window, cx| {
        shell.update(cx, |shell, cx| shell.open_command_palette(window, cx));
    })
    .unwrap();
    cx.refresh().unwrap();
    cx.simulate_keystrokes(window, "f i l e . n e w enter");
    cx.run_until_parked();
    cx.read(|cx| {
        assert_eq!(
            shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .tabs()
                .len(),
            4
        )
    });
}

#[gpui::test]
fn config_global_copy_binding_routes_to_editor_and_extension_view(cx: &mut TestAppContext) {
    use gpui::EntityInputHandler;

    let config = tempfile::tempdir().unwrap();
    let extensions = tempfile::tempdir().unwrap();
    std::fs::write(
        config.path().join("post-init.js"),
        "import * as knot from 'knot'; knot.keybinding('cmd-j', 'copy');",
    )
    .unwrap();
    install_extension(
        extensions.path(),
        "@example/outline-copy",
        &[],
        &[(
            "main.js",
            r#"
                import * as knot from 'knot';
                await knot.workbench.registerView({
                    id: 'example.outline',
                    title: 'Outline',
                    content: { kind: 'tree', provider: {
                        getChildren(parentId) {
                            return parentId === null
                                ? [{ id: 'first', label: 'Tree item', collapsibleState: 'none' }]
                                : [];
                        },
                    } },
                });
                await knot.commands.registerForView('example.outline', 'copy', async () => {
                    const text = await knot.workbench.selectedText();
                    await knot.workbench.writeClipboardText(text);
                });
                knot.keybinding('cmd-j', 'file.new', { view: 'example.outline' });
            "#,
        )],
    );
    let documents = install_globals(cx);
    cx.update(super::bind_product_keys);
    install_launch_gate(cx);
    let host = cx.update(super::super::extension_host::install);
    host.update(cx, |host, cx| {
        host.start_product_startup(
            Some(config.path().to_path_buf()),
            Ok(extensions.path().to_path_buf()),
            cx,
        )
    });
    assert!(wait_for_product_startup(cx));
    cx.read(|cx| {
        assert_eq!(
            keymap_command(cx, "cmd-j", Some("editor")).as_deref(),
            Some("copy")
        );
        assert_eq!(
            keymap_command(cx, "cmd-j", Some("example.outline")).as_deref(),
            Some("copy")
        );
    });
    let window = cx.windows()[0];
    let shell = cx
        .update_window(window, |_, window, _| {
            window.root::<ProductShell>().flatten()
        })
        .unwrap()
        .unwrap();
    let tree = wait_for_contributed_view(&shell, "example.outline", cx);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        cx.run_until_parked();
        if cx.read(|cx| tree.read(cx).root_labels() == ["Tree item"]) {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "outline did not load");
        std::thread::yield_now();
    }

    cx.update_window(window, |_, window, cx| {
        let editor = shell
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab()
            .editor()
            .unwrap()
            .clone();
        editor.update(cx, |editor, cx| {
            editor.replace_text_in_range(None, "Editor selection", window, cx);
            editor.execute_editing_command("editor.select-all", window, cx);
        });
        editor.focus_handle(cx).focus(window);
    })
    .unwrap();
    cx.refresh().unwrap();
    cx.simulate_keystrokes(window, "cmd-j");
    cx.run_until_parked();
    cx.read(|cx| {
        assert_eq!(
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .as_deref(),
            Some("Editor selection")
        );
    });

    cx.update_window(window, |_, window, cx| {
        tree.update(cx, |tree, cx| assert!(tree.select_item("first", cx)));
        tree.focus_handle(cx).focus(window);
    })
    .unwrap();
    cx.update(|cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string("untouched".into())));
    cx.refresh().unwrap();
    cx.simulate_keystrokes(window, "cmd-j");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        cx.run_until_parked();
        if cx.read(|cx| {
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .as_deref()
                == Some("Tree item")
        }) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "global copy binding did not route to the outline"
        );
        std::thread::yield_now();
    }
    cx.read(|cx| assert_eq!(documents.read(cx).documents().count(), 1));
}

#[gpui::test]
fn invalid_config_keybinding_batch_rolls_back_before_product_readiness(cx: &mut TestAppContext) {
    let config = tempfile::tempdir().unwrap();
    let extensions = tempfile::tempdir().unwrap();
    std::fs::write(
        config.path().join("pre-init.js"),
        "import * as knot from 'knot'; knot.keybinding('cmd-r', 'file.open'); knot.keybinding('bad-modifier-key', 'file.new');",
    )
    .unwrap();
    install_globals(cx);
    cx.update(super::bind_product_keys);
    install_launch_gate(cx);
    let host = cx.update(super::super::extension_host::install);
    host.update(cx, |host, cx| {
        host.start_product_startup(
            Some(config.path().to_path_buf()),
            Ok(extensions.path().to_path_buf()),
            cx,
        )
    });
    assert!(!wait_for_product_startup(cx));
    cx.read(|cx| {
        assert_eq!(keymap_command(cx, "cmd-r", Some("editor")), None);
        assert_eq!(host.read(cx).lifecycle_count(), 0);
        assert!(cx.global::<ApplicationWorkbenches>().0.borrow().is_empty());
    });
    assert_eq!(cx.windows().len(), 1);
}

#[gpui::test]
fn invalid_extension_keybinding_batch_does_not_block_other_extensions(cx: &mut TestAppContext) {
    let extensions = tempfile::tempdir().unwrap();
    install_extension(
        extensions.path(),
        "@example/good",
        &[],
        &[(
            "main.js",
            "import * as knot from 'knot'; knot.keybinding('cmd-r', 'file.new');",
        )],
    );
    install_extension(
        extensions.path(),
        "@example/bad",
        &[],
        &[(
            "main.js",
            "import * as knot from 'knot'; knot.keybinding('cmd-l', 'file.new'); knot.keybinding('bad-modifier-key', 'file.open');",
        )],
    );
    install_globals(cx);
    cx.update(super::bind_product_keys);
    install_launch_gate(cx);
    let host = cx.update(super::super::extension_host::install);
    host.update(cx, |host, cx| {
        host.start_product_startup(None, Ok(extensions.path().to_path_buf()), cx)
    });
    assert!(wait_for_product_startup(cx));
    let report = cx
        .read(super::super::extension_host::startup_report)
        .unwrap();
    assert!(report.report.entries.iter().any(|entry| {
        entry.name.as_deref() == Some("@example/good")
            && entry.result == super::super::extension_load::LoadResult::Loaded
    }));
    assert!(report.report.entries.iter().any(|entry| {
        entry.name.as_deref() == Some("@example/bad")
            && matches!(&entry.result, super::super::extension_load::LoadResult::Failed(cause) if cause.contains("keybinding operation 2"))
    }));
    cx.read(|cx| {
        assert_eq!(
            keymap_command(cx, "cmd-r", Some("editor")).as_deref(),
            Some("file.new")
        );
        assert_eq!(keymap_command(cx, "cmd-l", Some("editor")), None);
    });
}

#[gpui::test]
fn unloading_a_script_lifecycle_removes_its_keybindings(cx: &mut TestAppContext) {
    use crate::host::lifecycle::ExtensionKey;

    install_globals(cx);
    cx.update(super::bind_product_keys);
    let host = cx.update(super::super::extension_host::install);
    let key = ExtensionKey::new(ExtensionId::new(91), ExtensionLifecycleId::new(1));
    let loaded = Arc::new(Mutex::new(false));
    let done = loaded.clone();
    host.update(cx, |host, cx| {
        let execution = host
            .load_test_source(
                key,
                "import * as knot from 'knot'; knot.keybinding('cmd-r', 'file.open');",
            )
            .unwrap();
        cx.spawn(async move |_, _| {
            execution.await.unwrap();
            *done.lock().unwrap() = true;
        })
        .detach();
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !*loaded.lock().unwrap() {
        cx.run_until_parked();
        assert!(
            std::time::Instant::now() < deadline,
            "extension keybinding did not load"
        );
        std::thread::yield_now();
    }
    cx.read(|cx| {
        assert_eq!(
            keymap_command(cx, "cmd-r", None).as_deref(),
            Some("file.open")
        )
    });
    host.update(cx, |host, cx| host.unload(key, cx).unwrap());
    cx.read(|cx| assert_eq!(keymap_command(cx, "cmd-r", None), None));
}

#[gpui::test]
fn queued_file_open_runs_only_after_configured_startup(cx: &mut TestAppContext) {
    let config = tempfile::tempdir().unwrap();
    let extensions = tempfile::tempdir().unwrap();
    let file = config.path().join("queued.txt");
    std::fs::write(&file, "queued contents").unwrap();
    let request = OpenRequest::from_path(&file, config.path()).unwrap();
    let uri = request.uri().clone();
    let documents = install_globals(cx);
    install_launch_gate(cx);
    let host = cx.update(super::super::extension_host::install);
    cx.update(|cx| super::dispatch_launch_request(Some(request), cx));
    assert!(cx.windows().is_empty());
    host.update(cx, |host, cx| {
        host.start_product_startup(
            Some(config.path().to_path_buf()),
            Ok(extensions.path().to_path_buf()),
            cx,
        )
    });
    assert!(wait_for_product_startup(cx));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        cx.run_until_parked();
        let text = cx.read(|cx| {
            let documents = documents.read(cx);
            documents
                .document_for_resource(&uri)
                .and_then(|id| documents.get(id))
                .map(|document| document.model().read(cx).text().to_owned())
        });
        if let Some(text) = text {
            assert_eq!(text, "queued contents");
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "queued file did not open"
        );
        std::thread::yield_now();
    }
}

#[gpui::test]
fn pre_init_failure_skips_extensions_and_opens_only_the_error_window(cx: &mut TestAppContext) {
    let config = tempfile::tempdir().unwrap();
    let extensions = tempfile::tempdir().unwrap();
    std::fs::write(config.path().join("pre-init.js"), "import * as knot from 'knot'; const { commands } = knot; await commands.register('config.before-failure', () => {}); throw new Error('pre failed');").unwrap();
    install_extension(
        extensions.path(),
        "@example/skipped",
        &[],
        &[("main.js", "throw new Error('extension ran');")],
    );
    install_globals(cx);
    install_launch_gate(cx);
    let host = cx.update(super::super::extension_host::install);
    let queued = OpenRequest::from_uri_for_product(
        ResourceUri::parse("file:///never-open-on-failure.txt").unwrap(),
    );
    cx.update(|cx| super::dispatch_launch_request(Some(queued.clone()), cx));
    assert!(cx.windows().is_empty());
    host.update(cx, |host, cx| {
        host.start_product_startup(
            Some(config.path().to_path_buf()),
            Ok(extensions.path().to_path_buf()),
            cx,
        )
    });
    assert!(!wait_for_product_startup(cx));
    assert_eq!(cx.windows().len(), 1);
    assert!(
        cx.read(super::super::extension_host::startup_report)
            .is_none()
    );
    assert!(cx.read(|cx| cx.global::<ApplicationWorkbenches>().0.borrow().is_empty()));
    assert_eq!(cx.read(|cx| host.read(cx).lifecycle_count()), 0);
    let names = cx.read(|cx| {
        cx.global::<ApplicationProductCommands>()
            .0
            .read(cx)
            .definitions()
            .map(|definition| definition.name.to_string())
            .collect::<Vec<_>>()
    });
    assert!(!names.contains(&"config.before-failure".to_owned()));
    cx.update(|cx| super::dispatch_launch_request(Some(queued), cx));
    assert_eq!(cx.windows().len(), 1);
}

#[gpui::test]
fn invalid_config_import_prevents_extension_discovery(cx: &mut TestAppContext) {
    let config = tempfile::tempdir().unwrap();
    let extensions = tempfile::tempdir().unwrap();
    std::fs::write(config.path().join("pre-init.js"), "import './missing.js';").unwrap();
    install_extension(
        extensions.path(),
        "@example/skipped",
        &[],
        &[("main.js", "throw new Error('extension ran');")],
    );
    install_globals(cx);
    install_launch_gate(cx);
    let host = cx.update(super::super::extension_host::install);
    host.update(cx, |host, cx| {
        host.start_product_startup(
            Some(config.path().to_path_buf()),
            Ok(extensions.path().to_path_buf()),
            cx,
        )
    });
    assert!(!wait_for_product_startup(cx));
    assert_eq!(cx.windows().len(), 1);
    assert!(
        cx.read(super::super::extension_host::startup_report)
            .is_none()
    );
    assert_eq!(cx.read(|cx| host.read(cx).lifecycle_count()), 0);
}

#[gpui::test]
fn post_init_failure_unloads_config_and_installed_extensions(cx: &mut TestAppContext) {
    let config = tempfile::tempdir().unwrap();
    let extensions = tempfile::tempdir().unwrap();
    std::fs::write(
        config.path().join("pre-init.js"),
        "import * as knot from 'knot'; const { commands, editor } = knot; await commands.register('config.before', () => {}); await editor.registerCompletionProvider('config-provider', { provideCompletions() { return []; } });",
    )
    .unwrap();
    std::fs::write(
        config.path().join("post-init.js"),
        "throw new Error('post failed');",
    )
    .unwrap();
    install_extension(
        extensions.path(),
        "@example/loaded",
        &[],
        &[(
            "main.js",
            "import * as knot from 'knot'; const { commands } = knot; await commands.register('extension.before', () => {});",
        )],
    );
    install_globals(cx);
    install_launch_gate(cx);
    let host = cx.update(super::super::extension_host::install);
    host.update(cx, |host, cx| {
        host.start_product_startup(
            Some(config.path().to_path_buf()),
            Ok(extensions.path().to_path_buf()),
            cx,
        )
    });
    assert!(!wait_for_product_startup(cx));
    assert_eq!(cx.windows().len(), 1);
    assert!(cx.read(|cx| cx.global::<ApplicationWorkbenches>().0.borrow().is_empty()));
    let report = cx
        .read(super::super::extension_host::startup_report)
        .unwrap();
    assert_eq!(
        report.report.entries[0].result,
        super::super::extension_load::LoadResult::Loaded
    );
    assert_eq!(cx.read(|cx| host.read(cx).lifecycle_count()), 0);
    let names = cx.read(|cx| {
        cx.global::<ApplicationProductCommands>()
            .0
            .read(cx)
            .definitions()
            .map(|definition| definition.name.to_string())
            .collect::<Vec<_>>()
    });
    assert!(!names.contains(&"config.before".to_owned()));
    assert!(!names.contains(&"extension.before".to_owned()));
    assert_eq!(
        cx.read(super::super::extension_host::completion_provider_count),
        0
    );
}

#[gpui::test]
fn post_init_validation_failure_waits_for_installed_extensions(cx: &mut TestAppContext) {
    let config = tempfile::tempdir().unwrap();
    let extensions = tempfile::tempdir().unwrap();
    std::fs::write(
        config.path().join("pre-init.js"),
        "globalThis.preRan = true;",
    )
    .unwrap();
    std::fs::write(config.path().join("post-init.js"), "import './missing.js';").unwrap();
    install_extension(
        extensions.path(),
        "@example/loaded",
        &[],
        &[("main.js", "export {};")],
    );
    install_globals(cx);
    install_launch_gate(cx);
    let host = cx.update(super::super::extension_host::install);
    host.update(cx, |host, cx| {
        host.start_product_startup(
            Some(config.path().to_path_buf()),
            Ok(extensions.path().to_path_buf()),
            cx,
        )
    });
    assert!(!wait_for_product_startup(cx));
    let report = cx
        .read(super::super::extension_host::startup_report)
        .unwrap();
    assert_eq!(
        report.report.entries[0].result,
        super::super::extension_load::LoadResult::Loaded
    );
    assert_eq!(cx.read(|cx| host.read(cx).lifecycle_count()), 0);
    assert!(cx.read(|cx| cx.global::<ApplicationWorkbenches>().0.borrow().is_empty()));
}

#[gpui::test]
fn installed_api_version_rejection_prevents_evaluation_and_preserves_dependency_diagnostics(
    cx: &mut TestAppContext,
) {
    let root = tempfile::tempdir().unwrap();
    const PROBE: &str = "import {commands, workbench} from 'knot'; await workbench.writeClipboardText('rejected entry evaluated'); await commands.register('rejected-entry', () => {});";
    for (suffix, version) in [
        ("missing", None),
        ("null", Some(serde_json::Value::Null)),
        ("string", Some(serde_json::json!("1"))),
        ("boolean", Some(serde_json::json!(true))),
        ("zero", Some(serde_json::json!(0))),
        ("fraction", Some(serde_json::json!(1.5))),
        ("unsupported", Some(serde_json::json!(2))),
    ] {
        let name = format!("@example/api-{suffix}");
        install_extension(root.path(), &name, &[], &[("main.js", PROBE)]);
        let manifest_path = root.path().join(&name).join("knot.jsonc");
        let mut manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
        match version {
            Some(version) => manifest["apiVersion"] = version,
            None => {
                manifest.as_object_mut().unwrap().remove("apiVersion");
            }
        }
        std::fs::write(manifest_path, manifest.to_string()).unwrap();
        install_extension(
            root.path(),
            &format!("@example/dependent-{suffix}"),
            &[&name],
            &[("main.js", PROBE)],
        );
    }
    install_extension(
        root.path(),
        "@example/duplicate",
        &[],
        &[("main.js", PROBE)],
    );
    install_extension(
        root.path(),
        "@example/duplicate-peer",
        &[],
        &[("main.js", PROBE)],
    );
    let duplicate_path = root.path().join("@example/duplicate-peer/knot.jsonc");
    std::fs::write(
        duplicate_path,
        r#"{"name":"@example/duplicate","apiVersion":2,"version":"1.0.0","main":"main.js"}"#,
    )
    .unwrap();
    install_extension(
        root.path(),
        "@example/independent",
        &[],
        &[(
            "main.js",
            "import {commands} from 'knot'; await commands.register('admission.independent', () => {});",
        )],
    );
    let documents = install_globals(cx);
    let host = cx.update(super::super::extension_host::install);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (_shell, _window) = product_window(document, model, cx);
    cx.update(|cx| {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string("admission sentinel".into()))
    });
    host.update(cx, |host, cx| {
        host.start_installed_extensions(root.path().to_path_buf(), cx)
    });
    let report = wait_for_extension_report(&host, cx);
    assert_eq!(report.report.entries.len(), 17);
    for suffix in [
        "missing",
        "null",
        "string",
        "boolean",
        "zero",
        "fraction",
        "unsupported",
    ] {
        let rejected = format!("@example/api-{suffix}");
        let dependent = format!("@example/dependent-{suffix}");
        for name in [&rejected, &dependent] {
            let entry = report
                .report
                .entries
                .iter()
                .find(|entry| entry.name.as_ref() == Some(name))
                .unwrap();
            assert!(
                matches!(&entry.result, super::super::extension_load::LoadResult::Failed(cause) if cause.contains("apiVersion") && cause.contains("expected") && cause.contains("declared")),
                "{entry:?}"
            );
        }
    }
    let duplicates = report
        .report
        .entries
        .iter()
        .filter(|entry| entry.name.as_deref() == Some("@example/duplicate"))
        .collect::<Vec<_>>();
    assert_eq!(duplicates.len(), 2);
    assert!(duplicates.iter().all(|entry| matches!(&entry.result, super::super::extension_load::LoadResult::Failed(cause) if cause.contains("duplicate package ID"))));
    assert!(duplicates.iter().any(|entry| matches!(&entry.result, super::super::extension_load::LoadResult::Failed(cause) if cause.contains("unsupported apiVersion"))));
    cx.read(|cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "admission sentinel"
        );
        assert!(
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .catalog()
                .borrow()
                .resolve("rejected-entry")
                .is_err()
        );
        assert!(
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .catalog()
                .borrow()
                .resolve("admission.independent")
                .is_ok()
        );
        assert_eq!(host.read(cx).lifecycle_count(), 1);
    });
}

#[gpui::test]
fn checked_authoring_examples_run_as_an_installed_version_one_package(cx: &mut TestAppContext) {
    let root = tempfile::tempdir().unwrap();
    install_extension(
        root.path(),
        "@example/guide",
        &[],
        &[
            (
                "main.js",
                "import {deactivateFeature} from './activation.js'; import './errors.js'; import {commands} from 'knot'; await commands.register('guide.test-deactivate', deactivateFeature);",
            ),
            (
                "activation.js",
                include_str!("../../../scripts/extension-api/examples/activation.js"),
            ),
            (
                "errors.js",
                include_str!("../../../scripts/extension-api/examples/errors.js"),
            ),
        ],
    );
    let documents = install_globals(cx);
    cx.update(super::bind_product_keys);
    let original_binding = cx.read(|cx| keymap_command(cx, "cmd-g", Some("editor")));
    let host = cx.update(super::super::extension_host::install);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    model.update(cx, |model, _| model.replace(0..0, "text").unwrap());
    let (shell, window) = product_window(document, model.clone(), cx);
    host.update(cx, |host, cx| {
        host.start_installed_extensions(root.path().to_path_buf(), cx)
    });
    let report = wait_for_extension_report(&host, cx);
    assert_eq!(
        report.report.entries[0].result,
        super::super::extension_load::LoadResult::Loaded
    );
    cx.read(|cx| {
        assert_eq!(
            keymap_command(cx, "cmd-g", Some("editor")).as_deref(),
            Some("guide.compose")
        )
    });
    let annotate = dispatch_product_command(&shell, window, "guide.compose", cx);
    assert_eq!(wait_for_command(annotate, cx), CommandOutcome::Completed);
    cx.read(|cx| assert_eq!(model.read(cx).resolved_contributions().len(), 1));
    let invalid = dispatch_product_command(&shell, window, "guide.insert", cx);
    assert!(matches!(
        wait_for_command(invalid, cx),
        CommandOutcome::InvalidArgument { .. }
    ));
    let target = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.focus_active_surface(window, cx);
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let dispatcher = cx.read(|cx| cx.global::<ApplicationProductCommands>().0.clone());
    let insertion = dispatcher.update(cx, |dispatcher, cx| {
        dispatcher.dispatch(
            Command {
                name: "guide.insert".into(),
                arguments: CommandArgumentValue::String("!".into()),
            },
            target,
            cx,
        )
    });
    assert_eq!(wait_for_command(insertion, cx), CommandOutcome::Completed);
    cx.read(|cx| assert_eq!(model.read(cx).text(), "!text"));
    let native_child = dispatch_product_command(&shell, window, "guide.select-all", cx);
    assert_eq!(
        wait_for_command(native_child, cx),
        CommandOutcome::Completed
    );
    let native_copy = dispatch_product_command(&shell, window, super::COPY_COMMAND, cx);
    assert_eq!(wait_for_command(native_copy, cx), CommandOutcome::Completed);
    cx.read(|cx| assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), "!text"));
    let tree = wait_for_contributed_view(&shell, "guide.outline", cx);
    wait_for_api_condition(cx, |cx| tree.read(cx).root_labels() == ["Guide"]);
    let copy_editor = cx.read(|cx| {
        shell
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .and_then(|pane| pane.active_tab().editor().cloned())
            .expect("document editor is focused before the tree")
    });
    wait_for_api_condition(cx, |cx| {
        cx.global::<ApplicationProductCommands>()
            .0
            .read(cx)
            .catalog()
            .borrow()
            .resolve_view(tree.read(cx).instance_id(), "copy")
            .is_some()
    });
    cx.update_window(window, |_, window, cx| {
        tree.update(cx, |tree, cx| assert!(tree.select_item("root", cx)));
        tree.focus_handle(cx).focus(window);
    })
    .unwrap();
    let copy = dispatch_tree_copy(&shell, window, cx);
    assert_eq!(wait_for_command(copy, cx), CommandOutcome::Completed);
    cx.read(|cx| assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), "Guide"));
    wait_for_api_condition(cx, |cx| {
        super::super::extension_host::completion_provider_count(cx) == 1
    });
    cx.update_window(window, |_, window, cx| {
        copy_editor.read(cx).focus_handle(cx).focus(window);
        shell.update(cx, |shell, cx| shell.sync_focused_pane(window, cx));
    })
    .unwrap();
    let completion = dispatch_product_command(&shell, window, super::SHOW_COMPLETIONS_COMMAND, cx);
    assert_eq!(wait_for_command(completion, cx), CommandOutcome::Completed);
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
    wait_for_api_condition(cx, |cx| {
        editor
            .read(cx)
            .completion_state()
            .is_some_and(|(items, pending, failures, _)| {
                items == 1 && pending == 0 && failures == 0
            })
    });
    let cleanup = dispatch_product_command(&shell, window, "guide.test-deactivate", cx);
    assert_eq!(wait_for_command(cleanup, cx), CommandOutcome::Completed);
    cx.read(|cx| {
        assert!(!tree.read(cx).lifecycle_state().0);
        assert!(model.read(cx).resolved_contributions().is_empty());
        assert_eq!(
            keymap_command(cx, "cmd-g", Some("editor")),
            original_binding
        );
        assert_eq!(
            super::super::extension_host::completion_provider_count(cx),
            0
        );
        assert!(
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .catalog()
                .borrow()
                .resolve("guide.compose")
                .is_err()
        );
    });
}

#[gpui::test]
fn on_disk_extensions_load_in_order_invoke_dependencies_and_isolate_failures(
    cx: &mut TestAppContext,
) {
    let root = tempfile::tempdir().unwrap();
    install_extension(
        root.path(),
        "@example/base",
        &[],
        &[
            (
                "dist/main.js",
                r#"
            import * as knot from 'knot'; const { commands, editor, workbench } = knot;
            import { prefix } from './words.js';
            await commands.register('example.base', async ({ buffer }) => {
                const snapshot = await buffer.snapshot();
                await buffer.applyEdits([{
                    range: { startByteOffset: 0, endByteOffset: 0 },
                    text: prefix,
                }], { ifRevision: snapshot.revision });
            });
            await editor.registerCompletionProvider('example-provider', {
                provideCompletions() { return [{ label: 'Installed', insertText: 'Installed' }]; }
            });
        "#,
            ),
            ("dist/words.js", "export const prefix = 'base ';"),
        ],
    );
    install_extension(
        root.path(),
        "@example/dependent",
        &["@example/base"],
        &[(
            "main.js",
            r#"
            import * as knot from 'knot'; const { commands } = knot;
            await commands.register('example.dependent', async () => {
                await commands.invoke('example.base', null);
            });
        "#,
        )],
    );
    install_extension(
        root.path(),
        "@example/broken",
        &[],
        &[(
            "main.js",
            r#"
            import * as knot from 'knot'; const { commands, editor, workbench } = knot;
            await commands.register('example.shared', async () => {});
            await editor.registerCompletionProvider('broken-provider', {
                provideCompletions() { return []; }
            });
            await workbench.registerView({
                id: 'example.broken.outline',
                title: 'Broken outline',
                content: { kind: 'tree', provider: { getChildren() { return []; } } },
            });
            await commands.registerForView('example.broken.outline', 'copy', async () => {});
            throw new Error('broken entry');
        "#,
        )],
    );
    install_extension(
        root.path(),
        "@example/blocked",
        &["@example/broken"],
        &[("main.js", "throw new Error('must not run');")],
    );
    install_extension(
        root.path(),
        "@example/independent",
        &[],
        &[(
            "main.js",
            r#"
            import * as knot from 'knot'; const { commands } = knot;
            await commands.register('example.shared', async () => {});
        "#,
        )],
    );
    let invalid = root.path().join("@example/invalid");
    std::fs::create_dir_all(&invalid).unwrap();
    std::fs::write(invalid.join("knot.jsonc"), "{ invalid").unwrap();

    let documents = install_globals(cx);
    let host = cx.update(super::super::extension_host::install);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model.clone(), cx);
    host.update(cx, |host, cx| {
        host.start_installed_extensions(root.path().to_path_buf(), cx)
    });
    let report = wait_for_extension_report(&host, cx);
    cx.read(|cx| assert!(shell.read(cx).contributed_views.is_empty()));
    let result = |name| {
        report
            .report
            .entries
            .iter()
            .find(|entry| entry.name.as_deref() == Some(name))
            .unwrap()
            .result
            .clone()
    };
    assert_eq!(
        result("@example/base"),
        super::super::extension_load::LoadResult::Loaded
    );
    assert_eq!(
        result("@example/dependent"),
        super::super::extension_load::LoadResult::Loaded
    );
    assert_eq!(
        result("@example/independent"),
        super::super::extension_load::LoadResult::Loaded
    );
    assert!(
        matches!(result("@example/broken"), super::super::extension_load::LoadResult::Failed(cause) if cause.contains("broken entry"))
    );
    assert!(
        matches!(result("@example/blocked"), super::super::extension_load::LoadResult::Failed(cause) if cause.contains("@example/broken"))
    );
    assert!(report.report.entries.iter().any(|entry| entry.directory == invalid && matches!(&entry.result, super::super::extension_load::LoadResult::Failed(cause) if cause.contains("invalid knot.jsonc"))));
    let definitions = cx.read(|cx| {
        cx.global::<ApplicationProductCommands>()
            .0
            .read(cx)
            .definitions()
            .map(|definition| definition.name.to_string())
            .collect::<Vec<_>>()
    });
    assert!(definitions.contains(&"example.base".to_owned()));
    assert!(definitions.contains(&"example.dependent".to_owned()));
    assert!(definitions.contains(&"example.shared".to_owned()));
    assert_eq!(
        definitions
            .iter()
            .filter(|name| name.as_str() == "example.shared")
            .count(),
        1
    );
    cx.read(|cx| assert!(shell.read(cx).contributed_views.is_empty()));
    let mut execution = dispatch_product_command(&shell, window, "example.dependent", cx);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        cx.run_until_parked();
        match execution.completion.try_recv() {
            Ok(outcome) => {
                assert_eq!(outcome, CommandOutcome::Completed);
                break;
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "dependent command did not finish"
                );
                std::thread::yield_now();
            }
            Err(error) => panic!("dependent command completion closed: {error}"),
        }
    }
    assert_eq!(cx.read(|cx| model.read(cx).text()), "base ");
    assert_eq!(
        cx.read(super::super::extension_host::completion_provider_count),
        1
    );

    let mut report_command =
        dispatch_product_command(&shell, window, SHOW_EXTENSION_REPORT_COMMAND, cx);
    cx.run_until_parked();
    assert_eq!(
        report_command.completion.try_recv().unwrap(),
        CommandOutcome::Completed
    );
    assert!(cx.read(|cx| shell.read(cx).extension_report_open));
}

#[gpui::test]
fn extension_startup_shows_known_outcomes_while_a_later_entry_is_pending(cx: &mut TestAppContext) {
    let root = tempfile::tempdir().unwrap();
    install_extension(
        root.path(),
        "@example/a-ready",
        &[],
        &[("main.js", "export {};")],
    );
    install_extension(
        root.path(),
        "@example/z-pending",
        &[],
        &[("main.js", "await new Promise(() => {});")],
    );
    let invalid = root.path().join("@example/invalid");
    std::fs::create_dir_all(&invalid).unwrap();
    std::fs::write(invalid.join("knot.jsonc"), "{ invalid").unwrap();

    install_globals(cx);
    let host = cx.update(super::super::extension_host::install);
    host.update(cx, |host, cx| {
        host.start_installed_extensions(root.path().to_path_buf(), cx)
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        cx.run_until_parked();
        let report = cx
            .read(super::super::extension_host::startup_report)
            .unwrap();
        if report.report.entries.len() == 2 {
            assert_eq!(
                report.state,
                super::super::extension_host::StartupState::Loading
            );
            assert_eq!(report.report.entries[0].directory, invalid);
            assert!(matches!(
                &report.report.entries[0].result,
                super::super::extension_load::LoadResult::Failed(cause)
                    if cause.contains("invalid knot.jsonc")
            ));
            assert_eq!(
                report.report.entries[1].name.as_deref(),
                Some("@example/a-ready")
            );
            assert_eq!(
                report.report.entries[1].result,
                super::super::extension_load::LoadResult::Loaded
            );
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "startup outcomes were not published"
        );
        std::thread::yield_now();
    }
}

#[gpui::test]
fn extension_startup_rescans_changed_directories_and_cleans_old_registrations(
    cx: &mut TestAppContext,
) {
    let root = tempfile::tempdir().unwrap();
    install_extension(
        root.path(),
        "@example/old",
        &[],
        &[(
            "main.js",
            r#"
            import * as knot from 'knot'; const { commands, editor } = knot;
            await commands.register('example.old', async () => {});
            await editor.registerCompletionProvider('old-provider', {
                provideCompletions() { return []; }
            });
        "#,
        )],
    );
    install_globals(cx);
    let host = cx.update(super::super::extension_host::install);
    host.update(cx, |host, cx| {
        host.start_installed_extensions(root.path().to_path_buf(), cx)
    });
    let first = wait_for_extension_report(&host, cx);
    assert_eq!(first.report.entries.len(), 1);
    assert_eq!(
        first.report.entries[0].name.as_deref(),
        Some("@example/old")
    );
    assert_eq!(
        cx.read(super::super::extension_host::completion_provider_count),
        1
    );

    std::fs::remove_dir_all(root.path().join("@example/old")).unwrap();
    install_extension(
        root.path(),
        "@example/new",
        &[],
        &[(
            "main.js",
            r#"
            import * as knot from 'knot'; const { commands } = knot;
            await commands.register('example.new', async () => {});
        "#,
        )],
    );
    host.update(cx, |host, cx| {
        host.start_installed_extensions(root.path().to_path_buf(), cx)
    });
    let second = wait_for_extension_report(&host, cx);
    assert_eq!(second.report.entries.len(), 1);
    assert_eq!(
        second.report.entries[0].name.as_deref(),
        Some("@example/new")
    );
    assert_eq!(
        second.report.entries[0].result,
        super::super::extension_load::LoadResult::Loaded
    );
    assert_eq!(
        cx.read(super::super::extension_host::completion_provider_count),
        0
    );
    let definitions = cx.read(|cx| {
        cx.global::<ApplicationProductCommands>()
            .0
            .read(cx)
            .definitions()
            .map(|definition| definition.name.to_string())
            .collect::<Vec<_>>()
    });
    assert!(definitions.contains(&"example.new".to_owned()));
    assert!(!definitions.contains(&"example.old".to_owned()));
}

#[gpui::test]
fn checked_in_extension_example_runs_through_the_product_command_bridge(cx: &mut TestAppContext) {
    let storage = tempfile::tempdir().unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/extensions");
    let documents = install_globals(cx);
    let host = cx.update(super::super::extension_host::install);
    host.update(cx, |host, cx| {
        host.set_test_storage_root(storage.path().to_owned(), cx)
    });
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model.clone(), cx);
    host.update(cx, |host, cx| {
        host.start_installed_extensions(root.clone(), cx)
    });
    let report = wait_for_extension_report(&host, cx);
    assert_eq!(report.report.entries.len(), 5);
    assert!(
        report
            .report
            .entries
            .iter()
            .all(|entry| entry.result == super::super::extension_load::LoadResult::Loaded)
    );
    let mut execution = dispatch_product_command(&shell, window, "example.greet", cx);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        cx.run_until_parked();
        match execution.completion.try_recv() {
            Ok(outcome) => {
                assert_eq!(outcome, CommandOutcome::Completed);
                break;
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "example command did not finish"
                );
                std::thread::yield_now();
            }
            Err(error) => panic!("example command completion closed: {error}"),
        }
    }
    assert_eq!(cx.read(|cx| model.read(cx).text()), "Hello ");
    for activations in 1..=2 {
        if activations == 2 {
            host.update(cx, |host, cx| {
                host.start_installed_extensions(root.clone(), cx)
            });
            let report = wait_for_extension_report(&host, cx);
            assert!(
                report
                    .report
                    .entries
                    .iter()
                    .all(|entry| entry.result == super::super::extension_load::LoadResult::Loaded)
            );
        }
        let execution = dispatch_product_command(&shell, window, "example.storage-report", cx);
        assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
        let report: serde_json::Value = cx.read(|cx| {
            serde_json::from_str(&cx.read_from_clipboard().unwrap().text().unwrap()).unwrap()
        });
        assert_eq!(report["activations"]["found"], true);
        assert_eq!(
            report["activations"]["value"].as_f64(),
            Some(f64::from(activations))
        );
        assert_eq!(
            report["example"]["value"]["optional"],
            serde_json::Value::Null
        );
        assert_eq!(report["missing"], serde_json::json!({"found": false}));
        assert_eq!(
            report["keys"],
            serde_json::json!(["activations", "example"])
        );
    }
}

struct FixedOpenDialog(OpenDialogOutcome);

impl ProductOpenDialog for FixedOpenDialog {
    fn select(&self) -> OpenDialogFuture {
        let outcome = self.0.clone();
        Box::pin(async move { outcome })
    }
}

struct FixedSaveDialog(SaveDialogOutcome);

impl ProductSaveDialog for FixedSaveDialog {
    fn select(&self, _: gpui::SharedString) -> SaveDialogFuture {
        let outcome = self.0.clone();
        Box::pin(async move { outcome })
    }
}

#[gpui::test]
fn product_catalog_registers_every_slice_command(cx: &mut TestAppContext) {
    install_globals(cx);
    cx.read(|cx| {
        let dispatcher = cx.global::<ApplicationProductCommands>().0.read(cx);
        let mut actual = dispatcher
            .definitions()
            .map(|definition| definition.name.to_string())
            .collect::<Vec<_>>();
        let mut expected = super::super::product_commands::product_command_names()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        actual.sort();
        expected.sort();
        assert_eq!(actual, expected);
    });
}

#[gpui::test]
fn pooled_fixture_commands_and_completions_use_the_product_target(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model.clone(), cx);
    let host = cx.update(|cx| {
        let host = super::super::extension_host::install(cx);
        host.update(cx, |host, cx| host.start_test_fixture("product", cx));
        host
    });

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        cx.run_until_parked();
        let registered = cx.read(|cx| {
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .definitions()
                .any(|definition| definition.name.as_ref() == "knot.fixture.primary")
        });
        if registered {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "fixture command did not enter the product catalog"
        );
        std::thread::yield_now();
    }

    let mut execution = dispatch_product_command(&shell, window, "knot.fixture.primary", cx);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let outcome = loop {
        cx.run_until_parked();
        match execution.completion.try_recv() {
            Ok(outcome) => break outcome,
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                assert!(std::time::Instant::now() < deadline);
                std::thread::yield_now();
            }
            Err(error) => panic!("fixture command completion closed: {error}"),
        }
    };
    assert_eq!(outcome, CommandOutcome::Completed);
    assert_eq!(cx.read(|cx| model.read(cx).text()), "fixture ");

    let mut completion = dispatch_product_command(
        &shell,
        window,
        super::super::product_commands::SHOW_COMPLETIONS_COMMAND,
        cx,
    );
    cx.run_until_parked();
    assert_eq!(
        completion.completion.try_recv().unwrap(),
        CommandOutcome::Completed
    );

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
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        cx.run_until_parked();
        if cx.read(|cx| {
            editor
                .read(cx)
                .completion_state()
                .is_some_and(|(items, pending, failures, _)| {
                    items == 2 && pending == 0 && failures == 0
                })
        }) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "fixture completion items did not reach the product editor"
        );
        std::thread::yield_now();
    }

    drop(host);
    cx.update(|cx| {
        drop(cx.remove_global::<super::super::extension_host::ApplicationExtensionHost>())
    });
    cx.run_until_parked();
}

fn wait_for_command(
    mut execution: crate::app::CommandExecution,
    cx: &mut TestAppContext,
) -> CommandOutcome {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        cx.run_until_parked();
        match execution.completion.try_recv() {
            Ok(outcome) => return outcome,
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "command did not settle"
                );
                std::thread::yield_now();
            }
            Err(error) => panic!("command completion closed: {error}"),
        }
    }
}

fn wait_for_contributed_view(
    shell: &Entity<ProductShell>,
    id: &str,
    cx: &mut TestAppContext,
) -> Entity<crate::app::tree_view::TreeView> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        cx.run_until_parked();
        if let Some(view) = cx.read(|cx| {
            shell
                .read(cx)
                .contributed_views
                .get(id)
                .map(|view| view.tree().clone())
        }) {
            return view;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "contributed view did not attach"
        );
        std::thread::yield_now();
    }
}

fn dispatch_tree_command(
    shell: &Entity<ProductShell>,
    window: gpui::AnyWindowHandle,
    name: &'static str,
    cx: &mut TestAppContext,
) -> crate::app::CommandExecution {
    let target = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let dispatcher = cx.read(|cx| cx.global::<ApplicationProductCommands>().0.clone());
    dispatcher.update(cx, |dispatcher, cx| {
        dispatcher.dispatch(
            Command {
                name: name.into(),
                arguments: CommandArgumentValue::Null,
            },
            target,
            cx,
        )
    })
}

fn dispatch_tree_copy(
    shell: &Entity<ProductShell>,
    window: gpui::AnyWindowHandle,
    cx: &mut TestAppContext,
) -> crate::app::CommandExecution {
    dispatch_tree_command(shell, window, super::COPY_COMMAND, cx)
}

fn wait_for_api_condition(cx: &mut TestAppContext, mut ready: impl FnMut(&gpui::App) -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        cx.run_until_parked();
        if cx.read(&mut ready) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "public API fixture did not settle"
        );
        std::thread::yield_now();
    }
}

#[gpui::test]
fn public_api_capture_survives_focus_and_nested_handlers_without_leaking_to_callbacks(
    cx: &mut TestAppContext,
) {
    use crate::host::lifecycle::ExtensionKey;
    const SOURCE: &str = r#"
import { commands, editor, workbench } from "knot";
async function expectCode(operation, code) {
  try { await operation(); } catch (error) { if (error.code === code) return; throw error; }
  throw new Error(`expected ${code}`);
}
await expectCode(() => editor.activeBuffer(), "ContextUnavailable");
await expectCode(() => workbench.selectedText(), "ContextUnavailable");
await expectCode(() => commands.invoke("file.new"), "ContextUnavailable");
await workbench.writeClipboardText("activation");
let origin;
let previous;
await workbench.registerView({
  id: "example.outline",
  title: "Context-free",
  content: { kind: "tree", provider: {
async getChildren(parentId) {
  await expectCode(() => editor.activeBuffer(), "ContextUnavailable");
  await expectCode(() => workbench.selectedText(), "ContextUnavailable");
  return parentId === null ? [{ id: "provider", label: "context-free", collapsibleState: "none" }] : [];
},
  } },
});
await editor.registerCompletionProvider("context-free", {
  async provideCompletions(context) {
await expectCode(() => editor.activeBuffer(), "ContextUnavailable");
await expectCode(() => workbench.selectedText(), "ContextUnavailable");
await expectCode(() => editor.activeBuffer(origin.target), "InvalidTarget");
await context.buffer.snapshot();
return [{ label: "context-free", insertText: "context-free" }];
  },
});
await commands.register("api.inline", async (context) => {
  if (context.buffer !== origin.buffer || context.target.window !== origin.target.window) throw new Error("inline capture");
  if (await editor.activeBuffer(origin.target) !== origin.buffer) throw new Error("inherited authority");
});
await commands.register("api.capture", async (context) => {
  if (!Object.isFrozen(context.target) || typeof context.target.window !== "string") throw new Error("capture shape");
  if (previous) await expectCode(() => editor.activeBuffer(previous), "InvalidTarget");
  previous = context.target;
  origin = context;
  await expectCode(() => editor.activeBuffer({ ...context.target }), "InvalidTarget");
  await expectCode(() => workbench.selectedText(context.target), "UnsupportedOperation");
  await workbench.writeClipboardText("awaiting");
  if (await editor.activeBuffer() !== context.buffer || await editor.activeBuffer(context.target) !== context.buffer) throw new Error("focus redirected capture");
  const snapshot = await context.buffer.snapshot();
  if (snapshot.text !== "first") throw new Error(`wrong buffer: ${snapshot.text}`);
  if ((await commands.invoke("api.inline")).kind !== "completed") throw new Error("inline failed");
  if ((await commands.invoke("api.cross", { window: context.target.window })).kind !== "completed") throw new Error("cross failed");
  await context.buffer.onDidChange(async (change) => {
await expectCode(() => editor.activeBuffer(), "ContextUnavailable");
await expectCode(() => editor.activeBuffer(context.target), "InvalidTarget");
await expectCode(() => workbench.selectedText(), "ContextUnavailable");
await workbench.writeClipboardText((await change.buffer.snapshot()).text);
  });
});
"#;
    const CROSS: &str = r#"
import { commands, editor, workbench } from "knot";
await commands.register("api.cross", async (context) => {
  if (context.target.window !== context.arguments.window || await editor.activeBuffer(context.target) !== context.buffer) throw new Error("cross capture");
  await workbench.writeClipboardText((await context.buffer.snapshot()).text);
});
"#;
    let documents = install_globals(cx);
    let host = cx.update(super::super::extension_host::install);
    let first_document = cx.update(|cx| create_untitled_document(&documents, cx));
    let first_model = cx.read(|cx| {
        documents
            .read(cx)
            .get(first_document)
            .unwrap()
            .model()
            .clone()
    });
    first_model.update(cx, |model, _| model.replace(0..0, "first").unwrap());
    let (first_shell, first_window) = product_window(first_document, first_model.clone(), cx);
    let second_document = cx.update(|cx| create_untitled_document(&documents, cx));
    let second_model = cx.read(|cx| {
        documents
            .read(cx)
            .get(second_document)
            .unwrap()
            .model()
            .clone()
    });
    second_model.update(cx, |model, _| model.replace(0..0, "second").unwrap());
    let (second_shell, second_window) = product_window(second_document, second_model, cx);
    for (id, source) in [(701, SOURCE), (702, CROSS)] {
        host.update(cx, |host, cx| {
            let execution = host
                .load_test_source(
                    ExtensionKey::new(ExtensionId::new(id), ExtensionLifecycleId::new(1)),
                    source,
                )
                .unwrap();
            cx.spawn(async move |_, _| {
                execution.await.unwrap();
            })
            .detach();
        });
    }
    wait_for_api_condition(cx, |cx| {
        let catalog = cx
            .global::<ApplicationProductCommands>()
            .0
            .read(cx)
            .catalog();
        let catalog = catalog.borrow();
        catalog.resolve("api.capture").is_ok() && catalog.resolve("api.cross").is_ok()
    });
    wait_for_api_condition(cx, |cx| {
        first_shell
            .read(cx)
            .contributed_views
            .get("example.outline")
            .is_some_and(|view| view.tree().read(cx).root_labels() == ["context-free"])
    });
    // Capture before the palette takes focus, then admit the saved origin.
    let origin = cx
        .update_window(first_window, |_, window, cx| {
            first_shell.update(cx, |shell, cx| {
                shell.focus_active_surface(window, cx);
                shell.open_command_palette(window, cx);
                shell.command_palette.as_ref().unwrap().read(cx).origin()
            })
        })
        .unwrap();
    for _ in 0..2 {
        host.update(cx, |host, _| host.hold_clipboard_writes());
        let dispatcher = cx.read(|cx| cx.global::<ApplicationProductCommands>().0.clone());
        let execution = dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: "api.capture".into(),
                    arguments: CommandArgumentValue::Null,
                },
                origin.clone(),
                cx,
            )
        });
        wait_for_api_condition(cx, |cx| host.read(cx).pending_view_request_count() == 1);
        cx.update_window(second_window, |_, window, cx| {
            second_shell.read(cx).focus_active_surface(window, cx)
        })
        .unwrap();
        host.update(cx, |host, cx| host.release_view_requests(cx));
        assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
        cx.read(|cx| assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), "first"));
    }
    let editor = cx.read(|cx| {
        first_shell
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
    let completion = dispatch_product_command(
        &first_shell,
        first_window,
        super::SHOW_COMPLETIONS_COMMAND,
        cx,
    );
    assert_eq!(wait_for_command(completion, cx), CommandOutcome::Completed);
    wait_for_api_condition(cx, |cx| {
        editor
            .read(cx)
            .completion_state()
            .is_some_and(|(items, pending, failures, _)| {
                items == 1 && pending == 0 && failures == 0
            })
    });
    let (_shared_shell, _shared_window) = product_window(first_document, first_model.clone(), cx);
    cx.update_window(first_window, |_, window, _| window.remove_window())
        .unwrap();
    drop(first_shell);
    first_model.update(cx, |model, cx| {
        model.replace(0..5, "shared callback").unwrap();
        cx.notify();
    });
    wait_for_api_condition(cx, |cx| {
        cx.read_from_clipboard()
            .and_then(|item| item.text())
            .as_deref()
            == Some("shared callback")
    });
}

#[gpui::test]
fn public_api_context_rejects_replaced_closed_and_transferred_presentations(
    cx: &mut TestAppContext,
) {
    use crate::host::lifecycle::ExtensionKey;
    const SOURCE: &str = r#"
import { commands, editor, workbench } from "knot";
await commands.register("api.stale", async (context) => {
  await workbench.writeClipboardText("waiting");
  for (const operation of [() => editor.activeBuffer(), () => editor.activeBuffer(context.target), () => workbench.selectedText(context.target)]) {
let rejected = false;
try { await operation(); } catch (error) { if (error.code !== "InvalidTarget") throw error; rejected = true; }
if (!rejected) throw new Error("stale presentation accepted");
  }
  // An explicit live buffer remains usable after its presentation changes.
  await context.buffer.snapshot();
  await workbench.writeClipboardText("InvalidTarget");
});
await commands.register("api.nontext", async (context) => {
  if (context.buffer !== null || await editor.activeBuffer() !== null || await editor.activeBuffer(context.target) !== null) throw new Error("nontext buffer");
  try { await workbench.selectedText(); } catch (error) { if (error.code === "UnsupportedOperation") return; throw error; }
  throw new Error("unsupported selection accepted");
});
"#;
    let documents = install_globals(cx);
    let host = cx.update(super::super::extension_host::install);
    host.update(cx, |host, cx| {
        let execution = host
            .load_test_source(
                ExtensionKey::new(ExtensionId::new(703), ExtensionLifecycleId::new(1)),
                SOURCE,
            )
            .unwrap();
        cx.spawn(async move |_, _| {
            execution.await.unwrap();
        })
        .detach();
    });
    wait_for_api_condition(cx, |cx| {
        cx.global::<ApplicationProductCommands>()
            .0
            .read(cx)
            .catalog()
            .borrow()
            .resolve("api.nontext")
            .is_ok()
    });
    for change in ["replace", "close", "transfer"] {
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window) = product_window(document, model.clone(), cx);
        host.update(cx, |host, _| host.hold_clipboard_writes());
        let execution = dispatch_product_command(&shell, window, "api.stale", cx);
        wait_for_api_condition(cx, |cx| host.read(cx).pending_view_request_count() == 1);
        let target = cx
            .update_window(window, |_, window, cx| {
                shell.update(cx, |shell, cx| {
                    shell.capture_command_target(window, cx).unwrap()
                })
            })
            .unwrap();
        match change {
            "replace" => {
                let workbench = cx.read(|cx| shell.read(cx).workbench.clone());
                workbench.update(cx, |workbench, cx| {
                    assert!(workbench.replace_tab_document(
                        target.pane,
                        target.tab,
                        document,
                        model.clone(),
                        cx
                    ))
                });
            }
            "close" => {
                cx.update_window(window, |_, window, _| window.remove_window())
                    .unwrap();
            }
            "transfer" => {
                let destination_document = cx.update(|cx| create_untitled_document(&documents, cx));
                let destination_model = cx.read(|cx| {
                    documents
                        .read(cx)
                        .get(destination_document)
                        .unwrap()
                        .model()
                        .clone()
                });
                let (destination_shell, destination_window) =
                    product_window(destination_document, destination_model, cx);
                let source = cx.read(|cx| {
                    shell
                        .read(cx)
                        .capture_tab_relocation(target.pane, target.tab, cx)
                        .unwrap()
                });
                let destination = cx.read(|cx| {
                    let workbench = &destination_shell.read(cx).workbench;
                    let pane = workbench.read(cx).focused_pane().unwrap();
                    super::relocate::TabRelocationDestination {
                        window: destination_window,
                        workbench: workbench.downgrade(),
                        tab: crate::app::workbench::TabMoveDestination::At {
                            anchor: workbench
                                .read(cx)
                                .capture_tab_move(pane.id(), pane.active_tab().id())
                                .unwrap(),
                            placement: crate::app::workbench::TabMovePlacement::End,
                        },
                    }
                });
                let outcome = cx
                    .update_window(window, |_, window, cx| {
                        shell.update(cx, |shell, cx| {
                            shell.relocate_tab_to(&source, &destination, window, cx)
                        })
                    })
                    .unwrap();
                assert_eq!(outcome, CommandOutcome::Completed);
            }
            _ => unreachable!(),
        }
        host.update(cx, |host, cx| host.release_view_requests(cx));
        assert_eq!(
            wait_for_command(execution, cx),
            CommandOutcome::InvalidTarget
        );
        cx.read(|cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "InvalidTarget"
            )
        });
    }
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    cx.update_window(window, |_, window, cx| {
        shell.update(cx, |shell, cx| {
            let (session, view) = shell.create_terminal(cx);
            let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
            let tab = shell.workbench.update(cx, |workbench, _| {
                workbench.open_terminal_tab(pane, session, view).unwrap()
            });
            shell.activate_tab(pane, tab, window, cx);
        })
    })
    .unwrap();
    let execution = dispatch_product_command(&shell, window, "api.nontext", cx);
    assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
}

#[gpui::test]
fn installed_application_observer_tracks_zero_window_activation_and_native_commits(
    cx: &mut TestAppContext,
) {
    fn report(
        shell: &Entity<ProductShell>,
        window: gpui::AnyWindowHandle,
        cx: &mut TestAppContext,
    ) -> serde_json::Value {
        let execution = dispatch_product_command(shell, window, "example.application-state", cx);
        assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
        cx.read(|cx| {
            serde_json::from_str(&cx.read_from_clipboard().unwrap().text().unwrap()).unwrap()
        })
    }
    let root = tempfile::tempdir().unwrap();
    install_extension(
        root.path(),
        "@example/application-state",
        &[],
        &[(
            "main.js",
            include_str!("../../../examples/extensions/@example/application-state/dist/main.js"),
        )],
    );
    install_extension(
        root.path(),
        "@example/broken-observer",
        &[],
        &[(
            "main.js",
            "import {workbench} from 'knot'; await workbench.observe(() => { throw new Error('failed activation listener ran'); }); throw new Error('activation failed');",
        )],
    );
    let documents = install_globals(cx);
    let host = cx.update(super::super::extension_host::install);
    host.update(cx, |host, cx| {
        host.start_installed_extensions(root.path().to_path_buf(), cx)
    });
    let startup = wait_for_extension_report(&host, cx);
    assert_eq!(
        startup.report.entries[0].result,
        super::super::extension_load::LoadResult::Loaded
    );
    assert!(matches!(
        startup.report.entries[1].result,
        super::super::extension_load::LoadResult::Failed(_)
    ));
    cx.read(|cx| assert_eq!(host.read(cx).application_observer_count(), 1));
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let id = cx.read(|cx| {
        documents
            .read(cx)
            .get(document)
            .unwrap()
            .identity()
            .to_string()
    });
    let (first, first_window) = product_window(document, model.clone(), cx);
    cx.update_window(first_window, |_, window, cx| {
        window.activate_window();
        first.read(cx).focus_active_editor(window, cx);
    })
    .unwrap();
    let initial = report(&first, first_window, cx);
    assert_eq!(initial["initial"]["documents"], 0);
    assert_eq!(initial["initial"]["windows"], 0);
    assert_eq!(initial["documents"].as_array().unwrap().len(), 1);
    assert!(
        initial["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["type"] == "documentOpened" && event["document"] == id)
    );
    model.update(cx, |model, cx| {
        model.replace(0..0, "edit").unwrap();
        cx.notify();
    });
    let edited = report(&first, first_window, cx);
    assert_eq!(edited["documents"][0]["dirty"], true);
    assert_eq!(edited["documents"][0]["revision"], 1);
    model.update(cx, |model, cx| {
        model.undo().unwrap();
        cx.notify();
    });
    let split = dispatch_product_command(&first, first_window, super::SPLIT_HORIZONTAL_COMMAND, cx);
    assert_eq!(wait_for_command(split, cx), CommandOutcome::Completed);
    let (second, second_window) = product_window(document, model.clone(), cx);
    first.update(cx, |shell, cx| {
        shell.install_workspace(ResourceUri::parse("mem://observer/").unwrap(), cx)
    });
    let source = cx.read(|cx| {
        let shell = first.read(cx);
        let pane = shell.workbench.read(cx).focused_pane().unwrap();
        shell
            .capture_tab_relocation(pane.id(), pane.active_tab_id(), cx)
            .unwrap()
    });
    let destination = cx.read(|cx| {
        let workbench = &second.read(cx).workbench;
        let pane = workbench.read(cx).focused_pane().unwrap();
        super::relocate::TabRelocationDestination {
            window: second_window,
            workbench: workbench.downgrade(),
            tab: crate::app::workbench::TabMoveDestination::At {
                anchor: workbench
                    .read(cx)
                    .capture_tab_move(pane.id(), pane.active_tab_id())
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
    let updated = report(&first, first_window, cx);
    let mut tabs = updated["windows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|window| {
            window["panes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|pane| pane["tabs"].as_array().unwrap().len())
                .sum::<usize>()
        })
        .collect::<Vec<_>>();
    tabs.sort();
    assert_eq!(tabs, [2, 2]);
    assert_eq!(updated["documents"][0]["dirty"], false);
    assert_eq!(updated["documents"][0]["revision"], 2);
    assert_eq!(updated["windows"].as_array().unwrap().len(), 2);
    let events = updated["events"].as_array().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event["type"] == "documentOpened" && event["document"] == id)
            .count(),
        1
    );
    for kind in [
        "documentChanged",
        "windowOpened",
        "presentationsChanged",
        "workspaceChanged",
        "activeTargetChanged",
    ] {
        assert!(
            events.iter().any(|event| event["type"] == kind),
            "missing {kind}: {events:?}"
        );
    }
    let mut previous = updated["initial"]["watermark"].as_u64().unwrap();
    for event in events {
        let sequence = event["sequence"].as_u64().unwrap();
        assert!(sequence > previous);
        previous = sequence;
    }
    let installed_root = updated["windows"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|window| window["roots"].as_array().unwrap().first())
        .unwrap()["id"]
        .clone();
    first.update(cx, |shell, cx| {
        shell.install_workspace(ResourceUri::parse("mem://observer/").unwrap(), cx)
    });
    let retained = report(&first, first_window, cx);
    assert!(
        retained["windows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|window| window["generation"] == 2 && window["roots"][0]["id"] == installed_root)
    );
    first.update(cx, |shell, cx| {
        shell.replace_workspace_roots(Vec::new(), cx).unwrap()
    });
    let cleared = report(&first, first_window, cx);
    assert!(
        cleared["windows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|window| window["roots"].as_array().unwrap().is_empty())
    );
    let before_rejection = cleared["events"].as_array().unwrap().len();
    first.update(cx, |shell, cx| {
        assert!(
            shell
                .replace_workspace_roots(
                    vec![
                        ResourceUri::parse("mem://one/").unwrap(),
                        ResourceUri::parse("mem://two/").unwrap()
                    ],
                    cx
                )
                .is_err()
        )
    });
    assert_eq!(
        report(&first, first_window, cx)["events"]
            .as_array()
            .unwrap()
            .len(),
        before_rejection
    );
    first.update(cx, |shell, cx| {
        shell.install_workspace(ResourceUri::parse("mem://observer/").unwrap(), cx)
    });
    let reinstalled = report(&first, first_window, cx);
    assert!(
        reinstalled["windows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|window| window["generation"] == 4
                && window["roots"][0]["id"] != installed_root
                && !window["roots"].as_array().unwrap().is_empty())
    );
    for (shell, window) in [(&second, second_window), (&first, first_window)] {
        let close = dispatch_product_command(shell, window, super::CLOSE_WINDOW_COMMAND, cx);
        assert_eq!(wait_for_command(close, cx), CommandOutcome::Completed);
    }
    let replacement = cx.update(|cx| create_untitled_document(&documents, cx));
    let replacement_model =
        cx.read(|cx| documents.read(cx).get(replacement).unwrap().model().clone());
    let (replacement_shell, replacement_window) =
        product_window(replacement, replacement_model.clone(), cx);
    let closed = report(&replacement_shell, replacement_window, cx);
    assert!(!cx.read(|cx| model.read(cx).is_open()));
    let events = closed["events"].as_array().unwrap();
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "documentClosed" && event["document"] == id)
    );
    assert!(events.iter().any(|event| event["type"] == "windowClosed"));
    let stopped = dispatch_product_command(
        &replacement_shell,
        replacement_window,
        "example.stop-application-observer",
        cx,
    );
    assert_eq!(wait_for_command(stopped, cx), CommandOutcome::Completed);
    cx.read(|cx| assert_eq!(host.read(cx).application_observer_count(), 0));
    let stopped_count = report(&replacement_shell, replacement_window, cx)["events"]
        .as_array()
        .unwrap()
        .len();
    replacement_model.update(cx, |model, cx| {
        model.replace(0..0, "after disposal").unwrap();
        cx.notify();
    });
    assert_eq!(
        report(&replacement_shell, replacement_window, cx)["events"]
            .as_array()
            .unwrap()
            .len(),
        stopped_count
    );
}

#[gpui::test]
fn application_listener_can_query_edit_fail_and_dispose_without_blocking_queued_roots(
    cx: &mut TestAppContext,
) {
    use crate::host::lifecycle::ExtensionKey;
    const SOURCE: &str = r#"
import { commands, documents, workbench } from "knot";
let count = 0, last = 0;
const failing = await workbench.observe(() => { throw new Error("isolated observer failure"); });
const observation = await workbench.observe(async (event) => {
  if (event.sequence <= observation.watermark || event.sequence <= last) throw new Error("observation order");
  last = event.sequence;
  if (event.type !== "documentChanged") return;
  count++;
  const current = await documents.get(event.document.id);
  const snapshot = await current.buffer.snapshot();
  await current.buffer.applyEdits([{ range: { startByteOffset: 0, endByteOffset: 0 }, text: "listener " }], { ifRevision: snapshot.revision });
  const disposal = observation.dispose();
  if (disposal !== observation.dispose()) throw new Error("disposal promise changed");
  await disposal;
});
await commands.register("observation.verify", async () => {
  if (count !== 1) throw new Error(`listener count ${count}`);
  if (!(await (await documents.list())[0].buffer.snapshot()).text.startsWith("listener ")) throw new Error("listener mutation missing");
  await failing.dispose();
});
await commands.register("observation.live", () => {});
await commands.register("observation.install-for-unload", async () => { await workbench.observe(() => {}); });
"#;
    let documents = install_globals(cx);
    let host = cx.update(super::super::extension_host::install);
    let key = ExtensionKey::new(ExtensionId::new(708), ExtensionLifecycleId::new(1));
    host.update(cx, |host, cx| {
        let execution = host.load_test_source(key, SOURCE).unwrap();
        cx.spawn(async move |_, _| {
            execution.await.unwrap();
        })
        .detach();
    });
    wait_for_api_condition(cx, |cx| {
        cx.global::<ApplicationProductCommands>()
            .0
            .read(cx)
            .catalog()
            .borrow()
            .resolve("observation.install-for-unload")
            .is_ok()
    });
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model.clone(), cx);
    model.update(cx, |model, cx| {
        model.replace(0..0, "edit").unwrap();
        cx.notify();
    });
    let verify = dispatch_product_command(&shell, window, "observation.verify", cx);
    assert_eq!(wait_for_command(verify, cx), CommandOutcome::Completed);
    cx.read(|cx| assert_eq!(host.read(cx).application_observer_count(), 0));
    let install = dispatch_product_command(&shell, window, "observation.install-for-unload", cx);
    assert_eq!(wait_for_command(install, cx), CommandOutcome::Completed);
    cx.read(|cx| assert_eq!(host.read(cx).application_observer_count(), 1));
    host.update(cx, |host, cx| host.unload(key, cx).unwrap());
    cx.read(|cx| assert_eq!(host.read(cx).application_observer_count(), 0));
}

#[gpui::test]
fn application_queries_use_live_product_state_and_distinct_target_authority(
    cx: &mut TestAppContext,
) {
    use crate::host::lifecycle::ExtensionKey;
    const SOURCE: &str = r#"
import { commands, documents, windows, workspaces, editor } from "knot";
if ((await documents.list()).length || (await windows.list()).length) throw new Error("startup catalog not empty");
let savedId, savedWindows;
await commands.register("queries.catalog", async ({ target }) => {
  const docs = await documents.list();
  const wins = await windows.list();
  if (docs.length !== 1 || wins.length !== 2) throw new Error("catalog scope/dedup");
  const doc = docs[0];
  if (doc.kind !== "untitled" || doc.title !== "Clean" || doc.dirty || doc.resourceUri !== null || doc.revision !== 0) throw new Error("document metadata");
  if ((await doc.buffer.snapshot()).text !== "owned") throw new Error("document buffer");
  if ((await documents.get(doc.id)).buffer !== doc.buffer) throw new Error("document lookup");
  if (await documents.get("missing") !== null) throw new Error("missing identity");
  for (const win of wins) {
const pane = win.panes[0], tab = pane.tabs[0];
if (tab.document !== doc.id || tab.buffer !== doc.buffer || tab.kind !== "document") throw new Error("presentation association");
if (pane.selected !== tab.target || tab.target.pane !== pane.target || tab.target.window !== win.target) throw new Error("target identity");
const again = await windows.get(win.target);
if (again.panes[0].tabs[0].target !== tab.target) throw new Error("unstable target");
const workspace = await workspaces.get(win.target);
if (workspace.window !== win.target || workspace.roots.length !== 0 || workspace.generation !== 0) throw new Error("empty workspace");
for (const value of [docs, doc, wins, win, pane, pane.tabs, tab, tab.target, workspace, workspace.roots]) {
  if (!Object.isFrozen(value)) throw new Error("mutable snapshot");
}
  }
  for (const invalid of [target, {}, wins[0].panes[0].target, wins[0].panes[0].tabs[0].target]) {
try { await windows.get(invalid); } catch (error) {
  if (error.code === "InvalidTarget") continue;
  throw error;
}
throw new Error("invalid live window accepted");
  }
  try { await editor.activeBuffer(wins[0].panes[0].tabs[0].target); throw new Error("live target gained invocation authority"); }
  catch (error) { if (error.code !== "InvalidTarget") throw error; }
  savedId = doc.id; savedWindows = wins.map(win => win.target);
});
await commands.register("queries.closed", async () => {
  if (await documents.get(savedId) !== null) throw new Error("closed document catalogued");
  const values = await Promise.all(savedWindows.map(target => windows.get(target)));
  if (values.filter(value => value === null).length !== 1) throw new Error("closed window catalogued");
  const closed = savedWindows[values.findIndex(value => value === null)];
  if (await workspaces.get(closed) !== null) throw new Error("closed workspace catalogued");
});
await commands.register("queries.terminal", async () => {
  const wins = await windows.list();
  const terminal = wins.flatMap(win => win.panes.flatMap(pane => pane.tabs)).find(tab => tab.kind === "terminal");
  if (!terminal || terminal.buffer !== null || terminal.document !== null) throw new Error("terminal pretends to be a document");
  if (!wins.some(win => win.currentTarget === terminal.target)) throw new Error("terminal focus missing");
});
"#;
    let documents = install_globals(cx);
    let host = cx.update(super::super::extension_host::install);
    let key = ExtensionKey::new(ExtensionId::new(707), ExtensionLifecycleId::new(1));
    host.update(cx, |host, cx| {
        let execution = host.load_test_source(key, SOURCE).unwrap();
        cx.spawn(async move |_, _| {
            execution.await.unwrap();
        })
        .detach();
    });
    wait_for_api_condition(cx, |cx| {
        cx.global::<ApplicationProductCommands>()
            .0
            .read(cx)
            .catalog()
            .borrow()
            .resolve("queries.terminal")
            .is_ok()
    });
    let model = cx.new(|_| BufferModel::from_text("owned"));
    let document = documents.update(cx, |documents, cx| {
        documents.create_untitled("Clean", model.clone(), cx)
    });
    let (first, first_window) = product_window(document, model.clone(), cx);
    let (second, second_window) = product_window(document, model, cx);
    let (_diagnostic, _) =
        cx.add_window_view(|_, cx| crate::app::tree_view::TreeView::new("diagnostic", "1", cx));
    for (shell, window, command) in [
        (&first, first_window, "queries.catalog"),
        (&first, first_window, CLOSE_TAB_COMMAND),
        (&second, second_window, super::CLOSE_WINDOW_COMMAND),
        (&first, first_window, "queries.closed"),
        (&first, first_window, NEW_TERMINAL_COMMAND),
        (&first, first_window, "queries.terminal"),
    ] {
        let execution = dispatch_product_command(shell, window, command, cx);
        assert_eq!(
            wait_for_command(execution, cx),
            CommandOutcome::Completed,
            "{command}"
        );
    }
    host.update(cx, |host, cx| host.unload(key, cx).unwrap());
}

#[gpui::test]
fn final_document_close_invalidates_js_buffer_even_with_retained_editor(cx: &mut TestAppContext) {
    use crate::host::lifecycle::ExtensionKey;
    const SOURCE: &str = r#"
import { commands } from "knot";
let saved;
await commands.register("closure.setup", async ({ buffer }) => {
  saved = buffer;
  const snapshot = await buffer.snapshot();
  await buffer.contributions.replace([{ range: { startByteOffset: 0, endByteOffset: 1 }, decoration: "info" }], { ifRevision: snapshot.revision });
  await buffer.onDidChange(() => {});
});
await commands.register("closure.live", async () => {
  if ((await saved.snapshot()).text !== "owned") throw new Error("shared buffer lost");
});
await commands.register("closure.closed", async () => {
  for (const operation of [() => saved.snapshot(), () => saved.onDidChange(() => {}), () => saved.applyEdits([], { ifRevision: 0 }), () => saved.contributions.replace([], { ifRevision: 0 })]) {
try { await operation(); } catch (error) {
  if (error.code === "BufferClosed") continue;
  throw error;
}
throw new Error("closed buffer accepted operation");
  }
});
"#;
    let documents = install_globals(cx);
    let host = cx.update(super::super::extension_host::install);
    let model = cx.new(|_| BufferModel::from_text("owned"));
    let document = documents.update(cx, |documents, cx| {
        documents.create_untitled("Clean", model.clone(), cx)
    });
    let (first, first_window) = product_window(document, model.clone(), cx);
    let (second, second_window) = product_window(document, model.clone(), cx);
    let retained_editor = cx.read(|cx| {
        first
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
    let key = ExtensionKey::new(ExtensionId::new(706), ExtensionLifecycleId::new(1));
    host.update(cx, |host, cx| {
        let execution = host.load_test_source(key, SOURCE).unwrap();
        cx.spawn(async move |_, _| {
            execution.await.unwrap();
        })
        .detach();
    });
    wait_for_api_condition(cx, |cx| {
        cx.global::<ApplicationProductCommands>()
            .0
            .read(cx)
            .catalog()
            .borrow()
            .resolve("closure.closed")
            .is_ok()
    });
    let setup = dispatch_product_command(&first, first_window, "closure.setup", cx);
    assert_eq!(wait_for_command(setup, cx), CommandOutcome::Completed);
    let close = dispatch_product_command(&first, first_window, CLOSE_TAB_COMMAND, cx);
    assert_eq!(wait_for_command(close, cx), CommandOutcome::Completed);
    cx.read(|cx| {
        assert!(documents.read(cx).get(document).is_some());
        assert!(model.read(cx).is_open());
        assert_eq!(model.read(cx).resolved_contributions().len(), 1);
    });
    let live = dispatch_product_command(&second, second_window, "closure.live", cx);
    assert_eq!(wait_for_command(live, cx), CommandOutcome::Completed);
    let close = dispatch_product_command(&second, second_window, CLOSE_TAB_COMMAND, cx);
    assert_eq!(wait_for_command(close, cx), CommandOutcome::Completed);
    cx.read(|cx| {
        assert!(documents.read(cx).get(document).is_none());
        assert!(!model.read(cx).is_open());
        assert!(model.read(cx).resolved_contributions().is_empty());
        assert_eq!(retained_editor.read(cx).model(), &model);
    });
    let closed = dispatch_product_command(&second, second_window, "closure.closed", cx);
    assert_eq!(wait_for_command(closed, cx), CommandOutcome::Completed);
    host.update(cx, |host, cx| host.unload(key, cx).unwrap());
}

#[gpui::test]
fn public_api_disposal_and_unload_release_foreground_ownership(cx: &mut TestAppContext) {
    use crate::host::lifecycle::ExtensionKey;
    const SOURCE: &str = r#"
import { commands, editor, workbench, keybinding } from "knot";
let owned;
keybinding("cmd-alt-o", "api.owned");
await commands.register("api.owned-setup", async ({ buffer }) => {
  const snapshot = await buffer.snapshot();
  await buffer.contributions.replace([{ range: { startByteOffset: 0, endByteOffset: 1 }, decoration: "info" }], { ifRevision: snapshot.revision });
  owned = [
buffer.contributions,
await buffer.onDidChange(() => workbench.writeClipboardText("leaked listener")),
await commands.register("api.owned", () => {}),
await workbench.registerView({
  id: "example.owned",
  title: "Owned",
  content: { kind: "tree", provider: {
    getChildren() { return [{ id: "owned", label: "owned", collapsibleState: "none" }]; },
  } },
}),
await editor.registerCompletionProvider("owned", { provideCompletions() { return [{ label: "owned", insertText: "owned" }]; } }),
  ];
});
await commands.register("api.owned-cleanup", async () => {
  const cleanup = owned.map(owner => owner.dispose());
  for (let index = 0; index < owned.length; index++) if (cleanup[index] !== owned[index].dispose()) throw new Error("cleanup promise changed");
  await Promise.all(cleanup);
});
"#;
    let documents = install_globals(cx);
    cx.update(super::bind_product_keys);
    let host = cx.update(super::super::extension_host::install);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    model.update(cx, |model, _| model.replace(0..0, "owned").unwrap());
    let (shell, window) = product_window(document, model.clone(), cx);
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
    for explicit_cleanup in [true, false] {
        let key = ExtensionKey::new(
            ExtensionId::new(if explicit_cleanup { 704 } else { 705 }),
            ExtensionLifecycleId::new(1),
        );
        host.update(cx, |host, cx| {
            let execution = host.load_test_source(key, SOURCE).unwrap();
            cx.spawn(async move |_, _| {
                execution.await.unwrap();
            })
            .detach();
        });
        wait_for_api_condition(cx, |cx| {
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .catalog()
                .borrow()
                .resolve("api.owned-cleanup")
                .is_ok()
        });
        let setup = dispatch_product_command(&shell, window, "api.owned-setup", cx);
        assert_eq!(wait_for_command(setup, cx), CommandOutcome::Completed);
        wait_for_api_condition(cx, |cx| {
            shell
                .read(cx)
                .contributed_views
                .get("example.owned")
                .is_some_and(|view| view.tree().read(cx).root_labels() == ["owned"])
        });
        cx.read(|cx| assert_eq!(model.read(cx).resolved_contributions().len(), 1));
        let completion =
            dispatch_product_command(&shell, window, super::SHOW_COMPLETIONS_COMMAND, cx);
        assert_eq!(wait_for_command(completion, cx), CommandOutcome::Completed);
        wait_for_api_condition(cx, |cx| {
            editor
                .read(cx)
                .completion_state()
                .is_some_and(|(items, pending, _, _)| items == 1 && pending == 0)
        });
        if explicit_cleanup {
            let cleanup = dispatch_product_command(&shell, window, "api.owned-cleanup", cx);
            assert_eq!(wait_for_command(cleanup, cx), CommandOutcome::Completed);
        } else {
            host.update(cx, |host, cx| host.unload(key, cx).unwrap());
        }
        cx.read(|cx| {
            assert!(model.read(cx).resolved_contributions().is_empty());
            assert!(shell.read(cx).contributed_views.is_empty());
            assert!(
                cx.global::<ApplicationProductCommands>()
                    .0
                    .read(cx)
                    .catalog()
                    .borrow()
                    .resolve("api.owned")
                    .is_err()
            );
            let (items, pending, _, _) = editor.read(cx).completion_state().unwrap();
            assert_eq!((items, pending), (0, 0));
        });
        if explicit_cleanup {
            host.update(cx, |host, cx| host.unload(key, cx).unwrap());
        }
        cx.read(|cx| {
            assert!(keymap_command(cx, "cmd-alt-o", Some("editor")).is_none());
            assert!(
                cx.global::<ApplicationProductCommands>()
                    .0
                    .read(cx)
                    .catalog()
                    .borrow()
                    .resolve("api.owned-setup")
                    .is_err()
            );
        });
    }
    let key = ExtensionKey::new(ExtensionId::new(706), ExtensionLifecycleId::new(1));
    host.update(cx, |host, cx| {
        let execution = host.load_test_source(key, SOURCE).unwrap();
        cx.spawn(async move |_, _| {
            execution.await.unwrap();
        })
        .detach();
    });
    wait_for_api_condition(cx, |cx| {
        cx.global::<ApplicationProductCommands>()
            .0
            .read(cx)
            .catalog()
            .borrow()
            .resolve("api.owned-cleanup")
            .is_ok()
    });
    let setup = dispatch_product_command(&shell, window, "api.owned-setup", cx);
    assert_eq!(wait_for_command(setup, cx), CommandOutcome::Completed);
    cx.read(|cx| assert_eq!(model.read(cx).resolved_contributions().len(), 1));
    cx.update(|cx| {
        drop(cx.remove_global::<super::super::extension_host::ApplicationExtensionHost>())
    });
    cx.update(move |_| drop(host));
    cx.run_until_parked();
    cx.read(|cx| {
        assert!(model.read(cx).resolved_contributions().is_empty());
        assert!(shell.read(cx).contributed_views.is_empty());
        assert!(
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .catalog()
                .borrow()
                .resolve("api.owned")
                .is_err()
        );
        assert!(keymap_command(cx, "cmd-alt-o", Some("editor")).is_none());
    });
}

#[gpui::test]
fn contributed_view_registered_before_the_window_attaches_on_open(cx: &mut TestAppContext) {
    use crate::host::lifecycle::ExtensionKey;

    const SOURCE: &str = r#"
import { workbench } from "knot";
await workbench.registerView({
  id: "example.deferred",
  title: "Deferred",
  content: { kind: "tree", provider: {
getChildren() { return [{ id: "root", label: "Deferred", collapsibleState: "none" }]; },
  } },
});
"#;

    let documents = install_globals(cx);
    let host = cx.update(super::super::extension_host::install);
    let key = ExtensionKey::new(ExtensionId::new(90), ExtensionLifecycleId::new(1));
    host.update(cx, |host, cx| {
        let execution = host.load_test_source(key, SOURCE).unwrap();
        cx.spawn(async move |_, _| {
            execution.await.unwrap();
        })
        .detach();
    });
    wait_for_api_condition(cx, |cx| {
        super::super::extension_host::view_definitions(cx)
            .iter()
            .any(|definition| definition.id == "example.deferred")
    });

    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, _window) = product_window(document, model, cx);
    let tree = wait_for_contributed_view(&shell, "example.deferred", cx);
    wait_for_api_condition(cx, |cx| tree.read(cx).root_labels() == ["Deferred"]);
}

#[gpui::test]
fn focused_extension_tree_claims_copy_and_unload_removes_its_handler(cx: &mut TestAppContext) {
    use crate::host::lifecycle::ExtensionKey;

    const SOURCE: &str = r#"
import * as knot from "knot"; const { commands, workbench } = knot;
await workbench.registerView({
  id: "example.outline",
  title: "Outline",
  content: { kind: "tree", provider: {
getChildren(parentId) {
  return parentId === null ? [
    { id: "first", label: "Selected tree item", collapsibleState: "none", command: "test.view-first" },
    { id: "failure", label: "Fail", collapsibleState: "none" },
  ] : [];
},
  } },
});
const viewCopy = await commands.registerForView("example.outline", "copy", async (context) => {
  if (context.buffer !== null || await knot.editor.activeBuffer(context.target) !== null) throw new Error("outline has a text buffer");
  const text = await workbench.selectedText(context.target);
  if (text === "Fail") throw new Error("copy failed");
  await workbench.writeClipboardText(text);
});
await commands.register("test.view-first", async () => {
  throw new Error("global handler ran");
});
await commands.registerForView("example.outline", "test.view-first", async () => {
  await workbench.writeClipboardText("tree item");
});
await commands.register("test.view-null", () => { throw new Error("global null handler"); });
await commands.registerForView("example.outline", "test.view-null", async (context) => {
  if (context.target.view === null || await workbench.selectedText(context.target) !== null) throw new Error("missing selection is not null");
});
await commands.register("test.nested-view-copy", async () => {
  const outcome = await commands.invoke("copy");
  if (outcome.kind !== "completed") throw new Error(`nested copy: ${outcome.kind}`);
});
await commands.register("test.nested-new", async () => {
  const outcome = await commands.invoke("file.new");
  if (outcome.kind !== "completed") throw new Error(`nested new: ${outcome.kind}`);
});
await commands.register("test.dispose-view-copy", async () => viewCopy.dispose());
"#;

    let documents = install_globals(cx);
    cx.update(super::bind_product_keys);
    let host = cx.update(super::super::extension_host::install);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model.clone(), cx);
    let key = ExtensionKey::new(ExtensionId::new(87), ExtensionLifecycleId::new(1));
    host.update(cx, |host, cx| {
        let execution = host.load_test_source(key, SOURCE).unwrap();
        cx.spawn(async move |_, _| {
            execution.await.unwrap();
        })
        .detach();
    });

    let tree = wait_for_contributed_view(&shell, "example.outline", cx);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        cx.run_until_parked();
        if cx.read(|cx| {
            tree.read(cx).root_labels().len() == 2
                && cx
                    .global::<ApplicationProductCommands>()
                    .0
                    .read(cx)
                    .catalog()
                    .borrow()
                    .resolve_view(tree.read(cx).instance_id(), "copy")
                    .is_some()
        }) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "tree provider did not settle"
        );
        std::thread::yield_now();
    }

    cx.update_window(window, |_, window, cx| {
        tree.focus_handle(cx).focus(window);
        let target = shell.update(cx, |shell, cx| {
            shell.capture_command_target(window, cx).unwrap()
        });
        assert_eq!(target.view, Some(tree.read(cx).instance_id()));
    })
    .unwrap();
    let declined = dispatch_tree_copy(&shell, window, cx);
    assert_eq!(wait_for_command(declined, cx), CommandOutcome::Unavailable);
    let empty_selection = dispatch_tree_command(&shell, window, "test.view-null", cx);
    assert_eq!(
        wait_for_command(empty_selection, cx),
        CommandOutcome::Completed
    );
    let view_first = dispatch_tree_command(&shell, window, "test.view-first", cx);
    assert_eq!(wait_for_command(view_first, cx), CommandOutcome::Completed);
    cx.update(|cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string("untouched".into())));
    cx.refresh().unwrap();
    cx.simulate_keystrokes(window, "enter");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        cx.run_until_parked();
        if cx.read(|cx| {
            cx.read_from_clipboard()
                .is_some_and(|item| item.text().as_deref() == Some("tree item"))
        }) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "tree item command did not run"
        );
        std::thread::yield_now();
    }
    cx.update_window(window, |_, window, cx| {
        shell.read(cx).focus_active_editor(window, cx)
    })
    .unwrap();
    cx.update(|cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string("untouched".into())));
    cx.refresh().unwrap();
    {
        let mut window_cx = VisualTestContext::from_window(window, cx);
        let row = window_cx
            .debug_bounds("extension-tree-row-0")
            .expect("contributed tree row is laid out")
            .center();
        window_cx.simulate_mouse_down(row, MouseButton::Left, Modifiers::default());
        window_cx.simulate_mouse_up(row, MouseButton::Left, Modifiers::default());
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        cx.run_until_parked();
        if cx.read(|cx| {
            cx.read_from_clipboard()
                .is_some_and(|item| item.text().as_deref() == Some("tree item"))
        }) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "tree click command did not run"
        );
        std::thread::yield_now();
    }
    cx.update(|cx| tree.update(cx, |tree, cx| assert!(tree.select_item("first", cx))));
    let copy = dispatch_tree_copy(&shell, window, cx);
    assert_eq!(wait_for_command(copy, cx), CommandOutcome::Completed);
    cx.update(|cx| {
        assert!(
            cx.global_mut::<ApplicationKeymaps>()
                .0
                .set(
                    BindingOwner::Extension(key.extension, key.lifecycle),
                    "cmd-j",
                    Some("example.outline"),
                    Some(Command {
                        name: super::COPY_COMMAND.into(),
                        arguments: CommandArgumentValue::Null,
                    }),
                )
                .unwrap()
        );
        super::super::keymaps::rebuild(cx);
        cx.write_to_clipboard(gpui::ClipboardItem::new_string("untouched".into()));
    });
    cx.refresh().unwrap();
    cx.simulate_keystrokes(window, "cmd-j");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        cx.run_until_parked();
        if cx.read(|cx| {
            cx.read_from_clipboard()
                .is_some_and(|item| item.text().as_deref() == Some("Selected tree item"))
        }) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "view keybinding did not dispatch"
        );
        std::thread::yield_now();
    }
    let nested = dispatch_tree_command(&shell, window, "test.nested-view-copy", cx);
    assert_eq!(wait_for_command(nested, cx), CommandOutcome::Completed);
    let nested_native = dispatch_tree_command(&shell, window, "test.nested-new", cx);
    assert_eq!(
        wait_for_command(nested_native, cx),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert_eq!(
            shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .tabs()
                .len(),
            2
        );
    });
    cx.update_window(window, |_, window, cx| tree.focus_handle(cx).focus(window))
        .unwrap();
    cx.read(|cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "Selected tree item"
        )
    });

    let (second_shell, second_window) = product_window(document, model.clone(), cx);
    let second_tree = wait_for_contributed_view(&second_shell, "example.outline", cx);
    assert_ne!(
        cx.read(|cx| tree.read(cx).instance_id()),
        cx.read(|cx| second_tree.read(cx).instance_id())
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        cx.run_until_parked();
        if cx.read(|cx| second_tree.read(cx).root_labels().len() == 2) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "second tree did not receive provider data"
        );
        std::thread::yield_now();
    }
    cx.update_window(second_window, |_, window, cx| {
        second_tree.update(cx, |tree, cx| assert!(tree.select_item("first", cx)));
        second_tree.focus_handle(cx).focus(window);
    })
    .unwrap();
    assert_eq!(
        wait_for_command(dispatch_tree_copy(&second_shell, second_window, cx), cx),
        CommandOutcome::Completed
    );

    cx.update(|cx| tree.update(cx, |tree, cx| assert!(tree.select_item("failure", cx))));
    let failure = dispatch_tree_copy(&shell, window, cx);
    assert!(matches!(
        wait_for_command(failure, cx),
        CommandOutcome::HandlerFailure { .. }
    ));
    cx.update(|cx| tree.update(cx, |tree, cx| assert!(tree.select_item("first", cx))));
    let recovery = dispatch_tree_copy(&shell, window, cx);
    assert_eq!(wait_for_command(recovery, cx), CommandOutcome::Completed);

    let dispose = dispatch_product_command(&shell, window, "test.dispose-view-copy", cx);
    assert_eq!(wait_for_command(dispose, cx), CommandOutcome::Completed);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        cx.run_until_parked();
        if cx.read(|cx| {
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .catalog()
                .borrow()
                .resolve_view(tree.read(cx).instance_id(), "copy")
                .is_none()
        }) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "disposed view handler remained registered"
        );
        std::thread::yield_now();
    }
    let disposed = dispatch_tree_copy(&second_shell, second_window, cx);
    assert_eq!(wait_for_command(disposed, cx), CommandOutcome::Unavailable);

    host.update(cx, |host, cx| host.unload(key, cx).unwrap());
    cx.read(|cx| {
        assert!(
            cx.global::<ApplicationKeymaps>()
                .0
                .resolve(
                    &gpui::Keystroke::parse("cmd-j").unwrap().unparse(),
                    Some("example.outline")
                )
                .is_none()
        );
    });
    cx.read(|cx| {
        assert!(shell.read(cx).contributed_views.is_empty());
        assert!(second_shell.read(cx).contributed_views.is_empty());
        assert!(!tree.read(cx).lifecycle_state().0);
        assert!(!second_tree.read(cx).lifecycle_state().0);
    });
}

#[gpui::test]
fn startup_script_command_cannot_borrow_the_focused_product_window(cx: &mut TestAppContext) {
    use crate::host::lifecycle::ExtensionKey;

    let documents = install_globals(cx);
    let host = cx.update(super::super::extension_host::install);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    cx.update_window(window, |_, window, cx| {
        shell.read(cx).focus_active_editor(window, cx);
    })
    .unwrap();

    let key = ExtensionKey::new(ExtensionId::new(89), ExtensionLifecycleId::new(1));
    let (done, mut result) = tokio::sync::oneshot::channel();
    host.update(cx, |host, cx| {
        let execution = host
            .load_test_source(
                key,
                r#"
import * as knot from "knot"; const { commands } = knot;
let rejected = false;
try { await commands.invoke("file.new"); }
catch (error) { if (error.code !== "ContextUnavailable") throw error; rejected = true; }
if (!rejected) throw new Error("startup borrowed focus");
"#,
            )
            .unwrap();
        cx.spawn(async move |_, _| {
            let _ = done.send(execution.await);
        })
        .detach();
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        cx.run_until_parked();
        match result.try_recv() {
            Ok(outcome) => {
                outcome.unwrap();
                break;
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "script command did not settle"
                );
                std::thread::yield_now();
            }
            Err(error) => panic!("script completion closed: {error}"),
        }
    }
    cx.read(|cx| {
        assert_eq!(
            shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .tabs()
                .len(),
            1
        );
    });
}

#[gpui::test]
fn extension_command_with_editor_prefix_keeps_its_arguments(cx: &mut TestAppContext) {
    use crate::host::lifecycle::ExtensionKey;

    let documents = install_globals(cx);
    let host = cx.update(super::super::extension_host::install);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    let key = ExtensionKey::new(ExtensionId::new(91), ExtensionLifecycleId::new(1));
    host.update(cx, |host, cx| {
        let execution = host
            .load_test_source(
                key,
                r#"
import * as knot from "knot"; const { commands } = knot;
await commands.register("editor.custom", async ({ arguments: args }) => {
  if (args?.value !== 42) throw new Error("missing command arguments");
});
"#,
            )
            .unwrap();
        cx.spawn(async move |_, _| {
            execution.await.unwrap();
        })
        .detach();
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        cx.run_until_parked();
        if cx.read(|cx| {
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .catalog()
                .borrow()
                .resolve("editor.custom")
                .is_ok()
        }) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "command did not register"
        );
        std::thread::yield_now();
    }
    let target = cx
        .update_window(window, |_, window, cx| {
            shell.read(cx).focus_active_editor(window, cx);
            shell.update(cx, |shell, cx| {
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let dispatcher = cx.read(|cx| cx.global::<ApplicationProductCommands>().0.clone());
    let execution = dispatcher.update(cx, |dispatcher, cx| {
        dispatcher.dispatch(
            Command {
                name: "editor.custom".into(),
                arguments: CommandArgumentValue::Object(std::collections::BTreeMap::from([(
                    "value".into(),
                    CommandArgumentValue::Number(42.),
                )])),
            },
            target,
            cx,
        )
    });
    assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
}

#[gpui::test]
fn nested_script_waits_for_a_pending_native_command(cx: &mut TestAppContext) {
    use crate::host::lifecycle::ExtensionKey;

    let documents = install_globals(cx);
    cx.set_global(ApplicationOpenDialog(Arc::new(FixedOpenDialog(
        OpenDialogOutcome::Cancelled,
    ))));
    let host = cx.update(super::super::extension_host::install);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    let key = ExtensionKey::new(ExtensionId::new(90), ExtensionLifecycleId::new(1));
    host.update(cx, |host, cx| {
        let execution = host
            .load_test_source(
                key,
                r#"
import * as knot from "knot"; const { commands } = knot;
await commands.register("test.nested-open", async () => {
  const outcome = await commands.invoke("file.open");
  if (outcome.kind !== "cancelled") throw new Error(`open: ${outcome.kind}`);
});
"#,
            )
            .unwrap();
        cx.spawn(async move |_, _| {
            execution.await.unwrap();
        })
        .detach();
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        cx.run_until_parked();
        if cx.read(|cx| {
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .catalog()
                .borrow()
                .resolve("test.nested-open")
                .is_ok()
        }) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "nested command did not register"
        );
        std::thread::yield_now();
    }
    let execution = dispatch_product_command(&shell, window, "test.nested-open", cx);
    assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
    cx.read(|cx| assert_eq!(documents.read(cx).documents().count(), 1));
}

#[gpui::test]
fn delayed_clipboard_write_survives_presentation_closure_but_not_unload(cx: &mut TestAppContext) {
    use crate::host::lifecycle::ExtensionKey;

    const SOURCE: &str = r#"
import * as knot from "knot"; const { commands, workbench } = knot;
await workbench.registerView({
  id: "example.outline",
  title: "Outline",
  content: { kind: "tree", provider: {
getChildren(parentId) {
  return parentId === null
    ? [{ id: "held", label: "Held selection", collapsibleState: "none" }]
    : [];
},
  } },
});
await commands.registerForView("example.outline", "copy", async () => {
  const text = await workbench.selectedText();
  await workbench.writeClipboardText(text);
});
"#;

    let documents = install_globals(cx);
    let host = cx.update(super::super::extension_host::install);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model.clone(), cx);
    let key = ExtensionKey::new(ExtensionId::new(88), ExtensionLifecycleId::new(1));
    host.update(cx, |host, cx| {
        let execution = host.load_test_source(key, SOURCE).unwrap();
        cx.spawn(async move |_, _| {
            execution.await.unwrap();
        })
        .detach();
    });
    let tree = wait_for_contributed_view(&shell, "example.outline", cx);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        cx.run_until_parked();
        if cx.read(|cx| {
            tree.read(cx).root_labels().len() == 1
                && cx
                    .global::<ApplicationProductCommands>()
                    .0
                    .read(cx)
                    .catalog()
                    .borrow()
                    .resolve_view(tree.read(cx).instance_id(), "copy")
                    .is_some()
        }) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "view handler did not register"
        );
        std::thread::yield_now();
    }
    cx.update_window(window, |_, window, cx| {
        tree.update(cx, |tree, cx| assert!(tree.select_item("held", cx)));
        tree.focus_handle(cx).focus(window);
    })
    .unwrap();
    host.update(cx, |host, _| host.hold_clipboard_writes());
    cx.update(|cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string("untouched".into())));
    let execution = dispatch_tree_copy(&shell, window, cx);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        cx.run_until_parked();
        if cx.read(|cx| host.read(cx).pending_view_request_count() == 1) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "view request was not held"
        );
        std::thread::yield_now();
    }
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
    host.update(cx, |host, cx| host.release_view_requests(cx));
    assert_eq!(
        wait_for_command(execution, cx),
        CommandOutcome::InvalidTarget
    );
    cx.read(|cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "Held selection"
        )
    });
    let closed_view = cx.read(|cx| tree.read(cx).instance_id());
    drop(shell);
    cx.run_until_parked();
    cx.read(|cx| {
        assert!(
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .catalog()
                .borrow()
                .resolve_view(closed_view, "copy")
                .is_none()
        )
    });

    let (second_shell, second_window) = product_window(document, model, cx);
    let second_tree = wait_for_contributed_view(&second_shell, "example.outline", cx);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        cx.run_until_parked();
        if cx.read(|cx| second_tree.read(cx).root_labels().len() == 1) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "second tree did not load"
        );
        std::thread::yield_now();
    }
    cx.update_window(second_window, |_, window, cx| {
        second_tree.update(cx, |tree, cx| assert!(tree.select_item("held", cx)));
        second_tree.focus_handle(cx).focus(window);
    })
    .unwrap();
    host.update(cx, |host, _| host.hold_clipboard_writes());
    let interrupted = dispatch_tree_copy(&second_shell, second_window, cx);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        cx.run_until_parked();
        if cx.read(|cx| host.read(cx).pending_view_request_count() == 1) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "second clipboard write was not held"
        );
        std::thread::yield_now();
    }
    host.update(cx, |host, cx| host.unload(key, cx).unwrap());
    host.update(cx, |host, cx| host.release_view_requests(cx));
    assert_eq!(wait_for_command(interrupted, cx), CommandOutcome::Cancelled);
    cx.read(|cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "Held selection"
        );
        assert!(!second_tree.read(cx).lifecycle_state().0);
    });
}

#[gpui::test]
async fn dispatch_completion_preserves_the_captured_pane(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window_handle) = product_window(document, model, cx);

    cx.update_window(window_handle, |_, window, cx| {
        shell.update(cx, |shell, cx| {
            shell.split(SplitDirection::Horizontal, window, cx)
        });
    })
    .unwrap();
    cx.run_until_parked();
    cx.refresh().unwrap();

    let (first_pane, second_pane, first_editor, second_editor) = cx.read(|cx| {
        let workbench = shell.read(cx).workbench.read(cx);
        let first = &workbench.panes()[0];
        let second = &workbench.panes()[1];
        (
            first.id(),
            second.id(),
            first.active_tab().editor().unwrap().clone(),
            second.active_tab().editor().unwrap().clone(),
        )
    });
    let target = cx
        .update_window(window_handle, |_, window, cx| {
            first_editor.focus_handle(cx).focus(window);
            shell.update(cx, |shell, cx| {
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let execution = cx.update(|cx| {
        let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
        dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: NEW_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                target,
                cx,
            )
        })
    });
    cx.update_window(window_handle, |_, window, cx| {
        second_editor.focus_handle(cx).focus(window)
    })
    .unwrap();

    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        let workbench = shell.read(cx).workbench.read(cx);
        assert_eq!(workbench.pane(first_pane).unwrap().tabs().len(), 2);
        assert_eq!(workbench.pane(second_pane).unwrap().tabs().len(), 1);
    });
}

#[gpui::test]
async fn destroyed_product_targets_are_not_retargeted(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window_handle) = product_window(document, model, cx);
    cx.refresh().unwrap();

    let target = cx
        .update_window(window_handle, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.focus_active_editor(window, cx);
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    cx.update_window(window_handle, |_, window, cx| {
        shell.update(cx, |shell, cx| shell.close_active_tab(window, cx));
    })
    .unwrap();
    let execution = cx.update(|cx| {
        let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
        dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: SPLIT_HORIZONTAL_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                target,
                cx,
            )
        })
    });

    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::InvalidTarget
    );
    cx.read(|cx| assert_eq!(shell.read(cx).workbench.read(cx).panes().len(), 1));
}

#[gpui::test]
async fn keybinding_adapter_enters_the_product_dispatcher(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.update(super::bind_product_keys);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window_handle) = product_window(document, model, cx);
    cx.update_window(window_handle, |_, window, cx| {
        shell.read(cx).focus_active_editor(window, cx)
    })
    .unwrap();

    cx.simulate_keystrokes(
        window_handle,
        if cfg!(windows) { "ctrl-n" } else { "cmd-n" },
    );
    cx.run_until_parked();

    cx.read(|cx| {
        assert_eq!(
            shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .tabs()
                .len(),
            2
        );
        assert_eq!(
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .last_outcome(),
            Some(&CommandOutcome::Completed)
        );
    });
}

#[gpui::test]
fn personal_global_binding_overrides_editor_default_in_every_window(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.update(super::bind_product_keys);
    let mut windows = Vec::new();
    for _ in 0..2 {
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window) = product_window(document, model.clone(), cx);
        cx.update_window(window, |_, window, cx| {
            shell.read(cx).focus_active_editor(window, cx)
        })
        .unwrap();
        windows.push((window, model));
    }
    cx.update(|cx| {
        let changed = cx
            .global_mut::<ApplicationKeymaps>()
            .0
            .set(
                BindingOwner::Personal(ExtensionId::new(0), ExtensionLifecycleId::new(1)),
                "Enter",
                None,
                Some(Command {
                    name: super::super::product_commands::INSERT_TAB_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                }),
            )
            .unwrap();
        assert!(changed);
        super::super::keymaps::rebuild(cx);
    });
    cx.refresh().unwrap();
    for (window, _) in &windows {
        cx.simulate_keystrokes(*window, "enter");
    }
    cx.run_until_parked();
    cx.read(|cx| {
        for (_, model) in &windows {
            assert_eq!(model.read(cx).text(), "\t");
        }
    });
}

#[gpui::test]
fn unbinding_an_unmapped_letter_preserves_editor_input(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.update(super::bind_product_keys);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model.clone(), cx);
    cx.update_window(window, |_, window, cx| {
        shell.read(cx).focus_active_editor(window, cx)
    })
    .unwrap();
    cx.update(|cx| {
        let owner = BindingOwner::Personal(ExtensionId::new(0), ExtensionLifecycleId::new(1));
        assert!(
            cx.global_mut::<ApplicationKeymaps>()
                .0
                .set(owner, "a", Some("editor"), None)
                .unwrap()
        );
        super::super::keymaps::rebuild(cx);
    });
    cx.refresh().unwrap();
    cx.simulate_keystrokes(window, "a");
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(model.read(cx).text(), "a"));
}

#[gpui::test]
async fn editor_unbind_leaves_global_binding_available_in_terminal(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.update(super::bind_product_keys);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    cx.update_window(window, |_, window, cx| {
        shell.read(cx).focus_active_editor(window, cx)
    })
    .unwrap();
    cx.update(|cx| {
        assert!(
            cx.global_mut::<ApplicationKeymaps>()
                .0
                .set(
                    BindingOwner::Personal(ExtensionId::new(0), ExtensionLifecycleId::new(1)),
                    if cfg!(windows) { "ctrl-n" } else { "cmd-n" },
                    Some("editor"),
                    None,
                )
                .unwrap()
        );
        super::super::keymaps::rebuild(cx);
    });
    cx.refresh().unwrap();
    cx.simulate_keystrokes(window, if cfg!(windows) { "ctrl-n" } else { "cmd-n" });
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(shell.read(cx).workbench.read(cx).panes()[0].tabs().len(), 1));

    let terminal = dispatch_product_command(&shell, window, NEW_TERMINAL_COMMAND, cx);
    assert_eq!(
        terminal.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    cx.refresh().unwrap();
    cx.simulate_keystrokes(window, if cfg!(windows) { "ctrl-n" } else { "cmd-n" });
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(shell.read(cx).workbench.read(cx).panes()[0].tabs().len(), 3));
}

#[gpui::test]
fn sequence_prefix_dispatches_the_product_command(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.update(super::bind_product_keys);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    cx.update_window(window, |_, window, cx| {
        shell.read(cx).focus_active_editor(window, cx)
    })
    .unwrap();
    cx.refresh().unwrap();
    cx.simulate_keystrokes(
        window,
        if cfg!(windows) {
            "ctrl-k right"
        } else {
            "cmd-k right"
        },
    );
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(shell.read(cx).workbench.read(cx).panes().len(), 2));
}

#[gpui::test]
fn unavailable_high_priority_binding_does_not_invoke_native_default(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.update(super::bind_product_keys);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    cx.update_window(window, |_, window, cx| {
        shell.read(cx).focus_active_editor(window, cx)
    })
    .unwrap();
    cx.update(|cx| {
        assert!(
            cx.global_mut::<ApplicationKeymaps>()
                .0
                .set(
                    BindingOwner::Personal(ExtensionId::new(0), ExtensionLifecycleId::new(1)),
                    if cfg!(windows) { "ctrl-n" } else { "cmd-n" },
                    None,
                    Some(Command {
                        name: "missing.command".into(),
                        arguments: CommandArgumentValue::Null,
                    }),
                )
                .unwrap()
        );
        super::super::keymaps::rebuild(cx);
    });
    cx.refresh().unwrap();
    cx.simulate_keystrokes(window, if cfg!(windows) { "ctrl-n" } else { "cmd-n" });
    cx.run_until_parked();
    cx.read(|cx| {
        assert_eq!(shell.read(cx).workbench.read(cx).panes()[0].tabs().len(), 1);
        assert_eq!(
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .last_outcome(),
            Some(&CommandOutcome::Unavailable),
        );
    });
}

#[gpui::test]
fn menu_action_reaches_the_captured_window_in_its_dispatch_turn(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    cx.update_window(window, |_, window, cx| {
        shell.read(cx).focus_active_editor(window, cx)
    })
    .unwrap();
    cx.refresh().unwrap();

    cx.dispatch_action(window, ProductCommandSource::new(NEW_COMMAND));

    cx.read(|cx| {
        assert_eq!(
            shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .tabs()
                .len(),
            2
        );
        assert_eq!(
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .last_outcome(),
            Some(&CommandOutcome::Completed),
        );
    });
}

#[cfg(windows)]
#[gpui::test]
fn windows_native_ctrl_shift_p_opens_command_palette(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.update(super::bind_product_keys);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    cx.update_window(window, |_, window, cx| {
        shell.read(cx).focus_active_editor(window, cx);
    })
    .unwrap();
    cx.refresh().unwrap();
    cx.simulate_keystrokes(window, "ctrl-shift-p");
    cx.run_until_parked();
    cx.read(|cx| assert!(shell.read(cx).command_palette.is_some()));
}

#[gpui::test]
fn personal_config_ctrl_shift_p_opens_command_palette(cx: &mut TestAppContext) {
    let config = tempfile::tempdir().unwrap();
    let extensions = tempfile::tempdir().unwrap();
    std::fs::write(
        config.path().join("post-init.js"),
        "import * as knot from 'knot'; knot.keybinding('ctrl-shift-p', 'workbench.show-command-palette');",
    )
    .unwrap();
    install_globals(cx);
    cx.update(super::bind_product_keys);
    install_launch_gate(cx);
    let host = cx.update(super::super::extension_host::install);
    host.update(cx, |host, cx| {
        host.start_product_startup(
            Some(config.path().to_path_buf()),
            Ok(extensions.path().to_path_buf()),
            cx,
        )
    });
    assert!(wait_for_product_startup(cx));
    let window = cx.windows()[0];
    let shell = cx
        .update_window(window, |_, window, _| {
            window.root::<ProductShell>().flatten()
        })
        .unwrap()
        .unwrap();
    cx.update_window(window, |_, window, cx| {
        shell.read(cx).focus_active_editor(window, cx);
    })
    .unwrap();
    cx.refresh().unwrap();
    cx.simulate_keystrokes(window, "ctrl-shift-p");
    cx.run_until_parked();
    cx.read(|cx| assert!(shell.read(cx).command_palette.is_some()));
}

#[gpui::test]
async fn palette_discovers_commands_and_keeps_its_opening_target(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.update(super::bind_product_keys);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window_handle) = product_window(document, model, cx);
    cx.update_window(window_handle, |_, window, cx| {
        shell.update(cx, |shell, cx| {
            shell.split(SplitDirection::Horizontal, window, cx)
        });
    })
    .unwrap();
    cx.refresh().unwrap();

    let (first_pane, second_pane, first_editor) = cx.read(|cx| {
        let workbench = shell.read(cx).workbench.read(cx);
        let first = &workbench.panes()[0];
        (
            first.id(),
            workbench.panes()[1].id(),
            first.active_tab().editor().unwrap().clone(),
        )
    });
    cx.update_window(window_handle, |_, window, cx| {
        first_editor.focus_handle(cx).focus(window);
    })
    .unwrap();
    cx.simulate_keystrokes(
        window_handle,
        if cfg!(windows) {
            "ctrl-shift-p"
        } else {
            "cmd-shift-p"
        },
    );
    cx.run_until_parked();
    cx.update_window(window_handle, |_, _, cx| {
        assert!(shell.read(cx).command_palette.is_some());
        let workbench = shell.read(cx).workbench.clone();
        workbench.update(cx, |workbench, _| {
            workbench.focus_pane(second_pane);
        });
    })
    .unwrap();
    cx.refresh().unwrap();

    cx.simulate_keystrokes(window_handle, "f i l e . n e w enter");

    cx.read(|cx| {
        let workbench = shell.read(cx).workbench.read(cx);
        assert_eq!(workbench.pane(first_pane).unwrap().tabs().len(), 2);
        assert_eq!(workbench.pane(second_pane).unwrap().tabs().len(), 1);
    });
}

#[gpui::test]
async fn editing_commands_keep_the_captured_view_and_native_clipboard(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window_handle) = product_window(document, model.clone(), cx);
    let (target, dispatcher) = cx
        .update_window(window_handle, |_, window, cx| {
            shell.read(cx).focus_active_editor(window, cx);
            let target = shell.update(cx, |shell, cx| {
                shell.capture_command_target(window, cx).unwrap()
            });
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                "fn café() {\n    👩‍💻\n}\n".into(),
            ));
            shell.update(cx, |shell, cx| shell.new_tab(window, cx));
            (target, cx.global::<ApplicationProductCommands>().0.clone())
        })
        .unwrap();
    for command in [
        super::PASTE_COMMAND,
        super::SELECT_ALL_COMMAND,
        super::COPY_COMMAND,
    ] {
        let execution = dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: command.into(),
                    arguments: CommandArgumentValue::Null,
                },
                target.clone(),
                cx,
            )
        });
        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::Completed
        );
    }
    cx.read(|cx| {
        assert_eq!(model.read(cx).text(), "fn café() {\n    👩‍💻\n}\n");
        let active = shell
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab()
            .editor()
            .unwrap();
        assert_eq!(active.read(cx).model().read(cx).text(), "");
    });
    let execution = dispatcher.update(cx, |dispatcher, cx| {
        dispatcher.dispatch(
            Command {
                name: super::CUT_COMMAND.into(),
                arguments: CommandArgumentValue::Null,
            },
            target,
            cx,
        )
    });
    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    cx.update(|cx| {
        assert_eq!(model.read(cx).text(), "");
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "fn café() {\n    👩‍💻\n}\n"
        );
    });
}

#[cfg(unix)]
#[gpui::test]
async fn terminal_mouse_reporting_toggle_routes_to_the_focused_session_and_plain_drag_copies(
    cx: &mut TestAppContext,
) {
    use crate::app::{
        terminal_session::TerminalSession, terminal_view::TOGGLE_TERMINAL_MOUSE_REPORTING_COMMAND,
        terminal_view::TerminalView,
    };
    use alacritty_terminal::{tty, vte::ansi::Processor};
    let documents = install_globals(cx);
    cx.update(super::bind_product_keys);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window_handle) = product_window(document, model, cx);
    assert_eq!(
        dispatch_product_command(
            &shell,
            window_handle,
            TOGGLE_TERMINAL_MOUSE_REPORTING_COMMAND,
            cx
        )
        .completion
        .await
        .unwrap(),
        CommandOutcome::Unavailable
    );
    let session = cx.new(|cx| {
        TerminalSession::new_with_options(
            tty::Options {
                shell: Some(tty::Shell::new(
                    "/bin/sh".into(),
                    vec!["-c".into(), "read line".into()],
                )),
                drain_on_exit: true,
                ..Default::default()
            },
            cx,
        )
    });
    let view = cx
        .update_window(window_handle, |_, window, cx| {
            let id = cx
                .global::<ApplicationTerminalSessions>()
                .0
                .borrow_mut()
                .allocate_id();
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow_mut()
                .sessions
                .insert(id, session.clone());
            let view = cx.new(|cx| TerminalView::new(session.clone(), cx));
            shell.update(cx, |shell, cx| {
                let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
                let tab = shell.workbench.update(cx, |workbench, _| {
                    workbench.open_terminal_tab(pane, id, view.clone()).unwrap()
                });
                shell.activate_tab(pane, tab, window, cx);
            });
            view
        })
        .unwrap();
    cx.refresh().unwrap();
    assert_eq!(
        dispatch_product_command(
            &shell,
            window_handle,
            TOGGLE_TERMINAL_MOUSE_REPORTING_COMMAND,
            cx
        )
        .completion
        .await
        .unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| assert!(!session.read(cx).mouse_reporting_enabled()));
    let (start, end) = cx
        .update_window(window_handle, |_, window, cx| {
            let mut parser: Processor = Processor::new();
            parser.advance(
                &mut *session.read(cx).terminal().unwrap().lock(),
                b"\x1b[?1002h\x1b[?1006h\x1b[2J\x1b[3J\x1b[Hcopy me",
            );
            let bounds = view.read(cx).interaction_bounds();
            let width = px(session.read(cx).size().cell_width as f32 / window.scale_factor());
            (
                bounds.origin + point(px(0.1), px(0.1)),
                bounds.origin + point(width * 3 + px(0.1), px(0.1)),
            )
        })
        .unwrap();
    let mut window_cx = VisualTestContext::from_window(window_handle, cx);
    window_cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
    window_cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
    window_cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
    cx.simulate_keystrokes(
        window_handle,
        if cfg!(windows) { "ctrl-c" } else { "cmd-c" },
    );
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().as_deref(),
            Some("copy")
        );
        assert!(!session.read(cx).mouse_reporting_enabled());
    });
    assert_eq!(
        dispatch_product_command(
            &shell,
            window_handle,
            TOGGLE_TERMINAL_MOUSE_REPORTING_COMMAND,
            cx
        )
        .completion
        .await
        .unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| assert!(session.read(cx).mouse_reporting_enabled()));
    session.update(cx, |session, cx| session.close(cx));
}

#[gpui::test]
async fn copy_requires_focused_selection_and_declines_on_terminal(cx: &mut TestAppContext) {
    use alacritty_terminal::tty::{Options, Shell};
    use alacritty_terminal::{
        index::{Column, Line, Point as TermPoint, Side},
        selection::{Selection, SelectionType},
        vte::ansi::Handler,
    };
    use gpui::EntityInputHandler;

    #[cfg(windows)]
    let quiet_shell = Shell::new(
        "cmd.exe".into(),
        vec!["/C".into(), "ping -n 3 127.0.0.1 >nul".into()],
    );
    #[cfg(not(windows))]
    let quiet_shell = Shell::new("/bin/sh".into(), vec!["-c".into(), "sleep 2".into()]);

    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model.clone(), cx);
    cx.update_window(window, |_, window, cx| {
        let editor = shell
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab()
            .editor()
            .unwrap()
            .clone();
        editor.update(cx, |editor, cx| {
            editor.replace_text_in_range(None, "selected text", window, cx);
        });
    })
    .unwrap();
    let no_selection = dispatch_product_command(&shell, window, super::COPY_COMMAND, cx);
    assert_eq!(
        no_selection.completion.await.unwrap(),
        CommandOutcome::Unavailable
    );
    cx.update_window(window, |_, window, cx| {
        let editor = shell
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab()
            .editor()
            .unwrap()
            .clone();
        editor.update(cx, |editor, cx| {
            editor.execute_editing_command("editor.select-all", window, cx);
        });
    })
    .unwrap();
    let editor_target = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let editor_copy = dispatch_product_command(&shell, window, super::COPY_COMMAND, cx);
    assert_eq!(
        editor_copy.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    cx.update(|cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "selected text"
        )
    });
    let legacy_copy = dispatch_product_command(&shell, window, "editor.copy", cx);
    assert_eq!(
        legacy_copy.completion.await.unwrap(),
        CommandOutcome::Unavailable
    );

    cx.update_window(window, |_, window, cx| {
        shell.update(cx, |shell, cx| shell.open_command_palette(window, cx));
    })
    .unwrap();
    cx.refresh().unwrap();
    cx.simulate_keystrokes(window, "c o p y enter");
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "selected text"
        );
        assert_eq!(
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .last_outcome(),
            Some(&CommandOutcome::Completed)
        );
    });

    let terminal_session = cx.update(|cx| {
        cx.new(|cx| {
            crate::app::terminal_session::TerminalSession::new_with_options(
                Options {
                    shell: Some(quiet_shell),
                    drain_on_exit: true,
                    ..Default::default()
                },
                cx,
            )
        })
    });
    let target = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                let session = cx
                    .global::<ApplicationTerminalSessions>()
                    .0
                    .borrow_mut()
                    .allocate_id();
                cx.global::<ApplicationTerminalSessions>()
                    .0
                    .borrow_mut()
                    .sessions
                    .insert(session, terminal_session.clone());
                let view = cx.new(|cx| {
                    crate::app::terminal_view::TerminalView::new(terminal_session.clone(), cx)
                });
                let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
                let tab = shell.workbench.update(cx, |workbench, _| {
                    workbench.open_terminal_tab(pane, session, view).unwrap()
                });
                shell.activate_tab(pane, tab, window, cx);
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let dispatcher = cx.read(|cx| cx.global::<ApplicationProductCommands>().0.clone());
    let captured_editor = dispatcher.update(cx, |dispatcher, cx| {
        dispatcher.dispatch(
            Command {
                name: super::COPY_COMMAND.into(),
                arguments: CommandArgumentValue::Null,
            },
            editor_target,
            cx,
        )
    });
    assert_eq!(
        captured_editor.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "selected text"
        )
    });
    let execution = dispatcher.update(cx, |dispatcher, cx| {
        dispatcher.dispatch(
            Command {
                name: super::COPY_COMMAND.into(),
                arguments: CommandArgumentValue::Null,
            },
            target.clone(),
            cx,
        )
    });
    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Unavailable
    );
    cx.read(|cx| {
        let session_id = match target.surface {
            TabSurfaceId::Terminal(id) => id,
            _ => unreachable!(),
        };
        let session = cx
            .global::<ApplicationTerminalSessions>()
            .0
            .borrow()
            .sessions[&session_id]
            .clone();
        let terminal = session.read(cx).terminal().unwrap().clone();
        let mut terminal = terminal.lock();
        terminal.goto(0, 0);
        for ch in "selected terminal text".chars() {
            terminal.input(ch);
        }
        let mut selection = Selection::new(
            SelectionType::Simple,
            TermPoint::new(Line(0), Column(0)),
            Side::Left,
        );
        selection.update(TermPoint::new(Line(0), Column(21)), Side::Right);
        terminal.selection = Some(selection);
    });
    let selected_text = cx.read(|cx| {
        let session_id = match target.surface {
            TabSurfaceId::Terminal(id) => id,
            _ => unreachable!(),
        };
        let session = cx
            .global::<ApplicationTerminalSessions>()
            .0
            .borrow()
            .sessions[&session_id]
            .clone();
        session
            .read(cx)
            .terminal()
            .unwrap()
            .lock()
            .selection_to_string()
            .unwrap()
    });
    assert!(!selected_text.is_empty());
    let selected_terminal = dispatch_product_command(&shell, window, super::COPY_COMMAND, cx);
    assert_eq!(
        selected_terminal.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            selected_text
        );
    });
    let application =
        dispatch_product_command(&shell, window, super::SHOW_EXTENSION_REPORT_COMMAND, cx);
    assert_eq!(
        application.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| assert!(shell.read(cx).extension_report_open));

    let scripted = dispatcher.update(cx, |dispatcher, cx| {
        dispatcher.dispatch(
            Command {
                name: super::COPY_COMMAND.into(),
                arguments: CommandArgumentValue::Null,
            },
            target.clone(),
            cx,
        )
    });
    assert_eq!(
        scripted.completion.await.unwrap(),
        CommandOutcome::Completed
    );

    let unavailable = dispatcher.update(cx, |dispatcher, cx| {
        dispatcher.dispatch(
            Command {
                name: "missing.command".into(),
                arguments: CommandArgumentValue::Null,
            },
            target.clone(),
            cx,
        )
    });
    assert_eq!(
        unavailable.completion.await.unwrap(),
        CommandOutcome::Unavailable
    );

    let mut stale = target;
    stale.surface = TabSurfaceId::Document(document);
    let execution = dispatcher.update(cx, |dispatcher, cx| {
        dispatcher.dispatch(
            Command {
                name: super::COPY_COMMAND.into(),
                arguments: CommandArgumentValue::Null,
            },
            stale,
            cx,
        )
    });
    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::InvalidTarget
    );
}

#[gpui::test]
fn product_editor_accepts_utf16_composition_and_grapheme_deletion(cx: &mut TestAppContext) {
    use gpui::EntityInputHandler;
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window_handle) = product_window(document, model.clone(), cx);
    cx.update_window(window_handle, |_, window, cx| {
        shell.read(cx).focus_active_editor(window, cx);
        let editor = shell
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab()
            .editor()
            .unwrap()
            .clone();
        editor.update(cx, |editor, cx| {
            editor.replace_text_in_range(None, "# Notes\n👩‍💻 é ", window, cx);
            let start = "# Notes\n👩‍💻 é ".encode_utf16().count();
            editor.replace_and_mark_text_in_range(None, "に", Some(1..1), window, cx);
            assert_eq!(editor.marked_text_range(window, cx), Some(start..start + 1));
            editor.replace_and_mark_text_in_range(None, "日本", Some(2..2), window, cx);
            assert_eq!(editor.marked_text_range(window, cx), Some(start..start + 2));
            editor.replace_text_in_range(None, "日本語", window, cx);
            assert_eq!(editor.marked_text_range(window, cx), None);
            assert!(editor.execute_editing_command("editor.move-document-start", window, cx));
            assert!(editor.execute_editing_command("editor.move-down", window, cx));
            assert!(editor.execute_editing_command("editor.delete-forward", window, cx));
        });
    })
    .unwrap();
    cx.read(|cx| assert_eq!(model.read(cx).text(), "# Notes\n é 日本語"));
}

#[gpui::test]
fn editing_keybindings_use_semantic_commands(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.update(super::bind_product_keys);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window_handle) = product_window(document, model.clone(), cx);
    cx.update_window(window_handle, |_, window, cx| {
        shell.read(cx).focus_active_editor(window, cx)
    })
    .unwrap();
    cx.simulate_keystrokes(window_handle, "enter");
    cx.run_until_parked();
    cx.read(|cx| {
        assert_eq!(model.read(cx).text(), "\n");
        assert_eq!(
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .last_outcome(),
            Some(&CommandOutcome::Completed)
        );
    });
    cx.simulate_keystrokes(window_handle, "shift-up backspace tab");
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(model.read(cx).text(), "\t"));
    cx.simulate_input(window_handle, "café👩‍💻");
    cx.simulate_keystrokes(window_handle, "enter");
    cx.simulate_input(window_handle, "tail");
    cx.simulate_keystrokes(window_handle, "home");
    cx.simulate_input(window_handle, ">");
    cx.simulate_keystrokes(window_handle, "end");
    cx.simulate_input(window_handle, "<");
    cx.simulate_keystrokes(
        window_handle,
        if cfg!(windows) {
            "shift-home ctrl-c"
        } else {
            "shift-home cmd-c"
        },
    );
    cx.run_until_parked();
    cx.update(|cx| assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), ">tail<"));
    cx.simulate_keystrokes(
        window_handle,
        if cfg!(windows) {
            "ctrl-a ctrl-c"
        } else {
            "cmd-a cmd-c"
        },
    );
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "\tcafé👩‍💻\n>tail<"
        )
    });
    cx.simulate_keystrokes(
        window_handle,
        if cfg!(windows) {
            "ctrl-x ctrl-v"
        } else {
            "cmd-x cmd-v"
        },
    );
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(model.read(cx).text(), "\tcafé👩‍💻\n>tail<"));
}

#[gpui::test]
fn product_editor_captures_mouse_selection_outside_its_pane(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let text = "first line\nsecond line";
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    model.update(cx, |model, _| {
        model.replace(0..0, text).unwrap();
    });
    let (shell, window_handle) = product_window(document, model, cx);
    cx.refresh().unwrap();
    cx.run_until_parked();

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
    let bounds = cx.read(|cx| editor.read(cx).interaction_bounds());
    let start = point(bounds.origin.x + px(1.), bounds.origin.y + px(5.));
    let outside = point(
        bounds.origin.x + bounds.size.width + px(20.),
        bounds.origin.y + bounds.size.height + px(20.),
    );
    let mut window_cx = VisualTestContext::from_window(window_handle, cx);
    window_cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
    window_cx.simulate_mouse_move(outside, MouseButton::Left, Modifiers::default());
    window_cx.simulate_mouse_up(outside, MouseButton::Left, Modifiers::default());

    window_cx.read(|cx| {
        assert_eq!(editor.read(cx).selected_byte_range(), Some(0..text.len()));
    });
}

#[gpui::test]
async fn undo_command_routes_to_the_captured_editor(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window_handle) = product_window(document, model, cx);
    cx.update(|cx| {
        let model = documents.read(cx).get(document).unwrap().model().clone();
        model.update(cx, |model, _| model.replace(0..0, "edit").unwrap());
    });
    let (target, dispatcher) = cx
        .update_window(window_handle, |_, window, cx| {
            shell.read(cx).focus_active_editor(window, cx);
            let target = shell.update(cx, |shell, cx| {
                shell.capture_command_target(window, cx).unwrap()
            });
            (target, cx.global::<ApplicationProductCommands>().0.clone())
        })
        .unwrap();
    let execution = dispatcher.update(cx, |dispatcher, cx| {
        dispatcher.dispatch(
            Command {
                name: UNDO_COMMAND.into(),
                arguments: CommandArgumentValue::Null,
            },
            target.clone(),
            cx,
        )
    });

    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert_eq!(
            documents
                .read(cx)
                .get(document)
                .unwrap()
                .model()
                .read(cx)
                .text(),
            ""
        );
    });
    let execution = dispatcher.update(cx, |dispatcher, cx| {
        dispatcher.dispatch(
            Command {
                name: REDO_COMMAND.into(),
                arguments: CommandArgumentValue::Null,
            },
            target,
            cx,
        )
    });
    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert_eq!(
            documents
                .read(cx)
                .get(document)
                .unwrap()
                .model()
                .read(cx)
                .text(),
            "edit"
        );
    });
}

#[gpui::test]
async fn every_editor_motion_command_reaches_the_focused_view(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    let target = cx
        .update_window(window, |_, window, cx| {
            shell.read(cx).focus_active_editor(window, cx);
            shell.update(cx, |shell, cx| {
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let dispatcher = cx.read(|cx| cx.global::<ApplicationProductCommands>().0.clone());
    for name in super::super::product_commands::product_command_names().filter(|name| {
        name.starts_with(super::super::product_commands::EDITOR_MOVE_PREFIX)
            || name.starts_with(super::super::product_commands::EDITOR_SELECT_PREFIX)
    }) {
        let execution = dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: name.into(),
                    arguments: CommandArgumentValue::Null,
                },
                target.clone(),
                cx,
            )
        });
        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::Completed,
            "{name}"
        );
    }
}

#[gpui::test]
async fn native_participants_reject_arguments_before_mutation(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model.clone(), cx);
    let target = cx
        .update_window(window, |_, window, cx| {
            shell.read(cx).focus_active_editor(window, cx);
            shell.update(cx, |shell, cx| {
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    cx.update(|cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string("ignored".into())));
    let dispatcher = cx.read(|cx| cx.global::<ApplicationProductCommands>().0.clone());
    for name in [
        super::COPY_COMMAND,
        super::PASTE_COMMAND,
        NEW_COMMAND,
        super::NEW_WINDOW_COMMAND,
        SHOW_EXTENSION_REPORT_COMMAND,
    ] {
        let execution = dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: name.into(),
                    arguments: CommandArgumentValue::String("unexpected".into()),
                },
                target.clone(),
                cx,
            )
        });
        assert!(
            matches!(
                execution.completion.await.unwrap(),
                CommandOutcome::InvalidArgument { .. }
            ),
            "{name}"
        );
    }
    cx.read(|cx| {
        assert_eq!(model.read(cx).text(), "");
        assert_eq!(
            shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .tabs()
                .len(),
            1
        );
        assert_eq!(cx.windows().len(), 1);
    });
}

#[gpui::test]
async fn undo_keeps_the_document_captured_before_focus_changes(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let first_document = cx.update(|cx| create_untitled_document(&documents, cx));
    let first_model = cx.read(|cx| {
        documents
            .read(cx)
            .get(first_document)
            .unwrap()
            .model()
            .clone()
    });
    let (shell, window_handle) = product_window(first_document, first_model.clone(), cx);
    cx.update_window(window_handle, |_, window, cx| {
        shell.update(cx, |shell, cx| shell.new_tab(window, cx));
    })
    .unwrap();

    let (pane, first_tab, second_tab, second_document, second_model) = cx.read(|cx| {
        let workbench = shell.read(cx).workbench.read(cx);
        let pane = workbench.focused_pane().unwrap();
        let first = &pane.tabs()[0];
        let second = &pane.tabs()[1];
        let second_model = documents
            .read(cx)
            .get(second.document_id().unwrap())
            .unwrap()
            .model()
            .clone();
        (
            pane.id(),
            first.id(),
            second.id(),
            second.document_id().unwrap(),
            second_model,
        )
    });
    first_model.update(cx, |model, _| model.replace(0..0, "first").unwrap());
    second_model.update(cx, |model, _| model.replace(0..0, "second").unwrap());

    let target = cx
        .update_window(window_handle, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.activate_tab(pane, first_tab, window, cx);
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let dispatcher = cx.read(|cx| cx.global::<ApplicationProductCommands>().0.clone());
    let execution = dispatcher.update(cx, |dispatcher, cx| {
        dispatcher.dispatch(
            Command {
                name: UNDO_COMMAND.into(),
                arguments: CommandArgumentValue::Null,
            },
            target,
            cx,
        )
    });
    cx.update_window(window_handle, |_, window, cx| {
        shell.update(cx, |shell, cx| {
            shell.activate_tab(pane, second_tab, window, cx)
        });
    })
    .unwrap();

    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert_eq!(first_model.read(cx).text(), "");
        assert_eq!(second_model.read(cx).text(), "second");
        assert_eq!(
            shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .active_tab()
                .document_id()
                .unwrap(),
            second_document
        );
    });
}

#[gpui::test]
fn focus_transfer_and_distinct_views_break_typing_groups(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.update(super::bind_product_keys);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window_handle) = product_window(document, model.clone(), cx);
    cx.update_window(window_handle, |_, window, cx| {
        shell.update(cx, |shell, cx| {
            shell.split(SplitDirection::Horizontal, window, cx)
        });
    })
    .unwrap();
    cx.refresh().unwrap();
    cx.run_until_parked();
    let (first, second) = cx.read(|cx| {
        let workbench = shell.read(cx).workbench.read(cx);
        (
            workbench.panes()[0].active_tab().editor().unwrap().clone(),
            workbench.panes()[1].active_tab().editor().unwrap().clone(),
        )
    });
    cx.update_window(window_handle, |_, window, cx| {
        first.focus_handle(cx).focus(window)
    })
    .unwrap();
    cx.refresh().unwrap();
    cx.simulate_input(window_handle, "a");
    cx.update_window(window_handle, |_, window, cx| {
        second.focus_handle(cx).focus(window);
    })
    .unwrap();
    cx.refresh().unwrap();
    cx.run_until_parked();
    cx.update_window(window_handle, |_, window, cx| {
        first.focus_handle(cx).focus(window);
    })
    .unwrap();
    cx.refresh().unwrap();
    cx.run_until_parked();
    cx.simulate_input(window_handle, "b");
    cx.refresh().unwrap();
    cx.simulate_keystrokes(
        window_handle,
        if cfg!(windows) { "ctrl-z" } else { "cmd-z" },
    );
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(model.read(cx).text(), "a"));

    cx.simulate_keystrokes(
        window_handle,
        if cfg!(windows) {
            "ctrl-shift-z"
        } else {
            "cmd-shift-z"
        },
    );
    cx.run_until_parked();
    cx.update_window(window_handle, |_, window, cx| {
        second.focus_handle(cx).focus(window)
    })
    .unwrap();
    cx.refresh().unwrap();
    cx.run_until_parked();
    cx.simulate_input(window_handle, "X");
    cx.update_window(window_handle, |_, window, cx| {
        first.focus_handle(cx).focus(window)
    })
    .unwrap();
    cx.refresh().unwrap();
    cx.simulate_keystrokes(
        window_handle,
        if cfg!(windows) { "ctrl-z" } else { "cmd-z" },
    );
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(model.read(cx).text(), "ab"));
}

#[gpui::test]
fn shell_actions_render_tabs_and_both_split_directions(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, cx) = cx.add_window_view(|window, cx| {
        let workbench = cx.new(|cx| Workbench::new_for_document(document, model, cx));
        cx.global::<ApplicationWorkbenches>()
            .register(&workbench, window.window_handle());
        ProductShell::new(workbench)
    });

    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| shell.new_tab(window, cx));
        shell.update(cx, |shell, cx| {
            shell.split(SplitDirection::Horizontal, window, cx)
        });
        shell.update(cx, |shell, cx| {
            shell.split(SplitDirection::Vertical, window, cx)
        });
    });
    cx.run_until_parked();

    cx.read(|cx| {
        let workbench = shell.read(cx).workbench.read(cx);
        assert_eq!(workbench.panes().len(), 3);
        assert!(matches!(
            workbench.layout(),
            Some(WorkbenchLayout::Split {
                direction: SplitDirection::Horizontal,
                second,
                ..
            }) if matches!(
                second.as_ref(),
                WorkbenchLayout::Split {
                    direction: SplitDirection::Vertical,
                    ..
                }
            )
        ));
        assert_eq!(
            workbench
                .panes()
                .iter()
                .map(|pane| pane.tabs().len())
                .sum::<usize>(),
            4
        );
    });
}

#[gpui::test]
fn new_tab_targets_the_pane_whose_editor_has_focus(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, cx) = cx.add_window_view(|window, cx| {
        let workbench = cx.new(|cx| Workbench::new_for_document(document, model, cx));
        cx.global::<ApplicationWorkbenches>()
            .register(&workbench, window.window_handle());
        ProductShell::new(workbench)
    });

    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.split(SplitDirection::Horizontal, window, cx)
        });
    });
    cx.run_until_parked();
    cx.refresh().unwrap();

    let (first_pane, first_editor, second_editor) = cx.read(|cx| {
        let shell = shell.read(cx);
        let workbench = shell.workbench.read(cx);
        let first_pane = workbench.panes()[0].id();
        (
            first_pane,
            workbench
                .pane(first_pane)
                .unwrap()
                .active_tab()
                .editor()
                .unwrap()
                .clone(),
            workbench.panes()[1].active_tab().editor().unwrap().clone(),
        )
    });
    cx.update(|window, cx| second_editor.focus_handle(cx).focus(window));
    cx.run_until_parked();
    cx.refresh().unwrap();
    assert!(cx.update(|window, cx| second_editor.focus_handle(cx).is_focused(window)));
    cx.update(|window, cx| first_editor.focus_handle(cx).focus(window));
    cx.run_until_parked();
    cx.refresh().unwrap();
    assert!(cx.update(|window, cx| first_editor.focus_handle(cx).is_focused(window)));

    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| shell.new_tab(window, cx));
    });
    cx.run_until_parked();

    cx.read(|cx| {
        let workbench = shell.read(cx).workbench.read(cx);
        assert_eq!(workbench.pane(first_pane).unwrap().tabs().len(), 2);
        assert_eq!(
            workbench
                .panes()
                .iter()
                .find(|pane| pane.id() != first_pane)
                .unwrap()
                .tabs()
                .len(),
            1
        );
    });
}

#[gpui::test]
fn closing_the_final_clean_tab_creates_a_replacement(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let original = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(original).unwrap().model().clone());
    let (shell, cx) = cx.add_window_view(|window, cx| {
        let workbench = cx.new(|cx| Workbench::new_for_document(original, model, cx));
        cx.global::<ApplicationWorkbenches>()
            .register(&workbench, window.window_handle());
        ProductShell::new(workbench)
    });

    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| shell.close_active_tab(window, cx));
    });
    cx.run_until_parked();

    cx.read(|cx| {
        let replacement = shell
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab()
            .document_id()
            .unwrap();
        assert_ne!(replacement, original);
        assert!(documents.read(cx).get(original).is_none());
        assert!(documents.read(cx).get(replacement).is_some());
        assert_eq!(
            shell.read(cx).status,
            "created replacement untitled document"
        );
    });
}

#[gpui::test]
fn replacing_a_shared_blank_tab_preserves_the_other_windows_document(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let blank = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(blank).unwrap().model().clone());
    let (first, first_window) = product_window(blank, model.clone(), cx);
    let (second, _) = product_window(blank, model.clone(), cx);
    let replacement = cx.update(|cx| create_untitled_document(&documents, cx));
    cx.update_window(first_window, |_, _window, cx| {
        first.update(cx, |shell, cx| {
            let pane = shell.workbench.read(cx).focused_pane().unwrap();
            let target = super::OpenTarget {
                window: _window.window_handle(),
                workbench: shell.workbench.downgrade(),
                pane: pane.id(),
                tab: pane.active_tab_id(),
                surface: pane.active_tab().surface_id(),
                presentation: pane
                    .active_tab()
                    .editor()
                    .map(Entity::entity_id)
                    .or_else(|| pane.active_tab().terminal_view().map(Entity::entity_id)),
            };
            shell.present_open_document(replacement, &target, true, cx);
        });
    })
    .unwrap();
    cx.read(|cx| {
        assert!(documents.read(cx).get(blank).is_some());
        assert!(model.read(cx).is_open());
        assert_eq!(first.read(cx).workbench.read(cx).view_count(blank), 0);
        assert_eq!(second.read(cx).workbench.read(cx).view_count(blank), 1);
    });
}

#[gpui::test]
fn workbench_registry_counts_views_across_windows(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (_first, _) = product_window(document, model.clone(), cx);
    let (_second, _) = product_window(document, model, cx);
    cx.read(|cx| {
        assert_eq!(
            cx.global::<ApplicationWorkbenches>()
                .view_count(document, cx),
            2
        )
    });
}

#[gpui::test]
async fn retained_closed_shell_does_not_keep_the_last_live_document_open(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (closed_shell, closed_window) = product_window(document, model.clone(), cx);
    let (live_shell, live_window) = product_window(document, model.clone(), cx);
    assert_eq!(
        dispatch_product_command(
            &closed_shell,
            closed_window,
            super::CLOSE_WINDOW_COMMAND,
            cx
        )
        .completion
        .await
        .unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert_eq!(
            closed_shell
                .read(cx)
                .workbench
                .read(cx)
                .view_count(document),
            1
        );
        assert!(documents.read(cx).get(document).is_some());
    });
    assert_eq!(
        dispatch_product_command(&live_shell, live_window, CLOSE_TAB_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert!(documents.read(cx).get(document).is_none());
        assert!(!model.read(cx).is_open());
    });
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

#[gpui::test]
async fn cancelling_the_native_open_selection_settles_the_command(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.set_global(ApplicationOpenDialog(Arc::new(FixedOpenDialog(
        OpenDialogOutcome::Cancelled,
    ))));
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window_handle) = product_window(document, model, cx);
    let target = cx
        .update_window(window_handle, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.focus_active_editor(window, cx);
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let execution = cx.update(|cx| {
        let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
        dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: super::OPEN_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                target,
                cx,
            )
        })
    });

    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Cancelled
    );
    cx.read(|cx| assert_eq!(documents.read(cx).documents().count(), 1));
}

#[gpui::test]
async fn save_creates_a_destination_before_binding_it_as_persisted(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (filesystems, provider) = memory_filesystems_with_provider();
    cx.set_global(ApplicationFileSystems(filesystems));
    let uri = ResourceUri::parse("mem://product/created.txt").unwrap();
    let model = cx.new(|_| BufferModel::from_text("created text"));
    let document = documents.update(cx, |documents, cx| {
        documents.create_destination("created.txt", model.clone(), uri.clone(), cx)
    });
    let (shell, window_handle) = product_window(document, model, cx);
    let target = cx
        .update_window(window_handle, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.focus_active_editor(window, cx);
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let execution = cx.update(|cx| {
        let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
        dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: SAVE_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                target,
                cx,
            )
        })
    });

    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    assert_eq!(
        provider.read(uri.clone()).await.unwrap().bytes,
        b"created text"
    );
    cx.read(|cx| {
        let document = documents.read(cx).get(document).unwrap();
        assert_eq!(document.resource_uri(), Some(&uri));
        assert!(matches!(document.state(), DocumentState::Persisted { .. }));
        assert!(!document.is_dirty(cx));
    });
}

#[gpui::test]
async fn edit_racing_save_keeps_the_newer_revision_dirty(cx: &mut TestAppContext) {
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
    model.update(cx, |model, _| {
        let len = model.text().len();
        model.replace(0..len, "captured text").unwrap();
    });
    let captured_revision = model.read_with(cx, |model, _| model.revision());
    let (shell, window_handle) = product_window(document, model.clone(), cx);
    let (completion, receiver) = super::CommandCompletion::new();
    cx.update_window(window_handle, |_, window, cx| {
        let target = shell.update(cx, |shell, cx| {
            shell.focus_active_editor(window, cx);
            shell.capture_command_target(window, cx).unwrap()
        });
        shell.update(cx, |shell, cx| {
            shell.start_save_command(false, target, completion, cx)
        });
        model.update(cx, |model, _| {
            let len = model.text().len();
            model.replace(0..len, "newer text").unwrap();
        });
    })
    .unwrap();

    assert_eq!(receiver.await.unwrap(), CommandOutcome::Completed);
    assert_eq!(provider.read(uri).await.unwrap().bytes, b"captured text");
    cx.read(|cx| {
        let document = documents.read(cx).get(document).unwrap();
        assert_eq!(
            document.state().persisted_revision(),
            Some(captured_revision)
        );
        assert!(document.is_dirty(cx));
    });
}

#[gpui::test]
async fn save_completion_rejects_a_closed_captured_tab(cx: &mut TestAppContext) {
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
    let (shell, window_handle) = product_window(document, model.clone(), cx);
    let (completion, receiver) = super::CommandCompletion::new();
    cx.update_window(window_handle, |_, window, cx| {
        shell.update(cx, |shell, cx| {
            assert!(shell.split_pane(
                shell.workbench.read(cx).focused_pane_id().unwrap(),
                SplitDirection::Horizontal,
                window,
                cx,
            ));
        });
        let target = shell.update(cx, |shell, cx| {
            shell.capture_command_target(window, cx).unwrap()
        });
        shell.update(cx, |shell, cx| {
            shell.start_save_command(false, target.clone(), completion, cx);
            assert_eq!(
                shell.close_tab(
                    target.pane,
                    target.tab,
                    super::target_document(&target).unwrap(),
                    window,
                    cx
                ),
                CommandOutcome::Completed
            );
        });
    })
    .unwrap();

    assert_eq!(receiver.await.unwrap(), CommandOutcome::InvalidTarget);
    cx.read(|cx| {
        let document = documents.read(cx).get(document).unwrap();
        assert_eq!(document.state().persisted_revision(), Some(0));
        assert!(document.is_dirty(cx));
    });
}

#[gpui::test]
async fn save_as_overwrites_the_selected_file_and_retargets_only_after_success(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    let (filesystems, provider) = memory_filesystems_with_provider();
    cx.set_global(ApplicationFileSystems(filesystems));
    let uri = ResourceUri::parse("mem://product/notes.txt").unwrap();
    cx.set_global(ApplicationSaveDialog(Arc::new(FixedSaveDialog(
        SaveDialogOutcome::Selected(uri.clone()),
    ))));
    let model = cx.new(|_| BufferModel::from_text("replacement"));
    let document = cx.update(|cx| {
        documents.update(cx, |documents, cx| {
            documents.create_untitled("Untitled", model.clone(), cx)
        })
    });
    let (shell, window_handle) = product_window(document, model, cx);
    let target = cx
        .update_window(window_handle, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.focus_active_editor(window, cx);
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let execution = cx.update(|cx| {
        let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
        dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: SAVE_AS_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                target,
                cx,
            )
        })
    });

    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    assert_eq!(
        provider.read(uri.clone()).await.unwrap().bytes,
        b"replacement"
    );
    cx.read(|cx| {
        let document = documents.read(cx).get(document).unwrap();
        assert_eq!(document.resource_uri(), Some(&uri));
        assert_eq!(document.title(), "notes.txt");
    });
}

#[gpui::test]
async fn save_as_cancellation_preserves_untitled_identity(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.set_global(ApplicationSaveDialog(Arc::new(FixedSaveDialog(
        SaveDialogOutcome::Cancelled,
    ))));
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window_handle) = product_window(document, model, cx);
    let target = cx
        .update_window(window_handle, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.focus_active_editor(window, cx);
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let execution = cx.update(|cx| {
        let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
        dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: SAVE_AS_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                target,
                cx,
            )
        })
    });

    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Cancelled
    );
    cx.read(|cx| {
        assert!(matches!(
            documents.read(cx).get(document).unwrap().state(),
            DocumentState::Untitled { .. }
        ));
    });
}

#[gpui::test]
async fn save_as_failure_preserves_text_and_document_identity(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.set_global(ApplicationFileSystems(memory_filesystems()));
    cx.set_global(ApplicationSaveDialog(Arc::new(FixedSaveDialog(
        SaveDialogOutcome::Selected(ResourceUri::parse("mem://product/src").unwrap()),
    ))));
    let model = cx.new(|_| BufferModel::from_text("unsaved text"));
    let document = cx.update(|cx| {
        documents.update(cx, |documents, cx| {
            documents.create_untitled("Untitled", model.clone(), cx)
        })
    });
    let (shell, window_handle) = product_window(document, model.clone(), cx);
    let target = cx
        .update_window(window_handle, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.focus_active_editor(window, cx);
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let execution = cx.update(|cx| {
        let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
        dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: SAVE_AS_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                target,
                cx,
            )
        })
    });

    assert!(matches!(
        execution.completion.await.unwrap(),
        CommandOutcome::HandlerFailure { .. }
    ));
    cx.read(|cx| {
        let document = documents.read(cx).get(document).unwrap();
        assert!(matches!(document.state(), DocumentState::Untitled { .. }));
        assert_eq!(document.model(), &model);
        assert_eq!(model.read(cx).text(), "unsaved text");
    });
}

#[gpui::test]
async fn external_save_conflict_offers_reload_and_installs_the_external_version(
    cx: &mut TestAppContext,
) {
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
    model.update(cx, |model, _| model.replace(0..0, "local ").unwrap());
    provider.seed_file(uri.clone(), b"external text").unwrap();
    let (shell, window_handle) = product_window(document, model.clone(), cx);
    let target = cx
        .update_window(window_handle, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.focus_active_editor(window, cx);
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let execution = cx.update(|cx| {
        let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
        dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: SAVE_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                target,
                cx,
            )
        })
    });
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Reload");

    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert_eq!(model.read(cx).text(), "external text");
        assert!(!documents.read(cx).get(document).unwrap().is_dirty(cx));
    });
}

#[gpui::test]
async fn external_conflict_reload_rejects_an_edit_racing_its_read(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let root = ResourceUri::parse("mem://product/").unwrap();
    let provider = Arc::new(MemoryFileSystemProvider::new(root).unwrap());
    let uri = ResourceUri::parse("mem://product/notes.txt").unwrap();
    provider.seed_file(uri.clone(), b"loaded text").unwrap();
    let opened = provider.read(uri.clone()).await.unwrap();
    provider.seed_file(uri.clone(), b"external text").unwrap();
    let (read_release, gated_read) = tokio::sync::oneshot::channel();
    let gated_provider = Arc::new(GatedFileSystemProvider {
        inner: provider.clone(),
        read_release: Mutex::new(Some(gated_read)),
        replace_release: Mutex::new(None),
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
    model.update(cx, |model, _| model.replace(0..0, "local ").unwrap());
    let (shell, window_handle) = product_window(document, model.clone(), cx);
    let execution = dispatch_product_command(&shell, window_handle, SAVE_COMMAND, cx);
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Reload");
    cx.run_until_parked();

    model.update(cx, |model, _| model.replace(0..0, "newer ").unwrap());
    read_release.send(()).unwrap();

    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::InvalidTarget
    );
    assert_eq!(
        provider.read(uri.clone()).await.unwrap().bytes,
        b"external text"
    );
    cx.read(|cx| {
        let document = documents.read(cx).get(document).unwrap();
        assert_eq!(model.read(cx).text(), "newer local loaded text");
        assert_eq!(document.resource_uri(), Some(&uri));
        assert_eq!(document.state().persisted_revision(), Some(0));
        assert!(document.is_dirty(cx));
    });
}

#[gpui::test]
async fn cancelling_an_external_save_conflict_preserves_the_dirty_document(
    cx: &mut TestAppContext,
) {
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
    model.update(cx, |model, _| model.replace(0..0, "local ").unwrap());
    provider.seed_file(uri.clone(), b"external text").unwrap();
    let (shell, window_handle) = product_window(document, model.clone(), cx);
    let target = cx
        .update_window(window_handle, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.focus_active_editor(window, cx);
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let execution = cx.update(|cx| {
        let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
        dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: SAVE_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                target,
                cx,
            )
        })
    });
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Cancel");

    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Cancelled
    );
    assert_eq!(
        provider.read(uri.clone()).await.unwrap().bytes,
        b"external text"
    );
    cx.read(|cx| {
        let document = documents.read(cx).get(document).unwrap();
        assert_eq!(model.read(cx).text(), "local loaded text");
        assert_eq!(document.resource_uri(), Some(&uri));
        assert!(document.is_dirty(cx));
    });
}

#[gpui::test]
async fn save_as_after_an_external_conflict_retargets_only_after_saving(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (filesystems, provider) = memory_filesystems_with_provider();
    cx.set_global(ApplicationFileSystems(filesystems));
    let original_uri = ResourceUri::parse("mem://product/notes.txt").unwrap();
    let alternate_uri = ResourceUri::parse("mem://product/recovered.txt").unwrap();
    cx.set_global(ApplicationSaveDialog(Arc::new(FixedSaveDialog(
        SaveDialogOutcome::Selected(alternate_uri.clone()),
    ))));
    let opened = provider.read(original_uri.clone()).await.unwrap();
    let model = cx.new(|_| BufferModel::from_text("loaded text"));
    let document = documents.update(cx, |documents, cx| {
        documents.create_persisted(
            "notes.txt",
            model.clone(),
            original_uri.clone(),
            0,
            opened.version,
            cx,
        )
    });
    model.update(cx, |model, _| model.replace(0..0, "local ").unwrap());
    provider
        .seed_file(original_uri.clone(), b"external text")
        .unwrap();
    let (shell, window_handle) = product_window(document, model.clone(), cx);
    let target = cx
        .update_window(window_handle, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.focus_active_editor(window, cx);
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let execution = cx.update(|cx| {
        let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
        dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: SAVE_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                target,
                cx,
            )
        })
    });
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.read(|cx| {
        let document = documents.read(cx).get(document).unwrap();
        assert_eq!(document.resource_uri(), Some(&original_uri));
        assert!(document.is_dirty(cx));
    });
    cx.simulate_prompt_answer("Save As…");

    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    assert_eq!(
        provider.read(original_uri).await.unwrap().bytes,
        b"external text"
    );
    assert_eq!(
        provider.read(alternate_uri.clone()).await.unwrap().bytes,
        b"local loaded text"
    );
    cx.read(|cx| {
        let document = documents.read(cx).get(document).unwrap();
        assert_eq!(document.resource_uri(), Some(&alternate_uri));
        assert_eq!(document.title(), "recovered.txt");
        assert!(!document.is_dirty(cx));
    });
}

#[gpui::test]
async fn file_and_missing_file_open_replace_the_launch_placeholder(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.set_global(ApplicationFileSystems(memory_filesystems()));
    let placeholder = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(placeholder).unwrap().model().clone());
    let (shell, _) = product_window(placeholder, model, cx);

    let target = cx.read(|cx| current_open_target(&shell, cx));
    shell.update(cx, |shell, cx| {
        shell.start_open_request(
            OpenRequest::from_uri_for_product(
                ResourceUri::parse("mem://product/notes.txt").unwrap(),
            ),
            target,
            true,
            None,
            cx,
        );
    });
    cx.run_until_parked();

    cx.read(|cx| {
        let documents = documents.read(cx);
        assert_eq!(documents.documents().count(), 1);
        let document = documents.documents().next().unwrap();
        assert_eq!(document.model().read(cx).text(), "loaded text");
        assert!(matches!(document.state(), DocumentState::Persisted { .. }));
    });

    let target = cx.read(|cx| current_open_target(&shell, cx));
    shell.update(cx, |shell, cx| {
        shell.start_open_request(
            OpenRequest::from_uri_for_product(ResourceUri::parse("mem://product/new.txt").unwrap()),
            target,
            false,
            None,
            cx,
        );
    });
    cx.run_until_parked();
    cx.read(|cx| {
        let documents = documents.read(cx);
        let id = documents
            .document_for_resource(&ResourceUri::parse("mem://product/new.txt").unwrap())
            .unwrap();
        assert!(matches!(
            documents.get(id).unwrap().state(),
            DocumentState::Destination { .. }
        ));
    });
}

#[gpui::test]
async fn open_deduplicates_resources_and_rejects_stale_completions(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.set_global(ApplicationFileSystems(memory_filesystems()));
    let placeholder = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(placeholder).unwrap().model().clone());
    let (shell, _) = product_window(placeholder, model, cx);
    let target = cx.read(|cx| current_open_target(&shell, cx));

    shell.update(cx, |shell, cx| {
        shell.start_open_request(
            OpenRequest::from_uri_for_product(
                ResourceUri::parse("mem://product/notes.txt").unwrap(),
            ),
            target.clone(),
            true,
            None,
            cx,
        );
        shell.start_open_request(
            OpenRequest::from_uri_for_product(ResourceUri::parse("mem://product/new.txt").unwrap()),
            target,
            true,
            None,
            cx,
        );
    });
    cx.run_until_parked();

    cx.read(|cx| {
        let documents = documents.read(cx);
        assert_eq!(documents.documents().count(), 1);
        assert!(
            documents
                .document_for_resource(&ResourceUri::parse("mem://product/notes.txt").unwrap())
                .is_none()
        );
        assert!(
            documents
                .document_for_resource(&ResourceUri::parse("mem://product/new.txt").unwrap())
                .is_some()
        );
    });

    let target = cx.read(|cx| current_open_target(&shell, cx));
    shell.update(cx, |shell, cx| {
        shell.start_open_request(
            OpenRequest::from_uri_for_product(ResourceUri::parse("mem://product/new.txt").unwrap()),
            target,
            false,
            None,
            cx,
        );
    });
    cx.run_until_parked();
    cx.read(|cx| {
        assert_eq!(documents.read(cx).documents().count(), 1);
        assert_eq!(
            shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .tabs()
                .len(),
            1
        );
    });
}

#[gpui::test]
async fn folder_open_installs_a_workspace_and_failures_preserve_the_document(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    cx.set_global(ApplicationFileSystems(memory_filesystems()));
    let placeholder = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(placeholder).unwrap().model().clone());
    let (shell, _) = product_window(placeholder, model, cx);
    let target = cx.read(|cx| current_open_target(&shell, cx));

    shell.update(cx, |shell, cx| {
        shell.start_open_request(
            OpenRequest::from_uri_for_product(
                ResourceUri::parse("mem://product/invalid.txt").unwrap(),
            ),
            target,
            true,
            None,
            cx,
        );
    });
    cx.run_until_parked();
    cx.read(|cx| {
        assert_eq!(documents.read(cx).documents().count(), 1);
        assert_eq!(
            shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .active_tab()
                .document_id()
                .unwrap(),
            placeholder
        );
        assert!(shell.read(cx).status.contains("not valid UTF-8"));
    });

    let target = cx.read(|cx| current_open_target(&shell, cx));
    shell.update(cx, |shell, cx| {
        shell.start_open_request(
            OpenRequest::from_uri_for_product(ResourceUri::parse("mem://product/src").unwrap()),
            target,
            false,
            None,
            cx,
        );
    });
    cx.run_until_parked();
    cx.read(|cx| {
        let shell = shell.read(cx);
        assert_eq!(
            shell.workspace.snapshot().root().unwrap().to_string(),
            "mem://product/src"
        );
        assert!(shell.workspace_tree.is_some());
        assert_eq!(documents.read(cx).documents().count(), 1);
    });
}

#[gpui::test]
fn open_from_terminal_requests_picker_and_preserves_terminal(cx: &mut TestAppContext) {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), "opened from terminal").unwrap();
    let documents = install_globals(cx);
    cx.set_global(ApplicationOpenDialog(Arc::new(FixedOpenDialog(
        OpenDialogOutcome::Selected(file.path().to_path_buf()),
    ))));
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    let terminal_tab = shell.update(cx, |shell, cx| {
        let (session, view) = shell.create_terminal(cx);
        let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
        shell.workbench.update(cx, |workbench, _| {
            workbench.open_terminal_tab(pane, session, view).unwrap()
        })
    });
    cx.update_window(window, |_, window, cx| {
        shell.update(cx, |shell, cx| shell.focus_active_surface(window, cx));
    })
    .unwrap();
    let execution = dispatch_product_command(&shell, window, super::OPEN_COMMAND, cx);
    assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
    cx.read(|cx| {
        let pane = shell.read(cx).workbench.read(cx).focused_pane().unwrap();
        assert!(
            pane.tabs()
                .iter()
                .any(|tab| tab.id() == terminal_tab && tab.terminal_view().is_some())
        );
        assert_eq!(
            pane.active_tab()
                .editor()
                .unwrap()
                .read(cx)
                .model()
                .read(cx)
                .text(),
            "opened from terminal"
        );
    });
}

#[gpui::test]
async fn terminal_tab_has_a_captured_surface_and_document_commands_are_unavailable(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    let (pane, tab) = shell.update(cx, |shell, cx| {
        let (session, view) = shell.create_terminal(cx);
        let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
        let tab = shell.workbench.update(cx, |workbench, _| {
            workbench.open_terminal_tab(pane, session, view).unwrap()
        });
        (pane, tab)
    });
    let session = cx.read(|cx| {
        shell
            .read(cx)
            .workbench
            .read(cx)
            .pane(pane)
            .unwrap()
            .active_tab()
            .surface_id()
    });
    let target = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.activate_tab(pane, tab, window, cx);
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    assert_eq!(target.surface, session);
    cx.read(|cx| {
        assert_eq!(shell.read(cx).workbench.read(cx).view_count(document), 1);
        assert_eq!(
            cx.global::<ApplicationWorkbenches>()
                .view_count(document, cx),
            1
        );
    });
    let (completion, receiver) = super::CommandCompletion::new();
    shell.update(cx, |shell, cx| {
        shell.start_save_command(false, target.clone(), completion, cx)
    });
    assert_eq!(receiver.await.unwrap(), CommandOutcome::Unavailable);
    let dispatcher = cx.read(|cx| cx.global::<ApplicationProductCommands>().0.clone());
    let result = dispatcher.update(cx, |dispatcher, cx| {
        dispatcher.dispatch(
            Command {
                name: "editor.move-left".into(),
                arguments: CommandArgumentValue::Null,
            },
            target.clone(),
            cx,
        )
    });
    assert_eq!(
        result.completion.await.unwrap(),
        CommandOutcome::Unavailable
    );
    let mut wrong_surface = target;
    wrong_surface.surface = TabSurfaceId::Document(document);
    let result = dispatcher.update(cx, |dispatcher, cx| {
        dispatcher.dispatch(
            Command {
                name: NEW_COMMAND.into(),
                arguments: CommandArgumentValue::Null,
            },
            wrong_surface,
            cx,
        )
    });
    assert_eq!(
        result.completion.await.unwrap(),
        CommandOutcome::InvalidTarget
    );
}

#[gpui::test]
async fn closing_terminal_tab_preserves_dirty_document_and_invalidates_its_target(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    model.update(cx, |model, _| model.replace(0..0, "unsaved").unwrap());
    let (shell, window) = product_window(document, model, cx);
    let (pane, tab) = shell.update(cx, |shell, cx| {
        let (session, view) = shell.create_terminal(cx);
        let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
        let tab = shell.workbench.update(cx, |workbench, _| {
            workbench.open_terminal_tab(pane, session, view).unwrap()
        });
        (pane, tab)
    });
    let stale = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.activate_tab(pane, tab, window, cx);
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let execution = dispatch_product_command(&shell, window, CLOSE_TAB_COMMAND, cx);
    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    assert!(!cx.has_pending_prompt());
    cx.read(|cx| {
        let workbench = shell.read(cx).workbench.clone();
        assert_eq!(workbench.read(cx).tab_snapshots().len(), 1);
        assert_eq!(workbench.read(cx).view_count(document), 1);
        assert!(documents.read(cx).get(document).unwrap().is_dirty(cx));
    });
    let dispatcher = cx.read(|cx| cx.global::<ApplicationProductCommands>().0.clone());
    let result = dispatcher.update(cx, |dispatcher, cx| {
        dispatcher.dispatch(
            Command {
                name: NEW_COMMAND.into(),
                arguments: CommandArgumentValue::Null,
            },
            stale,
            cx,
        )
    });
    assert_eq!(
        result.completion.await.unwrap(),
        CommandOutcome::InvalidTarget
    );
}

#[gpui::test]
async fn mixed_window_and_quit_cancellation_preserve_every_surface(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    install_failing_recovery(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    model.update(cx, |model, _| model.replace(0..0, "unsaved").unwrap());
    let (shell, window) = product_window(document, model, cx);
    let (pane, tab) = shell.update(cx, |shell, cx| {
        let (session, view) = shell.create_terminal(cx);
        let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
        let tab = shell.workbench.update(cx, |workbench, _| {
            workbench.open_terminal_tab(pane, session, view).unwrap()
        });
        (pane, tab)
    });
    let session = cx.read(|cx| {
        shell
            .read(cx)
            .workbench
            .read(cx)
            .pane(pane)
            .unwrap()
            .active_tab()
            .surface_id()
    });
    cx.update_window(window, |_, window, cx| {
        shell.update(cx, |shell, cx| shell.activate_tab(pane, tab, window, cx));
    })
    .unwrap();
    for command in [CLOSE_WINDOW_COMMAND, QUIT_COMMAND] {
        let execution = dispatch_product_command(&shell, window, command, cx);
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Cancel");
        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::Cancelled
        );
        cx.read(|cx| {
            assert!(cx.windows().contains(&window));
            let snapshots = shell.read(cx).workbench.read(cx).tab_snapshots();
            assert_eq!(snapshots.len(), 2);
            assert!(
                snapshots
                    .iter()
                    .any(|tab| tab.surface == TabSurfaceId::Document(document))
            );
            assert!(snapshots.iter().any(|tab| tab.surface == session));
            let TabSurfaceId::Terminal(id) = session else {
                panic!()
            };
            let registry = cx.global::<ApplicationTerminalSessions>().0.borrow();
            assert_ne!(
                registry.sessions[&id].read(cx).status(),
                crate::app::terminal_session::TerminalStatus::Closed
            );
            assert!(documents.read(cx).get(document).unwrap().is_dirty(cx));
        });
    }
}

#[gpui::test]
async fn terminal_command_focuses_a_live_view_and_split_creates_an_independent_session(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);

    let command = dispatch_product_command(&shell, window, NEW_TERMINAL_COMMAND, cx);
    assert_eq!(command.completion.await.unwrap(), CommandOutcome::Completed);
    let (pane, first, document_tab) = cx.read(|cx| {
        let shell = shell.read(cx);
        let pane = shell.workbench.read(cx).focused_pane().unwrap();
        (
            pane.id(),
            pane.active_tab().surface_id(),
            pane.tabs()[0].id(),
        )
    });
    let TabSurfaceId::Terminal(first_id) = first else {
        panic!("terminal command opened a document")
    };
    cx.update_window(window, |_, window, cx| {
        let shell = shell.read(cx);
        let pane = shell.workbench.read(cx).pane(pane).unwrap();
        assert!(
            pane.active_tab()
                .terminal_view()
                .unwrap()
                .focus_handle(cx)
                .contains_focused(window, cx)
        );
    })
    .unwrap();
    cx.read(|cx| {
        assert!(
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions
                .contains_key(&first_id)
        );
    });

    cx.update_window(window, |_, window, cx| {
        shell.update(cx, |shell, cx| {
            shell.activate_tab(pane, document_tab, window, cx)
        });
    })
    .unwrap();
    cx.update_window(window, |_, window, cx| {
        shell.update(cx, |shell, cx| {
            let terminal_tab = shell.workbench.read(cx).pane(pane).unwrap().tabs()[1].id();
            shell.activate_tab(pane, terminal_tab, window, cx);
        });
    })
    .unwrap();
    cx.read(|cx| {
        assert_eq!(
            shell
                .read(cx)
                .workbench
                .read(cx)
                .pane(pane)
                .unwrap()
                .active_tab()
                .surface_id(),
            first
        );
        assert!(
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions
                .contains_key(&first_id)
        );
    });

    let split = dispatch_product_command(&shell, window, SPLIT_HORIZONTAL_COMMAND, cx);
    assert_eq!(split.completion.await.unwrap(), CommandOutcome::Completed);
    let second_id = cx.read(|cx| {
        let shell = shell.read(cx);
        let workbench = shell.workbench.read(cx);
        assert_eq!(workbench.panes().len(), 2);
        match workbench.focused_pane().unwrap().active_tab().surface_id() {
            TabSurfaceId::Terminal(id) => id,
            TabSurfaceId::Document(_) | TabSurfaceId::Tool(_) => {
                panic!("split did not create terminal")
            }
        }
    });
    assert_ne!(first_id, second_id);
    cx.read(|cx| {
        let registry = cx.global::<ApplicationTerminalSessions>().0.borrow();
        assert_eq!(registry.sessions.len(), 2);
        assert!(registry.sessions.contains_key(&first_id));
        assert!(registry.sessions.contains_key(&second_id));
    });
}

#[gpui::test]
async fn terminal_close_and_reopen_settle_the_exact_session(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    assert_eq!(
        dispatch_product_command(&shell, window, NEW_TERMINAL_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    let (first_id, first_session) = cx.read(|cx| {
        let TabSurfaceId::Terminal(id) = shell
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab()
            .surface_id()
        else {
            panic!()
        };
        (
            id,
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions[&id]
                .clone(),
        )
    });
    assert_eq!(
        dispatch_product_command(&shell, window, CLOSE_TAB_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert_eq!(
            first_session.read(cx).status(),
            crate::app::terminal_session::TerminalStatus::Closed
        );
        assert!(
            !cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions
                .contains_key(&first_id)
        );
    });
    assert_eq!(
        dispatch_product_command(&shell, window, NEW_TERMINAL_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        let TabSurfaceId::Terminal(second_id) = shell
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab()
            .surface_id()
        else {
            panic!()
        };
        assert_ne!(first_id, second_id);
        assert!(
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions
                .contains_key(&second_id)
        );
    });
}

#[gpui::test]
async fn closing_the_final_terminal_tab_installs_an_untitled_workbench(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    assert_eq!(
        dispatch_product_command(&shell, window, NEW_TERMINAL_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    let (pane, document_tab, session) = cx.read(|cx| {
        let shell = shell.read(cx);
        let pane = shell.workbench.read(cx).focused_pane().unwrap();
        let TabSurfaceId::Terminal(id) = pane.active_tab().surface_id() else {
            panic!()
        };
        (
            pane.id(),
            pane.tabs()[0].id(),
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions[&id]
                .clone(),
        )
    });
    cx.update_window(window, |_, window, cx| {
        shell.update(cx, |shell, cx| {
            shell.activate_tab(pane, document_tab, window, cx)
        });
    })
    .unwrap();
    assert_eq!(
        dispatch_product_command(&shell, window, CLOSE_TAB_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        let shell = shell.read(cx);
        let tabs = shell.workbench.read(cx).tab_snapshots();
        assert_eq!(tabs.len(), 1);
        assert!(matches!(tabs[0].surface, TabSurfaceId::Terminal(_)));
        assert!(documents.read(cx).get(document).is_none());
    });
    assert_eq!(
        dispatch_product_command(&shell, window, CLOSE_TAB_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        let shell = shell.read(cx);
        let tabs = shell.workbench.read(cx).tab_snapshots();
        assert_eq!(tabs.len(), 1);
        assert!(matches!(tabs[0].surface, TabSurfaceId::Document(_)));
        assert_eq!(
            session.read(cx).status(),
            crate::app::terminal_session::TerminalStatus::Closed
        );
        assert!(
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions
                .is_empty()
        );
    });
}

#[gpui::test]
async fn window_close_shuts_down_terminal_after_protected_decisions(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    assert_eq!(
        dispatch_product_command(&shell, window, NEW_TERMINAL_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    let session = cx.read(|cx| {
        let TabSurfaceId::Terminal(id) = shell
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab()
            .surface_id()
        else {
            panic!()
        };
        cx.global::<ApplicationTerminalSessions>()
            .0
            .borrow()
            .sessions[&id]
            .clone()
    });
    assert_eq!(
        dispatch_product_command(&shell, window, CLOSE_WINDOW_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert!(!cx.windows().contains(&window));
        assert_eq!(
            session.read(cx).status(),
            crate::app::terminal_session::TerminalStatus::Closed
        );
        assert!(
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions
                .is_empty()
        );
    });
}

#[gpui::test]
async fn mixed_window_save_failure_keeps_terminal_running(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.set_global(ApplicationFileSystems(memory_filesystems()));
    cx.set_global(ApplicationSaveDialog(Arc::new(FixedSaveDialog(
        SaveDialogOutcome::Selected(ResourceUri::parse("mem://product/src").unwrap()),
    ))));
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    model.update(cx, |model, _| model.replace(0..0, "unsaved").unwrap());
    let (shell, window) = product_window(document, model, cx);
    assert_eq!(
        dispatch_product_command(&shell, window, NEW_TERMINAL_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    let (id, session) = cx.read(|cx| {
        let TabSurfaceId::Terminal(id) = shell
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab()
            .surface_id()
        else {
            panic!()
        };
        (
            id,
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions[&id]
                .clone(),
        )
    });
    let execution = dispatch_product_command(&shell, window, CLOSE_WINDOW_COMMAND, cx);
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
        assert!(
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions
                .contains_key(&id)
        );
        assert_ne!(
            session.read(cx).status(),
            crate::app::terminal_session::TerminalStatus::Closed
        );
        assert!(documents.read(cx).get(document).unwrap().is_dirty(cx));
    });
}

#[gpui::test]
async fn quit_shuts_down_terminal_sessions_in_every_window(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let first = cx.update(|cx| create_untitled_document(&documents, cx));
    let first_model = cx.read(|cx| documents.read(cx).get(first).unwrap().model().clone());
    let (first_shell, first_window) = product_window(first, first_model, cx);
    let second = cx.update(|cx| create_untitled_document(&documents, cx));
    let second_model = cx.read(|cx| documents.read(cx).get(second).unwrap().model().clone());
    let (second_shell, second_window) = product_window(second, second_model, cx);
    for (shell, window) in [(&first_shell, first_window), (&second_shell, second_window)] {
        assert_eq!(
            dispatch_product_command(shell, window, NEW_TERMINAL_COMMAND, cx)
                .completion
                .await
                .unwrap(),
            CommandOutcome::Completed
        );
    }
    let sessions = cx.read(|cx| {
        cx.global::<ApplicationTerminalSessions>()
            .0
            .borrow()
            .sessions
            .values()
            .cloned()
            .collect::<Vec<_>>()
    });
    assert_eq!(sessions.len(), 2);
    assert_eq!(
        dispatch_product_command(&first_shell, first_window, QUIT_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert!(
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions
                .is_empty()
        );
        for session in &sessions {
            assert_eq!(
                session.read(cx).status(),
                crate::app::terminal_session::TerminalStatus::Closed
            );
        }
    });
}

#[gpui::test]
async fn moving_terminal_keeps_its_session_and_source_window_close_does_not_stop_it(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (source, source_window) = product_window(document, model, cx);
    assert_eq!(
        dispatch_product_command(&source, source_window, NEW_TERMINAL_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    let (id, session, grid, source_view) = cx.read(|cx| {
        let shell = source.read(cx);
        let tab = shell
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab();
        let TabSurfaceId::Terminal(id) = tab.surface_id() else {
            panic!()
        };
        let session = cx
            .global::<ApplicationTerminalSessions>()
            .0
            .borrow()
            .sessions[&id]
            .clone();
        let grid = session.read(cx).terminal().unwrap().clone();
        (id, session, grid, tab.terminal_view().unwrap().clone())
    });
    assert_eq!(
        dispatch_product_command(
            &source,
            source_window,
            MOVE_TERMINAL_TO_NEW_WINDOW_COMMAND,
            cx
        )
        .completion
        .await
        .unwrap(),
        CommandOutcome::Completed
    );
    let destination_window = cx.read(|cx| {
        *cx.windows()
            .iter()
            .find(|handle| **handle != source_window)
            .unwrap()
    });
    let destination = cx
        .update_window(destination_window, |_, window, _| {
            window.root::<ProductShell>().flatten().unwrap()
        })
        .unwrap();
    let destination_view = cx.read(|cx| {
        destination
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab()
            .terminal_view()
            .unwrap()
            .clone()
    });
    cx.update_window(destination_window, |_, window, cx| {
        assert!(
            destination_view
                .focus_handle(cx)
                .contains_focused(window, cx)
        );
    })
    .unwrap();
    let destination_size = crate::app::terminal_session::TerminalSize {
        columns: 80,
        lines: 20,
        cell_width: 9,
        cell_height: 18,
    };
    destination_view.update(cx, |view, cx| view.resize(destination_size, cx));
    cx.read(|cx| assert_eq!(session.read(cx).size(), destination_size));
    let stale_size = crate::app::terminal_session::TerminalSize {
        columns: 40,
        lines: 10,
        cell_width: 9,
        cell_height: 18,
    };
    source_view.update(cx, |view, cx| view.resize(stale_size, cx));
    cx.read(|cx| assert_eq!(session.read(cx).size(), destination_size));
    cx.read(|cx| {
        assert!(
            !source
                .read(cx)
                .workbench
                .read(cx)
                .tab_snapshots()
                .iter()
                .any(|tab| tab.surface == TabSurfaceId::Terminal(id))
        );
        let destination_shell = destination.read(cx);
        let tab = destination_shell
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab();
        assert_eq!(tab.surface_id(), TabSurfaceId::Terminal(id));
        assert_ne!(tab.terminal_view(), Some(&source_view));
        assert!(std::sync::Arc::ptr_eq(
            session.read(cx).terminal().unwrap(),
            &grid
        ));
        assert_eq!(
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions[&id],
            session
        );
    });
    assert_eq!(
        dispatch_product_command(&source, source_window, CLOSE_WINDOW_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert_eq!(
            session.read(cx).status(),
            crate::app::terminal_session::TerminalStatus::Running
        );
        assert!(cx.windows().contains(&destination_window));
    });
    assert_eq!(
        dispatch_product_command(&destination, destination_window, CLOSE_TAB_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert_eq!(
            session.read(cx).status(),
            crate::app::terminal_session::TerminalStatus::Closed
        );
        assert!(
            !cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions
                .contains_key(&id)
        );
    });
}

#[gpui::test]
async fn failed_terminal_destination_leaves_source_attached(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    assert_eq!(
        dispatch_product_command(&shell, window, NEW_TERMINAL_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    let outcome = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                let target = shell.capture_command_target(window, cx).unwrap();
                shell.move_terminal_to_new_window_with(&target, window, cx, |_| {
                    anyhow::bail!("simulated window creation failure")
                })
            })
        })
        .unwrap();
    assert!(matches!(outcome, CommandOutcome::HandlerFailure { .. }));
    cx.read(|cx| {
        let shell = shell.read(cx);
        let tab = shell
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab();
        let TabSurfaceId::Terminal(id) = tab.surface_id() else {
            panic!()
        };
        assert!(tab.terminal_view().is_some());
        assert!(
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions
                .contains_key(&id)
        );
        assert_eq!(cx.windows().len(), 1);
    });
}

#[gpui::test]
async fn vanished_terminal_destination_leaves_source_attached(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    assert_eq!(
        dispatch_product_command(&shell, window, MOVE_TERMINAL_TO_NEW_WINDOW_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Unavailable
    );
    assert_eq!(
        dispatch_product_command(&shell, window, NEW_TERMINAL_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    let outcome = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                let target = shell.capture_command_target(window, cx).unwrap();
                shell.move_terminal_to_new_window_with(&target, window, cx, |cx| {
                    let destination = open_tab_transfer_window(cx)?;
                    cx.update_window(destination.into(), |_, window, _| window.remove_window())?;
                    Ok(destination)
                })
            })
        })
        .unwrap();
    assert_eq!(outcome, CommandOutcome::InvalidTarget);
    cx.read(|cx| {
        let shell = shell.read(cx);
        let tab = shell
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab();
        let TabSurfaceId::Terminal(id) = tab.surface_id() else {
            panic!()
        };
        assert!(tab.terminal_view().is_some());
        assert!(
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions
                .contains_key(&id)
        );
        assert_eq!(cx.windows().len(), 1);
    });
}

#[gpui::test]
async fn stale_terminal_move_target_is_rejected(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    assert_eq!(
        dispatch_product_command(&shell, window, NEW_TERMINAL_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    let target = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    assert_eq!(
        dispatch_product_command(&shell, window, CLOSE_TAB_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    let command = crate::host::protocol::Command {
        name: MOVE_TERMINAL_TO_NEW_WINDOW_COMMAND.into(),
        arguments: crate::host::protocol::CommandArgumentValue::Null,
    };
    let execution = cx.update(|cx| {
        let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
        dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(command, target, cx)
        })
    });
    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::InvalidTarget
    );
    cx.read(|cx| assert_eq!(cx.windows().len(), 1));
}

#[gpui::test]
async fn process_exit_during_terminal_handoff_keeps_final_grid(cx: &mut TestAppContext) {
    use alacritty_terminal::tty::{Options, Shell};
    use std::{
        thread,
        time::{Duration, Instant},
    };

    #[cfg(windows)]
    let shell = Shell::new(
        "cmd.exe".into(),
        vec!["/C".into(), "echo handoff-output & exit /b 7".into()],
    );
    #[cfg(not(windows))]
    let shell = Shell::new(
        "/bin/sh".into(),
        vec![
            "-c".into(),
            "printf 'handoff-output\\n'; sleep 0.05; exit 7".into(),
        ],
    );

    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (source, source_window) = product_window(document, model, cx);
    let session = cx.update(|cx| {
        cx.new(|cx| {
            crate::app::terminal_session::TerminalSession::new_with_options(
                Options {
                    shell: Some(shell),
                    drain_on_exit: true,
                    ..Default::default()
                },
                cx,
            )
        })
    });
    let id = source.update(cx, |shell, cx| {
        let id = cx
            .global::<ApplicationTerminalSessions>()
            .0
            .borrow_mut()
            .allocate_id();
        cx.global::<ApplicationTerminalSessions>()
            .0
            .borrow_mut()
            .sessions
            .insert(id, session.clone());
        let view = cx.new(|cx| crate::app::terminal_view::TerminalView::new(session.clone(), cx));
        let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
        shell.workbench.update(cx, |workbench, _| {
            workbench.open_terminal_tab(pane, id, view).unwrap()
        });
        id
    });
    let outcome = cx
        .update_window(source_window, |_, window, cx| {
            source.update(cx, |shell, cx| {
                shell.focus_active_surface(window, cx);
                let target = shell.capture_command_target(window, cx).unwrap();
                shell.move_terminal_to_new_window_with(&target, window, cx, |cx| {
                    thread::sleep(Duration::from_millis(100));
                    open_tab_transfer_window(cx)
                })
            })
        })
        .unwrap();
    assert_eq!(outcome, CommandOutcome::Completed);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        cx.run_until_parked();
        if cx.read(|cx| {
            matches!(
                session.read(cx).status(),
                crate::app::terminal_session::TerminalStatus::Exited(_)
            )
        }) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "terminal did not exit after handoff"
        );
        thread::sleep(Duration::from_millis(5));
    }
    cx.read(|cx| {
        assert!(matches!(
            session.read(cx).status(),
            crate::app::terminal_session::TerminalStatus::Exited(_)
        ));
        let terminal = session.read(cx).terminal().unwrap().lock();
        let output: String = terminal
            .renderable_content()
            .display_iter
            .map(|cell| cell.cell.c)
            .collect();
        assert!(output.contains("handoff-output"));
        assert!(
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions
                .contains_key(&id)
        );
    });
}
