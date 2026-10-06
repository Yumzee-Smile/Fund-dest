/** JSON helpers that keep bigint values as decimal strings. */
export function toJson(v: unknown, indent = 2): string {
  return JSON.stringify(v, (_k, x) => (typeof x === "bigint" ? x.toString() : x), indent);
}
