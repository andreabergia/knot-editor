// Bundled project registry and project sessions. Each project has a stable
// identity, a name, and an ordered set of roots that no other project shares.
// Records are application-global extension storage and the
// presentation is a contributed tree. Root selection, window replacement, and
// text retention are the application's public operations, so this package needs
// no private host access.
//
// A project's session is active or suspended. A window binds to a project when
// its roots equal the project's roots, in any order, and the session is active,
// so a manually opened folder never binds a suspended project and its retained
// captures stay safe. Bindings live in memory: root edits in a bound window move
// the project and its other windows to the new roots, while clearing the roots
// or choosing another project's roots unbinds the window. Switching suspends the source project's windows with
// their unsaved text retained, closes them, and opens the destination's in one
// native commit. Commands and listeners are serial roots of this package, so a
// switch never interleaves with live tracking and no intermediate state is
// recorded.
import { commands, documents, recovery, storage, windows, workbench, workspaces } from "knot";

const RECORD_KEY = "projects";
const SCHEMA = 1;
const SESSION_SCHEMA = 1;
const SESSION_PREFIX = "session:";
const SWITCH_ATTEMPTS = 3;
const VIEW_ID = "builtin.projects";

/** @typedef {import("knot").JsonValue} JsonValue */
/** @typedef {import("knot").TreeItem} TreeItem */
/** @typedef {import("knot").ViewContext} ViewContext */
/** @typedef {import("knot").ViewRegistration} ViewRegistration */
/** @typedef {import("knot").WindowInfo} WindowInfo */
/** @typedef {import("knot").WindowTarget} WindowTarget */
/** @typedef {import("knot").DocumentInfo} DocumentInfo */
/** @typedef {import("knot").DocumentId} DocumentId */
/** @typedef {import("knot").CaptureId} CaptureId */
/** @typedef {import("knot").SessionEntryOutcome} SessionEntryOutcome */
/** @typedef {{ id: string, name: string, roots: string[], createdAt: number | null, lastOpenedAt: number | null }} Project */
/** @typedef {{ schema: number, projects: Project[], recentProjectId: string | null }} Registry */
/** @typedef {{ uri?: string, document?: DocumentId, capture?: CaptureId }} Entry */
/** @typedef {{ entries: Entry[], selected: number | null }} SessionWindow */
/** @typedef {{ schema: number, state: "active" | "suspended", windows: SessionWindow[] }} Session */

/** @returns {Registry} */
function emptyRegistry() {
  return { schema: SCHEMA, projects: [], recentProjectId: null };
}

/** @param {unknown} error @returns {string} */
function describe(error) {
  const code =
    error !== null && typeof error === "object"
      ? /** @type {{ code?: unknown }} */ (error).code
      : undefined;
  const message = error instanceof Error ? error.message : undefined;
  if (typeof code === "string") {
    return typeof message === "string" && message !== code ? `${code}: ${message}` : code;
  }
  return message ?? String(error);
}

/** @param {unknown} error @returns {string | null} */
function errorCode(error) {
  if (error === null || typeof error !== "object") return null;
  const code = /** @type {{ code?: unknown }} */ (error).code;
  return typeof code === "string" ? code : null;
}

/** @param {JsonValue} value @returns {Record<string, JsonValue> | null} */
function object(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? /** @type {Record<string, JsonValue>} */ (value)
    : null;
}

/** @param {JsonValue} value @returns {Project | null} */
function projectFromJson(value) {
  const record = object(value);
  if (record === null) return null;
  if (typeof record.id !== "string" || record.id.length === 0) return null;
  if (typeof record.name !== "string" || record.name.length === 0) return null;
  if (!Array.isArray(record.roots)) return null;
  const roots = record.roots.filter((root) => typeof root === "string");
  return {
    id: record.id,
    name: record.name,
    roots,
    createdAt: typeof record.createdAt === "number" ? record.createdAt : null,
    lastOpenedAt: typeof record.lastOpenedAt === "number" ? record.lastOpenedAt : null,
  };
}

/** @returns {Promise<Registry>} */
async function readRegistry() {
  const result = await storage.read(RECORD_KEY);
  if (!result.found) return emptyRegistry();
  const record = object(result.value);
  if (record === null) throw new Error("stored projects record is not an object");
  if (record.schema !== SCHEMA || !Array.isArray(record.projects)) {
    throw new Error(`unsupported projects record schema: ${String(record.schema)}`);
  }
  const projects = [];
  for (const entry of record.projects) {
    const project = projectFromJson(entry);
    if (project === null) throw new Error("stored projects record contains an invalid entry");
    projects.push(project);
  }
  return {
    schema: SCHEMA,
    projects,
    recentProjectId:
      typeof record.recentProjectId === "string" ? record.recentProjectId : null,
  };
}

/** @param {JsonValue} value @returns {Entry | null} */
function entryFromJson(value) {
  const record = object(value);
  if (record === null) return null;
  /** @type {Entry} */
  const entry = {};
  if (typeof record.uri === "string") entry.uri = record.uri;
  if (typeof record.document === "string") entry.document = /** @type {DocumentId} */ (record.document);
  if (typeof record.capture === "string") entry.capture = /** @type {CaptureId} */ (record.capture);
  return entry.uri === undefined && entry.document === undefined ? null : entry;
}

/** @param {JsonValue} value @returns {Session | null} */
function sessionFromJson(value) {
  const record = object(value);
  if (record === null || record.schema !== SESSION_SCHEMA || !Array.isArray(record.windows)) return null;
  if (record.state !== "active" && record.state !== "suspended") return null;
  /** @type {SessionWindow[]} */
  const sessionWindows = [];
  for (const window of record.windows) {
    const fields = object(window);
    if (fields === null || !Array.isArray(fields.entries)) return null;
    const entries = [];
    for (const entry of fields.entries) {
      const parsed = entryFromJson(entry);
      if (parsed !== null) entries.push(parsed);
    }
    const selected = typeof fields.selected === "number" ? fields.selected : null;
    sessionWindows.push({ entries, selected });
  }
  return { schema: SESSION_SCHEMA, state: record.state, windows: sessionWindows };
}

/** @type {Registry} */
let registry = emptyRegistry();
/** @type {string | null} */
let loadFailure = null;
/** @type {ViewRegistration | null} */
let view = null;
/** Last durable session record per project ID.
 * @type {Map<string, Session>} */
const sessions = new Map();
/** Project ID per bound window.
 * @type {Map<WindowTarget, string>} */
const bindings = new Map();

/** Reloads the shared record, retaining the last good state on failure. */
async function reload() {
  try {
    registry = await readRegistry();
    loadFailure = null;
  } catch (error) {
    loadFailure = describe(error);
  }
}

async function refreshTree() {
  if (view !== null) await view.invalidate(null);
}

/** @param {Registry} next */
async function commit(next) {
  await storage.write(RECORD_KEY, next);
  registry = next;
  loadFailure = null;
  await refreshTree();
}

/** Shows a message in the command's window, or in an explicit one when a
 * switch closed the command's window.
 * @param {string} message @param {WindowTarget} [target] */
async function report(message, target) {
  try {
    await workbench.showMessage({
      title: "Projects",
      message,
      ...(target === undefined ? {} : { target }),
    });
  } catch {
    // A report without a live window is not actionable; the tree still updates.
  }
}

function newProjectId() {
  return `p-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}

/** @param {string} uri @returns {string} */
function suggestedName(uri) {
  const segment = uri.split("/").filter((part) => part.length > 0).pop() ?? "project";
  try {
    return decodeURIComponent(segment);
  } catch {
    return segment;
  }
}

/** @param {Registry} candidate @param {string} name @param {string | null} exceptId */
function nameTaken(candidate, name, exceptId) {
  const normalized = name.trim().toLowerCase();
  return candidate.projects.some(
    (project) => project.id !== exceptId && project.name.trim().toLowerCase() === normalized,
  );
}

/** @param {string} id @returns {Project | null} */
function projectById(id) {
  return registry.projects.find((project) => project.id === id) ?? null;
}

/** @param {string} name @returns {Project | null} */
function projectByName(name) {
  const normalized = name.trim().toLowerCase();
  return (
    registry.projects.find((project) => project.name.trim().toLowerCase() === normalized) ?? null
  );
}

/** @param {JsonValue} value @returns {string | null} */
function idArgument(value) {
  const record = object(value);
  return record !== null && typeof record.id === "string" ? record.id : null;
}

/** @param {string} id */
function sessionKey(id) {
  return `${SESSION_PREFIX}${id}`;
}

/** @param {Session | null | undefined} session @returns {Set<CaptureId>} */
function capturesOf(session) {
  /** @type {Set<CaptureId>} */
  const captures = new Set();
  for (const window of session?.windows ?? []) {
    for (const entry of window.entries) {
      if (entry.capture !== undefined) captures.add(entry.capture);
    }
  }
  return captures;
}

/** @param {Iterable<CaptureId>} captures */
async function release(captures) {
  for (const capture of captures) {
    try {
      await recovery.release(capture);
    } catch {
      // An unreleased capture stays listed and is reconciled at activation.
    }
  }
}

/** @param {string} id @returns {Promise<Session | null>} */
async function readSession(id) {
  const result = await storage.read(sessionKey(id));
  return result.found ? sessionFromJson(result.value) : null;
}

/**
 * Durably replaces a session record, then releases the captures the previous
 * record referenced and the new one does not.
 * @param {string} id @param {Session} next @param {{ keepCaptures?: boolean }} [options]
 */
async function writeSession(id, next, options = {}) {
  const previous = sessions.get(id) ?? null;
  await storage.write(sessionKey(id), next);
  sessions.set(id, next);
  if (options.keepCaptures) return;
  const kept = capturesOf(next);
  await release([...capturesOf(previous)].filter((capture) => !kept.has(capture)));
}

/** @param {Session} left @param {Session} right */
function sameSession(left, right) {
  return JSON.stringify(left) === JSON.stringify(right);
}

/** @param {WindowInfo} window @returns {string[]} */
function windowRoots(window) {
  return window.workspace.roots.map((root) => root.uri);
}

/** Order-insensitive equality of two deduplicated root lists.
 * @param {readonly string[]} left @param {readonly string[]} right */
function sameRoots(left, right) {
  return left.length === right.length && left.every((root) => right.includes(root));
}

/** @param {readonly string[]} roots @returns {Project | null} */
function projectWithRoots(roots) {
  if (roots.length === 0) return null;
  return registry.projects.find((project) => sameRoots(project.roots, roots)) ?? null;
}

/** The first root's folder name, plus the number of further roots.
 * @param {Project} project @returns {string | null} */
function rootsLabel(project) {
  const [first, ...rest] = project.roots;
  if (first === undefined) return null;
  return rest.length === 0 ? suggestedName(first) : `${suggestedName(first)} +${rest.length}`;
}

/** @param {Project} project */
function isActive(project) {
  return sessions.get(project.id)?.state === "active";
}

/** Windows bound to the project, in listing order.
 * @param {Project} project @param {readonly WindowInfo[]} list */
function boundWindows(project, list) {
  return list.filter((window) => bindings.get(window.target) === project.id);
}

/** Windows whose roots equal the project's, in listing order.
 * @param {Project} project @param {readonly WindowInfo[]} list */
function matchingWindows(project, list) {
  return list.filter((window) => sameRoots(windowRoots(window), project.roots));
}

/**
 * Moves a project to the roots of its edited window and applies them to the
 * project's other windows. Returns the windows given the new roots.
 * @param {Project} project @param {string[]} roots @param {WindowInfo} edited
 * @param {readonly WindowInfo[]} list @returns {Promise<Set<WindowTarget>>}
 */
async function adoptRoots(project, roots, edited, list) {
  /** @type {Set<WindowTarget>} */
  const followed = new Set();
  const next = {
    ...registry,
    projects: registry.projects.map((entry) => (entry.id === project.id ? { ...entry, roots } : entry)),
  };
  try {
    await commit(next);
  } catch (error) {
    bindings.delete(edited.target);
    await report(`cannot update the folders of ${project.name}: ${describe(error)}`);
    return followed;
  }
  for (const window of boundWindows(project, list)) {
    if (window === edited) continue;
    followed.add(window.target);
    try {
      await workspaces.replaceRoots(roots, { target: window.target });
    } catch {
      // A window whose roots cannot follow leaves the project.
      bindings.delete(window.target);
    }
  }
  return followed;
}

/**
 * Brings the window bindings up to date with the listed windows. A bound
 * window whose roots changed to a set no other project uses moves its project;
 * clearing the roots or matching another project unbinds it. Unbound windows
 * bind to the active project with exactly their roots.
 * @param {readonly WindowInfo[]} list
 */
async function syncBindings(list) {
  const live = new Set(list.map((window) => window.target));
  for (const target of [...bindings.keys()]) if (!live.has(target)) bindings.delete(target);
  /** @type {Set<WindowTarget>} */
  const followed = new Set();
  for (const window of list) {
    const id = bindings.get(window.target);
    if (id === undefined || followed.has(window.target)) continue;
    const project = projectById(id);
    if (project === null || !isActive(project)) {
      bindings.delete(window.target);
      continue;
    }
    const roots = windowRoots(window);
    if (sameRoots(roots, project.roots)) continue;
    if (roots.length === 0 || projectWithRoots(roots) !== null) {
      bindings.delete(window.target);
      continue;
    }
    for (const target of await adoptRoots(project, roots, window, list)) followed.add(target);
  }
  for (const window of list) {
    if (bindings.has(window.target)) continue;
    const project = projectWithRoots(windowRoots(window));
    if (project !== null && isActive(project)) bindings.set(window.target, project.id);
  }
}

/** Distinct documents presented in windows, in tab order.
 * @param {readonly WindowInfo[]} list @returns {DocumentId[]} */
function presentedDocuments(list) {
  /** @type {DocumentId[]} */
  const ids = [];
  for (const window of list) {
    for (const pane of window.panes) {
      for (const tab of pane.tabs) {
        if (tab.document !== null && !ids.includes(tab.document)) ids.push(tab.document);
      }
    }
  }
  return ids;
}

/** @param {readonly WindowInfo[]} list @param {DocumentId} document */
function viewCount(list, document) {
  let count = 0;
  for (const window of list) {
    for (const pane of window.panes) {
      for (const tab of pane.tabs) if (tab.document === document) count += 1;
    }
  }
  return count;
}

/** @param {DocumentInfo} document */
async function isEmpty(document) {
  return (await document.buffer.snapshot()).text.length === 0;
}

/** Whether the document's text is lost when it stops being live.
 * @param {DocumentInfo} document */
async function needsCapture(document) {
  return document.dirty || (document.kind === "untitled" && !(await isEmpty(document)));
}

/**
 * Describes one window's documents. Clean files are references only; dirty
 * documents also record their identity, and captures when given.
 * @param {WindowInfo} window @param {Map<DocumentId, DocumentInfo>} byId
 * @param {Map<DocumentId, CaptureId>} captures @returns {Promise<SessionWindow>}
 */
async function describeWindow(window, byId, captures) {
  /** @type {Entry[]} */
  const entries = [];
  /** @type {DocumentId[]} */
  const listed = [];
  for (const id of presentedDocuments([window])) {
    const document = byId.get(id);
    if (document === undefined) continue;
    /** @type {Entry} */
    let entry;
    if (document.kind === "persisted" && !document.dirty && document.resourceUri !== null) {
      entry = { uri: document.resourceUri };
    } else if (document.kind === "untitled") {
      if (!document.dirty && (await isEmpty(document))) continue;
      entry = { document: id };
    } else {
      entry = document.resourceUri === null ? { document: id } : { uri: document.resourceUri, document: id };
    }
    const capture = captures.get(id);
    if (capture !== undefined) entry.capture = capture;
    entries.push(entry);
    listed.push(id);
  }
  const current = window.panes
    .flatMap((pane) => pane.tabs)
    .find((tab) => window.currentTarget !== null && tab.target === window.currentTarget);
  const selectedDocument =
    current?.document ??
    window.panes
      .map((pane) => pane.tabs.find((tab) => tab.target === pane.selected))
      .find((tab) => tab?.document != null)?.document ??
    null;
  const selected = selectedDocument === null ? -1 : listed.indexOf(selectedDocument);
  return { entries, selected: selected < 0 ? null : selected };
}

/** @param {readonly DocumentInfo[]} list */
function documentsById(list) {
  return new Map(list.map((document) => [document.id, document]));
}

/**
 * Records an active project's current membership. Losing its last window
 * suspends it with its last membership and no captures, because closure
 * already asked about dirty documents.
 * @param {Project} project @param {readonly WindowInfo[]} list @param {Map<DocumentId, DocumentInfo>} byId
 */
async function trackProject(project, list, byId) {
  const session = sessions.get(project.id);
  const bound = boundWindows(project, list);
  /** @type {Session} */
  const next =
    bound.length === 0
      ? {
          schema: SESSION_SCHEMA,
          state: "suspended",
          windows: (session?.windows ?? []).map((window) => ({
            entries: window.entries.map((entry) => {
              /** @type {Entry} */
              const reference = {};
              if (entry.uri !== undefined) reference.uri = entry.uri;
              if (entry.document !== undefined) reference.document = entry.document;
              return reference;
            }),
            selected: window.selected,
          })),
        }
      : {
          schema: SESSION_SCHEMA,
          state: "active",
          windows: await Promise.all(bound.map((window) => describeWindow(window, byId, new Map()))),
        };
  if (session === undefined || !sameSession(session, next)) await writeSession(project.id, next);
}

/** Live tracking: rewrite active records that changed. Suspended records are never written. */
async function track() {
  if (!registry.projects.some(isActive)) {
    bindings.clear();
    return;
  }
  try {
    const [list, docs] = await Promise.all([windows.list(), documents.list()]);
    await syncBindings(list);
    const byId = documentsById(docs);
    for (const project of registry.projects.filter(isActive)) await trackProject(project, list, byId);
  } catch {
    // Tracking is retried on the next change; quit stops storage first, so a
    // shutdown never records windows closing.
  }
}

/** Releases this package's captures that no session record references. */
async function reconcileCaptures() {
  /** @type {Set<CaptureId>} */
  const referenced = new Set();
  for (const session of sessions.values()) {
    for (const capture of capturesOf(session)) referenced.add(capture);
  }
  const listed = await recovery.list();
  await release(listed.map((capture) => capture.id).filter((id) => !referenced.has(id)));
}

/** @param {Project} project */
async function recordSelection(project) {
  const next = {
    ...registry,
    recentProjectId: project.id,
    projects: registry.projects.map((entry) =>
      entry.id === project.id ? { ...entry, lastOpenedAt: Date.now() } : entry,
    ),
  };
  try {
    await storage.write(RECORD_KEY, next);
    registry = next;
  } catch (error) {
    await report(`opened ${project.name}, but recording the selection failed: ${describe(error)}`);
  }
}

/** A window that can host another project: no root and no documents beyond
 * one clean, empty untitled document.
 * @param {WindowInfo} window @param {Map<DocumentId, DocumentInfo>} byId */
async function isReusable(window, byId) {
  if (window.workspace.roots.length > 0) return false;
  const presented = presentedDocuments([window]);
  if (presented.length > 1) return false;
  const document = presented.length === 1 ? byId.get(presented[0]) : undefined;
  if (document === undefined) return presented.length === 0;
  return document.kind === "untitled" && !document.dirty && (await isEmpty(document));
}

/** @param {SessionEntryOutcome} outcome @param {Entry} entry */
function problem(outcome, entry) {
  const name = entry.uri !== undefined ? suggestedName(entry.uri) : "an untitled document";
  if (outcome.kind === "missing") return `${name} no longer exists`;
  if (outcome.kind === "failed") return `${name} could not be restored (${outcome.code})`;
  return null;
}

/**
 * Switches the originating window to a project. The source project's windows
 * are suspended with their unsaved text retained before the native commit, so
 * a failed, cancelled, or interrupted switch never loses text.
 * @param {Project} project @param {WindowTarget} origin
 */
async function switchTo(project, origin) {
  if (project.roots.length === 0) {
    await report(`${project.name} has no root; rename or recreate the project`);
    return;
  }
  const initialList = await windows.list();
  await syncBindings(initialList);
  if (isActive(project)) {
    const bound = boundWindows(project, initialList);
    if (bound.length > 0) {
      await windows.focus(bound[0].target);
      await recordSelection(project);
      await refreshTree();
      return;
    }
  }
  const destination = sessions.get(project.id)?.windows ?? [];
  const targets = destination.length > 0 ? destination : [{ entries: [], selected: null }];
  // The source is fixed before the first attempt suspends its record.
  const sourceId = bindings.get(origin);
  const source = sourceId === undefined ? null : projectById(sourceId);
  for (let attempt = 1; ; attempt += 1) {
    const [list, docs] = await Promise.all([windows.list(), documents.list()]);
    const byId = documentsById(docs);
    const originWindow = list.find((window) => window.target === origin) ?? null;
    /** @type {WindowInfo[]} */
    let sources = [];
    if (source !== null && originWindow !== null && sameRoots(windowRoots(originWindow), source.roots)) {
      sources = [originWindow, ...boundWindows(source, list).filter((window) => window !== originWindow)];
    } else if (originWindow !== null && (await isReusable(originWindow, byId))) {
      sources = [originWindow];
    }
    const presented = presentedDocuments(sources);
    /** @type {Map<DocumentId, CaptureId>} */
    const captures = new Map();
    try {
      for (const id of presented) {
        const document = byId.get(id);
        if (document === undefined || viewCount(sources, id) < viewCount(list, id)) continue;
        if (await needsCapture(document)) captures.set(id, (await recovery.retain(id)).id);
      }
      if (source !== null) {
        await writeSession(source.id, {
          schema: SESSION_SCHEMA,
          state: "suspended",
          windows: await Promise.all(sources.map((window) => describeWindow(window, byId, captures))),
        });
      }
    } catch (error) {
      await release(captures.values());
      await report(`cannot switch to ${project.name}: ${describe(error)}`);
      return;
    }
    let result;
    try {
      result = await workbench.replaceWindows({
        suspend: presented.map((id) => ({ document: id, capture: captures.get(id) ?? null })),
        windows: targets.map((window) => ({
          roots: project.roots,
          entries: window.entries,
          selected: window.selected,
        })),
        release: sources.map((window) => window.target),
        focus: 0,
      });
    } catch (error) {
      if (errorCode(error) === "SessionConflict" && attempt < SWITCH_ATTEMPTS) continue;
      // The source is unchanged and still active; recording it again drops the
      // captures retained for this attempt.
      if (source !== null) {
        const [list, docs] = await Promise.all([windows.list(), documents.list()]);
        const suspended = sessions.get(source.id);
        if (suspended !== undefined) sessions.set(source.id, { ...suspended, state: "active" });
        await trackProject(source, list, documentsById(docs));
      }
      await report(`cannot switch to ${project.name}: ${describe(error)}`);
      return;
    }
    const [after, afterDocs] = await Promise.all([windows.list(), documents.list()]);
    const byIdAfter = documentsById(afterDocs);
    /** @type {Session} */
    const next = {
      schema: SESSION_SCHEMA,
      state: "active",
      windows: await Promise.all(
        matchingWindows(project, after).map((window) => describeWindow(window, byIdAfter, new Map())),
      ),
    };
    // Captures restored without durable automatic protection stay retained
    // until a later reconciliation, after automatic recovery has caught up.
    await writeSession(project.id, next, { keepCaptures: !result.protected });
    await syncBindings(after);
    const opened = result.windows[0]?.window;
    if (result.protected) {
      await reconcileCaptures();
    } else {
      await report(`${project.name} opened, but its recovered text is not yet protected`, opened);
    }
    await recordSelection(project);
    await refreshTree();
    const problems = [];
    for (const [index, window] of result.windows.entries()) {
      const entries = targets[index]?.entries ?? [];
      for (const [position, outcome] of window.entries.entries()) {
        const described = problem(outcome, entries[position] ?? {});
        if (described !== null) problems.push(described);
      }
    }
    if (problems.length > 0) await report(`Opened ${project.name}. ${problems.join("; ")}.`, opened);
    return;
  }
}

/** Marks every active session without a bound window as suspended, keeping
 * its entries. Only state is written; nothing is released. */
async function suspendUnbound() {
  const list = await windows.list();
  await syncBindings(list);
  for (const project of registry.projects) {
    const session = sessions.get(project.id);
    if (session?.state !== "active" || boundWindows(project, list).length > 0) continue;
    await writeSession(project.id, { ...session, state: "suspended" }, { keepCaptures: true });
  }
}

async function loadSessions() {
  sessions.clear();
  for (const project of registry.projects) {
    const session = await readSession(project.id);
    if (session !== null) sessions.set(project.id, session);
  }
}

await reload();
try {
  await loadSessions();
  await suspendUnbound();
  await reconcileCaptures();
} catch (error) {
  loadFailure = loadFailure ?? describe(error);
}

view = await workbench.registerView({
  id: VIEW_ID,
  title: "Projects",
  content: {
    kind: "tree",
    provider: {
      /**
       * @param {string | null} parentId
       * @param {ViewContext} context
       * @returns {Promise<TreeItem[]>}
       */
      async getChildren(parentId, context) {
        if (parentId !== null) return [];
        if (loadFailure !== null) {
          return [
            {
              id: "storage-error",
              label: "Projects storage unavailable",
              description: loadFailure,
              collapsibleState: "none",
              badge: "error",
            },
          ];
        }
        return registry.projects.map((project) => {
          const active = bindings.get(context.window) === project.id;
          return {
            id: project.id,
            label: project.name,
            description: rootsLabel(project),
            icon: "folder",
            collapsibleState: "none",
            statusText: active ? "active" : null,
            badge: active ? "success" : null,
            command: "projects.select",
            actions: [
              { label: "Rename", command: "projects.rename", arguments: { id: project.id } },
              { label: "Remove", command: "projects.remove", arguments: { id: project.id } },
            ],
          };
        });
      },
    },
  },
  placement: { location: "left", visible: false },
  actions: [
    { label: "New Project", command: "projects.create" },
    { label: "New Window", command: "projects.new-window" },
    { label: "Refresh", command: "projects.refresh" },
  ],
});

// The tool starts hidden so it never displaces the native explorer selection on
// fresh windows. A command-invoked show or hide is an explicit user arrangement,
// so the native tool-layout snapshot remembers it as the fresh-window default.
await commands.register("projects.show", async () => {
  if (view !== null) await view.show();
});

await commands.register("projects.hide", async () => {
  if (view !== null) await view.hide();
});

await commands.register("projects.create", async ({ target }) => {
  const picked = await workspaces.pickFolder();
  if (picked.kind === "dismissed") return;
  await reload();
  const existing = projectWithRoots([picked.uri]);
  if (existing !== null) {
    await report(`${existing.name} already uses this folder`);
    return;
  }
  const named = await workbench.showInput({
    title: "New project name",
    value: suggestedName(picked.uri),
  });
  if (named.kind === "dismissed") return;
  const name = named.value.trim();
  if (name.length === 0) {
    await report("a project name cannot be empty");
    return;
  }
  if (nameTaken(registry, name, null)) {
    await report(`a project named ${name} already exists`);
    return;
  }
  const now = Date.now();
  const project = {
    id: newProjectId(),
    name,
    roots: [picked.uri],
    createdAt: now,
    lastOpenedAt: now,
  };
  const next = {
    ...registry,
    projects: [...registry.projects, project],
    recentProjectId: project.id,
  };
  try {
    await commit(next);
  } catch (error) {
    await report(`cannot save project: ${describe(error)}`);
    return;
  }
  await switchTo(project, target.windowTarget);
});

await commands.register("projects.select", async ({ target }) => {
  await reload();
  if (registry.projects.length === 0) {
    await report("no projects yet; create one first");
    return;
  }
  const picked = await workbench.showPicker({
    title: "Select project",
    items: registry.projects.map((project) => ({
      id: project.id,
      label: project.name,
      description: rootsLabel(project),
    })),
  });
  if (picked.kind === "dismissed") return;
  const project = projectById(picked.id);
  if (project !== null) await switchTo(project, target.windowTarget);
});

// Row activation routes through the focused tree instance and reads the
// selected row's label, which is the unique project name.
await commands.registerForView(VIEW_ID, "projects.select", async ({ target }) => {
  await reload();
  const label = await workbench.selectedText();
  const project = label === null ? null : projectByName(label);
  if (project !== null) await switchTo(project, target.windowTarget);
});

// A window opened with the project's roots binds to it like any other window
// with those roots.
await commands.register("projects.new-window", async ({ target }) => {
  await reload();
  await syncBindings(await windows.list());
  const id = bindings.get(target.windowTarget);
  const project = id === undefined ? null : projectById(id);
  if (project === null) {
    await report("this window is not part of a project; select a project first");
    return;
  }
  try {
    await windows.open({ roots: project.roots });
  } catch (error) {
    await report(`cannot open a window for ${project.name}: ${describe(error)}`);
  }
});

await commands.register("projects.rename", async ({ arguments: args }) => {
  await reload();
  const id = idArgument(args);
  const project = id === null ? null : projectById(id);
  if (project === null) {
    await report("project not found");
    return;
  }
  const named = await workbench.showInput({ title: "Rename project", value: project.name });
  if (named.kind === "dismissed") return;
  const name = named.value.trim();
  if (name.length === 0) {
    await report("a project name cannot be empty");
    return;
  }
  if (nameTaken(registry, name, project.id)) {
    await report(`a project named ${name} already exists`);
    return;
  }
  const next = {
    ...registry,
    projects: registry.projects.map((entry) =>
      entry.id === project.id ? { ...entry, name } : entry,
    ),
  };
  try {
    await commit(next);
  } catch (error) {
    await report(`cannot rename project: ${describe(error)}`);
  }
});

await commands.register("projects.remove", async ({ arguments: args }) => {
  await reload();
  const id = idArgument(args);
  const project = id === null ? null : projectById(id);
  if (project === null) {
    await report("project not found");
    return;
  }
  const session = sessions.get(project.id) ?? null;
  const captures = capturesOf(session);
  const discarded =
    session?.state === "suspended" && captures.size > 0
      ? ` ${captures.size} unsaved ${captures.size === 1 ? "document" : "documents"} in its suspended session will be discarded.`
      : "";
  const confirmed = await workbench.showConfirmation({
    title: `Remove ${project.name}?`,
    message: `Open documents and folders stay open.${discarded}`,
    confirmLabel: "Remove",
    cancelLabel: "Cancel",
  });
  if (confirmed.kind !== "confirmed") return;
  const next = {
    ...registry,
    projects: registry.projects.filter((entry) => entry.id !== project.id),
    recentProjectId: registry.recentProjectId === project.id ? null : registry.recentProjectId,
  };
  try {
    await commit(next);
    await storage.delete(sessionKey(project.id));
  } catch (error) {
    await report(`cannot remove project: ${describe(error)}`);
    return;
  }
  // Captures are released only after both deletes are durable.
  sessions.delete(project.id);
  await release(captures);
});

await commands.register("projects.list", async () => {
  await reload();
  await refreshTree();
  // The tree is the list; showing it is the visible result.
  if (view !== null) await view.show();
  await workbench.writeClipboardText(
    JSON.stringify({
      schema: registry.schema,
      recentProjectId: registry.recentProjectId,
      failure: loadFailure,
      projects: registry.projects,
      sessions: Object.fromEntries(sessions),
    }),
  );
});

await commands.register("projects.refresh", async () => {
  await reload();
  await refreshTree();
  if (loadFailure !== null) await report(`cannot read projects: ${loadFailure}`);
});

/** Membership-relevant document metadata; revision-only changes are ignored.
 * @type {Map<string, string>} */
const tracked = new Map();
/** @param {DocumentInfo} document */
function membershipChanged(document) {
  const key = `${document.kind}|${document.resourceUri}|${document.dirty}`;
  if (tracked.get(document.id) === key) return false;
  tracked.set(document.id, key);
  return true;
}

await reload();
await workbench.observe(async (event) => {
  if (event.type === "documentChanged" && !membershipChanged(event.document)) return;
  if (event.type === "documentOpened") membershipChanged(event.document);
  if (event.type === "documentClosed") tracked.delete(event.document);
  await track();
  if (event.type === "workspaceChanged" || event.type === "windowClosed") await refreshTree();
});
