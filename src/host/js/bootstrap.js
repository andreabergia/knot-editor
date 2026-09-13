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

export async function activeBuffer() {
  return await request("activeBuffer");
}

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
