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
//! usage guard above to keep it honest. No fix is offered: `eachindex`/`axes` are
//! only equivalent for one-based, unit-step ranges, and the rewrite is a
//! judgment call, so the finding is reported without an edit.

use crate::ast::{
    AstNode, AstToken, BinaryExpr, CallExpr, Expr, ForBinding, HasArgList, IndexExpr, Name,
};
use crate::linter::diagnostic::Diagnostic;
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
         the collection. Second, `for i in 3.5`: iterating a bare numeric \
         literal runs the loop body once and is almost always a mistaken range. \
         No fix is offered, since the rewrites are not always equivalent."
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
        let Some((coll, func)) = one_based_length_range(&iterable) else {
            return;
        };
        if !indexes_collection(&binding, &coll, loop_var) {
            return;
        }

        let message = match func {
            LengthFn::Length => {
                format!("iterate `eachindex({coll})` instead of `1:length({coll})`")
            }
            LengthFn::Size => {
                format!("iterate `axes({coll}, d)` instead of `1:size({coll}, d)`")
            }
        };
        sink.push(Diagnostic::new(
            self.id(),
            iterable.syntax().text_range(),
            message,
        ));
    }
}

/// Which length-like builtin bounds the range.
enum LengthFn {
    Length,
    Size,
}

/// If `expr` is a one-based, unit-step range `1:length(c)` / `1:size(c, ...)`,
/// the collection name `c` and which builtin bounds it. A stepped range parses
/// as a `RANGE_EXPR`, not a `BINARY_EXPR`, so it never matches here.
fn one_based_length_range(expr: &Expr) -> Option<(String, LengthFn)> {
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
    Some((coll.text().to_string(), func))
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
