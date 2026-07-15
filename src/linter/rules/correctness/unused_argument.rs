//! `unused-argument`: a function parameter that is never read in its body.
//!
//! Driven by the semantic model, which records one parameter binding per
//! occurrence in the function's scope, so every signature form — long, short
//! (`f(x) = ...`), anonymous (`x -> ...`), and `do` — is covered uniformly, and
//! a read from a nested closure marks the binding as used (captures resolve to
//! the same binding).
//!
//! Two false-positive mitigations, after StaticLint's `UnusedFunctionArgument`:
//!
//! - **All-underscore names** (`_`, `__`, ...) are Julia's documented throwaway
//!   escape hatch, so they are never flagged.
//! - **Stub bodies** are exempt: a placeholder body that intentionally ignores
//!   its arguments to satisfy a signature. That is a single expression that is a
//!   literal (`f(x) = 0`), `nothing` (`f(x) = nothing`), or an
//!   `error(...)`/`throw(...)` call (`f(x) = error("not implemented")`) — the
//!   idiomatic shapes of an unimplemented or abstract method.
//!
//! Even so, methods that dispatch on an argument's type without reading its
//! value (`f(::Logger, msg) = ...` and friends) are a genuine and common source
//! of intentional-but-unread parameters that no local heuristic can tell apart
//! from a mistake. The rule is therefore **off by default** ([`default_enabled`]
//! returns `false`); users opt in with `--select unused-argument`.

use crate::ast::{AstNode, AstToken, CallExpr, Expr, Name};
use crate::linter::diagnostic::Diagnostic;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::semantic::BindingKind;
use crate::syntax::{SyntaxKind, SyntaxNode};

pub struct UnusedArgument;

impl Rule for UnusedArgument {
    fn id(&self) -> &'static str {
        "unused-argument"
    }

    /// Dispatch-only parameters make this noisy in idiomatic Julia, so it is
    /// opt-in rather than on by default.
    fn default_enabled(&self) -> bool {
        false
    }

    fn description(&self) -> &'static str {
        "Flag a function parameter that is never read in its body. Every \
         signature form is covered — long, short (`f(x) = ...`), anonymous, and \
         `do`. All-underscore names (`_`, `__`) follow Julia's throwaway \
         convention and are skipped, and stub methods whose body is a single \
         placeholder expression — a literal (`f(x) = 0`), `nothing`, or an \
         `error(...)`/`throw(...)` call — are exempt. Because methods that \
         dispatch on an argument's type without reading its value are common, \
         this rule is disabled by default; enable it with \
         `--select unused-argument`."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "`factor` is accepted but never used:",
            source: "function scale(x, factor)\n    2 * x\nend\n",
        }]
    }

    fn check_file(&self, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        for binding in ctx.model.bindings() {
            if binding.read {
                continue;
            }
            if !matches!(binding.kind, BindingKind::Param | BindingKind::KeywordParam) {
                continue;
            }
            if binding.name.chars().all(|c| c == '_') {
                continue;
            }
            if enclosing_body_is_stub(ctx.root, binding.def_range) {
                continue;
            }
            sink.push(Diagnostic::new(
                self.id(),
                binding.def_range,
                format!("function argument `{}` is never used", binding.name),
            ));
        }
    }
}

/// Whether the parameter defined at `def_range` belongs to a function whose
/// body is a single placeholder expression — the stub/interface-method shape
/// that intentionally accepts an argument it never reads.
fn enclosing_body_is_stub(root: &SyntaxNode, def_range: rowan::TextRange) -> bool {
    let node = match root.covering_element(def_range) {
        rowan::NodeOrToken::Node(node) => node,
        rowan::NodeOrToken::Token(token) => match token.parent() {
            Some(parent) => parent,
            None => return false,
        },
    };
    // The nearest enclosing function-like form owns this parameter (parameters
    // live in the signature, a direct child of that form).
    let Some(func) = node.ancestors().find(|n| {
        matches!(
            n.kind(),
            SyntaxKind::FUNCTION_DEF
                | SyntaxKind::ASSIGNMENT_EXPR
                | SyntaxKind::ARROW_EXPR
                | SyntaxKind::DO_EXPR
        )
    }) else {
        return false;
    };
    match sole_body_expr(&func) {
        Some(expr) => is_stub_expr(&expr),
        None => false,
    }
}

/// The single expression making up `func`'s body, or `None` if the body is
/// empty or has more than one statement.
fn sole_body_expr(func: &SyntaxNode) -> Option<SyntaxNode> {
    let mut body = match func.kind() {
        // The `end`-closed block holds the statements.
        SyntaxKind::FUNCTION_DEF | SyntaxKind::DO_EXPR => func
            .children()
            .find(|c| c.kind() == SyntaxKind::BLOCK)?
            .children(),
        // Short-form `lhs = rhs` and `params -> rhs`: the body is the rhs.
        SyntaxKind::ASSIGNMENT_EXPR | SyntaxKind::ARROW_EXPR => {
            let mut children = func.children();
            children.next(); // skip the signature / parameters
            children
        }
        _ => return None,
    };
    match (body.next(), body.next()) {
        (Some(only), None) => Some(only),
        _ => None,
    }
}

/// Whether `expr` is a placeholder body that intentionally ignores its
/// arguments: a bare literal, `nothing`, or an `error(...)`/`throw(...)` call.
fn is_stub_expr(expr: &SyntaxNode) -> bool {
    match expr.kind() {
        SyntaxKind::LITERAL | SyntaxKind::STRING_LITERAL | SyntaxKind::CMD_LITERAL => true,
        SyntaxKind::NAME => Name::cast(expr.clone()).is_some_and(|n| name_is(&n, &["nothing"])),
        SyntaxKind::CALL_EXPR => CallExpr::cast(expr.clone())
            .and_then(|call| call.callee())
            .is_some_and(
                |callee| matches!(callee, Expr::Name(n) if name_is(&n, &["error", "throw"])),
            ),
        _ => false,
    }
}

/// Whether `name`'s identifier is one of `names`.
fn name_is(name: &Name, names: &[&str]) -> bool {
    name.ident().is_some_and(|id| names.contains(&id.text()))
}
