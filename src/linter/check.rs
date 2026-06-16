//! The linting pipeline: parse each file, run the enabled rules, and report a
//! status. Parse diagnostics block linting a file (the rules need a clean tree).

use std::path::{Path, PathBuf};

use crate::config::LintConfig;
use crate::file_discovery::{FileDiscoveryError, collect_julia_files};
use crate::linter::diagnostic::Diagnostic;
use crate::linter::rules::{ResolvedRules, RuleContext};
use crate::linter::suppression::SuppressionMap;
use crate::parser::parse;
use crate::text::LineIndex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LintStatus {
    Clean,
    Findings { count: usize },
    ParseDiagnostics { count: usize },
}

#[derive(Debug, Clone)]
pub struct LintFileReport {
    pub path: Option<PathBuf>,
    pub status: LintStatus,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone)]
pub struct LintResult {
    pub checked_files: usize,
    pub total_findings: usize,
    pub reports: Vec<LintFileReport>,
}

#[derive(Debug)]
pub enum LintError {
    Discovery(FileDiscoveryError),
    Io { path: PathBuf, message: String },
}

impl std::fmt::Display for LintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LintError::Discovery(err) => write!(f, "{err}"),
            LintError::Io { path, message } => {
                write!(f, "failed to read {}: {message}", path.display())
            }
        }
    }
}

impl std::error::Error for LintError {}

/// Lint every `.jl` file under `paths` with default configuration.
pub fn check_paths(paths: &[PathBuf]) -> Result<LintResult, LintError> {
    check_paths_with_config(paths, &LintConfig::default())
}

/// Lint every `.jl` file under `paths`, honoring `config`.
pub fn check_paths_with_config(
    paths: &[PathBuf],
    config: &LintConfig,
) -> Result<LintResult, LintError> {
    let files = collect_julia_files(paths).map_err(LintError::Discovery)?;
    let rules = ResolvedRules::resolve(config.select.as_deref(), &config.ignore);

    let mut reports = Vec::with_capacity(files.len());
    let mut total_findings = 0;

    for path in &files {
        let text = std::fs::read_to_string(path).map_err(|err| LintError::Io {
            path: path.clone(),
            message: err.to_string(),
        })?;
        let report = check_text(Some(path), &text, &rules);
        if let LintStatus::Findings { count } = report.status {
            total_findings += count;
        }
        reports.push(report);
    }

    Ok(LintResult {
        checked_files: files.len(),
        total_findings,
        reports,
    })
}

/// Lint an in-memory document with no path (e.g. stdin).
pub fn check_document(text: &str) -> LintFileReport {
    let rules = ResolvedRules::resolve(None, &[]);
    check_text(None, text, &rules)
}

/// Core single-file pass: parse, run rules on a clean tree, filter suppressed
/// findings.
fn check_text(path: Option<&Path>, text: &str, rules: &ResolvedRules) -> LintFileReport {
    let parsed = parse(text);
    if !parsed.diagnostics.is_empty() {
        return LintFileReport {
            path: path.map(Path::to_path_buf),
            status: LintStatus::ParseDiagnostics {
                count: parsed.diagnostics.len(),
            },
            diagnostics: Vec::new(),
        };
    }

    let ctx = RuleContext {
        path,
        root: &parsed.cst,
    };
    let raw = rules.run(&ctx);

    let suppressions = SuppressionMap::build(text);
    let line_index = LineIndex::new(text);
    let diagnostics: Vec<Diagnostic> = raw
        .into_iter()
        .filter(|diag| {
            let line = line_index.byte_to_lc(diag.start).line;
            !suppressions.is_suppressed(&diag.rule, line)
        })
        .collect();

    let status = if diagnostics.is_empty() {
        LintStatus::Clean
    } else {
        LintStatus::Findings {
            count: diagnostics.len(),
        }
    };

    LintFileReport {
        path: path.map(Path::to_path_buf),
        status,
        diagnostics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_file_reports_clean() {
        let report = check_document("x = 1\n");
        assert_eq!(report.status, LintStatus::Clean);
    }
}
