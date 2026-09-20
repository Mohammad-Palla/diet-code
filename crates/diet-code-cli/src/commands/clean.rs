use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use clap::Args;
use diet_code_core::{edits, DeadCodeDetector, TreeSitterDetector};

#[derive(Args)]
pub struct CleanArgs {
    /// Repository root (default: current directory).
    #[arg(default_value = ".")]
    pub path: PathBuf,
    /// Print the patch plan without modifying anything.
    #[arg(long)]
    pub dry_run: bool,
    /// Keep the cleanup branch even if verification fails.
    #[arg(long)]
    pub keep: bool,
}

pub fn run(args: CleanArgs) -> Result<()> {
    let root = args.path.canonicalize().unwrap_or(args.path.clone());
    let result = TreeSitterDetector.analyze(&root)?;
    let plan = result.cleanup_plan();

    if args.dry_run {
        print_plan(&plan);
        return Ok(());
    }

    if plan.is_empty() {
        println!("Nothing to remove: no CERTAIN/HIGH findings.");
        return Ok(());
    }

    // 1. Verify clean Git working tree.
    ensure_clean_tree(&root)?;
    // 2. Record HEAD.
    let head = current_head(&root)?;
    println!("Recorded HEAD: {}", head.trim());

    // 3. Create branch.
    let branch = format!("diet-code/{}", Utc::now().format("%Y%m%d-%H%M%S"));
    git(&root, &["checkout", "-b", &branch])?;
    println!("Created branch: {}", branch);

    let apply_result = apply_plan(&root, &plan);
    if let Err(e) = apply_result {
        eprintln!("Cleanup failed: {:#}. Restoring {} ...", e, head.trim());
        let _ = git(&root, &["checkout", "--", "."]);
        let _ = git(&root, &["checkout", head.trim()]);
        bail!("cleanup aborted: {:#}", e);
    }

    // 6. Re-run analyzer (sanity: no crashes, report remaining).
    let after = TreeSitterDetector.analyze(&root)?;
    let (_, _, c, h, _, _) = after.summary_counts();
    println!("Post-cleanup remaining CERTAIN/HIGH: {}/{}", c, h);

    // 7. Run verification commands.
    let verify = diet_code_core::entrypoints::discover_verify_commands(&root, &result.config);
    if verify.is_empty() {
        println!("No verification scripts found (no test/build/typecheck); skipping verification.");
    } else {
        for cmd in &verify {
            println!("Running verification: {}", cmd);
            if !run_shell(&root, cmd) {
                eprintln!("verification failed: {}", cmd);
                if !args.keep {
                    println!(
                        "→ reverting cleanup (use --keep to retain the branch for inspection)"
                    );
                    let _ = git(&root, &["checkout", "--", "."]);
                    // Remove untracked deletions? Files were `git rm`'d — restore via checkout HEAD.
                    let _ = git(&root, &["reset", "--hard", "HEAD"]);
                    let _ = git(&root, &["checkout", head.trim()]);
                    let _ = git(&root, &["branch", "-D", &branch]);
                    bail!("verification failed → reverted cleanup");
                } else {
                    bail!("verification failed (kept branch {} per --keep)", branch);
                }
            }
        }
        println!("All verification commands passed.");
    }

    // 8. Show diff stat.
    let stat = git_output(
        &root,
        &["diff", "--stat", &format!("{}...HEAD", head.trim())],
    )
    .unwrap_or_default();
    println!("\nDiff vs {}:\n{}", head.trim(), stat);
    println!(
        "\nCleanup applied on branch {}. Review with `git diff {}...HEAD`.",
        branch,
        head.trim()
    );
    Ok(())
}

fn print_plan(plan: &diet_code_core::edits::CleanupPlan) {
    if plan.is_empty() {
        println!("Nothing to remove: no CERTAIN/HIGH findings.");
        return;
    }
    println!("Would remove:\n");
    for f in &plan.delete_files {
        println!("DELETE {}", f);
    }
    for s in &plan.remove_symbols {
        println!(
            "\nREMOVE SYMBOL\n{}:{}-{}\n{}()",
            s.file, s.start_line, s.end_line, s.symbol
        );
    }
    println!("\nNo other source modifications proposed.");
}

fn ensure_clean_tree(root: &Path) -> Result<()> {
    let out = git_output(root, &["status", "--porcelain"])?;
    if !out.trim().is_empty() {
        bail!("refusing to clean: Git working tree is not clean. Commit or stash changes first.");
    }
    Ok(())
}

fn current_head(root: &Path) -> Result<String> {
    git_output(root, &["rev-parse", "HEAD"]).context("reading current HEAD")
}

fn git(root: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new("git").args(args).current_dir(root).status()?;
    if !status.success() {
        bail!("git {:?} failed", args);
    }
    Ok(())
}

fn git_output(root: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git").args(args).current_dir(root).output()?;
    if !out.status.success() {
        bail!("git {:?} failed", args);
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn apply_plan(root: &Path, plan: &diet_code_core::edits::CleanupPlan) -> Result<()> {
    // Delete whole files via `git rm`.
    for f in &plan.delete_files {
        println!("Deleting file {}", f);
        git(root, &["rm", "-q", f])?;
    }

    // Group symbol removals by file, apply descending byte order.
    let mut by_file: HashMap<&str, Vec<_>> = HashMap::new();
    for s in &plan.remove_symbols {
        by_file.entry(s.file.as_str()).or_default().push(s.clone());
    }
    for (file, mut syms) in by_file {
        let abs = root.join(file);
        let text = std::fs::read_to_string(&abs).with_context(|| format!("reading {}", file))?;
        let mut text = edits::apply_symbol_removals(&text, &mut syms)
            .with_context(|| format!("applying removals in {}", file))?;
        // Remove now-unused imports only when they are mechanically proven
        // unnecessary. Python is excluded: `import pkg` is an executable
        // statement whose side effects (registering models, codecs, plugins)
        // can matter even when the bound name is unused, so dropping it is
        // never provably safe.
        if !(file.ends_with(".py") || file.ends_with(".pyi")) {
            text = edits::prune_unused_imports(&text);
        }
        std::fs::write(&abs, text).with_context(|| format!("writing {}", file))?;
    }
    Ok(())
}

fn run_shell(root: &Path, cmd: &str) -> bool {
    let status = if cfg!(windows) {
        Command::new("cmd")
            .args(["/C", cmd])
            .current_dir(root)
            .status()
    } else {
        Command::new("sh")
            .args(["-c", cmd])
            .current_dir(root)
            .status()
    };
    matches!(status, Ok(s) if s.success())
}
