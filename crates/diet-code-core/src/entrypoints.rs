use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

use crate::parser::to_rel_slash;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DietConfig {
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default, alias = "entryPoints")]
    pub entry_points: Vec<String>,
    #[serde(default, alias = "verifyCommands")]
    pub verify_commands: Vec<String>,
}

impl DietConfig {
    pub fn load(root: &Path) -> Self {
        let p = root.join("diet-code.json");
        let Ok(text) = std::fs::read_to_string(&p) else {
            return Self::default();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Default)]
pub struct EntrySets {
    pub production: HashSet<String>,
    pub tests: HashSet<String>,
    pub package_entries: HashSet<String>,
    /// All files reachable as package public surface (for conservative marking).
    pub package_export_files: HashSet<String>,
}

pub fn is_test_file(rel: &str) -> bool {
    // Conventional test directories: everything under them is test scope,
    // at any depth (`test/`, `src/v3/tests/`, ...).
    if rel.split('/').any(|comp| {
        matches!(
            comp,
            "test" | "tests" | "spec" | "specs" | "e2e" | "__tests__" | "__test__"
        )
    }) {
        return true;
    }
    let file = rel.rsplit('/').next().unwrap_or(rel);
    // strip extensions, check .test. / .spec. / .test-d. (tsd type tests)
    if file.contains(".test.") || file.contains(".spec.") || file.contains(".test-d.") {
        return true;
    }
    // Python: pytest/unittest collect `test_*.py` and `*_test.py`, and
    // `conftest.py` holds fixtures loaded by the runner.
    if let Some(stem) = file.strip_suffix(".py") {
        if stem == "conftest" || stem.starts_with("test_") || stem.ends_with("_test") {
            return true;
        }
    }
    false
}

/// Files referenced by CI workflow files (`require('./tools/x.js')` inside
/// `actions/github-script`, `run: node tools/x.js` steps, ...). Such files are
/// executed by automation, never imported: review only, never auto-remove.
pub fn workflow_referenced_files(root: &Path, files: &HashSet<String>) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut stack = vec![root.to_path_buf()];
    let mut yml_files = Vec::new();
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.filter_map(|e| e.ok()) {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                let rel = crate::parser::to_rel_slash(root, &path);
                if is_default_ignored(&rel) {
                    continue;
                }
                stack.push(path);
            } else if ft.is_file() {
                if let Some(name) = entry.file_name().to_str() {
                    if name.ends_with(".yml") || name.ends_with(".yaml") {
                        yml_files.push(path);
                    }
                }
            }
        }
    }
    for yml in yml_files {
        let Ok(text) = std::fs::read_to_string(&yml) else {
            continue;
        };
        let yml_dir = yml.parent().unwrap_or(root);
        for tok in workflow_file_tokens(&text) {
            // Try relative to repo root AND to the workflow file's directory
            // (github-script `require` is workspace-relative in practice).
            let mut cands = vec![tok.clone()];
            if let Ok(rel_dir) = yml_dir.strip_prefix(root) {
                cands.push(format!(
                    "{}/{}",
                    rel_dir.to_string_lossy().replace('\\', "/"),
                    tok
                ));
            }
            for cand in cands {
                let cand = cand.strip_prefix("./").unwrap_or(&cand);
                if files.contains(cand) {
                    out.insert(cand.to_string());
                }
            }
        }
    }
    out
}

fn workflow_file_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    // Quoted string literals ending in a source extension:
    // require('./tools/x.js'), import ... from "./y.ts", run: "node a.js".
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == '\'' || c == '"' || c == '`' {
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j] as char) != c {
                j += 1;
            }
            if j < bytes.len() {
                let lit = &text[i + 1..j];
                if has_source_ext(lit) && !lit.contains("${{") && !lit.contains('$') {
                    out.push(lit.trim_start_matches("./").to_string());
                }
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    // Bare shell tokens (`run: node tools/x.js`).
    for part in
        text.split(|c: char| c.is_whitespace() || matches!(c, '&' | '|' | ';' | '(' | ')' | ':'))
    {
        let t = part
            .trim()
            .trim_matches(|c| c == '"' || c == '\'' || c == '`')
            .trim();
        if has_source_ext(t) && !t.contains('*') && !t.starts_with('-') {
            out.push(t.trim_start_matches("./").to_string());
        }
    }
    out.sort();
    out.dedup();
    out
}
/// Agent-tooling directories (skills, hooks, plugins for coding agents).
/// Loaded by the agent runtime by convention, never by imports.
pub fn is_agent_tooling_dir(rel: &str) -> bool {
    matches!(
        rel.split('/').next().unwrap_or(""),
        ".claude" | ".cursor" | ".agents" | ".opencode" | ".codex" | ".windsurf" | ".aider"
    )
}

/// Files referenced by any package.json `scripts` in the repo
/// (e.g. `"bench": "tsx packages/bench/compile.ts"`, mocha file args).
/// Such files are executed by tooling, never imported: review only.
pub fn script_referenced_files(root: &Path, files: &HashSet<String>) -> HashSet<String> {
    let mut out = HashSet::new();
    for pkg_path in find_package_jsons(root) {
        let dir = pkg_path.parent().unwrap_or(root).to_path_buf();
        let Ok(text) = std::fs::read_to_string(&pkg_path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let mut raws: Vec<&str> = Vec::new();
        if let Some(scripts) = v.get("scripts").and_then(|s| s.as_object()) {
            raws.extend(scripts.values().filter_map(|x| x.as_str()));
        }
        for raw in raws {
            for tok in tokenize_script(raw) {
                if tok.contains('*') {
                    // Glob arg (`mocha ./test/*.mjs`): match repo-relative patterns.
                    let joined = join_and_normalize(&dir, &tok);
                    let rel_pat = crate::parser::to_rel_slash(root, &joined);
                    if let Ok(g) = globset::Glob::new(&rel_pat) {
                        let m = g.compile_matcher();
                        for f in files {
                            if m.is_match(f) {
                                out.insert(f.clone());
                            }
                        }
                    }
                } else if has_source_ext(&tok) {
                    let joined = join_and_normalize(&dir, &tok);
                    let rel = crate::parser::to_rel_slash(root, &joined);
                    if files.contains(&rel) {
                        out.insert(rel);
                    }
                }
            }
        }
    }
    out
}

fn find_package_jsons(root: &Path) -> Vec<std::path::PathBuf> {
    find_manifests(root, &["package.json"])
}

/// Walks the repository for manifest files with any of `names`, skipping
/// ignored directories and symlinks.
fn find_manifests(root: &Path, names: &[&str]) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.filter_map(|e| e.ok()) {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                let rel = crate::parser::to_rel_slash(root, &path);
                if is_default_ignored(&rel) {
                    continue;
                }
                stack.push(path);
            } else if ft.is_file() {
                if let Some(name) = entry.file_name().to_str() {
                    if names.contains(&name) {
                        out.push(path);
                    }
                }
            }
        }
    }
    out
}

fn tokenize_script(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in raw.split(|c: char| {
        c.is_whitespace() || c == '&' || c == '|' || c == ';' || c == '(' || c == ')'
    }) {
        let t = part.trim().trim_matches(|c| c == '"' || c == '\'').trim();
        if t.is_empty() || t.starts_with('-') || t.contains('=') && !t.contains('/') {
            continue;
        }
        // Strip `VAR=value` prefixes glued to commands? Handled above by `=` check.
        let t = t.strip_prefix("./").unwrap_or(t);
        out.push(t.to_string());
    }
    out
}

fn has_source_ext(tok: &str) -> bool {
    const EXTS: &[&str] = &[".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts"];
    EXTS.iter().any(|e| tok.ends_with(e))
}

fn join_and_normalize(dir: &Path, tok: &str) -> std::path::PathBuf {
    let p = dir.join(tok);
    // Lexical normalization (no IO).
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c.as_os_str()),
        }
    }
    out
}

pub fn discover_entrypoints(root: &Path, all_files: &[String], config: &DietConfig) -> EntrySets {
    let set: HashSet<String> = all_files.iter().cloned().collect();
    let mut out = EntrySets::default();

    // 1. Explicit config (highest priority) — production.
    for ep in &config.entry_points {
        let cand = normalize_entry(root, ep);
        if set.contains(&cand) {
            out.production.insert(cand);
        } else {
            // Try with extensions?
            for ext in ["ts", "tsx", "js", "jsx", "mjs", "cjs"] {
                let with_ext = format!("{}.{}", cand.trim_end_matches('/'), ext);
                if set.contains(&with_ext) {
                    out.production.insert(with_ext);
                    break;
                }
            }
        }
    }

    // 2. package.json fields.
    let (pkg_production, pkg_public) = package_entry_files(root, &set);
    for f in pkg_production {
        out.production.insert(f.clone());
        out.package_entries.insert(f);
    }
    for f in pkg_public {
        out.package_export_files.insert(f.clone());
        out.package_entries.insert(f);
    }

    // 3. Conventions (only if file exists), conservative.
    for conv in [
        "src/main.ts",
        "src/main.tsx",
        "src/main.js",
        "src/main.jsx",
        "src/index.ts",
        "src/index.tsx",
        "src/index.js",
        "src/index.jsx",
        "src/server.ts",
        "src/server.js",
        "src/cli.ts",
        "src/cli.js",
        "src/app.ts",
        "src/App.tsx",
        "index.ts",
        "index.js",
        "main.ts",
        "main.js",
    ] {
        if set.contains(conv) {
            // Only auto-add conventions when no explicit config and no package entries?
            // Spec: support conservative conventions. Add them but they are weak roots —
            // still mark as production so single-file fixtures work.
            out.production.insert(conv.to_string());
        }
    }

    // 3b. Nested source-root conventions. Projects without a package.json
    // (e.g. a Go/Rust app with an ES-module `web/` frontend built by a
    // bundler script) still root their module graph at a conventional
    // `<dir>/src/main.*` or `<dir>/src/index.*`. Treat such files as weak
    // production roots at any depth so their transitive imports are reachable.
    // Conservative: only the well-known bundler entry basenames, and only
    // inside a directory literally named `src`.
    for f in &set {
        if is_nested_source_entry(f) {
            out.production.insert(f.clone());
        }
    }

    // 3c. HTML `<script src>` entry points. An HTML file that loads a local
    // script is a deterministic web entry signal (served bundles, module
    // entries). Resolve the referenced script to a repo file and treat it as
    // a production root. This is a FACT (the browser loads it), never a guess.
    for f in html_script_entries(root, &set) {
        out.production.insert(f);
    }

    // 3d. Python entry points: installed console scripts, `python -m` targets
    // and the conventional app/server entries.
    let (py_production, py_public) = python_entries(root, &set);
    for f in py_production {
        out.production.insert(f);
    }
    for f in py_public {
        out.package_export_files.insert(f.clone());
        out.package_entries.insert(f);
    }

    // 4. Tests form separate root set.
    for f in all_files {
        if is_test_file(f) {
            out.tests.insert(f.clone());
        }
    }

    out
}

/// Whether `rel` is a conventional module-graph entry nested under a `src`
/// directory (e.g. `web/src/main.js`, `frontend/src/index.ts`). Conservative:
/// requires a parent segment literally named `src` and a well-known bundler
/// entry basename. Root-level `src/...` is already handled by the flat
/// convention list; this covers projects with no package.json whose frontend
/// lives in a subdirectory built by a bundler script.
fn is_nested_source_entry(rel: &str) -> bool {
    let parts: Vec<&str> = rel.split('/').collect();
    // Need at least `<dir>/src/<file>` (depth >= 3) so we don't double-handle
    // the flat `src/main.js` conventions or match a bare `main.js`.
    if parts.len() < 3 {
        return false;
    }
    let file = parts[parts.len() - 1];
    let parent = parts[parts.len() - 2];
    if parent != "src" {
        return false;
    }
    matches!(
        file,
        "main.ts"
            | "main.tsx"
            | "main.js"
            | "main.jsx"
            | "main.mjs"
            | "index.ts"
            | "index.tsx"
            | "index.js"
            | "index.jsx"
            | "index.mjs"
    )
}

/// Scan HTML files for `<script src="...">` and resolve each referenced local
/// script to a repo file. Returns the set of resolved production entries.
///
/// Resolution rules (all deterministic, no guessing beyond checkable facts):
/// - Ignore external scripts (`http:`, `https:`, `//cdn...`, `data:`).
/// - Relative `src` (`./app.js`, `src/main.js`) resolves against the HTML
///   file's own directory.
/// - Server-absolute `src` (`/static/app.js`) is a served path: try resolving
///   against the HTML dir after stripping a leading well-known static mount
///   segment (`static`, `assets`, `public`, `dist`, `build`, `js`), and fall
///   back to a unique basename match anywhere in the repo.
fn html_script_entries(root: &Path, files: &HashSet<String>) -> HashSet<String> {
    let mut out = HashSet::new();
    // HTML files are not part of the parsed source set (only JS/TS are), so
    // walk the tree to find them, skipping ignored directories and symlinks.
    let mut html_files: Vec<String> = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.filter_map(|e| e.ok()) {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_symlink() {
                continue;
            }
            let rel = crate::parser::to_rel_slash(root, &path);
            if ft.is_dir() {
                if !is_default_ignored(&rel) {
                    stack.push(path);
                }
                continue;
            }
            let lower = rel.to_ascii_lowercase();
            if (lower.ends_with(".html") || lower.ends_with(".htm")) && !is_default_ignored(&rel) {
                html_files.push(rel);
            }
        }
    }
    for html in html_files {
        let Ok(text) = std::fs::read_to_string(root.join(&html)) else {
            continue;
        };
        let html_dir = html.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        for src in extract_script_srcs(&text) {
            if let Some(resolved) = resolve_script_src(files, html_dir, &src) {
                out.insert(resolved);
            }
        }
    }
    out
}

/// Extract the `src` attribute of every `<script ... src="...">` tag.
/// Tolerant plain-text scan (no full HTML parser needed): handles single or
/// double quotes and arbitrary attribute order.
fn extract_script_srcs(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let lower = html.to_ascii_lowercase();
    let mut idx = 0;
    while let Some(rel) = lower[idx..].find("<script") {
        let tag_start = idx + rel;
        // Find end of this opening tag.
        let tag_end = lower[tag_start..]
            .find('>')
            .map(|e| tag_start + e)
            .unwrap_or(lower.len());
        let tag = &html[tag_start..tag_end];
        if let Some(src) = find_attr(tag, "src") {
            out.push(src);
        }
        idx = tag_end + 1;
        if idx >= lower.len() {
            break;
        }
    }
    out
}

/// Find `attr="value"` or `attr='value'` inside a tag substring.
fn find_attr(tag: &str, attr: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut search = 0;
    while let Some(pos) = lower[search..].find(attr) {
        let at = search + pos;
        // Ensure it's a standalone attribute (preceded by whitespace).
        let ok_prefix = at == 0 || tag.as_bytes()[at - 1].is_ascii_whitespace();
        let after = at + attr.len();
        let rest = &tag[after..];
        let rest_trimmed = rest.trim_start();
        if ok_prefix && rest_trimmed.starts_with('=') {
            let val = rest_trimmed[1..].trim_start();
            let quote = val.chars().next()?;
            if quote == '"' || quote == '\'' {
                let end = val[1..].find(quote)? + 1;
                return Some(val[1..end].to_string());
            }
        }
        search = after;
    }
    None
}

fn resolve_script_src(files: &HashSet<String>, html_dir: &str, src: &str) -> Option<String> {
    let src = src.trim();
    if src.is_empty() {
        return None;
    }
    // External / non-file sources.
    let lower = src.to_ascii_lowercase();
    if lower.starts_with("http:")
        || lower.starts_with("https:")
        || lower.starts_with("//")
        || lower.starts_with("data:")
        || lower.starts_with("blob:")
    {
        return None;
    }
    // Strip query/hash.
    let clean = src.split(['?', '#']).next().unwrap_or(src);
    // Only resolve script-like assets.
    let is_scriptish = ["js", "mjs", "cjs", "ts", "jsx", "tsx"]
        .iter()
        .any(|e| clean.to_ascii_lowercase().ends_with(&format!(".{e}")));
    if !is_scriptish {
        return None;
    }

    if let Some(server_abs) = clean.strip_prefix('/') {
        // Served path (`/static/app.js`). Try stripping a well-known mount
        // segment, resolving against the HTML's directory.
        let mount_segments = [
            "static", "assets", "public", "dist", "build", "js", "scripts",
        ];
        let mut candidates = Vec::new();
        if let Some((first, rest)) = server_abs.split_once('/') {
            if mount_segments.contains(&first) {
                candidates.push(join_rel(html_dir, rest));
                candidates.push(rest.to_string());
            }
        }
        candidates.push(join_rel(html_dir, server_abs));
        candidates.push(server_abs.to_string());
        for c in candidates {
            let norm = normalize_rel(&c);
            if files.contains(&norm) {
                return Some(norm);
            }
        }
        // Fall back to a UNIQUE basename match (avoid ambiguity).
        let base = server_abs.rsplit('/').next().unwrap_or(server_abs);
        let matches: Vec<&String> = files
            .iter()
            .filter(|f| f.rsplit('/').next() == Some(base))
            .collect();
        if matches.len() == 1 {
            return Some(matches[0].clone());
        }
        return None;
    }

    // Relative to the HTML directory.
    let joined = join_rel(html_dir, clean);
    let norm = normalize_rel(&joined);
    if files.contains(&norm) {
        return Some(norm);
    }
    None
}

fn join_rel(dir: &str, rel: &str) -> String {
    if dir.is_empty() {
        rel.to_string()
    } else {
        format!("{dir}/{rel}")
    }
}

/// Normalize a slash path: resolve `.` and `..` segments, drop leading `./`.
fn normalize_rel(path: &str) -> String {
    let mut stack: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            s => stack.push(s),
        }
    }
    stack.join("/")
}

fn normalize_entry(root: &Path, ep: &str) -> String {
    let p = Path::new(ep);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    };
    to_rel_slash(root, &abs)
}

/// Python entry points, from packaging metadata and interpreter conventions.
/// Returns `(production, public_surface)`.
///
/// `pyproject.toml` and `setup.py` declare console scripts as
/// `"command = pkg.module:func"`, and `[project] name` names the distributed
/// package. Instead of parsing TOML (and `setup.py`'s arbitrary Python), every
/// quoted string in the manifest is taken as a *candidate* dotted module path:
/// a candidate only becomes an entry when it resolves to a real `.py` file in
/// the repository, so unrelated strings (`">=3.9"`, `"README.md"`, dependency
/// names) filter themselves out.
fn python_entries(root: &Path, files: &HashSet<String>) -> (HashSet<String>, HashSet<String>) {
    let mut production: HashSet<String> = HashSet::new();
    let mut public: HashSet<String> = HashSet::new();

    for manifest in find_manifests(root, &["pyproject.toml", "setup.py"]) {
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        let dir = manifest.parent().unwrap_or(root);
        let base = crate::parser::to_rel_slash(root, dir);
        for module in manifest_module_candidates(&text) {
            let Some(file) = resolve_python_module(files, &base, &module) else {
                continue;
            };
            // A package root (`pkg/__init__.py`) is the distribution's public
            // API; a plain module (`pkg/cli.py`) is a program entry.
            if file.ends_with("__init__.py") {
                public.insert(file.clone());
                // Everything a consumer can deep-import from the distribution.
                if let Some(dir) = file.strip_suffix("/__init__.py") {
                    for m in distribution_public_modules(dir, files) {
                        public.insert(m);
                    }
                }
            }
            production.insert(file);
        }
    }

    for f in files {
        if !f.ends_with(".py") {
            continue;
        }
        let name = f.rsplit('/').next().unwrap_or(f);
        // `python -m pkg` executes `pkg/__main__.py`; Django generates
        // `manage.py`; a WSGI/ASGI server imports the callable in
        // `wsgi.py`/`asgi.py`.
        if matches!(name, "__main__.py" | "manage.py" | "wsgi.py" | "asgi.py") {
            production.insert(f.clone());
        }
        // `import pkg` executes `pkg/__init__.py`. Consumers outside the
        // repository reach it by package name, and no import inside the
        // repository records that, so a package initialiser is public surface:
        // reportable, never auto-removable.
        if name == "__init__.py" {
            public.insert(f.clone());
        }
    }

    (production, public)
}

/// Modules and packages named by a string literal anywhere in the repository's
/// Python sources or packaging metadata.
///
/// Python resolves module paths out of strings constantly, and none of it shows
/// up as an import edge: lazy class paths (`amqp_cls = 'celery.app.amqp:AMQP'`),
/// component settings (`"scrapy.extensions.logcount.LogCount"`), entry-point
/// groups (`group = "scrapy.commands"`), CLI arguments
/// (`["-A", "t.unit.bin.proj.app"]`), Django's `ROOT_URLCONF`. A module named
/// this way is loaded by name, so zero import edges says nothing about whether
/// it runs — it can never be proven dead by imports alone.
///
/// Returns `(module files, package directory prefixes)`: a string naming a
/// package protects the modules under it, which is how plugin directories are
/// loaded (`walk_modules(COMMANDS_MODULE)`).
///
/// At least two dotted segments are required, so a bare word in prose or a
/// dependency name cannot protect anything. The longest match wins, so
/// `"pkg.mod.Class"` protects `pkg/mod.py` rather than all of `pkg/`.
pub fn python_string_referenced_modules(
    root: &Path,
    sources: &[(String, String)],
    files: &HashSet<String>,
) -> (HashSet<String>, Vec<String>) {
    let mut referenced: HashSet<String> = HashSet::new();
    let mut prefixes: Vec<String> = Vec::new();

    let mut texts: Vec<String> = sources.iter().map(|(_, src)| src.clone()).collect();
    // Packaging metadata names plugin modules the same way (`entry_points`).
    for manifest in find_manifests(root, &["pyproject.toml", "setup.cfg", "tox.ini"]) {
        if let Ok(text) = std::fs::read_to_string(&manifest) {
            texts.push(text);
        }
    }

    for text in &texts {
        for raw in quoted_strings(text) {
            // `module:attr` and `module.Attr` both name a module on the left.
            let token = raw.split(':').next().unwrap_or("").trim();
            if !is_dotted_module(token) {
                continue;
            }
            let segments: Vec<&str> = token.split('.').collect();
            if segments.len() < 2 {
                continue;
            }
            for keep in (2..=segments.len()).rev() {
                let module = segments[..keep].join(".");
                let Some(file) = resolve_python_module(files, "", &module) else {
                    continue;
                };
                // A package name protects what it contains: that is exactly how
                // plugin and command directories get loaded.
                if let Some(dir) = file.strip_suffix("/__init__.py") {
                    if !prefixes.iter().any(|p| p == dir) {
                        prefixes.push(dir.to_string());
                    }
                }
                referenced.insert(file);
                break;
            }
        }
    }

    (referenced, prefixes)
}

/// Modules named by a *string* inside a Python tooling config. Sphinx resolves
/// `pygments_style = "flask_theme_support.FlaskyStyle"` by importing that module
/// at build time, usually after adding its directory to `sys.path`; nox and
/// invoke reference task modules the same way. Such a file is loaded by the
/// tool and never imported by the package, so it is reported for review and
/// never auto-removed.
///
/// Matching is restricted to the config's own directory subtree (when it has
/// one), since that is where a tool-local `sys.path` addition can point.
pub fn python_config_referenced_files(root: &Path, files: &HashSet<String>) -> HashSet<String> {
    let mut out = HashSet::new();
    for cfg in files
        .iter()
        .filter(|f| f.ends_with(".py") && is_tooling_config(f))
    {
        let Ok(text) = std::fs::read_to_string(root.join(cfg)) else {
            continue;
        };
        let dir = match cfg.rsplit_once('/') {
            Some((d, _)) => d.to_string(),
            None => String::new(),
        };
        for raw in quoted_strings(&text) {
            let token = raw.trim();
            if !is_dotted_module(token) {
                continue;
            }
            // `a.b.C` may name module `a.b` holding `C`, or module `a.b.C`.
            let segments: Vec<&str> = token.split('.').collect();
            for keep in (1..=segments.len()).rev() {
                let rel_path = format!("{}.py", segments[..keep].join("/"));
                let suffix = format!("/{}", rel_path);
                for f in files {
                    if !f.ends_with(".py") {
                        continue;
                    }
                    let in_scope = dir.is_empty() || f.starts_with(&format!("{}/", dir));
                    if in_scope && (*f == rel_path || f.ends_with(&suffix)) {
                        out.insert(f.clone());
                    }
                }
            }
        }
    }
    out
}

/// Modules a consumer can deep-import from a declared distribution.
///
/// Python has no `exports` map: once `pkg` is installed, `import pkg.any.module`
/// works for every module in it, so a public module cannot be proven dead from
/// repository imports alone — deprecation shims that only re-export from a
/// renamed private module look exactly like dead files. The underscore
/// convention is the one visibility signal Python offers, so `_private.py` (and
/// anything inside a `_private/` subpackage) stays analyzable while public
/// modules are treated as surface.
fn distribution_public_modules(pkg_dir: &str, files: &HashSet<String>) -> Vec<String> {
    if pkg_dir.is_empty() {
        // A package rooted at the repository root would swallow tests and
        // tooling; there is no distribution subtree to delimit.
        return Vec::new();
    }
    let prefix = format!("{}/", pkg_dir);
    let mut out = Vec::new();
    for f in files {
        if !(f.ends_with(".py") || f.ends_with(".pyi")) {
            continue;
        }
        let Some(rest) = f.strip_prefix(prefix.as_str()) else {
            continue;
        };
        if rest.is_empty() {
            continue;
        }
        let segments: Vec<&str> = rest.split('/').collect();
        let reachable = segments.iter().enumerate().all(|(i, seg)| {
            if i + 1 == segments.len() {
                let stem = seg
                    .strip_suffix(".py")
                    .or_else(|| seg.strip_suffix(".pyi"))
                    .unwrap_or(seg);
                stem == "__init__" || !stem.starts_with('_')
            } else {
                !seg.starts_with('_')
            }
        });
        if reachable {
            out.push(f.clone());
        }
    }
    out
}

/// Quoted strings in a Python manifest that could name a module, with any
/// `name = ` prefix and `:callable` suffix stripped.
fn manifest_module_candidates(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in quoted_strings(text) {
        let after_eq = raw.rsplit('=').next().unwrap_or(&raw);
        let module = after_eq.split(':').next().unwrap_or("").trim();
        if is_dotted_module(module) && !out.iter().any(|m| m == module) {
            out.push(module.to_string());
        }
    }
    out
}

/// String literals in a Python or TOML config, scanned one line at a time.
///
/// Per-line scanning matters: prose apostrophes (`don't`, `aren't`) appear in
/// comments in almost every real config, and a whole-file scan pairs them with
/// the next unrelated quote and silently loses every literal after that point.
/// A module path or entry point never spans lines, so resetting at each newline
/// costs nothing and cannot desynchronise. `#` outside a literal begins a
/// comment in both languages.
fn quoted_strings(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if c == '#' {
                break;
            }
            if c == '"' || c == '\'' {
                let mut j = i + 1;
                let mut buf = String::new();
                let mut closed = false;
                while j < chars.len() {
                    if chars[j] == '\\' && j + 1 < chars.len() {
                        buf.push(chars[j + 1]);
                        j += 2;
                        continue;
                    }
                    if chars[j] == c {
                        closed = true;
                        break;
                    }
                    buf.push(chars[j]);
                    j += 1;
                }
                if closed {
                    out.push(buf);
                    i = j + 1;
                    continue;
                }
                // Unterminated on this line: prose, not a literal.
                break;
            }
            i += 1;
        }
    }
    out
}

fn is_dotted_module(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    s.split('.').all(|seg| {
        !seg.is_empty()
            && seg
                .chars()
                .next()
                .is_some_and(|c| c.is_alphabetic() || c == '_')
            && seg.chars().all(|c| c.is_alphanumeric() || c == '_')
    })
}

/// Resolves a dotted module path against a manifest directory, honouring both
/// flat and `src/` layouts.
fn resolve_python_module(files: &HashSet<String>, base: &str, module: &str) -> Option<String> {
    let rel = module.replace('.', "/");
    for src_root in ["", "src"] {
        let mut prefix = String::new();
        for part in [base, src_root] {
            if part.is_empty() {
                continue;
            }
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
        }
        let path = if prefix.is_empty() {
            rel.clone()
        } else {
            format!("{}/{}", prefix, rel)
        };
        for cand in [format!("{}.py", path), format!("{}/__init__.py", path)] {
            if files.contains(&cand) {
                return Some(cand);
            }
        }
    }
    None
}

fn package_entry_files(root: &Path, files: &HashSet<String>) -> (Vec<String>, Vec<String>) {
    // Scan the root package.json AND every subpackage (monorepos): each
    // package's main/exports/bin/types resolve relative to its own directory.
    let mut pkg_paths = vec![root.join("package.json")];
    for p in find_package_jsons(root) {
        if p != root.join("package.json") {
            pkg_paths.push(p);
        }
    }
    let mut production = Vec::new();
    let mut public = Vec::new();
    for pkg_path in pkg_paths {
        let pkg_dir = pkg_path.parent().unwrap_or(root);
        let Ok(text) = std::fs::read_to_string(&pkg_path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        collect_package_entries(root, pkg_dir, files, &v, &mut production, &mut public);
    }

    production.sort();
    production.dedup();
    public.sort();
    public.dedup();
    (production, public)
}

fn collect_package_entries(
    root: &Path,
    pkg_dir: &Path,
    files: &HashSet<String>,
    v: &serde_json::Value,
    production: &mut Vec<String>,
    public: &mut Vec<String>,
) {
    // bin is always a production entry (executable).
    if let Some(bin) = v.get("bin") {
        match bin {
            serde_json::Value::String(s) => {
                if let Some(f) = match_to_file_in(root, pkg_dir, files, s) {
                    production.push(f);
                }
            }
            serde_json::Value::Object(map) => {
                for (_, val) in map {
                    if let Some(s) = val.as_str() {
                        if let Some(f) = match_to_file_in(root, pkg_dir, files, s) {
                            production.push(f);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Determine if this package is a library (has exports/types intended for consumers).
    let has_exports_field = v.get("exports").is_some();
    for field in ["main", "module", "browser"] {
        if let Some(s) = v.get(field).and_then(|x| x.as_str()) {
            if let Some(f) = match_to_file_in(root, pkg_dir, files, s) {
                if has_exports_field {
                    // Library: main is public surface, not an app root.
                    public.push(f);
                } else {
                    // Heuristic: if main points into src/ with no exports map, treat as production root
                    // for apps, but also record as package entry for conservative file findings.
                    production.push(f.clone());
                    public.push(f);
                }
            }
        }
    }
    if let Some(s) = v
        .get("types")
        .and_then(|x| x.as_str())
        .or(v.get("typings").and_then(|x| x.as_str()))
    {
        if let Some(f) = match_to_file_in(root, pkg_dir, files, s) {
            public.push(f);
        }
    }
    if let Some(exports) = v.get("exports") {
        let mut targets = Vec::new();
        collect_exports_targets(exports, &mut targets);
        for t in targets {
            // strip conditions like `require`/`import` handled by recursion; skip non-path conditions.
            if t.starts_with('.')
                || t.contains('/')
                || t.ends_with(".js")
                || t.ends_with(".ts")
                || t.ends_with(".mjs")
                || t.ends_with(".cjs")
            {
                if let Some(f) = match_to_file_in(root, pkg_dir, files, &t) {
                    public.push(f);
                }
            }
        }
    }
}

fn collect_exports_targets(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(arr) => {
            for x in arr {
                collect_exports_targets(x, out);
            }
        }
        serde_json::Value::Object(map) => {
            // All branches recurse: subpath keys ("./foo"), known conditions
            // ("import"/"require"/...), and unknown condition keys alike.
            for val in map.values() {
                collect_exports_targets(val, out);
            }
        }
        _ => {}
    }
}

/// Match a package entry target to a repo file, resolving relative to the
/// package directory that declares it (monorepo-aware).
fn match_to_file_in(
    root: &Path,
    pkg_dir: &Path,
    files: &HashSet<String>,
    target: &str,
) -> Option<String> {
    let t = target.trim();
    let t = t.strip_prefix("./").unwrap_or(t);
    let pkg_rel = crate::parser::to_rel_slash(root, pkg_dir);
    let prefixed = |s: &str| {
        if pkg_rel.is_empty() {
            s.to_string()
        } else {
            format!("{}/{}", pkg_rel, s)
        }
    };
    // Direct match (relative to the declaring package).
    let direct = prefixed(t);
    if files.contains(&direct) {
        return Some(direct);
    }
    if files.contains(t) {
        return Some(t.to_string());
    }
    // Try stripping dist/build -> src mapping? e.g. main: dist/index.js -> src/index.ts
    // Try candidates: same stem with different extensions, src/ variant.
    let stem_variants = candidate_stems(t);
    for cand in stem_variants {
        for c in [prefixed(&cand), cand.clone()] {
            if files.contains(&c) {
                return Some(c);
            }
        }
    }
    // Try root-relative resolution with extensions
    let abs_base = root.join(t);
    for ext in ["ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts"] {
        let with_ext = format!("{}.{}", t.trim_end_matches('/'), ext);
        if files.contains(&with_ext) {
            return Some(with_ext);
        }
        let _ = (abs_base.clone(), ext);
    }
    None
}

fn candidate_stems(t: &str) -> Vec<String> {
    let mut out = Vec::new();
    // Strip a trailing `.d.ts` first so declaration files keep their identity
    // (`types: index.d.ts` must map to types/index.d.ts, not index.d.ts).
    let without_ext = if let Some(s) = t.strip_suffix(".d.ts") {
        s
    } else {
        match t.rfind('.') {
            Some(i) => &t[..i],
            None => t,
        }
    };
    for prefix in [
        "dist/",
        "distribution/",
        "build/",
        "lib/",
        "out/",
        "esm/",
        "cjs/",
        "umd/",
        "output/",
    ] {
        if let Some(rest) = without_ext.strip_prefix(prefix) {
            // Published output maps back to a source tree; projects use either
            // `src/` or the spelled-out `source/` (e.g. sindresorhus packages),
            // or keep sources at the package root.
            for src_pre in ["src/", "source/", ""] {
                for ext in ["ts", "tsx", "js", "jsx", "mts", "cts", "mjs", "cjs"] {
                    out.push(format!("{}{}.{}", src_pre, rest, ext));
                }
                // The output entry may itself be a directory index
                // (`distribution/index.js` -> `source/index.ts` already covered,
                // but also `distribution/foo` -> `source/foo/index.ts`).
                for ext in ["ts", "tsx", "js", "jsx"] {
                    out.push(format!("{}{}/index.{}", src_pre, rest, ext));
                }
            }
        }
    }
    // bare stem + extensions
    for ext in ["ts", "tsx", "js", "jsx", "mts", "cts", "mjs", "cjs"] {
        out.push(format!("{}.{}", without_ext, ext));
    }
    // declaration files (types/index.d.ts etc.)
    out.push(format!("{}.d.ts", without_ext));
    out.push(format!("types/{}.d.ts", without_ext));
    // index variants
    for ext in ["ts", "tsx", "js", "jsx"] {
        out.push(format!("{}/index.{}", without_ext, ext));
    }
    out.push(format!("{}/index.d.ts", without_ext));
    // basename under conventional source roots (`types: index.d.ts` -> types/index.d.ts)
    if let Some(base) = without_ext.rsplit('/').next() {
        for dir in ["types/", "src/", "source/"] {
            out.push(format!("{}{}", dir, base));
            out.push(format!("{}{}.d.ts", dir, base.trim_end_matches(".d.ts")));
        }
    }
    out
}

/// Whether a file is a tooling configuration loaded by external tools
/// (bundlers, test runners, linters) rather than by imports. Such files must
/// never be auto-removed: import analysis cannot prove them unused.
pub fn is_tooling_config(rel: &str) -> bool {
    let f = rel.rsplit('/').next().unwrap_or(rel);
    const EXACT: &[&str] = &[
        "babel.config.js",
        "babel.config.cjs",
        "babel.config.mjs",
        "karma.conf.js",
        "karma.sauce.conf.js",
        "jest.config.js",
        "jest.config.cjs",
        "jest.config.mjs",
        "jest.config.ts",
        "vitest.config.ts",
        "vitest.config.js",
        "webpack.config.js",
        "rollup.config.js",
        "esbuild.config.js",
        "vite.config.ts",
        "vite.config.js",
        "playwright.config.ts",
        "playwright.config.js",
        "cypress.config.ts",
        "cypress.config.js",
        "eslint.config.js",
        "eslint.config.mjs",
        "prettier.config.js",
        "prettier.config.cjs",
        ".eslintrc.js",
        ".eslintrc.cjs",
        ".prettierrc.js",
        "postcss.config.js",
        "tailwind.config.js",
        "nodemon.json",
        // Python tooling: each of these is executed by its tool (Sphinx imports
        // `conf.py`, nox/invoke/fabric import their task files, setuptools runs
        // `setup.py`), never imported by the package itself.
        "conf.py",
        "setup.py",
        "noxfile.py",
        "tasks.py",
        "fabfile.py",
        "sitecustomize.py",
        "usercustomize.py",
        // Hatch loads `hatch_build.py` by filename for
        // `[tool.hatch.build.hooks.custom]`.
        "hatch_build.py",
    ];
    if EXACT.contains(&f) {
        return true;
    }
    const PREFIX: &[&str] = &[
        "karma.",
        "jest.config.",
        "webpack.",
        "rollup.",
        "playwright.",
        "cypress.",
        ".eslintrc",
        ".prettierrc",
        "vitest.",
        "vite.config.",
    ];
    PREFIX.iter().any(|p| f.starts_with(p))
}

/// Whether a file is demonstration code: an example or sample a reader runs by
/// hand (`locust -f examples/basic.py`, `python examples/demo.py`) and that
/// documentation links to. Nothing imports it by design, so import edges cannot
/// speak to whether it is wanted. Report for review, never auto-remove.
pub fn is_example_file(rel: &str) -> bool {
    rel.split('/').any(|comp| {
        matches!(
            comp,
            "example" | "examples" | "sample" | "samples" | "demo" | "demos"
        )
    })
}

/// Whether a file is loaded by test-runner convention rather than imports
/// (e.g. Jest auto-loads `__mocks__/`). Report for review, never auto-remove.
pub fn is_test_framework_magic(rel: &str) -> bool {
    rel.split('/').any(|comp| comp == "__mocks__")
}
/// How consumable this repository is as a package, for conservative file findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageKind {
    /// No package.json: treat as an application (normal rules).
    NoPackage,
    /// Private package or no entry fields: application-like (normal rules).
    App,
    /// Library with an `exports` map: public surface is known (normal rules;
    /// surface files are roots, everything else is analyzable).
    LibraryKnownSurface,
    /// Named, non-private library WITHOUT an `exports` map: consumers may deep-
    /// import any file, so exported files can never be proven dead by imports.
    LibraryUnknownSurface,
}

pub fn package_kind(root: &Path) -> PackageKind {
    let pkg = root.join("package.json");
    let Ok(text) = std::fs::read_to_string(&pkg) else {
        return PackageKind::NoPackage;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return PackageKind::NoPackage;
    };
    if v.get("private").and_then(|p| p.as_bool()).unwrap_or(false) {
        return PackageKind::App;
    }
    if v.get("name").and_then(|n| n.as_str()).is_none() {
        return PackageKind::App;
    }
    if v.get("exports").is_some() {
        return PackageKind::LibraryKnownSurface;
    }
    let has_entry = ["main", "module", "browser", "types", "typings", "bin"]
        .iter()
        .any(|f| v.get(f).is_some());
    if has_entry {
        PackageKind::LibraryUnknownSurface
    } else {
        PackageKind::App
    }
}
/// Load .dietignore + diet-code.json include/exclude into matcher helpers.
pub struct IgnoreRules {
    pub include: Vec<globset::GlobMatcher>,
    pub exclude: Vec<globset::GlobMatcher>,
    pub dietignore: Vec<globset::GlobMatcher>,
}

impl IgnoreRules {
    pub fn load(root: &Path, config: &DietConfig) -> Self {
        let include = config.include.iter().filter_map(|p| glob_for(p)).collect();
        let exclude = config.exclude.iter().filter_map(|p| glob_for(p)).collect();
        let mut dietignore = Vec::new();
        for name in [".dietignore", ".diet-ignore"] {
            let p = root.join(name);
            if let Ok(text) = std::fs::read_to_string(&p) {
                for line in text.lines() {
                    let line = line.trim();
                    if line.is_empty() || line.starts_with('#') {
                        continue;
                    }
                    if let Some(g) = glob_for(line) {
                        dietignore.push(g);
                    }
                }
            }
        }
        Self {
            include,
            exclude,
            dietignore,
        }
    }

    pub fn is_excluded(&self, rel: &str) -> bool {
        for g in &self.dietignore {
            if g.is_match(rel) {
                return true;
            }
        }
        for g in &self.exclude {
            if g.is_match(rel) {
                return true;
            }
        }
        false
    }

    pub fn is_included(&self, rel: &str) -> bool {
        if self.include.is_empty() {
            return true;
        }
        self.include.iter().any(|g| g.is_match(rel))
    }
}

fn glob_for(pattern: &str) -> Option<globset::GlobMatcher> {
    let p = pattern.trim();
    if p.is_empty() {
        return None;
    }
    // Support `dir/**`, `*.ext`, plain prefix `generated/**` etc.
    if let Ok(g) = globset::Glob::new(p) {
        return Some(g.compile_matcher());
    }
    None
}

pub const DEFAULT_IGNORED_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "dist",
    "build",
    "coverage",
    ".next",
    ".nuxt",
    ".turbo",
    ".cache",
    "vendor",
    "target",
    ".diet-code",
    ".agent-diet",
    "out",
];

pub fn is_default_ignored(rel: &str) -> bool {
    let first: Vec<&str> = rel.split('/').collect();
    for comp in &first {
        if DEFAULT_IGNORED_DIRS.contains(comp) {
            return true;
        }
    }
    false
}

/// Discover verify commands from package.json scripts + config override.
pub fn discover_verify_commands(root: &Path, config: &DietConfig) -> Vec<String> {
    if !config.verify_commands.is_empty() {
        return config.verify_commands.clone();
    }
    let pkg = root.join("package.json");
    let Ok(text) = std::fs::read_to_string(&pkg) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let scripts = v.get("scripts").and_then(|s| s.as_object());
    let mut out = Vec::new();
    // Prefer typecheck -> test -> build.
    let pm = detect_package_manager(root);
    if let Some(scripts) = scripts {
        if scripts.contains_key("typecheck") {
            out.push(format!("{} run typecheck", pm));
        }
        if scripts.contains_key("test") {
            // `npm test` works without `run`; normalize to `<pm> test` for npm, `<pm> test` others.
            if pm == "npm" {
                out.push("npm test".to_string());
            } else {
                out.push(format!("{} test", pm));
            }
        } else if scripts.contains_key("test:unit") {
            out.push(format!("{} run test:unit", pm));
        }
        if scripts.contains_key("build") {
            out.push(format!("{} run build", pm));
        }
    }
    out
}

fn detect_package_manager(root: &Path) -> &'static str {
    if root.join("pnpm-workspace.yaml").exists() || root.join("pnpm-lock.yaml").exists() {
        "pnpm"
    } else if root.join("yarn.lock").exists() {
        "yarn"
    } else if root.join("bun.lockb").exists() || root.join("bun.lock").exists() {
        "bun"
    } else {
        "npm"
    }
}

/// Check whether a file is referenced by tooling config (tsconfig include, package.json, etc.).
/// Returns human-readable reasons; empty = no config reference.
pub fn config_references(root: &Path, rel: &str) -> Vec<String> {
    let mut reasons = Vec::new();
    // tsconfig include/files
    let ts = root.join("tsconfig.json");
    if let Ok(text) = std::fs::read_to_string(&ts) {
        if text.contains(rel) {
            reasons.push("referenced by tsconfig.json".to_string());
        }
    }
    // diet-code.json entryPoints
    let dc = root.join("diet-code.json");
    if let Ok(text) = std::fs::read_to_string(&dc) {
        if text.contains(rel) {
            reasons.push("referenced by diet-code.json".to_string());
        }
    }
    reasons
}
