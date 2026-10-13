// Draft API version 1 example; checked as JavaScript, not loaded by the product.
import { commands, editor, workbench, keybinding, removeKeybinding } from "knot";

const command = await commands.register("guide.annotate", async (context) => {
  context.signal.throwIfAborted();
  const buffer = await editor.activeBuffer(context.target);
  if (buffer === null) return;
  const snapshot = await buffer.snapshot();
  const endByteOffset = snapshot.byteOffsetAtUtf16(snapshot.text.length);
  await buffer.contributions.replace(endByteOffset === 0 ? [] : [{
    range: { startByteOffset: 0, endByteOffset },
    decoration: "info",
    gutter: "info",
    command: "guide.annotate",
  }], { ifRevision: snapshot.revision });

  const subscription = await buffer.onDidChange(async (event) => {
    // Explicit buffer access works even without a command context.
    await event.buffer.snapshot();
    // Cleanup is an ownership barrier, so self-disposal is safe.
    await subscription.dispose();
  });
});

const child = await commands.register("guide.compose", async () => {
  const outcome = await commands.invoke("guide.annotate", { origin: "guide" });
  if (outcome.kind === "handlerFailure") throw new Error(outcome.message);
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
  await Promise.all([
    command.dispose(), child.dispose(), viewCommand.dispose(),
    completion.dispose(), tree.dispose(),
  ]);
}
