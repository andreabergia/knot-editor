import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";
import { documents, windows } from "knot";
import * as host from "knot/testing";
import {
  describeWindow,
  documentsById,
  entryFor,
  isReusable,
  presentedDocuments,
  selectedDocument,
  viewCount,
} from "../src/layout.js";

beforeEach(() => host.reset());

async function info(id: Parameters<typeof host.document>[0]) {
  const document = await documents.get(id);
  assert.ok(document !== null);
  return document;
}

test("clean files are references and dirty documents keep their identity", async () => {
  const clean = await info(host.addDocument({ uri: "file:///a" }));
  const dirty = await info(host.addDocument({ uri: "file:///b", dirty: true }));
  const destination = await info(host.addDocument({ uri: "file:///c", kind: "destination" }));
  const untitled = await info(host.addDocument({ text: "draft" }));
  assert.deepEqual(entryFor(clean, false), { uri: "file:///a" });
  assert.deepEqual(entryFor(dirty, false), { uri: "file:///b", document: dirty.id });
  assert.deepEqual(entryFor(destination, false), { uri: "file:///c", document: destination.id });
  assert.deepEqual(entryFor(untitled, false), { document: untitled.id });
  assert.equal(entryFor(untitled, true), null);
});

test("windows list each document once, in tab order, with its selection", async () => {
  const a = host.addDocument({ uri: "file:///a" });
  const b = host.addDocument({ uri: "file:///b" });
  const empty = host.addDocument();
  host.openWindow({ documents: [a, b, a, empty] });
  host.openWindow({ documents: [b] });
  const list = await windows.list();
  assert.deepEqual(presentedDocuments(list), [a, b, empty]);
  assert.equal(viewCount(list, a), 2);
  assert.equal(viewCount(list, b), 2);
  assert.equal(selectedDocument(list[0]!), empty);

  const byId = documentsById(await documents.list());
  const described = await describeWindow(list[0]!, byId, new Map());
  // The clean, empty untitled document is not recorded, so nothing is selected.
  assert.deepEqual(described.entries, [{ uri: "file:///a" }, { uri: "file:///b" }]);
  assert.equal(described.selected, null);
});

test("a rootless window with at most one clean, empty untitled document is reusable", async () => {
  const empty = host.openWindow({ documents: [host.addDocument()] });
  const blank = host.openWindow();
  const draft = host.openWindow({ documents: [host.addDocument({ text: "x" })] });
  const rooted = host.openWindow({ roots: ["file:///r/"] });
  const byId = documentsById(await documents.list());
  const reusable = async (target: typeof empty) =>
    isReusable((await windows.get(target))!, byId);
  assert.equal(await reusable(empty), true);
  assert.equal(await reusable(blank), true);
  assert.equal(await reusable(draft), false);
  assert.equal(await reusable(rooted), false);
});
