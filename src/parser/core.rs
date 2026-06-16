use crate::parser::events::Event;
use crate::parser::expr::parse_expr;
use crate::parser::lexer::lex;
use crate::parser::tree_builder::build_tree;
use crate::syntax::SyntaxNode;

pub use crate::parser::diagnostics::ParseDiagnostic;

/// The result of a parse: the lossless CST plus any diagnostics.
#[derive(Debug, Clone)]
pub struct ParseOutput {
    pub cst: SyntaxNode,
    pub diagnostics: Vec<ParseDiagnostic>,
}

/// Parse `text` into a lossless `rowan` CST. The drive loop emits root-level
/// trivia and statement separators directly, parsing each statement with the
/// Pratt expression parser; an unparseable token is consumed as one error token
/// so the loop always makes progress.
pub fn parse(text: &str) -> ParseOutput {
    let tokens = lex(text);
    let mut diagnostics = Vec::new();
    let mut events = Vec::new();

    let mut i = 0usize;
    while i < tokens.len() {
        if tokens[i].kind.is_trivia() || tokens[i].kind == crate::parser::lexer::TokKind::Semicolon
        {
            events.push(Event::Tok(i));
            i += 1;
            continue;
        }

        if let Some(expr) = parse_expr(&tokens, i, 0, &mut diagnostics) {
            events.extend(expr.events);
            i = expr.end;
        } else {
            events.push(Event::Tok(i));
            i += 1;
        }
    }

    let cst = build_tree(&tokens, &events);
    ParseOutput { cst, diagnostics }
}

/// Round-trip the input through the parser: concatenating every token in the CST
/// must reproduce the original text exactly (the losslessness invariant).
pub fn reconstruct(text: &str) -> String {
    parse(text)
        .cst
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .map(|tok| tok.text().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_lossless(input: &str) {
        assert_eq!(reconstruct(input), input, "not lossless: {input:?}");
    }

    #[test]
    fn lossless_corpus() {
        for input in [
            "",
            "x = 1\n",
            "y = a + b * c\n",
            "f(a, b, c)\n",
            "v[1]\n",
            "z = (a + b) / 2\n",
            "function g(x)\n    x ^ 2\nend\n",
            "if a >= b\n    a\nelseif c\n    c\nelse\n    b\nend\n",
            "begin\n    a\n    b\nend\n",
            "# a comment\nx = 1  # trailing\n",
            "#= block =#\nq = 2\n",
            "obj.field\n",
        ] {
            assert_lossless(input);
        }
    }

    #[test]
    fn builds_expected_top_level_shape() {
        let out = parse("x = 1 + 2\n");
        let kinds: Vec<_> = out.cst.children().map(|n| n.kind()).collect();
        assert_eq!(kinds, vec![crate::syntax::SyntaxKind::ASSIGNMENT_EXPR]);
        assert!(out.diagnostics.is_empty());
    }
}
