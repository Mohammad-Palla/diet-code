# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.0.0/) and the project adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.1] - 2026-09-20

### Fixed

- **Entry-point detection for web frontends without a `package.json`.**
  Repositories whose frontend is a nested ES-module tree (for example
  `web/src/main.js` bundled into `web/app.js` and loaded via
  `<script src="/static/app.js">`) had no production entry point discovered.
  That cascaded into false `HIGH` `dead_file` findings on transitively-imported
  modules ("0 production importers"), which `diet-code clean` would have
  `git rm`'d — deleting live source. Two deterministic, conservative
  entry-discovery signals now prevent this:
  - Nested source-root convention: `<dir>/src/{main,index}.{js,ts,jsx,tsx,mjs}`
    at any depth is treated as a weak production root.
  - HTML `<script src>` discovery: HTML files are scanned for local script
    references, resolving relative paths, server-absolute paths (with
    `/static/`, `/assets/`, `/public/`, `/dist/`, `/build/`, `/js/`,
    `/scripts/` mount-prefix stripping) and a unique-basename fallback;
    external URLs are ignored.

### Added

- Fixture `fixtures/html-entry-web/` proving both sides of the fix: a
  transitively-imported module stays live while a genuine orphan is still
  reported as `CERTAIN` dead.

## [0.1.0] - 2026-09-18

### Added

- Initial release: local-first static dead-code analyzer, deterministic
  cleanup (`clean`), and agent benchmark (`benchmark`) for
  TypeScript/JavaScript. Self-contained Rust binary distributed via npm
  (`@mohammadpalla/diet-code`), a shell installer, and crates.io.

[0.1.1]: https://github.com/Mohammad-Palla/diet-code/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/Mohammad-Palla/diet-code/releases/tag/v0.1.0
