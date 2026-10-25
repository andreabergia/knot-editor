//! Window-owned workspace operations through the public host.

use super::operations::{empty_product_window, install_operation_fixture};
use super::*;
use crate::app::extension_host;

#[gpui::test]
fn public_workspace_operations_validate_before_commit_and_preserve_documents(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    let (filesystems, _) = memory_filesystems_with_provider();
    cx.set_global(ApplicationFileSystems(filesystems));
    let host = cx.update(extension_host::install);
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
        let host = cx.update(extension_host::install);
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
    let host = cx.update(extension_host::install);
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
