import assert from "node:assert/strict";
import { test } from "node:test";
import type { WindowTarget } from "knot";
import { createState } from "../src/state.js";
import { projectItems } from "../src/tree.js";

const window = {} as WindowTarget;
const other = {} as WindowTarget;

function state() {
  const state = createState();
  state.registry.projects = [
    { id: "a", name: "Alpha", roots: ["file:///alpha/"], createdAt: null, lastOpenedAt: null },
    { id: "b", name: "Beta", roots: ["file:///beta/"], createdAt: null, lastOpenedAt: null },
  ];
  return state;
}

test("rows mark the window's own project and unreadable sessions", () => {
  const current = state();
  current.bindings.set(window, "a");
  current.unreadable.add("b");
  const [alpha, beta] = projectItems(current, window);
  assert.equal(alpha?.statusText, "active");
  assert.equal(alpha?.badge, "success");
  assert.equal(alpha?.description, "alpha");
  assert.deepEqual(alpha?.actions?.map((action) => action.arguments), [{ id: "a" }, { id: "a" }]);
  assert.equal(beta?.statusText, "unreadable");
  assert.equal(beta?.description, "session record unreadable");
  assert.equal(projectItems(current, other)[0]?.statusText, null);
});

test("a storage failure replaces the rows with one error row", () => {
  const current = state();
  current.loadFailure = "StorageUnavailable";
  assert.deepEqual(
    projectItems(current, window).map((item) => [item.id, item.description]),
    [["storage-error", "StorageUnavailable"]],
  );
});
