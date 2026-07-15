//! Core linter data types: diagnostics, fixes, and severities.

use std::path::PathBuf;

use rowan::TextRange;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
            Severity::Hint => "hint",
        }
    }
}

/// Whether a fix is safe to apply automatically or needs explicit opt-in
/// (`--unsafe-fixes`), because it might change behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Applicability {
    Safe,
    Unsafe,
}

/// A single byte-range replacement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Fix {
    pub description: String,
    pub content: String,
    pub start: usize,
    pub end: usize,
    pub applicability: Applicability,
}

/// Render-ready violation metadata. `name` is the short title (typically the
/// rule ID); `body` is the one-line explanation; `suggestion` is an optional
/// follow-on hint (rendered as a `help:` note).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ViolationData {
    pub name: String,
    pub body: String,
    pub suggestion: Option<String>,
}

impl ViolationData {
    pub fn new(name: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            body: body.into(),
            suggestion: None,
        }
    }

    pub fn with_suggestion(mut self, hint: impl Into<String>) -> Self {
        self.suggestion = Some(hint.into());
        self
    }
}

/// A lint finding anchored to a source range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    /// Static rule ID (e.g. `"unused-binding"`).
    pub rule: &'static str,
    pub severity: Severity,
    pub path: Option<PathBuf>,
    /// Source range, in bytes.
    #[serde(serialize_with = "serialize_text_range")]
    pub range: TextRange,
    pub message: ViolationData,
    pub fixes: Vec<Fix>,
}

fn serialize_text_range<S: serde::Serializer>(
    range: &TextRange,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeStruct;
    let mut s = serializer.serialize_struct("Range", 2)?;
    s.serialize_field("start", &u32::from(range.start()))?;
    s.serialize_field("end", &u32::from(range.end()))?;
    s.end()
}

impl Diagnostic {
    /// A finding for `rule` spanning `range`, with `message` as the violation
    /// body (the name defaults to the rule ID). `path` and `severity` are
    /// stamped centrally by the engine after the rule runs (see
    /// `ResolvedRules::run`), so rules never set either; the values here
    /// are placeholders.
    pub fn new(rule: &'static str, range: TextRange, message: impl Into<String>) -> Self {
        Self {
            rule,
            severity: Severity::Warning,
            path: None,
            range,
            message: ViolationData::new(rule, message),
            fixes: Vec::new(),
        }
    }
}
