// Live tracking of active sessions. Commands and listeners are serial roots
// of this package, so tracking never records the intermediate state of a
// switch. Native hot exit restores product windows at startup; tracking waits
// for launch so restored windows rebind by root set first.
import { documents, recovery, windows } from "knot";
import type { ApplicationEvent, CaptureId, DocumentId, DocumentInfo, WindowInfo } from "knot";
import { syncBindings } from "./bindings.js";
import { describeWindow, documentsById } from "./layout.js";
import type { Project } from "./registry.js";
import { describe } from "./report.js";
import {
  SESSION_SCHEMA,
  capturesOf,
  sameSession,
  suspendedWithoutCaptures,
  type Session,
} from "./session.js";
import {
  boundWindows,
  isActive,
  refreshTree,
  release,
  writeSession,
  type ProjectsState,
} from "./state.js";

/**
 * Records an active project's current membership. Losing its last window
 * suspends it with its last membership and no captures.
 */
export async function trackProject(
  state: ProjectsState,
  project: Project,
  list: readonly WindowInfo[],
  byId: Map<DocumentId, DocumentInfo>,
) {
  const session = state.sessions.get(project.id);
  const bound = boundWindows(state, project, list);
  const next: Session =
    bound.length === 0
      ? suspendedWithoutCaptures(session)
      : {
          schema: SESSION_SCHEMA,
          state: "active",
          windows: await Promise.all(bound.map((window) => describeWindow(window, byId, new Map()))),
        };
  if (session === undefined || !sameSession(session, next)) await writeSession(state, project.id, next);
}

/** Live tracking: rewrite active records that changed. Suspended records are never written. */
async function track(state: ProjectsState) {
  if (!state.launched) return;
  if (!state.registry.projects.some((project) => isActive(state, project))) {
    state.bindings.clear();
    return;
  }
  try {
    const [list, docs] = await Promise.all([windows.list(), documents.list()]);
    await syncBindings(state, list);
    const byId = documentsById(docs);
    for (const project of state.registry.projects.filter((project) => isActive(state, project))) {
      await trackProject(state, project, list, byId);
    }
  } catch {
    // Tracking is retried on the next change; quit stops storage first, so a
    // shutdown never records windows closing.
  }
}

/**
 * Releases this package's captures that no session record references. An
 * unreadable record may reference any capture, so nothing is released while
 * one exists.
 */
export async function reconcileCaptures(state: ProjectsState) {
  if (state.unreadable.size > 0) return;
  const referenced = new Set<CaptureId>();
  for (const session of state.sessions.values()) {
    for (const capture of capturesOf(session)) referenced.add(capture);
  }
  const listed = await recovery.list();
  await release(listed.map((capture) => capture.id).filter((id) => !referenced.has(id)));
}

/**
 * Marks every active session without a bound window as suspended, keeping
 * its entries. Only state is written; nothing is released.
 */
async function suspendUnbound(state: ProjectsState) {
  const list = await windows.list();
  await syncBindings(state, list);
  for (const project of state.registry.projects) {
    const session = state.sessions.get(project.id);
    if (session?.state !== "active" || boundWindows(state, project, list).length > 0) continue;
    await writeSession(state, project.id, { ...session, state: "suspended" }, { keepCaptures: true });
  }
}

/**
 * Once startup windows exist, rebinds restored windows by root set and
 * suspends the remaining active sessions.
 */
export async function afterLaunch(state: ProjectsState) {
  state.launched = true;
  try {
    await suspendUnbound(state);
    await reconcileCaptures(state);
  } catch (error) {
    state.loadFailure = state.loadFailure ?? describe(error);
  }
  await refreshTree(state);
}

/** Whether a document's membership-relevant metadata changed since last seen. */
function membershipChanged(state: ProjectsState, document: DocumentInfo) {
  const key = `${document.kind}|${document.resourceUri}|${document.dirty}`;
  if (state.tracked.get(document.id) === key) return false;
  state.tracked.set(document.id, key);
  return true;
}

export async function onApplicationEvent(state: ProjectsState, event: ApplicationEvent) {
  if (event.type === "launched") {
    await afterLaunch(state);
    return;
  }
  if (event.type === "documentChanged" && !membershipChanged(state, event.document)) return;
  if (event.type === "documentOpened") membershipChanged(state, event.document);
  if (event.type === "documentClosed") state.tracked.delete(event.document);
  await track(state);
  if (event.type === "workspaceChanged" || event.type === "windowClosed") await refreshTree(state);
}
