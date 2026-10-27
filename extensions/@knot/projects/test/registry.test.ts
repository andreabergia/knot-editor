import assert from "node:assert/strict";
import { test } from "node:test";
import {
  emptyRegistry,
  nameTaken,
  projectWithRoots,
  registryFromJson,
  rootsLabel,
  sameRoots,
  selected,
  suggestedName,
  withProject,
  withoutProject,
  type Project,
} from "../src/registry.js";

function project(id: string, name: string, roots: string[]): Project {
  return { id, name, roots, createdAt: null, lastOpenedAt: null };
}

const alpha = project("a", "Alpha", ["file:///alpha/"]);
const beta = project("b", " Beta ", ["file:///beta/", "file:///shared/"]);
const registry = { ...emptyRegistry(), projects: [alpha, beta], recentProjectId: "b" };

test("parses a stored registry and defaults optional fields", () => {
  const parsed = registryFromJson({
    schema: 1,
    projects: [{ id: "a", name: "Alpha", roots: ["file:///alpha/", 3], createdAt: "x" }],
    recentProjectId: 7,
  });
  assert.deepEqual(parsed, {
    schema: 1,
    projects: [{ id: "a", name: "Alpha", roots: ["file:///alpha/"], createdAt: null, lastOpenedAt: null }],
    recentProjectId: null,
  });
});

test("rejects unsupported schemas, shapes, and entries", () => {
  assert.throws(() => registryFromJson([]), /not an object/);
  assert.throws(() => registryFromJson({ schema: 2, projects: [] }), /unsupported projects record schema: 2/);
  assert.throws(() => registryFromJson({ schema: 1, projects: [{ id: "", name: "x", roots: [] }] }), /invalid entry/);
});

test("names compare trimmed and case-insensitively, except the renamed project", () => {
  assert.equal(nameTaken(registry, "  alpha ", null), true);
  assert.equal(nameTaken(registry, "BETA", null), true);
  assert.equal(nameTaken(registry, "alpha", "a"), false);
  assert.equal(nameTaken(registry, "Gamma", null), false);
});

test("root sets match regardless of order, and an empty set matches nothing", () => {
  assert.equal(sameRoots(["x", "y"], ["y", "x"]), true);
  assert.equal(sameRoots(["x"], ["x", "y"]), false);
  assert.equal(projectWithRoots(registry, ["file:///shared/", "file:///beta/"]), beta);
  assert.equal(projectWithRoots(registry, ["file:///beta/"]), null);
  assert.equal(projectWithRoots({ ...registry, projects: [project("e", "Empty", [])] }, []), null);
});

test("suggested names decode the last path segment", () => {
  assert.equal(suggestedName("file:///src/my%20app/"), "my app");
  assert.equal(suggestedName("file:///src/bad%E0/"), "bad%E0");
  assert.equal(suggestedName("file:///"), "file:");
});

test("root labels name the first root and count the rest", () => {
  assert.equal(rootsLabel(alpha), "alpha");
  assert.equal(rootsLabel(beta), "beta +1");
  assert.equal(rootsLabel(project("e", "Empty", [])), null);
});

test("record updates keep the recent selection consistent", () => {
  const gamma = project("g", "Gamma", ["file:///gamma/"]);
  assert.equal(withProject(registry, gamma).recentProjectId, "g");
  assert.equal(withoutProject(registry, "b").recentProjectId, null);
  assert.equal(withoutProject(registry, "a").recentProjectId, "b");
  const next = selected(registry, "a", 42);
  assert.equal(next.recentProjectId, "a");
  assert.equal(next.projects[0]?.lastOpenedAt, 42);
  assert.equal(next.projects[1]?.lastOpenedAt, null);
});
