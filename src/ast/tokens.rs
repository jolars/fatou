//! Typed token wrappers, the token-level counterpart to the [`AstNode`] node
//! wrappers in [`nodes`](super::nodes). Each is a zero-cost newtype over a
//! [`SyntaxToken`] that only casts when the token's kind matches, mirroring
//! rust-analyzer's `AstToken`/`tokens.rs` (rowan ships `AstNode` but no token
//! analogue, so the trait is defined here).
//!
//! Node accessors return these where a token is strongly typed (an identifier,
//! an operator); the raw [`SyntaxToken`] is always reachable via
//! [`AstToken::syntax`] for callers that need the untyped token.
//!
//! [`AstNode`]: rowan::ast::AstNode

use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

/// A typed view over a single [`SyntaxToken`], analogous to
/// [`rowan::ast::AstNode`] for nodes.
pub trait AstToken {
    /// Whether a token of `kind` can be cast to this wrapper.
    fn can_cast(kind: SyntaxKind) -> bool
    where
        Self: Sized;

    /// Cast `syntax` to this wrapper if its kind matches.
    fn cast(syntax: SyntaxToken) -> Option<Self>
    where
        Self: Sized;

    /// The wrapped raw token.
    fn syntax(&self) -> &SyntaxToken;

    /// The token's source text.
    fn text(&self) -> &str {
        self.syntax().text()
    }
}

/// The first child token of `parent` castable to the typed token `T`, the
/// [`AstToken`] analogue of [`rowan::ast::support::child`].
pub fn child_token<T: AstToken>(parent: &SyntaxNode) -> Option<T> {
    parent
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .find_map(T::cast)
}

/// Define a newtype wrapper over a [`SyntaxToken`] whose [`AstToken::can_cast`]
/// is the given predicate expression over a [`SyntaxKind`].
macro_rules! ast_token {
    ($(#[$meta:meta])* $name:ident, |$kind:ident| $can_cast:expr) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub struct $name(SyntaxToken);

        impl AstToken for $name {
            fn can_cast($kind: SyntaxKind) -> bool {
                $can_cast
            }

            fn cast(syntax: SyntaxToken) -> Option<Self> {
                Self::can_cast(syntax.kind()).then_some(Self(syntax))
            }

            fn syntax(&self) -> &SyntaxToken {
                &self.0
            }
        }
    };
}

ast_token!(
    /// A plain identifier token (`IDENT`).
    Ident,
    |kind| kind == SyntaxKind::IDENT
);

ast_token!(
    /// Any operator token: the built-in symbols plus the dotted, augmented, and
    /// unicode forms. Casts exactly the set [`SyntaxKind::is_operator`] accepts,
    /// the one predicate shared with the sexpr projector and the semantic
    /// builder.
    Operator,
    |kind| kind.is_operator()
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    fn first_token(src: &str, kind: SyntaxKind) -> SyntaxToken {
        parse(src)
            .cst
            .descendants_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == kind)
            .expect("token present")
    }

    #[test]
    fn ident_casts_only_idents() {
        let ident = first_token("foo + 1\n", SyntaxKind::IDENT);
        assert_eq!(Ident::cast(ident).unwrap().text(), "foo");
        let plus = first_token("foo + 1\n", SyntaxKind::PLUS);
        assert!(Ident::cast(plus).is_none());
    }

    #[test]
    fn operator_casts_the_is_operator_set() {
        let plus = first_token("a + b\n", SyntaxKind::PLUS);
        assert_eq!(Operator::cast(plus).unwrap().text(), "+");
        let ident = first_token("a + b\n", SyntaxKind::IDENT);
        assert!(Operator::cast(ident).is_none());
    }

    #[test]
    fn syntax_round_trips_to_the_raw_token() {
        let raw = first_token("x\n", SyntaxKind::IDENT);
        let typed = Ident::cast(raw.clone()).unwrap();
        assert_eq!(typed.syntax(), &raw);
    }
}
