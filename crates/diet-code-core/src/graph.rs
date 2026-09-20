use std::collections::{HashMap, HashSet};

use crate::imports::{RefKind, ReferenceRec};
use crate::symbols::SymbolKind;

/// File-level + symbol-level graph.
#[derive(Debug, Default)]
pub struct Graph {
    /// file -> files it imports (resolved).
    pub file_edges: HashMap<String, HashSet<String>>,
    /// file -> files that import it.
    pub file_importers: HashMap<String, HashSet<String>>,
    /// symbol id -> symbols it references (resolved symbol ids).
    pub symbol_edges: HashMap<String, Vec<SymbolEdge>>,
    /// symbol id -> symbols that reference it.
    pub symbol_callers: HashMap<String, Vec<String>>,
    /// symbol name -> ids (for name-based resolution).
    pub symbols_by_name: HashMap<String, Vec<String>>,
    /// (file, name) -> symbol id for precise lookup (first declaration wins).
    pub symbol_by_file_name: HashMap<(String, String), String>,
    /// Lexical scope children: (file, scope) -> name -> id.
    /// Scope "" is file top level; otherwise the enclosing symbol id.
    pub scope_children: HashMap<(String, String), HashMap<String, String>>,
    /// Child symbol id -> enclosing scope ("" for file top level).
    pub parent_of: HashMap<String, String>,
    /// Symbol id -> kind (for class/variable scope distinctions).
    pub kinds: HashMap<String, SymbolKind>,
    /// method name -> method ids (for fallback resolution).
    pub methods_by_name: HashMap<String, Vec<String>>,
    /// file -> entity ids defined in it.
    pub file_symbols: HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct SymbolEdge {
    pub to: String,
    pub kind: RefKind,
}

impl Graph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_file_edge(&mut self, from: &str, to: &str) {
        self.file_edges
            .entry(from.to_string())
            .or_default()
            .insert(to.to_string());
        self.file_importers
            .entry(to.to_string())
            .or_default()
            .insert(from.to_string());
    }

    pub fn add_symbol_edge(&mut self, from: &str, to: &str, kind: RefKind) {
        self.symbol_edges
            .entry(from.to_string())
            .or_default()
            .push(SymbolEdge {
                to: to.to_string(),
                kind,
            });
        self.symbol_callers
            .entry(to.to_string())
            .or_default()
            .push(from.to_string());
    }

    pub fn importers_of(&self, file: &str) -> HashSet<String> {
        self.file_importers.get(file).cloned().unwrap_or_default()
    }

    pub fn callers_of(&self, symbol_id: &str) -> Vec<String> {
        self.symbol_callers
            .get(symbol_id)
            .cloned()
            .unwrap_or_default()
    }
}

/// Registry of local import bindings per file:
/// file -> local_name -> (resolved_file, original_name, is_type_only).
pub type BindingMap = HashMap<String, HashMap<String, (Option<String>, String, bool)>>;
/// file -> local_name -> (resolved_file, original_name, is_type)
pub fn build_binding_maps(
    imports: &[crate::imports::ImportRec],
) -> BindingMap {
    let mut m: BindingMap = HashMap::new();
    for imp in imports {
        if imp.kind == crate::imports::ImportKind::SideEffect {
            continue;
        }
        // Skip pseudo bindings that don't introduce a usable name.
        if imp.local_name == "*require*"
            || imp.local_name == "*dynamic*"
            || imp.local_name == "*side-effect*"
        {
            continue;
        }
        // `const Cursor = require("./cursor")` binds the whole module under the
        // local name: resolve the local name in the target (like a default import).
        let original =
            if imp.kind == crate::imports::ImportKind::Require && imp.original_name == "*" {
                imp.local_name.clone()
            } else {
                imp.original_name.clone()
            };
        m.entry(imp.from_file.clone()).or_default().insert(
            imp.local_name.clone(),
            (imp.resolved_file.clone(), original, imp.is_type_only),
        );
    }
    m
}

/// Resolve references to symbol ids and populate graph edges.
/// `reexport_targets`: (file, exported_name) -> Vec<(target_file, target_original)> after barrel expansion.
#[allow(clippy::too_many_arguments)]
pub fn resolve_references(
    graph: &mut Graph,
    references: &[ReferenceRec],
    bindings: &BindingMap,
    reexport_targets: &HashMap<(String, String), Vec<(String, String)>>,
    namespace_imports: &HashMap<String, HashMap<String, String>>,
    _file_exports: &HashMap<String, HashSet<String>>,
    // (file, variable) -> class name, for `recv.method()` receiver resolution.
    var_types: &HashMap<(String, String), String>,
    // Ambient-global type names (from import/export-less script files) -> symbol id.
    global_types: &HashMap<String, String>,
    // Derived class id -> base class ids (resolved heritage).
    heritage: &HashMap<String, Vec<String>>,
    // (file, name) of invocation-shaped references (call/new/jsx/member) that
    // resolved to nothing: something invokes this name via a channel we can't
    // see (untyped receivers, dynamic keys, polymorphism, runtime protocols).
    dangling: &mut HashSet<(String, String)>,
) {
    for r in references {
        match r.context {
            crate::imports::RefContext::ThisMethod => {
                // Resolve `this.foo` through the scope `this` binds to.
                let mut linked = false;
                if let Some(from_sym) = &r.from_symbol {
                    if let Some((file, owner)) = this_scope(graph, from_sym) {
                        if file == r.from_file {
                            if let Some(id) = child_in_scope(graph, &file, &owner, &r.name) {
                                graph.add_symbol_edge(from_sym, &id, RefKind::Value);
                                linked = true;
                            }
                        }
                    }
                }
                if !linked {
                    dangling.insert((r.from_file.clone(), r.name.clone()));
                }
            }
            crate::imports::RefContext::Jsx
            | crate::imports::RefContext::Call
            | crate::imports::RefContext::New
            | crate::imports::RefContext::Identifier
            | crate::imports::RefContext::TypePosition => {
                let edge_kind = r.kind;
                // 1b. Lexically nested declarations shadow imports: an inner
                // `const schemas` wins over `import * as schemas` in that scope.
                // (Top-level conflicts would be a redeclaration SyntaxError, so
                // file-top scope is checked AFTER bindings, in step 3.)
                if let Some(id) = lookup_scoped_nested(graph, &r.from_file, &r.from_symbol, &r.name)
                {
                    let from = r
                        .from_symbol
                        .clone()
                        .unwrap_or_else(|| file_pseudo(&r.from_file));
                    graph.add_symbol_edge(&from, &id, edge_kind);
                    continue;
                }
                // 1c. Check import bindings for this file.
                if let Some(file_bindings) = bindings.get(&r.from_file) {
                    if let Some((resolved_file_opt, original, _is_type)) =
                        file_bindings.get(&r.name)
                    {
                        if let Some(resolved_file) = resolved_file_opt {
                            // Follow re-export chains: (resolved_file, original) -> final targets.
                            let targets =
                                follow_reexport(resolved_file, original, reexport_targets);
                            let targets = if targets.is_empty() {
                                vec![(resolved_file.clone(), original.clone())]
                            } else {
                                targets
                            };
                            for (tf, orig) in targets {
                                let target_name = if orig == "default" {
                                    // default import: match symbol with is_default_export in target file.
                                    // Handled via file+default lookup below.
                                    "default".to_string()
                                } else if orig == "*" {
                                    continue;
                                } else {
                                    orig.clone()
                                };
                                if let Some(id) = lookup_symbol(graph, &tf, &target_name) {
                                    let from = r
                                        .from_symbol
                                        .clone()
                                        .unwrap_or_else(|| file_pseudo(&r.from_file));
                                    graph.add_symbol_edge(&from, &id, edge_kind);
                                }
                            }
                            continue;
                        } else {
                            // Unresolved import (external package) — skip.
                            continue;
                        }
                    }
                }
                // 2. Namespace member: `ns.Foo` where base is namespace import.
                if let Some(base) = &r.base {
                    if base != "this" {
                        if let Some(ns_map) = namespace_imports.get(&r.from_file) {
                            if let Some(ns_file) = ns_map.get(base) {
                                let targets = follow_reexport(ns_file, &r.name, reexport_targets);
                                let targets = if targets.is_empty() {
                                    vec![(ns_file.clone(), r.name.clone())]
                                } else {
                                    targets
                                };
                                let mut done = false;
                                for (tf, orig) in targets {
                                    if let Some(id) = lookup_symbol(graph, &tf, &orig) {
                                        let from = r
                                            .from_symbol
                                            .clone()
                                            .unwrap_or_else(|| file_pseudo(&r.from_file));
                                        graph.add_symbol_edge(&from, &id, edge_kind);
                                        done = true;
                                    }
                                }
                                if done {
                                    continue;
                                }
                            }
                        }
                    }
                }
                // 3. Same-file symbol via lexical scope (nearest enclosing declaration wins,
                //    so shadowed names resolve correctly).
                if let Some(id) = lookup_scoped(graph, &r.from_file, &r.from_symbol, &r.name) {
                    let from = r
                        .from_symbol
                        .clone()
                        .unwrap_or_else(|| file_pseudo(&r.from_file));
                    if from != id {
                        graph.add_symbol_edge(&from, &id, edge_kind);
                    } else {
                        // recursive self-call still counts as a reference for "zero references" purposes?
                        // For finding reference counts we count callers excluding self? Keep edge but callers logic handles.
                        graph.add_symbol_edge(&from, &id, edge_kind);
                    }
                    continue;
                }
                // 4. Ambient-global types: `.d.ts`/script files without imports or
                //    exports declare globals (`declare interface ILocale`) visible
                //    across files. Only for TYPE references, never values.
                if r.kind == RefKind::Type {
                    if let Some(id) = global_types.get(&r.name) {
                        let from = r
                            .from_symbol
                            .clone()
                            .unwrap_or_else(|| file_pseudo(&r.from_file));
                        if from != *id {
                            graph.add_symbol_edge(&from, id, RefKind::Type);
                        }
                        continue;
                    }
                }
                // 5. Member refs (`obj.method`) where receiver unresolvable: try method-name match
                //    only if unambiguous single candidate across repo? Too risky — skip (per spec: only when safe).
                //    For `new Foo()` / `Foo()` where Foo defined in another file but not imported — do NOT link.
            }
            crate::imports::RefContext::Member => {
                // `obj.method()` with base.
                // If base == "this" handled above (ThisMethod). If base is namespace import, resolve.
                let mut linked = false;
                if let Some(base) = &r.base {
                    if base == "this" {
                        // Same as ThisMethod (member call form).
                        if let Some(from_sym) = &r.from_symbol {
                            if let Some((file, owner)) = this_scope(graph, from_sym) {
                                if file == r.from_file {
                                    if let Some(id) = child_in_scope(graph, &file, &owner, &r.name)
                                    {
                                        graph.add_symbol_edge(from_sym, &id, RefKind::Value);
                                        linked = true;
                                    }
                                }
                            }
                        }
                    } else if base == "super" {
                        // `super.foo()` dispatches to the parent class scope.
                        if let Some(from_sym) = &r.from_symbol {
                            if let Some((file, owner)) = super_scope(graph, heritage, from_sym) {
                                if file == r.from_file {
                                    if let Some(id) = child_in_scope(graph, &file, &owner, &r.name)
                                    {
                                        graph.add_symbol_edge(from_sym, &id, RefKind::Value);
                                        linked = true;
                                    }
                                }
                            }
                        }
                    } else {
                        if let Some(ns_map) = namespace_imports.get(&r.from_file) {
                            if let Some(ns_file) = ns_map.get(base) {
                                let targets = follow_reexport(ns_file, &r.name, reexport_targets);
                                let targets = if targets.is_empty() {
                                    vec![(ns_file.clone(), r.name.clone())]
                                } else {
                                    targets
                                };
                                for (tf, orig) in targets {
                                    if let Some(id) = lookup_symbol(graph, &tf, &orig) {
                                        let from = r
                                            .from_symbol
                                            .clone()
                                            .unwrap_or_else(|| file_pseudo(&r.from_file));
                                        graph.add_symbol_edge(&from, &id, RefKind::Value);
                                        linked = true;
                                    }
                                }
                            }
                        }
                    }
                    // Receiver-based method resolution:
                    // `s.method()` where `s: Class` (via `new Class()` / `: Class`),
                    // `new Class().method()`, `Class.static()`, or `owner.method()`
                    // where owner is a same-file object holding that method.
                    if !linked {
                        if let Some(id) = resolve_method_receiver(
                            graph,
                            bindings,
                            var_types,
                            global_types,
                            &r.from_file,
                            &r.from_symbol,
                            base,
                            &r.name,
                        ) {
                            let from = r
                                .from_symbol
                                .clone()
                                .unwrap_or_else(|| file_pseudo(&r.from_file));
                            graph.add_symbol_edge(&from, &id, RefKind::Value);
                            linked = true;
                        }
                    }
                    // Unresolvable receiver — do not link (avoid false edges),
                    // but record the invocation: something calls `.name` through
                    // a channel we can't see (polymorphism, dynamic keys).
                    if !linked {
                        dangling.insert((r.from_file.clone(), r.name.clone()));
                    }
                }
            }
            crate::imports::RefContext::ReExport | crate::imports::RefContext::Import => {}
        }
    }
}

fn file_pseudo(file: &str) -> String {
    format!("{}::__file__", file)
}

/// All same-file entity ids sharing a name (declaration merging, overloads).
fn owner_ids_in_file(graph: &Graph, file: &str, name: &str) -> Vec<String> {
    graph
        .symbols_by_name
        .get(name)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|id| id.starts_with(&format!("{}::", file)))
        .collect()
}

/// Child declaration `name` directly inside `scope` of `file`.
fn child_in_scope(graph: &Graph, file: &str, scope: &str, name: &str) -> Option<String> {
    graph
        .scope_children
        .get(&(file.to_string(), scope.to_string()))
        .and_then(|m| m.get(name))
        .cloned()
}

/// Lexical-scope lookup restricted to NESTED scopes (excludes file top level),
/// for import shadowing: inner declarations win over module-scope bindings.
pub fn lookup_scoped_nested(
    graph: &Graph,
    file: &str,
    from_symbol: &Option<String>,
    name: &str,
) -> Option<String> {
    let mut cur = from_symbol.clone().unwrap_or_default();
    if cur.ends_with("::__file__") || cur.is_empty() {
        return None; // top-level statements live at file scope (checked later)
    }
    loop {
        // Class scopes are skipped: methods are not lexically visible.
        if graph.kinds.get(&cur) != Some(&SymbolKind::Class) {
            if let Some(m) = graph.scope_children.get(&(file.to_string(), cur.clone())) {
                if let Some(id) = m.get(name) {
                    return Some(id.clone());
                }
            }
        }
        match graph.parent_of.get(&cur) {
            Some(p) if !p.is_empty() => cur = p.clone(),
            _ => return None,
        }
    }
}

/// Lexical-scope-aware same-file lookup: walk from the referring symbol up
/// through enclosing scopes to file top level; the nearest declaration wins.
/// This keeps shadowed names (loop counters, same-name locals in different
/// functions) resolving to the correct declaration instead of the first match.
pub fn lookup_scoped(
    graph: &Graph,
    file: &str,
    from_symbol: &Option<String>,
    name: &str,
) -> Option<String> {
    let mut scopes: Vec<String> = Vec::new();
    let mut cur = from_symbol.clone().unwrap_or_default();
    if cur.ends_with("::__file__") {
        cur.clear(); // top-level statements live at file scope
    }
    loop {
        // Class scopes are skipped (see lookup_scoped_nested).
        if cur.is_empty() || graph.kinds.get(&cur) != Some(&SymbolKind::Class) {
            scopes.push(cur.clone());
        }
        if cur.is_empty() {
            break;
        }
        match graph.parent_of.get(&cur) {
            Some(p) => cur = p.clone(),
            None => {
                scopes.push(String::new());
                break;
            }
        }
    }
    for scope in scopes {
        if let Some(m) = graph.scope_children.get(&(file.to_string(), scope)) {
            if let Some(id) = m.get(name) {
                return Some(id.clone());
            }
        }
    }
    None
}

/// Determine what `super` binds to at `from_symbol`: the parent class scope of
/// the nearest enclosing class. Returns (file, base class scope id).
pub fn super_scope(
    graph: &Graph,
    heritage: &HashMap<String, Vec<String>>,
    from_symbol: &str,
) -> Option<(String, String)> {
    let file = from_symbol.split("::").next()?.to_string();
    let mut cur = from_symbol.to_string();
    loop {
        if graph.kinds.get(&cur) == Some(&SymbolKind::Class) {
            let bases = heritage.get(&cur)?;
            let base = bases.first()?;
            let base_file = base.split("::").next().unwrap_or(&file);
            return Some((base_file.to_string(), base.clone()));
        }
        let parent = graph.parent_of.get(&cur).cloned().unwrap_or_default();
        if parent.is_empty() {
            return None;
        }
        cur = parent;
    }
}

/// Determine what `this` binds to at `from_symbol`: the nearest enclosing
/// method's owner (class / object-literal variable / namespace), walking
/// outward through arrow functions (which inherit `this`). Plain nested
/// functions get their own `this` (unresolvable statically) → None.
/// Returns (file, owner scope id).
fn this_scope(graph: &Graph, from_symbol: &str) -> Option<(String, String)> {
    let file = from_symbol.split("::").next()?.to_string();
    let mut cur = from_symbol.to_string();
    loop {
        match graph.kinds.get(&cur) {
            Some(SymbolKind::Method) => {
                let owner = graph.parent_of.get(&cur).cloned().unwrap_or_default();
                if owner.is_empty() {
                    return None;
                }
                return Some((file, owner));
            }
            // `this` directly in class scope (static blocks, field initializers).
            Some(SymbolKind::Class) => {
                return Some((file, cur));
            }
            // Arrow functions and variable initializers inherit the enclosing
            // `this` (`const x = this.foo()` inside a method) — walk outward.
            Some(SymbolKind::ArrowFunction) | Some(SymbolKind::Variable) => {}
            // Plain functions rebind `this`; namespaces/modules don't bind it —
            // statically unresolvable.
            _ => return None,
        }
        let parent = graph.parent_of.get(&cur).cloned().unwrap_or_default();
        if parent.is_empty() {
            return None; // module top level: `this` is the module/global object
        }
        cur = parent;
    }
}

/// Resolve `base.method` where base is a `new Class()` expression, a variable of
/// known class type, a class name visible in the file (static calls), or a
/// same-file variable owning an object literal with that method.
/// Conservative: links only when the (owner, method) pair resolves in scope.
#[allow(clippy::too_many_arguments)]
fn resolve_method_receiver(
    graph: &Graph,
    bindings: &BindingMap,
    var_types: &HashMap<(String, String), String>,
    global_types: &HashMap<String, String>,
    file: &str,
    from_symbol: &Option<String>,
    base: &str,
    method: &str,
) -> Option<String> {
    let base = base.trim();
    // `new Service().used()`.
    if let Some(rest) = base.strip_prefix("new ") {
        let cls = rest
            .trim()
            .trim_end_matches(['(', ')'])
            .split('(')
            .next()
            .unwrap_or("")
            .split('.')
            .next()
            .unwrap_or("")
            .trim();
        if cls.is_empty() {
            return None;
        }
        return find_class_method(graph, bindings, file, cls, method);
    }
    // Variable with tracked class type: `const s = new Service(); s.used()`.
    if let Some(cls) = var_types.get(&(file.to_string(), base.to_string())) {
        return find_class_method(graph, bindings, file, cls, method);
    }
    // Same-file visible owner: `const translator = { fmt() {} }; translator.fmt()`.
    // Declaration merging means several same-file entities can share the base
    // name (overloads, function+namespace) — try each owner's children.
    {
        let mut candidates: Vec<String> = Vec::new();
        if let Some(owner_id) = lookup_scoped(graph, file, from_symbol, base) {
            candidates.push(owner_id);
        }
        for oid in owner_ids_in_file(graph, file, base) {
            if !candidates.contains(&oid) {
                candidates.push(oid);
            }
        }
        for oid in candidates {
            if let Some(id) = child_in_scope(graph, file, &oid, method) {
                return Some(id);
            }
        }
    }
    // Imported class name used statically: `import { Service } from ...; Service.create()`.
    if base.chars().next().is_some_and(|c| c.is_uppercase()) {
        if let Some(fb) = bindings.get(file) {
            if let Some((Some(target), orig, _)) = fb.get(base) {
                let class_name = if orig == "default" {
                    "default"
                } else {
                    orig.as_str()
                };
                if let Some(class_id) = graph
                    .symbol_by_file_name
                    .get(&(target.clone(), class_name.to_string()))
                    .cloned()
                {
                    if graph.kinds.get(&class_id) == Some(&SymbolKind::Class) {
                        if let Some(id) = child_in_scope(graph, target, &class_id, method) {
                            return Some(id);
                        }
                    }
                }
            }
        }
        // Same-file class referenced by name without import (e.g. test file defines both).
        if let Some(class_id) = graph
            .symbol_by_file_name
            .get(&(file.to_string(), base.to_string()))
            .cloned()
        {
            if graph.kinds.get(&class_id) == Some(&SymbolKind::Class) {
                if let Some(id) = child_in_scope(graph, file, &class_id, method) {
                    return Some(id);
                }
            }
        }
    }
    // Ambient-global namespaces (`Deno.serve`, UMD globals): members resolve
    // through the declaring file's scope.
    if let Some(ns_id) = global_types.get(base) {
        if let Some(idx) = ns_id.find("::") {
            let ns_file = &ns_id[..idx];
            if let Some(id) = child_in_scope(graph, ns_file, ns_id, method) {
                return Some(id);
            }
        }
    }
    None
}

fn find_class_method(
    graph: &Graph,
    bindings: &BindingMap,
    file: &str,
    class: &str,
    method: &str,
) -> Option<String> {
    // Prefer the same file.
    if let Some(class_id) = graph
        .symbol_by_file_name
        .get(&(file.to_string(), class.to_string()))
        .cloned()
    {
        if graph.kinds.get(&class_id) == Some(&SymbolKind::Class) {
            if let Some(id) = child_in_scope(graph, file, &class_id, method) {
                return Some(id);
            }
        }
    }
    // Then files this file imports (the class is usually imported).
    if let Some(fb) = bindings.get(file) {
        let mut targets: Vec<String> = fb.values().filter_map(|(t, _, _)| t.clone()).collect();
        targets.sort();
        targets.dedup();
        let mut hits = Vec::new();
        for t in targets {
            if let Some(class_id) = graph
                .symbol_by_file_name
                .get(&(t.clone(), class.to_string()))
                .cloned()
            {
                if graph.kinds.get(&class_id) == Some(&SymbolKind::Class) {
                    if let Some(id) = child_in_scope(graph, &t, &class_id, method) {
                        hits.push(id);
                    }
                }
            }
        }
        if hits.len() == 1 {
            return hits.pop();
        }
        if !hits.is_empty() {
            return None; // ambiguous
        }
    }
    // Unique across the whole repo — safe.
    let cands = graph.methods_by_name.get(method)?.clone();
    let marker = format!("::class:{}::", class);
    let scoped: Vec<&String> = cands.iter().filter(|id| id.contains(&marker)).collect();
    if scoped.len() == 1 {
        return Some(scoped[0].clone());
    }
    None
}

fn lookup_symbol(graph: &Graph, file: &str, name: &str) -> Option<String> {
    if name == "default" {
        // Any symbol in file marked default? We stored name "default" for anonymous defaults.
        // Also named symbols with is_default_export aren't distinguishable by id; try name "default" first,
        // then fall back to nothing (conservative: link to file pseudo so file stays reachable).
        if let Some(id) = graph
            .symbol_by_file_name
            .get(&(file.to_string(), "default".to_string()))
        {
            return Some(id.clone());
        }
        return None;
    }
    graph
        .symbol_by_file_name
        .get(&(file.to_string(), name.to_string()))
        .cloned()
}

fn follow_reexport(
    file: &str,
    name: &str,
    map: &HashMap<(String, String), Vec<(String, String)>>,
) -> Vec<(String, String)> {
    // BFS through re-export map, expanding wildcards? Wildcards need file export lists — handled by caller via expansion beforehand.
    let mut out = Vec::new();
    let mut stack = vec![(file.to_string(), name.to_string())];
    let mut seen = HashSet::new();
    while let Some((f, n)) = stack.pop() {
        if !seen.insert((f.clone(), n.clone())) {
            continue;
        }
        if let Some(targets) = map.get(&(f.clone(), n.clone())) {
            for (tf, tn) in targets {
                if tn == "*" {
                    continue;
                }
                stack.push((tf.clone(), tn.clone()));
            }
        } else {
            // Terminal: if (f,n) is the original query, that's "no re-export" — return empty to signal direct.
            if f == file && n == name {
                return Vec::new();
            }
            out.push((f, n));
        }
    }
    out
}
