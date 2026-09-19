use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_diet-code"))
}

fn fixture_src(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
}

/// Copy a fixture into a fresh temp dir, init a git repo, commit. Returns the dir.
fn temp_repo(name: &str, tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("diet-code-cli-test-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    copy_dir(&fixture_src(name), &dir);
    git(&dir, &["init", "-q"]);
    git(&dir, &["config", "user.email", "test@test"]);
    git(&dir, &["config", "user.name", "test"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "fixture"]);
    dir
}

fn copy_dir(src: &Path, dst: &Path) {
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.path().is_dir() {
            std::fs::create_dir_all(&to).unwrap();
            copy_dir(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), to).unwrap();
        }
    }
}

fn git(dir: &Path, args: &[&str]) {
    let st = Command::new("git").args(args).current_dir(dir).status().unwrap();
    assert!(st.success(), "git {:?} failed", args);
}

fn run(args: &[&str]) -> (bool, String) {
    let out = Command::new(bin()).args(args).output().unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

#[test]
fn analyze_json_reports_fixture_finding() {
    let dir = temp_repo("unused-function", "analyze");
    let (ok, text) = run(&["analyze", dir.to_str().unwrap(), "--json", "--persist"]);
    assert!(ok, "analyze failed:\n{}", text);
    let v: serde_json::Value = serde_json::from_str(&text).expect("valid JSON report");
    assert_eq!(v["version"], 1);
    let findings = v["findings"].as_array().unwrap();
    assert!(
        findings.iter().any(|f| f["symbol"] == "unusedHelper" && f["kind"] == "dead_function"),
        "expected unusedHelper finding:\n{}",
        text
    );
}

#[test]
fn explain_shows_evidence() {
    let dir = temp_repo("unused-file", "explain");
    let (ok, text) = run(&["explain", "src/utils/old-auth.ts", "--path", dir.to_str().unwrap()]);
    assert!(ok, "explain failed:\n{}", text);
    assert!(text.contains("DEAD FILE"), "expected classification:\n{}", text);
    assert!(text.contains("Confidence"), "expected confidence:\n{}", text);
}

#[test]
fn clean_dry_run_proposes_patch_plan() {
    let dir = temp_repo("unused-function", "dryrun");
    let (ok, text) = run(&["clean", dir.to_str().unwrap(), "--dry-run"]);
    assert!(ok, "clean --dry-run failed:\n{}", text);
    assert!(text.contains("unusedHelper"), "plan must mention symbol:\n{}", text);
}

#[test]
fn clean_applies_deterministic_diff() {
    let dir = temp_repo("mixed-ts-js", "clean");
    let (ok, text) = run(&["clean", dir.to_str().unwrap()]);
    // No verify scripts in fixture -> skips verification, still applies.
    assert!(ok, "clean failed:\n{}", text);
    // g() removed, f() kept.
    let util = std::fs::read_to_string(dir.join("src/util.js")).unwrap();
    assert!(!util.contains("function g()"), "dead g() removed:\n{}", util);
    assert!(util.contains("function f()"), "live f() kept:\n{}", util);
    // On a branch, working tree reflects the diff.
    let branch = Command::new("git").args(["branch", "--show-current"]).current_dir(&dir).output().unwrap();
    assert!(String::from_utf8_lossy(&branch.stdout).contains("diet-code/"));
}

#[test]
fn benchmark_mock_runs_base_and_diet() {
    let dir = temp_repo("unused-function", "bench");
    let tasks = dir.join("tasks.json");
    std::fs::write(
        &tasks,
        r#"[{"name": "noop", "prompt": "Do nothing.", "verify": "true"}]"#,
    )
    .unwrap();
    let (ok, text) = run(&[
        "benchmark",
        "--path",
        dir.to_str().unwrap(),
        "--tasks",
        tasks.to_str().unwrap(),
        "--reps",
        "1",
        "--mock",
    ]);
    assert!(ok, "benchmark --mock failed:\n{}", text);
    assert!(text.contains("Success"), "expected report table:\n{}", text);
    // Report persisted.
    let entries: Vec<_> = std::fs::read_dir(dir.join(".diet-code"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("benchmark-"))
        .collect();
    assert!(!entries.is_empty(), "benchmark report must be persisted");
}
