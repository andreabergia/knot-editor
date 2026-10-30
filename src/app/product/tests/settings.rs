//! Settings shared by personal config and packages through the public API.
use super::*;
use crate::app::editor::EditorView;

const PRE_INIT: &str = r#"
import { languages, settings } from "knot";
await languages.register({
  id: "rust", name: "Rust", grammar: "rust", fileExtensions: ["rs"],
  queries: { highlights: "(identifier) @variable" },
});
await settings.set("demo.width", 6);
await settings.set("demo.flag", "yes");
await settings.set("editor.tabSize", 2);
"#;

const POST_INIT: &str = r#"
import { settings } from "knot";
await settings.set("demo.width", 12);
await settings.set("editor.tabSize", 8, { language: "rust" });
"#;

const PACKAGE: &str = r#"
import { commands, settings, workbench } from "knot";
async function failure(operation) {
  try { await operation(); } catch (error) { return error; }
  throw new Error("expected a failure");
}
await settings.define({ key: "demo.width", type: "integer", default: 10, description: "Width" });
await settings.define({ key: "demo.flag", type: "boolean", default: false, description: "Flag" });
const width = await settings.get("demo.width");
if (await settings.get("demo.flag") !== false) throw new Error("kept a mismatching value");
for (const key of ["demo.width", "editor.tabSize"]) {
  const duplicate = await failure(() => settings.define({
    key, type: "integer", default: 1, description: "Again",
  }));
  if (duplicate.code !== "DuplicateSetting") throw duplicate;
}
const mismatch = await failure(() => settings.set({ "demo.width": 7, "demo.flag": "yes" }));
if (mismatch.code !== "SettingTypeMismatch" || mismatch.details.key !== "demo.flag"
    || mismatch.details.expected !== "boolean") {
  throw mismatch;
}
if (await settings.get("demo.width") !== width) throw new Error("wrote part of a rejected call");
for (const invalid of [
  () => settings.set("demo.width", 1, { language: 5 }),
  () => settings.set("demo.width", undefined),
  () => settings.get(""),
  () => settings.define({ key: "demo.other", type: "integer", default: 1.5, description: "" }),
  () => settings.define({ key: "demo.other", type: "list", default: [], description: "" }),
]) {
  const error = await failure(invalid);
  if (!(error instanceof TypeError)) throw error;
}
if (await settings.get("demo.undefined") !== undefined) throw new Error("undefined key resolved");
await settings.set("editor.tabSize", 3);
await settings.observe("demo.width", async (key) => {
  await workbench.writeClipboardText(`${key}=${await settings.get(key)}`);
});
await commands.register("demo.started", async () => {
  await workbench.writeClipboardText(`started=${width}`);
});
"#;

fn clipboard(cx: &gpui::App) -> Option<String> {
    cx.read_from_clipboard().and_then(|item| item.text())
}

#[gpui::test]
fn config_and_packages_share_settings_across_phases_and_restart(cx: &mut TestAppContext) {
    let config = tempfile::tempdir().unwrap();
    let extensions = tempfile::tempdir().unwrap();
    std::fs::write(config.path().join("pre-init.js"), PRE_INIT).unwrap();
    std::fs::write(config.path().join("post-init.js"), POST_INIT).unwrap();
    install_extension(
        extensions.path(),
        "@example/settings",
        &[],
        &[("main.js", PACKAGE)],
    );
    install_globals(cx);
    install_launch_gate(cx);
    let host = cx.update(crate::app::extensions::host::install);
    host.update(cx, |host, cx| {
        host.start_product_startup(
            Some(config.path().to_path_buf()),
            Ok(extensions.path().to_path_buf()),
            cx,
        )
    });
    assert!(wait_for_product_startup(cx));
    let report = cx
        .read(crate::app::extensions::host::startup_report)
        .unwrap();
    assert!(
        report
            .report
            .entries
            .iter()
            .all(|entry| entry.result == crate::app::extensions::load::LoadResult::Loaded),
        "{:?}",
        report.report.entries
    );

    // The package saw the pre-init value, and its observer saw post-init's.
    wait_for_api_condition(cx, |cx| clipboard(cx).as_deref() == Some("demo.width=12"));
    let documents = cx.read(|cx| cx.global::<ApplicationDocuments>().0.clone());
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    let execution = dispatch_product_command(&shell, window, "demo.started", cx);
    assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
    assert_eq!(cx.read(clipboard).as_deref(), Some("started=6"));
    let warnings = cx.read(crate::app::extensions::host::setting_warnings);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("demo.flag"), "{warnings:?}");

    // Editors resolve the package's later global write over pre-init's, and
    // post-init's Rust value for a Rust buffer.
    let tab_sizes = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let rust = cx.new(|_| {
                let mut model = BufferModel::from_text("\tx");
                model.set_language(Some("rust".into()), true);
                model
            });
            let plain = cx.new(|_| BufferModel::from_text("\tx"));
            [rust, plain].map(|model| {
                let editor = cx.new(|cx| EditorView::new(model.clone(), cx));
                editor.read(cx).tab_size(model.read(cx), cx)
            })
        })
    };
    assert_eq!(tab_sizes(cx), [8, 3]);

    // Open product windows paint tab stops at those widths: the caret after
    // the leading tab sits that many character widths in.
    let painted_tab = |model: Entity<BufferModel>, cx: &mut TestAppContext| {
        model.update(cx, |model, _| model.replace(0..0, "\tab").unwrap());
        let document = cx.update(|cx| {
            documents.update(cx, |documents, cx| {
                documents.create_untitled("Untitled", model.clone(), cx)
            })
        });
        let (shell, window) = product_window(document, model, cx);
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
                let tab = editor.caret_x_at(1, window, cx);
                let letter = editor.caret_x_at(2, window, cx) - tab;
                (f32::from(tab) / f32::from(letter)).round() as usize
            })
        })
        .unwrap()
    };
    let rust = cx.new(|_| {
        let mut model = BufferModel::from_text("");
        model.set_language(Some("rust".into()), true);
        model
    });
    assert_eq!(painted_tab(rust, cx), 8);
    assert_eq!(painted_tab(cx.new(|_| BufferModel::from_text("")), cx), 3);
    assert_eq!(
        cx.read(crate::app::extensions::host::setting_observer_count),
        1
    );

    // Restart keeps every value: the fresh lifecycle defines its keys again
    // and starts from post-init's value.
    let restarted = Arc::new(Mutex::new(None));
    let result = restarted.clone();
    host.update(cx, |host, cx| {
        let task = host.restart_package("@example/settings".into(), cx);
        cx.spawn(async move |_, _| {
            *result.lock().unwrap() = Some(task.await);
        })
        .detach();
    });
    wait_for_api_condition(cx, |_| restarted.lock().unwrap().is_some());
    let entry = restarted.lock().unwrap().take().unwrap();
    assert_eq!(
        entry.result,
        crate::app::extensions::load::LoadResult::Loaded
    );
    let execution = dispatch_product_command(&shell, window, "demo.started", cx);
    assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
    assert_eq!(cx.read(clipboard).as_deref(), Some("started=12"));
    assert_eq!(tab_sizes(cx), [8, 3]);
    assert_eq!(
        cx.read(crate::app::extensions::host::setting_warnings)
            .len(),
        1
    );
    // Teardown removed the old lifecycle's observer; the new one replaced it.
    assert_eq!(
        cx.read(crate::app::extensions::host::setting_observer_count),
        1
    );
}

const ASSOCIATIONS: &str = r#"
import { commands, languages } from "knot";
let association = null;
await commands.register("assoc.remap", async () => {
  try {
    await languages.associate("python", { fileExtensions: [".rs"] });
    throw new Error("accepted a leading dot");
  } catch (error) {
    if (!(error instanceof TypeError)) throw error;
  }
  association = await languages.associate("python", { fileExtensions: ["rs"] });
});
await commands.register("assoc.dispose", () => association.dispose());
"#;

#[gpui::test]
fn personal_config_associates_an_extension_and_open_documents_redetect(cx: &mut TestAppContext) {
    let config = tempfile::tempdir().unwrap();
    let extensions = tempfile::tempdir().unwrap();
    let files = tempfile::tempdir().unwrap();
    let source = files.path().join("main.rs");
    std::fs::write(&source, "fn main() {}\n").unwrap();
    std::fs::write(config.path().join("post-init.js"), ASSOCIATIONS).unwrap();
    install_globals(cx);
    install_launch_gate(cx);
    cx.set_global(ApplicationOpenDialog(Arc::new(FixedOpenDialog(
        OpenDialogOutcome::Selected(source),
    ))));
    let host = cx.update(crate::app::extensions::host::install);
    host.update(cx, |host, cx| {
        host.start_product_startup(
            Some(config.path().to_path_buf()),
            Ok(extensions.path().to_path_buf()),
            cx,
        )
    });
    assert!(wait_for_product_startup(cx));

    let documents = cx.read(|cx| cx.global::<ApplicationDocuments>().0.clone());
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    let execution = dispatch_product_command(&shell, window, super::super::OPEN_COMMAND, cx);
    assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
    let opened = cx.read(|cx| {
        let pane = shell.read(cx).workbench.read(cx).focused_pane().unwrap();
        pane.active_tab().editor().unwrap().read(cx).model().clone()
    });
    let language = |cx: &mut TestAppContext| {
        cx.read(|cx| crate::app::languages::effective_language(opened.read(cx), cx))
    };
    assert_eq!(language(cx), "rust");

    // The bundled package's registration of "rs" comes back on disposal.
    for (command, expected) in [("assoc.remap", "python"), ("assoc.dispose", "rust")] {
        let execution = dispatch_product_command(&shell, window, command, cx);
        assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
        assert_eq!(language(cx), expected);
    }
}
