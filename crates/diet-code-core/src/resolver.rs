use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::parser::to_rel_slash;

/// TypeScript path-alias configuration.
#[derive(Debug, Clone, Default)]
pub struct TsAliasConfig {
    pub base_url: Option<PathBuf>, // absolute
    pub paths: Vec<(String, Vec<String>)>,
}

impl TsAliasConfig {
    pub fn load(root: &Path) -> Self {
        let tsconfig = root.join("tsconfig.json");
        let Ok(text) = std::fs::read_to_string(&tsconfig) else {
            return Self::default();
        };
        // Strip comments (best effort) then parse.
        let stripped = strip_json_comments(&text);
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&stripped) else {
            return Self::default();
        };
        let co = v.get("compilerOptions");
        let mut cfg = Self::default();
        if let Some(co) = co {
            if let Some(base) = co.get("baseUrl").and_then(|b| b.as_str()) {
                cfg.base_url = Some(root.join(base));
            }
            if let Some(paths) = co.get("paths").and_then(|p| p.as_object()) {
                for (k, vs) in paths {
                    if let Some(arr) = vs.as_array() {
                        let targets: Vec<String> =
                            arr.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect();
                        cfg.paths.push((k.clone(), targets));
                    }
                }
            }
        }
        cfg
    }

    /// Try to resolve an alias specifier to an absolute candidate base (without extension).
    pub fn resolve_alias(&self, root: &Path, spec: &str) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for (pattern, targets) in &self.paths {
            if let Some(rest) = match_alias(pattern, spec) {
                for t in targets {
                    let replaced = t.replace('*', &rest);
                    let base = if Path::new(&replaced).is_absolute() {
                        PathBuf::from(&replaced)
                    } else if let Some(bu) = &self.base_url {
                        bu.join(&replaced)
                    } else {
                        root.join(&replaced)
                    };
                    out.push(base);
                }
            }
        }
        out
    }
}

/// Strip a JS-like extension (`.js`/`.mjs`/`.cjs`/`.jsx`) to find a sibling
/// `.d.ts` declaration file.
fn js_like_stem(base: &Path) -> Option<PathBuf> {
    let s = base.to_string_lossy();
    for ext in [".js", ".mjs", ".cjs", ".jsx"] {
        if let Some(stripped) = s.strip_suffix(ext) {
            // Avoid treating `.d.ts`-style or extensionless paths here.
            if !stripped.is_empty() {
                return Some(PathBuf::from(stripped));
            }
        }
    }
    None
}
/// Lexically normalize a path (resolve `.`/`..` without touching the filesystem).
fn normalize_lexical(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}
fn match_alias(pattern: &str, spec: &str) -> Option<String> {
    if let Some(star) = pattern.find('*') {
        let (pre, post) = (&pattern[..star], &pattern[star + 1..]);
        if spec.starts_with(pre) && spec.ends_with(post) {
            let mid = &spec[pre.len()..spec.len() - post.len()];
            return Some(mid.to_string());
        }
        None
    } else if pattern == spec {
        Some(String::new())
    } else {
        None
    }
}

fn strip_json_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    let mut in_str = false;
    let mut escaped = false;
    while let Some(c) = chars.next() {
        if in_str {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        if c == '"' {
            in_str = true;
            out.push(c);
        } else if c == '/' {
            match chars.peek() {
                Some('/') => {
                    for nc in chars.by_ref() {
                        if nc == '\n' {
                            out.push('\n');
                            break;
                        }
                    }
                }
                Some('*') => {
                    chars.next();
                    let mut prev = ' ';
                    for nc in chars.by_ref() {
                        if prev == '*' && nc == '/' {
                            break;
                        }
                        prev = nc;
                    }
                }
                _ => out.push(c),
            }
        } else {
            out.push(c);
        }
    }
    out
}

const CANDIDATE_EXTS: &[&str] = &["ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts"];

/// Resolve a raw specifier from `from_file` to a repo-relative slash path, if possible.
/// `from_file_rel` is slash-separated relative path. `all_files` is set of known rel paths.
pub struct Resolver {
    pub root: PathBuf,
    pub aliases: TsAliasConfig,
    /// rel slash path -> abs path
    pub file_map: HashMap<String, PathBuf>,
    pub file_set: HashSet<String>,
    /// package.json `name`, for resolving self-imports (`import x from 'pkg'` inside pkg).
    pub package_name: Option<String>,
}

impl Resolver {
    pub fn new(root: &Path, files: &[PathBuf]) -> Self {
        let mut file_map = HashMap::new();
        let mut file_set = HashSet::new();
        for f in files {
            let rel = to_rel_slash(root, f);
            file_map.insert(rel.clone(), f.clone());
            file_set.insert(rel);
        }
        let aliases = TsAliasConfig::load(root);
        let package_name = std::fs::read_to_string(root.join("package.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(|s| s.to_string()));
        Self {
            root: root.to_path_buf(),
            aliases,
            file_map,
            file_set,
            package_name,
        }
    }

    pub fn is_relative(&self, spec: &str) -> bool {
        spec.starts_with("./") || spec.starts_with("../") || spec == "." || spec == ".."
    }

    pub fn is_bare_package(&self, spec: &str) -> bool {
        !self.is_relative(spec) && !spec.starts_with('/') && !spec.starts_with("@/") || (spec.contains('/') && !spec.starts_with('.') && !self.alias_matches(spec))
        // Actually bare packages are non-relative non-alias. We return true if it looks like a package
        // and no alias matches. Simplify: if not relative and no alias match and not absolute alias path.
    }

    fn alias_matches(&self, spec: &str) -> bool {
        for (pat, _) in &self.aliases.paths {
            if match_alias(pat, spec).is_some() {
                return true;
            }
        }
        false
    }

    pub fn resolve(&self, from_file_rel: &str, spec: &str) -> Option<String> {
        if spec.is_empty() {
            return None;
        }
        // Strip query/hash? not needed.
        let spec = spec.trim();
        if self.is_relative(spec) {
            let from_abs = self.file_map.get(from_file_rel)?;
            let from_dir = from_abs.parent().unwrap_or(&self.root);
            let joined = normalize_lexical(&from_dir.join(spec));
            return self.try_candidates(&joined);
        }
        // Absolute path alias (starting with /)? treat as root-relative.
        if spec.starts_with('/') {
            let joined = self.root.join(spec.trim_start_matches('/'));
            if let Some(r) = self.try_candidates(&joined) {
                return Some(r);
            }
        }
        // tsconfig paths aliases (including @/*)
        if self.alias_matches(spec) {
            for base in self.aliases.resolve_alias(&self.root, spec) {
                if let Some(r) = self.try_candidates(&base) {
                    return Some(r);
                }
            }
        }
        // baseUrl fallback: try root/baseUrl + spec
        if let Some(bu) = &self.aliases.base_url {
            let joined = bu.join(spec);
            if let Some(r) = self.try_candidates(&joined) {
                return Some(r);
            }
        }
        // Package self-imports: `import x from 'pkg'` inside pkg itself
        // (common in .d.ts files and for subpath self-references).
        if let Some(name) = self.package_name.clone() {
            if spec == name {
                for base in [
                    self.root.join("index"),
                    self.root.join("src/index"),
                    self.root.join("types/index"),
                ] {
                    if let Some(r) = self.try_candidates(&normalize_lexical(&base)) {
                        return Some(r);
                    }
                }
            } else if let Some(rest) = spec.strip_prefix(&format!("{}/", name)) {
                for base in [
                    self.root.join(rest),
                    self.root.join(format!("src/{}", rest)),
                    self.root.join(format!("types/{}", rest)),
                ] {
                    if let Some(r) = self.try_candidates(&normalize_lexical(&base)) {
                        return Some(r);
                    }
                }
            }
        }
        // Bare package imports (react, lodash) -> external, unresolvable. Return None.
        None
    }

    fn try_candidates(&self, base: &Path) -> Option<String> {
        // If spec already has extension and file exists
        if let Some(ext) = base.extension().and_then(|e| e.to_str()) {
            if CANDIDATE_EXTS.contains(&ext) && base.is_file() {
                return Some(to_rel_slash(&self.root, base));
            }
        }
        // `./x.js` where only `x.d.ts` exists (TypeScript nodenext style).
        if let Some(stem) = js_like_stem(base) {
            for dts in [
                format!("{}.d.ts", stem.to_string_lossy()),
                format!("{}.d.mts", stem.to_string_lossy()),
                format!("{}.d.cts", stem.to_string_lossy()),
            ] {
                let cand = PathBuf::from(dts);
                if cand.is_file() {
                    return Some(to_rel_slash(&self.root, &cand));
                }
            }
        }
        // Try base + .ext
        for ext in CANDIDATE_EXTS {
            let cand = base.with_extension(ext);
            // with_extension replaces existing; for extensionless base it's fine.
            // But for `foo` -> `foo.ts` good. Use set_extension approach via file name append:
            let cand2 = PathBuf::from(format!("{}.{}", base.to_string_lossy(), ext));
            // Declaration files for extensionless imports (`./x` -> `x.d.ts`).
            let cand3 = PathBuf::from(format!("{}.d.ts", base.to_string_lossy()));
            for cand in [&cand, &cand2, &cand3] {
                if cand.is_file() {
                    let rel = to_rel_slash(&self.root, cand);
                    if self.file_set.contains(&rel) {
                        return Some(rel);
                    }
                    // Even if not in discovered set (e.g. excluded?), still return if file exists and supported.
                    return Some(rel);
                }
            }
        }
        // Try directory index
        if base.is_dir() {
            for ext in CANDIDATE_EXTS {
                let cand = base.join(format!("index.{}", ext));
                if cand.is_file() {
                    let rel = to_rel_slash(&self.root, &cand);
                    return Some(rel);
                }
            }
        } else {
            // base may not exist as dir but `base/index.ts` might (when base path doesn't exist as file)
            for ext in CANDIDATE_EXTS {
                let cand = base.join(format!("index.{}", ext));
                if cand.is_file() {
                    let rel = to_rel_slash(&self.root, &cand);
                    return Some(rel);
                }
            }
        }
        None
    }
}
