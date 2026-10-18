import {
  activeBuffer,
  storageRead, storageWrite, storageDelete, storageList,
  observeWorkbench, closePresentation,
  openDocument, createUntitled, presentDocument, listDocuments, getDocument, listWindows, getWindow, getWorkspace, openFolder, replaceWorkspaceRoots,
  invalidCommandArguments,
  keybinding,
  removeKeybinding,
  invokeCommand,
  registerCommand,
  registerViewCommand,
  selectedViewText,
  writeClipboardText,
  registerCompletionProvider,
  registerView,
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
  close: closePresentation,
  registerView,
  selectedText: selectedViewText,
  writeClipboardText,
});

export const documents = Object.freeze({ list: listDocuments, get: getDocument, open: openDocument, createUntitled, present: presentDocument });
export const windows = Object.freeze({ list: listWindows, get: getWindow });
export const workspaces = Object.freeze({ get: getWorkspace, openFolder, replaceRoots: replaceWorkspaceRoots });

export const storage = Object.freeze({ read: storageRead, write: storageWrite, delete: storageDelete, list: storageList });
