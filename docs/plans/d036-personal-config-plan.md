# D036: Personal JavaScript configuration

Status: completed. All checkpoint review gates passed on 2026-10-04.

Source: [D003's deferred keymap need](../roadmap.md). This plan establishes
personal configuration independently; JavaScript-defined keymaps will have a
separate design session and plan.

## Outcome and design

A user can place optional `pre-init.js` and `post-init.js` modules in Knot's
per-user configuration directory. Knot runs pre-init before installed extension
entry modules and post-init after extension startup has finished. Both phases
use one persistent personal-config JavaScript lifecycle, so registrations and
module state from pre-init remain available in post-init. The modules may import
other JavaScript files inside the config directory and use the public
`knot:editor` facade. Missing config files mean no work for that phase.

The application owns file discovery, source capture, startup sequencing, and
error presentation. `host` owns the isolate, module evaluation, and JavaScript
values. Personal config has a distinct identity from installed extensions and
appears as such in startup diagnostics. Extension package manifests are not
required for config. Pre-init may configure Knot through supported APIs, but
does not choose, reorder, or load extensions. The first slice does not add a
settings file or a settings API solely to demonstrate config execution.

An error reading, validating, or evaluating either configured phase halts
normal startup. Pre-init failure prevents extension loading. Post-init failure
stops startup after extension loading. In either case, the normal editor is
unavailable and a dedicated error window shows the phase, config path, useful
error location and message, and Copy and Quit controls. Copy places the full
diagnostic on the clipboard. The process stays open to display the error until
the user quits. Missing files are not errors. Ordinary installed-extension
failures keep their existing independent-package reporting behavior.

Normal product windows and launch requests become usable only after successful
completion of both phases. The diagnostic window remains available even when
the personal config fails before any product window exists. On fatal config
failure, unload the config lifecycle and any loaded extensions; no registrations
from the failed launch remain active.

## Exclusions and deferred decisions

- D003 keymap APIs, precedence, and binding behavior are separate work.
- No declarative settings format, config reload, retry in the error window, or
  config-driven extension selection in this slice.
- Specific future settings APIs and their persistence semantics are deferred
  until a setting needs configuration.

## Checkpoints

### 1. Config source and phase contract ✅

- Resolve one platform-appropriate per-user config directory, injectable for
  tests. Capture `.js` and `.mjs` sources without blocking the foreground.
  Validate local imports and reject paths escaping the config directory.
- Treat each phase entry as optional; make unreadable, invalid, or missing
  imported files actionable errors with source locations where available.
- Test empty config, either phase alone, local imports, path escape, and read
  failures.
- **Review gate:** inspect the directory layout and diagnostic contract.

Implemented source layout: the selected config directory contains optional
`pre-init.js` and `post-init.js` entries, with relative `.js` and `.mjs` imports
below that directory. An existing `XDG_CONFIG_HOME/knot` takes precedence over
an existing `~/.config/knot`, then the platform `Knot` config directory. Only
one directory is active. Capture runs through the application boundary on a
caller-provided directory; product startup schedules it on the background
executor. The host compiles reachable modules
without evaluating them to validate syntax and static imports. Diagnostics
carry the phase, path, optional line and column, and cause. Checkpoint 1 tests
cover directory selection, optional entries, local imports, escapes, missing
imports, invalid syntax, and read failures. Post-init capture and validation
errors are retained until the post-init phase so installed extensions still
start after a successful pre-init. The XDG precedence was agreed during review.

### 2. One lifecycle with two ordered module evaluations ✅

- Extend the package-graph host boundary so the same isolate can evaluate the
  optional pre-init and post-init entries as separate awaited root turns.
  Preserve pre-init module state and registrations across the extension phase.
- Test evaluation order, shared state, top-level await, runtime errors, and
  lifecycle teardown without gpui.
- **Review gate:** inspect phase isolation and host error behavior.

The host accepts one immutable graph with either or both phase roots, loads it
without evaluating either root, then exposes separate awaited phase turns on
the same lifecycle. Pre-init imports cannot reach `post-init.js`, including
through a helper module. Compilation and runtime errors retain generated
source locations; the application owns the fatal startup response in
checkpoint 3. Host tests cover shared module state, an awaited command
registration, top-level await, post-init alone, phase errors, and unload.
The host phase behavior is covered by tests.

### 3. Product startup and fatal error window ✅

- Gate product readiness and incoming open requests on pre-init, installed
  extension startup, then post-init. Keep existing installed-extension failure
  behavior. Route config failures to a dedicated error window with Copy and
  Quit, and tear down all lifecycles from the aborted startup.
- Test successful startup, failure in each phase, skipped downstream phases,
  cleanup, queued open requests, and clipboard diagnostic content. Review the
  error window manually in a product build.
- **Review gate:** approve startup flow and the user-facing failure experience.

The product queues launch requests, captures config on the background executor,
awaits pre-init, installed-extension startup, then post-init, and opens product
windows only after success. Config failure unloads all admitted lifecycles and
opens a dedicated native window with phase, source location, message, Copy,
and Quit. Product tests cover successful startup, either phase failing,
independent installed-extension failures, cleanup of command and completion
registrations, queued file opens, and clipboard content. Manual product-window
review found that Cmd-Q did not reach the error window. The error window now
owns keyboard focus and handles the Quit command in its own key context; a
key-dispatch test covers the path. The product build passed manual review for
success, pre-init failure, post-init failure, Copy, Quit, and the Cmd-Q recheck.

The architecture subsystem references and validated decisions reflect the
implemented config ownership and lifecycle.
