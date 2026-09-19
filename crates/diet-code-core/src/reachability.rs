use std::collections::{HashMap, HashSet, VecDeque};

use crate::graph::Graph;

#[derive(Debug, Clone, Default)]
pub struct Reachability {
    pub prod_files: HashSet<String>,
    pub test_files: HashSet<String>,
    pub prod_symbols: HashSet<String>,
    pub test_symbols: HashSet<String>,
}

impl Reachability {
    pub fn file_status(&self, file: &str) -> (bool, bool) {
        (
            self.prod_files.contains(file),
            self.test_files.contains(file),
        )
    }

    pub fn symbol_status(&self, id: &str) -> (bool, bool) {
        (
            self.prod_symbols.contains(id),
            self.test_symbols.contains(id),
        )
    }
}

pub fn compute_reachability(
    graph: &Graph,
    file_to_symbols: &HashMap<String, Vec<String>>,
    prod_entries: &HashSet<String>,
    test_entries: &HashSet<String>,
    package_public_files: &HashSet<String>,
    script_refs: &HashSet<String>,
    all_files: &HashSet<String>,
) -> Reachability {
    let prod_files = bfs_files_union(graph, prod_entries, package_public_files, script_refs);
    let test_files = bfs_files(graph, test_entries);

    // Entry symbols: all symbols defined in entry files.
    let mut prod_seeds: HashSet<String> = HashSet::new();
    for f in prod_entries {
        if let Some(syms) = file_to_symbols.get(f) {
            for s in syms {
                prod_seeds.insert(s.clone());
            }
        }
        // Top-level statements of entry files (e.g. `outer()` with no local
        // declarations) reference symbols via the file pseudo-node — seed it.
        prod_seeds.insert(format!("{}::__file__", f));
        // Entry file itself reachable even with zero symbols.
    }
    // Script-referenced files execute standalone: their top-level statements run.
    for f in script_refs {
        prod_seeds.insert(format!("{}::__file__", f));
    }
    // Package public surface: exported symbols of public files are roots for symbol reachability?
    // NO — for app analysis they'd pollute. Instead treat them as roots only for marking
    // "publicly reachable" conservatively in findings (not as BFS seeds that would mark everything reachable).
    // So we do NOT seed BFS from public files, except: default-exported symbols of public files?
    // Decision: do not seed; findings logic checks package_public_files separately.

    let mut test_seeds: HashSet<String> = HashSet::new();
    for f in test_entries {
        if let Some(syms) = file_to_symbols.get(f) {
            for s in syms {
                test_seeds.insert(s.clone());
            }
        }
        test_seeds.insert(format!("{}::__file__", f));
    }

    let prod_symbols = bfs_symbols(graph, &prod_seeds, &prod_files, file_to_symbols);
    let test_symbols = bfs_symbols(graph, &test_seeds, &test_files, file_to_symbols);

    // Files with no symbols that are entries count as reachable (already in prod_files).
    // Symbols in unreachable files can never be reachable — prune.
    let _ = all_files;

    Reachability {
        prod_files,
        test_files,
        prod_symbols,
        test_symbols,
    }
}

/// File BFS from production entries UNION package public-surface entries
/// UNION script-referenced files (executed by tooling: their import closure runs).
/// A file imported by consumers (library subpath) is live as a file, even
/// though its exported-but-internally-unused symbols stay suspicious
/// (symbol seeds come from production entries only).
fn bfs_files_union(
    graph: &Graph,
    prod_entries: &HashSet<String>,
    public_entries: &HashSet<String>,
    script_entries: &HashSet<String>,
) -> HashSet<String> {
    let mut both = prod_entries.clone();
    for f in public_entries {
        both.insert(f.clone());
    }
    for f in script_entries {
        both.insert(f.clone());
    }
    bfs_files(graph, &both)
}

fn bfs_files(graph: &Graph, seeds: &HashSet<String>) -> HashSet<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    for s in seeds {
        if seen.insert(s.clone()) {
            queue.push_back(s.clone());
        }
    }
    while let Some(cur) = queue.pop_front() {
        if let Some(nexts) = graph.file_edges.get(&cur) {
            for n in nexts {
                if seen.insert(n.clone()) {
                    queue.push_back(n.clone());
                }
            }
        }
    }
    seen
}

fn bfs_symbols(
    graph: &Graph,
    seeds: &HashSet<String>,
    reachable_files: &HashSet<String>,
    file_to_symbols: &HashMap<String, Vec<String>>,
) -> HashSet<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    for s in seeds {
        if seen.insert(s.clone()) {
            queue.push_back(s.clone());
        }
    }
    // Also seed file-pseudo nodes for entry files so top-level refs resolve:
    // file pseudo `f::__file__` edges created during resolution (from_symbol fallback).
    let _ = reachable_files;
    let _ = file_to_symbols;
    while let Some(cur) = queue.pop_front() {
        // Follow symbol edges
        if let Some(nexts) = graph.symbol_edges.get(&cur) {
            for e in nexts {
                if seen.insert(e.to.clone()) {
                    queue.push_back(e.to.clone());
                }
            }
        }
        // If cur is a file pseudo, also traverse? Pseudos only have outgoing edges; incoming from nobody.
        // If cur is a symbol, its file is implicitly reachable — nothing more needed.
    }
    // Additionally: any symbol whose file-pseudo has an edge to it from a reachable symbol is covered.
    // Top-level statements in entry files reference symbols via pseudo `entry::__file__` — but pseudos
    // are never seeded (from_symbol fallback creates pseudo as SOURCE, not target). Instead, top-level
    // refs have source = pseudo of the referring file. For the BFS to traverse file A's top-level ref
    // `A::__file__ -> S`, we need pseudo A to be in the visited set whenever A is reachable.
    // Fix: seed pseudos of all reachable files? That would over-approximate (every import in a reachable
    // file counts as used). More precise: seed pseudos of ENTRY files only, then when a new symbol in
    // file B becomes reachable, seed pseudo of B? Actually if symbol S in file B is reachable, then
    // top-level code of B executes on import — its top-level refs should also be reachable.
    // Implement fixpoint: when symbol in file B reached, add pseudo B and traverse its edges.
    let mut changed = true;
    // Build pseudo -> edges map already in graph.symbol_edges.
    // Map file -> pseudo id.
    while changed {
        changed = false;
        // Collect pseudos to activate: entry files + files containing reached symbols.
        let mut pseudos: Vec<String> = Vec::new();
        for s in seen.clone().iter() {
            // symbol id format `file::...` — extract file prefix (first segment before `::`).
            if let Some(idx) = s.find("::") {
                let f = &s[..idx];
                let pseudo = format!("{}::__file__", f);
                if graph.symbol_edges.contains_key(&pseudo) && !seen.contains(&pseudo) {
                    pseudos.push(pseudo);
                }
            }
        }
        for p in pseudos {
            if seen.insert(p.clone()) {
                changed = true;
                // traverse its outgoing edges immediately
                let mut q = VecDeque::from([p]);
                while let Some(cur) = q.pop_front() {
                    if let Some(nexts) = graph.symbol_edges.get(&cur) {
                        for e in nexts {
                            if seen.insert(e.to.clone()) {
                                q.push_back(e.to.clone());
                                changed = true;
                            }
                        }
                    }
                }
            }
        }
    }
    // Remove pseudo nodes from result? Keep them out.
    seen.into_iter().filter(|s| !s.ends_with("::__file__")).collect()
}
