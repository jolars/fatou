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
    /// A malformed numeric literal Julia still lexes as a single (error) token:
    /// a hex float whose `p`/`P` binary exponent has no digits (`0x1p`, `0x1p+`),
    /// or a hex constant with no mantissa digits at all (`0x`, `0xp3`). Projects
    /// to JuliaSyntax's `(ErrorInvalidNumericConstant)`.
    ErrorInvalidNumber,
    /// A hex float with a `.` fraction but no `p`/`P` binary exponent (`0x1.8`,
    /// `0x.8`, `0x1.`). Julia requires the exponent; projects to
    /// `(ErrorHexFloatMustContainP)`.
    ErrorHexFloatNoP,

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
    MacroKw,
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
    Backslash,
    SlashSlash,
    Caret,
    Percent,
    /// The wrapping arithmetic operators `+%`, `-%`, `*%` (Julia 1.14). They
    /// share the precedence and unary/binary classification of their unwrapped
    /// counterparts — `+%`/`-%` sit at the `+` tier and are both unary and
    /// binary, `*%` sits at the `*` tier and is binary-only — and each keeps its
    /// own name, so `a +% b` projects `(call-i a +% b)`.
    PlusPercent,
    /// The concatenation-style operator `++`: a real `+`-tier operator name in
    /// Julia (binary-only, and variadic like `+`), unlike the invalid doubled
    /// `--`/`**`.
    PlusPlus,
    MinusPercent,
    StarPercent,
    /// The invalid doubled operators `**` and `--` (and broadcast `.**`/`.--`).
    /// Julia has no `**` (power is `^`) nor `--`, so JuliaSyntax lexes each as a
    /// single error operator at a fixed low precedence tier (between `+` and `:`)
    /// and projects it as `(Error**)` / `(ErrorInvalidOperator)`. `-->` (the
    /// arrow) is matched before `--`, so it is unaffected.
    StarStar,
    MinusMinus,
    EqEq,
    NotEq,
    /// `===` (identity) and `!==` (its negation): 3-char comparison-tier
    /// operators that must beat `==`/`!=` in longest match.
    EqEqEq,
    NotEqEq,
    Lt,
    Le,
    Gt,
    Ge,
    AndAnd,
    OrOr,
    Colon,
    ColonColon,
    /// The assignment-tier operator `:=`. Right-associative and as loose as `=`,
    /// but keeps its own head (`(:= a b)`) like the Unicode `≔`, rather than
    /// lowering to an assignment.
    ColonEq,
    Subtype,
    Supertype,
    Arrow,
    /// The arrow operator `-->` (right-associative, own head `(--> a b)`).
    LongArrow,
    /// The arrow operator `<-->` (right-associative, ordinary `(call-i a <--> b)`).
    LeftRightArrow,
    /// The arrow operator `<--` (right-associative, ordinary `(call-i a <-- b)`).
    LeftLongArrow,
    /// The pair operator `=>`.
    FatArrow,
    /// Bitshift operators `<<`, `>>`, `>>>` (left-associative).
    Shl,
    Shr,
    UShr,
    // Augmented (compound) assignment operators `op=`. Right-associative and at
    // the same precedence as `=`; modeled as `ASSIGNMENT_EXPR`.
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    BackslashEq,
    SlashSlashEq,
    CaretEq,
    PercentEq,
    /// Augmented assignment for the wrapping arithmetic operators: `+%=`, `-%=`,
    /// `*%=`.
    PlusPercentEq,
    MinusPercentEq,
    StarPercentEq,
    PipeEq,
    /// The augmented assignment `$=` (Julia's historical xor-assign, still a
    /// valid operator name — `base/show.jl` lists it among the infix operators).
    DollarEq,
    AmpEq,
    /// Bitshift augmented assignment `<<=`, `>>=`, `>>>=`.
    ShlEq,
    ShrEq,
    UShrEq,
    /// The Unicode augmented assignments `÷=` (integer-divide) and `⊻=` (xor).
    /// These two are the only Unicode operators with an augmented-assign form.
    DivEq,
    XorEq,
    Dot,
    /// The `..` range/interval operator (infix `a..b`).
    DotDot,
    DotDotDot,
    PipeGt,
    /// The left-pipe operator `<|` (right-associative).
    PipeLt,
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
    /// Broadcast forms of the invalid doubled operators `.**`/`.--` (project
    /// `(dotcall-i a (Error**) b)` / `(dotcall-i a (ErrorInvalidOperator) b)`).
    DotStarStar,
    DotMinusMinus,
    DotSlash,
    DotBackslash,
    DotSlashSlash,
    DotCaret,
    DotPercent,
    DotEq,
    DotEqEq,
    DotNotEq,
    /// The broadcast identity/inequality operators `.===`/`.!==` (project
    /// `(dotcall-i a === b)`/`(dotcall-i a !== b)`).
    DotEqEqEq,
    DotNotEqEq,
    DotLt,
    DotLe,
    DotGt,
    DotGe,
    /// The broadcast bitshift operators `.<<`/`.>>`/`.>>>` (project
    /// `(dotcall-i a << b)` and so on). Their augmented forms `.<<=`/`.>>=`/
    /// `.>>>=` are `DotShlEq`/`DotShrEq`/`DotUShrEq`.
    DotShl,
    DotShr,
    DotUShr,
    /// The broadcast type-comparison operators `.<:`/`.>:` (project
    /// `(dotcall-i a <: b)`/`(dotcall-i a >: b)`).
    DotSubtype,
    DotSupertype,
    /// The broadcast pair operator `.=>`.
    DotFatArrow,
    /// The broadcast arrow operator `.-->` (projects `(dotcall-i a --> b)`).
    DotLongArrow,
    /// The broadcast arrow operators `.<--`/`.<-->` (project
    /// `(dotcall-i a <-- b)`/`(dotcall-i a <--> b)`).
    DotLeftLongArrow,
    DotLeftRightArrow,
    /// The broadcast left-pipe-to-right `.|>` (projects `(dotcall-i a |> b)`).
    DotPipeGt,
    /// The broadcast `~` operator `.~`.
    DotTilde,
    /// The broadcast short-circuit operators `.&&` and `.||`.
    DotAndAnd,
    DotOrOr,
    /// The broadcast bitwise operators `.&` and `.|`.
    DotAmp,
    DotPipe,
    /// The broadcast unary-not operator `.!` (prefix-only, like plain `!`).
    DotBang,
    // Broadcast augmented assignment `.op=` (e.g. `.+=`). Same precedence and
    // modeling as the undotted forms.
    DotPlusEq,
    DotMinusEq,
    DotStarEq,
    DotSlashEq,
    DotBackslashEq,
    DotSlashSlashEq,
    DotCaretEq,
    DotPercentEq,
    DotAmpEq,
    DotPipeEq,
    /// Broadcast bitshift augmented assignment `.<<=`, `.>>=`, `.>>>=`.
    DotShlEq,
    DotShrEq,
    DotUShrEq,
    /// Broadcast forms of the Unicode augmented assignments `.÷=`, `.⊻=`.
    DotDivEq,
    DotXorEq,

    // Single-codepoint Unicode operators, grouped by precedence tier. The exact
    // operator text is carried by the token; the parser only needs the tier (for
    // binding power) and the projector reads the text. The lexer classifies each
    // operator char via the generated `unicode_op_kind` table.
    UniArrow,
    UniComparison,
    UniColon,
    UniPlus,
    UniTimes,
    UniPower,
    UniAssign,
    /// Prefix-only Unicode operators `¬ √ ∛ ∜` (the `unicode_ops` tier).
    UniRadical,

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
                | TokKind::MacroKw
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

    /// Push an operator token, first absorbing any trailing sub/superscript or
    /// prime suffix into its text when the operator kind takes one (`+₁`,
    /// `-->₁`, `f'ᵀ`). The kind is unchanged — only the text grows — so the
    /// parser still keys on the precedence tier and the projector reads the full
    /// operator text.
    fn push_op(&mut self, kind: TokKind, start: usize) {
        if op_takes_suffix(kind) {
            while self.pos < self.bytes.len() {
                let c = self.char_at(self.pos);
                if is_op_suffix_char(c) {
                    self.pos += c.len_utf8();
                } else {
                    break;
                }
            }
        }
        self.push(kind, start, self.pos);
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
            b'\'' if self.prev_ends_value() || self.prev_is_dot() => {
                self.pos += 1;
                self.push_op(TokKind::Transpose, start);
            }
            b'\'' => self.lex_char_literal(start),
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
        // or EOF. A newline does *not* stop it, single-quoted strings included
        // (`"a\nb"` is one content run), which is why an unterminated literal
        // always runs to the end of the file.
        while self.pos < self.bytes.len() {
            // In a raw (prefixed) string, a backslash run immediately before the
            // closing quote escapes it when the run length is odd (Julia's
            // raw-string rule: `\"` ⇒ literal quote, `\\\"` ⇒ `\` then literal
            // quote). Consume the run plus the escaped quote so it stays content.
            if raw && self.peek(0) == Some(b'\\') {
                let mut run = 0;
                while self.peek(run) == Some(b'\\') {
                    run += 1;
                }
                if self.peek(run) == Some(quote) && run % 2 == 1 {
                    self.pos += run + 1;
                } else {
                    self.pos += run;
                }
                continue;
            }
            if self.at_close_delim(quote, triple) {
                break;
            }
            if !raw && self.peek(0) == Some(b'$') && self.is_interp_start(1) {
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

    /// Consume one body character. In non-raw mode a backslash escapes the
    /// next character (so `\"`, `\$`, `\n` stay inside the content chunk) —
    /// the whole character, since an invalid escape may name a multi-byte one
    /// (`"\α"`). A `\`-newline line continuation may span a CRLF, so the
    /// whole `\r\n` is consumed with the backslash — otherwise the trailing
    /// `\n` would leak out and terminate a single-line string.
    fn consume_body_byte(&mut self, raw: bool) {
        if !raw && self.peek(0) == Some(b'\\') && self.pos + 1 < self.bytes.len() {
            if self.peek(1) == Some(b'\r') && self.peek(2) == Some(b'\n') {
                self.pos += 3;
            } else {
                self.pos += 1 + self.char_at(self.pos + 1).len_utf8();
            }
        } else {
            self.pos += self.char_at(self.pos).len_utf8();
        }
    }

    /// After a prefixed literal closes, lex an identifier-shaped flag suffix as a
    /// single suffix token (e.g. the `ims` in `r"pat"ims`, the `i2` in `x"s"i2`).
    /// The suffix must start with a letter; a digit-led suffix is a numeric macro
    /// argument instead (handled in `parse_string_literal`), not a flag string.
    fn lex_suffix(&mut self) {
        let start = self.pos;
        if matches!(self.peek(0), Some(c) if c.is_ascii_alphabetic()) {
            self.pos += 1;
            while matches!(self.peek(0), Some(c) if c.is_ascii_alphanumeric()) {
                self.pos += 1;
            }
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

    /// Whether the immediately preceding token is a field-access `.`. A `'`
    /// directly after a `.` is the removed `.'` transpose operator (`f.'`), not
    /// the start of a char literal: JuliaSyntax lexes it as a prime token and
    /// recovers `.'` as trailing junk (`f.'` ⇒ `f (error-t ')`). As with
    /// [`Self::prev_ends_value`] the check is on the *immediately* preceding
    /// token, so a space (`f. '`) leaves the `'` a char literal.
    fn prev_is_dot(&self) -> bool {
        self.tokens.last().map(|t| t.kind) == Some(TokKind::Dot)
    }

    /// `'` begins a char literal when it is *not* a postfix adjoint/transpose
    /// (see [`Self::prev_ends_value`]). It is always lexed as a [`TokKind::Char`]:
    /// a closing `'` (within one char or a backslash escape) terminates it; a
    /// newline or end of input without one leaves an *unterminated* char token
    /// spanning the opening quote and any content. JuliaSyntax also reads the
    /// unterminated form as a char and recovers with a missing-close marker
    /// (`'` ⇒ `(char (error))`, `'a` ⇒ `(char 'a' (error-t))`); the parser flags
    /// it with `UnterminatedLiteral` and the projector replays that shape.
    fn lex_char_literal(&mut self, start: usize) {
        // Scan to the closing `'`, skipping a backslash escape's following byte
        // so an escaped quote (`'\''`) does not terminate the literal. The
        // content may be several escapes (`'\xce\xb1'`) or over-long (`'ab'`);
        // validity (single codepoint, well-formed escapes) is decided later. A
        // newline is *content*, not a terminator — Julia scans to the next `'`
        // or end of input (`'\n'` is the newline char; `'a\n` is unterminated
        // over-long content), so the only stop short of `'` is EOF.
        let mut idx = self.pos + 1;
        let mut found = false;
        while idx < self.bytes.len() {
            match self.bytes[idx] {
                b'\'' => {
                    found = true;
                    break;
                }
                b'\\' => {
                    idx += 1;
                    if idx < self.bytes.len() {
                        idx += self.char_at(idx).len_utf8();
                    }
                }
                _ => idx += self.char_at(idx).len_utf8(),
            }
        }

        // Include the closing quote when present; otherwise span only the opening
        // quote and content (no close) so the parser can detect the unterminated
        // form from the token text.
        self.pos = if found { idx + 1 } else { idx };
        self.push(TokKind::Char, start, self.pos);
    }

    fn lex_number(&mut self, start: usize) {
        // Base-prefixed integers (`0x`, `0o`, `0b`). Julia's prefixes are
        // lowercase only, so `0X1` is *not* a hex literal — it falls through to
        // the decimal path and lexes as `0` followed by the identifier `X1`.
        if self.peek(0) == Some(b'0') {
            match self.peek(1) {
                Some(b'x') => {
                    self.pos += 2;
                    // Hex literal classification mirrors Julia's tokenizer. A
                    // `.`-fraction or `p`/`P` binary exponent makes it a (always
                    // Float64) hex float, but Julia constrains the shape: a hex
                    // literal needs at least one mantissa digit (integer or
                    // fractional), a `.` fraction requires a `p`/`P` exponent, and
                    // a `p`/`P` requires at least one exponent digit. Each
                    // violation is one of JuliaSyntax's two numeric error tokens,
                    // not a valid literal (e.g. `0x1p` is `(ErrorInvalidNumeric-
                    // Constant)`, `0x1.8` is `(ErrorHexFloatMustContainP)`).
                    let int_digits = self.consume_digits(|c| c.is_ascii_hexdigit());
                    // A `.` followed by another `.` is the `..` range operator
                    // (`0x1..n`), not a decimal point.
                    let mut had_dot = false;
                    let mut frac_digits = false;
                    if self.peek(0) == Some(b'.') && self.peek(1) != Some(b'.') {
                        had_dot = true;
                        self.pos += 1;
                        frac_digits = self.consume_digits(|c| c.is_ascii_hexdigit());
                    }
                    let mut had_exponent = false;
                    let mut exp_digits = false;
                    if matches!(self.peek(0), Some(b'p' | b'P')) {
                        had_exponent = true;
                        self.pos += 1;
                        if matches!(self.peek(0), Some(b'+' | b'-')) {
                            self.pos += 1;
                        }
                        exp_digits = self.consume_digits(|c| c.is_ascii_digit());
                    }
                    let has_mantissa = int_digits || frac_digits;
                    let kind = if had_exponent {
                        if has_mantissa && exp_digits {
                            TokKind::Float
                        } else {
                            TokKind::ErrorInvalidNumber
                        }
                    } else if had_dot {
                        TokKind::ErrorHexFloatNoP
                    } else if int_digits {
                        TokKind::HexInt
                    } else {
                        TokKind::ErrorInvalidNumber
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
        // optional sign. The marker only belongs to the number when a digit or
        // a sign follows it; otherwise it starts an identifier and the number
        // ends here (`3E₁` is `3 * E₁`, not a malformed float, and `3f` is
        // `3 * f`).
        if matches!(self.peek(0), Some(b'e' | b'E' | b'f'))
            && matches!(self.peek(1), Some(c) if c.is_ascii_digit() || c == b'+' || c == b'-')
        {
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
    /// a digit separator anywhere within the run. Returns whether at least one
    /// real (non-`_`) digit was consumed, which the hex path uses to tell a
    /// digit-bearing mantissa/exponent from an empty one (`0x1p` vs `0x1p3`).
    fn consume_digits(&mut self, is_digit: impl Fn(u8) -> bool) -> bool {
        let mut seen = false;
        while matches!(self.peek(0), Some(c) if is_digit(c) || c == b'_') {
            seen |= self.peek(0) != Some(b'_');
            self.pos += 1;
        }
        seen
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
            // A `!` immediately followed by `=` is the start of the `!=`/`!==`
            // operator, not an identifier suffix — stop here so the operator
            // lexer can claim it (`a!=b` ⇒ `a` `!=` `b`, `a!!=b` ⇒ `a!` `!=`
            // `b`), while a `!` followed by anything else stays in the
            // identifier (`a!b`, `push!`).
            if c == '!' && self.peek(1) == Some(b'=') {
                break;
            }
            if self.pos < self.bytes.len() && is_ident_continue(c) {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
    }

    /// Lex the operator at `start`, or one unknown byte if none matches.
    ///
    /// **Every arm below is ordered longest-match-first, and reordering them
    /// changes what the lexer accepts.** A shorter operator placed above a
    /// longer one that shares its prefix silently truncates it — `.>>` above
    /// `.>>>=` splits the latter into three tokens. The arm order *is* the
    /// longest-match rule; individual arms therefore do not restate it.
    /// `TODO.md` tracks replacing the ladder with a length-descending table,
    /// which would make that structural instead of positional.
    fn lex_operator_or_unknown(&mut self, start: usize) {
        let b0 = self.peek(0);
        let b1 = self.peek(1);

        // The `...` splat/vararg.
        if (b0, b1, self.peek(2)) == (Some(b'.'), Some(b'.'), Some(b'.')) {
            self.pos += 3;
            self.push_op(TokKind::DotDotDot, start);
            return;
        }

        // The `..` range operator (after `...`, before the broadcast-dot block
        // so a bare `..` isn't mistaken for a dotted operator or two lone dots).
        if (b0, b1) == (Some(b'.'), Some(b'.')) {
            self.pos += 2;
            self.push_op(TokKind::DotDot, start);
            return;
        }

        // Broadcasting (dotted) operators: a `.` immediately followed by an
        // operator char. We merge only `.`+operator — never `.`+ident (`a.b`),
        // `.(` (`f.(x)` stays `Dot LParen`), `..`, or `...` (matched above) — so
        // field access, the `@.` macro, and splat are all untouched.
        if b0 == Some(b'.') {
            // `.>>>=`, the broadcast unsigned-shift assignment.
            if (b1, self.peek(2), self.peek(3), self.peek(4))
                == (Some(b'>'), Some(b'>'), Some(b'>'), Some(b'='))
            {
                self.pos += 5;
                self.push_op(TokKind::DotUShrEq, start);
                return;
            }
            // `.>>>`, the broadcast unsigned shift (`.>>>=` is matched above, so
            // a 5th `=` never reaches here).
            if (b1, self.peek(2), self.peek(3)) == (Some(b'>'), Some(b'>'), Some(b'>')) {
                self.pos += 4;
                self.push_op(TokKind::DotUShr, start);
                return;
            }
            // `.//=`, the lone 4-char dotted op.
            if (b1, self.peek(2), self.peek(3)) == (Some(b'/'), Some(b'/'), Some(b'=')) {
                self.pos += 4;
                self.push_op(TokKind::DotSlashSlashEq, start);
                return;
            }
            // The 4-char broadcast bitshift assignments `.<<=`/`.>>=` beat the
            // dotted 2-char forms.
            if (b1, self.peek(2), self.peek(3)) == (Some(b'<'), Some(b'<'), Some(b'=')) {
                self.pos += 4;
                self.push_op(TokKind::DotShlEq, start);
                return;
            }
            if (b1, self.peek(2), self.peek(3)) == (Some(b'>'), Some(b'>'), Some(b'=')) {
                self.pos += 4;
                self.push_op(TokKind::DotShrEq, start);
                return;
            }
            // `.-->`, the broadcast arrow.
            if (b1, self.peek(2), self.peek(3)) == (Some(b'-'), Some(b'-'), Some(b'>')) {
                self.pos += 4;
                self.push_op(TokKind::DotLongArrow, start);
                return;
            }
            // `.<-->` and `.<--`, the broadcast left arrows.
            if (b1, self.peek(2), self.peek(3)) == (Some(b'<'), Some(b'-'), Some(b'-')) {
                if self.peek(4) == Some(b'>') {
                    self.pos += 5;
                    self.push_op(TokKind::DotLeftRightArrow, start);
                } else {
                    self.pos += 4;
                    self.push_op(TokKind::DotLeftLongArrow, start);
                }
                return;
            }
            // `.===`/`.!==`, the broadcast identity ops.
            if (b1, self.peek(2), self.peek(3)) == (Some(b'='), Some(b'='), Some(b'=')) {
                self.pos += 4;
                self.push_op(TokKind::DotEqEqEq, start);
                return;
            }
            if (b1, self.peek(2), self.peek(3)) == (Some(b'!'), Some(b'='), Some(b'=')) {
                self.pos += 4;
                self.push_op(TokKind::DotNotEqEq, start);
                return;
            }
            let dotted3 = match (b1, self.peek(2)) {
                (Some(b'='), Some(b'=')) => Some(TokKind::DotEqEq),
                (Some(b'!'), Some(b'=')) => Some(TokKind::DotNotEq),
                (Some(b'<'), Some(b'=')) => Some(TokKind::DotLe),
                (Some(b'>'), Some(b'=')) => Some(TokKind::DotGe),
                // Broadcast bitshift `.<<`/`.>>`. The augmented `.<<=`/`.>>=` and
                // the unsigned `.>>>` are matched above, so a doubled `<`/`>`
                // here is the plain shift.
                (Some(b'<'), Some(b'<')) => Some(TokKind::DotShl),
                (Some(b'>'), Some(b'>')) => Some(TokKind::DotShr),
                (Some(b'<'), Some(b':')) => Some(TokKind::DotSubtype),
                (Some(b'>'), Some(b':')) => Some(TokKind::DotSupertype),
                (Some(b'/'), Some(b'/')) => Some(TokKind::DotSlashSlash),
                // Broadcast invalid doubled operators `.**`/`.--` (the `.-->`
                // arrow is matched above, so `.--` here is never the arrow).
                (Some(b'*'), Some(b'*')) => Some(TokKind::DotStarStar),
                (Some(b'-'), Some(b'-')) => Some(TokKind::DotMinusMinus),
                (Some(b'='), Some(b'>')) => Some(TokKind::DotFatArrow),
                (Some(b'|'), Some(b'>')) => Some(TokKind::DotPipeGt),
                (Some(b'&'), Some(b'&')) => Some(TokKind::DotAndAnd),
                (Some(b'|'), Some(b'|')) => Some(TokKind::DotOrOr),
                // Broadcast augmented assignment `.op=`.
                (Some(b'+'), Some(b'=')) => Some(TokKind::DotPlusEq),
                (Some(b'-'), Some(b'=')) => Some(TokKind::DotMinusEq),
                (Some(b'*'), Some(b'=')) => Some(TokKind::DotStarEq),
                (Some(b'/'), Some(b'=')) => Some(TokKind::DotSlashEq),
                (Some(b'\\'), Some(b'=')) => Some(TokKind::DotBackslashEq),
                (Some(b'^'), Some(b'=')) => Some(TokKind::DotCaretEq),
                (Some(b'%'), Some(b'=')) => Some(TokKind::DotPercentEq),
                // Broadcast bitwise augmented assignment `.&=`/`.|=`. The `.&&`/
                // `.||` short-circuits and `.|>` pipe are matched above, so a
                // `&`/`|` followed by `=` here is the augmented-assign form.
                (Some(b'&'), Some(b'=')) => Some(TokKind::DotAmpEq),
                (Some(b'|'), Some(b'=')) => Some(TokKind::DotPipeEq),
                _ => None,
            };
            if let Some(kind) = dotted3 {
                self.pos += 3;
                self.push_op(kind, start);
                return;
            }
            let dotted2 = match b1 {
                Some(b'+') => Some(TokKind::DotPlus),
                Some(b'-') => Some(TokKind::DotMinus),
                Some(b'*') => Some(TokKind::DotStar),
                Some(b'/') => Some(TokKind::DotSlash),
                Some(b'\\') => Some(TokKind::DotBackslash),
                Some(b'^') => Some(TokKind::DotCaret),
                Some(b'%') => Some(TokKind::DotPercent),
                Some(b'=') => Some(TokKind::DotEq),
                Some(b'<') => Some(TokKind::DotLt),
                Some(b'>') => Some(TokKind::DotGt),
                Some(b'~') => Some(TokKind::DotTilde),
                // `.&&`/`.||`/`.|>` were already matched by the 3-char table, so a
                // lone `&`/`|` after the dot is the broadcast bitwise operator.
                Some(b'&') => Some(TokKind::DotAmp),
                Some(b'|') => Some(TokKind::DotPipe),
                // The broadcast unary-not `.!`. The `.!=`/`.!==` inequality ops
                // are matched above (they require a trailing `=`), so a lone `!`
                // after the dot is the prefix broadcast-not.
                Some(b'!') => Some(TokKind::DotBang),
                _ => None,
            };
            if let Some(kind) = dotted2 {
                self.pos += 2;
                self.push_op(kind, start);
                return;
            }
            // The broadcast minus sign `.−` and its augmented form `.−=`. U+2212
            // lexes as the ASCII `-` (see the undotted case below), so these are
            // plain `DotMinus`/`DotMinusEq` tokens spanning the sign.
            if matches!(b1, Some(b) if !b.is_ascii()) {
                let ch = self.char_at(self.pos + 1);
                if ch == MINUS_SIGN {
                    let eq = self.peek(1 + ch.len_utf8()) == Some(b'=');
                    let kind = if eq {
                        TokKind::DotMinusEq
                    } else {
                        TokKind::DotMinus
                    };
                    self.pos += 1 + ch.len_utf8() + usize::from(eq);
                    self.push_op(kind, start);
                    return;
                }
            }
            // The broadcast Unicode augmented assignments `.÷=`/`.⊻=` fuse `.` +
            // the operator + a trailing `=` into one token.
            if matches!(b1, Some(b) if !b.is_ascii()) {
                let ch = self.char_at(self.pos + 1);
                if matches!(ch, '\u{f7}' | '\u{22bb}') && self.peek(1 + ch.len_utf8()) == Some(b'=')
                {
                    let kind = if ch == '\u{f7}' {
                        TokKind::DotDivEq
                    } else {
                        TokKind::DotXorEq
                    };
                    self.pos += 1 + ch.len_utf8() + 1;
                    self.push_op(kind, start);
                    return;
                }
            }
            // A broadcast `.` fused to a single-codepoint Unicode infix operator
            // (`.…`, `.×`, `.⊕`, …) or to a prefix-only radical (`.√`, `.¬`):
            // emit the operator's precedence-tier kind on a token spanning `.op`.
            // The projector strips the leading `.` and heads `dotcall-i` (infix)
            // or `dotcall-pre` (radical). The assignment tier is not fused here —
            // it needs its own (currently deferred) projection.
            if b1.is_some_and(|b| !b.is_ascii()) {
                let ch = self.char_at(self.pos + 1);
                if let Some(kind) = unicode_op_kind(ch)
                    .filter(|&k| is_unicode_infix_tier(k) || k == TokKind::UniRadical)
                {
                    self.pos += 1 + ch.len_utf8();
                    self.push_op(kind, start);
                    return;
                }
            }
            // A lone `.` (or `..`) falls through to the single-char table below.
        }

        // `//=`, the lone 3-char ASCII op.
        if (b0, b1, self.peek(2)) == (Some(b'/'), Some(b'/'), Some(b'=')) {
            self.pos += 3;
            self.push_op(TokKind::SlashSlashEq, start);
            return;
        }

        // `<-->`, the 4-char arrow.
        if (b0, b1, self.peek(2), self.peek(3)) == (Some(b'<'), Some(b'-'), Some(b'-'), Some(b'>'))
        {
            self.pos += 4;
            self.push_op(TokKind::LeftRightArrow, start);
            return;
        }

        // `>>>=`, the unsigned-shift assignment.
        if (b0, b1, self.peek(2), self.peek(3)) == (Some(b'>'), Some(b'>'), Some(b'>'), Some(b'='))
        {
            self.pos += 4;
            self.push_op(TokKind::UShrEq, start);
            return;
        }

        // Three-char ASCII ops.
        let three = match (b0, b1, self.peek(2)) {
            (Some(b'-'), Some(b'-'), Some(b'>')) => Some(TokKind::LongArrow),
            // The left arrow `<--` (the 4-char `<-->` was matched above; a lone
            // `<-` stays `Lt` + unary minus).
            (Some(b'<'), Some(b'-'), Some(b'-')) => Some(TokKind::LeftLongArrow),
            (Some(b'>'), Some(b'>'), Some(b'>')) => Some(TokKind::UShr),
            // Identity `===` beats `==`; its negation `!==` beats `!=`.
            (Some(b'='), Some(b'='), Some(b'=')) => Some(TokKind::EqEqEq),
            (Some(b'!'), Some(b'='), Some(b'=')) => Some(TokKind::NotEqEq),
            // Bitshift augmented assignment (`>>>=` was matched above).
            (Some(b'<'), Some(b'<'), Some(b'=')) => Some(TokKind::ShlEq),
            (Some(b'>'), Some(b'>'), Some(b'=')) => Some(TokKind::ShrEq),
            // Augmented assignment for the wrapping operators.
            (Some(b'+'), Some(b'%'), Some(b'=')) => Some(TokKind::PlusPercentEq),
            (Some(b'-'), Some(b'%'), Some(b'=')) => Some(TokKind::MinusPercentEq),
            (Some(b'*'), Some(b'%'), Some(b'=')) => Some(TokKind::StarPercentEq),
            _ => None,
        };
        if let Some(kind) = three {
            self.pos += 3;
            self.push_op(kind, start);
            return;
        }

        // Two-char operators. The invalid doubled operators
        // `**`/`--` lex as single error tokens (`-->` is matched above as a
        // 3-char op, so `--` here is never the start of an arrow).
        let two = match (b0, b1) {
            (Some(b'*'), Some(b'*')) => Some(TokKind::StarStar),
            (Some(b'-'), Some(b'-')) => Some(TokKind::MinusMinus),
            (Some(b'/'), Some(b'/')) => Some(TokKind::SlashSlash),
            (Some(b'='), Some(b'=')) => Some(TokKind::EqEq),
            (Some(b'='), Some(b'>')) => Some(TokKind::FatArrow),
            (Some(b'!'), Some(b'=')) => Some(TokKind::NotEq),
            (Some(b'<'), Some(b'=')) => Some(TokKind::Le),
            (Some(b'>'), Some(b'=')) => Some(TokKind::Ge),
            (Some(b'&'), Some(b'&')) => Some(TokKind::AndAnd),
            (Some(b'|'), Some(b'|')) => Some(TokKind::OrOr),
            (Some(b':'), Some(b':')) => Some(TokKind::ColonColon),
            (Some(b':'), Some(b'=')) => Some(TokKind::ColonEq),
            (Some(b'<'), Some(b':')) => Some(TokKind::Subtype),
            (Some(b'>'), Some(b':')) => Some(TokKind::Supertype),
            (Some(b'-'), Some(b'>')) => Some(TokKind::Arrow),
            (Some(b'|'), Some(b'>')) => Some(TokKind::PipeGt),
            (Some(b'<'), Some(b'|')) => Some(TokKind::PipeLt),
            (Some(b'<'), Some(b'<')) => Some(TokKind::Shl),
            (Some(b'>'), Some(b'>')) => Some(TokKind::Shr),
            // Wrapping arithmetic `+%`/`-%`/`*%` (the `+%=` family was matched
            // by the 3-char table above). `--`/`->`/`**` are matched by earlier
            // arms, so a `%` after `+`/`-`/`*` is always the wrapping form.
            (Some(b'+'), Some(b'%')) => Some(TokKind::PlusPercent),
            // `++`. `+++` falls out as `++` then `+`, and `++=` as `++` then `=`
            // (there is no augmented `++=` form), both matching JuliaSyntax.
            (Some(b'+'), Some(b'+')) => Some(TokKind::PlusPlus),
            (Some(b'-'), Some(b'%')) => Some(TokKind::MinusPercent),
            (Some(b'*'), Some(b'%')) => Some(TokKind::StarPercent),
            // Augmented assignment `op=`.
            (Some(b'+'), Some(b'=')) => Some(TokKind::PlusEq),
            (Some(b'-'), Some(b'=')) => Some(TokKind::MinusEq),
            (Some(b'*'), Some(b'=')) => Some(TokKind::StarEq),
            (Some(b'/'), Some(b'=')) => Some(TokKind::SlashEq),
            (Some(b'\\'), Some(b'=')) => Some(TokKind::BackslashEq),
            (Some(b'^'), Some(b'=')) => Some(TokKind::CaretEq),
            (Some(b'%'), Some(b'=')) => Some(TokKind::PercentEq),
            (Some(b'|'), Some(b'=')) => Some(TokKind::PipeEq),
            // `$=`. A `$` elsewhere is the interpolation sigil, which never
            // precedes an `=`.
            (Some(b'$'), Some(b'=')) => Some(TokKind::DollarEq),
            (Some(b'&'), Some(b'=')) => Some(TokKind::AmpEq),
            _ => None,
        };
        if let Some(kind) = two {
            self.pos += 2;
            self.push_op(kind, start);
            return;
        }

        let one = match b0 {
            Some(b'=') => Some(TokKind::Eq),
            Some(b'+') => Some(TokKind::Plus),
            Some(b'-') => Some(TokKind::Minus),
            Some(b'*') => Some(TokKind::Star),
            Some(b'/') => Some(TokKind::Slash),
            Some(b'\\') => Some(TokKind::Backslash),
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
                self.push_op(kind, start);
            }
            None => {
                // A single-codepoint Unicode operator (`→`, `∈`, `√`, …): look
                // it up by char and emit its precedence-tier kind. The operator
                // text stays in the token; the parser keys on the tier.
                let ch = self.char_at(self.pos);
                // The two Unicode operators with an augmented-assign form: `÷=`
                // and `⊻=` fuse the trailing `=` into a single assignment token
                // (longest match, like the ASCII `op=` forms).
                if matches!(ch, '\u{f7}' | '\u{22bb}') && self.peek(ch.len_utf8()) == Some(b'=') {
                    let kind = if ch == '\u{f7}' {
                        TokKind::DivEq
                    } else {
                        TokKind::XorEq
                    };
                    self.pos += ch.len_utf8() + 1;
                    self.push_op(kind, start);
                } else if ch == MINUS_SIGN {
                    // U+2212 MINUS SIGN is the ASCII `-`: JuliaSyntax's tokenizer
                    // emits the `-` kind for it, with the augmented `−=` fused
                    // like `-=`. The multi-char `-> -- -%` forms are ASCII-only,
                    // so nothing else can follow.
                    let eq = self.peek(ch.len_utf8()) == Some(b'=');
                    let kind = if eq { TokKind::MinusEq } else { TokKind::Minus };
                    self.pos += ch.len_utf8() + usize::from(eq);
                    self.push_op(kind, start);
                } else if let Some(kind) = unicode_op_kind(ch) {
                    self.pos += ch.len_utf8();
                    self.push_op(kind, start);
                } else {
                    // Unknown: consume one full char to stay on a char boundary.
                    self.pos += ch.len_utf8();
                    self.push(TokKind::Unknown, start, self.pos);
                }
            }
        }
    }
}

/// Every Julia keyword, as written. The single source of truth for the set of
/// keywords, shared by the lexer's `keyword_kind` classification and the
/// language server's keyword completion. A test keeps it in step with
/// `keyword_kind`.
pub const KEYWORDS: &[&str] = &[
    "function",
    "macro",
    "end",
    "if",
    "elseif",
    "else",
    "begin",
    "true",
    "false",
    "while",
    "for",
    "do",
    "let",
    "quote",
    "try",
    "catch",
    "finally",
    "struct",
    "mutable",
    "module",
    "baremodule",
    "return",
    "break",
    "continue",
    "const",
    "global",
    "local",
    "import",
    "using",
    "export",
    "where",
];

fn keyword_kind(text: &str) -> Option<TokKind> {
    Some(match text {
        "function" => TokKind::FunctionKw,
        "macro" => TokKind::MacroKw,
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

/// Whether `c` may begin an identifier, mirroring JuliaSyntax's
/// `is_identifier_start_char` (`Base.is_id_start_char`). ASCII is handled inline;
/// non-ASCII code points defer to the generated `unicode_ident` tables.
pub fn is_ident_start(c: char) -> bool {
    if c.is_ascii() {
        c == '_' || c.is_ascii_alphabetic()
    } else {
        super::unicode_ident::is_unicode_ident_start(c)
    }
}

/// Whether `c` may continue an identifier, mirroring JuliaSyntax's
/// `is_identifier_char` (`Base.is_id_char`). ASCII is handled inline; non-ASCII
/// code points defer to the generated `unicode_ident` tables. The `!=`
/// operator split for a trailing `!` is handled by the caller in `scan_ident`.
pub fn is_ident_continue(c: char) -> bool {
    if c.is_ascii() {
        c == '_' || c == '!' || c.is_ascii_alphanumeric()
    } else {
        super::unicode_ident::is_unicode_ident_continue(c)
    }
}

/// The explicit operator-suffix characters Julia allows after an operator
/// (subscripts, superscripts, and primes), mirroring JuliaSyntax's `isopsuffix`
/// "additional allowed cases" set. The combining-mark categories (Mn/Mc/Me) that
/// `isopsuffix` also accepts are not handled here (a pragmatic subset, like
/// [`is_unicode_ident`]); these cover every realistic operator suffix.
const OP_SUFFIX_CHARS: &str = "²³¹ʰʲʳʷʸˡˢˣᴬᴮᴰᴱᴳᴴᴵᴶᴷᴸᴹᴺᴼᴾᴿᵀᵁᵂᵃᵇᵈᵉᵍᵏᵐᵒᵖᵗᵘᵛᵝᵞᵟᵠᵡᵢᵣᵤᵥᵦᵧᵨᵩᵪᶜᶠᶥᶦᶫᶰᶸᶻᶿ′″‴‵‶‷⁗⁰ⁱ⁴⁵⁶⁷⁸⁹⁺⁻⁼⁽⁾ⁿ₀₁₂₃₄₅₆₇₈₉₊₋₌₍₎ₐₑₒₓₕₖₗₘₙₚₛₜⱼⱽꜛꜜꜝ";

/// Whether `c` can extend an operator token as a sub/superscript or prime suffix
/// (`+₁`, `-->₁`, `f'ᵀ`). See [`OP_SUFFIX_CHARS`].
pub(crate) fn is_op_suffix_char(c: char) -> bool {
    OP_SUFFIX_CHARS.contains(c)
}

/// Whether an operator of the given kind may absorb trailing suffix characters.
/// Mirrors JuliaSyntax's `optakessuffix`: assignments, the short-circuit/type
/// operators, `: :: .. ... ! ~ -> ? $` and the radicals do **not** take a suffix;
/// the arithmetic/comparison/bitwise/arrow operators (and their broadcast forms)
/// do.
fn op_takes_suffix(kind: TokKind) -> bool {
    use TokKind::*;
    matches!(
        kind,
        Plus | Minus
            | Star
            | Slash
            | Backslash
            | SlashSlash
            | Caret
            | Percent
            | PlusPercent
            | MinusPercent
            | StarPercent
            | EqEq
            | NotEq
            | EqEqEq
            | NotEqEq
            | Lt
            | Le
            | Gt
            | Ge
            | Amp
            | Pipe
            | Shl
            | Shr
            | UShr
            | PipeGt
            | PipeLt
            | FatArrow
            | LongArrow
            | LeftRightArrow
            | LeftLongArrow
            | Transpose
            | DotPlus
            | DotMinus
            | DotStar
            | DotSlash
            | DotBackslash
            | DotSlashSlash
            | DotCaret
            | DotPercent
            | DotEqEq
            | DotNotEq
            | DotEqEqEq
            | DotNotEqEq
            | DotLt
            | DotLe
            | DotGt
            | DotGe
            | DotShl
            | DotShr
            | DotUShr
            | DotFatArrow
            | DotLongArrow
            | DotLeftLongArrow
            | DotLeftRightArrow
            | DotPipeGt
            | DotAmp
            | DotPipe
            | UniArrow
            | UniComparison
            | UniColon
            | UniPlus
            | UniTimes
            | UniPower
    )
}

/// U+2212 MINUS SIGN, which Julia treats as the ASCII `-`.
const MINUS_SIGN: char = '\u{2212}';

/// [`unicode_ops::unicode_op_kind`], extended with the operator chars Julia
/// folds onto an existing operator rather than giving a kind of their own: the
/// two middle dots `·` (U+00B7) and `·` (U+0387) both lex as the times-tier
/// `⋅` (U+22C5). They are absent from the generated table because it is keyed
/// on JuliaSyntax's *kinds*, where all three share the `⋅` entry; the
/// projector folds their text back to `⋅`.
fn unicode_op_kind(ch: char) -> Option<TokKind> {
    match ch {
        '\u{b7}' | '\u{387}' => Some(TokKind::UniTimes),
        _ => super::unicode_ops::unicode_op_kind(ch),
    }
}

/// Whether a Unicode operator tier is an infix `call-i` tier (so a broadcast
/// `.` may fuse to it as `dotcall-i`). Excludes the prefix-only radicals and the
/// assignment tier, whose broadcast forms are not modeled yet.
fn is_unicode_infix_tier(kind: TokKind) -> bool {
    use TokKind::*;
    matches!(
        kind,
        UniArrow | UniComparison | UniColon | UniPlus | UniTimes | UniPower
    )
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
    fn keywords_slice_agrees_with_keyword_kind() {
        // Every entry in the shared `KEYWORDS` slice must lex as a keyword, so
        // completion never offers a word the lexer treats as an identifier.
        for kw in KEYWORDS {
            assert_eq!(kinds(kw).len(), 1, "{kw} did not lex to a single token");
            assert!(
                keyword_kind(kw).is_some(),
                "{kw} is in KEYWORDS but not keyword_kind"
            );
        }
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
            // An invalid escape of a multi-byte character must not split it
            // (a byte-wise skip panics on the mid-character boundary).
            "s = \"a\\αb\"\n",
            "s = \"\\α",
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
        // A `!` stays in the identifier unless immediately followed by `=`, where
        // it begins the `!=`/`!==` operator instead (matching Julia's munch).
        assert_eq!(kinds("a!b"), vec![TokKind::Ident]);
        assert_eq!(
            kinds("a!=b"),
            vec![TokKind::Ident, TokKind::NotEq, TokKind::Ident]
        );
        assert_eq!(
            kinds("a!==b"),
            vec![TokKind::Ident, TokKind::NotEqEq, TokKind::Ident]
        );
        // Only the `!` before the `=` splits off: `a!` stays an identifier.
        assert_eq!(
            kinds("a!!=b"),
            vec![TokKind::Ident, TokKind::NotEq, TokKind::Ident]
        );
    }

    #[test]
    fn unicode_identifier_chars() {
        // Combining marks (category Mn) continue an identifier, so `x` + U+0304
        // is one token, not `x` followed by stray trivia. Regression for Flux's
        // gradient names like `x̄`, `ŷ`, `h̃` (smoke-test issue #17).
        assert_eq!(kinds("x\u{304}"), vec![TokKind::Ident]);
        assert_eq!(kinds("x\u{302}r"), vec![TokKind::Ident]);
        // Primes and a math symbol also continue an identifier (`α′`, `ρ∞`).
        assert_eq!(kinds("\u{3b1}\u{2032}"), vec![TokKind::Ident]);
        assert_eq!(kinds("\u{3c1}\u{221e}"), vec![TokKind::Ident]);
        // `∇` (U+2207) is a valid identifier *start* char (`∇batchnorm`).
        assert_eq!(kinds("\u{2207}batchnorm"), vec![TokKind::Ident]);
        // Every case lexes losslessly.
        assert!(roundtrips("x\u{304} = 1"));
        assert!(roundtrips("\u{2207}f(x) = x"));
    }

    #[test]
    fn identity_operators() {
        // `===`/`!==` beat `==`/`!=` in longest match.
        assert_eq!(
            kinds("a===b"),
            vec![TokKind::Ident, TokKind::EqEqEq, TokKind::Ident]
        );
        assert_eq!(
            kinds("a==b"),
            vec![TokKind::Ident, TokKind::EqEq, TokKind::Ident]
        );
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
        // A valid hex float needs a mantissa digit and, once a `p`/`P` appears,
        // an exponent digit; the fraction/exponent forms below are all valid.
        assert_eq!(kinds("0x1.p3"), vec![TokKind::Float]);
        assert_eq!(kinds("0x.8p3"), vec![TokKind::Float]);
        assert_eq!(kinds("0x1P+2"), vec![TokKind::Float]);
        assert_eq!(kinds("0x1_2p3"), vec![TokKind::Float]);
        // `e`/`f` are hex digits, not exponent markers, so no `p` means an int.
        assert_eq!(kinds("0x1e3"), vec![TokKind::HexInt]);
    }

    #[test]
    fn malformed_hex_literal_kinds() {
        // A `p`/`P` binary exponent with no digits is an invalid constant, as is
        // a hex literal with no mantissa digit at all (Julia keeps the whole run
        // as one error token rather than splitting it).
        for src in [
            "0x1p", "0x1p+", "0x1.8p", "0x1.p", "0x.p3", "0xp3", "0x", "0x_",
        ] {
            assert_eq!(
                kinds(src),
                vec![TokKind::ErrorInvalidNumber],
                "{src} should be an invalid numeric constant"
            );
        }
        // A `.` fraction with no `p`/`P` exponent must contain a `p`.
        for src in ["0x1.8", "0x1.", "0x.8", "0x.", "0x1.8e2"] {
            assert_eq!(
                kinds(src),
                vec![TokKind::ErrorHexFloatNoP],
                "{src} should require a p exponent"
            );
        }
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
    fn left_division_operators() {
        // `\` (left division) is a single-char infix operator; its augmented and
        // broadcast forms follow the slash family. The backslash byte must not be
        // confused with a string escape here (it never starts a string).
        assert_eq!(
            kinds("a\\b"),
            vec![TokKind::Ident, TokKind::Backslash, TokKind::Ident]
        );
        assert_eq!(
            kinds("a\\=b"),
            vec![TokKind::Ident, TokKind::BackslashEq, TokKind::Ident]
        );
        assert_eq!(
            kinds("a .\\ b"),
            vec![
                TokKind::Ident,
                TokKind::Whitespace,
                TokKind::DotBackslash,
                TokKind::Whitespace,
                TokKind::Ident,
            ]
        );
        assert_eq!(
            kinds("a.\\=b"),
            vec![TokKind::Ident, TokKind::DotBackslashEq, TokKind::Ident]
        );
    }

    #[test]
    fn left_arrow_operators() {
        // `<--` lexes as one arrow-tier token (longest match: `<-->` beats it,
        // and it beats `<` + `--`); likewise the broadcast `.<--`/`.<-->` beat
        // `.<`. A lone `<-` stays `<` + unary minus.
        assert_eq!(
            kinds("a<--b"),
            vec![TokKind::Ident, TokKind::LeftLongArrow, TokKind::Ident]
        );
        assert_eq!(
            kinds("a<-->b"),
            vec![TokKind::Ident, TokKind::LeftRightArrow, TokKind::Ident]
        );
        assert_eq!(
            kinds("a.<--b"),
            vec![TokKind::Ident, TokKind::DotLeftLongArrow, TokKind::Ident]
        );
        assert_eq!(
            kinds("a.<-->b"),
            vec![TokKind::Ident, TokKind::DotLeftRightArrow, TokKind::Ident]
        );
        assert_eq!(
            kinds("a<-b"),
            vec![TokKind::Ident, TokKind::Lt, TokKind::Minus, TokKind::Ident]
        );
    }

    #[test]
    fn compound_shift_and_unicode_assignments() {
        // Bitshift augmented assignments lex as one token (longest match beats
        // the bare shift `<<`/`>>`/`>>>` and, for `>>>=`, the unsigned shift).
        assert_eq!(
            kinds("a<<=b"),
            vec![TokKind::Ident, TokKind::ShlEq, TokKind::Ident]
        );
        assert_eq!(
            kinds("a>>=b"),
            vec![TokKind::Ident, TokKind::ShrEq, TokKind::Ident]
        );
        assert_eq!(
            kinds("a>>>=b"),
            vec![TokKind::Ident, TokKind::UShrEq, TokKind::Ident]
        );
        // The two Unicode augmented assignments `÷=` and `⊻=` fuse the `=`.
        assert_eq!(
            kinds("a÷=b"),
            vec![TokKind::Ident, TokKind::DivEq, TokKind::Ident]
        );
        assert_eq!(
            kinds("a⊻=b"),
            vec![TokKind::Ident, TokKind::XorEq, TokKind::Ident]
        );
        // Broadcast forms fuse the leading `.` as well.
        assert_eq!(
            kinds("a.<<=b"),
            vec![TokKind::Ident, TokKind::DotShlEq, TokKind::Ident]
        );
        assert_eq!(
            kinds("a.>>>=b"),
            vec![TokKind::Ident, TokKind::DotUShrEq, TokKind::Ident]
        );
        assert_eq!(
            kinds("a.÷=b"),
            vec![TokKind::Ident, TokKind::DotDivEq, TokKind::Ident]
        );
        assert_eq!(
            kinds("a.⊻=b"),
            vec![TokKind::Ident, TokKind::DotXorEq, TokKind::Ident]
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
        // Longest match: the 4-char `.===`/`.!==` beat the 3-char `.==`/`.!=`.
        assert_eq!(kinds("x .=== y").get(2), Some(&TokKind::DotEqEqEq));
        assert_eq!(kinds("x .!== y").get(2), Some(&TokKind::DotNotEqEq));
        // Broadcast unary-not `.!` — the `.!=`/`.!==` inequalities still win the
        // longest match, so a lone `!` after the dot is `DotBang`.
        assert_eq!(kinds(".!y").first(), Some(&TokKind::DotBang));
        assert_eq!(kinds("x .!= y").get(2), Some(&TokKind::DotNotEq));
        // The broadcast Unicode radicals fuse into one `UniRadical` token
        // spanning `.op`, like the infix tiers (`.×`) above.
        assert_eq!(kinds(".√[3]").first(), Some(&TokKind::UniRadical));
        assert_eq!(kinds(".¬a").first(), Some(&TokKind::UniRadical));
        // Broadcast bitwise augmented assignment `.&=`/`.|=` — distinct from the
        // `.&`/`.|` bitwise ops and the `.&&`/`.||`/`.|>` triples.
        assert_eq!(kinds("a .&= b").get(2), Some(&TokKind::DotAmpEq));
        assert_eq!(kinds("a .|= b").get(2), Some(&TokKind::DotPipeEq));
        assert_eq!(kinds("a .& b").get(2), Some(&TokKind::DotAmp));
        assert_eq!(kinds("a .| b").get(2), Some(&TokKind::DotPipe));
        assert_eq!(kinds("a .|> b").get(2), Some(&TokKind::DotPipeGt));
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
