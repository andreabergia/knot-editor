import {
  activeBuffer,
  observeWorkbench,
  openDocument, listDocuments, getDocument, listWindows, getWindow, getWorkspace,
  invalidCommandArguments,
  keybinding,
  removeKeybinding,
  invokeCommand,
  registerCommand,
  registerViewCommand,
  selectedViewText,
  writeClipboardText,
  registerCompletionProvider,
  registerTreeDataProvider,
} from "knot:bootstrap";

export { keybinding, removeKeybinding };

export const editor = Object.freeze({
  activeBuffer,
  registerCompletionProvider,
});

export const commands = Object.freeze({
  invalidArguments: invalidCommandArguments,
  invoke: invokeCommand,
  register: registerCommand,
  registerForView: registerViewCommand,
});

export const workbench = Object.freeze({
  observe: observeWorkbench,
  registerTreeDataProvider,
  selectedText: selectedViewText,
  writeClipboardText,
});

export const documents = Object.freeze({ list: listDocuments, get: getDocument, open: openDocument });
export const windows = Object.freeze({ list: listWindows, get: getWindow });
export const workspaces = Object.freeze({ get: getWorkspace });
