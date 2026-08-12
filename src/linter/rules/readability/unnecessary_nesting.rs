//! `unnecessary-nesting`: an `if` whose entire body is another `if`, where the
//! two tests are one `&&` test written on two levels of indentation.
//!
//! ```julia
//! if a          if a && b
//!     if b   =>     body
//!         body  end
//!     end
//! end
//! ```
//!
//! The two spellings agree on every input. `if` demands a `Bool` and raises a
//! `TypeError` on anything else, and `&&` does the same for its left operand
//! and hands its right one back untouched — so `if a && b` evaluates `a`,
//! stops on `false` exactly where the nested form skips the inner `if`, and
//! otherwise evaluates `b` under the same `Bool` demand. `if` opens no scope in
//! Julia, so merging the two blocks rebinds nothing, and a body-less `if`
//! yields `nothing` either way, so the value of the construct is preserved too.
//!
//! **An alternative on either `if` breaks that agreement**, which is why both
//! must be a bare `if`/`end`. With an outer `else`, the case "`a` holds, `b`
//! does not" runs nothing before the merge and the `else` branch after it; with
//! an inner `elseif`, the merge hands that branch the cases where `a` is false.
//! The same reasoning rules out reporting an `elseif` *clause* as the outer
//! half — its own `if` may carry a later branch that the merge would expose —
//! so only a whole `IF_EXPR` is dispatched.
//!
//! A body holding anything besides the inner `if` is left alone: the outer test
//! then guards more than the inner one, and there is nothing to merge.
//!
//! A deeper nest reports once per adjacent pair (`if a; if b; if c` is two
//! findings), and the fixes converge over the re-lint loop, since merging the
//! outer pair leaves an `if a && b` that again only guards `if c`.
//!
//! **The fix replaces the outer `if` with its own pieces**: the two tests and
//! the inner block, all spliced verbatim, so whatever they contain survives.
//! Only the inner header, the inner `end`, and the whitespace around them are
//! discarded — the fix is withheld (the finding still stands) when a comment
//! sits in that discarded text. The inner body keeps its original indentation,
//! which is the formatter's business and not a fix's (the pipeline is
//! fix-then-format).
//!
//! A test is spliced bare only when it binds at least as tightly as `&&`;
//! everything else is parenthesized, because `&&` binds tighter than `||` and
//! would otherwise capture an operand (`if a || c` nested in `if b` is
//! `(a || c) && b`, never `a || c && b`). The parser's precedence table is not
//! exposed over the CST, so the tight set is an explicit list of shapes and
//! operators rather than a lookup: a name, a literal, a bracketed or delimited
//! form, a call, a prefix operator, a comparison, and the arithmetic/`.`/`::`
//! tiers. Anything outside it is parenthesized rather than guessed at, an
//! unparenthesized macro call (`@foo a`, which swallows what follows it)
//! included.

use crate::ast::{AstNode, Block, Expr, HasCondition, IfExpr};
use crate::linter::diagnostic::{Applicability, Diagnostic, Fix};
use crate::linter::rules::rewrite::drops_a_comment;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::syntax::{SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken};

pub struct UnnecessaryNesting;

impl Rule for UnnecessaryNesting {
    fn id(&self) -> &'static str {
        "unnecessary-nesting"
    }

    fn description(&self) -> &'static str {
        "Flag an `if` whose entire body is another `if`, where neither carries \
         an `elseif` or an `else`. The two tests are one `&&` test spread over \
         two levels of indentation: `if a; if b; body; end; end` is \
         `if a && b; body; end`.\n\n\
         The two spellings agree on every input. `if` demands a `Bool` and \
         `&&` hands its right operand back untouched, so the merged test stops \
         on a false `a` exactly where the nested form skips the inner `if`, and \
         `if` opens no scope in Julia, so merging the blocks rebinds nothing.\n\n\
         An alternative on either `if` breaks that agreement and is not \
         reported: with an outer `else`, the case where `a` holds and `b` does \
         not runs nothing before the merge and the `else` branch after it. A \
         body holding anything besides the inner `if` is left alone for the \
         same reason — the outer test guards more than the inner one.\n\n\
         The fix splices the two tests and the inner block verbatim, \
         parenthesizing a test that binds looser than `&&` (`if a || c` nested \
         in `if b` becomes `(a || c) && b`). It is withheld — the finding still \
         stands — when a comment sits in the discarded headers. The inner body \
         keeps its indentation, which the formatter settles."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "An `if` guarding nothing but another `if` is one test:",
            source: "if isopen(io)\n    if !eof(io)\n        read(io)\n    end\nend\n",
        }]
    }

    fn interests(&self) -> &'static [SyntaxKind] {
        &[SyntaxKind::IF_EXPR]
    }

    fn check(&self, el: &SyntaxElement, _ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let Some(node) = el.as_node() else { return };
        let Some(outer) = IfExpr::cast(node.clone()) else {
            return;
        };
        let Some((inner, inner_body)) = merge_candidate(&outer) else {
            return;
        };
        let (Some(outer_test), Some(inner_test)) = (
            outer.condition().and_then(|c| c.expr()),
            inner.condition().and_then(|c| c.expr()),
        ) else {
            return;
        };

        let merged = format!(
            "{} && {}",
            operand_text(&outer_test),
            operand_text(&inner_test)
        );
        let message = if merged.contains('\n') {
            "this `if` only guards another `if`: merge the two tests with `&&`".to_string()
        } else {
            format!("this `if` only guards another `if`: write `if {merged}`")
        };

        let outer_range = node.text_range();
        let header = outer
            .condition()
            .map_or(outer_range, |cond| cond.syntax().text_range());
        let mut diag = Diagnostic::new(
            self.id(),
            rowan::TextRange::new(outer_range.start(), header.end()),
            message,
        );

        let keep = [
            outer_test.syntax().text_range(),
            inner_test.syntax().text_range(),
            inner_body.syntax().text_range(),
        ];
        if !drops_a_comment(node, &keep) {
            diag.fixes.push(Fix {
                description: "Merge the nested `if` into its parent with `&&`".to_string(),
                content: format!("if {merged}{}end", inner_body.syntax().text()),
                start: outer_range.start().into(),
                end: outer_range.end().into(),
                applicability: Applicability::Safe,
            });
        }
        sink.push(diag);
    }
}

/// The `if` that is the whole body of `outer`, together with its block, when
/// merging the two preserves behavior.
///
/// That needs three things: neither `if` carries an alternative (see the module
/// docs), and the outer body holds the inner `if` and nothing else. Trivia does
/// not count as a statement, so a comment beside the inner `if` still matches —
/// it is the fix, not the finding, that has to answer for one.
fn merge_candidate(outer: &IfExpr) -> Option<(IfExpr, Block)> {
    if has_alternative(outer) {
        return None;
    }
    let mut statements = outer.then_body()?.syntax().children();
    let only = statements.next()?;
    if statements.next().is_some() {
        return None;
    }
    let inner = IfExpr::cast(only)?;
    if has_alternative(&inner) {
        return None;
    }
    let inner_body = inner.then_body()?;
    Some((inner, inner_body))
}

/// Whether `if_expr` has a branch other than its `then` block.
fn has_alternative(if_expr: &IfExpr) -> bool {
    if_expr.elseif_clauses().next().is_some() || if_expr.else_clause().is_some()
}

/// `expr`'s source text as an operand of `&&`, parenthesized when a bare splice
/// would rebind.
fn operand_text(expr: &Expr) -> String {
    let text = expr.syntax().text().to_string();
    if binds_at_least_as_tight_as_and(expr) {
        text
    } else {
        format!("({text})")
    }
}

/// Whether `expr` can sit bare on either side of `&&`: it is delimited (a
/// literal, a name, anything bracketed), a call or index chain, a prefix
/// operator, or an infix operator from a tier that binds tighter than `&&`.
///
/// Everything else is parenthesized rather than guessed at. Deciding the rest
/// needs the parser's precedence table, which is not exposed over the CST, and
/// the parentheses cost nothing but two characters — `(x) && y` is the same
/// test. A macro call is deliberately absent: unparenthesized, `@foo a` takes
/// everything after it as an argument, so it is looser than any operator.
fn binds_at_least_as_tight_as_and(expr: &Expr) -> bool {
    match expr {
        Expr::Literal(_)
        | Expr::StringLiteral(_)
        | Expr::CmdLiteral(_)
        | Expr::NonstandardIdentifier(_)
        | Expr::Interpolation(_)
        | Expr::Name(_)
        | Expr::ParenExpr(_)
        | Expr::TupleExpr(_)
        | Expr::VectExpr(_)
        | Expr::MatrixExpr(_)
        | Expr::Comprehension(_)
        | Expr::Braces(_)
        | Expr::CurlyExpr(_)
        | Expr::CallExpr(_)
        | Expr::IndexExpr(_)
        | Expr::DotCallExpr(_)
        | Expr::UnaryExpr(_)
        | Expr::TypeAnnotation(_)
        | Expr::WhereExpr(_) => true,
        Expr::BinaryExpr(bin) => infix_token(bin.syntax()).is_some_and(|op| tight_infix(&op)),
        Expr::Other(node) => matches!(
            node.kind(),
            SyntaxKind::COMPARISON_EXPR | SyntaxKind::RANGE_EXPR | SyntaxKind::POSTFIX_EXPR
        ),
        _ => false,
    }
}

/// The infix operator's own token: the first non-trivia token directly under a
/// `BINARY_EXPR`. Not [`BinaryExpr::op`](crate::ast::BinaryExpr::op), because
/// `isa` and `in` are spelled as identifiers rather than operator tokens and
/// that accessor only casts the operator set.
fn infix_token(binary: &SyntaxNode) -> Option<SyntaxToken> {
    binary
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|token| {
            !matches!(
                token.kind(),
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE | SyntaxKind::COMMENT
            )
        })
}

/// Whether an infix operator binds tighter than `&&` on both edges.
///
/// The list is deliberately partial (see [`binds_at_least_as_tight_as_and`]):
/// it covers the tiers a test is actually written in — the comparisons,
/// `&&` itself, the arithmetic and bitwise families, the ranges, `::`, `.`, and
/// the pipes — plus `isa`/`in`, which the lexer hands over as identifiers.
/// The looser tiers (`||`, the arrows and `=>`, `~`, assignment) fall through
/// to `false` along with everything unlisted.
fn tight_infix(op: &SyntaxToken) -> bool {
    use SyntaxKind::*;
    if matches!(
        op.kind(),
        EQ_EQ
            | NOT_EQ
            | EQ_EQ_EQ
            | NOT_EQ_EQ
            | LT
            | LE
            | GT
            | GE
            | SUBTYPE
            | SUPERTYPE
            | DOT_EQ_EQ
            | DOT_NOT_EQ
            | DOT_EQ_EQ_EQ
            | DOT_NOT_EQ_EQ
            | DOT_LT
            | DOT_LE
            | DOT_GT
            | DOT_GE
            | DOT_SUBTYPE
            | DOT_SUPERTYPE
            | AND_AND
            | DOT_AND_AND
            | PLUS
            | MINUS
            | STAR
            | SLASH
            | BACKSLASH
            | PERCENT
            | CARET
            | SLASH_SLASH
            | DOT_PLUS
            | DOT_MINUS
            | DOT_STAR
            | DOT_SLASH
            | DOT_BACKSLASH
            | DOT_PERCENT
            | DOT_CARET
            | DOT_SLASH_SLASH
            | AMP
            | PIPE
            | SHL
            | SHR
            | USHR
            | COLON
            | DOT_DOT
            | COLON_COLON
            | DOT
            | PIPE_GT
    ) {
        return true;
    }
    op.kind() == IDENT && matches!(op.text(), "isa" | "in")
}
