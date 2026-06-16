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
        TokKind::Float => SyntaxKind::FLOAT,
        TokKind::String => SyntaxKind::STRING,
        TokKind::Char => SyntaxKind::CHAR,
        TokKind::FunctionKw => SyntaxKind::FUNCTION_KW,
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
        TokKind::Eq => SyntaxKind::EQ,
        TokKind::Plus => SyntaxKind::PLUS,
        TokKind::Minus => SyntaxKind::MINUS,
        TokKind::Star => SyntaxKind::STAR,
        TokKind::Slash => SyntaxKind::SLASH,
        TokKind::Caret => SyntaxKind::CARET,
        TokKind::Percent => SyntaxKind::PERCENT,
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
        TokKind::Arrow => SyntaxKind::ARROW,
        TokKind::Dot => SyntaxKind::DOT,
        TokKind::PipeGt => SyntaxKind::PIPE_GT,
        TokKind::Bang => SyntaxKind::BANG,
        TokKind::Amp => SyntaxKind::AMP,
        TokKind::Pipe => SyntaxKind::PIPE,
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
