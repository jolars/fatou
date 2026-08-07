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

/// `missing-comparison` rewrites `==`/`!=` against `missing` the same way, but
/// only under `--unsafe-fixes`: the rewrite turns a `missing` result into a
/// `Bool`.
#[test]
fn fixes_every_missing_comparison_under_unsafe() {
    let src = "\
a = x == missing
b = y != missing
";
    let outcome = fix_source(None, src, &select("missing-comparison"), true);
    insta::assert_snapshot!(outcome.output, @r"
    a = x === missing
    b = y !== missing
    ");
    assert_eq!(outcome.applied, 2);
    assert!(outcome.remaining.is_empty());
}

/// Without the opt-in, `missing-comparison` reports but changes nothing.
#[test]
fn withholds_missing_comparison_fix_by_default() {
    let src = "a = x == missing\n";
    let outcome = fix_source(None, src, &select("missing-comparison"), false);
    assert_eq!(outcome.output, src);
    assert_eq!(outcome.applied, 0);
    assert_eq!(outcome.remaining.len(), 1);
}

/// `index-from-length` rewrites the `1:length`/`1:size` prefix to
/// `eachindex`/`axes`, but only under `--unsafe-fixes`: the rewrite is only
/// value-equivalent when the collection's indices are one-based and dense.
#[test]
fn fixes_every_index_from_length_range_under_unsafe() {
    let src = "\
for i in 1:length(xs)
    println(xs[i])
end
for j in 1:size(A, 2)
    println(A[1, j])
end
";
    let outcome = fix_source(None, src, &select("index-from-length"), true);
    insta::assert_snapshot!(outcome.output, @r"
    for i in eachindex(xs)
        println(xs[i])
    end
    for j in axes(A, 2)
        println(A[1, j])
    end
    ");
    assert_eq!(outcome.applied, 2);
    assert!(outcome.remaining.is_empty());
}

/// Without the opt-in, `index-from-length` reports but changes nothing.
#[test]
fn withholds_index_from_length_fix_by_default() {
    let src = "for i in 1:length(xs)\n    println(xs[i])\nend\n";
    let outcome = fix_source(None, src, &select("index-from-length"), false);
    assert_eq!(outcome.output, src);
    assert_eq!(outcome.applied, 0);
    assert_eq!(outcome.remaining.len(), 1);
}

/// `typeof-comparison` rewrites the whole comparison to an `isa` test, but only
/// under `--unsafe-fixes`: it widens an exact-type test to the subtype tree.
/// Both operand orders and both operators converge in one pass.
#[test]
fn fixes_every_typeof_comparison_under_unsafe() {
    let src = "\
a = typeof(x) == Int
b = typeof(y.field) != Union{Int, Float64}
c = String == typeof(f(z))
";
    let outcome = fix_source(None, src, &select("typeof-comparison"), true);
    insta::assert_snapshot!(outcome.output, @r"
    a = x isa Int
    b = !(y.field isa Union{Int, Float64})
    c = f(z) isa String
    ");
    assert_eq!(outcome.applied, 3);
    assert!(outcome.remaining.is_empty());
}

/// Without the opt-in, `typeof-comparison` reports but changes nothing.
#[test]
fn withholds_typeof_comparison_fix_by_default() {
    let src = "a = typeof(x) == Int\n";
    let outcome = fix_source(None, src, &select("typeof-comparison"), false);
    assert_eq!(outcome.output, src);
    assert_eq!(outcome.applied, 0);
    assert_eq!(outcome.remaining.len(), 1);
}

/// `length-zero` collapses every emptiness spelling to `isempty` in one safe
/// pass, both operand orders included, leaving no findings behind.
#[test]
fn fixes_every_length_zero() {
    let src = "\
a = length(x) == 0
b = length(y.items) > 0
c = 1 <= length(f(z))
d = length(w) < 1
";
    let outcome = fix_source(None, src, &select("length-zero"), false);
    insta::assert_snapshot!(outcome.output, @r"
    a = isempty(x)
    b = !isempty(y.items)
    c = !isempty(f(z))
    d = isempty(w)
    ");
    assert_eq!(outcome.applied, 4);
    assert!(outcome.remaining.is_empty());
}
