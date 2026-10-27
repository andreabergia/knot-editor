// Loaded with `node --import` before tests: resolves "knot" to the fake host's
// API module and "knot/testing" to its controls, both from the compiled tests.
import { registerHooks } from "node:module";
import { pathToFileURL } from "node:url";
import { join } from "node:path";

const build = join(import.meta.dirname, "../.test-build/scripts/extensions/testing");
const modules = {
  knot: pathToFileURL(join(build, "knot.js")).href,
  "knot/testing": pathToFileURL(join(build, "host.js")).href,
};

registerHooks({
  resolve(specifier, context, next) {
    const url = modules[specifier];
    return url === undefined ? next(specifier, context) : { url, shortCircuit: true };
  },
});
