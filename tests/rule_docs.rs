//! Living-documentation tests: the lint-rule reference
//! (`docs/src/reference/rules.md`) is rendered from the rule metadata by
//! running the real linter, and pinned by snapshot so the docs cannot drift
//! from behavior. The generator (`examples/docgen.rs`) writes the same
//! `render_reference_page` output to the mdBook source tree.

use fatou::config::LintConfig;
use fatou::linter::{all_rules, check_source_with_target, render_reference_page, render_rule_doc};

/// Pin the rendered reference section for every documented rule. Any change to
/// a rule's diagnostic that alters its section fails here before the docs go
/// stale.
#[test]
fn rule_docs_render() {
    for rule in all_rules() {
        if rule.examples().is_empty() {
            continue;
        }
        insta::assert_snapshot!(rule.id().replace('-', "_"), render_rule_doc(rule.as_ref()));
    }
}

/// The committed reference page must equal what the generator would write, so a
/// metadata change that isn't regenerated fails CI instead of shipping stale
/// docs. Run `cargo run --example docgen` to refresh it.
#[test]
fn reference_page_is_committed() {
    let committed =
        std::fs::read_to_string("docs/src/reference/rules.md").expect("rules.md should exist");
    assert_eq!(
        committed,
        render_reference_page(),
        "docs/src/reference/rules.md is stale; run `cargo run --example docgen`",
    );
}

/// Every shipped rule must carry a description and at least one example, so the
/// generated reference is complete.
#[test]
fn every_rule_is_documented() {
    for rule in all_rules() {
        assert!(
            !rule.description().trim().is_empty(),
            "rule `{}` has no description",
            rule.id(),
        );
        assert!(
            !rule.examples().is_empty(),
            "rule `{}` has no examples",
            rule.id(),
        );
    }
}

/// Every documented example must actually produce a finding of its own rule —
/// guards against a snippet that looks plausible but no longer triggers. Lints
/// under the same synthetic `example.jl` path as `render_rule_doc`, which the
/// include-graph rules need for a base directory (and the self-include
/// example's own identity).
#[test]
fn documented_examples_actually_trigger() {
    for rule in all_rules() {
        for example in rule.examples() {
            let config = LintConfig {
                select: Some(vec![rule.id().to_string()]),
                ..Default::default()
            };
            let report = check_source_with_target(
                Some(std::path::Path::new("example.jl")),
                example.source,
                &config,
                rule.example_julia_target(),
            );
            assert!(
                report.diagnostics.iter().any(|d| d.rule == rule.id()),
                "example for rule `{}` produced no finding of that rule:\n{}",
                rule.id(),
                example.source,
            );
        }
    }
}
