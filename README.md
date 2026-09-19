# Diet Code

### Put your AI coding agent on a diet.

Your repository can be perfectly valid and still be full of code nobody needs
anymore. AI coding agents have to search through that repository, understand
it, test around it, and sometimes get distracted by it.

Diet Code finds structurally unreachable code — deterministically, with no
LLM — and lets you **measure** whether removing it makes agents do less work.

```text
diet-code analyze .
        ↓
dead-code evidence (FACTS, not scores)
        ↓
diet-code clean            # deterministic, CERTAIN/HIGH only
        ↓
minimal Git diff on diet-code/<timestamp>
        ↓
tests/build pass (fail-closed: revert on failure)
        ↓
diet-code benchmark        # same agent task on BASE vs DIET
        ↓
BEFORE/AFTER agent-work table
```

> **Diet Code is not an "AI-powered dead code detector."**
> Traditional tools ask: *"Is this code unused?"*
> Diet Code asks: *"After removing this obsolete code, does an AI agent
> actually do less work?"* The benchmark is the wedge; the detector is
> infrastructure.

---

## What v0.1 is (and is not)

**It is:** a local-first static repository analyzer + deterministic cleanup
tool + agent benchmark for TypeScript/JavaScript.

**It is not:** an AI refactoring assistant, MCP server, context server,
code-indexing SaaS, or generic linter. There is **no LLM inside the
analyzer** — `cargo run` works with no API key, no model, no network.

---

## Installation

You do **not** need Rust. Diet Code ships as a self-contained binary through
npm (and a shell installer); the Rust toolchain is only for building from
source.

```bash
# npm (recommended — Node 18+, no Rust, no native build)
npm install -g diet-code
npx diet-code analyze .          # or run without installing
```

```bash
# shell installer (Linux/macOS, installs to ~/.local/bin, verifies checksum)
curl -fsSL https://raw.githubusercontent.com/Mohammad-Palla/diet-code/master/scripts/install.sh | sh
```

```bash
# from source (Rust stable)
cargo install diet-code-cli          # from crates.io
cargo install --path crates/diet-code-cli   # from a checkout
```

The npm package downloads the prebuilt binary for your platform
(linux/macOS/Windows × x64/arm64) and verifies its SHA-256 checksum; the
launcher is a ~50-line zero-dependency shim that execs the Rust binary.
Overrides: `DIET_CODE_SKIP_DOWNLOAD=1`, `DIET_CODE_DOWNLOAD_BASE_URL=…`,
`DIET_CODE_BINARY_PATH=/path/to/diet-code`.

## CLI usage

```bash
# Analyze the current repository (human summary + .diet-code/analysis.json)
diet-code analyze .
diet-code analyze . --json        # stable JSON schema (v1)
diet-code analyze . --verbose     # all findings, not just CERTAIN/HIGH

# Explain one finding with its deterministic evidence
diet-code explain src/utils/oldAuth.ts
diet-code explain --path /path/to/repo <finding-id-or-file>

# Preview the deterministic patch plan (changes nothing)
diet-code clean --dry-run

# Apply it on a new diet-code/<timestamp> branch (CERTAIN/HIGH only),
# re-run the analyzer, run verification scripts, fail closed (revert)
# unless everything passes. --keep retains the branch on failure.
diet-code clean
diet-code clean --keep

# Benchmark: same agent tasks on BASE vs DIET, fresh dir + fresh session
# per run, randomized order, medians reported.
diet-code benchmark --tasks benchmarks/got/tasks.json --reps 3
diet-code benchmark --tasks tasks.json --reps 1 --agent-model opencode/muse-spark-1.3-contributor-free
diet-code benchmark --tasks tasks.json --mock   # plumbing check, no agent
```

`tasks.json` format:

```json
[
  {
    "name": "add-rate-limit",
    "prompt": "Add rate limiting to the payment API. Keep changes minimal.",
    "verify": "NODE_OPTIONS='--import=tsx/esm' ./node_modules/.bin/ava test/rate-limit.ts",
    "testFile": "rate-limit.test.ts"
  }
]
```

- `verify` decides success — never the agent's prose. It runs in each fresh
  workdir after the agent finishes.
- `testFile` (optional) is copied from beside `tasks.json` into every run
  workdir as `test/diet-task-<name>.ts`, identically in both arms.
- `--agent-command` overrides discovery (`opencode` preferred, `claude`
  fallback, `--mock` for CI). `--agent-model` selects the model
  (default `opencode/muse-spark-1.3-contributor-free`).
- Reports persist to `.diet-code/benchmark-<timestamp>.json`.

---

## Why dead repository structure matters to agents

An agent solving "add `response.redirected`" in a 300-file repo must list,
read, and rule out files: locales it will never touch, example apps for
other frameworks, benchmark harnesses, vendored scripts. Every irrelevant
file is tokens in, tool calls out, and another chance to get distracted
(e.g. editing the docs example instead of the source).

Diet Code's hypothesis: **removing genuinely unreachable structure reduces
measurable agent work (tokens, tool calls, files read) without changing
task success.** v0.1 exists to test that claim, not to assert it.

---

## Methodology

### Analyzer pipeline (all deterministic)

1. **Traversal** — recursive walk; ignores `.git/ node_modules/ dist/
   build/ coverage/ .next/ .nuxt/ .turbo/ .cache/ vendor/ target/` plus
   `diet-code.json` include/exclude and `.dietignore`; no symlinked dirs;
   parallel parsing (Rust + tree-sitter, TS vs TSX grammars per extension).
2. **AST extraction** — functions, arrows, function expressions, classes,
   methods, variables, enums, interfaces, type aliases, namespaces, with
   byte offsets (for edits) and 1-based lines (for output).
3. **Symbol table** — stable IDs (`file::kind:name`, full parent chains so
   shadowed names never collide); lexical-scope resolution (nearest
   declaration wins; class scopes excluded from bare-name lookup).
4. **Import resolution** (highest-priority correctness area) — relative +
   extensionless + directory index, `tsconfig.json` `baseUrl`/`paths`,
   barrel files (`export *`, named re-exports, chains), default/namespace/
   aliased imports, CommonJS `require` (incl. destructured), `module.exports`
   forms, `export =`, package self-imports, `/// <reference path>` via
   declaration files, subpackage `package.json` entries (monorepos).
5. **Value vs type edges** — `import type` creates `TYPE` edges; computed
   type keys, heritage clauses, annotations, and generics included.
6. **Reference extraction** — calls, `new`, identifiers, member calls with
   safe receivers (`this` via scope walk, `new X()`/typed vars/class names/
   object owners/namespaces), JSX components, `this.method()`, `super`
   (via heritage), object-shorthand uses, computed member reads.
7. **Dynamic-use detection** — `import(expr)`, `require(expr)`,
   `import.meta.resolve(expr)`, `path.join/resolve(__dirname, …)`,
   `eval`, `Reflect.*`, global computed access, `obj[key]()` dispatch,
   directory scans (`readdir`+`require`). Unresolvable dynamic loading
   protects nearby files (prefix dirs) instead of pretending.
8. **Entry points** — `diet-code.json` first, then `package.json`
   (`main/module/browser/exports/bin/types`, per package in monorepos),
   then conservative conventions (`src/main.*`, `src/index.*`, …).
   Tests (`test/`, `*.test.*`, …) form a **separate** root set.
9. **Reachability** — file BFS from production ∪ public-surface ∪
   script-referenced entries; symbol BFS from production entry symbols
   (+ top-level statements). Records production/test reachability separately.
10. **Findings + confidence + Git evidence** (supporting only).

### Trust over cleverness: FACTS vs recommendations

Every finding lists checkable facts: zero incoming references, unreachable
from entry points, zero importers, no package export, no configuration
reference, no dynamic-reference evidence. There are no invented scores.

Confidence is explicit, never numeric:

| Level | Meaning | Auto-removed? |
|---|---|---|
| `CERTAIN` | unexported, zero refs, unreachable, no dynamic risk | ✅ yes |
| `HIGH` | zero production consumers, no entry/config/dynamic risk (test-only refs allowed) | ✅ yes (verify decides) |
| `MEDIUM` | looks unused but external/dynamic use can't be ruled out (public surface, plugins, protocols, dispatch) | ❌ never |
| `LOW` | significant uncertainty (dynamic loading, tooling configs, scripts, ambiguous surface) | ❌ never |

Conservative by construction: exported symbols, package surfaces, tooling
configs (`babel/webpack/jest/karma/…`), script-referenced files, CI
workflow references, test-runner magic (`__mocks__`), agent-tooling dirs,
minified files, ambient/global types, plugin/prototype registration,
callback/hook objects, computed dispatch, inheritance hierarchies
(`super`, overrides share liveness), declaration merging, and
object spread are all capped at `MEDIUM`/`LOW` or skipped. What remains
`CERTAIN` survived all of that.

### Cleanup safety (`clean`)

Byte-range deletions from tree-sitter offsets (full lines only, no
reformatting), `git rm` for dead files, mechanically-proven unused-import
pruning only. Before touching anything: clean-tree check → record HEAD →
new branch → apply → re-analyze → run discovered verify scripts
(`typecheck`/`test`/`build` via the repo's package manager, or
`verifyCommands`) → revert on failure unless `--keep`.

### Benchmark anti-bias rules

Same prompt, same base commit, same model, fresh directory + fresh session
per run, same verify command, no findings leaked into prompts, no human
intervention, randomized run order (`base/diet/diet/base/…`), ≥3 reps
recommended (medians reported with min–max ranges). DIET differs from BASE
**only** by the deterministic cleanup — no AI cleanup, ever. Success comes
only from `verify` exit codes.

---

## Benchmark limitations (read before quoting numbers)

- **Small DIETs on mature repos.** Well-maintained projects yield little
  auto-removable product code (linters already catch unused locals). Our
  `got` DIET was 3 files / 171 LOC (a benchmark harness server + 2 docs
  examples). Effect sizes scale with diet size; do not extrapolate.
- **Scope.** v0.1 cannot price the *value* of docs/examples; they count as
  structure. `diet-code.json` `include`/`exclude` is the scoping mechanism.
- **Variance.** Free-tier models are slow and noisy (identical task/branch
  runs differed by 2–4× in tokens/time). Treat single-rep deltas as noise;
  use ≥3 reps and read the ranges.
- **Token accounting.** We sum fresh input + cache reads/writes (comparing
  raw `input` alone would reward warm caches, not smaller repos).
- **Time** is dominated by model latency, not repo size — it is reported,
  not claimed as an effect.
- **No causality beyond the experiment.** Numbers describe these tasks, this
  repo, this model, this day.

## Measured result: `got` × `opencode/mimo-v2.5-free` (2026-09-18)

Subject: [`sindresorhus/got`](https://github.com/sindresorhus/got) @ `687eb7d`
(85 files analyzed). Diet Code removed **3 files / 171 LOC** with zero
`MEDIUM`/`LOW` touched; existing tests (`headers`, `retry`, `hooks`:
282 passed) stayed green. Three feature tasks
(`throwHttpErrors` array form, `response.redirected`, `URLSearchParams`
body), reps=1 per arm (pilot scope — see limitations), fresh session +
fresh clone per run, randomized order.

| Task | | BASE | DIET |
|---|---|---|---|
| throw-http-errors-array | Input tokens | 66K | 45K (−31%) |
| | Tool calls | 12 | 8 (−33%) |
| | Files read | 9 | 6 (−33%) |
| | Time | 585s | 420s |
| | Success | 1/1 | 1/1 |
| response-redirected | Input tokens | 74K | 155K (+111%) |
| | Tool calls | 7 | 12 (+71%) |
| | Files read | 4 | 3 (−25%) |
| | Time | 580s | 229s |
| | Success | 1/1 | 1/1 |
| urlsearchparams-body | Input tokens | 34K | 71K (+106%) |
| | Tool calls | 6 | 5 (−17%) |
| | Files read | 5 | 2 (−60%) |
| | Time | 421s | 80s |
| | Success | 1/1 | 1/1 |

Reading: files-read was lower on DIET in all 3 tasks; tokens/tool calls
went both ways (run-to-run variance at reps=1 dominates — same task/branch
varied 2–4× across runs earlier); success unchanged 6/6. With reps=1 this
is a **pilot, not a verdict**: it validates the harness end-to-end (real
implementations, real passes) but cannot confirm or reject the thesis.
Full report: `benchmarks/got/benchmark-2026-09-18-mimo-r1.json`.

During development the analyzer was also validated against 15 real
repositories (dayjs, zod, commander, cheerio, marked, yargs, hono, execa,
got, axios, mocha, eslint, shelljs, ofetch, uptime-kuma, excalidraw,
TriliumNext): precision was driven up until zero-removable on clean repos
(dayjs, zod, shelljs, …) with no false `CERTAIN`s found by audit, rather
than chasing recall.

---

## Example findings

```text
[CERTAIN] dead_file benchmark/server.ts
  ✓ 0 production importers ✓ not a package entrypoint
  ✓ not configured as an entrypoint ✓ no supported dynamic reference

[CERTAIN] dead_variable src/v4/core/compile.ts:707-checkFn
  ✓ no references ✓ not exported ✓ not reachable ✓ no dynamic usage
```

```bash
diet-code explain src/v4/core/compile.ts:707-checkFn
# Classification: DEAD FUNCTION / Confidence: CERTAIN + evidence + git history
```

---

## Architecture

```text
crates/diet-code-core/src/
  lib.rs          orchestration: discover → parse → resolve → graph →
                  entrypoints → reachability → findings → git evidence
  parser.rs       tree-sitter extraction (TS/TSX/JS/JSX), imports, refs,
                  dynamic-use signals, escape/publication signals
  symbols.rs      Entity model (stable IDs, byte offsets)
  imports.rs      import / re-export / reference records (VALUE vs TYPE)
  resolver.rs     relative, tsconfig paths, barrels, CJS, self-imports
  graph.rs        file + symbol graphs, lexical-scope + receiver resolution
  entrypoints.rs  diet-code.json, package.json(s), conventions, test sets,
                  tooling/script/workflow/config guards
  reachability.rs BFS from production ∪ public ∪ script roots (+ test roots)
  findings.rs     finding kinds + stable JSON schema (version 1)
  confidence.rs   CERTAIN/HIGH/MEDIUM/LOW (+ auto-removable rule)
  edits.rs        deterministic byte-range removals + import pruning
crates/diet-code-cli/src/
  main.rs  commands/{analyze,explain,clean,benchmark}.rs  output.rs
fixtures/     20 fixture repos × expectations (cargo test)
benchmarks/got/  real-repo task pack + measured report
```

Core never touches the terminal, Git, or the network; the CLI owns I/O.
Agent telemetry is isolated behind `trait AgentRunParser` (opencode +
claude impls; Codex/Gemini/Cursor later).

## Contribution guide

1. **Precision first.** A new finding kind must come with fixtures proving
   both sides (detected when dead, silent when live), especially the
   framework/dispatch/dynamic patterns in `fixtures/`.
2. **Never auto-remove on suspicion.** If external, dynamic, or
   framework-driven use is possible, cap at `MEDIUM`/`LOW` with a reason.
3. **Deterministic only in core.** No network, no models, no clocks in
   analysis results. `cargo test --workspace` must pass offline (the
   `fossil-mcp` differential test is `#[ignore]`d and optional).
4. **Benchmark honesty.** Same prompt/commit/model/session-freshness,
   randomized order, verify-decides-success, report ranges, document
   limitations. A negative result is a result.

```bash
cargo test --workspace          # all suites, offline
cargo install --path crates/diet-code-cli
diet-code analyze .
```
