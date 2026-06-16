use crate::parser::cursor;
use crate::parser::lexer::Token;

/// A lightweight wrapper over the token slice with cursor helpers, threaded
/// through the parser so navigation reads the same way everywhere.
pub(crate) struct ParserCtx<'a> {
    tokens: &'a [Token],
}

impl<'a> ParserCtx<'a> {
    pub(crate) fn new(tokens: &'a [Token]) -> Self {
        Self { tokens }
    }

    pub(crate) fn token(&self, i: usize) -> Option<&'a Token> {
        self.tokens.get(i)
    }

    pub(crate) fn tokens(&self) -> &'a [Token] {
        self.tokens
    }

    pub(crate) fn skip_ws(&self, i: usize) -> usize {
        cursor::skip_ws(self.tokens, i)
    }

    pub(crate) fn skip_ws_and_newlines(&self, i: usize) -> usize {
        cursor::skip_ws_and_newlines(self.tokens, i)
    }

    pub(crate) fn skip_trivia(&self, i: usize) -> usize {
        cursor::skip_trivia(self.tokens, i)
    }
}
