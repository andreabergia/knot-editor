/**
 * Public API version 1 reference for module "knot".
 * See docs/extension-api.md for package admission and authoring.
 *
 * Offsets, revisions and generations are nonnegative safe integers. Recognized
 * fields are validated without coercion; unknown option/data fields are ignored.
 * Invalid types raise TypeError; numeric limits and coordinates raise RangeError.
 * Host failures are Error instances with a stable code (see HostError).
 * Accepted requests settle once at native acknowledgement. Cancellation before
 * mutation prevents it; rejection after a commit does not imply rollback.
 * Public snapshots, contexts, proxies and callback payloads are read-only.
 */
declare module "knot" {
  const windowIdentity: unique symbol;
  const viewIdentity: unique symbol;
  const targetAuthority: unique symbol;
  const bufferIdentity: unique symbol;
  /** Opaque live window identity. Do not fabricate, serialize or persist it. */
  export type WindowId = string & { readonly [windowIdentity]: true };
  /** Opaque live extension-addressable view identity; not a native handle. */
  export type ViewId = string & { readonly [viewIdentity]: true };

  const documentIdentity: unique symbol;
  const liveWindowIdentity: unique symbol;
  const livePaneIdentity: unique symbol;
  const presentationIdentity: unique symbol;
  const rootIdentity: unique symbol;
  /** Application-allocated identity, preserved across Save As and presentations.
   * Reopening allocates a new identity. A document restored from automatic
   * recovery keeps its identity across restart, so a session record may store
   * it; other identities are restart-local. */
  export type DocumentId = string & { readonly [documentIdentity]: true };
  /** Root identity retained while its normalized URI remains installed. */
  export type RootId = string & { readonly [rootIdentity]: true };
  /** Discoverable live targets have no command invocation authority.
   * Do not fabricate, copy, serialize or persist target objects. */
  export interface WindowTarget { readonly [liveWindowIdentity]: true; }
  export interface PaneTarget {
    readonly [livePaneIdentity]: true;
    readonly window: WindowTarget;
  }
  /** Exact tab presentation. Replacement, pane/window transfer, or closure
   * invalidates this target; it cannot select a replacement presentation. */
  export interface ViewTarget {
    readonly [presentationIdentity]: true;
    readonly window: WindowTarget;
    readonly pane: PaneTarget;
  }
  export interface DocumentInfo {
    readonly id: DocumentId;
    readonly title: string;
    readonly kind: "untitled" | "destination" | "persisted";
    readonly resourceUri: string | null;
    readonly dirty: boolean;
    readonly revision: number;
    readonly buffer: Buffer;
    /** Language ID, set explicitly or detected from the file name and first
     * line. "plaintext" when nothing matched or the ID is not registered; a
     * kept explicit ID reappears once something registers it. Changes
     * publish documentChanged. */
    readonly language: string;
  }
  export interface PresentationInfo {
    readonly target: ViewTarget;
    readonly kind: "document" | "terminal" | "tool";
    readonly document: DocumentId | null;
    readonly buffer: Buffer | null;
    /** Stable contribution ID for a tool presentation, otherwise null. */
    readonly tool: string | null;
  }
  export interface PaneInfo {
    readonly target: PaneTarget;
    readonly tabs: readonly PresentationInfo[];
    readonly selected: ViewTarget;
  }
  export interface WorkspaceRoot { readonly id: RootId; readonly uri: string; }
  export interface WorkspaceInfo {
    readonly window: WindowTarget;
    readonly roots: readonly WorkspaceRoot[];
    readonly generation: number;
  }
  export interface WindowInfo {
    readonly target: WindowTarget;
    readonly panes: readonly PaneInfo[];
    /** Actual focused tab surface; null for focus outside tabs. */
    readonly currentTarget: ViewTarget | null;
    readonly workspace: WorkspaceInfo;
  }
  /** Current metadata queries do not grant future target validity. Valid during
   * activation, including before any product window exists. */
  export interface DocumentDestinationOptions {
    /** Captured at request admission. Omission uses the command's window, or the
     * active product window outside a command; missing context rejects. */
    readonly target?: WindowTarget | PaneTarget | ViewTarget;
  }
  export interface OpenDocumentOptions extends DocumentDestinationOptions {
    /** Open a document not already shown in the destination window as that
     * pane's preview tab, replacing its current preview in place. A preview
     * becomes a regular tab when edited, pinned, double-clicked, dragged, or
     * opened again without `preview`. With the `workbench.previewTabs`
     * setting off, a preview request opens a regular tab. */
    readonly preview?: boolean;
  }
  export interface DocumentPresentation {
    readonly document: DocumentInfo;
    readonly presentation: PresentationInfo;
  }
  export const documents: Readonly<{
    list(): Promise<readonly DocumentInfo[]>;
    get(id: DocumentId): Promise<DocumentInfo | null>;
    /** File-only Open through the registered provider. Missing files become dirty
     * destinations; directories reject. Reuses an existing tab in the destination
     * window, otherwise creates one in the captured pane. Focus changes during I/O
     * never retarget the request. Resolves after commit without awaiting observers. */
    open(uri: string, options?: OpenDocumentOptions): Promise<DocumentPresentation>;
    /** Creates a clean, empty ordinary document together with its native tab. */
    createUntitled(options?: DocumentDestinationOptions): Promise<DocumentPresentation>;
    /** Presents a shared live document, reusing its tab in the destination window.
     * Rejects DocumentNotFound after final closure. */
    present(id: DocumentId, options?: DocumentDestinationOptions): Promise<DocumentPresentation>;
    /** Makes a language ID the document's explicit language, or returns it to
     * detection with null. An unregistered ID is accepted and kept, reporting
     * and painting as "plaintext" until a registration provides it. Open, Save As, and restore re-detect
     * only a document whose language is not explicit.
     * @throws TypeError for a non-text language; HostError with
     * DocumentNotFound or Cancelled. */
    setLanguage(id: DocumentId, language: string | null): Promise<void>;
  }>;

  export interface TerminalOptions extends DocumentDestinationOptions {
    /** Existing local directory URI the shell starts in. Omission uses the
     * window's default: the focused terminal's current directory, else the
     * root containing its most recently focused document, else its first
     * root, else the home directory. */
    readonly cwd?: string;
  }
  export const terminals: Readonly<{
    /** Opens and focuses a terminal tab in the captured pane, or in the normal
     * group when the pane is a dock. Resolves after commit.
     * @throws TypeError for a non-text cwd; HostError with InvalidUri for a
     * malformed or non-file URI, ResourceNotFound, ResourceWrongKind,
     * ProviderFailure, InvalidTarget, ContextUnavailable, or Cancelled. */
    open(options?: TerminalOptions): Promise<PresentationInfo>;
  }>;

  /** A language as registered: its ID and the patterns detection matches. */
  export interface LanguageInfo {
    /** LSP languageId, such as "rust", "typescriptreact", or "shellscript". */
    readonly id: string;
    /** Display name, such as "TypeScript JSX". */
    readonly name: string;
    /** Compiled-in grammar ID; see languages.grammars. */
    readonly grammar: string;
    /** Extensions without the leading dot, matched case-insensitively against
     * the end of a file name, so "d.ts" is possible. The longest match wins,
     * then the latest registration claiming it. */
    readonly fileExtensions: readonly string[];
    /** Exact file names, such as "Makefile"; they win over extensions. */
    readonly fileNames: readonly string[];
    /** Shebang programs, such as "python3"; "#!/usr/bin/env" is looked
     * through, and a versioned program also matches its unversioned name. */
    readonly interpreters: readonly string[];
  }
  /** File patterns a language claims for detection; see LanguageInfo. */
  export interface LanguagePatterns {
    readonly fileExtensions?: readonly string[];
    readonly fileNames?: readonly string[];
    readonly interpreters?: readonly string[];
  }
  export interface LanguageDefinition extends LanguagePatterns {
    readonly id: string;
    readonly name: string;
    readonly grammar: string;
    /** Tree-sitter queries using nvim-treesitter capture names. Injections
     * name languages by ID, or by a registered file extension such as "py". */
    readonly queries: Readonly<{ highlights: string; injections?: string }>;
  }
  /**
   * Syntax highlighting languages. The latest registration of an ID defines
   * the language, and the latest registration or association claiming a
   * pattern decides detection; disposal or teardown reveals the previous one. Personal config
   * replaces a package's language from post-init, which runs after packages
   * start. Every change re-detects open documents whose language is not
   * explicit. Register in activation, before startup restores windows, so
   * restored documents highlight from their first frame.
   */
  export const languages: Readonly<{
    /**
     * Register a lifecycle-owned language. Queries compile against the grammar
     * before resolution.
     * @throws TypeError for invalid fields; HostError with UnknownGrammar,
     * InvalidQuery (details: query "highlights" or "injections", message, and
     * the 0-indexed row and column), or Cancelled. */
    register(definition: LanguageDefinition): Promise<Disposable>;
    /**
     * Claim file patterns for a language ID without defining the language, so
     * personal config can redirect detection, such as "mdx" files to
     * "markdown", without replacing a package's registration. The claim takes
     * part in detection like a registration's: the latest claim of a pattern
     * wins, and disposal or teardown reveals the previous one. An unregistered
     * ID paints as "plaintext". Associations do not appear in list.
     * @throws TypeError for invalid fields; HostError with Cancelled. */
    associate(id: string, patterns: LanguagePatterns): Promise<Disposable>;
    /** The language each registered ID resolves to, sorted by ID. */
    list(): Promise<readonly LanguageInfo[]>;
    /** Every compiled-in grammar ID, sorted. */
    grammars(): Promise<readonly string[]>;
  }>;

  /**
   * The value type of every defined setting key. Native keys are declared
   * here; a package declares its own keys by merging into this interface:
   * `declare module "knot" { interface Settings { "lsp.timeout.launch": number } }`.
   */
  export interface Settings {
    /** Columns per tab stop in editor views. Native default 4; editors use
     * at least 1 and at most 32. */
    "editor.tabSize": number;
    /** Which theme applies: theme.dark's, theme.light's, or the one matching
     * the system appearance. Native default "dark"; other text means "dark". */
    "theme.mode": "dark" | "light" | "system";
    /** The theme name used in dark mode. Native default "One Dark". */
    "theme.dark": string;
    /** The theme name used in light mode. Native default "One Light". */
    "theme.light": string;
    /** The keymap preset that supplies default bindings. Native default
     * "knot"; an unregistered name selects "knot". */
    "keymap.preset": string;
  }
  export type SettingKey = keyof Settings & string;
  /** "integer" accepts safe integers; "number" accepts any finite number. */
  export type SettingType = "boolean" | "integer" | "number" | "string";
  export interface SettingDefinition<K extends SettingKey = SettingKey> {
    /** Flat dotted name saying what the key sets, such as "lsp.timeout.launch". */
    readonly key: K;
    readonly type: SettingType;
    /** Applies when no value matches a context; must be of `type`. */
    readonly default: Settings[K];
    /** Shown by a future settings UI. */
    readonly description: string;
  }
  /**
   * Facts that must all match where a value applies, or that describe the
   * context of a read. `language` is a document's effective language ID;
   * `view` is a native surface kind ("editor", "terminal") or a contributed
   * view ID, as for keybindings.
   */
  export interface SettingsScope {
    readonly language?: string | null;
    readonly view?: string | null;
  }
  /**
   * One application-wide store of settings shared by personal config and every
   * package. A write replaces the value for exactly its key and scope, and a
   * later write replaces an earlier one whoever made it, so personal overrides
   * belong in post-init. Values live in memory only: config and packages write
   * them again on every launch, and teardown and Restart Extension remove none.
   *
   * A read resolves the matching value with the most scope facts, ties going
   * to the latest write; with none, the definition's default applies. A value
   * may be written before its key is defined: define validates it, drops a
   * value of the wrong type with a note in the extension report, and keys never
   * defined stay inert. Writes resolve after the store commits.
   * @throws TypeError for invalid keys, values, scopes or definitions.
   */
  export const settings: Readonly<{
    /** Define a lifecycle-owned key. Teardown removes the definition and keeps
     * its values, so a restarted package defines it again.
     * @throws HostError with DuplicateSetting when native code or a live
     * lifecycle defined the key, or Cancelled. */
    define<K extends SettingKey>(definition: SettingDefinition<K>): Promise<void>;
    /** Write one value, or several at once; a value of the wrong type for a
     * defined key rejects the whole call and writes nothing.
     * @throws HostError with SettingTypeMismatch (details: key, expected) or
     * Cancelled. */
    set<K extends SettingKey>(key: K, value: Settings[K], scope?: SettingsScope): Promise<void>;
    set(values: Partial<Settings>, scope?: SettingsScope): Promise<void>;
    /** The value for a context, or the default when none matches; undefined
     * for a key that is not defined. */
    get<K extends SettingKey>(key: K, facts?: SettingsScope): Promise<Settings[K] | undefined>;
    /** Remove the value for exactly this key and scope, revealing the next
     * match. Resetting a missing value succeeds. */
    reset(key: SettingKey, scope?: SettingsScope): Promise<void>;
    /** Call `listener` with the key after every write or reset of it, in any
     * scope, including this caller's own. Call get for the contexts that
     * matter. Listeners run serially with other listeners and stop with
     * their lifecycle. */
    observe(key: SettingKey, listener: (key: SettingKey) => void | Promise<void>): Promise<Disposable>;
  }>;
  /** Which defaults fill the colors a theme leaves out. */
  export type ThemeAppearance = "dark" | "light";
  /**
   * The name of a theme color, native or defined with
   * `themes.defineColor`, such as "error", "text.muted", or "diff.add". A key
   * nobody defines paints as interface text, so a contribution may name a key
   * whose definition arrives later.
   */
  export type ColorKey = string;
  /**
   * Colors by key, each "#rrggbb" or "#rrggbbaa". Native keys are declared
   * here; a package declares the keys it defines by merging into this
   * interface. A theme may set any key: one nobody defines is kept and stays
   * inert until something defines it.
   */
  export interface ThemeColors {
    readonly [key: string]: string | undefined;
    /** Window and pane background. */
    readonly "background"?: string;
    /** Dividers and panel borders. */
    readonly "border"?: string;
    /** A resizable divider under the pointer. */
    readonly "border.hover"?: string;
    /** Inline action buttons. */
    readonly "button.background"?: string;
    /** An inline action button under the pointer. */
    readonly "button.hover.background"?: string;
    /** Where a dragged tab would land. */
    readonly "dropTarget.background"?: string;
    /** Text editor background. */
    readonly "editor.background"?: string;
    /** Text cursor. */
    readonly "editor.cursor"?: string;
    /** Unhighlighted editor text. */
    readonly "editor.foreground"?: string;
    /** Underline of text being composed by an input method. */
    readonly "editor.markedText"?: string;
    /** Selected text. */
    readonly "editor.selection"?: string;
    /** Errors and failures. */
    readonly "error"?: string;
    /** Informational markers. */
    readonly "info"?: string;
    /** Group headers in trees. */
    readonly "list.group.background"?: string;
    /** A list or tree row under the pointer. */
    readonly "list.hover.background"?: string;
    /** Characters a filter query matched in a list row. */
    readonly "list.match.foreground"?: string;
    /** The selected list or tree row. */
    readonly "list.selected.background"?: string;
    /** Text of the selected list or tree row. */
    readonly "list.selected.foreground"?: string;
    /** Pickers, the command palette, completion, and reports. */
    readonly "popover.background"?: string;
    /** Border of popovers. */
    readonly "popover.border"?: string;
    /** Dims the window behind a modal surface. */
    readonly "scrim"?: string;
    /** Scrollbar thumb. */
    readonly "scrollbar.thumb"?: string;
    /** Success markers. */
    readonly "success"?: string;
    /** The active tab. */
    readonly "tab.active.background"?: string;
    /** Title of the active tab. */
    readonly "tab.active.foreground"?: string;
    /** The tab bar behind the tabs. */
    readonly "tab.bar.background"?: string;
    /** Inactive tabs. */
    readonly "tab.inactive.background"?: string;
    /** Titles of inactive tabs. */
    readonly "tab.inactive.foreground"?: string;
    /** ANSI black. */
    readonly "terminal.ansi.black"?: string;
    /** ANSI blue. */
    readonly "terminal.ansi.blue"?: string;
    /** ANSI bright black. */
    readonly "terminal.ansi.brightBlack"?: string;
    /** ANSI bright blue. */
    readonly "terminal.ansi.brightBlue"?: string;
    /** ANSI bright cyan. */
    readonly "terminal.ansi.brightCyan"?: string;
    /** ANSI bright green. */
    readonly "terminal.ansi.brightGreen"?: string;
    /** ANSI bright magenta. */
    readonly "terminal.ansi.brightMagenta"?: string;
    /** ANSI bright red. */
    readonly "terminal.ansi.brightRed"?: string;
    /** ANSI bright white. */
    readonly "terminal.ansi.brightWhite"?: string;
    /** ANSI bright yellow. */
    readonly "terminal.ansi.brightYellow"?: string;
    /** ANSI cyan. */
    readonly "terminal.ansi.cyan"?: string;
    /** ANSI green. */
    readonly "terminal.ansi.green"?: string;
    /** ANSI magenta. */
    readonly "terminal.ansi.magenta"?: string;
    /** ANSI red. */
    readonly "terminal.ansi.red"?: string;
    /** ANSI white. */
    readonly "terminal.ansi.white"?: string;
    /** ANSI yellow. */
    readonly "terminal.ansi.yellow"?: string;
    /** Terminal background and default cell background. */
    readonly "terminal.background"?: string;
    /** Terminal cursor. */
    readonly "terminal.cursor"?: string;
    /** Default terminal text. */
    readonly "terminal.foreground"?: string;
    /** Selected terminal cells. */
    readonly "terminal.selection"?: string;
    /** Default interface text. */
    readonly "text"?: string;
    /** Interface text that acts, such as buttons and links. */
    readonly "text.accent"?: string;
    /** Secondary interface text. */
    readonly "text.muted"?: string;
    /** Warnings. */
    readonly "warning"?: string;
  }
  export interface ThemeSyntaxStyle {
    /** "#rrggbb" or "#rrggbbaa". */
    readonly color?: string;
    readonly fontStyle?: "normal" | "italic";
    /** From 100 to 900; 700 is bold. */
    readonly fontWeight?: number;
    readonly underline?: boolean;
    readonly strikethrough?: boolean;
  }
  export interface ThemeDefinition {
    readonly name: string;
    readonly appearance: ThemeAppearance;
    readonly colors?: ThemeColors;
    /** Styles by capture name (nvim-treesitter's) or a dotted prefix of
     * one, such as "markup"; a capture takes its longest styled prefix. */
    readonly syntax?: Readonly<Record<string, ThemeSyntaxStyle>>;
  }
  export interface ThemeInfo {
    readonly name: string;
    readonly appearance: ThemeAppearance;
  }
  export interface ColorDefinition {
    /** Flat dotted name, such as "diff.add". */
    readonly key: string;
    /** Shown by a future settings UI. */
    readonly description: string;
    /** For each appearance, "#rrggbb", "#rrggbbaa", or another key whose
     * color to use. */
    readonly defaults: Readonly<{ dark: string; light: string }>;
  }
  /**
   * Themes and color keys. The latest registration of a name defines that
   * theme, and disposal or teardown reveals the previous one. The theme.mode,
   * theme.dark, and theme.light settings select the theme; it is resolved
   * when startup completes and again whenever the selection or, in "system"
   * mode, the system appearance changes, so register themes in activation.
   * A color the theme leaves out takes its key's default for the theme's
   * appearance; an unregistered selection uses the defaults alone.
   */
  export const themes: Readonly<{
    /** Register a lifecycle-owned theme.
     * @throws TypeError for invalid fields or colors; HostError with
     * UnknownCapture (details: capture) or Cancelled. */
    register(definition: ThemeDefinition): Promise<Disposable>;
    /** The registration of `name` that applies, with every field present. */
    get(name: string): Promise<ThemeDefinition | null>;
    /** Every registered theme, sorted by name. */
    list(): Promise<readonly ThemeInfo[]>;
    /** Define a lifecycle-owned color key; teardown removes it.
     * @throws TypeError for invalid fields; HostError with DuplicateColor
     * when native code or a live lifecycle defined the key, or Cancelled. */
    defineColor(definition: ColorDefinition): Promise<void>;
  }>;
  /** Only product windows; diagnostic and startup-error windows are excluded.
   * get returns null after closure; fabricated/command targets reject InvalidTarget. */
  export const windows: Readonly<{
    list(): Promise<readonly WindowInfo[]>;
    get(target: WindowTarget): Promise<WindowInfo | null>;
    /** Opens a focused blank window, as New Blank Window does, then applies the
     * roots like workspaces.replaceRoots. When the roots are rejected the window
     * is closed again.
     * @throws HostError with ResourceNotFound, ResourceWrongKind, InvalidUri,
     * ProviderFailure, InvalidTarget, or Cancelled. */
    open(options?: OpenWindowOptions): Promise<WindowInfo>;
    /** Activates the window and focuses its active tab.
     * @throws HostError with InvalidTarget for a closed window, or Cancelled. */
    focus(target: WindowTarget): Promise<void>;
  }>;
  export interface OpenWindowOptions {
    /** Ordered roots for the new window; omission or an empty list opens it
     * without roots. */
    readonly roots?: readonly string[];
  }
  /** Window-owned ordered roots; empty roots are valid. Closed windows return null. */
  export interface WorkspaceDestinationOptions {
    /** Omission captures the command's window or active product window at admission. */
    readonly target?: WindowTarget;
  }
  export const workspaces: Readonly<{
    get(target: WindowTarget): Promise<WorkspaceInfo | null>;
    /** Validates an existing directory and replaces this window's roots. Missing
     * roots reject ResourceNotFound, files reject ResourceWrongKind. */
    openFolder(uri: string, options?: WorkspaceDestinationOptions): Promise<WorkspaceInfo>;
    /** Validates an ordered list of directory URIs before committing roots and
     * tree together. Duplicates after normalization keep their first position;
     * nested and overlapping roots are allowed. Empty roots clear; retained
     * normalized roots keep their IDs, so adding, removing, and reordering are
     * replacements of the whole list. Documents remain open. A newer Open/root
     * request or root generation invalidates pending work. */
    replaceRoots(roots: readonly string[], options?: WorkspaceDestinationOptions): Promise<WorkspaceInfo>;
    /** Presents the native directory prompt and resolves a normalized, existing
     * directory URI, or dismissal. The selected path is normalized through the
     * registered providers, so a missing directory rejects ResourceNotFound and
     * a file rejects ResourceWrongKind. Omission captures the command's window or
     * active product window at admission.
     * @throws HostError with OperationInProgress when another extension
     * interaction owns the window, InvalidTarget, ContextUnavailable, Cancelled,
     * InvalidUri, ResourceNotFound, ResourceWrongKind, or ProviderFailure. */
    pickFolder(options?: WorkspaceDestinationOptions): Promise<FolderPickerResult>;
  }>;

  export interface ApplicationSnapshot {
    readonly documents: readonly DocumentInfo[];
    readonly windows: readonly WindowInfo[];
    readonly activeWindow: WindowTarget | null;
    readonly activeTarget: ViewTarget | null;
    /** True once startup has committed its restored windows and launch
     * requests. Extensions activate before any window exists; wait for the
     * launched event before reconciling state against startup windows. */
    readonly launched: boolean;
  }
  export type ApplicationChange =
    | { readonly type: "documentOpened" | "documentChanged"; readonly document: DocumentInfo }
    | { readonly type: "documentClosed"; readonly document: DocumentId }
    | { readonly type: "windowOpened" | "presentationsChanged"; readonly window: WindowInfo }
    | { readonly type: "windowClosed"; readonly window: WindowTarget }
    | { readonly type: "workspaceChanged"; readonly workspace: WorkspaceInfo }
    | { readonly type: "activeTargetChanged"; readonly activeWindow: WindowTarget | null; readonly activeTarget: ViewTarget | null }
    /** The window's captured layout changed: cursor, selection, scroll, focused
     * pane, dock size, window bounds, or structure. Reported about a second
     * after changes settle; structural changes also report presentationsChanged. */
    | { readonly type: "viewStateChanged"; readonly window: WindowTarget }
    /** Sent once, when launched becomes true. */
    | { readonly type: "launched" };
  export type ApplicationEvent = ApplicationChange & { readonly sequence: number };
  export interface WorkbenchObservation extends Disposable {
    readonly snapshot: ApplicationSnapshot;
    readonly watermark: number;
  }

  /**
   * Host-issued capture, valid only in its originating command invocation tree.
   * Contextual methods validate both authority and the live presentation.
   * Copying this object or retaining it after invocation does not grant access.
   */
  export interface CommandTarget {
    readonly [targetAuthority]: true;
    /** Originating product window identity, captured before palette/focus changes. */
    readonly window: WindowId;
    /** Live target for the same window, accepted by window-targeted operations. */
    readonly windowTarget: WindowTarget;
    /** Captured extension-addressable view, or null when none is available. */
    readonly view: ViewId | null;
    /** Stable contributed view ID, or null for non-contributed surfaces. */
    readonly tool: string | null;
  }
  /**
   * JSON-compatible data: finite numbers, dense arrays and plain string-keyed
   * records recursively. Cycles, undefined, functions, symbols, bigint,
   * nonplain objects and custom toJSON conversion are rejected.
   */
  export type JsonValue = null | boolean | number | string | readonly JsonValue[]
    | { readonly [key: string]: JsonValue };

  /** Missing keys are distinct from stored JSON null. */
  export type StorageRead = { readonly found: false }
    | { readonly found: true; readonly value: JsonValue };
  /** Application-global private records for this package or personal config.
   * Available during activation, without a window or command context. Data survives
   * reload, package updates and uninstall. Keys are exact strings, including empty.
   * TypeError for invalid JSON/types; StorageUnavailable includes database/cause
   * diagnostics. Cancellation may reject after a write has committed.
   */
  export const storage: {
    readonly read: (key: string) => Promise<StorageRead>;
    /** Atomically replace one record. Success acknowledges commit. Concurrent
     * calls serialize; last committed write wins. Read/modify/write is not atomic.
     */
    readonly write: (key: string, value: JsonValue) => Promise<void>;
    /** Deleting a missing key succeeds. */
    readonly delete: (key: string) => Promise<void>;
    /** All keys in deterministic binary order. */
    readonly list: () => Promise<readonly string[]>;
  };

  const captureIdentity: unique symbol;
  /** Application-generated retained capture identity. Safe to persist, for
   * example in a session record; it addresses only this package's captures. */
  export type CaptureId = string & { readonly [captureIdentity]: true };
  export interface RetainedCapture {
    readonly id: CaptureId;
    /** The document captured; restart reuse of the identity is not guaranteed. */
    readonly document: DocumentId;
    readonly title: string;
    readonly kind: "untitled" | "destination" | "persisted";
    readonly resourceUri: string | null;
    /** Capture time in milliseconds since the Unix epoch. */
    readonly capturedAt: number;
  }
  /** Restored document and its presentation. "restored" reports how the
   * resource relates to the capture: a changed file keeps the capture-time
   * version, so Save reaches the conflict prompt; a missing file becomes a dirty
   * destination. "preserved" means a live document (owner, null once closed)
   * already holds the identity or URI; the text opened as a separate untitled
   * document instead. */
  export type RecoveryRestore = DocumentPresentation & Readonly<
    | { outcome: "restored"; resource: "untitled" | "unchanged" | "missing" | "changed" }
    | { outcome: "preserved"; owner: DocumentId | null }>;
  /**
   * Durable document-text captures retained by this package or personal config.
   * Available during activation. The owner is the caller's canonical identity,
   * never an argument: another owner's capture is indistinguishable from a
   * missing one. Captures are immutable and survive unload, failure, restart and
   * uninstall until released; the native File › Recover Unsaved Documents… listing
   * shows them with their owner and can restore or discard them. Automatic
   * unsaved-document protection is independent and unaffected.
   * RecoveryUnavailable includes a cause diagnostic.
   */
  export const recovery: Readonly<{
    /** This owner's captures, newest first. Reconcile it with your own records
     * to find and release captures a cancelled call left unreferenced. */
    list(): Promise<readonly RetainedCapture[]>;
    /** Capture an open document's current text, clean or dirty. Resolves only
     * after the capture is durably committed, so a record referencing the ID may
     * be committed next. Each call yields a new ID; earlier captures of the same
     * document remain. Cancellation may reject after the capture committed.
     * @throws HostError with DocumentNotFound, RecoveryUnavailable or Cancelled. */
    retain(document: DocumentId): Promise<RetainedCapture>;
    /** Delete a capture. Release an old capture only after the record that
     * replaced it is durable. Resolves false when this owner holds no such ID. */
    release(id: CaptureId): Promise<boolean>;
    /** Restore a capture as an ordinary document with an empty undo history and
     * present it like documents.present. Never writes a resource or replaces a
     * live document. The capture stays retained until released.
     * @throws HostError with CaptureNotFound, InvalidTarget, ContextUnavailable,
     * Cancelled, RecoveryUnavailable, or the documents.open resource codes when
     * the file baseline cannot be read. */
    restore(id: CaptureId, options?: DocumentDestinationOptions): Promise<RecoveryRestore>;
  }>;

  /** Stable host failure codes; names, messages and stacks are diagnostics.
   * Checked against native handler descriptors and common transport failures. */
  export type HostErrorCode = "ContextUnavailable" | "InvalidTarget" | "OwnershipDisposed"
    | "Cancelled" | "UnsupportedOperation" | "BufferClosed" | "BufferReadOnly"
    | "InvalidRange" | "InvalidEditBatch" | "RevisionConflict" | "ContributionSetNotFound"
    | "ViewNotFound" | "ViewIdInUse" | "InvalidViewId"
    | "ViewRegistrationNotFound" | "ContentKindMismatch"
    | "CompletionProviderNotFound" | "CommandNameInUse" | "CommandNotFound"
    | "InvalidUri" | "DocumentNotFound" | "OperationInProgress" | "PersistenceFailed" | "ResourceNotFound" | "ResourceWrongKind" | "InvalidUtf8" | "ProviderFailure" | "StorageUnavailable"
    | "CaptureNotFound" | "RecoveryUnavailable" | "SessionConflict"
    | "UnknownGrammar" | "InvalidQuery" | "LanguageRegistrationNotFound"
    | "DuplicateSetting" | "SettingTypeMismatch"
    | "UnknownCapture" | "DuplicateColor" | "ThemeRegistrationNotFound"
    | "KeymapPresetRegistrationNotFound";
  /**
   * Native operation failure. Narrow a caught Error by name === "KnotHostError"
   * and its string code property, then dispatch on code. No error constructor
   * is exported. Author-side TypeError/RangeError are separate errors.
   */
  export interface HostError extends Error {
    readonly name: "KnotHostError";
    /** Stable category for handling a failure independently of its message. */
    readonly code: HostErrorCode;
    /** Optional handler-specific JSON diagnostics; may be null. Narrow fields
     * before inspecting them. Use code for categories; message is diagnostic. */
    readonly details?: JsonValue;
  }
  /**
   * A lifecycle-owned registration, subscription or contribution set; native IDs
   * are hidden and must not be persisted. Unload/failed activation/shutdown release
   * ownership independently of extension awaits or final JavaScript callbacks.
   */
  export interface Disposable {
    /**
     * Mark inactive immediately and release native ownership.
     * Resolution acknowledges release, not completion of an executing callback;
     * awaiting disposal from that callback is safe. No new callback is admitted.
     * Repeated/concurrent calls share one promise and perform no second mutation.
     * Failure is retained for repeated calls; the object stays inactive and
     * lifecycle teardown releases any remaining ownership. No automatic retry.
     * Pending provider work is cancelled; an already-running callback may finish
     * or be cancelled under its own callback contract.
     *
     * @throws HostError for cancellation or stale native ownership. Command,
     * tree, completion and language registrations use their resource-specific
     * not-found codes; stale subscriptions use OwnershipDisposed. Contribution cleanup
     * can fail with BufferClosed; an absent contribution set is already released.
     */
    dispose(): Promise<void>;
  }
  /**
   * Half-open absolute UTF-8 byte range [startByteOffset, endByteOffset).
   * Endpoints must be ordered, within the buffer and on Unicode scalar boundaries.
   * Offsets are not JavaScript string indices or grapheme positions.
   */
  export interface ByteRange {
    readonly startByteOffset: number;
    readonly endByteOffset: number;
  }
  /** Replacement in the precommit buffer coordinates of one edit batch. */
  export interface TextEdit { readonly range: ByteRange; readonly text: string; }
  /** Optimistic concurrency: the current buffer revision must match exactly. */
  export interface RevisionOptions {
    /** Required; never defaults to the current revision. */
    readonly ifRevision: number;
  }
  /** Immutable text and revision captured together for the requested range. */
  export interface TextSnapshot {
    /** JavaScript UTF-16 text for range. */
    readonly text: string;
    /** Absolute byte range in the original buffer. */
    readonly range: ByteRange;
    /** Buffer revision when this snapshot was captured. */
    readonly revision: number;
    /**
     * Convert a UTF-16 code-unit index in text to a relative UTF-8 byte offset.
     * Add range.startByteOffset to obtain a whole-buffer coordinate.
     * @throws TypeError for a nonnumber; RangeError for invalid numeric values,
     * out-of-bounds offsets or an index splitting a surrogate pair.
     */
    byteOffsetAtUtf16(offset: number): number;
    /**
     * Convert a byte offset relative to text to a relative UTF-16 index.
     * @throws TypeError for a nonnumber; RangeError for invalid numeric values,
     * out-of-bounds offsets or an offset splitting a UTF-8 scalar.
     */
    utf16OffsetAtByte(offset: number): number;
  }
  /** One committed edit batch, delivered as a serial listener callback. */
  export interface BufferChange {
    readonly buffer: Buffer;
    /** Revision used by the original batch. */
    readonly beforeRevision: number;
    readonly revision: number;
    /** Ordered replacements in beforeRevision coordinates. */
    readonly edits: readonly TextEdit[];
  }
  /**
   * Anchored semantic range. Optional decoration, gutter and command may be null.
   * Edits stabilize its anchors. Command is a name, not structured arguments.
   */
  export interface EditorContribution {
    readonly range: ByteRange;
    /** Underlines the range with a wavy line in this key's color. */
    readonly decoration?: ColorKey | null;
    /** Marks the range's first line in the gutter in this key's color. */
    readonly gutter?: ColorKey | null;
    readonly command?: string | null;
  }
  /** One replaceable set per buffer and extension lifecycle. */
  export interface EditorContributions extends Disposable {
    /**
     * Atomically replace this lifecycle's entire set at the required revision.
     * Empty items clear it. Overlap is allowed; nonempty items on an empty buffer
     * are invalid; empty ranges on a nonempty buffer follow anchored-range rules.
     * Accepted replacements precede subsequent disposal in order; new calls after
     * disposal starts reject with OwnershipDisposed.
     * Resolution acknowledges native replacement, not paint.
     * @throws HostError with BufferClosed, RevisionConflict or InvalidRange;
     * OwnershipDisposed after this object's disposal starts; Cancelled on teardown.
     */
    replace(items: readonly EditorContribution[], options: RevisionOptions): Promise<void>;
  }
  /**
   * Opaque live text-buffer proxy. Each operation uses this buffer, never focus.
   * May be retained for asynchronous work within the extension lifecycle.
   * Closing one presentation does not invalidate a still-live shared buffer.
   * Do not persist it; it is not a persistent document identity.
   */
  export interface Buffer {
    readonly [bufferIdentity]: true;
    /**
     * Capture immutable text and revision. Omitted/null range means whole buffer.
     * @throws HostError with BufferClosed or InvalidRange, or Cancelled on teardown.
     */
    snapshot(range?: ByteRange | null): Promise<TextSnapshot>;
    /**
     * Atomically apply ordered, nonoverlapping replacements at ifRevision.
     * Repeated empty insertions at one position retain input order. A real batch
     * advances revision once; an empty/no-op batch keeps it and emits no change.
     * Resolution acknowledges the commit and returns its resulting revision.
     * @throws HostError with BufferClosed, BufferReadOnly, RevisionConflict,
     * InvalidRange or InvalidEditBatch; Cancelled before mutation or on teardown.
     */
    applyEdits(edits: readonly TextEdit[], options: RevisionOptions): Promise<Readonly<{ revision: number }>>;
    /**
     * Subscribe to committed changes; resolves once native ownership exists.
     * Use change.buffer inside asynchronous callbacks. A listener rejection
     * diagnoses only that callback; later FIFO deliveries continue.
     * Disposal stops new admission and discards queued unowned deliveries.
     * Registration racing teardown rejects instead of publishing a late listener.
     * @throws TypeError for a nonfunction; HostError with BufferClosed or Cancelled.
     */
    onDidChange(listener: (change: BufferChange) => void | Promise<void>): Promise<Disposable>;
    /** Lifecycle-owned semantic data; disposing it does not dispose this buffer. */
    readonly contributions: EditorContributions;
  }
  /**
   * Command cancellation signal with the supported interface below.
   * No browser AbortSignal inheritance, listener options or onabort property.
   * Cancellation does not roll back committed mutations.
   */
  export interface CancellationSignal {
    /** True after cancellation has been signalled. */
    readonly aborted: boolean;
    /** Cancellation reason, undefined before cancellation. */
    readonly reason: unknown;
    /** Subscribe before cancellation; no replay for an already-aborted signal. */
    addEventListener(type: "abort", listener: (event: CancellationEvent) => void): void;
    /** Remove the same listener function previously registered. */
    removeEventListener(type: "abort", listener: (event: CancellationEvent) => void): void;
    /** Throw reason when aborted; otherwise return normally. */
    throwIfAborted(): void;
  }
  /** Event delivered to cancellation listeners. */
  export interface CancellationEvent {
    readonly type: "abort";
    readonly target: CancellationSignal;
  }
  /**
   * Context captured at command admission, preserved across awaits and children.
   * Root, inline and cross-lifecycle handlers receive equivalent target authority.
   */
  export interface CommandContext {
    /** Originating window/presentation; unrelated targets are not authorized. */
    readonly target: CommandTarget;
    /** Captured text buffer, or null for a surface without a text buffer. */
    readonly buffer: Buffer | null;
    /** Caller-supplied JSON value, defaulting to null when omitted. */
    readonly arguments: JsonValue;
    /** Cancellation of this admitted command. */
    readonly signal: CancellationSignal;
  }
  /**
   * Awaited handler; return values do not become command results.
   * Throws become handlerFailure; invalidArguments produces invalidArgument.
   */
  export type CommandHandler = (context: CommandContext) => void | Promise<void>;
  /** How a command presents itself in the command palette. */
  export interface CommandOptions {
    /** Palette title; defaults to the command name. */
    readonly title?: string;
    /** Muted text shown beside the title. */
    readonly description?: string | null;
    /** False hides the command from the palette; it stays invocable. */
    readonly palette?: boolean;
  }
  /**
   * Execution status rather than a semantic result. Unavailable includes declined
   * routing and unsupported composition; invalidTarget identifies a stale capture.
   * InvalidArgument and handlerFailure include diagnostics in message.
   */
  export type CommandOutcome = Readonly<
    { kind: "completed" | "unavailable" | "invalidTarget" | "cancelled" }
    | { kind: "invalidArgument" | "handlerFailure"; message: string }
  >;
  /**
   * One semantic command-backed action on a row, group header, or frame.
   * Arguments are JSON and are passed to the command as its argument value;
   * an absent value is null. Clicking an action never activates the row.
   */
  export interface TreeAction {
    readonly label: string;
    readonly command: string;
    readonly arguments?: JsonValue;
  }
  /**
   * Native tree semantics. Optional description, icon and command are nullable.
   * A group is a non-activatable, non-disclosure header that organizes child
   * rows; its children are fetched immediately and remain visible.
   */
  export interface TreeItem {
    /** Author-supplied identity, stable and unique within the provider tree. */
    readonly id: string;
    readonly label: string;
    readonly description?: string | null;
    readonly icon?: "file" | "folder" | "symbol" | null;
    /** Required expansion state; never inferred from child results. */
    readonly collapsibleState: "none" | "collapsed" | "expanded";
    /** Activation command for ordinary item rows; ignored for groups. */
    readonly command?: string | null;
    /** Row kind; defaults to "item". */
    readonly kind?: "item" | "group";
    /** Right-aligned short status text, e.g. a Git change letter. */
    readonly statusText?: string | null;
    /** A badge dot in this key's color; put any short label in statusText. */
    readonly badge?: ColorKey | null;
    /** Row or group actions; frame actions belong to the contribution. */
    readonly actions?: readonly TreeAction[];
  }
  /** Explicit live context supplied to a contributed view's provider. */
  export interface ViewContext {
    /** Window owning this native instance. */
    readonly window: WindowTarget;
    /** Opaque native instance identity while this presentation stays live. */
    readonly instance: ViewId;
  }
  /** Tree callbacks have no implicit window, active buffer or public signal. */
  export interface TreeDataProvider {
    /**
     * Return children of parentId; null requests roots. Must return an iterable
     * of valid items, not a string or accidental array-like object.
     * Context identifies the requesting window and native instance explicitly.
     * Throws, rejections and invalid items become isolated request diagnostics.
     * Native generation validation discards stale results. Receiver cancellation
     * releases pending work without promising JavaScript stack unwind.
     */
    getChildren(parentId: string | null, context: ViewContext): Iterable<TreeItem> | Promise<Iterable<TreeItem>>;
  }
  export type ViewLocation = "left" | "right" | "bottom" | "document";
  /** Initial placement; remembered user layout overrides it on later windows. */
  export interface ViewPlacement {
    /** Dock or document group. Defaults to the left dock. */
    readonly location?: ViewLocation;
    /** Deterministic order within a group; defaults after native tools. */
    readonly order?: number;
    /** Initial visibility. Defaults to true. */
    readonly visible?: boolean;
  }
  /**
   * Native content for one contributed view. A tree is populated through its
   * provider; a text frame starts empty and is replaced with setText.
   */
  export type ViewContent =
    | Readonly<{ kind: "tree"; provider: TreeDataProvider }>
    | Readonly<{ kind: "text" }>;
  /** One lifecycle-owned native view definition. */
  export interface ViewContribution {
    /** Stable, unique, namespaced identity; `knot.` is reserved. */
    readonly id: string;
    readonly title: string;
    readonly content: ViewContent;
    /** Actions shown in the frame toolbar; only meaningful for tree frames. */
    readonly actions?: readonly TreeAction[];
    readonly placement?: ViewPlacement;
    /** Language ID a text frame's content is highlighted as; text frames only. */
    readonly language?: string;
  }
  /** Registration owns one contributed view across every live product window. */
  export interface ViewRegistration extends Disposable {
    readonly id: string;
    /**
     * Show this view in every live product window, or only in one explicit
     * window. Programmatic presentation never updates the fresh-window default.
     * @throws HostError with InvalidTarget for a stale window, or Cancelled on
     * teardown.
     */
    show(window?: WindowTarget): Promise<void>;
    /** Hide this view in every live product window, or only in one window. */
    hide(window?: WindowTarget): Promise<void>;
    /** Show if hidden, then focus this view in every window or one window. */
    focus(window?: WindowTarget): Promise<void>;
    /**
     * Invalidate root children when parentId is omitted/null, otherwise that
     * parent's children. Omitting window invalidates every live instance; an
     * explicit window must own a live instance.
     * Resolution acknowledges native invalidation, not fetch or paint. Calls
     * accepted before disposal retain request order and settle before cleanup
     * acknowledgement.
     * @throws HostError with OwnershipDisposed after disposal starts,
     * ViewRegistrationNotFound for stale native ownership, ViewNotFound for a
     * window without a live instance, or Cancelled on teardown.
     */
    invalidate(parentId?: string | null, window?: WindowTarget): Promise<void>;
    /**
     * Replace the complete text of a text frame in every live window, or in
     * one explicit window. Tree frames reject with ContentKindMismatch.
     * The last text is retained for windows created later; this is not an
     * editable document and has no persistence or backing resource.
     * @throws HostError with ContentKindMismatch, ViewNotFound (explicit window
     * without a live instance), OwnershipDisposed, or Cancelled on teardown.
     */
    setText(text: string, window?: WindowTarget): Promise<void>;
  }
  /** One selectable row offered to a picker; fuzzy-matched locally by label,
   * then description. */
  export interface PickerItem {
    readonly id: string;
    readonly label: string;
    /** Muted text beside the label. */
    readonly description?: string | null;
    /** A muted second line. */
    readonly detail?: string | null;
    /** Right-aligned text, such as a keybinding. */
    readonly hint?: string | null;
  }
  /** Selection or explicit dismissal; dismissal is not an operational failure. */
  export type PickerResult = Readonly<
    { kind: "selected"; id: string } | { kind: "dismissed" }>;
  /** Accepted text or explicit dismissal. */
  export type InputResult = Readonly<
    { kind: "accepted"; value: string } | { kind: "dismissed" }>;
  /** Confirmed or dismissed native confirmation. */
  export type ConfirmationResult = Readonly<
    { kind: "confirmed" } | { kind: "dismissed" }>;
  /** Selected directory URI or explicit dismissal; dismissal is not a failure. */
  export type FolderPickerResult = Readonly<
    { kind: "selected"; uri: string } | { kind: "dismissed" }>;
  /** An explicit target or the admitted command's window is required; a
   * context-free call without either rejects ContextUnavailable. */
  export interface TransientTargetOptions { readonly target?: WindowTarget; }
  export interface PickerOptions extends TransientTargetOptions {
    readonly title: string;
    readonly items: readonly PickerItem[];
    /**
     * Called with the highlighted item's ID, or null when nothing matches,
     * first for the initial highlight and then whenever it changes. Calls are
     * serial; while one runs, later changes collapse into the latest. Undoing
     * a preview after dismissal is the caller's job. A throw stops further
     * calls and is rethrown once the picker settles.
     */
    readonly onHighlight?: (id: string | null) => void | Promise<void>;
  }
  export interface InputOptions extends TransientTargetOptions {
    readonly title: string;
    /** Initial text; defaults to empty. */
    readonly value?: string;
  }
  export interface ConfirmationOptions extends TransientTargetOptions {
    readonly title: string;
    readonly message?: string;
    readonly confirmLabel?: string;
    readonly cancelLabel?: string;
  }
  export interface MessageOptions extends TransientTargetOptions {
    readonly title: string;
    readonly message?: string;
  }
  /** Native completion request; use its explicit buffer without borrowing focus. */
  export interface CompletionContext {
    readonly buffer: Buffer;
    readonly revision: number;
    /** Absolute UTF-8 cursor coordinate at revision. */
    readonly cursorByteOffset: number;
    /** Native text prefix for this request. */
    readonly prefix: string;
    /** Request correlation only; not reusable target authority. */
    readonly generation: number;
  }
  /** Candidate display label and insertion text; both are required strings. */
  export interface CompletionItem { readonly label: string; readonly insertText: string; }
  /** Native aggregation isolates failures of each completion provider. */
  export interface CompletionProvider {
    /**
     * Return an iterable of valid candidates, synchronously or asynchronously.
     * Throws, rejections and invalid items fail only this request. Native revision,
     * generation and ownership checks discard stale/cancelled results.
     * There is no public provider cancellation signal; disposal cancels pending
     * requests without waiting for an executing callback to unwind.
     */
    provideCompletions(context: CompletionContext): Iterable<CompletionItem> | Promise<Iterable<CompletionItem>>;
  }
  /** Explicit buffers and editor completion registration. */
  export const editor: Readonly<{
    /**
     * Resolve the originating command's text buffer; a valid nontext capture
     * returns null. Omission uses the current frame; an explicit target must be
     * its host-issued capture or authorized inherited capture. Never reads focus.
     * @throws HostError with ContextUnavailable outside a command, InvalidTarget
     * for forged/stale/closed/replaced captures, or Cancelled after cancellation.
     */
    activeBuffer(target?: CommandTarget): Promise<Buffer | null>;
    /**
     * Register a completion provider; label is diagnostic, not identity.
     * Resolves when native registration exists; teardown races reject with
     * Cancelled instead of publishing a late callable object.
     * @throws TypeError for invalid label/provider; HostError on native failure.
     */
    registerCompletionProvider(label: string, provider: CompletionProvider): Promise<Disposable>;
  }>;
  /** Register handlers and compose child invocations with captured context. */
  export const commands: Readonly<{
    /**
     * Register a global command name. The palette shows its title, which
     * defaults to the name, and description, and matches the name too.
     * Valid during activation. Resolves when native ownership exists;
     * teardown races reject instead of publishing a late callable handler.
     * @throws TypeError for invalid arguments; HostError with CommandNameInUse
     * when the name is already owned, or Cancelled on teardown.
     */
    register(name: string, handler: CommandHandler, options?: CommandOptions): Promise<Disposable>;
    /**
     * Handle an existing command for one registered contribution ID. Native
     * routing binds live instances and preserves the captured presentation.
     * Resolution acknowledges registration; disposal removes its bindings.
     * @throws HostError with ViewNotFound, CommandNotFound, CommandNameInUse
     * or Cancelled; TypeError for invalid argument types.
     */
    registerForView(viewKind: string, name: string, handler: CommandHandler): Promise<Disposable>;
    /**
     * Invoke a child of the current command; arguments default to null.
     * Inherits the originating window/buffer and resolves with execution status.
     * Await children serially: only one unfinished child per parent; recursive
     * handler ancestry and cross-lifecycle cycles back to ancestors are unavailable.
     * @throws HostError with ContextUnavailable outside a command, or Cancelled
     * for transport cancellation; TypeError for non-JSON-compatible arguments.
     */
    invoke(name: string, arguments_?: JsonValue): Promise<CommandOutcome>;
    /** Throw an author diagnostic converted by the handler to invalidArgument. */
    invalidArguments(message: string): never;
  }>;
  export type PresentationCloseOutcome = Readonly<{ kind: "closed" | "cancelled" }>;
  /** One document reference of a session window. A live document matching
   * `document` or `uri` is always presented and any capture is ignored; else
   * the capture is restored, else the file is opened. */
  export interface SessionEntry {
    readonly uri?: string | null;
    readonly document?: DocumentId | null;
    /** A capture retained by this owner. */
    readonly capture?: CaptureId | null;
  }
  /** A document presented in a released window. */
  export interface SuspendedDocument {
    readonly document: DocumentId;
    /** Required when the document is dirty or a non-empty untitled document
     * and its final view is removed; it must be retained at the document's
     * current revision. */
    readonly capture: CaptureId | null;
  }
  /** Restorable editor state as UTF-8 byte offsets; top is the start of the
   * first visible line. Offsets clamp to shorter text on restore. */
  export type EditorViewState = Readonly<{
    anchor: number;
    caret: number;
    top: number;
    /** Horizontal scroll in logical pixels. */
    scrollX: number;
  }>;
  /** Each tab kind records only what recreates it in the same position. A
   * terminal tab records its shell's working directory as a file URI and
   * restores as a fresh shell started there, or in the window's default
   * directory when it no longer exists; no process or content survives.
   * A document tab records its document's explicit language, if any;
   * restoration applies it unless the live document already has an explicit
   * language. */
  export type LayoutTab =
    | Readonly<{ kind: "document"; document: DocumentId | null; uri: string | null; view: EditorViewState; language?: string }>
    | Readonly<{ kind: "tool"; tool: string }>
    | Readonly<{ kind: "terminal"; directory?: string }>;
  export type LayoutNode =
    | Readonly<{ kind: "pane"; dock: "left" | "right" | "bottom" | null; selected: number; tabs: readonly LayoutTab[] }>
    | Readonly<{
        kind: "split";
        direction: "horizontal" | "vertical";
        /** The fraction of the split's extent given to `first`. */
        ratio: number;
        first: LayoutNode;
        second: LayoutNode;
      }>;
  /** Logical bounds, presentation state, and the display UUID. */
  export type WindowGeometry = Readonly<{
    x: number;
    y: number;
    width: number;
    height: number;
    state: "windowed" | "maximized" | "fullscreen";
    display: string | null;
  }>;
  /** Versioned, JSON-compatible window layout from workbench.captureLayout.
   * Store it verbatim; it holds no live handles and is assignable to JsonValue. */
  export type WindowLayout = Readonly<{
    version: 1;
    geometry: WindowGeometry | null;
    root: LayoutNode | null;
    /** Index of the focused pane in layout order. */
    focused: number | null;
    dockSizes: Readonly<{ left: number; right: number; bottom: number }>;
  }>;
  export interface SessionWindow {
    /** Ordered directory roots, validated and deduplicated as in
     * workspaces.replaceRoots. */
    readonly roots: readonly string[];
    readonly entries: readonly SessionEntry[];
    /** Index of the entry whose tab is selected when no layout applies. */
    readonly selected?: number | null;
    /** A captured layout. Each document tab resolves to the entry with the
     * same document, else the same URI; unmatched document tabs, unregistered
     * tools, and repeated documents in a pane are pruned, then empty panes and
     * one-child splits collapse. Entries the layout does not place open in the
     * normal group, and tools it does not name are hidden. An unsupported
     * version or shape is ignored. Geometry is fitted to connected displays. */
    readonly layout?: WindowLayout | JsonValue | null;
  }
  export interface ReplaceWindowsOptions {
    /** Every document presented in the released windows, exactly. */
    readonly suspend: readonly SuspendedDocument[];
    /** Destination windows in order; each one is created. */
    readonly windows: readonly SessionWindow[];
    /** Windows leaving the session. They close as any window does, stopping
     * their terminals. */
    readonly release?: readonly WindowTarget[];
    /** Index of the destination window that receives focus. */
    readonly focus?: number | null;
  }
  /** How one entry resolved. "live" presented an existing document; "missing"
   * means a clean file or untitled document no longer exists; "failed" entries
   * were skipped with a host error code while the rest committed. */
  export type SessionEntryOutcome = Readonly<
    | { kind: "live" | "opened"; document: DocumentId }
    | { kind: "restored"; document: DocumentId; resource: "untitled" | "unchanged" | "missing" | "changed" }
    | { kind: "missing" }
    | { kind: "failed"; code: HostErrorCode; message: string }>;
  export interface ReplaceWindowsResult {
    /** Created windows in destination order. layout is null without a layout
     * and "ignored" for an unsupported version or shape. */
    readonly windows: readonly Readonly<{ window: WindowTarget; entries: readonly SessionEntryOutcome[];
      layout: "applied" | "ignored" | null }>[];
    /** True when every restored dirty document was durably protected by
     * automatic recovery before resolution, so its captures may be released. */
    readonly protected: boolean;
  }
  /** Contributed view registration, captured selection, and clipboard access. */
  export const workbench: Readonly<{
    /** Atomically register and capture current metadata plus sequence watermark.
     * Later committed changes arrive as asynchronous FIFO listener roots after the
     * observing root finishes. Queries may already see newer state. Exceptions are
     * isolated; disposal/unload prevents queued author callbacks. Awaiting another
     * host request or self-disposal from a listener is supported. No replay/retries.
     * Valid during activation before windows exist; operations do not await listeners.
     */
    observe(listener: (event: ApplicationEvent) => void | Promise<void>): Promise<WorkbenchObservation>;
    /** Closes this exact native tab through protected closure. Dirty final views
     * offer Save / Don't Save / Cancel. Cancel returns { kind: "cancelled" };
     * a committed close returns { kind: "closed" } without awaiting observers.
     * Other presentations keep a shared document live. Replaced/transferred/closed
     * targets reject InvalidTarget; concurrent closure rejects OperationInProgress;
     * save failures reject PersistenceFailed and leave the presentation intact.
     * Caller cancellation/unload before closure commits prevents the close; an
     * accepted save may already have persisted content and cannot be rolled back. */
    close(target: ViewTarget): Promise<PresentationCloseOutcome>;
    /** Close a set of windows and open their replacements in one foreground
     * commit, without prompts: final-view text must already be retained.
     * Resources and captures are read before the commit; a commit then
     * revalidates the released windows, the suspended documents, and each
     * retained revision, so observers see only the committed result. Every
     * destination window is created with its layout and geometry before the
     * released windows close. Restoration never changes the fresh-window tool
     * default. Rejection changes nothing.
     * @throws HostError with OperationInProgress during closure or another
     * replacement, SessionConflict when an edit, tab, or document changed after
     * the caller's capture, InvalidTarget, Cancelled (caller cancellation or
     * unload before the commit), InvalidUri, or the
     * workspaces.openFolder resource codes for a destination root. */
    replaceWindows(options: ReplaceWindowsOptions): Promise<ReplaceWindowsResult>;
    /** Capture a window's layout for a later replaceWindows.
     * @throws HostError with InvalidTarget for a closed window, or Cancelled. */
    captureLayout(window: WindowTarget): Promise<WindowLayout>;
    /**
     * Register a lifecycle-owned native view, even before windows exist. The
     * contribution ID is the native view kind and command/keybinding scope.
     * Every product window receives at most one live instance. Resolution
     * acknowledges native ownership; teardown racing registration rejects
     * instead of publishing a late callable object.
     * @throws HostError with ViewIdInUse, InvalidViewId or Cancelled; TypeError
     * for invalid fields or a missing getChildren method.
     */
    registerView(contribution: ViewContribution): Promise<ViewRegistration>;
    /**
     * Present a searchable single-selection picker with a native text field and
     * local fuzzy matching. Resolution is a typed selection or dismissal.
     * @throws TypeError for invalid options; HostError with OperationInProgress
     * when another extension interaction owns the window, InvalidTarget for a
     * stale target, ContextUnavailable without a target or command capture, or
     * Cancelled on window closure, unload or caller cancellation.
     */
    showPicker(options: PickerOptions): Promise<PickerResult>;
    /**
     * Present a single-line text field with a title and optional initial text.
     * Accepting returns the current text; Escape returns dismissal.
     * @throws TypeError for invalid options; HostError with the same set as
     * showPicker.
     */
    showInput(options: InputOptions): Promise<InputResult>;
    /**
     * Present the native confirmation prompt. The cancel button and window
     * closure map to dismissal.
     * @throws TypeError for invalid options; HostError with the same set as
     * showPicker.
     */
    showConfirmation(options: ConfirmationOptions): Promise<ConfirmationResult>;
    /**
     * Present the native message prompt. Resolution carries no result.
     * @throws TypeError for invalid options; HostError with the same set as
     * showPicker.
     */
    showMessage(options: MessageOptions): Promise<void>;
    /**
     * Read selection for an admitted contributed view handler; no selection is
     * null. Omission uses its current capture; explicit target must be authorized
     * for that invocation. Native editor/terminal selection is not exposed here.
     * @throws HostError with ContextUnavailable outside a command, InvalidTarget
     * for invalid captures, UnsupportedOperation for unsupported surfaces,
     * or Cancelled after cancellation.
     */
    selectedText(target?: CommandTarget): Promise<string | null>;
    /**
     * Write application clipboard text without a window/selection requirement.
     * Valid during activation and callbacks. Resolution acknowledges the native
     * clipboard write. TypeError for nontext; HostError on cancellation/teardown.
     */
    writeClipboardText(text: string): Promise<void>;
  }>;
  /** Optional focused view scope; omission/null means application-wide. */
  export interface KeybindingOptions { readonly view?: string | null; }
  /** Command name and JSON arguments, defaulting to null when omitted. */
  export interface KeybindingCommand { readonly name: string; readonly arguments?: JsonValue; }
  /** One preset binding; a null command unbinds the key. A view scope names
   * a view kind; a binding for a kind that is not declared is skipped. */
  export interface KeymapPresetBinding {
    readonly key: string;
    readonly command: string | KeybindingCommand | null;
    readonly view?: string | null;
  }
  /** A named set of default bindings. Title defaults to the name. */
  export interface KeymapPresetDefinition {
    readonly name: string;
    readonly title?: string;
    readonly bindings: readonly KeymapPresetBinding[];
  }
  /** A preset as `getPreset` returns it, with every field present. */
  export interface KeymapPreset {
    readonly name: string;
    readonly title: string;
    readonly bindings: readonly {
      readonly key: string;
      readonly command: { readonly name: string; readonly arguments: JsonValue } | null;
      readonly view: string | null;
    }[];
  }
  export interface KeymapPresetInfo { readonly name: string; readonly title: string; }
  /** The operating system Knot runs on, for platform-specific configuration. */
  export const platform: "macos" | "windows" | "linux";
  /**
   * Key bindings and keymap presets. The selected preset supplies default
   * bindings beneath every `bind`; the `keymap.preset` setting selects it,
   * resolved when startup completes and whenever the setting changes. An
   * unregistered name selects the native `"knot"` preset.
   */
  export const keymaps: Readonly<{
    /**
     * Bind a gpui key sequence, e.g. "cmd-g", in the caller's lifecycle-owned slot.
     * Null command masks earlier bindings and leaves raw input available.
     * Every binding outranks the preset. Among the rest, a view-specific
     * binding beats a global one and then the latest write wins, so personal
     * config overrides a package's binding from post-init, which runs after
     * packages start. Teardown reveals the previous write.
     * Synchronous void call: mutations commit atomically at the end of the V8 turn.
     * Invalid input fails the whole batch through its turn/startup/command result,
     * with no partial map. No separate promise or asynchronous HostError is returned.
     */
    bind(key: string, command: string | KeybindingCommand | null, options?: KeybindingOptions | null): void;
    /**
     * Remove only the caller's slot for this key/scope, revealing lower owners.
     * Same synchronous atomic turn-batch validation/completion as bind.
     * Lifecycle teardown removes its slots without requiring explicit removal.
     */
    unbind(key: string, options?: KeybindingOptions | null): void;
    /** Register a lifecycle-owned preset. The latest registration of a name
     * wins and teardown reveals the previous one. A derived preset spreads
     * `getPreset`'s bindings. Registering the selected name takes effect at
     * the next selection change.
     * @throws TypeError for invalid fields or key sequences, or the reserved
     * name "knot"; HostError with Cancelled. */
    registerPreset(definition: KeymapPresetDefinition): Promise<Disposable>;
    /** The preset `name` selects, including the native "knot" preset for
     * the current platform; null when no preset has that name. */
    getPreset(name: string): Promise<KeymapPreset | null>;
    /** "knot" and every registered preset, sorted by name. */
    listPresets(): Promise<readonly KeymapPresetInfo[]>;
  }>;
}
