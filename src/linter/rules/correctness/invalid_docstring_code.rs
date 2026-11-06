//! `invalid-docstring-code`: malformed Julia in an explicit documentation fence.
//!
//! A fence that declares Julia code is executable documentation: editors parse
//! it as Julia, and Documenter may evaluate it. Static docstrings and clean
//! Markdown parses give this rule an exact nested source range. REPL and
//! doctest transcripts contribute only their input, never expected output.

use crate::linter::diagnostic::Diagnostic;
use crate::linter::rules::{Example, Rule, RuleContext};

pub struct InvalidDocstringCode;

impl Rule for InvalidDocstringCode {
    fn id(&self) -> &'static str {
        "invalid-docstring-code"
    }

    fn description(&self) -> &'static str {
        "Report syntax errors in Julia code blocks within docstrings. The rule checks `julia` \
         blocks and Documenter directives. For `julia-repl` and `jldoctest` transcripts, it \
         checks the input and skips expected output.\n\n\
         Other languages, dynamically constructed docstrings, and docstrings with malformed \
         Markdown are not checked."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "An incomplete Julia code block:",
            source: "\"\"\"\n~~~julia\nf(\n~~~\n\"\"\"\nf() = 1\n",
        }]
    }

    fn check_file(&self, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        for problem in &ctx.documentation_scan().invalid_code {
            sink.push(Diagnostic::new(
                self.id(),
                problem.range,
                format!("invalid Julia in documentation fence: {}", problem.message),
            ));
        }
    }
}
