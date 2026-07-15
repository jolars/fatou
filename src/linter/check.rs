//! The linting pipeline: parse each file, run the enabled rules, and report a
//! status. Parse diagnostics block the rules for a file (they need a clean
//! tree) but are still reported, under the [`PARSE_ERROR_RULE`] pseudo-rule.

use std::path::{Path, PathBuf};

use rayon::prelude::*;

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use crate::config::LintConfig;
use crate::file_discovery::{FileDiscoveryError, collect_julia_files};
use crate::index::{PackageIndex, build_system_index};
use rowan::TextRange;

use crate::linter::diagnostic::{Diagnostic, Severity, ViolationData};
use crate::linter::rules::{ResolutionContext, ResolvedRules, RuleContext};
use crate::linter::suppression::SuppressionMap;
use crate::parser::parse;
use crate::semantic::SemanticModel;
use crate::syntax::SyntaxNode;
use crate::text::LineIndex;

/// The pseudo-rule id under which parse diagnostics are reported.
pub const PARSE_ERROR_RULE: &str = "parse-error";

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
    /// `select`/`ignore` entries that name no shipped rule (likely typos).
    pub unknown_rules: Vec<String>,
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
    let (rules, unknown_rules) = ResolvedRules::resolve(config);

    // Files are independent; lint them in parallel. `collect` into an ordered
    // Vec keeps the sorted discovery order for deterministic reporting.
    let reports = files
        .par_iter()
        .map(|path| {
            let text = std::fs::read_to_string(path).map_err(|err| LintError::Io {
                path: path.clone(),
                message: err.to_string(),
            })?;
            Ok(check_text(Some(path), &text, &rules))
        })
        .collect::<Result<Vec<_>, LintError>>()?;

    let total_findings = reports
        .iter()
        .filter_map(|report| match report.status {
            LintStatus::Findings { count } => Some(count),
            _ => None,
        })
        .sum();

    Ok(LintResult {
        checked_files: files.len(),
        total_findings,
        reports,
        unknown_rules,
    })
}

/// Lint an in-memory document with no path (e.g. stdin).
pub fn check_document(text: &str) -> LintFileReport {
    let (rules, _) = ResolvedRules::resolve(&LintConfig::default());
    check_text(None, text, &rules)
}

/// Lint `text` under `config`, attributing findings to `path`. Used by the docs
/// generator (`crate::linter::docs`) to render each example's real diagnostics.
pub fn check_source(path: Option<&Path>, text: &str, config: &LintConfig) -> LintFileReport {
    let (rules, _) = ResolvedRules::resolve(config);
    check_text(path, text, &rules)
}

/// Core single-file pass: parse, run rules on a clean tree, filter suppressed
/// findings.
fn check_text(path: Option<&Path>, text: &str, rules: &ResolvedRules) -> LintFileReport {
    let parsed = parse(text);
    if !parsed.diagnostics.is_empty() {
        let diagnostics = parsed
            .diagnostics
            .iter()
            .map(|diag| Diagnostic {
                rule: PARSE_ERROR_RULE,
                severity: Severity::Error,
                path: path.map(Path::to_path_buf),
                range: TextRange::new((diag.start as u32).into(), (diag.end as u32).into()),
                message: ViolationData::new(PARSE_ERROR_RULE, diag.message.clone()),
                fixes: Vec::new(),
            })
            .collect();
        return LintFileReport {
            path: path.map(Path::to_path_buf),
            status: LintStatus::ParseDiagnostics {
                count: parsed.diagnostics.len(),
            },
            diagnostics,
        };
    }

    let model = SemanticModel::build(&parsed.cst);
    // The CLI resolves free reads against the built-in Base/Core export
    // snapshot — deterministic and cheap, where harvesting a real install per
    // lint run would not be. No workspace context: a bare file may be an
    // `include`d fragment, which is exactly why `undefined-name` is opt-in
    // here (the language server, which knows the workspace, enables it).
    let resolution = Some(ResolutionContext {
        packages: system_snapshot(),
        workspace: None,
    });
    let diagnostics = lint_parsed(path, text, &parsed.cst, &model, rules, resolution);

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

/// The built-in Base/Core export snapshot, built once per process. The
/// resolution floor for CLI lint runs (and docs generation), where locating
/// and harvesting a real Julia install would be slow and nondeterministic.
fn system_snapshot() -> &'static BTreeMap<String, Arc<PackageIndex>> {
    static SNAPSHOT: OnceLock<BTreeMap<String, Arc<PackageIndex>>> = OnceLock::new();
    SNAPSHOT.get_or_init(|| build_system_index(None))
}

/// Run `rules` against an already-parsed *clean* tree (rules need one; the
/// caller is responsible for gating on parse diagnostics) and filter suppressed
/// findings. Shared by [`check_text`] and the language server, whose warm path
/// lints off the salsa-cached tree and model instead of re-parsing (and passes
/// its own harvested library as the `resolution` context).
pub fn lint_parsed(
    path: Option<&Path>,
    text: &str,
    root: &SyntaxNode,
    model: &SemanticModel,
    rules: &ResolvedRules,
    resolution: Option<ResolutionContext<'_>>,
) -> Vec<Diagnostic> {
    let ctx = RuleContext {
        path,
        root,
        model,
        resolution,
    };
    let raw = rules.run(&ctx);

    let suppressions = SuppressionMap::build(text);
    let line_index = LineIndex::new(text);
    raw.into_iter()
        .filter(|diag| {
            let line = line_index.byte_to_lc(diag.range.start().into()).line;
            !suppressions.is_suppressed(diag.rule, line)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_file_reports_clean() {
        let report = check_document("x = 1\n");
        assert_eq!(report.status, LintStatus::Clean);
    }

    #[test]
    fn parse_diagnostics_are_surfaced() {
        let report = check_document("f (x)\n");
        assert!(matches!(
            report.status,
            LintStatus::ParseDiagnostics { count } if count > 0
        ));
        assert_eq!(report.diagnostics.len(), 1);
        let diag = &report.diagnostics[0];
        assert_eq!(diag.rule, PARSE_ERROR_RULE);
        assert_eq!(diag.severity, Severity::Error);
        assert!(!diag.message.body.is_empty());
    }
}
