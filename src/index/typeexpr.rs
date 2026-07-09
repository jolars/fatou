//! Structured type expressions and their lowering from the CST.
//!
//! A [`TypeExpr`] is the harvested form of anything in *type position*: a `::`
//! annotation, a struct/abstract supertype, a `where`-clause bound, or a
//! function return type. It is a shallow structural reading — names, type
//! applications, `Union`/`Tuple`, and type variables with bounds — with a
//! [`TypeExpr::Raw`] fallback that carries normalized source for anything
//! exotic or interpolated. Value positions (parameter defaults, const
//! right-hand sides) are *not* lowered to `TypeExpr`; they stay source strings.

use serde::{Deserialize, Serialize};

use crate::syntax::{SyntaxKind, SyntaxNode};

/// A structured type expression lowered from a CST node in type position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TypeExpr {
    /// A bare or qualified type name: `Int`, `Base.AbstractDict`.
    Name { path: Vec<String> },
    /// A type application `Foo{A, B}`.
    Applied {
        base: Box<TypeExpr>,
        args: Vec<TypeExpr>,
    },
    /// `Union{A, B, ...}`.
    Union { members: Vec<TypeExpr> },
    /// `Tuple{A, B, ...}`.
    Tuple { elems: Vec<TypeExpr> },
    /// A type variable with optional bounds: `T`, `T <: Real`, `L <: T <: U`.
    TypeVar {
        name: String,
        lower: Option<Box<TypeExpr>>,
        upper: Option<Box<TypeExpr>>,
    },
    /// Anything not otherwise recognized (interpolated, operator-typed, or a
    /// grammar shape without a dedicated case), carrying normalized source.
    Raw { text: String },
}

/// A node's source text with each internal run of whitespace collapsed to a
/// single space and the ends trimmed. Shared by [`TypeExpr::Raw`], parameter
/// defaults, and const value previews.
pub(crate) fn normalized_text(node: &SyntaxNode) -> String {
    let mut out = String::new();
    let mut in_ws = false;
    for ch in node.text().to_string().chars() {
        if ch.is_whitespace() {
            in_ws = true;
        } else {
            if in_ws && !out.is_empty() {
                out.push(' ');
            }
            in_ws = false;
            out.push(ch);
        }
    }
    out
}

/// Lower a CST node in type position (annotation, supertype right-hand side,
/// return type, type-application argument) to a [`TypeExpr`].
pub(crate) fn lower_type(node: &SyntaxNode) -> TypeExpr {
    match node.kind() {
        SyntaxKind::NAME => TypeExpr::Name {
            path: vec![ident_text(node).unwrap_or_default()],
        },
        SyntaxKind::NONSTANDARD_IDENTIFIER => TypeExpr::Name {
            path: vec![nonstandard_text(node).unwrap_or_default()],
        },
        SyntaxKind::CURLY_EXPR => lower_curly(node),
        SyntaxKind::PAREN_EXPR => match node.children().next() {
            Some(inner) => lower_type(&inner),
            None => raw(node),
        },
        SyntaxKind::BINARY_EXPR | SyntaxKind::COMPARISON_EXPR => match dotted_path(node) {
            Some(path) => TypeExpr::Name { path },
            None => raw(node),
        },
        _ => raw(node),
    }
}

/// Lower a single `where`-clause spec or curly type parameter to a
/// [`TypeExpr::TypeVar`]. A braced or argument-wrapped group is flattened by
/// [`lower_type_params`]; this handles one spec.
pub(crate) fn lower_type_param(node: &SyntaxNode) -> TypeExpr {
    match node.kind() {
        SyntaxKind::NAME => TypeExpr::TypeVar {
            name: ident_text(node).unwrap_or_default(),
            lower: None,
            upper: None,
        },
        SyntaxKind::BINARY_EXPR => lower_binary_bound(node),
        SyntaxKind::COMPARISON_EXPR => lower_comparison_bound(node),
        _ => raw(node),
    }
}

/// Flatten and lower a sequence of `where` specs (or curly type params),
/// descending through braced/argument groups (`where {T, S<:Real}`).
pub(crate) fn lower_type_params<'a>(
    specs: impl IntoIterator<Item = &'a SyntaxNode>,
) -> Vec<TypeExpr> {
    let mut out = Vec::new();
    for spec in specs {
        push_type_param(spec, &mut out);
    }
    out
}

fn push_type_param(node: &SyntaxNode, out: &mut Vec<TypeExpr>) {
    match node.kind() {
        SyntaxKind::BRACES | SyntaxKind::ARG | SyntaxKind::ARG_LIST => {
            for child in node.children() {
                push_type_param(&child, out);
            }
        }
        _ => out.push(lower_type_param(node)),
    }
}

/// `T <: U` (upper bound) or `T >: L` (lower bound).
fn lower_binary_bound(node: &SyntaxNode) -> TypeExpr {
    let mut children = node.children();
    let (Some(var), Some(bound)) = (children.next(), children.next()) else {
        return raw(node);
    };
    let Some(name) = ident_text(&var) else {
        return raw(node);
    };
    match bound_op(node) {
        Some(SyntaxKind::SUBTYPE) => TypeExpr::TypeVar {
            name,
            lower: None,
            upper: Some(Box::new(lower_type(&bound))),
        },
        Some(SyntaxKind::SUPERTYPE) => TypeExpr::TypeVar {
            name,
            lower: Some(Box::new(lower_type(&bound))),
            upper: None,
        },
        _ => raw(node),
    }
}

/// `L <: T <: U`: the variable is the middle operand, `L` its lower bound and
/// `U` its upper bound.
fn lower_comparison_bound(node: &SyntaxNode) -> TypeExpr {
    let parts: Vec<SyntaxNode> = node.children().collect();
    let ops: Vec<SyntaxKind> = node
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .map(|t| t.kind())
        .filter(|k| matches!(k, SyntaxKind::SUBTYPE | SyntaxKind::SUPERTYPE))
        .collect();
    if parts.len() == 3
        && ops == [SyntaxKind::SUBTYPE, SyntaxKind::SUBTYPE]
        && let Some(name) = ident_text(&parts[1])
    {
        return TypeExpr::TypeVar {
            name,
            lower: Some(Box::new(lower_type(&parts[0]))),
            upper: Some(Box::new(lower_type(&parts[2]))),
        };
    }
    raw(node)
}

/// The single `<:`/`>:` operator token of a binary bound.
fn bound_op(node: &SyntaxNode) -> Option<SyntaxKind> {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .map(|t| t.kind())
        .find(|k| matches!(k, SyntaxKind::SUBTYPE | SyntaxKind::SUPERTYPE))
}

fn lower_curly(node: &SyntaxNode) -> TypeExpr {
    let Some(base) = node.children().next() else {
        return raw(node);
    };
    let args: Vec<SyntaxNode> = node
        .children()
        .filter(|c| c.kind() == SyntaxKind::ARG_LIST)
        .flat_map(|list| list.children().collect::<Vec<_>>())
        .map(unwrap_arg)
        .collect();
    let base_name = ident_text(&base);
    match base_name.as_deref() {
        Some("Union") => TypeExpr::Union {
            members: args.iter().map(lower_type).collect(),
        },
        Some("Tuple") => TypeExpr::Tuple {
            elems: args.iter().map(lower_type).collect(),
        },
        _ => TypeExpr::Applied {
            base: Box::new(lower_type(&base)),
            args: args.iter().map(lower_type).collect(),
        },
    }
}

/// An `ARG` wrapper unwraps to its inner expression; anything else is itself.
fn unwrap_arg(node: SyntaxNode) -> SyntaxNode {
    if node.kind() == SyntaxKind::ARG {
        node.children().next().unwrap_or(node)
    } else {
        node
    }
}

fn raw(node: &SyntaxNode) -> TypeExpr {
    TypeExpr::Raw {
        text: normalized_text(node),
    }
}

/// The identifier text of a `NAME` node (or a `NONSTANDARD_IDENTIFIER`).
fn ident_text(node: &SyntaxNode) -> Option<String> {
    match node.kind() {
        SyntaxKind::NAME => node
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == SyntaxKind::IDENT)
            .map(|t| t.text().to_string()),
        SyntaxKind::NONSTANDARD_IDENTIFIER => nonstandard_text(node),
        _ => None,
    }
}

/// The quoted content of a `var"..."` identifier.
fn nonstandard_text(node: &SyntaxNode) -> Option<String> {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == SyntaxKind::STRING_CONTENT)
        .map(|t| t.text().to_string())
}

/// Read a dotted qualified name (`Base.AbstractDict`) as its components, or
/// `None` if `node` is not a pure dotted chain of names.
fn dotted_path(node: &SyntaxNode) -> Option<Vec<String>> {
    let mut reversed: Vec<String> = Vec::new();
    let mut cursor = node.clone();
    loop {
        if !matches!(cursor.kind(), SyntaxKind::BINARY_EXPR) || !has_dot(&cursor) {
            return None;
        }
        let mut children = cursor.children();
        let lhs = children.next()?;
        let rhs = children.next()?;
        if rhs.kind() != SyntaxKind::NAME {
            return None;
        }
        reversed.push(ident_text(&rhs)?);
        match lhs.kind() {
            SyntaxKind::NAME => {
                reversed.push(ident_text(&lhs)?);
                reversed.reverse();
                return Some(reversed);
            }
            SyntaxKind::BINARY_EXPR => cursor = lhs,
            _ => return None,
        }
    }
}

fn has_dot(node: &SyntaxNode) -> bool {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|t| t.kind() == SyntaxKind::DOT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    /// Parse `src` and return the first node of `kind` found in the tree.
    fn first(src: &str, kind: SyntaxKind) -> SyntaxNode {
        parse(src)
            .cst
            .descendants()
            .find(|n| n.kind() == kind)
            .unwrap_or_else(|| panic!("no {kind:?} in {src:?}"))
    }

    /// The type node of the first `x::<TYPE>` annotation in `src`.
    fn annotation_type(src: &str) -> SyntaxNode {
        let anno = first(src, SyntaxKind::TYPE_ANNOTATION);
        // The node after the `::` token.
        let mut seen_colon = false;
        for el in anno.children_with_tokens() {
            match el {
                rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::COLON_COLON => {
                    seen_colon = true;
                }
                rowan::NodeOrToken::Node(n) if seen_colon => return n,
                _ => {}
            }
        }
        panic!("no type after :: in {src:?}");
    }

    #[test]
    fn bare_name() {
        assert_eq!(
            lower_type(&annotation_type("f(x::Int) = x")),
            TypeExpr::Name {
                path: vec!["Int".into()]
            }
        );
    }

    #[test]
    fn qualified_name() {
        assert_eq!(
            lower_type(&annotation_type("f(x::Base.AbstractDict) = x")),
            TypeExpr::Name {
                path: vec!["Base".into(), "AbstractDict".into()]
            }
        );
    }

    #[test]
    fn applied() {
        assert_eq!(
            lower_type(&annotation_type("f(x::Foo{T, S}) = x")),
            TypeExpr::Applied {
                base: Box::new(TypeExpr::Name {
                    path: vec!["Foo".into()]
                }),
                args: vec![
                    TypeExpr::Name {
                        path: vec!["T".into()]
                    },
                    TypeExpr::Name {
                        path: vec!["S".into()]
                    },
                ],
            }
        );
    }

    #[test]
    fn union() {
        assert_eq!(
            lower_type(&annotation_type("f(x::Union{Int, Float64}) = x")),
            TypeExpr::Union {
                members: vec![
                    TypeExpr::Name {
                        path: vec!["Int".into()]
                    },
                    TypeExpr::Name {
                        path: vec!["Float64".into()]
                    },
                ]
            }
        );
    }

    #[test]
    fn tuple() {
        assert_eq!(
            lower_type(&annotation_type("f(x::Tuple{A, B}) = x")),
            TypeExpr::Tuple {
                elems: vec![
                    TypeExpr::Name {
                        path: vec!["A".into()]
                    },
                    TypeExpr::Name {
                        path: vec!["B".into()]
                    },
                ]
            }
        );
    }

    #[test]
    fn upper_bounded_type_var() {
        // `T <: Real` in a where clause.
        let where_expr = first("f(x) where {T <: Real} = x", SyntaxKind::WHERE_EXPR);
        let specs: Vec<SyntaxNode> = where_expr.children().skip(1).collect();
        assert_eq!(
            lower_type_params(specs.iter()),
            vec![TypeExpr::TypeVar {
                name: "T".into(),
                lower: None,
                upper: Some(Box::new(TypeExpr::Name {
                    path: vec!["Real".into()]
                })),
            }]
        );
    }

    #[test]
    fn double_bounded_type_var() {
        let where_expr = first("f(x) where {L <: T <: U} = x", SyntaxKind::WHERE_EXPR);
        let specs: Vec<SyntaxNode> = where_expr.children().skip(1).collect();
        assert_eq!(
            lower_type_params(specs.iter()),
            vec![TypeExpr::TypeVar {
                name: "T".into(),
                lower: Some(Box::new(TypeExpr::Name {
                    path: vec!["L".into()]
                })),
                upper: Some(Box::new(TypeExpr::Name {
                    path: vec!["U".into()]
                })),
            }]
        );
    }

    #[test]
    fn bare_type_var() {
        let where_expr = first("f(x) where T = x", SyntaxKind::WHERE_EXPR);
        let specs: Vec<SyntaxNode> = where_expr.children().skip(1).collect();
        assert_eq!(
            lower_type_params(specs.iter()),
            vec![TypeExpr::TypeVar {
                name: "T".into(),
                lower: None,
                upper: None,
            }]
        );
    }

    #[test]
    fn interpolated_is_raw() {
        let ty = annotation_type("f(x::$T) = x");
        assert!(matches!(lower_type(&ty), TypeExpr::Raw { .. }));
    }
}
