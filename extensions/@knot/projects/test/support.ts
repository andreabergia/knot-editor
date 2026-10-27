// Seeds stored records and starts the package against the fake host.
import type { JsonValue } from "knot";
import * as host from "knot/testing";
import { start } from "../src/extension.js";

export const ALPHA = "file:///src/alpha/";
export const BETA = "file:///src/beta/";

export type SeededWindow = { entries: JsonValue[]; selected?: number | null; layout?: JsonValue };

export function seedProject(id: string, name: string, roots: string[]) {
  const stored = host.record("projects") as { projects: JsonValue[] } | undefined;
  host.setRecord("projects", {
    schema: 1,
    projects: [...(stored?.projects ?? []), { id, name, roots, createdAt: 1, lastOpenedAt: 1 }],
    recentProjectId: null,
  });
}

export function seedSession(id: string, state: "active" | "suspended", windows: SeededWindow[] = []) {
  host.setRecord(`session:${id}`, {
    schema: 2,
    state,
    windows: windows.map((window) => ({
      entries: window.entries,
      selected: window.selected ?? null,
      layout: window.layout ?? null,
    })),
  });
}

/** Starts the package after startup windows exist, as a launched product. */
export async function startLaunched() {
  await start();
  await host.launch();
}

export function session(id: string) {
  return host.record(`session:${id}`) as
    | { state: string; windows: { entries: Record<string, string>[]; selected: number | null }[] }
    | undefined;
}

export function registry() {
  return host.record("projects") as {
    projects: { id: string; name: string; roots: string[] }[];
    recentProjectId: string | null;
  };
}
