use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Kind of extracted source entity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    FunctionExpression,
    ArrowFunction,
    Class,
    Method,
    Variable,
    Enum,
    Interface,
    TypeAlias,
    Namespace,
}

impl SymbolKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SymbolKind::Function => "function",
            SymbolKind::FunctionExpression => "function_expression",
            SymbolKind::ArrowFunction => "arrow_function",
            SymbolKind::Class => "class",
            SymbolKind::Method => "method",
            SymbolKind::Variable => "variable",
            SymbolKind::Enum => "enum",
            SymbolKind::Interface => "interface",
            SymbolKind::TypeAlias => "type",
            SymbolKind::Namespace => "namespace",
        }
    }

    /// Whether this kind represents executable code (vs pure types).
    pub fn is_value_level(&self) -> bool {
        match self {
            SymbolKind::Interface | SymbolKind::TypeAlias => false,
            SymbolKind::Enum => true, // enums emit runtime code in TS
            _ => true,
        }
    }
}

/// A single extracted declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    /// Stable id, e.g. `src/foo.ts::function:bar` or
    /// `src/foo.ts::class:Cls::method:m`.
    pub id: String,
    /// Slash-separated path relative to repository root.
    pub file: String,
    /// Absolute path (not serialized by default consumers, but useful internally).
    #[serde(skip)]
    pub abs_path: PathBuf,
    pub name: String,
    pub kind: SymbolKind,
    pub start_line: usize, // 1-based
    pub end_line: usize,   // 1-based, inclusive
    pub start_byte: usize,
    pub end_byte: usize,
    pub exported: bool,
    #[serde(default)]
    pub is_default_export: bool,
    pub parent: Option<String>,
}

impl Entity {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        rel_file: &str,
        abs_path: PathBuf,
        name: &str,
        kind: SymbolKind,
        start_line: usize,
        end_line: usize,
        start_byte: usize,
        end_byte: usize,
        exported: bool,
        parent: Option<String>,
    ) -> Self {
        // IDs embed the FULL parent chain (`file::function:f::variable:x`) so
        // that same-named declarations in different scopes never collide.
        // (Truncating to the last segment corrupts scope attribution.)
        let id = match &parent {
            Some(p) => format!("{}::{}:{}", p, kind.as_str(), name),
            None => format!("{}::{}:{}", rel_file, kind.as_str(), name),
        };
        Self {
            id,
            file: rel_file.to_string(),
            abs_path,
            name: name.to_string(),
            kind,
            start_line,
            end_line,
            start_byte,
            end_byte,
            exported,
            is_default_export: false,
            parent,
        }
    }
}
