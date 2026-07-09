//! Shared CST-shape helpers for reading definition signatures: peeling a
//! function signature into its call core, `where` specs, and return type;
//! splitting a `::` annotation; and finding the name a type definition
//! introduces. Used by the semantic [`builder`](super::builder) and by the
//! package [`index`](crate::index) harvester, which extracts the same shapes
//! without building a full scope model.

use crate::syntax::{SyntaxKind, SyntaxNode};

/// Split a `TYPE_ANNOTATION` into the annotated pattern (absent for the
/// unnamed-argument form `::Int`, where `::` precedes the only child) and
/// the type nodes after the `::`.
pub(crate) fn annotation_parts(node: &SyntaxNode) -> (Option<SyntaxNode>, Vec<SyntaxNode>) {
    let mut pattern = None;
    let mut types = Vec::new();
    let mut seen_colon = false;
    for element in node.children_with_tokens() {
        match element {
            rowan::NodeOrToken::Token(token) if token.kind() == SyntaxKind::COLON_COLON => {
                seen_colon = true;
            }
            rowan::NodeOrToken::Node(child) => {
                if seen_colon {
                    types.push(child);
                } else {
                    pattern = Some(child);
                }
            }
            _ => {}
        }
    }
    (pattern, types)
}

/// Peel `where` clauses and a return-type annotation off a signature
/// expression, down to the core (a `CALL_EXPR`, or a `TUPLE_EXPR`/`NAME`
/// for anonymous and bare forms). Returns the core, the `where` parameter
/// specs (outermost clause first), and the return type, if any.
pub(crate) fn peel_signature(
    start: SyntaxNode,
) -> (Option<SyntaxNode>, Vec<SyntaxNode>, Option<SyntaxNode>) {
    let mut wheres = Vec::new();
    let mut return_ty = None;
    let mut cursor = Some(start);
    while let Some(node) = cursor {
        match node.kind() {
            SyntaxKind::WHERE_EXPR => {
                let mut children = node.children();
                cursor = children.next();
                wheres.extend(children);
            }
            SyntaxKind::TYPE_ANNOTATION => {
                let (pattern, types) = annotation_parts(&node);
                // Only a call can carry a return type; `x::Int` is not a
                // signature layer.
                if pattern.as_ref().is_some_and(has_call_core) {
                    return_ty = types.into_iter().next();
                    cursor = pattern;
                } else {
                    return (Some(node), wheres, return_ty);
                }
            }
            _ => return (Some(node), wheres, return_ty),
        }
    }
    (None, wheres, return_ty)
}

/// Whether peeling `node` bottoms out at a `CALL_EXPR` (a function
/// signature rather than a plain assignment target).
pub(crate) fn has_call_core(node: &SyntaxNode) -> bool {
    let mut cursor = Some(node.clone());
    while let Some(n) = cursor {
        match n.kind() {
            SyntaxKind::CALL_EXPR => return true,
            SyntaxKind::WHERE_EXPR => cursor = n.children().next(),
            SyntaxKind::TYPE_ANNOTATION => cursor = annotation_parts(&n).0,
            _ => return false,
        }
    }
    false
}

/// The `NAME` a type-definition signature introduces: peels `Foo{T} <: Super`
/// layers down to `Foo`.
pub(crate) fn type_name_of(start: &SyntaxNode) -> Option<SyntaxNode> {
    match start.kind() {
        SyntaxKind::NAME => Some(start.clone()),
        SyntaxKind::CURLY_EXPR | SyntaxKind::BINARY_EXPR | SyntaxKind::COMPARISON_EXPR => {
            start.children().next().and_then(|c| type_name_of(&c))
        }
        _ => None,
    }
}
