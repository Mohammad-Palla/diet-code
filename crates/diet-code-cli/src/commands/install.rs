use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};

use crate::output;

/// The skill definition is compiled into the binary so `install` works from any
/// distribution channel (npm wrapper, shell installer, source build) without
/// needing the repository on disk.
const SKILL: &str = include_str!("../../../../skills/diet-code/SKILL.md");

#[derive(Args)]
pub struct InstallArgs {
    /// Assistant to register the skill with.
    #[arg(long, value_enum, default_value_t = Agent::Claude)]
    pub agent: Agent,
    /// Install for the current user instead of the current project
    /// (Claude Code only).
    #[arg(long)]
    pub global: bool,
    /// Project root to install into.
    #[arg(long, default_value = ".")]
    pub path: PathBuf,
    /// Print the destination and the skill body without writing anything.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
pub enum Agent {
    /// Claude Code — `.claude/skills/diet-code/SKILL.md`.
    Claude,
    /// Cursor — `.cursor/rules/diet-code.mdc`.
    Cursor,
    /// Any assistant that reads `AGENTS.md` (Codex, Amp, opencode, ...).
    Agents,
}

pub fn run(args: InstallArgs) -> Result<()> {
    let root = args.path.canonicalize().unwrap_or(args.path.clone());
    let dest = destination(&args, &root)?;

    output::header("");
    println!("Skill:  diet-code");
    println!("Agent:  {}", agent_name(args.agent));
    println!("Target: {}", dest.display());

    if args.dry_run {
        println!("\n--- would write ---\n");
        print!("{}", rendered(args.agent));
        return Ok(());
    }

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    match args.agent {
        // `AGENTS.md` is a shared file: replace only our own block so anything
        // else in it survives re-installation.
        Agent::Agents => {
            let existing = std::fs::read_to_string(&dest).unwrap_or_default();
            let updated = splice_block(&existing, &rendered(args.agent));
            std::fs::write(&dest, updated)
                .with_context(|| format!("writing {}", dest.display()))?;
        }
        _ => {
            std::fs::write(&dest, rendered(args.agent))
                .with_context(|| format!("writing {}", dest.display()))?;
        }
    }

    println!("\nInstalled. Try it:");
    println!("  /diet-code is <someFunction> needed");
    println!("  /diet-code");
    if args.agent == Agent::Claude && !args.global {
        println!("\nCommit `.claude/skills/diet-code/` to share it with the team.");
    }
    Ok(())
}

fn destination(args: &InstallArgs, root: &Path) -> Result<PathBuf> {
    Ok(match args.agent {
        Agent::Claude if args.global => {
            let home = home_dir().context("cannot determine the home directory")?;
            home.join(".claude/skills/diet-code/SKILL.md")
        }
        Agent::Claude => root.join(".claude/skills/diet-code/SKILL.md"),
        Agent::Cursor => root.join(".cursor/rules/diet-code.mdc"),
        Agent::Agents => root.join("AGENTS.md"),
    })
}

fn agent_name(agent: Agent) -> &'static str {
    match agent {
        Agent::Claude => "Claude Code",
        Agent::Cursor => "Cursor",
        Agent::Agents => "AGENTS.md (Codex, Amp, opencode, ...)",
    }
}

/// The skill body, adjusted for how each assistant expects to receive it.
fn rendered(agent: Agent) -> String {
    match agent {
        // Claude Code reads the frontmatter as-is.
        Agent::Claude => SKILL.to_string(),
        // Cursor rules use their own frontmatter keys.
        Agent::Cursor => {
            let body = strip_frontmatter(SKILL);
            format!(
                "---\ndescription: {}\nalwaysApply: false\n---\n\n{}",
                description(SKILL).unwrap_or_else(|| "diet-code dead-code evidence".to_string()),
                body
            )
        }
        // `AGENTS.md` is prose, not a skill container: drop the frontmatter and
        // wrap it in markers so re-installing is idempotent.
        Agent::Agents => format!(
            "{}\n{}\n{}\n",
            BLOCK_START,
            strip_frontmatter(SKILL).trim_end(),
            BLOCK_END
        ),
    }
}

const BLOCK_START: &str = "<!-- diet-code:start -->";
const BLOCK_END: &str = "<!-- diet-code:end -->";

/// Replaces an existing diet-code block, or appends one.
fn splice_block(existing: &str, block: &str) -> String {
    if let (Some(start), Some(end)) = (existing.find(BLOCK_START), existing.find(BLOCK_END)) {
        if start < end {
            let mut out = String::with_capacity(existing.len() + block.len());
            out.push_str(&existing[..start]);
            out.push_str(block.trim_end());
            out.push_str(&existing[end + BLOCK_END.len()..]);
            return out;
        }
    }
    if existing.trim().is_empty() {
        return block.to_string();
    }
    format!("{}\n\n{}", existing.trim_end(), block)
}

fn strip_frontmatter(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("---\n") else {
        return text;
    };
    match rest.find("\n---\n") {
        Some(end) => rest[end + 5..].trim_start_matches('\n'),
        None => text,
    }
}

fn description(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.strip_prefix("description:"))
        .map(|d| d.trim().trim_matches('"').to_string())
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_is_embedded_with_frontmatter() {
        assert!(SKILL.starts_with("---\n"));
        assert!(SKILL.contains("name: diet-code"));
        assert!(description(SKILL).is_some_and(|d| d.contains("needed")));
    }

    #[test]
    fn frontmatter_is_stripped_for_prose_targets() {
        let body = strip_frontmatter(SKILL);
        assert!(!body.starts_with("---"));
        assert!(body.starts_with("# /diet-code"));
    }

    /// Re-installing must not duplicate the block or disturb the rest of the file.
    #[test]
    fn agents_block_is_spliced_idempotently() {
        let original = "# Project notes\n\nKeep these.\n";
        let once = splice_block(original, &rendered(Agent::Agents));
        let twice = splice_block(&once, &rendered(Agent::Agents));
        assert_eq!(once, twice);
        assert!(once.contains("Keep these."));
        assert_eq!(once.matches(BLOCK_START).count(), 1);
    }

    #[test]
    fn empty_agents_file_gets_just_the_block() {
        let out = splice_block("", &rendered(Agent::Agents));
        assert!(out.starts_with(BLOCK_START));
        assert_eq!(out.matches(BLOCK_END).count(), 1);
    }

    #[test]
    fn cursor_rules_get_cursor_frontmatter() {
        let out = rendered(Agent::Cursor);
        assert!(out.starts_with("---\ndescription: "));
        assert!(out.contains("alwaysApply: false"));
        assert!(out.contains("# /diet-code"));
        assert!(!out.contains("name: diet-code"));
    }
}
