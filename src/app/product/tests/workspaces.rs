//! Window-owned workspace operations through the public host.

use super::operations::{empty_product_window, install_operation_fixture};
use super::*;
use crate::app::extensions;

#[gpui::test]
fn public_workspace_operations_validate_before_commit_and_preserve_documents(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    let (filesystems, _) = memory_filesystems_with_provider();
    cx.set_global(ApplicationFileSystems(filesystems));
    let host = cx.update(extensions::host::install);
    let key = install_operation_fixture(
        &host,
        r#"
import { commands, documents, windows, workspaces } from "knot";
async function rejects(action, code) {
  try { await action(); } catch (error) { if (error.code === code) return; throw error; }
  throw new Error(`expected ${code}`);
}
await rejects(() => workspaces.openFolder("mem://product/src"), "ContextUnavailable");
await rejects(() => workspaces.replaceRoots([]), "ContextUnavailable");
await commands.register("workspaces.validate", async ({ target: command }) => {
  const wins = await windows.list(), target = wins[1].target;
  const ids = (await documents.list()).map(document => document.id).join();
  const first = await workspaces.openFolder("mem://product/src", { target });
  if (first.window !== target || first.generation !== 1 || first.roots.length !== 1 || first.roots[0].uri !== "mem://product/src") throw new Error("folder result");
  for (const value of [first, first.roots, first.roots[0]]) if (!Object.isFrozen(value)) throw new Error("mutable workspace");
  const retained = await workspaces.replaceRoots(["mem://product/./src"], { target });
  if (retained.roots[0].id !== first.roots[0].id || retained.generation !== 2) throw new Error("retained root identity");
  await rejects(() => workspaces.openFolder("mem://product/notes.txt", { target }), "ResourceWrongKind");
  await rejects(() => workspaces.openFolder("mem://product/invalid.txt", { target }), "ResourceWrongKind");
  await rejects(() => workspaces.openFolder("mem://product/missing", { target }), "ResourceNotFound");
  await rejects(() => workspaces.replaceRoots(["mem://product/missing"], { target }), "ResourceNotFound");
  await rejects(() => workspaces.replaceRoots(["not a URI"], { target }), "InvalidUri");
  await rejects(() => workspaces.openFolder("other://product/src", { target }), "ProviderFailure");
  for (const invalid of [{}, command, wins[0].panes[0].target, wins[0].panes[0].selected]) await rejects(() => workspaces.replaceRoots([], { target: invalid }), "InvalidTarget");
  if (JSON.stringify(await workspaces.get(target)) !== JSON.stringify(retained)) throw new Error("failed validation changed roots");
  const cleared = await workspaces.replaceRoots([], { target });
  if (cleared.generation !== 3 || cleared.roots.length !== 0) throw new Error("clear");
  const restored = await workspaces.openFolder("mem://product/src", { target });
  if (restored.generation !== 4 || restored.roots[0].id === first.roots[0].id) throw new Error("reinstall");
  const both = await workspaces.replaceRoots(["mem://product/", "mem://product/src", "mem://product/./src"], { target });
  if (both.generation !== 5 || both.roots.length !== 2 || both.roots[1].id !== restored.roots[0].id) throw new Error("ordered, deduplicated roots");
  if ((await workspaces.get(wins[0].target)).roots.length !== 0) throw new Error("wrong window mutated");
  const implicit = await workspaces.openFolder("mem://product/src");
  if (implicit.window !== wins[0].target) throw new Error("originating command window lost");
  if ((await documents.list()).map(document => document.id).join() !== ids) throw new Error("root change closed documents");
});
"#,
        "workspaces.validate",
        cx,
    );
    let (first, window) = empty_product_window(&documents, cx);
    let (second, _) = empty_product_window(&documents, cx);
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&first, window, "workspaces.validate", cx),
            cx
        ),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert_eq!(first.read(cx).workspace.snapshot().generation(), 1);
        assert_eq!(second.read(cx).workspace.snapshot().generation(), 5);
        assert!(second.read(cx).workspace_tree.is_some());
        assert_eq!(documents.read(cx).documents().count(), 2);
    });
    host.update(cx, |host, cx| host.unload(key, cx).unwrap());
}

struct GatedStatProvider {
    inner: Arc<MemoryFileSystemProvider>,
    release: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}

impl FileSystemProvider for GatedStatProvider {
    fn normalize(&self, uri: ResourceUri) -> ProviderFuture<'_, ResourceUri> {
        self.inner.normalize(uri)
    }
    fn enumerate(&self, uri: ResourceUri) -> ProviderFuture<'_, Vec<ResourceEntry>> {
        self.inner.enumerate(uri)
    }
    fn read(&self, uri: ResourceUri) -> ProviderFuture<'_, ResourceFile> {
        self.inner.read(uri)
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
        self.inner.replace(uri, expected, bytes)
    }
    fn stat(&self, uri: ResourceUri) -> ProviderFuture<'_, ResourceStat> {
        let release = self.release.lock().unwrap().take();
        Box::pin(async move {
            if let Some(release) = release {
                let _ = release.await;
            }
            self.inner.stat(uri).await
        })
    }
}

#[gpui::test]
fn workspace_validation_retains_window_and_rejects_superseded_or_unloaded_work(
    cx: &mut TestAppContext,
) {
    for scenario in ["focus", "roots", "close", "unload"] {
        let documents = install_globals(cx);
        let (_, inner) = memory_filesystems_with_provider();
        let (release, gate) = tokio::sync::oneshot::channel();
        let provider = Arc::new(GatedStatProvider {
            inner,
            release: Mutex::new(Some(gate)),
        });
        let mut filesystems = FileSystemProviderRegistry::new();
        filesystems.register("mem", provider.clone()).unwrap();
        cx.set_global(ApplicationFileSystems(Arc::new(filesystems)));
        let host = cx.update(extensions::host::install);
        let (first, window) = empty_product_window(&documents, cx);
        let (second, second_window) = empty_product_window(&documents, cx);
        let key = install_operation_fixture(
            &host,
            r#"
import { commands, workspaces, workbench } from "knot";
await commands.register("workspaces.pending", async () => {
  try {
    const result = await workspaces.openFolder("mem://product/src");
    await workbench.writeClipboardText(`opened:${result.roots[0].uri}`);
  } catch (error) {
    if (error.code !== "InvalidTarget") throw error;
    await workbench.writeClipboardText(error.code);
  }
});
"#,
            "workspaces.pending",
            cx,
        );
        let pending = dispatch_product_command(&first, window, "workspaces.pending", cx);
        wait_for_api_condition(cx, |_| provider.release.lock().unwrap().is_none());
        match scenario {
            "focus" => {
                cx.update_window(second_window, |_, window, cx| {
                    window.activate_window();
                    second.read(cx).focus_active_surface(window, cx);
                })
                .unwrap();
            }
            "roots" => {
                first.update(cx, |shell, cx| {
                    shell.install_workspace(ResourceUri::parse("mem://product/").unwrap(), cx)
                });
            }
            "close" => {
                assert_eq!(
                    wait_for_command(
                        dispatch_product_command(&first, window, CLOSE_WINDOW_COMMAND, cx),
                        cx
                    ),
                    CommandOutcome::Completed
                );
            }
            "unload" => host.update(cx, |host, cx| host.unload(key, cx).unwrap()),
            _ => unreachable!(),
        }
        release.send(()).unwrap();
        let outcome = wait_for_command(pending, cx);
        assert_eq!(
            outcome,
            match scenario {
                "close" => CommandOutcome::InvalidTarget,
                "unload" => CommandOutcome::Cancelled,
                _ => CommandOutcome::Completed,
            }
        );
        cx.run_until_parked();
        let snapshot = cx.read(|cx| first.read(cx).workspace.snapshot());
        match scenario {
            "focus" => {
                assert_eq!(
                    snapshot.first_root().unwrap().to_string(),
                    "mem://product/src"
                );
                assert!(cx.read(|cx| second.read(cx).workspace.snapshot().roots().is_empty()));
            }
            "roots" => {
                assert_eq!(snapshot.first_root().unwrap().to_string(), "mem://product/");
            }
            "close" | "unload" => assert!(snapshot.roots().is_empty()),
            _ => unreachable!(),
        }
        if scenario != "unload" {
            assert_eq!(
                cx.read(|cx| cx.read_from_clipboard().unwrap().text().unwrap()),
                if scenario == "focus" {
                    "opened:mem://product/src"
                } else {
                    "InvalidTarget"
                }
            );
            host.update(cx, |host, cx| host.unload(key, cx).unwrap());
        }
        if scenario != "close" {
            cx.update_window(window, |_, window, _| window.remove_window())
                .unwrap();
        }
        cx.update_window(second_window, |_, window, _| window.remove_window())
            .unwrap();
    }
}

#[gpui::test]
fn explicit_workspace_operations_work_in_an_empty_product_workbench(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (filesystems, _) = memory_filesystems_with_provider();
    cx.set_global(ApplicationFileSystems(filesystems));
    let window = cx.update(|cx| open_tab_transfer_window(cx).unwrap());
    let shell = cx
        .update_window(*window, |root, _, _| {
            root.downcast::<ProductShell>().unwrap()
        })
        .unwrap();
    let host = cx.update(extensions::host::install);
    let key = install_operation_fixture(
        &host,
        r#"
import { commands, windows, workspaces } from "knot";
const window = (await windows.list())[0];
if (window.panes.length !== 0) throw new Error("expected empty workbench");
const opened = await workspaces.openFolder("mem://product/src", { target: window.target });
if (opened.generation !== 1 || opened.window !== window.target) throw new Error("empty window roots");
const cleared = await workspaces.replaceRoots([], { target: window.target });
if (cleared.generation !== 2 || cleared.roots.length !== 0) throw new Error("empty window clear");
await commands.register("workspaces.empty-ready", () => {});
"#,
        "workspaces.empty-ready",
        cx,
    );
    cx.read(|cx| {
        assert!(shell.read(cx).workspace_tree.is_some());
        assert!(
            shell
                .read(cx)
                .workspace_tree
                .as_ref()
                .unwrap()
                .read(cx)
                .workspace()
                .roots()
                .is_empty()
        );
        assert_eq!(shell.read(cx).workspace.snapshot().generation(), 2);
        assert_eq!(documents.read(cx).documents().count(), 0);
    });
    host.update(cx, |host, cx| host.unload(key, cx).unwrap());
}

#[gpui::test]
fn native_root_commands_add_reorder_and_remove_roots_without_closing_documents(
    cx: &mut TestAppContext,
) {
    let first_dir = tempfile::tempdir().unwrap();
    let second_dir = tempfile::tempdir().unwrap();
    let file_uri = |path: &std::path::Path| {
        ResourceUri::parse(url::Url::from_file_path(path).unwrap().as_str()).unwrap()
    };
    let first = file_uri(first_dir.path());
    let documents = install_globals(cx);
    cx.set_global(ApplicationOpenDialog(Arc::new(FixedOpenDialog(
        OpenDialogOutcome::Selected(second_dir.path().to_path_buf()),
    ))));
    let (shell, window) = empty_product_window(&documents, cx);
    shell.update(cx, |shell, cx| {
        shell.install_workspace(first.clone(), cx);
    });
    let roots = |cx: &TestAppContext| {
        cx.read(|cx| {
            shell
                .read(cx)
                .workspace
                .snapshot()
                .roots()
                .iter()
                .map(|root| (root.id, root.uri.clone()))
                .collect::<Vec<_>>()
        })
    };
    let installed = roots(cx);

    // Removing or moving without a selected root is unavailable.
    for command in [
        super::super::commands::REMOVE_FOLDER_COMMAND,
        super::super::commands::MOVE_FOLDER_UP_COMMAND,
    ] {
        assert_eq!(
            wait_for_command(dispatch_product_command(&shell, window, command, cx), cx),
            CommandOutcome::Unavailable
        );
    }

    let add = super::super::commands::ADD_FOLDER_COMMAND;
    assert_eq!(
        wait_for_command(dispatch_product_command(&shell, window, add, cx), cx),
        CommandOutcome::Completed
    );
    wait_for_api_condition(cx, |cx| {
        shell.read(cx).workspace.snapshot().roots().len() == 2
    });
    let added = roots(cx);
    assert_eq!(added[0], installed[0]);
    let second = added[1].1.clone();
    assert_eq!(second, file_uri(second_dir.path()));

    // Adding a folder that is already a root changes nothing.
    let generation = cx.read(|cx| shell.read(cx).workspace.snapshot().generation());
    shell.update(cx, |shell, cx| shell.add_workspace_root(second.clone(), cx));
    assert_eq!(
        cx.read(|cx| shell.read(cx).workspace.snapshot().generation()),
        generation
    );

    let tree = cx.read(|cx| shell.read(cx).workspace_tree.clone().unwrap());
    tree.update(cx, |tree, cx| {
        assert!(tree.activate_for_test(&second, false, cx))
    });
    let up = super::super::commands::MOVE_FOLDER_UP_COMMAND;
    assert_eq!(
        wait_for_command(dispatch_product_command(&shell, window, up, cx), cx),
        CommandOutcome::Completed
    );
    assert_eq!(roots(cx), [added[1].clone(), added[0].clone()]);
    assert_eq!(
        wait_for_command(dispatch_product_command(&shell, window, up, cx), cx),
        CommandOutcome::Unavailable
    );
    let down = super::super::commands::MOVE_FOLDER_DOWN_COMMAND;
    assert_eq!(
        wait_for_command(dispatch_product_command(&shell, window, down, cx), cx),
        CommandOutcome::Completed
    );
    assert_eq!(roots(cx), added);

    let remove = super::super::commands::REMOVE_FOLDER_COMMAND;
    let open_documents = cx.read(|cx| documents.read(cx).documents().count());
    assert_eq!(
        wait_for_command(dispatch_product_command(&shell, window, remove, cx), cx),
        CommandOutcome::Completed
    );
    assert_eq!(roots(cx), [installed[0].clone()]);
    assert_eq!(
        cx.read(|cx| documents.read(cx).documents().count()),
        open_documents
    );

    // Open replaces every root with the chosen folder.
    shell.update(cx, |shell, cx| {
        shell.add_workspace_root(second.clone(), cx);
    });
    assert_eq!(roots(cx).len(), 2);
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&shell, window, super::super::OPEN_COMMAND, cx),
            cx
        ),
        CommandOutcome::Completed
    );
    wait_for_api_condition(cx, |cx| {
        shell.read(cx).workspace.snapshot().roots().len() == 1
    });
    assert_eq!(roots(cx)[0].1, second);
}

#[gpui::test]
fn opening_a_window_applies_its_roots_or_closes_it_again(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (filesystems, _) = memory_filesystems_with_provider();
    cx.set_global(ApplicationFileSystems(filesystems));
    let (_shell, _window) = empty_product_window(&documents, cx);
    let host = cx.update(extensions::host::install);
    let key = install_operation_fixture(
        &host,
        r#"
import { commands, windows } from "knot";
const blank = await windows.open();
if (blank.workspace.roots.length !== 0) throw new Error("blank window has roots");
const opened = await windows.open({ roots: ["mem://product/src", "mem://product/"] });
const roots = opened.workspace.roots.map((root) => root.uri);
if (roots.join() !== "mem://product/src,mem://product/") throw new Error(`roots ${roots}`);
if ((await windows.list()).length !== 3) throw new Error("expected three windows");
let code = null;
try {
  await windows.open({ roots: ["mem://product/missing"] });
} catch (error) {
  code = error.code;
}
if (code !== "ResourceNotFound") throw new Error(`missing root ${code}`);
if ((await windows.list()).length !== 3) throw new Error("rejected window stayed open");
await commands.register("windows.open-ready", () => {});
"#,
        "windows.open-ready",
        cx,
    );
    host.update(cx, |host, cx| host.unload(key, cx).unwrap());
}
