//! Product shell integration tests and their shared fixtures.

mod closure;
mod commands;
mod editing;
mod extension_api;
mod extension_restart;
mod git_status;
mod keybindings;
mod languages;
mod observation;
mod operations;
mod persistence;
mod project_sessions;
mod projects;
mod recovery;
mod sessions;
mod startup;
mod terminal;
mod tool_layout;
mod tools;
mod transient;
mod window_restore;
mod workbench;
mod workspaces;
use std::path::Path;
use std::sync::{Arc, Mutex};

use gpui::{
    AppContext, Focusable, Modifiers, MouseButton, TestAppContext, VisualTestContext, point, px,
};

use super::super::commands::keymaps::{ApplicationKeymaps, BindingOwner, BindingResolution};
use crate::host::protocol::{
    Command, CommandArgumentValue, CommandOutcome, ExtensionId, ExtensionLifecycleId,
};

use super::{
    ApplicationDocuments, ApplicationFileSystems, ApplicationLaunchGate, ApplicationOpenDialog,
    ApplicationProductCommands, ApplicationProtectedClosure, ApplicationSaveDialog,
    ApplicationTerminalSessions, ApplicationWorkbenches, BufferModel, CLOSE_TAB_COMMAND,
    CLOSE_WINDOW_COMMAND, DocumentCollection, Entity, LaunchGate, NEW_COMMAND,
    NEW_TERMINAL_COMMAND, OpenDialogFuture, OpenDialogOutcome, OpenRequest, OpenTarget,
    ProductCommandDispatcher, ProductCommandSource, ProductOpenDialog, ProductSaveDialog,
    ProductShell, QUIT_COMMAND, REDO_COMMAND, RefCell, SAVE_AS_COMMAND, SAVE_COMMAND,
    SHOW_EXTENSION_REPORT_COMMAND, SPLIT_HORIZONTAL_COMMAND, SaveDialogFuture, SaveDialogOutcome,
    SplitDirection, TerminalSessions, UNDO_COMMAND, Workbench, WorkbenchLayout,
    create_untitled_document, install_protected_window_close, open_tab_transfer_window,
    product_filesystems,
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
    inner: Arc<dyn FileSystemProvider>,
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
    _host: &Entity<super::super::extensions::host::ProductExtensionHost>,
    cx: &mut TestAppContext,
) -> super::super::extensions::host::StartupReportSnapshot {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        cx.run_until_parked();
        if let Some(report) = cx.read(super::super::extensions::host::startup_report)
            && report.state == super::super::extensions::host::StartupState::Complete
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

fn wait_for_command(
    mut execution: crate::app::CommandExecution,
    cx: &mut TestAppContext,
) -> CommandOutcome {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        cx.run_until_parked();
        match execution.completion.try_recv() {
            Ok(outcome) => return outcome,
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "command did not settle; pending prompt: {:?}",
                    cx.pending_prompt()
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
