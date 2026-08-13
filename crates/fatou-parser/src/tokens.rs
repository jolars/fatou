//! The token table: the one list both token enums and their mapping come from.
//!
//! A lexed token is named twice — once as a [`TokKind`](crate::parser) (what the
//! lexer produced) and once as a [`SyntaxKind`](crate::syntax::SyntaxKind) (what
//! the token is materialized as in the CST) — and the two are joined by
//! `syntax_kind_for`. Writing that pairing out three times is how a token ends
//! up lexed but unmapped, or mapped to the wrong kind.
//!
//! Instead, each site defines a generator macro and expands [`token_table`] with
//! it: the table below is handed to the generator as rows of
//! `TokKindVariant SYNTAX_KIND_VARIANT,` (with the row's doc comment, which lands
//! on both variants), and the generator emits whatever shape it needs — the
//! `TokKind` enum, the token half of `SyntaxKind`, or the mapping between them.
//! Adding a token is one row here.
//!
//! The keyword rows are not written here: they come from
//! [`keyword_table`](crate::keywords::keyword_table), spliced in by
//! [`token_table_rows`], so keywords stay a one-row change in `keywords.rs`.
//!
//! **Rows are 1:1**, and cannot be otherwise: each column *declares* its
//! variant, so two rows naming one `SyntaxKind` is a duplicate-variant error.
//! The handful of tokens that genuinely are not 1:1 — the six `call-i` Unicode
//! operator tiers, which all materialize as one `UNICODE_OP` — are therefore
//! declared literally by each generator, right after the rows, where the
//! collapsing can be explained. A `TokKind` declared that way still has to be
//! mapped by hand, but `syntax_kind_for`'s match is exhaustive, so forgetting
//! is a compile error rather than a wrong tree.
//!
//! The callback indirection is what lets the table live at the crate root: it
//! passes bare tokens, so it never has to name `TokKind` (private to the
//! `parser` module tree) or `SyntaxKind` itself.

/// Expand `$callback` with the token table.
///
/// `$callback` must be a macro accepting rows of
/// `$(#[$meta])* TokKindVariant SYNTAX_KIND_VARIANT,` and must already be in
/// scope (`macro_rules!` macros are textually scoped, so define it just above
/// the call).
macro_rules! token_table {
    ($callback:ident) => {
        crate::keywords::keyword_table! { [crate::tokens::token_table_rows] $callback }
    };
}

/// The token rows, with the keyword rows spliced in.
///
/// Invoked by [`token_table`] as
/// [`keyword_table`](crate::keywords::keyword_table)'s callback, so the keyword
/// half arrives as the `"text" TokKindVariant SYNTAX_KIND_VARIANT,` rows of the
/// keyword table and the rest is written out below. Not called directly.
macro_rules! token_table_rows {
    ($callback:ident $($kw_text:literal $kw:ident $kw_syn:ident,)*) => {
        $callback! {
            // --- Trivia ---
            Whitespace WHITESPACE,
            Newline NEWLINE,
            Comment COMMENT,
            BlockComment BLOCK_COMMENT,

            // --- Literals / identifiers ---
            Ident IDENT,
            Integer INTEGER,
            BinInt BIN_INT,
            OctInt OCT_INT,
            HexInt HEX_INT,
            Float FLOAT,
            Float32 FLOAT32,
            Char CHAR,
            /// A malformed numeric literal Julia still lexes as a single (error)
            /// token: a hex float whose `p`/`P` binary exponent has no digits
            /// (`0x1p`, `0x1p+`), or a hex constant with no mantissa digits at
            /// all (`0x`, `0xp3`). Projects to JuliaSyntax's
            /// `(ErrorInvalidNumericConstant)`.
            ErrorInvalidNumber ERROR_INVALID_NUMBER,
            /// A hex float with a `.` fraction but no `p`/`P` binary exponent
            /// (`0x1.8`, `0x.8`, `0x1.`). Julia requires the exponent; projects
            /// to `(ErrorHexFloatMustContainP)`.
            ErrorHexFloatNoP ERROR_HEX_FLOAT_NO_P,
            /// A stray character Julia does not recognize (a subscript that
            /// cannot start an identifier, a lone unknown glyph). Never dropped,
            /// which keeps losslessness a property of the lexer alone. Projects
            /// to `(ErrorUnknownCharacter)`.
            Unknown ERROR_UNKNOWN_CHAR,

            // --- String / command literal pieces ---
            // A single literal is lexed as a run of these (plus `Dollar`/`Ident`
            // and, inside `$(...)`, normal-mode tokens), which the parser
            // reassembles into a `STRING_LITERAL`/`CMD_LITERAL` node.
            StringDelimOpen STRING_DELIM_OPEN,
            StringDelimClose STRING_DELIM_CLOSE,
            CmdDelimOpen CMD_DELIM_OPEN,
            CmdDelimClose CMD_DELIM_CLOSE,
            /// A run of literal characters inside a string/command (escapes
            /// included).
            StringContent STRING_CONTENT,
            /// A non-standard literal prefix immediately before a quote, e.g.
            /// `r`, `raw`.
            StringPrefix STRING_PREFIX,
            /// Suffix flag letters immediately after a prefixed literal, e.g.
            /// `ims`.
            StringSuffix STRING_SUFFIX,

            // --- Keywords, from the shared keyword table ---
            $($kw $kw_syn,)*

            // --- Operators ---
            Eq EQ,
            Plus PLUS,
            Minus MINUS,
            Star STAR,
            Slash SLASH,
            Backslash BACKSLASH,
            SlashSlash SLASH_SLASH,
            Caret CARET,
            Percent PERCENT,
            /// The wrapping arithmetic operators `+%`, `-%`, `*%` (Julia 1.14).
            /// They share the precedence and unary/binary classification of
            /// their unwrapped counterparts — `+%`/`-%` sit at the `+` tier and
            /// are both unary and binary, `*%` sits at the `*` tier and is
            /// binary-only — and each keeps its own name, so `a +% b` projects
            /// `(call-i a +% b)`.
            PlusPercent PLUS_PERCENT,
            /// The concatenation-style operator `++`: a real `+`-tier operator
            /// name in Julia (binary-only, and variadic like `+`), unlike the
            /// invalid doubled `--`/`**`.
            PlusPlus PLUS_PLUS,
            MinusPercent MINUS_PERCENT,
            StarPercent STAR_PERCENT,
            /// The invalid doubled operator `**` (and broadcast `.**`). Julia has
            /// no `**` (power is `^`), so JuliaSyntax lexes it as a single error
            /// operator at a fixed low precedence tier (between `+` and `:`) and
            /// projects it as `(Error**)`.
            StarStar STAR_STAR,
            /// The invalid doubled operator `--` (and broadcast `.--`), lexed
            /// like `**` and projected as `(ErrorInvalidOperator)`. `-->` (the
            /// arrow) is matched before `--`, so it is unaffected.
            MinusMinus MINUS_MINUS,
            EqEq EQ_EQ,
            NotEq NOT_EQ,
            /// `===` (identity): a 3-char comparison-tier operator that must beat
            /// `==` in longest match.
            EqEqEq EQ_EQ_EQ,
            /// `!==`, the negation of `===`.
            NotEqEq NOT_EQ_EQ,
            Lt LT,
            Le LE,
            Gt GT,
            Ge GE,
            AndAnd AND_AND,
            OrOr OR_OR,
            Colon COLON,
            ColonColon COLON_COLON,
            /// The assignment-tier operator `:=`. Right-associative and as loose
            /// as `=`, but keeps its own head (`(:= a b)`) like the Unicode `≔`,
            /// rather than lowering to an assignment.
            ColonEq COLON_EQ,
            Subtype SUBTYPE,
            Supertype SUPERTYPE,
            Arrow ARROW,
            /// The arrow operator `-->` (right-associative, own head
            /// `(--> a b)`).
            LongArrow LONG_ARROW,
            /// The arrow operator `<-->` (right-associative, ordinary
            /// `(call-i a <--> b)`).
            LeftRightArrow LEFT_RIGHT_ARROW,
            /// The arrow operator `<--` (right-associative, ordinary
            /// `(call-i a <-- b)`).
            LeftLongArrow LEFT_LONG_ARROW,
            /// The pair operator `=>`.
            FatArrow FAT_ARROW,
            /// The bitshift operator `<<` (left-associative).
            Shl SHL,
            /// The bitshift operator `>>` (left-associative).
            Shr SHR,
            /// The bitshift operator `>>>` (left-associative).
            UShr USHR,

            // Augmented (compound) assignment operators `op=`. Right-associative
            // and at the same precedence as `=`; modeled as `ASSIGNMENT_EXPR`.
            PlusEq PLUS_EQ,
            MinusEq MINUS_EQ,
            StarEq STAR_EQ,
            SlashEq SLASH_EQ,
            BackslashEq BACKSLASH_EQ,
            SlashSlashEq SLASH_SLASH_EQ,
            CaretEq CARET_EQ,
            PercentEq PERCENT_EQ,
            /// Augmented assignment for the wrapping arithmetic operators:
            /// `+%=`, `-%=`, `*%=`.
            PlusPercentEq PLUS_PERCENT_EQ,
            MinusPercentEq MINUS_PERCENT_EQ,
            StarPercentEq STAR_PERCENT_EQ,
            PipeEq PIPE_EQ,
            /// The augmented assignment `$=` (Julia's historical xor-assign,
            /// still a valid operator name — `base/show.jl` lists it among the
            /// infix operators).
            DollarEq DOLLAR_EQ,
            AmpEq AMP_EQ,
            /// Bitshift augmented assignment `<<=`.
            ShlEq SHL_EQ,
            /// Bitshift augmented assignment `>>=`.
            ShrEq SHR_EQ,
            /// Bitshift augmented assignment `>>>=`.
            UShrEq USHR_EQ,
            /// The Unicode augmented assignment `÷=` (integer-divide). With
            /// `⊻=`, one of the only two Unicode operators with an
            /// augmented-assign form.
            DivEq DIV_EQ,
            /// The Unicode augmented assignment `⊻=` (xor).
            XorEq XOR_EQ,

            Dot DOT,
            /// The `..` range/interval operator (infix `a..b`).
            DotDot DOT_DOT,
            DotDotDot DOT_DOT_DOT,
            PipeGt PIPE_GT,
            /// The left-pipe operator `<|` (right-associative).
            PipeLt PIPE_LT,
            Bang BANG,
            Amp AMP,
            Pipe PIPE,
            /// The `~` operator (infix `a ~ b` and prefix `~a`).
            Tilde TILDE,
            Question QUESTION,
            /// Postfix transpose/adjoint `'` (only when it follows a value;
            /// otherwise a `'` opens a `Char` literal).
            Transpose TRANSPOSE,

            // --- Broadcasting (dotted) operators ---
            // A `.` fused to a following operator.
            DotPlus DOT_PLUS,
            DotMinus DOT_MINUS,
            DotStar DOT_STAR,
            /// Broadcast form of the invalid doubled `**` (projects
            /// `(dotcall-i a (Error**) b)`).
            DotStarStar DOT_STAR_STAR,
            /// Broadcast form of the invalid doubled `--` (projects
            /// `(dotcall-i a (ErrorInvalidOperator) b)`).
            DotMinusMinus DOT_MINUS_MINUS,
            DotSlash DOT_SLASH,
            DotBackslash DOT_BACKSLASH,
            DotSlashSlash DOT_SLASH_SLASH,
            DotCaret DOT_CARET,
            DotPercent DOT_PERCENT,
            DotEq DOT_EQ,
            DotEqEq DOT_EQ_EQ,
            DotNotEq DOT_NOT_EQ,
            /// The broadcast identity operator `.===` (projects
            /// `(dotcall-i a === b)`).
            DotEqEqEq DOT_EQ_EQ_EQ,
            /// The broadcast inequality operator `.!==` (projects
            /// `(dotcall-i a !== b)`).
            DotNotEqEq DOT_NOT_EQ_EQ,
            DotLt DOT_LT,
            DotLe DOT_LE,
            DotGt DOT_GT,
            DotGe DOT_GE,
            /// The broadcast bitshift operator `.<<` (projects
            /// `(dotcall-i a << b)`); the augmented forms `.<<=`/`.>>=`/`.>>>=`
            /// are their own kinds below.
            DotShl DOT_SHL,
            /// The broadcast bitshift operator `.>>`.
            DotShr DOT_SHR,
            /// The broadcast bitshift operator `.>>>`.
            DotUShr DOT_USHR,
            /// The broadcast type-comparison operator `.<:` (projects
            /// `(dotcall-i a <: b)`).
            DotSubtype DOT_SUBTYPE,
            /// The broadcast type-comparison operator `.>:`.
            DotSupertype DOT_SUPERTYPE,
            /// The broadcast pair operator `.=>`.
            DotFatArrow DOT_FAT_ARROW,
            /// The broadcast arrow operator `.-->` (projects
            /// `(dotcall-i a --> b)`).
            DotLongArrow DOT_LONG_ARROW,
            /// The broadcast arrow operator `.<--` (projects
            /// `(dotcall-i a <-- b)`).
            DotLeftLongArrow DOT_LEFT_LONG_ARROW,
            /// The broadcast arrow operator `.<-->`.
            DotLeftRightArrow DOT_LEFT_RIGHT_ARROW,
            /// The broadcast left-pipe-to-right `.|>` (projects
            /// `(dotcall-i a |> b)`).
            DotPipeGt DOT_PIPE_GT,
            /// The broadcast `~` operator `.~`.
            DotTilde DOT_TILDE,
            /// The broadcast short-circuit operator `.&&`.
            DotAndAnd DOT_AND_AND,
            /// The broadcast short-circuit operator `.||`.
            DotOrOr DOT_OR_OR,
            /// The broadcast bitwise operator `.&`.
            DotAmp DOT_AMP,
            /// The broadcast bitwise operator `.|`.
            DotPipe DOT_PIPE,
            /// The broadcast unary-not operator `.!` (prefix-only, like plain
            /// `!`).
            DotBang DOT_BANG,

            // Broadcast augmented assignment `.op=` (e.g. `.+=`). Same precedence
            // and modeling as the undotted forms.
            DotPlusEq DOT_PLUS_EQ,
            DotMinusEq DOT_MINUS_EQ,
            DotStarEq DOT_STAR_EQ,
            DotSlashEq DOT_SLASH_EQ,
            DotBackslashEq DOT_BACKSLASH_EQ,
            DotSlashSlashEq DOT_SLASH_SLASH_EQ,
            DotCaretEq DOT_CARET_EQ,
            DotPercentEq DOT_PERCENT_EQ,
            DotAmpEq DOT_AMP_EQ,
            DotPipeEq DOT_PIPE_EQ,
            /// Broadcast bitshift augmented assignment `.<<=`.
            DotShlEq DOT_SHL_EQ,
            /// Broadcast bitshift augmented assignment `.>>=`.
            DotShrEq DOT_SHR_EQ,
            /// Broadcast bitshift augmented assignment `.>>>=`.
            DotUShrEq DOT_USHR_EQ,
            /// Broadcast form of the Unicode augmented assignment `.÷=`.
            DotDivEq DOT_DIV_EQ,
            /// Broadcast form of the Unicode augmented assignment `.⊻=`.
            DotXorEq DOT_XOR_EQ,

            // --- Delimiters / punctuation ---
            LParen LPAREN,
            RParen RPAREN,
            LBracket LBRACKET,
            RBracket RBRACKET,
            LBrace LBRACE,
            RBrace RBRACE,
            Comma COMMA,
            Semicolon SEMICOLON,
            At AT,
            Dollar DOLLAR,

            // --- Single-codepoint Unicode operators ---
            // The exact operator text is carried by the token, so the kind only
            // has to name the precedence tier the parser binds by. The six
            // `call-i` tiers therefore share one `SyntaxKind` and are declared
            // beside each generator's use of this table; these two do not.
            /// The assignment-tier Unicode operators (`≔`, `⩴`, `≕`), which keep
            /// their own head rather than lowering to an assignment.
            UniAssign UNICODE_ASSIGN_OP,
            /// The prefix-only Unicode operators `¬ √ ∛ ∜` (the `unicode_ops`
            /// tier).
            UniRadical UNICODE_RADICAL,
        }
    };
}

pub(crate) use {token_table, token_table_rows};
