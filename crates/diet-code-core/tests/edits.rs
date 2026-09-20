use diet_code_core::edits::{
    apply_symbol_removals, prune_unused_imports, remove_byte_range, SymbolRemoval,
};

fn removal(file: &str, symbol: &str, start_byte: usize, end_byte: usize) -> SymbolRemoval {
    SymbolRemoval {
        file: file.to_string(),
        symbol: symbol.to_string(),
        start_line: 0,
        end_line: 0,
        start_byte,
        end_byte,
    }
}

#[test]
fn removes_full_declaration_lines() {
    let text = "import { a } from \"./x\";\n\nexport function used() {\n  return 1;\n}\n\nfunction dead() {\n  return 2;\n}\n\nexport const y = used();\n";
    let start = text.find("function dead()").unwrap();
    let end = text.find("}\n\nexport const").unwrap() + 1;
    let out = remove_byte_range(text, start, end).unwrap();
    assert!(
        !out.contains("dead"),
        "dead declaration must be gone:\n{}",
        out
    );
    assert!(
        out.contains("export function used()"),
        "unrelated code untouched:\n{}",
        out
    );
    assert!(
        out.contains("export const y"),
        "trailing code untouched:\n{}",
        out
    );
    // No half-lines remain.
    for line in out.lines() {
        assert!(
            !line.starts_with('}') || line == "}",
            "no orphan braces: {:?}",
            line
        );
    }
}

#[test]
fn does_not_reformat_unrelated_code() {
    let text = "const x=1;\nfunction dead( ){return  x;}\nconst y  =  2;\n";
    let start = text.find("function dead").unwrap();
    let end = text.find("return  x;}").unwrap() + "return  x;".len();
    let out = remove_byte_range(text, start, end).unwrap();
    assert_eq!(out, "const x=1;\nconst y  =  2;\n");
}

#[test]
fn rejects_stale_ranges() {
    assert!(remove_byte_range("hi", 5, 9).is_err());
    assert!(remove_byte_range("hi", 1, 1).is_err());
}

#[test]
fn applies_multiple_removals_stably() {
    let text = "function a() {\n  return 1;\n}\n\nfunction b() {\n  return 2;\n}\n\nfunction c() {\n  return 3;\n}\n";
    let mk = |name: &str| {
        let s = text.find(&format!("function {}()", name)).unwrap();
        let e = text[s..].find("}\n").unwrap() + s + 1;
        removal("f.ts", name, s, e)
    };
    let mut rs = vec![mk("a"), mk("c")];
    let out = apply_symbol_removals(text, &mut rs).unwrap();
    assert!(out.contains("function b()"), "b survives:\n{}", out);
    assert!(
        !out.contains("function a()") && !out.contains("function c()"),
        "a/c removed:\n{}",
        out
    );
}

#[test]
fn prunes_only_proven_unused_imports() {
    let text = "import { used, unused } from \"./x\";\nimport def from \"./y\";\nimport \"./side-effect\";\n\nused();\n";
    let out = prune_unused_imports(text);
    // `unused` shares a line with `used`, so the line must be kept (conservative).
    assert!(out.contains("./x"), "shared import line kept:\n{}", out);
    assert!(
        !out.contains("./y"),
        "fully unused default import pruned:\n{}",
        out
    );
    assert!(
        out.contains("./side-effect"),
        "side-effect import kept:\n{}",
        out
    );
}
