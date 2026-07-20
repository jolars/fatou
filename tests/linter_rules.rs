//! Behavioral coverage for the first lint rules: each rule's triggering cases,
//! and the non-triggering cases that guard against false positives.

use fatou::config::LintConfig;
use fatou::linter::{Severity, check_source};

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
