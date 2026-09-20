// Genuinely dead: imported by nobody, unreachable from the package entry.
// The fix must NOT over-suppress — this orphan is still reported.
export function orphan(): string {
	return 'orphan';
}
