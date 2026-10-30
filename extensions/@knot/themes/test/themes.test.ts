import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";
import { themes } from "knot";
import * as host from "knot/testing";
import { start } from "../src/extension.js";
import { THEMES } from "../src/themes.js";

beforeEach(() => host.reset());

test("registers every theme before start resolves", async () => {
  await start();
  assert.deepEqual(
    host.themeDefinitions().map((definition) => definition.name),
    THEMES.map((theme) => theme.name),
  );
  assert.deepEqual(
    (await themes.list()).map((theme) => `${theme.name}:${theme.appearance}`),
    ["One Dark:dark", "One Light:light"],
  );
});

test("every theme sets exactly the native color keys", () => {
  for (const theme of THEMES) {
    assert.deepEqual(Object.keys(theme.colors ?? {}).sort(), [...host.NATIVE_COLOR_KEYS], theme.name);
  }
});

test("every theme styles only known captures and valid colors", () => {
  const color = /^#(?:[0-9a-f]{6}|[0-9a-f]{8})$/;
  for (const theme of THEMES) {
    for (const [name, style] of Object.entries(theme.syntax ?? {})) {
      assert.ok(host.CAPTURE_NAMES.includes(name), `${theme.name}: ${name}`);
      if (style.color !== undefined) assert.match(style.color, color, `${theme.name}: ${name}`);
    }
    for (const [key, value] of Object.entries(theme.colors ?? {})) {
      assert.match(value ?? "", color, `${theme.name}: ${key}`);
    }
  }
});
