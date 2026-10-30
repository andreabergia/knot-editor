// An in-memory Knot host for extension tests. Under `node --test`, extension
// code importing "knot" receives `api` (through ./knot.ts), while tests import
// "knot/testing" to arrange windows and documents, script prompt answers, run
// commands, emit application events, and inspect what the extension did.
//
// The fake implements behavior, not mocks, but only as much as tests need:
// anything else throws `not faked`. It does not reproduce host validation,
// scheduling, or durability; product tests cover the real boundary.
import type * as Knot from "knot";

type HostErrorCode = Knot.HostErrorCode;

/** Every value the "knot" module exports, without its private identity brands. */
type KnotApi = {
  [K in keyof typeof Knot as (typeof Knot)[K] extends symbol ? never : K]: (typeof Knot)[K];
};

export type FakeDocument = {
  id: Knot.DocumentId;
  title: string;
  kind: Knot.DocumentInfo["kind"];
  resourceUri: string | null;
  dirty: boolean;
  revision: number;
  text: string;
  language: string;
};

type FakeTab = { target: Knot.ViewTarget; document: Knot.DocumentId };

type FakeWindow = {
  target: Knot.WindowTarget;
  pane: Knot.PaneTarget;
  roots: Knot.WorkspaceRoot[];
  generation: number;
  tabs: FakeTab[];
  selected: number;
};

type FakeView = {
  contribution: Knot.ViewContribution;
  visible: boolean;
  text: string;
  invalidations: number;
};

type Answers = {
  showPicker: Knot.PickerResult;
  showInput: Knot.InputResult;
  showConfirmation: Knot.ConfirmationResult;
  pickFolder: Knot.FolderPickerResult;
};

type Prompt = { kind: keyof Answers; options: unknown };

type Handler = Knot.CommandHandler;

/** Everything the fake host holds. `reset()` replaces it. */
function emptyState() {
  return {
    nextId: 1,
    records: new Map<string, string>(),
    documents: new Map<Knot.DocumentId, FakeDocument>(),
    windows: [] as FakeWindow[],
    focused: null as Knot.WindowTarget | null,
    captures: new Map<Knot.CaptureId, Knot.RetainedCapture & { text: string }>(),
    views: new Map<string, FakeView>(),
    commands: new Map<string, Handler>(),
    viewCommands: new Map<string, Handler>(),
    listeners: [] as ((event: Knot.ApplicationEvent) => void | Promise<void>)[],
    sequence: 0,
    launched: false,
    answers: new Map<keyof Answers, unknown[]>(),
    prompts: [] as Prompt[],
    messages: [] as Knot.MessageOptions[],
    clipboard: null as string | null,
    selectedText: null as string | null,
    failures: new Map<string, HostErrorCode[]>(),
    replaced: [] as Knot.ReplaceWindowsOptions[],
    languages: [] as Knot.LanguageDefinition[],
    associations: [] as { id: string; patterns: Knot.LanguagePatterns }[],
    settingDefinitions: new Map<string, Knot.SettingDefinition>([
      ["editor.tabSize", { key: "editor.tabSize", type: "integer", default: 4, description: "Columns per tab stop in editor views" }],
      ["theme.mode", { key: "theme.mode", type: "string", default: "dark", description: "Which theme applies" }],
      ["theme.dark", { key: "theme.dark", type: "string", default: "One Dark", description: "The theme used in dark mode" }],
      ["theme.light", { key: "theme.light", type: "string", default: "One Light", description: "The theme used in light mode" }],
    ]),
    settingValues: [] as FakeSettingValue[],
    settingListeners: [] as { key: string; listener: (key: Knot.SettingKey) => void | Promise<void> }[],
    themes: [] as Knot.ThemeDefinition[],
    colors: new Map<string, Knot.ColorDefinition>(),
  };
}

type FakeSettingValue = {
  key: string;
  language: string | null;
  view: string | null;
  value: Knot.JsonValue;
};

let state = emptyState();

/** Clears every record, window, registration, and scripted answer. */
export function reset() {
  state = emptyState();
}

function nextId(prefix: string) {
  return `${prefix}-${state.nextId++}`;
}

export function hostError(code: HostErrorCode, message: string = code): Knot.HostError {
  const error = new Error(message) as Error & { name: string; code: HostErrorCode };
  error.name = "KnotHostError";
  error.code = code;
  return error as Knot.HostError;
}

function notFaked(name: string): (...args: any[]) => never {
  return () => {
    throw new Error(`not faked: ${name}`);
  };
}

/** Makes the next call to `operation` (for example "storage.write") reject with `code`. */
export function failNext(operation: string, code: HostErrorCode) {
  const queued = state.failures.get(operation) ?? [];
  queued.push(code);
  state.failures.set(operation, queued);
}

function maybeFail(operation: string) {
  const code = state.failures.get(operation)?.shift();
  if (code !== undefined) throw hostError(code, `${operation} failed`);
}

const json = (value: Knot.JsonValue): Knot.JsonValue => JSON.parse(JSON.stringify(value));

// Records ------------------------------------------------------------------

/** Every stored record, parsed. */
export function records(): Record<string, Knot.JsonValue> {
  return Object.fromEntries([...state.records].map(([key, value]) => [key, JSON.parse(value)]));
}

export function record(key: string): Knot.JsonValue | undefined {
  const value = state.records.get(key);
  return value === undefined ? undefined : JSON.parse(value);
}

export function setRecord(key: string, value: Knot.JsonValue) {
  state.records.set(key, JSON.stringify(value));
}

// Documents and windows ----------------------------------------------------

function buffer(document: FakeDocument): Knot.Buffer {
  return {
    snapshot: async () => ({ text: document.text, revision: document.revision }),
  } as unknown as Knot.Buffer;
}

function documentInfo(document: FakeDocument): Knot.DocumentInfo {
  const { text: _text, ...info } = document;
  return { ...info, buffer: buffer(document) };
}

/** Adds a live document. A URI makes it persisted unless a kind is given. */
export function addDocument(
  options: {
    uri?: string;
    kind?: FakeDocument["kind"];
    dirty?: boolean;
    text?: string;
    language?: string;
  } = {},
): Knot.DocumentId {
  const id = nextId("doc") as Knot.DocumentId;
  const resourceUri = options.uri ?? null;
  state.documents.set(id, {
    id,
    title: resourceUri?.split("/").pop() ?? "Untitled",
    kind: options.kind ?? (resourceUri === null ? "untitled" : "persisted"),
    resourceUri,
    dirty: options.dirty ?? false,
    revision: 0,
    text: options.text ?? "",
    language: options.language ?? "plaintext",
  });
  return id;
}

export function document(id: Knot.DocumentId): FakeDocument {
  const found = state.documents.get(id);
  if (found === undefined) throw new Error(`no document ${id}`);
  return found;
}

export function documentIds(): Knot.DocumentId[] {
  return [...state.documents.keys()];
}

function rootsFor(uris: readonly string[], previous: Knot.WorkspaceRoot[] = []) {
  const unique = [...new Set(uris)];
  return unique.map(
    (uri) =>
      previous.find((root) => root.uri === uri) ?? { id: nextId("root") as Knot.RootId, uri },
  );
}

/** Opens a window presenting the documents as tabs of one pane, the first selected. */
export function openWindow(
  options: { roots?: readonly string[]; documents?: readonly Knot.DocumentId[] } = {},
): Knot.WindowTarget {
  const target = { id: nextId("window") } as unknown as Knot.WindowTarget;
  const pane = { window: target } as unknown as Knot.PaneTarget;
  const window: FakeWindow = {
    target,
    pane,
    roots: rootsFor(options.roots ?? []),
    generation: 0,
    tabs: [],
    selected: 0,
  };
  state.windows.push(window);
  for (const id of options.documents ?? []) present(target, id);
  return target;
}

function fakeWindow(target: Knot.WindowTarget): FakeWindow {
  const found = state.windows.find((window) => window.target === target);
  if (found === undefined) throw hostError("InvalidTarget", "window is closed");
  return found;
}

/** Adds a tab for the document to the window and selects it. */
export function present(target: Knot.WindowTarget, id: Knot.DocumentId) {
  const window = fakeWindow(target);
  const view = { window: target, pane: window.pane } as unknown as Knot.ViewTarget;
  window.tabs.push({ target: view, document: id });
  window.selected = window.tabs.length - 1;
}

/** Closes a window; documents no longer presented anywhere close with it. */
export function closeWindow(target: Knot.WindowTarget) {
  state.windows = state.windows.filter((window) => window.target !== target);
  if (state.focused === target) state.focused = null;
  for (const id of [...state.documents.keys()]) {
    if (!state.windows.some((window) => window.tabs.some((tab) => tab.document === id))) {
      state.documents.delete(id);
    }
  }
}

export function setRoots(target: Knot.WindowTarget, roots: readonly string[]): Knot.WorkspaceInfo {
  const window = fakeWindow(target);
  window.roots = rootsFor(roots, window.roots);
  window.generation += 1;
  return workspaceInfo(window);
}

export function windowTargets(): Knot.WindowTarget[] {
  return state.windows.map((window) => window.target);
}

export function rootsOf(target: Knot.WindowTarget): string[] {
  return fakeWindow(target).roots.map((root) => root.uri);
}

export function documentsIn(target: Knot.WindowTarget): Knot.DocumentId[] {
  return fakeWindow(target).tabs.map((tab) => tab.document);
}

export function focusedWindow() {
  return state.focused;
}

function workspaceInfo(window: FakeWindow): Knot.WorkspaceInfo {
  return { window: window.target, roots: [...window.roots], generation: window.generation };
}

function windowInfo(window: FakeWindow): Knot.WindowInfo {
  const tabs = window.tabs.map((tab) => {
    const document = state.documents.get(tab.document);
    return {
      target: tab.target,
      kind: "document" as const,
      document: tab.document,
      buffer: document === undefined ? null : buffer(document),
      tool: null,
    };
  });
  const selected = window.tabs[window.selected]?.target ?? null;
  return {
    target: window.target,
    panes: [{ target: window.pane, tabs, selected: selected as Knot.ViewTarget }],
    currentTarget: selected,
    workspace: workspaceInfo(window),
  };
}

// Recovery -----------------------------------------------------------------

export function captureIds(): Knot.CaptureId[] {
  return [...state.captures.keys()];
}

export function captureText(id: Knot.CaptureId): string | undefined {
  return state.captures.get(id)?.text;
}

// Prompts, messages, and views ---------------------------------------------

/** Queues the result of the next prompt of this kind. */
export function answer<K extends keyof Answers>(kind: K, result: Answers[K]) {
  const queued = state.answers.get(kind) ?? [];
  queued.push(result);
  state.answers.set(kind, queued);
}

function prompt<K extends keyof Answers>(kind: K, options: unknown): Answers[K] {
  state.prompts.push({ kind, options });
  const result = state.answers.get(kind)?.shift();
  if (result === undefined) throw new Error(`no scripted answer for ${kind}`);
  return result as Answers[K];
}

export function prompts(): readonly Prompt[] {
  return state.prompts;
}

/** Messages shown so far, as `title: message` strings. */
export function messages(): string[] {
  return state.messages.map((options) => options.message ?? options.title);
}

export function clipboard() {
  return state.clipboard;
}

export function setSelectedText(text: string | null) {
  state.selectedText = text;
}

export function view(id: string): FakeView {
  const found = state.views.get(id);
  if (found === undefined) throw new Error(`no view ${id}`);
  return found;
}

/** The tree view's children of `parentId`, as presented to `window`. */
export async function treeItems(
  id: string,
  window: Knot.WindowTarget,
  parentId: string | null = null,
): Promise<Knot.TreeItem[]> {
  const content = view(id).contribution.content;
  if (content.kind !== "tree") throw new Error(`${id} is not a tree`);
  const instance = `${id}@window` as Knot.ViewId;
  return [...(await content.provider.getChildren(parentId, { window, instance }))];
}

// Commands and events ------------------------------------------------------

class InvalidArguments extends Error {}

function context(window: Knot.WindowTarget, args: Knot.JsonValue, tool: string | null): Knot.CommandContext {
  const signal = {
    aborted: false,
    reason: undefined,
    addEventListener() {},
    removeEventListener() {},
    throwIfAborted() {},
  };
  const target = { window: "window", windowTarget: window, view: null, tool } as unknown as Knot.CommandTarget;
  return { target, buffer: null, arguments: args, signal };
}

/**
 * Runs a command as if invoked in `window`, through the view-claimed handler
 * when `view` names one. Handler errors propagate to the test.
 */
export async function run(
  name: string,
  options: { window?: Knot.WindowTarget; arguments?: Knot.JsonValue; view?: string } = {},
) {
  const window = options.window ?? state.windows[0]?.target;
  if (window === undefined) throw new Error("run needs a window");
  const viewHandler =
    options.view === undefined ? undefined : state.viewCommands.get(`${options.view}:${name}`);
  const handler = viewHandler ?? state.commands.get(name);
  if (handler === undefined) throw new Error(`no command ${name}`);
  await handler(context(window, options.arguments ?? null, options.view ?? null));
}

export function commandNames(): string[] {
  return [...state.commands.keys()];
}

/** Delivers an application event to every observer, awaiting each in turn. */
export async function emit(change: Knot.ApplicationChange) {
  state.sequence += 1;
  for (const listener of state.listeners) await listener({ ...change, sequence: state.sequence });
}

/** Marks startup complete and delivers the launched event. */
export async function launch() {
  state.launched = true;
  await emit({ type: "launched" });
}

export function replaced(): readonly Knot.ReplaceWindowsOptions[] {
  return state.replaced;
}

// Session replacement ------------------------------------------------------

function restoreEntry(entry: Knot.SessionEntry): { id: Knot.DocumentId | null; outcome: Knot.SessionEntryOutcome } {
  if (entry.document != null && state.documents.has(entry.document)) {
    return { id: entry.document, outcome: { kind: "live", document: entry.document } };
  }
  if (entry.capture != null) {
    const capture = state.captures.get(entry.capture);
    if (capture !== undefined) {
      const id = addDocument({
        ...(capture.resourceUri === null ? {} : { uri: capture.resourceUri }),
        kind: capture.kind,
        dirty: true,
        text: capture.text,
      });
      return { id, outcome: { kind: "restored", document: id, resource: capture.resourceUri === null ? "untitled" : "unchanged" } };
    }
  }
  if (entry.uri != null) {
    const live = [...state.documents.values()].find((document) => document.resourceUri === entry.uri);
    if (live !== undefined) return { id: live.id, outcome: { kind: "live", document: live.id } };
    const id = addDocument({ uri: entry.uri });
    return { id, outcome: { kind: "opened", document: id } };
  }
  return { id: null, outcome: { kind: "missing" } };
}

/**
 * A minimal commit: closes the released windows, closes suspended documents
 * that are no longer presented, and opens each requested window with its
 * entries resolved live first, then from captures, then by URI.
 */
function replaceWindows(options: Knot.ReplaceWindowsOptions): Knot.ReplaceWindowsResult {
  maybeFail("workbench.replaceWindows");
  state.replaced.push(options);
  const suspended = new Set(options.suspend.map((entry) => entry.document));
  state.windows = state.windows.filter((window) => !(options.release ?? []).includes(window.target));
  for (const id of suspended) {
    if (!state.windows.some((window) => window.tabs.some((tab) => tab.document === id))) {
      state.documents.delete(id);
    }
  }
  const opened = options.windows.map((request) => {
    const target = openWindow({ roots: request.roots });
    const entries = request.entries.map((entry) => {
      const { id, outcome } = restoreEntry(entry);
      if (id !== null) present(target, id);
      return outcome;
    });
    return { window: target, entries, layout: request.layout == null ? null : ("applied" as const) };
  });
  const focus = options.focus == null ? undefined : opened[options.focus];
  if (focus !== undefined) state.focused = focus.window;
  return { windows: opened, protected: true };
}

// Languages ----------------------------------------------------------------

/** The grammars compiled into Knot, as `languages.grammars()` lists them. */
const GRAMMARS = [
  "bash", "c", "cpp", "css", "go", "html", "java", "javascript", "json", "markdown",
  "markdown_inline", "python", "query", "rust", "toml", "tsx", "typescript", "yaml",
];

/** The names a highlight capture resolves to, as `src/app/theme.rs` lists
 * them; queries should use these exactly so no capture is silently ignored. */
export const CAPTURE_NAMES: readonly string[] = [
  "attribute", "attribute.builtin", "boolean", "character", "character.special", "comment",
  "comment.documentation", "comment.error", "comment.note", "comment.todo", "comment.warning",
  "constant", "constant.builtin", "constant.macro", "constructor", "diff.delta", "diff.minus",
  "diff.plus", "function", "function.builtin", "function.call", "function.macro",
  "function.method", "function.method.call", "keyword", "keyword.conditional",
  "keyword.conditional.ternary", "keyword.coroutine", "keyword.debug", "keyword.directive",
  "keyword.directive.define", "keyword.exception", "keyword.function", "keyword.import",
  "keyword.modifier", "keyword.operator", "keyword.repeat", "keyword.return", "keyword.type",
  "label", "markup.heading", "markup.heading.1", "markup.heading.2", "markup.heading.3",
  "markup.heading.4", "markup.heading.5", "markup.heading.6", "markup.italic", "markup.link",
  "markup.link.label", "markup.link.url", "markup.list", "markup.list.checked",
  "markup.list.unchecked", "markup.math", "markup.quote", "markup.raw", "markup.raw.block",
  "markup.strikethrough", "markup.strong", "markup.underline", "module", "module.builtin",
  "number", "number.float", "operator", "property", "punctuation.bracket",
  "punctuation.delimiter", "punctuation.special", "string", "string.documentation",
  "string.escape", "string.regexp", "string.special", "string.special.path",
  "string.special.symbol", "string.special.url", "tag", "tag.attribute", "tag.builtin",
  "tag.delimiter", "type", "type.builtin", "type.definition", "variable", "variable.builtin",
  "variable.member", "variable.parameter", "variable.parameter.builtin",
];

/** The color keys native code defines, as the native table lists them. */
export const NATIVE_COLOR_KEYS: readonly string[] = [
  "background", "border", "border.hover", "button.background", "button.hover.background",
  "dropTarget.background", "editor.background", "editor.cursor", "editor.foreground",
  "editor.markedText", "editor.selection", "error", "info", "list.group.background",
  "list.hover.background", "list.match.foreground", "list.selected.background", "list.selected.foreground",
  "popover.background", "popover.border", "scrim", "scrollbar.thumb", "success",
  "tab.active.background", "tab.active.foreground", "tab.bar.background",
  "tab.inactive.background", "tab.inactive.foreground", "terminal.ansi.black",
  "terminal.ansi.blue", "terminal.ansi.brightBlack", "terminal.ansi.brightBlue",
  "terminal.ansi.brightCyan", "terminal.ansi.brightGreen", "terminal.ansi.brightMagenta",
  "terminal.ansi.brightRed", "terminal.ansi.brightWhite", "terminal.ansi.brightYellow",
  "terminal.ansi.cyan", "terminal.ansi.green", "terminal.ansi.magenta", "terminal.ansi.red",
  "terminal.ansi.white", "terminal.ansi.yellow", "terminal.background", "terminal.cursor",
  "terminal.foreground", "terminal.selection", "text", "text.accent", "text.muted", "warning",
];

/** Every live theme registration, in registration order. */
export function themeDefinitions(): readonly Knot.ThemeDefinition[] {
  return state.themes;
}

/** Every live extension color definition, in definition order. */
export function colorDefinitions(): readonly Knot.ColorDefinition[] {
  return [...state.colors.values()];
}

/** Every live association, in the order it was made. The fake does not detect. */
export function languageAssociations(): readonly { id: string; patterns: Knot.LanguagePatterns }[] {
  return state.associations;
}

/** Every live language registration, queries included, in registration order. */
export function languageDefinitions(): readonly Knot.LanguageDefinition[] {
  return state.languages;
}

function languageInfo(definition: Knot.LanguageDefinition): Knot.LanguageInfo {
  return {
    id: definition.id,
    name: definition.name,
    grammar: definition.grammar,
    fileExtensions: definition.fileExtensions ?? [],
    fileNames: definition.fileNames ?? [],
    interpreters: definition.interpreters ?? [],
  };
}

// Settings -----------------------------------------------------------------

const settingTypes: Record<Knot.SettingType, (value: unknown) => boolean> = {
  boolean: (value) => typeof value === "boolean",
  integer: (value) => Number.isSafeInteger(value),
  number: (value) => Number.isFinite(value),
  string: (value) => typeof value === "string",
};

function settingScope(scope: Knot.SettingsScope | undefined) {
  return { language: scope?.language ?? null, view: scope?.view ?? null };
}

function sameScope(entry: FakeSettingValue, scope: ReturnType<typeof settingScope>) {
  return entry.language === scope.language && entry.view === scope.view;
}

async function notifySetting(key: string) {
  for (const entry of [...state.settingListeners]) {
    if (entry.key === key) await entry.listener(key as Knot.SettingKey);
  }
}

/** The definitions packages made, keyed by setting key. */
export function settingDefinitions(): ReadonlyMap<string, Knot.SettingDefinition> {
  return state.settingDefinitions;
}

// The "knot" module --------------------------------------------------------

const LAYOUT: Knot.WindowLayout = {
  version: 1,
  geometry: null,
  root: null,
  focused: null,
  dockSizes: { left: 0, right: 0, bottom: 0 },
};

export const api: KnotApi = {
  documents: {
    list: async () => [...state.documents.values()].map(documentInfo),
    get: async (id) => {
      const document = state.documents.get(id);
      return document === undefined ? null : documentInfo(document);
    },
    open: notFaked("documents.open"),
    createUntitled: notFaked("documents.createUntitled"),
    present: notFaked("documents.present"),
    setLanguage: async (id, language) => {
      const document = state.documents.get(id);
      if (document === undefined) throw hostError("DocumentNotFound");
      // The fake neither detects nor checks registration.
      document.language = language ?? "plaintext";
    },
  },
  languages: {
    // Queries are not compiled; product tests cover InvalidQuery.
    register: async (definition) => {
      if (!GRAMMARS.includes(definition.grammar)) throw hostError("UnknownGrammar");
      state.languages.push(definition);
      return {
        dispose: async () => {
          state.languages = state.languages.filter((candidate) => candidate !== definition);
        },
      };
    },
    associate: async (id, patterns) => {
      const association = { id, patterns };
      state.associations.push(association);
      return {
        dispose: async () => {
          state.associations = state.associations.filter((candidate) => candidate !== association);
        },
      };
    },
    // Later registrations win, as in the native registry.
    list: async () => {
      const byId = new Map(state.languages.map((definition) => [definition.id, definition]));
      // Code-unit order, as the native registry sorts IDs, not locale order.
      return [...byId.values()]
        .map(languageInfo)
        .sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
    },
    grammars: async () => GRAMMARS,
  },
  // Values are kept in write order, so the latest entry wins a tie. The fake
  // keeps mismatching pending values rather than reporting them.
  settings: {
    define: async (definition) => {
      if (state.settingDefinitions.has(definition.key)) throw hostError("DuplicateSetting");
      state.settingDefinitions.set(definition.key, definition);
      const accepts = settingTypes[definition.type];
      state.settingValues = state.settingValues.filter(
        (entry) => entry.key !== definition.key || accepts(entry.value),
      );
    },
    set: async (keyOrValues: string | Partial<Knot.Settings>, valueOrScope?: unknown, scope?: Knot.SettingsScope) => {
      const values = (typeof keyOrValues === "string"
        ? { [keyOrValues]: valueOrScope }
        : keyOrValues) as Record<string, Knot.JsonValue>;
      const facts = settingScope(typeof keyOrValues === "string" ? scope : (valueOrScope as Knot.SettingsScope));
      for (const [key, value] of Object.entries(values)) {
        const definition = state.settingDefinitions.get(key);
        if (definition !== undefined && !settingTypes[definition.type](value)) {
          throw hostError("SettingTypeMismatch");
        }
      }
      for (const [key, value] of Object.entries(values)) {
        state.settingValues = state.settingValues.filter((entry) => entry.key !== key || !sameScope(entry, facts));
        state.settingValues.push({ key, ...facts, value: json(value) });
        await notifySetting(key);
      }
    },
    get: async (key, facts) => {
      const definition = state.settingDefinitions.get(key);
      if (definition === undefined) return undefined;
      const context = settingScope(facts);
      let best: FakeSettingValue | undefined;
      let bestFacts = -1;
      for (const entry of state.settingValues) {
        if (entry.key !== key) continue;
        if (entry.language !== null && entry.language !== context.language) continue;
        if (entry.view !== null && entry.view !== context.view) continue;
        const count = Number(entry.language !== null) + Number(entry.view !== null);
        if (count >= bestFacts) {
          best = entry;
          bestFacts = count;
        }
      }
      return (best === undefined ? definition.default : best.value) as never;
    },
    reset: async (key, scope) => {
      const facts = settingScope(scope);
      const before = state.settingValues.length;
      state.settingValues = state.settingValues.filter((entry) => entry.key !== key || !sameScope(entry, facts));
      if (state.settingValues.length !== before) await notifySetting(key);
    },
    observe: async (key, listener) => {
      const entry = { key, listener };
      state.settingListeners.push(entry);
      return {
        dispose: async () => {
          state.settingListeners = state.settingListeners.filter((candidate) => candidate !== entry);
        },
      };
    },
  },
  // The fake neither validates colors nor selects a theme.
  themes: {
    register: async (definition) => {
      const unknown = Object.keys(definition.syntax ?? {}).find((name) =>
        !CAPTURE_NAMES.some((capture) => capture === name || capture.startsWith(`${name}.`)));
      if (unknown !== undefined) throw hostError("UnknownCapture");
      state.themes.push(definition);
      return {
        dispose: async () => {
          state.themes = state.themes.filter((candidate) => candidate !== definition);
        },
      };
    },
    // Later registrations win, as in the native registry.
    get: async (name) => {
      const definition = [...state.themes].reverse().find((candidate) => candidate.name === name);
      return definition === undefined
        ? null
        : { ...definition, colors: definition.colors ?? {}, syntax: definition.syntax ?? {} };
    },
    list: async () => {
      const byName = new Map(state.themes.map((definition) => [definition.name, definition.appearance]));
      return [...byName]
        .map(([name, appearance]) => ({ name, appearance }))
        .sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
    },
    defineColor: async (definition) => {
      if (NATIVE_COLOR_KEYS.includes(definition.key) || state.colors.has(definition.key)) {
        throw hostError("DuplicateColor");
      }
      state.colors.set(definition.key, definition);
    },
  },
  windows: {
    list: async () => state.windows.map(windowInfo),
    get: async (target) => {
      const window = state.windows.find((candidate) => candidate.target === target);
      return window === undefined ? null : windowInfo(window);
    },
    open: async (options = {}) => {
      maybeFail("windows.open");
      const target = openWindow({ roots: options.roots ?? [] });
      state.focused = target;
      return windowInfo(fakeWindow(target));
    },
    focus: async (target) => {
      fakeWindow(target);
      state.focused = target;
    },
  },
  workspaces: {
    get: async (target) => {
      const window = state.windows.find((candidate) => candidate.target === target);
      return window === undefined ? null : workspaceInfo(window);
    },
    openFolder: notFaked("workspaces.openFolder"),
    replaceRoots: async (roots, options = {}) => {
      maybeFail("workspaces.replaceRoots");
      const target = options.target ?? state.windows[0]?.target;
      if (target === undefined) throw hostError("ContextUnavailable");
      return setRoots(target, roots);
    },
    pickFolder: async (options) => prompt("pickFolder", options),
  },
  terminals: {
    open: notFaked("terminals.open"),
  },
  storage: {
    read: async (key) => {
      maybeFail("storage.read");
      const value = state.records.get(key);
      return value === undefined ? { found: false } : { found: true, value: JSON.parse(value) };
    },
    write: async (key, value) => {
      maybeFail("storage.write");
      state.records.set(key, JSON.stringify(json(value)));
    },
    delete: async (key) => {
      maybeFail("storage.delete");
      state.records.delete(key);
    },
    list: async () => [...state.records.keys()].sort(),
  },
  recovery: {
    list: async () => [...state.captures.values()].reverse().map(({ text: _text, ...capture }) => capture),
    retain: async (id) => {
      maybeFail("recovery.retain");
      const document = state.documents.get(id);
      if (document === undefined) throw hostError("DocumentNotFound");
      const capture = {
        id: nextId("capture") as Knot.CaptureId,
        document: id,
        title: document.title,
        kind: document.kind,
        resourceUri: document.resourceUri,
        capturedAt: Date.now(),
        text: document.text,
      };
      state.captures.set(capture.id, capture);
      const { text: _text, ...retained } = capture;
      return retained;
    },
    release: async (id) => {
      maybeFail("recovery.release");
      return state.captures.delete(id);
    },
    restore: notFaked("recovery.restore"),
  },
  workbench: {
    observe: async (listener) => {
      state.listeners.push(listener);
      return {
        snapshot: {
          documents: [...state.documents.values()].map(documentInfo),
          windows: state.windows.map(windowInfo),
          activeWindow: state.focused,
          activeTarget: null,
          launched: state.launched,
        },
        watermark: state.sequence,
        dispose: async () => {
          state.listeners = state.listeners.filter((candidate) => candidate !== listener);
        },
      };
    },
    close: notFaked("workbench.close"),
    replaceWindows: async (options) => replaceWindows(options),
    captureLayout: async (target) => {
      fakeWindow(target);
      return LAYOUT;
    },
    registerView: async (contribution) => {
      if (state.views.has(contribution.id)) throw hostError("ViewIdInUse");
      const registered: FakeView = {
        contribution,
        visible: contribution.placement?.visible ?? true,
        text: "",
        invalidations: 0,
      };
      state.views.set(contribution.id, registered);
      return {
        id: contribution.id,
        show: async () => {
          registered.visible = true;
        },
        hide: async () => {
          registered.visible = false;
        },
        focus: async () => {
          registered.visible = true;
        },
        invalidate: async () => {
          registered.invalidations += 1;
        },
        setText: async (text) => {
          registered.text = text;
        },
        dispose: async () => {
          state.views.delete(contribution.id);
        },
      };
    },
    showPicker: async (options) => prompt("showPicker", options),
    showInput: async (options) => prompt("showInput", options),
    showConfirmation: async (options) => prompt("showConfirmation", options),
    showMessage: async (options) => {
      state.messages.push(options);
    },
    selectedText: async () => state.selectedText,
    writeClipboardText: async (text) => {
      state.clipboard = text;
    },
  },
  commands: {
    register: async (name, handler) => {
      if (state.commands.has(name)) throw hostError("CommandNameInUse");
      state.commands.set(name, handler);
      return {
        dispose: async () => {
          state.commands.delete(name);
        },
      };
    },
    registerForView: async (viewKind, name, handler) => {
      state.viewCommands.set(`${viewKind}:${name}`, handler);
      return {
        dispose: async () => {
          state.viewCommands.delete(`${viewKind}:${name}`);
        },
      };
    },
    invoke: async (name, args = null) => {
      const handler = state.commands.get(name);
      const window = state.windows[0]?.target;
      if (handler === undefined || window === undefined) return { kind: "unavailable" };
      try {
        await handler(context(window, args, null));
        return { kind: "completed" };
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        return error instanceof InvalidArguments
          ? { kind: "invalidArgument", message }
          : { kind: "handlerFailure", message };
      }
    },
    invalidArguments: (message) => {
      throw new InvalidArguments(message);
    },
  },
  editor: {
    activeBuffer: notFaked("editor.activeBuffer"),
    registerCompletionProvider: notFaked("editor.registerCompletionProvider"),
  },
  keybinding: notFaked("keybinding"),
  removeKeybinding: notFaked("removeKeybinding"),
};
