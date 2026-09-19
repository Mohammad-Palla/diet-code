use serde::{Deserialize, Serialize};

use crate::confidence::Confidence;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    DeadFile,
    DeadFunction,
    DeadClass,
    DeadMethod,
    DeadVariable,
    DeadType,
}

impl FindingKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            FindingKind::DeadFile => "dead_file",
            FindingKind::DeadFunction => "dead_function",
            FindingKind::DeadClass => "dead_class",
            FindingKind::DeadMethod => "dead_method",
            FindingKind::DeadVariable => "dead_variable",
            FindingKind::DeadType => "dead_type",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitEvidence {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_seen: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_author: Option<String>,
    #[serde(default)]
    pub commit_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub id: String,
    #[serde(rename = "kind")]
    pub kind: FindingKind,
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(rename = "startLine")]
    pub start_line: usize,
    #[serde(rename = "endLine")]
    pub end_line: usize,
    #[serde(skip)]
    pub start_byte: usize,
    #[serde(skip)]
    pub end_byte: usize,
    pub confidence: Confidence,
    #[serde(rename = "productionReachable")]
    pub production_reachable: bool,
    #[serde(rename = "testReachable")]
    pub test_reachable: bool,
    #[serde(rename = "referenceCount")]
    pub reference_count: usize,
    pub reasons: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git: Option<GitEvidence>,
}

impl Finding {
    pub fn short_label(&self) -> String {
        match &self.symbol {
            Some(s) => format!("{} {}:{}", self.kind.as_str(), self.file, s),
            None => format!("{} {}", self.kind.as_str(), self.file),
        }
    }
}
