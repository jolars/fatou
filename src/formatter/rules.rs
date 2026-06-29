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
use crate::syntax::{SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken};

/// Lower a parsed document (the `ROOT` node) into an `Ir` document.
pub fn lower(root: &SyntaxNode) -> Ir {
    lower_node(root)
}

fn lower_node(node: &SyntaxNode) -> Ir {
    match node.kind() {
        SyntaxKind::BINARY_EXPR | SyntaxKind::ASSIGNMENT_EXPR => lower_binary(node),
        SyntaxKind::ARROW_EXPR => lower_arrow(node),
        SyntaxKind::WHERE_EXPR => lower_where(node),
        SyntaxKind::COMPARISON_EXPR => lower_comparison(node),
        SyntaxKind::TERNARY_EXPR => lower_ternary(node),
        SyntaxKind::RANGE_EXPR => lower_range(node),
        SyntaxKind::TYPE_ANNOTATION => lower_type_annotation(node),
        SyntaxKind::MATRIX_EXPR => lower_matrix(node),
        SyntaxKind::BEGIN_EXPR | SyntaxKind::QUOTE_EXPR => lower_block_expr(node),
        SyntaxKind::LET_EXPR => lower_let(node),
        SyntaxKind::WHILE_EXPR | SyntaxKind::FOR_EXPR => lower_loop(node),
        SyntaxKind::IF_EXPR => lower_if(node),
        SyntaxKind::TRY_EXPR => lower_try(node),
        SyntaxKind::ARG_LIST => lower_arg_list(node),
        SyntaxKind::TUPLE_EXPR | SyntaxKind::VECT_EXPR | SyntaxKind::BRACES => {
            lower_collection(node)
        }
        SyntaxKind::PAREN_EXPR => lower_paren(node),
        SyntaxKind::PAREN_BLOCK => lower_paren_block(node),
        SyntaxKind::BARE_TUPLE_EXPR => lower_bare_tuple(node),
        SyntaxKind::KEYWORD_ARG => lower_keyword_arg(node),
        SyntaxKind::PARAMETERS => lower_parameters(node),
        SyntaxKind::FOR_BINDING => lower_for_binding(node),
        SyntaxKind::RETURN_EXPR
        | SyntaxKind::CONST_STMT
        | SyntaxKind::GLOBAL_STMT
        | SyntaxKind::LOCAL_STMT => lower_keyword_stmt(node),
        SyntaxKind::USING_STMT | SyntaxKind::IMPORT_STMT => lower_import_stmt(node),
        SyntaxKind::EXPORT_STMT | SyntaxKind::PUBLIC_STMT => lower_export_stmt(node),
        SyntaxKind::LITERAL => lower_literal(node),
        _ => lower_transparent(node),
    }
}

/// Emit every child in order, lowering child nodes recursively and passing
/// tokens through verbatim. Keeps unhandled constructs (including their
/// whitespace and comments) byte-identical while still normalizing any handled
/// descendant.
fn lower_transparent(node: &SyntaxNode) -> Ir {
    let mut parts = Vec::new();
    let mut iter = node.children_with_tokens().peekable();
    while let Some(el) = iter.next() {
        match el {
            NodeOrToken::Node(child) => parts.push(lower_node(&child)),
            NodeOrToken::Token(tok) => parts.push(lower_trivia(&tok, iter.peek())),
        }
    }
    Ir::concat(parts)
}

/// Lower a token in transparent context, trimming trailing horizontal
/// whitespace the way Runic's `trim_trailing_whitespace` does: a `WHITESPACE`
/// run sitting immediately before a line break is dropped, and a line
/// `COMMENT`'s trailing blanks are stripped. String content and block comments
/// are left verbatim—Runic preserves trailing whitespace inside both.
fn lower_trivia(tok: &SyntaxToken, next: Option<&SyntaxElement>) -> Ir {
    match tok.kind() {
        SyntaxKind::WHITESPACE
            if matches!(
                next,
                Some(NodeOrToken::Token(t)) if t.kind() == SyntaxKind::NEWLINE
            ) =>
        {
            Ir::text("")
        }
        SyntaxKind::COMMENT => Ir::text(tok.text().trim_end_matches([' ', '\t'])),
        _ => Ir::text(tok.text().to_string()),
    }
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

/// Lay out a parenthesized expression (`(a + b)`) with **no padding** inside the
/// parentheses: `( a + b )` → `(a + b)`, `(  x  )` → `(x)`. Runic strips the
/// incidental whitespace flanking the inner expression. The single inner node is
/// lowered recursively, so a nested paren (`( (a) )` → `((a))`) and the inner
/// expression's own spacing keep normalizing.
///
/// As with [`lower_arrow`], only the clean single-line shape `( <expr> )` is
/// reshaped: an interleaved comment or newline (a multi-line paren Runic may
/// reflow and reindent), error recovery, or a missing/extra operand falls back to
/// the verbatim transparent lowering. The `;`-block form `(a; b)` is a distinct
/// `PAREN_BLOCK` node, and a tuple `(a, b)` is a `TUPLE_EXPR`, so neither reaches
/// here.
fn lower_paren(node: &SyntaxNode) -> Ir {
    let mut operands: Vec<SyntaxNode> = Vec::new();
    let mut saw_lparen = false;
    let mut saw_rparen = false;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) => operands.push(child),
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::WHITESPACE => {}
                SyntaxKind::LPAREN if !saw_lparen => saw_lparen = true,
                SyntaxKind::RPAREN if !saw_rparen => saw_rparen = true,
                _ => return lower_transparent(node),
            },
        }
    }

    let (true, true, [inner]) = (saw_lparen, saw_rparen, operands.as_slice()) else {
        return lower_transparent(node);
    };

    Ir::concat([Ir::text("("), lower_node(inner), Ir::text(")")])
}

/// Lay out a `;`-block `(a; b)` (a `PAREN_BLOCK`, distinct from the single-value
/// `PAREN_EXPR` and the comma tuple `TUPLE_EXPR`). The block is `LPAREN`, a
/// leading statement node, then one `PARAMETERS` node per `; <stmt>` (each
/// carrying the `SEMICOLON`, optional whitespace, and the statement), then
/// `RPAREN`. Runic packs each separator tight-left/space-right and strips the
/// padding flanking the inner expressions: `( a ; b )` → `(a; b)`,
/// `(a;b;)` → `(a; b)` (a trailing `;` produces an arg-less `PARAMETERS` that is
/// dropped). Every statement is lowered recursively, so a nested block
/// (`((a;b);c)` → `((a; b); c)`) and each statement's own spacing keep
/// normalizing.
///
/// Only the multi-statement single-line shape is reshaped. A single-statement
/// block (`(a;)`, always carrying a trailing `;` since a bare `(a)` is a
/// `PAREN_EXPR`) is left to the transparent fallback—Runic *preserves* the
/// trailing `;` there (`(a;)` → `(a;)`), which the verbatim lowering already
/// matches for the unpadded form. An interleaved comment or newline (a
/// multi-line block Runic may reflow and reindent), error recovery, or any other
/// unexpected child also falls back to the transparent lowering.
fn lower_paren_block(node: &SyntaxNode) -> Ir {
    let mut statements: Vec<Ir> = Vec::new();
    let mut saw_lparen = false;
    let mut saw_rparen = false;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) => {
                if child.kind() == SyntaxKind::PARAMETERS {
                    match paren_block_statement(&child) {
                        Ok(Some(ir)) => statements.push(ir),
                        Ok(None) => {} // trailing `;` — arg-less, dropped
                        Err(()) => return lower_transparent(node),
                    }
                } else {
                    statements.push(lower_node(&child));
                }
            }
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::WHITESPACE => {}
                SyntaxKind::LPAREN if !saw_lparen => saw_lparen = true,
                SyntaxKind::RPAREN if !saw_rparen => saw_rparen = true,
                _ => return lower_transparent(node),
            },
        }
    }

    if !saw_lparen || !saw_rparen || statements.len() < 2 {
        return lower_transparent(node);
    }

    let mut parts: Vec<Ir> = Vec::with_capacity(statements.len() * 2 + 1);
    parts.push(Ir::text("("));
    for (i, stmt) in statements.into_iter().enumerate() {
        if i > 0 {
            parts.push(Ir::text("; "));
        }
        parts.push(stmt);
    }
    parts.push(Ir::text(")"));
    Ir::concat(parts)
}

/// Extract the lowered statement from a `PARAMETERS` node inside a `PAREN_BLOCK`:
/// `SEMICOLON`, optional whitespace, and at most one statement node. Returns the
/// lowered statement, `None` for an arg-less trailing `;`, or `Err` on any
/// unmodeled shape (comment, newline, a stray comma, or a second statement).
fn paren_block_statement(params: &SyntaxNode) -> Result<Option<Ir>, ()> {
    let mut statement: Option<Ir> = None;
    let mut saw_semicolon = false;

    for el in params.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) => {
                if statement.is_some() {
                    return Err(());
                }
                statement = Some(lower_node(&child));
            }
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::WHITESPACE => {}
                SyntaxKind::SEMICOLON if !saw_semicolon => saw_semicolon = true,
                _ => return Err(()),
            },
        }
    }

    if !saw_semicolon {
        return Err(());
    }
    Ok(statement)
}

/// Lay out a `where` clause (`f(x) where T`, `Tuple{T} where {T <: Real}`) with a
/// single space on each side of `where` and the bound **always wrapped in
/// braces**: `where T` → `where {T}`. A bound that is already a `{...}` brace node
/// is normalized in place (via [`lower_collection`]), so `where { T , S }` →
/// `where {T, S}`; any other bound (a bare name, a `<:`/`>:` subtype, a paren or
/// curly expression) is wrapped: `where T<:Real` → `where {T <: Real}`. Both
/// operands are lowered recursively, so a nested `where` (`f(x) where T where S`,
/// itself a left-nested `WHERE_EXPR`) and the bound's own spacing keep
/// normalizing.
///
/// As with [`lower_arrow`], only the clean single-line shape `<lhs> where <rhs>`
/// is reshaped: an interleaved comment or newline (a multi-line clause Runic may
/// reflow), error recovery, or a missing operand falls back to the verbatim
/// transparent lowering.
fn lower_where(node: &SyntaxNode) -> Ir {
    let mut operands: Vec<SyntaxNode> = Vec::new();
    let mut kw: Option<SyntaxToken> = None;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) => operands.push(child),
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::WHITESPACE => {}
                SyntaxKind::WHERE_KW if kw.is_none() => kw = Some(tok),
                _ => return lower_transparent(node),
            },
        }
    }

    let (Some(_), [lhs, rhs]) = (kw, operands.as_slice()) else {
        return lower_transparent(node);
    };

    let bound = if rhs.kind() == SyntaxKind::BRACES {
        lower_node(rhs)
    } else {
        Ir::concat([Ir::text("{"), lower_node(rhs), Ir::text("}")])
    };

    Ir::concat([lower_node(lhs), Ir::text(" where "), bound])
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
    // First non-whitespace token is the keyword; everything after it (sans
    // incidental whitespace) is the operand sequence.
    let mut kw: Option<SyntaxToken> = None;
    let mut rest: Vec<NodeOrToken<SyntaxNode, SyntaxToken>> = Vec::new();

    for el in node.children_with_tokens() {
        match &el {
            NodeOrToken::Token(tok) if tok.kind() == SyntaxKind::WHITESPACE => {}
            NodeOrToken::Token(tok) if kw.is_none() => kw = Some(tok.clone()),
            _ => rest.push(el),
        }
    }

    let Some(kw) = kw else {
        return lower_transparent(node);
    };

    // Bare keyword (`return`), or a single operand node (`return x`,
    // `const x = 1`, `local a = 1`)—the operand is recursed so its own spacing
    // normalizes.
    match rest.as_slice() {
        [] => return Ir::text(kw.text().to_string()),
        [NodeOrToken::Node(operand)] => {
            return Ir::concat([
                Ir::text(kw.text().to_string()),
                Ir::text(" "),
                lower_node(operand),
            ]);
        }
        _ => {}
    }

    // Comma name list (`global a, b`, `local x, y, z`): the parser drops the
    // `NAME`/`IDENT`/`COMMA` children directly into the statement node, so this
    // is *not* an operand subtree. Accept only the clean alternating shape—an
    // item (a `NAME` node or a bare `IDENT` token) then a `COMMA`—and `", "`-join
    // it. Anything else (an `=`/`::` assignment-list form, a comment, a trailing
    // comma) bails to the lossless transparent passthrough.
    let mut parts: Vec<Ir> = vec![Ir::text(kw.text().to_string()), Ir::text(" ")];
    let mut expect_item = true;

    for el in &rest {
        match el {
            NodeOrToken::Node(child) if expect_item => {
                parts.push(lower_node(child));
                expect_item = false;
            }
            NodeOrToken::Token(tok) if expect_item && tok.kind() == SyntaxKind::IDENT => {
                parts.push(Ir::text(tok.text().to_string()));
                expect_item = false;
            }
            NodeOrToken::Token(tok) if !expect_item && tok.kind() == SyntaxKind::COMMA => {
                parts.push(Ir::text(", "));
                expect_item = true;
            }
            _ => return lower_transparent(node),
        }
    }

    // A dangling `expect_item` means a leading or trailing comma (or an empty
    // list); neither is a clean name list.
    if expect_item {
        return lower_transparent(node);
    }

    Ir::concat(parts)
}

/// Lay out a `using`/`import` statement: the keyword, then a comma-separated list
/// of `IMPORT_PATH`/`IMPORT_ALIAS` items, optionally `:`-led into a selector list
/// (`using A: x, y`). Runic spaces every comma (`, `) and packs the selector colon
/// tight-left, space-right (`A: x`); the paths themselves (`A.B`, `.A`, `Foo as
/// Bar`) are lowered transparently, so their internal tokens pass through verbatim.
///
/// Only the clean alternating shape—item, separator, item, …—is reshaped. A
/// comment/newline (a multi-line import Runic may reflow), a leading/trailing/
/// doubled separator, or any unexpected token bails to the lossless transparent
/// lowering.
fn lower_import_stmt(node: &SyntaxNode) -> Ir {
    let mut kw: Option<SyntaxToken> = None;
    let mut rest: Vec<NodeOrToken<SyntaxNode, SyntaxToken>> = Vec::new();

    for el in node.children_with_tokens() {
        match &el {
            NodeOrToken::Token(tok) if tok.kind() == SyntaxKind::WHITESPACE => {}
            NodeOrToken::Token(tok) if kw.is_none() => kw = Some(tok.clone()),
            _ => rest.push(el),
        }
    }

    let Some(kw) = kw else {
        return lower_transparent(node);
    };

    // Alternate item (a path/alias node) and separator (`,` → `, `, the selector
    // `:` → `: `), starting and ending on an item.
    let mut parts: Vec<Ir> = vec![Ir::text(kw.text().to_string()), Ir::text(" ")];
    let mut expect_item = true;

    for el in &rest {
        match el {
            NodeOrToken::Node(child) if expect_item => {
                parts.push(lower_node(child));
                expect_item = false;
            }
            NodeOrToken::Token(tok) if !expect_item && tok.kind() == SyntaxKind::COMMA => {
                parts.push(Ir::text(", "));
                expect_item = true;
            }
            NodeOrToken::Token(tok) if !expect_item && tok.kind() == SyntaxKind::COLON => {
                parts.push(Ir::text(": "));
                expect_item = true;
            }
            _ => return lower_transparent(node),
        }
    }

    // A dangling `expect_item` means a leading/trailing/doubled separator or an
    // empty list—none is the clean shape this rule models.
    if expect_item {
        return lower_transparent(node);
    }

    Ir::concat(parts)
}

/// Lay out an `export`/`public` statement: the keyword, one space, then the
/// comma-separated name list `", "`-joined (`export a,b` → `export a, b`,
/// `public foo,bar` → `public foo, bar`). Runic spaces every comma (tight-left,
/// space-right) and leaves the names themselves alone.
///
/// Unlike the `using`/`import` list, an exported name is **not** always a single
/// node: it may be an identifier, an operator (`export +, -`), a macro
/// (`export @m`), or a `var"…"` form (several adjacent tokens). So the rule
/// tracks comma boundaries rather than a strict node/separator alternation: the
/// first token of each name gets a leading space, and any further tokens of the
/// *same* name are glued verbatim (no incidental whitespace exists between them).
/// Bails to the lossless transparent lowering on a comment/newline (a multi-line
/// list Runic may reflow) or a leading/trailing/doubled comma.
fn lower_export_stmt(node: &SyntaxNode) -> Ir {
    let mut kw: Option<SyntaxToken> = None;
    let mut rest: Vec<NodeOrToken<SyntaxNode, SyntaxToken>> = Vec::new();

    for el in node.children_with_tokens() {
        match &el {
            NodeOrToken::Token(tok) if tok.kind() == SyntaxKind::WHITESPACE => {}
            NodeOrToken::Token(tok)
                if matches!(
                    tok.kind(),
                    SyntaxKind::COMMENT | SyntaxKind::BLOCK_COMMENT | SyntaxKind::NEWLINE
                ) =>
            {
                return lower_transparent(node);
            }
            NodeOrToken::Token(tok) if kw.is_none() => kw = Some(tok.clone()),
            _ => rest.push(el),
        }
    }

    let Some(kw) = kw else {
        return lower_transparent(node);
    };

    let mut parts: Vec<Ir> = vec![Ir::text(kw.text().to_string())];
    // `expect_item` is true after the keyword and after each comma: the next item
    // token opens a new name and takes a leading space. While false, we are inside
    // a name—a comma closes it, any other token is glued on verbatim.
    let mut expect_item = true;

    for el in &rest {
        match el {
            NodeOrToken::Token(tok) if !expect_item && tok.kind() == SyntaxKind::COMMA => {
                parts.push(Ir::text(","));
                expect_item = true;
            }
            // A comma where an item is expected is a leading/doubled comma.
            NodeOrToken::Token(tok) if tok.kind() == SyntaxKind::COMMA => {
                return lower_transparent(node);
            }
            _ => {
                if expect_item {
                    parts.push(Ir::text(" "));
                    expect_item = false;
                }
                parts.push(match el {
                    NodeOrToken::Node(child) => lower_node(child),
                    NodeOrToken::Token(tok) => Ir::text(tok.text().to_string()),
                });
            }
        }
    }

    // A dangling `expect_item` means a trailing comma or an empty list—neither is
    // a clean name list.
    if expect_item {
        return lower_transparent(node);
    }

    Ir::concat(parts)
}

/// Lay out a literal (`42`, `1.`, `0xFF`, `true`, `:sym`): every token but a
/// **float** or **hex integer** passes through verbatim. A `FLOAT`/`FLOAT32`
/// token is normalized to the target style's canonical form via
/// [`normalize_float`] (`.5` → `0.5`, `1.` → `1.0`, `1E10` → `1.0e10`,
/// `1f0` → `1.0f0`); a `HEX_INT` token is zero-padded to a fixed width via
/// [`normalize_hex`] (`0xF` → `0x0F`). Decimal, octal, and binary integers and
/// boolean literals are untouched. A token we don't fully model (an underscored
/// or hex float, or any shape that doesn't parse cleanly) is left verbatim.
fn lower_literal(node: &SyntaxNode) -> Ir {
    Ir::concat(node.children_with_tokens().map(|el| match el {
        NodeOrToken::Node(child) => lower_node(&child),
        NodeOrToken::Token(tok) => match tok.kind() {
            SyntaxKind::FLOAT | SyntaxKind::FLOAT32 => {
                Ir::text(normalize_float(tok.text()).unwrap_or_else(|| tok.text().to_string()))
            }
            SyntaxKind::HEX_INT => {
                Ir::text(normalize_hex(tok.text()).unwrap_or_else(|| tok.text().to_string()))
            }
            _ => Ir::text(tok.text().to_string()),
        },
    }))
}

/// Zero-pad a hexadecimal integer literal to Runic's fixed widths, or return
/// `None` to leave it verbatim. Runic's `format_hex_literals` pads the literal
/// (`0x` prefix included) to the next of the canonical spans `0x` + 2/4/8/16/32
/// hex chars (the widths of `UInt8`/`UInt16`/`UInt32`/`UInt64`/`UInt128`), by
/// inserting `0`s right after the `0x`. The byte span—**not** the digit count—is
/// what is measured, so underscores count toward the width (`0x1_2` → `0x01_2`).
///
/// A literal already at a canonical span, or one whose span is ≥ 34 (a BigInt
/// hex literal, wider than `UInt128`), is returned `None` (left verbatim). The
/// digit case is preserved (`0xDEADBEEF` is untouched, not lowercased). Output
/// always lands exactly on a canonical span, so the rule is idempotent.
fn normalize_hex(text: &str) -> Option<String> {
    // Canonical total spans: `0x` (2 bytes) + 2/4/8/16/32 hex chars.
    const TARGETS: [usize; 5] = [4, 6, 10, 18, 34];
    let rest = text.strip_prefix("0x")?;
    let span = text.len();
    // Already canonical, or a BigInt literal wider than UInt128: leave verbatim.
    if span >= 34 || TARGETS.contains(&span) {
        return None;
    }
    let target = *TARGETS.iter().find(|&&t| t > span)?;
    let mut out = String::with_capacity(target);
    out.push_str("0x");
    out.extend(std::iter::repeat_n('0', target - span));
    out.push_str(rest);
    Some(out)
}

/// Normalize a decimal float literal to the target style's canonical form, or
/// return `None` to leave it verbatim. The canonical form (matching Runic's
/// `format_float_literals`) is `[sign] <int>.<frac> [e|f [sign] <exp>]` where:
///
/// - the integral part has its leading zeros stripped but keeps at least one
///   digit (`.5` → `0`, `007.` → `7`);
/// - the decimal point is always present, with at least one fractional digit
///   (`1.` → `1.0`, `1e5` → `1.0e5`);
/// - trailing zeros in the fraction are stripped, keeping at least one
///   (`1.50` → `1.5`, `1.00` → `1.0`);
/// - the exponent marker is lowercased (`E` → `e`; the `f` Float32 marker stays),
///   with leading zeros stripped from the exponent; and
/// - a Unicode minus (`−`, U+2212) is normalized to ASCII `-`.
///
/// Underscored and hex (`0x…p…`) floats are left verbatim (Runic skips them too),
/// as is any token that doesn't parse cleanly into the shape above.
fn normalize_float(text: &str) -> Option<String> {
    // Underscored and hex floats are out of scope—Runic skips them as well.
    if text.contains('_') || text.contains("0x") || text.contains("0X") {
        return None;
    }

    let mut chars = text.chars().peekable();
    let mut out = String::new();

    // Optional leading sign (`+`, `-`, or Unicode minus → ASCII `-`).
    match chars.peek() {
        Some('+') => {
            out.push('+');
            chars.next();
        }
        Some('-') | Some('\u{2212}') => {
            out.push('-');
            chars.next();
        }
        _ => {}
    }

    // Integral digits.
    let mut int_part = String::new();
    while chars.peek().is_some_and(char::is_ascii_digit) {
        int_part.push(chars.next().unwrap());
    }

    // Optional decimal point and fractional digits.
    let mut frac_part = String::new();
    if chars.peek() == Some(&'.') {
        chars.next();
        while chars.peek().is_some_and(char::is_ascii_digit) {
            frac_part.push(chars.next().unwrap());
        }
    }

    // Optional exponent: marker (`e`/`E`/`f`), an optional sign, then digits.
    let mut marker = String::new();
    let mut exp_part = String::new();
    if matches!(chars.peek(), Some('e' | 'E' | 'f')) {
        let m = chars.next().unwrap();
        marker.push(if m == 'E' { 'e' } else { m });
        match chars.peek() {
            Some('+') => {
                marker.push('+');
                chars.next();
            }
            Some('-') | Some('\u{2212}') => {
                marker.push('-');
                chars.next();
            }
            _ => {}
        }
        while chars.peek().is_some_and(char::is_ascii_digit) {
            exp_part.push(chars.next().unwrap());
        }
    }

    // Any trailing character means a shape we don't model—leave it verbatim.
    if chars.next().is_some() {
        return None;
    }

    out.push_str(&strip_leading_zeros(&int_part));
    out.push('.');
    out.push_str(&strip_trailing_zeros(&frac_part));
    if !marker.is_empty() {
        out.push_str(&marker);
        out.push_str(&strip_leading_zeros(&exp_part));
    }
    Some(out)
}

/// Strip leading zeros, collapsing an empty or all-zero string to a single `0`.
fn strip_leading_zeros(s: &str) -> String {
    let t = s.trim_start_matches('0');
    if t.is_empty() {
        "0".to_string()
    } else {
        t.to_string()
    }
}

/// Strip trailing zeros, collapsing an empty or all-zero string to a single `0`.
fn strip_trailing_zeros(s: &str) -> String {
    let t = s.trim_end_matches('0');
    if t.is_empty() {
        "0".to_string()
    } else {
        t.to_string()
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
                // `ARG` is a positional element; `KEYWORD_ARG` is a named-tuple
                // element (`(a = 1, b = 2)`), lowered by `lower_keyword_arg`.
                SyntaxKind::ARG | SyntaxKind::KEYWORD_ARG => {
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

/// Lay out a bare (bracketless) tuple — `x, y`, `a, b, c`, the lhs/rhs of a
/// multiple assignment (`a, b = 1, 2`), a multi-value `return x, y` — with
/// normalized comma punctuation: no space before a comma, one space after it.
/// Elements are bare nodes (not `ARG`-wrapped) separated by commas; each is
/// lowered recursively so its own normalization still applies (`f(x),g(y)` →
/// `f(x), g(y)`).
///
/// Only the clean alternating shape `<el> , <el> [ , <el> ]…` is reshaped. A
/// leading/doubled/trailing comma (the trailing form is a parse error at this
/// level anyway), an interleaved comment or newline, or any unexpected token
/// falls back to the verbatim transparent lowering.
fn lower_bare_tuple(node: &SyntaxNode) -> Ir {
    let mut parts: Vec<Ir> = Vec::new();
    let mut item_count = 0usize;
    let mut pending_comma = false;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::WHITESPACE => {}
                SyntaxKind::COMMA => {
                    if pending_comma || item_count == 0 {
                        return lower_transparent(node);
                    }
                    pending_comma = true;
                }
                _ => return lower_transparent(node),
            },
            NodeOrToken::Node(child) => {
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
        }
    }

    if pending_comma {
        return lower_transparent(node);
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

/// Lay out a `for` binding — the iteration clause of a comprehension or generator
/// (`[x for i = 1:3]`, `(x for i ∈ s)`) or a `for` loop (`for i = 1:3 … end`) —
/// normalizing the iteration operator to the keyword `in`, the target style's
/// canonical form: `for i = 1:3` → `for i in 1:3`, `for i ∈ s` → `for i in s`.
/// An already-`in` binding keeps `in` with one space on each side. Multiple
/// comma-separated bindings (`for i = 1:3, j = 1:3`) are each normalized and
/// `", "`-joined, and a trailing comprehension filter (`for i = 1:3 if cond`) is
/// reproduced with one space around `if`. Targets and iterables are lowered
/// recursively, so their own spacing keeps normalizing.
///
/// The `for` keyword is a child of this node in a comprehension/generator but of
/// the parent in a `for` loop, so it is emitted iff present. Only the clean
/// single-line shape is reshaped: an interleaved comment or newline, a filter that
/// is not a single expression, or any binding shape this does not model falls back
/// to the verbatim transparent lowering.
fn lower_for_binding(node: &SyntaxNode) -> Ir {
    let mut for_kw = false;
    let mut els: Vec<SyntaxElement> = Vec::new();

    for el in node.children_with_tokens() {
        match &el {
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::WHITESPACE => {}
                SyntaxKind::FOR_KW if !for_kw && els.is_empty() => for_kw = true,
                SyntaxKind::COMMENT | SyntaxKind::BLOCK_COMMENT | SyntaxKind::NEWLINE => {
                    return lower_transparent(node);
                }
                _ => els.push(el),
            },
            NodeOrToken::Node(_) => els.push(el),
        }
    }

    // Partition the post-`for` elements into comma-separated binding groups plus
    // an optional trailing `if <filter>` tail.
    let mut groups: Vec<Vec<SyntaxElement>> = vec![Vec::new()];
    let mut filter: Option<SyntaxNode> = None;

    let mut iter = els.into_iter();
    while let Some(el) = iter.next() {
        match &el {
            NodeOrToken::Token(tok) if tok.kind() == SyntaxKind::COMMA => {
                groups.push(Vec::new());
            }
            NodeOrToken::Token(tok) if tok.kind() == SyntaxKind::IF_KW => {
                // The remainder is the filter: exactly one expression node.
                let rest: Vec<SyntaxElement> = iter.collect();
                match rest.as_slice() {
                    [NodeOrToken::Node(f)] => filter = Some(f.clone()),
                    _ => return lower_transparent(node),
                }
                break;
            }
            _ => groups.last_mut().unwrap().push(el),
        }
    }

    let mut specs: Vec<Ir> = Vec::with_capacity(groups.len());
    for group in &groups {
        match lower_for_spec(group) {
            Some(ir) => specs.push(ir),
            None => return lower_transparent(node),
        }
    }
    if specs.is_empty() {
        return lower_transparent(node);
    }

    let mut parts: Vec<Ir> = Vec::with_capacity(specs.len() * 2 + 2);
    if for_kw {
        parts.push(Ir::text("for "));
    }
    for (i, spec) in specs.into_iter().enumerate() {
        if i > 0 {
            parts.push(Ir::text(", "));
        }
        parts.push(spec);
    }
    if let Some(filter) = filter {
        parts.push(Ir::text(" if "));
        parts.push(lower_node(&filter));
    }
    Ir::concat(parts)
}

/// Lower one `for`-binding group (`<target> <op> <iterable>`) to `<target> in
/// <iterable>`, normalizing the iteration operator to `in`. Two CST shapes occur:
/// a wrapped `=`/`∈` binding is a single `ASSIGNMENT_EXPR`/`BINARY_EXPR` node,
/// while an already-`in` binding is the flat triple `target`, `in`, `iterable`.
/// Returns `None` (the caller bails to transparent) on any other shape.
fn lower_for_spec(group: &[SyntaxElement]) -> Option<Ir> {
    match group {
        // Wrapped `=` (`ASSIGNMENT_EXPR`) or `∈` (`BINARY_EXPR`) binding.
        [NodeOrToken::Node(node)]
            if matches!(
                node.kind(),
                SyntaxKind::ASSIGNMENT_EXPR | SyntaxKind::BINARY_EXPR
            ) =>
        {
            let (lhs, rhs) = for_iteration_operands(node)?;
            Some(Ir::concat([lhs, Ir::text(" in "), rhs]))
        }
        // Flat `in` binding: target, the `in` keyword, iterable.
        [
            NodeOrToken::Node(target),
            NodeOrToken::Token(kw),
            NodeOrToken::Node(iterable),
        ] if kw.kind() == SyntaxKind::IDENT && kw.text() == "in" => Some(Ir::concat([
            lower_node(target),
            Ir::text(" in "),
            lower_node(iterable),
        ])),
        _ => None,
    }
}

/// Split a wrapped `for`-binding node (`i = 1:3` or `i ∈ s`) into its lowered
/// target and iterable, accepting only the `=` and `∈` iteration operators.
/// Returns `None` on any other operator, a stray comment, or an operand count ≠ 2.
fn for_iteration_operands(node: &SyntaxNode) -> Option<(Ir, Ir)> {
    let mut operands: Vec<SyntaxNode> = Vec::new();
    let mut op_count = 0usize;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) => operands.push(child),
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::WHITESPACE => {}
                SyntaxKind::EQ => op_count += 1,
                SyntaxKind::UNICODE_OP if tok.text() == "∈" => op_count += 1,
                _ => return None,
            },
        }
    }

    let (1, [lhs, rhs]) = (op_count, operands.as_slice()) else {
        return None;
    };
    Some((lower_node(lhs), lower_node(rhs)))
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
/// style preserves the source's choice of a same-line space versus a line break.
/// When broken, the gap may also carry own-line comments and blank lines (the
/// latter capped at [`MAX_BLANK_LINES`]); [`GapLine`] records them in order.
enum Sep {
    Space,
    Break(Vec<GapLine>),
}

/// One physical line inside a broken bracket's gap (the span between two items,
/// before the first item, or after the last): either a preserved blank line or an
/// own-line comment. Trailing comments (`item, # …`) are not gap lines — they ride
/// on the item they follow.
enum GapLine {
    Blank,
    Comment(String),
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
    // A trailing comment riding on item `i` (`item, # …`), rendered after its
    // comma. Aligned with `items`; a `None` slot is pushed for every item.
    let mut item_comments: Vec<Option<String>> = Vec::new();
    let mut seps: Vec<Sep> = Vec::new();
    // The gap being accumulated since the last item: own-line comments and blank
    // lines, in order. Flushed into a `Sep::Break`, the leading gap, or the
    // trailing gap when the next item or the close bracket arrives.
    let mut gap: Vec<GapLine> = Vec::new();
    let mut leading: Vec<GapLine> = Vec::new();
    let mut header_comment: Option<String> = None;
    // Newlines since the last line-content token (item or own-line comment), used
    // to size blank-line runs.
    let mut newlines = 0usize;
    let mut comma = false;
    let mut leading_comma = false;

    // Append `newlines`-worth of blank lines to the gap (one newline ends the
    // previous line; the rest are blanks, capped), then reset the counter.
    let flush_blanks = |gap: &mut Vec<GapLine>, newlines: &mut usize| {
        for _ in 0..newlines.saturating_sub(1).min(MAX_BLANK_LINES) {
            gap.push(GapLine::Blank);
        }
        *newlines = 0;
    };

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
                SyntaxKind::COMMENT | SyntaxKind::BLOCK_COMMENT => {
                    // A block-comment token always ends with `=#`, so the trim is a
                    // no-op for it; its multi-line interior and any trailing blanks
                    // are preserved verbatim, matching Runic. A line comment's own
                    // trailing whitespace is trimmed as before.
                    let text = tok.text().trim_end_matches([' ', '\t']).to_string();
                    if newlines == 0 {
                        // Same line as the previous content: a trailing comment on
                        // the last item, or — before any item — on the open bracket.
                        let slot = match items.last_mut() {
                            Some(_) => item_comments.last_mut().unwrap(),
                            None => &mut header_comment,
                        };
                        if slot.is_some() {
                            return lower_transparent(node);
                        }
                        *slot = Some(text);
                    } else {
                        // An own-line comment: a line of its own inside the gap.
                        flush_blanks(&mut gap, &mut newlines);
                        gap.push(GapLine::Comment(text));
                    }
                }
                _ => return lower_transparent(node),
            },
            NodeOrToken::Node(child) => match child.kind() {
                SyntaxKind::ARG | SyntaxKind::KEYWORD_ARG => {
                    // A same-line separator (`a, b`) needs no newline and no gap
                    // content; capture it before `flush_blanks` zeroes the counter.
                    let same_line = newlines == 0 && gap.is_empty();
                    flush_blanks(&mut gap, &mut newlines);
                    if items.is_empty() {
                        if leading_comma {
                            return lower_transparent(node);
                        }
                        leading = std::mem::take(&mut gap);
                    } else {
                        if !comma {
                            return lower_transparent(node);
                        }
                        seps.push(if same_line {
                            Sep::Space
                        } else {
                            Sep::Break(std::mem::take(&mut gap))
                        });
                    }
                    items.push(lower_node(&child));
                    item_comments.push(None);
                    comma = false;
                }
                _ => return lower_transparent(node),
            },
        }
    }

    // The final gap runs from the last item to the close bracket.
    flush_blanks(&mut gap, &mut newlines);
    let trailing = gap;
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

    // Render a gap's own-line comments and blank lines, in order. A comment opens
    // its own indented line via a `HardLine`; a blank line is a bare newline.
    fn render_gap(inner: &mut Vec<Ir>, lines: &[GapLine]) {
        for line in lines {
            match line {
                GapLine::Blank => inner.push(Ir::BlankLine),
                GapLine::Comment(text) => {
                    inner.push(Ir::HardLine);
                    inner.push(Ir::text(text.clone()));
                }
            }
        }
    }

    let n = items.len();
    let mut inner: Vec<Ir> = Vec::new();
    render_gap(&mut inner, &leading);
    inner.push(Ir::HardLine); // framing break after the open bracket
    for (i, item) in items.into_iter().enumerate() {
        inner.push(item);
        let is_last = i + 1 == n;
        if !is_last || want_trailing {
            inner.push(Ir::text(","));
        }
        // The trailing comment rides after the comma, canonicalized to one
        // leading space (a Tenet-1 divergence: Runic preserves the source spacing).
        if let Some(text) = &item_comments[i] {
            inner.push(Ir::text(" "));
            inner.push(Ir::text(text.clone()));
        }
        if !is_last {
            match &seps[i] {
                Sep::Space => inner.push(Ir::text(" ")),
                Sep::Break(lines) => {
                    render_gap(&mut inner, lines);
                    inner.push(Ir::HardLine);
                }
            }
        }
    }
    render_gap(&mut inner, &trailing);

    // A comment on the open-bracket line rides after it, canonicalized to one
    // leading space (the same Tenet-1 divergence as a trailing item comment).
    let mut out: Vec<Ir> = vec![Ir::text(open)];
    if let Some(text) = header_comment {
        out.push(Ir::text(" "));
        out.push(Ir::text(text));
    }
    out.push(Ir::indent(Ir::concat(inner)));
    out.push(Ir::HardLine); // framing break before the close bracket
    out.push(Ir::text(close));
    Ir::concat(out)
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
                // A line or block comment is kept verbatim as a non-whitespace line
                // element (a line comment's own trailing whitespace trimmed; a block
                // comment ends with `=#` so the trim is a no-op, keeping its
                // multi-line interior verbatim). The matrix interior is preserved, so
                // the pre-comment spacing matches Runic byte-for-byte; an own-line
                // comment becomes a content line of its own.
                SyntaxKind::COMMENT | SyntaxKind::BLOCK_COMMENT => {
                    lines.last_mut().unwrap().push((
                        false,
                        Ir::text(tok.text().trim_end_matches([' ', '\t']).to_string()),
                    ))
                }
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

/// Lay out a keyword block whose body is a bare `BLOCK` — `begin … end` and
/// `quote … end` — by indenting each statement one step, matching the target
/// style. The shape is `<kw> BLOCK <end>`; the body is lowered by
/// [`lower_block_body`].
///
/// A **non-empty** block is always exploded to the vertical form, even if the
/// source wrote it on one line: `begin x end` → `begin⏎    x⏎end`. An **empty**
/// block keeps its source layout (`begin end`, `begin⏎end`) via the transparent
/// fallback, which is byte-identical to Runic's preservation there. Any shape
/// this does not fully model — a comment in the body, two statements with no
/// separator, a missing `end`, or an unexpected child — also falls back to the
/// verbatim transparent lowering.
fn lower_block_expr(node: &SyntaxNode) -> Ir {
    let kw = match node.kind() {
        SyntaxKind::BEGIN_EXPR => "begin",
        SyntaxKind::QUOTE_EXPR => "quote",
        _ => return lower_transparent(node),
    };

    let mut block: Option<SyntaxNode> = None;
    let mut saw_end = false;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) if child.kind() == SyntaxKind::BLOCK => {
                if block.is_some() {
                    return lower_transparent(node);
                }
                block = Some(child);
            }
            NodeOrToken::Node(_) => return lower_transparent(node),
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::BEGIN_KW | SyntaxKind::QUOTE_KW => {}
                SyntaxKind::END_KW => saw_end = true,
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => {}
                _ => return lower_transparent(node),
            },
        }
    }

    let (Some(block), true) = (block, saw_end) else {
        return lower_transparent(node);
    };
    let Some(body) = lower_block_body(&block) else {
        return lower_transparent(node);
    };

    Ir::concat([Ir::text(kw), body, Ir::HardLine, Ir::text("end")])
}

/// Lay out a `let` block (`let x = 1 … end`) by indenting its body one step,
/// matching the target style. The shape is `let [LET_BINDINGS] BLOCK end`; the
/// header is `let` plus, when present, a space and the recursively-lowered
/// binding list, and the body is lowered by [`lower_block_body`].
///
/// A **non-empty** body is always exploded to the vertical form, even when the
/// source wrote it on one line (`let x = 1; y = 2 end` → `let x = 1⏎    y = 2⏎
/// end`): the binding-from-body separator `;` opens the `BLOCK`, so the body
/// statements already live inside it. An **empty** body (`let end`, `let⏎end`,
/// or `let x = 1⏎end`) keeps its source layout via the transparent fallback,
/// which is byte-identical to the target's preservation there.
///
/// The binding list is lowered recursively (so `let x = 1` keeps its spacing),
/// but it is not otherwise reshaped: the parser leaves the second and later
/// bindings as flat tokens rather than wrapped nodes, so a tight multi-binding
/// header (`let x=1,y=2`) is not normalized here and is kept out of the fixture.
/// Any shape this does not fully model — a comment in the body, two statements
/// with no separator, a missing `end`, or an unexpected child — also falls back
/// to the verbatim transparent lowering.
fn lower_let(node: &SyntaxNode) -> Ir {
    let mut bindings: Option<SyntaxNode> = None;
    let mut block: Option<SyntaxNode> = None;
    let mut saw_let = false;
    let mut saw_end = false;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) => match child.kind() {
                SyntaxKind::LET_BINDINGS if bindings.is_none() && block.is_none() => {
                    bindings = Some(child)
                }
                SyntaxKind::BLOCK if block.is_none() => block = Some(child),
                _ => return lower_transparent(node),
            },
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::LET_KW if !saw_let => saw_let = true,
                SyntaxKind::END_KW => saw_end = true,
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => {}
                _ => return lower_transparent(node),
            },
        }
    }

    let (true, true, Some(block)) = (saw_let, saw_end, block) else {
        return lower_transparent(node);
    };
    let Some(body) = lower_block_body(&block) else {
        return lower_transparent(node);
    };

    let mut parts: Vec<Ir> = vec![Ir::text("let")];
    if let Some(bindings) = bindings {
        parts.push(Ir::text(" "));
        parts.push(lower_node(&bindings));
    }
    parts.push(body);
    parts.push(Ir::HardLine);
    parts.push(Ir::text("end"));
    Ir::concat(parts)
}

/// Lay out a `while` or `for` loop by indenting its body one step, reusing the
/// [`lower_block_body`] engine that `begin`/`quote`/`let` already share. The
/// shape is `<kw> <header> BLOCK end`, where the header is a `CONDITION`
/// (`while`) or a `FOR_BINDING` (`for`); both are lowered recursively, so the
/// condition's inner spacing normalizes and `lower_for_binding` rewrites the
/// iteration operator to `in` (`for i = 1:3` → `for i in 1:3`). The `for`
/// keyword is the loop's own child here, not the binding's, so
/// `lower_for_binding` emits no `for ` prefix and this rule supplies it.
///
/// A **non-empty** body is always exploded to the vertical form, even when the
/// source wrote it on one line (`while x; y; z; end` → `while x⏎    y; z⏎end`):
/// the leading `;` opens the `BLOCK`, so the body statements live inside it. An
/// **empty** body (`while x end`, `for i in y end`) makes `lower_block_body`
/// return `None`, and the transparent fallback preserves the source layout
/// byte-for-byte, matching the target. Loop bodies are never `return`-inserted
/// (only function bodies are), so there is no semantic-rewrite risk. Any shape
/// this does not fully model — a body comment, a missing `end`, an unexpected
/// child — also falls back to the verbatim transparent lowering.
fn lower_loop(node: &SyntaxNode) -> Ir {
    let kw = match node.kind() {
        SyntaxKind::WHILE_EXPR => "while",
        SyntaxKind::FOR_EXPR => "for",
        _ => return lower_transparent(node),
    };

    let mut header: Option<SyntaxNode> = None;
    let mut block: Option<SyntaxNode> = None;
    let mut saw_kw = false;
    let mut saw_end = false;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) => match child.kind() {
                SyntaxKind::CONDITION | SyntaxKind::FOR_BINDING
                    if header.is_none() && block.is_none() =>
                {
                    header = Some(child)
                }
                SyntaxKind::BLOCK if block.is_none() => block = Some(child),
                _ => return lower_transparent(node),
            },
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::WHILE_KW | SyntaxKind::FOR_KW if !saw_kw => saw_kw = true,
                SyntaxKind::END_KW => saw_end = true,
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => {}
                _ => return lower_transparent(node),
            },
        }
    }

    let (true, true, Some(header), Some(block)) = (saw_kw, saw_end, header, block) else {
        return lower_transparent(node);
    };
    let Some(body) = lower_block_body(&block) else {
        return lower_transparent(node);
    };

    Ir::concat([
        Ir::text(kw),
        Ir::text(" "),
        lower_node(&header),
        body,
        Ir::HardLine,
        Ir::text("end"),
    ])
}

/// Lay out an `if`/`elseif`/`else` chain, indenting each branch body one step
/// and emitting the branch keywords at column 0, reusing the
/// [`lower_block_body`] engine the other block rules share. The shape is
/// `IF_KW CONDITION BLOCK (ELSEIF_CLAUSE)* (ELSE_CLAUSE)? END_KW`; each
/// `ELSEIF_CLAUSE` is `ELSEIF_KW CONDITION BLOCK` and the `ELSE_CLAUSE` is
/// `ELSE_KW BLOCK`. The leading condition is lowered recursively (its inner
/// spacing normalizes), the body of every branch is delegated to
/// `lower_block_body`, and each subsequent clause is rendered by
/// [`lower_branch_clause`] as a `HardLine` + keyword at column 0 followed by its
/// indented body.
///
/// Like the loop and `begin`/`quote`/`let` rules, a **non-empty** body is always
/// exploded to the vertical form even when the source wrote it on one line
/// (`if x; y; end` → `if x⏎    y⏎end`). An **empty** branch body makes
/// `lower_block_body` return `None`; rather than partially reshape the chain, the
/// whole `if` falls back to the verbatim transparent lowering. `if` bodies are
/// never `return`-inserted (only function bodies are), so there is no
/// semantic-rewrite risk. Any unmodeled shape — a body comment, a missing `end`,
/// an unexpected child — also bails to transparent.
fn lower_if(node: &SyntaxNode) -> Ir {
    let mut condition: Option<SyntaxNode> = None;
    let mut block: Option<SyntaxNode> = None;
    let mut clauses: Vec<SyntaxNode> = Vec::new();
    let mut saw_if = false;
    let mut saw_end = false;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) => match child.kind() {
                SyntaxKind::CONDITION if condition.is_none() && block.is_none() => {
                    condition = Some(child)
                }
                SyntaxKind::BLOCK if condition.is_some() && block.is_none() => block = Some(child),
                SyntaxKind::ELSEIF_CLAUSE | SyntaxKind::ELSE_CLAUSE if block.is_some() => {
                    clauses.push(child)
                }
                _ => return lower_transparent(node),
            },
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::IF_KW if !saw_if => saw_if = true,
                SyntaxKind::END_KW => saw_end = true,
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => {}
                _ => return lower_transparent(node),
            },
        }
    }

    let (true, true, Some(condition), Some(block)) = (saw_if, saw_end, condition, block) else {
        return lower_transparent(node);
    };
    let Some(body) = lower_block_body(&block) else {
        return lower_transparent(node);
    };

    let mut parts: Vec<Ir> = vec![Ir::text("if"), Ir::text(" "), lower_node(&condition), body];
    for clause in &clauses {
        let Some(clause_ir) = lower_branch_clause(clause) else {
            return lower_transparent(node);
        };
        parts.push(clause_ir);
    }
    parts.push(Ir::HardLine);
    parts.push(Ir::text("end"));
    Ir::concat(parts)
}

/// Lay out a `try`/`catch`/`else`/`finally` block, indenting the `try` body and
/// each clause body one step with the keywords at column 0, reusing the
/// [`lower_block_body`] engine. The shape is
/// `TRY_KW BLOCK (CATCH_CLAUSE)? (ELSE_CLAUSE)? (FINALLY_CLAUSE)? END_KW`; the
/// `try` body is delegated to `lower_block_body` and each clause to
/// [`lower_branch_clause`]. A `catch` clause may carry a bound variable
/// (`catch e`), which is the clause's first child node before its `BLOCK` and is
/// lowered recursively (a plain `NAME`, a `$`-interpolation, or a `var"…"`).
///
/// As with `if` and the loops, a **non-empty** body always explodes vertical; an
/// **empty** body (any branch) makes `lower_block_body` return `None` and the
/// whole `try` falls back to the verbatim transparent lowering. `try` bodies are
/// never `return`-inserted, so there is no semantic-rewrite risk. Any unmodeled
/// shape bails to transparent.
fn lower_try(node: &SyntaxNode) -> Ir {
    let mut block: Option<SyntaxNode> = None;
    let mut clauses: Vec<SyntaxNode> = Vec::new();
    let mut saw_try = false;
    let mut saw_end = false;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) => match child.kind() {
                SyntaxKind::BLOCK if block.is_none() => block = Some(child),
                SyntaxKind::CATCH_CLAUSE | SyntaxKind::ELSE_CLAUSE | SyntaxKind::FINALLY_CLAUSE
                    if block.is_some() =>
                {
                    clauses.push(child)
                }
                _ => return lower_transparent(node),
            },
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::TRY_KW if !saw_try => saw_try = true,
                SyntaxKind::END_KW => saw_end = true,
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => {}
                _ => return lower_transparent(node),
            },
        }
    }

    let (true, true, Some(block)) = (saw_try, saw_end, block) else {
        return lower_transparent(node);
    };
    let Some(body) = lower_block_body(&block) else {
        return lower_transparent(node);
    };

    let mut parts: Vec<Ir> = vec![Ir::text("try"), body];
    for clause in &clauses {
        let Some(clause_ir) = lower_branch_clause(clause) else {
            return lower_transparent(node);
        };
        parts.push(clause_ir);
    }
    parts.push(Ir::HardLine);
    parts.push(Ir::text("end"));
    Ir::concat(parts)
}

/// Render one clause of an `if`/`try` chain (`elseif`/`else`/`catch`/`finally`)
/// as a `HardLine` + the keyword at column 0, an optional space-separated header
/// (the `elseif` condition or the `catch` variable), and the indented body. The
/// header, when present, is lowered recursively. Returns `None` (the caller bails
/// the whole construct to transparent) for an empty body or any unmodeled shape.
fn lower_branch_clause(clause: &SyntaxNode) -> Option<Ir> {
    let kw = match clause.kind() {
        SyntaxKind::ELSEIF_CLAUSE => "elseif",
        SyntaxKind::ELSE_CLAUSE => "else",
        SyntaxKind::CATCH_CLAUSE => "catch",
        SyntaxKind::FINALLY_CLAUSE => "finally",
        _ => return None,
    };

    let mut header: Option<SyntaxNode> = None;
    let mut block: Option<SyntaxNode> = None;
    let mut saw_kw = false;

    for el in clause.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) => match child.kind() {
                SyntaxKind::BLOCK if block.is_none() => block = Some(child),
                _ if block.is_none() && header.is_none() => header = Some(child),
                _ => return None,
            },
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::ELSEIF_KW
                | SyntaxKind::ELSE_KW
                | SyntaxKind::CATCH_KW
                | SyntaxKind::FINALLY_KW
                    if !saw_kw =>
                {
                    saw_kw = true
                }
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => {}
                _ => return None,
            },
        }
    }

    let (true, Some(block)) = (saw_kw, block) else {
        return None;
    };
    let body = lower_block_body(&block)?;

    let mut parts: Vec<Ir> = vec![Ir::HardLine, Ir::text(kw)];
    if let Some(header) = header {
        parts.push(Ir::text(" "));
        parts.push(lower_node(&header));
    }
    parts.push(body);
    Some(Ir::concat(parts))
}

/// One source line of a block body: zero or more statements (`; `-joined) plus an
/// optional trailing line comment.
#[derive(Default)]
struct BodyLine {
    stmts: Vec<Ir>,
    comment: Option<Ir>,
}

impl BodyLine {
    /// A line carrying neither a statement nor a comment — a blank line.
    fn is_blank(&self) -> bool {
        self.stmts.is_empty() && self.comment.is_none()
    }
}

/// Lower the statements of a `BLOCK` into an indented, vertically-broken body,
/// returning `None` (the caller bails to the transparent lowering) for an empty
/// block or any shape this does not model.
///
/// Statements are grouped into **lines**: a `NEWLINE` starts a new line, while a
/// `;` keeps the next statement on the current line (`begin x; y end` →
/// `⏎    x; y`). Each statement is lowered recursively, so its own normalization
/// still applies and a nested block indents further. Blank lines are preserved
/// (capped at [`MAX_BLANK_LINES`] via an [`Ir::BlankLine`]): between statements,
/// after the keyword (leading), and before `end` (trailing) — the framing break
/// the layout always adds absorbs one newline on each side.
///
/// Comments (line `#` and block `#= … =#`) are preserved. An **own-line** comment
/// becomes its own line, re-indented to the body; a multi-line block comment keeps
/// its continuation lines verbatim (only the `#=` line takes the body indent). A
/// **trailing** comment (the line already holds a statement) is attached after it
/// with a single space — a Tenet-1 divergence: Runic preserves the user's pre-`#`
/// whitespace (≥1 space) verbatim, but Fatou canonicalizes to exactly one space.
/// Two statements with no separator, a node after a comment, or any unexpected
/// token returns `None`.
fn lower_block_body(block: &SyntaxNode) -> Option<Ir> {
    // `expect_sep` guards against two adjacent statement nodes with no `;`/newline
    // between them, and against a node following a comment on the same line.
    let mut lines: Vec<BodyLine> = vec![BodyLine::default()];
    let mut expect_sep = false;

    for el in block.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) => {
                if expect_sep {
                    return None;
                }
                lines.last_mut().unwrap().stmts.push(lower_node(&child));
                expect_sep = true;
            }
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::WHITESPACE => {}
                // A `;` after a comment would put a following statement on the
                // wrong side of the recorded comment, so bail. (A line comment runs
                // to end of line so this never arises there, but an inline block
                // comment can be followed by `; stmt` on the same line.)
                SyntaxKind::SEMICOLON => {
                    if lines.last().unwrap().comment.is_some() {
                        return None;
                    }
                    expect_sep = false;
                }
                SyntaxKind::NEWLINE => {
                    lines.push(BodyLine::default());
                    expect_sep = false;
                }
                // A line comment closes its line: own-line (the line is empty) or
                // trailing (a statement precedes it). Either way it becomes the
                // line's `comment`; `expect_sep` then bails a node that follows
                // without an intervening newline. A second comment on one line is
                // impossible (a comment runs to end of line), so guard with `None`.
                SyntaxKind::COMMENT => {
                    let line = lines.last_mut().unwrap();
                    if line.comment.is_some() {
                        return None;
                    }
                    let text = tok.text().trim_end_matches([' ', '\t']);
                    line.comment = Some(Ir::text(text));
                    expect_sep = true;
                }
                // A block comment is preserved verbatim — its interior (including
                // continuation-line indentation) is kept byte-for-byte (Runic only
                // re-indents the line the `#=` opens, which the framing `HardLine`
                // here supplies). Own-line or trailing, it fills the line's
                // `comment` slot exactly like a line comment; `expect_sep`/the
                // `;`-guard then bail any content that would follow it on the same
                // line (an unmodeled inline `#= … =#` mid-expression bails its
                // owning node, so it never reaches block level).
                SyntaxKind::BLOCK_COMMENT => {
                    let line = lines.last_mut().unwrap();
                    if line.comment.is_some() {
                        return None;
                    }
                    line.comment = Some(Ir::text(tok.text()));
                    expect_sep = true;
                }
                _ => return None,
            },
        }
    }

    // Locate the content span. Empty lines outside it are the leading (after the
    // keyword) and trailing (before `end`) framing lines plus any kept blanks:
    // one empty line on each side is absorbed into the framing break, the rest
    // become preserved blanks (capped at `MAX_BLANK_LINES`). All-empty ⇒ `None`.
    let first = lines.iter().position(|l| !l.is_blank())?;
    let last = lines.iter().rposition(|l| !l.is_blank()).unwrap();
    let leading_blanks = first.saturating_sub(1).min(MAX_BLANK_LINES);
    let trailing_blanks = (lines.len() - 1 - last)
        .saturating_sub(1)
        .min(MAX_BLANK_LINES);
    let content = &lines[first..=last];

    let mut inner: Vec<Ir> =
        Vec::with_capacity(content.len() * 2 + leading_blanks + trailing_blanks);
    for _ in 0..leading_blanks {
        inner.push(Ir::BlankLine);
    }
    // Interior empty lines are blank lines: emit a bare newline each (a `HardLine`
    // would leave the indent as trailing whitespace), capped at `MAX_BLANK_LINES`.
    let mut pending_blanks = 0usize;
    for line in content {
        if line.is_blank() {
            pending_blanks += 1;
            continue;
        }
        for _ in 0..pending_blanks.min(MAX_BLANK_LINES) {
            inner.push(Ir::BlankLine);
        }
        pending_blanks = 0;
        inner.push(Ir::HardLine); // framing break / re-indent for this line
        for (j, stmt) in line.stmts.iter().enumerate() {
            if j > 0 {
                inner.push(Ir::text("; "));
            }
            inner.push(stmt.clone());
        }
        if let Some(comment) = &line.comment {
            // One canonical space before a trailing comment; an own-line comment
            // (no preceding statement) sits flush at the body indent.
            if !line.stmts.is_empty() {
                inner.push(Ir::text(" "));
            }
            inner.push(comment.clone());
        }
    }
    for _ in 0..trailing_blanks {
        inner.push(Ir::BlankLine);
    }

    Some(Ir::indent(Ir::concat(inner)))
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
