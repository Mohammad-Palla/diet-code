// Imported ONLY transitively (main -> feature -> deep). Before the fix this
// was falsely flagged HIGH dead_file ("0 production importers") because no
// production entry was discovered. It must now be LIVE (no finding).
export function deepHelper() {
  return "deep";
}
