//! Recognize the `@eval` definitions whose bindings the ordinary model captures.

use crate::ast::{AstNode, AstToken, Expr, HasArgList, MacroCall};
use crate::syntax::SyntaxKind;

use super::SemanticModel;
use super::signature::peel_signature;

impl SemanticModel {
    pub(crate) fn is_static_eval_definition(&self, call: &MacroCall) -> bool {
        let Some(name) = call.name() else {
            return false;
        };
        let scope = self.scope_at(call.syntax().text_range().start());
        if !self.scope(scope).kind.is_global() {
            // `@eval` in a local scope defines globals, unlike the model's
            // ordinary traversal of macro arguments.
            return false;
        }
        let parts: Vec<_> = name.ident_tokens().collect();
        let builtin = match parts.as_slice() {
            [name] if name.text() == "eval" => self.resolve_name("eval", scope, true).is_none(),
            [owner, name] if owner.text() == "Base" && name.text() == "eval" => {
                self.resolve_name("Base", scope, false).is_none()
            }
            _ => false,
        };
        if !builtin {
            return false;
        }
        for parent in call.syntax().ancestors().skip(1) {
            match parent.kind() {
                SyntaxKind::ROOT | SyntaxKind::MODULE_DEF => break,
                SyntaxKind::BLOCK => {}
                // Definitions nested in other expressions are not necessarily
                // published by both the semantic model and the package index.
                _ => return false,
            }
        }
        if call.syntax().descendants().skip(1).any(|node| {
            matches!(
                node.kind(),
                SyntaxKind::INTERPOLATION | SyntaxKind::MACRO_CALL
            )
        }) {
            // Interpolation executes while constructing the expression, and
            // an unknown macro can change which definitions it introduces.
            return false;
        }
        let args: Vec<_> = if let Some(list) = call.arg_list() {
            list.args().filter_map(|arg| arg.expr()).collect()
        } else {
            call.syntax().children().filter_map(Expr::cast).collect()
        };
        let [definition] = args.as_slice() else {
            return false;
        };
        let signature = match definition {
            Expr::FunctionDef(function) => function.signature().and_then(|sig| sig.expr()),
            Expr::AssignmentExpr(assignment)
                if assignment.op().is_some_and(|op| op.text() == "=") =>
            {
                assignment.lhs()
            }
            _ => None,
        };
        let Some(signature) = signature else {
            return false;
        };
        let Some(core) = peel_signature(signature.syntax().clone()).0 else {
            return false;
        };
        matches!(Expr::cast(core), Some(Expr::CallExpr(call)) if call.callee_ident().is_some())
    }
}
