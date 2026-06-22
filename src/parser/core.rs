use crate::parser::events::Event;
use crate::parser::expr::parse_stmt;
use crate::parser::lexer::{TokKind, Token, lex};
use crate::parser::tree_builder::{build_tree, syntax_kind_for};
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
        // tracking whether it carries a `;` separator. `leftover_mark` records
        // the event offset right after the line's first complete statement so a
        // trailing junk run on a separator-less line can be wrapped in an
        // `(error-t …)` node (`x y` ⇒ `x (error-t y)`).
        let mut line = Vec::new();
        let mut has_semicolon = false;
        let mut leftover_mark: Option<usize> = None;
        let mut first_is_doc_string = false;
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
                        if leftover_mark.is_none() {
                            first_is_doc_string = stmt_is_doc_string(&expr.events, &tokens);
                        }
                        line.extend(expr.events);
                        i = expr.end;
                        leftover_mark.get_or_insert(line.len());
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
        } else if let Some(mark) = leftover_mark.filter(|&m| {
            // Junk after the first statement on a separator-less line — but a
            // bare docstring (`"a"\nfoo`) owns its trailing statement, so leave
            // that to `fold_docstrings`.
            !first_is_doc_string && line[m..].iter().any(|e| is_significant_event(e, &tokens))
        }) {
            events.extend(line[..mark].iter().cloned());
            let tail = &line[mark..];
            // Leading trivia stays outside the error node; everything from the
            // first significant leftover token onward is the recovered run.
            let lead = tail
                .iter()
                .take_while(|e| !is_significant_event(e, &tokens))
                .count();
            events.extend(tail[..lead].iter().cloned());
            events.push(Event::Start(SyntaxKind::ERROR_TRIVIA));
            events.extend(tail[lead..].iter().cloned());
            events.push(Event::Finish);
        } else {
            events.extend(line);
        }
    }

    let events = fold_docstrings(&events, &tokens, true);

    let cst = build_tree(&tokens, &events);
    ParseOutput { cst, diagnostics }
}

/// Whether an event carries significant (non-trivia) content: any node opener,
/// or a non-trivia leaf token.
fn is_significant_event(event: &Event, tokens: &[Token]) -> bool {
    match event {
        Event::Start(_) => true,
        Event::Tok(idx) => !tokens[*idx].kind.is_trivia(),
        Event::Finish => false,
    }
}

/// Whether a statement's events open a bare (doc-eligible) `STRING_LITERAL` —
/// the first inner token is not a `STRING_PREFIX`. Such a statement starts a
/// potential docstring, so a trailing statement on the same logical line is left
/// to `fold_docstrings` rather than wrapped as junk.
fn stmt_is_doc_string(events: &[Event], tokens: &[Token]) -> bool {
    matches!(
        events.first(),
        Some(Event::Start(SyntaxKind::STRING_LITERAL))
    ) && string_is_doc_eligible(&events[1..], tokens)
}

/// One child of a statement container: either a leaf token or a fully-formed
/// subtree (its node kind plus the *inner* events between its `Start`/`Finish`).
enum Item {
    Leaf(usize),
    Subtree(SyntaxKind, Vec<Event>),
}

/// Whether a node of `kind` is a statement container in which the docstring fold
/// applies (a string literal statement directly followed by another statement).
fn is_doc_container(kind: SyntaxKind) -> bool {
    matches!(kind, SyntaxKind::BLOCK | SyntaxKind::TOPLEVEL_SEMICOLON)
}

/// Whether a `STRING_LITERAL` subtree is a plain string (doc-eligible) rather
/// than a prefixed string macro (`r"…"`, `b"…"`), which is a macro call in
/// JuliaSyntax and never a docstring. Eligible iff its first token is not a
/// `STRING_PREFIX`.
fn string_is_doc_eligible(inner: &[Event], tokens: &[Token]) -> bool {
    match inner.first() {
        Some(Event::Tok(idx)) => syntax_kind_for(tokens[*idx].kind) != SyntaxKind::STRING_PREFIX,
        _ => true,
    }
}

/// Recursively fold docstrings in a statement container's child sequence.
/// `inner` is the flat event list of one node's children (the whole event
/// stream, for the implicit `ROOT`). A bare unprefixed `STRING_LITERAL`
/// statement immediately followed by another statement — at most one newline of
/// intervening trivia, no `;` — folds into a `DOC` node `(doc str target)`,
/// mirroring JuliaSyntax's `parse_docstring`. The pass descends into every
/// subtree so nested blocks (function/module/begin bodies) fold too.
fn fold_docstrings(inner: &[Event], tokens: &[Token], is_container: bool) -> Vec<Event> {
    // Split the flat event list into this level's direct children.
    let mut items: Vec<Item> = Vec::new();
    let mut k = 0;
    while k < inner.len() {
        match inner[k] {
            Event::Tok(idx) => {
                items.push(Item::Leaf(idx));
                k += 1;
            }
            Event::Start(kind) => {
                let mut depth = 1usize;
                let mut j = k + 1;
                while j < inner.len() {
                    match inner[j] {
                        Event::Start(_) => depth += 1,
                        Event::Finish => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        Event::Tok(_) => {}
                    }
                    j += 1;
                }
                // Recurse into the subtree's inner events.
                let child = fold_docstrings(&inner[k + 1..j], tokens, is_doc_container(kind));
                items.push(Item::Subtree(kind, child));
                k = j + 1;
            }
            Event::Finish => k += 1,
        }
    }

    let mut out: Vec<Event> = Vec::new();
    let mut idx = 0;
    while idx < items.len() {
        if is_container
            && let Item::Subtree(SyntaxKind::STRING_LITERAL, str_inner) = &items[idx]
            && string_is_doc_eligible(str_inner, tokens)
            && let Some(target) = doc_target(&items, idx, tokens)
        {
            // Wrap the string, the intervening trivia, and the target in a `DOC`.
            out.push(Event::Start(SyntaxKind::DOC));
            emit_items(&items[idx..=target], &mut out);
            out.push(Event::Finish);
            idx = target + 1;
            continue;
        }
        emit_items(&items[idx..=idx], &mut out);
        idx += 1;
    }
    out
}

/// Given a string statement at `start`, find the index of the statement it
/// documents: the next subtree child, reachable across at most one newline of
/// trivia and no `;`. Returns `None` if no eligible target follows.
fn doc_target(items: &[Item], start: usize, tokens: &[Token]) -> Option<usize> {
    let mut newlines = 0;
    let mut j = start + 1;
    while j < items.len() {
        match &items[j] {
            Item::Subtree(..) => return Some(j),
            Item::Leaf(t) => {
                let kind = tokens[*t].kind;
                if kind == TokKind::Newline {
                    newlines += 1;
                    if newlines > 1 {
                        return None;
                    }
                } else if !kind.is_trivia() {
                    return None;
                }
                j += 1;
            }
        }
    }
    None
}

/// Append the events for `items` to `out`, rebuilding each subtree from its
/// recorded kind and inner events.
fn emit_items(items: &[Item], out: &mut Vec<Event>) {
    for item in items {
        match item {
            Item::Leaf(idx) => out.push(Event::Tok(*idx)),
            Item::Subtree(kind, inner) => {
                out.push(Event::Start(*kind));
                out.extend(inner.iter().cloned());
                out.push(Event::Finish);
            }
        }
    }
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
