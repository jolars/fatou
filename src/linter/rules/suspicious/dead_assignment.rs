//! A stored local value that no reachable path reads.

use crate::linter::diagnostic::Diagnostic;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::semantic::liveness;

pub struct DeadAssignment;

impl Rule for DeadAssignment {
    fn id(&self) -> &'static str {
        "dead-assignment"
    }

    fn description(&self) -> &'static str {
        concat!(
            "Report a local assignment whose stored value cannot be read before it is replaced ",
            "or leaves scope. A new iteration replaces the loop variable, so an update is only ",
            "useful if something reads it within that iteration. The analysis is conservative ",
            "around closures, macros, and exception handlers. For a never-read local covered ",
            "by `unused-binding`, this rule skips the first assignment."
        )
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "The second assignment replaces the first value of `x`:",
            source: "function f()\n    x = first_value()\n    x = second_value()\n    return x\nend\n",
        }]
    }

    fn check_file(&self, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        for (range, binding) in liveness::dead_stores(ctx.root, ctx.model, ctx.control_flow()) {
            let name = &ctx.model.binding(binding).name;
            sink.push(Diagnostic::new(
                self.id(),
                range,
                format!("value assigned to `{name}` is never read"),
            ));
        }
    }
}
