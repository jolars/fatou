//! Shape helpers shared by the three `const`-declaration rules — `const-local`,
//! `global-const-in-function`, and `local-const`. Julia gives each spelling of a
//! bad `const` its own lowering error, so the rules divide the space by which
//! scope modifier the declaration carries, and all three need the same reading
//! of that modifier.

use crate::syntax::{SyntaxKind, SyntaxNode};

/// The `global`/`local` modifier attached to a `const` declaration.
pub struct ScopeModifier {
    /// `GLOBAL_STMT` or `LOCAL_STMT`.
    pub kind: SyntaxKind,
    /// The outermost node of the modifier/`const` pair — the declaration's full
    /// extent, and so what a diagnostic should span.
    pub outer: SyntaxNode,
}

/// The `global`/`local` modifier on `const_stmt`, if it carries one.
///
/// Julia accepts either order and the parser nests them as written: `global
/// const x = 1` is `GLOBAL_STMT > CONST_STMT`, while `const global x = 1` is
/// `CONST_STMT > GLOBAL_STMT`. The two spellings mean the same thing, so both
/// have to be read the same way.
pub fn scope_modifier(const_stmt: &SyntaxNode) -> Option<ScopeModifier> {
    let is_modifier = |kind| matches!(kind, SyntaxKind::GLOBAL_STMT | SyntaxKind::LOCAL_STMT);
    if let Some(parent) = const_stmt.parent().filter(|p| is_modifier(p.kind())) {
        return Some(ScopeModifier {
            kind: parent.kind(),
            outer: parent,
        });
    }
    let child = const_stmt.children().find(|c| is_modifier(c.kind()))?;
    Some(ScopeModifier {
        kind: child.kind(),
        outer: const_stmt.clone(),
    })
}

/// Whether `node` is a construct that keeps the code inside it from being
/// lowered where it is written: quoted code is data, and a macro may rewrite
/// what it is handed. Every `const` rule stays silent inside one, and that
/// verdict wins over any scope the walk has already crossed.
pub fn is_unlowered_context(node: &SyntaxNode) -> bool {
    matches!(
        node.kind(),
        SyntaxKind::QUOTE_EXPR | SyntaxKind::QUOTE_SYM | SyntaxKind::MACRO_CALL
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    /// The first `CONST_STMT` in the parse of `src`.
    fn const_stmt(src: &str) -> SyntaxNode {
        parse(src)
            .cst
            .descendants()
            .find(|n| n.kind() == SyntaxKind::CONST_STMT)
            .expect("a `const` statement")
    }

    #[test]
    fn scope_modifier_reads_both_orders() {
        for (src, kind) in [
            ("global const x = 1\n", SyntaxKind::GLOBAL_STMT),
            ("const global x = 1\n", SyntaxKind::GLOBAL_STMT),
            ("local const x = 1\n", SyntaxKind::LOCAL_STMT),
            ("const local x = 1\n", SyntaxKind::LOCAL_STMT),
        ] {
            let modifier = scope_modifier(&const_stmt(src)).expect("a modifier");
            assert_eq!(modifier.kind, kind, "for {src:?}");
            // The span covers the modifier keyword as well as the `const`.
            assert_eq!(modifier.outer.text().to_string(), src.trim_end());
        }
    }

    #[test]
    fn scope_modifier_is_none_for_a_plain_const() {
        assert!(scope_modifier(&const_stmt("const x = 1\n")).is_none());
        assert!(scope_modifier(&const_stmt("mutable struct S\n    const x::Int\nend\n")).is_none());
    }
}
