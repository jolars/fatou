//! `comparison-negation`: negating a parenthesized equality test, where Julia
//! has an operator that spells the negation directly.
//!
//! | negated form  | direct form  |
//! |---------------|--------------|
//! | `!(a == b)`   | `a != b`     |
//! | `!(a != b)`   | `a == b`     |
//! | `!(a === b)`  | `a !== b`    |
//! | `!(a !== b)`  | `a === b`    |
//!
//! The Unicode spellings `≠`, `≡`, and `≢` are the same four operators and are
//! rewritten alongside them, staying Unicode where Julia has a Unicode
//! negation (`≡` becomes `≢`) and falling back to ASCII where it does not
//! (`≠` becomes `==`, there being no Unicode `==`).
//!
//! **Only the equality family.** The rewrite is exact because Base *defines*
//! the negated operator as the negation: `!=(x, y) = !(x == y)` and
//! `!==(x, y) = !(x === y)`. The orderings do not have that guarantee and are
//! deliberately left alone — `<` and `>=` are independent methods, not
//! negations of each other, and they disagree on exactly the inputs a partial
//! order is partial about. `!(NaN < 1)` is `true` while `NaN >= 1` is `false`,
//! so rewriting one as the other is a behavior change, not an idiom. The same
//! reasoning excludes `in`: `∉` really is `!∈`, but the only negated spelling
//! is Unicode, and injecting a non-ASCII operator into a file is a bigger
//! decision than an idiom rewrite should make on its own.
//!
//! Both broadcast positions are out of scope. `a .== b` is a container of
//! comparisons rather than a test, and `.!` negates elementwise; each carries
//! its own operator kind, so neither reaches the rewrite. A comparison *chain*
//! (`a == b == c`) folds into a `COMPARISON_EXPR` rather than a `BINARY_EXPR`
//! and has no two-operand rewrite, so it never matches either — the same shape
//! rule [`LengthZero`](super::LengthZero) follows.
//!
//! **The fix is `Safe`,** and it is the negation's own source text with one
//! token swapped: the replacement is everything between the operands, operator
//! included, with the operator's bytes exchanged for its negation. Whatever
//! the operands contain — spacing, a comment between them, a nested call —
//! survives byte for byte, and `!(a==b)` stays tight (`a!=b`) rather than
//! acquiring spaces the author did not write.
//!
//! Two things withhold the fix, and the finding still stands for both.
//!
//! The first is a comment in the deleted text. Only `!`, `(`, and `)` sit
//! outside the operands, so a `#` there is always a comment and never a
//! string, and dropping it would be lossy. A comment *between* the operands
//! travels with the replacement and costs nothing.
//!
//! The second is a position where the replacement would rebind. This is the
//! rule's one real hazard: `!(...)` binds as tightly as a unary operator on a
//! parenthesized operand, while the comparison replacing it binds far more
//! loosely, so a neighbouring operator can capture an operand that used to be
//! sealed behind the parentheses. `x + !(a == b)` would become
//! `x + a != b`, which is `(x + a) != b`. Deciding this in general needs the
//! parser's precedence table, which the CST does not expose, so the fix is
//! offered only from an allow-list of positions that provably cannot rebind:
//! a statement, a delimited slot (a comma-separated argument or element, a
//! parenthesized expression, a comprehension's `if`), a condition, and the
//! operand of something looser than a comparison (`=`, `&&`, `||`, `?:`,
//! `->`, `return`). A macro argument qualifies only when nothing follows it,
//! since a later argument would sit right where the spliced comparison ends.

use crate::ast::{AstNode, AstToken, BinaryExpr, Expr, Operator, UnaryExpr};
use crate::linter::diagnostic::{Applicability, Diagnostic, Fix};
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::syntax::{SyntaxElement, SyntaxKind, SyntaxNode};

pub struct ComparisonNegation;

impl Rule for ComparisonNegation {
    fn id(&self) -> &'static str {
        "comparison-negation"
    }

    fn description(&self) -> &'static str {
        "Flag `!` applied to a parenthesized equality test, which Julia spells \
         with a single operator: `!(a == b)` is `a != b`, `!(a === b)` is \
         `a !== b`, and both read back the other way. The Unicode spellings \
         `≠`, `≡`, and `≢` collapse the same way.\n\n\
         The rewrite is exact rather than merely equivalent-in-practice: Base \
         defines `!=(x, y) = !(x == y)` and `!==(x, y) = !(x === y)`, so the \
         two spellings agree on every input by construction.\n\n\
         Only the equality family is reported. The orderings are left alone \
         because `<` and `>=` are independent methods rather than negations of \
         each other, and they disagree on exactly the inputs a partial order \
         is partial about: `!(NaN < 1)` is `true` while `NaN >= 1` is `false`. \
         The broadcast forms `a .== b` and `.!x` are containers of values \
         rather than tests, and a comparison chain (`a == b == c`) has no \
         two-operand rewrite, so none of them is flagged.\n\n\
         The rule reports a safe fix that reuses the comparison's own source \
         text with the operator swapped, so spacing and any comment between \
         the operands survive. The fix is withheld — the finding still stands \
         — when a comment sits in the deleted `!(` or `)`, and when the \
         negation sits somewhere a bare comparison would rebind, as in \
         `x + !(a == b)`."
    }

    fn examples(&self) -> &'static [Example] {
        &[
            Example {
                caption: "Negating an equality test spells the inequality:",
                source: "if !(status == :ok)\n    retry()\nend\n",
            },
            Example {
                caption: "The identity comparison negates the same way:",
                source: "found = !(lookup(key) === nothing)\n",
            },
        ]
    }

    fn interests(&self) -> &'static [SyntaxKind] {
        &[SyntaxKind::UNARY_EXPR]
    }

    fn check(&self, el: &SyntaxElement, _ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let Some(unary) = el.as_node().cloned().and_then(UnaryExpr::cast) else {
            return;
        };
        // `!` only. `.!` carries its own kind and negates elementwise, and
        // every other unary operator is a different question.
        if unary.op().map(|op| op.syntax().kind()) != Some(SyntaxKind::BANG) {
            return;
        }
        let Some(Expr::ParenExpr(paren)) = unary.operand() else {
            return;
        };
        let Some(Expr::BinaryExpr(bin)) = paren.expr() else {
            return;
        };
        let Some(op) = bin.op() else { return };
        let Some(negated) = negate(&op) else { return };

        let message = format!(
            "write the comparison directly: `!(a {op} b)` is `a {negated} b`",
            op = op.text()
        );
        let mut diag = Diagnostic::new(self.id(), unary.syntax().text_range(), message);
        if let Some(fix) = direct_rewrite(&unary, &bin, &op, negated) {
            diag.fixes.push(fix);
        }
        sink.push(diag);
    }
}

/// The operator that spells this comparison's negation, for the equality
/// family only.
///
/// Base defines each of these as the negation of its partner
/// (`!=(x, y) = !(x == y)`), which is what makes the rewrite exact. The
/// orderings have no such definition and answer `None`, as does every other
/// operator — including the broadcast `.==`, which carries a distinct kind.
///
/// The negation stays Unicode where Julia has one (`≡` negates to `≢`) and
/// falls back to ASCII where it does not: `≠` negates to `==`, there being no
/// Unicode spelling of `==`.
fn negate(op: &Operator) -> Option<&'static str> {
    Some(match op.syntax().kind() {
        SyntaxKind::EQ_EQ => "!=",
        SyntaxKind::NOT_EQ => "==",
        SyntaxKind::EQ_EQ_EQ => "!==",
        SyntaxKind::NOT_EQ_EQ => "===",
        SyntaxKind::UNICODE_OP => match op.text() {
            "≠" => "==",
            "≡" => "≢",
            "≢" => "≡",
            _ => return None,
        },
        _ => return None,
    })
}

/// The safe fix replacing the whole negation with the direct comparison.
///
/// The replacement is `bin`'s own text with the operator's bytes swapped, so
/// the operands and everything between them survive verbatim. Both offsets are
/// token boundaries within `bin`'s text, so the slicing is char-boundary safe.
fn direct_rewrite(
    unary: &UnaryExpr,
    bin: &BinaryExpr,
    op: &Operator,
    negated: &str,
) -> Option<Fix> {
    if !splices_without_rebinding(unary) || drops_a_comment(unary, bin) {
        return None;
    }
    let text = bin.syntax().text().to_string();
    let base = bin.syntax().text_range().start();
    let head = usize::from(op.syntax().text_range().start() - base);
    let tail = usize::from(op.syntax().text_range().end() - base);
    let range = unary.syntax().text_range();
    Some(Fix {
        description: format!("Rewrite as `{negated}`"),
        content: format!("{}{negated}{}", &text[..head], &text[tail..]),
        start: range.start().into(),
        end: range.end().into(),
        applicability: Applicability::Safe,
    })
}

/// Whether a `#` sits inside `unary` but outside `bin` — that is, in the `!(`
/// and `)` the rewrite deletes. Nothing else lives there, so a `#` is always a
/// comment, and dropping it would be lossy.
fn drops_a_comment(unary: &UnaryExpr, bin: &BinaryExpr) -> bool {
    let outer = unary.syntax().text_range();
    let inner = bin.syntax().text_range();
    let text = unary.syntax().text().to_string();
    let head = usize::from(inner.start() - outer.start());
    let tail = usize::from(inner.end() - outer.start());
    text[..head].contains('#') || text[tail..].contains('#')
}

/// Whether a bare comparison can take `unary`'s place without any neighbouring
/// operator capturing one of its operands.
///
/// `!(...)` binds like a unary operator on a parenthesized operand; the
/// comparison replacing it binds at the comparison tier, far more loosely. So
/// this is an allow-list of positions that provably seal the replacement in:
/// a statement, a delimited slot, a condition, and the operand of a construct
/// looser than a comparison. Everything else — an arithmetic operand, another
/// negation, a neighbouring comparison — answers `false`, and the fix is
/// withheld rather than guessed at.
fn splices_without_rebinding(unary: &UnaryExpr) -> bool {
    let Some(parent) = unary.syntax().parent() else {
        return false;
    };
    match parent.kind() {
        // Statement position, and the delimited slots: a closing bracket,
        // brace, or paren bounds the comparison on the right.
        SyntaxKind::ROOT
        | SyntaxKind::BLOCK
        | SyntaxKind::PAREN_EXPR
        | SyntaxKind::PAREN_BLOCK
        | SyntaxKind::COMPREHENSION_IF
        // An `if`/`while`/`elseif` test, which the following newline or `end`
        // bounds.
        | SyntaxKind::CONDITION
        // Constructs whose operator binds looser than a comparison, so the
        // replacement stays one operand of them.
        | SyntaxKind::ASSIGNMENT_EXPR
        | SyntaxKind::KEYWORD_ARG
        | SyntaxKind::ARROW_EXPR
        | SyntaxKind::TERNARY_EXPR
        | SyntaxKind::RETURN_EXPR => true,
        // `&&` and `||` are the only binary operators looser than a
        // comparison; everything else (arithmetic, `|>`, another comparison)
        // would capture an operand.
        SyntaxKind::BINARY_EXPR => matches!(
            binary_op(&parent),
            Some(SyntaxKind::AND_AND | SyntaxKind::OR_OR)
        ),
        // A comma-separated argument or element. A `MATRIX_EXPR` row uses the
        // same node but separates by whitespace, where `[a != b c]` would not
        // be two elements at all.
        SyntaxKind::ARG => matches!(
            parent.parent().map(|it| it.kind()),
            Some(SyntaxKind::ARG_LIST | SyntaxKind::VECT_EXPR | SyntaxKind::TUPLE_EXPR)
        ),
        // A macro argument, but only the last one: arguments are separated by
        // whitespace, so a following argument would begin exactly where the
        // spliced comparison ends.
        SyntaxKind::MACRO_CALL => parent
            .children()
            .last()
            .is_some_and(|last| last == *unary.syntax()),
        _ => false,
    }
}

/// The kind of `binary`'s operator token, which sits between its two operands.
fn binary_op(binary: &SyntaxNode) -> Option<SyntaxKind> {
    BinaryExpr::cast(binary.clone())?
        .op()
        .map(|op| op.syntax().kind())
}
