import {
  activeBuffer,
  storageRead, storageWrite, storageDelete, storageList,
  recoveryList, recoveryRetain, recoveryRelease, recoveryRestore,
  observeWorkbench, closePresentation, replaceWindows,
  openDocument, createUntitled, presentDocument, listDocuments, getDocument, listWindows, getWindow, focusWindow, getWorkspace, openFolder, replaceWorkspaceRoots, pickFolder,
  invalidCommandArguments,
  keybinding,
  removeKeybinding,
  invokeCommand,
  registerCommand,
  registerViewCommand,
  selectedViewText,
  writeClipboardText,
  showPicker,
  showInput,
  showConfirmation,
  showMessage,
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
  replaceWindows,
  registerView,
  selectedText: selectedViewText,
  writeClipboardText,
  showPicker,
  showInput,
  showConfirmation,
  showMessage,
});

export const documents = Object.freeze({ list: listDocuments, get: getDocument, open: openDocument, createUntitled, present: presentDocument });
export const windows = Object.freeze({ list: listWindows, get: getWindow, focus: focusWindow });
export const workspaces = Object.freeze({ get: getWorkspace, openFolder, replaceRoots: replaceWorkspaceRoots, pickFolder });

export const storage = Object.freeze({ read: storageRead, write: storageWrite, delete: storageDelete, list: storageList });
export const recovery = Object.freeze({ list: recoveryList, retain: recoveryRetain, release: recoveryRelease, restore: recoveryRestore });
