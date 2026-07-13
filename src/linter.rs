//! The linter: parse a file, build the semantic model, run rules over the CST
//! in one shared pass, and report findings.
//!
//! The linter is purely *semantic*: any check the formatter's `--check` mode can
//! perform belongs to the formatter, not here (see `AGENTS.md`). Rules consume a
//! [`crate::semantic::SemanticModel`] and the CST shape and emit
//! [`Diagnostic`]s; suppression (`# fatou-ignore`) is applied at the check
//! layer. Each rule carries a `description` and worked `examples`, from which
//! the rule-reference pages are generated (`docs`), so the docs cannot drift
//! from behavior.

pub mod check;
pub mod diagnostic;
pub mod docs;
pub mod fix;
pub mod render;
pub mod rules;
pub mod suppression;

pub use check::{
    LintError, LintFileReport, LintResult, LintStatus, check_document, check_paths,
    check_paths_with_config, check_source, lint_parsed,
};
pub use diagnostic::{Applicability, Diagnostic, Fix, Severity};
pub use docs::render_rule_doc;
pub use fix::{Applied, FixOutcome, apply_fixes, fix_source};
pub use render::{OutputMode, render_findings};
pub use rules::{Example, ResolvedRules, Rule, RuleContext, all_rule_ids, all_rules};
