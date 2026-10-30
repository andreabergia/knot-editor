import { commands, editor, workbench, keybinding, removeKeybinding } from "knot";
import type { Buffer, CommandTarget, Disposable, HostError, HostErrorCode } from "knot";

void commands.register("described", async () => {}, {
  title: "Described Command",
  description: "Shown beside the title.",
  palette: false,
});
// @ts-expect-error palette is a boolean
void commands.register("bad", async () => {}, { palette: "no" });
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
// @ts-expect-error decorations name a color key
const token: import("knot").EditorContribution = { range: { startByteOffset: 0, endByteOffset: 1 }, decoration: 4 };
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
  const language: string = document.language;
  await documents.setLanguage(id, language === "plaintext" ? "rust" : null);
  // @ts-expect-error returning to detection is an explicit null
  await documents.setLanguage(id);
}

import { languages } from "knot";
import type { LanguageInfo } from "knot";
const grammarIds: readonly string[] = await languages.grammars();
const rust = await languages.register({
  id: "rust",
  name: "Rust",
  grammar: grammarIds[0] ?? "rust",
  fileExtensions: ["rs"],
  interpreters: ["rust-script"],
  queries: { highlights: "(identifier) @variable", injections: "" },
});
const registered: readonly LanguageInfo[] = await languages.list();
void registered;
await rust.dispose();
// @ts-expect-error a highlights query is required
await languages.register({ id: "bad", name: "Bad", grammar: "rust", queries: {} });
await workbench.registerView({ id: "check.rust-log", title: "Log", content: { kind: "text" }, language: "rust" });
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
await workspaces.replaceRoots([...workspace.roots.map((root) => root.uri), "file:///var"], { target: workspace.window });
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

// Transient interactions resolve typed selection, acceptance and dismissal.
const picked: import("knot").PickerResult = await workbench.showPicker({
  title: "Pick",
  items: [{ id: "a", label: "A", description: null }],
});
if (picked.kind === "selected") {
  const id: string = picked.id;
  void id;
}
const typed: import("knot").InputResult = await workbench.showInput({
  title: "Name", value: "seed", target: opened.presentation.target.window,
});
if (typed.kind === "accepted") {
  const value: string = typed.value;
  void value;
}
const confirmed: import("knot").ConfirmationResult = await workbench.showConfirmation({
  title: "Sure?", message: "detail", confirmLabel: "Yes", cancelLabel: "No",
});
if (confirmed.kind === "confirmed") {
  // Confirmation carries no payload.
}
const pickedFolder: import("knot").FolderPickerResult = await workspaces.pickFolder();
if (pickedFolder.kind === "selected") {
  const uri: string = pickedFolder.uri;
  void uri;
}
// @ts-expect-error folder picker results are selected or dismissed only
void pickedFolder.value;
// @ts-expect-error target options accept a live window target, not a string
await workspaces.pickFolder({ target: "window" });
await workbench.showMessage({ title: "Notice", message: "body" });
// @ts-expect-error picker items require an id
await workbench.showPicker({ title: "Bad", items: [{ label: "missing" }] });
// @ts-expect-error input title is required
await workbench.showInput({ value: "no title" });
// @ts-expect-error command targets are not live window targets
await workbench.showMessage({ title: "Notice", target: {} as CommandTarget });

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
  if (event.type === "launched") {
    // @ts-expect-error the launch event carries no window
    void event.window;
  }
  if (event.type === "viewStateChanged") {
    const settled: import("knot").WindowLayout = await workbench.captureLayout(event.window);
    void settled;
  }
  // @ts-expect-error application event payloads are immutable
  event.sequence = sequence + 1;
});
const watermark: number = observation.watermark;
const observedWindows: readonly import("knot").WindowInfo[] = observation.snapshot.windows;
const launchedAlready: boolean = observation.snapshot.launched;
void launchedAlready;
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

import { recovery } from "knot";
import type { CaptureId, RecoveryRestore, RetainedCapture } from "knot";
const [openDocument] = await documents.list();
if (openDocument) {
  const capture: RetainedCapture = await recovery.retain(openDocument.id);
  const persisted: string = capture.id;
  const captureKind: "untitled" | "destination" | "persisted" = capture.kind;
  const capturedAt: number = capture.capturedAt;
  void [persisted, captureKind, capturedAt];
  const restored: RecoveryRestore = await recovery.restore(capture.id);
  if (restored.outcome === "restored") {
    const resource: "untitled" | "unchanged" | "missing" | "changed" = restored.resource;
    void resource;
  } else {
    const liveOwner: import("knot").DocumentId | null = restored.owner;
    void liveOwner;
  }
  const restoredDocument: import("knot").DocumentInfo = restored.document;
  void restoredDocument;
  const released: boolean = await recovery.release(capture.id);
  void released;
  // @ts-expect-error captures are immutable
  capture.title = "renamed";
}
const captures: readonly RetainedCapture[] = await recovery.list();
void captures;
// @ts-expect-error capture IDs are opaque and cannot be fabricated
await recovery.release("forged");
// @ts-expect-error retain takes a document identity, not a URI
await recovery.retain(1);
// @ts-expect-error no owner can be supplied
await recovery.list("@other/package");
void (null as unknown as CaptureId);

import type { LayoutNode, LayoutTab, ReplaceWindowsResult, SessionEntryOutcome, WindowLayout } from "knot";
const [sourceWindow] = await windows.list();
if (sourceWindow && openDocument) {
  await windows.focus(sourceWindow.target);
  const kept = await recovery.retain(openDocument.id);
  const layout: WindowLayout = await workbench.captureLayout(sourceWindow.target);
  const version: 1 = layout.version;
  const describeNode = (node: LayoutNode): number => node.kind === "split"
    ? describeNode(node.first) + describeNode(node.second)
    : node.tabs.filter((tab: LayoutTab) => tab.kind === "document" && tab.view.caret >= 0).length;
  void [version, layout.geometry?.state, layout.root === null ? 0 : describeNode(layout.root)];
  // Layouts are JSON data, so a session record can store them verbatim.
  await storage.write("session", { layout });
  // @ts-expect-error layouts are read-only snapshots
  layout.focused = 0;
  const replaced: ReplaceWindowsResult = await workbench.replaceWindows({
    suspend: [{ document: openDocument.id, capture: kept.id }],
    windows: [
      { roots: ["file:///project/"], selected: 0, layout,
        entries: [{ uri: "file:///project/a.txt" }, { document: openDocument.id, capture: kept.id }] },
      { roots: ["file:///project/", "file:///notes/"], entries: [], layout: null },
    ],
    release: [sourceWindow.target],
    focus: 0,
  });
  const protectedText: boolean = replaced.protected;
  for (const window of replaced.windows) {
    const target: import("knot").WindowTarget = window.window;
    for (const outcome of window.entries) {
      const entry: SessionEntryOutcome = outcome;
      if (entry.kind === "restored") {
        const resource: "untitled" | "unchanged" | "missing" | "changed" = entry.resource;
        void resource;
      } else if (entry.kind === "failed") {
        const code: HostErrorCode = entry.code;
        void code;
      } else if (entry.kind !== "missing") {
        const document: import("knot").DocumentId = entry.document;
        void document;
      }
    }
    const applied: "applied" | "ignored" | null = window.layout;
    void [target, applied];
  }
  void protectedText;
  // @ts-expect-error every suspended document states its capture, even when null
  await workbench.replaceWindows({ suspend: [{ document: openDocument.id }], windows: [] });
  // @ts-expect-error window targets are opaque and cannot be fabricated
  await workbench.replaceWindows({ suspend: [], windows: [], release: ["1"] });
  // @ts-expect-error destination windows are always created, never reused
  await workbench.replaceWindows({ suspend: [], windows: [{ target: sourceWindow.target, roots: [], entries: [] }] });
  // @ts-expect-error captureLayout takes a live window target
  await workbench.captureLayout("1");
  // @ts-expect-error captures are opaque identities
  await workbench.replaceWindows({ suspend: [], windows: [{ roots: [], entries: [{ capture: "forged" }] }] });
  // @ts-expect-error focus takes a live window target
  await windows.focus("1");
  const opened: import("knot").WindowInfo = await windows.open({ roots: ["file:///project"] });
  await windows.focus(opened.target);
  await windows.open();
  // @ts-expect-error roots are URI strings
  await windows.open({ roots: [opened.target] });
}
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

import { settings } from "knot";
import type { SettingKey, SettingsScope } from "knot";
declare module "knot" {
  interface Settings {
    "check.timeout": number;
  }
}
await settings.define({ key: "check.timeout", type: "integer", default: 52, description: "Timeout" });
await settings.set("editor.tabSize", 2, { language: "rust" });
await settings.set({ "editor.tabSize": 4, "check.timeout": 10 }, { view: "editor", language: "rust" });
const facts: SettingsScope = { language: "rust", view: "editor" };
const tabSize: number | undefined = await settings.get("editor.tabSize", facts);
void tabSize;
await settings.reset("editor.tabSize", { language: "rust" });
const watch: Disposable = await settings.observe("check.timeout", (key: SettingKey) => {
  void key;
});
await watch.dispose();
// @ts-expect-error values follow the declared key type
await settings.set("editor.tabSize", "2");
// @ts-expect-error undeclared keys are not settings
await settings.get("check.missing");
// @ts-expect-error the default follows the declared key type
await settings.define({ key: "check.timeout", type: "integer", default: "52", description: "Timeout" });
const association: Disposable = await languages.associate("markdown", { fileExtensions: ["mdx"], fileNames: ["NOTES"] });
await association.dispose();
// @ts-expect-error patterns are lists
await languages.associate("markdown", { fileExtensions: "mdx" });
