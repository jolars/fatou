//! Per-construct formatting rules: lower the lossless CST into the layout
//! [`Ir`](crate::formatter::ir::Ir) the [`printer`](crate::formatter::printer)
//! renders. The walk is a **walking skeleton**: only the constructs with a rule
//! reshape their layout; every other node is lowered *transparently* (children
//! emitted in order, tokens verbatim), so unhandled syntax stays byte-identical
//! while any handled descendant is still normalized. As rules land, nodes move
//! from the transparent fallback to a dedicated arm.
//!
//! Target style is Runic.jl's (see `AGENTS.md`); the oracle gate lives in
//! `tests/runic_oracle.rs`.

use rowan::NodeOrToken;

use crate::formatter::ir::Ir;
use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

/// Lower a parsed document (the `ROOT` node) into an `Ir` document.
pub fn lower(root: &SyntaxNode) -> Ir {
    lower_node(root)
}

fn lower_node(node: &SyntaxNode) -> Ir {
    match node.kind() {
        SyntaxKind::BINARY_EXPR | SyntaxKind::ASSIGNMENT_EXPR => lower_binary(node),
        SyntaxKind::ARROW_EXPR => lower_arrow(node),
        SyntaxKind::COMPARISON_EXPR => lower_comparison(node),
        SyntaxKind::TERNARY_EXPR => lower_ternary(node),
        SyntaxKind::RANGE_EXPR => lower_range(node),
        SyntaxKind::TYPE_ANNOTATION => lower_type_annotation(node),
        SyntaxKind::MATRIX_EXPR => lower_matrix(node),
        SyntaxKind::ARG_LIST => lower_arg_list(node),
        SyntaxKind::TUPLE_EXPR | SyntaxKind::VECT_EXPR | SyntaxKind::BRACES => {
            lower_collection(node)
        }
        SyntaxKind::KEYWORD_ARG => lower_keyword_arg(node),
        SyntaxKind::PARAMETERS => lower_parameters(node),
        SyntaxKind::RETURN_EXPR
        | SyntaxKind::CONST_STMT
        | SyntaxKind::GLOBAL_STMT
        | SyntaxKind::LOCAL_STMT => lower_keyword_stmt(node),
        _ => lower_transparent(node),
    }
}

/// Emit every child in order, lowering child nodes recursively and passing
/// tokens through verbatim. Keeps unhandled constructs (including their
/// whitespace and comments) byte-identical while still normalizing any handled
/// descendant.
fn lower_transparent(node: &SyntaxNode) -> Ir {
    Ir::concat(node.children_with_tokens().map(|el| match el {
        NodeOrToken::Node(child) => lower_node(&child),
        NodeOrToken::Token(tok) => Ir::text(tok.text().to_string()),
    }))
}

/// Lay out a binary or assignment expression with normalized operator spacing:
/// a single space on each side, except for the tight `^` the target style packs
/// without spaces.
///
/// Only the clean shape `<lhs> [ws] <op> [ws] <rhs>` is reshaped; anything else
/// (an interleaved comment or newline, error recovery, a missing operand) falls
/// back to the verbatim-preserving transparent lowering so we never mangle a
/// construct we don't fully understand.
fn lower_binary(node: &SyntaxNode) -> Ir {
    let mut operands: Vec<SyntaxNode> = Vec::new();
    let mut op: Option<SyntaxToken> = None;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) => operands.push(child),
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::WHITESPACE => {}
                SyntaxKind::COMMENT | SyntaxKind::BLOCK_COMMENT | SyntaxKind::NEWLINE => {
                    return lower_transparent(node);
                }
                _ if op.is_none() => op = Some(tok),
                _ => return lower_transparent(node),
            },
        }
    }

    let (Some(op), [lhs, rhs]) = (op, operands.as_slice()) else {
        return lower_transparent(node);
    };

    let lhs = lower_node(lhs);
    let rhs = lower_node(rhs);
    let op_text = Ir::text(op.text().to_string());

    if is_tight_binop(op.kind()) {
        Ir::concat([lhs, op_text, rhs])
    } else {
        Ir::concat([lhs, Ir::text(" "), op_text, Ir::text(" "), rhs])
    }
}

/// Lay out an anonymous-function arrow (`x -> y`, `(a, b) -> a + b`) with a single
/// space on each side of the `->`. Operand nodes are lowered recursively, so a
/// nested arrow (`x -> y -> z`, right-associative) or a normalized body
/// (`map(x -> x^2, a)`) keeps formatting. The target style always spaces the
/// arrow.
///
/// As with [`lower_binary`], only the clean single-line shape `<lhs> -> <rhs>` is
/// reshaped: an interleaved comment or newline (a multi-line body), error
/// recovery, or a missing operand falls back to the verbatim transparent lowering.
fn lower_arrow(node: &SyntaxNode) -> Ir {
    let mut operands: Vec<SyntaxNode> = Vec::new();
    let mut op: Option<SyntaxToken> = None;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) => operands.push(child),
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::WHITESPACE => {}
                SyntaxKind::ARROW if op.is_none() => op = Some(tok),
                _ => return lower_transparent(node),
            },
        }
    }

    let (Some(_), [lhs, rhs]) = (op, operands.as_slice()) else {
        return lower_transparent(node);
    };

    Ir::concat([lower_node(lhs), Ir::text(" -> "), lower_node(rhs)])
}

/// Lay out a keyword statement (`return x`, `const x = 1`, `global y`, `local z`)
/// with a single space between the leading keyword and its operand. The operand
/// is lowered recursively, so its own normalization still applies
/// (`return  x+1` → `return x + 1`, `const  x=1` → `const x = 1`). A bare keyword
/// (`return`) emits the keyword alone.
///
/// Only the clean shape `<kw> [ws] <operand>?` is reshaped. Anything else—an
/// interleaved comment (Runic preserves the spacing around a trailing comment), a
/// comma-separated name list (`global a, b`, a bare-tuple shape we don't model),
/// or any unexpected token—falls back to the verbatim transparent lowering.
fn lower_keyword_stmt(node: &SyntaxNode) -> Ir {
    let mut kw: Option<SyntaxToken> = None;
    let mut operands: Vec<SyntaxNode> = Vec::new();

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) => operands.push(child),
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::WHITESPACE => {}
                _ if kw.is_none() => kw = Some(tok),
                _ => return lower_transparent(node),
            },
        }
    }

    let Some(kw) = kw else {
        return lower_transparent(node);
    };

    match operands.as_slice() {
        [] => Ir::text(kw.text().to_string()),
        [operand] => Ir::concat([
            Ir::text(kw.text().to_string()),
            Ir::text(" "),
            lower_node(operand),
        ]),
        _ => lower_transparent(node),
    }
}

/// Lay out a comparison chain (`a == b == c`, `x < y <= z`) with a single space
/// on each side of every operator. The node alternates operand/operator and may
/// hold more than two operands; comparison operators are never tight, so every
/// gap is one space.
///
/// As with [`lower_binary`], only the clean alternating shape is reshaped: any
/// interleaved comment or newline, error recovery, or a degenerate operand count
/// falls back to the verbatim transparent lowering.
fn lower_comparison(node: &SyntaxNode) -> Ir {
    // Children in source order, with incidental whitespace dropped: operands
    // become lowered `Ir`, operator tokens become their text. The result must
    // alternate operand, operator, operand, … starting and ending on an operand.
    let mut parts: Vec<Ir> = Vec::new();
    let mut expect_operand = true;
    let mut operand_count = 0usize;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) => {
                if !expect_operand {
                    return lower_transparent(node);
                }
                if operand_count > 0 {
                    parts.push(Ir::text(" "));
                }
                parts.push(lower_node(&child));
                operand_count += 1;
                expect_operand = false;
            }
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::WHITESPACE => {}
                SyntaxKind::COMMENT | SyntaxKind::BLOCK_COMMENT | SyntaxKind::NEWLINE => {
                    return lower_transparent(node);
                }
                _ => {
                    if expect_operand {
                        return lower_transparent(node);
                    }
                    parts.push(Ir::text(" "));
                    parts.push(Ir::text(tok.text().to_string()));
                    expect_operand = true;
                }
            },
        }
    }

    // A well-formed chain ends on an operand and has at least two of them.
    if expect_operand || operand_count < 2 {
        return lower_transparent(node);
    }

    Ir::concat(parts)
}

/// Lay out a ternary conditional (`a ? b : c`) with a single space on each side of
/// both the `?` and the `:`. The node alternates operand/`?`/operand/`:`/operand;
/// a nested ternary (`a ? b : c ? d : e`, right-associative) is the final operand
/// and is lowered recursively, so it keeps normalizing. The target style normalizes
/// to one space around each operator.
///
/// As with [`lower_comparison`], only the clean single-line alternating shape with
/// `?`/`:` operators is reshaped: any interleaved comment or newline (a multi-line
/// ternary), error recovery, or an unexpected token falls back to the verbatim
/// transparent lowering.
fn lower_ternary(node: &SyntaxNode) -> Ir {
    let mut parts: Vec<Ir> = Vec::new();
    let mut expect_operand = true;
    let mut operand_count = 0usize;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) => {
                if !expect_operand {
                    return lower_transparent(node);
                }
                if operand_count > 0 {
                    parts.push(Ir::text(" "));
                }
                parts.push(lower_node(&child));
                operand_count += 1;
                expect_operand = false;
            }
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::WHITESPACE => {}
                SyntaxKind::QUESTION | SyntaxKind::COLON if !expect_operand => {
                    parts.push(Ir::text(" "));
                    parts.push(Ir::text(tok.text().to_string()));
                    expect_operand = true;
                }
                _ => return lower_transparent(node),
            },
        }
    }

    // A well-formed ternary ends on an operand and has three of them.
    if expect_operand || operand_count != 3 {
        return lower_transparent(node);
    }

    Ir::concat(parts)
}

/// Lay out a stepped range expression (`1:2:10`, `a:b:c`) with every `:` packed
/// tight: no spaces around any colon, matching the target style. The node
/// alternates operand/`:`, starting and ending on an operand with at least two of
/// them. (The two-operand range `a:b` parses as a `BINARY_EXPR` and is tightened
/// by [`lower_binary`] via [`is_tight_binop`]; this arm handles the `RANGE_EXPR`
/// the parser folds from a step.)
///
/// As with the other operator rules, only the clean alternating shape is
/// reshaped: an interleaved comment or newline, error recovery, or a degenerate
/// operand count falls back to the verbatim transparent lowering.
fn lower_range(node: &SyntaxNode) -> Ir {
    let mut parts: Vec<Ir> = Vec::new();
    let mut expect_operand = true;
    let mut operand_count = 0usize;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) => {
                if !expect_operand {
                    return lower_transparent(node);
                }
                parts.push(lower_node(&child));
                operand_count += 1;
                expect_operand = false;
            }
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::WHITESPACE => {}
                SyntaxKind::COLON if !expect_operand => {
                    parts.push(Ir::text(":"));
                    expect_operand = true;
                }
                _ => return lower_transparent(node),
            },
        }
    }

    if expect_operand || operand_count < 2 {
        return lower_transparent(node);
    }

    Ir::concat(parts)
}

/// Lay out a type annotation (`x::Int`, `::Int`) with the `::` packed tight: no
/// surrounding spaces, matching the target style. Operand nodes are lowered
/// recursively. Bails to the transparent lowering on a comment/newline or any
/// unexpected token (or a missing `::`).
fn lower_type_annotation(node: &SyntaxNode) -> Ir {
    let mut parts: Vec<Ir> = Vec::new();
    let mut seen_colons = false;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) => parts.push(lower_node(&child)),
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::WHITESPACE => {}
                SyntaxKind::COLON_COLON if !seen_colons => {
                    seen_colons = true;
                    parts.push(Ir::text("::"));
                }
                _ => return lower_transparent(node),
            },
        }
    }

    if !seen_colons {
        return lower_transparent(node);
    }

    Ir::concat(parts)
}

/// Lay out a call/index argument list (`f(a, b)`, `a[1, 2]`) — or a curly type
/// parameter list (`Vector{Int}`, `Dict{A, B}`) — with normalized punctuation: no
/// padding inside the brackets, no space before a comma, one space after it, and a
/// single-line trailing comma dropped (`g(a,)` → `g(a)`, `Array{Int,}` →
/// `Array{Int}`).
///
/// Items are `ARG`/`KEYWORD_ARG` nodes separated by commas; an optional trailing
/// `PARAMETERS` node carries `;`-separated keyword arguments and attaches without
/// a comma. Only the clean single-line shape is reshaped: any interleaved comment
/// or newline, a doubled/orphaned comma, or an unexpected child falls back to the
/// verbatim transparent lowering (which keeps multi-line arg lists byte-identical).
fn lower_arg_list(node: &SyntaxNode) -> Ir {
    if has_newline_token(node) {
        return lower_multiline_bracket(node);
    }

    let mut parts: Vec<Ir> = Vec::new();
    let mut first_item = true;
    let mut pending_comma = false;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::LPAREN
                | SyntaxKind::RPAREN
                | SyntaxKind::LBRACKET
                | SyntaxKind::RBRACKET
                | SyntaxKind::LBRACE
                | SyntaxKind::RBRACE => parts.push(Ir::text(tok.text().to_string())),
                SyntaxKind::WHITESPACE => {}
                SyntaxKind::COMMA => {
                    if pending_comma {
                        return lower_transparent(node);
                    }
                    pending_comma = true;
                }
                _ => return lower_transparent(node),
            },
            NodeOrToken::Node(child) => match child.kind() {
                SyntaxKind::ARG | SyntaxKind::KEYWORD_ARG => {
                    if !first_item {
                        if !pending_comma {
                            return lower_transparent(node);
                        }
                        parts.push(Ir::text(", "));
                    }
                    parts.push(lower_node(&child));
                    first_item = false;
                    pending_comma = false;
                }
                // `;`-separated parameters attach directly (the `;` is the
                // separator), so they must not follow a comma.
                SyntaxKind::PARAMETERS => {
                    if pending_comma {
                        return lower_transparent(node);
                    }
                    parts.push(lower_node(&child));
                    first_item = false;
                }
                _ => return lower_transparent(node),
            },
        }
    }

    Ir::concat(parts)
}

/// Lay out a bracketed collection literal — a tuple `(a, b)`, a vector `[1, 2]`,
/// or a brace set `{a, b}` — with normalized punctuation: no padding inside the
/// brackets, no space before a comma, one space after it.
///
/// The trailing comma is dropped (`[a, b,]` → `[a, b]`) **except** for a
/// single-element tuple, where the comma is semantic and kept (`(a,)` stays
/// `(a,)`, the one-tuple). Items are `ARG` nodes separated by commas; anything
/// richer — a `;`-separated matrix row (`PARAMETERS`), an interleaved comment or
/// newline, a doubled/orphaned comma, or an unexpected child — falls back to the
/// verbatim transparent lowering. Space-separated matrices are a distinct
/// `MATRIX_EXPR` node and never reach here.
fn lower_collection(node: &SyntaxNode) -> Ir {
    if has_newline_token(node) {
        return lower_multiline_bracket(node);
    }

    let keep_singleton_comma = node.kind() == SyntaxKind::TUPLE_EXPR;
    let mut parts: Vec<Ir> = Vec::new();
    let mut item_count = 0usize;
    let mut pending_comma = false;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::LPAREN | SyntaxKind::LBRACKET | SyntaxKind::LBRACE => {
                    parts.push(Ir::text(tok.text().to_string()))
                }
                SyntaxKind::RPAREN | SyntaxKind::RBRACKET | SyntaxKind::RBRACE => {
                    if pending_comma && keep_singleton_comma && item_count == 1 {
                        parts.push(Ir::text(","));
                    }
                    parts.push(Ir::text(tok.text().to_string()));
                }
                SyntaxKind::WHITESPACE => {}
                SyntaxKind::COMMA => {
                    if pending_comma {
                        return lower_transparent(node);
                    }
                    pending_comma = true;
                }
                _ => return lower_transparent(node),
            },
            NodeOrToken::Node(child) => match child.kind() {
                SyntaxKind::ARG => {
                    if item_count > 0 {
                        if !pending_comma {
                            return lower_transparent(node);
                        }
                        parts.push(Ir::text(", "));
                    }
                    parts.push(lower_node(&child));
                    item_count += 1;
                    pending_comma = false;
                }
                _ => return lower_transparent(node),
            },
        }
    }

    Ir::concat(parts)
}

/// Lay out a keyword argument (`x = 1`) with a single space on each side of the
/// `=`. Bails to the transparent lowering on a comment/newline or any shape
/// other than `<lhs> = <rhs>`.
fn lower_keyword_arg(node: &SyntaxNode) -> Ir {
    let mut parts: Vec<Ir> = Vec::new();
    let mut seen_eq = false;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) => parts.push(lower_node(&child)),
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::WHITESPACE => {}
                SyntaxKind::EQ if !seen_eq => {
                    seen_eq = true;
                    parts.push(Ir::text(" = "));
                }
                _ => return lower_transparent(node),
            },
        }
    }

    if !seen_eq {
        return lower_transparent(node);
    }

    Ir::concat(parts)
}

/// Lay out a `;`-separated parameter block (`; a = 1, b = 2`): one space after
/// the leading semicolon, items separated by `, `. Bails to the transparent
/// lowering on a comment/newline, a doubled/orphaned comma, an unexpected child,
/// or a missing semicolon.
fn lower_parameters(node: &SyntaxNode) -> Ir {
    let mut parts: Vec<Ir> = Vec::new();
    let mut first_item = true;
    let mut pending_comma = false;
    let mut seen_semi = false;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::SEMICOLON if !seen_semi => {
                    seen_semi = true;
                    parts.push(Ir::text(";"));
                }
                SyntaxKind::WHITESPACE => {}
                SyntaxKind::COMMA => {
                    if pending_comma {
                        return lower_transparent(node);
                    }
                    pending_comma = true;
                }
                _ => return lower_transparent(node),
            },
            NodeOrToken::Node(child) => {
                if !matches!(child.kind(), SyntaxKind::ARG | SyntaxKind::KEYWORD_ARG) {
                    return lower_transparent(node);
                }
                if first_item {
                    parts.push(Ir::text(" "));
                } else {
                    if !pending_comma {
                        return lower_transparent(node);
                    }
                    parts.push(Ir::text(", "));
                }
                parts.push(lower_node(&child));
                first_item = false;
                pending_comma = false;
            }
        }
    }

    if !seen_semi {
        return lower_transparent(node);
    }

    Ir::concat(parts)
}

/// Whether `node` contains a `NEWLINE` token anywhere in its descendants. This
/// is the trigger for vertical bracket layout: the target style breaks a bracket
/// across lines iff its content already spans ≥2 source lines, and the trigger is
/// **contagious** — `foo(g(a,\nb), c)` breaks the outer call too because the inner
/// call's newline is a descendant. A `NEWLINE` token (not a `\n` buried inside a
/// string or comment) is the precise signal: a bracket whose only newline lives
/// inside an un-reflowable string is left for the transparent fallback rather than
/// half-broken.
fn has_newline_token(node: &SyntaxNode) -> bool {
    node.descendants_with_tokens()
        .any(|el| el.kind() == SyntaxKind::NEWLINE)
}

/// The separator between two consecutive items of a broken bracket: the target
/// style preserves the source's choice of a same-line space versus a line break,
/// and—when broken—how many blank lines the source kept between them (Runic caps
/// blank lines at [`MAX_BLANK_LINES`]).
enum Sep {
    Space,
    Newline { blanks: usize },
}

/// The maximum number of consecutive blank lines the target style keeps; anything
/// more is collapsed to this. Runic caps blanks at two everywhere (top level,
/// inside brackets, inside matrices).
const MAX_BLANK_LINES: usize = 2;

/// Whether a broken bracket grows a trailing comma. Calls *preserve* the source
/// (keep iff already present, never add); every other bracket — index `x[…]`,
/// tuple `(…)`, vector `[…]`, brace set `{…}` — *adds* one.
fn adds_trailing_comma(node: &SyntaxNode) -> bool {
    match node.kind() {
        SyntaxKind::ARG_LIST => node
            .parent()
            .is_some_and(|p| p.kind() == SyntaxKind::INDEX_EXPR),
        _ => true,
    }
}

/// Lay out a bracketed list — a call/index `ARG_LIST` or a `(…)`/`[…]`/`{…}`
/// collection — that spans multiple source lines, matching the target style:
///
/// - **Framing.** A line break is added right after the open bracket and right
///   before the close bracket, with the content indented one step. The close
///   bracket lands back at the bracket's own indent.
/// - **Inter-item layout is preserved, not exploded.** Between two items the
///   source's choice is kept: a same-line space stays a space (`), c`), a line
///   break stays a break, and a blank line stays a blank line (capped at
///   [`MAX_BLANK_LINES`] via an [`Ir::BlankLine`]). Runic only adds the *framing*
///   breaks.
/// - **Leading and trailing blank lines are preserved** too: a blank in the
///   *leading* gap (open bracket → first item) or the *trailing* gap (last item →
///   close bracket) survives one newline of framing and keeps the rest as blanks
///   (capped at [`MAX_BLANK_LINES`]).
/// - **Trailing comma** follows [`adds_trailing_comma`].
///
/// Only a clean shape is reshaped. Anything this does not fully model falls back
/// to the verbatim transparent lowering: a comment, a `;`-separated `PARAMETERS`
/// block or bare semicolon, a doubled or leading comma, two items with no comma
/// between them, an empty bracket, or any unexpected child or token.
fn lower_multiline_bracket(node: &SyntaxNode) -> Ir {
    let mut open: Option<String> = None;
    let mut close: Option<String> = None;
    let mut items: Vec<Ir> = Vec::new();
    let mut seps: Vec<Sep> = Vec::new();
    // Whitespace state for the gap since the last item (or the open bracket).
    let mut newlines = 0usize;
    let mut comma = false;
    let mut leading_comma = false;
    let mut leading_blanks = 0usize;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::LPAREN | SyntaxKind::LBRACKET | SyntaxKind::LBRACE => {
                    open = Some(tok.text().to_string())
                }
                SyntaxKind::RPAREN | SyntaxKind::RBRACKET | SyntaxKind::RBRACE => {
                    close = Some(tok.text().to_string())
                }
                SyntaxKind::WHITESPACE => {}
                SyntaxKind::NEWLINE => newlines += 1,
                SyntaxKind::COMMA => {
                    if comma {
                        return lower_transparent(node);
                    }
                    if items.is_empty() {
                        leading_comma = true;
                    }
                    comma = true;
                }
                _ => return lower_transparent(node),
            },
            NodeOrToken::Node(child) => match child.kind() {
                SyntaxKind::ARG | SyntaxKind::KEYWORD_ARG => {
                    if items.is_empty() {
                        // The leading gap (open bracket → first item): one newline
                        // is the framing break; any extra is a preserved blank line
                        // (capped at `MAX_BLANK_LINES`).
                        leading_blanks = newlines.saturating_sub(1).min(MAX_BLANK_LINES);
                        if leading_comma {
                            return lower_transparent(node);
                        }
                    } else {
                        if !comma {
                            return lower_transparent(node);
                        }
                        seps.push(if newlines >= 1 {
                            Sep::Newline {
                                blanks: (newlines - 1).min(MAX_BLANK_LINES),
                            }
                        } else {
                            Sep::Space
                        });
                    }
                    items.push(lower_node(&child));
                    newlines = 0;
                    comma = false;
                }
                _ => return lower_transparent(node),
            },
        }
    }

    // The final gap runs from the last item to the close bracket: one newline is
    // the framing break; any extra is a preserved blank line (capped at
    // `MAX_BLANK_LINES`).
    let trailing_blanks = newlines.saturating_sub(1).min(MAX_BLANK_LINES);
    let trailing_comma = comma;

    let (Some(open), Some(close)) = (open, close) else {
        return lower_transparent(node);
    };
    if items.is_empty() {
        return lower_transparent(node);
    }

    let want_trailing = if adds_trailing_comma(node) {
        true
    } else {
        trailing_comma
    };

    let n = items.len();
    let mut inner: Vec<Ir> = Vec::with_capacity(n * 2 + 1 + leading_blanks + trailing_blanks);
    for _ in 0..leading_blanks {
        inner.push(Ir::BlankLine);
    }
    inner.push(Ir::HardLine); // framing break after the open bracket
    for (i, item) in items.into_iter().enumerate() {
        inner.push(item);
        if i + 1 < n {
            inner.push(Ir::text(","));
            match seps[i] {
                Sep::Newline { blanks } => {
                    for _ in 0..blanks {
                        inner.push(Ir::BlankLine);
                    }
                    inner.push(Ir::HardLine);
                }
                Sep::Space => inner.push(Ir::text(" ")),
            }
        } else if want_trailing {
            inner.push(Ir::text(","));
        }
    }
    for _ in 0..trailing_blanks {
        inner.push(Ir::BlankLine);
    }

    Ir::concat([
        Ir::text(open),
        Ir::indent(Ir::concat(inner)),
        Ir::HardLine, // framing break before the close bracket
        Ir::text(close),
    ])
}

/// Lay out a matrix literal (`[1 2; 3 4]`) that spans multiple source lines,
/// matching the target style: a framing break right after `[` and right before
/// `]`, with each source line re-indented one step. The matrix interior is
/// otherwise preserved **verbatim** — intra-row spacing, multi-space gaps,
/// same-line `;`-separated rows, and `;` placement are all kept (Runic does not
/// normalize inside a matrix); only the leading and trailing whitespace of each
/// source line is dropped in favor of the standard indent. Nested handled
/// constructs still normalize because each row is lowered recursively.
///
/// A single-line matrix (no `NEWLINE` among its children) has no rule: it is left
/// to the transparent fallback, which is byte-identical to Runic's verbatim
/// preservation. This arm only reshapes once the matrix already spans ≥2 lines.
///
/// Blank lines are preserved everywhere (capped at [`MAX_BLANK_LINES`] via an
/// [`Ir::BlankLine`]): between rows, right after `[` (leading), and right before
/// `]` (trailing). One empty line on each side is the framing `[`/`]` line itself
/// and is absorbed into the framing break. Only the clean shape is reshaped: a
/// comment or any unexpected token falls back to the verbatim transparent lowering.
fn lower_matrix(node: &SyntaxNode) -> Ir {
    if !has_newline_token(node) {
        return lower_transparent(node);
    }

    let mut open: Option<String> = None;
    let mut close: Option<String> = None;
    // Each source line is a list of `(is_whitespace, ir)` elements. Whitespace at
    // the ends of a line is trimmed (replaced by the framing indent); interior
    // whitespace is preserved verbatim. A new line starts at every `NEWLINE`.
    let mut lines: Vec<Vec<(bool, Ir)>> = vec![Vec::new()];

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::LBRACKET => open = Some(tok.text().to_string()),
                SyntaxKind::RBRACKET => close = Some(tok.text().to_string()),
                SyntaxKind::NEWLINE => lines.push(Vec::new()),
                SyntaxKind::WHITESPACE => lines
                    .last_mut()
                    .unwrap()
                    .push((true, Ir::text(tok.text().to_string()))),
                SyntaxKind::SEMICOLON => lines.last_mut().unwrap().push((false, Ir::text(";"))),
                _ => return lower_transparent(node),
            },
            NodeOrToken::Node(child) => match child.kind() {
                // A row is a multi-element `MATRIX_ROW`, or a bare `ARG` when the
                // row holds a single element (a newline-separated column vector).
                SyntaxKind::MATRIX_ROW | SyntaxKind::ARG => {
                    lines.last_mut().unwrap().push((false, lower_node(&child)))
                }
                _ => return lower_transparent(node),
            },
        }
    }

    let (Some(open), Some(close)) = (open, close) else {
        return lower_transparent(node);
    };

    // Trim leading/trailing whitespace from every line.
    for line in &mut lines {
        while line.first().is_some_and(|(ws, _)| *ws) {
            line.remove(0);
        }
        while line.last().is_some_and(|(ws, _)| *ws) {
            line.pop();
        }
    }

    // Locate the content span. Empty lines outside it are the open/close framing
    // lines plus any blank lines the source kept: the line carrying `[` and the
    // line carrying `]` are absorbed into the framing breaks, and one extra empty
    // line on each side becomes a preserved blank (capped at `MAX_BLANK_LINES`).
    let first = lines.iter().position(|l| !l.is_empty());
    let last = lines.iter().rposition(|l| !l.is_empty());
    let (Some(first), Some(last)) = (first, last) else {
        return lower_transparent(node);
    };
    // `first` empty lines precede the content; one is the framing `[` line, the
    // rest are blanks. Likewise for the trailing empty lines before `]`.
    let leading_blanks = first.saturating_sub(1).min(MAX_BLANK_LINES);
    let trailing_blanks = (lines.len() - 1 - last)
        .saturating_sub(1)
        .min(MAX_BLANK_LINES);
    // Interior empty lines are blank lines: emit a bare newline each (a `HardLine`
    // would leave the indent as trailing whitespace), capped at `MAX_BLANK_LINES`
    // consecutive.
    let content = &lines[first..=last];
    let mut inner: Vec<Ir> =
        Vec::with_capacity(content.len() * 2 + leading_blanks + trailing_blanks);
    for _ in 0..leading_blanks {
        inner.push(Ir::BlankLine);
    }
    let mut pending_blanks = 0usize;
    for line in content {
        if line.is_empty() {
            pending_blanks += 1;
            continue;
        }
        for _ in 0..pending_blanks.min(MAX_BLANK_LINES) {
            inner.push(Ir::BlankLine);
        }
        pending_blanks = 0;
        inner.push(Ir::HardLine); // framing break / re-indent for this line
        inner.extend(line.iter().map(|(_, ir)| ir.clone()));
    }
    for _ in 0..trailing_blanks {
        inner.push(Ir::BlankLine);
    }

    Ir::concat([
        Ir::text(open),
        Ir::indent(Ir::concat(inner)),
        Ir::HardLine, // framing break before the close bracket
        Ir::text(close),
    ])
}

/// Binary operators the target style keeps tight (no surrounding spaces).
/// Everything else binary gets a space on each side.
///
/// Three operators qualify. The *plain* `^`: Runic always packs it (`a ^ b` →
/// `a^b`). The range `:` in its two-operand `BINARY_EXPR` form (`a : b` →
/// `a:b`); Runic packs every range colon tight. And the field-access `.`
/// (`a.b.c`): Julia *requires* it tight — `a . b` is a parse error — so a space
/// here would emit invalid code. The broadcast `.^` (`DOT_CARET`) is spaced like
/// other dotted operators, and the *stepped* range `a:b:c` parses as a
/// `RANGE_EXPR` handled by [`lower_range`].
///
/// Note `&&`/`||` are deliberately **not** here. Runic *preserves* the user's
/// spacing around them (it normalizes neither `a&&b` nor `a && b`), which Tenet 1
/// forbids — Fatou must be deterministic. We canonicalize them as spaced (the
/// idiomatic form, and what Runic yields for already-spaced input); inputs
/// written tight therefore diverge from Runic and are recorded in
/// `tests/oracle/runic-blocked.txt`.
fn is_tight_binop(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::CARET | SyntaxKind::COLON | SyntaxKind::DOT
    )
}
