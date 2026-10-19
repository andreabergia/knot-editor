import { commands, editor, workbench, keybinding, removeKeybinding } from "knot";
import type { Buffer, CommandTarget, Disposable, HostError, HostErrorCode } from "knot";

const registration: Promise<Disposable> = commands.register("check", async (context) => {
  const target: CommandTarget = context.target;
  const buffer: Buffer | null = await editor.activeBuffer(target);
  context.signal.addEventListener("abort", (event) => event.target.throwIfAborted());
  if (buffer === null) return;
  const snapshot = await buffer.snapshot(null);
  const revision: number = snapshot.revision;
  await buffer.applyEdits([], { ifRevision: revision });
  await buffer.contributions.replace([], { ifRevision: revision });
  const subscription = await buffer.onDidChange(async (event) => {
    const sameBuffer: Buffer = event.buffer;
    await sameBuffer.snapshot();
  });
  const completion: Promise<void> = subscription.dispose();
  await completion;
  await buffer.contributions.dispose();
  const outcome = await commands.invoke("child", { nested: [null, true, 42, "text"] });
  if (outcome.kind === "invalidArgument" || outcome.kind === "handlerFailure") {
    const message: string = outcome.message;
    void message;
  }
  // @ts-expect-error target is opaque and cannot be fabricated
  const forged: CommandTarget = { window: "1", view: null };
  // @ts-expect-error revision option is required
  await buffer.applyEdits([]);
  // @ts-expect-error revision field is required
  await buffer.contributions.replace([], {});
  // @ts-expect-error UTF-8 offsets are numbers
  await buffer.snapshot({ startByteOffset: "0", endByteOffset: 1 });
  // @ts-expect-error buffer proxy is opaque
  const fakeBuffer: Buffer = {};
  // @ts-expect-error supported cancellation interface has no browser options
  context.signal.addEventListener("abort", () => {}, { once: true });
  void [forged, fakeBuffer];
});
void registration;

const tree = await workbench.registerView({
  id: "check.outline",
  title: "Check",
  content: {
    kind: "tree",
    provider: {
      getChildren(parentId, context) {
        const parent: string | null = parentId;
        void parent;
        const window = context.window;
        const instance: import("knot").ViewId = context.instance;
        void [window, instance];
        return [
          {
            id: "group", label: "Group", collapsibleState: "none", kind: "group",
            actions: [{ label: "Stage all", command: "check.stage", arguments: { id: 1 } }],
          },
          {
            id: "id", label: "item", collapsibleState: "collapsed", icon: "folder",
            statusText: "M", badge: "warning",
            command: "check.open",
            actions: [{ label: "Discard", command: "check.discard", arguments: { path: "a" } }],
          },
        ];
      },
    },
  },
  actions: [{ label: "Refresh", command: "check.refresh", arguments: {} }],
  placement: { location: "left", order: 1, visible: true },
});
const invalidation: Promise<void> = tree.invalidate(null);
const disposal: Promise<void> = tree.dispose();
void [invalidation, disposal];
await tree.show();
await tree.hide();
await tree.focus();

const text = await workbench.registerView({
  id: "check.log",
  title: "Check log",
  content: { kind: "text" },
});
const replacement: Promise<void> = text.setText("first line\n");
void replacement;
await text.setText("second line\n");
await text.setText("third line\n", undefined);
await text.dispose();

// @ts-expect-error text frames have no tree provider
const badContent: import("knot").ViewContent = { kind: "text", provider: {} };
// @ts-expect-error tree action command is required
const badAction: import("knot").TreeAction = { label: "Missing" };
void [badContent, badAction];
await editor.registerCompletionProvider("check", {
  async provideCompletions(context) {
    await context.buffer.snapshot();
    return [{ label: context.prefix, insertText: context.prefix }];
  },
});
const keyResult: void = keybinding("cmd-k", { name: "check", arguments: null }, { view: "editor" });
removeKeybinding("cmd-k", null);
void keyResult;

// @ts-expect-error command arguments must be JSON-compatible
await commands.invoke("check", () => {});
// @ts-expect-error numeric command names are invalid
await commands.invoke(42);
// @ts-expect-error completion item requires insertText
await editor.registerCompletionProvider("bad", { provideCompletions: () => [{ label: "bad" }] });
// @ts-expect-error tree content requires a provider
await workbench.registerView({ id: "check.bad", title: "Bad", content: { kind: "tree" } });
// @ts-expect-error tree item requires collapsibleState
await workbench.registerView({ id: "check.bad", title: "Bad", content: { kind: "tree", provider: { getChildren: () => [{ id: "bad", label: "bad" }] } } });
// @ts-expect-error decoration token is closed
const token: import("knot").EditorContribution = { range: { startByteOffset: 0, endByteOffset: 1 }, decoration: "blue" };
// @ts-expect-error clipboard requires text
await workbench.writeClipboardText(null);
// @ts-expect-error contextual targets cannot be null
await editor.activeBuffer(null);

import { documents, windows, workspaces } from "knot";
import type { DocumentId, WindowTarget, PaneTarget, ViewTarget } from "knot";
for (const document of await documents.list()) {
  const id: DocumentId = document.id;
  const current = await documents.get(id);
  if (current) await current.buffer.snapshot();
}
for (const window of await windows.list()) {
  const target: WindowTarget = window.target;
  await windows.get(target);
  await workspaces.get(target);
  const pane: PaneTarget | undefined = window.panes[0]?.target;
  const view: ViewTarget | null = window.currentTarget;
  if (view) {
    const parent: WindowTarget = view.window;
    void parent;
    // @ts-expect-error live view targets have no command invocation authority
    await editor.activeBuffer(view);
  }
  // @ts-expect-error live targets cannot be fabricated
  const forgedWindow: WindowTarget = {};
  // @ts-expect-error panes cannot be used as windows
  await windows.get(pane);
  // @ts-expect-error command targets cannot be used as live windows
  await windows.get({} as CommandTarget);
  void forgedWindow;
}
const opened: import("knot").DocumentPresentation = await documents.open("file:///tmp/document.txt");
await documents.open("file:///tmp/document.txt", { target: opened.presentation.target });
await documents.open("file:///tmp/document.txt", { target: opened.presentation.target.pane });
await documents.open("file:///tmp/document.txt", { target: opened.presentation.target.window });
// @ts-expect-error invocation targets do not grant discoverable live authority
await documents.open("file:///tmp/document.txt", { target: {} as CommandTarget });
// @ts-expect-error URI must be text
await documents.open(42);
// @ts-expect-error options cannot fabricate a live target
await documents.open("file:///tmp/document.txt", { target: {} });

const untitled = await documents.createUntitled({ target: opened.presentation.target.pane });
await documents.present(untitled.document.id, { target: opened.presentation.target.window });
// @ts-expect-error untitled options need live destinations
await documents.createUntitled({ target: {} as CommandTarget });
const workspace: import("knot").WorkspaceInfo = await workspaces.openFolder("file:///tmp", { target: opened.presentation.target.window });
await workspaces.replaceRoots([], { target: workspace.window });
await workspaces.replaceRoots(["file:///tmp"]);
// @ts-expect-error folder destinations require a window, not a pane
await workspaces.openFolder("file:///tmp", { target: opened.presentation.target.pane });
// @ts-expect-error root replacement accepts URI strings, not root metadata
await workspaces.replaceRoots(workspace.roots);
// @ts-expect-error root collections must be arrays
await workspaces.replaceRoots("file:///tmp");

const closed: import("knot").PresentationCloseOutcome = await workbench.close(opened.presentation.target);
const closedKind: "closed" | "cancelled" = closed.kind;
// @ts-expect-error closure requires an exact view, not a window
await workbench.close(workspace.window);
// @ts-expect-error command targets cannot close live presentations
await workbench.close({} as CommandTarget);
void closedKind;

// @ts-expect-error plain URIs are not document identities
await documents.present("file:///tmp/document.txt");

// @ts-expect-error plain URIs are not document identities
await documents.get("file:///tmp/document.txt");

const observation = await workbench.observe(async (event) => {
  const sequence: number = event.sequence;
  if (event.type === "documentOpened" || event.type === "documentChanged") {
    const documentId: DocumentId = event.document.id;
    await documents.get(documentId);
  }
  // @ts-expect-error application event payloads are immutable
  event.sequence = sequence + 1;
});
const watermark: number = observation.watermark;
const observedWindows: readonly import("knot").WindowInfo[] = observation.snapshot.windows;
await observation.dispose();
void [watermark, observedWindows];
// @ts-expect-error an application observer requires a listener
await workbench.observe();


import { storage } from "knot";
import type { JsonValue, StorageRead } from "knot";
const stored: StorageRead = await storage.read("settings");
if (stored.found) {
  const value: JsonValue = stored.value;
  void value;
} else {
  // @ts-expect-error missing results have no value
  void stored.value;
}
await storage.write("settings", { schema: 1, optional: null });
await storage.write("null", null);
await storage.delete("absent");
const storageKeys: readonly string[] = await storage.list();
void storageKeys;
// @ts-expect-error keys are strings
await storage.read(1);
// @ts-expect-error writes require a value
await storage.write("missing");
// @ts-expect-error JSON data excludes undefined
await storage.write("undefined", undefined);
// @ts-expect-error JSON data excludes functions
await storage.write("function", () => {});
// @ts-expect-error invalid-argument helper requires a string
commands.invalidArguments(1);
// @ts-expect-error no application namespace is declared
import { application } from "knot";
void [token, application];

function handleError(error: HostError) {
  const code: HostErrorCode = error.code;
  const message: string = error.message;
  const details: import("knot").JsonValue | undefined = error.details;
  // @ts-expect-error structured diagnostics are read-only
  error.details = null;
  // @ts-expect-error error codes are a closed union
  const unsupported: HostErrorCode = "Anything";
  void [code, message, details, unsupported];
}
void handleError;
