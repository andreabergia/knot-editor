//! Preview tabs opened from the explorer and the public API.
use super::operations::empty_product_window;
use super::*;
use crate::app::settings::{ApplicationSettings, SettingsStore, WORKBENCH_PREVIEW_TABS};

fn explorer_window(cx: &mut TestAppContext) -> (Entity<ProductShell>, gpui::AnyWindowHandle) {
    let documents = install_globals(cx);
    let (filesystems, provider) = memory_filesystems_with_provider();
    provider
        .seed_file(
            ResourceUri::parse("mem://product/other.txt").unwrap(),
            b"other",
        )
        .unwrap();
    cx.set_global(ApplicationFileSystems(filesystems));
    let (shell, window) = empty_product_window(&documents, cx);
    shell.update(cx, |shell, cx| {
        shell.install_workspace(ResourceUri::parse("mem://product/").unwrap(), cx);
    });
    cx.run_until_parked();
    (shell, window)
}

/// Click a file in the explorer: once for a preview, twice for a tab.
fn click(
    shell: &Entity<ProductShell>,
    window: gpui::AnyWindowHandle,
    name: &str,
    preview: bool,
    cx: &mut TestAppContext,
) {
    let tree = cx.read(|cx| shell.read(cx).workspace_tree.clone().unwrap());
    let uri = ResourceUri::parse(&format!("mem://product/{name}")).unwrap();
    cx.update_window(window, |_, window, cx| {
        tree.focus_handle(cx).focus(window);
        tree.update(cx, |tree, cx| {
            assert!(tree.activate_for_test(&uri, preview, cx))
        });
    })
    .unwrap();
    cx.run_until_parked();
}

/// The normal group's file tabs as names with whether each is a preview.
fn file_tabs(shell: &Entity<ProductShell>, cx: &TestAppContext) -> Vec<(String, bool)> {
    cx.read(|cx| {
        let documents = cx.global::<ApplicationDocuments>().0.read(cx);
        let workbench = shell.read(cx).workbench.read(cx);
        let pane = workbench.normal_pane().unwrap();
        workbench
            .pane(pane)
            .unwrap()
            .tabs()
            .iter()
            .filter_map(|tab| {
                let uri = documents.get(tab.document_id()?)?.resource_uri()?;
                Some((
                    uri.to_string()
                        .trim_start_matches("mem://product/")
                        .to_owned(),
                    tab.is_preview(),
                ))
            })
            .collect()
    })
}

#[gpui::test]
async fn explorer_clicks_replace_one_preview_and_double_clicks_or_edits_keep_tabs(
    cx: &mut TestAppContext,
) {
    let (shell, window) = explorer_window(cx);
    click(&shell, window, "notes.txt", true, cx);
    assert_eq!(file_tabs(&shell, cx), [("notes.txt".into(), true)]);
    // Layouts record no preview state, so a restored preview is a tab.
    let layout = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| shell.capture_layout(window, cx))
        })
        .unwrap();
    let recorded = serde_json::to_string(&layout).unwrap();
    assert!(recorded.contains("notes.txt") && !recorded.contains("preview"));
    click(&shell, window, "other.txt", true, cx);
    assert_eq!(file_tabs(&shell, cx), [("other.txt".into(), true)]);
    // The replaced preview's document closed with its only view.
    cx.read(|cx| {
        let documents = cx.global::<ApplicationDocuments>().0.read(cx);
        let notes = ResourceUri::parse("mem://product/notes.txt").unwrap();
        assert!(documents.document_for_resource(&notes).is_none());
    });

    // The second click of a double click keeps the tab.
    click(&shell, window, "other.txt", false, cx);
    assert_eq!(file_tabs(&shell, cx), [("other.txt".into(), false)]);
    click(&shell, window, "notes.txt", true, cx);
    assert_eq!(
        file_tabs(&shell, cx),
        [("other.txt".into(), false), ("notes.txt".into(), true)]
    );

    // Keep Tab Open makes the focused preview regular.
    cx.run_until_parked();
    let kept = dispatch_product_command(
        &shell,
        window,
        crate::app::commands::dispatch::KEEP_TAB_OPEN_COMMAND,
        cx,
    );
    cx.run_until_parked();
    assert_eq!(kept.completion.await.unwrap(), CommandOutcome::Completed);
    assert_eq!(
        file_tabs(&shell, cx),
        [("other.txt".into(), false), ("notes.txt".into(), false)]
    );
    click(&shell, window, "notes.txt", true, cx);
    click(&shell, window, "other.txt", true, cx);
    click(&shell, window, "notes.txt", true, cx);
    assert_eq!(
        file_tabs(&shell, cx),
        [("other.txt".into(), false), ("notes.txt".into(), false)]
    );

    // Editing a preview keeps it.
    let model = cx.read(|cx| {
        let documents = cx.global::<ApplicationDocuments>().0.read(cx);
        let notes = ResourceUri::parse("mem://product/notes.txt").unwrap();
        let document = documents.document_for_resource(&notes).unwrap();
        documents.get(document).unwrap().model().clone()
    });
    model.update(cx, |model, cx| {
        model.replace(0..0, "edited ").unwrap();
        cx.notify();
    });
    shell.update(cx, |shell, cx| {
        let editor = shell
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab()
            .editor()
            .unwrap()
            .clone();
        editor.update(cx, |_, cx| cx.notify());
    });
    cx.run_until_parked();
    assert_eq!(
        file_tabs(&shell, cx),
        [("other.txt".into(), false), ("notes.txt".into(), false)]
    );
}

#[gpui::test]
fn turning_previews_off_opens_regular_tabs(cx: &mut TestAppContext) {
    let (shell, window) = explorer_window(cx);
    let mut settings = SettingsStore::default();
    settings
        .set(WORKBENCH_PREVIEW_TABS, false.into(), Default::default())
        .unwrap();
    cx.set_global(ApplicationSettings(settings));
    click(&shell, window, "notes.txt", true, cx);
    click(&shell, window, "other.txt", true, cx);
    assert_eq!(
        file_tabs(&shell, cx),
        [("notes.txt".into(), false), ("other.txt".into(), false)]
    );
}

#[gpui::test]
fn public_open_presents_previews_on_request(cx: &mut TestAppContext) {
    let (shell, window) = explorer_window(cx);
    let host = cx.update(crate::app::extensions::host::install);
    let key = super::operations::install_operation_fixture(
        &host,
        r#"
import { commands, documents } from "knot";
await commands.register("previews.open", async () => {
  const first = await documents.open("mem://product/notes.txt", { preview: true });
  const second = await documents.open("mem://product/other.txt", { preview: true });
  if (first.presentation.target.tab !== second.presentation.target.tab) throw new Error("preview not replaced");
  try { await documents.open("mem://product/notes.txt", { preview: "yes" }); throw new Error("accepted"); }
  catch (error) { if (!(error instanceof TypeError)) throw error; }
  await documents.open("mem://product/notes.txt");
});
"#,
        "previews.open",
        cx,
    );
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&shell, window, "previews.open", cx),
            cx
        ),
        CommandOutcome::Completed
    );
    assert_eq!(
        file_tabs(&shell, cx),
        [("other.txt".into(), true), ("notes.txt".into(), false)]
    );
    host.update(cx, |host, cx| host.unload(key, cx).unwrap());
}
