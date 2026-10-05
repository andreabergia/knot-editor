const nativeBindings = globalThis.__knotNativeBindings;
delete globalThis.__knotNativeBindings;

const hostErrorNames = Object.freeze({
  UnsupportedOperation: "UnsupportedOperationError",
  BufferClosed: "BufferClosedError",
  InvalidRange: "RangeError",
  InvalidEditBatch: "InvalidEditBatchError",
  RevisionConflict: "RevisionConflictError",
  ContributionSetNotFound: "ContributionSetNotFoundError",
  TreeViewNotFound: "TreeViewNotFoundError",
  ViewNotFound: "ViewNotFoundError",
  TreeProviderInUse: "TreeProviderInUseError",
  TreeProviderNotFound: "TreeProviderNotFoundError",
  CompletionProviderNotFound: "CompletionProviderNotFoundError",
  CommandNameInUse: "CommandNameInUseError",
  CommandNotFound: "CommandNotFoundError",
  Cancelled: "AbortError",
});

function hostError(error) {
  if (error === "Cancelled") {
    activeCommandFrames.at(-1)?.controller.abort(new Error("command cancelled"));
  }
  const exception = new Error(error);
  exception.name = hostErrorNames[error] ?? "KnotHostError";
  throw exception;
}

async function requestForInvocation(invocation, operation, ...arguments_) {
  try {
    return await nativeBindings.request(
      invocation,
      operation,
      ...arguments_,
    );
  } catch (error) {
    hostError(String(error));
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

class KnotAbortSignal {
  #aborted = false;
  #reason = undefined;
  #listeners = new Set();
  get aborted() { return this.#aborted; }
  get reason() { return this.#reason; }
  addEventListener(type, listener) {
    if (type === "abort") this.#listeners.add(listener);
  }
  removeEventListener(type, listener) {
    if (type === "abort") this.#listeners.delete(listener);
  }
  throwIfAborted() {
    if (this.#aborted) throw this.#reason;
  }
  abort(reason) {
    if (this.#aborted) return;
    this.#aborted = true;
    this.#reason = reason;
    for (const listener of this.#listeners) {
      listener.call(this, { type: "abort", target: this });
    }
    this.#listeners.clear();
  }
}

class KnotAbortController {
  signal = new KnotAbortSignal();
  abort(reason = new Error("command cancelled")) { this.signal.abort(reason); }
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
      const value = snapshotTable(this).byteAtUtf16[offset];
      if (value === undefined) {
        throw new RangeError("UTF-16 offset splits a surrogate pair or is out of bounds");
      }
      return value;
    },
    utf16OffsetAtByte(offset) {
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

  let contributionsDisposed = false;
  const contributions = Object.freeze({
    async replace(items, options) {
      if (contributionsDisposed) {
        const error = new Error("editor contribution set is disposed");
        error.name = "ContributionSetDisposedError";
        throw error;
      }
      await request(
        "replaceEditorContributions",
        handle,
        items,
        options?.ifRevision,
      );
    },
    async dispose() {
      if (contributionsDisposed) return;
      contributionsDisposed = true;
      await request("disposeEditorContributions", handle);
    },
  });

  buffer = Object.freeze({
    async snapshot(range) {
      return snapshotFromNative(await request("snapshot", handle, range));
    },
    async applyEdits(edits, options) {
      const revision = await request("applyEdits", handle, edits, options?.ifRevision);
      return Object.freeze({ revision });
    },
    async onDidChange(listener) {
      if (typeof listener !== "function") {
        throw new TypeError("onDidChange requires a listener");
      }
      const subscription = await request("subscribeBufferChanges", handle);
      bufferChangeListeners.set(subscription, { handle, listener });
      let disposed = false;
      return Object.freeze({
        dispose() {
          if (disposed) return;
          disposed = true;
          bufferChangeListeners.delete(subscription);
          void request("unsubscribeBufferChanges", subscription);
        },
      });
    },
    contributions,
  });
  buffers.set(handle, buffer);
  return buffer;
}

export async function activeBuffer() {
  const handle = await request("activeBuffer");
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
  let disposed = false;
  return Object.freeze({
    dispose() {
      if (disposed) return;
      disposed = true;
      commandHandlers.delete(registration);
      void request("unregisterCommand", registration);
    },
  });
}

export async function registerViewCommand(viewKind, name, handler) {
  if (typeof viewKind !== "string" || typeof name !== "string" || typeof handler !== "function") {
    throw new TypeError("commands.registerForView requires a view, command name, and handler");
  }
  const registration = await request("registerViewCommand", viewKind, name);
  commandHandlers.set(registration, handler);
  let disposed = false;
  return Object.freeze({
    dispose() {
      if (disposed) return;
      disposed = true;
      commandHandlers.delete(registration);
      void request("unregisterCommand", registration);
    },
  });
}

export async function selectedViewText() {
  return request("selectedViewText");
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
  validateCommandArguments(arguments_);
  const dispatch = await request("invokeCommand", name, arguments_);
  if (dispatch.kind === "outcome") return Object.freeze(dispatch.outcome);

  const outcome = await invokeRegisteredCommand(
    dispatch.invocation,
    dispatch.registration,
    activeCommandFrames.at(-1)?.buffer ?? null,
    arguments_,
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
  if (value === null || typeof value === "string" || typeof value === "boolean") return;
  if (typeof value === "number") {
    if (Number.isFinite(value)) return;
    throw new TypeError("command arguments require finite numbers");
  }
  if (typeof value !== "object") {
    throw new TypeError("command arguments must be JSON-compatible");
  }
  if (seen.has(value)) throw new TypeError("command arguments cannot contain cycles");
  seen.add(value);
  if (Array.isArray(value)) {
    for (const item of value) validateCommandArguments(item, seen);
  } else {
    for (const item of Object.values(value)) validateCommandArguments(item, seen);
  }
  seen.delete(value);
}

export function invalidCommandArguments(message) {
  const error = new Error(String(message));
  error.name = "InvalidCommandArgumentsError";
  throw error;
}

async function invokeRegisteredCommand(
  invocation,
  registration,
  activeHandle,
  commandArguments,
) {
  const handler = commandHandlers.get(registration);
  if (!handler) {
    return {
      kind: "handlerFailure",
      message: "Knot command registration is disposed",
    };
  }
  const controller = new KnotAbortController();
  const frame = { invocation, controller, buffer: activeHandle };
  activeCommandFrames.push(frame);
  try {
    try {
      await handler(Object.freeze({
        buffer: activeHandle === null ? null : bufferFor(activeHandle),
        arguments: commandArguments,
        signal: controller.signal,
      }));
      return controller.signal.aborted
        ? { kind: "cancelled" }
        : { kind: "completed" };
    } catch (error) {
      if (controller.signal.aborted || error?.name === "AbortError") {
        return { kind: "cancelled" };
      }
      const message = String(error?.stack ?? error);
      if (error?.name === "InvalidCommandArgumentsError") {
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
) => invokeRegisteredCommand(
  invocation,
  registration,
  activeHandle,
  commandArguments,
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
  let disposed = false;
  return Object.freeze({
    invalidate(parentId) {
      if (disposed) return;
      void request("invalidateTreeDataProvider", registration, parentId ?? null);
    },
    dispose() {
      if (disposed) return;
      disposed = true;
      treeProviders.delete(registration);
      void request("unregisterTreeDataProvider", registration);
    },
  });
}

globalThis.__knotRequestTreeChildren = async (registration, parentId, generation) => {
  const provider = treeProviders.get(registration);
  if (!provider) {
    return { error: "tree data provider is disposed" };
  }
  try {
    return { items: Array.from(await provider.getChildren(parentId)) };
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
  let disposed = false;
  return Object.freeze({
    dispose() {
      if (disposed) return;
      disposed = true;
      completionProviders.delete(registration);
      void request("unregisterCompletionProvider", registration);
    },
  });
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
      items: Array.from(provided, (item) => {
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
