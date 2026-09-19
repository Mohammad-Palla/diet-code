# diet-code (npm)

> Put your AI coding agent on a diet.

Deterministic dead-code analyzer, cleanup tool, and agent benchmark for
TypeScript/JavaScript. This npm package ships a **self-contained Rust
binary** — no Rust toolchain, no native build step, no runtime dependencies.

## Install

```bash
npm install -g diet-code
```

or run without installing:

```bash
npx diet-code analyze .
```

The `postinstall` script downloads the prebuilt binary for your platform
(linux/macOS/Windows × x64/arm64) from GitHub Releases and verifies its
SHA-256 checksum. No binary leaves your machine; no telemetry.

## Usage

```bash
diet-code analyze .              # dead-code evidence + summary
diet-code analyze . --json       # stable JSON report
diet-code explain <finding>      # evidence for one finding
diet-code clean --dry-run        # preview the deterministic patch plan
diet-code clean                  # apply it on a diet-code/<timestamp> branch
diet-code benchmark --tasks tasks.json --reps 3
```

Full docs, methodology, and benchmark results:
<https://github.com/Mohammad-Palla/diet-code#readme>

Prefer building from source? `cargo install diet-code-cli` (crates.io) or
`cargo install --path crates/diet-code-cli` (this repo).

## Supported platforms

| OS | arch | artifact |
|---|---|---|
| Linux | x64, arm64 | static musl binary |
| macOS | x64 (Intel), arm64 (Apple Silicon) | |
| Windows | x64 | `.exe` |

Anything else prints a clear error with the from-source fallback. You can
also bypass the download (`DIET_CODE_SKIP_DOWNLOAD=1`), point at a mirror
(`DIET_CODE_DOWNLOAD_BASE_URL=…`), or override the binary entirely
(`DIET_CODE_BINARY_PATH=/path/to/diet-code`).

## License

MIT — see LICENSE.
