import * as knot from "knot";
const { commands } = knot;
import { commandName } from "./names.js";

await commands.register("example.greet", async () => {
  await commands.invoke(commandName, null);
});
