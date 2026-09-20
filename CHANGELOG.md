# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.0.0/) and the project adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0] - 2026-09-20

### Added

- **Python support.** Python is now analyzed alongside TypeScript/JavaScript.
  A dedicated extractor emits the same internal shape as the TS/JS visitor, so
  the graph, reachability, findings and cleanup passes are unchanged, while
  Python's own module and visibility rules are modelled explicitly:
  - Dotted specifiers resolve through packages (`pkg/mod.py` vs
    `pkg/mod/__init__.py`), relative `.`/`..` levels, and flat, `src/` and
    monorepo roots. Importing `pkg.mod` also records the implicit
    `pkg/__init__.py` edge the interpreter really executes.
  - Visibility follows Python, not ES modules: a module-level name is importable
    unless underscore-prefixed, so public names cap at `MEDIUM` while
    `_private` ones can reach `CERTAIN`. `__all__` is read additively, module
    dunders (`__version__`, `__author__`) are public metadata, and a name
    another module imports is used regardless of the underscore convention.
  - Because Python has no `exports` map, every public module of a declared
    distribution is deep-importable and is never auto-removed; underscore-
    prefixed modules stay analyzable.
  - Dunder methods are interpreter protocols and never dead-code candidates. A
    decorated definition is treated as used because it is handed to a callable
    that cannot be followed — except for language/stdlib transformers
    (`staticmethod`, `property`, `dataclass`, ...), so an unused
    `@staticmethod` is still reported. No framework is inferred.
  - Entry points: `pyproject.toml`/`setup.py` console scripts and declared
    packages, `__main__` guards, `__main__.py`, `manage.py`, `wsgi.py`/`asgi.py`.
    Test scope follows pytest/unittest conventions (`test_*.py`, `*_test.py`,
    `conftest.py`).
  - Dynamic loading is respected rather than guessed at: modules named in
    strings (settings, entry-point groups, lazy class paths, CLI module
    arguments) are protected, a string naming a package protects what it
    contains, and `importlib.import_module`/`__import__` protect their static
    prefix whether written as interpolation or concatenation.
- Fixtures `fixtures/python-{basic,framework,nested,plugins,sphinx-ext}/`
  proving both sides of each rule: detected when dead, silent when live.

### Fixed

- **`clean --apply` could delete a live import.** `prune_unused_imports` treated
  any line starting with `import ` as an ES binding import, so
  `import numpy as np` was parsed with the `import ... from` grammar, its
  binding read as `numpy as np`, and the line deleted while `np` was still used.
  The same flaw applied to TypeScript's `import fs = require("fs")`. A line with
  no `from` clause is no longer assumed to be a readable binding import.

### Safety

- Python cleanup is deliberately narrower than TS/JS cleanup, because the
  language is whitespace-significant and duck-typed:
  - Only declarations starting at column 0 are auto-removable; removing the
    only statement of a `class`, `def` or `except` block would leave a syntax
    error.
  - Python imports are never pruned: `import pkg` is an executable statement
    whose side effects can be the reason it is present.

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

[0.2.0]: https://github.com/Mohammad-Palla/diet-code/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/Mohammad-Palla/diet-code/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/Mohammad-Palla/diet-code/releases/tag/v0.1.0
