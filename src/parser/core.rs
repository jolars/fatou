use crate::parser::events::Event;
use crate::parser::expr::parse_stmt;
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

        if let Some(expr) = parse_stmt(&tokens, i, &mut diagnostics) {
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
            "a[end]\n",
            "a[end - 1]\n",
            "a[2:end]\n",
            "m[end, end]\n",
            "z = (a + b) / 2\n",
            "function g(x)\n    x ^ 2\nend\n",
            "if a >= b\n    a\nelseif c\n    c\nelse\n    b\nend\n",
            "begin\n    a\n    b\nend\n",
            "map(xs) do x\n    x + 1\nend\n",
            "open(\"f\") do\n    read()\nend\n",
            "# a comment\nx = 1  # trailing\n",
            "#= block =#\nq = 2\n",
            "obj.field\n",
            "x::Int\n",
            "Vector{T}\n",
            "foo(x::T) where {T<:Number} = x\n",
            "f(a, b; c=1, d=2)\n",
            "g(args...; kwargs...)\n",
            "y = a .+ b .* c\n",
            "z = .-x\n",
            "a .= f.(b, c)\n",
            "t = (a, b)\n",
            "u = (a,)\n",
            "e = ()\n",
            "nt = (x = 1, y = 2)\n",
            "y = a ? b : c\n",
            "z = a == b ? x + 1 : x - 1\n",
            "w = a ? b : c ? d : e\n",
            "v = [1, 2, 3]\n",
            "e = []\n",
            "m = [1 2; 3 4]\n",
            "s = [1 +2]\n",
            "c = [1; 2; 3]\n",
            "q = [x for x in xs]\n",
            "r = [x^2 for x in 1:10 if x > 2]\n",
            "g = (x for x in xs)\n",
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
