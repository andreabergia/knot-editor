import {
  activeBuffer,
  invalidCommandArguments,
  invokeCommand,
  registerCommand,
  registerCompletionProvider,
  registerTreeDataProvider,
} from "knot:bootstrap";

export const editor = Object.freeze({
  activeBuffer,
  registerCompletionProvider,
});

export const commands = Object.freeze({
  invalidArguments: invalidCommandArguments,
  invoke: invokeCommand,
  register: registerCommand,
});

export const workbench = Object.freeze({
  registerTreeDataProvider,
});
