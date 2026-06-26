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
        SyntaxKind::COMPARISON_EXPR => lower_comparison(node),
        SyntaxKind::ARG_LIST => lower_arg_list(node),
        SyntaxKind::TUPLE_EXPR | SyntaxKind::VECT_EXPR | SyntaxKind::BRACES => {
            lower_collection(node)
        }
        SyntaxKind::KEYWORD_ARG => lower_keyword_arg(node),
        SyntaxKind::PARAMETERS => lower_parameters(node),
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

/// Lay out a call/index argument list (`f(a, b)`, `a[1, 2]`) with normalized
/// punctuation: no padding inside the brackets, no space before a comma, one
/// space after it, and a single-line trailing comma dropped (`g(a,)` → `g(a)`).
///
/// Items are `ARG`/`KEYWORD_ARG` nodes separated by commas; an optional trailing
/// `PARAMETERS` node carries `;`-separated keyword arguments and attaches without
/// a comma. Only the clean single-line shape is reshaped: any interleaved comment
/// or newline, a doubled/orphaned comma, or an unexpected child falls back to the
/// verbatim transparent lowering (which keeps multi-line arg lists byte-identical).
fn lower_arg_list(node: &SyntaxNode) -> Ir {
    let mut parts: Vec<Ir> = Vec::new();
    let mut first_item = true;
    let mut pending_comma = false;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::LPAREN
                | SyntaxKind::RPAREN
                | SyntaxKind::LBRACKET
                | SyntaxKind::RBRACKET => parts.push(Ir::text(tok.text().to_string())),
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

/// Binary operators the target style keeps tight (no surrounding spaces).
/// Everything else binary gets a space on each side.
///
/// Only the *plain* `^` qualifies: Runic always packs it (`a ^ b` → `a^b`), so
/// tight is the deterministic match. The broadcast `.^` (`DOT_CARET`) is spaced
/// like other dotted operators, and range `:` is a separate `RANGE_EXPR`.
///
/// Note `&&`/`||` are deliberately **not** here. Runic *preserves* the user's
/// spacing around them (it normalizes neither `a&&b` nor `a && b`), which Tenet 1
/// forbids — Fatou must be deterministic. We canonicalize them as spaced (the
/// idiomatic form, and what Runic yields for already-spaced input); inputs
/// written tight therefore diverge from Runic and are recorded in
/// `tests/oracle/runic-blocked.txt`.
fn is_tight_binop(kind: SyntaxKind) -> bool {
    matches!(kind, SyntaxKind::CARET)
}
