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
        .map(|d| d.message)
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
