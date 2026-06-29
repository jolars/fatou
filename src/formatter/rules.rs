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
        SyntaxKind::WHERE_EXPR => lower_where(node),
        SyntaxKind::COMPARISON_EXPR => lower_comparison(node),
        SyntaxKind::TERNARY_EXPR => lower_ternary(node),
        SyntaxKind::RANGE_EXPR => lower_range(node),
        SyntaxKind::TYPE_ANNOTATION => lower_type_annotation(node),
        SyntaxKind::MATRIX_EXPR => lower_matrix(node),
        SyntaxKind::ARG_LIST => lower_arg_list(node),
        SyntaxKind::TUPLE_EXPR | SyntaxKind::VECT_EXPR | SyntaxKind::BRACES => {
            lower_collection(node)
        }
        SyntaxKind::BARE_TUPLE_EXPR => lower_bare_tuple(node),
        SyntaxKind::KEYWORD_ARG => lower_keyword_arg(node),
        SyntaxKind::PARAMETERS => lower_parameters(node),
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
