use rowan::GreenNodeBuilder;

use crate::keywords::keyword_table;
use crate::parser::events::Event;
use crate::parser::lexer::{TokKind, Token};
use crate::syntax::{SyntaxKind, SyntaxNode};

/// Build a lossless `rowan` CST from the token stream and the event stream.
pub(crate) fn build_tree(tokens: &[Token], events: &[Event]) -> SyntaxNode {
    #[cfg(debug_assertions)]
    debug_assert_balanced(events);

    let mut builder = GreenNodeBuilder::new();
    builder.start_node(SyntaxKind::ROOT.into());

    for event in events {
        match *event {
            Event::Start(kind) => builder.start_node(kind.into()),
            Event::Tok(idx) => push_token(&mut builder, &tokens[idx]),
            Event::Finish => builder.finish_node(),
        }
    }

    builder.finish_node();
    let green = builder.finish();
    SyntaxNode::new_root(green)
}

fn push_token(builder: &mut GreenNodeBuilder<'_>, tok: &Token) {
    builder.token(syntax_kind_for(tok.kind).into(), tok.text.as_str());
}

/// Debug-only guard that the event stream opens and closes in balance: every
/// [`Event::Start`] is matched by a later [`Event::Finish`], no `Finish`
/// underflows past the root, and the stream returns to depth zero. A leaked
/// `open()`/`precede` splice — an unclosed node or a stray `Finish` — otherwise
/// only surfaces as an opaque panic deep inside `rowan`'s builder (or, worse, a
/// silently misshapen tree); this catches it at the source with the offending
/// index. Compiled out of release builds.
#[cfg(debug_assertions)]
fn debug_assert_balanced(events: &[Event]) {
    let mut depth: i32 = 0;
    for (i, event) in events.iter().enumerate() {
        match event {
            Event::Start(_) => depth += 1,
            Event::Finish => {
                depth -= 1;
                assert!(
                    depth >= 0,
                    "unbalanced parser events: `Finish` at index {i} with no open node"
                );
            }
            Event::Tok(_) => {}
        }
    }
    assert_eq!(
        depth, 0,
        "unbalanced parser events: {depth} node(s) left open at end of stream"
    );
}

/// Generate the keyword half of [`syntax_kind_for`] from the shared keyword
/// table: the or-pattern that selects a keyword token, and the 1:1 mapping
/// behind it. Both come from the same rows, so the arm below can never miss a
/// keyword — and because it is a pattern, not a guard, `syntax_kind_for` stays
/// exhaustive over `TokKind`.
macro_rules! define_keyword_mapping {
    ($($text:literal $tok:ident $syn:ident,)*) => {
        macro_rules! keyword_tok_pat {
            () => { $(TokKind::$tok)|* };
        }

        fn keyword_syntax_kind(kind: TokKind) -> SyntaxKind {
            match kind {
                $(TokKind::$tok => SyntaxKind::$syn,)*
                _ => unreachable!("not a keyword token: {kind:?}"),
            }
        }
    };
}

keyword_table!(define_keyword_mapping);

/// The `SyntaxKind` a lexed token of `kind` is materialized as in the CST. The
/// single source of truth for the token-kind mapping.
pub(crate) fn syntax_kind_for(kind: TokKind) -> SyntaxKind {
    match kind {
        // Keywords map 1:1, generated from the shared keyword table.
        keyword_tok_pat!() => keyword_syntax_kind(kind),
        TokKind::Whitespace => SyntaxKind::WHITESPACE,
        TokKind::Newline => SyntaxKind::NEWLINE,
        TokKind::Comment => SyntaxKind::COMMENT,
        TokKind::BlockComment => SyntaxKind::BLOCK_COMMENT,
        TokKind::Ident => SyntaxKind::IDENT,
        TokKind::Integer => SyntaxKind::INTEGER,
        TokKind::BinInt => SyntaxKind::BIN_INT,
        TokKind::OctInt => SyntaxKind::OCT_INT,
        TokKind::HexInt => SyntaxKind::HEX_INT,
        TokKind::Float => SyntaxKind::FLOAT,
        TokKind::Float32 => SyntaxKind::FLOAT32,
        TokKind::ErrorInvalidNumber => SyntaxKind::ERROR_INVALID_NUMBER,
        TokKind::ErrorHexFloatNoP => SyntaxKind::ERROR_HEX_FLOAT_NO_P,
        TokKind::Char => SyntaxKind::CHAR,
        TokKind::StringDelimOpen => SyntaxKind::STRING_DELIM_OPEN,
        TokKind::StringDelimClose => SyntaxKind::STRING_DELIM_CLOSE,
        TokKind::CmdDelimOpen => SyntaxKind::CMD_DELIM_OPEN,
        TokKind::CmdDelimClose => SyntaxKind::CMD_DELIM_CLOSE,
        TokKind::StringContent => SyntaxKind::STRING_CONTENT,
        TokKind::StringPrefix => SyntaxKind::STRING_PREFIX,
        TokKind::StringSuffix => SyntaxKind::STRING_SUFFIX,
        TokKind::Eq => SyntaxKind::EQ,
        TokKind::Plus => SyntaxKind::PLUS,
        TokKind::Minus => SyntaxKind::MINUS,
        TokKind::Star => SyntaxKind::STAR,
        TokKind::Slash => SyntaxKind::SLASH,
        TokKind::Backslash => SyntaxKind::BACKSLASH,
        TokKind::SlashSlash => SyntaxKind::SLASH_SLASH,
        TokKind::Caret => SyntaxKind::CARET,
        TokKind::Percent => SyntaxKind::PERCENT,
        TokKind::PlusPercent => SyntaxKind::PLUS_PERCENT,
        TokKind::PlusPlus => SyntaxKind::PLUS_PLUS,
        TokKind::MinusPercent => SyntaxKind::MINUS_PERCENT,
        TokKind::StarPercent => SyntaxKind::STAR_PERCENT,
        TokKind::StarStar => SyntaxKind::STAR_STAR,
        TokKind::MinusMinus => SyntaxKind::MINUS_MINUS,
        TokKind::EqEq => SyntaxKind::EQ_EQ,
        TokKind::NotEq => SyntaxKind::NOT_EQ,
        TokKind::EqEqEq => SyntaxKind::EQ_EQ_EQ,
        TokKind::NotEqEq => SyntaxKind::NOT_EQ_EQ,
        TokKind::Lt => SyntaxKind::LT,
        TokKind::Le => SyntaxKind::LE,
        TokKind::Gt => SyntaxKind::GT,
        TokKind::Ge => SyntaxKind::GE,
        TokKind::AndAnd => SyntaxKind::AND_AND,
        TokKind::OrOr => SyntaxKind::OR_OR,
        TokKind::Colon => SyntaxKind::COLON,
        TokKind::ColonColon => SyntaxKind::COLON_COLON,
        TokKind::ColonEq => SyntaxKind::COLON_EQ,
        TokKind::Subtype => SyntaxKind::SUBTYPE,
        TokKind::Supertype => SyntaxKind::SUPERTYPE,
        TokKind::Arrow => SyntaxKind::ARROW,
        TokKind::LongArrow => SyntaxKind::LONG_ARROW,
        TokKind::LeftRightArrow => SyntaxKind::LEFT_RIGHT_ARROW,
        TokKind::LeftLongArrow => SyntaxKind::LEFT_LONG_ARROW,
        TokKind::FatArrow => SyntaxKind::FAT_ARROW,
        TokKind::Shl => SyntaxKind::SHL,
        TokKind::Shr => SyntaxKind::SHR,
        TokKind::UShr => SyntaxKind::USHR,
        TokKind::PlusEq => SyntaxKind::PLUS_EQ,
        TokKind::MinusEq => SyntaxKind::MINUS_EQ,
        TokKind::StarEq => SyntaxKind::STAR_EQ,
        TokKind::SlashEq => SyntaxKind::SLASH_EQ,
        TokKind::BackslashEq => SyntaxKind::BACKSLASH_EQ,
        TokKind::SlashSlashEq => SyntaxKind::SLASH_SLASH_EQ,
        TokKind::CaretEq => SyntaxKind::CARET_EQ,
        TokKind::PercentEq => SyntaxKind::PERCENT_EQ,
        TokKind::PlusPercentEq => SyntaxKind::PLUS_PERCENT_EQ,
        TokKind::MinusPercentEq => SyntaxKind::MINUS_PERCENT_EQ,
        TokKind::StarPercentEq => SyntaxKind::STAR_PERCENT_EQ,
        TokKind::PipeEq => SyntaxKind::PIPE_EQ,
        TokKind::DollarEq => SyntaxKind::DOLLAR_EQ,
        TokKind::AmpEq => SyntaxKind::AMP_EQ,
        TokKind::ShlEq => SyntaxKind::SHL_EQ,
        TokKind::ShrEq => SyntaxKind::SHR_EQ,
        TokKind::UShrEq => SyntaxKind::USHR_EQ,
        TokKind::DivEq => SyntaxKind::DIV_EQ,
        TokKind::XorEq => SyntaxKind::XOR_EQ,
        TokKind::Dot => SyntaxKind::DOT,
        TokKind::DotDot => SyntaxKind::DOT_DOT,
        TokKind::DotDotDot => SyntaxKind::DOT_DOT_DOT,
        TokKind::PipeGt => SyntaxKind::PIPE_GT,
        TokKind::PipeLt => SyntaxKind::PIPE_LT,
        TokKind::Bang => SyntaxKind::BANG,
        TokKind::Amp => SyntaxKind::AMP,
        TokKind::Pipe => SyntaxKind::PIPE,
        TokKind::Tilde => SyntaxKind::TILDE,
        TokKind::Question => SyntaxKind::QUESTION,
        TokKind::Transpose => SyntaxKind::TRANSPOSE,
        TokKind::DotPlus => SyntaxKind::DOT_PLUS,
        TokKind::DotMinus => SyntaxKind::DOT_MINUS,
        TokKind::DotStar => SyntaxKind::DOT_STAR,
        TokKind::DotStarStar => SyntaxKind::DOT_STAR_STAR,
        TokKind::DotMinusMinus => SyntaxKind::DOT_MINUS_MINUS,
        TokKind::DotSlash => SyntaxKind::DOT_SLASH,
        TokKind::DotBackslash => SyntaxKind::DOT_BACKSLASH,
        TokKind::DotSlashSlash => SyntaxKind::DOT_SLASH_SLASH,
        TokKind::DotCaret => SyntaxKind::DOT_CARET,
        TokKind::DotPercent => SyntaxKind::DOT_PERCENT,
        TokKind::DotEq => SyntaxKind::DOT_EQ,
        TokKind::DotEqEq => SyntaxKind::DOT_EQ_EQ,
        TokKind::DotNotEq => SyntaxKind::DOT_NOT_EQ,
        TokKind::DotEqEqEq => SyntaxKind::DOT_EQ_EQ_EQ,
        TokKind::DotNotEqEq => SyntaxKind::DOT_NOT_EQ_EQ,
        TokKind::DotLt => SyntaxKind::DOT_LT,
        TokKind::DotLe => SyntaxKind::DOT_LE,
        TokKind::DotGt => SyntaxKind::DOT_GT,
        TokKind::DotGe => SyntaxKind::DOT_GE,
        TokKind::DotShl => SyntaxKind::DOT_SHL,
        TokKind::DotShr => SyntaxKind::DOT_SHR,
        TokKind::DotUShr => SyntaxKind::DOT_USHR,
        TokKind::DotSubtype => SyntaxKind::DOT_SUBTYPE,
        TokKind::DotSupertype => SyntaxKind::DOT_SUPERTYPE,
        TokKind::DotFatArrow => SyntaxKind::DOT_FAT_ARROW,
        TokKind::DotLongArrow => SyntaxKind::DOT_LONG_ARROW,
        TokKind::DotLeftLongArrow => SyntaxKind::DOT_LEFT_LONG_ARROW,
        TokKind::DotLeftRightArrow => SyntaxKind::DOT_LEFT_RIGHT_ARROW,
        TokKind::DotPipeGt => SyntaxKind::DOT_PIPE_GT,
        TokKind::DotTilde => SyntaxKind::DOT_TILDE,
        TokKind::DotAndAnd => SyntaxKind::DOT_AND_AND,
        TokKind::DotOrOr => SyntaxKind::DOT_OR_OR,
        TokKind::DotAmp => SyntaxKind::DOT_AMP,
        TokKind::DotPipe => SyntaxKind::DOT_PIPE,
        TokKind::DotBang => SyntaxKind::DOT_BANG,
        TokKind::DotPlusEq => SyntaxKind::DOT_PLUS_EQ,
        TokKind::DotAmpEq => SyntaxKind::DOT_AMP_EQ,
        TokKind::DotPipeEq => SyntaxKind::DOT_PIPE_EQ,
        TokKind::DotMinusEq => SyntaxKind::DOT_MINUS_EQ,
        TokKind::DotStarEq => SyntaxKind::DOT_STAR_EQ,
        TokKind::DotSlashEq => SyntaxKind::DOT_SLASH_EQ,
        TokKind::DotBackslashEq => SyntaxKind::DOT_BACKSLASH_EQ,
        TokKind::DotSlashSlashEq => SyntaxKind::DOT_SLASH_SLASH_EQ,
        TokKind::DotCaretEq => SyntaxKind::DOT_CARET_EQ,
        TokKind::DotPercentEq => SyntaxKind::DOT_PERCENT_EQ,
        TokKind::DotShlEq => SyntaxKind::DOT_SHL_EQ,
        TokKind::DotShrEq => SyntaxKind::DOT_SHR_EQ,
        TokKind::DotUShrEq => SyntaxKind::DOT_USHR_EQ,
        TokKind::DotDivEq => SyntaxKind::DOT_DIV_EQ,
        TokKind::DotXorEq => SyntaxKind::DOT_XOR_EQ,
        // The six `call-i` Unicode operator tiers collapse to one token kind;
        // the projector recovers the operator text from the token itself.
        TokKind::UniArrow
        | TokKind::UniComparison
        | TokKind::UniColon
        | TokKind::UniPlus
        | TokKind::UniTimes
        | TokKind::UniPower => SyntaxKind::UNICODE_OP,
        TokKind::UniAssign => SyntaxKind::UNICODE_ASSIGN_OP,
        TokKind::UniRadical => SyntaxKind::UNICODE_RADICAL,
        TokKind::LParen => SyntaxKind::LPAREN,
        TokKind::RParen => SyntaxKind::RPAREN,
        TokKind::LBracket => SyntaxKind::LBRACKET,
        TokKind::RBracket => SyntaxKind::RBRACKET,
        TokKind::LBrace => SyntaxKind::LBRACE,
        TokKind::RBrace => SyntaxKind::RBRACE,
        TokKind::Comma => SyntaxKind::COMMA,
        TokKind::Semicolon => SyntaxKind::SEMICOLON,
        TokKind::At => SyntaxKind::AT,
        TokKind::Dollar => SyntaxKind::DOLLAR,
        TokKind::Unknown => SyntaxKind::ERROR_UNKNOWN_CHAR,
    }
}
