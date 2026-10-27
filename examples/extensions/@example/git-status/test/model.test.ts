import assert from "node:assert/strict";
import { test } from "node:test";
import {
  addRepository,
  children,
  discard,
  initialModel,
  move,
  moveAll,
  removeRepository,
  stringArgument,
  summary,
} from "../src/model.js";

test("staging and unstaging move single entries between groups", () => {
  const model = initialModel();
  assert.equal(move(model, "notes.txt", "changed"), true);
  assert.deepEqual(model.staged.at(-1), { path: "notes.txt", status: "untracked" });
  assert.equal(move(model, "notes.txt", "changed"), false);
  assert.equal(move(model, "src/main.rs", "staged"), true);
  assert.equal(model.changed.at(-1)?.path, "src/main.rs");
});

test("discard removes only changed entries", () => {
  const model = initialModel();
  assert.equal(discard(model, "src/main.rs"), false);
  assert.equal(discard(model, "notes.txt"), true);
  assert.deepEqual(summary(model), ["M  src/main.rs", "A  src/app/tree_view.rs", "M  docs/architecture/semantic-ui.md"]);
});

test("moving everything empties the source group", () => {
  const model = initialModel();
  assert.equal(moveAll(model, "changed"), true);
  assert.equal(model.changed.length, 0);
  assert.equal(model.staged.length, 4);
  assert.equal(moveAll(model, "changed"), false);
});

test("repositories are added by path and removal moves the selection", () => {
  const model = initialModel();
  assert.equal(addRepository(model, "   "), null);
  assert.equal(addRepository(model, " /src/tools/ "), "repo-3");
  assert.deepEqual(model.repositories.at(-1), { id: "repo-3", name: "tools", path: "/src/tools/" });
  assert.equal(model.activeRepository, "repo-3");
  removeRepository(model, "repo-3");
  assert.equal(model.activeRepository, "knot");
  removeRepository(model, "site");
  removeRepository(model, "knot");
  assert.equal(model.activeRepository, null);
});

test("the tree groups files with status badges and per-row actions", () => {
  const model = initialModel();
  assert.deepEqual(
    children(model, null).map((item) => [item.id, item.description]),
    [["staged", "2 file(s)"], ["changes", "2 file(s)"], ["repositories", "2 tracked"]],
  );
  const [docs, notes] = children(model, "changes");
  assert.equal(docs?.badge, "warning");
  assert.equal(notes?.statusText, "U");
  assert.deepEqual(notes?.actions?.map((action) => action.label), ["Stage", "Discard"]);
  const [knot, site] = children(model, "repositories");
  assert.equal(knot?.description, "/src/knot-editor (current)");
  assert.equal(site?.statusText, null);
  assert.deepEqual(children(model, "unknown"), []);
});

test("command arguments yield named string fields only", () => {
  assert.equal(stringArgument({ path: "a" }, "path"), "a");
  assert.equal(stringArgument({ path: 1 }, "path"), null);
  assert.equal(stringArgument(["a"], "path"), null);
  assert.equal(stringArgument(null, "path"), null);
});
