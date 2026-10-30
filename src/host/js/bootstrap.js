const nativeBindings = globalThis.__knotNativeBindings;
delete globalThis.__knotNativeBindings;

export function keybinding(key, command, options) {
  if (command !== null && typeof command === "object" && !Array.isArray(command)) {
    try {
      command = {
        name: command.name,
        arguments: command.arguments === undefined ? null : validateJsonValue(command.arguments),
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
function treeAction(value) {
  record(value, "tree action must be an object");
  return {
    label: string(value.label, "tree action label must be text"),
    command: string(value.command, "tree action command must be text"),
    arguments: value.arguments === undefined ? null : validateJsonValue(value.arguments),
  };
}
function treeActions(value) {
  return value == null ? [] : arrayItems(value, treeAction);
}
function treeItem(value) {
  record(value, "tree item must be an object");
  const collapsibleState = token(value.collapsibleState, ["none", "collapsed", "expanded"]);
  if (collapsibleState === null) throw new TypeError("collapsibleState is required");
  const kind = token(value.kind, ["item", "group"]);
  return {
    id: string(value.id, "tree item ID must be text"),
    label: string(value.label, "tree item label must be text"),
    description: optionalText(value.description),
    icon: token(value.icon, ["file", "folder", "symbol"]),
    collapsibleState,
    command: optionalText(value.command),
    kind: kind === null ? "item" : kind,
    statusText: optionalText(value.statusText),
    badge: token(value.badge, ["neutral", "info", "success", "warning", "error"]),
    actions: treeActions(value.actions),
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

function pickerItem(value) {
  record(value, "picker item must be an object");
  return {
    id: string(value.id, "picker item ID must be text"),
    label: string(value.label, "picker item label must be text"),
    description: optionalText(value.description),
  };
}

function transientTarget(options) {
  record(options, "transient options are required");
  return optionalWindowAuthority(options.target);
}

export async function showPicker(options) {
  const target = transientTarget(options);
  const title = string(options.title, "picker title must be text");
  const items = arrayItems(options.items, pickerItem);
  return request("showPicker", title, items, target);
}

export async function showInput(options) {
  const target = transientTarget(options);
  const title = string(options.title, "input title must be text");
  const value = optionalText(options.value);
  return request("showInput", title, value, target);
}

export async function showConfirmation(options) {
  const target = transientTarget(options);
  const title = string(options.title, "confirmation title must be text");
  const message = optionalText(options.message);
  const confirmLabel = optionalText(options.confirmLabel);
  const cancelLabel = optionalText(options.cancelLabel);
  return request("showConfirmation", title, message, confirmLabel, cancelLabel, target);
}

export async function showMessage(options) {
  const target = transientTarget(options);
  const title = string(options.title, "message title must be text");
  const message = optionalText(options.message);
  await request("showMessage", title, message, target);
}

export async function invokeCommand(name, commandArguments) {
  if (typeof name !== "string") {
    throw new TypeError("commands.invoke requires a command name");
  }
  const arguments_ = commandArguments === undefined ? null : commandArguments;
  const normalized = validateJsonValue(arguments_);
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

function validateJsonValue(value, seen = new Set()) {
  if (value === null || typeof value === "string" || typeof value === "boolean") return value;
  if (typeof value === "number") {
    if (Number.isFinite(value)) return value;
    throw new TypeError("JSON values require finite numbers");
  }
  if (typeof value !== "object") throw new TypeError("values must be JSON-compatible");
  if (seen.has(value)) throw new TypeError("JSON values cannot contain cycles");
  const prototype = Object.getPrototypeOf(value);
  if ((Array.isArray(value) ? prototype !== Array.prototype : prototype !== null && prototype !== Object.prototype) ||
      typeof value.toJSON === "function" || Object.getOwnPropertySymbols(value).length !== 0) {
    throw new TypeError("values require plain JSON data");
  }
  seen.add(value);
  let copy;
  if (Array.isArray(value)) {
    if (Object.keys(value).some((key) => {
      const index = Number(key);
      return !Number.isInteger(index) || index < 0 || index >= value.length || String(index) !== key;
    })) throw new TypeError("JSON arrays must contain only indexed data");
    copy = [];
    Object.defineProperty(copy, "toJSON", { value: undefined });
    for (let index = 0; index < value.length; index++) {
      if (!Object.hasOwn(value, index)) throw new TypeError("JSON arrays must be dense");
      copy.push(validateJsonValue(value[index], seen));
    }
  } else {
    copy = Object.create(null);
    for (const key of Object.keys(value)) copy[key] = validateJsonValue(value[key], seen);
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
    window: capture.window,
    windowTarget: windowTarget({ window: capture.window }),
    view: capture.view,
    tool: capture.tool,
  });
  if (target) targetAuthorities.set(target, capture.authority);
  const frame = { invocation, controller, buffer: activeHandle, capture, target };
  activeCommandFrames.push(frame);
  try {
    try {
      await handler(Object.freeze({
        target,
        buffer: activeHandle === null ? null : bufferFor(activeHandle),
        arguments: validateJsonValue(commandArguments),
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

export async function registerView(contribution) {
  const value = record(contribution, "workbench.registerView requires a contribution object");
  const id = string(value.id, "view id must be text");
  if (id.length === 0 || id.startsWith("knot.") || /\s/.test(id)
      || ["editor", "terminal", "workspace-tree"].includes(id)) {
    throw new TypeError("view id must be a unique, non-reserved namespaced string");
  }
  const title = string(value.title, "view title must be text");
  const content = record(value.content, "view content must be an object");
  let provider = null;
  if (content.kind === "tree") {
    provider = record(content.provider, "tree content requires a provider object");
    if (typeof provider.getChildren !== "function") {
      throw new TypeError("tree provider requires a getChildren method");
    }
  } else if (content.kind !== "text") {
    throw new TypeError("view content kind must be \"tree\" or \"text\"");
  }
  const actions = treeActions(value.actions);
  const language = optionalText(value.language);
  if (language !== null && content.kind !== "text") {
    throw new TypeError("view language applies only to text content");
  }
  let location = "left";
  let order = null;
  let visible = true;
  if (value.placement !== undefined) {
    const placement = record(value.placement, "view placement must be an object");
    if (placement.location !== undefined) {
      location = string(placement.location, "view placement location must be text");
      if (!["left", "right", "bottom", "document"].includes(location)) {
        throw new TypeError("view placement location is invalid");
      }
    }
    if (placement.order !== undefined) {
      order = safeInteger(placement.order);
    }
    if (placement.visible !== undefined) {
      if (typeof placement.visible !== "boolean") {
        throw new TypeError("view visible must be a boolean");
      }
      visible = placement.visible;
    }
  }
  const registration = await request(
    "registerView",
    id,
    title,
    location,
    order,
    visible ? 1 : 0,
    content.kind,
    actions,
    language,
  );
  if (provider !== null) treeProviders.set(registration, provider);
  const owned = ownership(() => treeProviders.delete(registration),
    () => request("unregisterView", registration));
  return Object.freeze({
    id,
    async show(window) {
      owned.assertActive();
      await owned.track(request("showView", registration, optionalWindowAuthority(window)));
    },
    async hide(window) {
      owned.assertActive();
      await owned.track(request("hideView", registration, optionalWindowAuthority(window)));
    },
    async focus(window) {
      owned.assertActive();
      await owned.track(request("focusView", registration, optionalWindowAuthority(window)));
    },
    async invalidate(parentId, window) {
      owned.assertActive();
      const parent = optionalText(parentId);
      await owned.track(request(
        "invalidateView",
        registration,
        parent,
        optionalWindowAuthority(window),
      ));
    },
    async setText(text, window) {
      owned.assertActive();
      await owned.track(request(
        "replaceViewText",
        registration,
        string(text, "view text must be text"),
        optionalWindowAuthority(window),
      ));
    },
    dispose: () => owned.dispose(),
  });
}

globalThis.__knotRequestTreeChildren = async (registration, window, instance, parentId, generation) => {
  const provider = treeProviders.get(registration);
  if (!provider) {
    return { error: "tree data provider is disposed" };
  }
  try {
    return { items: iterableItems(await provider.getChildren(parentId, Object.freeze({
      window: windowTarget({ window }),
      instance: string(instance, "view instance must be text"),
    })), treeItem) };
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
function optionalWindowAuthority(target) {
  return target == null ? null : windowAuthority(target);
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
      tabs: Object.freeze(pane.tabs.map(presentationFromNative)),
    }))),
  });
}
function presentationFromNative(tab) {
  return Object.freeze({
    target: presentationTarget(tab.target), kind: tab.kind, document: tab.document,
    buffer: tab.buffer === null ? null : bufferFor(tab.buffer), tool: tab.tool,
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
export async function closePresentation(target) {
  if (!livePresentations.has(target)) hostError("InvalidTarget");
  return Object.freeze(await request("closePresentation", destinationAuthority({ target })));
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

export async function openTerminal(options) {
  const cwd = options === undefined ? undefined : record(options, "terminal options must be an object").cwd;
  const tab = await request("openTerminal", cwd === undefined ? null : string(cwd, "terminal directory must be text"),
    destinationAuthority(options));
  return presentationFromNative(tab);
}

export async function setDocumentLanguage(id, language) {
  await request("setDocumentLanguage", string(id, "document identity must be text"),
    optionalText(language));
}

function textList(value, message) {
  return value === undefined ? [] : arrayItems(value, (item) => string(item, message));
}
function languageId(value) {
  const id = string(value, "language id must be text");
  if (id.length === 0 || /\s/.test(id)) {
    throw new TypeError("language id must be nonempty and contain no whitespace");
  }
  return id;
}
function languagePatterns(value) {
  const fileExtensions = textList(value.fileExtensions, "file extensions must be text");
  if (fileExtensions.some((extension) => extension.length === 0 || extension.startsWith("."))) {
    throw new TypeError("file extensions must be nonempty and omit the leading dot");
  }
  const fileNames = textList(value.fileNames, "file names must be text");
  const interpreters = textList(value.interpreters, "interpreters must be text");
  return [fileExtensions, fileNames, interpreters];
}
export async function associateLanguage(id, patterns) {
  const language = languageId(id);
  const claimed = languagePatterns(record(patterns, "languages.associate requires patterns"));
  const association = await request("associateLanguage", language, ...claimed);
  const owned = ownership(() => {}, () => request("unregisterLanguage", association));
  return Object.freeze({ dispose: () => owned.dispose() });
}
export async function registerLanguage(definition) {
  const value = record(definition, "languages.register requires a language object");
  const id = languageId(value.id);
  const name = string(value.name, "language name must be text");
  const grammar = string(value.grammar, "language grammar must be text");
  const [fileExtensions, fileNames, interpreters] = languagePatterns(value);
  const queries = record(value.queries, "language queries must be an object");
  const highlights = string(queries.highlights, "highlights query must be text");
  const injections = queries.injections === undefined
    ? ""
    : string(queries.injections, "injections query must be text");
  const registration = await request("registerLanguage", id, name, grammar, fileExtensions,
    fileNames, interpreters, highlights, injections);
  const owned = ownership(() => {}, () => request("unregisterLanguage", registration));
  return Object.freeze({ dispose: () => owned.dispose() });
}
const settingTypes = {
  boolean: (value) => typeof value === "boolean",
  integer: Number.isSafeInteger,
  number: Number.isFinite,
  string: (value) => typeof value === "string",
};
const settingListeners = new Map();
function settingKey(value) {
  if (typeof value !== "string" || value.length === 0) throw new TypeError("setting key must be nonempty text");
  return value;
}
function settingsScope(value) {
  if (value == null) return [null, null];
  record(value, "settings scope must be an object");
  return [optionalText(value.language), optionalText(value.view)];
}
export async function defineSetting(definition) {
  const value = record(definition, "settings.define requires a definition object");
  const key = settingKey(value.key);
  const type = string(value.type, "setting type must be text");
  if (!Object.hasOwn(settingTypes, type)) {
    throw new TypeError("setting type must be boolean, integer, number, or string");
  }
  if (!settingTypes[type](value.default)) throw new TypeError(`setting default must be of type ${type}`);
  const description = string(value.description, "setting description must be text");
  await request("defineSetting", key, type, value.default, description);
}
export async function setSettings(keyOrValues, valueOrScope, scope) {
  let values;
  if (typeof keyOrValues === "string") {
    values = { [settingKey(keyOrValues)]: valueOrScope };
  } else {
    values = record(keyOrValues, "settings.set requires a key or an object of values");
    scope = valueOrScope;
    Object.keys(values).forEach(settingKey);
  }
  const converted = Object.create(null);
  for (const key of Object.keys(values)) {
    if (values[key] === undefined) throw new TypeError("setting values must be JSON values");
    converted[key] = validateJsonValue(values[key]);
  }
  await request("setSettings", converted, ...settingsScope(scope));
}
export async function getSetting(key, facts) {
  const value = await request("getSetting", settingKey(key), ...settingsScope(facts));
  return value === null ? undefined : validateJsonValue(value);
}
export async function resetSetting(key, scope) {
  await request("resetSetting", settingKey(key), ...settingsScope(scope));
}
export async function observeSetting(key, listener) {
  settingKey(key);
  if (typeof listener !== "function") throw new TypeError("settings.observe requires a listener");
  const subscription = await request("observeSetting", key);
  settingListeners.set(subscription, listener);
  const owned = ownership(() => settingListeners.delete(subscription),
    () => request("unobserveSetting", subscription));
  return Object.freeze({ dispose: () => owned.dispose() });
}
globalThis.__knotDispatchSettingChange = async (subscription, key) => {
  const listener = settingListeners.get(subscription);
  if (listener) await listener(key);
};
export async function listLanguages() {
  return Object.freeze((await request("listLanguages")).map((language) => Object.freeze({
    ...language,
    fileExtensions: Object.freeze(language.fileExtensions),
    fileNames: Object.freeze(language.fileNames),
    interpreters: Object.freeze(language.interpreters),
  })));
}
const hexColor = /^#(?:[0-9a-fA-F]{6}|[0-9a-fA-F]{8})$/;
function themeColor(value, name) {
  if (typeof value !== "string" || !hexColor.test(value)) {
    throw new TypeError(`theme color ${name} must be #rrggbb or #rrggbbaa`);
  }
  return value;
}
function syntaxStyle(value, capture) {
  record(value, `syntax style ${capture} must be an object`);
  const style = {};
  for (const [field, entry] of Object.entries(value)) {
    if (entry === undefined) continue;
    if (field === "color") style.color = themeColor(entry, capture);
    else if (field === "fontStyle") {
      if (entry !== "normal" && entry !== "italic") {
        throw new TypeError(`syntax style ${capture} fontStyle must be "normal" or "italic"`);
      }
      style.fontStyle = entry;
    } else if (field === "fontWeight") {
      if (!Number.isInteger(entry) || entry < 100 || entry > 900) {
        throw new TypeError(`syntax style ${capture} fontWeight must be an integer from 100 to 900`);
      }
      style.fontWeight = entry;
    } else if (field === "underline" || field === "strikethrough") {
      if (typeof entry !== "boolean") throw new TypeError(`syntax style ${capture} ${field} must be boolean`);
      style[field] = entry;
    } else {
      throw new TypeError(`syntax style ${capture} has unknown field ${field}`);
    }
  }
  return style;
}
export async function registerTheme(definition) {
  const value = record(definition, "themes.register requires a theme object");
  const name = string(value.name, "theme name must be text");
  if (name.length === 0) throw new TypeError("theme name must be nonempty");
  if (value.appearance !== "dark" && value.appearance !== "light") {
    throw new TypeError('theme appearance must be "dark" or "light"');
  }
  const colors = {};
  for (const [key, color] of Object.entries(record(value.colors ?? {}, "theme colors must be an object"))) {
    if (color !== undefined) colors[key] = themeColor(color, key);
  }
  const syntax = {};
  for (const [capture, style] of Object.entries(record(value.syntax ?? {}, "theme syntax must be an object"))) {
    if (style !== undefined) syntax[capture] = syntaxStyle(style, capture);
  }
  const registration = await request("registerTheme",
    { name, appearance: value.appearance, colors, syntax });
  const owned = ownership(() => {}, () => request("unregisterTheme", registration));
  return Object.freeze({ dispose: () => owned.dispose() });
}
export async function getTheme(name) {
  const theme = await request("getTheme", string(name, "theme name must be text"));
  if (theme === null) return null;
  const syntax = Object.fromEntries(Object.entries(theme.syntax)
    .map(([capture, style]) => [capture, Object.freeze(style)]));
  return Object.freeze({ ...theme, colors: Object.freeze(theme.colors), syntax: Object.freeze(syntax) });
}
export async function listThemes() {
  return Object.freeze((await request("listThemes")).map((theme) => Object.freeze(theme)));
}
export async function defineColor(definition) {
  const value = record(definition, "themes.defineColor requires a definition object");
  const key = string(value.key, "color key must be text");
  if (key.length === 0) throw new TypeError("color key must be nonempty");
  const description = string(value.description, "color description must be text");
  const defaults = record(value.defaults, "color defaults must be an object");
  const fallback = (appearance) => {
    const entry = string(defaults[appearance], `color default ${appearance} must be text`);
    if (entry.length === 0 || (entry.startsWith("#") && !hexColor.test(entry))) {
      throw new TypeError(`color default ${appearance} must be #rrggbb, #rrggbbaa, or a color key`);
    }
    return entry;
  };
  await request("defineColor", key, description, fallback("dark"), fallback("light"));
}
export async function listGrammars() {
  return Object.freeze(await request("listGrammars"));
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
function workspaceDestination(options) {
  if (options === undefined) return null;
  const target = record(options, "workspace options must be an object").target;
  return target === undefined ? null : windowAuthority(target);
}
export async function openFolder(uri, options) {
  return workspaceFromNative(await request("openFolder", string(uri, "resource URI must be text"), workspaceDestination(options)));
}
export async function replaceWorkspaceRoots(roots, options) {
  const uris = arrayItems(roots, uri => string(uri, "workspace root URI must be text"));
  return workspaceFromNative(await request("replaceWorkspaceRoots", uris, workspaceDestination(options)));
}
export async function pickFolder(options) {
  const value = await request("pickFolder", workspaceDestination(options));
  return Object.freeze(value.kind === "selected"
    ? { kind: "selected", uri: value.uri }
    : { kind: "dismissed" });
}
export async function openWindow(options) {
  const roots = options === undefined
    ? undefined
    : record(options, "window options must be an object").roots;
  const uris = roots === undefined ? [] : arrayItems(roots, uri => string(uri, "workspace root URI must be text"));
  return windowFromNative(await request("openWindow", uris));
}
export async function focusWindow(target) {
  await request("focusWindow", windowAuthority(target));
}
function sessionEntry(value) {
  record(value, "session entry must be an object");
  const entry = {
    uri: optionalText(value.uri),
    document: optionalText(value.document),
    capture: optionalText(value.capture),
  };
  if (entry.uri === null && entry.document === null) {
    throw new TypeError("session entry requires a uri or document");
  }
  return entry;
}
function optionalIndex(value) {
  return value == null ? null : safeInteger(value);
}
export async function replaceWindows(options) {
  record(options, "replaceWindows requires options");
  const suspend = arrayItems(options.suspend, (value) => {
    record(value, "suspended document must be an object");
    return {
      document: string(value.document, "document identity must be text"),
      capture: optionalText(value.capture),
    };
  });
  const windows = arrayItems(options.windows, (value) => {
    record(value, "destination window must be an object");
    return {
      roots: arrayItems(value.roots, (uri) => string(uri, "workspace root URI must be text")),
      entries: arrayItems(value.entries, sessionEntry),
      selected: optionalIndex(value.selected),
      layout: value.layout == null ? null : validateJsonValue(value.layout),
    };
  });
  const release = options.release == null ? [] : arrayItems(options.release, windowAuthority);
  const value = await request("replaceWindows", {
    suspend, windows, release, focus: optionalIndex(options.focus),
  });
  return Object.freeze({
    protected: value.protected,
    windows: Object.freeze(value.windows.map((window) => Object.freeze({
      window: windowTarget(window.window),
      entries: Object.freeze(window.entries.map((entry) => Object.freeze(entry))),
      layout: window.layout,
    }))),
  });
}
export async function captureLayout(target) {
  return validateJsonValue(await request("captureLayout", windowAuthority(target)));
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
    launched: value.launched,
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
    case "windowClosed": case "viewStateChanged": payload = { window: windowTarget(value.window) }; break;
    case "launched": payload = {}; break;
    case "workspaceChanged": payload = { workspace: workspaceFromNative(value.workspace) }; break;
    case "activeTargetChanged": payload = {
      activeWindow: value.activeWindow === null ? null : windowTarget(value.activeWindow),
      activeTarget: value.activeTarget === null ? null : presentationTarget(value.activeTarget),
    }; break;
    default: throw new Error("unknown application event");
  }
  await listener(Object.freeze({ type: value.type, sequence: value.sequence, ...payload }));
};

export async function storageRead(key) {
  const result = await request("storageRead", string(key, "storage key must be text"));
  return Object.freeze(result.found ? { found: true, value: validateJsonValue(result.value) } : { found: false });
}
export async function storageWrite(key, value) {
  await request("storageWrite", string(key, "storage key must be text"), validateJsonValue(value));
}
export async function storageDelete(key) {
  await request("storageDelete", string(key, "storage key must be text"));
}
export async function storageList() {
  return Object.freeze(await request("storageList"));
}

function captureFromNative(value) {
  return Object.freeze(value);
}
export async function recoveryList() {
  return Object.freeze((await request("recoveryList")).map(captureFromNative));
}
export async function recoveryRetain(document) {
  return captureFromNative(await request("recoveryRetain", string(document, "document identity must be text")));
}
export async function recoveryRelease(id) {
  return await request("recoveryRelease", string(id, "capture identity must be text"));
}
export async function recoveryRestore(id, options) {
  const value = await request("recoveryRestore", string(id, "capture identity must be text"), destinationAuthority(options));
  const presentation = documentPresentationFromNative(value.presentation);
  return Object.freeze(value.outcome === "restored"
    ? { outcome: "restored", resource: value.resource, ...presentation }
    : { outcome: "preserved", owner: value.owner, ...presentation });
}
