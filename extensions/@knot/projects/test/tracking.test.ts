import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";
import { documents, recovery } from "knot";
import type { DocumentId } from "knot";
import * as host from "knot/testing";
import { start } from "../src/extension.js";
import { ALPHA, BETA, registry, seedProject, seedSession, session, startLaunched } from "./support.js";

beforeEach(() => host.reset());

async function info(id: DocumentId) {
  const document = await documents.get(id);
  assert.ok(document !== null);
  return document;
}

test("launch rebinds restored windows and suspends sessions without one", async () => {
  seedProject("a", "Alpha", [ALPHA]);
  seedProject("b", "Beta", [BETA]);
  seedSession("a", "active");
  const draft = host.addDocument({ text: "kept" });
  const capture = (await recovery.retain(draft)).id;
  seedSession("b", "active", [{ entries: [{ document: draft, capture }] }]);
  host.openWindow({ roots: [ALPHA] });
  await start();

  // Nothing is tracked or suspended before startup windows are committed.
  await host.emit({ type: "documentOpened", document: await info(draft) });
  assert.equal(session("b")?.state, "active");

  await host.launch();
  assert.equal(session("a")?.state, "active");
  assert.equal(session("b")?.state, "suspended");
  assert.equal(session("b")?.windows[0]?.entries[0]?.capture, capture);
  assert.deepEqual(host.captureIds(), [capture]);
});

test("a package started after launch reconciles at once", async () => {
  seedProject("a", "Alpha", [ALPHA]);
  seedSession("a", "active");
  await host.launch();
  await start();
  assert.equal(session("a")?.state, "suspended");
});

test("orphan captures are released unless an unreadable session may reference them", async () => {
  const orphan = (await recovery.retain(host.addDocument({ text: "x" }))).id;
  seedProject("a", "Alpha", [ALPHA]);
  host.setRecord("session:a", { schema: 9, captures: [orphan] });
  await startLaunched();
  assert.deepEqual(host.captureIds(), [orphan]);
  assert.deepEqual(host.record("session:a"), { schema: 9, captures: [orphan] });

  host.reset();
  const released = (await recovery.retain(host.addDocument({ text: "x" }))).id;
  await startLaunched();
  assert.equal(host.captureIds().includes(released), false);
});

test("closing a project's last window suspends it without captures", async () => {
  seedProject("a", "Alpha", [ALPHA]);
  seedSession("a", "active");
  const draft = host.addDocument({ dirty: true, uri: `${ALPHA}main.rs` });
  const window = host.openWindow({ roots: [ALPHA], documents: [draft] });
  await startLaunched();
  await host.emit({ type: "documentChanged", document: await info(draft) });
  assert.deepEqual(session("a")?.windows[0]?.entries, [{ uri: `${ALPHA}main.rs`, document: draft }]);

  host.closeWindow(window);
  await host.emit({ type: "windowClosed", window });
  assert.equal(session("a")?.state, "suspended");
  assert.deepEqual(session("a")?.windows[0]?.entries, [{ uri: `${ALPHA}main.rs`, document: draft }]);
  assert.deepEqual(host.captureIds(), []);
});

test("root edits in a bound window move the project and its other windows", async () => {
  seedProject("a", "Alpha", [ALPHA]);
  seedProject("b", "Beta", [BETA]);
  seedSession("a", "active");
  const edited = host.openWindow({ roots: [ALPHA] });
  const follower = host.openWindow({ roots: [ALPHA] });
  await startLaunched();

  const extra = "file:///src/extra/";
  const workspace = host.setRoots(edited, [ALPHA, extra]);
  await host.emit({ type: "workspaceChanged", workspace });
  assert.deepEqual(registry().projects[0]?.roots, [ALPHA, extra]);
  assert.deepEqual(host.rootsOf(follower), [ALPHA, extra]);
  assert.equal(session("a")?.windows.length, 2);

  // Choosing another project's roots unbinds the window instead of merging.
  const moved = host.setRoots(follower, [BETA]);
  await host.emit({ type: "workspaceChanged", workspace: moved });
  assert.deepEqual(registry().projects[0]?.roots, [ALPHA, extra]);
  assert.deepEqual(registry().projects[1]?.roots, [BETA]);
  assert.equal(session("a")?.windows.length, 1);

  // Clearing the last bound window's roots suspends the project, keeping its roots.
  const cleared = host.setRoots(edited, []);
  await host.emit({ type: "workspaceChanged", workspace: cleared });
  assert.equal(session("a")?.state, "suspended");
  assert.deepEqual(registry().projects[0]?.roots, [ALPHA, extra]);
  assert.deepEqual(host.rootsOf(follower), [BETA]);
});
