pub mod confidence;
pub mod edits;
pub mod entrypoints;
pub mod findings;
pub mod graph;
pub mod imports;
pub mod parser;
pub mod reachability;
pub mod resolver;
pub mod symbols;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::Utc;
use rayon::prelude::*;

use confidence::Confidence;
use edits::CleanupPlan;
use entrypoints::{DietConfig, EntrySets, IgnoreRules, is_default_ignored, is_test_file};
use findings::{Finding, FindingKind, GitEvidence};
use graph::{build_binding_maps, resolve_references, Graph};
use imports::{ImportRec, ReExportRec, ReferenceRec};
use parser::{ParsedFile, to_rel_slash};
use reachability::{compute_reachability, Reachability};
use resolver::Resolver;
use symbols::{Entity, SymbolKind};

pub const SCHEMA_VERSION: u32 = 1;

pub trait DeadCodeDetector {
    fn analyze(&self, root: &Path) -> anyhow::Result<AnalysisResult>;
}

pub struct TreeSitterDetector;

impl DeadCodeDetector for TreeSitterDetector {
    fn analyze(&self, root: &Path) -> anyhow::Result<AnalysisResult> {
        analyze_repository(root)
    }
}

/// Full in-memory analysis result. Serializes to the stable JSON schema via `to_json_value`.
pub struct AnalysisResult {
    pub version: u32,
    pub repository: String,
    pub generated_at: String,
    pub root: PathBuf,
    pub files: Vec<String>,
    pub entities: Vec<Entity>,
    pub findings: Vec<Finding>,
    pub reachability: Reachability,
    pub entries: EntrySets,
    pub file_dynamic: HashMap<String, Vec<String>>,
    /// (file, name) -> is exported (after local-export marking + re-export computation).
    pub file_exported_names: HashMap<String, HashSet<String>>,
    /// Protected directory prefixes from dynamic imports: (importer_dir, prefix_rel) list.
    pub dynamic_protected_prefixes: Vec<String>,
    pub file_importers_prod: HashMap<String, usize>,
    pub file_importers_test: HashMap<String, usize>,
    pub package_kind: entrypoints::PackageKind,
    /// file -> class names constructed in return/export position (may escape).
    pub escaped_classes: HashMap<String, HashSet<String>>,
    /// files assigning to `obj.prop` (plugin/prototype-registration pattern).
    pub publishes_members: HashSet<String>,
    /// files whose ambient-global types are referenced from other files.
    pub files_with_used_globals: HashSet<String>,
    /// files executed via package.json scripts (tooling entry points).
    pub script_refs: HashSet<String>,
    /// files referenced by CI workflow files.
    pub workflow_refs: HashSet<String>,
    /// executable scripts (`process.argv`/shebang): run manually, not imported.
    pub executable_scripts: HashSet<String>,
    /// script-like files: no exports at all + top-level calls (may be run directly).
    pub script_like_files: HashSet<String>,
    /// methods defined in object literals passed as call arguments (callbacks/hooks).
    pub hook_methods: HashSet<(String, String, usize)>,
    /// methods of objects assigned to module exports (public surface).
    pub surface_methods: HashSet<(String, String, usize)>,
    /// owner scopes whose methods may be invoked via computed dispatch (`obj[key]()`).
    pub dispatch_owners: HashSet<String>,
    /// (file, name) of unresolvable `.name()` invocations in reachable code.
    pub dangling_calls: HashSet<(String, String)>,
    /// (file, variable) spread into object literals (`{...opts}`).
    pub spread_vars: HashSet<(String, String)>,
    /// derived class id -> base class ids (resolved heritage for overrides).
    pub heritage_links: HashMap<String, Vec<String>>,
    pub config: DietConfig,
}

impl AnalysisResult {
    pub fn summary_counts(&self) -> (usize, usize, usize, usize, usize, usize) {
        let (mut c, mut h, mut m, mut l) = (0, 0, 0, 0);
        for f in &self.findings {
            match f.confidence {
                Confidence::Certain => c += 1,
                Confidence::High => h += 1,
                Confidence::Medium => m += 1,
                Confidence::Low => l += 1,
            }
        }
        (self.files.len(), self.entities.len(), c, h, m, l)
    }

    pub fn removable_loc(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.confidence.auto_removable())
            .map(|f| f.end_line.saturating_sub(f.start_line).saturating_add(1))
            .sum()
    }

    pub fn to_json_value(&self) -> serde_json::Value {
        let (files, symbols, certain, high, medium, low) = self.summary_counts();
        serde_json::json!({
            "version": self.version,
            "repository": self.repository,
            "generatedAt": self.generated_at,
            "summary": {
                "files": files,
                "symbols": symbols,
                "certain": certain,
                "high": high,
                "medium": medium,
                "low": low,
                "removableLoc": self.removable_loc(),
            },
            "findings": self.findings,
        })
    }

    pub fn cleanup_plan(&self) -> CleanupPlan {
        build_cleanup_plan(self)
    }

    pub fn find(&self, query: &str) -> Vec<&Finding> {
        self.findings
            .iter()
            .filter(|f| f.file == query || f.id == query || f.short_label().contains(query))
            .collect()
    }
}

pub fn analyze_repository(root: &Path) -> anyhow::Result<AnalysisResult> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let config = DietConfig::load(&root);
    let rules = IgnoreRules::load(&root, &config);

    // 1. Discover files.
    let mut abs_files = discover_files(&root, &rules);
    abs_files.sort();
    abs_files.dedup();

    // 2. Parse in parallel.
    let parsed: Vec<ParsedFile> = abs_files
        .par_iter()
        .filter_map(|abs| parser::parse_file(&root, abs))
        .collect();

    let mut rel_files: Vec<String> = parsed.iter().map(|p| p.rel_path.clone()).collect();
    rel_files.sort();
    rel_files.dedup();
    let file_set: HashSet<String> = rel_files.iter().cloned().collect();

    // 3. Resolve imports / re-exports.
    let resolver = Resolver::new(&root, &abs_files);
    let mut imports: Vec<ImportRec> = Vec::new();
    let mut reexports: Vec<ReExportRec> = Vec::new();
    let mut references: Vec<ReferenceRec> = Vec::new();
    let mut entities: Vec<Entity> = Vec::new();
    let mut file_dynamic: HashMap<String, Vec<String>> = HashMap::new();
    let mut local_exports_map: HashMap<String, Vec<parser::LocalExport>> = HashMap::new();
    let mut var_types: HashMap<(String, String), String> = HashMap::new();
    let mut escaped_classes: HashMap<String, HashSet<String>> = HashMap::new();
    let mut publishes_members: HashSet<String> = HashSet::new();
    let mut executable_scripts: HashSet<String> = HashSet::new();
    let mut script_like_files: HashSet<String> = HashSet::new();
    let mut hook_methods: HashSet<(String, String, usize)> = HashSet::new();
    let mut surface_methods: HashSet<(String, String, usize)> = HashSet::new();
    let mut dispatch_bases: Vec<(String, String, Option<String>)> = Vec::new();
    let mut dangling_calls: HashSet<(String, String)> = HashSet::new();
    let mut spread_vars: HashSet<(String, String)> = HashSet::new();
    let mut heritage_pairs: Vec<(String, String, String)> = Vec::new();

    for p in &parsed {
        for mut imp in p.imports.clone() {
            imp.resolved_file = resolver.resolve(&p.rel_path, &imp.source_raw);
            imports.push(imp);
        }
        for mut re in p.reexports.clone() {
            re.resolved_file = resolver.resolve(&p.rel_path, &re.source_raw);
            reexports.push(re);
        }
        references.extend(p.references.iter().cloned());
        entities.extend(p.entities.iter().cloned());
        for (var, cls) in &p.var_types {
            var_types.entry((p.rel_path.clone(), var.clone())).or_insert_with(|| cls.clone());
        }
        if !p.escaped_classes.is_empty() {
            escaped_classes.entry(p.rel_path.clone()).or_default().extend(p.escaped_classes.iter().cloned());
        }
        if p.publishes_members {
            publishes_members.insert(p.rel_path.clone());
        }
        if p.is_executable_script {
            executable_scripts.insert(p.rel_path.clone());
        }
        if p.has_top_level_calls {
            script_like_files.insert(p.rel_path.clone());
        }
        for (name, line) in &p.hook_methods {
            hook_methods.insert((p.rel_path.clone(), name.clone(), *line));
        }
        for (name, line) in &p.surface_methods {
            surface_methods.insert((p.rel_path.clone(), name.clone(), *line));
        }
        for (derived, base) in &p.heritage {
            if let Some(file) = derived.split("::").next() {
                heritage_pairs.push((file.to_string(), derived.clone(), base.clone()));
            }
        }
        for (base, scope) in &p.dynamic_dispatch_bases {
            dispatch_bases.push((p.rel_path.clone(), base.clone(), scope.clone()));
        }
        for var in &p.spread_vars {
            spread_vars.insert((p.rel_path.clone(), var.clone()));
        }
        if !p.dynamic_details.is_empty() {
            file_dynamic.insert(p.rel_path.clone(), p.dynamic_details.clone());
        }
        local_exports_map.insert(p.rel_path.clone(), p.local_exports.clone());
    }

    // 4. Declaration merging (TypeScript): overload signatures and same-name,
    //    same-scope namespace/type pairs form ONE public name. If any such
    //    declaration is exported, all of them are surface. (Scoped by parent so
    //    nested shadowing is unaffected.)
    {
        let mut union: HashMap<(String, String, String), (bool, bool)> = HashMap::new();
        for e in &entities {
            let key = (e.file.clone(), e.parent.clone().unwrap_or_default(), e.name.clone());
            let slot = union.entry(key).or_insert((false, false));
            slot.0 |= e.exported;
            slot.1 |= e.is_default_export;
        }
        for e in entities.iter_mut() {
            let key = (e.file.clone(), e.parent.clone().unwrap_or_default(), e.name.clone());
            if let Some((exp, def)) = union.get(&key) {
                e.exported |= exp;
                e.is_default_export |= def;
            }
        }
    }

    // 5. Build graph nodes.
    let mut graph = Graph::new();
    let mut file_to_symbols: HashMap<String, Vec<String>> = HashMap::new();
    for e in &entities {
        graph
            .symbols_by_name
            .entry(e.name.clone())
            .or_default()
            .push(e.id.clone());
        // Only first declaration wins for (file,name) lookup (overloads etc.).
        graph
            .symbol_by_file_name
            .entry((e.file.clone(), e.name.clone()))
            .or_insert_with(|| e.id.clone());
        // Lexical scope maps: (file, enclosing-scope) -> name -> id.
        graph
            .scope_children
            .entry((e.file.clone(), e.parent.clone().unwrap_or_default()))
            .or_default()
            .entry(e.name.clone())
            .or_insert_with(|| e.id.clone());
        graph
            .parent_of
            .insert(e.id.clone(), e.parent.clone().unwrap_or_default());
        graph.kinds.insert(e.id.clone(), e.kind.clone());
        // Default exports are also addressable as `default` (for `import foo from`).
        if e.is_default_export && e.name != "default" {
            graph
                .symbol_by_file_name
                .entry((e.file.clone(), "default".to_string()))
                .or_insert_with(|| e.id.clone());
        }
        if e.kind == SymbolKind::Method {
            graph
                .methods_by_name
                .entry(e.name.clone())
                .or_default()
                .push(e.id.clone());
        }
        file_to_symbols.entry(e.file.clone()).or_default().push(e.id.clone());
    }
    // Ensure every file has an entry in file_to_symbols.
    for f in &rel_files {
        file_to_symbols.entry(f.clone()).or_default();
    }

    // File edges (only resolved; include side-effect/require/dynamic-literal).
    for imp in &imports {
        if let Some(target) = &imp.resolved_file {
            if file_set.contains(target) {
                graph.add_file_edge(&imp.from_file, target);
            }
        }
    }
    for re in &reexports {
        if let Some(target) = &re.resolved_file {
            if file_set.contains(target) {
                graph.add_file_edge(&re.from_file, target);
            }
        }
    }

    // 6. Barrel expansion: (file, exported_name) -> Vec<(target_file, target_original)>.
    let reexport_map = build_reexport_map(&reexports, &entities, &file_set);

    // File -> exported names (direct entities + re-exports incl. wildcard expansion).
    let file_exported_names = build_file_export_names(&entities, &reexports, &reexport_map, &local_exports_map, &rel_files);

    // Namespace imports: file -> local_ns -> resolved target file.
    // (`const lib = require("./lib")` binds the whole module too.)
    let mut namespace_imports: HashMap<String, HashMap<String, String>> = HashMap::new();
    for imp in &imports {
        let is_ns = imp.kind == imports::ImportKind::Namespace
            || (imp.kind == imports::ImportKind::Require && imp.original_name == "*");
        if is_ns {
            if let Some(t) = &imp.resolved_file {
                namespace_imports
                    .entry(imp.from_file.clone())
                    .or_default()
                    .insert(imp.local_name.clone(), t.clone());
            }
        }
    }

    // 7. Resolve symbol references.
    let bindings = build_binding_maps(&imports);
    // Ambient-global types from import/export-less script files.
    let ambient_files: HashSet<String> = parsed.iter().filter(|p| p.is_ambient).map(|p| p.rel_path.clone()).collect();
    let mut global_types: HashMap<String, String> = HashMap::new();
    for e in &entities {
        if !ambient_files.contains(&e.file) || e.parent.is_some() {
            continue;
        }
        match e.kind {
            SymbolKind::Interface
            | SymbolKind::TypeAlias
            | SymbolKind::Enum
            | SymbolKind::Namespace
            | SymbolKind::Class => {
                global_types.entry(e.name.clone()).or_insert_with(|| e.id.clone());
            }
            _ => {}
        }
    }
    // Heritage links: derived class id -> base class ids. Same-file classes
    // first, then classes imported into the derived file.
    let mut heritage_links: HashMap<String, Vec<String>> = HashMap::new();
    for (file, derived, base_name) in &heritage_pairs {
        let mut target: Option<String> = None;
        if let Some(id) = graph
            .symbol_by_file_name
            .get(&(file.clone(), base_name.clone()))
        {
            if graph.kinds.get(id) == Some(&SymbolKind::Class) {
                target = Some(id.clone());
            }
        }
        if target.is_none() {
            if let Some(fb) = bindings.get(file) {
                if let Some((resolved, orig, _)) = fb.get(base_name) {
                    if let Some(t) = resolved {
                        let cn = if orig == "default" { "default" } else { orig.as_str() };
                        if let Some(id) = graph
                            .symbol_by_file_name
                            .get(&(t.clone(), cn.to_string()))
                        {
                            if graph.kinds.get(id) == Some(&SymbolKind::Class) {
                                target = Some(id.clone());
                            }
                        }
                    }
                }
            }
        }
        if let Some(id) = target {
            heritage_links.entry(derived.clone()).or_default().push(id);
        }
    }

    resolve_references(&mut graph, &references, &bindings, &reexport_map, &namespace_imports, &file_exported_names, &var_types, &global_types, &heritage_links, &mut dangling_calls);

    // Files executed via package.json scripts anywhere in the repo.
    let script_refs = entrypoints::script_referenced_files(&root, &file_set);
    // Files referenced by CI workflow files.
    let workflow_refs = entrypoints::workflow_referenced_files(&root, &file_set);
    // Owner scopes whose methods may run via computed dispatch (`handlers[key]()`):
    // the base variable/class itself (resolved in its lexical scope), or the
    // class of a typed variable.
    let mut dispatch_owners: HashSet<String> = HashSet::new();
    for (file, base, scope) in &dispatch_bases {
        if let Some(owner) = graph::lookup_scoped(&graph, file, scope, base) {
            match graph.kinds.get(&owner) {
                Some(SymbolKind::Variable) | Some(SymbolKind::Class) => {
                    dispatch_owners.insert(owner);
                    continue;
                }
                _ => {}
            }
        }
        if let Some(cls) = var_types.get(&(file.clone(), base.clone())) {
            // Same-file class first, else any top-level class with that name.
            let mut found = graph
                .symbol_by_file_name
                .get(&(file.clone(), cls.clone()))
                .filter(|id| graph.kinds.get(*id) == Some(&SymbolKind::Class))
                .cloned();
            if found.is_none() {
                found = graph
                    .symbols_by_name
                    .get(cls)
                    .map(|ids| {
                        ids.iter().find(|id| {
                            graph.kinds.get(*id) == Some(&SymbolKind::Class)
                                && graph.parent_of.get(*id).map(|p| p.is_empty()).unwrap_or(false)
                        })
                    })
                    .flatten()
                    .cloned();
            }
            if let Some(id) = found {
                dispatch_owners.insert(id);
            }
        }
    }
    // Files whose ambient globals are used elsewhere are load-bearing.
    let mut files_with_used_globals: HashSet<String> = HashSet::new();
    for id in global_types.values() {
        if graph.symbol_callers.get(id).map(|c| !c.is_empty()).unwrap_or(false) {
            if let Some(file) = id.split("::").next() {
                files_with_used_globals.insert(file.to_string());
            }
        }
    }

    // 8. Entrypoints.
    let entries = entrypoints::discover_entrypoints(&root, &rel_files, &config);

    // 9. Reachability.
    let reachability = compute_reachability(
        &graph,
        &file_to_symbols,
        &entries.production,
        &entries.tests,
        &entries.package_export_files,
        &script_refs,
        &file_set,
    );

    // 10. Dynamic protected prefixes (variable dynamic imports with static prefix).
    let dynamic_protected_prefixes = compute_dynamic_prefixes(&parsed, &root);

    // 11. Importer counts split prod/test.
    let mut file_importers_prod: HashMap<String, usize> = HashMap::new();
    let mut file_importers_test: HashMap<String, usize> = HashMap::new();
    for f in &rel_files {
        let importers = graph.importers_of(f);
        let mut p = 0;
        let mut t = 0;
        for im in &importers {
            if reachability.prod_files.contains(im) {
                p += 1;
            }
            if reachability.test_files.contains(im) || is_test_file(im) {
                t += 1;
            }
        }
        file_importers_prod.insert(f.clone(), p);
        file_importers_test.insert(f.clone(), t);
    }

    // 12. Findings.
    let mut result = AnalysisResult {
        version: SCHEMA_VERSION,
        repository: root
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("repository")
            .to_string(),
        generated_at: Utc::now().to_rfc3339(),
        root: root.clone(),
        files: rel_files,
        entities,
        findings: Vec::new(),
        reachability,
        entries,
        file_dynamic,
        file_exported_names,
        dynamic_protected_prefixes,
        file_importers_prod,
        file_importers_test,
        package_kind: entrypoints::package_kind(&root),
        escaped_classes,
        publishes_members,
        files_with_used_globals,
        script_refs,
        workflow_refs,
        executable_scripts,
        script_like_files,
        hook_methods,
        surface_methods,
        dispatch_owners,
        dangling_calls,
        spread_vars,
        heritage_links,
        config,
    };
    result.findings = compute_findings(&result, &graph);

    // 13. Git evidence (best effort, per finding file).
    attach_git_evidence(&root, &mut result.findings);

    // Stable ordering: confidence desc, then file, then line.
    result.findings.sort_by(|a, b| {
        b.confidence
            .rank()
            .cmp(&a.confidence.rank())
            .then(a.file.cmp(&b.file))
            .then(a.start_line.cmp(&b.start_line))
    });

    Ok(result)
}

fn discover_files(root: &Path, rules: &IgnoreRules) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    let mut seen_dirs: HashSet<PathBuf> = HashSet::new();
    while let Some(dir) = stack.pop() {
        let canon = dir.canonicalize().unwrap_or_else(|_| dir.clone());
        if !seen_dirs.insert(canon) {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            // Do not follow symlinked directories by default.
            if let Ok(ft) = entry.file_type() {
                if ft.is_symlink() {
                    // Allow symlinked files? Skip symlinked dirs; skip symlinked files too for determinism.
                    continue;
                }
                if ft.is_dir() {
                    let rel = to_rel_slash(root, &path);
                    if is_default_ignored(&rel) {
                        continue;
                    }
                    // Check dietignore/exclude on dir prefix.
                    if rules.is_excluded(&rel) || rules.is_excluded(&format!("{}/", rel)) {
                        continue;
                    }
                    stack.push(path);
                } else if ft.is_file() {
                    if !parser::is_supported_file(&path) {
                        continue;
                    }
                    let rel = to_rel_slash(root, &path);
                    if is_default_ignored(&rel) {
                        continue;
                    }
                    if rules.is_excluded(&rel) {
                        continue;
                    }
                    if !rules.is_included(&rel) {
                        continue;
                    }
                    out.push(path);
                }
            }
        }
    }
    out
}

/// Build (file, exported_name) -> targets, expanding `export *` via target file export lists.
fn build_reexport_map(
    reexports: &[ReExportRec],
    entities: &[Entity],
    file_set: &HashSet<String>,
) -> HashMap<(String, String), Vec<(String, String)>> {
    // Direct named re-exports.
    let mut map: HashMap<(String, String), Vec<(String, String)>> = HashMap::new();
    // Collect direct exports per file: entity exported names + default.
    let mut direct: HashMap<String, HashSet<String>> = HashMap::new();
    for e in entities {
        if e.exported {
            direct.entry(e.file.clone()).or_default().insert(e.name.clone());
        }
    }
    // Wildcards to expand after fixed-point of named? Wildcard expansion needs target's full export list
    // including ITS re-exports — iterate to fixed point.
    // First insert named.
    let mut wildcards: Vec<&ReExportRec> = Vec::new();
    for re in reexports {
        let Some(target) = re.resolved_file.clone() else {
            continue;
        };
        if !file_set.contains(&target) {
            continue;
        }
        if re.is_wildcard {
            wildcards.push(re);
        } else {
            map.entry((re.from_file.clone(), re.exported_name.clone()))
                .or_default()
                .push((target, re.original_name.clone()));
        }
    }
    // Expand wildcards iteratively (bounded).
    for _ in 0..10 {
        let mut progress = false;
        for re in wildcards.clone() {
            let target = re.resolved_file.clone().unwrap();
            // Target's export names = direct + already-known re-export keys.
            let mut names: HashSet<String> = direct.get(&target).cloned().unwrap_or_default();
            for ((f, n), _) in map.iter() {
                if f == &target && n != "*" {
                    names.insert(n.clone());
                }
            }
            // `export *` skips default.
            names.remove("default");
            for n in names {
                if n == "*" {
                    continue;
                }
                let key = (re.from_file.clone(), n.clone());
                // For `export * as ns`, the namespace itself is exported, not members — skip member expansion.
                if re.exported_name != "*" {
                    // `export * as ns from` -> export name `ns` mapping to wildcard; record once.
                    let ns_key = (re.from_file.clone(), re.exported_name.clone());
                    if !map.contains_key(&ns_key) {
                        map.insert(ns_key, vec![(target.clone(), "*".to_string())]);
                        progress = true;
                    }
                    break;
                }
                if !map.contains_key(&key) {
                    map.insert(key, vec![(target.clone(), n.clone())]);
                    progress = true;
                }
            }
        }
        if !progress {
            break;
        }
    }
    map
}

fn build_file_export_names(
    entities: &[Entity],
    reexports: &[ReExportRec],
    reexport_map: &HashMap<(String, String), Vec<(String, String)>>,
    local_exports: &HashMap<String, Vec<parser::LocalExport>>,
    files: &[String],
) -> HashMap<String, HashSet<String>> {
    let mut m: HashMap<String, HashSet<String>> = HashMap::new();
    for f in files {
        m.insert(f.clone(), HashSet::new());
    }
    for e in entities {
        if e.exported {
            m.entry(e.file.clone()).or_default().insert(e.name.clone());
        }
    }
    // Announced exports without declarations (`export { x }`, `export default x`,
    // `export const { a } = ...`) still make the file a surface.
    for (file, les) in local_exports {
        for le in les {
            m.entry(file.clone()).or_default().insert(le.exported_name.clone());
        }
    }
    for re in reexports {
        if re.is_wildcard {
            if re.exported_name != "*" {
                m.entry(re.from_file.clone()).or_default().insert(re.exported_name.clone());
            } else {
                // expand members known via map
                for ((f, n), _) in reexport_map.iter() {
                    if f == &re.from_file {
                        m.entry(f.clone()).or_default().insert(n.clone());
                    }
                }
            }
        } else {
            m.entry(re.from_file.clone()).or_default().insert(re.exported_name.clone());
        }
    }
    m
}

fn compute_dynamic_prefixes(parsed: &[ParsedFile], _root: &Path) -> Vec<String> {
    // AST-recorded prefixes (`import(`./plugins/${n}`)` in F protects
    // dir(F)/plugins/**; `require(path.join(__dirname, "rules", n))`
    // protects that dir), plus a textual backstop for unusual formatting.
    let mut out = Vec::new();
    for p in parsed {
        out.extend(p.dynamic_prefixes.iter().cloned());
        for line in p.source.lines() {
            // `require(path.join(__dirname, name))` / `require(path.resolve(...))`
            // with a non-literal segment: directory-scan loading — protect the
            // importer's own directory (but not when a literal names the file).
            if line.contains("require(")
                && (line.contains("__dirname") || line.contains("__filename"))
                && !line.contains('"')
                && !line.contains('\'')
            {
                let dir = parent_dir(&p.rel_path);
                if !dir.is_empty() {
                    out.push(dir);
                }
            }
            // `import(`...`)`, `import.meta.resolve(`...`)`, and `require(`...`)`
            // with `${}` templates.
            for marker in ["import(", "import.meta.resolve(", "require("] {
                let mut search = line;
                while let Some(idx) = search.find(marker) {
                    // For `import(`, skip the `import.meta.resolve(` case (handled by its own marker).
                    if marker == "import(" && search[idx..].starts_with("import.meta.resolve(") {
                        search = &search[idx + 1..];
                        continue;
                    }
                    let rest = &search[idx..];
                    // backtick template with ${}
                    if rest.contains('`') && rest.contains("${") {
                        // Extract static prefix between backtick and ${.
                        if let Some(bt) = rest.find('`') {
                            let after = &rest[bt + 1..];
                            if let Some(dollar) = after.find("${") {
                                let prefix = after[..dollar].to_string();
                                let prefix_is_dir = prefix.ends_with('/');
                                // Resolve prefix relative to importer's dir.
                                let importer_dir = parent_dir(&p.rel_path);
                                let joined = join_rel(&importer_dir, &prefix);
                                let norm = normalize_rel(&joined);
                                // Protect everything under the referenced directory.
                                let protodir = if prefix_is_dir {
                                    norm
                                } else {
                                    match norm.rfind('/') {
                                        Some(i) => norm[..i].to_string(),
                                        None => String::new(),
                                    }
                                };
                                if !protodir.is_empty() {
                                    out.push(protodir);
                                }
                            }
                        }
                    }
                    search = &search[idx + 1..];
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

fn parent_dir(rel: &str) -> String {
    match rel.rfind('/') {
        Some(i) => rel[..i].to_string(),
        None => String::new(),
    }
}

fn join_rel(dir: &str, spec: &str) -> String {
    if dir.is_empty() {
        return spec.to_string();
    }
    format!("{}/{}", dir, spec)
}

fn normalize_rel(p: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for comp in p.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    parts.join("/")
}

fn is_protected_by_dynamic_prefix(prefixes: &[String], file: &str) -> bool {
    for pre in prefixes {
        if file == pre || file.starts_with(&format!("{}/", pre)) {
            return true;
        }
    }
    false
}

#[allow(clippy::too_many_lines)]
fn compute_findings(result: &AnalysisResult, graph: &Graph) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut dead_files: HashSet<String> = HashSet::new();

    // ---- A. Dead files ----
    for file in &result.files {
        if is_test_file(file) {
            continue; // test files are their own root set
        }
        if result.entries.production.contains(file) {
            continue;
        }
        if result.entries.package_entries.contains(file) {
            continue;
        }
        let (prod_reach, test_reach) = result.reachability.file_status(file);
        if prod_reach {
            continue;
        }
        let prod_importers = result.file_importers_prod.get(file).copied().unwrap_or(0);
        if prod_importers > 0 {
            continue;
        }
        // Ambient globals used from other files make the file load-bearing.
        if result.files_with_used_globals.contains(file) {
            continue;
        }
        // Executed via package.json scripts: tooling entry point, not dead.
        if result.script_refs.contains(file) {
            findings.push(mk_finding(
                file, None, FindingKind::DeadFile, 1, 1, 0, 0,
                Confidence::Low, false, test_reach,
                vec!["referenced by package.json scripts; executed by tooling".to_string()],
            ));
            continue;
        }
        // Referenced by CI workflows: executed by automation, not dead.
        if result.workflow_refs.contains(file) {
            findings.push(mk_finding(
                file, None, FindingKind::DeadFile, 1, 1, 0, 0,
                Confidence::Low, false, test_reach,
                vec!["referenced by CI workflow files; executed by automation".to_string()],
            ));
            continue;
        }
        // Executable scripts are run manually (CLI), never imported.
        if result.executable_scripts.contains(file) {
            findings.push(mk_finding(
                file, None, FindingKind::DeadFile, 1, 1, 0, 0,
                Confidence::Low, false, test_reach,
                vec!["executable script (process.argv/shebang); run manually, not imported".to_string()],
            ));
            continue;
        }
        // Script-like files (no exports + top-level calls) may be run directly.
        let file_has_exports = result.file_exported_names.get(file).map(|s| !s.is_empty()).unwrap_or(false);
        if !file_has_exports && result.script_like_files.contains(file) {
            findings.push(mk_finding(
                file, None, FindingKind::DeadFile, 1, 1, 0, 0,
                Confidence::Low, false, test_reach,
                vec!["no exports and top-level side-effect calls; script-like file that may be run directly".to_string()],
            ));
            continue;
        }
        // Agent-tooling directories are loaded by the agent runtime by convention.
        if entrypoints::is_agent_tooling_dir(file) {
            findings.push(mk_finding(
                file, None, FindingKind::DeadFile, 1, 1, 0, 0,
                Confidence::Medium, false, test_reach,
                vec!["inside an agent-tooling directory; loaded by convention, not imports".to_string()],
            ));
            continue;
        }
        let dynamic_here = result.file_dynamic.get(file).map(|v| !v.is_empty()).unwrap_or(false);
        let protected = is_protected_by_dynamic_prefix(&result.dynamic_protected_prefixes, file);
        let config_refs = entrypoints::config_references(&result.root, file);
        // Test-runner magic dirs (Jest `__mocks__/`) are loaded by the runner,
        // never by imports: review only.
        if entrypoints::is_test_framework_magic(file) {
            findings.push(mk_finding(
                file, None, FindingKind::DeadFile, 1, 1, 0, 0,
                Confidence::Medium, false, test_reach,
                vec!["loaded by test-runner convention (__mocks__/); not referenced by imports".to_string()],
            ));
            continue;
        }
        // Tooling configs are loaded by external tools (babel/jest/karma/...),
        // never by imports: report for review, never auto-remove.
        if entrypoints::is_tooling_config(file) {
            findings.push(mk_finding(
                file, None, FindingKind::DeadFile, 1, 1, 0, 0,
                Confidence::Medium, false, test_reach,
                vec!["tooling configuration file; loaded by external tooling rather than imports".to_string()],
            ));
            continue;
        }
        // Library without an `exports` map: consumers may deep-import any file
        // that exports symbols, so such files can never be proven dead by imports.
        let has_exports = result.file_exported_names.get(file).map(|s| !s.is_empty()).unwrap_or(false);
        if has_exports && result.package_kind == entrypoints::PackageKind::LibraryUnknownSurface {
            findings.push(mk_finding(
                file, None, FindingKind::DeadFile, 1, 1, 0, 0,
                Confidence::Medium, false, test_reach,
                vec!["file exports symbols in a consumable package without an exports map; external subpath imports possible".to_string()],
            ));
            continue;
        }
        if !config_refs.is_empty() {
            findings.push(mk_finding(
                file, None, FindingKind::DeadFile, 1, 1, 0, 0,
                Confidence::Low, false, test_reach,
                vec![format!("referenced by configuration ({})", config_refs.join(", "))],
            ));
            continue;
        }
        // Package public surface check (in case entries missed).
        if result.entries.package_export_files.contains(file) {
            findings.push(mk_finding(
                file, None, FindingKind::DeadFile, 1, 1, 0, 0,
                Confidence::Low, false, test_reach,
                vec!["part of package public export surface".to_string()],
            ));
            continue;
        }
        if dynamic_here || protected {
            findings.push(mk_finding(
                file, None, FindingKind::DeadFile, 1, 1, 0, 0,
                Confidence::Low, false, test_reach,
                vec!["unresolved dynamic usage nearby; static reachability unreliable".to_string()],
            ));
            continue;
        }
        // Determine confidence: zero total importers => CERTAIN, else HIGH.
        let total_importers: usize = graph.importers_of(file).len();
        let test_importers = result.file_importers_test.get(file).copied().unwrap_or(0);
        let mut reasons = vec![
            "0 production importers".to_string(),
            "not a package entrypoint".to_string(),
            "not configured as an entrypoint".to_string(),
            "no supported dynamic reference".to_string(),
        ];
        if test_reach || test_importers > 0 {
            reasons.push(format!("referenced by {} test file(s) only", test_importers));
        }
        let confidence = if total_importers == 0 && !test_reach {
            Confidence::Certain
        } else {
            Confidence::High
        };
        // Line range: whole file. Use 1..line_count.
        let line_count = count_lines_for_file(result, file);
        findings.push(mk_finding(
            file, None, FindingKind::DeadFile, 1, line_count, 0, 0,
            confidence, false, test_reach, reasons,
        ));
        if confidence.auto_removable() {
            dead_files.insert(file.clone());
        }
    }

    // ---- B-F. Dead symbols ----
    // Entity lookup for parent export status.
    let entity_by_id: HashMap<&str, &Entity> =
        result.entities.iter().map(|e| (e.id.as_str(), e)).collect();
    // Owner liveness: a reference to any descendant (method of a class,
    // member of a namespace/object) keeps the owner alive too — you cannot
    // delete `Service` while `Service.create()` is called.
    let mut children_of: HashMap<&str, Vec<&str>> = HashMap::new();
    for e in &result.entities {
        if let Some(p) = &e.parent {
            children_of.entry(p.as_str()).or_default().push(e.id.as_str());
        }
    }
    let effective_callers = |id: &str| -> HashSet<String> {
        // Transitive descendants...
        let mut subtree: HashSet<&str> = HashSet::new();
        let mut stack = vec![id];
        while let Some(cur) = stack.pop() {
            if !subtree.insert(cur) {
                continue;
            }
            if let Some(kids) = children_of.get(cur) {
                for k in kids {
                    stack.push(k);
                }
            }
        }
        // ...whose callers come from outside the subtree — with one exception:
        // a VARIABLE read inside its own initializer (`const x = f(() => x)`)
        // captures the binding, so that self-edge counts as usage. (For
        // functions/classes/methods, self-edges are mere recursion and need
        // an external caller.)
        let counts_self = matches!(
            graph.kinds.get(id),
            Some(SymbolKind::Variable)
                | Some(SymbolKind::Interface)
                | Some(SymbolKind::TypeAlias)
                | Some(SymbolKind::Enum)
                | Some(SymbolKind::Namespace)
        );
        let mut out = HashSet::new();
        for member in &subtree {
            for c in graph.callers_of(member) {
                if !subtree.contains(c.as_str()) || (counts_self && *member == id && c == id) {
                    out.insert(c);
                }
            }
        }
        out
    };

    // Heritage components (undirected) for override-aware sharing: a call to
    // `Base.moveNext()` may dispatch to `Derived.moveNext()` at runtime, so
    // same-name methods across a hierarchy share liveness.
    let mut comp_of: HashMap<&str, usize> = HashMap::new();
    {
        let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
        for (derived, bases) in &result.heritage_links {
            for base in bases {
                adj.entry(derived.as_str()).or_default().push(base.as_str());
                adj.entry(base.as_str()).or_default().push(derived.as_str());
            }
        }
        let mut ncomp = 0usize;
        for &node in adj.keys() {
            if comp_of.contains_key(node) {
                continue;
            }
            let mut stack = vec![node];
            while let Some(cur) = stack.pop() {
                if comp_of.contains_key(cur) {
                    continue;
                }
                comp_of.insert(cur, ncomp);
                if let Some(ns) = adj.get(cur) {
                    for n in ns {
                        stack.push(n);
                    }
                }
            }
            ncomp += 1;
        }
    }
    // Nearest enclosing class of an entity (if any).
    let enclosing_class = |e: &Entity| -> Option<String> {
        let mut cur = e.parent.clone();
        while let Some(pid) = cur {
            match entity_by_id.get(pid.as_str()) {
                Some(pe) if pe.kind == SymbolKind::Class => return Some(pid),
                Some(pe) => cur = pe.parent.clone(),
                None => return None,
            }
        }
        None
    };
    // Do same-name methods elsewhere in the hierarchy have callers?
    let heritage_group_has_callers = |class_id: &str, name: &str| -> bool {
        let Some(&comp) = comp_of.get(class_id) else {
            return false;
        };
        for (cid, &c) in &comp_of {
            if c != comp {
                continue;
            }
            let Some(idx) = cid.find("::") else {
                continue;
            };
            let (f, scope) = (&cid[..idx], *cid);
            if let Some(mid) = graph
                .scope_children
                .get(&(f.to_string(), scope.to_string()))
                .and_then(|m| m.get(name))
            {
                if graph.callers_of(mid).iter().any(|caller| caller != mid) {
                    return true;
                }
            }
        }
        false
    };

    for e in &result.entities {
        // Skip symbols inside removable dead files (file finding covers them).
        if dead_files.contains(&e.file) {
            continue;
        }
        // Never report constructors.
        if e.kind == SymbolKind::Method && (e.name == "constructor" || e.name.starts_with('#')) && e.name != "#private-unused" {
            if e.name == "constructor" {
                continue;
            }
            // `#private` fields: fall through to normal logic (they can be dead).
        }
        let (prod_reach, test_reach) = result.reachability.symbol_status(&e.id);
        if prod_reach {
            continue;
        }
        // External callers count (exclude self-recursion), aggregated over
        // descendants: using `Service.create()` keeps `Service` alive.
        let external: HashSet<String> = effective_callers(&e.id);
        let ref_count = external.len();
        if ref_count > 0 {
            // Referenced (even if only from tests or unreachable files) — decide:
            // - If all callers are in test files/unreachable and symbol is unexported... still referenced; skip symbol finding.
            //   Production-dead but test-used code is reported at most via file-level; symbol keeps no finding
            //   to avoid proposing test breakage at symbol granularity. (File-level HIGH covers the case.)
            continue;
        }
        // Override-aware sharing: same-name methods elsewhere in the class
        // hierarchy with callers keep this one alive (polymorphic dispatch).
        if e.kind == SymbolKind::Method || is_class_field_fn(e, &entity_by_id) {
            if let Some(class_id) = enclosing_class(e) {
                if heritage_group_has_callers(&class_id, &e.name) {
                    continue;
                }
            }
        }
        // Zero external references. Check test reachability: if test-reachable via re-export/import chain
        // without direct caller edge (e.g. barrel), treat as test-used -> skip.
        if test_reach {
            continue;
        }

        let file_has_dynamic = result.file_dynamic.get(&e.file).map(|v| !v.is_empty()).unwrap_or(false);
        let protected = is_protected_by_dynamic_prefix(&result.dynamic_protected_prefixes, &e.file);
        let is_entry_file = result.entries.production.contains(&e.file);
        let is_public_file = result.entries.package_export_files.contains(&e.file)
            || result.entries.package_entries.contains(&e.file);
        // Members of an exported namespace / ambient module augmentation are
        // reachable through that namespace by consumers (`ns.Member`).
        let namespace_surface = has_exported_namespace_ancestor(&e.parent, &entity_by_id);
        let effectively_exported = e.exported || namespace_surface;

        // Entry-file top-level exported symbols are public-within-app roots? They were BFS seeds,
        // hence prod_reach=true normally. If somehow unreachable (empty seeds?), be conservative.
        let kind = match e.kind {
            SymbolKind::Function | SymbolKind::FunctionExpression | SymbolKind::ArrowFunction => {
                FindingKind::DeadFunction
            }
            SymbolKind::Class => FindingKind::DeadClass,
            SymbolKind::Method => FindingKind::DeadMethod,
            SymbolKind::Variable => FindingKind::DeadVariable,
            SymbolKind::Enum | SymbolKind::Interface | SymbolKind::TypeAlias | SymbolKind::Namespace => {
                FindingKind::DeadType
            }
        };

        // Confidence gate.
        let confidence: Confidence;
        let mut reasons: Vec<String> = vec![
            "no references".to_string(),
            if effectively_exported {
                if namespace_surface && !e.exported {
                    "reachable through an exported namespace".to_string()
                } else {
                    "exported but no internal importers".to_string()
                }
            } else {
                "not exported".to_string()
            },
            "not reachable from entry points".to_string(),
        ];

        if file_has_dynamic || protected {
            confidence = Confidence::Low;
            reasons.push("unresolved dynamic usage nearby".to_string());
        } else if effectively_exported {
            if is_public_file {
                confidence = Confidence::Low;
                reasons.push("part of package public export surface".to_string());
            } else if is_entry_file {
                confidence = Confidence::Low;
                reasons.push("exported from an entrypoint file".to_string());
            } else if e.kind == SymbolKind::Method {
                // Methods aren't exported individually; parent check below handles.
                confidence = Confidence::Medium;
                reasons.push("external or dynamic use cannot be ruled out".to_string());
            } else {
                // Exported but not on package surface: suspicious but external use possible.
                confidence = Confidence::Medium;
                reasons.push("external or dynamic use cannot be ruled out".to_string());
            }
        } else if e.kind == SymbolKind::Method || is_class_field_fn(e, &entity_by_id) {
            // Check parent class export status: public class => method is public API.
            let parent_exported = e.parent.as_ref().and_then(|p| entity_parent_exported(p, &entity_by_id)).unwrap_or(false);
            // Instances may escape via `return new X()` / exported initializers...
            let parent_class = e.parent.as_deref().and_then(parent_class_name);
            let escaped = parent_class.as_deref().map(|cls| {
                result.escaped_classes.get(&e.file).map(|s| s.contains(cls)).unwrap_or(false)
            }).unwrap_or(false);
            // ...or the file publishes properties outward (plugin/prototype pattern).
            let publishes = result.publishes_members.contains(&e.file);
            // Methods passed as callbacks/hooks in call arguments...
            let is_hook = result.hook_methods.contains(&(e.file.clone(), e.name.clone(), e.start_line));
            // ...or invoked via computed dispatch (`handlers[key](...)`).
            let dispatched = e.parent.as_ref().map(|p| result.dispatch_owners.contains(p)).unwrap_or(false);
            // ...or part of an exported module surface object (`module.exports = { create() {} }`).
            let surfaced = result.surface_methods.contains(&(e.file.clone(), e.name.clone(), e.start_line));
            // ...or invoked through an unresolvable channel with a matching name
            // (`cursor.moveNext()` in this file or in files importing it:
            // polymorphism, dynamic keys, runtime protocols).
            let dangling_hit = result.dangling_calls.contains(&(e.file.clone(), e.name.clone()))
                || graph.importers_of(&e.file).iter().any(|im| {
                    result.dangling_calls.contains(&(im.clone(), e.name.clone()))
                });
            // ...or owned by an object spread into other objects (`{...handlers}`:
            // methods may be invoked through the copies).
            let spread_hit = e.parent.as_deref().and_then(|p| {
                parent_var_name(p).map(|v| result.spread_vars.contains(&(e.file.clone(), v)))
            }).unwrap_or(false);
            if is_hook {
                confidence = Confidence::Medium;
                reasons.push("method in an object literal passed as a call argument; may be invoked as a callback/hook".to_string());
            } else if surfaced {
                confidence = Confidence::Medium;
                reasons.push("method of an exported module surface object; may be called by consumers or frameworks".to_string());
            } else if dispatched {
                confidence = Confidence::Medium;
                reasons.push("owner object invoked via computed dispatch (obj[key]()); any method may run".to_string());
            } else if dangling_hit {
                confidence = Confidence::Medium;
                reasons.push("unresolved call sites with this method name exist; may dispatch here".to_string());
            } else if spread_hit {
                confidence = Confidence::Medium;
                reasons.push("owner object is spread into other objects; methods may be invoked through copies".to_string());
            } else if escaped || publishes {
                confidence = Confidence::Medium;
                reasons.push("instances or properties escape via exported scope (plugin/prototype pattern); external callers possible".to_string());
            } else if parent_exported || is_public_file {
                confidence = Confidence::Medium;
                reasons.push("enclosing class is exported; external callers possible".to_string());
            } else {
                confidence = Confidence::Certain;
                reasons.push("no dynamic usage detected".to_string());
            }
        } else {
            confidence = Confidence::Certain;
            reasons.push("no dynamic usage detected".to_string());
        }

        // Test-reachable symbols with zero direct callers (odd) — mark HIGH at most? Already skipped test_reach above.

        // Class with exported status but file dead already handled. Methods of dead classes skipped? Class itself
        // will be reported; methods inside a CERTAIN dead class would double-report. Suppress methods when parent
        // class is itself unreferenced & unexported (it'll get its own finding).
        if e.kind == SymbolKind::Method {
            if let Some(parent_id) = &e.parent {
                if let Some(parent) = entity_by_id.get(parent_id.as_str()) {
                    let parent_callers = graph.callers_of(&parent.id);
                    let parent_ext = parent_callers.iter().filter(|c| *c != &parent.id).count();
                    let (p_prod, _) = result.reachability.symbol_status(&parent.id);
                    if !parent.exported && parent_ext == 0 && !p_prod {
                        continue; // parent class finding covers it
                    }
                }
            }
        }

        findings.push(Finding {
            id: format!("{}:{}-{}", e.file, e.start_line, e.name),
            kind,
            file: e.file.clone(),
            symbol: Some(e.name.clone()),
            start_line: e.start_line,
            end_line: e.end_line,
            start_byte: e.start_byte,
            end_byte: e.end_byte,
            confidence,
            production_reachable: prod_reach,
            test_reachable: test_reach,
            reference_count: ref_count,
            reasons,
            git: None,
        });
    }

    // Consistency pass: a dead-file candidate imported by a KEPT non-test file
    // must be kept too — deleting it would break its importer. (Test-only
    // importers don't count: verification decides those, per the HIGH model.)
    let removable_files: HashSet<String> = findings
        .iter()
        .filter(|f| f.kind == FindingKind::DeadFile && f.confidence.auto_removable())
        .map(|f| f.file.clone())
        .collect();
    for f in findings.iter_mut() {
        if f.kind != FindingKind::DeadFile || !f.confidence.auto_removable() {
            continue;
        }
        let kept_importer = graph.importers_of(&f.file).iter().any(|im| {
            !removable_files.contains(im) && !is_test_file(im)
        });
        if kept_importer {
            f.confidence = Confidence::Medium;
            f.reasons.push(
                "imported by files that are kept; removing it would break them".to_string(),
            );
        }
    }

    findings
}

fn entity_parent_exported(parent_id: &str, by_id: &HashMap<&str, &Entity>) -> Option<bool> {
    by_id.get(parent_id).map(|e| e.exported)
}

/// Arrow/function-expression class fields (`onError = (h) => {...}`) behave
/// like methods for confidence purposes (public if the class is public).
fn is_class_field_fn(e: &Entity, by_id: &HashMap<&str, &Entity>) -> bool {
    if !matches!(e.kind, SymbolKind::FunctionExpression | SymbolKind::ArrowFunction) {
        return false;
    }
    match e.parent.as_deref().and_then(|p| by_id.get(p)) {
        Some(parent) => parent.kind == SymbolKind::Class,
        None => false,
    }
}

/// Variable name owning a method, from a parent id like `file::variable:opts`.
fn parent_var_name(parent_id: &str) -> Option<String> {
    for seg in parent_id.split("::") {
        if let Some(name) = seg.strip_prefix("variable:") {
            return Some(name.to_string());
        }
    }
    None
}

/// Class name owning a method, from a parent id like `file::class:Local`.
fn parent_class_name(parent_id: &str) -> Option<String> {    for seg in parent_id.split("::") {
        if let Some(name) = seg.strip_prefix("class:") {
            return Some(name.to_string());
        }
    }
    None
}

fn has_exported_namespace_ancestor(
    parent: &Option<String>,
    by_id: &HashMap<&str, &Entity>,
) -> bool {
    let mut cur = parent.clone();
    while let Some(pid) = cur {
        match by_id.get(pid.as_str()) {
            Some(e) => {
                if e.kind == SymbolKind::Namespace && e.exported {
                    return true;
                }
                cur = e.parent.clone();
            }
            None => return false,
        }
    }
    false
}

fn mk_finding(
    file: &str,
    symbol: Option<String>,
    kind: FindingKind,
    start_line: usize,
    end_line: usize,
    start_byte: usize,
    end_byte: usize,
    confidence: Confidence,
    prod: bool,
    test: bool,
    reasons: Vec<String>,
) -> Finding {
    Finding {
        id: match &symbol {
            Some(s) => format!("{}:{}-{}", file, start_line, s),
            None => format!("{}:file", file),
        },
        kind,
        file: file.to_string(),
        symbol,
        start_line,
        end_line,
        start_byte,
        end_byte,
        confidence,
        production_reachable: prod,
        test_reachable: test,
        reference_count: 0,
        reasons,
        git: None,
    }
}

fn count_lines_for_file(result: &AnalysisResult, file: &str) -> usize {
    // Best effort: max end_line among entities, else 1.
    let mut max = 0;
    for e in &result.entities {
        if e.file == file && e.end_line > max {
            max = e.end_line;
        }
    }
    if max == 0 {
        // Read file to count lines.
        let abs = result.root.join(file);
        if let Ok(text) = std::fs::read_to_string(&abs) {
            max = text.lines().count().max(1);
        } else {
            max = 1;
        }
    }
    max
}

fn attach_git_evidence(root: &Path, findings: &mut [Finding]) {
    // Collect unique files.
    let mut files: Vec<String> = findings.iter().map(|f| f.file.clone()).collect();
    files.sort();
    files.dedup();
    let mut cache: HashMap<String, GitEvidence> = HashMap::new();
    for f in files {
        cache.insert(f.clone(), git_evidence_for(root, &f));
    }
    for finding in findings.iter_mut() {
        if let Some(ev) = cache.get(&finding.file) {
            finding.git = Some(ev.clone());
        }
    }
}

fn git_evidence_for(root: &Path, rel: &str) -> GitEvidence {
    let empty = GitEvidence {
        first_seen: None,
        last_modified: None,
        last_author: None,
        commit_count: 0,
    };
    // `git log --follow --format=%H|%ad|%an --date=short -- <file>`
    let out = std::process::Command::new("git")
        .args(["log", "--follow", "--format=%H|%ad|%an", "--date=short", "--", rel])
        .current_dir(root)
        .output();
    let Ok(out) = out else {
        return empty;
    };
    if !out.status.success() {
        return empty;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return empty;
    }
    let commit_count = lines.len();
    // lines[0] = newest, lines[last] = oldest.
    let newest = lines[0].split('|').collect::<Vec<_>>();
    let oldest = lines[lines.len() - 1].split('|').collect::<Vec<_>>();
    GitEvidence {
        first_seen: oldest.get(1).map(|s| s.to_string()),
        last_modified: newest.get(1).map(|s| s.to_string()),
        last_author: newest.get(2).map(|s| s.to_string()),
        commit_count,
    }
}

fn build_cleanup_plan(result: &AnalysisResult) -> CleanupPlan {
    let mut delete_files = Vec::new();
    let mut remove_symbols = Vec::new();
    for f in &result.findings {
        if !f.confidence.auto_removable() {
            continue;
        }
        match f.kind {
            FindingKind::DeadFile => delete_files.push(f.file.clone()),
            _ => {
                remove_symbols.push(crate::edits::SymbolRemoval {
                    file: f.file.clone(),
                    symbol: f.symbol.clone().unwrap_or_default(),
                    start_line: f.start_line,
                    end_line: f.end_line,
                    start_byte: f.start_byte,
                    end_byte: f.end_byte,
                });
            }
        }
    }
    delete_files.sort();
    delete_files.dedup();
    // Drop symbol removals inside deleted files.
    let del: HashSet<&str> = delete_files.iter().map(|s| s.as_str()).collect();
    remove_symbols.retain(|s| !del.contains(s.file.as_str()));
    // Sort descending by byte so application order is safe.
    remove_symbols.sort_by(|a, b| a.file.cmp(&b.file).then(b.start_byte.cmp(&a.start_byte)));
    CleanupPlan { delete_files, remove_symbols }
}
