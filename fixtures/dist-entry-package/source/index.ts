// Package entry (published as distribution/index.js, but the SOURCE lives
// here at source/index.ts). Imports a util directly.
import {normalize} from './utils/normalize.js';

export function main(input: string): string {
	return normalize(input);
}
