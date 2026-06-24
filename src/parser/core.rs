use crate::parser::events::Event;
use crate::parser::expr::parse_stmt;
use crate::parser::lexer::{TokKind, Token, lex};
use crate::parser::tree_builder::{build_tree, syntax_kind_for};
use crate::syntax::{SyntaxKind, SyntaxNode};

pub use crate::parser::diagnostics::ParseDiagnostic;
use crate::parser::diagnostics::{DiagnosticKind, push_diagnostic};

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
        // `ERROR` node and flagged with a `TrailingJunk` diagnostic (projected
        // `(error-t …)`: `x y` ⇒ `x (error-t y)`).
        let mut line = Vec::new();
        let mut has_semicolon = false;
        let mut leftover_mark: Option<usize> = None;
        let mut first_is_doc_string = false;
        // A doc-eligible string is only a docstring when a *real* statement
        // follows it. If the leftover after the string can't start a statement
        // (a stray closer/keyword: `"doc" ]`), the string is a plain statement
        // and the remainder is junk — un-gate the raw trailing-junk collection.
        let mut doc_no_target = false;
        // A separator-less line: after the first complete statement, JuliaSyntax
        // bumps the remainder as flat error tokens rather than re-parsing it. A
        // line that carries a `;` keeps the per-segment behavior (deferred), so
        // flat-junk collection is gated off when one is present.
        let line_has_semicolon = rest_of_line_has_semicolon(&tokens, i);
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
                    if !line_has_semicolon
                        && leftover_mark.is_some()
                        && (!first_is_doc_string || doc_no_target)
                    {
                        // Trailing junk after the first statement: collect raw
                        // (no structural re-parse) so the wrapping below puts the
                        // whole run in one ERROR node and the projector renders
                        // delimiters, commas, and `@` as `✘` (`x y, z` ⇒
                        // `x (error-t y ✘ z)`, `x@y` ⇒ `x (error-t ✘ y)`).
                        line.push(Event::Tok(i));
                        i += 1;
                    } else if let Some(expr) = parse_stmt(&tokens, i, &mut diagnostics) {
                        if leftover_mark.is_none() {
                            first_is_doc_string = stmt_is_doc_string(&expr.events, &tokens);
                        }
                        line.extend(expr.events);
                        i = expr.end;
                        leftover_mark.get_or_insert(line.len());
                    } else if leftover_mark.is_none()
                        && is_close_delimiter_tok(tokens[i].kind)
                        && !rest_of_line_has_semicolon(&tokens, i)
                    {
                        // A stray *closing* delimiter at statement start (no
                        // preceding statement) is JuliaSyntax's leading empty
                        // `(error)` plus an `(error-t ✘ …)` that swallows the
                        // rest of the line: `)` ⇒ `(error) (error-t ✘)`,
                        // `) x` ⇒ `(error) (error-t ✘ x)`, `)))` ⇒ `… ✘ ✘ ✘`.
                        // The `;`-segment forms emit a subtler double marker, so
                        // they are left to the loose-token fallback below.
                        line.push(Event::Start(SyntaxKind::ERROR));
                        line.push(Event::Finish);
                        let run_start = tokens[i].start;
                        line.push(Event::Start(SyntaxKind::ERROR));
                        while i < tokens.len() && tokens[i].kind != TokKind::Newline {
                            line.push(Event::Tok(i));
                            i += 1;
                        }
                        line.push(Event::Finish);
                        push_diagnostic(
                            &mut diagnostics,
                            DiagnosticKind::StrayCloser,
                            "stray closing delimiter",
                            run_start,
                            tokens[i - 1].end,
                        );
                    } else if leftover_mark.is_none() && is_stray_block_keyword_tok(tokens[i].kind)
                    {
                        // A middle/closing block keyword (`end`, `else`,
                        // `elseif`, `catch`, `finally`) where a statement is
                        // expected is not a block opener; JuliaSyntax wraps it
                        // alone in `(error <kw>)` and bumps the rest of the line
                        // as a separate trailing-junk run (`end y z` ⇒
                        // `(error end) (error-t y z)`). Emit the wrapped keyword
                        // as the line's first "statement" and set `leftover_mark`
                        // so the existing trailing-junk machinery handles the
                        // remainder.
                        line.push(Event::Start(SyntaxKind::ERROR));
                        line.push(Event::Tok(i));
                        line.push(Event::Finish);
                        push_diagnostic(
                            &mut diagnostics,
                            DiagnosticKind::StrayKeyword,
                            "unexpected block keyword",
                            tokens[i].start,
                            tokens[i].end,
                        );
                        i += 1;
                        leftover_mark = Some(line.len());
                    } else {
                        // A leftover token that can't start a statement. After a
                        // doc-eligible string this means the string has no
                        // documentable target, so the rest of the line is junk.
                        if first_is_doc_string && leftover_mark.is_some() {
                            doc_no_target = true;
                        }
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
            // that to `fold_docstrings`. A docstring defers only when its
            // leftover actually begins with a documentable statement subtree
            // (`"doc" foo`); a leftover that opens with junk (`"doc" ]`) is
            // recovered here so the stray closer becomes `(error-t ✘)`.
            let tail = &line[m..];
            let defer_to_doc = first_is_doc_string && leftover_starts_with_subtree(tail, &tokens);
            !defer_to_doc && tail.iter().any(|e| is_significant_event(e, &tokens))
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
            let first_junk = tail[lead..].iter().find_map(|e| match e {
                Event::Tok(idx) => Some(*idx),
                _ => None,
            });
            events.push(Event::Start(SyntaxKind::ERROR));
            events.extend(tail[lead..].iter().cloned());
            events.push(Event::Finish);
            if let Some(idx) = first_junk {
                push_diagnostic(
                    &mut diagnostics,
                    DiagnosticKind::TrailingJunk,
                    "trailing tokens after statement",
                    tokens[idx].start,
                    tokens[idx].end,
                );
            }
        } else {
            events.extend(line);
        }
    }

    let events = fold_docstrings(&events, &tokens, true);

    let cst = build_tree(&tokens, &events);
    flag_invalid_const_decls(&cst, &mut diagnostics);
    flag_invalid_function_signatures(&cst, &mut diagnostics);
    flag_invalid_catch_vars(&cst, &mut diagnostics);
    ParseOutput { cst, diagnostics }
}

/// Flag each `const` whose declaration is not the plain `=` assignment
/// JuliaSyntax requires. A bare declaration (`const x`), a non-`=` assignment
/// (`const x += 1`, `const x .= 1`), or a `global`/`local` declaration without an
/// `=` (`const global x`) is invalid, so JuliaSyntax wraps the whole `const` in
/// `(error …)` (`const x` ⇒ `(error (const x))`). A valid `const x = 1` — or a
/// `global`/`local`-wrapped `=` (`const global x = 1`) — is left alone. The
/// diagnostic is a zero-width point at the `const` keyword start; the projector
/// reconstructs the error wrapper from it (the CST topology stays faithful).
fn flag_invalid_const_decls(cst: &SyntaxNode, diagnostics: &mut Vec<ParseDiagnostic>) {
    for node in cst
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::CONST_STMT)
    {
        if !const_decl_is_assignment(&node) && !is_struct_const_field(&node) {
            let pos = usize::from(node.text_range().start());
            push_diagnostic(
                diagnostics,
                DiagnosticKind::ConstNotAssignment,
                "expected assignment after `const`",
                pos,
                pos,
            );
        }
    }
}

/// Flag each `function`/`macro` whose signature is a bare identifier name but
/// which carries a non-empty body. A header like `function f end` (a bare name
/// with a truly empty body) is the valid forward-declaration form `(function f)`,
/// but once a body is present (`function f body end`) or the body block is
/// explicitly opened with a `;` (`function f; end`), the bare name is no longer a
/// valid signature: JuliaSyntax error-wraps it (`(function (error f) (block
/// body))`). The diagnostic is a zero-width point at the `SIGNATURE` node's start;
/// the projector reconstructs the error wrapper from it (the CST stays faithful).
fn flag_invalid_function_signatures(cst: &SyntaxNode, diagnostics: &mut Vec<ParseDiagnostic>) {
    for node in cst
        .descendants()
        .filter(|n| matches!(n.kind(), SyntaxKind::FUNCTION_DEF | SyntaxKind::MACRO_DEF))
    {
        let Some(sig) = node.children().find(|c| c.kind() == SyntaxKind::SIGNATURE) else {
            continue;
        };
        let sig_is_bare_name = sig.first_child().is_some_and(|inner| {
            matches!(inner.kind(), SyntaxKind::NAME | SyntaxKind::INTERPOLATION)
        });
        if sig_is_bare_name && !function_body_is_empty(&node) {
            let pos = usize::from(sig.text_range().start());
            push_diagnostic(
                diagnostics,
                DiagnosticKind::InvalidFunctionSignature,
                "invalid function signature: expected a call, not a bare name",
                pos,
                pos,
            );
        }
    }
}

/// Flag each `catch` clause whose variable is not a plain identifier. The catch
/// variable must be a bare identifier (`catch e`), a `$`-interpolation
/// (`catch $e`), or a `var"…"` non-standard identifier (`catch var"e"`); any
/// other expression (`catch e+3`, `catch e.f`, `catch f(e)`, `catch 3`) is
/// invalid and JuliaSyntax wraps it in `(error …)` (`catch e+3` ⇒ `(catch
/// (error (call-i e + 3)) …)`). The diagnostic is a zero-width point at the
/// catch-variable node's start; the projector reconstructs the error wrapper
/// from it (the CST topology stays faithful).
fn flag_invalid_catch_vars(cst: &SyntaxNode, diagnostics: &mut Vec<ParseDiagnostic>) {
    for node in cst
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::CATCH_CLAUSE)
    {
        let Some(var) = node.children().find(|c| c.kind() != SyntaxKind::BLOCK) else {
            continue;
        };
        if !matches!(
            var.kind(),
            SyntaxKind::NAME | SyntaxKind::INTERPOLATION | SyntaxKind::NONSTANDARD_IDENTIFIER
        ) {
            let pos = usize::from(var.text_range().start());
            push_diagnostic(
                diagnostics,
                DiagnosticKind::CatchVarNotIdentifier,
                "catch variable must be an identifier",
                pos,
                pos,
            );
        }
    }
}

/// Whether a `function`/`macro`'s body is syntactically empty — no statement
/// nodes and no `;` separator opening the block. A bare-name header keeps the
/// forward-declaration form only while this holds; a `;` (`function f; end`) or
/// any body statement marks the block as genuinely opened.
fn function_body_is_empty(node: &SyntaxNode) -> bool {
    match node.children().find(|c| c.kind() == SyntaxKind::BLOCK) {
        Some(block) => {
            block.first_child().is_none()
                && !block
                    .children_with_tokens()
                    .any(|el| el.kind() == SyntaxKind::SEMICOLON)
        }
        None => true,
    }
}

/// Whether `node` is a `const` field declaration directly inside a struct body
/// (`struct A const a end` ⇒ `(const a)`, not wrapped). Such a bare `const` is
/// valid; the exemption is narrow — a `const` nested inside an `if`/`begin`
/// within the struct (its block's parent is not the `STRUCT_DEF`) is still an
/// error.
fn is_struct_const_field(node: &SyntaxNode) -> bool {
    node.parent()
        .filter(|b| b.kind() == SyntaxKind::BLOCK)
        .and_then(|b| b.parent())
        .is_some_and(|p| p.kind() == SyntaxKind::STRUCT_DEF)
}

/// Whether a `const`'s declaration is a plain `=` assignment. The body is the
/// `const`'s first child node, unwrapping any `global`/`local` declaration; it is
/// valid only when it is an `ASSIGNMENT_EXPR` headed by a plain `=` (not
/// `.=`/`+=`/…).
fn const_decl_is_assignment(node: &SyntaxNode) -> bool {
    let mut body = node.first_child();
    while let Some(n) = body {
        match n.kind() {
            SyntaxKind::GLOBAL_STMT | SyntaxKind::LOCAL_STMT => body = n.first_child(),
            SyntaxKind::ASSIGNMENT_EXPR => {
                return n
                    .children_with_tokens()
                    .filter_map(|el| el.into_token())
                    .any(|t| t.kind() == SyntaxKind::EQ);
            }
            _ => return false,
        }
    }
    false
}

/// Whether `kind` is a closing delimiter (`)`, `]`, `}`), which when stray at
/// statement start drives JuliaSyntax's leading-`(error)` recovery.
fn is_close_delimiter_tok(kind: TokKind) -> bool {
    matches!(kind, TokKind::RParen | TokKind::RBracket | TokKind::RBrace)
}

/// Whether `kind` is a middle/closing block keyword (`end`, `else`, `elseif`,
/// `catch`, `finally`) — a token that only closes or continues an enclosing
/// block. Where a statement is expected one of these is stray, so JuliaSyntax
/// wraps it alone in `(error <kw>)` (`@doc x\nend` ⇒ `(macrocall @doc x) (error
/// end)`, `end y z` ⇒ `(error end) (error-t y z)`).
fn is_stray_block_keyword_tok(kind: TokKind) -> bool {
    matches!(
        kind,
        TokKind::EndKw
            | TokKind::ElseKw
            | TokKind::ElseifKw
            | TokKind::CatchKw
            | TokKind::FinallyKw
    )
}

/// Whether the logical line starting at `start` (up to the next newline or EOF)
/// carries a top-level `;`. The stray-closer recovery only applies to the clean
/// separator-less form; `;`-segment lines keep the loose-token fallback.
fn rest_of_line_has_semicolon(tokens: &[Token], start: usize) -> bool {
    tokens[start..]
        .iter()
        .take_while(|t| t.kind != TokKind::Newline)
        .any(|t| t.kind == TokKind::Semicolon)
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

/// Whether the first significant event in `tail` opens a node (a real statement
/// subtree). A docstring's trailing leftover defers to `fold_docstrings` only
/// when it begins with such a subtree (its documentable target); a leftover that
/// opens with a bare junk token is recovered as trailing junk instead.
fn leftover_starts_with_subtree(tail: &[Event], tokens: &[Token]) -> bool {
    matches!(
        tail.iter().find(|e| is_significant_event(e, tokens)),
        Some(Event::Start(_))
    )
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
            // An error-recovery node is never a documentable target (`"doc"\n]`
            // ⇒ `(string) (error) (error-t ✘)`, not a `(doc …)`); the string is
            // a plain statement.
            Item::Subtree(SyntaxKind::ERROR, _) => return None,
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
