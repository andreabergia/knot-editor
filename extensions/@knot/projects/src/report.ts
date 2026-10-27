import { workbench } from "knot";
import type { WindowTarget } from "knot";

export function describe(error: unknown): string {
  const code =
    error !== null && typeof error === "object" ? (error as { code?: unknown }).code : undefined;
  const message = error instanceof Error ? error.message : undefined;
  if (typeof code === "string") {
    return typeof message === "string" && message !== code ? `${code}: ${message}` : code;
  }
  return message ?? String(error);
}

export function errorCode(error: unknown): string | null {
  if (error === null || typeof error !== "object") return null;
  const code = (error as { code?: unknown }).code;
  return typeof code === "string" ? code : null;
}

/**
 * Shows a message in the command's window, or in an explicit one when a
 * switch closed the command's window.
 */
export async function report(message: string, target?: WindowTarget) {
  try {
    await workbench.showMessage({
      title: "Projects",
      message,
      ...(target === undefined ? {} : { target }),
    });
  } catch {
    // A report without a live window is not actionable; the tree still updates.
  }
}
