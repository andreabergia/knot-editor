// A Git-status-style contributed frame backed by fixture data. This package
// demonstrates grouped lists, status badges, row/group/frame actions, file
// activation, a read-only log frame, and picker/input/confirmation flows
// through the public API only. It does not read or write a Git repository and
// keeps tracked repositories in memory.
import { commands, workbench } from "knot";
import {
  addRepository,
  children,
  discard,
  initialModel,
  move,
  moveAll,
  removeRepository,
  repositoryById,
  stringArgument,
  summary,
} from "./model.js";

export async function start() {
  const model = initialModel();

  const tree = await workbench.registerView({
    id: "example.git-status",
    title: "Source Control",
    content: {
      kind: "tree",
      provider: { getChildren: (parentId) => children(model, parentId) },
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

  async function writeLog() {
    await log.setText(`${model.log.join("\n")}\n`);
  }

  async function publish() {
    model.log = ["$ git status --short", ...summary(model)];
    await writeLog();
    await tree.invalidate();
  }

  await commands.register("example.git.refresh", publish);

  // An awaited picker inside a command: the extension turn yields while the
  // native overlay owns the window, and resumes with a typed result.
  await commands.register("example.git.select-repository", async () => {
    const result = await workbench.showPicker({
      title: "Switch repository",
      items: model.repositories.map((repository) => ({
        id: repository.id,
        label: repository.name,
        description: repository.path,
      })),
    });
    if (result.kind !== "selected") return;
    model.activeRepository = result.id;
    model.log.push(`repository ${result.id}`);
    await publish();
  });

  await commands.register("example.git.add-repository", async () => {
    const result = await workbench.showInput({ title: "Add repository path", value: "" });
    if (result.kind !== "accepted") return;
    if (addRepository(model, result.value) === null) return;
    model.log.push(`add ${result.value.trim()}`);
    await publish();
  });

  await commands.register("example.git.rename-repository", async ({ arguments: args }) => {
    const id = stringArgument(args, "id");
    const repository = id === null ? null : repositoryById(model, id);
    if (repository === null) return;
    const result = await workbench.showInput({ title: "Rename repository", value: repository.name });
    if (result.kind !== "accepted") return;
    const name = result.value.trim();
    if (name.length === 0) return;
    repository.name = name;
    model.log.push(`rename ${repository.id} ${name}`);
    await publish();
  });

  await commands.register("example.git.remove-repository", async ({ arguments: args }) => {
    const id = stringArgument(args, "id");
    const repository = id === null ? null : repositoryById(model, id);
    if (repository === null) return;
    const result = await workbench.showConfirmation({
      title: `Remove ${repository.name}?`,
      message: repository.path,
      confirmLabel: "Remove",
      cancelLabel: "Cancel",
    });
    if (result.kind !== "confirmed") return;
    removeRepository(model, repository.id);
    model.log.push(`remove ${repository.id}`);
    await publish();
  });

  // The global definition owns the command name. Selected-item access belongs to
  // a view-claimed handler, which is the one that runs for frame activation.
  await commands.register("example.git.activate", async () => {});
  await commands.registerForView("example.git-status", "example.git.activate", async () => {
    const selected = await workbench.selectedText();
    if (selected === null) return;
    await workbench.writeClipboardText(selected);
    model.log.push(`open ${selected}`);
    await writeLog();
  });

  await commands.register("example.git.stage", async ({ arguments: args }) => {
    const path = stringArgument(args, "path");
    if (path !== null && move(model, path, "changed")) await publish();
  });

  await commands.register("example.git.unstage", async ({ arguments: args }) => {
    const path = stringArgument(args, "path");
    if (path !== null && move(model, path, "staged")) await publish();
  });

  await commands.register("example.git.discard", async ({ arguments: args }) => {
    const path = stringArgument(args, "path");
    if (path !== null && discard(model, path)) await publish();
  });

  await commands.register("example.git.stage-all", async () => {
    if (moveAll(model, "changed")) await publish();
  });

  await commands.register("example.git.unstage-all", async () => {
    if (moveAll(model, "staged")) await publish();
  });

  await publish();
}
