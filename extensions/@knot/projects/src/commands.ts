import { commands, storage, windows, workbench, workspaces } from "knot";
import type { JsonValue } from "knot";
import { syncBindings } from "./bindings.js";
import { object } from "./json.js";
import {
  nameTaken,
  newProjectId,
  projectById,
  projectByName,
  projectWithRoots,
  renamed,
  rootsLabel,
  suggestedName,
  withProject,
  withoutProject,
} from "./registry.js";
import { describe, report } from "./report.js";
import { capturesOf, sessionKey } from "./session.js";
import { commit, refreshTree, release, reload, type ProjectsState } from "./state.js";
import { switchTo } from "./switch.js";
import { VIEW_ID } from "./tree.js";

function idArgument(value: JsonValue): string | null {
  const record = object(value);
  return record !== null && typeof record.id === "string" ? record.id : null;
}

export async function registerCommands(state: ProjectsState) {
  // The tool starts hidden so it never displaces the native explorer selection on
  // fresh windows. A command-invoked show or hide is an explicit user arrangement,
  // so the native tool-layout snapshot remembers it as the fresh-window default.
  await commands.register("projects.show", async () => {
    if (state.view !== null) await state.view.show();
  });

  await commands.register("projects.hide", async () => {
    if (state.view !== null) await state.view.hide();
  });

  await commands.register("projects.create", async ({ target }) => {
    const picked = await workspaces.pickFolder();
    if (picked.kind === "dismissed") return;
    await reload(state);
    const existing = projectWithRoots(state.registry, [picked.uri]);
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
    if (nameTaken(state.registry, name, null)) {
      await report(`a project named ${name} already exists`);
      return;
    }
    const now = Date.now();
    const project = { id: newProjectId(), name, roots: [picked.uri], createdAt: now, lastOpenedAt: now };
    try {
      await commit(state, withProject(state.registry, project));
    } catch (error) {
      await report(`cannot save project: ${describe(error)}`);
      return;
    }
    await switchTo(state, project, target.windowTarget);
  });

  await commands.register("projects.select", async ({ target }) => {
    await reload(state);
    if (state.registry.projects.length === 0) {
      await report("no projects yet; create one first");
      return;
    }
    const picked = await workbench.showPicker({
      title: "Select project",
      items: state.registry.projects.map((project) => ({
        id: project.id,
        label: project.name,
        description: rootsLabel(project),
      })),
    });
    if (picked.kind === "dismissed") return;
    const project = projectById(state.registry, picked.id);
    if (project !== null) await switchTo(state, project, target.windowTarget);
  });

  // Row activation routes through the focused tree instance and reads the
  // selected row's label, which is the unique project name.
  await commands.registerForView(VIEW_ID, "projects.select", async ({ target }) => {
    await reload(state);
    const label = await workbench.selectedText();
    const project = label === null ? null : projectByName(state.registry, label);
    if (project !== null) await switchTo(state, project, target.windowTarget);
  });

  // A window opened with the project's roots binds to it like any other window
  // with those roots.
  await commands.register("projects.new-window", async ({ target }) => {
    await reload(state);
    await syncBindings(state, await windows.list());
    const id = state.bindings.get(target.windowTarget);
    const project = id === undefined ? null : projectById(state.registry, id);
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
    await reload(state);
    const id = idArgument(args);
    const project = id === null ? null : projectById(state.registry, id);
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
    if (nameTaken(state.registry, name, project.id)) {
      await report(`a project named ${name} already exists`);
      return;
    }
    try {
      await commit(state, renamed(state.registry, project.id, name));
    } catch (error) {
      await report(`cannot rename project: ${describe(error)}`);
    }
  });

  await commands.register("projects.remove", async ({ arguments: args }) => {
    await reload(state);
    const id = idArgument(args);
    const project = id === null ? null : projectById(state.registry, id);
    if (project === null) {
      await report("project not found");
      return;
    }
    const session = state.sessions.get(project.id) ?? null;
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
    try {
      await commit(state, withoutProject(state.registry, project.id));
      await storage.delete(sessionKey(project.id));
    } catch (error) {
      await report(`cannot remove project: ${describe(error)}`);
      return;
    }
    // Captures are released only after both deletes are durable.
    state.sessions.delete(project.id);
    await release(captures);
  });

  await commands.register("projects.list", async () => {
    await reload(state);
    await refreshTree(state);
    // The tree is the list; showing it is the visible result.
    if (state.view !== null) await state.view.show();
    await workbench.writeClipboardText(
      JSON.stringify({
        schema: state.registry.schema,
        recentProjectId: state.registry.recentProjectId,
        failure: state.loadFailure,
        projects: state.registry.projects,
        sessions: Object.fromEntries(state.sessions),
      }),
    );
  });

  await commands.register("projects.refresh", async () => {
    await reload(state);
    await refreshTree(state);
    if (state.loadFailure !== null) await report(`cannot read projects: ${state.loadFailure}`);
  });
}
