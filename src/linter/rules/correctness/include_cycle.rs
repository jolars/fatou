//! `include-cycle`: a static `include("path")` whose target transitively
//! `include`s this file again, the self-include being the smallest case.
//! Running the file recurses until the stack overflows (or, more typically,
//! fails on the duplicate definitions along the way).
//!
//! Like `missing-include-file`, the rule only matches the driver-precomputed
//! include problems (see [`crate::linter::include_graph`]) back to their call
//! sites by the literal's decoded path; the include following — which may read files
//! outside the lint set from disk, so a cycle closing through an unlinted file
//! is still caught — happens in the pre-pass. Every linted file on a cycle
//! blames its own `include` call, so the finding lands where the user is
//! looking. A diamond (one file reached along two disjoint paths) is not a
//! cycle. No fix: breaking a cycle means restructuring, not deleting a line.

use rowan::ast::AstNode;

use crate::ast::CallExpr;
use crate::linter::diagnostic::Diagnostic;
use crate::linter::include_graph::IncludeProblemKind;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::project::{include_literal, literal_path};
use crate::syntax::{SyntaxElement, SyntaxKind};

pub struct IncludeCycle;

impl Rule for IncludeCycle {
    fn id(&self) -> &'static str {
        "include-cycle"
    }

    fn description(&self) -> &'static str {
        "Flag a static `include(\"path\")` whose target transitively `include`s \
         this file again — including a file that includes itself. Running such \
         a file recurses without end. Every linted file on the cycle is \
         flagged at its own `include` call; a file merely included twice along \
         different paths (a diamond) is not a cycle."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "A file (here `example.jl`) that includes itself:",
            source: "include(\"example.jl\")\n",
        }]
    }

    fn interests(&self) -> &'static [SyntaxKind] {
        &[SyntaxKind::CALL_EXPR]
    }

    fn check(&self, el: &SyntaxElement, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let Some(call) = el.as_node().cloned().and_then(CallExpr::cast) else {
            return;
        };
        let Some(literal) = include_literal(&call) else {
            return;
        };
        let Some(path) = literal_path(&literal) else {
            return;
        };
        let cycles = ctx
            .includes
            .iter()
            .any(|problem| problem.kind == IncludeProblemKind::Cycle && problem.path == path);
        if cycles {
            sink.push(Diagnostic::new(
                self.id(),
                literal.syntax().text_range(),
                format!("include cycle: \"{path}\" transitively includes this file"),
            ));
        }
    }
}
