import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";
import type { CaptureId, DocumentId, WindowTarget } from "knot";
import * as host from "knot/testing";
import { problem } from "../src/switch.js";
import { ALPHA, BETA, seedProject, seedSession, session, startLaunched } from "./support.js";

beforeEach(() => host.reset());

/** Alpha is active in one window with a clean file and an unsaved draft; Beta is suspended. */
async function alphaWithDraft() {
  seedProject("a", "Alpha", [ALPHA]);
  seedProject("b", "Beta", [BETA]);
  seedSession("a", "active");
  seedSession("b", "suspended", [{ entries: [{ uri: `${BETA}notes.md` }], selected: 0 }]);
  const file = host.addDocument({ uri: `${ALPHA}main.rs` });
  const draft = host.addDocument({ text: "unsaved" });
  const window = host.openWindow({ roots: [ALPHA], documents: [file, draft] });
  await startLaunched();
  return { window, file, draft };
}

async function selectBeta(window: WindowTarget) {
  host.answer("showPicker", { kind: "selected", id: "b" });
  await host.run("projects.select", { window });
}

test("switching suspends the source with its unsaved text and opens the destination", async () => {
  const { window, draft } = await alphaWithDraft();
  await selectBeta(window);

  const alpha = session("a");
  assert.equal(alpha?.state, "suspended");
  const [entries] = alpha!.windows.map((window) => window.entries);
  assert.equal(entries?.[0]?.uri, `${ALPHA}main.rs`);
  assert.equal(entries?.[1]?.document, draft);
  const capture = entries?.[1]?.capture as CaptureId;
  assert.equal(host.captureText(capture), "unsaved");
  assert.deepEqual(host.captureIds(), [capture]);

  const [replacement] = host.replaced();
  assert.deepEqual(replacement?.release, [window]);
  assert.deepEqual(replacement?.suspend.map((entry) => entry.capture !== null), [false, true]);
  const [opened] = host.windowTargets();
  assert.deepEqual(host.rootsOf(opened!), [BETA]);
  assert.equal(host.focusedWindow(), opened);
  assert.equal(session("b")?.state, "active");
  assert.equal(session("b")?.windows[0]?.entries[0]?.uri, `${BETA}notes.md`);
});

test("switching back restores the retained text", async () => {
  const { window } = await alphaWithDraft();
  await selectBeta(window);
  host.answer("showPicker", { kind: "selected", id: "a" });
  await host.run("projects.select", { window: host.windowTargets()[0]! });
  const [alpha] = host.windowTargets();
  const texts = host.documentsIn(alpha!).map((id) => host.document(id).text);
  assert.deepEqual(texts, ["", "unsaved"]);
  assert.equal(session("a")?.state, "active");
  // The restored text is protected again, so the suspended capture is released.
  assert.deepEqual(host.captureIds(), []);
});

test("a conflicting commit retries with fresh captures", async () => {
  const { window } = await alphaWithDraft();
  host.failNext("workbench.replaceWindows", "SessionConflict");
  await selectBeta(window);
  assert.equal(host.replaced().length, 1);
  assert.equal(host.captureIds().length, 1);
  assert.deepEqual(host.messages(), []);
});

test("a failed commit keeps the source active and releases this attempt's captures", async () => {
  const { window } = await alphaWithDraft();
  host.failNext("workbench.replaceWindows", "ProviderFailure");
  await selectBeta(window);
  assert.deepEqual(host.windowTargets(), [window]);
  assert.equal(session("a")?.state, "active");
  assert.equal(session("b")?.state, "suspended");
  assert.deepEqual(host.captureIds(), []);
  assert.deepEqual(host.messages(), ["cannot switch to Beta: ProviderFailure: workbench.replaceWindows failed"]);
});

test("switching gives up after three conflicts", async () => {
  const { window } = await alphaWithDraft();
  for (let attempt = 0; attempt < 3; attempt += 1) {
    host.failNext("workbench.replaceWindows", "SessionConflict");
  }
  await selectBeta(window);
  assert.deepEqual(host.windowTargets(), [window]);
  assert.equal(session("a")?.state, "active");
  assert.match(host.messages()[0]!, /cannot switch to Beta: SessionConflict/);
});

test("a project already open in a window is focused instead of switched", async () => {
  seedProject("a", "Alpha", [ALPHA]);
  seedSession("a", "active");
  const window = host.openWindow({ roots: [ALPHA] });
  const other = host.openWindow();
  await startLaunched();
  host.answer("showPicker", { kind: "selected", id: "a" });
  await host.run("projects.select", { window: other });
  assert.equal(host.replaced().length, 0);
  assert.equal(host.focusedWindow(), window);
});

test("an unreadable or rootless project is reported and never switched to", async () => {
  seedProject("a", "Alpha", [ALPHA]);
  seedProject("e", "Empty", []);
  host.setRecord("session:a", { schema: 9 });
  host.openWindow();
  await startLaunched();
  for (const id of ["a", "e"]) {
    host.answer("showPicker", { kind: "selected", id });
    await host.run("projects.select");
  }
  assert.equal(host.replaced().length, 0);
  assert.deepEqual(host.messages(), [
    "Alpha has a session record this version cannot read; it is kept unchanged",
    "Empty has no root; rename or recreate the project",
  ]);
  assert.deepEqual(host.record("session:a"), { schema: 9 });
});

test("restore problems name the entry", () => {
  assert.equal(problem({ kind: "missing" }, { uri: "file:///a/notes.md" }), "notes.md no longer exists");
  assert.equal(
    problem({ kind: "failed", code: "InvalidUtf8", message: "" }, {}),
    "an untitled document could not be restored (InvalidUtf8)",
  );
  assert.equal(problem({ kind: "opened", document: "d" as DocumentId }, {}), null);
});
