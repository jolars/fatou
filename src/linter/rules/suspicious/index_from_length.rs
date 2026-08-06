//! `index-from-length`: two shapes of a suspect `for`-loop iteration spec.
//!
//! - **`for i in 1:length(x)` (or `1:size(x, d)`) where `i` indexes `x`.** The
//!   idiomatic Julia is `for i in eachindex(x)` (or `axes(x, d)`), which is
//!   correct for arbitrary index bases and keeps the bound in sync with the
//!   collection. The rule only fires when the loop variable is actually used to
//!   index the same collection, so a bare counter (`for i in 1:length(x)`
//!   without an `x[i]`) is left alone.
//!
//! - **`for i in 3.5` — iterating a bare numeric literal.** A number is iterable
//!   in Julia (it yields itself once), so this parses and runs but almost always
//!   means a range was intended.
//!
//! The match is purely name-based: any call to `length`/`size` counts, with no
//! attempt to resolve the callee. Where StaticLint exempts collections it can
//! prove are `Vector`/`Array` (for which `1:length` is fine), we have no type
//! information, so this rule is **opinionated** and reports those too — hence the
//! usage guard above to keep it honest.
//!
//! The first shape carries an **unsafe** fix rewriting the `1:length`/`1:size`
//! prefix to `eachindex`/`axes`, leaving the argument list untouched. The edit
//! is value-equivalent whenever the collection's indices are one-based and
//! dense — every case where the original loop was correct on an `Array` — but
//! without type information that cannot be proven: `eachindex` falls back to
//! `keys` for a dictionary (an integer-keyed `Dict` loses its 1..n iteration
//! order), a shadowed `length`/`size` breaks the equivalence, and for an offset
//! array the rewrite deliberately changes which indices run. Hence
//! `--unsafe-fixes`. The fix is withheld when the call is not the plain Base
//! arity (`length(x)`/`size(x, d)` exactly, no keywords or splats) or when a
//! comment sits inside the replaced prefix; the numeric-literal shape never has
//! a fix, since the intended range is unknowable.

use crate::ast::{
    AstNode, AstToken, BinaryExpr, CallExpr, Expr, ForBinding, HasArgList, IndexExpr, Name,
};
use crate::linter::diagnostic::{Applicability, Diagnostic, Fix};
use crate::linter::rules::matchers::CallShape;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::syntax::{SyntaxElement, SyntaxKind};

pub struct IndexFromLength;

impl Rule for IndexFromLength {
    fn id(&self) -> &'static str {
        "index-from-length"
    }

    fn description(&self) -> &'static str {
        "Flag two suspect `for`-loop iteration specs. First, `for i in \
         1:length(x)` (or `1:size(x, d)`) where the loop variable then indexes \
         `x`: prefer `eachindex(x)` (or `axes(x, d)`), which stays correct for \
         collections whose indices are not one-based. The match is name-based — \
         any `length`/`size` call counts — and, lacking type information to \
         exempt collections that really are one-based like `Vector`, the rule is \
         opinionated, so it only fires when the loop variable actually indexes \
         the collection. The first shape carries an unsafe fix rewriting the \
         `1:length`/`1:size` prefix to `eachindex`/`axes`: the rewrite is only \
         value-equivalent when the collection's indices are one-based and \
         dense, which cannot be proven without type information, so it needs \
         `--unsafe-fixes`. Second, `for i in 3.5`: iterating a bare numeric \
         literal runs the loop body once and is almost always a mistaken \
         range; no fix, since the intended range is unknowable."
    }

    fn examples(&self) -> &'static [Example] {
        &[
            Example {
                caption: "`1:length(x)` used to index `x`:",
                source: "for i in 1:length(x)\n    println(x[i])\nend\n",
            },
            Example {
                caption: "Iterating a bare number loops once:",
                source: "for i in 3.5\n    println(i)\nend\n",
            },
        ]
    }

    fn interests(&self) -> &'static [SyntaxKind] {
        &[SyntaxKind::FOR_BINDING]
    }

    fn check(&self, el: &SyntaxElement, _ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let Some(binding) = el.as_node().cloned().and_then(ForBinding::cast) else {
            return;
        };
        let Some(iterable) = binding.iterable() else {
            return;
        };

        // Iterating a bare numeric literal (`for i in 3.5`): loops once.
        if let Expr::Literal(lit) = &iterable
            && lit.numeric_token().is_some()
        {
            sink.push(Diagnostic::new(
                self.id(),
                iterable.syntax().text_range(),
                "iterating a numeric literal runs the loop body once; did you mean a range?"
                    .to_string(),
            ));
            return;
        }

        // `1:length(x)` / `1:size(x, d)` where the loop variable indexes `x`.
        let Some(loop_var) = binding.pattern().and_then(|pat| pat.name_ident()) else {
            return;
        };
        let loop_var = loop_var.text();
        let Some((call, coll, func)) = one_based_length_range(&iterable) else {
            return;
        };
        if !indexes_collection(&binding, &coll, loop_var) {
            return;
        }

        // A plain call at Base's arity is the only shape the fix (and the
        // message's concrete dimension) may trust.
        let shape = CallShape::of(&call);
        let plain = shape.is_plain(func.arity());
        let message = match func {
            LengthFn::Length => {
                format!("iterate `eachindex({coll})` instead of `1:length({coll})`")
            }
            LengthFn::Size => match plain.then(|| dimension_text(&shape)).flatten() {
                Some(dim) => {
                    format!("iterate `axes({coll}, {dim})` instead of `1:size({coll}, {dim})`")
                }
                None => format!("iterate `axes({coll}, d)` instead of `1:size({coll}, d)`"),
            },
        };
        let mut diag = Diagnostic::new(self.id(), iterable.syntax().text_range(), message);
        if plain && let Some(fix) = prefix_rewrite(&iterable, &call, func) {
            diag.fixes.push(fix);
        }
        sink.push(diag);
    }
}

/// Which length-like builtin bounds the range.
#[derive(Clone, Copy)]
enum LengthFn {
    Length,
    Size,
}

impl LengthFn {
    /// The builtin's Base arity: `length(x)` / `size(x, d)`.
    fn arity(self) -> usize {
        match self {
            LengthFn::Length => 1,
            LengthFn::Size => 2,
        }
    }

    /// The iteration builtin the fix rewrites to.
    fn replacement(self) -> &'static str {
        match self {
            LengthFn::Length => "eachindex",
            LengthFn::Size => "axes",
        }
    }

    fn name(self) -> &'static str {
        match self {
            LengthFn::Length => "length",
            LengthFn::Size => "size",
        }
    }
}

/// The `size` call's dimension argument as source text, if it fits on one line
/// (a multi-line expression would mangle the one-line message).
fn dimension_text(shape: &CallShape) -> Option<String> {
    let dim = shape.positional_exprs()?.into_iter().nth(1)?;
    let text = dim.syntax().text().to_string();
    (!text.contains('\n')).then_some(text)
}

/// The unsafe fix rewriting the `1:length`/`1:size` prefix to
/// `eachindex`/`axes`. The edit stops at the argument list, which stays
/// byte-for-byte intact; it is withheld if a comment sits inside the replaced
/// prefix (`1:#= dim =#length(x)`), which the rewrite would drop.
fn prefix_rewrite(iterable: &Expr, call: &CallExpr, func: LengthFn) -> Option<Fix> {
    let args = call.arg_list()?;
    let start = iterable.syntax().text_range().start();
    let end = args.syntax().text_range().start();
    let prefix = iterable
        .syntax()
        .text()
        .slice(rowan::TextRange::new(0.into(), end - start))
        .to_string();
    if prefix.contains('#') {
        return None;
    }
    Some(Fix {
        description: format!("Replace `1:{}` with `{}`", func.name(), func.replacement()),
        content: func.replacement().to_string(),
        start: start.into(),
        end: end.into(),
        applicability: Applicability::Unsafe,
    })
}

/// If `expr` is a one-based, unit-step range `1:length(c)` / `1:size(c, ...)`,
/// the bounding call, the collection name `c`, and which builtin bounds it. A
/// stepped range parses as a `RANGE_EXPR`, not a `BINARY_EXPR`, so it never
/// matches here.
fn one_based_length_range(expr: &Expr) -> Option<(CallExpr, String, LengthFn)> {
    let Expr::BinaryExpr(range) = expr else {
        return None;
    };
    let range: &BinaryExpr = range;
    if range.op()?.syntax().kind() != SyntaxKind::COLON {
        return None;
    }
    // Lower bound must be the literal `1`.
    match range.lhs()? {
        Expr::Literal(lit) if lit.numeric_token().is_some_and(|t| t.text() == "1") => {}
        _ => return None,
    }
    let Expr::CallExpr(call) = range.rhs()? else {
        return None;
    };
    let func = length_fn(&call)?;
    let coll = call.arg_list()?.args().next()?.expr()?.name_ident()?;
    let coll = coll.text().to_string();
    Some((call, coll, func))
}

/// The length-like builtin a call names, matched purely on the callee's simple
/// name (`length`/`size`); a qualified callee (`Base.length`) does not match.
fn length_fn(call: &CallExpr) -> Option<LengthFn> {
    match call.callee_ident()?.text() {
        "length" => Some(LengthFn::Length),
        "size" => Some(LengthFn::Size),
        _ => None,
    }
}

/// Whether, anywhere in the loop this binding drives, `coll` is indexed with an
/// expression that mentions `loop_var` (`x[i]`, `x[i + 1]`, ...).
fn indexes_collection(binding: &ForBinding, coll: &str, loop_var: &str) -> bool {
    let Some(scope) = binding.syntax().parent() else {
        return false;
    };
    scope
        .descendants()
        .filter_map(IndexExpr::cast)
        .filter(|idx| {
            idx.base()
                .and_then(|base| base.name_ident())
                .is_some_and(|name| name.text() == coll)
        })
        .any(|idx| {
            idx.arg_list().is_some_and(|args| {
                args.syntax()
                    .descendants()
                    .filter_map(Name::cast)
                    .filter_map(|n| n.ident())
                    .any(|n| n.text() == loop_var)
            })
        })
}
