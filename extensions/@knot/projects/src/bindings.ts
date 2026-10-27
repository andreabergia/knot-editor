// Window bindings. A window binds to a project when its roots equal the
// project's roots, in any order, and the project's session is active. Bindings
// live in memory and are rederived from root sets, never persisted.
import { workspaces } from "knot";
import type { WindowInfo, WindowTarget } from "knot";
import { projectById, projectWithRoots, sameRoots, withRoots, type Project, type Registry } from "./registry.js";
import { describe, report } from "./report.js";
import { boundWindows, commit, isActive, type ProjectsState } from "./state.js";
import { windowRoots } from "./layout.js";

/**
 * What happens to a window bound to `project` whose roots are now `roots`: it
 * stays bound, the project adopts a root set no other project uses, or the
 * window unbinds because its project is gone or suspended, its roots were
 * cleared, or it now matches another project.
 */
export function bindingChange(
  registry: Registry,
  project: Project | null,
  active: boolean,
  roots: readonly string[],
): "keep" | "adopt" | "unbind" {
  if (project === null || !active) return "unbind";
  if (sameRoots(roots, project.roots)) return "keep";
  if (roots.length === 0 || projectWithRoots(registry, roots) !== null) return "unbind";
  return "adopt";
}

/**
 * Moves a project to the roots of its edited window and applies them to the
 * project's other windows. Returns the windows given the new roots.
 */
async function adoptRoots(
  state: ProjectsState,
  project: Project,
  roots: string[],
  edited: WindowInfo,
  list: readonly WindowInfo[],
): Promise<Set<WindowTarget>> {
  const followed = new Set<WindowTarget>();
  try {
    await commit(state, withRoots(state.registry, project.id, roots));
  } catch (error) {
    state.bindings.delete(edited.target);
    await report(`cannot update the folders of ${project.name}: ${describe(error)}`);
    return followed;
  }
  for (const window of boundWindows(state, project, list)) {
    if (window === edited) continue;
    followed.add(window.target);
    try {
      await workspaces.replaceRoots(roots, { target: window.target });
    } catch {
      // A window whose roots cannot follow leaves the project.
      state.bindings.delete(window.target);
    }
  }
  return followed;
}

/**
 * Brings the window bindings up to date with the listed windows: bound windows
 * keep, move, or leave their project, and unbound windows bind to the active
 * project with exactly their roots.
 */
export async function syncBindings(state: ProjectsState, list: readonly WindowInfo[]) {
  const live = new Set(list.map((window) => window.target));
  for (const target of [...state.bindings.keys()]) if (!live.has(target)) state.bindings.delete(target);
  const followed = new Set<WindowTarget>();
  for (const window of list) {
    const id = state.bindings.get(window.target);
    if (id === undefined || followed.has(window.target)) continue;
    const project = projectById(state.registry, id);
    const roots = windowRoots(window);
    const change = bindingChange(
      state.registry,
      project,
      project !== null && isActive(state, project),
      roots,
    );
    if (change === "unbind") state.bindings.delete(window.target);
    if (change !== "adopt" || project === null) continue;
    for (const target of await adoptRoots(state, project, roots, window, list)) followed.add(target);
  }
  for (const window of list) {
    if (state.bindings.has(window.target)) continue;
    const project = projectWithRoots(state.registry, windowRoots(window));
    if (project !== null && isActive(state, project)) state.bindings.set(window.target, project.id);
  }
}
