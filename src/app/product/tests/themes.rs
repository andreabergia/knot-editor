//! Theme registration, color definitions, and selection through the public
//! extension API.
use super::*;
use crate::app::theme::{Appearance, color, theme};
use crate::host::lifecycle::ExtensionKey;

const SOURCE: &str = r##"
import { commands, settings, themes } from "knot";
async function failure(operation) {
  try { await operation(); } catch (error) { return error; }
  throw new Error("expected a failure");
}
for (const invalid of [
  () => themes.register({ name: "Bad", appearance: "dim" }),
  () => themes.register({ name: "Bad", appearance: "dark", colors: { background: "red" } }),
  () => themes.register({ name: "Bad", appearance: "dark", syntax: { type: { fontWeight: 950 } } }),
  () => themes.register({ name: "Bad", appearance: "dark", syntax: { type: { shadow: true } } }),
  () => themes.defineColor({ key: "diff.add", description: "", defaults: { dark: "#12", light: "#000000" } }),
]) {
  const error = await failure(invalid);
  if (!(error instanceof TypeError)) throw error;
}
const unknown = await failure(() => themes.register({
  name: "Bad", appearance: "dark", syntax: { keywords: { color: "#ffffff" } },
}));
if (unknown.code !== "UnknownCapture" || unknown.details.capture !== "keywords") throw unknown;
await themes.defineColor({
  key: "diff.add", description: "Added lines", defaults: { dark: "success", light: "#00ff00" },
});
const duplicate = await failure(() => themes.defineColor({
  key: "background", description: "Again", defaults: { dark: "#000000", light: "#000000" },
}));
if (duplicate.code !== "DuplicateColor" || duplicate.details.key !== "background") throw duplicate;

await themes.register({
  name: "One Dark", appearance: "dark",
  colors: { background: "#101010", success: "#20a020", "other.package.key": "#123456" },
  syntax: { keyword: { color: "#ff0000", fontWeight: 700 } },
});
await themes.register({ name: "Paper", appearance: "light", colors: { background: "#fefefe" } });
const base = await themes.get("One Dark");
const derived = await themes.register({
  ...base, name: "My Dark", colors: { ...base.colors, background: "#000001" },
});
const listed = await themes.list();
if (listed.map((entry) => `${entry.name}:${entry.appearance}`).join() !== "My Dark:dark,One Dark:dark,Paper:light"
    || await themes.get("Missing") !== null || base.syntax.keyword.fontWeight !== 700) {
  throw new Error(JSON.stringify({ listed, base }));
}
await commands.register("theme.select", async ({ arguments: [key, value] }) => {
  await settings.set(key, value);
});
await commands.register("theme.dropDerived", async () => {
  await derived.dispose();
  if ((await themes.list()).some((entry) => entry.name === "My Dark")) throw new Error("kept");
});
"##;

fn background(cx: &TestAppContext) -> gpui::Hsla {
    cx.read(|cx| theme(cx).color(color::BACKGROUND))
}

#[gpui::test]
fn registered_themes_apply_at_startup_and_follow_the_selection_settings(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let host = cx.update(super::super::super::extensions::host::install);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    let key = ExtensionKey::new(ExtensionId::new(960), ExtensionLifecycleId::new(1));
    let loaded = Arc::new(Mutex::new(None));
    let result = loaded.clone();
    host.update(cx, |host, cx| {
        let execution = host.load_test_source(key, SOURCE).unwrap();
        cx.spawn(async move |_, _| {
            *result.lock().unwrap() = Some(execution.await.map(|_| ()));
        })
        .detach();
    });
    wait_for_api_condition(cx, |_| loaded.lock().unwrap().is_some());
    loaded.lock().unwrap().take().unwrap().unwrap();

    // Registering changes nothing until startup resolves the selection.
    assert_eq!(background(cx), gpui::rgb(0x1e1e1e).into());
    cx.update(crate::app::themes::apply_at_startup);
    cx.run_until_parked();
    assert_eq!(background(cx), gpui::rgb(0x101010).into());
    cx.read(|cx| {
        // An extension key defaults to a native key the theme sets.
        assert_eq!(theme(cx).color("diff.add"), gpui::rgb(0x20a020).into());
        // A key nobody defined stays inert.
        assert_eq!(
            theme(cx).color("other.package.key"),
            theme(cx).color(color::TEXT)
        );
    });

    let select = |key: &str, value: &str, cx: &mut TestAppContext| {
        let execution = dispatch_product_command_with(
            &shell,
            window,
            "theme.select",
            CommandArgumentValue::Array(vec![
                CommandArgumentValue::String(key.into()),
                CommandArgumentValue::String(value.into()),
            ]),
            cx,
        );
        assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
        cx.run_until_parked();
    };
    select("theme.dark", "My Dark", cx);
    assert_eq!(background(cx), gpui::rgb(0x000001).into());
    select("theme.light", "Paper", cx);
    assert_eq!(background(cx), gpui::rgb(0x000001).into());
    select("theme.mode", "light", cx);
    assert_eq!(background(cx), gpui::rgb(0xfefefe).into());
    cx.read(|cx| {
        assert_eq!(theme(cx).appearance, Appearance::Light);
        // diff.add takes its own light default.
        assert_eq!(theme(cx).color("diff.add"), gpui::rgb(0x00ff00).into());
    });

    // Disposing the active theme keeps its colors until the selection
    // changes, and an unregistered selection uses the defaults.
    select("theme.mode", "dark", cx);
    let execution = dispatch_product_command(&shell, window, "theme.dropDerived", cx);
    assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
    cx.run_until_parked();
    assert_eq!(background(cx), gpui::rgb(0x000001).into());
    select("theme.dark", "My Dark ", cx);
    assert_eq!(background(cx), gpui::rgb(0x1e1e1e).into());

    // Teardown removes the extension's themes and color keys.
    select("theme.dark", "One Dark", cx);
    assert_eq!(background(cx), gpui::rgb(0x101010).into());
    host.update(cx, |host, cx| host.unload(key, cx)).unwrap();
    cx.run_until_parked();
    cx.update(crate::app::themes::selection_changed);
    assert_eq!(background(cx), gpui::rgb(0x101010).into());
    cx.update(crate::app::themes::apply_at_startup);
    cx.read(|cx| {
        assert_eq!(
            theme(cx).color(color::BACKGROUND),
            gpui::rgb(0x1e1e1e).into()
        );
        assert_eq!(theme(cx).color("diff.add"), theme(cx).color(color::TEXT));
    });
}
