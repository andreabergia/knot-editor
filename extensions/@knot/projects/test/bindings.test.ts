import assert from "node:assert/strict";
import { test } from "node:test";
import { bindingChange } from "../src/bindings.js";
import { emptyRegistry, type Project } from "../src/registry.js";

const alpha: Project = { id: "a", name: "Alpha", roots: ["x", "y"], createdAt: null, lastOpenedAt: null };
const beta: Project = { id: "b", name: "Beta", roots: ["z"], createdAt: null, lastOpenedAt: null };
const registry = { ...emptyRegistry(), projects: [alpha, beta] };

test("a bound window keeps its project while its roots match", () => {
  assert.equal(bindingChange(registry, alpha, true, ["y", "x"]), "keep");
});

test("new roots no other project uses move the project", () => {
  assert.equal(bindingChange(registry, alpha, true, ["x", "y", "w"]), "adopt");
});

test("cleared roots, another project's roots, or a gone or suspended project unbind", () => {
  assert.equal(bindingChange(registry, alpha, true, []), "unbind");
  assert.equal(bindingChange(registry, alpha, true, ["z"]), "unbind");
  assert.equal(bindingChange(registry, null, true, ["x", "y"]), "unbind");
  assert.equal(bindingChange(registry, alpha, false, ["x", "y"]), "unbind");
});
