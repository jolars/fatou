//! Core linter data types: diagnostics, fixes, and severities.

use std::path::PathBuf;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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

/// A lint finding anchored to a byte range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    pub path: Option<PathBuf>,
    pub start: usize,
    pub end: usize,
    pub rule: String,
    pub severity: Severity,
    pub message: String,
    pub fixes: Vec<Fix>,
    pub suppressed: bool,
}
