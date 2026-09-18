//! Shared matching for the `Test.@test` rule family.
//!
//! A spelling match is not enough: projects routinely define their own DSL
//! macros, and a nested module does not inherit its parent's imports. This
//! module therefore joins the typed macro shape to the semantic model's
//! file-local `using`/`import` entries and scope tree.

use crate::ast::{AstNode, AstToken, Expr, HasArgList, MacroCall};
use crate::linter::rules::RuleContext;
use crate::syntax::SyntaxNode;

/// A real `Test.@test` invocation and its single primary expression.
pub(crate) struct TestInvocation {
    pub(crate) expression: Expr,
}

/// Match `call` as an invocation of Test's `@test` macro.
///
/// Both macro spellings (`Test.@test` and `@Test.test`), imported aliases,
/// parenthesized arguments, and trailing macro keyword assignments qualify.
/// The Test load must precede the call and be visible from its lexical scope.
pub(crate) fn test_invocation(call: &MacroCall, ctx: &RuleContext<'_>) -> Option<TestInvocation> {
    let root = call.name()?.ident_tokens().next()?;
    // Quoted calls have no semantic occurrence, and their declarations do not
    // participate in resolution. Only evaluated calls can use this contract.
    let scope = ctx
        .model
        .idents()
        .iter()
        .find(|ident| ident.range == root.syntax().text_range())?
        .scope;
    if !ctx.model.is_test_macro(call, scope, "test") {
        return None;
    }

    Some(TestInvocation {
        expression: primary_expression(call)?,
    })
}

/// Whether `node` is exactly the primary expression of a real Test invocation.
pub(crate) fn is_direct_test_expression(node: &SyntaxNode, ctx: &RuleContext<'_>) -> bool {
    node.ancestors().skip(1).any(|ancestor| {
        MacroCall::cast(ancestor)
            .and_then(|call| test_invocation(&call, ctx))
            .is_some_and(|invocation| {
                invocation.expression.syntax().text_range() == node.text_range()
            })
    })
}

fn primary_expression(call: &MacroCall) -> Option<Expr> {
    if let Some(args) = call.arg_list() {
        let mut positional = args.args().filter_map(|arg| arg.expr());
        let expression = positional.next()?;
        return positional.next().is_none().then_some(expression);
    }

    let mut children = call.syntax().children().filter_map(Expr::cast);
    let expression = children.next()?;
    children
        .all(|arg| matches!(arg, Expr::AssignmentExpr(_)))
        .then_some(expression)
}
