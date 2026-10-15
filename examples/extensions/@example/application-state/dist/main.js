import { commands, documents, windows, workbench } from "knot";

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
