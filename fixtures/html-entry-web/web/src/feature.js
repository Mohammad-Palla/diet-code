// Imported directly by main.js, and itself imports deep.js transitively.
// This proves transitive reachability from a nested-src entry.
import { deepHelper } from "./deep.js";

export function runFeature() {
  return deepHelper();
}
