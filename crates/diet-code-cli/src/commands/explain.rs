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
        println!("No findings match '{}'.", args.query);
        println!("Run `diet-code analyze --verbose` to list all findings.");
        return Ok(());
    }
    output::header("");
    for f in hits {
        println!("{}", f.file);
        if let Some(sym) = &f.symbol {
            println!("Symbol: {} (lines {}-{})", sym, f.start_line, f.end_line);
        }
        println!("\nClassification: {}", f.kind.as_str().to_uppercase().replace('_', " "));
        println!("Confidence: {}", output::confidence_label(&f.confidence));
        println!("\nEvidence");
        for r in &f.reasons {
            println!("✓ {}", r);
        }
        println!("✓ {} production importer(s) in reachability graph", prod_importers(&result, &f.file));
        println!(
            "✓ production reachable: {} / test reachable: {} / references: {}",
            f.production_reachable, f.test_reachable, f.reference_count
        );
        if let Some(g) = &f.git {
            println!("\nGit evidence (supporting only)");
            println!("✓ last modified: {}", g.last_modified.as_deref().unwrap_or("unknown"));
            println!("✓ first seen: {}", g.first_seen.as_deref().unwrap_or("unknown"));
            println!("✓ last author: {}", g.last_author.as_deref().unwrap_or("unknown"));
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
