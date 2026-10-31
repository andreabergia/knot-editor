// Loaded with `node --import` before tests: resolves "knot" to the fake host's
// API module, "knot/testing" to its controls, and "knot/testing/languages" to
// the shared language-package checks, all from the compiled tests.
import { registerHooks } from "node:module";
import { pathToFileURL } from "node:url";
import { join } from "node:path";

const build = join(import.meta.dirname, "../.test-build/scripts/extensions/testing");
const modules = {
  knot: pathToFileURL(join(build, "knot.js")).href,
  "knot/testing": pathToFileURL(join(build, "host.js")).href,
  "knot/testing/languages": pathToFileURL(join(build, "languages.js")).href,
};

registerHooks({
  resolve(specifier, context, next) {
    const url = modules[specifier];
    return url === undefined ? next(specifier, context) : { url, shortCircuit: true };
  },
});
