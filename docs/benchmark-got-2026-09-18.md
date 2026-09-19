# Benchmark record: got × mimo-v2.5-free, 2026-09-18

## Subject

- Repository: `sindresorhus/got` @ `687eb7d` ("Fix assigning a partial `pagination` object to `Options`"), cloned fresh.
- Analyzer result: 85 files, 11551… (got: 85 files) — **3 CERTAIN files, 0 HIGH, 171 removable LOC**:
  - `benchmark/server.ts` (local HTTPS test server for manual benchmarks, no importers)
  - `documentation/examples/gh-got.js`
  - `documentation/examples/uppercase-headers.js`
- `MEDIUM`/`LOW` untouched. Pre-clean verification: `ava test/headers.ts
  test/retry.ts test/hooks.ts` → 282 passed on BASE and on DIET-applied copy.

## Tasks (`benchmarks/got/tasks.json`, tests beside it)

1. `throw-http-errors-array` — accept `number[]` in `throwHttpErrors`; listed
   statuses resolve, others throw; boolean behavior unchanged.
   Verify: `ava test/diet-task-throw-http-errors-array.ts test/retry.ts`.
2. `response-redirected` — fetch-parity `redirected` boolean on responses
   (promise + stream APIs + types).
   Verify: `ava test/diet-task-response-redirected.ts test/redirects.ts`.
3. `urlsearchparams-body` — accept `URLSearchParams` as request body
   (form-encoding + content-type + length).
   Verify: `ava test/diet-task-urlsearchparams-body.ts test/post.ts`.

All task tests failed pre-change for the right reasons (validation error /
`undefined` / chunk error); no-change guards passed.

## Protocol

- Agent: `opencode run --format json --auto -m opencode/mimo-v2.5-free`
  (default reasoning) via `diet-code benchmark --reps 1`.
- Fresh `git clone` + fresh agent session per run; `node_modules` symlinked
  from the source root (identical both arms); task test file staged
  identically in both arms; run order randomized by seeded shuffle;
  DIET = deterministic cleanup only (3 file deletions, no AI).
- Success = verify exit code only. Telemetry parsed from the JSON event
  stream (`step_finish.tokens` incl. cache, `tool_use` events; file
  exploration = read/glob/grep/list tool uses).

## Results (reps=1 — pilot scope)

| Task | Input BASE→DIET | Tools BASE→DIET | Files BASE→DIET | Time BASE→DIET | Success |
|---|---|---|---|---|---|
| throw-http-errors-array | 66K→45K (−31%) | 12→8 (−33%) | 9→6 (−33%) | 585s→420s | 1/1 both |
| response-redirected | 74K→155K (+111%) | 7→12 (+71%) | 4→3 (−25%) | 580s→229s | 1/1 both |
| urlsearchparams-body | 34K→71K (+106%) | 6→5 (−17%) | 5→2 (−60%) | 421s→80s | 1/1 both |

Raw data: `benchmarks/got/benchmark-2026-09-18-mimo-r1.json`.

## Reading (honest)

- Files-read lower on DIET in 3/3 tasks. Tokens/tools move both ways.
- Free-tier run-to-run variance is large (same task/branch varied 2–4× in
  tokens/time across runs), so reps=1 deltas are noise-dominated. No
  causality claimed. Success unchanged (6/6) — the diet broke nothing.
- The DIET includes docs/harness files (v0.1 prices structure, not value;
  scoping is via `diet-code.json`). A ≥3-rep rerun is required for a verdict.
