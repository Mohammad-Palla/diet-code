use serde::{Deserialize, Serialize};

/// How a name is imported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportKind {
    Named,
    Default,
    Namespace,
    SideEffect,
    Require,
    DynamicLiteral,
}

/// A single resolved-or-not import record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportRec {
    /// Slash-separated importer file (relative).
    pub from_file: String,
    /// Raw module specifier as written, e.g. `./foo`.
    pub source_raw: String,
    /// Resolved slash-separated relative path, if resolvable to a repo file.
    pub resolved_file: Option<String>,
    /// Local binding name in the importer.
    pub local_name: String,
    /// Original exported name in the source module (`default`, `*`, or symbol name).
    pub original_name: String,
    pub is_type_only: bool,
    pub kind: ImportKind,
    pub line: usize,
}

/// A re-export record (`export ... from "..."`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReExportRec {
    pub from_file: String,
    pub source_raw: String,
    pub resolved_file: Option<String>,
    /// Name in the source module (`*` for `export *`).
    pub original_name: String,
    /// Name exposed by the re-exporting file.
    pub exported_name: String,
    pub is_type_only: bool,
    pub is_wildcard: bool,
    pub line: usize,
}

/// Value vs type reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum RefKind {
    Value,
    Type,
}

/// How a reference was observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefContext {
    Call,
    New,
    Identifier,
    Member,
    ThisMethod,
    Jsx,
    TypePosition,
    ReExport,
    Import,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceRec {
    /// File where the reference occurs (relative slash path).
    pub from_file: String,
    /// Enclosing symbol id, if inside a symbol body; None = top-level.
    pub from_symbol: Option<String>,
    /// Referenced name (identifier or method/JSX name).
    pub name: String,
    /// For member refs, the base text (`obj` in `obj.foo`), if known.
    pub base: Option<String>,
    pub kind: RefKind,
    pub context: RefContext,
    pub line: usize,
}
