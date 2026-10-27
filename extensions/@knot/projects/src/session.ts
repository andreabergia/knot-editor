// Session records: one `session:<id>` storage record per project describing
// its windows, their documents, and their layouts. Suspended records may hold
// retained captures of unsaved text.
import type { CaptureId, DocumentId, JsonValue } from "knot";
import { object } from "./json.js";

export const SESSION_SCHEMA = 2;

export type Entry = {
  uri?: string;
  document?: DocumentId;
  capture?: CaptureId;
};

export type SessionWindow = {
  entries: Entry[];
  selected: number | null;
  layout: JsonValue | null;
};

export type Session = {
  schema: number;
  state: "active" | "suspended";
  windows: SessionWindow[];
};

export const EMPTY_WINDOW: SessionWindow = { entries: [], selected: null, layout: null };

export function sessionKey(id: string) {
  return `session:${id}`;
}

function entryFromJson(value: JsonValue): Entry | null {
  const record = object(value);
  if (record === null) return null;
  const entry: Entry = {};
  if (typeof record.uri === "string") entry.uri = record.uri;
  if (typeof record.document === "string") entry.document = record.document as DocumentId;
  if (typeof record.capture === "string") entry.capture = record.capture as CaptureId;
  return entry.uri === undefined && entry.document === undefined ? null : entry;
}

/**
 * Reads a session record; schema 1 windows have no layout. Any other schema
 * or shape is unreadable and must be preserved as stored.
 */
export function sessionFromJson(value: JsonValue): Session | "unreadable" {
  const record = object(value);
  if (record === null || (record.schema !== 1 && record.schema !== SESSION_SCHEMA)) return "unreadable";
  if (!Array.isArray(record.windows)) return "unreadable";
  if (record.state !== "active" && record.state !== "suspended") return "unreadable";
  const windows: SessionWindow[] = [];
  for (const window of record.windows) {
    const fields = object(window);
    if (fields === null || !Array.isArray(fields.entries)) return "unreadable";
    const entries = [];
    for (const entry of fields.entries) {
      const parsed = entryFromJson(entry);
      if (parsed !== null) entries.push(parsed);
    }
    const selected = typeof fields.selected === "number" ? fields.selected : null;
    const layout = record.schema === 1 ? null : (fields.layout ?? null);
    windows.push({ entries, selected, layout });
  }
  return { schema: SESSION_SCHEMA, state: record.state, windows };
}

export function capturesOf(session: Session | null | undefined): Set<CaptureId> {
  const captures = new Set<CaptureId>();
  for (const window of session?.windows ?? []) {
    for (const entry of window.entries) {
      if (entry.capture !== undefined) captures.add(entry.capture);
    }
  }
  return captures;
}

export function sameSession(left: Session, right: Session) {
  return JSON.stringify(left) === JSON.stringify(right);
}

/**
 * The suspended form of a session that lost its last window: the same
 * membership and layouts without captures, because closure already asked
 * about dirty documents.
 */
export function suspendedWithoutCaptures(session: Session | undefined): Session {
  return {
    schema: SESSION_SCHEMA,
    state: "suspended",
    windows: (session?.windows ?? []).map((window) => ({
      entries: window.entries.map((entry) => {
        const reference: Entry = {};
        if (entry.uri !== undefined) reference.uri = entry.uri;
        if (entry.document !== undefined) reference.document = entry.document;
        return reference;
      }),
      selected: window.selected,
      layout: window.layout,
    })),
  };
}
