//! Rendering rule reference pages from rule metadata.
//!
//! [`render_rule_doc`] is the single source of truth shared by the snapshot
//! test (`tests/rule_docs.rs`) and the docs generator (`examples/docgen.rs`), so
//! the pinned docs and the generated files can never diverge. It runs the *real*
//! linter on each example, so the rendered diagnostics always reflect current
//! behavior.

use std::fmt::Write as _;
use std::path::PathBuf;

use crate::config::LintConfig;
use crate::linter::check::check_source;
use crate::linter::fix::apply_fixes;
use crate::linter::render::{OutputMode, render_findings};
use crate::linter::rules::Rule;

/// The synthetic path used when linting an example snippet. The same value keys
/// both the lint run and the `render_findings` source lookup.
fn example_path() -> PathBuf {
    PathBuf::from("example.jl")
}

/// Render the markdown reference page for a single rule.
pub fn render_rule_doc(rule: &dyn Rule) -> String {
    let mut out = String::new();
    let id = rule.id();
    let _ = writeln!(out, "# `{id}`");

    let description = rule.description().trim();
    if !description.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "{description}");
    }

    // Restrict to this rule so an example can't trip a different one.
    let config = LintConfig {
        select: Some(vec![id.to_string()]),
        ..Default::default()
    };
    let path = example_path();

    for example in rule.examples() {
        let _ = writeln!(out);
        if !example.caption.is_empty() {
            let _ = writeln!(out, "{}", example.caption);
            let _ = writeln!(out);
        }
        fenced(&mut out, "julia", example.source);

        let report = check_source(Some(&path), example.source, &config);
        let source = example.source.to_string();
        let rendered = render_findings(&report.diagnostics, OutputMode::Pretty, &|p| {
            (p == Some(path.as_path())).then(|| source.clone())
        });
        let _ = writeln!(out);
        fenced(&mut out, "text", &rendered);

        // Show the result of the safe fixes, when the example carries any.
        let fixed = apply_fixes(example.source, &report.diagnostics, false);
        if fixed.output != example.source {
            let _ = writeln!(out);
            let _ = writeln!(out, "After applying the fix:");
            let _ = writeln!(out);
            fenced(&mut out, "julia", &fixed.output);
        }
    }

    out
}

/// Write a fenced code block, normalizing the body to end with exactly one
/// newline so the closing fence always sits on its own line (idempotence).
fn fenced(out: &mut String, lang: &str, body: &str) {
    let _ = writeln!(out, "```{lang}");
    let _ = out.write_str(body);
    if !body.ends_with('\n') {
        let _ = out.write_str("\n");
    }
    let _ = writeln!(out, "```");
}

/// Convenience for the generator: `(id, page)` for every documented rule (one
/// carrying at least one example), in registry order.
pub fn documented_pages() -> Vec<(&'static str, String)> {
    crate::linter::rules::all_rules()
        .iter()
        .filter(|rule| !rule.examples().is_empty())
        .map(|rule| (rule.id(), render_rule_doc(rule.as_ref())))
        .collect()
}
