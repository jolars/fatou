//! Static contracts of Julia's control-flow macros. Block labels and goto
//! labels use separate namespaces; neither introduces a variable binding.

use smol_str::SmolStr;

use crate::ast::{Arg, AstNode, AstToken, Expr, HasArgList, KeywordArg, MacroCall};
use crate::syntax::SyntaxNode;

#[derive(Clone)]
pub(super) enum ControlBody {
    Expr(Expr),
    /// Macro argument lists use `KEYWORD_ARG` for the expression `x = value`.
    Assignment(KeywordArg),
}

impl ControlBody {
    fn expr(mut expr: Expr) -> Self {
        while let Expr::ParenExpr(paren) = &expr {
            let Some(inner) = paren.expr() else { break };
            expr = inner;
        }
        Self::Expr(expr)
    }

    pub(super) fn syntax(&self) -> &SyntaxNode {
        match self {
            Self::Expr(expr) => expr.syntax(),
            Self::Assignment(assignment) => assignment.syntax(),
        }
    }

    fn label_name(&self) -> Option<SmolStr> {
        match self {
            Self::Expr(expr) => label_name(expr),
            Self::Assignment(_) => None,
        }
    }
}

pub(super) enum ControlMacro {
    Label(SmolStr),
    Goto(SmolStr),
    Block {
        name: Option<SmolStr>,
        body: ControlBody,
    },
}

/// Recognize the built-in spellings, including parenthesized arguments and
/// `Base.@label` / `@Base.label`. Unrelated qualified macros stay opaque.
pub(super) fn control_macro(call: &MacroCall) -> Option<ControlMacro> {
    let name = call.name()?;
    let parts: Vec<_> = name.ident_tokens().collect();
    let simple = match parts.as_slice() {
        [simple] => simple.text(),
        [owner, simple] if owner.text() == "Base" => simple.text(),
        _ => return None,
    };
    if !matches!(simple, "label" | "goto") {
        return None;
    }
    let args: Vec<_> = if let Some(list) = call.arg_list() {
        list.syntax()
            .children()
            .map(|node| {
                if let Some(assignment) = KeywordArg::cast(node.clone()) {
                    Some(ControlBody::Assignment(assignment))
                } else {
                    Arg::cast(node)?.expr().map(ControlBody::expr)
                }
            })
            .collect::<Option<_>>()?
    } else {
        call.syntax()
            .children()
            .filter_map(Expr::cast)
            .map(ControlBody::expr)
            .collect()
    };
    match (simple, args.as_slice()) {
        ("goto", [arg]) => arg.label_name().map(ControlMacro::Goto),
        ("label", [arg]) => match arg.label_name() {
            Some(name) if name != "_" => Some(ControlMacro::Label(name)),
            Some(_) => None,
            None => Some(ControlMacro::Block {
                name: None,
                body: arg.clone(),
            }),
        },
        ("label", [arg, body]) => {
            let name = arg.label_name()?;
            (name != "_").then(|| ControlMacro::Block {
                name: Some(name),
                body: body.clone(),
            })
        }
        _ => None,
    }
}

pub(super) fn label_name(expr: &Expr) -> Option<SmolStr> {
    match expr {
        Expr::Name(name) => name.ident().map(|ident| SmolStr::new(ident.text())),
        Expr::NonstandardIdentifier(name) => name.content().map(|text| SmolStr::new(text.text())),
        _ => None,
    }
}
