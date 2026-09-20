// Imported by source/index.ts, and itself imports constants transitively.
// Before the distribution/->source/ mapping fix this whole tree was flagged
// HIGH dead_file because the package entry (distribution/index.js) resolved
// to nothing.
import {PREFIX} from '../core/constants.js';

export function normalize(input: string): string {
	return PREFIX + input.trim();
}
