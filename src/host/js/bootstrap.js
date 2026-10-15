const nativeBindings = globalThis.__knotNativeBindings;
delete globalThis.__knotNativeBindings;

export function keybinding(key, command, options) {
  if (command !== null && typeof command === "object" && !Array.isArray(command)) {
    try {
      command = {
        name: command.name,
        arguments: command.arguments === undefined ? null : validateCommandArguments(command.arguments),
      };
    } catch (error) {
      nativeBindings.keybinding("invalid", String(error));
      return;
    }
  }
  nativeBindings.keybinding("set", key, command, options);
}

export function removeKeybinding(key, options) {
  nativeBindings.keybinding("remove", key, undefined, options);
}

function hostError(code) {
  const exception = new Error(code);
  exception.name = "KnotHostError";
  exception.code = code;
  throw exception;
}

function ownership(removeLocal, release) {
  let inactive = false;
  let cleanup;
  const pending = new Set();
  return {
    assertActive() { if (inactive) hostError("OwnershipDisposed"); },
    track(promise) {
      pending.add(promise);
      promise.then(() => pending.delete(promise), () => pending.delete(promise));
      return promise;
    },
    dispose() {
      if (cleanup) return cleanup;
      inactive = true;
      removeLocal();
      const releasePromise = release();
      const earlier = [...pending];
      cleanup = Promise.allSettled([...earlier, releasePromise]).then((results) => {
        const releaseResult = results.at(-1);
        if (releaseResult.status === "rejected") throw releaseResult.reason;
      });
      return cleanup;
    },
  };
}

function record(value, message) {
  if (value === null || typeof value !== "object" || Array.isArray(value)) throw new TypeError(message);
  return value;
}
function string(value, message) {
  if (typeof value !== "string") throw new TypeError(message);
  return value;
}
function safeInteger(value) {
  if (typeof value !== "number") throw new TypeError("expected a number");
  if (!Number.isSafeInteger(value) || value < 0) throw new RangeError("expected a nonnegative safe integer");
  return value;
}
function byteRange(value) {
  record(value, "range must be an object");
  const startByteOffset = safeInteger(value.startByteOffset);
  const endByteOffset = safeInteger(value.endByteOffset);
  if (startByteOffset > endByteOffset) throw new RangeError("range endpoints must be ordered");
  return { startByteOffset, endByteOffset };
}
function revision(options) {
  return safeInteger(record(options, "revision options are required").ifRevision);
}
function arrayItems(items, convert) {
  if (!Array.isArray(items)) throw new TypeError("items must be an array");
  return Array.from(items, convert);
}
function optionalText(value) {
  return value == null ? null : string(value, "expected text or null");
}
function token(value, allowed) {
  if (value == null) return null;
  if (!allowed.includes(value)) throw new TypeError("invalid semantic token");
  return value;
}
function contribution(value) {
  record(value, "contribution must be an object");
  return {
    range: byteRange(value.range),
    decoration: token(value.decoration, ["info", "warning", "error"]),
    gutter: token(value.gutter, ["info", "warning", "error"]),
    command: optionalText(value.command),
  };
}
function iterableItems(value, convert) {
  if (value == null || typeof value === "string" || typeof value[Symbol.iterator] !== "function") {
    throw new TypeError("provider result must be an iterable of items");
  }
  return Array.from(value, convert);
}
function treeItem(value) {
  record(value, "tree item must be an object");
  const collapsibleState = token(value.collapsibleState, ["none", "collapsed", "expanded"]);
  if (collapsibleState === null) throw new TypeError("collapsibleState is required");
  return {
    id: string(value.id, "tree item ID must be text"),
    label: string(value.label, "tree item label must be text"),
    description: optionalText(value.description),
    icon: token(value.icon, ["file", "folder", "symbol"]),
    collapsibleState,
    command: optionalText(value.command),
  };
}

async function requestForInvocation(invocation, operation, ...arguments_) {
  try {
    return await nativeBindings.request(
      invocation,
      operation,
      ...arguments_,
    );
  } catch (error) {
    if (error?.code === "Cancelled") activeCommandFrames.at(-1)?.controller.abort(error);
    throw error;
  }
}

function request(operation, ...arguments_) {
  return requestForInvocation(
    activeCommandFrames.at(-1)?.invocation ?? null,
    operation,
    ...arguments_,
  );
}

const buffers = new Map();
const commandHandlers = new Map();
const bufferChangeListeners = new Map();
const treeProviders = new Map();
const completionProviders = new Map();
const activeCommandFrames = [];
const snapshotTables = new WeakMap();
const targetAuthorities = new WeakMap();
const invalidArgumentErrors = new WeakSet();
const signalAborters = new WeakMap();

class KnotAbortSignal {
  #aborted = false;
  #reason = undefined;
  #listeners = new Set();
  constructor() { signalAborters.set(this, (reason) => this.#abort(reason)); }
  get aborted() { return this.#aborted; }
  get reason() { return this.#reason; }
  addEventListener(type, listener) {
    if (type !== "abort" || typeof listener !== "function") throw new TypeError("expected an abort listener");
    if (!this.#aborted) this.#listeners.add(listener);
  }
  removeEventListener(type, listener) {
    if (type !== "abort" || typeof listener !== "function") throw new TypeError("expected an abort listener");
    this.#listeners.delete(listener);
  }
  throwIfAborted() {
    if (this.#aborted) throw this.#reason;
  }
  #abort(reason) {
    if (this.#aborted) return;
    this.#aborted = true;
    this.#reason = reason;
    for (const listener of this.#listeners) {
      try { listener.call(this, Object.freeze({ type: "abort", target: this })); }
      catch (error) {
        let message;
        try { message = String(error?.stack ?? error); } catch { message = "abort listener failed"; }
        nativeBindings.reportCancellationListenerFailure(message);
      }
    }
    this.#listeners.clear();
  }
}

class KnotAbortController {
  signal = Object.freeze(new KnotAbortSignal());
  abort(reason = new Error("command cancelled")) { signalAborters.get(this.signal)(reason); }
}

function byteLengthOfCodePoint(codePoint) {
  return codePoint <= 0x7f ? 1 : codePoint <= 0x7ff ? 2 : codePoint <= 0xffff ? 3 : 4;
}

function snapshotTable(snapshot) {
  let table = snapshotTables.get(snapshot);
  if (table) return table;

  const byteAtUtf16 = [0];
  const utf16AtByte = new Map([[0, 0]]);
  let byteOffset = 0;
  let utf16Offset = 0;
  for (const character of snapshot.text) {
    byteOffset += byteLengthOfCodePoint(character.codePointAt(0));
    utf16Offset += character.length;
    byteAtUtf16[utf16Offset] = byteOffset;
    utf16AtByte.set(byteOffset, utf16Offset);
  }
  table = { byteAtUtf16, utf16AtByte };
  snapshotTables.set(snapshot, table);
  return table;
}

function snapshotFromNative(snapshot) {
  return Object.freeze({
    text: snapshot.text,
    range: Object.freeze({
      startByteOffset: snapshot.range.startByteOffset,
      endByteOffset: snapshot.range.endByteOffset,
    }),
    revision: snapshot.revision,
    byteOffsetAtUtf16(offset) {
      safeInteger(offset);
      const offsets = snapshotTable(this).byteAtUtf16;
      const value = Object.hasOwn(offsets, offset) ? offsets[offset] : undefined;
      if (value === undefined) {
        throw new RangeError("UTF-16 offset splits a surrogate pair or is out of bounds");
      }
      return value;
    },
    utf16OffsetAtByte(offset) {
      safeInteger(offset);
      const value = snapshotTable(this).utf16AtByte.get(offset);
      if (value === undefined) {
        throw new RangeError("byte offset splits a UTF-8 scalar or is out of bounds");
      }
      return value;
    },
  });
}

function bufferFor(handle) {
  let buffer = buffers.get(handle);
  if (buffer) return buffer;

  const contributionOwnership = ownership(() => {}, () => request("disposeEditorContributions", handle));
  const contributions = Object.freeze({
    async replace(items, options) {
      contributionOwnership.assertActive();
      const converted = arrayItems(items, contribution);
      const ifRevision = revision(options);
      await contributionOwnership.track(request("replaceEditorContributions", handle, converted, ifRevision));
    },
    dispose: () => contributionOwnership.dispose(),
  });

  buffer = Object.freeze({
    async snapshot(range) {
      return snapshotFromNative(await request("snapshot", handle, range == null ? null : byteRange(range)));
    },
    async applyEdits(edits, options) {
      const converted = arrayItems(edits, (edit) => {
        record(edit, "edit must be an object");
        return { range: byteRange(edit.range), text: string(edit.text, "edit text must be a string") };
      });
      const resultRevision = await request("applyEdits", handle, converted, revision(options));
      return Object.freeze({ revision: resultRevision });
    },
    async onDidChange(listener) {
      if (typeof listener !== "function") {
        throw new TypeError("onDidChange requires a listener");
      }
      const subscription = await request("subscribeBufferChanges", handle);
      bufferChangeListeners.set(subscription, { handle, listener });
      const owned = ownership(() => bufferChangeListeners.delete(subscription),
        () => request("unsubscribeBufferChanges", subscription));
      return Object.freeze({ dispose: () => owned.dispose() });
    },
    contributions,
  });
  buffers.set(handle, buffer);
  return buffer;
}

function contextualAuthority(target) {
  if (target === undefined) return null;
  const frame = activeCommandFrames.at(-1);
  const authority = targetAuthorities.get(target);
  if (authority === undefined || !frame?.target ||
      authority !== targetAuthorities.get(frame.target)) hostError("InvalidTarget");
  return authority;
}

export async function activeBuffer(target) {
  const handle = await request("activeBuffer", contextualAuthority(target));
  return handle === null ? null : bufferFor(handle);
}

globalThis.__knotDispatchBufferChange = async (subscription, change) => {
  const entry = bufferChangeListeners.get(subscription);
  if (!entry) return;
  await entry.listener(Object.freeze({
    buffer: bufferFor(entry.handle),
    beforeRevision: change.beforeRevision,
    revision: change.revision,
    edits: Object.freeze(change.edits.map((edit) => Object.freeze({
      range: Object.freeze({
        startByteOffset: edit.range.startByteOffset,
        endByteOffset: edit.range.endByteOffset,
      }),
      text: edit.text,
    }))),
  }));
};

export async function registerCommand(name, handler) {
  if (typeof name !== "string" || typeof handler !== "function") {
    throw new TypeError("commands.register requires a name and handler");
  }
  const registration = await request("registerCommand", name, name);
  commandHandlers.set(registration, handler);
  const owned = ownership(() => commandHandlers.delete(registration),
    () => request("unregisterCommand", registration));
  return Object.freeze({ dispose: () => owned.dispose() });
}

export async function registerViewCommand(viewKind, name, handler) {
  if (typeof viewKind !== "string" || typeof name !== "string" || typeof handler !== "function") {
    throw new TypeError("commands.registerForView requires a view, command name, and handler");
  }
  const registration = await request("registerViewCommand", viewKind, name);
  commandHandlers.set(registration, handler);
  const owned = ownership(() => commandHandlers.delete(registration),
    () => request("unregisterCommand", registration));
  return Object.freeze({ dispose: () => owned.dispose() });
}

export async function selectedViewText(target) {
  return request("selectedViewText", contextualAuthority(target));
}

export async function writeClipboardText(text) {
  if (typeof text !== "string") {
    throw new TypeError("workbench.writeClipboardText requires text");
  }
  await request("writeClipboardText", text);
}

export async function invokeCommand(name, commandArguments) {
  if (typeof name !== "string") {
    throw new TypeError("commands.invoke requires a command name");
  }
  const arguments_ = commandArguments === undefined ? null : commandArguments;
  const normalized = validateCommandArguments(arguments_);
  const dispatch = await request("invokeCommand", name, normalized);
  if (dispatch.kind === "outcome") return Object.freeze(dispatch.outcome);

  const outcome = await invokeRegisteredCommand(
    dispatch.invocation,
    dispatch.registration,
    activeCommandFrames.at(-1)?.buffer ?? null,
    normalized,
    activeCommandFrames.at(-1)?.capture ?? null,
  );
  await requestForInvocation(
    dispatch.invocation,
    "completeInlineCommand",
    dispatch.invocation,
    outcome,
  );
  return Object.freeze(outcome);
}

function validateCommandArguments(value, seen = new Set()) {
  if (value === null || typeof value === "string" || typeof value === "boolean") return value;
  if (typeof value === "number") {
    if (Number.isFinite(value)) return value;
    throw new TypeError("command arguments require finite numbers");
  }
  if (typeof value !== "object") throw new TypeError("command arguments must be JSON-compatible");
  if (seen.has(value)) throw new TypeError("command arguments cannot contain cycles");
  const prototype = Object.getPrototypeOf(value);
  if ((Array.isArray(value) ? prototype !== Array.prototype : prototype !== null && prototype !== Object.prototype) ||
      typeof value.toJSON === "function" || Object.getOwnPropertySymbols(value).length !== 0) {
    throw new TypeError("command arguments require plain JSON data");
  }
  seen.add(value);
  let copy;
  if (Array.isArray(value)) {
    if (Object.keys(value).some((key) => {
      const index = Number(key);
      return !Number.isInteger(index) || index < 0 || index >= value.length || String(index) !== key;
    })) throw new TypeError("command arrays must contain only indexed data");
    copy = [];
    Object.defineProperty(copy, "toJSON", { value: undefined });
    for (let index = 0; index < value.length; index++) {
      if (!Object.hasOwn(value, index)) throw new TypeError("command arrays must be dense");
      copy.push(validateCommandArguments(value[index], seen));
    }
  } else {
    copy = Object.create(null);
    for (const key of Object.keys(value)) copy[key] = validateCommandArguments(value[key], seen);
  }
  seen.delete(value);
  return Object.freeze(copy);
}

export function invalidCommandArguments(message) {
  const error = new Error(string(message, "invalidArguments requires a message"));
  error.name = "InvalidCommandArgumentsError";
  invalidArgumentErrors.add(error);
  throw error;
}

async function invokeRegisteredCommand(
  invocation,
  registration,
  activeHandle,
  commandArguments,
  capture,
) {
  const handler = commandHandlers.get(registration);
  if (!handler) {
    return {
      kind: "handlerFailure",
      message: "Knot command registration is disposed",
    };
  }
  const controller = new KnotAbortController();
  const target = capture === null ? null : Object.freeze({
    window: capture.window, view: capture.view,
  });
  if (target) targetAuthorities.set(target, capture.authority);
  const frame = { invocation, controller, buffer: activeHandle, capture, target };
  activeCommandFrames.push(frame);
  try {
    try {
      await handler(Object.freeze({
        target,
        buffer: activeHandle === null ? null : bufferFor(activeHandle),
        arguments: validateCommandArguments(commandArguments),
        signal: controller.signal,
      }));
      return controller.signal.aborted
        ? { kind: "cancelled" }
        : { kind: "completed" };
    } catch (error) {
      if (controller.signal.aborted) {
        return { kind: "cancelled" };
      }
      const message = String(error?.stack ?? error);
      if (invalidArgumentErrors.has(error)) {
        return { kind: "invalidArgument", message };
      }
      return { kind: "handlerFailure", message };
    }
  } finally {
    const popped = activeCommandFrames.pop();
    if (popped !== frame) throw new Error("Knot command frame stack is corrupted");
  }
}

globalThis.__knotInvokeCommand = (
  invocation,
  registration,
  activeHandle,
  commandArguments,
  capture,
) => invokeRegisteredCommand(
  invocation,
  registration,
  activeHandle,
  commandArguments,
  capture,
);

globalThis.__knotCancelCommand = (invocation) => {
  const frame = activeCommandFrames.find((frame) => frame.invocation === invocation);
  frame?.controller.abort(new Error("command cancelled"));
};

export async function registerTreeDataProvider(viewId, provider) {
  if (typeof viewId !== "string" || typeof provider?.getChildren !== "function") {
    throw new TypeError("workbench.registerTreeDataProvider requires a view ID and getChildren provider");
  }
  const registration = await request("registerTreeDataProvider", viewId);
  treeProviders.set(registration, provider);
  const owned = ownership(() => treeProviders.delete(registration),
    () => request("unregisterTreeDataProvider", registration));
  return Object.freeze({
    async invalidate(parentId) {
      owned.assertActive();
      const parent = optionalText(parentId);
      await owned.track(request("invalidateTreeDataProvider", registration, parent));
    },
    dispose: () => owned.dispose(),
  });
}

globalThis.__knotRequestTreeChildren = async (registration, parentId, generation) => {
  const provider = treeProviders.get(registration);
  if (!provider) {
    return { error: "tree data provider is disposed" };
  }
  try {
    return { items: iterableItems(await provider.getChildren(parentId), treeItem) };
  } catch (error) {
    return { error: String(error?.stack ?? error) };
  }
};

export async function registerCompletionProvider(label, provider) {
  if (typeof label !== "string" || typeof provider?.provideCompletions !== "function") {
    throw new TypeError("editor.registerCompletionProvider requires a label and provideCompletions provider");
  }
  const registration = await request("registerCompletionProvider", label);
  completionProviders.set(registration, provider);
  const owned = ownership(() => completionProviders.delete(registration),
    () => request("unregisterCompletionProvider", registration));
  return Object.freeze({ dispose: () => owned.dispose() });
}

globalThis.__knotRequestCompletions = async (
  registration,
  handle,
  revision,
  cursorByteOffset,
  prefix,
  generation,
) => {
  const provider = completionProviders.get(registration);
  if (!provider) {
    return { error: "completion provider is disposed" };
  }
  try {
    const provided = await provider.provideCompletions(Object.freeze({
      buffer: bufferFor(handle), revision, cursorByteOffset, prefix, generation,
    }));
    return {
      items: iterableItems(provided, (item) => {
        if (typeof item?.label !== "string" || typeof item?.insertText !== "string") {
          throw new TypeError("completion items require string label and insertText properties");
        }
        return Object.freeze({ label: item.label, insertText: item.insertText });
      }),
    };
  } catch (error) {
    return { error: String(error?.stack ?? error) };
  }
};

const liveWindows = new WeakMap();
const livePanes = new WeakMap();
const livePresentations = new WeakMap();
const windowTargets = new Map();
const paneTargets = new Map();
const presentationTargets = new Map();

function windowTarget(value) {
  let target = windowTargets.get(value.window);
  if (!target) {
    target = Object.freeze({});
    liveWindows.set(target, value);
    windowTargets.set(value.window, target);
  }
  return target;
}
function paneTarget(value) {
  const key = `${value.window.window}/${value.workbench}/${value.pane}`;
  let target = paneTargets.get(key);
  if (!target) {
    target = Object.freeze({ window: windowTarget(value.window) });
    livePanes.set(target, value);
    paneTargets.set(key, target);
  }
  return target;
}
function presentationTarget(value) {
  const key = `${value.pane.window.window}/${value.pane.workbench}/${value.pane.pane}/${value.tab}/${value.presentation}`;
  let target = presentationTargets.get(key);
  if (!target) {
    const pane = paneTarget(value.pane);
    target = Object.freeze({ window: pane.window, pane });
    livePresentations.set(target, value);
    presentationTargets.set(key, target);
  }
  return target;
}
function windowAuthority(target) {
  const value = liveWindows.get(target);
  if (!value) hostError("InvalidTarget");
  return value.window;
}
function documentFromNative(value) {
  return Object.freeze({ ...value, buffer: bufferFor(value.buffer) });
}
function workspaceFromNative(value) {
  return Object.freeze({ window: windowTarget(value.window), generation: value.generation,
    roots: Object.freeze(value.roots.map(root => Object.freeze(root))) });
}
function windowFromNative(value) {
  return Object.freeze({ target: windowTarget(value.target),
    currentTarget: value.currentTarget === null ? null : presentationTarget(value.currentTarget),
    workspace: workspaceFromNative(value.workspace),
    panes: Object.freeze(value.panes.map(pane => Object.freeze({
      target: paneTarget(pane.target), selected: presentationTarget(pane.selected),
      tabs: Object.freeze(pane.tabs.map(tab => Object.freeze({
        target: presentationTarget(tab.target), kind: tab.kind, document: tab.document,
        buffer: tab.buffer === null ? null : bufferFor(tab.buffer),
      }))),
    }))),
  });
}
function destinationAuthority(options) {
  if (options === undefined) return null;
  const target = record(options, "destination options must be an object").target;
  if (target === undefined) return null;
  const window = liveWindows.get(target);
  if (window) return { kind: "window", window: window.window };
  const pane = livePanes.get(target);
  if (pane) return { kind: "pane", window: pane.window.window, workbench: pane.workbench, pane: pane.pane };
  const view = livePresentations.get(target);
  if (view) return { kind: "view", window: view.pane.window.window, workbench: view.pane.workbench,
    pane: view.pane.pane, tab: view.tab, presentation: view.presentation };
  hostError("InvalidTarget");
}
function documentPresentationFromNative(value) {
  const presentation = value.presentation;
  return Object.freeze({ document: documentFromNative(value.document),
    presentation: Object.freeze({ target: presentationTarget(presentation.target), kind: presentation.kind,
      document: presentation.document, buffer: bufferFor(presentation.buffer) }) });
}
export async function createUntitled(options) {
  return documentPresentationFromNative(await request("createUntitled", destinationAuthority(options)));
}
export async function presentDocument(id, options) {
  return documentPresentationFromNative(await request("presentDocument", string(id, "document identity must be text"), destinationAuthority(options)));
}
export async function openDocument(uri, options) {
  const value = await request("openDocument", string(uri, "resource URI must be text"), destinationAuthority(options));
  return documentPresentationFromNative(value);
}

export async function listDocuments() {
  return Object.freeze((await request("listDocuments")).map(documentFromNative));
}
export async function getDocument(id) {
  const value = await request("getDocument", string(id, "document identity must be text"));
  return value === null ? null : documentFromNative(value);
}
export async function listWindows() {
  return Object.freeze((await request("listWindows")).map(windowFromNative));
}
export async function getWindow(target) {
  const value = await request("getWindow", windowAuthority(target));
  return value === null ? null : windowFromNative(value);
}
export async function getWorkspace(target) {
  const value = await request("getWorkspace", windowAuthority(target));
  return value === null ? null : workspaceFromNative(value);
}

const applicationListeners = new Map();
function applicationSnapshotFromNative(value) {
  return Object.freeze({
    documents: Object.freeze(value.documents.map(documentFromNative)),
    windows: Object.freeze(value.windows.map(windowFromNative)),
    activeWindow: value.activeWindow === null ? null : windowTarget(value.activeWindow),
    activeTarget: value.activeTarget === null ? null : presentationTarget(value.activeTarget),
  });
}
export async function observeWorkbench(listener) {
  if (typeof listener !== "function") throw new TypeError("observe requires a listener");
  const value = await request("observeWorkbench");
  applicationListeners.set(value.subscription, listener);
  const owned = ownership(() => applicationListeners.delete(value.subscription),
    () => request("unobserveWorkbench", value.subscription));
  return Object.freeze({ snapshot: applicationSnapshotFromNative(value.snapshot), watermark: value.watermark,
    dispose: () => owned.dispose() });
}
globalThis.__knotDispatchApplicationChange = async (subscription, value) => {
  const listener = applicationListeners.get(subscription);
  if (!listener) return;
  let payload;
  switch (value.type) {
    case "documentOpened": case "documentChanged": payload = { document: documentFromNative(value.document) }; break;
    case "documentClosed": payload = { document: value.document }; break;
    case "windowOpened": case "presentationsChanged": payload = { window: windowFromNative(value.window) }; break;
    case "windowClosed": payload = { window: windowTarget(value.window) }; break;
    case "workspaceChanged": payload = { workspace: workspaceFromNative(value.workspace) }; break;
    case "activeTargetChanged": payload = {
      activeWindow: value.activeWindow === null ? null : windowTarget(value.activeWindow),
      activeTarget: value.activeTarget === null ? null : presentationTarget(value.activeTarget),
    }; break;
    default: throw new Error("unknown application event");
  }
  await listener(Object.freeze({ type: value.type, sequence: value.sequence, ...payload }));
};
