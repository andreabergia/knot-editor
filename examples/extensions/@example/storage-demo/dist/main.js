import { commands, storage, workbench } from "knot";

// Activation needs no window or command context.
const previous = await storage.read("activations");
const activations = previous.found && typeof previous.value === "number" ? previous.value + 1 : 1;
await storage.batch([
  { type: "write", key: "activations", value: activations },
  { type: "write", key: "example", value: { version: 1, tags: ["durable", "private"], optional: null } },
  { type: "delete", key: "obsolete" },
]);

await commands.register("example.storage-report", async () => {
  await workbench.writeClipboardText(JSON.stringify({
    activations: await storage.read("activations"),
    example: await storage.read("example"),
    missing: await storage.read("missing"),
    keys: await storage.list(),
  }));
});
