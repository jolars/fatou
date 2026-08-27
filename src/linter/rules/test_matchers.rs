//! Shared matching for the `Test.@test` rule family.
//!
//! A spelling match is not enough: projects routinely define their own DSL
//! macros, and a nested module does not inherit its parent's imports. This
//! module therefore joins the typed macro shape to the semantic model's
//! file-local `using`/`import` entries and scope tree.

use crate::ast::{AstNode, AstToken, Expr, HasArgList, MacroCall};
use crate::linter::rules::RuleContext;
use crate::semantic::{BindingKind, LoadKind, ModuleLoad, ScopeId};
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
    let name = call.name()?;
    let parts: Vec<_> = name.ident_tokens().collect();
    let macro_token = parts.last()?;
    let at = call.syntax().text_range().start();
    let loads: Vec<_> = ctx
        .model
        .module_loads()
        .iter()
        .filter(|load| is_visible_test_load(ctx, load, at))
        .collect();

    match parts.as_slice() {
        [called] => {
            if !loads
                .iter()
                .any(|load| exposes_bare_macro(load, called.text()))
            {
                return None;
            }
            let ident =
                ctx.model.idents().iter().find(|ident| {
                    ident.is_macro && ident.range == macro_token.syntax().text_range()
                })?;
            if ident
                .binding
                .is_some_and(|id| ctx.model.binding(id).kind != BindingKind::Import)
            {
                return None;
            }
        }
        [qualifier, called] if called.text() == "test" => {
            if !loads
                .iter()
                .any(|load| binds_test_module(load, qualifier.text()))
            {
                return None;
            }
            let qualifier_read =
                ctx.model.idents().iter().find(|ident| {
                    !ident.is_macro && ident.range == qualifier.syntax().text_range()
                })?;
            if !qualifier_read
                .binding
                .is_some_and(|id| ctx.model.binding(id).kind == BindingKind::Import)
            {
                return None;
            }
        }
        _ => return None,
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

fn is_visible_test_load(ctx: &RuleContext<'_>, load: &ModuleLoad, at: rowan::TextSize) -> bool {
    load.path.leading_dots == 0
        && load.path.components.as_slice() == ["Test"]
        && load.range.end() <= at
        && scope_visible(ctx, load.scope, ctx.model.scope_at(at))
}

fn scope_visible(ctx: &RuleContext<'_>, declared: ScopeId, at: ScopeId) -> bool {
    let mut cursor = Some(at);
    while let Some(id) = cursor {
        if id == declared {
            return true;
        }
        let scope = ctx.model.scope(id);
        if scope.kind.is_global() {
            return false;
        }
        cursor = scope.parent;
    }
    false
}

fn exposes_bare_macro(load: &ModuleLoad, called: &str) -> bool {
    match &load.items {
        None => load.kind == LoadKind::Using && load.alias.is_none() && called == "test",
        Some(items) => items.iter().any(|item| {
            if item.name != "@test" {
                return false;
            }
            item.alias
                .as_deref()
                .unwrap_or("@test")
                .trim_start_matches('@')
                == called
        }),
    }
}

fn binds_test_module(load: &ModuleLoad, qualifier: &str) -> bool {
    if load.items.is_some() {
        return false;
    }
    load.alias.as_deref().unwrap_or("Test") == qualifier
}
