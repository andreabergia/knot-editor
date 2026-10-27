// The Projects tool: a contributed tree listing every project.
import { workbench } from "knot";
import type { TreeItem, WindowTarget } from "knot";
import { rootsLabel } from "./registry.js";
import type { ProjectsState } from "./state.js";

export const VIEW_ID = "builtin.projects";

/** The tree rows for one window: every project, marking the window's own. */
export function projectItems(state: ProjectsState, window: WindowTarget): TreeItem[] {
  if (state.loadFailure !== null) {
    return [
      {
        id: "storage-error",
        label: "Projects storage unavailable",
        description: state.loadFailure,
        collapsibleState: "none",
        badge: "error",
      },
    ];
  }
  return state.registry.projects.map((project) => {
    const active = state.bindings.get(window) === project.id;
    const broken = state.unreadable.has(project.id);
    return {
      id: project.id,
      label: project.name,
      description: broken ? "session record unreadable" : rootsLabel(project),
      icon: "folder",
      collapsibleState: "none",
      statusText: broken ? "unreadable" : active ? "active" : null,
      badge: broken ? "error" : active ? "success" : null,
      command: "projects.select",
      actions: [
        { label: "Rename", command: "projects.rename", arguments: { id: project.id } },
        { label: "Remove", command: "projects.remove", arguments: { id: project.id } },
      ],
    };
  });
}

export async function registerTree(state: ProjectsState) {
  state.view = await workbench.registerView({
    id: VIEW_ID,
    title: "Projects",
    content: {
      kind: "tree",
      provider: {
        async getChildren(parentId, context) {
          return parentId === null ? projectItems(state, context.window) : [];
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
}
