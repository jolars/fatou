//! Behavioral coverage for the first lint rules: each rule's triggering cases,
//! and the non-triggering cases that guard against false positives.

use fatou::config::{DiscouragedFunctionConfig, LintConfig, RulesConfig};
use fatou::linter::{Applicability, Severity, check_source};

/// Lint `src` with only `rule` enabled and return the messages it produced, in
/// source order.
fn findings(rule: &str, src: &str) -> Vec<String> {
    let config = LintConfig {
        select: Some(vec![rule.to_string()]),
        ..Default::default()
    };
    let report = check_source(None, src, &config);
    report
        .diagnostics
        .into_iter()
        .filter(|d| d.rule == rule)
        .map(|d| d.message.body)
        .collect()
}

fn count(rule: &str, src: &str) -> usize {
    findings(rule, src).len()
}

// --- unused-binding --------------------------------------------------------

#[test]
fn unused_binding_flags_dead_local() {
    assert_eq!(
        count(
            "unused-binding",
            "function f(x)\n    t = x + 1\n    x\nend\n"
        ),
        1
    );
}

#[test]
fn unused_binding_flags_let_var() {
    assert_eq!(count("unused-binding", "let a = 1\n    2\nend\n"), 1);
}

#[test]
fn unused_binding_ignores_read_local() {
    assert_eq!(
        count("unused-binding", "function f()\n    t = 1\n    t\nend\n"),
        0
    );
}

#[test]
fn unused_binding_ignores_parameters_and_loop_vars() {
    // A parameter and a `for` variable are meaningful even when unread.
    assert_eq!(count("unused-binding", "function f(x)\n    1\nend\n"), 0);
    assert_eq!(
        count("unused-binding", "for i in 1:3\n    println(\"hi\")\nend\n"),
        0
    );
}

#[test]
fn unused_binding_ignores_top_level_and_underscore() {
    // Globals and definitions are API surface; `_`-prefixed names are throwaway.
    assert_eq!(count("unused-binding", "x = 1\nconst K = 2\nf() = 3\n"), 0);
    assert_eq!(
        count(
            "unused-binding",
            "function f(x)\n    _tmp = x\n    x\nend\n"
        ),
        0
    );
}

#[test]
fn unused_binding_ignores_macro_keyword_argument() {
    // `key=value` arguments to a macro are keyword arguments, not locals.
    assert_eq!(
        count(
            "unused-binding",
            "function f(ex)\n    @warn \"failed\" exception=(ex, catch_backtrace())\nend\n"
        ),
        0
    );
    assert_eq!(
        count(
            "unused-binding",
            "function g(x, y)\n    @test x ≈ y rtol=eps(Float64)\nend\n"
        ),
        0
    );
}

#[test]
fn unused_binding_flags_dead_local_in_macro_block_argument() {
    // A real assignment inside a scope-transparent macro's block argument is
    // still a local: `@testset` (like `@inbounds`) runs its body as written.
    assert_eq!(
        count(
            "unused-binding",
            "function f()\n    @testset begin\n        t = 1\n    end\nend\n"
        ),
        1
    );
}

#[test]
fn unused_binding_exempts_direct_macro_argument_assignment() {
    // `@show a, b = expr` passes the whole tuple-assignment to the macro as its
    // argument, so `@show` reads every name (it prints each). The names are not
    // dead locals even though the surrounding scope never reads them again.
    assert_eq!(
        count(
            "unused-binding",
            "function f()\n    @show typ, uniform_size = compute()\nend\n"
        ),
        0
    );
    // A single-name direct argument assignment is likewise a macro read.
    assert_eq!(
        count(
            "unused-binding",
            "function f()\n    @show x = compute()\nend\n"
        ),
        0
    );
    // The exemption is scoped to the direct argument: a dead local nested inside
    // a block the macro splices unevaluated stays a genuine finding (mirrors the
    // scope-transparent `@testset` case above).
    assert_eq!(
        count(
            "unused-binding",
            "function f()\n    @testset begin\n        x = compute()\n    end\nend\n"
        ),
        1
    );
}

#[test]
fn unused_binding_exempts_attribute_dsl_macro_block() {
    // A consuming attribute DSL reads each `name = default` in its block as an
    // attribute, so those names are not dead locals.
    assert_eq!(
        count(
            "unused-binding",
            "function f(d)\n    @gen_defaults! d begin\n        color = nothing\n    end\nend\n"
        ),
        0
    );
    assert_eq!(
        count(
            "unused-binding",
            "function f()\n    @DocumentedAttributes begin\n        space = :data\n    end\nend\n"
        ),
        0
    );
    // A qualified DSL name (`Makie.@recipe`) is matched on its final component.
    assert_eq!(
        count(
            "unused-binding",
            "function f()\n    Makie.@recipe Foo begin\n        marker = automatic\n    end\nend\n"
        ),
        0
    );
}

#[test]
fn unused_binding_ignores_typed_defaulted_parameter() {
    // `space::Symbol = :data` is a parameter with a default, not a local, so
    // it is exempt like any parameter even when unread. Unlike the untyped
    // `space = :data` (a `KEYWORD_ARG`), it parses as an `ARG` wrapping an
    // assignment, which the scope builder previously mistook for a local.
    assert_eq!(
        count("unused-binding", "f(pl, space::Symbol = :data) = pl\n"),
        0
    );
    assert_eq!(
        count(
            "unused-binding",
            "function g(pl; opt::Int = 1)\n    return pl\nend\n"
        ),
        0
    );
}

#[test]
fn unused_binding_ignores_kwdef_field_default() {
    // `@kwdef` field defaults are struct fields, not local variables.
    assert_eq!(
        count(
            "unused-binding",
            "Base.@kwdef struct C\n    a::Int = 2\n    name::String = \"x\"\nend\n"
        ),
        0
    );
}

#[test]
fn unused_binding_counts_prefixed_string_macro_interpolation() {
    // A non-standard string literal (`js"..."`) keeps its body verbatim, so the
    // lexer never splits `$x` into an INTERPOLATION node. Such macros still
    // interpolate at expansion time, so `$x`/`$(...)` count as reads.
    assert_eq!(
        count(
            "unused-binding",
            "function f()\n    x = 1\n    js\"value $x\"\nend\n"
        ),
        0
    );
    assert_eq!(
        count(
            "unused-binding",
            "function f()\n    a = 1\n    b = 2\n    js\"$(a + b)\"\nend\n"
        ),
        0
    );
    // Only the interpolated name is spared: `y` is still a dead local.
    assert_eq!(
        count(
            "unused-binding",
            "function f()\n    x = 1\n    y = 2\n    js\"only $x\"\nend\n"
        ),
        1
    );
    // Inside `$(...)`, a field access reads the receiver, not the field name.
    assert_eq!(
        count(
            "unused-binding",
            "function f(a)\n    b = 1\n    js\"$(a.b)\"\nend\n"
        ),
        1
    );
}

// --- unused-import ---------------------------------------------------------

#[test]
fn unused_import_flags_unused_item_and_whole_import() {
    assert_eq!(count("unused-import", "using A: foo\n1\n"), 1);
    assert_eq!(count("unused-import", "import Printf\n1\n"), 1);
    assert_eq!(count("unused-import", "import A as B\n1\n"), 1);
}

#[test]
fn unused_import_exempts_whole_module_using() {
    // `using A` attaches exports resolved elsewhere; never flag the bare form.
    assert_eq!(count("unused-import", "using A\n1\n"), 0);
    assert_eq!(count("unused-import", "using A.B\n1\n"), 0);
}

#[test]
fn unused_import_counts_qualified_and_direct_use() {
    assert_eq!(count("unused-import", "import A\nA.f()\n"), 0);
    assert_eq!(count("unused-import", "using A: foo\nfoo()\n"), 0);
}

#[test]
fn unused_import_counts_reexport_as_use() {
    assert_eq!(count("unused-import", "import A: foo\nexport foo\n"), 0);
}

#[test]
fn unused_import_flags_only_the_unused_item() {
    let msgs = findings("unused-import", "using A: foo, bar\nbar()\n");
    assert_eq!(msgs.len(), 1);
    assert!(msgs[0].contains("foo"), "{msgs:?}");
}

#[test]
fn unused_import_counts_method_extension_as_use() {
    // Importing a name to add methods to it (extend it) is a use.
    assert_eq!(
        count(
            "unused-import",
            "import Base: *\n*(a::Bool, b::Bool) = a & b\n"
        ),
        0
    );
    assert_eq!(
        count(
            "unused-import",
            "import Base: convert\nconvert(::Type{Int}, x) = 0\n"
        ),
        0
    );
    // Infix operator method definition `a::T + b = ...` extends `+` too.
    assert_eq!(
        count(
            "unused-import",
            "import Base: +\nstruct MyStruct x::Int end\na::MyStruct + b = a.x + b\n"
        ),
        0
    );
    assert_eq!(
        count("unused-import", "import Base: ==\na == b = true\n"),
        0
    );
}

#[test]
fn unused_import_still_flags_never_referenced_import() {
    // No definition or reference of `foo`: still unused.
    assert_eq!(count("unused-import", "import Base: foo\nbar() = 1\n"), 1);
}

#[test]
fn unused_import_counts_quoted_use() {
    // A name used only inside a quote resolves, by macro hygiene, to the
    // enclosing module's binding, so the import is used.
    assert_eq!(
        count(
            "unused-import",
            "using S: median\nf() = quote median(x) end\n"
        ),
        0
    );
    assert_eq!(
        count("unused-import", "using S: median\nf() = :(median(x))\n"),
        0
    );
    // A bare string is not quoted code: an import only appearing there is
    // still unused.
    assert_eq!(
        count("unused-import", "using S: median\nx = \"median\"\n"),
        1
    );
}

#[test]
fn unused_import_counts_operator_use() {
    // Infix use of an imported operator.
    assert_eq!(
        count("unused-import", "import Base: ==\nf(a, b) = a == b\n"),
        0
    );
    // Parenthesized operator method definition `(==)(a, b) = ...`.
    assert_eq!(
        count(
            "unused-import",
            "import Base: ==\n(==)(a::S, b::S) = true\n"
        ),
        0
    );
    // Operator passed as a value.
    assert_eq!(
        count("unused-import", "import Base: +\nf(xs) = reduce(+, xs)\n"),
        0
    );
    // Never referenced: still unused.
    assert_eq!(count("unused-import", "import Base: ==\nf() = 1\n"), 1);
}

#[test]
fn unused_import_counts_string_macro_use() {
    // `u"ns"` desugars to `@u_str`, so an explicit import of it is used.
    assert_eq!(
        count("unused-import", "using Unitful: @u_str\nx = u\"ns\"\n"),
        0
    );
    // A juxtaposed prefix (`1u"ns"`) is the same string macro.
    assert_eq!(
        count("unused-import", "using Unitful: @u_str\nx = 1u\"ns\"\n"),
        0
    );
    // The command form `` p`...` `` desugars to `@p_cmd`.
    assert_eq!(count("unused-import", "using M: @p_cmd\nx = p`ls`\n"), 0);
}

#[test]
fn unused_import_still_flags_unused_string_macro() {
    // Imported but never applied as a string macro: still unused.
    assert_eq!(count("unused-import", "using Unitful: @u_str\nx = 1\n"), 1);
}

#[test]
fn unused_import_counts_prefixed_string_macro_interpolation() {
    // An imported name interpolated inside a non-standard string macro
    // (`js"$median"`) is a use, even though the body lexes verbatim.
    assert_eq!(
        count(
            "unused-import",
            "using M: @js_str, median\nx = js\"$median\"\n"
        ),
        0
    );
    // Not interpolated anywhere: still unused.
    assert_eq!(
        count(
            "unused-import",
            "using M: @js_str, median\nx = js\"nothing\"\n"
        ),
        1
    );
}

// --- duplicate-argument ----------------------------------------------------

#[test]
fn duplicate_argument_flags_long_and_short_forms() {
    assert_eq!(
        count("duplicate-argument", "function g(a, b, a)\n    a\nend\n"),
        1
    );
    assert_eq!(count("duplicate-argument", "f(x, x) = x\n"), 1);
}

#[test]
fn duplicate_argument_flags_positional_keyword_clash() {
    assert_eq!(count("duplicate-argument", "f(x; x) = x\n"), 1);
}

#[test]
fn duplicate_argument_ignores_distinct_names() {
    assert_eq!(
        count("duplicate-argument", "function g(a, b, c)\n    a\nend\n"),
        0
    );
}

#[test]
fn duplicate_argument_does_not_confuse_separate_signatures() {
    // Same name in two different functions is fine.
    assert_eq!(count("duplicate-argument", "f(x) = x\ng(x) = x\n"), 0);
}

// --- duplicate-keyword-argument --------------------------------------------

#[test]
fn duplicate_keyword_argument_flags_a_repeat_before_the_semicolon() {
    assert_eq!(count("duplicate-keyword-argument", "h(a = 1, a = 2)\n"), 1);
}

#[test]
fn duplicate_keyword_argument_spans_the_semicolon() {
    // Keywords on both sides of the `;` share one namespace.
    assert_eq!(count("duplicate-keyword-argument", "h(a = 1; a = 2)\n"), 1);
    assert_eq!(
        count("duplicate-keyword-argument", "h(; a = 1, a = 2)\n"),
        1
    );
}

#[test]
fn duplicate_keyword_argument_sees_the_shorthand() {
    // `h(; a)` passes the binding `a` under its own name.
    assert_eq!(count("duplicate-keyword-argument", "h(; a, a)\n"), 1);
    assert_eq!(count("duplicate-keyword-argument", "h(a = 1; a)\n"), 1);
}

#[test]
fn duplicate_keyword_argument_reports_every_repeat() {
    assert_eq!(
        count("duplicate-keyword-argument", "h(a = 1, a = 2, a = 3)\n"),
        2
    );
}

#[test]
fn duplicate_keyword_argument_points_at_the_repeated_name() {
    let src = "h(alpha = 1, alpha = 2)\n";
    let config = LintConfig {
        select: Some(vec!["duplicate-keyword-argument".to_string()]),
        ..Default::default()
    };
    let report = check_source(None, src, &config);
    assert_eq!(report.diagnostics.len(), 1);
    // The second `alpha`, not the whole call.
    assert_eq!(&src[report.diagnostics[0].range], "alpha");
    assert_eq!(
        usize::from(report.diagnostics[0].range.start()),
        src.rfind("alpha").unwrap()
    );
}

#[test]
fn duplicate_keyword_argument_ignores_distinct_keywords() {
    assert_eq!(count("duplicate-keyword-argument", "h(a = 1, b = 2)\n"), 0);
    assert_eq!(count("duplicate-keyword-argument", "h(x, y; a = 1)\n"), 0);
}

#[test]
fn duplicate_keyword_argument_ignores_repeated_positionals() {
    // Passing the same value twice positionally is legal.
    assert_eq!(count("duplicate-keyword-argument", "h(x, x)\n"), 0);
}

#[test]
fn duplicate_keyword_argument_ignores_splatted_keywords() {
    // What `kw...` carries is unknowable, so a splat alone never fires.
    assert_eq!(count("duplicate-keyword-argument", "h(; kw...)\n"), 0);
    assert_eq!(count("duplicate-keyword-argument", "h(a = 1; kw...)\n"), 0);
}

#[test]
fn duplicate_keyword_argument_still_fires_alongside_a_splat() {
    // Julia rejects the repeat regardless of what the splat carries.
    assert_eq!(
        count("duplicate-keyword-argument", "h(a = 1; kw..., a = 2)\n"),
        1
    );
}

#[test]
fn duplicate_keyword_argument_ignores_definition_signatures() {
    // A signature declares parameters; the repeat there is
    // `duplicate-argument`'s finding, not this rule's.
    assert_eq!(
        count(
            "duplicate-keyword-argument",
            "function f(; a = 1, a = 2)\n    a\nend\n"
        ),
        0
    );
    assert_eq!(
        count("duplicate-keyword-argument", "f(a = 1, a = 2) = a\n"),
        0
    );
}

#[test]
fn duplicate_keyword_argument_does_not_merge_separate_calls() {
    assert_eq!(
        count("duplicate-keyword-argument", "h(a = 1)\ng(a = 2)\n"),
        0
    );
    // A nested call has its own keyword namespace.
    assert_eq!(count("duplicate-keyword-argument", "h(a = g(a = 2))\n"), 0);
}

#[test]
fn duplicate_keyword_argument_skips_quoted_and_macro_code() {
    // Quoted code is data, and a macro may rewrite what it receives.
    assert_eq!(
        count("duplicate-keyword-argument", "ex = :(h(a = 1, a = 2))\n"),
        0
    );
    assert_eq!(
        count("duplicate-keyword-argument", "@m h(a = 1, a = 2)\n"),
        0
    );
}

// --- unused-argument -------------------------------------------------------

#[test]
fn unused_argument_flags_unread_positional() {
    // `factor` is never read; the body is not a lone literal.
    assert_eq!(
        count(
            "unused-argument",
            "function scale(x, factor)\n    2 * x\nend\n"
        ),
        1
    );
}

#[test]
fn unused_argument_flags_short_form_and_keyword() {
    assert_eq!(count("unused-argument", "f(x) = rand()\n"), 1);
    assert_eq!(count("unused-argument", "f(; k = 1) = rand()\n"), 1);
}

#[test]
fn unused_argument_flags_anonymous_and_do_forms() {
    assert_eq!(count("unused-argument", "map(x -> rand(), xs)\n"), 1);
    assert_eq!(
        count("unused-argument", "map(xs) do x\n    rand()\nend\n"),
        1
    );
}

#[test]
fn unused_argument_ignores_read_parameter() {
    assert_eq!(count("unused-argument", "f(x) = x + 1\n"), 0);
    // Captured by a closure counts as read.
    assert_eq!(
        count("unused-argument", "function f(x)\n    () -> x\nend\n"),
        0
    );
}

#[test]
fn unused_argument_ignores_underscore_names() {
    assert_eq!(count("unused-argument", "f(_) = rand()\n"), 0);
    assert_eq!(count("unused-argument", "f(__) = rand()\n"), 0);
}

#[test]
fn unused_argument_ignores_stub_bodies() {
    // Placeholder bodies that intentionally ignore their arguments: a lone
    // literal, `nothing`, or an `error(...)`/`throw(...)` call.
    assert_eq!(count("unused-argument", "f(x) = 0\n"), 0);
    assert_eq!(count("unused-argument", "f(x) = \"todo\"\n"), 0);
    assert_eq!(
        count("unused-argument", "function stub(x)\n    0\nend\n"),
        0
    );
    assert_eq!(count("unused-argument", "f(x) = nothing\n"), 0);
    assert_eq!(
        count("unused-argument", "f(x) = error(\"not implemented\")\n"),
        0
    );
    assert_eq!(
        count("unused-argument", "f(x) = throw(ArgumentError(\"nope\"))\n"),
        0
    );
}

#[test]
fn unused_argument_flags_nonstub_single_expression_bodies() {
    // A bare identifier that is not `nothing`, and an ordinary call, are real
    // bodies, not stubs -> the unused parameter is still flagged.
    assert_eq!(count("unused-argument", "f(x) = y\n"), 1);
    assert_eq!(count("unused-argument", "f(x) = g()\n"), 1);
    // An assignment body is not a stub either.
    assert_eq!(
        count(
            "unused-argument",
            "function required(x)\n    tmp = true\n    tmp\nend\n"
        ),
        1
    );
}

#[test]
fn unused_argument_is_disabled_by_default() {
    // Noisy opt-in rule: absent an explicit `--select`, it stays silent.
    let report = check_source(None, "f(x) = rand()\n", &LintConfig::default());
    assert!(
        report
            .diagnostics
            .iter()
            .all(|d| d.rule != "unused-argument")
    );
}

// --- assignment-in-condition -----------------------------------------------

#[test]
fn assignment_in_condition_flags_if_and_while() {
    assert_eq!(
        count("assignment-in-condition", "if x = 5\n    x\nend\n"),
        1
    );
    assert_eq!(
        count("assignment-in-condition", "while x = f()\n    x\nend\n"),
        1
    );
}

#[test]
fn assignment_in_condition_flags_elseif() {
    assert_eq!(
        count(
            "assignment-in-condition",
            "if a\n    1\nelseif b = 2\n    2\nend\n"
        ),
        1
    );
}

#[test]
fn assignment_in_condition_flags_parenthesized() {
    assert_eq!(
        count("assignment-in-condition", "if (x = 5)\n    x\nend\n"),
        1
    );
}

#[test]
fn assignment_in_condition_ignores_comparisons() {
    assert_eq!(
        count("assignment-in-condition", "if x == 5\n    x\nend\n"),
        0
    );
    assert_eq!(
        count("assignment-in-condition", "while x === y\n    1\nend\n"),
        0
    );
}

#[test]
fn assignment_in_condition_ignores_plain_condition_and_call_kwarg() {
    assert_eq!(count("assignment-in-condition", "if cond\n    1\nend\n"), 0);
    // A keyword argument inside a call in the condition is not an assignment.
    assert_eq!(
        count("assignment-in-condition", "if f(x = 1)\n    1\nend\n"),
        0
    );
}

// --- nothing-comparison ----------------------------------------------------

#[test]
fn nothing_comparison_flags_eq_and_ne() {
    assert_eq!(count("nothing-comparison", "x == nothing\n"), 1);
    assert_eq!(count("nothing-comparison", "x != nothing\n"), 1);
}

#[test]
fn nothing_comparison_flags_nothing_on_either_side() {
    assert_eq!(count("nothing-comparison", "nothing == x\n"), 1);
    assert_eq!(count("nothing-comparison", "nothing != x\n"), 1);
}

#[test]
fn nothing_comparison_ignores_identity_operators() {
    // `===` / `!==` are already the recommended form.
    assert_eq!(count("nothing-comparison", "x === nothing\n"), 0);
    assert_eq!(count("nothing-comparison", "x !== nothing\n"), 0);
}

#[test]
fn nothing_comparison_ignores_unrelated_comparisons() {
    assert_eq!(count("nothing-comparison", "x == y\n"), 0);
    assert_eq!(count("nothing-comparison", "isnothing(x)\n"), 0);
    // The `Nothing` *type* is a different, capitalized identifier.
    assert_eq!(count("nothing-comparison", "x == Nothing\n"), 0);
}

#[test]
fn nothing_comparison_carries_a_safe_fix() {
    let config = LintConfig {
        select: Some(vec!["nothing-comparison".to_string()]),
        ..Default::default()
    };
    let src = "x == nothing\n";
    let report = check_source(None, src, &config);
    let fix = &report.diagnostics[0].fixes[0];
    assert_eq!(fix.content, "===");
    // The replacement spans exactly the `==` operator token.
    assert_eq!(&src[fix.start..fix.end], "==");
}

// --- missing-comparison ----------------------------------------------------

#[test]
fn missing_comparison_flags_eq_and_ne() {
    assert_eq!(count("missing-comparison", "x == missing\n"), 1);
    assert_eq!(count("missing-comparison", "x != missing\n"), 1);
}

#[test]
fn missing_comparison_flags_missing_on_either_side() {
    assert_eq!(count("missing-comparison", "missing == x\n"), 1);
    assert_eq!(count("missing-comparison", "missing != x\n"), 1);
}

#[test]
fn missing_comparison_ignores_identity_operators() {
    // `===` / `!==` already answer the identity question.
    assert_eq!(count("missing-comparison", "x === missing\n"), 0);
    assert_eq!(count("missing-comparison", "x !== missing\n"), 0);
}

#[test]
fn missing_comparison_ignores_unrelated_comparisons() {
    assert_eq!(count("missing-comparison", "x == y\n"), 0);
    assert_eq!(count("missing-comparison", "ismissing(x)\n"), 0);
    // The `Missing` *type* is a different, capitalized identifier.
    assert_eq!(count("missing-comparison", "x == Missing\n"), 0);
}

#[test]
fn missing_comparison_ignores_broadcast_comparison() {
    // `x .== missing` is an elementwise comparison over a container, a
    // different operator with its own token kind. It is not the scalar
    // identity question this rule is about.
    assert_eq!(count("missing-comparison", "x .== missing\n"), 0);
    assert_eq!(count("missing-comparison", "x .!= missing\n"), 0);
}

#[test]
fn missing_comparison_ignores_comparison_chains() {
    // A chain folds into a COMPARISON_EXPR, not a BINARY_EXPR.
    assert_eq!(count("missing-comparison", "a < b == missing\n"), 0);
}

#[test]
fn missing_comparison_carries_an_unsafe_fix() {
    let config = LintConfig {
        select: Some(vec!["missing-comparison".to_string()]),
        ..Default::default()
    };
    let src = "x == missing\n";
    let report = check_source(None, src, &config);
    let fix = &report.diagnostics[0].fixes[0];
    assert_eq!(fix.content, "===");
    // The replacement spans exactly the `==` operator token.
    assert_eq!(&src[fix.start..fix.end], "==");
    // Unlike `nothing-comparison`, the rewrite changes the expression's value
    // (`missing` becomes a `Bool`), so it needs `--unsafe-fixes`.
    assert_eq!(fix.applicability, fatou::linter::Applicability::Unsafe);
}

// --- severity ----------------------------------------------------------------

/// The severity a single finding of `rule` in `src` carries under `config`.
fn severity_of(rule: &str, src: &str, config: &LintConfig) -> Severity {
    let report = check_source(None, src, config);
    let diag = report
        .diagnostics
        .iter()
        .find(|d| d.rule == rule)
        .expect("rule should fire");
    diag.severity
}

#[test]
fn findings_carry_the_rule_default_severity() {
    let config = LintConfig::default();
    // duplicate-argument is a hard error (Julia rejects the definition).
    assert_eq!(
        severity_of("duplicate-argument", "f(x, x) = x\n", &config),
        Severity::Error
    );
    assert_eq!(
        severity_of("unused-import", "using A: foo\n1\n", &config),
        Severity::Warning
    );
}

#[test]
fn config_overrides_severity_per_rule() {
    let config = LintConfig {
        severity: [
            ("unused-import".to_string(), Severity::Error),
            ("duplicate-argument".to_string(), Severity::Hint),
        ]
        .into(),
        ..Default::default()
    };
    // Both directions: promote a warning-by-default rule and demote an
    // error-by-default one.
    assert_eq!(
        severity_of("unused-import", "using A: foo\n1\n", &config),
        Severity::Error
    );
    assert_eq!(
        severity_of("duplicate-argument", "f(x, x) = x\n", &config),
        Severity::Hint
    );
}

#[test]
fn severity_override_applies_to_node_dispatch_rules() {
    // assignment-in-condition runs via the shared CST traversal (`interests`),
    // not `check_file`; the engine must stamp that path too.
    let config = LintConfig {
        severity: [("assignment-in-condition".to_string(), Severity::Error)].into(),
        ..Default::default()
    };
    assert_eq!(
        severity_of("assignment-in-condition", "if x = 5\n    x\nend\n", &config),
        Severity::Error
    );
}

#[test]
fn assignment_in_condition_carries_a_safe_fix() {
    let config = LintConfig {
        select: Some(vec!["assignment-in-condition".to_string()]),
        ..Default::default()
    };
    let report = check_source(None, "if x = 5\n    x\nend\n", &config);
    let fix = &report.diagnostics[0].fixes[0];
    assert_eq!(fix.content, "==");
    // The replacement spans exactly the `=` token.
    assert_eq!(&"if x = 5\n    x\nend\n"[fix.start..fix.end], "=");
}

// --- undefined-name ----------------------------------------------------------

#[test]
fn undefined_name_flags_an_unknown_identifier() {
    let msgs = findings("undefined-name", "x = undefined_var + 1\n");
    assert_eq!(msgs.len(), 1);
    assert!(msgs[0].contains("undefined_var"), "{msgs:?}");
}

#[test]
fn undefined_name_resolves_base_and_core_names() {
    // `println`, `sqrt`, `pi`, and `Int` come from the built-in Base/Core
    // export snapshot; a plain script using them is clean.
    assert_eq!(
        count("undefined-name", "x::Int = 4\nprintln(sqrt(x) * pi)\n"),
        0
    );
}

#[test]
fn undefined_name_respects_locals_params_and_globals() {
    assert_eq!(
        count(
            "undefined-name",
            "total = 0\nfunction add(x)\n    y = x + total\n    y\nend\n"
        ),
        0
    );
}

#[test]
fn undefined_name_binds_infix_operator_def_operands_as_params() {
    // `a::T + b = ...` is an operator method definition: the operands are
    // parameters, so the body's reads of them resolve rather than dangle.
    assert_eq!(count("undefined-name", "a::Int + b = a * b\n"), 0);
}

#[test]
fn undefined_name_allows_use_before_definition_at_top_level() {
    // Julia resolves globals at call time, so a function may call a sibling
    // defined later in the file.
    assert_eq!(count("undefined-name", "g() = h()\nh() = 1\n"), 0);
}

#[test]
fn undefined_name_flags_an_unknown_macro() {
    let msgs = findings("undefined-name", "@nosuchmacro x = 1\n");
    assert_eq!(msgs.len(), 1);
    assert!(msgs[0].contains("@nosuchmacro"), "{msgs:?}");
}

#[test]
fn undefined_name_resolves_base_macros() {
    assert_eq!(count("undefined-name", "@assert true\n"), 0);
}

#[test]
fn undefined_name_skips_value_reads_inside_macro_calls() {
    // A macro receives unevaluated expressions and may bind names itself
    // (`@testset`, DSL macros), so value reads inside a macro call are exempt.
    // The unknown macro itself is still the one finding here.
    assert_eq!(
        count("undefined-name", "@nosuchmacro some_dsl_name + other\n"),
        1
    );
    assert_eq!(count("undefined-name", "@assert never_bound == 1\n"), 0);
}

#[test]
fn undefined_name_skips_files_with_unresolvable_whole_module_usings() {
    // `using Foo` may export anything; without Foo's index nothing in the
    // file can be called undefined.
    assert_eq!(count("undefined-name", "using Foo\nnotdefined()\n"), 0);
    // Relative usings never resolve against the library either.
    assert_eq!(count("undefined-name", "using .Local\nnotdefined()\n"), 0);
}

#[test]
fn undefined_name_still_fires_with_item_list_imports() {
    // `using Foo: bar` binds exactly `bar`; the file stays checkable and the
    // unrelated unknown name is still flagged.
    let src = "using Foo: bar\nbar()\nnotdefined()\n";
    let msgs = findings("undefined-name", src);
    assert_eq!(msgs.len(), 1);
    assert!(msgs[0].contains("notdefined"), "{msgs:?}");
}

#[test]
fn undefined_name_skips_files_that_eval() {
    // `eval`/`@eval` can define names statically invisible to the model.
    assert_eq!(count("undefined-name", "eval(:(x = 1))\nuses_x() = x\n"), 0);
    assert_eq!(count("undefined-name", "@eval $name = 1\nmystery()\n"), 0);
}

#[test]
fn undefined_name_skips_files_that_include() {
    // Without project context an `include` splices in unknown definitions.
    assert_eq!(
        count("undefined-name", "include(\"defs.jl\")\nfrom_include()\n"),
        0
    );
}

#[test]
fn undefined_name_skips_module_implicit_names() {
    // Every module implicitly defines `eval` and `include`; `new` is the
    // inner-constructor primitive. (The `include` call here is a *literal*
    // self-include-free file... it also triggers the include bail, so use a
    // shape that exercises `new` alone.)
    assert_eq!(
        count(
            "undefined-name",
            "struct P\n    x\n    P(x) = new(x)\nend\n"
        ),
        0
    );
}

#[test]
fn undefined_name_flags_reads_in_string_interpolation() {
    let msgs = findings("undefined-name", "greet(name) = \"hi $namee\"\n");
    assert_eq!(msgs.len(), 1);
    assert!(msgs[0].contains("namee"), "{msgs:?}");
}

#[test]
fn undefined_name_leaves_string_macros_alone() {
    assert_eq!(count("undefined-name", "pattern = r\"a.b\"\n"), 0);
}

#[test]
fn undefined_name_is_opt_in() {
    // Too noisy without project context (a bare file may be an `include`d
    // fragment reading its host's globals), so the CLI leaves it off unless
    // selected; the language server enables it for workspace member files.
    let report = check_source(None, "x = undefined_var\n", &LintConfig::default());
    assert!(
        report.diagnostics.is_empty(),
        "undefined-name must be off by default, got {:?}",
        report.diagnostics
    );
}

// --- break-outside-loop ------------------------------------------------------

#[test]
fn break_outside_loop_flags_top_level_break_and_continue() {
    let msgs = findings("break-outside-loop", "break\n");
    assert_eq!(msgs.len(), 1);
    assert!(msgs[0].contains("`break`"), "{msgs:?}");

    let msgs = findings("break-outside-loop", "continue\n");
    assert_eq!(msgs.len(), 1);
    assert!(msgs[0].contains("`continue`"), "{msgs:?}");
}

#[test]
fn break_outside_loop_flags_loopless_function_and_if() {
    assert_eq!(
        count(
            "break-outside-loop",
            "function f(x)\n    if x > 0\n        break\n    end\nend\n"
        ),
        1
    );
    assert_eq!(
        count("break-outside-loop", "if true\n    continue\nend\n"),
        1
    );
}

#[test]
fn break_outside_loop_flags_function_boundaries_inside_loops() {
    // A closure body is a new function: `break` cannot reach the outer loop.
    assert_eq!(
        count(
            "break-outside-loop",
            "for i in 1:3\n    function f()\n        break\n    end\nend\n"
        ),
        1
    );
    assert_eq!(
        count(
            "break-outside-loop",
            "for i in 1:3\n    g = x -> break\nend\n"
        ),
        1
    );
    // The do-block body is an anonymous function too.
    assert_eq!(
        count(
            "break-outside-loop",
            "for i in 1:3\n    foreach(1:2) do x\n        break\n    end\nend\n"
        ),
        1
    );
}

#[test]
fn break_outside_loop_ignores_break_inside_loops() {
    assert_eq!(
        count(
            "break-outside-loop",
            "for i in 1:3\n    if i == 2\n        break\n    end\n    continue\nend\n"
        ),
        0
    );
    assert_eq!(
        count(
            "break-outside-loop",
            "while true\n    let\n        break\n    end\nend\n"
        ),
        0
    );
    // `try` does not sever the loop connection.
    assert_eq!(
        count(
            "break-outside-loop",
            "for i in 1:3\n    try\n        break\n    catch\n    end\nend\n"
        ),
        0
    );
}

#[test]
fn break_outside_loop_treats_loop_headers_as_inside() {
    // The iterator spec and the `while` condition are within the loop's
    // break scope (verified against Julia 1.12 lowering).
    assert_eq!(
        count("break-outside-loop", "for i in (break; 1:3)\nend\n"),
        0
    );
    assert_eq!(count("break-outside-loop", "while (break; true)\nend\n"), 0);
}

#[test]
fn break_outside_loop_walks_through_enclosing_scope_positions() {
    // A do-call's *arguments* and a comprehension's iterator run in the
    // enclosing scope: legal inside a loop, an error without one.
    assert_eq!(
        count(
            "break-outside-loop",
            "for i in 1:3\n    foreach((break; 1:2)) do x\n        x\n    end\nend\n"
        ),
        0
    );
    assert_eq!(
        count(
            "break-outside-loop",
            "for i in 1:3\n    [x for x in (break; 1:2)]\nend\n"
        ),
        0
    );
    assert_eq!(
        count("break-outside-loop", "[x for x in (break; 1:2)]\n"),
        1
    );
}

#[test]
fn break_outside_loop_stays_silent_in_quotes_and_macro_calls() {
    // Quoted code is data; a macro may rewrite its arguments arbitrarily.
    assert_eq!(count("break-outside-loop", "quote\n    break\nend\n"), 0);
    assert_eq!(count("break-outside-loop", "ex = :(break)\n"), 0);
    assert_eq!(count("break-outside-loop", "@inbounds break\n"), 0);
}

#[test]
fn break_outside_loop_ignores_break_in_macro_lambda_argument() {
    // A macro may rewrite an arrow-shaped argument into a loop body, so a
    // `break`/`continue` inside it (even nested in a `->`) is not flagged.
    assert_eq!(
        count(
            "break-outside-loop",
            "@nloops N i A d -> begin\n    continue\nend\n"
        ),
        0
    );
    assert_eq!(
        count(
            "break-outside-loop",
            "for c in xs\n    @stm c begin\n        pat -> break\n    end\nend\n"
        ),
        0
    );
}

// --- constant-condition ------------------------------------------------------

#[test]
fn constant_condition_flags_literal_if_test() {
    assert_eq!(count("constant-condition", "if true\n    1\nend\n"), 1);
    assert_eq!(count("constant-condition", "if false\n    1\nend\n"), 1);
    assert_eq!(
        count(
            "constant-condition",
            "if x\n    1\nelseif true\n    2\nend\n"
        ),
        1
    );
    // `Condition::expr` unwraps a single paren layer.
    assert_eq!(count("constant-condition", "if (true)\n    1\nend\n"), 1);
}

#[test]
fn constant_condition_flags_while_false() {
    assert_eq!(count("constant-condition", "while false\n    1\nend\n"), 1);
}

#[test]
fn constant_condition_exempts_while_true() {
    // `while true` + `break` is Julia's idiomatic infinite loop; there is no
    // dedicated loop construct to rewrite it to.
    assert_eq!(
        count("constant-condition", "while true\n    break\nend\n"),
        0
    );
}

#[test]
fn constant_condition_flags_literal_lazy_operand() {
    assert_eq!(count("constant-condition", "x && true\n"), 1);
    assert_eq!(count("constant-condition", "false && g()\n"), 1);
    assert_eq!(count("constant-condition", "true || g()\n"), 1);
    assert_eq!(count("constant-condition", "x || false\n"), 1);
    // Each literal operand is its own finding.
    assert_eq!(count("constant-condition", "true && false\n"), 2);
}

#[test]
fn constant_condition_reports_lazy_operand_once_inside_a_condition() {
    // The `&&` operand check fires; the condition check stays out of it (the
    // test expression is a `BINARY_EXPR`, not a literal).
    assert_eq!(count("constant-condition", "if x && true\n    1\nend\n"), 1);
}

#[test]
fn constant_condition_ignores_nonliteral_tests_and_eager_operators() {
    assert_eq!(count("constant-condition", "if x\n    1\nend\n"), 0);
    // Eager bitwise `&`/`|` and broadcast `.&&`/`.||` operate on values.
    assert_eq!(count("constant-condition", "x & true\n"), 0);
    assert_eq!(count("constant-condition", "x | false\n"), 0);
    assert_eq!(count("constant-condition", "x .&& true\n"), 0);
    // A ternary test is out of scope (no `CONDITION` node).
    assert_eq!(count("constant-condition", "true ? a : b\n"), 0);
}

#[test]
fn constant_condition_ignores_static_and_quoted_conditions() {
    // `@static` selects a branch on a compile-time constant by design.
    assert_eq!(
        count("constant-condition", "@static if false\n    1\nend\n"),
        0
    );
    assert_eq!(count("constant-condition", "@static true && f()\n"), 0);
    // Quoted code is data, not an evaluated condition.
    assert_eq!(
        count("constant-condition", "ex = :(if true\n    1\nend)\n"),
        0
    );
}

#[test]
fn constant_condition_ignores_literals_in_value_position() {
    assert_eq!(count("constant-condition", "x = true\n"), 0);
    assert_eq!(count("constant-condition", "f(true)\n"), 0);
    assert_eq!(count("constant-condition", "return true\n"), 0);
}

// --- module-shadows-parent ---------------------------------------------------

#[test]
fn module_shadows_parent_flags_nested_same_name() {
    let msgs = findings("module-shadows-parent", "module A\nmodule A\nend\nend\n");
    assert_eq!(msgs.len(), 1);
    assert!(
        msgs[0].contains("`A`"),
        "message names the module: {msgs:?}"
    );
}

#[test]
fn module_shadows_parent_flags_baremodule_forms() {
    // Both keywords produce the same module shape, in either position.
    assert_eq!(
        count(
            "module-shadows-parent",
            "baremodule A\nmodule A\nend\nend\n"
        ),
        1
    );
    assert_eq!(
        count(
            "module-shadows-parent",
            "module A\nbaremodule A\nend\nend\n"
        ),
        1
    );
}

#[test]
fn module_shadows_parent_ignores_distinct_names() {
    assert_eq!(
        count("module-shadows-parent", "module A\nmodule B\nend\nend\n"),
        0
    );
}

#[test]
fn module_shadows_parent_ignores_top_level_module() {
    assert_eq!(count("module-shadows-parent", "module A\nend\n"), 0);
}

#[test]
fn module_shadows_parent_ignores_grandparent_match() {
    // Only the direct parent counts: `A.B.A` is unusual but unambiguous.
    assert_eq!(
        count(
            "module-shadows-parent",
            "module A\nmodule B\nmodule A\nend\nend\nend\n"
        ),
        0
    );
}

#[test]
fn module_shadows_parent_flags_each_shadowing_sibling() {
    assert_eq!(
        count(
            "module-shadows-parent",
            "module A\nmodule A\nend\nmodule A\nend\nend\n"
        ),
        2
    );
}

#[test]
fn module_shadows_parent_stays_silent_in_quotes_and_macro_calls() {
    // Quoted code is data, and a macro may rewrite its argument into anything.
    assert_eq!(
        count(
            "module-shadows-parent",
            "module A\nquote\nmodule A\nend\nend\nend\n"
        ),
        0
    );
    assert_eq!(
        count(
            "module-shadows-parent",
            "module A\n@eval module A\nend\nend\n"
        ),
        0
    );
}

// --- noteq-definition --------------------------------------------------------

#[test]
fn noteq_definition_flags_long_form() {
    let msgs = findings(
        "noteq-definition",
        "function !=(a, b)\n    !(a == b)\nend\n",
    );
    assert_eq!(msgs.len(), 1);
    assert!(msgs[0].contains("`==`"), "message points at `==`: {msgs:?}");
}

#[test]
fn noteq_definition_flags_short_form() {
    assert_eq!(count("noteq-definition", "!=(a::Foo, b::Foo) = true\n"), 1);
}

#[test]
fn noteq_definition_flags_infix_short_form() {
    // `a != b = true` is a legal infix definition of `!=`.
    assert_eq!(count("noteq-definition", "a != b = true\n"), 1);
}

#[test]
fn noteq_definition_flags_unicode_infix_form() {
    assert_eq!(count("noteq-definition", "a \u{2260} b = true\n"), 1);
}

#[test]
fn noteq_definition_flags_unicode_prefix_form() {
    // `≠(a, b) = ...` — the unicode operator as a call name.
    assert_eq!(count("noteq-definition", "\u{2260}(a, b) = !(a == b)\n"), 1);
    assert_eq!(
        count(
            "noteq-definition",
            "function \u{2260}(a, b)\n    !(a == b)\nend\n"
        ),
        1
    );
    // Another unicode comparison operator is not `!=`.
    assert_eq!(count("noteq-definition", "\u{2264}(a, b) = true\n"), 0);
}

#[test]
fn noteq_definition_flags_qualified_forms() {
    // `Base.:!=` and `Base.:(!=)`, in both the short and the long form.
    assert_eq!(count("noteq-definition", "Base.:!=(a, b) = true\n"), 1);
    assert_eq!(
        count(
            "noteq-definition",
            "function Base.:(!=)(a, b)\n    true\nend\n"
        ),
        1
    );
}

#[test]
fn noteq_definition_flags_parenthesized_callee() {
    assert_eq!(count("noteq-definition", "(!=)(a, b) = false\n"), 1);
}

#[test]
fn noteq_definition_peels_where_and_return_type() {
    assert_eq!(
        count("noteq-definition", "!=(a::T, b::T) where {T} = true\n"),
        1
    );
    assert_eq!(count("noteq-definition", "!=(a, b)::Bool = true\n"), 1);
}

#[test]
fn noteq_definition_ignores_comparisons_and_calls() {
    // Using `!=` is fine; only defining it is flagged.
    assert_eq!(count("noteq-definition", "a != b\n"), 0);
    assert_eq!(count("noteq-definition", "x = a != b\n"), 0);
    assert_eq!(count("noteq-definition", "!=(a, b)\n"), 0);
    assert_eq!(count("noteq-definition", "x = !=(a, b)\n"), 0);
}

#[test]
fn noteq_definition_ignores_eqeq_definition() {
    // Defining `==` is exactly what the rule asks for.
    assert_eq!(count("noteq-definition", "==(a::Foo, b::Foo) = true\n"), 0);
    assert_eq!(
        count("noteq-definition", "function ==(a, b)\n    true\nend\n"),
        0
    );
}

#[test]
fn noteq_definition_ignores_keyword_default_comparison() {
    // A `!=` comparison as a keyword default is a use, not a definition.
    assert_eq!(count("noteq-definition", "f(; x = a != b) = x\n"), 0);
}

// --- unused-type-parameter ---------------------------------------------------

#[test]
fn unused_type_parameter_flags_short_form() {
    assert_eq!(
        findings("unused-type-parameter", "f(x) where T = x\n"),
        ["type parameter `T` is never used"]
    );
}

#[test]
fn unused_type_parameter_flags_long_form_braced() {
    assert_eq!(
        count(
            "unused-type-parameter",
            "function f(x) where {T}\n    x\nend\n"
        ),
        1
    );
}

#[test]
fn unused_type_parameter_flags_only_the_unused_param() {
    assert_eq!(
        findings("unused-type-parameter", "f(x::S) where {T, S} = x\n"),
        ["type parameter `T` is never used"]
    );
}

#[test]
fn unused_type_parameter_flags_in_chained_where() {
    assert_eq!(
        findings("unused-type-parameter", "f(x::S) where T where S = x\n"),
        ["type parameter `T` is never used"]
    );
}

#[test]
fn unused_type_parameter_ignores_annotation_use() {
    assert_eq!(
        count("unused-type-parameter", "f(x::T) where {T<:Number} = x\n"),
        0
    );
}

#[test]
fn unused_type_parameter_ignores_operator_signature() {
    // Operator-named methods bind their parameters in the function scope too,
    // so a `::T` annotation counts as a use of the `where` parameter.
    assert_eq!(
        count(
            "unused-type-parameter",
            "+(x::T, y::T) where {T<:Real} = x\n"
        ),
        0
    );
    assert_eq!(
        count(
            "unused-type-parameter",
            "==(a::T, b::T) where {T} = a === b\n"
        ),
        0
    );
}

#[test]
fn unused_type_parameter_ignores_body_use() {
    assert_eq!(
        count(
            "unused-type-parameter",
            "function f(x) where {T}\n    convert(T, x)\nend\n"
        ),
        0
    );
}

#[test]
fn unused_type_parameter_ignores_type_selector_use() {
    assert_eq!(
        count("unused-type-parameter", "f(::Type{T}) where T = T\n"),
        0
    );
}

#[test]
fn unused_type_parameter_ignores_use_as_bound() {
    // `T` appears only as `S`'s upper bound — still a use.
    assert_eq!(
        count("unused-type-parameter", "f(x::S) where {T, S<:T} = x\n"),
        0
    );
}

#[test]
fn unused_type_parameter_ignores_struct_type_params() {
    // Phantom struct parameters (`struct Unit{T} end`) are idiomatic Julia;
    // only `where` clause parameters are in scope for this rule.
    assert_eq!(count("unused-type-parameter", "struct Unit{T}\nend\n"), 0);
}

#[test]
fn unused_type_parameter_ignores_constructor_curly_callee() {
    // The `P{T}` callee of a parametric inner constructor reads `T`.
    assert_eq!(
        count(
            "unused-type-parameter",
            "struct P{T}\n    P{T}() where T = new()\nend\n"
        ),
        0
    );
}

#[test]
fn unused_type_parameter_skips_underscore_names() {
    assert_eq!(count("unused-type-parameter", "f(x) where _ = x\n"), 0);
}

#[test]
fn unused_type_parameter_stays_silent_in_quoted_code() {
    assert_eq!(count("unused-type-parameter", ":(f(x) where T = x)\n"), 0);
}

// --- missing-include-file --------------------------------------------------

/// Lint `src` as if it lived at `path`, with only `rule` enabled. The file at
/// `path` need not exist; `path` supplies the base directory the include graph
/// resolves relative targets against.
fn findings_at(rule: &str, path: &std::path::Path, src: &str) -> Vec<String> {
    let config = LintConfig {
        select: Some(vec![rule.to_string()]),
        ..Default::default()
    };
    let report = check_source(Some(path), src, &config);
    report
        .diagnostics
        .into_iter()
        .filter(|d| d.rule == rule)
        .map(|d| d.message.body)
        .collect()
}

fn count_at(rule: &str, path: &std::path::Path, src: &str) -> usize {
    findings_at(rule, path, src).len()
}

#[test]
fn missing_include_file_flags_nonexistent_target() {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("main.jl");
    assert_eq!(
        count_at("missing-include-file", &main, "include(\"missing.jl\")\n"),
        1
    );
}

#[test]
fn missing_include_file_ignores_existing_target() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.jl"), "x = 1\n").unwrap();
    let main = dir.path().join("main.jl");
    assert_eq!(
        count_at("missing-include-file", &main, "include(\"a.jl\")\n"),
        0
    );
}

#[test]
fn missing_include_file_flags_directory_target() {
    // `include` of a directory throws just like a missing file.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    let main = dir.path().join("main.jl");
    assert_eq!(
        count_at("missing-include-file", &main, "include(\"sub\")\n"),
        1
    );
}

#[test]
fn missing_include_file_ignores_dynamic_includes() {
    // Dynamic, interpolated, qualified, and two-argument includes cannot be
    // resolved statically and are skipped.
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("main.jl");
    let src = "include(x)\ninclude(\"$d/a.jl\")\nM.include(\"a.jl\")\ninclude(f, \"a.jl\")\n";
    assert_eq!(count_at("missing-include-file", &main, src), 0);
}

#[test]
fn missing_include_file_stays_silent_without_a_path() {
    // A pathless document (stdin) has no base directory to resolve against.
    assert_eq!(
        count("missing-include-file", "include(\"missing.jl\")\n"),
        0
    );
}

#[test]
fn missing_include_file_is_suppressible() {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("main.jl");
    assert_eq!(
        count_at(
            "missing-include-file",
            &main,
            "# fatou-ignore missing-include-file\ninclude(\"missing.jl\")\n"
        ),
        0
    );
}

// --- include-cycle ---------------------------------------------------------

#[test]
fn include_cycle_flags_self_include() {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("main.jl");
    assert_eq!(
        count_at("include-cycle", &main, "include(\"main.jl\")\n"),
        1
    );
}

#[test]
fn include_cycle_flags_two_file_cycle() {
    // `a.jl` on disk includes us back; the cycle closes through a file that is
    // not part of the lint set.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.jl"), "include(\"main.jl\")\n").unwrap();
    let main = dir.path().join("main.jl");
    assert_eq!(count_at("include-cycle", &main, "include(\"a.jl\")\n"), 1);
}

#[test]
fn include_cycle_ignores_diamond() {
    // Two paths to the same file are not a cycle.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.jl"), "include(\"c.jl\")\n").unwrap();
    std::fs::write(dir.path().join("b.jl"), "include(\"c.jl\")\n").unwrap();
    std::fs::write(dir.path().join("c.jl"), "x = 1\n").unwrap();
    let main = dir.path().join("main.jl");
    assert_eq!(
        count_at(
            "include-cycle",
            &main,
            "include(\"a.jl\")\ninclude(\"b.jl\")\n"
        ),
        0
    );
}

#[test]
fn include_cycle_ignores_acyclic_chain() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.jl"), "include(\"b.jl\")\n").unwrap();
    std::fs::write(dir.path().join("b.jl"), "x = 1\n").unwrap();
    let main = dir.path().join("main.jl");
    assert_eq!(count_at("include-cycle", &main, "include(\"a.jl\")\n"), 0);
}

#[test]
fn include_cycle_does_not_flag_the_missing_rule_and_vice_versa() {
    // A missing target is not a cycle, and a cycle's target is not missing.
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("main.jl");
    assert_eq!(
        count_at("include-cycle", &main, "include(\"missing.jl\")\n"),
        0
    );
    assert_eq!(
        count_at("missing-include-file", &main, "include(\"main.jl\")\n"),
        0
    );
}

// --- duplicate-include -----------------------------------------------------

#[test]
fn duplicate_include_flags_a_repeated_target() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.jl"), "x = 1\n").unwrap();
    let main = dir.path().join("main.jl");
    assert_eq!(
        count_at(
            "duplicate-include",
            &main,
            "include(\"a.jl\")\ninclude(\"a.jl\")\n"
        ),
        1
    );
}

#[test]
fn duplicate_include_flags_every_repeat_after_the_first() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.jl"), "x = 1\n").unwrap();
    let main = dir.path().join("main.jl");
    assert_eq!(
        count_at(
            "duplicate-include",
            &main,
            "include(\"a.jl\")\ninclude(\"a.jl\")\ninclude(\"a.jl\")\n"
        ),
        2
    );
}

#[test]
fn duplicate_include_flags_the_repeat_not_the_first_include() {
    // The finding lands on the second call's literal, the one that re-runs the
    // file — not on the include that legitimately brought it in.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.jl"), "x = 1\n").unwrap();
    let main = dir.path().join("main.jl");
    let src = "include(\"a.jl\")\ninclude(\"a.jl\")\n";
    let config = LintConfig {
        select: Some(vec!["duplicate-include".to_string()]),
        ..Default::default()
    };
    let report = check_source(Some(&main), src, &config);
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(
        usize::from(report.diagnostics[0].range.start()),
        src.rfind("\"a.jl\"").unwrap()
    );
}

#[test]
fn duplicate_include_sees_through_a_differently_spelled_path() {
    // Both literals resolve to the same file, so the second one is a repeat.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.jl"), "x = 1\n").unwrap();
    let main = dir.path().join("main.jl");
    assert_eq!(
        count_at(
            "duplicate-include",
            &main,
            "include(\"a.jl\")\ninclude(\"./a.jl\")\n"
        ),
        1
    );
}

#[test]
fn duplicate_include_flags_a_repeat_of_a_missing_file() {
    // Repetition is a property of the source text: whether the target exists is
    // `missing-include-file`'s business.
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("main.jl");
    assert_eq!(
        count_at(
            "duplicate-include",
            &main,
            "include(\"gone.jl\")\ninclude(\"gone.jl\")\n"
        ),
        1
    );
}

#[test]
fn duplicate_include_ignores_distinct_targets() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.jl"), "x = 1\n").unwrap();
    std::fs::write(dir.path().join("b.jl"), "y = 2\n").unwrap();
    let main = dir.path().join("main.jl");
    assert_eq!(
        count_at(
            "duplicate-include",
            &main,
            "include(\"a.jl\")\ninclude(\"b.jl\")\n"
        ),
        0
    );
}

#[test]
fn duplicate_include_ignores_repeats_in_distinct_modules() {
    // Including the same file into two modules runs its definitions into two
    // separate namespaces, which is the point.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.jl"), "x = 1\n").unwrap();
    let main = dir.path().join("main.jl");
    let src = "module A\ninclude(\"a.jl\")\nend\nmodule B\ninclude(\"a.jl\")\nend\n";
    assert_eq!(count_at("duplicate-include", &main, src), 0);
}

#[test]
fn duplicate_include_flags_repeats_within_one_module() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.jl"), "x = 1\n").unwrap();
    let main = dir.path().join("main.jl");
    let src = "module A\ninclude(\"a.jl\")\ninclude(\"a.jl\")\nend\n";
    assert_eq!(count_at("duplicate-include", &main, src), 1);
}

#[test]
fn duplicate_include_ignores_a_diamond() {
    // Two included files that both include a third are not a repeat *here*:
    // the rule only reports a file this file includes twice itself.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.jl"), "include(\"c.jl\")\n").unwrap();
    std::fs::write(dir.path().join("b.jl"), "include(\"c.jl\")\n").unwrap();
    std::fs::write(dir.path().join("c.jl"), "x = 1\n").unwrap();
    let main = dir.path().join("main.jl");
    assert_eq!(
        count_at(
            "duplicate-include",
            &main,
            "include(\"a.jl\")\ninclude(\"b.jl\")\n"
        ),
        0
    );
}

#[test]
fn duplicate_include_ignores_dynamic_includes() {
    // Repeated but not statically resolvable: the paths are unknown.
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("main.jl");
    let src = "include(x)\ninclude(x)\ninclude(\"$d/a.jl\")\ninclude(\"$d/a.jl\")\n";
    assert_eq!(count_at("duplicate-include", &main, src), 0);
}

#[test]
fn duplicate_include_stays_silent_without_a_path() {
    // A pathless document (stdin) has no base directory to resolve against.
    assert_eq!(
        count(
            "duplicate-include",
            "include(\"a.jl\")\ninclude(\"a.jl\")\n"
        ),
        0
    );
}

#[test]
fn duplicate_include_is_suppressible() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.jl"), "x = 1\n").unwrap();
    let main = dir.path().join("main.jl");
    assert_eq!(
        count_at(
            "duplicate-include",
            &main,
            "include(\"a.jl\")\n# fatou-ignore duplicate-include\ninclude(\"a.jl\")\n"
        ),
        0
    );
}

// --- call-arity -------------------------------------------------------------

#[test]
fn call_arity_flags_extra_positional_args() {
    let msgs = findings("call-arity", "half(x) = x / 2\nhalf(1, 2)\n");
    assert_eq!(msgs.len(), 1, "{msgs:?}");
    assert!(msgs[0].contains("half"), "{msgs:?}");
    assert!(msgs[0].contains('2'), "{msgs:?}");
}

#[test]
fn call_arity_flags_missing_positional_args() {
    assert_eq!(
        count("call-arity", "function f(x, y)\n    x + y\nend\nf(1)\n"),
        1
    );
}

#[test]
fn call_arity_respects_defaulted_positionals() {
    // `f()` (too few) and `f(1, 2, 3)` (too many) flag; the two in-range
    // calls do not.
    assert_eq!(
        count(
            "call-arity",
            "f(x, y = 2) = x + y\nf(1)\nf(1, 2)\nf()\nf(1, 2, 3)\n"
        ),
        2
    );
}

#[test]
fn call_arity_respects_vararg() {
    assert_eq!(
        count("call-arity", "f(x, xs...) = x\nf()\nf(1)\nf(1, 2, 3, 4)\n"),
        1
    );
}

#[test]
fn call_arity_checks_the_whole_dispatch_group() {
    // Any method admitting the count clears the call.
    assert_eq!(
        count(
            "call-arity",
            "f(x) = 1\nf(x, y) = 2\nf(1)\nf(1, 2)\nf(1, 2, 3)\n"
        ),
        1
    );
}

#[test]
fn call_arity_flags_unknown_keyword() {
    let msgs = findings("call-arity", "f(x; a = 1) = x\nf(1, b = 2)\n");
    assert_eq!(msgs.len(), 1, "{msgs:?}");
    assert!(msgs[0].contains('b'), "{msgs:?}");
}

#[test]
fn call_arity_accepts_declared_keywords() {
    assert_eq!(
        count("call-arity", "f(x; a = 1) = x\nf(1; a = 2)\nf(1, a = 3)\n"),
        0
    );
}

#[test]
fn call_arity_accepts_shorthand_keyword() {
    // `f(1; a)` passes the binding `a` as the keyword `a`.
    assert_eq!(count("call-arity", "f(x; a = 1) = x\na = 1\nf(1; a)\n"), 0);
}

#[test]
fn call_arity_skips_calls_with_positional_splat() {
    assert_eq!(count("call-arity", "f(x) = x\nxs = (1, 2)\nf(xs...)\n"), 0);
}

#[test]
fn call_arity_keyword_splat_skips_the_keyword_check() {
    // The splat may carry any keyword name...
    assert_eq!(
        count(
            "call-arity",
            "f(x; a = 1) = x\nkw = (b = 2,)\nf(1; kw...)\n"
        ),
        0
    );
    // ...but the positional count is still checked.
    assert_eq!(
        count("call-arity", "f(x; a = 1) = x\nkw = (a = 1,)\nf(; kw...)\n"),
        1
    );
}

#[test]
fn call_arity_skips_do_block_calls() {
    // The `do` block passes a leading function argument invisibly.
    assert_eq!(
        count("call-arity", "f(g, x) = g(x)\nf(1) do x\n    x\nend\n"),
        0
    );
}

#[test]
fn call_arity_skips_macro_calls_and_quotes() {
    assert_eq!(
        count(
            "call-arity",
            "f(x) = x\n@info f(1, 2)\nex = :(f(1, 2))\nblock = quote\n    f(1, 2)\nend\n"
        ),
        0
    );
}

#[test]
fn call_arity_eval_bails_the_file() {
    // `eval` may define further methods the model cannot see.
    assert_eq!(
        count("call-arity", "f(x) = x\neval(:(f(x, y) = x))\nf(1, 2)\n"),
        0
    );
}

#[test]
fn call_arity_include_bails_without_workspace() {
    // An included sibling may add methods of `f`.
    assert_eq!(
        count("call-arity", "f(x) = x\ninclude(\"more.jl\")\nf(1, 2)\n"),
        0
    );
}

#[test]
fn call_arity_unresolvable_using_bails_the_file() {
    // `Mystery` may export an `f` that masks the file's own.
    assert_eq!(count("call-arity", "using Mystery\nf(x) = x\nf(1, 2)\n"), 0);
}

#[test]
fn call_arity_skips_constructors() {
    // Implicit and inner constructors are invisible to the harvest.
    assert_eq!(
        count("call-arity", "struct P\n    x\n    y\nend\nP(1, 2, 3)\n"),
        0
    );
    // A same-named outer-constructor group does not re-enable the check.
    assert_eq!(
        count("call-arity", "struct Q\n    x\nend\nQ(x, y) = Q(x)\nQ(1)\n"),
        0
    );
}

#[test]
fn call_arity_skips_callable_values() {
    assert_eq!(count("call-arity", "g = sin\ng(1, 2)\n"), 0);
}

#[test]
fn call_arity_skips_local_functions() {
    // Closures never reach the harvest's method table.
    assert_eq!(
        count(
            "call-arity",
            "function outer()\n    inner(x) = x\n    inner(1, 2)\nend\nouter()\n"
        ),
        0
    );
}

#[test]
fn call_arity_skips_bodyless_declarations() {
    // `function f end` announces methods defined elsewhere.
    assert_eq!(count("call-arity", "function f end\nf(1, 2)\n"), 0);
}

#[test]
fn call_arity_is_silent_for_base_calls_on_the_fallback_snapshot() {
    // The CLI's baked-in Base index carries names, not signatures.
    assert_eq!(count("call-arity", "sqrt(1.0, 2.0, 3.0)\n"), 0);
}

#[test]
fn call_arity_checks_inside_nested_modules() {
    assert_eq!(count("call-arity", "module A\nf(x) = x\nf(1, 2)\nend\n"), 1);
}

// --- redefined-constant ----------------------------------------------------

#[test]
fn redefined_constant_flags_const_reassignment() {
    let msgs = findings("redefined-constant", "const x = 1\nx = 2\n");
    assert_eq!(msgs.len(), 1);
    assert!(msgs[0].contains("reassignment of constant `x`"), "{msgs:?}");
}

#[test]
fn redefined_constant_flags_const_redeclaration() {
    assert_eq!(count("redefined-constant", "const x = 1\nconst x = 2\n"), 1);
}

#[test]
fn redefined_constant_flags_augmented_const_write() {
    assert_eq!(count("redefined-constant", "const x = 1\nx += 1\n"), 1);
}

#[test]
fn redefined_constant_flags_function_over_value() {
    let msgs = findings("redefined-constant", "x = 1\nfunction x()\nend\n");
    assert_eq!(msgs.len(), 1);
    assert!(msgs[0].contains("cannot define function `x`"), "{msgs:?}");
}

#[test]
fn redefined_constant_flags_short_form_over_value() {
    assert_eq!(count("redefined-constant", "x = 1\nx() = 2\n"), 1);
}

#[test]
fn redefined_constant_flags_const_over_value() {
    let msgs = findings("redefined-constant", "x = 1\nconst x = 2\n");
    assert_eq!(msgs.len(), 1);
    assert!(msgs[0].contains("cannot declare `x` constant"), "{msgs:?}");
}

#[test]
fn redefined_constant_flags_function_name_reassignment() {
    assert_eq!(count("redefined-constant", "f() = 1\nf = 2\n"), 1);
}

#[test]
fn redefined_constant_flags_type_name_reassignment() {
    assert_eq!(count("redefined-constant", "struct S\nend\nS = 1\n"), 1);
}

#[test]
fn redefined_constant_flags_local_function_over_value() {
    assert_eq!(
        count(
            "redefined-constant",
            "function g()\n    x = 1\n    x() = 2\nend\n"
        ),
        1
    );
}

#[test]
fn redefined_constant_ignores_plain_reassignment() {
    assert_eq!(count("redefined-constant", "x = 1\nx = 2\n"), 0);
}

#[test]
fn redefined_constant_ignores_method_addition() {
    assert_eq!(count("redefined-constant", "f() = 1\nf(x) = 2\n"), 0);
}

#[test]
fn redefined_constant_ignores_outer_constructor() {
    // `S(x) = ...` over a struct name defines an outer constructor.
    assert_eq!(
        count("redefined-constant", "struct S\nend\nS(x) = S()\n"),
        0
    );
}

#[test]
fn redefined_constant_ignores_disjoint_if_branches() {
    // Only one branch runs, so the second `const` is legal.
    assert_eq!(
        count(
            "redefined-constant",
            "if a\n    const x = 1\nelse\n    const x = 2\nend\n"
        ),
        0
    );
}

#[test]
fn redefined_constant_ignores_disjoint_elseif_chain() {
    assert_eq!(
        count(
            "redefined-constant",
            "if a\n    const x = 1\nelseif b\n    const x = 2\nelse\n    const x = 3\nend\n"
        ),
        0
    );
}

#[test]
fn redefined_constant_ignores_disjoint_function_over_value() {
    assert_eq!(
        count(
            "redefined-constant",
            "if a\n    x = 1\nelse\n    x() = 2\nend\n"
        ),
        0
    );
}

#[test]
fn redefined_constant_flags_same_branch_reassignment() {
    assert_eq!(
        count(
            "redefined-constant",
            "if a\n    const x = 1\n    x = 2\nend\n"
        ),
        1
    );
}

#[test]
fn redefined_constant_flags_write_after_branched_def() {
    // The write outside the `if` is not in a disjoint branch of the def.
    assert_eq!(
        count("redefined-constant", "if a\n    const x = 1\nend\nx = 2\n"),
        1
    );
}

#[test]
fn redefined_constant_ignores_local_closure_rebind() {
    // A local function name is an ordinary local, not a constant.
    assert_eq!(
        count(
            "redefined-constant",
            "function g()\n    f() = 1\n    f = 2\nend\n"
        ),
        0
    );
}

#[test]
fn redefined_constant_ignores_macro_namespace() {
    // `@x` and the value `x` live in different namespaces.
    assert_eq!(count("redefined-constant", "x = 1\nmacro x()\nend\n"), 0);
    assert_eq!(count("redefined-constant", "macro m()\nend\nm = 1\n"), 0);
}

#[test]
fn redefined_constant_ignores_struct_redefinition() {
    // An identical struct redefinition is legal; field comparison is out of
    // scope, so type-over-type stays silent.
    assert_eq!(
        count("redefined-constant", "struct S\nend\nstruct S\nend\n"),
        0
    );
}

#[test]
fn redefined_constant_ignores_imported_name() {
    assert_eq!(count("redefined-constant", "import A\nA = 1\n"), 0);
}

#[test]
fn redefined_constant_ignores_global_and_local_declarations() {
    // A bare `global`/`local` declaration is not a write of the name.
    assert_eq!(
        count(
            "redefined-constant",
            "length(r::Int) = r\nlet\n    global length\n    length(x::Float64) = x\nend\n"
        ),
        0
    );
    assert_eq!(
        count(
            "redefined-constant",
            "const K = 1\nfunction f()\n    local K\n    K = 2\nend\n"
        ),
        0
    );
}

#[test]
fn redefined_constant_ignores_method_on_forward_declared_global() {
    // `global f` forward-declares; the following method definitions are the
    // first real definition, not a redefinition of a value.
    assert_eq!(
        count(
            "redefined-constant",
            "function outer()\n    global isaint\n    isaint(a::Int) = true\n    isaint(a) = false\nend\n"
        ),
        0
    );
}

#[test]
fn redefined_constant_ignores_constructor_on_const_type_alias() {
    // Adding constructors to a `const` that aliases a type is legal.
    assert_eq!(
        count(
            "redefined-constant",
            "const Tok = Core.Token\nTok(x::Int) = new(x)\n"
        ),
        0
    );
}

#[test]
fn redefined_constant_flags_function_over_const_literal() {
    // A `const` bound to a non-callable literal still collides with a later
    // function definition.
    let msgs = findings("redefined-constant", "const c = 0\nc() = 1\n");
    assert_eq!(msgs.len(), 1);
    assert!(msgs[0].contains("cannot define function `c`"), "{msgs:?}");
}

#[test]
fn redefined_constant_ignores_const_field_in_constructor() {
    // A `const` struct field is a field, not an assignable constant: an inner
    // constructor's like-named local does not reassign it.
    let src = "mutable struct Foo\n    const edges::Vector{Any}\n    function Foo()\n        edges = Any[]\n        new(edges)\n    end\nend\n";
    assert_eq!(count("redefined-constant", src), 0);
}

#[test]
fn redefined_constant_ignores_assignment_in_macro_block() {
    // A name assigned inside a macro-call block (`@recipe ... begin x = ... end`)
    // is the macro's DSL, not a global constant: a later top-level definition of
    // the same name does not redefine anything the linter can see.
    let src = "@recipe Foo (a,) begin\n    marker = automatic\nend\nfunction marker(x)\n    x\nend\nmarker(x, y) = y\n";
    assert_eq!(count("redefined-constant", src), 0);
}

#[test]
fn redefined_constant_ignores_name_across_macro_blocks() {
    // The same local name in two separate macro-call blocks (e.g. two
    // `@testset`/`@reference_test` bodies) does not collide: each block is its
    // own scope once the macro expands.
    let src = "@block begin\n    f = value\nend\n@block begin\n    f(x) = x\nend\n";
    assert_eq!(count("redefined-constant", src), 0);
}

// --- julia-version-compat ----------------------------------------------------

use fatou::julia_version::{Version, parse_compat};

/// Lint `src` for `julia-version-compat` under a given target floor (a Julia
/// compat spec), returning the messages produced.
fn version_findings(target: &str, src: &str) -> Vec<String> {
    let config = LintConfig {
        select: Some(vec!["julia-version-compat".to_string()]),
        ..Default::default()
    };
    let report = fatou::linter::check_source_with_target(
        None,
        src,
        &config,
        Some(parse_compat(target).unwrap()),
    );
    report
        .diagnostics
        .into_iter()
        .filter(|d| d.rule == "julia-version-compat")
        .map(|d| d.message.body)
        .collect()
}

#[test]
fn version_compat_flags_public_below_1_11() {
    let msgs = version_findings("1.10", "module M\npublic foo\nend\n");
    assert_eq!(msgs.len(), 1);
    assert!(
        msgs[0].contains("public") && msgs[0].contains("1.11"),
        "{msgs:?}"
    );
}

#[test]
fn version_compat_allows_public_at_1_11() {
    assert!(version_findings("1.11", "module M\npublic foo\nend\n").is_empty());
}

#[test]
fn version_compat_flags_import_as_below_1_6() {
    let msgs = version_findings("1.5", "import A as B\n");
    assert_eq!(msgs.len(), 1);
    assert!(
        msgs[0].contains("as") && msgs[0].contains("1.6"),
        "{msgs:?}"
    );
}

#[test]
fn version_compat_flags_using_as_below_1_6() {
    assert_eq!(version_findings("1.0", "using C: d as e\n").len(), 1);
}

#[test]
fn version_compat_allows_import_as_at_1_6() {
    assert!(version_findings("1.6", "import A as B\n").is_empty());
}

#[test]
fn version_compat_flags_both_features_under_old_floor() {
    let msgs = version_findings("1.0", "module M\npublic foo\nimport A as B\nend\n");
    assert_eq!(msgs.len(), 2);
}

#[test]
fn version_compat_silent_without_a_target() {
    // No declared target (bare file, no project) -> nothing to check against.
    let config = LintConfig {
        select: Some(vec!["julia-version-compat".to_string()]),
        ..Default::default()
    };
    let report = check_source(None, "module M\npublic foo\nend\n", &config);
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
}

#[test]
fn version_compat_uses_range_floor_not_ceiling() {
    // A "1.6 - 1.11" range must be judged by its floor (1.6), so `public`
    // (needs 1.11) is still flagged even though the ceiling reaches 1.11.
    let range = parse_compat("1.6 - 1.11").unwrap();
    assert_eq!(range.min, Version::new(1, 6, 0));
    let msgs = version_findings("1.6 - 1.11", "module M\npublic foo\nend\n");
    assert_eq!(msgs.len(), 1);
}

// --- type-piracy -------------------------------------------------------------
// These run in CLI mode (no workspace): a "foreign" name resolves against the
// built-in Base/Core snapshot, and an "owned" type is one defined in the file.

#[test]
fn type_piracy_flags_qualified_base_extension() {
    let msgs = findings("type-piracy", "Base.show(x::Int) = 0\n");
    assert_eq!(msgs.len(), 1, "{msgs:?}");
    assert!(msgs[0].contains("Base.show"), "{msgs:?}");
}

#[test]
fn type_piracy_flags_long_form_extension() {
    assert_eq!(count("type-piracy", "function Base.show(x::Int)\nend\n"), 1);
}

#[test]
fn type_piracy_flags_bare_imported_operator() {
    // `import Base: +` makes `+` a foreign (imported) function; `Int` is a
    // foreign type, so the method pirates.
    assert_eq!(
        count("type-piracy", "import Base: +\n+(a::Int, b::Int) = 0\n"),
        1
    );
}

#[test]
fn type_piracy_flags_bare_imported_name() {
    assert_eq!(
        count(
            "type-piracy",
            "import Base: getindex\ngetindex(x::Int) = 0\n"
        ),
        1
    );
}

#[test]
fn type_piracy_untyped_argument_is_any_and_does_not_rescue() {
    // An untyped positional argument is `Any` (a Base type), so it never makes
    // an otherwise-pirating method non-pirating.
    assert_eq!(count("type-piracy", "Base.show(x) = 0\n"), 1);
}

#[test]
fn type_piracy_unbounded_type_var_does_not_rescue() {
    // `T` is a `where` type variable, not a type you own.
    assert_eq!(
        count("type-piracy", "Base.show(x::Int, y::T) where {T} = 0\n"),
        1
    );
}

#[test]
fn type_piracy_owned_argument_type_is_clean() {
    assert_eq!(
        count(
            "type-piracy",
            "struct MyType end\nBase.show(x::MyType) = 0\n"
        ),
        0
    );
}

#[test]
fn type_piracy_owned_type_parameter_is_clean() {
    // `AbstractVector` is foreign, but the type parameter `MyType` is owned.
    assert_eq!(
        count(
            "type-piracy",
            "struct MyType end\nBase.show(x::AbstractVector{MyType}) = 0\n"
        ),
        0
    );
}

#[test]
fn type_piracy_owned_where_bound_is_clean() {
    assert_eq!(
        count(
            "type-piracy",
            "struct MyType end\nBase.show(x::T) where {T <: MyType} = 0\n"
        ),
        0
    );
}

#[test]
fn type_piracy_own_function_is_clean() {
    // A fresh function defined here (not imported) is owned.
    assert_eq!(count("type-piracy", "f(x::Int) = x\n"), 0);
}

#[test]
fn type_piracy_withholds_on_unresolved_argument_type() {
    // `Frobnicator` resolves nowhere: it might be an owned type the resolver
    // cannot see, so the finding is withheld.
    assert_eq!(count("type-piracy", "Base.show(x::Frobnicator) = 0\n"), 0);
}

#[test]
fn type_piracy_withholds_on_unresolved_qualifier() {
    assert_eq!(count("type-piracy", "Bad.frob(x::Int) = 0\n"), 0);
}

#[test]
fn type_piracy_skips_quoted_definition() {
    assert_eq!(count("type-piracy", "ex = :(Base.show(x::Int) = 0)\n"), 0);
}

#[test]
fn type_piracy_skips_macro_wrapped_definition() {
    // A macro may rewrite the signature, so its written shape is not trusted.
    assert_eq!(count("type-piracy", "@inline Base.show(x::Int) = 0\n"), 0);
}

#[test]
fn type_piracy_ignores_non_call_assignment() {
    // A qualified property assignment is not a method definition.
    assert_eq!(count("type-piracy", "Base.x = 1\n"), 0);
}

// --- index-from-length -------------------------------------------------------

#[test]
fn index_from_length_flags_length_range_indexing() {
    // `1:length(x)` used to index `x` -> suggest `eachindex`.
    assert_eq!(
        count("index-from-length", "for i in 1:length(x)\n    x[i]\nend\n"),
        1
    );
}

#[test]
fn index_from_length_flags_size_range_indexing() {
    // `1:size(x, 1)` used to index `x` -> suggest `axes`.
    assert_eq!(
        count(
            "index-from-length",
            "for i in 1:size(x, 1)\n    x[i]\nend\n"
        ),
        1
    );
}

#[test]
fn index_from_length_flags_index_using_the_loop_var_in_an_expression() {
    // The loop variable need only appear inside the index, not be the whole of it.
    assert_eq!(
        count(
            "index-from-length",
            "for i in 1:length(x)\n    y = x[i + 1]\nend\n"
        ),
        1
    );
}

#[test]
fn index_from_length_ignores_range_without_indexing() {
    // The loop variable is never used to index the collection: not this rule.
    assert_eq!(
        count(
            "index-from-length",
            "for i in 1:length(x)\n    println(i)\nend\n"
        ),
        0
    );
}

#[test]
fn index_from_length_ignores_indexing_a_different_collection() {
    assert_eq!(
        count("index-from-length", "for i in 1:length(x)\n    y[i]\nend\n"),
        0
    );
}

#[test]
fn index_from_length_ignores_plain_and_nonunit_ranges() {
    // A plain numeric upper bound is fine, and a lower bound other than `1`
    // is not `eachindex`-equivalent.
    assert_eq!(
        count("index-from-length", "for i in 1:10\n    x[i]\nend\n"),
        0
    );
    assert_eq!(
        count("index-from-length", "for i in 2:length(x)\n    x[i]\nend\n"),
        0
    );
}

#[test]
fn index_from_length_ignores_stepped_range() {
    // `1:2:length(x)` is not equivalent to `eachindex(x)`.
    assert_eq!(
        count(
            "index-from-length",
            "for i in 1:2:length(x)\n    x[i]\nend\n"
        ),
        0
    );
}

#[test]
fn index_from_length_ignores_eachindex() {
    assert_eq!(
        count(
            "index-from-length",
            "for i in eachindex(x)\n    x[i]\nend\n"
        ),
        0
    );
}

#[test]
fn index_from_length_carries_an_unsafe_eachindex_fix() {
    let config = LintConfig {
        select: Some(vec!["index-from-length".to_string()]),
        ..Default::default()
    };
    let src = "for i in 1:length(x)\n    x[i]\nend\n";
    let report = check_source(None, src, &config);
    let fix = &report.diagnostics[0].fixes[0];
    assert_eq!(fix.content, "eachindex");
    // The replacement spans exactly the `1:length` prefix, leaving the
    // argument list untouched.
    assert_eq!(&src[fix.start..fix.end], "1:length");
    // `eachindex` is only value-equivalent when the collection's indices are
    // one-based and dense, which we cannot know, so it needs `--unsafe-fixes`.
    assert_eq!(fix.applicability, fatou::linter::Applicability::Unsafe);
}

#[test]
fn index_from_length_carries_an_unsafe_axes_fix() {
    let config = LintConfig {
        select: Some(vec!["index-from-length".to_string()]),
        ..Default::default()
    };
    let src = "for j in 1:size(x, 2)\n    x[1, j]\nend\n";
    let report = check_source(None, src, &config);
    let fix = &report.diagnostics[0].fixes[0];
    assert_eq!(fix.content, "axes");
    assert_eq!(&src[fix.start..fix.end], "1:size");
    assert_eq!(fix.applicability, fatou::linter::Applicability::Unsafe);
}

#[test]
fn index_from_length_names_the_actual_dimension() {
    // With a plain two-argument `size` call, the message shows the real
    // dimension argument rather than the `d` placeholder.
    assert_eq!(
        findings(
            "index-from-length",
            "for j in 1:size(x, 2)\n    x[1, j]\nend\n"
        ),
        ["iterate `axes(x, 2)` instead of `1:size(x, 2)`"]
    );
}

#[test]
fn index_from_length_withholds_the_fix_for_odd_arities() {
    let config = LintConfig {
        select: Some(vec!["index-from-length".to_string()]),
        ..Default::default()
    };
    // `1:size(x)` has no dimension argument: `axes(x)` is a tuple of ranges,
    // not a range, so the rewrite would change what is iterated.
    let report = check_source(None, "for i in 1:size(x)\n    x[i]\nend\n", &config);
    assert_eq!(report.diagnostics.len(), 1);
    assert!(report.diagnostics[0].fixes.is_empty());
    // A two-argument `length` is not Base's `length`.
    let report = check_source(None, "for i in 1:length(x, y)\n    x[i]\nend\n", &config);
    assert_eq!(report.diagnostics.len(), 1);
    assert!(report.diagnostics[0].fixes.is_empty());
}

#[test]
fn index_from_length_withholds_the_fix_when_the_prefix_holds_a_comment() {
    let config = LintConfig {
        select: Some(vec!["index-from-length".to_string()]),
        ..Default::default()
    };
    // The edit replaces the `1:length` prefix; a comment in there would be
    // dropped, so the fix is withheld and only the finding reported.
    let report = check_source(
        None,
        "for i in 1:#= dim =#length(x)\n    x[i]\nend\n",
        &config,
    );
    assert_eq!(report.diagnostics.len(), 1);
    assert!(report.diagnostics[0].fixes.is_empty());
}

#[test]
fn index_from_length_flags_iterating_a_numeric_literal() {
    assert_eq!(count("index-from-length", "for i in 3.5\n    i\nend\n"), 1);
    assert_eq!(count("index-from-length", "for i in 5\n    i\nend\n"), 1);
}

#[test]
fn index_from_length_ignores_iterating_a_range_or_collection() {
    assert_eq!(count("index-from-length", "for i in 1:5\n    i\nend\n"), 0);
    assert_eq!(count("index-from-length", "for x in xs\n    x\nend\n"), 0);
}

// --- discouraged-function ----------------------------------------------------

/// Lint `src` with only `discouraged-function` enabled, under `rules`.
fn discouraged(rules: DiscouragedFunctionConfig, src: &str) -> Vec<String> {
    let config = LintConfig {
        select: Some(vec!["discouraged-function".to_string()]),
        rules: RulesConfig {
            discouraged_function: rules,
        },
        ..Default::default()
    };
    check_source(None, src, &config)
        .diagnostics
        .into_iter()
        .filter(|d| d.rule == "discouraged-function")
        .map(|d| d.message.body)
        .collect()
}

/// A deny-list table: `functions` replacing the built-ins, `extend` added on top.
fn deny_list(functions: &[(&str, &str)], extend: &[(&str, &str)]) -> DiscouragedFunctionConfig {
    let owned = |pairs: &[(&str, &str)]| {
        pairs
            .iter()
            .map(|(n, s)| (n.to_string(), s.to_string()))
            .collect()
    };
    DiscouragedFunctionConfig {
        functions: owned(functions),
        extend_functions: owned(extend),
    }
}

#[test]
fn discouraged_function_flags_a_builtin_entry() {
    let found = discouraged(
        DiscouragedFunctionConfig::default(),
        "function f()\n    exit(1)\nend\n",
    );
    assert_eq!(found.len(), 1);
    assert!(
        found[0].starts_with("`exit` is discouraged:"),
        "unexpected message: {}",
        found[0]
    );
}

#[test]
fn discouraged_function_spans_the_callee_only() {
    let src = "function f()\n    exit(1)\nend\n";
    let config = LintConfig {
        select: Some(vec!["discouraged-function".to_string()]),
        ..Default::default()
    };
    let report = check_source(None, src, &config);
    let diag = report
        .diagnostics
        .iter()
        .find(|d| d.rule == "discouraged-function")
        .expect("expected a finding");
    assert_eq!(&src[diag.range], "exit");
}

#[test]
fn discouraged_function_ignores_the_do_block_form() {
    // `cd(dir) do ... end` is the alternative the built-in entry recommends.
    assert!(
        discouraged(
            DiscouragedFunctionConfig::default(),
            "cd(\"/tmp\") do\n    rm(\"x\")\nend\n",
        )
        .is_empty()
    );
}

#[test]
fn discouraged_function_ignores_a_qualified_callee() {
    assert!(
        discouraged(
            DiscouragedFunctionConfig::default(),
            "function f()\n    Base.exit(1)\nend\n",
        )
        .is_empty()
    );
}

#[test]
fn discouraged_function_ignores_a_local_shadowing_a_builtin() {
    assert!(
        discouraged(
            DiscouragedFunctionConfig::default(),
            "function f()\n    exit = x -> x\n    exit(1)\nend\n",
        )
        .is_empty()
    );
}

#[test]
fn discouraged_function_ignores_a_value_read_that_is_not_a_call() {
    assert!(
        discouraged(
            DiscouragedFunctionConfig::default(),
            "function f(xs)\n    map(exit, xs)\nend\n",
        )
        .is_empty()
    );
}

#[test]
fn discouraged_function_ignores_a_definition_signature() {
    // A signature is a `CALL_EXPR` too, but it declares rather than calls.
    assert!(
        discouraged(
            DiscouragedFunctionConfig::default(),
            "function exit(code)\n    code\nend\n",
        )
        .is_empty()
    );
}

#[test]
fn discouraged_function_functions_replaces_the_builtin_set() {
    let cfg = deny_list(&[("sleep", "use a timer")], &[]);
    assert_eq!(
        discouraged(cfg.clone(), "function f()\n    sleep(1)\nend\n").len(),
        1
    );
    assert!(
        discouraged(cfg, "function f()\n    exit(1)\nend\n").is_empty(),
        "the built-ins are replaced, not extended"
    );
}

#[test]
fn discouraged_function_extend_functions_keeps_the_builtin_set() {
    let cfg = DiscouragedFunctionConfig {
        extend_functions: deny_list(&[], &[("sleep", "use a timer")]).extend_functions,
        ..Default::default()
    };
    assert_eq!(
        discouraged(cfg.clone(), "function f()\n    sleep(1)\nend\n").len(),
        1
    );
    assert_eq!(
        discouraged(cfg, "function f()\n    exit(1)\nend\n").len(),
        1
    );
}

#[test]
fn discouraged_function_empty_table_silences_the_rule() {
    assert!(discouraged(deny_list(&[], &[]), "function f()\n    exit(1)\nend\n").is_empty());
}

#[test]
fn discouraged_function_flags_a_user_added_non_base_name() {
    // A project-configured name cannot be confirmed against Base, so it is
    // reported on the weaker shadow check alone.
    assert_eq!(
        discouraged(
            deny_list(&[("helper", "inline it")], &[]),
            "function f()\n    helper(1)\nend\n",
        )
        .len(),
        1
    );
}

#[test]
fn discouraged_function_ignores_a_shadowed_user_added_name() {
    assert!(
        discouraged(
            deny_list(&[("helper", "inline it")], &[]),
            "function f()\n    helper = x -> x\n    helper(1)\nend\n",
        )
        .is_empty()
    );
}

#[test]
fn discouraged_function_honors_suppression() {
    let config = LintConfig {
        select: Some(vec!["discouraged-function".to_string()]),
        ..Default::default()
    };
    let report = check_source(
        None,
        "function f()\n    # fatou-ignore discouraged-function\n    exit(1)\nend\n",
        &config,
    );
    assert!(report.diagnostics.is_empty());
}

// --- const-local -----------------------------------------------------------

#[test]
fn const_local_flags_a_function_body() {
    assert_eq!(
        count("const-local", "function k()\n    const z = 1\nend\n"),
        1
    );
}

#[test]
fn const_local_flags_every_local_scope_kind() {
    // Julia rejects `const` in each of these: hard scopes (`let`, function
    // bodies) and soft ones (`for`/`while`/`try`) alike.
    for src in [
        "let\n    const z = 1\nend\n",
        "for i in 1:3\n    const z = 1\nend\n",
        "while false\n    const z = 1\nend\n",
        "try\n    const z = 1\ncatch\nend\n",
        "try\ncatch e\n    const z = 1\nend\n",
        "try\nfinally\n    const z = 1\nend\n",
        "macro m()\n    const z = 1\nend\n",
        "f = () -> (const z = 1)\n",
        "foreach(xs) do x\n    const z = 1\nend\n",
        "[begin\n    const z = 1\nend for i in 1:1]\n",
    ] {
        assert_eq!(
            count("const-local", src),
            1,
            "expected a finding for {src:?}"
        );
    }
}

#[test]
fn const_local_flags_a_short_form_definition_body() {
    // `f() = ...` is a function body just as much as a `function` block.
    assert_eq!(count("const-local", "f() = (const z = 1)\n"), 1);
    assert_eq!(count("const-local", "f(x)::Int = (const z = 1)\n"), 1);
    assert_eq!(count("const-local", "f(x) where {T} = (const z = 1)\n"), 1);
}

#[test]
fn const_local_flags_an_inner_constructor() {
    // The `struct` body is not a local scope, but the constructor inside it is.
    assert_eq!(
        count(
            "const-local",
            "struct S\n    x::Int\n    S() = (const z = 1; new(1))\nend\n"
        ),
        1
    );
}

#[test]
fn const_local_flags_a_nested_scope_inside_a_module() {
    // The enclosing `module` is global, but the function inside it is not.
    assert_eq!(
        count(
            "const-local",
            "module M\n    function g()\n        const z = 1\n    end\nend\n"
        ),
        1
    );
}

#[test]
fn const_local_flags_a_default_argument() {
    // A default value is evaluated inside the function's own scope.
    assert_eq!(
        count("const-local", "function f(x = (const z = 1))\n    x\nend\n"),
        1
    );
}

#[test]
fn const_local_flags_a_comprehension_filter() {
    assert_eq!(
        count("const-local", "[x for x in 1:1 if (const z = 1; true)]\n"),
        1
    );
}

#[test]
fn const_local_points_at_the_const_statement() {
    let src = "function k()\n    const z = 1\nend\n";
    let config = LintConfig {
        select: Some(vec!["const-local".to_string()]),
        ..Default::default()
    };
    let report = check_source(None, src, &config);
    assert_eq!(report.diagnostics.len(), 1);
    // The statement, not the whole function.
    assert_eq!(&src[report.diagnostics[0].range], "const z = 1");
    assert_eq!(report.diagnostics[0].severity, Severity::Error);
}

#[test]
fn const_local_ignores_global_scopes() {
    for src in [
        "const z = 1\n",
        "begin\n    const z = 1\nend\n",
        "if true\n    const z = 1\nend\n",
        "module M\n    const z = 1\nend\n",
        "baremodule B\n    const z = 1\nend\n",
    ] {
        assert_eq!(
            count("const-local", src),
            0,
            "unexpected finding for {src:?}"
        );
    }
}

#[test]
fn const_local_ignores_a_const_struct_field() {
    // `const` fields of a mutable struct are legal since Julia 1.8.
    assert_eq!(
        count("const-local", "mutable struct S\n    const x::Int\nend\n"),
        0
    );
    assert_eq!(
        count(
            "const-local",
            "module M\n    mutable struct S\n        const x::Int\n    end\nend\n"
        ),
        0
    );
}

#[test]
fn const_local_ignores_positions_that_evaluate_in_the_enclosing_scope() {
    // The iterator spec, the `while` condition, a `let`'s first binding and a
    // `do`-call's call part are all evaluated outside the scope they open.
    for src in [
        "for i in (const z = 1; 1:3)\nend\n",
        "while (const z = 1; false)\nend\n",
        "let x = (const z = 1; 2)\n    x\nend\n",
        "foreach((const z = 1; xs)) do x\n    x\nend\n",
        "[i for i in (const z = 1; 1:3)]\n",
    ] {
        assert_eq!(
            count("const-local", src),
            0,
            "unexpected finding for {src:?}"
        );
    }
}

#[test]
fn const_local_ignores_quoted_code() {
    // Quoted code is data; it is never lowered where it is written.
    for src in [
        "quote\n    const z = 1\nend\n",
        "function f()\n    :(const z = 1)\nend\n",
        ":(function f()\n    const z = 1\nend)\n",
    ] {
        assert_eq!(
            count("const-local", src),
            0,
            "unexpected finding for {src:?}"
        );
    }
}

#[test]
fn const_local_ignores_macro_arguments() {
    // A macro may rewrite what it is handed, so the code as written may never
    // be lowered — the same exemption `break-outside-loop` makes.
    assert_eq!(
        count("const-local", "@eval function f()\n    const z = 1\nend\n"),
        0
    );
    assert_eq!(
        count("const-local", "@foo xs do x\n    const z = 1\nend\n"),
        0
    );
}

#[test]
fn const_local_ignores_a_global_const_declaration() {
    // `global const` is legal in a soft local scope, and inside a function it
    // is a different error ("`global const` declaration not allowed inside
    // function") — not this rule's finding either way.
    for src in [
        "let\n    global const z = 1\nend\n",
        "for i in 1:3\n    global const z = 1\nend\n",
        "function f()\n    global const z = 1\nend\n",
        "function f()\n    const global z = 1\nend\n",
    ] {
        assert_eq!(
            count("const-local", src),
            0,
            "unexpected finding for {src:?}"
        );
    }
}

#[test]
fn const_local_ignores_a_local_const_declaration() {
    // `local const z = 1` is rejected everywhere, top level included, with a
    // different message ("expected assignment after \"const\"").
    assert_eq!(
        count("const-local", "function f()\n    local const z = 1\nend\n"),
        0
    );
}

#[test]
fn const_local_ignores_an_ordinary_assignment_body() {
    // `x = (...)` is not a definition, so its right-hand side is not a body.
    assert_eq!(count("const-local", "x = (const z = 1)\n"), 0);
}

#[test]
fn const_local_honors_suppression() {
    let config = LintConfig {
        select: Some(vec!["const-local".to_string()]),
        ..Default::default()
    };
    let report = check_source(
        None,
        "function k()\n    # fatou-ignore const-local\n    const z = 1\nend\n",
        &config,
    );
    assert!(report.diagnostics.is_empty());
}

// --- global-const-in-function ----------------------------------------------

#[test]
fn global_const_in_function_flags_a_function_body() {
    assert_eq!(
        count(
            "global-const-in-function",
            "function f()\n    global const x = 1\nend\n"
        ),
        1
    );
}

#[test]
fn global_const_in_function_flags_both_modifier_orders() {
    // `global const x = 1` nests the `const` under the modifier; `const global
    // x = 1` the other way round. Julia rejects both the same way.
    assert_eq!(
        count(
            "global-const-in-function",
            "function f()\n    const global x = 1\nend\n"
        ),
        1
    );
}

#[test]
fn global_const_in_function_flags_every_function_like_body() {
    for src in [
        "macro m()\n    global const x = 1\nend\n",
        "f = () -> (global const x = 1)\n",
        "foreach(xs) do y\n    global const x = 1\nend\n",
        "[(global const x = 1) for i in 1:1]\n",
        "((global const x = 1) for i in 1:1)\n",
        "Int[(global const x = 1) for i in 1:1]\n",
        "[i for i in 1:1 if (global const x = 1; true)]\n",
    ] {
        assert_eq!(
            count("global-const-in-function", src),
            1,
            "expected a finding for {src:?}"
        );
    }
}

#[test]
fn global_const_in_function_flags_a_short_form_definition_body() {
    assert_eq!(
        count("global-const-in-function", "f() = (global const x = 1)\n"),
        1
    );
    assert_eq!(
        count(
            "global-const-in-function",
            "f(x)::Int = (global const y = 1)\n"
        ),
        1
    );
}

#[test]
fn global_const_in_function_flags_a_default_argument() {
    // A default value is evaluated inside the function's own scope.
    assert_eq!(
        count(
            "global-const-in-function",
            "function f(x = (global const y = 1))\n    x\nend\n"
        ),
        1
    );
}

#[test]
fn global_const_in_function_flags_a_soft_scope_nested_in_a_function() {
    // A `let`/`for`/`while`/`try` opens no *function*, so the enclosing
    // function still owns the declaration.
    for src in [
        "function f()\n    let\n        global const x = 1\n    end\nend\n",
        "function f()\n    for i in 1:3\n        global const x = 1\n    end\nend\n",
        "function f()\n    while false\n        global const x = 1\n    end\nend\n",
        "function f()\n    try\n        global const x = 1\n    catch\n    end\nend\n",
        "function f()\n    if true\n        global const x = 1\n    end\nend\n",
        "function f()\n    begin\n        global const x = 1\n    end\nend\n",
    ] {
        assert_eq!(
            count("global-const-in-function", src),
            1,
            "expected a finding for {src:?}"
        );
    }
}

#[test]
fn global_const_in_function_flags_a_function_inside_a_module() {
    assert_eq!(
        count(
            "global-const-in-function",
            "module M\n    function g()\n        global const x = 1\n    end\nend\n"
        ),
        1
    );
}

#[test]
fn global_const_in_function_flags_an_inner_constructor() {
    assert_eq!(
        count(
            "global-const-in-function",
            "struct S\n    x::Int\n    S() = (global const y = 1; new(1))\nend\n"
        ),
        1
    );
}

#[test]
fn global_const_in_function_points_at_the_whole_declaration() {
    let src = "function f()\n    global const x = 1\nend\n";
    let config = LintConfig {
        select: Some(vec!["global-const-in-function".to_string()]),
        ..Default::default()
    };
    let report = check_source(None, src, &config);
    assert_eq!(report.diagnostics.len(), 1);
    // The `global` modifier is part of the offending construct.
    assert_eq!(&src[report.diagnostics[0].range], "global const x = 1");
    assert_eq!(report.diagnostics[0].severity, Severity::Error);
}

#[test]
fn global_const_in_function_ignores_soft_local_scopes() {
    // Outside a function, `global const` is legal — including in each soft
    // local scope.
    for src in [
        "global const x = 1\n",
        "const global x = 1\n",
        "begin\n    global const x = 1\nend\n",
        "if true\n    global const x = 1\nend\n",
        "let\n    global const x = 1\nend\n",
        "for i in 1:3\n    global const x = 1\nend\n",
        "while false\n    global const x = 1\nend\n",
        "try\n    global const x = 1\ncatch\nend\n",
        "module M\n    global const x = 1\nend\n",
        "struct S\n    x::Int\nend\n",
    ] {
        assert_eq!(
            count("global-const-in-function", src),
            0,
            "unexpected finding for {src:?}"
        );
    }
}

#[test]
fn global_const_in_function_ignores_positions_that_evaluate_outside_the_closure() {
    // A comprehension's iterator spec, a `while` condition, a `for` iterator
    // and a `do`-call's call part are evaluated in the enclosing scope.
    for src in [
        "[i for i in (global const x = 1; 1:3)]\n",
        "(i for i in (global const x = 1; 1:3))\n",
        "while (global const x = 1; false)\nend\n",
        "for i in (global const x = 1; 1:3)\nend\n",
        "foreach((global const x = 1; xs)) do y\n    y\nend\n",
        "let x = (global const y = 1; 2)\n    x\nend\n",
    ] {
        assert_eq!(
            count("global-const-in-function", src),
            0,
            "unexpected finding for {src:?}"
        );
    }
}

#[test]
fn global_const_in_function_ignores_a_plain_const() {
    // A `const` with no `global` modifier is `const-local`'s finding.
    assert_eq!(
        count(
            "global-const-in-function",
            "function f()\n    const x = 1\nend\n"
        ),
        0
    );
    assert_eq!(
        count(
            "global-const-in-function",
            "function f()\n    global x = 1\nend\n"
        ),
        0
    );
}

#[test]
fn global_const_in_function_ignores_a_local_const() {
    // `local const` is `local-const`'s finding, with a different Julia error.
    assert_eq!(
        count(
            "global-const-in-function",
            "function f()\n    local const x = 1\nend\n"
        ),
        0
    );
}

#[test]
fn global_const_in_function_ignores_quoted_code() {
    for src in [
        "function f()\n    :(global const x = 1)\nend\n",
        "function f()\n    quote\n        global const x = 1\n    end\nend\n",
        ":(function f()\n    global const x = 1\nend)\n",
    ] {
        assert_eq!(
            count("global-const-in-function", src),
            0,
            "unexpected finding for {src:?}"
        );
    }
}

#[test]
fn global_const_in_function_ignores_macro_arguments() {
    assert_eq!(
        count(
            "global-const-in-function",
            "@eval function f()\n    global const x = 1\nend\n"
        ),
        0
    );
    assert_eq!(
        count(
            "global-const-in-function",
            "function f()\n    @eval global const x = 1\nend\n"
        ),
        0
    );
}

#[test]
fn global_const_in_function_honors_suppression() {
    let config = LintConfig {
        select: Some(vec!["global-const-in-function".to_string()]),
        ..Default::default()
    };
    let report = check_source(
        None,
        "function f()\n    # fatou-ignore global-const-in-function\n    global const x = 1\nend\n",
        &config,
    );
    assert!(report.diagnostics.is_empty());
}

// --- local-const -----------------------------------------------------------

#[test]
fn local_const_flags_a_local_const_declaration() {
    assert_eq!(count("local-const", "local const z = 1\n"), 1);
}

#[test]
fn local_const_flags_both_modifier_orders() {
    assert_eq!(count("local-const", "const local z = 1\n"), 1);
}

#[test]
fn local_const_flags_every_scope_including_the_top_level() {
    // Unlike `const-local`, this one needs no scope test: Julia rejects it
    // everywhere.
    for src in [
        "local const z = 1\n",
        "function f()\n    local const z = 1\nend\n",
        "macro m()\n    local const z = 1\nend\n",
        "let\n    local const z = 1\nend\n",
        "for i in 1:3\n    local const z = 1\nend\n",
        "while false\n    local const z = 1\nend\n",
        "try\n    local const z = 1\ncatch\nend\n",
        "module M\n    local const z = 1\nend\n",
        "begin\n    local const z = 1\nend\n",
        "f() = (local const z = 1)\n",
        "struct S\n    local const z = 1\nend\n",
    ] {
        assert_eq!(
            count("local-const", src),
            1,
            "expected a finding for {src:?}"
        );
    }
}

#[test]
fn local_const_points_at_the_whole_declaration() {
    let src = "local const z = 1\n";
    let config = LintConfig {
        select: Some(vec!["local-const".to_string()]),
        ..Default::default()
    };
    let report = check_source(None, src, &config);
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(&src[report.diagnostics[0].range], "local const z = 1");
    assert_eq!(report.diagnostics[0].severity, Severity::Error);
}

#[test]
fn local_const_ignores_a_plain_or_global_const() {
    for src in [
        "const z = 1\n",
        "global const z = 1\n",
        "const global z = 1\n",
        "function f()\n    const z = 1\nend\n",
        "function f()\n    local z = 1\n    z\nend\n",
        "mutable struct S\n    const x::Int\nend\n",
    ] {
        assert_eq!(
            count("local-const", src),
            0,
            "unexpected finding for {src:?}"
        );
    }
}

#[test]
fn local_const_ignores_quoted_code() {
    for src in [
        "quote\n    local const z = 1\nend\n",
        ":(local const z = 1)\n",
        "function f()\n    :(local const z = 1)\nend\n",
    ] {
        assert_eq!(
            count("local-const", src),
            0,
            "unexpected finding for {src:?}"
        );
    }
}

#[test]
fn local_const_ignores_macro_arguments() {
    assert_eq!(count("local-const", "@eval local const z = 1\n"), 0);
}

#[test]
fn local_const_honors_suppression() {
    let config = LintConfig {
        select: Some(vec!["local-const".to_string()]),
        ..Default::default()
    };
    let report = check_source(
        None,
        "# fatou-ignore local-const\nlocal const z = 1\n",
        &config,
    );
    assert!(report.diagnostics.is_empty());
}

// --- unreachable-code ------------------------------------------------------

#[test]
fn unreachable_code_flags_tail_after_return() {
    assert_eq!(
        count(
            "unreachable-code",
            "function f()\n    return 1\n    dead()\nend\n"
        ),
        1
    );
}

#[test]
fn unreachable_code_reports_a_dead_run_once() {
    // Three dead statements are one dead block, so one finding at its head.
    assert_eq!(
        findings(
            "unreachable-code",
            "function f()\n    return 1\n    a()\n    b()\n    c()\nend\n"
        )
        .len(),
        1
    );
}

#[test]
fn unreachable_code_flags_tail_after_throw_and_error() {
    for src in [
        "function f()\n    throw(ArgumentError(\"x\"))\n    dead()\nend\n",
        "function f()\n    error(\"boom\")\n    dead()\nend\n",
        "function f()\n    rethrow()\n    dead()\nend\n",
    ] {
        assert_eq!(count("unreachable-code", src), 1, "no finding for {src:?}");
    }
}

#[test]
fn unreachable_code_flags_tail_when_both_if_arms_diverge() {
    assert_eq!(
        count(
            "unreachable-code",
            "function f(a)\n    if a\n        return 1\n    else\n        return 2\n    end\n    dead()\nend\n"
        ),
        1
    );
}

#[test]
fn unreachable_code_flags_tail_after_while_true_without_break() {
    assert_eq!(
        count(
            "unreachable-code",
            "function f()\n    while true\n        work()\n    end\n    dead()\nend\n"
        ),
        1
    );
}

#[test]
fn unreachable_code_flags_top_level_tail() {
    assert_eq!(count("unreachable-code", "error(\"boom\")\ndead()\n"), 1);
}

#[test]
fn unreachable_code_ignores_a_shadowed_terminator_name() {
    // The CFG matches `throw`/`error`/`rethrow` by name; a local shadow means
    // the divergence is unconfirmed, so the region stays silent.
    for src in [
        "function f(throw)\n    throw(1)\n    tail()\nend\n",
        "function f()\n    error = identity\n    error(\"x\")\n    tail()\nend\n",
    ] {
        assert_eq!(
            count("unreachable-code", src),
            0,
            "unexpected finding for {src:?}"
        );
    }
}

#[test]
fn unreachable_code_ignores_live_tails() {
    for src in [
        // A `for` may run zero times.
        "function f(xs)\n    for x in xs\n        return x\n    end\n    alive()\nend\n",
        // An `if` with no `else` falls through.
        "function f(a)\n    if a\n        return 1\n    end\n    alive()\nend\n",
        // A `catch` runs when the `try` body throws.
        "function f()\n    try\n        return 1\n    catch\n        alive()\n    end\nend\n",
        // `break` exits the infinite loop.
        "function f()\n    while true\n        break\n    end\n    alive()\nend\n",
        // A short-circuit `return` is conditional.
        "function f(a)\n    a && return 1\n    alive()\nend\n",
        // Nothing follows the divergence at all.
        "function f()\n    return 1\nend\n",
    ] {
        assert_eq!(
            count("unreachable-code", src),
            0,
            "unexpected finding for {src:?}"
        );
    }
}

#[test]
fn unreachable_code_ignores_a_label_a_goto_reaches() {
    assert_eq!(
        count(
            "unreachable-code",
            "function f(a)\n    a && @goto done\n    return 1\n    @label done\n    alive()\nend\n"
        ),
        0
    );
}

#[test]
fn unreachable_code_honors_suppression() {
    let config = LintConfig {
        select: Some(vec!["unreachable-code".to_string()]),
        ..Default::default()
    };
    let report = check_source(
        None,
        "function f()\n    return 1\n    # fatou-ignore unreachable-code\n    dead()\nend\n",
        &config,
    );
    assert!(report.diagnostics.is_empty());
}

// --- duplicate-method ------------------------------------------------------

#[test]
fn duplicate_method_flags_an_identical_signature() {
    assert_eq!(
        count("duplicate-method", "f(x::Int) = 1\nf(x::Int) = 2\n"),
        1
    );
}

#[test]
fn duplicate_method_flags_the_later_definitions_only() {
    // Three identical signatures: the first is the one being overwritten, so
    // the second and third are reported.
    let src = "f(x::Int) = 1\nf(x::Int) = 2\nf(x::Int) = 3\n";
    assert_eq!(count("duplicate-method", src), 2);
}

#[test]
fn duplicate_method_ignores_argument_names() {
    // Dispatch is on types, not names, so these are the same method.
    assert_eq!(
        count("duplicate-method", "f(x::Int) = 1\nf(y::Int) = 2\n"),
        1
    );
}

#[test]
fn duplicate_method_flags_the_long_and_short_forms_alike() {
    assert_eq!(
        count(
            "duplicate-method",
            "function f(x::Int)\n    1\nend\nf(x::Int) = 2\n"
        ),
        1
    );
}

#[test]
fn duplicate_method_flags_untyped_arguments() {
    // An unannotated argument is `Any`, so both methods are `f(::Any)`.
    assert_eq!(count("duplicate-method", "f(x) = 1\nf(y) = 2\n"), 1);
}

#[test]
fn duplicate_method_ignores_keyword_arguments() {
    // Keyword arguments take no part in dispatch: the second definition
    // replaces the first, and calling `f(1; a = 1)` then fails.
    assert_eq!(
        count(
            "duplicate-method",
            "f(x::Int; a = 1) = a\nf(x::Int; b = 2) = b\n"
        ),
        1
    );
}

#[test]
fn duplicate_method_ignores_default_values() {
    // `f(x::Int = 0)` defines `f(::Int)` (and `f()`), so it collides with the
    // plain `f(x::Int)` above it.
    assert_eq!(
        count("duplicate-method", "f(x::Int) = 1\nf(x::Int = 0) = 2\n"),
        1
    );
}

#[test]
fn duplicate_method_ignores_the_return_type() {
    assert_eq!(
        count(
            "duplicate-method",
            "f(x::Int)::Int = 1\nf(x::Int)::Float64 = 2.0\n"
        ),
        1
    );
}

#[test]
fn duplicate_method_flags_matching_where_clauses() {
    assert_eq!(
        count(
            "duplicate-method",
            "f(x::T) where {T <: Real} = 1\nf(x::T) where T <: Real = 2\n"
        ),
        1
    );
}

#[test]
fn duplicate_method_flags_qualified_extensions() {
    assert_eq!(
        count(
            "duplicate-method",
            "Base.show(io::IO, x::Int) = 1\nBase.show(io::IO, x::Int) = 2\n"
        ),
        1
    );
}

#[test]
fn duplicate_method_flags_operator_definitions() {
    assert_eq!(
        count(
            "duplicate-method",
            "+(a::Foo, b::Foo) = 1\n+(a::Foo, b::Foo) = 2\n"
        ),
        1
    );
}

#[test]
fn duplicate_method_names_the_function() {
    let msgs = findings("duplicate-method", "f(x::Int) = 1\nf(x::Int) = 2\n");
    assert_eq!(msgs.len(), 1, "{msgs:?}");
    assert!(msgs[0].contains("`f`"), "{msgs:?}");
}

#[test]
fn duplicate_method_ignores_differing_signatures() {
    for src in [
        // Different argument types.
        "f(x::Int) = 1\nf(x::Float64) = 2\n",
        // Different arity.
        "f(x::Int) = 1\nf(x::Int, y::Int) = 2\n",
        // Different `where` bounds.
        "f(x::T) where {T <: Real} = 1\nf(x::T) where {T <: Integer} = 2\n",
        // A bound versus none.
        "f(x::T) where {T} = 1\nf(x::T) where {T <: Real} = 2\n",
        // A vararg versus a single argument.
        "f(x::Int) = 1\nf(x::Int...) = 2\n",
        // An annotated argument versus a bare one.
        "f(x::Int) = 1\nf(x) = 2\n",
        // A qualified extension versus a bare definition of the same name.
        "show(x::Int) = 1\nBase.show(x::Int) = 2\n",
        // Different type applications.
        "f(x::Vector{Int}) = 1\nf(x::Vector{Float64}) = 2\n",
        // Different functions entirely.
        "f(x::Int) = 1\ng(x::Int) = 2\n",
    ] {
        assert_eq!(
            count("duplicate-method", src),
            0,
            "unexpected finding for {src:?}"
        );
    }
}

#[test]
fn duplicate_method_ignores_definitions_in_separate_modules() {
    assert_eq!(
        count(
            "duplicate-method",
            "module A\nf(x::Int) = 1\nend\nmodule B\nf(x::Int) = 2\nend\n"
        ),
        0
    );
}

#[test]
fn duplicate_method_flags_definitions_within_one_module() {
    assert_eq!(
        count(
            "duplicate-method",
            "module A\nf(x::Int) = 1\nf(x::Int) = 2\nend\n"
        ),
        1
    );
}

#[test]
fn duplicate_method_ignores_local_definitions() {
    // Two closures in two different function bodies are separate locals.
    assert_eq!(
        count(
            "duplicate-method",
            "function g()\n    h(x::Int) = 1\n    h\nend\nfunction k()\n    h(x::Int) = 2\n    h\nend\n"
        ),
        0
    );
}

#[test]
fn duplicate_method_ignores_conditional_branches() {
    for src in [
        // `@static` picks exactly one branch at parse time.
        "@static if Sys.iswindows()\n    f(x::Int) = 1\nelse\n    f(x::Int) = 2\nend\n",
        // A plain `if` runs one branch too.
        "if VERSION >= v\"1.9\"\n    f(x::Int) = 1\nelse\n    f(x::Int) = 2\nend\n",
    ] {
        assert_eq!(
            count("duplicate-method", src),
            0,
            "unexpected finding for {src:?}"
        );
    }
}

#[test]
fn duplicate_method_ignores_macro_wrapped_definitions() {
    // A macro may reshape the signature it is handed, so the written form is
    // not evidence of what gets defined.
    assert_eq!(
        count(
            "duplicate-method",
            "@inline f(x::Int) = 1\n@inline f(x::Int) = 2\n"
        ),
        0
    );
}

#[test]
fn duplicate_method_ignores_bodyless_declarations() {
    assert_eq!(
        count("duplicate-method", "function f end\nfunction f end\n"),
        0
    );
}

#[test]
fn duplicate_method_honors_suppression() {
    let config = LintConfig {
        select: Some(vec!["duplicate-method".to_string()]),
        ..Default::default()
    };
    let report = check_source(
        None,
        "f(x::Int) = 1\n# fatou-ignore duplicate-method\nf(x::Int) = 2\n",
        &config,
    );
    assert!(report.diagnostics.is_empty());
}

// --- loop-variable-shadow --------------------------------------------------

#[test]
fn loop_variable_shadow_flags_nested_reuse() {
    assert_eq!(
        count(
            "loop-variable-shadow",
            "for i in 1:3\n    for i in 1:2\n        f(i)\n    end\nend\n"
        ),
        1
    );
}

#[test]
fn loop_variable_shadow_flags_reuse_through_soft_scopes() {
    // `while`/`try` scope but do not separate the two loops.
    for src in [
        "for i in 1:3\n    while c\n        for i in 1:2\n            f(i)\n        end\n    end\nend\n",
        "for i in 1:3\n    try\n        for i in 1:2\n            f(i)\n        end\n    catch\n    end\nend\n",
        "for i in 1:3\n    let t = 1\n        for i in 1:2\n            f(i, t)\n        end\n    end\nend\n",
    ] {
        assert_eq!(
            count("loop-variable-shadow", src),
            1,
            "expected a finding for {src:?}"
        );
    }
}

#[test]
fn loop_variable_shadow_flags_repeated_clause_in_one_for() {
    // `for i in a, i in b` chains two loop scopes, the second shadowing the
    // first, so the outer index is unreachable in the body.
    assert_eq!(
        count(
            "loop-variable-shadow",
            "for i in a, i in b\n    f(i)\nend\n"
        ),
        1
    );
}

#[test]
fn loop_variable_shadow_flags_destructured_reuse() {
    assert_eq!(
        count(
            "loop-variable-shadow",
            "for (k, v) in d\n    for (k, w) in e\n        f(k, v, w)\n    end\nend\n"
        ),
        1
    );
}

#[test]
fn loop_variable_shadow_flags_assignment_to_the_loop_variable() {
    assert_eq!(
        count(
            "loop-variable-shadow",
            "for i in 1:3\n    i = 0\n    f(i)\nend\n"
        ),
        1
    );
}

#[test]
fn loop_variable_shadow_flags_augmented_assignment() {
    assert_eq!(
        count(
            "loop-variable-shadow",
            "for i in 1:3\n    i += 1\n    f(i)\nend\n"
        ),
        1
    );
}

#[test]
fn loop_variable_shadow_flags_assignment_to_a_destructured_variable() {
    assert_eq!(
        count(
            "loop-variable-shadow",
            "for (k, v) in d\n    v = 0\n    f(k, v)\nend\n"
        ),
        1
    );
}

#[test]
fn loop_variable_shadow_reports_both_defects_independently() {
    assert_eq!(
        count(
            "loop-variable-shadow",
            "for i in 1:3\n    for i in 1:2\n        i = 0\n        f(i)\n    end\nend\n"
        ),
        2
    );
}

#[test]
fn loop_variable_shadow_ignores_outer_rebinding() {
    // `for outer i` is the sanctioned way to reuse an enclosing loop's index:
    // it assigns to that variable instead of binding a fresh one, so there is
    // no shadowing to report.
    assert_eq!(
        count(
            "loop-variable-shadow",
            "for i in 1:3\n    for outer i in 1:2\n        f(i)\n    end\nend\n"
        ),
        0
    );
    // The `=` spelling of the same spec, and a variable that merely happens to
    // be named `outer`, which does shadow.
    assert_eq!(
        count(
            "loop-variable-shadow",
            "for i in 1:3\n    for outer i = 1:2\n        f(i)\n    end\nend\n"
        ),
        0
    );
    assert_eq!(
        count(
            "loop-variable-shadow",
            "for outer in 1:3\n    for outer in 1:2\n        f(outer)\n    end\nend\n"
        ),
        1
    );
}

#[test]
fn loop_variable_shadow_ignores_distinct_names() {
    assert_eq!(
        count(
            "loop-variable-shadow",
            "for i in 1:3\n    for j in 1:2\n        f(i, j)\n    end\nend\n"
        ),
        0
    );
}

#[test]
fn loop_variable_shadow_ignores_sibling_loops() {
    assert_eq!(
        count(
            "loop-variable-shadow",
            "for i in 1:3\n    f(i)\nend\nfor i in 1:2\n    g(i)\nend\n"
        ),
        0
    );
}

#[test]
fn loop_variable_shadow_ignores_reuse_across_a_function_body() {
    // A nested definition, a closure, and a `do` block are separate units of
    // code; their textual nesting inside the loop is incidental.
    for src in [
        "for i in 1:3\n    function g()\n        for i in 1:2\n            f(i)\n        end\n    end\n    g()\nend\n",
        "for i in 1:3\n    h = () -> begin\n        for i in 1:2\n            f(i)\n        end\n    end\n    h()\nend\n",
        "for i in 1:3\n    map(xs) do x\n        for i in 1:2\n            f(i, x)\n        end\n    end\nend\n",
    ] {
        assert_eq!(
            count("loop-variable-shadow", src),
            0,
            "unexpected finding for {src:?}"
        );
    }
}

#[test]
fn loop_variable_shadow_ignores_non_loop_outer_bindings() {
    // Shadowing a plain local or a parameter is `for`'s normal behavior, not
    // the nested-index bug this rule is about.
    for src in [
        "function f(i)\n    for i in 1:3\n        g(i)\n    end\nend\n",
        "function f()\n    i = 0\n    for i in 1:3\n        g(i)\n    end\nend\n",
        "for i in [x for i in 1:2]\n    f(i)\nend\n",
    ] {
        assert_eq!(
            count("loop-variable-shadow", src),
            0,
            "unexpected finding for {src:?}"
        );
    }
}

#[test]
fn loop_variable_shadow_ignores_comprehension_variables() {
    // A comprehension's clause variable is scoped to the comprehension and
    // reusing a name there is idiomatic; only statement `for`s are in scope.
    assert_eq!(
        count(
            "loop-variable-shadow",
            "for i in 1:3\n    xs = [i for i in 1:2]\n    f(xs)\nend\n"
        ),
        0
    );
}

#[test]
fn loop_variable_shadow_ignores_reads_and_other_writes() {
    for src in [
        "for i in 1:3\n    j = i\n    f(j)\nend\n",
        "for i in 1:3\n    xs[i] = 0\nend\n",
        "for i in 1:3\n    f(i = 1)\nend\n",
    ] {
        assert_eq!(
            count("loop-variable-shadow", src),
            0,
            "unexpected finding for {src:?}"
        );
    }
}

#[test]
fn loop_variable_shadow_honors_suppression() {
    let config = LintConfig {
        select: Some(vec!["loop-variable-shadow".to_string()]),
        ..Default::default()
    };
    let report = check_source(
        None,
        "for i in 1:3\n    # fatou-ignore loop-variable-shadow\n    for i in 1:2\n        f(i)\n    end\nend\n",
        &config,
    );
    assert!(report.diagnostics.is_empty());
}

// --- typeof-comparison -----------------------------------------------------

/// The single diagnostic `typeof-comparison` reports for `src`, or `None`.
fn typeof_diag(src: &str) -> Option<fatou::linter::Diagnostic> {
    let config = LintConfig {
        select: Some(vec!["typeof-comparison".to_string()]),
        ..Default::default()
    };
    check_source(None, src, &config)
        .diagnostics
        .into_iter()
        .find(|d| d.rule == "typeof-comparison")
}

#[test]
fn typeof_comparison_flags_both_operators_and_both_orders() {
    for src in [
        "typeof(x) == Int\n",
        "typeof(x) != Int\n",
        "Int == typeof(x)\n",
        "Int != typeof(x)\n",
    ] {
        assert_eq!(count("typeof-comparison", src), 1, "no finding for {src:?}");
    }
}

#[test]
fn typeof_comparison_spans_the_whole_comparison() {
    let src = "if typeof(x) == Int\n    1\nend\n";
    let diag = typeof_diag(src).expect("expected a finding");
    assert_eq!(&src[diag.range], "typeof(x) == Int");
}

#[test]
fn typeof_comparison_ignores_the_deliberate_exact_tests() {
    for src in [
        // `===`/`!==` are the spelling for exact-type identity.
        "typeof(x) === Int\n",
        "typeof(x) !== Int\n",
        // Broadcast comparison over a container is a different question.
        "typeof(x) .== Int\n",
        // Comparing two dynamic types has no `isa` spelling at all.
        "typeof(a) == typeof(b)\n",
        // A chain folds into a `COMPARISON_EXPR`, not a `BINARY_EXPR`.
        "a < typeof(x) == Int\n",
        // Already idiomatic.
        "x isa Int\n",
        // Not a comparison against a `typeof` call.
        "sizeof(x) == 8\n",
    ] {
        assert_eq!(
            count("typeof-comparison", src),
            0,
            "unexpected finding for {src:?}"
        );
    }
}

#[test]
fn typeof_comparison_ignores_an_unconfirmed_typeof() {
    for src in [
        // A local shadow: the callee is not Base's `typeof`.
        "function f(typeof)\n    typeof(x) == Int\nend\n",
        // A qualified callee spells a different name.
        "Base.typeof(x) == Int\n",
        // A file whose `using` cannot be resolved answers nothing about names.
        "using NotAPackageAnyoneHas\ntypeof(x) == Int\n",
    ] {
        assert_eq!(
            count("typeof-comparison", src),
            0,
            "unexpected finding for {src:?}"
        );
    }
}

#[test]
fn typeof_comparison_ignores_a_call_that_is_not_base_arity() {
    for src in [
        "typeof(xs...) == Int\n",
        "typeof(x; y = 1) == Int\n",
        "typeof() == Int\n",
    ] {
        assert_eq!(
            count("typeof-comparison", src),
            0,
            "unexpected finding for {src:?}"
        );
    }
}

#[test]
fn typeof_comparison_offers_an_unsafe_isa_rewrite() {
    let diag = typeof_diag("typeof(x) == Int\n").expect("expected a finding");
    assert_eq!(diag.fixes.len(), 1);
    assert_eq!(diag.fixes[0].content, "x isa Int");
    assert_eq!(diag.fixes[0].applicability, Applicability::Unsafe);

    let diag = typeof_diag("typeof(x) != Int\n").expect("expected a finding");
    assert_eq!(diag.fixes[0].content, "!(x isa Int)");
}

#[test]
fn typeof_comparison_withholds_the_fix_on_a_loose_binding_operand() {
    // Splicing `a ? b : c` in as `isa`'s left operand would rebind the `:`
    // arm, so the finding ships without a fix.
    let diag = typeof_diag("typeof(a ? b : c) == Int\n").expect("expected a finding");
    assert!(diag.fixes.is_empty());
    // A macro call slurps the rest of the line.
    let diag = typeof_diag("typeof(@f x) == Int\n").expect("expected a finding");
    assert!(diag.fixes.is_empty());
}

#[test]
fn typeof_comparison_withholds_the_fix_around_a_comment() {
    let diag = typeof_diag("typeof(x) == #= t =# Int\n").expect("expected a finding");
    assert!(diag.fixes.is_empty());
}

#[test]
fn typeof_comparison_fixes_a_field_access_and_a_nested_call() {
    let diag = typeof_diag("typeof(a.b) == Int\n").expect("expected a finding");
    assert_eq!(diag.fixes[0].content, "a.b isa Int");
    let diag = typeof_diag("typeof(f(y)) == Union{Int, Float64}\n").expect("expected a finding");
    assert_eq!(diag.fixes[0].content, "f(y) isa Union{Int, Float64}");
}

#[test]
fn typeof_comparison_honors_suppression() {
    let config = LintConfig {
        select: Some(vec!["typeof-comparison".to_string()]),
        ..Default::default()
    };
    let report = check_source(
        None,
        "# fatou-ignore typeof-comparison\ntypeof(x) == Int\n",
        &config,
    );
    assert!(report.diagnostics.is_empty());
}

// --- length-zero -----------------------------------------------------------

/// The single diagnostic `length-zero` reports for `src`, or `None`.
fn length_zero_diag(src: &str) -> Option<fatou::linter::Diagnostic> {
    let config = LintConfig {
        select: Some(vec!["length-zero".to_string()]),
        ..Default::default()
    };
    check_source(None, src, &config)
        .diagnostics
        .into_iter()
        .find(|d| d.rule == "length-zero")
}

#[test]
fn length_zero_flags_every_emptiness_spelling() {
    for src in [
        "length(x) == 0\n",
        "length(x) <= 0\n",
        "length(x) < 1\n",
        "0 == length(x)\n",
        "0 >= length(x)\n",
        "1 > length(x)\n",
    ] {
        assert_eq!(count("length-zero", src), 1, "no finding for {src:?}");
        let diag = length_zero_diag(src).expect("expected a finding");
        assert!(
            diag.message.body.contains("`isempty(x)`") && !diag.message.body.contains("!isempty"),
            "wrong direction for {src:?}: {}",
            diag.message.body
        );
    }
}

#[test]
fn length_zero_flags_every_nonemptiness_spelling() {
    for src in [
        "length(x) != 0\n",
        "length(x) > 0\n",
        "length(x) >= 1\n",
        "0 != length(x)\n",
        "0 < length(x)\n",
        "1 <= length(x)\n",
    ] {
        assert_eq!(count("length-zero", src), 1, "no finding for {src:?}");
        let diag = length_zero_diag(src).expect("expected a finding");
        assert!(
            diag.message.body.contains("`!isempty(x)`"),
            "wrong direction for {src:?}: {}",
            diag.message.body
        );
    }
}

#[test]
fn length_zero_spans_the_whole_comparison() {
    let src = "if length(xs) == 0\n    1\nend\n";
    let diag = length_zero_diag(src).expect("expected a finding");
    assert_eq!(&src[diag.range], "length(xs) == 0");
}

#[test]
fn length_zero_ignores_comparisons_that_are_not_emptiness_tests() {
    for src in [
        // A different question entirely.
        "length(x) == 1\n",
        "length(x) > 1\n",
        "length(x) < 2\n",
        // Constant by construction, and `constant-condition`'s territory at
        // most -- never an emptiness test.
        "length(x) >= 0\n",
        "length(x) < 0\n",
        // Broadcast comparison over a container is a different question.
        "length(x) .== 0\n",
        // `===` is the deliberate identity spelling.
        "length(x) === 0\n",
        // A chain folds into a `COMPARISON_EXPR`, not a `BINARY_EXPR`.
        "0 < length(x) < 3\n",
        // Not an integer-literal bound.
        "length(x) == 0.0\n",
        "length(x) == n\n",
        // Already idiomatic.
        "isempty(x)\n",
        // A different builtin.
        "size(x) == 0\n",
    ] {
        assert_eq!(
            count("length-zero", src),
            0,
            "unexpected finding for {src:?}"
        );
    }
}

#[test]
fn length_zero_ignores_an_unconfirmed_length() {
    for src in [
        // A local shadow: the callee is not Base's `length`.
        "function f(length)\n    length(x) == 0\nend\n",
        // A qualified callee spells a different name.
        "Base.length(x) == 0\n",
        // A file whose `using` cannot be resolved answers nothing about names.
        "using NotAPackageAnyoneHas\nlength(x) == 0\n",
    ] {
        assert_eq!(
            count("length-zero", src),
            0,
            "unexpected finding for {src:?}"
        );
    }
}

#[test]
fn length_zero_ignores_a_call_that_is_not_base_arity() {
    for src in [
        "length(x, 2) == 0\n",
        "length(xs...) == 0\n",
        "length() == 0\n",
        "length(x; dims = 1) == 0\n",
    ] {
        assert_eq!(
            count("length-zero", src),
            0,
            "unexpected finding for {src:?}"
        );
    }
}

#[test]
fn length_zero_offers_a_safe_isempty_rewrite() {
    let diag = length_zero_diag("length(x) == 0\n").expect("expected a finding");
    assert_eq!(diag.fixes.len(), 1);
    assert_eq!(diag.fixes[0].content, "isempty(x)");
    assert_eq!(diag.fixes[0].applicability, Applicability::Safe);

    let diag = length_zero_diag("0 < length(f(y).items)\n").expect("expected a finding");
    assert_eq!(diag.fixes[0].content, "!isempty(f(y).items)");
}

#[test]
fn length_zero_withholds_the_fix_when_isempty_is_shadowed() {
    // Splicing in `isempty` would call the file's own definition, not Base's.
    let diag =
        length_zero_diag("isempty(v) = false\nlength(x) == 0\n").expect("expected a finding");
    assert!(diag.fixes.is_empty());
}

#[test]
fn length_zero_withholds_the_fix_around_a_comment() {
    let diag = length_zero_diag("length(x) == #= n =# 0\n").expect("expected a finding");
    assert!(diag.fixes.is_empty());
    // A comment in the argument list but outside the argument goes too.
    let diag = length_zero_diag("length(#= c =# x) == 0\n").expect("expected a finding");
    assert!(diag.fixes.is_empty());
    // One *inside* the argument is carried over with it, so the fix stands.
    let diag = length_zero_diag("length(f(#= c =# y)) == 0\n").expect("expected a finding");
    assert_eq!(diag.fixes[0].content, "isempty(f(#= c =# y))");
}

#[test]
fn length_zero_honors_suppression() {
    let config = LintConfig {
        select: Some(vec!["length-zero".to_string()]),
        ..Default::default()
    };
    let report = check_source(
        None,
        "# fatou-ignore length-zero\nlength(x) == 0\n",
        &config,
    );
    assert!(report.diagnostics.is_empty());
}

// --- comparison-negation ---------------------------------------------------

/// The single diagnostic `comparison-negation` reports for `src`, or `None`.
fn comparison_negation_diag(src: &str) -> Option<fatou::linter::Diagnostic> {
    let config = LintConfig {
        select: Some(vec!["comparison-negation".to_string()]),
        ..Default::default()
    };
    check_source(None, src, &config)
        .diagnostics
        .into_iter()
        .find(|d| d.rule == "comparison-negation")
}

#[test]
fn comparison_negation_flags_every_equality_spelling() {
    for (src, rewrite) in [
        ("!(a == b)\n", "a != b"),
        ("!(a != b)\n", "a == b"),
        ("!(a === b)\n", "a !== b"),
        ("!(a !== b)\n", "a === b"),
        ("!(a ≠ b)\n", "a == b"),
        ("!(a ≡ b)\n", "a ≢ b"),
        ("!(a ≢ b)\n", "a ≡ b"),
    ] {
        assert_eq!(
            count("comparison-negation", src),
            1,
            "no finding for {src:?}"
        );
        let diag = comparison_negation_diag(src).expect("expected a finding");
        assert_eq!(diag.fixes[0].content, rewrite, "wrong rewrite for {src:?}");
    }
}

#[test]
fn comparison_negation_spans_the_whole_negation() {
    let src = "if !(x.status == :ok)\n    1\nend\n";
    let diag = comparison_negation_diag(src).expect("expected a finding");
    assert_eq!(&src[diag.range], "!(x.status == :ok)");
}

#[test]
fn comparison_negation_ignores_orderings_and_other_operators() {
    for src in [
        // Not equivalent for a partial order: `!(NaN < 1)` is `true` while
        // `NaN >= 1` is `false`.
        "!(x < y)\n",
        "!(x <= y)\n",
        "!(x > y)\n",
        "!(x >= y)\n",
        // No ASCII negated spelling, and injecting `∉` is not this rule's call.
        "!(a in b)\n",
        "!(a ∈ b)\n",
        // Not a comparison at all.
        "!(a + b)\n",
        "!(a && b)\n",
        "!(a = b)\n",
        "!(f(x))\n",
        "!x\n",
        // Already direct.
        "a != b\n",
    ] {
        assert_eq!(
            count("comparison-negation", src),
            0,
            "unexpected finding for {src:?}"
        );
    }
}

#[test]
fn comparison_negation_ignores_broadcast_forms() {
    for src in [
        // An elementwise comparison is a container of values, not a test.
        "!(a .== b)\n",
        "!(a .!= b)\n",
        "!(a .=== b)\n",
        // `.!` negates elementwise; the result is a container either way.
        ".!(a == b)\n",
        ".!(a .== b)\n",
    ] {
        assert_eq!(
            count("comparison-negation", src),
            0,
            "unexpected finding for {src:?}"
        );
    }
}

#[test]
fn comparison_negation_ignores_a_comparison_chain() {
    // A chain folds into a `COMPARISON_EXPR`, and `!(a == b == c)` has no
    // two-operand rewrite.
    assert_eq!(count("comparison-negation", "!(a == b == c)\n"), 0);
}

#[test]
fn comparison_negation_fixes_in_positions_that_cannot_rebind() {
    for (src, rewrite) in [
        ("!(a == b)\n", "a != b"),
        ("x = !(a == b)\n", "a != b"),
        ("if !(a == b)\n    1\nend\n", "a != b"),
        ("while !(a == b)\n    1\nend\n", "a != b"),
        ("f(!(a == b))\n", "a != b"),
        ("f(k = !(a == b))\n", "a != b"),
        ("[!(a == b), c]\n", "a != b"),
        ("(!(a == b))\n", "a != b"),
        ("!(a == b) && c\n", "a != b"),
        ("c || !(a == b)\n", "a != b"),
        ("c ? !(a == b) : y\n", "a != b"),
        ("function f()\n    return !(a == b)\nend\n", "a != b"),
        ("x -> !(a == b)\n", "a != b"),
        ("@assert !(a == b)\n", "a != b"),
        ("[x for x in v if !(a == b)]\n", "a != b"),
    ] {
        let diag = comparison_negation_diag(src).unwrap_or_else(|| panic!("no finding: {src:?}"));
        assert_eq!(diag.fixes.len(), 1, "no fix for {src:?}");
        assert_eq!(diag.fixes[0].content, rewrite, "wrong rewrite for {src:?}");
        assert_eq!(diag.fixes[0].applicability, Applicability::Safe);
    }
}

#[test]
fn comparison_negation_withholds_the_fix_where_the_comparison_would_rebind() {
    for src in [
        // A tighter operator would capture an operand: `x + a != b` is
        // `(x + a) != b`.
        "x + !(a == b)\n",
        "!(a == b) * y\n",
        "x .+ !(a == b)\n",
        // `!a != b` is `(!a) != b`.
        "!!(a == b)\n",
        // Splicing a comparison beside another one builds a chain.
        "!(a == b) == c\n",
        // A space-separated matrix row: `[a != b c]` is not two elements.
        "[!(a == b) c]\n",
        // Another macro argument follows, and nothing bounds the comparison.
        "@assert !(a == b) \"msg\"\n",
    ] {
        let diag = comparison_negation_diag(src).unwrap_or_else(|| panic!("no finding: {src:?}"));
        assert!(diag.fixes.is_empty(), "unexpected fix for {src:?}");
    }
}

#[test]
fn comparison_negation_preserves_the_operands_own_text() {
    // Spacing around the operator, and anything between the operands, is the
    // author's and survives byte for byte.
    let diag = comparison_negation_diag("!(a==b)\n").expect("expected a finding");
    assert_eq!(diag.fixes[0].content, "a!=b");
    let diag = comparison_negation_diag("!(f(#= c =# y) == g(z))\n").expect("expected a finding");
    assert_eq!(diag.fixes[0].content, "f(#= c =# y) != g(z)");
    let diag = comparison_negation_diag("!(a #= c =# == b)\n").expect("expected a finding");
    assert_eq!(diag.fixes[0].content, "a #= c =# != b");
}

#[test]
fn comparison_negation_withholds_the_fix_around_a_dropped_comment() {
    // A comment outside the operands sits in the deleted `!(` / `)` and would
    // be lost.
    let diag = comparison_negation_diag("!(#= c =# a == b)\n").expect("expected a finding");
    assert!(diag.fixes.is_empty());
    let diag = comparison_negation_diag("!(a == b #= c =#)\n").expect("expected a finding");
    assert!(diag.fixes.is_empty());
}

#[test]
fn comparison_negation_honors_suppression() {
    let config = LintConfig {
        select: Some(vec!["comparison-negation".to_string()]),
        ..Default::default()
    };
    let report = check_source(
        None,
        "# fatou-ignore comparison-negation\n!(a == b)\n",
        &config,
    );
    assert!(report.diagnostics.is_empty());
}

// --- redundant-boolean -----------------------------------------------------

/// The single diagnostic `redundant-boolean` reports for `src`, or `None`.
fn redundant_boolean_diag(src: &str) -> Option<fatou::linter::Diagnostic> {
    let config = LintConfig {
        select: Some(vec!["redundant-boolean".to_string()]),
        ..Default::default()
    };
    check_source(None, src, &config)
        .diagnostics
        .into_iter()
        .find(|d| d.rule == "redundant-boolean")
}

#[test]
fn redundant_boolean_flags_every_comparison_spelling() {
    for (src, rewrite) in [
        ("x == true\n", "x"),
        ("x != false\n", "x"),
        ("x == false\n", "!x"),
        ("x != true\n", "!x"),
        // Mirrored: `==` and `!=` are symmetric, so the literal may come first.
        ("true == x\n", "x"),
        ("false != x\n", "x"),
        ("false == x\n", "!x"),
        ("true != x\n", "!x"),
    ] {
        assert_eq!(count("redundant-boolean", src), 1, "no finding for {src:?}");
        let diag = redundant_boolean_diag(src).expect("expected a finding");
        assert_eq!(diag.fixes[0].content, rewrite, "wrong rewrite for {src:?}");
        assert_eq!(diag.fixes[0].applicability, Applicability::Unsafe);
    }
}

#[test]
fn redundant_boolean_flags_both_conditional_spellings() {
    for (src, rewrite) in [("c ? true : false\n", "c"), ("c ? false : true\n", "!c")] {
        assert_eq!(count("redundant-boolean", src), 1, "no finding for {src:?}");
        let diag = redundant_boolean_diag(src).expect("expected a finding");
        assert_eq!(diag.fixes[0].content, rewrite, "wrong rewrite for {src:?}");
        assert_eq!(diag.fixes[0].applicability, Applicability::Safe);
    }
}

#[test]
fn redundant_boolean_spans_the_whole_expression() {
    let src = "if x.ready == true\n    1\nend\n";
    let diag = redundant_boolean_diag(src).expect("expected a finding");
    assert_eq!(&src[diag.range], "x.ready == true");

    let src = "y = check(v) ? false : true\n";
    let diag = redundant_boolean_diag(src).expect("expected a finding");
    assert_eq!(&src[diag.range], "check(v) ? false : true");
}

#[test]
fn redundant_boolean_ignores_identity_and_broadcast_comparisons() {
    for src in [
        // `===` is the deliberate identity spelling: it is `false` for every
        // non-`Bool`, which is exactly what its author asked for.
        "x === true\n",
        "x !== false\n",
        // The broadcast forms are containers of values, not tests.
        "x .== true\n",
        "x .!= false\n",
        // Not a comparison to a boolean literal at all.
        "x == 1\n",
        "x == nothing\n",
        "x == \"true\"\n",
        "x < true\n",
        "x && true\n",
    ] {
        assert_eq!(
            count("redundant-boolean", src),
            0,
            "unexpected finding for {src:?}"
        );
    }
}

#[test]
fn redundant_boolean_ignores_a_comparison_between_two_boolean_literals() {
    // Neither operand is the value being tested, so there is nothing to keep.
    for src in ["true == false\n", "true == true\n", "false != true\n"] {
        assert_eq!(
            count("redundant-boolean", src),
            0,
            "unexpected finding for {src:?}"
        );
    }
}

#[test]
fn redundant_boolean_ignores_a_comparison_chain() {
    // A chain folds into a `COMPARISON_EXPR`, and `a == true == b` has no
    // two-operand rewrite.
    assert_eq!(count("redundant-boolean", "a == true == b\n"), 0);
}

#[test]
fn redundant_boolean_ignores_a_conditional_without_two_opposed_literals() {
    for src in [
        "c ? true : x\n",
        "c ? x : false\n",
        // Constant either way: a different rule's question, not an idiom.
        "c ? true : true\n",
        "c ? false : false\n",
        "c ? 1 : 0\n",
        "c ? \"yes\" : \"no\"\n",
    ] {
        assert_eq!(
            count("redundant-boolean", src),
            0,
            "unexpected finding for {src:?}"
        );
    }
}

#[test]
fn redundant_boolean_parenthesizes_an_operand_that_would_rebind_under_negation() {
    for (src, rewrite) in [
        // `!` binds tighter than every infix operator but `.`, so a loose
        // operand needs the parentheses.
        ("a + b == false\n", "!(a + b)"),
        ("a || b ? false : true\n", "!(a || b)"),
        ("-x == false\n", "!(-x)"),
        // These already bind at least as tightly as `!`.
        ("f(x) == false\n", "!f(x)"),
        ("x.ready == false\n", "!x.ready"),
        ("v[i] == false\n", "!v[i]"),
        ("(a + b) == false\n", "!(a + b)"),
    ] {
        let diag = redundant_boolean_diag(src).unwrap_or_else(|| panic!("no finding: {src:?}"));
        assert_eq!(diag.fixes[0].content, rewrite, "wrong rewrite for {src:?}");
    }
}

#[test]
fn redundant_boolean_preserves_the_kept_operands_own_text() {
    // Whatever the surviving operand contains travels with it byte for byte.
    let diag = redundant_boolean_diag("x==true\n").expect("expected a finding");
    assert_eq!(diag.fixes[0].content, "x");
    let diag = redundant_boolean_diag("f(#= c =# y) == true\n").expect("expected a finding");
    assert_eq!(diag.fixes[0].content, "f(#= c =# y)");
}

#[test]
fn redundant_boolean_withholds_the_fix_around_a_dropped_comment() {
    // A comment outside the surviving operand sits in the deleted text and
    // would be lost; the finding still stands.
    for src in [
        "x == #= c =# true\n",
        "x #= c =# == true\n",
        // Julia requires real whitespace around a ternary's `:`, so the only
        // comment slots are around the test and inside the arms.
        "c #= c =# ? true : false\n",
        "c ? #= c =# true : false\n",
    ] {
        let diag = redundant_boolean_diag(src).unwrap_or_else(|| panic!("no finding: {src:?}"));
        assert!(diag.fixes.is_empty(), "unexpected fix for {src:?}");
    }
}

#[test]
fn redundant_boolean_honors_suppression() {
    let config = LintConfig {
        select: Some(vec!["redundant-boolean".to_string()]),
        ..Default::default()
    };
    let report = check_source(
        None,
        "# fatou-ignore redundant-boolean\nx == true\n",
        &config,
    );
    assert!(report.diagnostics.is_empty());
}

// --- unresolved-import -----------------------------------------------------

/// The rule is silent for a file the driver gave no project context: a loose
/// buffer, a script, or a `test/` file resolves its `using` clauses against
/// another environment entirely, so `check_source`'s single-file mode must
/// report nothing however exotic the import. (The rule's own behavior is
/// covered by unit tests in `src/linter/rules/correctness/unresolved_import.rs`,
/// which can attach a declared dependency set.)
#[test]
fn unresolved_import_needs_project_context() {
    assert_eq!(
        count(
            "unresolved-import",
            "using Frobnicate\nimport Whatsit: thing\n",
        ),
        0,
    );
}

// --- kwarg-default-mismatch ------------------------------------------------

#[test]
fn kwarg_default_mismatch_flags_float_default_for_int() {
    assert_eq!(
        count(
            "kwarg-default-mismatch",
            "function g(; y::Int = 1.0)\n    y\nend\n"
        ),
        1
    );
}

#[test]
fn kwarg_default_mismatch_flags_short_form_definition() {
    assert_eq!(
        count("kwarg-default-mismatch", "h(; s::String = 3) = s\n"),
        1
    );
}

#[test]
fn kwarg_default_mismatch_flags_each_mismatched_keyword() {
    assert_eq!(
        count(
            "kwarg-default-mismatch",
            "q(; y::Int = 1.0, z::String = 2) = y\n"
        ),
        2
    );
}

#[test]
fn kwarg_default_mismatch_flags_the_whole_literal_vocabulary() {
    for src in [
        "f(; b::Bool = 1) = b\n",
        "f(; c::Char = \"a\") = c\n",
        "f(; s::Symbol = \"a\") = s\n",
        "f(; s::String = :a) = s\n",
        "f(; y::Int = true) = y\n",
        "f(; y::Float64 = 1.0f0) = y\n",
        "f(; y::Float32 = 1.0) = y\n",
        "f(; y::Float16 = 1.0) = y\n",
        // A sized integer type is a different type from the machine `Int` a
        // decimal literal has, on every platform.
        "f(; y::Int8 = 1) = y\n",
        "f(; y::Int128 = 1) = y\n",
        "f(; y::UInt = 1) = y\n",
        "f(; y::UInt8 = 1) = y\n",
    ] {
        assert_eq!(
            count("kwarg-default-mismatch", src),
            1,
            "expected a finding for {src:?}"
        );
    }
}

#[test]
fn kwarg_default_mismatch_flags_a_signed_literal() {
    // The parser folds the sign into the literal, and `-1.0` is a `Float64`
    // just as `1.0` is.
    assert_eq!(
        count("kwarg-default-mismatch", "f(; y::Int = -1.0) = y\n"),
        1
    );
}

#[test]
fn kwarg_default_mismatch_ignores_a_matching_default() {
    for src in [
        "f(; y::Int = 1) = y\n",
        "f(; y::Int = -1) = y\n",
        "f(; y::Float64 = 1.0) = y\n",
        "f(; y::Float64 = 1e3) = y\n",
        "f(; y::Float32 = 1.0f0) = y\n",
        "f(; s::String = \"a\") = s\n",
        "f(; c::Char = 'a') = c\n",
        "f(; s::Symbol = :a) = s\n",
        "f(; b::Bool = true) = b\n",
        "f(; b::Bool = false) = b\n",
    ] {
        assert_eq!(
            count("kwarg-default-mismatch", src),
            0,
            "unexpected finding for {src:?}"
        );
    }
}

#[test]
fn kwarg_default_mismatch_ignores_an_abstract_or_parametric_annotation() {
    // A default only has to be an instance of the declared type, and an
    // abstract or parametric type is not one the rule reasons about.
    for src in [
        "f(; y::Real = 1.0) = y\n",
        "f(; y::Integer = 1) = y\n",
        "f(; y::Number = 1.0) = y\n",
        "f(; y::Any = 1.0) = y\n",
        "f(; y::AbstractString = \"a\") = y\n",
        "f(; y::Union{Int,Float64} = 1.0) = y\n",
        "f(; y::Vector{Int} = 1.0) = y\n",
        "f(; y::T = 1.0) where {T} = y\n",
    ] {
        assert_eq!(
            count("kwarg-default-mismatch", src),
            0,
            "unexpected finding for {src:?}"
        );
    }
}

#[test]
fn kwarg_default_mismatch_ignores_a_qualified_annotation() {
    // `Core.Int` is Base's `Int`, but the resolution gate the rule opens with
    // confirms a bare name only.
    assert_eq!(
        count("kwarg-default-mismatch", "f(; y::Core.Int = 1.0) = y\n"),
        0
    );
}

#[test]
fn kwarg_default_mismatch_ignores_a_shadowed_type_name() {
    // `Int` here is a local binding, not Base's type.
    assert_eq!(
        count(
            "kwarg-default-mismatch",
            "Int = Float64\nf(; y::Int = 1.0) = y\n"
        ),
        0
    );
}

#[test]
fn kwarg_default_mismatch_ignores_a_non_literal_default() {
    for src in [
        "f(; y::Int = zero(Int)) = y\n",
        "f(; y::Int = n) = y\n",
        "f(; y::Int = 1 + 1) = y\n",
        "f(; y::String = \"a\" * b) = y\n",
    ] {
        assert_eq!(
            count("kwarg-default-mismatch", src),
            0,
            "unexpected finding for {src:?}"
        );
    }
}

#[test]
fn kwarg_default_mismatch_ignores_literals_whose_type_is_not_pinned_down() {
    for src in [
        // A hexadecimal, octal, or binary literal's width — and so its type —
        // depends on how many digits it has.
        "f(; y::Int = 0x01) = y\n",
        "f(; y::Int = 0b1) = y\n",
        "f(; y::Int = 0o7) = y\n",
        // Too big for an `Int64`, so Julia widens it.
        "f(; y::Float64 = 99999999999999999999999) = y\n",
        // `1im` is a `Complex`, not the integer its token suggests.
        "f(; y::Int = 1im) = y\n",
        // A non-standard string literal is whatever its macro returns.
        "f(; y::Int = r\"a\") = y\n",
        // An interpolated string is a `String`, but its parts are not literals.
        "f(; y::Int = \"a$(b)\") = y\n",
        // `Int32` is the machine `Int` on a 32-bit platform.
        "f(; y::Int32 = 1) = y\n",
    ] {
        assert_eq!(
            count("kwarg-default-mismatch", src),
            0,
            "unexpected finding for {src:?}"
        );
    }
}

#[test]
fn kwarg_default_mismatch_ignores_a_positional_optional_argument() {
    // Out of scope: this rule is about the keyword parameters after the `;`.
    assert_eq!(count("kwarg-default-mismatch", "f(y::Int = 1.0) = y\n"), 0);
}

#[test]
fn kwarg_default_mismatch_ignores_an_unannotated_keyword() {
    assert_eq!(count("kwarg-default-mismatch", "f(; y = 1.0) = y\n"), 0);
}

#[test]
fn kwarg_default_mismatch_ignores_a_call_site() {
    // `g(; y::Int = 1.0)` passes an annotated expression as a keyword value; it
    // declares no parameter, so nothing is lowered into a dispatch constraint.
    assert_eq!(count("kwarg-default-mismatch", "g(; y::Int = 1.0)\n"), 0);
}

#[test]
fn kwarg_default_mismatch_ignores_quoted_and_macro_spans() {
    // Quoted code is data, and a macro may rewrite what it is handed.
    assert_eq!(
        count("kwarg-default-mismatch", "ex = :(f(; y::Int = 1.0) = y)\n"),
        0
    );
    assert_eq!(
        count("kwarg-default-mismatch", "@mac f(; y::Int = 1.0) = y\n"),
        0
    );
}

#[test]
fn kwarg_default_mismatch_names_both_types() {
    let found = findings("kwarg-default-mismatch", "f(; y::Int = 1.0) = y\n");
    assert_eq!(found.len(), 1);
    assert!(found[0].contains('y'), "{found:?}");
    assert!(found[0].contains("Int"), "{found:?}");
    assert!(found[0].contains("Float64"), "{found:?}");
}

// --- function-has-no-methods -----------------------------------------------

#[test]
fn function_has_no_methods_flags_a_call_to_a_bare_declaration() {
    let msgs = findings(
        "function-has-no-methods",
        "function process end\n\nprocess(1)\n",
    );
    assert_eq!(msgs.len(), 1, "{msgs:?}");
    assert!(msgs[0].contains("process"), "{msgs:?}");
}

#[test]
fn function_has_no_methods_accepts_a_declaration_with_a_method() {
    // The declaration is a forward reference; the method makes the call fine,
    // whichever order the two are written in.
    assert_eq!(
        count(
            "function-has-no-methods",
            "function process end\nprocess(x) = x\nprocess(1)\n"
        ),
        0
    );
    assert_eq!(
        count(
            "function-has-no-methods",
            "function process end\nfunction process(x)\n    x\nend\nprocess(1)\n"
        ),
        0
    );
}

#[test]
fn function_has_no_methods_sees_a_method_under_a_wrapper_macro() {
    // `@inline f(x) = x` still defines a method: the harvest recurses into a
    // macro call's definition-shaped arguments.
    assert_eq!(
        count(
            "function-has-no-methods",
            "function process end\n@inline process(x) = x\nprocess(1)\n"
        ),
        0
    );
}

#[test]
fn function_has_no_methods_ignores_library_names() {
    assert_eq!(count("function-has-no-methods", "sqrt(2)\nprintln(1)\n"), 0);
}

#[test]
fn function_has_no_methods_exempts_a_declared_api_hook() {
    // An `export`ed or `public` bare declaration is an interface hook a
    // package extension or a downstream package fills in.
    assert_eq!(
        count(
            "function-has-no-methods",
            "module M\nexport process\n\nfunction process end\n\nprocess(1)\nend\n"
        ),
        0
    );
    assert_eq!(
        count(
            "function-has-no-methods",
            "module M\npublic process\n\nfunction process end\n\nprocess(1)\nend\n"
        ),
        0
    );
}

#[test]
fn function_has_no_methods_ignores_a_same_named_type() {
    // `Foo` is a type in one module and a bare declaration in another: the
    // union cannot tell which the call reaches, so it stays quiet.
    assert_eq!(
        count(
            "function-has-no-methods",
            "module A\nstruct Foo end\nend\n\nmodule B\nfunction Foo end\n\nFoo(1)\nend\n"
        ),
        0
    );
}

#[test]
fn function_has_no_methods_is_silent_when_the_file_evals() {
    // `@eval` can define the missing method at runtime.
    assert_eq!(
        count(
            "function-has-no-methods",
            "function process end\nfor T in (Int, Float64)\n    @eval process(x::$T) = x\nend\nprocess(1)\n"
        ),
        0
    );
}

#[test]
fn function_has_no_methods_skips_quoted_and_macro_call_sites() {
    // Quoted code is data, and a macro may rewrite what it is handed.
    assert_eq!(
        count(
            "function-has-no-methods",
            "function process end\nex = :(process(1))\n"
        ),
        0
    );
    assert_eq!(
        count(
            "function-has-no-methods",
            "function process end\n@mac process(1)\n"
        ),
        0
    );
}
