// The project registry record: one application-global storage record holding
// every project's stable ID, name, and roots, plus the recent selection.
import type { JsonValue } from "knot";
import { object } from "./json.js";

export const RECORD_KEY = "projects";
export const SCHEMA = 1;

export type Project = {
  id: string;
  name: string;
  roots: string[];
  createdAt: number | null;
  lastOpenedAt: number | null;
};

export type Registry = {
  schema: number;
  projects: Project[];
  recentProjectId: string | null;
};

export function emptyRegistry(): Registry {
  return { schema: SCHEMA, projects: [], recentProjectId: null };
}

function projectFromJson(value: JsonValue): Project | null {
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

/** Parses a stored registry record, throwing on any unsupported shape. */
export function registryFromJson(value: JsonValue): Registry {
  const record = object(value);
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
    recentProjectId: typeof record.recentProjectId === "string" ? record.recentProjectId : null,
  };
}

export function newProjectId() {
  return `p-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}

/** The decoded last path segment of a URI. */
export function suggestedName(uri: string): string {
  const segment = uri.split("/").filter((part) => part.length > 0).pop() ?? "project";
  try {
    return decodeURIComponent(segment);
  } catch {
    return segment;
  }
}

/** Names compare trimmed and case-insensitively. */
export function nameTaken(registry: Registry, name: string, exceptId: string | null) {
  const normalized = name.trim().toLowerCase();
  return registry.projects.some(
    (project) => project.id !== exceptId && project.name.trim().toLowerCase() === normalized,
  );
}

export function projectById(registry: Registry, id: string): Project | null {
  return registry.projects.find((project) => project.id === id) ?? null;
}

export function projectByName(registry: Registry, name: string): Project | null {
  const normalized = name.trim().toLowerCase();
  return registry.projects.find((project) => project.name.trim().toLowerCase() === normalized) ?? null;
}

/** Order-insensitive equality of two deduplicated root lists. */
export function sameRoots(left: readonly string[], right: readonly string[]) {
  return left.length === right.length && left.every((root) => right.includes(root));
}

export function projectWithRoots(registry: Registry, roots: readonly string[]): Project | null {
  if (roots.length === 0) return null;
  return registry.projects.find((project) => sameRoots(project.roots, roots)) ?? null;
}

/** The first root's folder name, plus the number of further roots. */
export function rootsLabel(project: Project): string | null {
  const [first, ...rest] = project.roots;
  if (first === undefined) return null;
  return rest.length === 0 ? suggestedName(first) : `${suggestedName(first)} +${rest.length}`;
}

function updated(registry: Registry, id: string, change: (project: Project) => Project): Registry {
  return {
    ...registry,
    projects: registry.projects.map((project) => (project.id === id ? change(project) : project)),
  };
}

/** Adds a project and makes it the recent selection. */
export function withProject(registry: Registry, project: Project): Registry {
  return { ...registry, projects: [...registry.projects, project], recentProjectId: project.id };
}

export function withoutProject(registry: Registry, id: string): Registry {
  return {
    ...registry,
    projects: registry.projects.filter((project) => project.id !== id),
    recentProjectId: registry.recentProjectId === id ? null : registry.recentProjectId,
  };
}

export function renamed(registry: Registry, id: string, name: string): Registry {
  return updated(registry, id, (project) => ({ ...project, name }));
}

export function withRoots(registry: Registry, id: string, roots: string[]): Registry {
  return updated(registry, id, (project) => ({ ...project, roots }));
}

/** Records a project as the recent selection, opened at `now`. */
export function selected(registry: Registry, id: string, now: number): Registry {
  return { ...updated(registry, id, (project) => ({ ...project, lastOpenedAt: now })), recentProjectId: id };
}
