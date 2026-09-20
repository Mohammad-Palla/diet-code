use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use diet_code_core::{DeadCodeDetector, TreeSitterDetector};

use crate::output;

#[derive(Args)]
pub struct ExplainArgs {
    /// Finding id, symbol, or file path to explain (e.g. src/utils/oldAuth.ts).
    pub query: String,
    /// Repository root (default: current directory).
    #[arg(long, default_value = ".")]
    pub path: PathBuf,
}

pub fn run(args: ExplainArgs) -> Result<()> {
    let root = args.path.canonicalize().unwrap_or(args.path.clone());
    let result = TreeSitterDetector.analyze(&root)?;
    let hits = result.find(&args.query);
    if hits.is_empty() {
        // No finding means nothing suspects this symbol — which is the answer to
        // "is this needed?", not a failure. Report the live evidence instead of
        // leaving the question open.
        return explain_live(&result, &root, &args.query);
    }
    output::header("");
    for f in hits {
        println!("{}", f.file);
        if let Some(sym) = &f.symbol {
            println!("Symbol: {} (lines {}-{})", sym, f.start_line, f.end_line);
        }
        println!(
            "\nClassification: {}",
            f.kind.as_str().to_uppercase().replace('_', " ")
        );
        println!("Confidence: {}", output::confidence_label(&f.confidence));
        println!("\nEvidence");
        for r in &f.reasons {
            println!("✓ {}", r);
        }
        println!(
            "✓ {} production importer(s) in reachability graph",
            prod_importers(&result, &f.file)
        );
        println!(
            "✓ production reachable: {} / test reachable: {} / references: {}",
            f.production_reachable, f.test_reachable, f.reference_count
        );
        if let Some(g) = &f.git {
            println!("\nGit evidence (supporting only)");
            println!(
                "✓ last modified: {}",
                g.last_modified.as_deref().unwrap_or("unknown")
            );
            println!(
                "✓ first seen: {}",
                g.first_seen.as_deref().unwrap_or("unknown")
            );
            println!(
                "✓ last author: {}",
                g.last_author.as_deref().unwrap_or("unknown")
            );
            println!("✓ commits touching file: {}", g.commit_count);
        }
        println!("\nRecommendation:");
        if f.confidence.auto_removable() {
            println!("Safe candidate for removal (`diet-code clean --dry-run`).");
        } else {
            println!("Needs human review — NOT removed automatically.");
        }
        println!();
    }
    Ok(())
}

fn prod_importers(result: &diet_code_core::AnalysisResult, file: &str) -> usize {
    result.file_importers_prod.get(file).copied().unwrap_or(0)
}

/// Explains a symbol or file that produced no finding: it is kept, and the
/// evidence says why. Answers the question directly rather than reporting that
/// no finding matched.
fn explain_live(
    result: &diet_code_core::AnalysisResult,
    root: &std::path::Path,
    query: &str,
) -> Result<()> {
    let matches: Vec<&diet_code_core::symbols::Entity> = result
        .entities
        .iter()
        .filter(|e| e.name == query || e.file == query || e.id == query)
        .collect();

    if matches.is_empty() {
        // Distinguish "not a symbol we indexed" from "indexed and kept".
        let known_file = result.files.iter().any(|f| f == query);
        output::header("");
        if known_file {
            let (prod, test) = result.reachability.file_status(query);
            println!("{}", query);
            println!("\nClassification: KEPT (no dead-code finding)");
            println!("\nEvidence");
            println!("✓ production reachable: {prod} / test reachable: {test}");
            println!(
                "✓ {} production importer(s) in reachability graph",
                prod_importers(result, query)
            );
            print_git(root, query);
            println!("\nConclusion:");
            println!("No evidence this file is dead. Keep it.");
        } else {
            println!(
                "'{}' is not an indexed symbol or file in this repository.",
                query
            );
            println!(
                "Indexed {} files and {} symbols. Check the name, or run \
                 `diet-code analyze --verbose` to list findings.",
                result.files.len(),
                result.entities.len()
            );
        }
        println!();
        return Ok(());
    }

    output::header("");
    for e in matches {
        let (prod, test) = result.reachability.symbol_status(&e.id);
        println!("{}", e.file);
        println!("Symbol: {} (lines {}-{})", e.name, e.start_line, e.end_line);
        println!("\nClassification: KEPT (no dead-code finding)");
        println!("\nEvidence");
        println!("✓ kind: {}", e.kind.as_str());
        println!(
            "✓ {}",
            if e.exported {
                "part of this file's importable surface"
            } else {
                "local to this file"
            }
        );
        println!("✓ production reachable: {prod} / test reachable: {test}");
        println!(
            "✓ {} production importer(s) of {}",
            prod_importers(result, &e.file),
            e.file
        );
        print_git(root, &e.file);
        println!("\nConclusion:");
        let public_surface = result.entries.package_export_files.contains(&e.file)
            || result.entries.package_entries.contains(&e.file);
        if prod {
            println!("Needed: reachable from a production entry point.");
        } else if public_surface {
            println!(
                "Public API of this package: consumers outside the repository can import it, \
                 so repository references alone cannot settle whether it is needed."
            );
        } else if test {
            println!(
                "Referenced only from tests — review whether the tests are the sole consumer."
            );
        } else {
            println!(
                "No dead-code finding was raised, so something the analyzer trusts still \
                 reaches it (dynamic use, public surface, or a framework entry). Not removable."
            );
        }
        println!();
    }
    Ok(())
}

fn print_git(root: &std::path::Path, file: &str) {
    let g = diet_code_core::git_evidence_for_file(root, file);
    println!("\nGit evidence (supporting only)");
    println!(
        "✓ last modified: {}",
        g.last_modified.as_deref().unwrap_or("unknown")
    );
    println!(
        "✓ first seen: {}",
        g.first_seen.as_deref().unwrap_or("unknown")
    );
    println!(
        "✓ last author: {}",
        g.last_author.as_deref().unwrap_or("unknown")
    );
    println!("✓ commits touching file: {}", g.commit_count);
}
