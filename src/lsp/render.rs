//! Shared rendering of harvested library signatures and binding kinds.
//!
//! Both completion (`completionItem/resolve`'s `detail` line) and hover draw a
//! one-line signature from the same [`FunctionGroup`]/[`TypeDef`] shapes, so the
//! renderers live here rather than in either handler.

use crate::index::{FunctionGroup, Method, Param, TypeDef, TypeExpr, TypeKind};
use crate::semantic::BindingKind;

/// A one-line signature for a function group, from its first method.
pub(crate) fn function_detail(group: &FunctionGroup) -> String {
    match group.methods.first() {
        Some(method) => format!("{}{}", group.name, render_method(method)),
        None => group.name.clone(),
    }
}

/// The `(params; keywords)` of a method, rendered compactly.
pub(crate) fn render_method(method: &Method) -> String {
    // Share the assembly with [`signature_label`]; the empty name yields a bare
    // `(params; keywords)::ret` and the offsets are discarded.
    signature_label("", method).0
}

/// A method's full signature label (`name(params; keywords)::ret`) together with
/// the `[start, end)` UTF-16 offsets of each parameter within it, positional
/// then keyword in order. Feeds LSP signature help's per-parameter highlighting.
pub(crate) fn signature_label(name: &str, method: &Method) -> (String, Vec<(u32, u32)>) {
    let mut out = String::from(name);
    out.push('(');
    let mut spans: Vec<(u32, u32)> = Vec::new();
    let mut push_param = |out: &mut String, param: &Param| {
        let start = out.encode_utf16().count() as u32;
        out.push_str(&render_param(param));
        let end = out.encode_utf16().count() as u32;
        spans.push((start, end));
    };
    for (i, param) in method.params.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        push_param(&mut out, param);
    }
    if !method.keyword_params.is_empty() {
        out.push_str("; ");
        for (i, param) in method.keyword_params.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            push_param(&mut out, param);
        }
    }
    out.push(')');
    if let Some(ret) = &method.return_type {
        out.push_str("::");
        out.push_str(&render_type(ret));
    }
    (out, spans)
}

pub(crate) fn render_param(param: &Param) -> String {
    let mut out = param.name.clone().unwrap_or_default();
    if let Some(ty) = &param.type_annotation {
        out.push_str("::");
        out.push_str(&render_type(ty));
    }
    if param.is_vararg {
        out.push_str("...");
    }
    if let Some(default) = &param.default {
        out.push_str(" = ");
        out.push_str(default);
    }
    out
}

/// A short type-kind and supertype detail for a type definition.
pub(crate) fn type_detail(def: &TypeDef) -> String {
    let head = match def.kind {
        TypeKind::Struct { mutable: true } => "mutable struct",
        TypeKind::Struct { mutable: false } => "struct",
        TypeKind::Abstract => "abstract type",
        TypeKind::Primitive { .. } => "primitive type",
    };
    match &def.supertype {
        Some(sup) => format!("{head} {} <: {}", def.name, render_type(sup)),
        None => format!("{head} {}", def.name),
    }
}

/// Render a [`TypeExpr`] back to Julia-ish source for a signature preview.
pub(crate) fn render_type(ty: &TypeExpr) -> String {
    match ty {
        TypeExpr::Name { path } => path.join("."),
        TypeExpr::Applied { base, args } => {
            format!("{}{{{}}}", render_type(base), render_types(args))
        }
        TypeExpr::Union { members } => format!("Union{{{}}}", render_types(members)),
        TypeExpr::Tuple { elems } => format!("Tuple{{{}}}", render_types(elems)),
        TypeExpr::TypeVar { name, lower, upper } => match (lower, upper) {
            (Some(l), Some(u)) => format!("{} <: {name} <: {}", render_type(l), render_type(u)),
            (None, Some(u)) => format!("{name} <: {}", render_type(u)),
            (Some(l), None) => format!("{name} >: {}", render_type(l)),
            (None, None) => name.clone(),
        },
        TypeExpr::Raw { text } => text.clone(),
    }
}

fn render_types(types: &[TypeExpr]) -> String {
    types.iter().map(render_type).collect::<Vec<_>>().join(", ")
}

/// A short human label for a binding's kind, shown as completion `detail` and in
/// a local hover.
pub(crate) fn binding_detail(kind: BindingKind) -> &'static str {
    use BindingKind::*;
    match kind {
        Global => "global",
        Local => "local",
        Const => "const",
        Param => "parameter",
        KeywordParam => "keyword",
        ForVar => "loop variable",
        LetVar => "let binding",
        CatchParam => "catch variable",
        TypeParam => "type parameter",
        Field => "field",
        Function => "function",
        Macro => "macro",
        Type => "type",
        Module => "module",
        Import => "import",
    }
}
