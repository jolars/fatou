use rowan::GreenNodeBuilder;

use crate::parser::events::Event;
use crate::parser::lexer::{TokKind, Token};
use crate::syntax::{SyntaxKind, SyntaxNode};

/// Build a lossless `rowan` CST from the token stream and the event stream.
pub(crate) fn build_tree(tokens: &[Token], events: &[Event]) -> SyntaxNode {
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

/// The `SyntaxKind` a lexed token of `kind` is materialized as in the CST. The
/// single source of truth for the token-kind mapping.
pub(crate) fn syntax_kind_for(kind: TokKind) -> SyntaxKind {
    match kind {
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
        TokKind::Char => SyntaxKind::CHAR,
        TokKind::StringDelimOpen => SyntaxKind::STRING_DELIM_OPEN,
        TokKind::StringDelimClose => SyntaxKind::STRING_DELIM_CLOSE,
        TokKind::CmdDelimOpen => SyntaxKind::CMD_DELIM_OPEN,
        TokKind::CmdDelimClose => SyntaxKind::CMD_DELIM_CLOSE,
        TokKind::StringContent => SyntaxKind::STRING_CONTENT,
        TokKind::StringPrefix => SyntaxKind::STRING_PREFIX,
        TokKind::StringSuffix => SyntaxKind::STRING_SUFFIX,
        TokKind::FunctionKw => SyntaxKind::FUNCTION_KW,
        TokKind::MacroKw => SyntaxKind::MACRO_KW,
        TokKind::EndKw => SyntaxKind::END_KW,
        TokKind::IfKw => SyntaxKind::IF_KW,
        TokKind::ElseifKw => SyntaxKind::ELSEIF_KW,
        TokKind::ElseKw => SyntaxKind::ELSE_KW,
        TokKind::BeginKw => SyntaxKind::BEGIN_KW,
        TokKind::TrueKw => SyntaxKind::TRUE_KW,
        TokKind::FalseKw => SyntaxKind::FALSE_KW,
        TokKind::WhileKw => SyntaxKind::WHILE_KW,
        TokKind::ForKw => SyntaxKind::FOR_KW,
        TokKind::DoKw => SyntaxKind::DO_KW,
        TokKind::LetKw => SyntaxKind::LET_KW,
        TokKind::QuoteKw => SyntaxKind::QUOTE_KW,
        TokKind::TryKw => SyntaxKind::TRY_KW,
        TokKind::CatchKw => SyntaxKind::CATCH_KW,
        TokKind::FinallyKw => SyntaxKind::FINALLY_KW,
        TokKind::StructKw => SyntaxKind::STRUCT_KW,
        TokKind::MutableKw => SyntaxKind::MUTABLE_KW,
        TokKind::ModuleKw => SyntaxKind::MODULE_KW,
        TokKind::BaremoduleKw => SyntaxKind::BAREMODULE_KW,
        TokKind::ReturnKw => SyntaxKind::RETURN_KW,
        TokKind::BreakKw => SyntaxKind::BREAK_KW,
        TokKind::ContinueKw => SyntaxKind::CONTINUE_KW,
        TokKind::ConstKw => SyntaxKind::CONST_KW,
        TokKind::GlobalKw => SyntaxKind::GLOBAL_KW,
        TokKind::LocalKw => SyntaxKind::LOCAL_KW,
        TokKind::ImportKw => SyntaxKind::IMPORT_KW,
        TokKind::UsingKw => SyntaxKind::USING_KW,
        TokKind::ExportKw => SyntaxKind::EXPORT_KW,
        TokKind::WhereKw => SyntaxKind::WHERE_KW,
        TokKind::Eq => SyntaxKind::EQ,
        TokKind::Plus => SyntaxKind::PLUS,
        TokKind::Minus => SyntaxKind::MINUS,
        TokKind::Star => SyntaxKind::STAR,
        TokKind::Slash => SyntaxKind::SLASH,
        TokKind::SlashSlash => SyntaxKind::SLASH_SLASH,
        TokKind::Caret => SyntaxKind::CARET,
        TokKind::Percent => SyntaxKind::PERCENT,
        TokKind::StarStar => SyntaxKind::STAR_STAR,
        TokKind::MinusMinus => SyntaxKind::MINUS_MINUS,
        TokKind::EqEq => SyntaxKind::EQ_EQ,
        TokKind::NotEq => SyntaxKind::NOT_EQ,
        TokKind::Lt => SyntaxKind::LT,
        TokKind::Le => SyntaxKind::LE,
        TokKind::Gt => SyntaxKind::GT,
        TokKind::Ge => SyntaxKind::GE,
        TokKind::AndAnd => SyntaxKind::AND_AND,
        TokKind::OrOr => SyntaxKind::OR_OR,
        TokKind::Colon => SyntaxKind::COLON,
        TokKind::ColonColon => SyntaxKind::COLON_COLON,
        TokKind::Subtype => SyntaxKind::SUBTYPE,
        TokKind::Supertype => SyntaxKind::SUPERTYPE,
        TokKind::Arrow => SyntaxKind::ARROW,
        TokKind::LongArrow => SyntaxKind::LONG_ARROW,
        TokKind::LeftRightArrow => SyntaxKind::LEFT_RIGHT_ARROW,
        TokKind::FatArrow => SyntaxKind::FAT_ARROW,
        TokKind::Shl => SyntaxKind::SHL,
        TokKind::Shr => SyntaxKind::SHR,
        TokKind::UShr => SyntaxKind::USHR,
        TokKind::PlusEq => SyntaxKind::PLUS_EQ,
        TokKind::MinusEq => SyntaxKind::MINUS_EQ,
        TokKind::StarEq => SyntaxKind::STAR_EQ,
        TokKind::SlashEq => SyntaxKind::SLASH_EQ,
        TokKind::SlashSlashEq => SyntaxKind::SLASH_SLASH_EQ,
        TokKind::CaretEq => SyntaxKind::CARET_EQ,
        TokKind::PercentEq => SyntaxKind::PERCENT_EQ,
        TokKind::PipeEq => SyntaxKind::PIPE_EQ,
        TokKind::AmpEq => SyntaxKind::AMP_EQ,
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
        TokKind::DotSlashSlash => SyntaxKind::DOT_SLASH_SLASH,
        TokKind::DotCaret => SyntaxKind::DOT_CARET,
        TokKind::DotPercent => SyntaxKind::DOT_PERCENT,
        TokKind::DotEq => SyntaxKind::DOT_EQ,
        TokKind::DotEqEq => SyntaxKind::DOT_EQ_EQ,
        TokKind::DotNotEq => SyntaxKind::DOT_NOT_EQ,
        TokKind::DotLt => SyntaxKind::DOT_LT,
        TokKind::DotLe => SyntaxKind::DOT_LE,
        TokKind::DotGt => SyntaxKind::DOT_GT,
        TokKind::DotGe => SyntaxKind::DOT_GE,
        TokKind::DotSubtype => SyntaxKind::DOT_SUBTYPE,
        TokKind::DotSupertype => SyntaxKind::DOT_SUPERTYPE,
        TokKind::DotFatArrow => SyntaxKind::DOT_FAT_ARROW,
        TokKind::DotLongArrow => SyntaxKind::DOT_LONG_ARROW,
        TokKind::DotPipeGt => SyntaxKind::DOT_PIPE_GT,
        TokKind::DotTilde => SyntaxKind::DOT_TILDE,
        TokKind::DotAndAnd => SyntaxKind::DOT_AND_AND,
        TokKind::DotOrOr => SyntaxKind::DOT_OR_OR,
        TokKind::DotAmp => SyntaxKind::DOT_AMP,
        TokKind::DotPipe => SyntaxKind::DOT_PIPE,
        TokKind::DotPlusEq => SyntaxKind::DOT_PLUS_EQ,
        TokKind::DotMinusEq => SyntaxKind::DOT_MINUS_EQ,
        TokKind::DotStarEq => SyntaxKind::DOT_STAR_EQ,
        TokKind::DotSlashEq => SyntaxKind::DOT_SLASH_EQ,
        TokKind::DotSlashSlashEq => SyntaxKind::DOT_SLASH_SLASH_EQ,
        TokKind::DotCaretEq => SyntaxKind::DOT_CARET_EQ,
        TokKind::DotPercentEq => SyntaxKind::DOT_PERCENT_EQ,
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
        TokKind::Unknown => SyntaxKind::ERROR,
    }
}
