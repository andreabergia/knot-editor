import * as knot from "knot";
const { commands } = knot;

await commands.register("example.insert-greeting", async ({ buffer }) => {
  const snapshot = await buffer.snapshot();
  await buffer.applyEdits([{
    range: { startByteOffset: 0, endByteOffset: 0 },
    text: "Hello ",
  }], { ifRevision: snapshot.revision });
});
