import { commands } from "knot:editor";
import { commandName } from "./names.js";

await commands.register("example.greet", async () => {
  await commands.invoke(commandName, null);
});
