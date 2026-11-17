//! Static contracts of Julia's control-flow macros. Block labels and goto
//! labels use separate namespaces; neither introduces a variable binding.

use smol_str::SmolStr;

use crate::ast::{Arg, AstNode, AstToken, Expr, HasArgList, KeywordArg, MacroCall};
use crate::syntax::SyntaxNode;

use super::{BindingKind, ScopeId, SemanticModel};

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

impl SemanticModel {
    /// Apply the built-in contract only after checking file-local masking in
    /// the appropriate namespace. Unknown macros keep ordinary argument reads.
    pub(super) fn control_macro(&self, call: &MacroCall, scope: ScopeId) -> Option<ControlMacro> {
        let control = control_macro(call)?;
        let name = call.name()?;
        let parts: Vec<_> = name.ident_tokens().collect();
        let (lookup, simple, qualified) = match parts.as_slice() {
            [simple] => (simple.text(), simple.text(), false),
            [owner, simple] => (owner.text(), simple.text(), true),
            _ => return None,
        };
        let Some(id) = self.resolve_name(lookup, scope, !qualified) else {
            return Some(control);
        };
        let binding = self.binding(id);
        if binding.kind != BindingKind::Import {
            return None;
        }
        // An explicit import masks the implicit Base macro unless it imports
        // that same macro (or, for a qualified call, the Base module itself).
        self.module_loads
            .iter()
            .any(|load| {
                if load.scope != binding.scope
                    || !load.range.contains_range(binding.def_range)
                    || load.path.leading_dots != 0
                {
                    return false;
                }
                if qualified {
                    return load.items.is_none() && load.path.components.as_slice() == ["Base"];
                }
                if let Some(items) = &load.items {
                    load.path.components.as_slice() == ["Base"]
                        && items.iter().any(|item| {
                            item.name.strip_prefix('@') == Some(simple)
                                && item.range.contains_range(binding.def_range)
                        })
                } else {
                    load.path.components.len() == 2
                        && load.path.components[0] == "Base"
                        && load.path.components[1].strip_prefix('@') == Some(simple)
                }
            })
            .then_some(control)
    }
}

/// Recognize the built-in spellings, including parenthesized arguments and
/// `Base.@label` / `@Base.label`. This is syntactic recognition for the CFG;
/// binding analysis uses [`SemanticModel::control_macro`] to check masking.
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
