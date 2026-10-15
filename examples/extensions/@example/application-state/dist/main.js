import { commands, documents, windows, workbench, workspaces } from "knot";

/** @type {{ sequence: number, type: string, document: string | null, documentCount: number }[]} */
const events = [];
const observation = await workbench.observe(async (event) => {
  events.push({ sequence: event.sequence, type: event.type,
    document: "document" in event
      ? (typeof event.document === "string" ? event.document : event.document.id) : null,
    documentCount: (await documents.list()).length });
});

await commands.register("example.application-state", async () => {
  const currentDocuments = await documents.list();
  const currentWindows = await windows.list();
  await workbench.writeClipboardText(JSON.stringify({
    initial: { documents: observation.snapshot.documents.length,
      windows: observation.snapshot.windows.length, watermark: observation.watermark },
    documents: currentDocuments.map(document => ({ id: document.id, title: document.title,
      kind: document.kind, resourceUri: document.resourceUri,
      dirty: document.dirty, revision: document.revision })),
    windows: currentWindows.map(window => ({ roots: window.workspace.roots,
      generation: window.workspace.generation, panes: window.panes.map(pane => ({
        tabs: pane.tabs.map(tab => ({ kind: tab.kind, document: tab.document,
          selected: pane.selected === tab.target, focused: window.currentTarget === tab.target })),
      })) })),
    events,
  }, null, 2));
});
await commands.register("example.stop-application-observer", async () => {
  await observation.dispose();
});

await commands.register("example.open-document", async ({ arguments: args }) => {
  let uri;
  if (args !== null && typeof args === "object" && !Array.isArray(args) && "uri" in args && typeof args.uri === "string") {
    uri = args.uri;
  } else if (args === null) {
    uri = (await documents.list()).find(document => document.resourceUri !== null)?.resourceUri;
  }
  if (typeof uri !== "string") return commands.invalidArguments("Open a file first, or pass { uri: anAbsoluteResourceUri }.");
  const result = await documents.open(uri);
  await workbench.writeClipboardText(JSON.stringify({ document: result.document.id,
    title: result.document.title, resourceUri: result.document.resourceUri,
    kind: result.document.kind, dirty: result.document.dirty }, null, 2));
});

await commands.register("example.open-folder", async ({ arguments: args }) => {
  let uri;
  if (args !== null && typeof args === "object" && !Array.isArray(args) && "uri" in args && typeof args.uri === "string") {
    uri = args.uri;
  } else if (args === null) {
    const resource = (await documents.list()).find(document => document.resourceUri !== null)?.resourceUri;
    if (resource) uri = resource.slice(0, resource.lastIndexOf("/") + 1);
  }
  if (typeof uri !== "string") return commands.invalidArguments("Open a file first, or pass { uri: anAbsoluteDirectoryUri }.");
  const result = await workspaces.openFolder(uri);
  await workbench.writeClipboardText(JSON.stringify({ roots: result.roots, generation: result.generation }, null, 2));
});
await commands.register("example.clear-roots", async () => {
  const result = await workspaces.replaceRoots([]);
  await workbench.writeClipboardText(JSON.stringify({ roots: result.roots, generation: result.generation }, null, 2));
});
