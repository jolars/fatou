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
