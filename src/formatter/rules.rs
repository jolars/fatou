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
