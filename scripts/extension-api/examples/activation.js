// API version 1 authoring example.
import { commands, editor, workbench, keybinding, removeKeybinding } from "knot";

/** @type {Set<import("knot").Disposable>} */
const bufferOwnership = new Set();
let active = true;

const command = await commands.register("guide.annotate", async (context) => {
  if (!active) return;
  context.signal.throwIfAborted();
  const buffer = await editor.activeBuffer(context.target);
  if (buffer === null || !active) return;
  const snapshot = await buffer.snapshot();
  if (!active) return;
  bufferOwnership.add(buffer.contributions);
  const endByteOffset = snapshot.byteOffsetAtUtf16(snapshot.text.length);
  await buffer.contributions.replace(endByteOffset === 0 ? [] : [{
    range: { startByteOffset: 0, endByteOffset },
    decoration: "info",
    gutter: "info",
    command: "guide.annotate",
  }], { ifRevision: snapshot.revision });

  if (!active) return;
  const subscription = await buffer.onDidChange(async (event) => {
    // Explicit buffer access works even without a command context.
    await event.buffer.snapshot();
    // Cleanup is an ownership barrier, so self-disposal is safe.
    await subscription.dispose();
    bufferOwnership.delete(subscription);
  });
  bufferOwnership.add(subscription);
  if (!active) await subscription.dispose();
});

const child = await commands.register("guide.compose", async () => {
  const outcome = await commands.invoke("guide.annotate", { origin: "guide" });
  if (outcome.kind === "handlerFailure") throw new Error(outcome.message);
});

// Native and JavaScript children share captured routing and awaited outcomes.
const nativeChild = await commands.register("guide.select-all", async () => {
  const outcome = await commands.invoke("editor.select-all");
  if (outcome.kind !== "completed") {
    throw new Error(`Select All did not complete: ${outcome.kind}`);
  }
});

const tree = await workbench.registerTreeDataProvider("outline", {
  getChildren(parentId) {
    return parentId === null ? [{
      id: "root", label: "Guide", collapsibleState: "none",
      description: null, icon: "symbol", command: "guide.annotate",
    }] : [];
  },
});
await tree.invalidate();

const viewCommand = await commands.registerForView("outline", "copy", async ({ target }) => {
  const selected = await workbench.selectedText(target);
  if (selected !== null) await workbench.writeClipboardText(selected);
});

const completion = await editor.registerCompletionProvider("guide", {
  async provideCompletions(context) {
    const snapshot = await context.buffer.snapshot();
    if (snapshot.revision !== context.revision) return [];
    return [{ label: `${context.prefix}Guide`, insertText: `${context.prefix}Guide` }];
  },
});

// These mutations commit together at the end of this V8 turn.
keybinding("cmd-g", { name: "guide.compose", arguments: { source: "key" } });
keybinding("cmd-c", "copy", { view: "outline" });
keybinding("cmd-alt-g", null, { view: "editor" });
removeKeybinding("cmd-alt-g", { view: "editor" });

// Called by an author-controlled feature toggle, not a promised unload hook.
export async function deactivateFeature() {
  active = false;
  removeKeybinding("cmd-g");
  removeKeybinding("cmd-c", { view: "outline" });
  await Promise.all([
    ...Array.from(bufferOwnership, (owner) => owner.dispose()),
    command.dispose(), child.dispose(), nativeChild.dispose(), viewCommand.dispose(),
    completion.dispose(), tree.dispose(),
  ]);
}
