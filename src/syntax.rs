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
    MACRO_KW,
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
    BACKSLASH,
    SLASH_SLASH,
    CARET,
    PERCENT,
    // Invalid doubled operators `**`/`--` (project `(Error**)` /
    // `(ErrorInvalidOperator)`).
    STAR_STAR,
    MINUS_MINUS,
    EQ_EQ,
    NOT_EQ,
    EQ_EQ_EQ,
    NOT_EQ_EQ,
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
    LONG_ARROW,
    LEFT_RIGHT_ARROW,
    LEFT_LONG_ARROW,
    FAT_ARROW,
    SHL,
    SHR,
    USHR,
    // Augmented (compound) assignment operators `op=`.
    PLUS_EQ,
    MINUS_EQ,
    STAR_EQ,
    SLASH_EQ,
    BACKSLASH_EQ,
    SLASH_SLASH_EQ,
    CARET_EQ,
    PERCENT_EQ,
    PIPE_EQ,
    AMP_EQ,
    SHL_EQ,
    SHR_EQ,
    USHR_EQ,
    DIV_EQ,
    XOR_EQ,
    DOT,
    DOT_DOT,
    DOT_DOT_DOT,
    PIPE_GT,
    PIPE_LT,
    BANG,
    AMP,
    PIPE,
    TILDE,
    QUESTION,
    TRANSPOSE,

    // --- Broadcasting (dotted) operator tokens ---
    DOT_PLUS,
    DOT_MINUS,
    DOT_STAR,
    // Broadcast invalid doubled operators `.**`/`.--`.
    DOT_STAR_STAR,
    DOT_MINUS_MINUS,
    DOT_SLASH,
    DOT_BACKSLASH,
    DOT_SLASH_SLASH,
    DOT_CARET,
    DOT_PERCENT,
    DOT_EQ,
    DOT_EQ_EQ,
    DOT_NOT_EQ,
    DOT_EQ_EQ_EQ,
    DOT_NOT_EQ_EQ,
    DOT_LT,
    DOT_LE,
    DOT_GT,
    DOT_GE,
    DOT_SUBTYPE,
    DOT_SUPERTYPE,
    DOT_FAT_ARROW,
    DOT_LONG_ARROW,
    DOT_LEFT_LONG_ARROW,
    DOT_LEFT_RIGHT_ARROW,
    DOT_PIPE_GT,
    DOT_TILDE,
    DOT_AND_AND,
    DOT_OR_OR,
    DOT_AMP,
    DOT_PIPE,
    // Broadcast augmented assignment `.op=`.
    DOT_PLUS_EQ,
    DOT_MINUS_EQ,
    DOT_STAR_EQ,
    DOT_SLASH_EQ,
    DOT_BACKSLASH_EQ,
    DOT_SLASH_SLASH_EQ,
    DOT_CARET_EQ,
    DOT_PERCENT_EQ,
    DOT_SHL_EQ,
    DOT_SHR_EQ,
    DOT_USHR_EQ,
    DOT_DIV_EQ,
    DOT_XOR_EQ,

    // Single-codepoint Unicode operator tokens. The tier distinctions the parser
    // needs live in the `TokKind`; here only the projection shape matters, so the
    // six `call-i` tiers collapse to `UNICODE_OP`, the assignment tier projects
    // its own head, and the radicals are prefix-only.
    UNICODE_OP,
    UNICODE_ASSIGN_OP,
    UNICODE_RADICAL,

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

    /// The error-recovery node kind: unknown tokens and recovered runs. Projected
    /// `(error)`, or `(error-t)` for the byte-bearing recovery runs that the
    /// projector identifies from the diagnostics side-channel. Recovery that is
    /// merely *absent* (missing `end`, disallowed whitespace) lives only in the
    /// diagnostics, not the tree (the rust-analyzer model). Keep this the **last**
    /// variant: [`JuliaLanguage::kind_from_raw`] uses it as the upper bound of the
    /// valid discriminant range.
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
