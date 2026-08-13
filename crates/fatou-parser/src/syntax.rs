//! The Julia syntax-kind set and the `rowan` [`Language`] binding.
//!
//! [`SyntaxKind`] holds both token kinds and node kinds in a single
//! `#[repr(u16)]` enum (rust-analyzer style). The variants are contiguous and
//! `ERROR` is the last one, so [`JuliaLanguage::kind_from_raw`] recovers a kind
//! from its raw `u16` with a bounds-checked transmute instead of a large match.

use rowan::Language;

use crate::keywords::keyword_table;
use crate::tokens::token_table;

/// Generate [`SyntaxKind`] from the shared token table. The node kinds — which
/// no token materializes as — are written out here; the token kinds come from
/// the table, so a `TokKind` can never be lexed into a kind that does not exist.
macro_rules! define_syntax_kind {
    ($($(#[$meta:meta])* $tok:ident $syn:ident,)*) => {
#[allow(non_camel_case_types)]
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[repr(u16)]
pub enum SyntaxKind {
    // --- Nodes ---
    ROOT,
    LITERAL,
    STRING_LITERAL,
    CMD_LITERAL,
    NONSTANDARD_IDENTIFIER,
    INTERPOLATION,
    NAME,
    BINARY_EXPR,
    RANGE_EXPR,
    COMPARISON_EXPR,
    UNARY_EXPR,
    PAREN_EXPR,
    TUPLE_EXPR,
    BARE_TUPLE_EXPR,
    PAREN_BLOCK,
    TOPLEVEL_SEMICOLON,
    DOC,
    VECT_EXPR,
    MATRIX_EXPR,
    MATRIX_ROW,
    TYPED_MATRIX_EXPR,
    BRACESCAT_EXPR,
    COMPREHENSION,
    BRACES_COMPREHENSION,
    TYPED_COMPREHENSION,
    GENERATOR,
    COMPREHENSION_IF,
    CALL_EXPR,
    INDEX_EXPR,
    DOT_CALL_EXPR,
    CURLY_EXPR,
    BRACES,
    ARG_LIST,
    ARG,
    KEYWORD_ARG,
    PARAMETERS,
    TYPE_ANNOTATION,
    WHERE_EXPR,
    SPLAT_EXPR,
    POSTFIX_EXPR,
    END_MARKER,
    BEGIN_MARKER,
    OPERATOR_ATOM,
    ASSIGNMENT_EXPR,
    ARROW_EXPR,
    JUXTAPOSE_EXPR,
    TERNARY_EXPR,
    IF_EXPR,
    ELSEIF_CLAUSE,
    ELSE_CLAUSE,
    CONDITION,
    FUNCTION_DEF,
    MACRO_DEF,
    SIGNATURE,
    BLOCK,
    BEGIN_EXPR,
    WHILE_EXPR,
    FOR_EXPR,
    FOR_BINDING,
    /// The `outer i` pattern of an iteration spec: the contextual `outer`
    /// keyword plus the loop variable it rebinds from the enclosing scope.
    OUTER_BINDING,
    LET_EXPR,
    LET_BINDINGS,
    QUOTE_EXPR,
    QUOTE_SYM,
    TRY_EXPR,
    CATCH_CLAUSE,
    FINALLY_CLAUSE,
    STRUCT_DEF,
    ABSTRACT_DEF,
    PRIMITIVE_DEF,
    TYPEGROUP_DEF,
    MODULE_DEF,
    DO_EXPR,
    DO_PARAMS,
    RETURN_EXPR,
    BREAK_EXPR,
    CONTINUE_EXPR,
    CONST_STMT,
    GLOBAL_STMT,
    LOCAL_STMT,
    IMPORT_STMT,
    USING_STMT,
    EXPORT_STMT,
    PUBLIC_STMT,
    IMPORT_PATH,
    IMPORT_ALIAS,
    MACRO_CALL,
    MACRO_NAME,

    // --- Tokens, from the shared token table ---
    $($(#[$meta])* $syn,)*

    /// The kind every one of the six `call-i` Unicode operator tiers
    /// materializes as: the tier distinction the parser binds by is a `TokKind`
    /// concern, and here only the projection shape matters. (The assignment
    /// tier and the radicals do project their own heads, so they keep their own
    /// kinds above.)
    UNICODE_OP,

    /// The error-recovery node kind: unknown tokens and recovered runs. Projected
    /// `(error)`, or `(error-t)` for the byte-bearing recovery runs that the
    /// projector identifies from the diagnostics side-channel. Recovery that is
    /// merely *absent* (missing `end`, disallowed whitespace) lives only in the
    /// diagnostics, not the tree (the rust-analyzer model). Keep this the **last**
    /// variant: [`JuliaLanguage::kind_from_raw`] uses it as the upper bound of the
    /// valid discriminant range.
    ERROR,
}
    };
}

token_table!(define_syntax_kind);

impl SyntaxKind {
    /// The number of distinct kinds. The discriminants are contiguous from
    /// `ROOT` (0) through `ERROR`, so this is the size of a flat table indexed
    /// by `kind as usize` (used by the linter's node-dispatch table).
    pub const COUNT: usize = SyntaxKind::ERROR as usize + 1;

    /// Whether this token kind is an operator symbol (including dotted,
    /// assignment, and unicode forms). One list shared by the sexpr
    /// projector and the semantic builder (operator names are importable
    /// and exportable: `import A: +`, `export ==`).
    pub fn is_operator(self) -> bool {
        use SyntaxKind::*;
        let kind = self;
        matches!(
            kind,
            EQ | PLUS
                | MINUS
                | STAR
                | STAR_STAR
                | MINUS_MINUS
                | SLASH
                | BACKSLASH
                | SLASH_SLASH
                | CARET
                | PERCENT
                | PLUS_PERCENT
                | PLUS_PLUS
                | MINUS_PERCENT
                | STAR_PERCENT
                | EQ_EQ
                | NOT_EQ
                | EQ_EQ_EQ
                | NOT_EQ_EQ
                | LT
                | LE
                | GT
                | GE
                | AND_AND
                | OR_OR
                | DOT_AND_AND
                | DOT_OR_OR
                | COLON
                | COLON_EQ
                | DOT_DOT
                | COLON_COLON
                | TILDE
                | DOT_TILDE
                | SUBTYPE
                | SUPERTYPE
                | ARROW
                | LONG_ARROW
                | LEFT_RIGHT_ARROW
                | LEFT_LONG_ARROW
                | FAT_ARROW
                | SHL
                | SHR
                | USHR
                | DOT
                | PIPE_GT
                | PIPE_LT
                | BANG
                | AMP
                | PIPE
                | DOT_PLUS
                | DOT_MINUS
                | DOT_STAR
                | DOT_STAR_STAR
                | DOT_MINUS_MINUS
                | DOT_SLASH
                | DOT_BACKSLASH
                | DOT_SLASH_SLASH
                | DOT_CARET
                | DOT_PERCENT
                | DOT_EQ
                | DOT_EQ_EQ
                | DOT_NOT_EQ
                | DOT_EQ_EQ_EQ
                | DOT_NOT_EQ_EQ
                | DOT_LT
                | DOT_LE
                | DOT_GT
                | DOT_GE
                | DOT_SHL
                | DOT_SHR
                | DOT_USHR
                | DOT_SUBTYPE
                | DOT_SUPERTYPE
                | DOT_FAT_ARROW
                | DOT_LONG_ARROW
                | DOT_LEFT_LONG_ARROW
                | DOT_LEFT_RIGHT_ARROW
                | DOT_PIPE_GT
                | DOT_AMP
                | DOT_PIPE
                | DOT_BANG
                | PLUS_EQ
                | MINUS_EQ
                | STAR_EQ
                | SLASH_EQ
                | BACKSLASH_EQ
                | SLASH_SLASH_EQ
                | CARET_EQ
                | PERCENT_EQ
                | PLUS_PERCENT_EQ
                | MINUS_PERCENT_EQ
                | STAR_PERCENT_EQ
                | PIPE_EQ
                | DOLLAR_EQ
                | AMP_EQ
                | SHL_EQ
                | SHR_EQ
                | USHR_EQ
                | DIV_EQ
                | XOR_EQ
                | DOT_PLUS_EQ
                | DOT_AMP_EQ
                | DOT_PIPE_EQ
                | DOT_MINUS_EQ
                | DOT_STAR_EQ
                | DOT_SLASH_EQ
                | DOT_BACKSLASH_EQ
                | DOT_SLASH_SLASH_EQ
                | DOT_CARET_EQ
                | DOT_PERCENT_EQ
                | DOT_SHL_EQ
                | DOT_SHR_EQ
                | DOT_USHR_EQ
                | DOT_DIV_EQ
                | DOT_XOR_EQ
                | UNICODE_OP
                | UNICODE_ASSIGN_OP
                | UNICODE_RADICAL
        )
    }
}

/// Generate [`SyntaxKind::is_keyword`] from the shared keyword table, so the
/// kind-side predicate cannot drift from the token-side one in the lexer.
macro_rules! define_keyword_predicate {
    ($($text:literal $tok:ident $syn:ident,)*) => {
        impl SyntaxKind {
            /// Whether this token kind is one of Julia's reserved words. The
            /// value keywords `true`/`false` are included: they are keywords to
            /// the lexer, whatever a consumer then makes of them (the sexpr
            /// projector, for one, treats them as literals rather than syntax).
            pub fn is_keyword(self) -> bool {
                matches!(self, $(SyntaxKind::$syn)|*)
            }
        }
    };
}

keyword_table!(define_keyword_predicate);

impl From<SyntaxKind> for rowan::SyntaxKind {
    fn from(kind: SyntaxKind) -> Self {
        Self(kind as u16)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum JuliaLanguage {}

impl Language for JuliaLanguage {
    type Kind = SyntaxKind;

    fn kind_from_raw(raw: rowan::SyntaxKind) -> Self::Kind {
        assert!(
            raw.0 <= SyntaxKind::ERROR as u16,
            "raw syntax kind {} out of range",
            raw.0
        );
        // SAFETY: `SyntaxKind` is `#[repr(u16)]` with contiguous discriminants
        // `0..=ERROR` and no holes, so any `u16` in that (asserted) range is a
        // valid discriminant.
        unsafe { std::mem::transmute::<u16, SyntaxKind>(raw.0) }
    }

    fn kind_to_raw(kind: Self::Kind) -> rowan::SyntaxKind {
        kind.into()
    }
}

pub type SyntaxNode = rowan::SyntaxNode<JuliaLanguage>;
pub type SyntaxToken = rowan::SyntaxToken<JuliaLanguage>;
pub type SyntaxElement = rowan::SyntaxElement<JuliaLanguage>;
/// A node's identity without the tree: its kind plus its text range, resolvable
/// back to a [`SyntaxNode`] against the root it came from. The key a per-file
/// analysis uses to name a node it does not hold ([`rowan`]'s cursors are `Rc`
/// based, so keeping one alive keeps the whole tree alive).
pub type NodePtr = rowan::ast::SyntaxNodePtr<JuliaLanguage>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_round_trips_through_raw() {
        for kind in [
            SyntaxKind::ROOT,
            SyntaxKind::STRING_LITERAL,
            SyntaxKind::IDENT,
            SyntaxKind::STRING_CONTENT,
            SyntaxKind::FUNCTION_KW,
            SyntaxKind::DOLLAR,
            SyntaxKind::ERROR,
        ] {
            let raw = JuliaLanguage::kind_to_raw(kind);
            assert_eq!(JuliaLanguage::kind_from_raw(raw), kind);
        }
    }
}
