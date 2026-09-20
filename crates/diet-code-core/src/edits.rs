use serde::{Deserialize, Serialize};

/// Deterministic cleanup plan: file deletions + byte-range symbol removals.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupPlan {
    pub delete_files: Vec<String>,
    pub remove_symbols: Vec<SymbolRemoval>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolRemoval {
    pub file: String,
    pub symbol: String,
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
}

impl CleanupPlan {
    pub fn is_empty(&self) -> bool {
        self.delete_files.is_empty() && self.remove_symbols.is_empty()
    }

    pub fn total(&self) -> usize {
        self.delete_files.len() + self.remove_symbols.len()
    }
}

/// Remove bytes [start, end) extended to full lines (including the trailing
/// newline), so no half-lines remain. Deterministic; unrelated formatting untouched.
pub fn remove_byte_range(text: &str, start: usize, end: usize) -> anyhow::Result<String> {
    let bytes = text.as_bytes();
    if start >= bytes.len() || end > bytes.len() || start >= end {
        anyhow::bail!(
            "stale byte range ({}..{}) for text of {} bytes",
            start,
            end,
            bytes.len()
        );
    }
    let mut s = start;
    while s > 0 && bytes[s - 1] != b'\n' {
        s -= 1;
    }
    let mut e = end;
    while e < bytes.len() && bytes[e] != b'\n' {
        e += 1;
    }
    if e < bytes.len() {
        e += 1; // consume newline
    }
    let mut out = String::with_capacity(text.len() - (e - s));
    out.push_str(&text[..s]);
    // Avoid leaving doubled blank lines.
    let left_blank = out.ends_with("\n\n");
    let right_blank = text[e..].starts_with('\n');
    if left_blank && right_blank {
        out.push_str(&text[e + 1..]);
    } else {
        out.push_str(&text[e..]);
    }
    Ok(out)
}

/// Apply several symbol removals to one file's text. Removals must not overlap;
/// they are applied in descending byte order for stability.
pub fn apply_symbol_removals(
    text: &str,
    removals: &mut [SymbolRemoval],
) -> anyhow::Result<String> {
    removals.sort_by_key(|r| std::cmp::Reverse(r.start_byte));
    let mut out = text.to_string();
    for r in removals.iter() {
        out = remove_byte_range(&out, r.start_byte, r.end_byte)?;
    }
    Ok(out)
}

/// Remove import lines whose bindings no longer occur in the file text.
/// Conservative: only touches lines whose every binding is word-absent elsewhere.
pub fn prune_unused_imports(text: &str) -> String {
    use std::collections::{HashMap, HashSet};
    let lines: Vec<&str> = text.lines().collect();
    let mut bindings_per_line: HashMap<usize, Vec<String>> = HashMap::new();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        if t.starts_with("import ")
            || t.starts_with("import{")
            || t.starts_with("import*")
            || t.starts_with("import\"")
            || t.starts_with("import'")
        {
            let locals = rough_import_locals(t);
            if !locals.is_empty() {
                bindings_per_line.insert(i, locals);
            }
        }
    }
    if bindings_per_line.is_empty() {
        return text.to_string();
    }
    let mut drop_lines: HashSet<usize> = HashSet::new();
    for (idx, locals) in &bindings_per_line {
        let others: String = lines
            .iter()
            .enumerate()
            .filter(|(j, _)| j != idx)
            .map(|(_, l)| *l)
            .collect::<Vec<_>>()
            .join("\n");
        if locals.iter().all(|name| !contains_word(&others, name)) {
            drop_lines.insert(*idx);
        }
    }
    if drop_lines.is_empty() {
        return text.to_string();
    }
    let kept: Vec<&str> = lines
        .iter()
        .enumerate()
        .filter(|(i, _)| !drop_lines.contains(i))
        .map(|(_, l)| *l)
        .collect();
    let mut out = kept.join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn rough_import_locals(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let t = line.trim().trim_start_matches("import").trim();
    if t.starts_with('\'') || t.starts_with('"') {
        return out;
    }
    let clause = match t.split(" from ").next() {
        Some(c) => c,
        None => return out,
    };
    let clause = clause.trim().trim_start_matches("type").trim();
    if clause.starts_with('*') {
        if let Some(ns) = clause.split(" as ").nth(1) {
            out.push(ns.trim().trim_matches(',').to_string());
        }
        return out;
    }
    if clause.starts_with('{') {
        let inner = clause.trim_matches(|c| c == '{' || c == '}');
        for part in inner.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let local = part.split(" as ").last().unwrap_or(part).trim();
            let local = local.trim_start_matches("type ").trim();
            if !local.is_empty() {
                out.push(local.to_string());
            }
        }
        return out;
    }
    let (def, rest) = match clause.split_once(',') {
        Some((d, r)) => (d.trim(), Some(r.trim())),
        None => (clause, None),
    };
    if !def.is_empty() && def != "type" {
        out.push(def.to_string());
    }
    if let Some(r) = rest {
        out.extend(rough_import_locals(&format!("import {}", r)));
    }
    out.into_iter().filter(|s| !s.is_empty()).collect()
}

fn contains_word(haystack: &str, word: &str) -> bool {
    if word.is_empty() || word.starts_with('*') {
        return true;
    }
    let mut idx = 0;
    while let Some(pos) = haystack[idx..].find(word) {
        let s = idx + pos;
        let e = s + word.len();
        let before = haystack[..s]
            .chars()
            .last()
            .map(|c| c.is_alphanumeric() || c == '_' || c == '$');
        let after = haystack[e..]
            .chars()
            .next()
            .map(|c| c.is_alphanumeric() || c == '_' || c == '$');
        if !before.unwrap_or(false) && !after.unwrap_or(false) {
            return true;
        }
        idx = e;
    }
    false
}
