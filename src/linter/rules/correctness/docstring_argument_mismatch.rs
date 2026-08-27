//! `docstring-argument-mismatch`: a stale name in `# Arguments`.
//!
//! This is deliberately not a documentation-coverage rule. It checks only the
//! conventional first inline-code name of a list item and reports a name that
//! is absent from the attached function or macro signature. Undocumented
//! parameters and non-conventional prose remain policy, not correctness.

use crate::linter::diagnostic::Diagnostic;
use crate::linter::rules::{Example, Rule, RuleContext};

pub struct DocstringArgumentMismatch;

impl Rule for DocstringArgumentMismatch {
    fn id(&self) -> &'static str {
        "docstring-argument-mismatch"
    }

    fn description(&self) -> &'static str {
        "Flag a conventional ``- `name`: ...`` entry in a static docstring's \
         `# Arguments` section when `name` is not a positional or keyword \
         parameter of the attached function or macro. Missing entries are not \
         reported: documentation coverage and less structured argument prose \
         remain opt-in policy rather than a default correctness check."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "The signature says `radius`, but the argument entry retained a typo:",
            source: "\"\"\"\n# Arguments\n- `raduis`: The radius.\n\"\"\"\narea(radius) = pi * radius^2\n",
        }]
    }

    fn check_file(&self, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        for mismatch in &ctx.documentation_scan().argument_mismatches {
            sink.push(Diagnostic::new(
                self.id(),
                mismatch.range,
                format!(
                    "documented argument `{}` is not in the attached signature",
                    mismatch.name
                ),
            ));
        }
    }
}
