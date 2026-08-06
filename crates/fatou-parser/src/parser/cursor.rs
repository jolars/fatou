use crate::parser::lexer::{TokKind, Token};

/// Skip horizontal whitespace only.
pub(crate) fn skip_ws(tokens: &[Token], mut i: usize) -> usize {
    while matches!(tokens.get(i).map(|t| t.kind), Some(TokKind::Whitespace)) {
        i += 1;
    }
    i
}

/// Skip horizontal whitespace and `#= … =#` block comments. A block comment on a
/// keyword's line is trivia the header sits behind (`let #= c =# x = 1`), unlike
/// a line comment, which runs to the end of the line and so genuinely ends the
/// header. The comment's own newlines are skipped with it, since a multi-line
/// `#= … =#` does not terminate the header either.
pub(crate) fn skip_ws_and_block_comments(tokens: &[Token], mut i: usize) -> usize {
    while matches!(
        tokens.get(i).map(|t| t.kind),
        Some(TokKind::Whitespace | TokKind::BlockComment)
    ) {
        i += 1;
    }
    i
}

/// Skip whitespace and newlines.
pub(crate) fn skip_ws_and_newlines(tokens: &[Token], mut i: usize) -> usize {
    while matches!(
        tokens.get(i).map(|t| t.kind),
        Some(TokKind::Whitespace | TokKind::Newline)
    ) {
        i += 1;
    }
    i
}

/// Skip whitespace, newlines, and comments. Used when an operand is pending
/// (after an infix operator): an intervening comment is trivia before the
/// operand rather than an operand of its own.
pub(crate) fn skip_trivia(tokens: &[Token], mut i: usize) -> usize {
    while matches!(tokens.get(i).map(|t| t.kind), Some(k) if k.is_trivia()) {
        i += 1;
    }
    i
}

/// Advance to just past the next newline (or to end of input).
pub(crate) fn consume_to_line_end(tokens: &[Token], mut i: usize) -> usize {
    while i < tokens.len() && !matches!(tokens[i].kind, TokKind::Newline) {
        i += 1;
    }
    if i < tokens.len() && matches!(tokens[i].kind, TokKind::Newline) {
        i += 1;
    }
    i
}
