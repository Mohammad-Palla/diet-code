use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use clap::Args;
use diet_code_core::{DeadCodeDetector, TreeSitterDetector};
use serde::{Deserialize, Serialize};

#[derive(Args)]
pub struct BenchmarkArgs {
    /// Path to tasks.json (array of {name, prompt, verify}).
    #[arg(long)]
    pub tasks: PathBuf,
    /// Repository root (default: current directory).
    #[arg(long, default_value = ".")]
    pub path: PathBuf,
    /// Repetitions per branch (minimum 3 recommended).
    #[arg(long, default_value_t = 3)]
    pub reps: usize,
    /// Agent command to invoke (default: auto-discovered, prefers `opencode`).
    #[arg(long)]
    pub agent_command: Option<String>,
    /// Model for the agent, e.g. `opencode/muse-spark-1.3-contributor-free`.
    #[arg(long, default_value = "opencode/muse-spark-1.3-contributor-free")]
    pub agent_model: String,
    /// Per-run wall-clock timeout in seconds.
    #[arg(long, default_value_t = 1200)]
    pub timeout_secs: u64,
    /// Extra args passed to the agent command.
    #[arg(long)]
    pub agent_args: Vec<String>,
    /// Mock mode: do not invoke a real agent; simulate a run (for CI/tests).
    #[arg(long)]
    pub mock: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct Task {
    name: String,
    prompt: String,
    verify: String,
    /// Optional test file (resolved relative to the tasks.json directory)
    /// copied into `<workdir>/test/diet-task-<name>.ts` for every run, so the
    /// verify command can target task-specific tests identically in both arms.
    #[serde(default)]
    test_file: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct RunMetrics {
    task: String,
    branch: String, // "base" | "diet"
    rep: usize,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    total_tokens: Option<u64>,
    tool_calls: Option<u64>,
    files_read: Option<u64>,
    files_modified: usize,
    elapsed_secs: f64,
    success: bool,
    agent_exit: Option<i32>,
    token_note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct TaskReport {
    task: String,
    base: Summary,
    diet: Summary,
    input_token_change_pct: Option<f64>,
    tool_call_change_pct: Option<f64>,
    files_read_change_pct: Option<f64>,
    elapsed_change_pct: Option<f64>,
    success_base: String,
    success_diet: String,
}

#[derive(Debug, Clone, Serialize)]
struct Summary {
    input_tokens_median: Option<f64>,
    output_tokens_median: Option<f64>,
    total_tokens_median: Option<f64>,
    tool_calls_median: Option<f64>,
    files_read_median: Option<f64>,
    files_modified_median: f64,
    elapsed_median: f64,
}

/// Isolated parser for agent telemetry so Codex/Gemini/Cursor CLIs can be added later.
trait AgentRunParser: Send + Sync {
    fn name(&self) -> &'static str;
    /// Parse agent stdout/stderr + workdir into token/tool metrics.
    fn parse(&self, stdout: &str, stderr: &str, workdir: &Path) -> ParsedTelemetry;
}

#[derive(Debug, Default)]
struct ParsedTelemetry {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    tool_calls: Option<u64>,
    files_read: Option<u64>,
}

struct ClaudeParser;

impl AgentRunParser for ClaudeParser {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn parse(&self, stdout: &str, stderr: &str, workdir: &Path) -> ParsedTelemetry {
        // Best effort, version-tolerant:
        // 1. Streamed JSON lines with usage fields.
        let mut tel = ParsedTelemetry::default();
        for line in stdout.lines().chain(stderr.lines()) {
            let line = line.trim();
            if !line.starts_with('{') {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                // `{"usage":{"input_tokens":..,"output_tokens":..}}` or message.usage
                for u in [
                    v.get("usage"),
                    v.get("message").and_then(|m| m.get("usage")),
                    v.get("metrics").and_then(|m| m.get("usage")),
                ]
                .into_iter()
                .flatten()
                {
                    if tel.input_tokens.is_none() {
                        tel.input_tokens = u.get("input_tokens").and_then(|x| x.as_u64());
                    }
                    if tel.output_tokens.is_none() {
                        tel.output_tokens = u.get("output_tokens").and_then(|x| x.as_u64());
                    }
                }
                // tool_use counting
                if let Some(content) = v
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                {
                    for item in content {
                        if item.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                            tel.tool_calls = Some(tel.tool_calls.unwrap_or(0) + 1);
                        }
                    }
                }
                if v.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                    tel.tool_calls = Some(tel.tool_calls.unwrap_or(0) + 1);
                }
            }
        }
        // 2. Local session files (~/.claude/projects/...) — best effort, never fail.
        if tel.input_tokens.is_none() {
            if let Some(sess) = newest_session_file() {
                if let Ok(t) = parse_claude_session(&sess) {
                    if t.input_tokens.is_some() {
                        tel = t;
                    }
                }
            }
        }
        // 3. files_read: count File/Read tool mentions in transcript (rough).
        if tel.files_read.is_none() {
            let n = count_tool_mentions(stdout, stderr, &["Read", "Glob", "Grep"]);
            if n > 0 {
                tel.files_read = Some(n);
            }
        }
        let _ = workdir;
        tel
    }
}

/// Parses `opencode run --format json` event streams:
/// `step_finish` parts carry per-step `{input, output}` tokens and
/// `tool_use` events carry the executed tool name.
struct OpencodeParser;

impl AgentRunParser for OpencodeParser {
    fn name(&self) -> &'static str {
        "opencode"
    }

    fn parse(&self, stdout: &str, _stderr: &str, _workdir: &Path) -> ParsedTelemetry {
        let mut tel = ParsedTelemetry::default();
        for line in stdout.lines() {
            let line = line.trim();
            if !line.starts_with('{') {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            match v.get("type").and_then(|t| t.as_str()) {
                Some("step_finish") => {
                    if let Some(tok) = v.get("part").and_then(|p| p.get("tokens")) {
                        // Count ALL consumed context: fresh input plus cache
                        // reads/writes. (Comparing raw `input` alone would
                        // reward warm caches instead of smaller repos.)
                        let input = tok.get("input").and_then(|x| x.as_u64()).unwrap_or(0);
                        let cache = tok.get("cache");
                        let read = cache
                            .and_then(|c| c.get("read"))
                            .and_then(|x| x.as_u64())
                            .unwrap_or(0);
                        let write = cache
                            .and_then(|c| c.get("write"))
                            .and_then(|x| x.as_u64())
                            .unwrap_or(0);
                        tel.input_tokens =
                            Some(tel.input_tokens.unwrap_or(0) + input + read + write);
                        let output = tok.get("output").and_then(|x| x.as_u64()).unwrap_or(0);
                        let reasoning = tok.get("reasoning").and_then(|x| x.as_u64()).unwrap_or(0);
                        tel.output_tokens =
                            Some(tel.output_tokens.unwrap_or(0) + output + reasoning);
                    }
                }
                Some("tool_use") => {
                    tel.tool_calls = Some(tel.tool_calls.unwrap_or(0) + 1);
                    let tool = v
                        .get("part")
                        .and_then(|p| p.get("tool"))
                        .and_then(|t| t.as_str())
                        .unwrap_or("");
                    // File-exploration tools: what the agent had to read/search.
                    if matches!(tool, "read" | "glob" | "grep" | "list") {
                        tel.files_read = Some(tel.files_read.unwrap_or(0) + 1);
                    }
                }
                _ => {}
            }
        }
        tel
    }
}

fn count_tool_mentions(stdout: &str, stderr: &str, tools: &[&str]) -> u64 {
    let mut n = 0;
    for line in stdout.lines().chain(stderr.lines()) {
        for t in tools {
            if line.contains(&format!("\"name\":\"{}\"", t))
                || line.contains(&format!("tool_use.*{}", t))
            {
                n += 1;
                break;
            }
        }
    }
    n
}

fn newest_session_file() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let dir = PathBuf::from(home).join(".claude").join("projects");
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    let mut stack = vec![dir];
    let mut seen = 0;
    while let Some(d) = stack.pop() {
        if seen > 200 {
            break;
        }
        seen += 1;
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                let mtime = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
                if let Some(mt) = mtime {
                    if best.as_ref().map(|(t, _)| mt > *t).unwrap_or(true) {
                        best = Some((mt, p));
                    }
                }
            }
        }
    }
    best.map(|(_, p)| p)
}

fn parse_claude_session(path: &Path) -> Result<ParsedTelemetry> {
    let text = std::fs::read_to_string(path)?;
    let mut tel = ParsedTelemetry::default();
    for line in text.lines().rev().take(500) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if tel.input_tokens.is_none() {
            for u in [
                v.get("usage"),
                v.get("message").and_then(|m| m.get("usage")),
            ]
            .into_iter()
            .flatten()
            {
                tel.input_tokens = u.get("input_tokens").and_then(|x| x.as_u64());
                tel.output_tokens = u.get("output_tokens").and_then(|x| x.as_u64());
                if tel.input_tokens.is_some() {
                    break;
                }
            }
        }
        if v.get("type").and_then(|t| t.as_str()) == Some("assistant") {
            if let Some(arr) = v
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())
            {
                for item in arr {
                    if item.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                        tel.tool_calls = Some(tel.tool_calls.unwrap_or(0) + 1);
                    }
                }
            }
        }
        if tel.input_tokens.is_some() && tel.tool_calls.unwrap_or(0) > 0 {
            break;
        }
    }
    Ok(tel)
}

pub fn run(args: BenchmarkArgs) -> Result<()> {
    let root = args.path.canonicalize().unwrap_or(args.path.clone());
    let tasks_text = std::fs::read_to_string(&args.tasks)
        .with_context(|| format!("reading tasks file {}", args.tasks.display()))?;
    let tasks: Vec<Task> = serde_json::from_str(&tasks_text).context("parsing tasks.json")?;
    if tasks.is_empty() {
        bail!("no tasks in {}", args.tasks.display());
    }
    let reps = args.reps.max(1);
    // testFile entries resolve relative to the tasks.json directory.
    let tasks_dir = args
        .tasks
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    // The cleaned tree MUST come only from Diet Code's deterministic cleanup (no AI cleanup).
    println!("Analyzing repository for deterministic cleanup...");
    let analysis = TreeSitterDetector.analyze(&root)?;
    let plan = analysis.cleanup_plan();
    println!(
        "Cleanup plan: {} files + {} symbols (CERTAIN/HIGH only)",
        plan.delete_files.len(),
        plan.remove_symbols.len()
    );

    // Snapshot current HEAD so BASE == current commit.
    let base_commit = git_output(&root, &["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    println!("Base commit: {}", base_commit);

    // Agent discovery: prefers `opencode` (free-tier friendly), falls back to `claude`.
    let (agent_cmd, agent_kind) = resolve_agent_command(args.agent_command.clone(), args.mock)?;
    println!("Agent: {} ({:?})", agent_cmd, agent_kind);
    println!("Model: {}", args.agent_model);

    let parser: Box<dyn AgentRunParser> = match agent_kind {
        AgentKind::Opencode => Box::new(OpencodeParser),
        AgentKind::Claude | AgentKind::External => Box::new(ClaudeParser),
    };
    println!("Telemetry parser: {}", parser.name());

    // Build run order: randomized base/diet interleaving (seeded shuffle for reproducibility).
    let mut order: Vec<(&str, usize, usize)> = Vec::new(); // (branch, task_idx, rep)
    for (ti, _) in tasks.iter().enumerate() {
        for rep in 0..reps {
            order.push(("base", ti, rep));
            order.push(("diet", ti, rep));
        }
    }
    shuffle_deterministic(&mut order, 0xD1E7);

    let work_base = std::env::temp_dir().join(format!("diet-code-bench-{}", std::process::id()));
    std::fs::create_dir_all(&work_base).ok();

    let mut runs: Vec<RunMetrics> = Vec::new();
    for (branch, ti, rep) in order {
        let task = &tasks[ti];
        println!(
            "\n=== task '{}' branch={} rep={} ===",
            task.name,
            branch,
            rep + 1
        );
        match run_once(
            &root,
            &work_base,
            &tasks_dir,
            &base_commit,
            &plan,
            branch,
            task,
            rep,
            &agent_cmd,
            &agent_kind,
            &args.agent_model,
            &args.agent_args,
            args.timeout_secs,
            args.mock,
            parser.as_ref(),
        ) {
            Ok(m) => {
                println!(
                    "  success={} elapsed={:.1}s files_modified={} tokens={:?}",
                    m.success, m.elapsed_secs, m.files_modified, m.total_tokens
                );
                runs.push(m);
            }
            Err(e) => {
                eprintln!("  run failed: {:#}", e);
                runs.push(RunMetrics {
                    task: task.name.clone(),
                    branch: branch.to_string(),
                    rep,
                    input_tokens: None,
                    output_tokens: None,
                    total_tokens: None,
                    tool_calls: None,
                    files_read: None,
                    files_modified: 0,
                    elapsed_secs: 0.0,
                    success: false,
                    agent_exit: None,
                    token_note: Some(format!("run error: {:#}", e)),
                });
            }
        }
    }

    // Aggregate medians per task.
    let reports = summarize(&runs, &tasks);
    print_report(&reports, &runs);

    // Persist.
    let out_dir = root.join(".diet-code");
    std::fs::create_dir_all(&out_dir).ok();
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let payload = serde_json::json!({
        "generatedAt": chrono::Utc::now().to_rfc3339(),
        "baseCommit": base_commit,
        "reps": reps,
        "runs": runs,
        "reports": reports,
    });
    let out_path = out_dir.join(format!("benchmark-{}.json", stamp));
    std::fs::write(&out_path, serde_json::to_string_pretty(&payload)?)?;
    println!("\nBenchmark saved to {}", out_path.display());

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentKind {
    Opencode,
    Claude,
    External,
}

fn resolve_agent_command(override_cmd: Option<String>, mock: bool) -> Result<(String, AgentKind)> {
    if mock {
        return Ok(("__mock__".to_string(), AgentKind::External));
    }
    if let Some(c) = override_cmd {
        let kind = if c.contains("opencode") {
            AgentKind::Opencode
        } else if c.contains("claude") {
            AgentKind::Claude
        } else {
            AgentKind::External
        };
        return Ok((c, kind));
    }
    // Runtime discovery: prefer `opencode` (works with free-tier models),
    // fall back to `claude`.
    let opencode = Command::new("opencode")
        .arg("--help")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if matches!(opencode, Ok(s) if s.success()) {
        return Ok(("opencode".to_string(), AgentKind::Opencode));
    }
    let probe = Command::new("claude")
        .arg("--help")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match probe {
        Ok(s) if s.success() => Ok(("claude".to_string(), AgentKind::Claude)),
        _ => bail!("no agent found: `opencode --help` and `claude --help` both failed. Pass --agent-command or use --mock."),
    }
}

#[allow(clippy::too_many_arguments)]
fn run_once(
    root: &Path,
    work_base: &Path,
    tasks_dir: &Path,
    base_commit: &str,
    plan: &diet_code_core::edits::CleanupPlan,
    branch: &str,
    task: &Task,
    rep: usize,
    agent_cmd: &str,
    agent_kind: &AgentKind,
    agent_model: &str,
    agent_args: &[String],
    timeout_secs: u64,
    mock: bool,
    parser: &dyn AgentRunParser,
) -> Result<RunMetrics> {
    // Fresh working directory per run.
    let dir = work_base.join(format!("{}-{}-{}", task.name, branch, rep));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.parent().unwrap()).ok();
    // Clone base commit fresh (no preloaded Diet Code findings in the prompt; same base commit).
    let clone_status = Command::new("git")
        .args([
            "clone",
            "-q",
            &root.to_string_lossy(),
            &dir.to_string_lossy(),
        ])
        .status()?;
    if !clone_status.success() {
        bail!("git clone failed");
    }
    let _ = Command::new("git")
        .args(["checkout", "-q", base_commit])
        .current_dir(&dir)
        .status();
    // For DIET: apply the identical deterministic cleanup (byte-range edits + git rm), no AI involved.
    if branch == "diet" {
        apply_deterministic_plan(&dir, plan)?;
    }
    // Share installed dependencies without copying them: `git clone` only carries
    // committed files, so link node_modules from the source root when present.
    link_node_modules(root, &dir);
    // Stage the task's test file identically in both arms (untracked harness file).
    if let Some(tf) = &task.test_file {
        let src = tasks_dir.join(tf);
        let dst = dir.join(format!("test/diet-task-{}.ts", task.name));
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::copy(&src, &dst)
            .with_context(|| format!("copying task test file {}", src.display()))?;
    }

    let start = Instant::now();
    let (stdout, stderr, exit_code) = if mock {
        // Simulate an agent doing work proportional to repo size: list files.
        let count = count_source_files(&dir);
        std::thread::sleep(std::time::Duration::from_millis(50));
        (
            format!("{{\"mock\":true,\"files\":{}}}", count),
            String::new(),
            Some(0),
        )
    } else {
        invoke_agent(
            agent_cmd,
            agent_kind,
            agent_model,
            agent_args,
            &dir,
            &task.prompt,
            timeout_secs,
        )?
    };
    let elapsed = start.elapsed().as_secs_f64();

    let tel = parser.parse(&stdout, &stderr, &dir);
    let total = match (tel.input_tokens, tel.output_tokens) {
        (Some(i), Some(o)) => Some(i + o),
        (Some(i), None) => Some(i),
        _ => None,
    };

    // Task success MUST come from the provided verify command — never trust agent prose.
    let verify_ok = run_shell(&dir, &task.verify);
    let files_modified = count_modified_files(&dir);

    Ok(RunMetrics {
        task: task.name.clone(),
        branch: branch.to_string(),
        rep,
        input_tokens: tel.input_tokens,
        output_tokens: tel.output_tokens,
        total_tokens: total,
        tool_calls: tel.tool_calls,
        files_read: tel.files_read,
        files_modified,
        elapsed_secs: elapsed,
        success: verify_ok,
        agent_exit: exit_code,
        token_note: if total.is_none() {
            Some("token metrics unavailable".to_string())
        } else {
            None
        },
    })
}

fn invoke_agent(
    cmd: &str,
    kind: &AgentKind,
    model: &str,
    extra: &[String],
    workdir: &Path,
    prompt: &str,
    timeout_secs: u64,
) -> Result<(String, String, Option<i32>)> {
    match kind {
        AgentKind::Opencode => {
            // `opencode run --format json --auto -m <model> --dir <workdir> "<prompt>"`
            // Fresh session per invocation; JSON event stream carries token/tool telemetry.
            let mut args: Vec<String> = vec![
                "run".to_string(),
                "--format".to_string(),
                "json".to_string(),
                "--auto".to_string(),
                "-m".to_string(),
                model.to_string(),
                "--dir".to_string(),
                workdir.to_string_lossy().to_string(),
            ];
            for a in extra {
                args.push(a.clone());
            }
            args.push(prompt.to_string());
            run_with_timeout(cmd, &args, workdir, timeout_secs)
        }
        AgentKind::Claude | AgentKind::External => {
            // Non-interactive invocation. Flags differ per installed version; prefer `-p` when supported.
            let help = Command::new(cmd).arg("--help").output();
            let help_text = help
                .map(|o| {
                    format!(
                        "{}{}",
                        String::from_utf8_lossy(&o.stdout),
                        String::from_utf8_lossy(&o.stderr)
                    )
                })
                .unwrap_or_default();
            let mut args: Vec<String> = Vec::new();
            if help_text.contains("-p, --print") || help_text.contains("--print") {
                args.push("-p".to_string());
            }
            for a in extra {
                args.push(a.clone());
            }
            args.push(prompt.to_string());
            run_with_timeout(cmd, &args, workdir, timeout_secs)
        }
    }
}

/// Run a child process with a wall-clock timeout; kills on expiry.
fn run_with_timeout(
    cmd: &str,
    args: &[String],
    workdir: &Path,
    timeout_secs: u64,
) -> Result<(String, String, Option<i32>)> {
    use std::process::Stdio;
    let mut child = Command::new(cmd)
        .args(args)
        .current_dir(workdir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs.max(60));
    loop {
        match child.try_wait()? {
            Some(status) => {
                let out = child.wait_with_output()?;
                return Ok((
                    String::from_utf8_lossy(&out.stdout).to_string(),
                    String::from_utf8_lossy(&out.stderr).to_string(),
                    status.code(),
                ));
            }
            None => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let out = child.wait_with_output()?;
                    return Ok((
                        String::from_utf8_lossy(&out.stdout).to_string(),
                        format!(
                            "{}\n[TIMEOUT after {}s: agent killed]",
                            String::from_utf8_lossy(&out.stderr),
                            timeout_secs
                        ),
                        None,
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        }
    }
}

/// Symlink the source root's node_modules into a run directory so verify
/// commands work without reinstalling dependencies per run.
fn link_node_modules(root: &Path, dir: &Path) {
    let src = root.join("node_modules");
    let dst = dir.join("node_modules");
    if src.is_dir() && !dst.exists() {
        #[cfg(unix)]
        std::os::unix::fs::symlink(&src, &dst).ok();
    }
}

fn apply_deterministic_plan(
    workdir: &Path,
    plan: &diet_code_core::edits::CleanupPlan,
) -> Result<()> {
    for f in &plan.delete_files {
        let p = workdir.join(f);
        if p.exists() {
            std::fs::remove_file(&p).ok();
        }
    }
    let mut by_file: HashMap<&str, Vec<diet_code_core::edits::SymbolRemoval>> = HashMap::new();
    for s in &plan.remove_symbols {
        by_file.entry(s.file.as_str()).or_default().push(s.clone());
    }
    for (file, mut syms) in by_file {
        let abs = workdir.join(file);
        if !abs.exists() {
            continue;
        }
        let text = std::fs::read_to_string(&abs).unwrap_or_default();
        // Same deterministic removal as `clean` (stale ranges keep the file).
        if let Ok(next) = diet_code_core::edits::apply_symbol_removals(&text, &mut syms) {
            std::fs::write(&abs, next).ok();
        }
    }
    Ok(())
}

fn count_source_files(dir: &Path) -> usize {
    let mut n = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_dir() {
                if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                    if name == ".git" || name == "node_modules" || name == "target" {
                        continue;
                    }
                }
                stack.push(p);
            } else if let Some(ext) = p.extension().and_then(|s| s.to_str()) {
                if matches!(ext, "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs") {
                    n += 1;
                }
            }
        }
    }
    n
}

fn count_modified_files(workdir: &Path) -> usize {
    let out = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(workdir)
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
            // Ignore the benchmark's own scaffolding: the node_modules symlink
            // and the staged task test files (identical in both arms).
            .filter(|l| !l.ends_with("node_modules") && !l.ends_with("node_modules/"))
            .filter(|l| !l.contains("diet-task-"))
            .count(),
        Err(_) => 0,
    }
}

fn run_shell(workdir: &Path, cmd: &str) -> bool {
    let status = if cfg!(windows) {
        Command::new("cmd")
            .args(["/C", cmd])
            .current_dir(workdir)
            .status()
    } else {
        Command::new("sh")
            .args(["-c", cmd])
            .current_dir(workdir)
            .status()
    };
    matches!(status, Ok(s) if s.success())
}

fn git_output(root: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git").args(args).current_dir(root).output()?;
    if !out.status.success() {
        bail!("git {:?} failed", args);
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn shuffle_deterministic<T>(v: &mut [T], mut seed: u64) {
    // xorshift shuffle (no extra deps).
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for i in (1..v.len()).rev() {
        let j = (rnd() as usize) % (i + 1);
        v.swap(i, j);
    }
}

fn median(mut xs: Vec<f64>) -> Option<f64> {
    if xs.is_empty() {
        return None;
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let m = xs.len() / 2;
    if xs.len() % 2 == 1 {
        Some(xs[m])
    } else {
        Some((xs[m - 1] + xs[m]) / 2.0)
    }
}

fn pct_change(base: Option<f64>, diet: Option<f64>) -> Option<f64> {
    match (base, diet) {
        (Some(b), Some(d)) if b != 0.0 => Some((d - b) / b * 100.0),
        _ => None,
    }
}

fn summarize(runs: &[RunMetrics], tasks: &[Task]) -> Vec<TaskReport> {
    let mut out = Vec::new();
    for t in tasks {
        let base: Vec<&RunMetrics> = runs
            .iter()
            .filter(|r| r.task == t.name && r.branch == "base")
            .collect();
        let diet: Vec<&RunMetrics> = runs
            .iter()
            .filter(|r| r.task == t.name && r.branch == "diet")
            .collect();
        let sum = |rs: &[&RunMetrics], f: fn(&RunMetrics) -> Option<f64>| {
            median(rs.iter().filter_map(|r| f(r)).collect())
        };
        let b = Summary {
            input_tokens_median: sum(&base, |r| r.input_tokens.map(|x| x as f64)),
            output_tokens_median: sum(&base, |r| r.output_tokens.map(|x| x as f64)),
            total_tokens_median: sum(&base, |r| r.total_tokens.map(|x| x as f64)),
            tool_calls_median: sum(&base, |r| r.tool_calls.map(|x| x as f64)),
            files_read_median: sum(&base, |r| r.files_read.map(|x| x as f64)),
            files_modified_median: median(base.iter().map(|r| r.files_modified as f64).collect())
                .unwrap_or(0.0),
            elapsed_median: median(base.iter().map(|r| r.elapsed_secs).collect()).unwrap_or(0.0),
        };
        let d = Summary {
            input_tokens_median: sum(&diet, |r| r.input_tokens.map(|x| x as f64)),
            output_tokens_median: sum(&diet, |r| r.output_tokens.map(|x| x as f64)),
            total_tokens_median: sum(&diet, |r| r.total_tokens.map(|x| x as f64)),
            tool_calls_median: sum(&diet, |r| r.tool_calls.map(|x| x as f64)),
            files_read_median: sum(&diet, |r| r.files_read.map(|x| x as f64)),
            files_modified_median: median(diet.iter().map(|r| r.files_modified as f64).collect())
                .unwrap_or(0.0),
            elapsed_median: median(diet.iter().map(|r| r.elapsed_secs).collect()).unwrap_or(0.0),
        };
        let sb = format!(
            "{}/{}",
            base.iter().filter(|r| r.success).count(),
            base.len()
        );
        let sd = format!(
            "{}/{}",
            diet.iter().filter(|r| r.success).count(),
            diet.len()
        );
        out.push(TaskReport {
            task: t.name.clone(),
            input_token_change_pct: pct_change(b.input_tokens_median, d.input_tokens_median),
            tool_call_change_pct: pct_change(b.tool_calls_median, d.tool_calls_median),
            files_read_change_pct: pct_change(b.files_read_median, d.files_read_median),
            elapsed_change_pct: pct_change(Some(b.elapsed_median), Some(d.elapsed_median)),
            success_base: sb,
            success_diet: sd,
            base: b,
            diet: d,
        });
    }
    out
}

fn print_report(reports: &[TaskReport], runs: &[RunMetrics]) {
    println!("\nAgent work (medians; ranges show min–max across reps)\n");
    println!("{:<24} {:>12} {:>12}", "", "BASE", "DIET");
    for r in reports {
        let range = |task: &str, branch: &str, f: fn(&RunMetrics) -> Option<f64>| {
            let xs: Vec<f64> = runs
                .iter()
                .filter(|x| x.task == task && x.branch == branch)
                .filter_map(f)
                .collect();
            if xs.is_empty() {
                return "n/a".to_string();
            }
            let lo = xs.iter().cloned().fold(f64::INFINITY, f64::min);
            let hi = xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            format!("{}–{}", fmt_opt(Some(lo)), fmt_opt(Some(hi)))
        };
        println!("\nTask: {}", r.task);
        println!(
            "{:<24} {:>12} {:>12}",
            "Input tokens",
            fmt_opt(r.base.input_tokens_median),
            fmt_opt(r.diet.input_tokens_median)
        );
        println!(
            "{:<24} {:>12} {:>12}",
            "  range",
            range(&r.task, "base", |x| x.input_tokens.map(|v| v as f64)),
            range(&r.task, "diet", |x| x.input_tokens.map(|v| v as f64))
        );
        println!(
            "{:<24} {:>12} {:>12}",
            "Tool calls",
            fmt_opt(r.base.tool_calls_median),
            fmt_opt(r.diet.tool_calls_median)
        );
        println!(
            "{:<24} {:>12} {:>12}",
            "  range",
            range(&r.task, "base", |x| x.tool_calls.map(|v| v as f64)),
            range(&r.task, "diet", |x| x.tool_calls.map(|v| v as f64))
        );
        println!(
            "{:<24} {:>12} {:>12}",
            "Files read",
            fmt_opt(r.base.files_read_median),
            fmt_opt(r.diet.files_read_median)
        );
        println!(
            "{:<24} {:>12.1} {:>12.1}",
            "Time (s)", r.base.elapsed_median, r.diet.elapsed_median
        );
        println!(
            "{:<24} {:>12} {:>12}",
            "Success", r.success_base, r.success_diet
        );
        println!();
        println!(
            "Input token change: {}",
            fmt_pct(r.input_token_change_pct, "token metrics unavailable")
        );
        println!(
            "Tool calls:         {}",
            fmt_pct(r.tool_call_change_pct, "tool-call metrics unavailable")
        );
        println!(
            "Files explored:     {}",
            fmt_pct(r.files_read_change_pct, "files-read metrics unavailable")
        );
        println!(
            "Success:              base {} vs diet {}",
            r.success_base, r.success_diet
        );
    }
    println!("\nNever claim causality beyond the actual experiment.");
}

fn fmt_opt(v: Option<f64>) -> String {
    match v {
        Some(x) => {
            if x >= 1000.0 {
                format!("{:.0}K", x / 1000.0)
            } else {
                format!("{:.0}", x)
            }
        }
        None => "n/a".to_string(),
    }
}

fn fmt_pct(v: Option<f64>, fallback: &str) -> String {
    match v {
        Some(x) => format!("{:+.1}%", x),
        None => fallback.to_string(),
    }
}
