//! Hand-written, lossless tokenizer for the Julia subset.
//!
//! Every byte of the input ends up in exactly one [`Token`] (including
//! whitespace, newlines, and comments), so the token stream can be reassembled
//! into the original text. Unrecognized bytes become [`TokKind::Unknown`]
//! single-byte tokens rather than being dropped, which keeps losslessness a
//! property of the lexer alone.
//!
//! This is a walking-skeleton lexer: it covers identifiers, numeric/string/char
//! literals, the common operators, delimiters, and the block keywords. Growing
//! the grammar (string interpolation, parametric `{}`, macros, etc.) starts
//! here. See `TODO.md`.

/// A lexed token kind. Maps to a [`crate::syntax::SyntaxKind`] in
/// [`crate::parser::tree_builder::syntax_kind_for`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokKind {
    // Trivia
    Whitespace,
    Newline,
    Comment,
    BlockComment,

    // Literals / identifiers
    Ident,
    Integer,
    Float,
    String,
    Char,

    // Keywords
    FunctionKw,
    EndKw,
    IfKw,
    ElseifKw,
    ElseKw,
    BeginKw,
    TrueKw,
    FalseKw,

    // Operators
    Eq,
    Plus,
    Minus,
    Star,
    Slash,
    Caret,
    Percent,
    EqEq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
    AndAnd,
    OrOr,
    Colon,
    ColonColon,
    Arrow,
    Dot,
    PipeGt,
    Bang,
    Amp,
    Pipe,

    // Delimiters / punctuation
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Semicolon,
    At,
    Dollar,

    /// Any byte we do not recognize. Materialized as `SyntaxKind::ERROR`.
    Unknown,
}

impl TokKind {
    /// Whether this token is trivia (whitespace, newline, or a comment) — never
    /// part of the grammar, always carried through as it is.
    pub(crate) fn is_trivia(self) -> bool {
        matches!(
            self,
            TokKind::Whitespace | TokKind::Newline | TokKind::Comment | TokKind::BlockComment
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Token {
    pub(crate) kind: TokKind,
    pub(crate) text: String,
    pub(crate) start: usize,
    pub(crate) end: usize,
}

/// Tokenize `input` into a lossless token stream.
pub(crate) fn lex(input: &str) -> Vec<Token> {
    Lexer::new(input).run()
}

struct Lexer<'a> {
    input: &'a str,
    bytes: &'a [u8],
    pos: usize,
    tokens: Vec<Token>,
}

impl<'a> Lexer<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input,
            bytes: input.as_bytes(),
            pos: 0,
            tokens: Vec::new(),
        }
    }

    fn run(mut self) -> Vec<Token> {
        while self.pos < self.bytes.len() {
            self.next_token();
        }
        self.tokens
    }

    fn peek(&self, ahead: usize) -> Option<u8> {
        self.bytes.get(self.pos + ahead).copied()
    }

    fn push(&mut self, kind: TokKind, start: usize, end: usize) {
        self.tokens.push(Token {
            kind,
            text: self.input[start..end].to_string(),
            start,
            end,
        });
    }

    fn next_token(&mut self) {
        let start = self.pos;
        let b = self.bytes[self.pos];

        match b {
            b' ' | b'\t' => self.lex_whitespace(start),
            b'\r' | b'\n' => self.lex_newline(start),
            b'#' => self.lex_comment(start),
            b'"' => self.lex_string(start),
            b'\'' => self.lex_char_or_unknown(start),
            b'0'..=b'9' => self.lex_number(start),
            b'.' if self.peek(1).is_some_and(|c| c.is_ascii_digit()) => self.lex_number(start),
            _ => {
                if is_ident_start(self.char_at(self.pos)) {
                    self.lex_ident_or_keyword(start);
                } else {
                    self.lex_operator_or_unknown(start);
                }
            }
        }
    }

    /// The `char` beginning at byte offset `at` (for unicode identifier checks).
    fn char_at(&self, at: usize) -> char {
        self.input[at..].chars().next().unwrap_or('\0')
    }

    fn lex_whitespace(&mut self, start: usize) {
        while matches!(self.peek(0), Some(b' ' | b'\t')) {
            self.pos += 1;
        }
        self.push(TokKind::Whitespace, start, self.pos);
    }

    fn lex_newline(&mut self, start: usize) {
        // A single newline token per line break: `\r\n`, `\r`, or `\n`.
        match self.peek(0) {
            Some(b'\r') if self.peek(1) == Some(b'\n') => self.pos += 2,
            _ => self.pos += 1,
        }
        self.push(TokKind::Newline, start, self.pos);
    }

    fn lex_comment(&mut self, start: usize) {
        if self.peek(1) == Some(b'=') {
            self.lex_block_comment(start);
            return;
        }
        // Line comment: `#` to end of line (newline excluded).
        self.pos += 1;
        while !matches!(self.peek(0), Some(b'\n' | b'\r') | None) {
            self.pos += 1;
        }
        self.push(TokKind::Comment, start, self.pos);
    }

    /// Nested block comment `#= ... =#` (Julia allows nesting). Unterminated
    /// comments run to end of input — still lossless.
    fn lex_block_comment(&mut self, start: usize) {
        self.pos += 2; // consume `#=`
        let mut depth = 1usize;
        while depth > 0 && self.pos < self.bytes.len() {
            match (self.peek(0), self.peek(1)) {
                (Some(b'#'), Some(b'=')) => {
                    self.pos += 2;
                    depth += 1;
                }
                (Some(b'='), Some(b'#')) => {
                    self.pos += 2;
                    depth -= 1;
                }
                _ => self.pos += 1,
            }
        }
        self.push(TokKind::BlockComment, start, self.pos);
    }

    fn lex_string(&mut self, start: usize) {
        let triple = self.peek(1) == Some(b'"') && self.peek(2) == Some(b'"');
        if triple {
            self.pos += 3;
            while self.pos < self.bytes.len() {
                if self.peek(0) == Some(b'"')
                    && self.peek(1) == Some(b'"')
                    && self.peek(2) == Some(b'"')
                {
                    self.pos += 3;
                    break;
                }
                self.consume_string_byte();
            }
        } else {
            self.pos += 1;
            while self.pos < self.bytes.len() {
                match self.peek(0) {
                    Some(b'"') => {
                        self.pos += 1;
                        break;
                    }
                    Some(b'\n') => break, // unterminated; stop at the line break
                    _ => self.consume_string_byte(),
                }
            }
        }
        self.push(TokKind::String, start, self.pos);
    }

    /// Consume one byte inside a string, honoring a backslash escape.
    fn consume_string_byte(&mut self) {
        if self.peek(0) == Some(b'\\') && self.pos + 1 < self.bytes.len() {
            self.pos += 2;
        } else {
            self.pos += 1;
        }
    }

    /// `'` begins a char literal when it is *not* a postfix adjoint/transpose.
    /// We lex it as a char when a closing `'` is found within a short window
    /// (one char, or a backslash escape); otherwise it is an [`TokKind::Unknown`]
    /// single byte (transpose support is a TODO).
    fn lex_char_or_unknown(&mut self, start: usize) {
        let after_open = self.pos + 1;
        // Try `'\x'` (escape) or `'c'` (single char), then a closing quote.
        let content_end = if self.bytes.get(after_open) == Some(&b'\\') {
            // Escape: consume backslash + following char.
            let mut idx = after_open + 1;
            if idx < self.bytes.len() {
                idx += 1;
            }
            idx
        } else if after_open < self.bytes.len() {
            // A single (possibly multibyte) char.
            after_open + self.char_at(after_open).len_utf8()
        } else {
            after_open
        };

        if self.bytes.get(content_end) == Some(&b'\'') {
            self.pos = content_end + 1;
            self.push(TokKind::Char, start, self.pos);
        } else {
            self.pos += 1;
            self.push(TokKind::Unknown, start, self.pos);
        }
    }

    fn lex_number(&mut self, start: usize) {
        // Hex / binary / octal integer prefixes.
        if self.peek(0) == Some(b'0') && matches!(self.peek(1), Some(b'x' | b'X' | b'b' | b'o')) {
            self.pos += 2;
            while matches!(self.peek(0), Some(c) if c.is_ascii_hexdigit() || c == b'_') {
                self.pos += 1;
            }
            self.push(TokKind::Integer, start, self.pos);
            return;
        }

        let mut is_float = false;
        while matches!(self.peek(0), Some(c) if c.is_ascii_digit() || c == b'_') {
            self.pos += 1;
        }
        // Fractional part: a `.` followed by a digit, or a trailing `.`.
        if self.peek(0) == Some(b'.') {
            is_float = true;
            self.pos += 1;
            while matches!(self.peek(0), Some(c) if c.is_ascii_digit() || c == b'_') {
                self.pos += 1;
            }
        }
        // Exponent: `e`/`E`/`f` with an optional sign.
        if matches!(self.peek(0), Some(b'e' | b'E' | b'f')) {
            is_float = true;
            self.pos += 1;
            if matches!(self.peek(0), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            while matches!(self.peek(0), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
        }

        let kind = if is_float {
            TokKind::Float
        } else {
            TokKind::Integer
        };
        self.push(kind, start, self.pos);
    }

    fn lex_ident_or_keyword(&mut self, start: usize) {
        // First char already known to be an identifier start.
        self.pos += self.char_at(self.pos).len_utf8();
        loop {
            let c = self.char_at(self.pos);
            if self.pos < self.bytes.len() && is_ident_continue(c) {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
        let text = &self.input[start..self.pos];
        let kind = keyword_kind(text).unwrap_or(TokKind::Ident);
        self.push(kind, start, self.pos);
    }

    fn lex_operator_or_unknown(&mut self, start: usize) {
        let b0 = self.peek(0);
        let b1 = self.peek(1);

        // Two-char operators first (longest match).
        let two = match (b0, b1) {
            (Some(b'='), Some(b'=')) => Some(TokKind::EqEq),
            (Some(b'!'), Some(b'=')) => Some(TokKind::NotEq),
            (Some(b'<'), Some(b'=')) => Some(TokKind::Le),
            (Some(b'>'), Some(b'=')) => Some(TokKind::Ge),
            (Some(b'&'), Some(b'&')) => Some(TokKind::AndAnd),
            (Some(b'|'), Some(b'|')) => Some(TokKind::OrOr),
            (Some(b':'), Some(b':')) => Some(TokKind::ColonColon),
            (Some(b'-'), Some(b'>')) => Some(TokKind::Arrow),
            (Some(b'|'), Some(b'>')) => Some(TokKind::PipeGt),
            _ => None,
        };
        if let Some(kind) = two {
            self.pos += 2;
            self.push(kind, start, self.pos);
            return;
        }

        let one = match b0 {
            Some(b'=') => Some(TokKind::Eq),
            Some(b'+') => Some(TokKind::Plus),
            Some(b'-') => Some(TokKind::Minus),
            Some(b'*') => Some(TokKind::Star),
            Some(b'/') => Some(TokKind::Slash),
            Some(b'^') => Some(TokKind::Caret),
            Some(b'%') => Some(TokKind::Percent),
            Some(b'<') => Some(TokKind::Lt),
            Some(b'>') => Some(TokKind::Gt),
            Some(b':') => Some(TokKind::Colon),
            Some(b'.') => Some(TokKind::Dot),
            Some(b'!') => Some(TokKind::Bang),
            Some(b'&') => Some(TokKind::Amp),
            Some(b'|') => Some(TokKind::Pipe),
            Some(b'(') => Some(TokKind::LParen),
            Some(b')') => Some(TokKind::RParen),
            Some(b'[') => Some(TokKind::LBracket),
            Some(b']') => Some(TokKind::RBracket),
            Some(b'{') => Some(TokKind::LBrace),
            Some(b'}') => Some(TokKind::RBrace),
            Some(b',') => Some(TokKind::Comma),
            Some(b';') => Some(TokKind::Semicolon),
            Some(b'@') => Some(TokKind::At),
            Some(b'$') => Some(TokKind::Dollar),
            _ => None,
        };
        match one {
            Some(kind) => {
                self.pos += 1;
                self.push(kind, start, self.pos);
            }
            None => {
                // Unknown: consume one full char to stay on a char boundary.
                self.pos += self.char_at(self.pos).len_utf8();
                self.push(TokKind::Unknown, start, self.pos);
            }
        }
    }
}

fn keyword_kind(text: &str) -> Option<TokKind> {
    Some(match text {
        "function" => TokKind::FunctionKw,
        "end" => TokKind::EndKw,
        "if" => TokKind::IfKw,
        "elseif" => TokKind::ElseifKw,
        "else" => TokKind::ElseKw,
        "begin" => TokKind::BeginKw,
        "true" => TokKind::TrueKw,
        "false" => TokKind::FalseKw,
        _ => return None,
    })
}

fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_alphabetic() || (!c.is_ascii() && is_unicode_ident(c))
}

fn is_ident_continue(c: char) -> bool {
    c == '_' || c == '!' || c.is_alphanumeric() || (!c.is_ascii() && is_unicode_ident(c))
}

/// Non-ASCII identifier characters: accept any alphabetic/alphanumeric or common
/// math/symbol code points Julia allows (a pragmatic superset for the skeleton).
fn is_unicode_ident(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '\u{391}'..='\u{3c9}' | '\u{2070}'..='\u{209f}')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(input: &str) -> Vec<TokKind> {
        lex(input).into_iter().map(|t| t.kind).collect()
    }

    fn roundtrips(input: &str) -> bool {
        let joined: String = lex(input).into_iter().map(|t| t.text).collect();
        joined == input
    }

    #[test]
    fn lossless_over_assorted_input() {
        for input in [
            "x = 1 + 2\n",
            "f(a, b)\n",
            "#= a #= nested =# b =#\n",
            "function g(x)\n    x ^ 2\nend\n",
            "s = \"hello\\n\"\nc = 'a'\n",
            "if a >= b\n    a\nelseif c\n    c\nelse\n    b\nend\n",
            "α = β + 1\n",
            "0x1f + 0b1010\n",
        ] {
            assert!(roundtrips(input), "did not round-trip: {input:?}");
        }
    }

    #[test]
    fn keywords_and_operators() {
        assert_eq!(
            kinds("a == b"),
            vec![
                TokKind::Ident,
                TokKind::Whitespace,
                TokKind::EqEq,
                TokKind::Whitespace,
                TokKind::Ident
            ]
        );
        assert_eq!(keyword_kind("function"), Some(TokKind::FunctionKw));
        assert_eq!(keyword_kind("ends"), None);
    }

    #[test]
    fn bang_in_identifier() {
        assert_eq!(kinds("push!"), vec![TokKind::Ident]);
    }
}
