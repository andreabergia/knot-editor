// The package's in-memory state and its durable writes. Records are the
// source of truth; the state mirrors the last committed registry and sessions.
import { recovery, storage } from "knot";
import type { CaptureId, ViewRegistration, WindowInfo, WindowTarget } from "knot";
import {
  RECORD_KEY,
  emptyRegistry,
  registryFromJson,
  sameRoots,
  selected,
  type Project,
  type Registry,
} from "./registry.js";
import { describe, report } from "./report.js";
import { capturesOf, sessionFromJson, sessionKey, type Session } from "./session.js";
import { windowRoots } from "./layout.js";

export type ProjectsState = {
  registry: Registry;
  /** Why the registry could not be read, while the last good state is kept. */
  loadFailure: string | null;
  view: ViewRegistration | null;
  /** Last durable session record per project ID. */
  sessions: Map<string, Session>;
  /** Projects whose session record has an unknown schema. */
  unreadable: Set<string>;
  /** Project ID per bound window. */
  bindings: Map<WindowTarget, string>;
  /** Whether startup windows are committed; tracking waits for them. */
  launched: boolean;
  /** Membership-relevant metadata per document; revision-only changes are ignored. */
  tracked: Map<string, string>;
};

export function createState(): ProjectsState {
  return {
    registry: emptyRegistry(),
    loadFailure: null,
    view: null,
    sessions: new Map(),
    unreadable: new Set(),
    bindings: new Map(),
    launched: false,
    tracked: new Map(),
  };
}

/** Reloads the shared record, retaining the last good state on failure. */
export async function reload(state: ProjectsState) {
  try {
    const result = await storage.read(RECORD_KEY);
    state.registry = result.found ? registryFromJson(result.value) : emptyRegistry();
    state.loadFailure = null;
  } catch (error) {
    state.loadFailure = describe(error);
  }
}

export async function refreshTree(state: ProjectsState) {
  if (state.view !== null) await state.view.invalidate(null);
}

export async function commit(state: ProjectsState, next: Registry) {
  await storage.write(RECORD_KEY, next);
  state.registry = next;
  state.loadFailure = null;
  await refreshTree(state);
}

/** Records the project as the recent selection; failure is reported only. */
export async function recordSelection(state: ProjectsState, project: Project) {
  const next = selected(state.registry, project.id, Date.now());
  try {
    await storage.write(RECORD_KEY, next);
    state.registry = next;
  } catch (error) {
    await report(`opened ${project.name}, but recording the selection failed: ${describe(error)}`);
  }
}

export async function release(captures: Iterable<CaptureId>) {
  for (const capture of captures) {
    try {
      await recovery.release(capture);
    } catch {
      // An unreleased capture stays listed and is reconciled at activation.
    }
  }
}

export async function loadSessions(state: ProjectsState) {
  state.sessions.clear();
  state.unreadable.clear();
  for (const project of state.registry.projects) {
    const result = await storage.read(sessionKey(project.id));
    if (!result.found) continue;
    const session = sessionFromJson(result.value);
    if (session === "unreadable") state.unreadable.add(project.id);
    else state.sessions.set(project.id, session);
  }
}

/**
 * Durably replaces a session record, then releases the captures the previous
 * record referenced and the new one does not.
 */
export async function writeSession(
  state: ProjectsState,
  id: string,
  next: Session,
  options: { keepCaptures?: boolean } = {},
) {
  const previous = state.sessions.get(id) ?? null;
  await storage.write(sessionKey(id), next);
  state.sessions.set(id, next);
  if (options.keepCaptures) return;
  const kept = capturesOf(next);
  await release([...capturesOf(previous)].filter((capture) => !kept.has(capture)));
}

export function isActive(state: ProjectsState, project: Project) {
  return state.sessions.get(project.id)?.state === "active";
}

/** Windows bound to the project, in listing order. */
export function boundWindows(state: ProjectsState, project: Project, list: readonly WindowInfo[]) {
  return list.filter((window) => state.bindings.get(window.target) === project.id);
}

/** Windows whose roots equal the project's, in listing order. */
export function matchingWindows(project: Project, list: readonly WindowInfo[]) {
  return list.filter((window) => sameRoots(windowRoots(window), project.roots));
}
