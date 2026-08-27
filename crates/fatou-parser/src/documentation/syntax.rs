//! Syntax kinds and Rowan bindings for documentation trees.

use rowan::Language;

/// Nodes and tokens in a lossless Julia Markdown tree.
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum SyntaxKind {
    // Nodes.
    ROOT,
    PARAGRAPH,
    ATX_HEADING,
    SETEXT_HEADING,
    BLOCK_QUOTE,
    ADMONITION,
    LIST,
    LIST_ITEM,
    INDENTED_CODE_BLOCK,
    FENCED_CODE_BLOCK,
    MATH_BLOCK,
    FOOTNOTE_DEFINITION,
    TABLE,
    TABLE_ROW,
    TABLE_CELL,
    THEMATIC_BREAK,
    EMPHASIS,
    STRONG,
    INLINE_CODE,
    INLINE_MATH,
    LINK,
    IMAGE,
    AUTOLINK,
    FOOTNOTE_REFERENCE,
    INTERPOLATION,
    HARD_BREAK,
    ERROR,

    // Tokens.
    TEXT,
    WHITESPACE,
    NEWLINE,
    BLANK_LINE,
    MARKER,
    HEADING_MARKER,
    LIST_MARKER,
    QUOTE_MARKER,
    ADMONITION_CATEGORY,
    ADMONITION_TITLE,
    FOOTNOTE_LABEL,
    FENCE,
    INFO_STRING,
    CODE_CONTENT,
    MATH_CONTENT,
    LINK_DESTINATION,
    ESCAPE,
    SOFT_BREAK,
    EN_DASH,
    ERROR_TOKEN,
}

impl SyntaxKind {
    const LAST: Self = Self::ERROR_TOKEN;
}

/// Rowan language used by the documentation CST.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DocumentationLanguage {}

impl Language for DocumentationLanguage {
    type Kind = SyntaxKind;

    fn kind_from_raw(raw: rowan::SyntaxKind) -> Self::Kind {
        assert!(
            raw.0 <= SyntaxKind::LAST as u16,
            "invalid documentation syntax kind"
        );
        // SAFETY: every discriminant is contiguous from `ROOT` through
        // `ERROR_TOKEN`, and the bound check above excludes other values.
        unsafe { std::mem::transmute::<u16, SyntaxKind>(raw.0) }
    }

    fn kind_to_raw(kind: Self::Kind) -> rowan::SyntaxKind {
        rowan::SyntaxKind(kind as u16)
    }
}

pub type SyntaxNode = rowan::SyntaxNode<DocumentationLanguage>;
pub type SyntaxToken = rowan::SyntaxToken<DocumentationLanguage>;
pub type SyntaxElement = rowan::SyntaxElement<DocumentationLanguage>;
