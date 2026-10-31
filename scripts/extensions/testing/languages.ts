// Shared tests for language packages. A package's test calls
// `testLanguagePackage` with its `start` and its `LANGUAGES`; `testLanguageSet`
// checks every bundled package's languages together.
import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";
import type { LanguageDefinition } from "knot";
import { languages } from "knot";
import * as host from "knot/testing";

/** The predicates and directives the native engine evaluates; any other
 * predicate would be ignored, so its pattern would match unconditionally. */
const SUPPORTED_PREDICATES = new Set([
  "#eq?", "#not-eq?", "#any-eq?", "#any-not-eq?",
  "#match?", "#not-match?", "#any-match?", "#any-not-match?",
  "#any-of?", "#not-any-of?", "#set!",
]);

const INJECTION_CAPTURES = new Set(["injection.content", "injection.language"]);

/** A query's source without comments and string literals. */
function code(query: string): string {
  return query.replace(/"(?:[^"\\]|\\.)*"|;[^\n]*/g, (token) => (token.startsWith('"') ? '""' : ""));
}

function captures(query: string): Set<string> {
  return new Set([...code(query).matchAll(/@([\w.-]+)/g)].map((match) => match[1]!));
}

function predicates(query: string): Set<string> {
  return new Set([...code(query).matchAll(/#[\w-]+[?!]/g)].map((match) => match[0]));
}

function queries(definitions: readonly LanguageDefinition[]) {
  return definitions.flatMap((language) =>
    Object.entries(language.queries).map(([kind, source]) => ({ id: language.id, kind, source })),
  );
}

/** Checks one package's registrations and the queries it ships. */
export function testLanguagePackage(start: () => Promise<void>, definitions: readonly LanguageDefinition[]) {
  beforeEach(() => host.reset());

  test("registers every language before start resolves", async () => {
    await start();
    assert.deepEqual(
      host.languageDefinitions().map((definition) => definition.id),
      definitions.map((language) => language.id),
    );
    assert.equal((await languages.list()).length, definitions.length);
  });

  test("highlight queries capture only known highlight names", () => {
    const known = new Set(host.CAPTURE_NAMES);
    for (const { id, kind, source } of queries(definitions)) {
      if (kind !== "highlights") continue;
      const unknown = [...captures(source)].filter((name) => !name.startsWith("_") && !known.has(name));
      assert.deepEqual(unknown, [], `${id} highlights`);
    }
  });

  test("injection queries capture only injection names", () => {
    for (const { id, kind, source } of queries(definitions)) {
      if (kind !== "injections") continue;
      const unknown = [...captures(source)].filter(
        (name) => !name.startsWith("_") && !INJECTION_CAPTURES.has(name),
      );
      assert.deepEqual(unknown, [], `${id} injections`);
    }
  });

  test("queries use only predicates the native engine evaluates", () => {
    for (const { id, kind, source } of queries(definitions)) {
      const unsupported = [...predicates(source)].filter((name) => !SUPPORTED_PREDICATES.has(name));
      assert.deepEqual(unsupported, [], `${id} ${kind}`);
    }
  });
}

/** Checks the languages every bundled package registers, taken together. */
export function testLanguageSet(definitions: readonly LanguageDefinition[]) {
  test("covers every compiled-in grammar", async () => {
    const used = new Set(definitions.map((language) => language.grammar));
    assert.deepEqual([...used].sort(), [...(await languages.grammars())]);
  });

  test("fixed injection languages are registered", () => {
    const names = new Set(definitions.flatMap((language) => [language.id, ...(language.fileExtensions ?? [])]));
    for (const { id, kind, source } of queries(definitions)) {
      for (const [, name] of source.matchAll(/#set! injection\.language "([^"]+)"/g)) {
        assert.ok(names.has(name!), `${id} ${kind} injects unregistered ${name}`);
      }
    }
  });

  test("no two languages claim the same file name or extension", () => {
    for (const key of ["fileNames", "fileExtensions", "interpreters"] as const) {
      const claims = definitions.flatMap((language) => (language[key] ?? []).map((pattern) => pattern.toLowerCase()));
      assert.deepEqual(
        claims.filter((pattern, index) => claims.indexOf(pattern) !== index),
        [],
        key,
      );
    }
  });
}
