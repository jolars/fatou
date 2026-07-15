//! End-to-end coverage for the autofix engine (`apply_fixes` / `fix_source`):
//! real fixes driven through the linter, plus applicability gating over
//! synthetic diagnostics.

use fatou::config::LintConfig;
use fatou::linter::{Applicability, Diagnostic, Fix, apply_fixes, check_source, fix_source};

fn select(rule: &str) -> LintConfig {
    LintConfig {
        select: Some(vec![rule.to_string()]),
        ..Default::default()
    }
}

/// A whole file with several fixable findings converges in one `fix_source`
/// call, each `=` becoming `==`, leaving no findings behind.
#[test]
fn fixes_every_assignment_in_condition() {
    let src = "\
if a = 1
    while b = 2
        b
    end
end
";
    let outcome = fix_source(None, src, &select("assignment-in-condition"), false);
    insta::assert_snapshot!(outcome.output, @r"
    if a == 1
        while b == 2
            b
        end
    end
    ");
    assert_eq!(outcome.applied, 2);
    assert!(outcome.remaining.is_empty());
}

/// The safe `=` -> `==` fix is applied without opting into unsafe fixes.
#[test]
fn safe_fix_applies_by_default() {
    let src = "if x = 5\n    x\nend\n";
    let report = check_source(None, src, &select("assignment-in-condition"));
    let applied = apply_fixes(src, &report.diagnostics, false);
    assert_eq!(applied.output, "if x == 5\n    x\nend\n");
    assert_eq!(applied.applied, 1);
}

/// An unsafe fix is withheld by default and applied only with `include_unsafe`.
#[test]
fn unsafe_fix_requires_opt_in() {
    let diag = Diagnostic {
        fixes: vec![Fix {
            description: "rewrite".to_string(),
            content: "xyz".to_string(),
            start: 0,
            end: 3,
            applicability: Applicability::Unsafe,
        }],
        ..Diagnostic::new("synthetic", rowan::TextRange::new(0.into(), 3.into()), "")
    };

    let withheld = apply_fixes("abc", std::slice::from_ref(&diag), false);
    assert_eq!(withheld.output, "abc");
    assert_eq!(withheld.applied, 0);

    let opted_in = apply_fixes("abc", &[diag], true);
    assert_eq!(opted_in.output, "xyz");
    assert_eq!(opted_in.applied, 1);
}

/// `nothing-comparison` rewrites `==`/`!=` against `nothing` to the identity
/// operators `===`/`!==` in one pass, leaving no findings behind.
#[test]
fn fixes_every_nothing_comparison() {
    let src = "\
a = x == nothing
b = y != nothing
";
    let outcome = fix_source(None, src, &select("nothing-comparison"), false);
    insta::assert_snapshot!(outcome.output, @r"
    a = x === nothing
    b = y !== nothing
    ");
    assert_eq!(outcome.applied, 2);
    assert!(outcome.remaining.is_empty());
}
