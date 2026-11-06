//! `blanket-suppression`: a `# fatou-ignore` directive that names no rule.
//!
//! Two shapes reach it, and they fail in opposite directions. A file-wide
//! `# fatou-ignore-file` (bare or with only a reason) silences *every* rule in
//! the file, including the ones that have not been written yet — the linter
//! goes dark on that file for good. A bare node-level `# fatou-ignore` silences
//! *nothing*: the directive form requires a rule ID, so the comment reads like a
//! suppression while the finding it was meant to hide is still reported.
//!
//! Either way the fix is the same in kind — name the rule — but not one this
//! rule can write: nothing in the source says which rule the author meant. So
//! the finding carries no fix.
//!
//! An unknown rule ID is *not* blanket: the author named a rule, they just named
//! one that does not exist, which is `misnamed-suppression`'s finding.

use crate::linter::diagnostic::Diagnostic;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::linter::suppression::DirectiveKind;

pub struct BlanketSuppression;

impl Rule for BlanketSuppression {
    fn id(&self) -> &'static str {
        "blanket-suppression"
    }

    fn description(&self) -> &'static str {
        "Report a suppression comment with no rule ID. A bare `# fatou-ignore-file` disables \
         every rule, including rules added later. A bare `# fatou-ignore` suppresses nothing. \
         Name the specific rules to suppress."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "A file suppression without a rule ID:",
            source: "# fatou-ignore-file: generated code\n\nstruct Point\n    x::Float64\nend\n",
        }]
    }

    fn check_file(&self, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        for directive in ctx.suppressions.directives() {
            if directive.rule.is_some() {
                continue;
            }
            let message = match directive.kind {
                DirectiveKind::FileAll => {
                    "suppression names no rule; it silences every rule in this file"
                }
                // A node directive with no rule ID never made it into the
                // lookup tables, so it suppresses nothing.
                _ => "suppression names no rule; it suppresses nothing",
            };
            let mut diag = Diagnostic::new(self.id(), directive.comment, message);
            diag.message = diag
                .message
                .with_suggestion("name the rule: `# fatou-ignore <rule>: <reason>`");
            sink.push(diag);
        }
    }
}
