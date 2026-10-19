// A Git-status-style contributed frame backed by fixture data. This package
// demonstrates grouped lists, status badges, row/group/frame actions, file
// activation, a read-only log frame, and picker/input/confirmation flows
// through the public API only. It does not read or write a Git repository and
// keeps tracked repositories in memory.
import { commands, workbench } from "knot";

/** @typedef {"modified" | "added" | "deleted" | "renamed" | "untracked" | "conflicted"} Status */
/** @typedef {{ path: string, status: Status }} Entry */
/** @typedef {{ id: string, name: string, path: string }} Repository */
/** @typedef {import("knot").TreeItem} TreeItem */
/** @typedef {import("knot").TreeAction} TreeAction */
/** @typedef {import("knot").JsonValue} JsonValue */

/** @type {Record<Status, string>} */
const STATUS_LETTERS = {
  modified: "M",
  added: "A",
  deleted: "D",
  renamed: "R",
  untracked: "U",
  conflicted: "!",
};

/** @type {Record<Status, import("knot").TreeBadge>} */
const STATUS_BADGES = {
  modified: "warning",
  added: "success",
  deleted: "error",
  renamed: "info",
  untracked: "neutral",
  conflicted: "error",
};

/** @type {Entry[]} */
let staged = [
  { path: "src/main.rs", status: "modified" },
  { path: "src/app/tree_view.rs", status: "added" },
];
/** @type {Entry[]} */
let changed = [
  { path: "docs/architecture/semantic-ui.md", status: "modified" },
  { path: "notes.txt", status: "untracked" },
];
/** @type {Repository[]} */
let repositories = [
  { id: "knot", name: "knot", path: "/src/knot-editor" },
  { id: "site", name: "website", path: "/src/site" },
];
/** @type {string | null} */
let activeRepository = "knot";
/** @type {string[]} */
let logLines = ["$ git status --short"];

/**
 * @param {Entry} entry
 * @param {TreeAction[]} actions
 * @returns {TreeItem}
 */
function entryRow(entry, actions) {
  return {
    id: entry.path,
    label: entry.path,
    description: null,
    icon: "file",
    collapsibleState: "none",
    kind: "item",
    statusText: STATUS_LETTERS[entry.status],
    badge: STATUS_BADGES[entry.status],
    command: "example.git.activate",
    actions,
  };
}

/**
 * @param {string} id
 * @param {string} label
 * @param {Entry[]} entries
 * @param {TreeAction} action
 * @returns {TreeItem}
 */
function group(id, label, entries, action) {
  return {
    id,
    label,
    description: `${entries.length} file(s)`,
    icon: null,
    collapsibleState: "none",
    kind: "group",
    statusText: null,
    badge: null,
    actions: [action],
  };
}

const tree = await workbench.registerView({
  id: "example.git-status",
  title: "Source Control",
  content: {
    kind: "tree",
    provider: {
      getChildren(parentId) {
        if (parentId === null) {
          return [
            group("staged", "Staged", staged, {
              label: "Unstage all",
              command: "example.git.unstage-all",
            }),
            group("changes", "Changes", changed, {
              label: "Stage all",
              command: "example.git.stage-all",
            }),
            {
              id: "repositories",
              label: "Repositories",
              description: `${repositories.length} tracked`,
              icon: null,
              collapsibleState: "none",
              kind: "group",
              statusText: null,
              badge: null,
              actions: [{ label: "Add", command: "example.git.add-repository" }],
            },
          ];
        }
        if (parentId === "repositories") {
          return repositories.map((repository) => ({
            id: `repository:${repository.id}`,
            label: repository.name,
            description: repository.id === activeRepository
              ? `${repository.path} (current)`
              : repository.path,
            icon: "folder",
            collapsibleState: "none",
            kind: "item",
            statusText: repository.id === activeRepository ? "current" : null,
            badge: repository.id === activeRepository ? "info" : null,
            command: null,
            actions: [
              {
                label: "Rename",
                command: "example.git.rename-repository",
                arguments: { id: repository.id },
              },
              {
                label: "Remove",
                command: "example.git.remove-repository",
                arguments: { id: repository.id },
              },
            ],
          }));
        }
        if (parentId === "staged") {
          return staged.map((entry) => entryRow(entry, [{
            label: "Unstage",
            command: "example.git.unstage",
            arguments: { path: entry.path },
          }]));
        }
        if (parentId === "changes") {
          return changed.map((entry) => entryRow(entry, [
            {
              label: "Stage",
              command: "example.git.stage",
              arguments: { path: entry.path },
            },
            {
              label: "Discard",
              command: "example.git.discard",
              arguments: { path: entry.path },
            },
          ]));
        }
        return [];
      },
    },
  },
  actions: [
    { label: "Refresh", command: "example.git.refresh", arguments: { source: "frame" } },
    { label: "Stage all", command: "example.git.stage-all", arguments: { source: "frame" } },
    { label: "Switch repository", command: "example.git.select-repository" },
  ],
});

const log = await workbench.registerView({
  id: "example.git-log",
  title: "Source Control Log",
  content: { kind: "text" },
  placement: { location: "bottom" },
});

function summary() {
  return [
    ...staged.map((entry) => `${STATUS_LETTERS[entry.status]}  ${entry.path}`),
    ...changed.map((entry) => `${STATUS_LETTERS[entry.status]}  ${entry.path}`),
  ];
}

async function publish() {
  logLines = ["$ git status --short", ...summary()];
  await log.setText(`${logLines.join("\n")}\n`);
  await tree.invalidate();
}

/**
 * @param {Entry[]} list
 * @param {string} path
 * @returns {number}
 */
function find(list, path) {
  return list.findIndex((entry) => entry.path === path);
}

/**
 * @param {string} path
 * @param {Entry[]} from
 * @param {Entry[]} to
 * @returns {boolean}
 */
function move(path, from, to) {
  const index = find(from, path);
  if (index === -1) return false;
  const entry = from[index];
  if (entry === undefined) return false;
  from.splice(index, 1);
  to.push(entry);
  return true;
}

/**
 * @param {JsonValue} value
 * @returns {string | null}
 */
function pathArgument(value) {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return null;
  const record = /** @type {{ readonly [key: string]: JsonValue }} */ (value);
  const path = record["path"];
  return typeof path === "string" ? path : null;
}

/**
 * @param {JsonValue} value
 * @returns {string | null}
 */
function idArgument(value) {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return null;
  const record = /** @type {{ readonly [key: string]: JsonValue }} */ (value);
  const id = record["id"];
  return typeof id === "string" ? id : null;
}

/**
 * @param {string} id
 * @returns {Repository | null}
 */
function repositoryById(id) {
  return repositories.find((repository) => repository.id === id) ?? null;
}

await commands.register("example.git.refresh", async () => {
  await publish();
});

// An awaited picker inside a command: the extension turn yields while the
// native overlay owns the window, and resumes with a typed result.
await commands.register("example.git.select-repository", async () => {
  const result = await workbench.showPicker({
    title: "Switch repository",
    items: repositories.map((repository) => ({
      id: repository.id,
      label: repository.name,
      description: repository.path,
    })),
  });
  if (result.kind !== "selected") return;
  activeRepository = result.id;
  logLines.push(`repository ${result.id}`);
  await publish();
});

await commands.register("example.git.add-repository", async () => {
  const result = await workbench.showInput({ title: "Add repository path", value: "" });
  if (result.kind !== "accepted") return;
  const path = result.value.trim();
  if (path.length === 0) return;
  const segments = path.split("/").filter((segment) => segment.length > 0);
  const id = `repo-${repositories.length + 1}`;
  repositories.push({ id, name: segments.at(-1) ?? path, path });
  activeRepository = id;
  logLines.push(`add ${path}`);
  await publish();
});

await commands.register("example.git.rename-repository", async ({ arguments: args }) => {
  const id = idArgument(args);
  if (id === null) return;
  const repository = repositoryById(id);
  if (repository === null) return;
  const result = await workbench.showInput({
    title: "Rename repository",
    value: repository.name,
  });
  if (result.kind !== "accepted") return;
  const name = result.value.trim();
  if (name.length === 0) return;
  repository.name = name;
  logLines.push(`rename ${id} ${name}`);
  await publish();
});

await commands.register("example.git.remove-repository", async ({ arguments: args }) => {
  const id = idArgument(args);
  if (id === null) return;
  const repository = repositoryById(id);
  if (repository === null) return;
  const result = await workbench.showConfirmation({
    title: `Remove ${repository.name}?`,
    message: repository.path,
    confirmLabel: "Remove",
    cancelLabel: "Cancel",
  });
  if (result.kind !== "confirmed") return;
  repositories = repositories.filter((candidate) => candidate.id !== id);
  if (activeRepository === id) activeRepository = repositories[0]?.id ?? null;
  logLines.push(`remove ${id}`);
  await publish();
});

// The global definition owns the command name. Selected-item access belongs to
// a view-claimed handler, which is the one that runs for frame activation.
await commands.register("example.git.activate", async () => {});
await commands.registerForView("example.git-status", "example.git.activate", async () => {
  const selected = await workbench.selectedText();
  if (selected !== null) {
    await workbench.writeClipboardText(selected);
    logLines.push(`open ${selected}`);
    await log.setText(`${logLines.join("\n")}\n`);
  }
});

await commands.register("example.git.stage", async ({ arguments: args }) => {
  const path = pathArgument(args);
  if (path !== null && await move(path, changed, staged)) await publish();
});

await commands.register("example.git.unstage", async ({ arguments: args }) => {
  const path = pathArgument(args);
  if (path !== null && await move(path, staged, changed)) await publish();
});

await commands.register("example.git.discard", async ({ arguments: args }) => {
  const path = pathArgument(args);
  if (path === null) return;
  const index = find(changed, path);
  if (index !== -1) {
    changed.splice(index, 1);
    await publish();
  }
});

await commands.register("example.git.stage-all", async () => {
  if (changed.length > 0) {
    staged = staged.concat(changed);
    changed = [];
    await publish();
  }
});

await commands.register("example.git.unstage-all", async () => {
  if (staged.length > 0) {
    changed = changed.concat(staged);
    staged = [];
    await publish();
  }
});

await publish();
