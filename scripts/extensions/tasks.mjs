// Extension tooling tasks. Every directory with a tsconfig.json under
// extensions/@knot or examples/extensions is a TypeScript package.
//
//   node tasks.mjs check          type-check the contract fixtures and every package
//   node tasks.mjs build <dir>    emit one package's dist/ in place
import { execFileSync } from "node:child_process";
import { existsSync, readdirSync } from "node:fs";
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

function compile(...args) {
  execFileSync(process.execPath, [tsc, ...args], { stdio: "inherit" });
}

const [task, argument] = process.argv.slice(2);
if (task === "check") {
  compile("-p", join(tools, "tsconfig.json"));
  for (const directory of packages()) compile("-p", join(directory, "tsconfig.json"), "--noEmit");
} else if (task === "build" && argument !== undefined) {
  compile("-p", join(resolve(process.env.INIT_CWD ?? process.cwd(), argument), "tsconfig.json"));
} else {
  console.error("usage: node tasks.mjs check | build <package directory>");
  process.exit(2);
}
