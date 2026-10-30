import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";
import { settings } from "knot";
import * as host from "knot/testing";

declare module "knot" {
  interface Settings {
    "test.width": number;
  }
}

beforeEach(() => host.reset());

test("values resolve by scope facts, then by the latest write", async () => {
  assert.equal(await settings.get("editor.tabSize", { language: "rust", view: "editor" }), 4);
  await settings.set("editor.tabSize", 8, { language: "rust" });
  await settings.set("editor.tabSize", 2);
  await settings.set({ "editor.tabSize": 3 }, { language: "rust", view: "editor" });
  assert.equal(await settings.get("editor.tabSize", { language: "rust", view: "editor" }), 3);
  assert.equal(await settings.get("editor.tabSize", { language: "rust", view: "terminal" }), 8);
  assert.equal(await settings.get("editor.tabSize", { language: "python" }), 2);

  await settings.reset("editor.tabSize", { language: "rust", view: "editor" });
  assert.equal(await settings.get("editor.tabSize", { language: "rust", view: "editor" }), 8);
});

test("a definition adopts earlier values and types later writes", async () => {
  await settings.set("test.width", 6);
  assert.equal(await settings.get("test.width"), undefined);
  await settings.define({ key: "test.width", type: "integer", default: 10, description: "Width" });
  assert.equal(await settings.get("test.width"), 6);
  assert.equal(host.settingDefinitions().get("test.width")?.default, 10);

  await assert.rejects(
    settings.define({ key: "test.width", type: "integer", default: 1, description: "Again" }),
    { code: "DuplicateSetting" },
  );
  // @ts-expect-error the merged declaration types the value
  await assert.rejects(settings.set("test.width", "wide"), { code: "SettingTypeMismatch" });
  assert.equal(await settings.get("test.width"), 6);
});

test("observers see writes and resets of their key in any scope", async () => {
  const seen: string[] = [];
  const watch = await settings.observe("editor.tabSize", (key) => {
    seen.push(key);
  });
  await settings.set("editor.tabSize", 2, { language: "rust" });
  await settings.reset("editor.tabSize", { language: "rust" });
  await settings.reset("editor.tabSize", { language: "rust" });
  await watch.dispose();
  await settings.set("editor.tabSize", 3);
  assert.deepEqual(seen, ["editor.tabSize", "editor.tabSize"]);
});
