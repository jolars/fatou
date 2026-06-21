use crate::parser::events::Event;
use crate::parser::expr::parse_stmt;
use crate::parser::lexer::{TokKind, lex};
use crate::parser::tree_builder::build_tree;
use crate::syntax::{SyntaxKind, SyntaxNode};

pub use crate::parser::diagnostics::ParseDiagnostic;

/// The result of a parse: the lossless CST plus any diagnostics.
#[derive(Debug, Clone)]
pub struct ParseOutput {
    pub cst: SyntaxNode,
    pub diagnostics: Vec<ParseDiagnostic>,
}

/// Parse `text` into a lossless `rowan` CST. The drive loop works one logical
/// line at a time: leading trivia (including newlines) is emitted directly at
/// the root, then the line's statements are parsed with the Pratt parser. A line
/// that carries a top-level `;` groups its statements into a `TOPLEVEL_SEMICOLON`
/// node (mirroring JuliaSyntax's `toplevel-;`); a plain line stays bare. An
/// unparseable token is consumed as one error token so the loop always makes
/// progress.
pub fn parse(text: &str) -> ParseOutput {
    let tokens = lex(text);
    let mut diagnostics = Vec::new();
    let mut events = Vec::new();

    let mut i = 0usize;
    while i < tokens.len() {
        // Leading trivia and blank/newline runs belong directly at the root.
        if tokens[i].kind.is_trivia() {
            events.push(Event::Tok(i));
            i += 1;
            continue;
        }

        // Collect a single logical line (up to the next newline or EOF),
        // tracking whether it carries a `;` separator.
        let mut line = Vec::new();
        let mut has_semicolon = false;
        while i < tokens.len() {
            match tokens[i].kind {
                TokKind::Newline => break,
                TokKind::Semicolon => {
                    has_semicolon = true;
                    line.push(Event::Tok(i));
                    i += 1;
                }
                k if k.is_trivia() => {
                    line.push(Event::Tok(i));
                    i += 1;
                }
                _ => {
                    if let Some(expr) = parse_stmt(&tokens, i, &mut diagnostics) {
                        line.extend(expr.events);
                        i = expr.end;
                    } else {
                        line.push(Event::Tok(i));
                        i += 1;
                    }
                }
            }
        }

        if has_semicolon {
            events.push(Event::Start(SyntaxKind::TOPLEVEL_SEMICOLON));
            events.extend(line);
            events.push(Event::Finish);
        } else {
            events.extend(line);
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
            "a; b\n",
            "a;;;b;;\n",
            ";a\n",
        ] {
            assert_lossless(input);
        }
    }

    #[test]
    fn groups_top_level_semicolon_line() {
        let out = parse("a; b\nc\n");
        let kinds: Vec<_> = out.cst.children().map(|n| n.kind()).collect();
        use crate::syntax::SyntaxKind::{NAME, TOPLEVEL_SEMICOLON};
        assert_eq!(kinds, vec![TOPLEVEL_SEMICOLON, NAME]);
    }

    #[test]
    fn builds_expected_top_level_shape() {
        let out = parse("x = 1 + 2\n");
        let kinds: Vec<_> = out.cst.children().map(|n| n.kind()).collect();
        assert_eq!(kinds, vec![crate::syntax::SyntaxKind::ASSIGNMENT_EXPR]);
        assert!(out.diagnostics.is_empty());
    }
}
