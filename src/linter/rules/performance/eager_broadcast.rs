//! `eager-broadcast`: reducing a broadcast that exists only to be reduced.
//!
//! `sum(abs.(xs))` broadcasts `abs` over `xs` into a fresh array and then sums
//! that array away. Every reducer this rule knows takes the function directly
//! — `sum(abs, xs)` — and applies it element by element as it reduces, so the
//! intermediate array is never built. The shape generalizes over the Base
//! reducers that carry an `(f, itr)` method: `all`, `any`, `count`, `maximum`,
//! `minimum`, `prod`, and `sum`.
//!
//! Only the exact `reducer(f.(x))` shape matches: one positional argument to
//! the reducer, one to the broadcast, no keywords on either. The excluded
//! shapes are excluded because they mean something else — `sum(f.(x), dims=1)`
//! reduces along an axis (which the two-argument method spells `sum(f, x;
//! dims=1)`, a different edit), `f.(x, y)` fuses two containers into a call
//! `sum(f, x, y)` does not describe, and `any(x .> 0)` broadcasts an operator
//! rather than a function. The reducer must be confirmed to be Base's, the gate
//! every idiom rule opens with; the broadcast callee is whatever the source
//! wrote, since the rewrite only moves it.
//!
//! **The fix is `Unsafe`.** The two spellings agree on every container, and
//! part ways on what is not one. Broadcasting treats a scalar — a number, a
//! string, a `Ref` — as a zero-dimensional container of itself, where the
//! reducer's `(f, itr)` method iterates it; and `any`/`all` stop at the first
//! decisive element, so a rewritten `f` runs fewer times, which is visible when
//! it prints, mutates, or throws. Neither is decidable without types, so the
//! rewrite waits for `--unsafe-fixes`.
//!
//! The edit replaces the broadcast argument alone (`f.(x)` -> `f, x`), leaving
//! the reducer and its parentheses untouched, and reuses the callee's and the
//! operand's own source text. It is withheld — the finding still stands — when
//! a comment sits between them, which the rewrite would drop.

use crate::ast::{AstNode, AstToken, Expr};
use crate::linter::diagnostic::{Applicability, Diagnostic, Fix};
use crate::linter::rules::matchers;
use crate::linter::rules::rewrite;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::syntax::{SyntaxElement, SyntaxKind};

/// The Base reducers with an `(f, itr)` method that folds the broadcast away.
/// Each reduces to a single value over all elements, so the mapped array it
/// would have consumed is pure overhead.
const REDUCERS: &[&str] = &["all", "any", "count", "maximum", "minimum", "prod", "sum"];

pub struct EagerBroadcast;

impl Rule for EagerBroadcast {
    fn id(&self) -> &'static str {
        "eager-broadcast"
    }

    fn description(&self) -> &'static str {
        "Flag a Base reducer applied to a broadcast that exists only to be \
         reduced: `sum(abs.(xs))` builds a whole mapped array and then sums it \
         away, where `sum(abs, xs)` applies `abs` as it reduces and allocates \
         nothing. The reducers with such an `(f, itr)` method are `all`, \
         `any`, `count`, `maximum`, `minimum`, `prod`, and `sum`.\n\n\
         Only the exact `reducer(f.(x))` shape is flagged. A keyword argument \
         on either call, a second positional argument, and a fused \
         multi-container broadcast (`hypot.(xs, ys)`) all describe a different \
         call and are left alone, as is a broadcast *operator* (`any(xs .> \
         0)`), which names no function to pass. The reducer must be confirmed \
         to be Base's, so a local shadow, a qualified `Base.sum`, or a file \
         whose imports cannot be resolved reports nothing.\n\n\
         The fix moves the function into the reducer's first argument, but \
         needs `--unsafe-fixes`: broadcasting treats a scalar as a \
         zero-dimensional container of itself where the two-argument method \
         iterates it, and `any`/`all` stop at the first decisive element, so a \
         function that prints, mutates, or throws runs a different number of \
         times. The fix is withheld — the finding still stands — when a \
         comment sits in the rewritten span."
    }

    fn examples(&self) -> &'static [Example] {
        &[
            Example {
                caption: "The mapped array is built only to be summed away:",
                source: "total = sum(abs.(residuals))\n",
            },
            Example {
                caption: "`any` need not test every element:",
                source: "if any(isnan.(xs))\n    error(\"bad data\")\nend\n",
            },
        ]
    }

    fn interests(&self) -> &'static [SyntaxKind] {
        &[SyntaxKind::CALL_EXPR]
    }

    fn check(&self, el: &SyntaxElement, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let Some(node) = el.as_node() else { return };
        let Some(name) = matchers::call_expr(node)
            .and_then(|call| call.callee_ident())
            .map(|ident| ident.text().to_string())
            .filter(|name| REDUCERS.contains(&name.as_str()))
        else {
            return;
        };
        let Some((call, mut args)) = matchers::plain_call(node, &name, 1) else {
            return;
        };
        let Some(Expr::DotCallExpr(broadcast)) = args.pop() else {
            return;
        };
        let Some((_, func, mut operands)) = matchers::plain_broadcast(broadcast.syntax(), 1) else {
            return;
        };
        let Some(operand) = operands.pop() else {
            return;
        };
        if !ctx.resolves_to_base(&call) {
            return;
        }

        let message = match (
            rewrite::inline_text(func.syntax()),
            rewrite::inline_text(operand.syntax()),
        ) {
            (Some(f), Some(x)) => format!(
                "call `{name}({f}, {x})` instead of `{name}({f}.({x}))`, which \
                 materializes the broadcast result"
            ),
            _ => format!(
                "pass the function to `{name}` instead of broadcasting it, which \
                 materializes the broadcast result"
            ),
        };
        let mut diag = Diagnostic::new(self.id(), call.syntax().text_range(), message);

        let span = broadcast.syntax().text_range();
        let keep = [func.syntax().text_range(), operand.syntax().text_range()];
        if !rewrite::drops_a_comment(broadcast.syntax(), &keep) {
            diag.fixes.push(Fix {
                description: format!("Pass the function as `{name}`'s first argument"),
                content: format!("{}, {}", func.syntax().text(), operand.syntax().text()),
                start: span.start().into(),
                end: span.end().into(),
                applicability: Applicability::Unsafe,
            });
        }
        sink.push(diag);
    }
}
