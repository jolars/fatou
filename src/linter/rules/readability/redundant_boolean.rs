//! `redundant-boolean`: comparing a test against a boolean literal, or
//! branching to one, where the test already *is* the answer.
//!
//! | written                | means      |
//! |------------------------|------------|
//! | `x == true`            | `x`        |
//! | `x != false`           | `x`        |
//! | `x == false`           | `!x`       |
//! | `x != true`            | `!x`       |
//! | `c ? true : false`     | `c`        |
//! | `c ? false : true`     | `!c`       |
//!
//! `==` and `!=` are symmetric, so the mirrored spellings (`true == x`) collapse
//! the same way and are matched by the same table.
//!
//! Distinct from
//! [`ConstantCondition`](crate::linter::rules::suspicious::ConstantCondition),
//! which owns the literal-*as*-test case (`if true`): there the literal is the
//! whole test and the branch is decided before the code runs, here the literal
//! is the thing the real test is compared against and nothing is decided early.
//!
//! **The two halves do not have the same safety, so they do not ship the same
//! fix.**
//!
//! The conditional half is exact. `?:` requires a `Bool` test — it raises a
//! `TypeError` on anything else — so on every input that does not throw,
//! `c ? true : false` evaluates `c` once and hands back that very `Bool`. Its
//! fix is `Safe`.
//!
//! The comparison half is not, because Julia's `==` is not identity. It
//! promotes across the numeric tower (`1 == true` is `true`), it answers
//! `missing` for `missing`, and it is user-overloadable, so `x == true` and `x`
//! agree only when `x` is already a `Bool` — which is what the author meant,
//! but not something the linter can see. That is precisely the redundancy worth
//! reporting and precisely why rewriting it is a behavior change, so its fix is
//! `Unsafe` and waits for `--unsafe-fixes`. (The roadmap entry called this one
//! safe; it is not, for the same reason `comparison-negation` dropped the
//! orderings.)
//!
//! Four shapes are deliberately left alone. `===` / `!==` carry their own
//! operator kinds and *are* the identity spelling: `x === true` is `false` for
//! every non-`Bool`, which is exactly what its author asked for. The broadcast
//! `.==` / `.!=` are containers of values rather than tests. A comparison
//! *chain* (`a == true == b`) folds into a `COMPARISON_EXPR` rather than a
//! `BINARY_EXPR` and has no two-operand rewrite — the same shape rule
//! [`LengthZero`](super::LengthZero) follows. And a comparison of two boolean
//! literals (`true == false`) has no operand to keep, so there is nothing to
//! rewrite it *to*; a conditional whose arms agree (`c ? true : true`) is
//! constant rather than redundant and belongs to no rule here.
//!
//! **The replacement is the surviving operand's own source text**, so whatever
//! it contains — spacing, a comment, a nested call — survives byte for byte.
//!
//! No rebinding reasoning is needed for the positive rewrite, and this is the
//! mirror image of [`ComparisonNegation`](super::ComparisonNegation)'s one
//! hazard: there the replacement bound *looser* than what it replaced, so a
//! neighbour could capture an operand. Here it binds at least as tightly. The
//! kept expression is already an operand of a comparison-tier `==` (or a `?:`
//! test), so anything containing the original unparenthesized had to bind
//! looser still, and the survivor fits wherever the whole did.
//!
//! The negated rewrite has to earn the same guarantee, since `!` binds at the
//! unary tier and would capture: `a + b == false` must not become `!a + b`,
//! which is `(!a) + b`. The operand is spliced bare only from an allow-list of
//! shapes that bind at least as tightly as `!` does; everything else is
//! wrapped, and `!(a + b)` is an atom in any position.
//!
//! The fix is **withheld** (the finding still stands) when a comment sits in
//! the replaced span *outside* the surviving operand, which the rewrite would
//! drop. Only the operator and the literals live there — `==`/`!=`, `?`, `:`,
//! `true`, `false` — so a `#` outside the operand is always a comment and never
//! a string. A comment *inside* the operand travels with it and costs nothing.

use crate::ast::{AstNode, AstToken, BinaryExpr, Expr, TernaryExpr};
use crate::linter::diagnostic::{Applicability, Diagnostic, Fix};
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::syntax::{SyntaxElement, SyntaxKind, SyntaxNode};

pub struct RedundantBoolean;

impl Rule for RedundantBoolean {
    fn id(&self) -> &'static str {
        "redundant-boolean"
    }

    fn description(&self) -> &'static str {
        "Flag a test compared against a boolean literal, or a conditional whose \
         two arms are the literals themselves. `x == true` and `x != false` are \
         `x`; `x == false` and `x != true` are `!x`; `c ? true : false` is `c` \
         and `c ? false : true` is `!c`. Because `==` and `!=` are symmetric, \
         the mirrored spellings (`true == x`) collapse the same way.\n\n\
         This is distinct from `constant-condition`, which owns the \
         literal-*as*-test case (`if true`), where the branch is decided before \
         the code runs.\n\n\
         The two halves do not ship the same fix. The conditional rewrite is \
         reported as a safe fix: `?:` requires a `Bool` test, so on every input \
         that does not throw, `c ? true : false` hands back that very `Bool`. \
         The comparison rewrite is reported as an unsafe fix, because `==` is \
         not identity — it promotes across the numeric tower (`1 == true` is \
         `true`), answers `missing` for `missing`, and is overloadable — so the \
         two spellings agree only when the operand is already a `Bool`.\n\n\
         The deliberate `===` / `!==`, the broadcast `.==` / `.!=`, and a \
         comparison chain are left alone, as are a comparison of two boolean \
         literals (no operand survives it) and a conditional whose arms agree \
         (constant rather than redundant).\n\n\
         The fix reuses the surviving operand's own source text, parenthesizing \
         it when a bare `!` would rebind (`a + b == false` becomes \
         `!(a + b)`). It is withheld — the finding still stands — when a \
         comment sits in the replaced span outside that operand."
    }

    fn examples(&self) -> &'static [Example] {
        &[
            Example {
                caption: "A conditional over the two literals is its own test:",
                source: "ready = queue_started(q) ? true : false\n",
            },
            Example {
                caption: "Comparing a test to a boolean literal restates it:",
                source: "if x.valid == false\n    reject(x)\nend\n",
            },
        ]
    }

    fn interests(&self) -> &'static [SyntaxKind] {
        &[SyntaxKind::BINARY_EXPR, SyntaxKind::TERNARY_EXPR]
    }

    fn check(&self, el: &SyntaxElement, _ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let Some(node) = el.as_node() else { return };
        let Some(found) = (match node.kind() {
            SyntaxKind::BINARY_EXPR => comparison(node),
            SyntaxKind::TERNARY_EXPR => conditional(node),
            _ => None,
        }) else {
            return;
        };

        let rewrite = if found.negated {
            negated_text(&found.kept)
        } else {
            found.kept.syntax().text().to_string()
        };
        let (message, description, applicability) = match found.form {
            Form::Comparison { literal } => (
                format!("comparing to `{literal}` is redundant: write `{rewrite}`"),
                format!("Drop the comparison to `{literal}`"),
                Applicability::Unsafe,
            ),
            Form::Conditional => (
                format!("this conditional just yields `{rewrite}`"),
                if found.negated {
                    "Replace the conditional with the negation of its test"
                } else {
                    "Replace the conditional with its test"
                }
                .to_string(),
                Applicability::Safe,
            ),
        };

        let range = node.text_range();
        let mut diag = Diagnostic::new(self.id(), range, message);
        if !drops_a_comment(node, &found.kept) {
            diag.fixes.push(Fix {
                description,
                content: rewrite,
                start: range.start().into(),
                end: range.end().into(),
                applicability,
            });
        }
        sink.push(diag);
    }
}

/// A matched redundancy: the operand that survives the rewrite, whether it
/// survives negated, and which spelling produced it.
struct Redundancy {
    kept: Expr,
    negated: bool,
    form: Form,
}

/// Which of the two spellings matched. They differ in how the finding reads and
/// in how safe the rewrite is (see the module docs).
enum Form {
    /// `x == true` and friends, carrying the literal's own text for the
    /// message.
    Comparison { literal: &'static str },
    /// `c ? true : false` and its flip.
    Conditional,
}

/// A comparison of one operand against a boolean literal, in either order.
///
/// Only `==` / `!=`: `===` / `!==` and the broadcast forms carry their own
/// operator kinds and are out of scope, and a chain is a `COMPARISON_EXPR` that
/// never reaches here. Two boolean literals leave no operand to keep, so they
/// answer `None` rather than picking a side arbitrarily.
fn comparison(node: &SyntaxNode) -> Option<Redundancy> {
    let bin = BinaryExpr::cast(node.clone())?;
    let op = bin.op()?;
    let (lhs, rhs) = (bin.lhs()?, bin.rhs()?);
    let (kept, literal) = match (bool_literal(&lhs), bool_literal(&rhs)) {
        (Some(_), Some(_)) | (None, None) => return None,
        (None, Some(literal)) => (lhs, literal),
        (Some(literal), None) => (rhs, literal),
    };
    let negated = match (op.syntax().kind(), literal) {
        (SyntaxKind::EQ_EQ, true) | (SyntaxKind::NOT_EQ, false) => false,
        (SyntaxKind::EQ_EQ, false) | (SyntaxKind::NOT_EQ, true) => true,
        _ => return None,
    };
    Some(Redundancy {
        kept,
        negated,
        form: Form::Comparison {
            literal: if literal { "true" } else { "false" },
        },
    })
}

/// A conditional whose two arms are the boolean literals, in either order.
///
/// Arms that agree (`c ? true : true`) are constant rather than redundant and
/// answer `None`, as does an incomplete ternary, whose missing arm the parser
/// leaves absent.
fn conditional(node: &SyntaxNode) -> Option<Redundancy> {
    let ternary = TernaryExpr::cast(node.clone())?;
    let then_branch = bool_literal(&ternary.then_branch()?)?;
    let else_branch = bool_literal(&ternary.else_branch()?)?;
    if then_branch == else_branch {
        return None;
    }
    Some(Redundancy {
        kept: ternary.condition()?,
        negated: !then_branch,
        form: Form::Conditional,
    })
}

/// `expr` as the boolean literal it is: `Some(true)` for `true`, `Some(false)`
/// for `false`, `None` for everything else — a number, a string, a name, and
/// `nothing` included.
fn bool_literal(expr: &Expr) -> Option<bool> {
    let Expr::Literal(literal) = expr else {
        return None;
    };
    Some(literal.bool_token()?.kind() == SyntaxKind::TRUE_KW)
}

/// `expr`'s source text under a `!`, parenthesized when a bare `!` would
/// rebind.
fn negated_text(expr: &Expr) -> String {
    let text = expr.syntax().text().to_string();
    if binds_at_least_as_tight_as_not(expr) {
        format!("!{text}")
    } else {
        format!("!({text})")
    }
}

/// Whether `expr` can take a prefix `!` without parentheses: it is delimited (a
/// literal, a name, anything bracketed), or a postfix chain — all of which bind
/// at least as tightly as the unary tier `!` sits in.
///
/// A `BINARY_EXPR` qualifies only for field access (`a.b`), the one infix
/// operator that binds tighter than a prefix `!`. Deciding the rest needs the
/// parser's precedence table, which is not exposed over the CST, so everything
/// else is parenthesized rather than guessed at — `!(a + b)` is an atom in any
/// position, and the parentheses cost nothing but a character.
fn binds_at_least_as_tight_as_not(expr: &Expr) -> bool {
    match expr {
        Expr::Literal(_)
        | Expr::StringLiteral(_)
        | Expr::CmdLiteral(_)
        | Expr::NonstandardIdentifier(_)
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
        | Expr::DotCallExpr(_) => true,
        Expr::BinaryExpr(bin) => bin
            .op()
            .is_some_and(|op| op.syntax().kind() == SyntaxKind::DOT),
        _ => false,
    }
}

/// Whether a `#` sits inside `whole` but outside `kept` — that is, in the text
/// the rewrite deletes. Only the operator and the boolean literals live there,
/// so a `#` is always a comment and dropping it would be lossy.
///
/// Both offsets are token boundaries within `whole`'s own text, so the slicing
/// is char-boundary safe.
fn drops_a_comment(whole: &SyntaxNode, kept: &Expr) -> bool {
    let outer = whole.text_range();
    let inner = kept.syntax().text_range();
    let text = whole.text().to_string();
    let head = usize::from(inner.start() - outer.start());
    let tail = usize::from(inner.end() - outer.start());
    text[..head].contains('#') || text[tail..].contains('#')
}
