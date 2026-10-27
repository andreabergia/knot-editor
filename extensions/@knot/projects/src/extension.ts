// Bundled project registry and project sessions. Each project has a stable
// identity, a name, and an ordered set of roots that no other project shares.
// Records are application-global extension storage and the presentation is a
// contributed tree. Root selection, window replacement, and text retention are
// the application's public operations, so this package needs no private host
// access.
//
// A project's session is active or suspended. A window binds to a project when
// its roots equal the project's roots, in any order, and the session is active,
// so a manually opened folder never binds a suspended project and its retained
// captures stay safe. Root edits in a bound window move the project and its
// other windows to the new roots, while clearing the roots or choosing another
// project's roots unbinds the window. Each session window stores its captured
// layout, so switching back and restarting restore splits, docks, and view
// state.
//
// A session record with an unknown schema is kept unchanged and never switched
// to, and while one exists no capture is released as an orphan, because that
// record may reference it.
import { workbench } from "knot";
import { registerCommands } from "./commands.js";
import { describe } from "./report.js";
import { createState, loadSessions, reload } from "./state.js";
import { afterLaunch, onApplicationEvent } from "./tracking.js";
import { registerTree } from "./tree.js";

export async function start() {
  const state = createState();
  await reload(state);
  try {
    await loadSessions(state);
  } catch (error) {
    state.loadFailure = state.loadFailure ?? describe(error);
  }
  await registerTree(state);
  await registerCommands(state);
  await reload(state);
  const observation = await workbench.observe((event) => onApplicationEvent(state, event));
  // A package restarted after launch sees it in the snapshot instead of an event.
  if (observation.snapshot.launched) await afterLaunch(state);
}
