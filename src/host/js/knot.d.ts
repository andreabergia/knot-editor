/** Proposed contract; see docs/extension-api.md for implementation status. */
declare module "knot" {
  const windowIdentity: unique symbol;
  const viewIdentity: unique symbol;
  const targetAuthority: unique symbol;
  const bufferIdentity: unique symbol;
  export type WindowId = string & { readonly [windowIdentity]: true };
  export type ViewId = string & { readonly [viewIdentity]: true };

  /** Foreground-issued capture; only usable in its originating invocation tree. */
  export interface CommandTarget {
    readonly [targetAuthority]: true;
    readonly window: WindowId;
    readonly view: ViewId | null;
  }
  export type JsonValue = null | boolean | number | string | readonly JsonValue[]
    | { readonly [key: string]: JsonValue };
  export type HostErrorCode = "ContextUnavailable" | "InvalidTarget" | "OwnershipDisposed"
    | "Cancelled" | "UnsupportedOperation" | "BufferClosed" | "BufferReadOnly"
    | "InvalidRange" | "InvalidEditBatch" | "RevisionConflict" | "ContributionSetNotFound"
    | "TreeViewNotFound" | "ViewNotFound" | "TreeProviderInUse" | "TreeProviderNotFound"
    | "CompletionProviderNotFound" | "CommandNameInUse" | "CommandNotFound";
  /** Host errors are Error instances named KnotHostError with a stable code. */
  export interface HostError extends Error {
    readonly name: "KnotHostError";
    readonly code: HostErrorCode;
  }
  export interface Disposable {
    /** Locally inactive immediately; shared promise acknowledges foreground release. */
    dispose(): Promise<void>;
  }
  /** Half-open, absolute UTF-8 bytes; endpoints must be scalar boundaries. */
  export interface ByteRange {
    readonly startByteOffset: number;
    readonly endByteOffset: number;
  }
  export interface TextEdit { readonly range: ByteRange; readonly text: string; }
  export interface RevisionOptions { readonly ifRevision: number; }
  export interface TextSnapshot {
    readonly text: string;
    readonly range: ByteRange;
    readonly revision: number;
    /** Both conversions are relative to snapshot.text, including ranged snapshots. */
    byteOffsetAtUtf16(offset: number): number;
    utf16OffsetAtByte(offset: number): number;
  }
  export interface BufferChange {
    readonly buffer: Buffer;
    readonly beforeRevision: number;
    readonly revision: number;
    readonly edits: readonly TextEdit[];
  }
  export type SemanticToken = "info" | "warning" | "error";
  export interface EditorContribution {
    readonly range: ByteRange;
    readonly decoration?: SemanticToken | null;
    readonly gutter?: SemanticToken | null;
    readonly command?: string | null;
  }
  export interface EditorContributions extends Disposable {
    replace(items: readonly EditorContribution[], options: RevisionOptions): Promise<void>;
  }
  export interface Buffer {
    readonly [bufferIdentity]: true;
    snapshot(range?: ByteRange | null): Promise<TextSnapshot>;
    applyEdits(edits: readonly TextEdit[], options: RevisionOptions): Promise<Readonly<{ revision: number }>>;
    onDidChange(listener: (change: BufferChange) => void | Promise<void>): Promise<Disposable>;
    readonly contributions: EditorContributions;
  }
  /** Knot's supported subset; no browser AbortSignal inheritance. */
  export interface CancellationSignal {
    readonly aborted: boolean;
    readonly reason: unknown;
    addEventListener(type: "abort", listener: (event: CancellationEvent) => void): void;
    removeEventListener(type: "abort", listener: (event: CancellationEvent) => void): void;
    throwIfAborted(): void;
  }
  export interface CancellationEvent {
    readonly type: "abort";
    readonly target: CancellationSignal;
  }
  export interface CommandContext {
    readonly target: CommandTarget;
    readonly buffer: Buffer | null;
    readonly arguments: JsonValue;
    readonly signal: CancellationSignal;
  }
  export type CommandHandler = (context: CommandContext) => void | Promise<void>;
  export type CommandOutcome = Readonly<
    { kind: "completed" | "unavailable" | "invalidTarget" | "cancelled" }
    | { kind: "invalidArgument" | "handlerFailure"; message: string }
  >;
  export interface TreeItem {
    readonly id: string;
    readonly label: string;
    readonly description?: string | null;
    readonly icon?: "file" | "folder" | "symbol" | null;
    readonly collapsibleState: "none" | "collapsed" | "expanded";
    readonly command?: string | null;
  }
  export interface TreeDataProvider {
    getChildren(parentId: string | null): Iterable<TreeItem> | Promise<Iterable<TreeItem>>;
  }
  export interface TreeRegistration extends Disposable {
    /** Acknowledges native invalidation, not refetch or paint. Omission/null means root. */
    invalidate(parentId?: string | null): Promise<void>;
  }
  export interface CompletionContext {
    readonly buffer: Buffer;
    readonly revision: number;
    readonly cursorByteOffset: number;
    readonly prefix: string;
    readonly generation: number;
  }
  export interface CompletionItem { readonly label: string; readonly insertText: string; }
  export interface CompletionProvider {
    provideCompletions(context: CompletionContext): Iterable<CompletionItem> | Promise<Iterable<CompletionItem>>;
  }
  export const editor: Readonly<{
    activeBuffer(target?: CommandTarget): Promise<Buffer | null>;
    registerCompletionProvider(label: string, provider: CompletionProvider): Promise<Disposable>;
  }>;
  export const commands: Readonly<{
    register(name: string, handler: CommandHandler): Promise<Disposable>;
    /** Currently only the declared outline view; command name must already exist. */
    registerForView(viewKind: "outline", name: string, handler: CommandHandler): Promise<Disposable>;
    /** Child invocation only; arguments default to null. */
    invoke(name: string, arguments_?: JsonValue): Promise<CommandOutcome>;
    invalidArguments(message: string): never;
  }>;
  export const workbench: Readonly<{
    registerTreeDataProvider(viewKind: "outline", provider: TreeDataProvider): Promise<TreeRegistration>;
    /** Only a supported captured outline view handler; no selection returns null. */
    selectedText(target?: CommandTarget): Promise<string | null>;
    writeClipboardText(text: string): Promise<void>;
  }>;
  export interface KeybindingOptions { readonly view?: "editor" | "terminal" | "outline" | null; }
  export interface KeybindingCommand { readonly name: string; readonly arguments?: JsonValue; }
  /** Synchronous turn batch; null masks lower-priority bindings in this scope. */
  export function keybinding(key: string, command: string | KeybindingCommand | null, options?: KeybindingOptions | null): void;
  export function removeKeybinding(key: string, options?: KeybindingOptions | null): void;
}
