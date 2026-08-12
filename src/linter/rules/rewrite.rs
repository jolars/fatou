//! Helpers for a rule whose fix respells a construct by splicing the
//! construct's own sub-expressions into a new form.
//!
//! Such a fix reuses source text — `sum(f.(x))` becomes `sum(f, x)` by keeping
//! `f` and `x` byte for byte — which makes whatever those pieces contain
//! survive, and makes everything *between* them disappear. Two questions
//! follow, and both have the same answer for every rule that asks them:
//! whether the discarded gaps carry a comment ([`drops_a_comment`]), and
//! whether a piece is short enough to quote in a one-line diagnostic message
//! ([`inline_text`]).
//!
//! Deciding *what* to splice stays with the rule; only these two shared
//! judgments live here.

use rowan::{TextRange, TextSize};

use crate::syntax::SyntaxNode;

/// Whether respelling `outer` while keeping only the `keep` ranges would drop a
/// comment — the cue to withhold the fix and report the finding alone.
///
/// `keep` is the source-ordered, non-overlapping list of sub-ranges the
/// rewrite reuses verbatim; everything else inside `outer` is discarded, so a
/// `#` there is lost. A `#` inside a *string literal* in a discarded gap costs
/// a fix that would have been fine, which is the harmless direction: a `keep`
/// list that is not ordered and nested answers `true` for the same reason.
pub fn drops_a_comment(outer: &SyntaxNode, keep: &[TextRange]) -> bool {
    let span = outer.text_range();
    let text = outer.text().to_string();
    let offset = |at: TextSize| usize::from(at - span.start());

    let mut cursor = span.start();
    for kept in keep {
        if kept.start() < cursor || kept.end() > span.end() {
            return true;
        }
        if text[offset(cursor)..offset(kept.start())].contains('#') {
            return true;
        }
        cursor = kept.end();
    }
    text[offset(cursor)..].contains('#')
}

/// `node`'s source text, when it fits on one line.
///
/// A diagnostic message is one line: quoting a multi-line sub-expression in it
/// would wrap the caret rendering, so a rule quotes what this returns and falls
/// back to naming the rewrite in the abstract when it returns `None`.
pub fn inline_text(node: &SyntaxNode) -> Option<String> {
    let text = node.text().to_string();
    (!text.contains('\n')).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{AstNode, CallExpr, HasArgList};
    use crate::parser::parse;

    /// The first call in the parse of `src`, and its argument list's range.
    fn call_and_args(src: &str) -> (SyntaxNode, TextRange) {
        let call = parse(src)
            .cst
            .descendants()
            .find_map(CallExpr::cast)
            .expect("a call");
        let args = call.arg_list().expect("an argument list");
        (call.syntax().clone(), args.syntax().text_range())
    }

    #[test]
    fn drops_a_comment_sees_only_the_discarded_gaps() {
        // The comment is outside the kept argument list, so the rewrite loses it.
        let (call, args) = call_and_args("length #= why =# (xs)\n");
        assert!(drops_a_comment(&call, &[args]));

        // Inside it, it travels with the text that is reused.
        let (call, args) = call_and_args("length(#= why =# xs)\n");
        assert!(!drops_a_comment(&call, &[args]));

        // Nothing kept: every comment in the construct is discarded.
        let (call, _) = call_and_args("length(#= why =# xs)\n");
        assert!(drops_a_comment(&call, &[]));
        let (call, _) = call_and_args("length(xs)\n");
        assert!(!drops_a_comment(&call, &[]));
    }

    #[test]
    fn drops_a_comment_rejects_a_keep_list_it_cannot_trust() {
        let (call, args) = call_and_args("length(xs)\n");
        // Out of order, and out of `outer` — conservative either way.
        assert!(drops_a_comment(&call, &[args, args]));
        assert!(drops_a_comment(
            &call,
            &[TextRange::new(0.into(), 100.into())]
        ));
    }

    #[test]
    fn inline_text_declines_a_multi_line_node() {
        let (call, _) = call_and_args("length(xs)\n");
        assert_eq!(inline_text(&call).as_deref(), Some("length(xs)"));
        let (call, _) = call_and_args("length([\n    1,\n])\n");
        assert!(inline_text(&call).is_none());
    }
}
