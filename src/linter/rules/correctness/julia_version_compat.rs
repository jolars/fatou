//! Syntax compatibility with the floor of the declared runtime support range.
//!
//! The parser accepts a superset of Julia syntax. This rule restores the
//! compatibility signal for syntax introduced after the oldest supported
//! runtime, independently of the syntax edition used to parse the package.
//! Without a runtime target there is nothing to check.
//!
//! Julia 1.14 features follow Julia's release notes and the locked JuliaSyntax
//! grammar. Primitive widths are checked only for integer literals (possibly
//! parenthesized or prefixed with `+`); other expressions are never evaluated.
//! Macro checks inspect argument separators, not newlines within an argument.
//! Return checks distinguish inherited index markers and ternary separators
//! from contexts established inside the return value itself.

use rowan::{TextRange, WalkEvent};

use crate::ast::{
    AstNode, AstToken, BreakExpr, ContinueExpr, Expr, MacroCall, Operator, PrimitiveDef, ReturnExpr,
};
use crate::julia_version::Version;
use crate::linter::diagnostic::{Diagnostic, Severity};
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::syntax::{SyntaxElement, SyntaxKind};

const JULIA_1_14: Version = Version::new(1, 14, 0);

pub struct JuliaVersionCompat;

impl Rule for JuliaVersionCompat {
    fn id(&self) -> &'static str {
        "julia-version-compat"
    }

    fn default_severity(&self) -> Severity {
        Severity::Error
    }

    fn description(&self) -> &'static str {
        "Report syntax unavailable in a Julia version the project supports. `public` requires \
         Julia 1.11, and `import ... as` requires 1.6. Julia 1.14 adds `typegroup`, wrapping \
         arithmetic and assignment operators (including broadcasts), labeled `break` and \
         `continue`, primitive types with non-byte-multiple literal widths, parenthesized \
         multiline macro arguments, and `return` forms that inherit index markers or a \
         ternary branch's closing colon. Width expressions are not evaluated.\n\n\
         The runtime target comes from `--julia-version`, `[julia] version`, or the project's \
         `Project.toml` or `Manifest.toml`. The rule checks the oldest supported runtime, \
         independently of the selected syntax edition, and does nothing when no target is known."
    }

    fn examples(&self) -> &'static [Example] {
        &[
            Example {
                caption: "Targeting Julia 1.0, but `public` needs 1.11 and `as` needs 1.6:",
                source: "module M\npublic foo\nimport A as B\nend\n",
            },
            Example {
                caption: "These constructs require a Julia 1.14 runtime:",
                source: "typegroup\nstruct A end\nend\nx +%= 1\n@label outer for i in xs\n    continue outer\nend\nprimitive type Bits 7 end\n(@info \"message\"\n    x=1)\na[return end]\nx ? return : y\n",
            },
        ]
    }

    fn example_julia_target(&self) -> Option<crate::julia_version::VersionRange> {
        Some(crate::julia_version::VersionRange {
            min: Version::new(1, 0, 0),
            max: Some(Version::new(2, 0, 0)),
        })
    }

    fn interests(&self) -> &'static [SyntaxKind] {
        use SyntaxKind::*;
        &[
            PUBLIC_STMT,
            IMPORT_ALIAS,
            TYPEGROUP_DEF,
            PLUS_PERCENT,
            MINUS_PERCENT,
            STAR_PERCENT,
            PLUS_PERCENT_EQ,
            MINUS_PERCENT_EQ,
            STAR_PERCENT_EQ,
            DOT_PLUS_PERCENT,
            DOT_MINUS_PERCENT,
            DOT_STAR_PERCENT,
            DOT_PLUS_PERCENT_EQ,
            DOT_MINUS_PERCENT_EQ,
            DOT_STAR_PERCENT_EQ,
            BREAK_EXPR,
            CONTINUE_EXPR,
            PRIMITIVE_DEF,
            MACRO_CALL,
            RETURN_EXPR,
        ]
    }

    fn check(&self, el: &SyntaxElement, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let Some(target) = ctx.julia_target else {
            return;
        };
        let (introduced, label, range) = match el.kind() {
            SyntaxKind::PUBLIC_STMT => (
                Version::new(1, 11, 0),
                "the `public` keyword",
                el.text_range(),
            ),
            SyntaxKind::IMPORT_ALIAS => (
                Version::new(1, 6, 0),
                "renaming with `as` in `import`/`using`",
                el.text_range(),
            ),
            _ if target.covers_feature(JULIA_1_14) => return,
            _ => {
                let Some((label, range)) = julia_1_14_feature(el) else {
                    return;
                };
                (JULIA_1_14, label, range)
            }
        };
        if !target.covers_feature(introduced) {
            sink.push(Diagnostic::new(
                self.id(),
                range,
                format!(
                    "{label} requires Julia {introduced}, but the project supports {} and up",
                    target.min
                ),
            ));
        }
    }
}

fn julia_1_14_feature(el: &SyntaxElement) -> Option<(&'static str, TextRange)> {
    // All dispatched operator kinds are wrapping operators. Token dispatch also
    // catches unary uses, operator values, quoted operators, and suffixed names.
    if let Some(operator) = el.as_token().cloned().and_then(Operator::cast) {
        return Some(("wrapping arithmetic syntax", operator.syntax().text_range()));
    }
    let node = el.as_node()?.clone();
    match node.kind() {
        SyntaxKind::TYPEGROUP_DEF => Some(("`typegroup`", node.first_token()?.text_range())),
        SyntaxKind::BREAK_EXPR => {
            let label = BreakExpr::cast(node.clone())?.label()?;
            Some((
                "labeled `break`",
                TextRange::new(node.text_range().start(), label.syntax().text_range().end()),
            ))
        }
        SyntaxKind::CONTINUE_EXPR => {
            let label = ContinueExpr::cast(node.clone())?.label()?;
            Some((
                "labeled `continue`",
                TextRange::new(node.text_range().start(), label.syntax().text_range().end()),
            ))
        }
        SyntaxKind::PRIMITIVE_DEF => {
            let width = PrimitiveDef::cast(node)?.bit_width()?;
            (literal_width(&width)? % 8 != 0).then(|| {
                (
                    "a primitive type with a non-byte-multiple width",
                    width.syntax().text_range(),
                )
            })
        }
        SyntaxKind::MACRO_CALL => {
            let call = MacroCall::cast(node)?;
            uses_new_macro_newlines(&call).then(|| {
                (
                    "parenthesized multiline macro argument syntax",
                    call.name()
                        .expect("parsed macro has a name")
                        .syntax()
                        .text_range(),
                )
            })
        }
        SyntaxKind::RETURN_EXPR => {
            let ret = ReturnExpr::cast(node.clone())?;
            (inherits_index_markers(&ret) || inherits_ternary_colon(&ret)).then(|| {
                (
                    "`return` with inherited index or ternary context",
                    node.first_token()
                        .expect("parsed return has a keyword")
                        .text_range(),
                )
            })
        }
        _ => None,
    }
}

fn uses_new_macro_newlines(call: &MacroCall) -> bool {
    let mut arg_count = 0;
    let mut continued = false;
    for (_, newlines) in call.space_args_with_newlines() {
        if newlines != 0 {
            // Before 1.14, @doc could take its second argument on the next
            // line. A blank line or any other continued argument is new syntax.
            if arg_count != 1 || newlines != 1 {
                return true;
            }
            continued = true;
        }
        arg_count += 1;
    }
    continued
        && (arg_count != 2
            || !call
                .name()
                .and_then(|name| name.macro_token())
                .is_some_and(|token| token.text() == "doc"))
}

fn literal_width(expr: &Expr) -> Option<u128> {
    match expr {
        Expr::ParenExpr(paren) => literal_width(&paren.expr()?),
        Expr::UnaryExpr(unary) if unary.op()?.text() == "+" => literal_width(&unary.operand()?),
        Expr::Literal(literal) => {
            let token = literal.numeric_token()?;
            let radix = match token.kind() {
                SyntaxKind::INTEGER => 10,
                SyntaxKind::HEX_INT => 16,
                SyntaxKind::OCT_INT => 8,
                SyntaxKind::BIN_INT => 2,
                _ => return None,
            };
            let text = token.text();
            let digits = if radix == 10 { text } else { &text[2..] };
            u128::from_str_radix(&digits.replace('_', ""), radix).ok()
        }
        _ => None,
    }
}

fn inherits_index_markers(ret: &ReturnExpr) -> bool {
    let Some(value) = ret.value() else {
        return false;
    };
    let mut walk = value.syntax().preorder();
    while let Some(event) = walk.next() {
        let WalkEvent::Enter(node) = event else {
            continue;
        };
        match Expr::cast(node) {
            Some(Expr::BeginMarker(_) | Expr::EndMarker(_)) => return true,
            // A nested index introduces its own markers, even on old runtimes.
            // Nested returns own their compatibility findings.
            Some(Expr::IndexExpr(_) | Expr::ReturnExpr(_)) => walk.skip_subtree(),
            Some(Expr::Other(node))
                if matches!(
                    node.kind(),
                    SyntaxKind::TYPED_COMPREHENSION | SyntaxKind::TYPED_MATRIX_EXPR
                ) =>
            {
                walk.skip_subtree()
            }
            _ => {}
        }
    }
    false
}

fn inherits_ternary_colon(ret: &ReturnExpr) -> bool {
    let mut child = ret.syntax().clone();
    for parent in ret.syntax().ancestors().skip(1) {
        match Expr::cast(parent.clone()) {
            Some(Expr::TernaryExpr(ternary)) => {
                if ternary
                    .then_branch()
                    .is_some_and(|branch| branch.syntax() == &child)
                {
                    // A newline already terminated the return on old runtimes.
                    // A newline inside a block comment does not terminate it.
                    return !ternary.has_newline_before_colon();
                }
            }
            // Only unparenthesized operators carry the enclosing colon context.
            // Delimited expressions and blocks start a fresh context. A nested
            // return is reported by its enclosing return instead.
            Some(
                Expr::BinaryExpr(_)
                | Expr::UnaryExpr(_)
                | Expr::AssignmentExpr(_)
                | Expr::ArrowExpr(_)
                | Expr::SplatExpr(_)
                | Expr::TypeAnnotation(_)
                | Expr::WhereExpr(_),
            ) => {}
            Some(Expr::Other(node))
                if matches!(
                    node.kind(),
                    SyntaxKind::BARE_TUPLE_EXPR
                        | SyntaxKind::RANGE_EXPR
                        | SyntaxKind::COMPARISON_EXPR
                        | SyntaxKind::POSTFIX_EXPR
                        | SyntaxKind::JUXTAPOSE_EXPR
                ) => {}
            _ => return false,
        }
        child = parent;
    }
    false
}
