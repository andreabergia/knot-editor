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

/** The settings that make a theme visible: its appearance's theme, and the
 * mode unless the mode follows the system. */
function choosing(base: Selection, mode: string | undefined, name: string, appearance: ThemeAppearance) {
  const selection: Selection = { ...base, [appearance]: name };
  if (mode !== "system") selection.mode = appearance;
  return selection;
}

/** Picks a registered theme and makes it the theme of its appearance, and
 * the active mode unless the mode follows the system. The highlighted theme
 * is previewed, and dismissing restores the settings from before. */
export async function select(stored: Selection): Promise<Selection> {
  const mode = await settings.get("theme.mode");
  const current: Record<ThemeAppearance, string | undefined> = {
    dark: await settings.get("theme.dark"),
    light: await settings.get("theme.light"),
  };
  const before: Selection = {};
  if (mode !== undefined) before.mode = mode;
  if (current.dark !== undefined) before.dark = current.dark;
  if (current.light !== undefined) before.light = current.light;
  const registered = await themes.list();
  const byName = (name: string | null) => registered.find((candidate) => candidate.name === name);
  let previewed = false;
  const picked = await workbench.showPicker({
    title: "Select theme",
    items: registered.map((theme) => ({
      id: theme.name,
      label: theme.name,
      description: current[theme.appearance] === theme.name ? `${theme.appearance}, selected` : theme.appearance,
    })),
    onHighlight: async (id) => {
      const theme = byName(id);
      if (theme === undefined) return;
      previewed = true;
      await apply(choosing(before, mode, theme.name, theme.appearance));
    },
  });
  const theme = picked.kind === "selected" ? byName(picked.id) : undefined;
  if (theme === undefined) {
    if (previewed) await apply(before);
    return stored;
  }
  const selection = choosing(stored, mode, theme.name, theme.appearance);
  await apply(selection);
  await storage.write(RECORD_KEY, selection);
  return selection;
}
