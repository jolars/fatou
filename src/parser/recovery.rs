use crate::parser::cursor::consume_to_line_end;
use crate::parser::events::{Event, ExprParse};
use crate::parser::lexer::Token;
use crate::syntax::SyntaxKind;

/// An `ERROR` node wrapping tokens `start..end`.
pub(crate) fn error_expr_with_range(start: usize, end: usize) -> ExprParse {
    error_node_with_range(SyntaxKind::ERROR, start, end)
}

/// An error node of `kind` (`ERROR` for a bare `(error)`, `ERROR_TRIVIA` for a
/// `(error-t)` truncation marker) wrapping tokens `start..end`. The range may be
/// empty (`start == end`) for a zero-width synthesized node.
pub(crate) fn error_node_with_range(kind: SyntaxKind, start: usize, end: usize) -> ExprParse {
    let mut events = Vec::new();
    events.push(Event::Start(kind));
    for idx in start..end {
        events.push(Event::Tok(idx));
    }
    events.push(Event::Finish);
    ExprParse { start, end, events }
}

/// Recover by consuming the rest of the line into an `ERROR` node.
pub(crate) fn error_expr_to_line_end(
    tokens: &[Token],
    start: usize,
    recovery_from: usize,
) -> ExprParse {
    let end = consume_to_line_end(tokens, recovery_from);
    error_expr_with_range(start, end)
}
