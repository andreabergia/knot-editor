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
  TreeProviderInUse: "TreeProviderInUseError",
  TreeProviderNotFound: "TreeProviderNotFoundError",
  CompletionProviderNotFound: "CompletionProviderNotFoundError",
  CommandNameInUse: "CommandNameInUseError",
  CommandNotFound: "CommandNotFoundError",
  Cancelled: "AbortError",
});

function hostError(error) {
  const exception = new Error(error);
  exception.name = hostErrorNames[error] ?? "KnotHostError";
  throw exception;
}

async function request(operation, ...arguments_) {
  try {
    return await nativeBindings.request(operation, ...arguments_);
  } catch (error) {
    hostError(String(error));
  }
}

const buffers = new Map();
const bufferChangeListeners = new Map();
const snapshotTables = new WeakMap();

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
  return await request("registerCommand", name, handler);
}

export async function invokeCommand(name, commandArguments) {
  if (typeof name !== "string") {
    throw new TypeError("commands.invoke requires a command name");
  }
  return await request("invokeCommand", name, commandArguments);
}

export function invalidCommandArguments(message) {
  const error = new Error(String(message));
  error.name = "InvalidCommandArgumentsError";
  throw error;
}

export async function registerTreeDataProvider(viewId, provider) {
  if (typeof viewId !== "string" || typeof provider?.getChildren !== "function") {
    throw new TypeError("workbench.registerTreeDataProvider requires a view ID and getChildren provider");
  }
  return await request("registerTreeDataProvider", viewId, provider);
}

export async function registerCompletionProvider(label, provider) {
  if (typeof label !== "string" || typeof provider?.provideCompletions !== "function") {
    throw new TypeError("editor.registerCompletionProvider requires a label and provideCompletions provider");
  }
  return await request("registerCompletionProvider", label, provider);
}
