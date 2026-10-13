import { commands } from "knot";

/** @param {unknown} error @returns {error is import("knot").HostError} */
function isHostError(error) {
  return error instanceof Error && error.name === "KnotHostError"
    && "code" in error && typeof error.code === "string";
}

await commands.register("guide.insert", async ({ buffer, arguments: value }) => {
  if (typeof value !== "string") commands.invalidArguments("Expected text");
  if (buffer === null) return;
  const snapshot = await buffer.snapshot();
  try {
    await buffer.applyEdits([{
      range: { startByteOffset: 0, endByteOffset: 0 }, text: value,
    }], { ifRevision: snapshot.revision });
  } catch (error) {
    if (isHostError(error) && error.code === "RevisionConflict") {
      // Deliberately abandon this edit rather than silently retry a newer revision.
      return;
    }
    throw error;
  }
});
