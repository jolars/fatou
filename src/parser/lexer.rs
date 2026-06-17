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
    BinInt,
    OctInt,
    HexInt,
    Float,
    Float32,
    Char,

    // String / command literal pieces. A single literal is lexed as a run of
    // these (plus `Dollar`/`Ident` and, inside `$(...)`, normal-mode tokens),
    // which the parser reassembles into a `STRING_LITERAL`/`CMD_LITERAL` node.
    StringDelimOpen,
    StringDelimClose,
    CmdDelimOpen,
    CmdDelimClose,
    /// A run of literal characters inside a string/command (escapes included).
    StringContent,
    /// A non-standard literal prefix immediately before a quote, e.g. `r`, `raw`.
    StringPrefix,
    /// Suffix flag letters immediately after a prefixed literal, e.g. `ims`.
    StringSuffix,

    // Keywords
    FunctionKw,
    EndKw,
    IfKw,
    ElseifKw,
    ElseKw,
    BeginKw,
    TrueKw,
    FalseKw,
    WhileKw,
    ForKw,
    DoKw,
    LetKw,
    QuoteKw,
    TryKw,
    CatchKw,
    FinallyKw,
    StructKw,
    MutableKw,
    ModuleKw,
    BaremoduleKw,
    ReturnKw,
    BreakKw,
    ContinueKw,
    ConstKw,
    GlobalKw,
    LocalKw,
    ImportKw,
    UsingKw,
    ExportKw,
    WhereKw,

    // Operators
    Eq,
    Plus,
    Minus,
    Star,
    Slash,
    SlashSlash,
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
    Subtype,
    Supertype,
    Arrow,
    /// The pair operator `=>`.
    FatArrow,
    // Augmented (compound) assignment operators `op=`. Right-associative and at
    // the same precedence as `=`; modeled as `ASSIGNMENT_EXPR`.
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    SlashSlashEq,
    CaretEq,
    PercentEq,
    PipeEq,
    AmpEq,
    Dot,
    /// The `..` range/interval operator (infix `a..b`).
    DotDot,
    DotDotDot,
    PipeGt,
    Bang,
    Amp,
    Pipe,
    /// The `~` operator (infix `a ~ b` and prefix `~a`).
    Tilde,
    Question,
    /// Postfix transpose/adjoint `'` (only when it follows a value; otherwise a
    /// `'` opens a [`TokKind::Char`] literal).
    Transpose,

    // Broadcasting (dotted) operators: a `.` fused to a following operator.
    DotPlus,
    DotMinus,
    DotStar,
    DotSlash,
    DotSlashSlash,
    DotCaret,
    DotPercent,
    DotEq,
    DotEqEq,
    DotNotEq,
    DotLt,
    DotLe,
    DotGt,
    DotGe,
    /// The broadcast pair operator `.=>`.
    DotFatArrow,
    /// The broadcast `~` operator `.~`.
    DotTilde,
    /// The broadcast short-circuit operators `.&&` and `.||`.
    DotAndAnd,
    DotOrOr,
    // Broadcast augmented assignment `.op=` (e.g. `.+=`). Same precedence and
    // modeling as the undotted forms.
    DotPlusEq,
    DotMinusEq,
    DotStarEq,
    DotSlashEq,
    DotSlashSlashEq,
    DotCaretEq,
    DotPercentEq,

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

    /// Whether this token is a reserved keyword. Used to recognize a keyword
    /// quoted as a symbol (`:end`, `:function`).
    pub(crate) fn is_keyword(self) -> bool {
        matches!(
            self,
            TokKind::FunctionKw
                | TokKind::EndKw
                | TokKind::IfKw
                | TokKind::ElseifKw
                | TokKind::ElseKw
                | TokKind::BeginKw
                | TokKind::TrueKw
                | TokKind::FalseKw
                | TokKind::WhileKw
                | TokKind::ForKw
                | TokKind::DoKw
                | TokKind::LetKw
                | TokKind::QuoteKw
                | TokKind::TryKw
                | TokKind::CatchKw
                | TokKind::FinallyKw
                | TokKind::StructKw
                | TokKind::MutableKw
                | TokKind::ModuleKw
                | TokKind::BaremoduleKw
                | TokKind::ReturnKw
                | TokKind::BreakKw
                | TokKind::ContinueKw
                | TokKind::ConstKw
                | TokKind::GlobalKw
                | TokKind::LocalKw
                | TokKind::ImportKw
                | TokKind::UsingKw
                | TokKind::ExportKw
                | TokKind::WhereKw
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

/// The lexer's context stack. The base context is normal Julia code; opening a
/// string/command delimiter pushes a `Str`/`Cmd` frame, and a `$(` interpolation
/// inside one pushes an `Interp` frame (back to normal lexing) until its matching
/// `)` pops it. A nested string inside `$(...)` simply pushes another `Str` frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Inside a `"..."` / `"""..."""` string body.
    Str {
        triple: bool,
        /// Non-standard (prefixed) literal: body is taken verbatim, no `$`/escape
        /// splitting, and a trailing flag run is lexed as a suffix.
        raw: bool,
        prefixed: bool,
    },
    /// Inside a `` `...` `` / ` ```...``` ` command body.
    Cmd {
        triple: bool,
        raw: bool,
        prefixed: bool,
    },
    /// Inside a `$( ... )` interpolation; `depth` counts unbalanced `(`.
    Interp { depth: usize },
}

struct Lexer<'a> {
    input: &'a str,
    bytes: &'a [u8],
    pos: usize,
    tokens: Vec<Token>,
    mode_stack: Vec<Mode>,
}

impl<'a> Lexer<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input,
            bytes: input.as_bytes(),
            pos: 0,
            tokens: Vec::new(),
            mode_stack: Vec::new(),
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
        // Inside a string/command body, the body lexer owns the bytes until the
        // closing delimiter (or an interpolation, which pushes its own frame).
        if matches!(
            self.mode_stack.last(),
            Some(Mode::Str { .. } | Mode::Cmd { .. })
        ) {
            self.lex_in_string_mode();
            return;
        }

        let start = self.pos;
        let b = self.bytes[self.pos];

        // Inside a `$( ... )` interpolation, track paren nesting so the matching
        // `)` returns us to the enclosing string/command body.
        if matches!(self.mode_stack.last(), Some(Mode::Interp { .. })) {
            if b == b'(' {
                self.pos += 1;
                self.push(TokKind::LParen, start, self.pos);
                if let Some(Mode::Interp { depth }) = self.mode_stack.last_mut() {
                    *depth += 1;
                }
                return;
            }
            if b == b')' {
                self.pos += 1;
                self.push(TokKind::RParen, start, self.pos);
                if matches!(self.mode_stack.last(), Some(Mode::Interp { depth }) if *depth == 1) {
                    self.mode_stack.pop();
                } else if let Some(Mode::Interp { depth }) = self.mode_stack.last_mut() {
                    *depth -= 1;
                }
                return;
            }
        }

        match b {
            b' ' | b'\t' => self.lex_whitespace(start),
            b'\r' | b'\n' => self.lex_newline(start),
            b'#' => self.lex_comment(start),
            b'"' => self.lex_open_string(start, false),
            b'`' => self.lex_open_cmd(start, false),
            b'\'' if self.prev_ends_value() => {
                self.pos += 1;
                self.push(TokKind::Transpose, start, self.pos);
            }
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

    /// Open a `"..."` / `"""..."""` string: emit the opening delimiter token and
    /// push a `Str` body frame. `prefixed` is set when a non-standard literal
    /// prefix (`r`, `raw`, …) directly precedes the quote, which makes the body
    /// raw (no `$`/escape processing) and enables a trailing suffix scan.
    fn lex_open_string(&mut self, start: usize, prefixed: bool) {
        let triple = self.peek(1) == Some(b'"') && self.peek(2) == Some(b'"');
        self.pos += if triple { 3 } else { 1 };
        self.push(TokKind::StringDelimOpen, start, self.pos);
        self.mode_stack.push(Mode::Str {
            triple,
            raw: prefixed,
            prefixed,
        });
    }

    /// Open a `` `...` `` / ` ```...``` ` command literal, analogous to a string.
    fn lex_open_cmd(&mut self, start: usize, prefixed: bool) {
        let triple = self.peek(1) == Some(b'`') && self.peek(2) == Some(b'`');
        self.pos += if triple { 3 } else { 1 };
        self.push(TokKind::CmdDelimOpen, start, self.pos);
        self.mode_stack.push(Mode::Cmd {
            triple,
            raw: prefixed,
            prefixed,
        });
    }

    /// Lex one token inside a string/command body: a literal-content chunk, a
    /// closing delimiter (plus optional suffix), or an interpolation sigil.
    fn lex_in_string_mode(&mut self) {
        let frame = *self.mode_stack.last().expect("string mode frame");
        let (quote, triple, raw, prefixed) = match frame {
            Mode::Str {
                triple,
                raw,
                prefixed,
            } => (b'"', triple, raw, prefixed),
            Mode::Cmd {
                triple,
                raw,
                prefixed,
            } => (b'`', triple, raw, prefixed),
            Mode::Interp { .. } => unreachable!("lex_in_string_mode called in interp mode"),
        };
        let close_kind = if quote == b'"' {
            TokKind::StringDelimClose
        } else {
            TokKind::CmdDelimClose
        };

        let start = self.pos;

        // Closing delimiter at the very start of this call: empty trailing chunk.
        if self.at_close_delim(quote, triple) {
            self.pos += if triple { 3 } else { 1 };
            self.push(close_kind, start, self.pos);
            self.mode_stack.pop();
            if prefixed {
                self.lex_suffix();
            }
            return;
        }

        // Accumulate a content chunk until the close delimiter, an interpolation,
        // EOF, or (for single-quoted strings) an unterminating newline.
        while self.pos < self.bytes.len() {
            if self.at_close_delim(quote, triple) {
                break;
            }
            if !raw && self.peek(0) == Some(b'$') && self.is_interp_start(1) {
                break;
            }
            if !triple && quote == b'"' && self.peek(0) == Some(b'\n') {
                // Unterminated single-line string: stop before the newline.
                break;
            }
            self.consume_body_byte(raw);
        }

        if self.pos > start {
            self.push(TokKind::StringContent, start, self.pos);
        }

        // Decide what stopped the chunk.
        if self.at_close_delim(quote, triple) {
            let delim_start = self.pos;
            self.pos += if triple { 3 } else { 1 };
            self.push(close_kind, delim_start, self.pos);
            self.mode_stack.pop();
            if prefixed {
                self.lex_suffix();
            }
        } else if !raw && self.peek(0) == Some(b'$') && self.is_interp_start(1) {
            self.lex_interp_sigil();
        } else {
            // Unterminated (newline or EOF): leave the body frame; the parser
            // assembles whatever was emitted. Losslessness still holds.
            self.mode_stack.pop();
        }
    }

    /// Whether a closing delimiter (`triple` → three of `quote`) begins at `pos`.
    fn at_close_delim(&self, quote: u8, triple: bool) -> bool {
        if triple {
            self.peek(0) == Some(quote)
                && self.peek(1) == Some(quote)
                && self.peek(2) == Some(quote)
        } else {
            self.peek(0) == Some(quote)
        }
    }

    /// Whether the byte at `self.pos + ahead` begins an interpolation operand
    /// (an identifier-start char or an opening paren).
    fn is_interp_start(&self, ahead: usize) -> bool {
        match self.peek(ahead) {
            Some(b'(') => true,
            Some(_) => is_ident_start(self.char_at(self.pos + ahead)),
            None => false,
        }
    }

    /// Emit the `$` sigil and set up the interpolation operand: either a bare
    /// identifier (lexed inline) or a `(` that opens an `Interp` frame.
    fn lex_interp_sigil(&mut self) {
        let dollar = self.pos;
        self.pos += 1;
        self.push(TokKind::Dollar, dollar, self.pos);
        if self.peek(0) == Some(b'(') {
            let paren = self.pos;
            self.pos += 1;
            self.push(TokKind::LParen, paren, self.pos);
            self.mode_stack.push(Mode::Interp { depth: 1 });
        } else {
            // `$ident`: lex the longest identifier (so `$foo.bar` interpolates
            // `foo` and `.bar` stays content).
            self.lex_interp_ident();
        }
    }

    /// Consume one body byte. In non-raw mode a backslash escapes the next byte
    /// (so `\"`, `\$`, `\n` stay inside the content chunk).
    fn consume_body_byte(&mut self, raw: bool) {
        if !raw && self.peek(0) == Some(b'\\') && self.pos + 1 < self.bytes.len() {
            self.pos += 2;
        } else {
            self.pos += self.char_at(self.pos).len_utf8();
        }
    }

    /// After a prefixed literal closes, lex a run of ASCII-alpha flag letters as
    /// a single suffix token (e.g. the `ims` in `r"pat"ims`).
    fn lex_suffix(&mut self) {
        let start = self.pos;
        while matches!(self.peek(0), Some(c) if c.is_ascii_alphabetic()) {
            self.pos += 1;
        }
        if self.pos > start {
            self.push(TokKind::StringSuffix, start, self.pos);
        }
    }

    /// Whether the immediately preceding token ends a value, making a following
    /// `'` a postfix transpose/adjoint rather than the start of a char literal.
    /// The check is on the *immediately* preceding token (not skipping trivia),
    /// which mirrors Julia's whitespace sensitivity: `A'` is transpose but `A '`
    /// is `A` followed by a (here unterminated) char literal.
    fn prev_ends_value(&self) -> bool {
        matches!(
            self.tokens.last().map(|t| t.kind),
            Some(
                TokKind::Ident
                    | TokKind::Integer
                    | TokKind::Float
                    | TokKind::Char
                    | TokKind::TrueKw
                    | TokKind::FalseKw
                    | TokKind::RParen
                    | TokKind::RBracket
                    | TokKind::RBrace
                    | TokKind::StringDelimClose
                    | TokKind::CmdDelimClose
                    | TokKind::StringSuffix
                    | TokKind::Transpose
            )
        )
    }

    /// `'` begins a char literal when it is *not* a postfix adjoint/transpose
    /// (see [`Self::prev_ends_value`]). We lex it as a char when a closing `'`
    /// is found within a short window (one char, or a backslash escape);
    /// otherwise it is an [`TokKind::Unknown`] single byte.
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
        // Base-prefixed integers (`0x`, `0o`, `0b`). Julia's prefixes are
        // lowercase only, so `0X1` is *not* a hex literal — it falls through to
        // the decimal path and lexes as `0` followed by the identifier `X1`.
        if self.peek(0) == Some(b'0') {
            match self.peek(1) {
                Some(b'x') => {
                    self.pos += 2;
                    self.consume_digits(|c| c.is_ascii_hexdigit());
                    // A `.`-fraction or `p`/`P` binary exponent turns the hex
                    // literal into a (always Float64) hex float.
                    let mut is_float = false;
                    if self.peek(0) == Some(b'.') {
                        is_float = true;
                        self.pos += 1;
                        self.consume_digits(|c| c.is_ascii_hexdigit());
                    }
                    if matches!(self.peek(0), Some(b'p' | b'P')) {
                        is_float = true;
                        self.pos += 1;
                        if matches!(self.peek(0), Some(b'+' | b'-')) {
                            self.pos += 1;
                        }
                        self.consume_digits(|c| c.is_ascii_digit());
                    }
                    let kind = if is_float {
                        TokKind::Float
                    } else {
                        TokKind::HexInt
                    };
                    self.push(kind, start, self.pos);
                    return;
                }
                Some(b'o') => {
                    self.pos += 2;
                    self.consume_digits(|c| (b'0'..=b'7').contains(&c));
                    self.push(TokKind::OctInt, start, self.pos);
                    return;
                }
                Some(b'b') => {
                    self.pos += 2;
                    self.consume_digits(|c| matches!(c, b'0' | b'1'));
                    self.push(TokKind::BinInt, start, self.pos);
                    return;
                }
                _ => {}
            }
        }

        let mut is_float = false;
        let mut is_f32 = false;
        self.consume_digits(|c| c.is_ascii_digit());
        // Fractional part: a `.` followed by a digit, or a trailing `.`. A `.`
        // that is itself followed by another `.` belongs to the `..` range
        // operator (`1..n` is `1 .. n`), so it is not consumed as a decimal point.
        if self.peek(0) == Some(b'.') && self.peek(1) != Some(b'.') {
            is_float = true;
            self.pos += 1;
            self.consume_digits(|c| c.is_ascii_digit());
        }
        // Exponent: `e`/`E` mark a `Float`, `f` marks a `Float32`; both take an
        // optional sign.
        if matches!(self.peek(0), Some(b'e' | b'E' | b'f')) {
            if self.peek(0) == Some(b'f') {
                is_f32 = true;
            } else {
                is_float = true;
            }
            self.pos += 1;
            if matches!(self.peek(0), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            while matches!(self.peek(0), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
        }

        let kind = if is_f32 {
            TokKind::Float32
        } else if is_float {
            TokKind::Float
        } else {
            TokKind::Integer
        };
        self.push(kind, start, self.pos);
    }

    /// Advance past a run of digits accepted by `is_digit`, with `_` allowed as
    /// a digit separator anywhere within the run.
    fn consume_digits(&mut self, is_digit: impl Fn(u8) -> bool) {
        while matches!(self.peek(0), Some(c) if is_digit(c) || c == b'_') {
            self.pos += 1;
        }
    }

    fn lex_ident_or_keyword(&mut self, start: usize) {
        self.scan_ident();
        // Non-standard literal: an identifier immediately before `"`/`` ` `` with
        // no intervening whitespace is a prefix (`r"..."`, `raw"..."`, `` v`...` ``).
        // Keywords are never prefixes.
        if matches!(self.peek(0), Some(b'"' | b'`'))
            && keyword_kind(&self.input[start..self.pos]).is_none()
        {
            self.push(TokKind::StringPrefix, start, self.pos);
            let open = self.pos;
            if self.peek(0) == Some(b'"') {
                self.lex_open_string(open, true);
            } else {
                self.lex_open_cmd(open, true);
            }
            return;
        }
        let text = &self.input[start..self.pos];
        let kind = keyword_kind(text).unwrap_or(TokKind::Ident);
        self.push(kind, start, self.pos);
    }

    /// Lex a bare `$ident` interpolation operand. Unlike [`Self::lex_ident_or_keyword`]
    /// this never treats a following quote as a prefix, so the closing quote of
    /// `"$x"` is not mistaken for the start of a non-standard literal.
    fn lex_interp_ident(&mut self) {
        let start = self.pos;
        self.scan_ident();
        self.push(TokKind::Ident, start, self.pos);
    }

    /// Advance `pos` over a full identifier (the first char is already known to
    /// be an identifier start).
    fn scan_ident(&mut self) {
        self.pos += self.char_at(self.pos).len_utf8();
        loop {
            let c = self.char_at(self.pos);
            if self.pos < self.bytes.len() && is_ident_continue(c) {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
    }

    fn lex_operator_or_unknown(&mut self, start: usize) {
        let b0 = self.peek(0);
        let b1 = self.peek(1);

        // Three-char operators first (longest match): the `...` splat/vararg.
        if (b0, b1, self.peek(2)) == (Some(b'.'), Some(b'.'), Some(b'.')) {
            self.pos += 3;
            self.push(TokKind::DotDotDot, start, self.pos);
            return;
        }

        // The `..` range operator (after `...`, before the broadcast-dot block
        // so a bare `..` isn't mistaken for a dotted operator or two lone dots).
        if (b0, b1) == (Some(b'.'), Some(b'.')) {
            self.pos += 2;
            self.push(TokKind::DotDot, start, self.pos);
            return;
        }

        // Broadcasting (dotted) operators: a `.` immediately followed by an
        // operator char. We merge only `.`+operator — never `.`+ident (`a.b`),
        // `.(` (`f.(x)` stays `Dot LParen`), `..`, or `...` (matched above) — so
        // field access, the `@.` macro, and splat are all untouched. Longest
        // match: try the 3-char dotted comparisons before the 2-char ops.
        if b0 == Some(b'.') {
            // The lone 4-char dotted op `.//=` must beat the 3-char `.//`.
            if (b1, self.peek(2), self.peek(3)) == (Some(b'/'), Some(b'/'), Some(b'=')) {
                self.pos += 4;
                self.push(TokKind::DotSlashSlashEq, start, self.pos);
                return;
            }
            let dotted3 = match (b1, self.peek(2)) {
                (Some(b'='), Some(b'=')) => Some(TokKind::DotEqEq),
                (Some(b'!'), Some(b'=')) => Some(TokKind::DotNotEq),
                (Some(b'<'), Some(b'=')) => Some(TokKind::DotLe),
                (Some(b'>'), Some(b'=')) => Some(TokKind::DotGe),
                (Some(b'/'), Some(b'/')) => Some(TokKind::DotSlashSlash),
                (Some(b'='), Some(b'>')) => Some(TokKind::DotFatArrow),
                (Some(b'&'), Some(b'&')) => Some(TokKind::DotAndAnd),
                (Some(b'|'), Some(b'|')) => Some(TokKind::DotOrOr),
                // Broadcast augmented assignment `.op=`.
                (Some(b'+'), Some(b'=')) => Some(TokKind::DotPlusEq),
                (Some(b'-'), Some(b'=')) => Some(TokKind::DotMinusEq),
                (Some(b'*'), Some(b'=')) => Some(TokKind::DotStarEq),
                (Some(b'/'), Some(b'=')) => Some(TokKind::DotSlashEq),
                (Some(b'^'), Some(b'=')) => Some(TokKind::DotCaretEq),
                (Some(b'%'), Some(b'=')) => Some(TokKind::DotPercentEq),
                _ => None,
            };
            if let Some(kind) = dotted3 {
                self.pos += 3;
                self.push(kind, start, self.pos);
                return;
            }
            let dotted2 = match b1 {
                Some(b'+') => Some(TokKind::DotPlus),
                Some(b'-') => Some(TokKind::DotMinus),
                Some(b'*') => Some(TokKind::DotStar),
                Some(b'/') => Some(TokKind::DotSlash),
                Some(b'^') => Some(TokKind::DotCaret),
                Some(b'%') => Some(TokKind::DotPercent),
                Some(b'=') => Some(TokKind::DotEq),
                Some(b'<') => Some(TokKind::DotLt),
                Some(b'>') => Some(TokKind::DotGt),
                Some(b'~') => Some(TokKind::DotTilde),
                _ => None,
            };
            if let Some(kind) = dotted2 {
                self.pos += 2;
                self.push(kind, start, self.pos);
                return;
            }
            // A lone `.` (or `..`) falls through to the single-char table below.
        }

        // The lone 3-char ASCII op `//=` must beat the 2-char `//`.
        if (b0, b1, self.peek(2)) == (Some(b'/'), Some(b'/'), Some(b'=')) {
            self.pos += 3;
            self.push(TokKind::SlashSlashEq, start, self.pos);
            return;
        }

        // Two-char operators next (longest match).
        let two = match (b0, b1) {
            (Some(b'/'), Some(b'/')) => Some(TokKind::SlashSlash),
            (Some(b'='), Some(b'=')) => Some(TokKind::EqEq),
            (Some(b'='), Some(b'>')) => Some(TokKind::FatArrow),
            (Some(b'!'), Some(b'=')) => Some(TokKind::NotEq),
            (Some(b'<'), Some(b'=')) => Some(TokKind::Le),
            (Some(b'>'), Some(b'=')) => Some(TokKind::Ge),
            (Some(b'&'), Some(b'&')) => Some(TokKind::AndAnd),
            (Some(b'|'), Some(b'|')) => Some(TokKind::OrOr),
            (Some(b':'), Some(b':')) => Some(TokKind::ColonColon),
            (Some(b'<'), Some(b':')) => Some(TokKind::Subtype),
            (Some(b'>'), Some(b':')) => Some(TokKind::Supertype),
            (Some(b'-'), Some(b'>')) => Some(TokKind::Arrow),
            (Some(b'|'), Some(b'>')) => Some(TokKind::PipeGt),
            // Augmented assignment `op=`.
            (Some(b'+'), Some(b'=')) => Some(TokKind::PlusEq),
            (Some(b'-'), Some(b'=')) => Some(TokKind::MinusEq),
            (Some(b'*'), Some(b'=')) => Some(TokKind::StarEq),
            (Some(b'/'), Some(b'=')) => Some(TokKind::SlashEq),
            (Some(b'^'), Some(b'=')) => Some(TokKind::CaretEq),
            (Some(b'%'), Some(b'=')) => Some(TokKind::PercentEq),
            (Some(b'|'), Some(b'=')) => Some(TokKind::PipeEq),
            (Some(b'&'), Some(b'=')) => Some(TokKind::AmpEq),
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
            Some(b'~') => Some(TokKind::Tilde),
            Some(b'?') => Some(TokKind::Question),
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
        "while" => TokKind::WhileKw,
        "for" => TokKind::ForKw,
        "do" => TokKind::DoKw,
        "let" => TokKind::LetKw,
        "quote" => TokKind::QuoteKw,
        "try" => TokKind::TryKw,
        "catch" => TokKind::CatchKw,
        "finally" => TokKind::FinallyKw,
        "struct" => TokKind::StructKw,
        "mutable" => TokKind::MutableKw,
        "module" => TokKind::ModuleKw,
        "baremodule" => TokKind::BaremoduleKw,
        "return" => TokKind::ReturnKw,
        "break" => TokKind::BreakKw,
        "continue" => TokKind::ContinueKw,
        "const" => TokKind::ConstKw,
        "global" => TokKind::GlobalKw,
        "local" => TokKind::LocalKw,
        "import" => TokKind::ImportKw,
        "using" => TokKind::UsingKw,
        "export" => TokKind::ExportKw,
        "where" => TokKind::WhereKw,
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
            "for i in 1:10\n    x\nend\n",
            "while x > 0\n    x\nend\n",
            "map(xs) do x, y\n    x + y\nend\n",
            "try\n    f()\ncatch e\n    g()\nfinally\n    h()\nend\n",
            "mutable struct Counter\n    n\nend\n",
            "module M\nend\n",
            "α = β + 1\n",
            "0x1f + 0b1010\n",
            "x = 0o755\ny = 0x1.8p3\nz = 1f0\n",
            "r = 3//4 + 1_000\nq = a .// b\n",
            "n = 1.5e-3\nm = .5\nk = 2.\n",
            "s = \"a$x b\"\n",
            "s = \"a$(f(x))b\"\n",
            "s = \"\"\"x$(y)\"\"\"\n",
            "c = `echo $x`\n",
            "r = raw\"\\d+\"\n",
            "m = r\"pat\"ims\n",
            "v = v\"1.2.3\"\n",
            "b = b\"\\x00\"\n",
            "s = \"$foo.bar\"\n",
            "s = \"\\$lit\"\n",
            "s = \"$$\"\n",
            "s = \"unterminated\n",
            "s = \"$(g(\"nested\"))\"\n",
        ] {
            assert!(roundtrips(input), "did not round-trip: {input:?}");
        }
    }

    #[test]
    fn interpolation_with_bare_ident() {
        assert_eq!(
            kinds("\"a$x\""),
            vec![
                TokKind::StringDelimOpen,
                TokKind::StringContent,
                TokKind::Dollar,
                TokKind::Ident,
                TokKind::StringDelimClose,
            ]
        );
    }

    #[test]
    fn interpolation_with_parenthesized_expr() {
        assert_eq!(
            kinds("\"$(y)\""),
            vec![
                TokKind::StringDelimOpen,
                TokKind::Dollar,
                TokKind::LParen,
                TokKind::Ident,
                TokKind::RParen,
                TokKind::StringDelimClose,
            ]
        );
    }

    #[test]
    fn nested_parens_in_interpolation() {
        // `$(f(x))`: the inner `)` must not close the interpolation early.
        assert_eq!(
            kinds("\"$(f(x))\""),
            vec![
                TokKind::StringDelimOpen,
                TokKind::Dollar,
                TokKind::LParen,
                TokKind::Ident,
                TokKind::LParen,
                TokKind::Ident,
                TokKind::RParen,
                TokKind::RParen,
                TokKind::StringDelimClose,
            ]
        );
    }

    #[test]
    fn raw_literal_does_not_interpolate() {
        // `raw"..."` keeps `$x` and the backslash as literal content.
        assert_eq!(
            kinds("raw\"$x\\d\""),
            vec![
                TokKind::StringPrefix,
                TokKind::StringDelimOpen,
                TokKind::StringContent,
                TokKind::StringDelimClose,
            ]
        );
    }

    #[test]
    fn prefix_and_suffix_flags() {
        assert_eq!(
            kinds("r\"pat\"ims"),
            vec![
                TokKind::StringPrefix,
                TokKind::StringDelimOpen,
                TokKind::StringContent,
                TokKind::StringDelimClose,
                TokKind::StringSuffix,
            ]
        );
    }

    #[test]
    fn command_literal_interpolates() {
        assert_eq!(
            kinds("`cmd $x`"),
            vec![
                TokKind::CmdDelimOpen,
                TokKind::StringContent,
                TokKind::Dollar,
                TokKind::Ident,
                TokKind::CmdDelimClose,
            ]
        );
    }

    #[test]
    fn escaped_dollar_is_content() {
        // `\$` does not introduce an interpolation.
        assert_eq!(
            kinds("\"\\$x\""),
            vec![
                TokKind::StringDelimOpen,
                TokKind::StringContent,
                TokKind::StringDelimClose,
            ]
        );
    }

    #[test]
    fn plain_string_adjacent_ident_is_not_a_suffix() {
        // Only prefixed literals take a suffix; `"a"b` is a string then an ident.
        assert_eq!(
            kinds("\"a\"b"),
            vec![
                TokKind::StringDelimOpen,
                TokKind::StringContent,
                TokKind::StringDelimClose,
                TokKind::Ident,
            ]
        );
    }

    #[test]
    fn prefix_requires_adjacent_quote() {
        // A space between the ident and the quote means it is a plain variable.
        assert_eq!(
            kinds("r \"x\""),
            vec![
                TokKind::Ident,
                TokKind::Whitespace,
                TokKind::StringDelimOpen,
                TokKind::StringContent,
                TokKind::StringDelimClose,
            ]
        );
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

    #[test]
    fn numeric_literal_kinds() {
        // Decimal int, big int (still a plain integer token), and underscores.
        assert_eq!(kinds("123"), vec![TokKind::Integer]);
        assert_eq!(kinds("1_000"), vec![TokKind::Integer]);
        assert_eq!(
            kinds("12345678901234567890123456789"),
            vec![TokKind::Integer]
        );
        // Base-prefixed integers.
        assert_eq!(kinds("0x1f"), vec![TokKind::HexInt]);
        assert_eq!(kinds("0o755"), vec![TokKind::OctInt]);
        assert_eq!(kinds("0b1010"), vec![TokKind::BinInt]);
        // Floats: fractional, leading/trailing dot, scientific.
        assert_eq!(kinds("3.14"), vec![TokKind::Float]);
        assert_eq!(kinds(".5"), vec![TokKind::Float]);
        assert_eq!(kinds("2."), vec![TokKind::Float]);
        assert_eq!(kinds("1.5e-3"), vec![TokKind::Float]);
        // `f` exponent marks Float32; hex floats are Float64.
        assert_eq!(kinds("1f0"), vec![TokKind::Float32]);
        assert_eq!(kinds("2.5f-3"), vec![TokKind::Float32]);
        assert_eq!(kinds("0x1p0"), vec![TokKind::Float]);
        assert_eq!(kinds("0x1.8p3"), vec![TokKind::Float]);
    }

    #[test]
    fn rational_operators() {
        assert_eq!(
            kinds("3//4"),
            vec![TokKind::Integer, TokKind::SlashSlash, TokKind::Integer]
        );
        assert_eq!(
            kinds("a .// b"),
            vec![
                TokKind::Ident,
                TokKind::Whitespace,
                TokKind::DotSlashSlash,
                TokKind::Whitespace,
                TokKind::Ident,
            ]
        );
    }

    #[test]
    fn uppercase_hex_prefix_is_not_a_literal() {
        // Julia's base prefixes are lowercase only: `0X1` is `0` then ident `X1`.
        assert_eq!(kinds("0X1"), vec![TokKind::Integer, TokKind::Ident]);
    }

    #[test]
    fn inf_and_nan_are_identifiers() {
        // `Inf`/`NaN` are ordinary identifiers in Julia, not numeric literals.
        assert_eq!(kinds("Inf"), vec![TokKind::Ident]);
        assert_eq!(kinds("NaN"), vec![TokKind::Ident]);
        assert_eq!(kinds("Inf32"), vec![TokKind::Ident]);
    }

    #[test]
    fn subtype_and_supertype_operators() {
        assert_eq!(
            kinds("T<:U"),
            vec![TokKind::Ident, TokKind::Subtype, TokKind::Ident]
        );
        assert_eq!(
            kinds("T>:U"),
            vec![TokKind::Ident, TokKind::Supertype, TokKind::Ident]
        );
    }

    #[test]
    fn splat_is_three_dots() {
        assert_eq!(kinds("x..."), vec![TokKind::Ident, TokKind::DotDotDot]);
        // Longest match: `...` is the splat, `..` is the range operator.
        assert_eq!(kinds(".."), vec![TokKind::DotDot]);
        assert_eq!(
            kinds("a.b"),
            vec![TokKind::Ident, TokKind::Dot, TokKind::Ident]
        );
    }

    #[test]
    fn broadcasting_operators() {
        assert_eq!(
            kinds("a .+ b"),
            vec![
                TokKind::Ident,
                TokKind::Whitespace,
                TokKind::DotPlus,
                TokKind::Whitespace,
                TokKind::Ident
            ]
        );
        // Longest match: `.==` is `DotEqEq`, `.=` is `DotEq`.
        assert_eq!(kinds("x .== y").get(2), Some(&TokKind::DotEqEq));
        assert_eq!(kinds("x .= y").get(2), Some(&TokKind::DotEq));
        assert_eq!(kinds("a .<= b").get(2), Some(&TokKind::DotLe));
        // A `.` fuses to operators but never to an ident (`a.b` field access).
        assert_eq!(
            kinds("a.b"),
            vec![TokKind::Ident, TokKind::Dot, TokKind::Ident]
        );
        // `..` is its own range operator, not two lone dots or a broadcast `.`.
        assert_eq!(kinds(".."), vec![TokKind::DotDot]);
        // `f.(` stays `Dot LParen` so the parser can form a broadcast call.
        assert_eq!(
            kinds("f.(x)"),
            vec![
                TokKind::Ident,
                TokKind::Dot,
                TokKind::LParen,
                TokKind::Ident,
                TokKind::RParen
            ]
        );
    }

    #[test]
    fn where_is_a_keyword() {
        assert_eq!(keyword_kind("where"), Some(TokKind::WhereKw));
        assert_eq!(kinds("where"), vec![TokKind::WhereKw]);
    }
}
