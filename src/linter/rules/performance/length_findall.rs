//! `length-findall`: building an index vector only to ask how long it is.
//!
//! `findall` allocates a vector of every index whose element matches, so
//! `length(findall(isodd, xs))` pays for that vector and then throws it away.
//! `count(isodd, xs)` answers the same question with a counter. Both `findall`
//! arities have a `count` counterpart: `length(findall(mask))` over a boolean
//! collection is `count(mask)`.
//!
//! Both callees must be confirmed to be Base's, and both calls must be plain —
//! `findall` at one or two positional arguments, `length` at one. The *fix*
//! additionally needs `count` to mean Base's at the rewrite site, since it
//! splices in a name the source does not contain yet; the finding stands when
//! that gate fails.
//!
//! **The fix is `Unsafe`.** `findall` walks a collection's *keys* while `count`
//! iterates its *elements*, which is the same walk for an array and not for a
//! `Dict`: `findall(p, d)` tests each value and returns the matching keys,
//! where `count(p, d)` tests each `key => value` pair. Telling the two apart
//! needs types, so the rewrite waits for `--unsafe-fixes`.
//!
//! The edit replaces the whole pair with `count` carrying `findall`'s own
//! argument list, spliced byte for byte, so whatever it contains — a comment
//! included — survives. It is withheld when a comment sits outside that list
//! but inside the replaced span.

use crate::ast::{AstNode, CallExpr, Expr, HasArgList};
use crate::linter::diagnostic::{Applicability, Diagnostic, Fix};
use crate::linter::rules::matchers::{self, CallShape};
use crate::linter::rules::rewrite;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::syntax::{SyntaxElement, SyntaxKind};

pub struct LengthFindall;

impl Rule for LengthFindall {
    fn id(&self) -> &'static str {
        "length-findall"
    }

    fn description(&self) -> &'static str {
        "Flag `length(findall(p, x))`, which allocates a vector of every \
         matching index just to ask how many there are. `count(p, x)` answers \
         with a counter and no allocation, and covers the one-argument form \
         too: `length(findall(mask))` is `count(mask)`.\n\n\
         Both calls must be plain — no keyword arguments, no splats — and both \
         `length` and `findall` must be confirmed to be Base's, so a local \
         shadow, a qualified `Base.findall`, or a file whose imports cannot be \
         resolved reports nothing.\n\n\
         The fix rewrites the pair to a `count` call carrying `findall`'s own \
         argument list, but needs `--unsafe-fixes`: `findall` walks a \
         collection's keys where `count` iterates its elements, which is the \
         same walk for an array and a different one for a `Dict`, whose values \
         `findall` tests and whose `key => value` pairs `count` does. It is \
         withheld — the finding still stands — when the file's `count` is not \
         Base's, or when a comment sits in the rewritten span outside the \
         argument list."
    }

    fn examples(&self) -> &'static [Example] {
        &[
            Example {
                caption: "The index vector is built only to be measured:",
                source: "n = length(findall(isodd, xs))\n",
            },
            Example {
                caption: "The one-argument form counts a mask:",
                source: "hits = length(findall(mask))\n",
            },
        ]
    }

    fn interests(&self) -> &'static [SyntaxKind] {
        &[SyntaxKind::CALL_EXPR]
    }

    fn check(&self, el: &SyntaxElement, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let Some(node) = el.as_node() else { return };
        let Some((outer, mut args)) = matchers::plain_call(node, "length", 1) else {
            return;
        };
        let Some(Expr::CallExpr(inner)) = args.pop() else {
            return;
        };
        let Some(findall) = matchers::call_named(inner.syntax(), "findall") else {
            return;
        };
        // `findall(p, x)` and `findall(mask)` both count; anything else is a
        // shape this rule does not know.
        let shape = CallShape::of(&findall);
        if !(shape.is_plain(1) || shape.is_plain(2)) {
            return;
        }
        if !ctx.resolves_to_base(&outer) || !ctx.resolves_to_base(&findall) {
            return;
        }

        let inner_args = shape
            .positional
            .iter()
            .map(|arg| arg.syntax().text().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let message = if inner_args.contains('\n') {
            "call `count` instead of measuring `findall`'s result, which builds \
             an index vector just to count it"
                .to_string()
        } else {
            format!(
                "call `count({inner_args})` instead of `length(findall({inner_args}))`, \
                 which builds an index vector just to count it"
            )
        };
        let mut diag = Diagnostic::new(self.id(), outer.syntax().text_range(), message);
        if let Some(fix) = count_rewrite(ctx, &outer, &findall) {
            diag.fixes.push(fix);
        }
        sink.push(diag);
    }
}

/// The unsafe fix replacing the whole `length(findall(...))` with a `count`
/// call over `findall`'s own argument list.
///
/// Withheld when `count` would not mean Base's at this point in the file, or
/// when a comment sits in the replaced span outside the reused argument list —
/// the only other tokens there are the two callees and `length`'s parentheses,
/// so a `#` among them is always a comment and never a string.
fn count_rewrite(ctx: &RuleContext<'_>, outer: &CallExpr, findall: &CallExpr) -> Option<Fix> {
    let span = outer.syntax().text_range();
    if !ctx.name_resolves_to_base("count", span.start()) {
        return None;
    }
    let args = findall.arg_list()?;
    if rewrite::drops_a_comment(outer.syntax(), &[args.syntax().text_range()]) {
        return None;
    }
    Some(Fix {
        description: "Count the matches directly with `count`".to_string(),
        content: format!("count{}", args.syntax().text()),
        start: span.start().into(),
        end: span.end().into(),
        applicability: Applicability::Unsafe,
    })
}
