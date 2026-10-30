import {
  activeBuffer,
  storageRead, storageWrite, storageDelete, storageList,
  recoveryList, recoveryRetain, recoveryRelease, recoveryRestore,
  observeWorkbench, closePresentation, replaceWindows, captureLayout,
  openDocument, createUntitled, presentDocument, listDocuments, getDocument, setDocumentLanguage, listWindows, getWindow, openWindow, focusWindow, getWorkspace, openFolder, replaceWorkspaceRoots, pickFolder, openTerminal,
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
  registerLanguage,
  associateLanguage,
  listLanguages,
  listGrammars,
  defineSetting, setSettings, getSetting, resetSetting, observeSetting,
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
  captureLayout,
  registerView,
  selectedText: selectedViewText,
  writeClipboardText,
  showPicker,
  showInput,
  showConfirmation,
  showMessage,
});

export const documents = Object.freeze({ list: listDocuments, get: getDocument, open: openDocument, createUntitled, present: presentDocument, setLanguage: setDocumentLanguage });
export const languages = Object.freeze({ register: registerLanguage, associate: associateLanguage, list: listLanguages, grammars: listGrammars });
export const settings = Object.freeze({ define: defineSetting, set: setSettings, get: getSetting, reset: resetSetting, observe: observeSetting });
export const terminals = Object.freeze({ open: openTerminal });
export const windows = Object.freeze({ list: listWindows, get: getWindow, open: openWindow, focus: focusWindow });
export const workspaces = Object.freeze({ get: getWorkspace, openFolder, replaceRoots: replaceWorkspaceRoots, pickFolder });

export const storage = Object.freeze({ read: storageRead, write: storageWrite, delete: storageDelete, list: storageList });
export const recovery = Object.freeze({ list: recoveryList, retain: recoveryRetain, release: recoveryRelease, restore: recoveryRestore });
