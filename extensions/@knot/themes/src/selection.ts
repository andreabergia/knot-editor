// The theme the user chose with Select Theme. Settings are not persisted, so
// the package keeps the settings it wrote in its storage and writes them again
// at start, where later personal config still overrides them.
import { settings, storage, themes, workbench } from "knot";
import type { JsonValue, ThemeAppearance } from "knot";

export const RECORD_KEY = "selection";

export type Selection = {
  mode?: "dark" | "light" | "system";
  dark?: string;
  light?: string;
};

function selectionFromJson(value: JsonValue): Selection {
  const record =
    value !== null && typeof value === "object" && !Array.isArray(value)
      ? (value as Record<string, JsonValue>)
      : {};
  const selection: Selection = {};
  if (record.mode === "dark" || record.mode === "light" || record.mode === "system") {
    selection.mode = record.mode;
  }
  if (typeof record.dark === "string") selection.dark = record.dark;
  if (typeof record.light === "string") selection.light = record.light;
  return selection;
}

async function apply(selection: Selection) {
  if (selection.mode !== undefined) await settings.set("theme.mode", selection.mode);
  if (selection.dark !== undefined) await settings.set("theme.dark", selection.dark);
  if (selection.light !== undefined) await settings.set("theme.light", selection.light);
}

/** Writes the stored choice again; a missing or unreadable record keeps the
 * native defaults. */
export async function restore(): Promise<Selection> {
  let selection: Selection = {};
  try {
    const result = await storage.read(RECORD_KEY);
    if (result.found) selection = selectionFromJson(result.value);
  } catch {
    return selection;
  }
  await apply(selection);
  return selection;
}

/** Picks a registered theme and makes it the theme of its appearance, and
 * the active mode unless the mode follows the system. */
export async function select(stored: Selection): Promise<Selection> {
  const current: Record<ThemeAppearance, string | undefined> = {
    dark: await settings.get("theme.dark"),
    light: await settings.get("theme.light"),
  };
  const registered = await themes.list();
  const picked = await workbench.showPicker({
    title: "Select theme",
    items: registered.map((theme) => ({
      id: theme.name,
      label: theme.name,
      description: current[theme.appearance] === theme.name ? `${theme.appearance}, selected` : theme.appearance,
    })),
  });
  if (picked.kind === "dismissed") return stored;
  const theme = registered.find((candidate) => candidate.name === picked.id);
  if (theme === undefined) return stored;
  const selection: Selection = { ...stored, [theme.appearance]: theme.name };
  if ((await settings.get("theme.mode")) !== "system") selection.mode = theme.appearance;
  await apply(selection);
  await storage.write(RECORD_KEY, selection);
  return selection;
}
