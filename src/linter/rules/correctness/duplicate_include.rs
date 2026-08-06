//! `duplicate-include`: a static `include("path")` whose target an earlier
//! `include` in the same file already pulled in. Julia's `include` has no
//! include-guard: the second call silently evaluates the file again, redefining
//! its methods and re-running its top-level side effects. Either the repeat is
//! dead weight or one of the two `include`s meant a different file.
//!
//! Like `missing-include-file` and `include-cycle`, the check is driven by the
//! include problems the lint driver precomputes (see
//! [`crate::linter::include_graph`]), so the rule itself never touches the
//! filesystem and stays silent where the driver supplies no problems (a
//! pathless document, and the language server, which publishes its own
//! include-graph diagnostics). Only statically resolvable includes participate,
//! so a repeated dynamic or interpolated `include` is invisible here as
//! everywhere else.
//!
//! Repetition is decided on the *resolved* target, so two spellings of one file
//! (`"a.jl"` and `"./a.jl"`) still count as a repeat — which is why the rule
//! matches problems back to their call sites by edge index rather than by the
//! raw literal, and why it enumerates the file's include sites in one
//! whole-file pass. Two guards keep the honest patterns out: the same file
//! included into two different `module` blocks runs into two namespaces and is
//! not flagged, and one file reached along two different include *paths* (a
//! diamond) is nobody's local repeat. Only the second and later `include`s are
//! flagged; the first is the one doing the intended work. No fix: which of the
//! two calls to drop — and whether the repeat was meant to name another file —
//! is the author's call.

use std::collections::HashSet;

use rowan::ast::AstNode;

use crate::ast::CallExpr;
use crate::linter::diagnostic::Diagnostic;
use crate::linter::include_graph::IncludeProblemKind;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::project::include_literal;

pub struct DuplicateInclude;

impl Rule for DuplicateInclude {
    fn id(&self) -> &'static str {
        "duplicate-include"
    }

    fn description(&self) -> &'static str {
        "Flag a static `include(\"path\")` that pulls in a file an earlier \
         `include` in the same file already did. Julia has no include guard, so \
         the repeat evaluates the file a second time, redefining its methods \
         and re-running its top-level code. Paths are compared after resolution, \
         so `\"a.jl\"` and `\"./a.jl\"` count as the same file. Including one \
         file into two different `module` blocks is not flagged — that runs its \
         definitions into two separate namespaces — and neither is a file \
         reached twice along different include chains."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "The same file included twice, so its definitions run again:",
            source: "include(\"util.jl\")\ninclude(\"util.jl\")\n",
        }]
    }

    fn check_file(&self, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let repeats: HashSet<usize> = ctx
            .includes
            .iter()
            .filter(|problem| problem.kind == IncludeProblemKind::Duplicate)
            .map(|problem| problem.edge)
            .collect();
        if repeats.is_empty() {
            return;
        }

        // The same enumeration `include_edges` performs, through the same
        // staticness test, so the indices line up.
        let sites = ctx
            .root
            .descendants()
            .filter_map(CallExpr::cast)
            .filter_map(|call| include_literal(&call));
        for (index, literal) in sites.enumerate() {
            if !repeats.contains(&index) {
                continue;
            }
            let raw: String = literal
                .content_tokens()
                .map(|token| token.text().to_string())
                .collect();
            sink.push(Diagnostic::new(
                self.id(),
                literal.syntax().text_range(),
                format!("duplicate include: \"{raw}\" is already included earlier in this file"),
            ));
        }
    }
}
