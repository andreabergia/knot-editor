// What a window presents, described as session entries. Clean files are
// references only; dirty and untitled documents also record their identity.
import { workbench } from "knot";
import type { CaptureId, DocumentId, DocumentInfo, WindowInfo } from "knot";
import type { Entry, SessionWindow } from "./session.js";

export function windowRoots(window: WindowInfo): string[] {
  return window.workspace.roots.map((root) => root.uri);
}

/** Distinct documents presented in windows, in tab order. */
export function presentedDocuments(list: readonly WindowInfo[]): DocumentId[] {
  const ids: DocumentId[] = [];
  for (const window of list) {
    for (const pane of window.panes) {
      for (const tab of pane.tabs) {
        if (tab.document !== null && !ids.includes(tab.document)) ids.push(tab.document);
      }
    }
  }
  return ids;
}

export function viewCount(list: readonly WindowInfo[], document: DocumentId) {
  let count = 0;
  for (const window of list) {
    for (const pane of window.panes) {
      for (const tab of pane.tabs) if (tab.document === document) count += 1;
    }
  }
  return count;
}

export function documentsById(list: readonly DocumentInfo[]) {
  return new Map(list.map((document) => [document.id, document]));
}

/**
 * The entry recording a presented document, or null for a clean, empty
 * untitled document, which is not worth restoring. `empty` is only consulted
 * for clean untitled documents.
 */
export function entryFor(document: DocumentInfo, empty: boolean): Entry | null {
  if (document.kind === "persisted" && !document.dirty && document.resourceUri !== null) {
    return { uri: document.resourceUri };
  }
  if (document.kind === "untitled") {
    return !document.dirty && empty ? null : { document: document.id };
  }
  return document.resourceUri === null
    ? { document: document.id }
    : { uri: document.resourceUri, document: document.id };
}

/** The focused document, else the first pane's selected document. */
export function selectedDocument(window: WindowInfo): DocumentId | null {
  const current = window.panes
    .flatMap((pane) => pane.tabs)
    .find((tab) => window.currentTarget !== null && tab.target === window.currentTarget);
  return (
    current?.document ??
    window.panes
      .map((pane) => pane.tabs.find((tab) => tab.target === pane.selected))
      .find((tab) => tab?.document != null)?.document ??
    null
  );
}

export async function isEmpty(document: DocumentInfo) {
  return (await document.buffer.snapshot()).text.length === 0;
}

/** Whether the document's text is lost when it stops being live. */
export async function needsCapture(document: DocumentInfo) {
  return document.dirty || (document.kind === "untitled" && !(await isEmpty(document)));
}

/** Describes one window's documents, with captures when given, and its layout. */
export async function describeWindow(
  window: WindowInfo,
  byId: Map<DocumentId, DocumentInfo>,
  captures: Map<DocumentId, CaptureId>,
): Promise<SessionWindow> {
  const entries: Entry[] = [];
  const listed: DocumentId[] = [];
  for (const id of presentedDocuments([window])) {
    const document = byId.get(id);
    if (document === undefined) continue;
    const empty = document.kind === "untitled" && !document.dirty && (await isEmpty(document));
    const entry = entryFor(document, empty);
    if (entry === null) continue;
    const capture = captures.get(id);
    if (capture !== undefined) entry.capture = capture;
    entries.push(entry);
    listed.push(id);
  }
  const selected = selectedDocument(window);
  const index = selected === null ? -1 : listed.indexOf(selected);
  return {
    entries,
    selected: index < 0 ? null : index,
    layout: await workbench.captureLayout(window.target),
  };
}

/**
 * A window that can host another project: no root and no documents beyond
 * one clean, empty untitled document.
 */
export async function isReusable(window: WindowInfo, byId: Map<DocumentId, DocumentInfo>) {
  if (window.workspace.roots.length > 0) return false;
  const [first, ...rest] = presentedDocuments([window]);
  if (first === undefined) return true;
  const document = rest.length === 0 ? byId.get(first) : undefined;
  if (document === undefined) return false;
  return document.kind === "untitled" && !document.dirty && (await isEmpty(document));
}
