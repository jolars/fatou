//! `unexplained-suppression`: a `# fatou-ignore` directive with no reason.
//!
//! A suppression is a claim — "this finding is wrong, or not worth fixing,
//! here" — and the reason is the only record of why. Without one the next
//! reader cannot tell a considered exemption from a finding someone silenced to
//! get a build green, and cannot tell whether the exemption still holds.
//!
//! Off by default: the reason is optional by design (`suppression.rs`), a
//! project that does not want the convention should not be told it is wrong,
//! and turning it on is one `select` entry. No fix — the reason is exactly the
//! part that has to be written by a human.

use crate::linter::diagnostic::Diagnostic;
use crate::linter::rules::{Example, Rule, RuleContext};

pub struct UnexplainedSuppression;

impl Rule for UnexplainedSuppression {
    fn id(&self) -> &'static str {
        "unexplained-suppression"
    }

    fn default_enabled(&self) -> bool {
        false
    }

    fn description(&self) -> &'static str {
        "Flag a `# fatou-ignore` directive that states no reason. The `: \
         <reason>` part is optional, so this rule is **off by default**; select \
         it in a project that wants every suppression to record why the finding \
         was accepted. There is no fix: the reason is the part only the author \
         can supply."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "A suppression with nothing to say for itself:",
            source: "# fatou-ignore unused-binding\nfunction f()\n    handle = open_device()\n    1\nend\n",
        }]
    }

    fn check_file(&self, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        for directive in ctx.suppressions.directives() {
            if directive.has_reason() {
                continue;
            }
            let mut diag =
                Diagnostic::new(self.id(), directive.comment, "suppression states no reason");
            diag.message = diag
                .message
                .with_suggestion("add one: `# fatou-ignore <rule>: <reason>`");
            sink.push(diag);
        }
    }
}
