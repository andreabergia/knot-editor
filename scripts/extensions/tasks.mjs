// Extension tooling tasks. Every directory with a tsconfig.json under
// extensions/@knot or examples/extensions is a TypeScript package.
//
//   node tasks.mjs check          type-check the contract fixtures, every package, and the tests
//   node tasks.mjs test           compile packages and tests with the fake host, then run node:test
//   node tasks.mjs build <dir>    emit one package's dist/ in place
import { execFileSync } from "node:child_process";
import { existsSync, readdirSync, rmSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const tools = dirname(fileURLToPath(import.meta.url));
const root = resolve(tools, "../..");
const tsc = join(tools, "node_modules/typescript/bin/tsc");

function packages() {
  return ["extensions", "examples/extensions"].flatMap((base) =>
    readdirSync(join(root, base), { withFileTypes: true })
      .filter((scope) => scope.isDirectory())
      .flatMap((scope) =>
        readdirSync(join(root, base, scope.name)).map((name) => join(root, base, scope.name, name)),
      )
      .filter((directory) => existsSync(join(directory, "tsconfig.json"))),
  );
}

function node(...args) {
  try {
    execFileSync(process.execPath, args, { stdio: "inherit" });
  } catch (error) {
    process.exit(error.status ?? 1);
  }
}

function compile(...args) {
  node(tsc, ...args);
}

function testFiles(directory) {
  return readdirSync(directory, { withFileTypes: true, recursive: true })
    .filter((entry) => entry.isFile() && entry.name.endsWith(".test.js"))
    .map((entry) => join(entry.parentPath, entry.name));
}

const [task, argument] = process.argv.slice(2);
if (task === "check") {
  compile("-p", join(tools, "tsconfig.json"));
  compile("-p", join(tools, "tsconfig.test.json"), "--noEmit");
  for (const directory of packages()) compile("-p", join(directory, "tsconfig.json"), "--noEmit");
} else if (task === "test") {
  const build = join(tools, ".test-build");
  rmSync(build, { recursive: true, force: true });
  compile("-p", join(tools, "tsconfig.test.json"));
  const files = ["extensions", "examples"].flatMap((base) =>
    existsSync(join(build, base)) ? testFiles(join(build, base)) : [],
  );
  node("--import", join(tools, "testing/register.mjs"), "--test", ...files);
} else if (task === "build" && argument !== undefined) {
  compile("-p", join(resolve(process.env.INIT_CWD ?? process.cwd(), argument), "tsconfig.json"));
} else {
  console.error("usage: node tasks.mjs check | test | build <package directory>");
  process.exit(2);
}
