import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";
import { recovery } from "knot";
import * as host from "knot/testing";
import { VIEW_ID } from "../src/tree.js";
import { ALPHA, BETA, registry, seedProject, seedSession, session, startLaunched } from "./support.js";

beforeEach(() => host.reset());

test("creating a project records it and switches a blank window to it", async () => {
  const blank = host.openWindow({ documents: [host.addDocument()] });
  await startLaunched();
  host.answer("pickFolder", { kind: "selected", uri: ALPHA });
  host.answer("showInput", { kind: "accepted", value: "  Alpha " });
  await host.run("projects.create", { window: blank });

  const [project] = registry().projects;
  assert.equal(project?.name, "Alpha");
  assert.deepEqual(project?.roots, [ALPHA]);
  assert.equal(registry().recentProjectId, project?.id);
  assert.deepEqual(host.prompts().map((prompt) => prompt.kind), ["pickFolder", "showInput"]);
  assert.deepEqual((host.prompts()[1]?.options as { value: string }).value, "alpha");
  const [window] = host.windowTargets();
  assert.notEqual(window, blank);
  assert.deepEqual(host.rootsOf(window!), [ALPHA]);
  assert.equal(session(project!.id)?.state, "active");
  const [row] = await host.treeItems(VIEW_ID, window!);
  assert.equal(row?.statusText, "active");
});

test("creation rejects a used folder, an empty name, and a taken name", async () => {
  seedProject("a", "Alpha", [ALPHA]);
  host.openWindow();
  await startLaunched();
  host.answer("pickFolder", { kind: "selected", uri: ALPHA });
  await host.run("projects.create");
  host.answer("pickFolder", { kind: "selected", uri: BETA });
  host.answer("showInput", { kind: "accepted", value: "  " });
  await host.run("projects.create");
  host.answer("pickFolder", { kind: "selected", uri: BETA });
  host.answer("showInput", { kind: "accepted", value: "ALPHA" });
  await host.run("projects.create");
  assert.deepEqual(host.messages(), [
    "Alpha already uses this folder",
    "a project name cannot be empty",
    "a project named ALPHA already exists",
  ]);
  assert.equal(registry().projects.length, 1);
  assert.equal(host.replaced().length, 0);
});

test("renaming keeps names unique and reports a missing project", async () => {
  seedProject("a", "Alpha", [ALPHA]);
  seedProject("b", "Beta", [BETA]);
  host.openWindow();
  await startLaunched();
  host.answer("showInput", { kind: "accepted", value: "beta" });
  await host.run("projects.rename", { arguments: { id: "a" } });
  host.answer("showInput", { kind: "accepted", value: "Gamma" });
  await host.run("projects.rename", { arguments: { id: "a" } });
  await host.run("projects.rename", { arguments: { id: "missing" } });
  assert.deepEqual(registry().projects.map((project) => project.name), ["Gamma", "Beta"]);
  assert.deepEqual(host.messages(), ["a project named beta already exists", "project not found"]);
});

test("removal warns about suspended text and releases it only after both deletes", async () => {
  seedProject("a", "Alpha", [ALPHA]);
  const draft = host.addDocument({ text: "unsaved" });
  host.openWindow({ documents: [draft] });
  const capture = (await recovery.retain(draft)).id;
  seedSession("a", "suspended", [{ entries: [{ document: draft, capture }] }]);
  await startLaunched();

  host.answer("showConfirmation", { kind: "confirmed" });
  host.failNext("storage.delete", "StorageUnavailable");
  await host.run("projects.remove", { arguments: { id: "a" } });
  const [confirmation] = host.prompts();
  assert.match(
    (confirmation?.options as { message: string }).message,
    /1 unsaved document in its suspended session will be discarded/,
  );
  assert.deepEqual(host.captureIds(), [capture]);
  assert.match(host.messages()[0]!, /cannot remove project: StorageUnavailable/);

  host.answer("showConfirmation", { kind: "confirmed" });
  seedProject("a", "Alpha", [ALPHA]);
  await host.run("projects.remove", { arguments: { id: "a" } });
  assert.deepEqual(registry().projects, []);
  assert.equal(session("a"), undefined);
  assert.deepEqual(host.captureIds(), []);
});

test("removing the active project leaves its window as it is", async () => {
  seedProject("a", "Alpha", [ALPHA]);
  seedSession("a", "active");
  const window = host.openWindow({ roots: [ALPHA] });
  await startLaunched();
  host.answer("showConfirmation", { kind: "confirmed" });
  await host.run("projects.remove", { arguments: { id: "a" } });
  const [confirmation] = host.prompts();
  assert.equal((confirmation?.options as { message: string }).message, "Open documents and folders stay open.");
  assert.deepEqual(registry().projects, []);
  assert.deepEqual(host.rootsOf(window), [ALPHA]);
});

test("a dismissed confirmation removes nothing", async () => {
  seedProject("a", "Alpha", [ALPHA]);
  host.openWindow();
  await startLaunched();
  host.answer("showConfirmation", { kind: "dismissed" });
  await host.run("projects.remove", { arguments: { id: "a" } });
  assert.equal(registry().projects.length, 1);
});

test("selecting from the picker or a tree row switches to the project", async () => {
  seedProject("a", "Alpha", [ALPHA]);
  seedProject("b", "Beta", [BETA]);
  host.openWindow();
  await startLaunched();

  host.answer("showPicker", { kind: "dismissed" });
  await host.run("projects.select");
  assert.equal(host.replaced().length, 0);

  host.answer("showPicker", { kind: "selected", id: "b" });
  await host.run("projects.select");
  assert.deepEqual(host.rootsOf(host.windowTargets()[0]!), [BETA]);

  host.setSelectedText("alpha");
  await host.run("projects.select", { view: VIEW_ID });
  assert.equal(host.replaced().length, 2);
  assert.deepEqual(host.windowTargets().map(host.rootsOf), [[ALPHA]]);
});

test("a new project window opens with the project's roots", async () => {
  seedProject("a", "Alpha", [ALPHA]);
  seedSession("a", "active");
  const window = host.openWindow({ roots: [ALPHA] });
  const loose = host.openWindow();
  await startLaunched();
  await host.run("projects.new-window", { window: loose });
  assert.deepEqual(host.messages(), ["this window is not part of a project; select a project first"]);
  await host.run("projects.new-window", { window });
  assert.deepEqual(host.windowTargets().map(host.rootsOf), [[ALPHA], [], [ALPHA]]);
});
