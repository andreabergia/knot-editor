import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";
import * as host from "knot/testing";
import { start } from "../src/extension.js";

beforeEach(async () => {
  host.reset();
  host.openWindow();
  await start();
});

const log = () => host.view("example.git-log").text;

test("activation publishes the status log and both frames", () => {
  assert.equal(
    log(),
    "$ git status --short\nM  src/main.rs\nA  src/app/tree_view.rs\nM  docs/architecture/semantic-ui.md\nU  notes.txt\n",
  );
  assert.equal(host.view("example.git-status").invalidations, 1);
});

test("row actions mutate the fixture and refresh the frames", async () => {
  await host.run("example.git.stage", { arguments: { path: "notes.txt" } });
  const [staged] = await host.treeItems("example.git-status", host.windowTargets()[0]!);
  assert.equal(staged?.description, "3 file(s)");
  await host.run("example.git.discard", { arguments: { path: "missing" } });
  assert.equal(host.view("example.git-status").invalidations, 2);
});

test("repositories are added, renamed, and removed through prompts", async () => {
  host.answer("showInput", { kind: "accepted", value: "/src/tools" });
  await host.run("example.git.add-repository");
  host.answer("showInput", { kind: "accepted", value: "Tools" });
  await host.run("example.git.rename-repository", { arguments: { id: "repo-3" } });
  host.answer("showConfirmation", { kind: "dismissed" });
  await host.run("example.git.remove-repository", { arguments: { id: "knot" } });
  const window = host.windowTargets()[0]!;
  const names = async () =>
    (await host.treeItems("example.git-status", window, "repositories")).map((item) => item.label);
  assert.deepEqual(await names(), ["knot", "website", "Tools"]);

  host.answer("showConfirmation", { kind: "confirmed" });
  await host.run("example.git.remove-repository", { arguments: { id: "knot" } });
  assert.deepEqual(await names(), ["website", "Tools"]);
});

test("activating a row copies the selected path through the view handler", async () => {
  host.setSelectedText("notes.txt");
  await host.run("example.git.activate", { view: "example.git-status" });
  assert.equal(host.clipboard(), "notes.txt");
  assert.match(log(), /open notes\.txt\n$/);
});
