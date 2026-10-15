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
   * Reopening allocates a new identity; restart reuse is not guaranteed. */
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
    readonly kind: "untitled" | "destination" | "persisted" | "generated";
    readonly resourceUri: string | null;
    readonly dirty: boolean;
    readonly revision: number;
    readonly buffer: Buffer;
  }
  export interface PresentationInfo {
    readonly target: ViewTarget;
    readonly kind: "document" | "terminal";
    readonly document: DocumentId | null;
    readonly buffer: Buffer | null;
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
  export const documents: Readonly<{
    list(): Promise<readonly DocumentInfo[]>;
    get(id: DocumentId): Promise<DocumentInfo | null>;
  }>;
  /** Only product windows; diagnostic and startup-error windows are excluded.
   * get returns null after closure; fabricated/command targets reject InvalidTarget. */
  export const windows: Readonly<{
    list(): Promise<readonly WindowInfo[]>;
    get(target: WindowTarget): Promise<WindowInfo | null>;
  }>;
  /** Window-owned ordered roots; empty roots are valid. Closed windows return null. */
  export const workspaces: Readonly<{
    get(target: WindowTarget): Promise<WorkspaceInfo | null>;
  }>;

  /**
   * Host-issued capture, valid only in its originating command invocation tree.
   * Contextual methods validate both authority and the live presentation.
   * Copying this object or retaining it after invocation does not grant access.
   */
  export interface CommandTarget {
    readonly [targetAuthority]: true;
    /** Originating product window, captured before palette/focus changes. */
    readonly window: WindowId;
    /** Captured extension-addressable view, or null when none is available. */
    readonly view: ViewId | null;
  }
  /**
   * JSON-compatible data: finite numbers, dense arrays and plain string-keyed
   * records recursively. Cycles, undefined, functions, symbols, bigint,
   * nonplain objects and custom toJSON conversion are rejected.
   */
  export type JsonValue = null | boolean | number | string | readonly JsonValue[]
    | { readonly [key: string]: JsonValue };
  /** Stable host failure codes; names, messages and stacks are diagnostics. */
  export type HostErrorCode = "ContextUnavailable" | "InvalidTarget" | "OwnershipDisposed"
    | "Cancelled" | "UnsupportedOperation" | "BufferClosed" | "BufferReadOnly"
    | "InvalidRange" | "InvalidEditBatch" | "RevisionConflict" | "ContributionSetNotFound"
    | "TreeViewNotFound" | "ViewNotFound" | "TreeProviderInUse" | "TreeProviderNotFound"
    | "CompletionProviderNotFound" | "CommandNameInUse" | "CommandNotFound";
  /**
   * Native operation failure. Narrow a caught Error by name === "KnotHostError"
   * and its string code property, then dispatch on code. No error constructor
   * is exported. Author-side TypeError/RangeError are separate errors.
   */
  export interface HostError extends Error {
    readonly name: "KnotHostError";
    /** Stable category for handling a failure independently of its message. */
    readonly code: HostErrorCode;
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
     * tree and completion registrations use their resource-specific not-found
     * codes; stale subscriptions use OwnershipDisposed. Contribution cleanup
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
  /** Theme-aware decoration and gutter token interpreted by native views. */
  export type SemanticToken = "info" | "warning" | "error";
  /**
   * Anchored semantic range. Optional decoration, gutter and command may be null.
   * Edits stabilize its anchors. Command is a name, not structured arguments.
   */
  export interface EditorContribution {
    readonly range: ByteRange;
    readonly decoration?: SemanticToken | null;
    readonly gutter?: SemanticToken | null;
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
  /**
   * Execution status rather than a semantic result. Unavailable includes declined
   * routing and unsupported composition; invalidTarget identifies a stale capture.
   * InvalidArgument and handlerFailure include diagnostics in message.
   */
  export type CommandOutcome = Readonly<
    { kind: "completed" | "unavailable" | "invalidTarget" | "cancelled" }
    | { kind: "invalidArgument" | "handlerFailure"; message: string }
  >;
  /** Native tree semantics; optional description, icon and command are nullable. */
  export interface TreeItem {
    /** Author-supplied identity, stable and unique within the provider tree. */
    readonly id: string;
    readonly label: string;
    readonly description?: string | null;
    readonly icon?: "file" | "folder" | "symbol" | null;
    /** Required expansion state; never inferred from child results. */
    readonly collapsibleState: "none" | "collapsed" | "expanded";
    readonly command?: string | null;
  }
  /** Tree callbacks have no implicit window, active buffer or public signal. */
  export interface TreeDataProvider {
    /**
     * Return children of parentId; null requests roots. Must return an iterable
     * of valid items, not a string or accidental array-like object.
     * Throws, rejections and invalid items become isolated request diagnostics.
     * Native generation validation discards stale results. Receiver cancellation
     * releases pending work without promising JavaScript stack unwind.
     */
    getChildren(parentId: string | null): Iterable<TreeItem> | Promise<Iterable<TreeItem>>;
  }
  /** Registration owns the declared tree kind across its live native surfaces. */
  export interface TreeRegistration extends Disposable {
    /**
     * Invalidate root children when parentId is omitted/null, otherwise that
     * parent's children. Resolution acknowledges native invalidation, not fetch
     * or paint. Calls accepted before disposal retain request order and settle
     * before cleanup acknowledgement.
     * @throws HostError with OwnershipDisposed after disposal starts,
     * TreeProviderNotFound for stale native ownership, or Cancelled on teardown.
     */
    invalidate(parentId?: string | null): Promise<void>;
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
     * Register a global command name; its palette title is the same name.
     * Valid during activation. Resolves when native ownership exists;
     * teardown races reject instead of publishing a late callable handler.
     * @throws TypeError for invalid arguments; HostError with CommandNameInUse
     * when the name is already owned, or Cancelled on teardown.
     */
    register(name: string, handler: CommandHandler): Promise<Disposable>;
    /**
     * Handle an existing command for the declared outline view kind. Native
     * routing binds live instances and preserves the captured presentation.
     * Resolution acknowledges registration; disposal removes its bindings.
     * @throws HostError with ViewNotFound, CommandNotFound, CommandNameInUse
     * or Cancelled; TypeError for invalid argument types.
     */
    registerForView(viewKind: "outline", name: string, handler: CommandHandler): Promise<Disposable>;
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
  /** Native tree registration, supported captured selection and clipboard access. */
  export const workbench: Readonly<{
    /**
     * Register the declared outline tree provider, even before windows exist.
     * Resolution acknowledges native ownership on its kind; teardown racing
     * registration rejects instead of publishing a late callable object.
     * @throws HostError with TreeViewNotFound, TreeProviderInUse or Cancelled;
     * TypeError for invalid argument types or a missing getChildren method.
     */
    registerTreeDataProvider(viewKind: "outline", provider: TreeDataProvider): Promise<TreeRegistration>;
    /**
     * Read selection for an admitted outline view handler; no selection is null.
     * Omission uses its current capture; explicit target must be authorized for
     * that invocation. Native editor/terminal selection is not exposed here.
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
  export interface KeybindingOptions { readonly view?: "editor" | "terminal" | "outline" | null; }
  /** Command name and JSON arguments, defaulting to null when omitted. */
  export interface KeybindingCommand { readonly name: string; readonly arguments?: JsonValue; }
  /**
   * Bind a gpui key sequence, e.g. "cmd-g", in the caller's lifecycle-owned slot.
   * Null command masks lower-priority bindings and leaves raw input available.
   * Personal config outranks extensions, which outrank defaults; view-specific
   * and later registrations resolve ties within owner priority.
   * Synchronous void call: mutations commit atomically at the end of the V8 turn.
   * Invalid input fails the whole batch through its turn/startup/command result,
   * with no partial map. No separate promise or asynchronous HostError is returned.
   */
  export function keybinding(key: string, command: string | KeybindingCommand | null, options?: KeybindingOptions | null): void;
  /**
   * Remove only the caller's slot for this key/scope, revealing lower owners.
   * Same synchronous atomic turn-batch validation/completion as keybinding.
   * Lifecycle teardown removes its slots without requiring explicit removal.
   */
  export function removeKeybinding(key: string, options?: KeybindingOptions | null): void;
}
