//! Applying a diagnostic's [`Fix`]es to source text.
//!
//! [`apply_fixes`] is the pure core: it takes the diagnostics produced from
//! `source` and rewrites the byte ranges their fixes name, gated by
//! [`Applicability`]. [`fix_source`] wraps it in a re-lint loop so that fixes
//! skipped for overlapping a neighbor (only one of an overlapping pair is
//! applied per pass) get another chance once the offsets have settled.
//!
//! A fix is not a formatter (see `AGENTS.md`): each fix must stay locally
//! legible on its own, but need not satisfy line width or canonical layout.

use std::path::Path;

use crate::config::LintConfig;
use crate::linter::check::check_source;
use crate::linter::diagnostic::{Applicability, Diagnostic, Fix};

/// The result of a single [`apply_fixes`] pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub output: String,
    pub applied: usize,
}

/// Apply the fixes carried by `diagnostics` to `source`.
///
/// Only [`Applicability::Safe`] fixes are applied unless `include_unsafe` is
/// set. Fixes are applied at most one-per-overlapping-region: sorted by start,
/// a fix is skipped when its range overlaps an already-accepted one. Accepted
/// fixes are applied right-to-left so earlier byte offsets stay valid.
pub fn apply_fixes(source: &str, diagnostics: &[Diagnostic], include_unsafe: bool) -> Applied {
    let mut fixes: Vec<&Fix> = diagnostics
        .iter()
        .flat_map(|diag| diag.fixes.iter())
        .filter(|fix| include_unsafe || fix.applicability == Applicability::Safe)
        .collect();

    // Stable order by start (then end) so overlap resolution is deterministic.
    fixes.sort_by_key(|fix| (fix.start, fix.end));

    let mut accepted: Vec<&Fix> = Vec::with_capacity(fixes.len());
    let mut last_end = 0usize;
    for fix in fixes {
        if fix.start >= last_end {
            last_end = fix.end;
            accepted.push(fix);
        }
    }

    let mut output = source.to_string();
    // Right-to-left: later replacements don't shift earlier offsets.
    for fix in accepted.iter().rev() {
        output.replace_range(fix.start..fix.end, &fix.content);
    }

    Applied {
        output,
        applied: accepted.len(),
    }
}

/// The outcome of driving [`apply_fixes`] to a fixpoint over `text`.
#[derive(Debug, Clone)]
pub struct FixOutcome {
    /// The fixed source (equal to the input when nothing was applied).
    pub output: String,
    /// Total fixes applied across all passes.
    pub applied: usize,
    /// Diagnostics still present after the last pass (nothing left to fix, or
    /// unfixable / opted-out findings).
    pub remaining: Vec<Diagnostic>,
}

/// Guards against a fix that keeps re-triggering its own rule.
const MAX_PASSES: usize = 10;

/// Lint `text` under `config` and apply its fixes, re-linting until no further
/// fix applies (or [`MAX_PASSES`] is hit). `path` only labels the diagnostics.
pub fn fix_source(
    path: Option<&Path>,
    text: &str,
    config: &LintConfig,
    include_unsafe: bool,
) -> FixOutcome {
    let mut current = text.to_string();
    let mut total = 0usize;

    for _ in 0..MAX_PASSES {
        let report = check_source(path, &current, config);
        let Applied { output, applied } =
            apply_fixes(&current, &report.diagnostics, include_unsafe);
        if applied == 0 {
            return FixOutcome {
                output: current,
                applied: total,
                remaining: report.diagnostics,
            };
        }
        total += applied;
        current = output;
    }

    // Hit the pass cap: report whatever remains on the settled text.
    let report = check_source(path, &current, config);
    FixOutcome {
        output: current,
        applied: total,
        remaining: report.diagnostics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rowan::TextRange;

    fn diag(fixes: Vec<Fix>) -> Diagnostic {
        Diagnostic {
            fixes,
            ..Diagnostic::new("test", TextRange::empty(0.into()), "")
        }
    }

    fn fix(start: usize, end: usize, content: &str, applicability: Applicability) -> Fix {
        Fix {
            description: String::new(),
            content: content.to_string(),
            start,
            end,
            applicability,
        }
    }

    #[test]
    fn single_safe_fix_is_applied() {
        let src = "if x = 5";
        let d = diag(vec![fix(5, 6, "==", Applicability::Safe)]);
        let out = apply_fixes(src, &[d], false);
        assert_eq!(out.output, "if x == 5");
        assert_eq!(out.applied, 1);
    }

    #[test]
    fn unsafe_fix_is_gated() {
        let src = "abc";
        let d = diag(vec![fix(0, 1, "X", Applicability::Unsafe)]);

        let skipped = apply_fixes(src, std::slice::from_ref(&d), false);
        assert_eq!(skipped.output, "abc");
        assert_eq!(skipped.applied, 0);

        let applied = apply_fixes(src, &[d], true);
        assert_eq!(applied.output, "Xbc");
        assert_eq!(applied.applied, 1);
    }

    #[test]
    fn two_disjoint_fixes_apply_with_correct_offsets() {
        // Replacements of differing lengths; right-to-left keeps offsets valid.
        let src = "a b c";
        let d = diag(vec![
            fix(0, 1, "AAAA", Applicability::Safe),
            fix(4, 5, "C", Applicability::Safe),
        ]);
        let out = apply_fixes(src, &[d], false);
        assert_eq!(out.output, "AAAA b C");
        assert_eq!(out.applied, 2);
    }

    #[test]
    fn overlapping_fixes_apply_only_the_first() {
        let src = "abcdef";
        let d = diag(vec![
            fix(0, 3, "X", Applicability::Safe),
            fix(2, 5, "Y", Applicability::Safe),
        ]);
        let out = apply_fixes(src, &[d], false);
        // Second fix overlaps [0,3) and is skipped this pass.
        assert_eq!(out.output, "Xdef");
        assert_eq!(out.applied, 1);
    }

    #[test]
    fn fix_source_reaches_a_fixpoint() {
        let config = LintConfig {
            select: Some(vec!["assignment-in-condition".to_string()]),
            ..Default::default()
        };
        let outcome = fix_source(None, "if x = 5\n    x\nend\n", &config, false);
        assert_eq!(outcome.output, "if x == 5\n    x\nend\n");
        assert_eq!(outcome.applied, 1);
        assert!(outcome.remaining.is_empty());

        // A second run over the fixed text is a no-op.
        let again = fix_source(None, &outcome.output, &config, false);
        assert_eq!(again.applied, 0);
        assert_eq!(again.output, outcome.output);
    }
}
