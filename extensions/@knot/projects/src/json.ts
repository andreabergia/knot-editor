import type { JsonValue } from "knot";

/** The value as a JSON object, or null for any other JSON value. */
export function object(value: JsonValue): Record<string, JsonValue> | null {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, JsonValue>)
    : null;
}
