import assert from "node:assert/strict";
import { test } from "node:test";
import type { CaptureId, DocumentId } from "knot";
import { capturesOf, sessionFromJson, suspendedWithoutCaptures } from "../src/session.js";

const layout = { version: 1, root: null };

test("reads schema 2 sessions and drops entries without a reference", () => {
  const session = sessionFromJson({
    schema: 2,
    state: "active",
    windows: [
      {
        entries: [{ uri: "file:///a" }, { capture: "c1" }, { document: "d1", capture: "c2" }, 5],
        selected: 1,
        layout,
      },
    ],
  });
  assert.deepEqual(session, {
    schema: 2,
    state: "active",
    windows: [
      {
        entries: [{ uri: "file:///a" }, { document: "d1", capture: "c2" }],
        selected: 1,
        layout,
      },
    ],
  });
});

test("schema 1 windows have no layout", () => {
  const session = sessionFromJson({
    schema: 1,
    state: "suspended",
    windows: [{ entries: [], selected: "x", layout }],
  });
  assert.deepEqual(session, {
    schema: 2,
    state: "suspended",
    windows: [{ entries: [], selected: null, layout: null }],
  });
});

test("unknown schemas, states, and shapes are unreadable", () => {
  assert.equal(sessionFromJson({ schema: 3, state: "active", windows: [] }), "unreadable");
  assert.equal(sessionFromJson({ schema: 2, state: "closed", windows: [] }), "unreadable");
  assert.equal(sessionFromJson({ schema: 2, state: "active", windows: [{}] }), "unreadable");
  assert.equal(sessionFromJson(null), "unreadable");
});

test("a session that loses its last window keeps membership without captures", () => {
  const session = {
    schema: 2,
    state: "active" as const,
    windows: [
      {
        entries: [
          { uri: "file:///a", document: "d1" as DocumentId, capture: "c1" as CaptureId },
          { document: "d2" as DocumentId },
        ],
        selected: 0,
        layout,
      },
    ],
  };
  assert.deepEqual([...capturesOf(session)], ["c1"]);
  const suspended = suspendedWithoutCaptures(session);
  assert.equal(suspended.state, "suspended");
  assert.deepEqual(suspended.windows[0]?.entries, [{ uri: "file:///a", document: "d1" }, { document: "d2" }]);
  assert.equal(suspended.windows[0]?.layout, layout);
  assert.deepEqual(capturesOf(suspended).size, 0);
  assert.deepEqual(suspendedWithoutCaptures(undefined).windows, []);
});
