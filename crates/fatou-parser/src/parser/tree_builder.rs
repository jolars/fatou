use rowan::GreenNodeBuilder;

use crate::parser::events::Event;
use crate::parser::lexer::{TokKind, Token};
use crate::syntax::{SyntaxKind, SyntaxNode};
use crate::tokens::token_table;

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
    builder.token(syntax_kind_for(tok.kind).into(), tok.text);
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

/// Generate [`syntax_kind_for`] from the shared token table: one arm per row,
/// plus the collapsing arm for the Unicode tiers the table cannot hold. Because
/// the arms come from the same rows the two enums do, a token can be neither
/// unmapped (the match stays exhaustive over `TokKind`) nor mapped to a kind
/// that does not exist.
macro_rules! define_syntax_kind_for {
    ($($(#[$meta:meta])* $tok:ident $syn:ident,)*) => {
        /// The `SyntaxKind` a lexed token of `kind` is materialized as in the
        /// CST.
        pub(crate) fn syntax_kind_for(kind: TokKind) -> SyntaxKind {
            match kind {
                $(TokKind::$tok => SyntaxKind::$syn,)*
                // The six `call-i` Unicode operator tiers collapse to one kind;
                // the projector recovers the operator text from the token itself.
                TokKind::UniArrow
                | TokKind::UniComparison
                | TokKind::UniColon
                | TokKind::UniPlus
                | TokKind::UniTimes
                | TokKind::UniPower => SyntaxKind::UNICODE_OP,
            }
        }
    };
}

token_table!(define_syntax_kind_for);
