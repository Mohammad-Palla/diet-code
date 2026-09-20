use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use diet_code_core::{confidence::Confidence, DeadCodeDetector, TreeSitterDetector};

use crate::output;

#[derive(Args)]
pub struct AnalyzeArgs {
    /// Repository root to analyze (default: current directory).
    #[arg(default_value = ".")]
    pub path: PathBuf,
    /// Print the full stable JSON report.
    #[arg(long)]
    pub json: bool,
    /// Print all findings, not just the highest-confidence ones.
    #[arg(long)]
    pub verbose: bool,
    /// Write analysis.json into .diet-code/.
    #[arg(long, default_value_t = true)]
    pub persist: bool,
}

pub fn run(args: AnalyzeArgs) -> Result<()> {
    let root = args.path.canonicalize().unwrap_or(args.path.clone());
    let detector = TreeSitterDetector;
    let result = detector.analyze(&root)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&result.to_json_value())?);
    } else {
        print_human(&result, args.verbose);
    }

    if args.persist {
        let dir = root.join(".diet-code");
        std::fs::create_dir_all(&dir).ok();
        let payload = serde_json::to_string_pretty(&result.to_json_value())?;
        std::fs::write(dir.join("analysis.json"), payload)
            .context("writing .diet-code/analysis.json")?;
    }

    Ok(())
}

fn print_human(result: &diet_code_core::AnalysisResult, verbose: bool) {
    use diet_code_core::findings::FindingKind;

    output::header(&format!("Repository: {}", result.repository));
    println!("Language: TypeScript / JavaScript\n");

    let (files, symbols, certain, high, medium, low) = result.summary_counts();
    println!("Files analyzed       {}", files);
    println!("Symbols indexed      {}", symbols);
    println!("\nDead code candidates");
    output::print_divider();
    println!("CERTAIN                {}", certain);
    println!("HIGH                   {}", high);
    println!("MEDIUM                 {}", medium);
    println!("LOW                    {}", low);

    let mut n_files = 0;
    let mut n_funcs = 0;
    let mut n_classes = 0;
    for f in &result.findings {
        if !f.confidence.auto_removable() {
            continue;
        }
        match f.kind {
            FindingKind::DeadFile => n_files += 1,
            FindingKind::DeadFunction => n_funcs += 1,
            FindingKind::DeadClass => n_classes += 1,
            _ => {}
        }
    }
    println!("\nPotentially removable");
    output::print_divider();
    println!("Files                  {}", n_files);
    println!("Functions              {}", n_funcs);
    println!("Classes                 {}", n_classes);
    println!("LOC                  {}", result.removable_loc());

    // Highest-confidence findings first (already sorted).
    let list: Vec<_> = result
        .findings
        .iter()
        .filter(|f| {
            verbose || f.confidence == Confidence::Certain || f.confidence == Confidence::High
        })
        .take(if verbose { usize::MAX } else { 20 })
        .collect();
    if !list.is_empty() {
        println!("\nTop findings");
        output::print_divider();
        for f in list {
            match &f.symbol {
                Some(s) => println!(
                    "[{}] {} {}:{}-{} ({} refs)",
                    output::confidence_label(&f.confidence),
                    f.kind.as_str(),
                    f.file,
                    f.start_line,
                    s,
                    f.reference_count
                ),
                None => println!(
                    "[{}] {} {}",
                    output::confidence_label(&f.confidence),
                    f.kind.as_str(),
                    f.file
                ),
            }
        }
        if !verbose && result.findings.len() > 20 {
            println!(
                "\n... and {} more (use --verbose or --json)",
                result.findings.len() - 20
            );
        }
    } else {
        println!("\nNo CERTAIN/HIGH dead-code candidates found.");
    }
}
