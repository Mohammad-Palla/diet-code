use diet_code_core::confidence::Confidence;
use diet_code_core::findings::FindingKind;
use diet_code_core::{DeadCodeDetector, TreeSitterDetector};
use std::path::PathBuf;

fn fixture_root(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
}

fn analyze(name: &str) -> diet_code_core::AnalysisResult {
    let root = fixture_root(name);
    TreeSitterDetector.analyze(&root).expect("analyze fixture")
}

fn kinds(result: &diet_code_core::AnalysisResult) -> Vec<(FindingKind, String, Confidence)> {
    result
        .findings
        .iter()
        .map(|f| {
            (
                f.kind.clone(),
                f.symbol.clone().unwrap_or_else(|| f.file.clone()),
                f.confidence,
            )
        })
        .collect()
}

fn has(result: &diet_code_core::AnalysisResult, kind: FindingKind, symbol_or_file: &str) -> bool {
    result.findings.iter().any(|f| {
        f.kind == kind && (f.symbol.as_deref() == Some(symbol_or_file) || f.file == symbol_or_file)
    })
}

fn auto_removable(
    result: &diet_code_core::AnalysisResult,
) -> Vec<&diet_code_core::findings::Finding> {
    result
        .findings
        .iter()
        .filter(|f| f.confidence.auto_removable())
        .collect()
}

#[test]
fn unused_function_is_certain() {
    let r = analyze("unused-function");
    assert!(
        has(&r, FindingKind::DeadFunction, "unusedHelper"),
        "expected dead unusedHelper, got {:?}",
        kinds(&r)
    );
    let f = r
        .findings
        .iter()
        .find(|f| f.symbol.as_deref() == Some("unusedHelper"))
        .unwrap();
    assert_eq!(f.confidence, Confidence::Certain);
    assert!(
        !has(&r, FindingKind::DeadFunction, "usedFunc"),
        "usedFunc must not be dead: {:?}",
        kinds(&r)
    );
}

#[test]
fn unused_class_is_certain() {
    let r = analyze("unused-class");
    assert!(
        has(&r, FindingKind::DeadClass, "OldAuth"),
        "got {:?}",
        kinds(&r)
    );
    assert!(!has(&r, FindingKind::DeadClass, "Used"));
}

#[test]
fn unused_file_is_reported() {
    let r = analyze("unused-file");
    assert!(
        has(&r, FindingKind::DeadFile, "src/utils/old-auth.ts"),
        "got {:?}",
        kinds(&r)
    );
    let f = r
        .findings
        .iter()
        .find(|f| f.file == "src/utils/old-auth.ts")
        .unwrap();
    assert!(
        f.confidence.auto_removable(),
        "dead file should be auto-removable"
    );
    assert!(!has(&r, FindingKind::DeadFile, "src/main.ts"));
}

#[test]
fn barrel_export_marks_reexported_symbol_live() {
    let r = analyze("barrel-export");
    assert!(
        auto_removable(&r).is_empty(),
        "barrel re-export must keep foo live: {:?}",
        kinds(&r)
    );
}

#[test]
fn alias_import_resolves_tsconfig_paths() {
    let r = analyze("alias-import");
    assert!(
        auto_removable(&r).is_empty(),
        "aliased import must resolve: {:?}",
        kinds(&r)
    );
}

#[test]
fn default_export_resolves() {
    let r = analyze("default-export");
    assert!(
        auto_removable(&r).is_empty(),
        "default import must resolve: {:?}",
        kinds(&r)
    );
}

#[test]
fn namespace_import_resolves_members() {
    let r = analyze("namespace-import");
    assert!(
        !has(&r, FindingKind::DeadFunction, "helper"),
        "got {:?}",
        kinds(&r)
    );
}

#[test]
fn type_only_import_counts_as_reference() {
    let r = analyze("type-only-import");
    assert!(
        !has(&r, FindingKind::DeadType, "User"),
        "type-only use must keep User live: {:?}",
        kinds(&r)
    );
}

#[test]
fn jsx_reference_counts() {
    let r = analyze("jsx-reference");
    assert!(
        auto_removable(&r).is_empty(),
        "JSX usage must keep Card live: {:?}",
        kinds(&r)
    );
}

#[test]
fn class_method_this_and_instance_calls() {
    let r = analyze("class-method");
    // `dead` is never called; its class is live and unexported -> CERTAIN.
    assert!(
        has(&r, FindingKind::DeadMethod, "dead"),
        "got {:?}",
        kinds(&r)
    );
    let f = r
        .findings
        .iter()
        .find(|f| f.symbol.as_deref() == Some("dead"))
        .unwrap();
    assert_eq!(f.confidence, Confidence::Certain);
    // live/help/run must not be dead.
    for name in ["live", "help", "run"] {
        assert!(
            !has(&r, FindingKind::DeadMethod, name),
            "{} must be live: {:?}",
            name,
            kinds(&r)
        );
    }
    assert!(!has(&r, FindingKind::DeadFunction, "run"));
}

#[test]
fn dynamic_import_literal_is_resolvable() {
    let r = analyze("dynamic-import-literal");
    assert!(
        !has(&r, FindingKind::DeadFile, "src/plugin.ts"),
        "literal dynamic import must keep plugin live: {:?}",
        kinds(&r)
    );
    assert!(auto_removable(&r).is_empty(), "got {:?}", kinds(&r));
}

#[test]
fn dynamic_import_variable_protects_targets() {
    let r = analyze("dynamic-import-variable");
    // Plugins under the dynamic prefix must never be CERTAIN/HIGH.
    for f in r
        .findings
        .iter()
        .filter(|f| f.file.starts_with("src/plugins/"))
    {
        assert!(
            !f.confidence.auto_removable(),
            "dynamic-protected file must not be auto-removable: {:?}",
            f
        );
    }
}

#[test]
fn commonjs_require_binds_names() {
    let r = analyze("commonjs-require");
    assert!(
        !has(&r, FindingKind::DeadFunction, "f"),
        "required f must be live: {:?}",
        kinds(&r)
    );
    assert!(auto_removable(&r).is_empty(), "got {:?}", kinds(&r));
}

#[test]
fn package_exports_are_conservative() {
    let r = analyze("package-exports");
    assert!(
        auto_removable(&r).is_empty(),
        "package-exported surface must not be auto-removable: {:?}",
        kinds(&r)
    );
    assert!(
        has(&r, FindingKind::DeadFunction, "bonus"),
        "bonus should still be reported (LOW): {:?}",
        kinds(&r)
    );
}

#[test]
fn test_only_reference_is_not_certain() {
    let r = analyze("test-only-reference");
    assert!(
        !r.findings
            .iter()
            .any(|f| f.confidence == Confidence::Certain),
        "test-used code must never be CERTAIN: {:?}",
        kinds(&r)
    );
    // Production-dead file referenced only by tests is HIGH per the confidence model.
    assert!(
        has(&r, FindingKind::DeadFile, "src/util.ts"),
        "got {:?}",
        kinds(&r)
    );
}

#[test]
fn public_library_is_conservative() {
    let r = analyze("public-library");
    assert!(
        auto_removable(&r).is_empty(),
        "public API must not be auto-removable: {:?}",
        kinds(&r)
    );
}

#[test]
fn circular_imports_stay_reachable() {
    let r = analyze("circular-import");
    assert!(
        r.findings.is_empty(),
        "circular but reachable code must be clean: {:?}",
        kinds(&r)
    );
}

#[test]
fn re_export_chain_resolves() {
    let r = analyze("re-export-chain");
    assert!(
        auto_removable(&r).is_empty(),
        "re-export chain must stay live: {:?}",
        kinds(&r)
    );
}

#[test]
fn nested_unused_function_is_certain() {
    let r = analyze("nested-functions");
    assert!(
        has(&r, FindingKind::DeadFunction, "inner"),
        "got {:?}",
        kinds(&r)
    );
    let f = r
        .findings
        .iter()
        .find(|f| f.symbol.as_deref() == Some("inner"))
        .unwrap();
    assert_eq!(f.confidence, Confidence::Certain);
    assert!(!has(&r, FindingKind::DeadFunction, "innerUsed"));
    assert!(!has(&r, FindingKind::DeadFunction, "outer"));
}

#[test]
fn mixed_ts_js_cross_references() {
    let r = analyze("mixed-ts-js");
    assert!(
        has(&r, FindingKind::DeadFunction, "g"),
        "got {:?}",
        kinds(&r)
    );
    assert!(!has(&r, FindingKind::DeadFunction, "f"));
}

/// A project with no package.json whose frontend is a nested ES-module tree
/// (`web/src/main.js`) loaded via an HTML `<script src>` bundle. Before the
/// entry-discovery fix, transitively-imported modules were falsely flagged
/// HIGH `dead_file` ("0 production importers"). Both sides must hold:
/// transitively-reachable modules are LIVE, a true orphan is still reported.
#[test]
fn html_entry_web_reachability() {
    let r = analyze("html-entry-web");

    // Live side: nothing on the reachable chain is a dead file.
    for live in [
        "web/app.js",
        "web/src/main.js",
        "web/src/feature.js",
        "web/src/deep.js",
    ] {
        assert!(
            !has(&r, FindingKind::DeadFile, live),
            "{live} must be live (reachable), got {:?}",
            kinds(&r)
        );
    }

    // Specifically: the transitively-imported module must never be auto-removable.
    assert!(
        !auto_removable(&r)
            .iter()
            .any(|f| f.file == "web/src/deep.js"),
        "transitively-imported deep.js must never be auto-removable: {:?}",
        kinds(&r)
    );

    // Dead side: the true orphan is still reported.
    assert!(
        has(&r, FindingKind::DeadFile, "web/src/orphan.js"),
        "orphan.js must be reported dead, got {:?}",
        kinds(&r)
    );
}

/// A package whose `main`/`exports` point at built output in `distribution/`
/// while the real sources live in `source/` (the sindresorhus layout, e.g.
/// `ky`). Before the dist->src entry mapping covered `distribution/` +
/// `source/`, the package entry resolved to nothing, so the entire `source/`
/// tree was flagged HIGH `dead_file`. Both sides must hold: reachable source
/// stays live, a true orphan is still reported.
#[test]
fn dist_entry_maps_to_source_tree() {
    let r = analyze("dist-entry-package");

    for live in [
        "source/index.ts",
        "source/utils/normalize.ts",
        "source/core/constants.ts",
    ] {
        assert!(
            !has(&r, FindingKind::DeadFile, live),
            "{live} must be live (reachable via distribution->source entry), got {:?}",
            kinds(&r)
        );
    }
    assert!(
        !auto_removable(&r)
            .iter()
            .any(|f| f.file == "source/core/constants.ts"),
        "transitively-imported constants.ts must never be auto-removable: {:?}",
        kinds(&r)
    );

    // The genuine orphan is still reported.
    assert!(
        has(&r, FindingKind::DeadFile, "source/utils/orphan.ts"),
        "orphan.ts must be reported dead, got {:?}",
        kinds(&r)
    );
}

/// Differential validation oracle (development only).
/// If `fossil-mcp` is installed locally it is used as a test oracle to surface
/// missing graph edges / false positives. Never required: all other tests must
/// pass without it, and the shipped tool never invokes it.
#[test]
#[ignore]
fn differential_against_fossil_mcp_if_present() {
    if std::process::Command::new("fossil-mcp")
        .arg("--help")
        .output()
        .is_err()
    {
        eprintln!("fossil-mcp not installed; skipping differential oracle");
        return;
    }
    // Oracle present: compare on a subset of fixtures and log disagreements for
    // human review. Disagreements never fail the suite (oracle, not ground truth).
    for name in [
        "unused-function",
        "barrel-export",
        "namespace-import",
        "circular-import",
    ] {
        let root = fixture_root(name);
        let ours = analyze(name);
        let out = std::process::Command::new("fossil-mcp")
            .arg("analyze")
            .arg(&root)
            .output();
        match out {
            Ok(o) => {
                eprintln!(
                    "fixture {}: diet-code={} findings; fossil-mcp exit={} stdout_bytes={}",
                    name,
                    ours.findings.len(),
                    o.status,
                    o.stdout.len()
                );
            }
            Err(e) => eprintln!("fixture {}: fossil-mcp invocation failed: {}", name, e),
        }
    }
}
