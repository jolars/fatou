//! `kwarg-default-mismatch`: a keyword parameter whose literal default cannot
//! be an instance of its declared type.
//!
//! A keyword's `::T` is **not** an implicit `convert`. Julia lowers
//! `g(; y::Int = 1.0)` into an inner method whose signature carries `y::Int`,
//! and the outer `g()` calls it with `1.0` — so the default is only ever
//! reachable through dispatch, and `g()` raises
//! `MethodError(var"#g#1", (1.0, g))` every time. The code parses, the method
//! exists, and calling it the way its own signature advertises cannot work:
//! `Error`, like the other lowering-time failures in this directory.
//!
//! The check is exact, not approximate, because dispatch is: `y::T` accepts the
//! default only when `typeof(default) <: T`. That makes it worth flagging
//! `y::Float64 = 1` and `y::Int8 = 1` too — no promotion happens here — but it
//! also means the rule must know both types *precisely*, so it fires only when
//! three things line up:
//!
//! 1. The parameter is a keyword one — written after the `;` of a
//!    *definition*'s parameter list. A call site's `g(; y::Int = 1.0)` passes an
//!    annotated expression as a keyword value and declares nothing.
//! 2. The annotation is a bare name from [`CONCRETE_CORE_TYPES`], the concrete
//!    `Core` types whose instances the literal grammar can spell, *and* it
//!    resolves to Base/Core rather than to a local shadow or an import (see
//!    [`RuleContext::read_resolves_to_base`]). An abstract type (`Real`,
//!    `Integer`, `Any`), a parametric one (`Vector{Int}`), a `Union`, a
//!    type variable, and a qualified name (`Core.Int`) are all left alone.
//! 3. The default is a literal whose type is pinned down by its own spelling
//!    (see [`LiteralType::of`]).
//!
//! Every gap between those is a deliberate false negative. Two are worth
//! naming. A hexadecimal, octal, or binary integer literal takes its width —
//! and so its type — from its digit count (`0x01` is a `UInt8`, `0x0001` a
//! `UInt16`), which is more arithmetic than the finding is worth; and a plain
//! decimal literal is the *machine* `Int`, which is `Int32` on a 32-bit
//! platform, so `y::Int32 = 1` is accepted rather than risk a finding that is
//! wrong on the target the code actually runs on.
//!
//! No fix: the source does not say which half the author meant. Widening
//! `::Int` to `::Real` and rounding `1.0` to `1` are different programs, and
//! nothing in the text picks between them.
//!
//! The rule is one `PARAMETERS` visit plus one resolver question per candidate
//! annotation — the whole `TypeExpr` lowering in `crate::index::typeexpr` would
//! buy nothing here, since the only annotation shape that survives gate 2 is
//! the bare name whose *token* the resolution gate needs anyway.

use crate::ast::{Arg, AstNode, AstToken, Expr, Parameters, TypeAnnotation};
use crate::linter::diagnostic::{Diagnostic, Severity};
use crate::linter::rules::matchers;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::syntax::{SyntaxElement, SyntaxKind, SyntaxNode};

pub struct KwargDefaultMismatch;

/// The concrete `Core` types this rule reasons about: every one whose instances
/// the literal grammar can spell directly. A type outside this set — abstract,
/// parametric, user-defined, or simply unlisted — is never the subject of a
/// finding.
const CONCRETE_CORE_TYPES: &[&str] = &[
    "Int", "Int8", "Int16", "Int32", "Int64", "Int128", "UInt", "UInt8", "UInt16", "UInt32",
    "UInt64", "UInt128", "Float16", "Float32", "Float64", "Bool", "Char", "String", "Symbol",
];

/// The runtime type of a default whose spelling pins it down.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LiteralType {
    /// A plain decimal integer that fits in an `Int64`: the machine `Int`.
    Int,
    Float64,
    Float32,
    Bool,
    Char,
    String,
    Symbol,
}

impl LiteralType {
    /// The type of `expr`, when its spelling alone determines it. `None` for
    /// anything computed, and for the literals whose type depends on more than
    /// their kind (see the module docs).
    fn of(expr: &Expr) -> Option<Self> {
        match expr {
            Expr::Literal(literal) => Self::of_literal(literal.syntax()),
            // A non-standard literal (`r"a"`) is whatever its string macro
            // returns, and an interpolated one is built at run time.
            Expr::StringLiteral(string) => {
                let plain = string.prefix().is_none()
                    && string.suffix().is_none()
                    && string.interpolations().next().is_none();
                plain.then_some(Self::String)
            }
            // Only the bare-name form is a `Symbol`: `:(x + 1)` is an `Expr`,
            // and a quoted operator or keyword (`:+`, `:if`) carries no node to
            // check.
            Expr::QuoteSym(sym) => matches!(sym.expr()?, Expr::Name(_)).then_some(Self::Symbol),
            _ => None,
        }
    }

    /// The type of a `LITERAL` node, read off its single value token. A leading
    /// sign is folded into the node and does not change the type; anything
    /// *else* riding along (the `im` of `1im`, which makes a `Complex`) means
    /// the token does not speak for the whole literal.
    fn of_literal(node: &SyntaxNode) -> Option<Self> {
        let mut tokens = node
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .filter(|token| !matches!(token.kind(), SyntaxKind::WHITESPACE | SyntaxKind::COMMENT));
        let first = tokens.next()?;
        let value = match first.kind() {
            SyntaxKind::MINUS | SyntaxKind::PLUS => tokens.next()?,
            _ => first.clone(),
        };
        if tokens.next().is_some() {
            return None;
        }
        match value.kind() {
            SyntaxKind::INTEGER => {
                fits_in_int64(value.text(), first.kind() == SyntaxKind::MINUS).then_some(Self::Int)
            }
            SyntaxKind::FLOAT => Some(Self::Float64),
            SyntaxKind::FLOAT32 => Some(Self::Float32),
            SyntaxKind::TRUE_KW | SyntaxKind::FALSE_KW => Some(Self::Bool),
            SyntaxKind::CHAR => Some(Self::Char),
            // A hexadecimal, octal, or binary literal's type follows its digit
            // count, which the rule does not compute.
            _ => None,
        }
    }

    /// The type's Julia name, for the finding's message.
    fn name(self) -> &'static str {
        match self {
            Self::Int => "Int",
            Self::Float64 => "Float64",
            Self::Float32 => "Float32",
            Self::Bool => "Bool",
            Self::Char => "Char",
            Self::String => "String",
            Self::Symbol => "Symbol",
        }
    }

    /// Whether a default of this type is an instance of the concrete type
    /// `annotation` names. Deliberately one-sided: `false` is only ever
    /// returned for a pair the rule is sure about.
    fn satisfies(self, annotation: &str) -> bool {
        match self {
            // `Int` is an alias for the platform's machine integer, so both
            // widths are accepted rather than risk a finding that holds on one
            // platform only.
            Self::Int => matches!(annotation, "Int" | "Int64" | "Int32"),
            other => annotation == other.name(),
        }
    }
}

/// Whether the decimal integer literal `text` (underscores and all) is small
/// enough that Julia gives it an `Int64` rather than widening to `Int128` or
/// `BigInt`. `negated` admits the extra value at the bottom of the range.
fn fits_in_int64(text: &str, negated: bool) -> bool {
    let digits: String = text.chars().filter(|c| *c != '_').collect();
    let Ok(magnitude) = digits.parse::<u128>() else {
        return false;
    };
    let limit = if negated {
        1u128 << 63
    } else {
        (1u128 << 63) - 1
    };
    magnitude <= limit
}

impl Rule for KwargDefaultMismatch {
    fn id(&self) -> &'static str {
        "kwarg-default-mismatch"
    }

    /// The default can never satisfy the constraint it is lowered against, so
    /// calling the method the way its signature advertises raises a
    /// `MethodError`.
    fn default_severity(&self) -> Severity {
        Severity::Error
    }

    fn description(&self) -> &'static str {
        "Flag a keyword parameter whose literal default cannot be an instance \
         of its declared type, as in `g(; y::Int = 1.0)`. A keyword's `::T` is \
         not an implicit `convert`: Julia lowers it into a dispatch constraint \
         on the inner method the default is passed to, so `g()` raises a \
         `MethodError` every time. The check is exact, like dispatch itself — \
         `y::Float64 = 1` and `y::Int8 = 1` are mismatches too — and fires only \
         when both sides are certain: a bare, concrete `Core` type that \
         resolves to Base, and a default whose own spelling pins its type down. \
         Abstract and parametric annotations (`Real`, `Vector{Int}`), computed \
         defaults, and the literals whose type follows their digit count \
         (`0x01`) are all left alone."
    }

    fn examples(&self) -> &'static [Example] {
        &[
            Example {
                caption: "`y` is declared `Int`, so the `Float64` default never matches it:",
                source: "function scale(xs; y::Int = 1.0)\n    xs .* y\nend\n",
            },
            Example {
                caption: "Julia does not promote here either — the default has to be \
                          an `Int8` already:",
                source: "counter(; start::Int8 = 0) = start\n",
            },
        ]
    }

    /// Every keyword parameter list is a `PARAMETERS` node — the `;`-block of
    /// an argument list.
    fn interests(&self) -> &'static [SyntaxKind] {
        &[SyntaxKind::PARAMETERS]
    }

    fn check(&self, el: &SyntaxElement, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let Some(node) = el.as_node() else {
            return;
        };
        let Some(params) = Parameters::cast(node.clone()) else {
            return;
        };
        if !declares_keyword_parameters(node) {
            return;
        }

        for arg in params.args() {
            check_parameter(self.id(), &arg, ctx, sink);
        }
    }
}

/// Whether this `;`-block belongs to a *definition*'s parameter list rather
/// than a call's argument list. Only the former lowers its keywords into a
/// method signature; `g(; y::Int = 1.0)` at a call site passes an annotated
/// expression as a value.
fn declares_keyword_parameters(params: &SyntaxNode) -> bool {
    params
        .parent()
        .filter(|parent| parent.kind() == SyntaxKind::ARG_LIST)
        .and_then(|arg_list| arg_list.parent())
        .filter(|call| call.kind() == SyntaxKind::CALL_EXPR)
        .is_some_and(|call| matchers::in_signature_position(&call))
}

/// Report `arg` when it is an annotated keyword parameter whose literal default
/// cannot be an instance of the declared type.
fn check_parameter(id: &'static str, arg: &Arg, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
    // An annotated keyword parameter is `ARG > ASSIGNMENT_EXPR` with a
    // `TYPE_ANNOTATION` on the left; a plain `y = 1` is a `KEYWORD_ARG`
    // instead, and a splat is neither.
    let Some(Expr::AssignmentExpr(assignment)) = arg.expr() else {
        return;
    };
    if assignment
        .op()
        .is_none_or(|op| op.syntax().kind() != SyntaxKind::EQ)
    {
        return;
    }
    let Some(Expr::TypeAnnotation(annotation)) = assignment.lhs() else {
        return;
    };
    let Some(name) = annotation
        .pattern()
        .and_then(|pattern| pattern.name_ident())
    else {
        return;
    };
    let Some(declared) = concrete_core_type(&annotation, ctx) else {
        return;
    };
    let Some(default) = assignment.rhs() else {
        return;
    };
    let Some(actual) = LiteralType::of(&default) else {
        return;
    };
    if actual.satisfies(declared) {
        return;
    }

    let literal = default.syntax().text();
    let mut diagnostic = Diagnostic::new(
        id,
        assignment.syntax().text_range(),
        format!(
            "keyword argument `{}` is declared `::{declared}`, but its default `{literal}` has \
             type `{}`",
            name.text(),
            actual.name(),
        ),
    );
    diagnostic.message = diagnostic.message.with_suggestion(
        "a keyword's `::T` is a dispatch constraint, not a `convert`, so the default raises a \
         `MethodError`",
    );
    sink.push(diagnostic);
}

/// The concrete `Core` type `annotation` declares, when it is a bare name from
/// [`CONCRETE_CORE_TYPES`] *confirmed* to be Base/Core's (see
/// [`RuleContext::read_resolves_to_base`], which also declines a name inside a
/// macro call or quoted code).
fn concrete_core_type(annotation: &TypeAnnotation, ctx: &RuleContext<'_>) -> Option<&'static str> {
    let Some(Expr::Name(ty)) = annotation.ty() else {
        return None;
    };
    let ident = ty.ident()?;
    let known = CONCRETE_CORE_TYPES
        .iter()
        .find(|name| **name == ident.text())?;
    ctx.read_resolves_to_base(ident.syntax()).then_some(*known)
}
