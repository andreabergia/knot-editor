import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";
import { settings } from "knot";
import * as host from "knot/testing";
import { start } from "../src/extension.js";
import { RECORD_KEY } from "../src/selection.js";

beforeEach(() => {
  host.reset();
  host.openWindow();
});

async function selection() {
  return {
    mode: await settings.get("theme.mode"),
    dark: await settings.get("theme.dark"),
    light: await settings.get("theme.light"),
  };
}

/** Starts the package again with only its stored records, as after a restart. */
async function restart() {
  const records = host.records();
  host.reset();
  host.openWindow();
  for (const [key, value] of Object.entries(records)) host.setRecord(key, value);
  await start();
}

test("selecting a theme sets its appearance's theme and the mode", async () => {
  await start();
  host.answer("showPicker", { kind: "selected", id: "One Light" });
  await host.run("themes.select");

  assert.deepEqual(await selection(), { mode: "light", dark: "One Dark", light: "One Light" });
  const [prompt] = host.prompts();
  assert.deepEqual(
    (prompt?.options as { items: { label: string; description: string }[] }).items,
    [
      { id: "One Dark", label: "One Dark", description: "dark, selected" },
      { id: "One Light", label: "One Light", description: "light, selected" },
    ],
  );
});

test("the choice survives a restart of the package", async () => {
  await start();
  host.answer("showPicker", { kind: "selected", id: "One Light" });
  await host.run("themes.select");
  await restart();

  assert.deepEqual(host.record(RECORD_KEY), { light: "One Light", mode: "light" });
  assert.deepEqual(await selection(), { mode: "light", dark: "One Dark", light: "One Light" });
});

test("in system mode a choice keeps the mode", async () => {
  await start();
  await settings.set("theme.mode", "system");
  host.answer("showPicker", { kind: "selected", id: "One Light" });
  await host.run("themes.select");

  assert.deepEqual(await selection(), { mode: "system", dark: "One Dark", light: "One Light" });
  assert.deepEqual(host.record(RECORD_KEY), { light: "One Light" });
});

test("dismissing the picker changes nothing", async () => {
  await start();
  host.answer("showPicker", { kind: "dismissed" });
  await host.run("themes.select");

  assert.deepEqual(await selection(), { mode: "dark", dark: "One Dark", light: "One Light" });
  assert.equal(host.record(RECORD_KEY), undefined);
});

test("an unreadable record keeps the defaults", async () => {
  host.setRecord(RECORD_KEY, { mode: "sepia", dark: 4 });
  await start();
  assert.deepEqual(await selection(), { mode: "dark", dark: "One Dark", light: "One Light" });
});
