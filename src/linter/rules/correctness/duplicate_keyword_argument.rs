//! `duplicate-keyword-argument`: the same keyword supplied twice at one call
//! site.
//!
//! Julia rejects `h(a = 1, a = 2)` at lowering (`syntax: keyword argument "a"
//! repeated in call to "h"`), so the code parses clean and then cannot run —
//! always a bug. The call-side sibling of the definition-side
//! [`DuplicateArgument`](super::DuplicateArgument), and it shares that rule's
//! namespace: keywords before and after the `;` are one set, the `;`-block
//! shorthand `h(; a)` included, so `h(a = 1; a)` is a duplicate too.
//!
//! A keyword splat (`h(; kw...)`) carries an unknowable set of names, so it
//! contributes none: [`CallShape`] leaves it out of `keywords` entirely and the
//! rule never guesses what is inside it. It does *not* silence the check for
//! the names that *are* written — Julia rejects `h(a = 1; kw..., a = 2)` just
//! the same — so an open keyword set weakens nothing here.
//!
//! Two shapes are exempt. A definition's signature is a `CALL_EXPR` too, but it
//! declares parameters rather than passing arguments; the repeat there is
//! `duplicate-argument`'s finding (excluded by
//! [`matchers::call_expr`]). And quoted code is data rather than a call, while
//! a macro receives its arguments unevaluated and may rewrite them into
//! something that never reaches lowering as written — the same span exemption
//! `call-arity` takes.
//!
//! No fix: which of the two spellings the author meant is not recoverable from
//! the source, and Julia defines no winner to preserve.

use crate::ast::AstToken;
use crate::linter::diagnostic::{Diagnostic, Severity};
use crate::linter::rules::matchers::{self, CallShape};
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::syntax::{SyntaxElement, SyntaxKind};

pub struct DuplicateKeywordArgument;

impl Rule for DuplicateKeywordArgument {
    fn id(&self) -> &'static str {
        "duplicate-keyword-argument"
    }

    /// Julia rejects the call at lowering, so it can never run.
    fn default_severity(&self) -> Severity {
        Severity::Error
    }

    fn description(&self) -> &'static str {
        "Flag the same keyword argument supplied more than once at a call \
         site. Julia rejects such a call at lowering (`keyword argument \"a\" \
         repeated in call to \"h\"`), so it parses but cannot run. Keywords \
         before and after the `;` share one namespace, and the `;`-block \
         shorthand `h(; a)` counts as passing `a`. A keyword splat \
         (`h(; kw...)`) names nothing the rule can read, so it neither \
         triggers the finding nor silences it for the keywords written out. \
         Calls inside quoted code or a macro call are exempt, since neither is \
         lowered as written."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "`label` is supplied twice in one call:",
            source: "plot(xs, ys, label = \"before\", label = \"after\")\n",
        }]
    }

    fn interests(&self) -> &'static [SyntaxKind] {
        &[SyntaxKind::CALL_EXPR]
    }

    fn check(&self, el: &SyntaxElement, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let Some(node) = el.as_node() else {
            return;
        };
        // A definition's signature declares parameters rather than passing
        // arguments — `duplicate-argument`'s business, not this rule's.
        let Some(call) = matchers::call_expr(node) else {
            return;
        };
        if ctx.file_scan().in_skipped(node.text_range()) {
            return;
        }

        let shape = CallShape::of(&call);
        let mut seen: Vec<&str> = Vec::new();
        for keyword in &shape.keywords {
            let name = keyword.name.text();
            if seen.contains(&name) {
                sink.push(Diagnostic::new(
                    self.id(),
                    keyword.name.syntax().text_range(),
                    format!("keyword argument `{name}` is passed more than once"),
                ));
            } else {
                seen.push(name);
            }
        }
    }
}
