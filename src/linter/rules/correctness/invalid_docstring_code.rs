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
        "Flag malformed Julia inside a static docstring's explicit Julia-bearing \
         fence. Ordinary `julia` and Documenter directive bodies are parsed \
         directly; `julia-repl` and `jldoctest` transcripts parse only their \
         Julia inputs, not expected output. Plain and foreign-language fences, \
         opaque docstrings, and docstrings whose Markdown does not parse \
         cleanly are left alone."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "The declared Julia input is incomplete:",
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
