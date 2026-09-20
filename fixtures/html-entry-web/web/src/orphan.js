// Genuinely dead: imported by nobody, unreachable from any entry. Proves the
// fix does not over-suppress — a real orphan module is still reported.
export function orphanHelper() {
  return "orphan";
}
