// Bundled project registry. Each project has a stable identity, a name, and an
// ordered roots array; this checkpoint records one root per project. Records are
// application-global extension storage and the presentation is a contributed
// tree. Root selection is delegated to the application's workspace operations,
// so this package needs no private host access.
import { commands, storage, workbench, workspaces } from "knot";

const RECORD_KEY = "projects";
const SCHEMA = 1;
const VIEW_ID = "builtin.projects";

/** @typedef {import("knot").JsonValue} JsonValue */
/** @typedef {import("knot").TreeItem} TreeItem */
/** @typedef {import("knot").ViewContext} ViewContext */
/** @typedef {import("knot").ViewRegistration} ViewRegistration */
/** @typedef {{ id: string, name: string, roots: string[], createdAt: number | null, lastOpenedAt: number | null }} Project */
/** @typedef {{ schema: number, projects: Project[], recentProjectId: string | null }} Registry */

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

/** @param {JsonValue} value @returns {Project | null} */
function projectFromJson(value) {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return null;
  const record = /** @type {Record<string, JsonValue>} */ (value);
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
  if (result.value === null || typeof result.value !== "object" || Array.isArray(result.value)) {
    throw new Error("stored projects record is not an object");
  }
  const record = /** @type {Record<string, JsonValue>} */ (result.value);
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

/** @type {Registry} */
let registry = emptyRegistry();
/** @type {string | null} */
let loadFailure = null;
/** @type {ViewRegistration | null} */
let view = null;

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

/** @param {string} message */
async function report(message) {
  try {
    await workbench.showMessage({ title: "Projects", message });
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
  if (value === null || typeof value !== "object" || Array.isArray(value)) return null;
  const id = /** @type {Record<string, JsonValue>} */ (value).id;
  return typeof id === "string" ? id : null;
}

/**
 * Opens a project's root in the originating window and records the selection.
 * @param {Project} project
 */
async function openProject(project) {
  const root = project.roots[0];
  if (root === undefined) {
    await report(`${project.name} has no root; rename or recreate the project`);
    return;
  }
  try {
    await workspaces.openFolder(root);
  } catch (error) {
    await report(`cannot open ${project.name}: ${describe(error)}`);
    return;
  }
  const next = {
    schema: SCHEMA,
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
  await refreshTree();
}

await reload();

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
        const workspace = await workspaces.get(context.window);
        const activeRoot = workspace === null ? null : (workspace.roots[0]?.uri ?? null);
        return registry.projects.map((project) => {
          const root = project.roots[0] ?? null;
          const active = root !== null && root === activeRoot;
          return {
            id: project.id,
            label: project.name,
            description: root,
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
    { label: "Refresh", command: "projects.refresh" },
  ],
});

// The tool starts hidden so it never displaces the native explorer selection on
// fresh windows; the layout remembers it once the user shows it.
await commands.register("projects.show", async () => {
  if (view !== null) await view.show();
});

await commands.register("projects.hide", async () => {
  if (view !== null) await view.hide();
});

await commands.register("projects.create", async () => {
  const picked = await workspaces.pickFolder();
  if (picked.kind === "dismissed") return;
  await reload();
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
    schema: SCHEMA,
    projects: [...registry.projects, project],
    recentProjectId: project.id,
  };
  try {
    await commit(next);
  } catch (error) {
    await report(`cannot save project: ${describe(error)}`);
    return;
  }
  await openProject(project);
});

await commands.register("projects.select", async () => {
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
      description: project.roots[0] ?? null,
    })),
  });
  if (picked.kind === "dismissed") return;
  const project = projectById(picked.id);
  if (project !== null) await openProject(project);
});

// Row activation routes through the focused tree instance and reads the
// selected row's label, which is the unique project name.
await commands.registerForView(VIEW_ID, "projects.select", async () => {
  await reload();
  const label = await workbench.selectedText();
  const project = label === null ? null : projectByName(label);
  if (project !== null) await openProject(project);
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
    schema: SCHEMA,
    recentProjectId: registry.recentProjectId,
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
  const confirmed = await workbench.showConfirmation({
    title: `Remove ${project.name}?`,
    message: "Open documents and folders stay open.",
    confirmLabel: "Remove",
    cancelLabel: "Cancel",
  });
  if (confirmed.kind !== "confirmed") return;
  const next = {
    schema: SCHEMA,
    projects: registry.projects.filter((entry) => entry.id !== project.id),
    recentProjectId: registry.recentProjectId === project.id ? null : registry.recentProjectId,
  };
  try {
    await commit(next);
  } catch (error) {
    await report(`cannot remove project: ${describe(error)}`);
  }
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
    }),
  );
});

await commands.register("projects.refresh", async () => {
  await reload();
  await refreshTree();
  if (loadFailure !== null) await report(`cannot read projects: ${loadFailure}`);
});

await reload();
await workbench.observe((event) => {
  if (event.type === "workspaceChanged") return refreshTree();
  return undefined;
});
