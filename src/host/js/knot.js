import {
  activeBuffer,
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
  registerTreeDataProvider,
  selectedText: selectedViewText,
  writeClipboardText,
});
