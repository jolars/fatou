//! `test-isa-call`: Test assertions using the function-call spelling
//! `@test isa(value, Type)` instead of Julia's comparison spelling
//! `@test value isa Type`.
//!
//! The two expressions lower to the same call—even when `isa` is shadowed—so
//! the rewrite preserves behavior. The shared Test matcher ensures this is a
//! real, in-scope stdlib assertion rather than an unrelated macro named
//! `@test`.

use crate::ast::{AstNode, AstToken, CallExpr, Expr, MacroCall};
use crate::linter::diagnostic::{Applicability, Diagnostic, Fix};
use crate::linter::rules::matchers;
use crate::linter::rules::test_matchers::test_invocation;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::syntax::{SyntaxElement, SyntaxKind};

pub struct TestIsaCall;

impl Rule for TestIsaCall {
    fn id(&self) -> &'static str {
        "test-isa-call"
    }

    fn description(&self) -> &'static str {
        "Flag `@test isa(value, Type)` and prefer `@test value isa Type`. The \
         rule only matches the real `Test.@test` macro after a visible, \
         file-local Test load. It reports a safe fix that reuses both \
         arguments' source text and adds parentheses when an operand would \
         otherwise rebind. The fix is withheld when it would discard a \
         comment outside the retained arguments."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "Use Julia's comparison spelling in a Test assertion:",
            source: "using Test\n@test isa(result, AbstractVector)\n",
        }]
    }

    fn interests(&self) -> &'static [SyntaxKind] {
        &[SyntaxKind::MACRO_CALL]
    }

    fn check(&self, el: &SyntaxElement, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let Some(macro_call) = el.as_node().cloned().and_then(MacroCall::cast) else {
            return;
        };
        let Some(invocation) = test_invocation(&macro_call, ctx) else {
            return;
        };
        let Expr::CallExpr(isa_call) = invocation.expression else {
            return;
        };
        let Some((_call, args)) = matchers::plain_call(isa_call.syntax(), "isa", 2) else {
            return;
        };
        let [value, ty]: [Expr; 2] = args.try_into().expect("plain arity checked");
        let replacement = format!("{} isa {}", operand_text(&value), operand_text(&ty));
        let range = isa_call.syntax().text_range();
        let mut diag = Diagnostic::new(
            self.id(),
            range,
            format!("write `{replacement}` instead of calling `isa`"),
        );
        if !drops_comment_outside_arguments(&isa_call, [&value, &ty]) {
            diag.fixes.push(Fix {
                description: "Rewrite as an `isa` comparison".to_string(),
                content: replacement,
                start: range.start().into(),
                end: range.end().into(),
                applicability: Applicability::Safe,
            });
        }
        sink.push(diag);
    }
}

fn operand_text(expr: &Expr) -> String {
    let text = expr.syntax().text().to_string();
    if binds_at_least_as_tightly_as_isa(expr) {
        text
    } else {
        format!("({text})")
    }
}

fn binds_at_least_as_tightly_as_isa(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Literal(_)
            | Expr::StringLiteral(_)
            | Expr::CmdLiteral(_)
            | Expr::NonstandardIdentifier(_)
            | Expr::Name(_)
            | Expr::UnaryExpr(_)
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
    ) || matches!(expr, Expr::BinaryExpr(bin) if bin.op().is_some_and(|op| op.text() == "."))
}

fn drops_comment_outside_arguments(call: &CallExpr, args: [&Expr; 2]) -> bool {
    let outer = call.syntax().text_range();
    let text = call.syntax().text().to_string();
    let mut stripped = text;
    for arg in args.iter().rev() {
        let range = arg.syntax().text_range();
        let start = usize::from(range.start() - outer.start());
        let end = usize::from(range.end() - outer.start());
        stripped.replace_range(start..end, "");
    }
    stripped.contains('#')
}
