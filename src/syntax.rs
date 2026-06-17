//! The Julia syntax-kind set and the `rowan` [`Language`] binding.
//!
//! [`SyntaxKind`] holds both token kinds and node kinds in a single
//! `#[repr(u16)]` enum (rust-analyzer style). The variants are contiguous and
//! `ERROR` is the last one, so [`JuliaLanguage::kind_from_raw`] recovers a kind
//! from its raw `u16` with a bounds-checked transmute instead of a large match.

use rowan::Language;

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
    INTERPOLATION,
    NAME,
    BINARY_EXPR,
    UNARY_EXPR,
    PAREN_EXPR,
    TUPLE_EXPR,
    VECT_EXPR,
    MATRIX_EXPR,
    MATRIX_ROW,
    COMPREHENSION,
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
    ASSIGNMENT_EXPR,
    ARROW_EXPR,
    TERNARY_EXPR,
    IF_EXPR,
    ELSEIF_CLAUSE,
    ELSE_CLAUSE,
    CONDITION,
    FUNCTION_DEF,
    SIGNATURE,
    BLOCK,
    BEGIN_EXPR,
    WHILE_EXPR,
    FOR_EXPR,
    FOR_BINDING,
    LET_EXPR,
    LET_BINDINGS,
    QUOTE_EXPR,
    TRY_EXPR,
    CATCH_CLAUSE,
    FINALLY_CLAUSE,
    STRUCT_DEF,
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
    MACRO_CALL,
    MACRO_NAME,

    // --- Trivia tokens ---
    WHITESPACE,
    NEWLINE,
    COMMENT,
    BLOCK_COMMENT,

    // --- Literal / identifier tokens ---
    IDENT,
    INTEGER,
    BIN_INT,
    OCT_INT,
    HEX_INT,
    FLOAT,
    FLOAT32,
    CHAR,
    STRING_DELIM_OPEN,
    STRING_DELIM_CLOSE,
    CMD_DELIM_OPEN,
    CMD_DELIM_CLOSE,
    STRING_CONTENT,
    STRING_PREFIX,
    STRING_SUFFIX,

    // --- Keyword tokens ---
    FUNCTION_KW,
    END_KW,
    IF_KW,
    ELSEIF_KW,
    ELSE_KW,
    BEGIN_KW,
    TRUE_KW,
    FALSE_KW,
    WHILE_KW,
    FOR_KW,
    LET_KW,
    QUOTE_KW,
    TRY_KW,
    CATCH_KW,
    FINALLY_KW,
    STRUCT_KW,
    MUTABLE_KW,
    MODULE_KW,
    BAREMODULE_KW,
    DO_KW,
    RETURN_KW,
    BREAK_KW,
    CONTINUE_KW,
    CONST_KW,
    GLOBAL_KW,
    LOCAL_KW,
    IMPORT_KW,
    USING_KW,
    EXPORT_KW,
    WHERE_KW,

    // --- Operator tokens ---
    EQ,
    PLUS,
    MINUS,
    STAR,
    SLASH,
    SLASH_SLASH,
    CARET,
    PERCENT,
    EQ_EQ,
    NOT_EQ,
    LT,
    LE,
    GT,
    GE,
    AND_AND,
    OR_OR,
    COLON,
    COLON_COLON,
    SUBTYPE,
    SUPERTYPE,
    ARROW,
    DOT,
    DOT_DOT_DOT,
    PIPE_GT,
    BANG,
    AMP,
    PIPE,
    QUESTION,
    TRANSPOSE,

    // --- Broadcasting (dotted) operator tokens ---
    DOT_PLUS,
    DOT_MINUS,
    DOT_STAR,
    DOT_SLASH,
    DOT_SLASH_SLASH,
    DOT_CARET,
    DOT_PERCENT,
    DOT_EQ,
    DOT_EQ_EQ,
    DOT_NOT_EQ,
    DOT_LT,
    DOT_LE,
    DOT_GT,
    DOT_GE,

    // --- Delimiter / punctuation tokens ---
    LPAREN,
    RPAREN,
    LBRACKET,
    RBRACKET,
    LBRACE,
    RBRACE,
    COMMA,
    SEMICOLON,
    AT,
    DOLLAR,

    /// Both the unknown-token kind and the error-recovery node kind. Keep this
    /// the **last** variant: [`JuliaLanguage::kind_from_raw`] uses it as the
    /// upper bound of the valid discriminant range.
    ERROR,
}

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
