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

/// `comparison-negation` collapses each negated equality to its direct
/// spelling in one safe pass, leaving no findings behind.
#[test]
fn fixes_every_comparison_negation() {
    let src = "\
a = !(x == y)
b = !(p.kind !== Symbol)
if !(n ≠ 0) && !(s == \"\")
    g()
end
";
    let outcome = fix_source(None, src, &select("comparison-negation"), false);
    insta::assert_snapshot!(outcome.output, @r#"
    a = x != y
    b = p.kind === Symbol
    if n == 0 && s != ""
        g()
    end
    "#);
    assert_eq!(outcome.applied, 4);
    assert!(outcome.remaining.is_empty());
}

/// `redundant-boolean` collapses both conditional spellings in one safe pass,
/// leaving no findings behind.
#[test]
fn fixes_every_redundant_boolean_conditional() {
    let src = "\
a = ready ? true : false
b = (n > 0) ? false : true
if valid(x) ? true : false
    g()
end
";
    let outcome = fix_source(None, src, &select("redundant-boolean"), false);
    insta::assert_snapshot!(outcome.output, @r"
    a = ready
    b = !(n > 0)
    if valid(x)
        g()
    end
    ");
    assert_eq!(outcome.applied, 3);
    assert!(outcome.remaining.is_empty());
}

/// The comparison half rewrites to the operand alone, but only under
/// `--unsafe-fixes`: `==` is not identity, so the two spellings part ways for a
/// non-`Bool` operand. Both operand orders converge in one pass.
#[test]
fn fixes_every_redundant_boolean_comparison_under_unsafe() {
    let src = "\
a = flag == true
b = x.ready != true
c = false == done(y)
d = a + b == false
";
    let outcome = fix_source(None, src, &select("redundant-boolean"), true);
    insta::assert_snapshot!(outcome.output, @r"
    a = flag
    b = !x.ready
    c = !done(y)
    d = !(a + b)
    ");
    assert_eq!(outcome.applied, 4);
    assert!(outcome.remaining.is_empty());
}

/// Without the opt-in, the comparison half reports but changes nothing.
#[test]
fn withholds_redundant_boolean_comparison_fix_by_default() {
    let src = "a = flag == true\n";
    let outcome = fix_source(None, src, &select("redundant-boolean"), false);
    assert_eq!(outcome.output, src);
    assert_eq!(outcome.applied, 0);
    assert_eq!(outcome.remaining.len(), 1);
}

/// `eager-broadcast` moves the broadcast function into the reducer's first
/// argument, but only under `--unsafe-fixes`: broadcasting treats a scalar as a
/// container, and `any`/`all` stop at the first decisive element.
#[test]
fn fixes_every_eager_broadcast_under_unsafe() {
    let src = "\
a = any(isodd.(xs))
b = sum(abs.(f(y)))
c = maximum(sqrt.(xs))
";
    let outcome = fix_source(None, src, &select("eager-broadcast"), true);
    insta::assert_snapshot!(outcome.output, @r"
    a = any(isodd, xs)
    b = sum(abs, f(y))
    c = maximum(sqrt, xs)
    ");
    assert_eq!(outcome.applied, 3);
    assert!(outcome.remaining.is_empty());
}

/// Without the opt-in, `eager-broadcast` reports but changes nothing.
#[test]
fn withholds_eager_broadcast_fix_by_default() {
    let src = "a = sum(abs.(xs))\n";
    let outcome = fix_source(None, src, &select("eager-broadcast"), false);
    assert_eq!(outcome.output, src);
    assert_eq!(outcome.applied, 0);
    assert_eq!(outcome.remaining.len(), 1);
}

/// `sorted-extremum` replaces the whole indexing with the extremum call, but
/// only under `--unsafe-fixes`: `sort` orders `NaN` last where `minimum`
/// propagates it.
#[test]
fn fixes_every_sorted_extremum_under_unsafe() {
    let src = "\
lo = sort(xs)[1]
hi = sort(f(y))[end]
first_word = sort(words)[begin]
";
    let outcome = fix_source(None, src, &select("sorted-extremum"), true);
    insta::assert_snapshot!(outcome.output, @r"
    lo = minimum(xs)
    hi = maximum(f(y))
    first_word = minimum(words)
    ");
    assert_eq!(outcome.applied, 3);
    assert!(outcome.remaining.is_empty());
}

/// Without the opt-in, `sorted-extremum` reports but changes nothing.
#[test]
fn withholds_sorted_extremum_fix_by_default() {
    let src = "lo = sort(xs)[1]\n";
    let outcome = fix_source(None, src, &select("sorted-extremum"), false);
    assert_eq!(outcome.output, src);
    assert_eq!(outcome.applied, 0);
    assert_eq!(outcome.remaining.len(), 1);
}

/// `length-findall` replaces the pair with a `count` call carrying `findall`'s
/// own argument list, but only under `--unsafe-fixes`: `findall` walks a
/// collection's keys where `count` iterates its elements.
#[test]
fn fixes_every_length_findall_under_unsafe() {
    let src = "\
n = length(findall(isodd, xs))
m = length(findall(mask))
";
    let outcome = fix_source(None, src, &select("length-findall"), true);
    insta::assert_snapshot!(outcome.output, @r"
    n = count(isodd, xs)
    m = count(mask)
    ");
    assert_eq!(outcome.applied, 2);
    assert!(outcome.remaining.is_empty());
}

/// Without the opt-in, `length-findall` reports but changes nothing.
#[test]
fn withholds_length_findall_fix_by_default() {
    let src = "n = length(findall(isodd, xs))\n";
    let outcome = fix_source(None, src, &select("length-findall"), false);
    assert_eq!(outcome.output, src);
    assert_eq!(outcome.applied, 0);
    assert_eq!(outcome.remaining.len(), 1);
}

/// `fixed-regex` drops the `r` prefix of a metacharacter-free pattern, turning
/// the regex literal into the string literal it already spells. Safe, so it
/// applies without the opt-in.
#[test]
fn fixes_every_fixed_regex() {
    let src = "\
a = occursin(r\"abc\", s)
b = occursin(r\"\"\"x\"y\"\"\", s)
";
    let outcome = fix_source(None, src, &select("fixed-regex"), false);
    insta::assert_snapshot!(outcome.output, @r#"
    a = occursin("abc", s)
    b = occursin("""x"y""", s)
    "#);
    assert_eq!(outcome.applied, 2);
    assert!(outcome.remaining.is_empty());
}

/// `string-boundary` rewrites the leading-anchor form by default — `^` is a
/// start-of-subject test either way — and holds the trailing-anchor one back,
/// since PCRE's `$` also matches before a final newline.
#[test]
fn fixes_the_leading_anchor_boundary_by_default() {
    let src = "\
a = occursin(r\"^abc\", s)
b = occursin(r\"abc$\", s)
";
    let outcome = fix_source(None, src, &select("string-boundary"), false);
    insta::assert_snapshot!(outcome.output, @r#"
    a = startswith(s, "abc")
    b = occursin(r"abc$", s)
    "#);
    assert_eq!(outcome.applied, 1);
    assert_eq!(outcome.remaining.len(), 1);
}

/// With `--unsafe-fixes` the suffix form is rewritten too.
#[test]
fn fixes_the_trailing_anchor_boundary_under_unsafe() {
    let src = "b = occursin(r\"abc$\", s)\n";
    let outcome = fix_source(None, src, &select("string-boundary"), true);
    insta::assert_snapshot!(outcome.output, @r#"b = endswith(s, "abc")"#);
    assert_eq!(outcome.applied, 1);
    assert!(outcome.remaining.is_empty());
}

/// Several rule sets at once, for the suppression meta rules: they can only
/// judge a directive when the rule it names is in the run.
fn select_many(rules: &[&str]) -> LintConfig {
    LintConfig {
        select: Some(rules.iter().map(|id| id.to_string()).collect()),
        ..Default::default()
    }
}

/// `misnamed-suppression` rewrites each unknown rule ID to the one shipped rule
/// it plainly meant, leaving the reason prose alone.
#[test]
fn fixes_every_misnamed_suppression() {
    let src = "\
# fatou-ignore-file unused_import: vendored
# fatou-ignore unused-bindings: set up by the C library
handle = open_device()
";
    let outcome = fix_source(None, src, &select("misnamed-suppression"), false);
    insta::assert_snapshot!(outcome.output, @r"
    # fatou-ignore-file unused-import: vendored
    # fatou-ignore unused-binding: set up by the C library
    handle = open_device()
    ");
    assert_eq!(outcome.applied, 2);
    assert!(outcome.remaining.is_empty());
}

/// A directive with nothing after it takes its whole line with it.
#[test]
fn fixes_a_dangling_outdated_suppression() {
    let src = "\
function f(x)
    x + 1
end

# fatou-ignore unused-binding: the scratch value below
";
    let outcome = fix_source(None, src, &select("outdated-suppression"), false);
    insta::assert_snapshot!(outcome.output, @r"
    function f(x)
        x + 1
    end
    ");
    assert_eq!(outcome.applied, 1);
    assert!(outcome.remaining.is_empty());
}

/// A stale directive on its own line takes its indentation and line break; one
/// trailing code keeps the code on its line.
#[test]
fn fixes_stale_suppressions_in_both_placements() {
    let src = "\
function f()
    # fatou-ignore unused-binding: was needed once
    1
end
y = 2  # fatou-ignore unused-import: likewise
";
    let outcome = fix_source(
        None,
        src,
        &select_many(&["outdated-suppression", "unused-binding", "unused-import"]),
        false,
    );
    insta::assert_snapshot!(outcome.output, @r"
    function f()
        1
    end
    y = 2
    ");
    assert_eq!(outcome.applied, 2);
    assert!(outcome.remaining.is_empty());
}
