//! `sorted-extremum`: sorting a whole collection to read one end of it.
//!
//! `sort(xs)[1]` orders every element — `O(n log n)`, plus a copy of the
//! collection — to answer a question one pass gives: `minimum(xs)`. The
//! mirrored spelling `sort(xs)[end]` is `maximum(xs)`, and `[begin]` is
//! `[1]`'s.
//!
//! Only a plain `sort(x)` matches. `sort(x; rev=true)`, `sort(x; by=abs)`, and
//! `sort(x; lt=...)` all decide *which* element lands at each end, so the index
//! no longer picks the extremum `minimum`/`maximum` would return; a keyword
//! therefore leaves the expression alone rather than being rewritten around.
//! `sort!` is a different function (it also mutates), and any index but the two
//! ends asks a question no reducer answers — `sort(xs)[2]` really does need the
//! order.
//!
//! Two namespace gates, as in
//! [`LengthZero`](crate::linter::rules::readability::LengthZero): `sort` must
//! be confirmed to be Base's for the finding, and the *fix* additionally needs
//! `minimum`/`maximum` to mean Base's at the rewrite site, since it splices in
//! a name the source does not contain yet.
//!
//! **The fix is `Unsafe`.** `sort` and `minimum` disagree about a collection
//! that contains `NaN`: sorting orders `NaN` last (`isless` puts it there), so
//! `sort(xs)[1]` returns the smallest real number, while `minimum` propagates
//! the `NaN`. The two also part ways on emptiness only in which error they
//! raise. Whether a collection can hold a `NaN` needs types, so the rewrite
//! waits for `--unsafe-fixes`.
//!
//! The edit replaces the whole indexing with the extremum call, reusing the
//! collection's own source text; it is withheld when a comment sits in the
//! replaced span outside that text.

use crate::ast::{AstNode, Expr, HasArgList, IndexExpr};
use crate::linter::diagnostic::{Applicability, Diagnostic, Fix};
use crate::linter::rules::matchers;
use crate::linter::rules::rewrite;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::syntax::{SyntaxElement, SyntaxKind, SyntaxNode};

pub struct SortedExtremum;

impl Rule for SortedExtremum {
    fn id(&self) -> &'static str {
        "sorted-extremum"
    }

    fn description(&self) -> &'static str {
        "Flag indexing one end of a freshly sorted collection: `sort(xs)[1]` \
         copies and orders every element to answer what `minimum(xs)` answers \
         in one pass, and `sort(xs)[end]` (like `sort(xs)[begin]` for the other \
         end) does the same for `maximum(xs)`.\n\n\
         Only a plain `sort(x)` at either end is flagged. A `rev`, `by`, or \
         `lt` keyword decides which element lands where, so the index no longer \
         picks the extremum a reducer would return; any other index \
         (`sort(xs)[2]`) genuinely needs the order; and `sort!` is a different \
         function. `sort` must be confirmed to be Base's, so a local shadow or \
         a qualified `Base.sort` reports nothing.\n\n\
         The fix replaces the indexing with the extremum call, but needs \
         `--unsafe-fixes`: sorting orders `NaN` last, so `sort(xs)[1]` returns \
         the smallest real number where `minimum(xs)` propagates the `NaN`. It \
         is withheld — the finding still stands — when the file's \
         `minimum`/`maximum` is not Base's, or when a comment sits in the \
         rewritten span outside the collection."
    }

    fn examples(&self) -> &'static [Example] {
        &[
            Example {
                caption: "Sorting to read the smallest element:",
                source: "lowest = sort(scores)[1]\n",
            },
            Example {
                caption: "And the largest:",
                source: "highest = sort(scores)[end]\n",
            },
        ]
    }

    fn interests(&self) -> &'static [SyntaxKind] {
        &[SyntaxKind::INDEX_EXPR]
    }

    fn check(&self, el: &SyntaxElement, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let Some(index) = el.as_node().cloned().and_then(IndexExpr::cast) else {
            return;
        };
        let Some(Expr::CallExpr(sort)) = index.base() else {
            return;
        };
        let Some((sort, mut args)) = matchers::plain_call(sort.syntax(), "sort", 1) else {
            return;
        };
        let (Some(coll), Some(end)) = (args.pop(), selected_end(&index)) else {
            return;
        };
        if !ctx.resolves_to_base(&sort) {
            return;
        }

        let reducer = end.reducer();
        let message = match rewrite::inline_text(index.syntax()) {
            Some(indexing) => {
                let coll = coll.syntax().text();
                format!(
                    "call `{reducer}({coll})` instead of `{indexing}`, which sorts \
                     the whole collection"
                )
            }
            None => format!(
                "call `{reducer}` instead of indexing a sorted copy, which sorts \
                 the whole collection"
            ),
        };
        let mut diag = Diagnostic::new(self.id(), index.syntax().text_range(), message);
        if let Some(fix) = extremum_rewrite(ctx, &index, &coll, reducer) {
            diag.fixes.push(fix);
        }
        sink.push(diag);
    }
}

/// Which end of the sorted collection an index selects.
#[derive(Clone, Copy)]
enum End {
    First,
    Last,
}

impl End {
    /// The Base reducer answering the same question in one pass.
    fn reducer(self) -> &'static str {
        match self {
            End::First => "minimum",
            End::Last => "maximum",
        }
    }
}

/// The end `index` selects, if it selects one: the literal `1` or `begin` for
/// the first element and `end` for the last.
///
/// The index list must hold exactly that one argument. A second index
/// (`sort(A)[1, 1]`) indexes a matrix by dimension, where the first element of
/// the sorted copy is not what `minimum` returns for the whole array.
fn selected_end(index: &IndexExpr) -> Option<End> {
    let args = index.arg_list()?;
    let mut children = args.syntax().children();
    let arg = children.next()?;
    if children.next().is_some() || arg.kind() != SyntaxKind::ARG {
        return None;
    }
    let selector = arg.first_child()?;
    match selector.kind() {
        SyntaxKind::BEGIN_MARKER => Some(End::First),
        SyntaxKind::END_MARKER => Some(End::Last),
        SyntaxKind::LITERAL => is_one(&selector).then_some(End::First),
        _ => None,
    }
}

/// Whether a literal is the integer `1` written as such. A float (`1.0`) or
/// another base (`0x1`) is a different spelling of an index no one writes, and
/// not worth trusting a rewrite to.
fn is_one(literal: &SyntaxNode) -> bool {
    crate::ast::Literal::cast(literal.clone())
        .and_then(|lit| lit.numeric_token())
        .is_some_and(|token| token.kind() == SyntaxKind::INTEGER && token.text() == "1")
}

/// The unsafe fix replacing the whole indexing with the extremum call.
///
/// Withheld when `reducer` would not mean Base's at this point in the file, or
/// when a comment sits in the replaced span outside the collection — the only
/// other tokens there are `sort`, the brackets, and the index, so a `#` outside
/// the collection is always a comment and never a string.
fn extremum_rewrite(
    ctx: &RuleContext<'_>,
    index: &IndexExpr,
    coll: &Expr,
    reducer: &'static str,
) -> Option<Fix> {
    let span = index.syntax().text_range();
    if !ctx.name_resolves_to_base(reducer, span.start()) {
        return None;
    }
    if rewrite::drops_a_comment(index.syntax(), &[coll.syntax().text_range()]) {
        return None;
    }
    Some(Fix {
        description: format!("Replace the sorted indexing with `{reducer}`"),
        content: format!("{reducer}({})", coll.syntax().text()),
        start: span.start().into(),
        end: span.end().into(),
        applicability: Applicability::Unsafe,
    })
}
