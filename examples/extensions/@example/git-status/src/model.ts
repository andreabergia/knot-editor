// Fixture source-control state and the tree it presents. Everything here is
// plain data, so the extension's behavior can be tested without a host.
import type { JsonValue, TreeAction, TreeBadge, TreeItem } from "knot";

export type Status = "modified" | "added" | "deleted" | "renamed" | "untracked" | "conflicted";
export type Entry = { path: string; status: Status };
export type Repository = { id: string; name: string; path: string };

export type Model = {
  staged: Entry[];
  changed: Entry[];
  repositories: Repository[];
  activeRepository: string | null;
  log: string[];
};

const STATUS_LETTERS: Record<Status, string> = {
  modified: "M",
  added: "A",
  deleted: "D",
  renamed: "R",
  untracked: "U",
  conflicted: "!",
};

const STATUS_BADGES: Record<Status, TreeBadge> = {
  modified: "warning",
  added: "success",
  deleted: "error",
  renamed: "info",
  untracked: "neutral",
  conflicted: "error",
};

export function initialModel(): Model {
  return {
    staged: [
      { path: "src/main.rs", status: "modified" },
      { path: "src/app/tree_view.rs", status: "added" },
    ],
    changed: [
      { path: "docs/architecture/semantic-ui.md", status: "modified" },
      { path: "notes.txt", status: "untracked" },
    ],
    repositories: [
      { id: "knot", name: "knot", path: "/src/knot-editor" },
      { id: "site", name: "website", path: "/src/site" },
    ],
    activeRepository: "knot",
    log: ["$ git status --short"],
  };
}

/** `git status --short` lines for every staged and changed entry. */
export function summary(model: Model): string[] {
  return [...model.staged, ...model.changed].map(
    (entry) => `${STATUS_LETTERS[entry.status]}  ${entry.path}`,
  );
}

/** Moves one entry between the staged and changed lists. */
export function move(model: Model, path: string, from: "staged" | "changed"): boolean {
  const source = model[from];
  const index = source.findIndex((entry) => entry.path === path);
  const entry = source[index];
  if (entry === undefined) return false;
  source.splice(index, 1);
  model[from === "staged" ? "changed" : "staged"].push(entry);
  return true;
}

export function discard(model: Model, path: string): boolean {
  const before = model.changed.length;
  model.changed = model.changed.filter((entry) => entry.path !== path);
  return model.changed.length !== before;
}

export function moveAll(model: Model, from: "staged" | "changed"): boolean {
  if (model[from].length === 0) return false;
  if (from === "changed") {
    model.staged = model.staged.concat(model.changed);
    model.changed = [];
  } else {
    model.changed = model.changed.concat(model.staged);
    model.staged = [];
  }
  return true;
}

/** Adds and selects a repository; returns its ID, or null for a blank path. */
export function addRepository(model: Model, input: string): string | null {
  const path = input.trim();
  if (path.length === 0) return null;
  const segments = path.split("/").filter((segment) => segment.length > 0);
  const id = `repo-${model.repositories.length + 1}`;
  model.repositories.push({ id, name: segments.at(-1) ?? path, path });
  model.activeRepository = id;
  return id;
}

export function removeRepository(model: Model, id: string) {
  model.repositories = model.repositories.filter((repository) => repository.id !== id);
  if (model.activeRepository === id) model.activeRepository = model.repositories[0]?.id ?? null;
}

export function repositoryById(model: Model, id: string): Repository | null {
  return model.repositories.find((repository) => repository.id === id) ?? null;
}

/** Reads a string field from command arguments shaped `{ [field]: string }`. */
export function stringArgument(value: JsonValue, field: string): string | null {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return null;
  const fieldValue = (value as { readonly [key: string]: JsonValue })[field];
  return typeof fieldValue === "string" ? fieldValue : null;
}

function entryRow(entry: Entry, actions: TreeAction[]): TreeItem {
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

function group(id: string, label: string, description: string, action: TreeAction): TreeItem {
  return {
    id,
    label,
    description,
    icon: null,
    collapsibleState: "none",
    kind: "group",
    statusText: null,
    badge: null,
    actions: [action],
  };
}

export function children(model: Model, parentId: string | null): TreeItem[] {
  if (parentId === null) {
    return [
      group("staged", "Staged", `${model.staged.length} file(s)`, {
        label: "Unstage all",
        command: "example.git.unstage-all",
      }),
      group("changes", "Changes", `${model.changed.length} file(s)`, {
        label: "Stage all",
        command: "example.git.stage-all",
      }),
      group("repositories", "Repositories", `${model.repositories.length} tracked`, {
        label: "Add",
        command: "example.git.add-repository",
      }),
    ];
  }
  if (parentId === "repositories") {
    return model.repositories.map((repository) => {
      const current = repository.id === model.activeRepository;
      return {
        id: `repository:${repository.id}`,
        label: repository.name,
        description: current ? `${repository.path} (current)` : repository.path,
        icon: "folder",
        collapsibleState: "none",
        kind: "item",
        statusText: current ? "current" : null,
        badge: current ? "info" : null,
        command: null,
        actions: [
          { label: "Rename", command: "example.git.rename-repository", arguments: { id: repository.id } },
          { label: "Remove", command: "example.git.remove-repository", arguments: { id: repository.id } },
        ],
      };
    });
  }
  if (parentId === "staged") {
    return model.staged.map((entry) =>
      entryRow(entry, [
        { label: "Unstage", command: "example.git.unstage", arguments: { path: entry.path } },
      ]),
    );
  }
  if (parentId === "changes") {
    return model.changed.map((entry) =>
      entryRow(entry, [
        { label: "Stage", command: "example.git.stage", arguments: { path: entry.path } },
        { label: "Discard", command: "example.git.discard", arguments: { path: entry.path } },
      ]),
    );
  }
  return [];
}
