---
name: diet-code
description: "Use whenever someone asks whether code is still needed, used, or safe to delete — 'is this function needed', 'is this file dead', 'can I remove X', 'does anything call Y', 'find dead code', 'what can we delete'. Answers with deterministic reachability evidence plus Git history that explains why the code exists, and never deletes anything without explicit approval."
---

# /diet-code

Answer "is this still needed?" with proof instead of a guess. diet-code builds an
import and symbol graph of the repository, walks it from real entry points, and
reports what nothing reaches — then Git history explains *why* the code is still
there. Works on TypeScript/JavaScript and Python.

## Usage

```
/diet-code is <symbol> needed              # verdict + evidence + Git history for one symbol
/diet-code is <path/to/file> dead          # same, for a file
/diet-code can I delete <symbol>           # same question, answered the same way
/diet-code what calls <symbol>             # reachability evidence for one symbol
/diet-code                                 # scan the repository, summarise candidates
/diet-code scan                            # same as bare invocation
/diet-code scan <path>                     # scan a specific directory
/diet-code explain <symbol|file>           # raw evidence for one symbol or file
/diet-code clean                           # show what deterministic cleanup WOULD remove
/diet-code clean --apply                   # apply it (branch + verify + revert on failure)
/diet-code --help                          # print this Usage block
```

## What diet-code is for

Codebases accumulate code nobody uses and nobody dares delete: `payments_v2/`,
`oldAuth.ts`, `calculateLegacyTax()`. The blocker is never finding suspects — it
is *proving* a suspect is safe to remove. diet-code exists to produce that proof,
and to say plainly when it cannot.

It is not a linter, not a formatter, and there is no model inside the analyzer.
Every number it prints is a fact about the graph or about Git.

## What You Must Do When Invoked

If the user invoked `/diet-code --help` or `/diet-code -h` with no other
arguments, print the `## Usage` block above verbatim and stop.

### Step 1 — Make sure the CLI is available

```bash
diet-code --version 2>/dev/null || npx -y @mohammadpalla/diet-code --version
```

If `diet-code` is not on PATH, use `npx -y @mohammadpalla/diet-code` for every
command below. Do not install anything globally without asking.

### Step 2 — Decide which question was asked

Read the user's words and pick exactly one mode:

| The user is asking | Mode |
|---|---|
| about one named symbol or file ("is `X` needed", "can I delete `X`", "what calls `X`") | **Symbol** → Step 3 |
| about the repository as a whole ("find dead code", bare `/diet-code`, `scan`) | **Scan** → Step 4 |
| to actually remove code ("clean it up", "delete the dead code") | **Cleanup** → Step 5 |

If the user named no symbol but their editor selection contains one, use the
selection. If the request is ambiguous between Symbol and Scan, prefer Symbol on
the thing they named; do not scan the whole repository to answer a question about
one function.

### Step 3 — Symbol mode

Run the analyzer first, then Git. Both halves are required: the analyzer says
whether anything reaches the code, Git says why it is still there.

```bash
diet-code explain --path <repo-root> "<symbol-or-file>"
```

`explain` runs a fresh analysis, so no prior scan is needed. It prints one of:

- a **finding** with a confidence level (`CERTAIN`, `HIGH`, `MEDIUM`, `LOW`) —
  nothing reaches the code;
- **`KEPT (no dead-code finding)`** — something does reach it, with the reason;
- **not indexed** — the name is wrong, or the file is not a supported language.
  Re-read the name and try the exact spelling before concluding anything.

Then gather the history. Use the file and line range that `explain` printed:

```bash
# Why the file exists and what last touched it.
git log --follow --format='%ad  %an  %s' --date=short -- <file> | head -10

# History of just this symbol's lines (often names the migration that stranded
# it). `-s` suppresses the diffs, leaving one line per commit.
git log -L <start>,<end>:<file> -s --format='%h  %ad  %an  %s' --date=short 2>/dev/null | head -10

# When mentions of the name appeared or disappeared anywhere in the repo. This
# is the one that usually explains a dead symbol: it finds the commit that
# deleted the last caller. Use a symbol name here, not a path — a path yields
# nothing, which is not evidence of anything. For a dead *file*, search for how
# it was imported instead (its basename or import specifier).
git log -S '<symbol>' --format='%h  %ad  %an  %s' --date=short | head -10
```

If `git rev-parse --is-shallow-repository` prints `true`, history is truncated:
"first seen" and commit counts are floors, not facts. Say so rather than dating
the code from a shallow clone.

Read the commit subjects. A subject like "migrate auth to OAuth" next to a symbol
with zero references is the actual explanation. A subject like "temporary
fallback, keep until v2 rollout" is a reason **not** to delete, even at
`CERTAIN` — say so.

Then answer in this shape, and keep it short:

```
Verdict: NOT NEEDED (CERTAIN) | LIKELY UNUSED (MEDIUM/LOW) | NEEDED | UNRESOLVED

Evidence
- <the analyzer's reasons, as printed>
- <production/test reachability, importer count>

History
- created <date>, last touched <date> by <author>
- <the commit subject that explains it, quoted>

Recommendation
- <what to do, and the exact command if removal is warranted>
```

Map confidence to what you tell them, and do not upgrade it:

| Confidence | What it means | What you say |
|---|---|---|
| `CERTAIN` | not exported, zero references, unreachable, no dynamic risk | safe to remove; `diet-code clean` covers it |
| `HIGH` | no production consumers; test-only references allowed | safe to remove, verify tests |
| `MEDIUM` | external or dynamic use cannot be ruled out | needs human review; never auto-removed |
| `LOW` | dynamic loading, tooling config, public surface, script | do not remove on this evidence |
| `KEPT` | something reaches it | it is needed; say what reaches it |

### Step 4 — Scan mode

```bash
diet-code analyze <path>              # human summary
diet-code analyze <path> --json       # stable JSON when you need to filter
diet-code analyze <path> --verbose    # every finding, not just the top ones
```

Report the counts by confidence and the highest-confidence candidates. Then stop
and offer the next step — do not start explaining all of them, and do not begin
deleting. If there are many findings, list the `CERTAIN`/`HIGH` ones and say how
many `MEDIUM`/`LOW` remain.

### Step 5 — Cleanup mode

```bash
diet-code clean --path <repo-root>    # show what it would do
```

Show the plan and **stop for approval**. Only `CERTAIN` and `HIGH` findings are
ever in the plan; `MEDIUM` and `LOW` are excluded by design.

Only after the user explicitly approves:

```bash
diet-code clean --path <repo-root> --apply
```

`clean` refuses to run on a dirty tree, records `HEAD`, works on a new branch,
re-analyzes, runs whatever verify scripts it discovers (`test`, `build`,
`typecheck`, or `verifyCommands`), and reverts on failure unless `--keep`.

## Rules

1. **Never delete code in this skill.** Removal happens only through
   `diet-code clean --apply`, only after the user approves the printed plan. Do
   not hand-edit files to remove code you believe is dead.
2. **Never raise a confidence level.** If the analyzer says `MEDIUM`, the answer
   is "needs review", even when it looks obviously dead to you.
3. **Report Git history as supporting evidence, not proof.** "Last touched 19
   months ago" is not a reason to delete; zero references is. An old file that
   something still reaches is not dead.
4. **A commit message can veto a `CERTAIN`.** If history says the code is kept
   deliberately (migration fallback, compatibility shim, vendored copy), say so
   and recommend keeping it.
5. **Say when you do not know.** "Not indexed" and `LOW` are real answers.
   Do not fill the gap with a guess about whether something is used.
6. **Do not describe diet-code as AI-powered.** The analyzer is deterministic;
   your contribution is reading the history and explaining the result.

## Language notes

- **TypeScript/JavaScript**: resolves relative and extensionless imports,
  `tsconfig` paths, barrels and re-export chains, CommonJS `require`,
  `module.exports`, dynamic `import()`, and JSX.
- **Python**: resolves dotted modules through packages (`pkg/mod.py` vs
  `pkg/mod/__init__.py`) and relative `.`/`..` levels. Visibility follows the
  underscore convention, so a public module-level name caps at `MEDIUM` (any
  consumer may import it) while `_private` names can reach `CERTAIN`. Dunder
  methods, decorated definitions, `__init__.py` re-exports, and modules named in
  strings (settings, entry points, `importlib`) are all treated as reachable.
  Python declarations nested in a block are never auto-removed, because deleting
  the only statement of a `class` or `def` leaves a syntax error, and Python
  imports are never pruned because `import pkg` can matter for its side effects.

If the repository is neither TypeScript/JavaScript nor Python, say so rather than
reporting an empty result as "no dead code".
