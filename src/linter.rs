//! The linter: parse a file, run rules over the CST, and report findings.
//!
//! Groundwork phase: the rule trait, registry, suppression directives
//! (`# fatou-ignore`), and rendering are in place, but no rules ship yet — a
//! clean file reports [`LintStatus::Clean`] and a file with parse errors reports
//! [`LintStatus::ParseDiagnostics`]. Rules land in a later phase (`TODO.md`).

pub mod check;
pub mod diagnostic;
pub mod render;
pub mod rules;
pub mod suppression;

pub use check::{
    LintError, LintFileReport, LintResult, LintStatus, check_document, check_paths,
    check_paths_with_config,
};
pub use diagnostic::{Applicability, Diagnostic, Fix, Severity};
pub use render::{OutputMode, render_findings};
pub use rules::{ResolvedRules, Rule, RuleContext};
