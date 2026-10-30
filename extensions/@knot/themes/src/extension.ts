// Bundled color themes. Every theme registers before start() resolves,
// because the application resolves the selected theme once every package has
// started, before it restores windows.
import { commands, themes } from "knot";
import { restore, select } from "./selection.js";
import { THEMES } from "./themes.js";

export async function start() {
  await Promise.all(THEMES.map((definition) => themes.register(definition)));
  let selection = await restore();
  await commands.register("themes.select", async () => {
    selection = await select(selection);
  });
}
