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

const tree = await workbench.registerTreeDataProvider("outline", {
  getChildren(parentId) {
    const parent: string | null = parentId;
    void parent;
    return [{ id: "id", label: "item", collapsibleState: "collapsed", icon: "folder" }];
  },
});
const invalidation: Promise<void> = tree.invalidate(null);
const disposal: Promise<void> = tree.dispose();
void [invalidation, disposal];
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
// @ts-expect-error arbitrary native view handlers are unsupported
await commands.registerForView("terminal", "copy", () => {});
// @ts-expect-error unsupported native tree kind
await workbench.registerTreeDataProvider("projects", { getChildren: () => [] });
// @ts-expect-error completion item requires insertText
await editor.registerCompletionProvider("bad", { provideCompletions: () => [{ label: "bad" }] });
// @ts-expect-error tree item requires collapsibleState
await workbench.registerTreeDataProvider("outline", { getChildren: () => [{ id: "bad", label: "bad" }] });
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
// @ts-expect-error plain URIs are not document identities
await documents.get("file:///tmp/document.txt");

// @ts-expect-error invalid-argument helper requires a string
commands.invalidArguments(1);
// @ts-expect-error no application namespace is declared
import { application } from "knot";
void [token, application];

function handleError(error: HostError) {
  const code: HostErrorCode = error.code;
  const message: string = error.message;
  // @ts-expect-error error codes are a closed union
  const unsupported: HostErrorCode = "Anything";
  void [code, message, unsupported];
}
void handleError;
