# Distributing Diet Code

The analyzer stays in Rust; users are not required to install Rust. The core
is distributed as a prebuilt static binary through several channels, all
driven by one GitHub Release.

## Channels

| Channel | Command | Notes |
|---|---|---|
| npm (primary) | `npm install -g @mohammadpalla/diet-code` | Node 18+, no native build; `postinstall` downloads + checksum-verifies the platform binary |
| npx | `npx @mohammadpalla/diet-code analyze .` | zero-install run |
| shell installer | `curl -fsSL …/scripts/install.sh \| sh` | Linux/macOS → `~/.local/bin`, checksum-verified |
| from a checkout | `cargo install --path crates/diet-code-cli` | Rust stable; not published to crates.io |
| GitHub Release assets | direct download | used by the above |

## How the npm wrapper works

```
npm/diet-code/
  package.json     bin → bin/diet-code.js; postinstall → install.js
  platforms.json   os/arch → release asset name
  install.js       download asset + checksums.txt, verify sha256, chmod +x
  bin/diet-code.js 50-line launcher: spawnSync the vendored binary
```

- No dependencies; Node ≥ 18 (`fetch` is built in).
- The binary is staged into `bin/vendor/` at install time (gitignored).
- **Fails closed**: if `checksums.txt` is missing or mismatched, install
  aborts rather than running unverified code.
- Unsupported platform → clear error pointing at a source build from a checkout.

Local test without publishing:

```bash
cargo build --release -p diet-code-cli
DIET_CODE_BINARY_PATH="$PWD/target/release/diet-code" node npm/diet-code/smoke-test.cjs
DIET_CODE_SKIP_DOWNLOAD=1 npm pack ./npm/diet-code          # inspect tarball
```

## Release process

Everything is automated in `.github/workflows/release.yml`.

1. Bump versions in lockstep:
   - `crates/diet-code-core/Cargo.toml`
   - `crates/diet-code-cli/Cargo.toml`
   - `npm/diet-code/package.json` (the workflow overwrites this from the tag,
     but keep it in sync for local tests)
2. Commit `chore: release vX.Y.Z`.
3. Tag and push:
   ```bash
   git tag -a v0.1.0 -m "Diet Code v0.1.0"
   git push origin v0.1.0
   ```
4. The workflow:
   - builds 5 targets (linux x64/arm64 musl, macOS x64/arm64, windows x64),
   - smoke-tests binaries where runnable,
   - generates `checksums.txt`,
   - creates the GitHub Release with all assets,
   - publishes `diet-code` to npm (needs the `NPM_TOKEN` repo secret).

The crates are **not** published to crates.io. `diet-code-core` is a path
dependency without a version, so `cargo package` cannot build `diet-code-cli`
for a registry; publishing would also require moving
`skills/diet-code/SKILL.md` inside the CLI crate, since `install` embeds it with
`include_str!`. npm, the shell installer and the Release assets are the
distribution channels.

Required repo secret: `NPM_TOKEN` (npm automation token). Never commit tokens;
GitHub Actions injects them.

## Adding a platform

1. Add the target to the `matrix.include` list in `release.yml`.
2. Add the `os-arch` → asset name to `npm/diet-code/platforms.json`.
3. Add the same mapping to `scripts/install.sh`.

## Versioning

Keep the npm package version, the crates' versions, and the Git tag equal
(`v0.1.0` ↔ `0.1.0`). The npm workflow enforces this by stamping the tag's
version into `package.json` before publishing.

## Why not ship Rust-to-WASM or a Node addon?

- WASM would require bundling a JS runtime and loses native filesystem speed
  (traversal is CPU-heavy).
- node-gyp/native addons reintroduce per-platform compilation and toolchain
  requirements — exactly what we are avoiding.
- Prebuilt binaries + a thin JS shim give npm ergonomics with native speed
  and zero user-side build.
