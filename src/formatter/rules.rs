//! Per-construct formatting rules: lower the lossless CST into the layout
//! [`Ir`](crate::formatter::ir::Ir) the [`printer`](crate::formatter::printer)
//! renders. The walk is a **walking skeleton**: only the constructs with a rule
//! reshape their layout; every other node is lowered *transparently* (children
//! emitted in order, tokens verbatim), so unhandled syntax stays byte-identical
//! while any handled descendant is still normalized. As rules land, nodes move
//! from the transparent fallback to a dedicated arm.
//!
//! The style is Fatou's own (see `AGENTS.md`); the hand-authored fixture gate
//! lives in `tests/formatter.rs`. NOTE: many rules below still mirror the source's
//! line breaks (a legacy of the removed Runic target), which Tenet 1 forbids; they
//! are re-evaluated construct-by-construct as the width-driven reflow engine lands.

use rowan::NodeOrToken;

use crate::formatter::ir::Ir;
use crate::syntax::{SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken};

/// Lower a parsed document (the `ROOT` node) into an `Ir` document.
pub fn lower(root: &SyntaxNode) -> Ir {
    if root.kind() == SyntaxKind::ROOT {
        lower_root(root)
    } else {
        lower_node(root)
    }
}

fn lower_node(node: &SyntaxNode) -> Ir {
    match node.kind() {
        SyntaxKind::BINARY_EXPR | SyntaxKind::ASSIGNMENT_EXPR => lower_binary(node),
        SyntaxKind::ARROW_EXPR => lower_arrow(node),
        SyntaxKind::WHERE_EXPR => lower_where(node),
        SyntaxKind::COMPARISON_EXPR => lower_comparison(node),
        SyntaxKind::TERNARY_EXPR => lower_ternary(node),
        SyntaxKind::RANGE_EXPR => lower_range(node),
        SyntaxKind::UNARY_EXPR => lower_unary(node),
        SyntaxKind::SPLAT_EXPR => lower_splat(node),
        SyntaxKind::TYPE_ANNOTATION => lower_type_annotation(node),
        SyntaxKind::MATRIX_EXPR | SyntaxKind::BRACESCAT_EXPR => lower_matrix(node),
        SyntaxKind::BEGIN_EXPR | SyntaxKind::QUOTE_EXPR => lower_block_expr(node),
        SyntaxKind::LET_EXPR => lower_let(node),
        SyntaxKind::WHILE_EXPR | SyntaxKind::FOR_EXPR => lower_loop(node),
        SyntaxKind::STRUCT_DEF => lower_struct(node),
        SyntaxKind::FUNCTION_DEF | SyntaxKind::MACRO_DEF => lower_function(node),
        SyntaxKind::DO_EXPR => lower_do(node),
        SyntaxKind::ABSTRACT_DEF | SyntaxKind::PRIMITIVE_DEF => lower_type_decl(node),
        SyntaxKind::MODULE_DEF => lower_module(node),
        SyntaxKind::IF_EXPR => lower_if(node),
        SyntaxKind::TRY_EXPR => lower_try(node),
        SyntaxKind::ARG_LIST => lower_arg_list(node),
        SyntaxKind::CALL_EXPR => lower_call(node),
        SyntaxKind::INDEX_EXPR => lower_index(node),
        SyntaxKind::MACRO_CALL => lower_macro_call(node),
        SyntaxKind::TUPLE_EXPR | SyntaxKind::VECT_EXPR | SyntaxKind::BRACES => {
            lower_collection(node)
        }
        SyntaxKind::COMPREHENSION | SyntaxKind::GENERATOR | SyntaxKind::BRACES_COMPREHENSION => {
            lower_comprehension(node)
        }
        SyntaxKind::PAREN_EXPR => lower_paren(node),
        SyntaxKind::PAREN_BLOCK => lower_paren_block(node),
        SyntaxKind::BARE_TUPLE_EXPR | SyntaxKind::LET_BINDINGS => lower_comma_list(node),
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
        SyntaxKind::STRING_LITERAL | SyntaxKind::CMD_LITERAL => lower_string_literal(node),
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
/// whitespace: a `WHITESPACE` run sitting immediately before a line break is
/// dropped, and a line `COMMENT`'s trailing blanks are stripped. String content
/// and block comments are left verbatim — their interior whitespace is the
/// user's.
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
/// a single space on each side, except for the tight operators (`^`, `:`, `.`)
/// the target style packs without spaces. A chain of one precedence tier lays out as
/// one n-ary group: the parser already folds `a + b + c` into a single flat
/// `BINARY_EXPR` with three operands, and [`collect_binary_chain`] flattens the
/// chains the parser keeps *nested* — both same-operator (`&&`, `||`, `|>`, `=>`, …)
/// and mixed same-tier (`+`/`-`, `*`/`/`, `<<`/`>>`) — into the same shape, so every
/// operator in the tier breaks together regardless of how the parser associated it.
///
/// **Width-driven (Tenet 1).** Source line breaks carry no layout information: the
/// chain lays out flat (`a + b + c`) when it fits `line_width`, else it breaks. The
/// break shape follows Air's model:
///
/// - **Operator-trailing.** The operator stays on the line it ends; the following
///   operand wraps. Each breakable operator gap is an [`Ir::Line`] (a space when
///   flat, a newline when broken).
/// - **Uniform same-tier break.** A too-wide chain of one precedence tier breaks at
///   *every* operator (`a &&⏎ b &&⏎ c`, `a +⏎ b -⏎ c`), not just the outermost —
///   mixed same-tier operators (`+`/`-`, `*`/`/`, `<<`/`>>`) flatten together like a
///   same-operator chain. A subexpression in a tighter or looser tier is its own
///   node/group, so it stays flat on its line while the enclosing chain breaks
///   (`a +⏎ b * c +⏎ d`, `a &&⏎ b || c` never splits the tighter `b || c`). When an
///   inner subexpression is *itself* forced to break, its indent nests on the parent's.
/// - **Assignment operators never break.** An `ASSIGNMENT_EXPR` joins its operator
///   with flat spaces (` = `) and emits no group of its own; the break is biased
///   into the right-hand side, whose own group absorbs it
///   (`x = a +⏎ b`, never `x =⏎ a + b`).
///
/// Only the clean alternating shape is reshaped; anything else (an interleaved
/// comment, error recovery, a missing operand) falls back to the
/// verbatim-preserving transparent lowering so we never mangle a construct we
/// don't fully understand.
fn lower_binary(node: &SyntaxNode) -> Ir {
    // A `.`-rooted spine of enough calls is a fluent method chain, laid out as a
    // trailing-dot break group rather than a flat tight `.` access.
    if let Some(ir) = try_lower_chain(node) {
        return ir;
    }

    // Assignment operators (`=`, `+=`, …) never introduce a break; the break is
    // biased into the right-hand side, so the operator gap is a flat space.
    let is_assignment = node.kind() == SyntaxKind::ASSIGNMENT_EXPR;

    let Some(op_kind) = binary_op_kind(node) else {
        return lower_transparent(node);
    };
    // Flatten a same-operator nested chain (`a && b && c`, `a |> b |> c`) into one
    // group so every operator breaks together, matching the parser's own n-ary
    // folding of `+`/`*`. Tight operators and assignment never break, so they gain
    // nothing from flattening and keep their nested lowering.
    let flatten = !is_assignment && !is_tight_binop(op_kind);
    let mut items: Vec<SyntaxElement> = Vec::new();
    if !collect_binary_chain(node, op_kind, flatten, &mut items) {
        return lower_transparent(node);
    }

    let mut first: Option<Ir> = None;
    let mut rest: Vec<Ir> = Vec::new();
    // Separator to emit before the upcoming operand: `None` after a tight operator
    // (the operand abuts it), else the breakable `Ir::Line` (or a flat space for an
    // assignment operator).
    let mut next_sep: Option<Ir> = None;
    let mut expect_operand = true;
    let mut operand_count = 0usize;
    let mut op_count = 0usize;
    // The operand node most recently seen, so a tight `.^` can check it for the
    // integer-literal retokenization hazard before snugging.
    let mut prev_operand: Option<SyntaxNode> = None;

    for el in items {
        match el {
            NodeOrToken::Node(child) => {
                if !expect_operand {
                    return lower_transparent(node);
                }
                let ir = lower_node(&child);
                if operand_count == 0 {
                    first = Some(ir);
                } else {
                    if let Some(sep) = next_sep.take() {
                        rest.push(sep);
                    }
                    rest.push(ir);
                }
                prev_operand = Some(child);
                operand_count += 1;
                expect_operand = false;
            }
            NodeOrToken::Token(tok) => {
                if expect_operand {
                    return lower_transparent(node);
                }
                // An operator. It sits trailing on the line it ends.
                let tight = !is_assignment
                    && is_tight_binop(tok.kind())
                    // A `.^` after an integer literal would re-lex the operand's
                    // trailing digit(s) plus the operator's leading `.` as a float.
                    && !(tok.kind() == SyntaxKind::DOT_CARET
                        && prev_operand
                            .as_ref()
                            .is_some_and(dot_caret_snug_retokenizes));
                if tight {
                    rest.push(Ir::text(tok.text().to_string()));
                    next_sep = None;
                } else {
                    rest.push(Ir::text(format!(" {}", tok.text())));
                    next_sep = Some(if is_assignment {
                        Ir::text(" ")
                    } else {
                        Ir::Line
                    });
                }
                op_count += 1;
                expect_operand = true;
            }
        }
    }

    // A well-formed chain ends on an operand, has at least two of them, and one
    // fewer operator than operands.
    let Some(first) = first else {
        return lower_transparent(node);
    };
    if expect_operand || operand_count < 2 || op_count + 1 != operand_count {
        return lower_transparent(node);
    }

    if is_assignment {
        // No break at the operator; the right-hand side's own group absorbs any
        // break, so this node adds neither a group nor an indent.
        let mut parts = vec![first];
        parts.extend(rest);
        return Ir::concat(parts);
    }

    // One width-driven group with its own continuation indent: flat when it fits,
    // else operator-trailing with the wrapped operands indented one step.
    Ir::group(Ir::concat([first, Ir::indent(Ir::concat(rest))]))
}

/// The kind of `node`'s operator token — the first token that lands where an
/// operator is expected (after the left operand). Returns `None` for a malformed
/// binary node with no operator, letting the caller bail transparent. Trivia and
/// comments are skipped so the search is position-based, mirroring the operand /
/// operator alternation the layout loop assumes.
fn binary_op_kind(node: &SyntaxNode) -> Option<SyntaxKind> {
    let mut expect_operand = true;
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(_) => expect_operand = false,
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::WHITESPACE
                | SyntaxKind::NEWLINE
                | SyntaxKind::COMMENT
                | SyntaxKind::BLOCK_COMMENT => {}
                _ if !expect_operand => return Some(tok.kind()),
                _ => {}
            },
        }
    }
    None
}

/// Append `node`'s operand / operator alternation to `items`, dropping trivia. When
/// `flatten` is set, an operand child that is itself a `BINARY_EXPR` whose operator
/// breaks in the same tier as `op_kind` (see [`same_break_tier`]) is descended into
/// rather than pushed whole, so a chain the parser nested collapses into one flat
/// operand/operator stream and thus one break group. This folds both a same-operator
/// nested chain (`a && b && c`, right-nested; `a |> b |> c`, left-nested) and a mixed
/// same-precedence chain (`a + b - c`, `a * b / c`, left-nested). A tighter or
/// looser subexpression (`a + b * c`, `a && b || c`) sits in a different tier, so it
/// is pushed whole and lowered as its own group.
///
/// Returns `false` on any shape we don't fully model — an interleaved comment, a
/// missing operand, two operands in a row — so the caller falls back to the
/// verbatim transparent lowering.
fn collect_binary_chain(
    node: &SyntaxNode,
    op_kind: SyntaxKind,
    flatten: bool,
    items: &mut Vec<SyntaxElement>,
) -> bool {
    let mut expect_operand = true;
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) => {
                if !expect_operand {
                    return false;
                }
                if flatten
                    && child.kind() == SyntaxKind::BINARY_EXPR
                    && binary_op_kind(&child).is_some_and(|k| same_break_tier(k, op_kind))
                {
                    if !collect_binary_chain(&child, op_kind, flatten, items) {
                        return false;
                    }
                } else {
                    items.push(NodeOrToken::Node(child));
                }
                expect_operand = false;
            }
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => {}
                SyntaxKind::COMMENT | SyntaxKind::BLOCK_COMMENT => return false,
                _ if !expect_operand => {
                    items.push(NodeOrToken::Token(tok));
                    expect_operand = true;
                }
                _ => return false,
            },
        }
    }
    // A well-formed chain ends on an operand.
    !expect_operand
}

/// Whether two operator kinds break together in one chain group: the same kind
/// (`a + b + c`), or two different operators sharing a precedence tier
/// (`a + b - c`, `a * b / c`, `a << b >> c`, `a => b --> c`). The parser nests a
/// mixed same-tier chain (left for the arithmetic tiers, right for the arrow/pair
/// tier), so flattening on tier — not just exact kind — collapses it into one
/// break group, matching the uniform break a same-operator chain already gets.
/// The flatten is layout-only: the operand/operator text stream is identical
/// under either association. Unicode operators collapse to one `UNICODE_OP` kind
/// that spans several tiers, so they only flatten on exact-kind equality (no
/// tier).
fn same_break_tier(a: SyntaxKind, b: SyntaxKind) -> bool {
    a == b || binary_prec_class(a).is_some_and(|c| Some(c) == binary_prec_class(b))
}

/// The break-flatten tier of a binary operator whose tier holds more than one
/// operator kind, mirroring the parser's `infix_binding_power` classes. Only
/// these multi-operator tiers matter for flattening; a single-kind tier (`^`,
/// `//`, `|>`, `&&`, …) is handled by exact-kind equality, and tight operators
/// never break, so they are `None` here.
fn binary_prec_class(kind: SyntaxKind) -> Option<u8> {
    use SyntaxKind::*;
    Some(match kind {
        // Plus tier: `+ - |` (and broadcast `.+ .- .|`), left-associative.
        PLUS | MINUS | PIPE | DOT_PLUS | DOT_MINUS | DOT_PIPE => 0,
        // Times tier: `* / \ % &` (and their broadcast forms), left-associative.
        STAR | SLASH | BACKSLASH | PERCENT | AMP | DOT_STAR | DOT_SLASH | DOT_BACKSLASH
        | DOT_PERCENT | DOT_AMP => 1,
        // Bitshift tier: `<< >> >>>`, left-associative.
        SHL | SHR | USHR => 2,
        // Arrow/pair tier: `=> --> <-- <-->` (and their broadcast forms),
        // right-associative.
        FAT_ARROW | LONG_ARROW | LEFT_RIGHT_ARROW | LEFT_LONG_ARROW | DOT_FAT_ARROW
        | DOT_LONG_ARROW | DOT_LEFT_LONG_ARROW | DOT_LEFT_RIGHT_ARROW => 3,
        _ => return None,
    })
}

/// A DOT-spine needs at least this many *called* links before it lays out as a
/// broken method chain. One call (`recv.method(args)`) is not a chain — it stays
/// transparent so its argument list, not the dot, absorbs any break.
const MIN_CHAIN_CALLS: usize = 2;

/// One link in a fluent-chain spine, in print order (receiver-first). `args` is
/// `Some` for a called link (`.name(args)`) and `None` for a bare field access
/// (`.name`).
struct ChainLink {
    name: SyntaxNode,
    args: Option<SyntaxNode>,
}

/// Try to lay out `node` as a fluent method chain — a left-nested run of `.name`
/// field accesses and `.name(args)` calls over a base receiver. Returns `None`
/// (so the caller keeps its normal lowering) unless the spine has at least
/// [`MIN_CHAIN_CALLS`] called links; a shorter spine is a qualified name or a
/// single call, which never break at the dot.
///
/// **Layout (Tenet 1).** One width-driven group: flat
/// `recv.a(x).b(y)` when it fits `line_width`, else the receiver stays on the
/// opening line and each *called* link wraps to its own continuation-indented
/// line with the `.` trailing the line before it (`recv.` / `····a(x).` / …).
/// The trailing-dot spelling is the only broken form Julia reparses as the same
/// chain (a leading-dot `recv⏎.a(x)` is a parse error). Bare field accesses
/// (module qualifiers, receiver-prefix accesses like `obj.config`) never break;
/// they glue to the segment before them, matching the "only call links break"
/// rule.
fn try_lower_chain(node: &SyntaxNode) -> Option<Ir> {
    let (base, links) = collect_chain(node)?;
    let call_count = links.iter().filter(|l| l.args.is_some()).count();
    if call_count < MIN_CHAIN_CALLS {
        return None;
    }

    let mut inner: Vec<Ir> = Vec::new();
    for link in &links {
        let name_ir = lower_node(&link.name);
        match &link.args {
            // A called link breaks before its `.`: the dot ends the previous
            // line (trailing-dot), then the name and args wrap one step.
            Some(args) => inner.push(Ir::concat([
                Ir::text("."),
                Ir::SoftLine,
                name_ir,
                lower_arg_list(args),
            ])),
            // A bare field access glues to the current line.
            None => inner.push(Ir::concat([Ir::text("."), name_ir])),
        }
    }

    Some(Ir::group(Ir::concat([
        lower_node(&base),
        Ir::indent(Ir::concat(inner)),
    ])))
}

/// Collect the fluent-chain spine rooted at `node`: peel the left-nested
/// `.name(args)` calls and `.name` field accesses down to the base receiver,
/// returning `(base, links)` with the links in print order. `None` on any shape
/// we don't fully model — a comment interleaved in a call or dot access, a
/// broadcast `f.(x)` dot, or a non-`.` operator — so the caller bails to the
/// verbatim transparent lowering.
fn collect_chain(node: &SyntaxNode) -> Option<(SyntaxNode, Vec<ChainLink>)> {
    let mut links: Vec<ChainLink> = Vec::new(); // outer-first; reversed below
    let mut cur = node.clone();
    loop {
        match cur.kind() {
            SyntaxKind::CALL_EXPR => {
                let (callee, arg_list) = call_parts(&cur)?;
                let Some((inner, name)) = dot_access_parts(&callee) else {
                    // A base call (`f(x)`): the whole `CALL_EXPR` is the receiver.
                    break;
                };
                links.push(ChainLink {
                    name,
                    args: Some(arg_list),
                });
                cur = inner;
            }
            SyntaxKind::BINARY_EXPR => {
                let Some((inner, name)) = dot_access_parts(&cur) else {
                    break;
                };
                links.push(ChainLink { name, args: None });
                cur = inner;
            }
            _ => break,
        }
    }
    links.reverse();
    Some((cur, links))
}

/// The callee and `ARG_LIST` of a clean `callee(args)` call, skipping trivia.
/// `None` if the call carries a comment or any token/node beyond that pair (a
/// broadcast `f.(x)` leaves a stray `.` token, for instance).
fn call_parts(node: &SyntaxNode) -> Option<(SyntaxNode, SyntaxNode)> {
    let mut nodes: Vec<SyntaxNode> = Vec::new();
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) => nodes.push(child),
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => {}
                _ => return None,
            },
        }
    }
    let [callee, arg_list] = nodes.as_slice() else {
        return None;
    };
    if arg_list.kind() != SyntaxKind::ARG_LIST {
        return None;
    }
    Some((callee.clone(), arg_list.clone()))
}

/// The receiver and field-name nodes of a plain `.`-access `recv.name`, skipping
/// trivia (including a source newline, so a source-broken chain reflows). `None`
/// unless the shape is exactly `<node> . <node>` with a plain `DOT` operator — a
/// broadcast `.+`/`.^`, a comment, or an extra child bails.
fn dot_access_parts(node: &SyntaxNode) -> Option<(SyntaxNode, SyntaxNode)> {
    if node.kind() != SyntaxKind::BINARY_EXPR {
        return None;
    }
    let mut lhs: Option<SyntaxNode> = None;
    let mut rhs: Option<SyntaxNode> = None;
    let mut saw_dot = false;
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) => {
                let slot = if saw_dot { &mut rhs } else { &mut lhs };
                if slot.is_some() {
                    return None;
                }
                *slot = Some(child);
            }
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => {}
                SyntaxKind::DOT if !saw_dot => saw_dot = true,
                _ => return None,
            },
        }
    }
    match (lhs, saw_dot, rhs) {
        (Some(l), true, Some(r)) => Some((l, r)),
        _ => None,
    }
}

/// Lower a `CALL_EXPR`. A fluent method chain (see [`try_lower_chain`]) folds
/// into one width-driven group; every other call stays transparent (its
/// `ARG_LIST` still normalizes and breaks on its own).
fn lower_call(node: &SyntaxNode) -> Ir {
    try_lower_chain(node).unwrap_or_else(|| lower_transparent(node))
}

/// Lay out an anonymous-function arrow (`x -> y`, `(a, b) -> a + b`) with a single
/// space on each side of the `->`. Operand nodes are lowered recursively, so a
/// nested arrow (`x -> y -> z`, right-associative) or a normalized body
/// (`map(x -> x^2, a)`) keeps formatting. The target style always spaces the
/// arrow.
///
/// **Width-driven (Tenet 1).** Like an assignment operator in [`lower_binary`], the
/// `->` never introduces a break of its own: it stays trailing-flat (` -> `) and the
/// break is biased into the right-hand side, whose own group absorbs it
/// (`arg -> body +⏎ more`, never `arg ->⏎ body + more`). Source line breaks carry no
/// layout information and are ignored like whitespace, so a multi-line body reflows.
///
/// As with [`lower_binary`], only the clean single-lambda shape `<lhs> -> <rhs>` is
/// reshaped: an interleaved comment, error recovery, or a missing operand falls back
/// to the verbatim transparent lowering.
/// Lay out a unary prefix expression (`-a`, `!b`, `~x`, `√x`, `¬p`) — the operator
/// snugs directly to its operand with no space, normalizing whatever whitespace the
/// parser left between them (Tenet 1). The operand recurses through [`lower_node`], so
/// it normalizes internally (`-f( x )` → `-f(x)`).
///
/// A `UNARY_EXPR` is always the prefix shape `<op> <operand>` (a postfix `'` is a
/// separate `POSTFIX_EXPR`). Snugging is unsafe when the operand itself leads with a
/// symbolic operator: `- -a` parses as nested unary, and `--a` would retokenize as the
/// `--` operator. In that case — and on any interleaved comment, missing operand, or
/// unexpected shape — fall back to the verbatim transparent lowering.
fn lower_unary(node: &SyntaxNode) -> Ir {
    let mut op: Option<String> = None;
    let mut operand: Option<SyntaxNode> = None;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => {}
                SyntaxKind::COMMENT | SyntaxKind::BLOCK_COMMENT => {
                    return lower_transparent(node);
                }
                // The single prefix operator, which must precede the operand.
                _ if op.is_none() && operand.is_none() => op = Some(tok.text().to_string()),
                _ => return lower_transparent(node),
            },
            NodeOrToken::Node(child) => {
                if op.is_none() || operand.is_some() {
                    return lower_transparent(node);
                }
                operand = Some(child);
            }
        }
    }

    let (Some(op), Some(operand)) = (op, operand) else {
        return lower_transparent(node);
    };

    // Snugging is unsafe when the operand leads with another operator: the two could
    // merge into a longer operator (`- -a` → `--a`). Keep such forms verbatim.
    if operand.kind() == SyntaxKind::UNARY_EXPR || operand_leads_with_operator(&operand) {
        return lower_transparent(node);
    }

    Ir::concat([Ir::text(op), lower_node(&operand)])
}

/// Whether `node`'s first token begins with a symbolic operator character — a
/// conservative guard against a prefix operator retokenizing when snugged onto it.
fn operand_leads_with_operator(node: &SyntaxNode) -> bool {
    node.first_token()
        .and_then(|t| t.text().chars().next())
        .is_some_and(|c| "+-*/\\^%!~<>=&|:$?".contains(c))
}

/// Lay out a splat (`x...`) — the postfix `...` snugs directly to its operand with
/// no space, normalizing whatever whitespace the parser left between them (Tenet 1).
/// The operand recurses through [`lower_node`], so it normalizes internally
/// (`f(a , b) ...` → `f(a, b)...`). This is the postfix analog of [`lower_unary`].
///
/// A `SPLAT_EXPR` is always the shape `<operand> ...`. Snugging is unsafe when the
/// operand prints a trailing `.` — it would merge with the leading `.` of `...` into
/// `....`. A bare float `1.` is the only literal that spells a trailing dot, and
/// [`normalize_float`] always rewrites it to a safe trailing digit (`1.` → `1.0`), so
/// a `LITERAL` operand is always snug-safe; any *other* operand whose last token ends
/// in `.` (an unmodeled shape lowered verbatim) bails to the transparent lowering, as
/// do an interleaved comment, a missing operand, or an unexpected shape.
///
/// Snugging is also withheld when the operand ends in a closing bracket (`)`/`]`/`}` —
/// a call, index, paren, curly, or bracket collection): Fatou's parser currently
/// rejects `g(x)...` (splat directly after a closing bracket) as a lone operator even
/// though it is valid Julia, so the snug form would fail the stability reparse. Such
/// operands bail to the verbatim (spaced) lowering until the parser gap is closed
/// (handed off to `parser-parity`); remove this guard and widen the fixture then.
fn lower_splat(node: &SyntaxNode) -> Ir {
    let mut operand: Option<SyntaxNode> = None;
    let mut dots: Option<String> = None;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => {}
                SyntaxKind::DOT_DOT_DOT if operand.is_some() && dots.is_none() => {
                    dots = Some(tok.text().to_string());
                }
                _ => return lower_transparent(node),
            },
            NodeOrToken::Node(child) => {
                if operand.is_some() {
                    return lower_transparent(node);
                }
                operand = Some(child);
            }
        }
    }

    let (Some(operand), Some(dots)) = (operand, dots) else {
        return lower_transparent(node);
    };

    // Snugging is unsafe when the operand *prints* a trailing `.`: it would merge with
    // the leading `.` of `...` into `....`. A float literal is the only trailing-dot
    // spelling and normalizes to a safe trailing digit, so a `LITERAL` operand always
    // snugs; guard only the (essentially unproducible) verbatim shape whose last raw
    // token ends in `.`.
    let trails_with_dot = operand.kind() != SyntaxKind::LITERAL
        && operand
            .last_token()
            .is_some_and(|t| t.text().ends_with('.'));
    // The parser can't yet reparse a splat snugged onto a closing bracket (`g(x)...`),
    // so a bracket-closing operand keeps its verbatim spacing. See the doc comment.
    let ends_in_bracket = operand.last_token().is_some_and(|t| {
        matches!(
            t.kind(),
            SyntaxKind::RPAREN | SyntaxKind::RBRACKET | SyntaxKind::RBRACE
        )
    });
    if trails_with_dot || ends_in_bracket {
        return lower_transparent(node);
    }

    Ir::concat([lower_node(&operand), Ir::text(dots)])
}

fn lower_arrow(node: &SyntaxNode) -> Ir {
    let mut operands: Vec<SyntaxNode> = Vec::new();
    let mut op: Option<SyntaxToken> = None;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) => operands.push(child),
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => {}
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

/// Lay out a parenthesized expression (`(a + b)`).
///
/// Width-driven (Tenet 1): the parens frame a single inner node in one `Ir::group`.
/// **No padding** inside the parentheses—`( a + b )` → `(a + b)`, `(  x  )` → `(x)`;
/// the incidental whitespace flanking the inner expression is stripped. When the
/// content fits `line_width` the group stays flat (`(a + b)`); otherwise it takes
/// the tight framing (`(` alone, the inner expression indented one step, `)` flush).
/// Source line breaks never force this — `x = (\n1 + 2\n)` collapses to `(1 + 2)`
/// because it fits; only the content's own width (or a hard break it carries, e.g. a
/// nested block) drives the split. The inner node is lowered recursively, so a
/// nested paren (`( (a) )` → `((a))`) and the inner expression's own spacing keep
/// normalizing, and a broken binary's continuation indent composes on top of the
/// paren's content indent.
///
/// As with [`lower_arrow`], only clean shapes are reshaped: an interleaved comment
/// (in a direct gap), error recovery, or a missing/extra operand falls back to the
/// verbatim transparent lowering. The `;`-block form `(a; b)` is a distinct
/// `PAREN_BLOCK` node, and a tuple `(a, b)` is a `TUPLE_EXPR`, so neither reaches
/// here.
///
/// **Blank lines are stripped**: a single parenthesized value gains nothing from
/// interior blank lines, so the loop skips every `NEWLINE`/`WHITESPACE` token and
/// only the inner node reaches the layout.
fn lower_paren(node: &SyntaxNode) -> Ir {
    match paren_reflow_body(node) {
        Some(body) => Ir::group(body),
        None => lower_transparent(node),
    }
}

/// Build the ungrouped body of a single-value parenthesized expression `(inner)`
/// — the tight framing (`(` / +indent body / `)`) — for [`lower_paren`]'s width
/// group, and also folded into [`lower_index`]'s shared outer group (via
/// [`construct_reflow_body`]) so a too-wide paren-subject chain yields at its own
/// brackets and the index rides the closing `)`, exactly as a tuple subject does.
///
/// Width-driven (Tenet 1): flat `(inner)` when it fits `line_width`, else the
/// inner on its own indented line. Source line breaks never force the break; only
/// the inner content's width (or a hard break it carries, e.g. a nested block)
/// does. Interior blank lines are already dropped — the loop skips every
/// `NEWLINE`/`WHITESPACE` token, so only the single inner node reaches the layout.
///
/// `None` on any unmodeled shape (a stray token, a doubled operand, a missing
/// bracket) — the caller falls back to the verbatim transparent lowering. The
/// `;`-block form `(a; b)` is a distinct `PAREN_BLOCK` node, and a tuple `(a, b)`
/// is a `TUPLE_EXPR`, so neither reaches here.
fn paren_reflow_body(node: &SyntaxNode) -> Option<Ir> {
    let mut inner: Option<SyntaxNode> = None;
    let mut extra_operand = false;
    let mut saw_lparen = false;
    let mut saw_rparen = false;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) => {
                if inner.is_some() {
                    extra_operand = true;
                }
                inner = Some(child);
            }
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => {}
                SyntaxKind::LPAREN if !saw_lparen => saw_lparen = true,
                SyntaxKind::RPAREN if !saw_rparen => saw_rparen = true,
                _ => return None,
            },
        }
    }

    let (true, true, false, Some(inner)) = (saw_lparen, saw_rparen, extra_operand, inner) else {
        return None;
    };

    Some(Ir::concat([
        Ir::text("("),
        Ir::indent(Ir::concat([Ir::SoftLine, lower_node(&inner)])),
        Ir::SoftLine,
        Ir::text(")"),
    ]))
}

/// Lay out a `;`-block `(a; b)` (a `PAREN_BLOCK`, distinct from the single-value
/// `PAREN_EXPR` and the comma tuple `TUPLE_EXPR`). The block is `LPAREN`, a
/// leading statement node, then one `PARAMETERS` node per `; <stmt>` (each
/// carrying the `SEMICOLON`, optional whitespace, and the statement), then
/// `RPAREN`. Each separator packs tight-left/space-right and the padding
/// flanking the inner expressions is stripped: `( a ; b )` → `(a; b)`,
/// `(a;b;)` → `(a; b)` (a trailing `;` produces an arg-less `PARAMETERS` that is
/// dropped). Every statement is lowered recursively, so a nested block
/// (`((a;b);c)` → `((a; b); c)`) and each statement's own spacing keep
/// normalizing.
///
/// The multi-statement block is width-driven (Tenet 1): flat `(a; b; c)` when it
/// fits `line_width`, else one statement per indented line with the `;` kept snug
/// after each statement but the last, the brackets framing their own lines. Source
/// line breaks and interior blanks never force the break — the token loops skip
/// every `NEWLINE`/`WHITESPACE`, so `(\na;\nb\n)` reflows to `(a; b)`.
///
/// A single-statement block (`(a;)`, always carrying a trailing `;` since a bare
/// `(a)` is a `PAREN_EXPR`) is left to the transparent fallback, which preserves
/// the trailing `;` (`(a;)` → `(a;)`). An interleaved comment, error recovery, or
/// any other unexpected child also falls back to the transparent lowering.
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
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => {}
                SyntaxKind::LPAREN if !saw_lparen => saw_lparen = true,
                SyntaxKind::RPAREN if !saw_rparen => saw_rparen = true,
                _ => return lower_transparent(node),
            },
        }
    }

    if !saw_lparen || !saw_rparen || statements.len() < 2 {
        return lower_transparent(node);
    }

    // Width-driven (Tenet 1): one `Ir::group`. Flat `(a; b; c)` when it fits
    // `line_width`; else one statement per indented line with the `;` separators
    // kept snug after each statement but the last (`;` can't lead a line), the
    // brackets framing their own lines. Source line breaks and interior blanks
    // never force the break — the loops above skip every `NEWLINE`/`WHITESPACE`.
    let last = statements.len() - 1;
    let mut body: Vec<Ir> = Vec::with_capacity(statements.len() * 2);
    body.push(Ir::SoftLine);
    for (i, stmt) in statements.into_iter().enumerate() {
        body.push(stmt);
        if i < last {
            body.push(Ir::text(";"));
            body.push(Ir::Line);
        }
    }
    Ir::group(Ir::concat([
        Ir::text("("),
        Ir::indent(Ir::concat(body)),
        Ir::SoftLine,
        Ir::text(")"),
    ]))
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
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => {}
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
/// is reshaped: an interleaved comment or newline, error recovery, or a missing
/// operand falls back to the verbatim transparent lowering.
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
/// interleaved comment, a comma-separated name list (`global a, b`, a bare-tuple
/// shape we don't model), or any unexpected token—falls back to the verbatim
/// transparent lowering.
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
/// (`using A: x, y`). Every comma is spaced (`, `) and the selector colon packs
/// tight-left, space-right (`A: x`); the paths themselves (`A.B`, `.A`, `Foo as
/// Bar`) are lowered transparently, so their internal tokens pass through verbatim.
///
/// The layout is width-driven: flat `using A, B, C` when it fits, else each
/// comma-group on its own line with the comma trailing and the wrapped groups
/// indented one continuation step (the first group stays on the opening line after
/// the keyword). The selector colon is **not** a break point—`using Mod: a, b, c`
/// breaks only at the commas, so `Mod: a` heads the opening line. A bare list has
/// no brackets to frame the break, so the comma serves as the breakable separator;
/// there is no broken-only trailing comma. Source line breaks carry no layout
/// information (Tenet 1): `using A,\n B` reflows to the same form as `using A, B`.
///
/// Only the clean alternating shape—item, separator, item, …—is reshaped. A
/// comment, a leading/trailing/doubled separator, or any unexpected token bails to
/// the lossless transparent lowering.
fn lower_import_stmt(node: &SyntaxNode) -> Ir {
    let mut kw: Option<SyntaxToken> = None;
    let mut rest_els: Vec<NodeOrToken<SyntaxNode, SyntaxToken>> = Vec::new();

    for el in node.children_with_tokens() {
        match &el {
            NodeOrToken::Token(tok) if tok.kind() == SyntaxKind::WHITESPACE => {}
            NodeOrToken::Token(tok) if kw.is_none() => kw = Some(tok.clone()),
            _ => rest_els.push(el),
        }
    }

    let Some(kw) = kw else {
        return lower_transparent(node);
    };

    // The opening line carries the keyword and the first comma-group (a bare path,
    // or the `Mod: name` selector head whose colon never breaks); each later
    // comma-group wraps beneath it. Commas are the only break points.
    let mut first: Vec<Ir> = vec![Ir::text(kw.text().to_string()), Ir::text(" ")];
    let mut rest: Vec<Ir> = Vec::new();
    let mut seen_comma = false;
    let mut expect_item = true;

    for el in &rest_els {
        match el {
            NodeOrToken::Node(child) if expect_item => {
                let ir = lower_node(child);
                if seen_comma {
                    rest.push(ir);
                } else {
                    first.push(ir);
                }
                expect_item = false;
            }
            NodeOrToken::Token(tok) if !expect_item && tok.kind() == SyntaxKind::COMMA => {
                // The comma trails its group; the next group wraps at the break.
                rest.push(Ir::text(","));
                rest.push(Ir::Line);
                seen_comma = true;
                expect_item = true;
            }
            NodeOrToken::Token(tok) if !expect_item && tok.kind() == SyntaxKind::COLON => {
                // The selector colon stays on the opening line with its module.
                if seen_comma {
                    rest.push(Ir::text(": "));
                } else {
                    first.push(Ir::text(": "));
                }
                expect_item = true;
            }
            // Source line breaks carry no layout information under Tenet 1.
            NodeOrToken::Token(tok)
                if matches!(tok.kind(), SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE) => {}
            _ => return lower_transparent(node),
        }
    }

    // A dangling `expect_item` means a leading/trailing/doubled separator or an
    // empty list—none is the clean shape this rule models.
    if expect_item {
        return lower_transparent(node);
    }

    if rest.is_empty() {
        return Ir::concat(first);
    }

    // One width-driven group with its own continuation indent.
    Ir::group(Ir::concat([
        Ir::concat(first),
        Ir::indent(Ir::concat(rest)),
    ]))
}

/// Lay out an `export`/`public` statement: the keyword, one space, then the
/// comma-separated name list (`export a,b` → `export a, b`, `public foo,bar` →
/// `public foo, bar`). Every comma is spaced (tight-left, space-right); the names
/// themselves are left alone.
///
/// Unlike the `using`/`import` list, an exported name is **not** always a single
/// node: it may be an identifier, an operator (`export +, -`), a macro
/// (`export @m`), or a `var"…"` form (several adjacent tokens). So the rule
/// tracks comma boundaries rather than a strict node/separator alternation: the
/// first token of each name gets a leading space, and any further tokens of the
/// *same* name are glued verbatim (no incidental whitespace exists between them).
///
/// The layout is width-driven, matching the `using`/`import` list: flat
/// `export a, b, c` when it fits, else each name on its own line with the comma
/// trailing and the wrapped names indented one continuation step (the first name
/// stays on the opening line after the keyword). Source line breaks carry no
/// layout information (Tenet 1). Bails to the lossless transparent lowering on a
/// comment or a leading/trailing/doubled comma.
fn lower_export_stmt(node: &SyntaxNode) -> Ir {
    let mut kw: Option<SyntaxToken> = None;
    let mut rest_els: Vec<NodeOrToken<SyntaxNode, SyntaxToken>> = Vec::new();

    for el in node.children_with_tokens() {
        match &el {
            NodeOrToken::Token(tok)
                if matches!(tok.kind(), SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE) => {}
            NodeOrToken::Token(tok)
                if matches!(tok.kind(), SyntaxKind::COMMENT | SyntaxKind::BLOCK_COMMENT) =>
            {
                return lower_transparent(node);
            }
            NodeOrToken::Token(tok) if kw.is_none() => kw = Some(tok.clone()),
            _ => rest_els.push(el),
        }
    }

    let Some(kw) = kw else {
        return lower_transparent(node);
    };

    // The opening line carries the keyword and the first name; each later name
    // wraps beneath it. Commas are the only break points. `expect_item` is true
    // after the keyword and after each comma: the next token opens a new name and
    // takes a leading space (flat) or the continuation indent (broken). While
    // false, we are inside a name—a comma closes it, any other token is glued on.
    let mut first: Vec<Ir> = vec![Ir::text(kw.text().to_string())];
    let mut rest: Vec<Ir> = Vec::new();
    let mut seen_comma = false;
    let mut expect_item = true;

    for el in &rest_els {
        match el {
            NodeOrToken::Token(tok) if !expect_item && tok.kind() == SyntaxKind::COMMA => {
                // The comma trails its name; the next name wraps at the break.
                rest.push(Ir::text(","));
                rest.push(Ir::Line);
                seen_comma = true;
                expect_item = true;
            }
            // A comma where an item is expected is a leading/doubled comma.
            NodeOrToken::Token(tok) if tok.kind() == SyntaxKind::COMMA => {
                return lower_transparent(node);
            }
            _ => {
                let piece = match el {
                    NodeOrToken::Node(child) => lower_node(child),
                    NodeOrToken::Token(tok) => Ir::text(tok.text().to_string()),
                };
                let bucket = if seen_comma { &mut rest } else { &mut first };
                if expect_item {
                    // Opening a new name: a leading space only heads the first
                    // group; every later group gets its space from the `Ir::Line`.
                    if !seen_comma {
                        bucket.push(Ir::text(" "));
                    }
                    expect_item = false;
                }
                bucket.push(piece);
            }
        }
    }

    // A dangling `expect_item` means a trailing comma or an empty list—neither is
    // a clean name list.
    if expect_item {
        return lower_transparent(node);
    }

    if rest.is_empty() {
        return Ir::concat(first);
    }

    // One width-driven group with its own continuation indent.
    Ir::group(Ir::concat([
        Ir::concat(first),
        Ir::indent(Ir::concat(rest)),
    ]))
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

/// Lay out a string or command literal (`"…"`, `"""…"""`, `` `…` ``). The literal
/// content and delimiters are **verbatim** — a string's characters are the user's,
/// never reflowed. Each `$(…)` interpolation, however, is embedded Julia code whose
/// surrounding whitespace and newlines do not affect the string's value (a single
/// expression; `$(\n x\n)` ≡ `$(x)`), so its expression is normalized like any other
/// and **forced flat** ([`render_flat`]): `$( y + z )` → `$(y + z)`, and a source
/// break inside the parens collapses. This is Tenet 1 for the interpolated code
/// while leaving the string content untouched, and it removes the pre-rule bug where
/// an overflowing string let [`lower_paren`] break *inside* the literal.
///
/// A bare `$name` interpolation has no parens to normalize and passes through. Any
/// interpolation that cannot be flattened (a comment or block forcing a hard break)
/// bails the whole literal to its verbatim source text — always lossless and
/// idempotent.
fn lower_string_literal(node: &SyntaxNode) -> Ir {
    string_literal_body(node).unwrap_or_else(|| Ir::text(node.text().to_string()))
}

/// Build the concatenated body of a string/command literal: every token verbatim,
/// every `INTERPOLATION` child normalized and forced flat. `None` if any
/// interpolation cannot be laid out flat (the caller emits the literal verbatim).
fn string_literal_body(node: &SyntaxNode) -> Option<Ir> {
    let mut parts = Vec::new();
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(tok) => parts.push(Ir::text(tok.text().to_string())),
            NodeOrToken::Node(child) => {
                if child.kind() != SyntaxKind::INTERPOLATION {
                    return None;
                }
                // The interpolation lowers normally (`$` verbatim, the `$(…)` paren
                // through `lower_paren`), then renders strictly flat so it can never
                // break inside the literal.
                parts.push(Ir::text(render_flat(&lower_node(&child))?));
            }
        }
    }
    Some(Ir::concat(parts))
}

/// Render an [`Ir`] in strictly flat layout to a string, or `None` if it carries a
/// forced break that cannot be flattened (a [`HardLine`](Ir::HardLine)/
/// [`BlankLine`](Ir::BlankLine), or an embedded newline in transparent text).
/// Every [`Group`](Ir::Group) is taken flat regardless of width; a [`Line`](Ir::Line)
/// is a space, a [`SoftLine`](Ir::SoftLine) is empty, an [`IfBreak`](Ir::IfBreak) is
/// its flat string.
fn render_flat(ir: &Ir) -> Option<String> {
    let mut out = String::new();
    render_flat_into(ir, &mut out).then_some(out)
}

fn render_flat_into(ir: &Ir, out: &mut String) -> bool {
    match ir {
        Ir::Text(s) => {
            if s.contains('\n') {
                return false;
            }
            out.push_str(s);
            true
        }
        Ir::Concat(items) => items.iter().all(|item| render_flat_into(item, out)),
        Ir::Indent(inner) | Ir::Group(inner) => render_flat_into(inner, out),
        Ir::Line => {
            out.push(' ');
            true
        }
        Ir::SoftLine => true,
        Ir::HardLine | Ir::BlankLine => false,
        Ir::IfBreak(_, flat) => {
            out.push_str(flat);
            true
        }
        Ir::HugGroup {
            prefix,
            body,
            close,
            ..
        } => {
            render_flat_into(prefix, out)
                && render_flat_into(body, out)
                && render_flat_into(close, out)
        }
    }
}

/// Zero-pad a hexadecimal integer literal to a fixed type width, or return
/// `None` to leave it verbatim. The literal (`0x` prefix included) is padded to
/// the next of the canonical spans `0x` + 2/4/8/16/32 hex chars (the widths of
/// `UInt8`/`UInt16`/`UInt32`/`UInt64`/`UInt128`), by
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

/// Normalize a decimal float literal to the canonical form, or return `None` to
/// leave it verbatim. The canonical form is
/// `[sign] <int>.<frac> [e|f [sign] <exp>]` where:
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
/// Underscored and hex (`0x…p…`) floats are left verbatim, as is any token that
/// doesn't parse cleanly into the shape above.
fn normalize_float(text: &str) -> Option<String> {
    // Underscored and hex floats are out of scope.
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
/// gap is breakable.
///
/// **Width-driven (Tenet 1)**, identical to [`lower_binary`]'s non-assignment
/// path: one `Ir::group` with its own continuation indent — flat (`a < b <= c`)
/// when it fits `line_width`, else operator-trailing (each operator stays on the
/// line it ends) with the wrapped operands indented one step. Source line breaks
/// carry no layout information and are ignored like whitespace.
///
/// As with [`lower_binary`], only the clean alternating shape is reshaped: an
/// interleaved comment, error recovery, or a degenerate operand count falls back
/// to the verbatim transparent lowering.
fn lower_comparison(node: &SyntaxNode) -> Ir {
    // Children in source order, with incidental whitespace dropped: operands
    // become lowered `Ir`, operator tokens become their text. The result must
    // alternate operand, operator, operand, … starting and ending on an operand.
    let mut first: Option<Ir> = None;
    let mut rest: Vec<Ir> = Vec::new();
    // Separator to emit before the upcoming operand: the breakable `Ir::Line`, set
    // when a comparison operator is consumed.
    let mut next_sep: Option<Ir> = None;
    let mut expect_operand = true;
    let mut operand_count = 0usize;
    let mut op_count = 0usize;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) => {
                if !expect_operand {
                    return lower_transparent(node);
                }
                let ir = lower_node(&child);
                if operand_count == 0 {
                    first = Some(ir);
                } else {
                    if let Some(sep) = next_sep.take() {
                        rest.push(sep);
                    }
                    rest.push(ir);
                }
                operand_count += 1;
                expect_operand = false;
            }
            NodeOrToken::Token(tok) => match tok.kind() {
                // Source line breaks carry no layout information under Tenet 1.
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => {}
                SyntaxKind::COMMENT | SyntaxKind::BLOCK_COMMENT => {
                    return lower_transparent(node);
                }
                _ => {
                    if expect_operand {
                        return lower_transparent(node);
                    }
                    // A comparison operator sits trailing on the line it ends; the
                    // following operand wraps at the breakable gap.
                    rest.push(Ir::text(format!(" {}", tok.text())));
                    next_sep = Some(Ir::Line);
                    op_count += 1;
                    expect_operand = true;
                }
            },
        }
    }

    // A well-formed chain ends on an operand, has at least two of them, and one
    // fewer operator than operands.
    let Some(first) = first else {
        return lower_transparent(node);
    };
    if expect_operand || operand_count < 2 || op_count + 1 != operand_count {
        return lower_transparent(node);
    }

    // One width-driven group with its own continuation indent: flat when it fits,
    // else operator-trailing with the wrapped operands indented one step.
    Ir::group(Ir::concat([first, Ir::indent(Ir::concat(rest))]))
}

/// Lay out a ternary conditional (`a ? b : c`) with a single space on each side of
/// both the `?` and the `:`. The node alternates operand/`?`/operand/`:`/operand;
/// a nested ternary (`a ? b : c ? d : e`, right-associative) is the final operand
/// and is lowered recursively, so it keeps normalizing.
///
/// Width-driven (Tenet 1), following the same Air-style model as [`lower_binary`]:
/// one `Ir::group` per ternary node, each with its own continuation indent. When
/// the conditional fits `line_width` it stays flat (`a ? b : c`); otherwise it
/// breaks operator-trailing (Julia forbids `?`/`:` from *leading* a line, so a
/// break only ever lands in the gap after an operator) with the two branch operands
/// wrapped one indent step in. Each breakable gap is an [`Ir::Line`] (a space when
/// flat, a newline when broken). Because every ternary owns its `Ir::indent`, a
/// nested `?:`-chain that is itself forced to break nests one level deeper on top of
/// its parent's indent; a nested chain that still fits at its column stays flat.
/// Source line breaks carry no layout information and are ignored like whitespace.
///
/// Only the clean alternating shape with `?`/`:` operators is reshaped: an
/// interleaved comment, error recovery, or an unexpected token or operand count
/// falls back to the verbatim transparent lowering.
fn lower_ternary(node: &SyntaxNode) -> Ir {
    let mut first: Option<Ir> = None;
    let mut rest: Vec<Ir> = Vec::new();
    // Separator to emit before the upcoming operand: the breakable `Ir::Line`, set
    // when an operator (`?`/`:`) is consumed.
    let mut next_sep: Option<Ir> = None;
    let mut expect_operand = true;
    let mut operand_count = 0usize;
    let mut op_count = 0usize;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) => {
                if !expect_operand {
                    return lower_transparent(node);
                }
                let ir = lower_node(&child);
                if operand_count == 0 {
                    first = Some(ir);
                } else {
                    if let Some(sep) = next_sep.take() {
                        rest.push(sep);
                    }
                    rest.push(ir);
                }
                operand_count += 1;
                expect_operand = false;
            }
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => {}
                SyntaxKind::QUESTION | SyntaxKind::COLON if !expect_operand => {
                    // `?`/`:` sit trailing on the line they end.
                    rest.push(Ir::text(format!(" {}", tok.text())));
                    next_sep = Some(Ir::Line);
                    op_count += 1;
                    expect_operand = true;
                }
                _ => return lower_transparent(node),
            },
        }
    }

    // A well-formed ternary ends on an operand and has three of them separated by
    // exactly two operators (`?` and `:`).
    let Some(first) = first else {
        return lower_transparent(node);
    };
    if expect_operand || operand_count != 3 || op_count != 2 {
        return lower_transparent(node);
    }

    Ir::group(Ir::concat([first, Ir::indent(Ir::concat(rest))]))
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
/// parameter list (`Vector{Int}`, `Dict{A, B}`) — as a **width-driven group**:
/// the [`printer`](crate::formatter::printer) collapses it to one line when it
/// fits `line_width` and explodes it (one item per line, indented, close bracket
/// flush) when it does not. Per Tenet 1, the layout depends **only** on width:
/// the source's own line breaks and any source trailing comma are ignored, so
/// `f(1,\n2)` and `f(1, 2)` both format to `f(1, 2)`.
///
/// Flat punctuation is normalized — no padding inside the brackets, no space
/// before a comma, one space after it. A trailing comma is added **only in the
/// broken layout** (via [`Ir::IfBreak`]); the flat form never carries one
/// (`g(a,)` → `g(a)`).
///
/// Items are `ARG`/`KEYWORD_ARG` nodes separated by commas. Two cases keep their
/// existing handling rather than the width-driven group:
///
/// - **Comments.** A list carrying an own-line or trailing comment cannot
///   collapse, so it routes to the comment-aware [`lower_multiline_bracket`].
///   (Comments are a separate, still-source-mirroring construct.)
/// - **A `;`-separated `PARAMETERS` tail** (`f(a; b = 1)`): folded into the same
///   width-driven group as the positional args. Flat when it fits
///   (`f(a, b; c = 1)`); when broken, one arg per line with the `;` snug after the
///   last positional (`b;`), each keyword on its own line, and a trailing comma
///   after the last keyword. A keyword-only call keeps the `;` on the open line
///   (`f(;`). An unmodeled params shape (comment, doubled comma, unexpected child)
///   still falls back to the flat form.
///
/// Any other unmodeled shape — a doubled/leading comma, two items with no comma
/// between them, an unexpected child or token — falls back to the verbatim
/// transparent lowering.
fn lower_arg_list(node: &SyntaxNode) -> Ir {
    // A comment can't be reflowed away; keep the comment-aware multiline path.
    if bracket_has_comment(node) {
        return lower_multiline_bracket(node);
    }

    let Some(ArgListParts {
        open,
        close,
        mut items,
        params: params_node,
        last_huggable,
    }) = collect_arg_list(node)
    else {
        return lower_transparent(node);
    };

    // A `;` keyword tail: fold it into one width-driven group with the positional
    // args. Flat `(a, b; c = 1)`; when broken, the `;` snugs after the last
    // positional (`b;`) and each keyword drops to its own line, or — with no
    // positional args — the `;` rides the open bracket (`f(;`).
    if let Some(pnode) = params_node {
        if let Some((pitems, last_param_huggable)) = collect_param_items(&pnode) {
            // Trailing-parameter hug (mirrors the positional hug below): the
            // last keyword's bracket value hugs this bracket — everything
            // before it (the positionals, the `; `, the earlier keywords, the
            // `name = `) is the flat first-line prefix, and the width-driven
            // group below is the explode fallback.
            let hug = last_param_huggable.then(|| {
                let mut prefix = params_hug_prefix(&open, &items, &pitems);
                let mut body = pitems.last().expect("hug requires a last item").clone();
                // A trailing pair value hugs through its `=>`: the keyword's
                // `name = lhs => ` joins the flat prefix and the value alone
                // is the body (see [`pair_hug_grouped_parts`]).
                if let Some((extra, value_body)) = last_list_item(&pnode)
                    .as_ref()
                    .and_then(pair_hug_grouped_parts)
                {
                    prefix.push(extra);
                    body = value_body;
                }
                (Ir::concat(prefix), body)
            });
            let close_text = Ir::text(close.clone());

            let grouped = Ir::group(arg_list_params_body(&open, items, pitems, &close));
            if let Some((prefix, body)) = hug {
                return Ir::hug_group(prefix, body, close_text, grouped);
            }
            return grouped;
        }

        // An unmodeled params shape (comment, doubled comma, …): keep the flat
        // form, lowering the tail through the transparent-safe `lower_parameters`.
        let mut parts: Vec<Ir> = vec![Ir::text(open)];
        for (i, item) in items.into_iter().enumerate() {
            if i > 0 {
                parts.push(Ir::text(", "));
            }
            parts.push(item);
        }
        parts.push(lower_node(&pnode));
        parts.push(Ir::text(close));
        return Ir::concat(parts);
    }

    // An empty list never breaks.
    if items.is_empty() {
        return Ir::concat([Ir::text(open), Ir::text(close)]);
    }

    // Trailing-argument hug: when the last positional argument is itself a
    // bracket-delimited construct (`f(g(…))`, `f([…])`, `map(f, […])`), it hugs
    // this bracket instead of exploding onto its own indented line. The leading
    // arguments render flat in the prefix; the hugged argument carries its own
    // width-driven group, so it stays flat when it fits and otherwise breaks in
    // place — its opening bracket riding this line, its closing bracket stacking
    // with ours. When even the hug layout's first line (the prefix plus the
    // hugged construct's opening bracket) overflows `line_width`, the printer
    // falls back to the standard explode group, one item per line.
    if last_huggable {
        let explode = arg_list_explode_group(&open, &items, &close);
        let mut body = items.pop().expect("hug requires a last item");
        let mut prefix: Vec<Ir> = vec![Ir::text(open)];
        for item in items {
            prefix.push(item);
            prefix.push(Ir::text(", "));
        }
        // A trailing pair hugs through its `=>`: the `lhs => ` joins the flat
        // prefix and the value alone is the body (see [`pair_hug_grouped_parts`]).
        if let Some((extra, value_body)) = last_list_item(node)
            .as_ref()
            .and_then(pair_hug_grouped_parts)
        {
            prefix.push(extra);
            body = value_body;
        }
        return Ir::hug_group(Ir::concat(prefix), body, Ir::text(close), explode);
    }

    arg_list_explode_group(&open, &items, &close)
}

/// The parsed pieces of a clean argument list: the bracket tokens, the lowered
/// comma-separated items, the `; …` keyword tail node (if any), and whether the
/// last positional item can hug the closing bracket (see [`item_is_huggable`]).
struct ArgListParts {
    open: String,
    close: String,
    items: Vec<Ir>,
    params: Option<SyntaxNode>,
    last_huggable: bool,
}

/// Walk an `ARG_LIST`'s children into [`ArgListParts`]. `None` on any unmodeled
/// shape — a leading/doubled comma, two items with no comma between, an item
/// after the `;` tail, a missing bracket, or an unexpected child or token — the
/// caller falls back to the verbatim transparent lowering. Source newlines carry
/// no layout information under Tenet 1 and are skipped like spaces.
fn collect_arg_list(node: &SyntaxNode) -> Option<ArgListParts> {
    let mut open: Option<String> = None;
    let mut close: Option<String> = None;
    let mut items: Vec<Ir> = Vec::new();
    let mut params: Option<SyntaxNode> = None;
    let mut pending_comma = false;
    let mut last_huggable = false;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::LPAREN | SyntaxKind::LBRACKET | SyntaxKind::LBRACE => {
                    open = Some(tok.text().to_string())
                }
                SyntaxKind::RPAREN | SyntaxKind::RBRACKET | SyntaxKind::RBRACE => {
                    close = Some(tok.text().to_string())
                }
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => {}
                SyntaxKind::COMMA => {
                    // A comma before any item (leading) or right after another
                    // (doubled) is not a clean list.
                    if pending_comma || items.is_empty() {
                        return None;
                    }
                    pending_comma = true;
                }
                _ => return None,
            },
            NodeOrToken::Node(child) => match child.kind() {
                SyntaxKind::ARG | SyntaxKind::KEYWORD_ARG => {
                    // An item after the `;` tail, or a second item with no comma
                    // between, is unmodeled.
                    if params.is_some() || (!items.is_empty() && !pending_comma) {
                        return None;
                    }
                    last_huggable = item_is_huggable(&child);
                    items.push(lower_node(&child));
                    pending_comma = false;
                }
                // `;`-separated parameters attach directly (the `;` is the
                // separator), so they must not follow a comma.
                SyntaxKind::PARAMETERS => {
                    if pending_comma || params.is_some() {
                        return None;
                    }
                    params = Some(child);
                }
                _ => return None,
            },
        }
    }

    Some(ArgListParts {
        open: open?,
        close: close?,
        items,
        params,
        last_huggable,
    })
}

/// The standard width-driven arg-list group: flat `(a, b, c)`, or one item per
/// indented line with a broken-only trailing comma when it doesn't fit.
fn arg_list_explode_group(open: &str, items: &[Ir], close: &str) -> Ir {
    Ir::group(arg_list_explode_body(open, items, close))
}

/// The ungrouped body behind [`arg_list_explode_group`] — also folded directly
/// into [`lower_index`]'s shared outer group (via [`call_reflow_body`]), where
/// the enclosing group must own the arg list's break opportunities.
fn arg_list_explode_body(open: &str, items: &[Ir], close: &str) -> Ir {
    bracket_explode_body(open, items, close, Ir::if_break(",", ""))
}

/// The ungrouped width-driven body of an arg list with a `;` keyword tail: flat
/// `(a, b; kw = 1)`, or — when the owning group breaks — one item per indented
/// line with the `;` snug after the last positional (`b;`), each keyword on its
/// own line, and a broken-only trailing comma; a keyword-only list keeps the `;`
/// on the open bracket (`f(;`). Grouped by [`lower_arg_list`], or folded raw
/// into [`lower_index`]'s shared outer group (via [`call_reflow_body`]).
fn arg_list_params_body(open: &str, items: Vec<Ir>, pitems: Vec<Ir>, close: &str) -> Ir {
    let mut group_parts: Vec<Ir> = vec![Ir::text(open)];
    let mut inner: Vec<Ir> = Vec::new();
    if items.is_empty() {
        // Keyword-only: `;` rides the open bracket, outside the indent.
        group_parts.push(Ir::text(";"));
    } else {
        inner.push(Ir::SoftLine);
        for (i, item) in items.into_iter().enumerate() {
            if i > 0 {
                inner.push(Ir::text(","));
                inner.push(Ir::Line);
            }
            inner.push(item);
        }
        // `;` snugs to the last positional (no comma, no break before it).
        inner.push(Ir::text(";"));
    }
    for (j, p) in pitems.into_iter().enumerate() {
        if j > 0 {
            inner.push(Ir::text(","));
        }
        // A break/space before every keyword, including the first (which
        // sits after the `;`).
        inner.push(Ir::Line);
        inner.push(p);
    }
    inner.push(Ir::if_break(",", ""));
    group_parts.push(Ir::indent(Ir::concat(inner)));
    group_parts.push(Ir::SoftLine);
    group_parts.push(Ir::text(close));
    Ir::concat(group_parts)
}

/// The flat first-line prefix of a keyword-tail hug: the open bracket, the
/// positional args, the `;` (snug to the last positional, or riding the open
/// bracket when there is none), and every keyword but the last — the hugged
/// one — each followed by `, `.
fn params_hug_prefix(open: &str, items: &[Ir], pitems: &[Ir]) -> Vec<Ir> {
    let mut prefix: Vec<Ir> = vec![Ir::text(open.to_string())];
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            prefix.push(Ir::text(", "));
        }
        prefix.push(item.clone());
    }
    // The `;` snugs to the last positional (or rides the open bracket), one
    // space before the first keyword.
    prefix.push(Ir::text("; "));
    for p in &pitems[..pitems.len() - 1] {
        prefix.push(p.clone());
        prefix.push(Ir::text(", "));
    }
    prefix
}

/// The ungrouped width-driven body of a collection literal — the arg list's
/// explode body, except that the one-tuple's semantic comma is emitted in both
/// layout modes instead of only when broken.
fn collection_explode_body(open: &str, items: &[Ir], close: &str, singleton_comma: bool) -> Ir {
    let trailing = if singleton_comma {
        Ir::text(",")
    } else {
        Ir::if_break(",", "")
    };
    bracket_explode_body(open, items, close, trailing)
}

/// The shared bracketed-list body: flat `(a, b, c)` when it fits, else one item
/// per indented line, with `trailing` after the last item and the close bracket
/// flush on its own line.
fn bracket_explode_body(open: &str, items: &[Ir], close: &str, trailing: Ir) -> Ir {
    let mut inner: Vec<Ir> = vec![Ir::SoftLine];
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            inner.push(Ir::text(","));
            inner.push(Ir::Line);
        }
        inner.push(item.clone());
    }
    inner.push(trailing);

    Ir::concat([
        Ir::text(open),
        Ir::indent(Ir::concat(inner)),
        Ir::SoftLine,
        Ir::text(close),
    ])
}

/// Whether an arg-list or collection item ends in a bracket-delimited construct
/// that can hug an enclosing bracket. When such an item is the *last* one of its
/// list, it hugs: the construct's opening bracket rides the enclosing bracket's
/// line and the closers stack, rather than the item exploding onto its own
/// indented line. A positional `ARG` hugs when its sole child is a huggable
/// construct; a `KEYWORD_ARG` hugs when it has the clean `name = <value>` shape
/// (the one [`lower_keyword_arg`] reflows) and the value is one — the name and
/// `=` join the flat prefix (`f(x, kw = [`). A bare token, a splat, a
/// unary/binary expression, or anything else keeps the normal layout. The pair
/// operators are hug-transparent (see [`pair_hug_chain`]): a value of the form
/// `lhs => <huggable construct>` — or the longer chain `a => b => <construct>` —
/// hugs too, the whole `a => b => ` joining the flat prefix like a keyword's
/// `name = `.
fn item_is_huggable(item: &SyntaxNode) -> bool {
    match item.kind() {
        SyntaxKind::ARG => {
            // Exactly one child node (a clean sole wrapper), and it is a
            // huggable value.
            let mut children = item.children();
            let (Some(child), None) = (children.next(), children.next()) else {
                return false;
            };
            value_is_huggable(&child)
        }
        SyntaxKind::KEYWORD_ARG => {
            // Exactly `name = value` — two child nodes with nothing but the `=`
            // (and spaces) between them; a comment or newline would make
            // `lower_keyword_arg` bail to a transparent body the hug layout
            // cannot own.
            let mut nodes = 0usize;
            let mut value: Option<SyntaxNode> = None;
            let mut seen_eq = false;
            for el in item.children_with_tokens() {
                match el {
                    NodeOrToken::Node(child) => {
                        nodes += 1;
                        value = Some(child);
                    }
                    NodeOrToken::Token(tok) => match tok.kind() {
                        SyntaxKind::WHITESPACE => {}
                        SyntaxKind::EQ if !seen_eq => seen_eq = true,
                        _ => return false,
                    },
                }
            }
            nodes == 2 && seen_eq && value.as_ref().is_some_and(value_is_huggable)
        }
        _ => false,
    }
}

/// Whether an item's value can hug: a bracket-delimited construct itself, or a
/// clean pair chain wrapping one (see [`pair_hug_chain`]).
fn value_is_huggable(value: &SyntaxNode) -> bool {
    huggable_kind(value.kind()) || pair_hug_chain(value).is_some()
}

/// Parse a clean two-operand `=>`/`.=>` pair into its left operand, operator
/// text, and right operand. `None` for a non-`BINARY_EXPR`, a non-pair operator
/// (`-->`, `<-->`, … share the parser tier but not the pair idiom), a comment,
/// or any shape but the clean two-operand form; interior newlines are
/// layout-free, as in [`lower_binary`].
fn pair_operands(node: &SyntaxNode) -> Option<(SyntaxNode, String, SyntaxNode)> {
    if node.kind() != SyntaxKind::BINARY_EXPR {
        return None;
    }
    let mut lhs: Option<SyntaxNode> = None;
    let mut op: Option<String> = None;
    let mut rhs: Option<SyntaxNode> = None;
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) => match (&lhs, &op, &rhs) {
                (None, None, None) => lhs = Some(child),
                (Some(_), Some(_), None) => rhs = Some(child),
                _ => return None,
            },
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => {}
                SyntaxKind::FAT_ARROW | SyntaxKind::DOT_FAT_ARROW
                    if lhs.is_some() && op.is_none() =>
                {
                    op = Some(tok.text().to_string());
                }
                _ => return None,
            },
        }
    }
    Some((lhs?, op?, rhs?))
}

/// Peel a right-nested chain of clean huggable pairs — `a => <construct>` or
/// the longer `a => b => <construct>` — into the accumulated flat prefix
/// (`a => `, `a => b => `, …) and the innermost huggable construct. The pair
/// spellings are hug-transparent: a trailing pair (chain) item hugs through
/// them, the whole `a => b => ` joining the flat hug prefix exactly as a
/// keyword's `name = ` does. `None` unless every link is a clean two-operand
/// `=>`/`.=>` pair (see [`pair_operands`]) and the innermost value is a huggable
/// bracket construct (see [`huggable_kind`]).
fn pair_hug_chain(node: &SyntaxNode) -> Option<(Ir, SyntaxNode)> {
    let (lhs, op, rhs) = pair_operands(node)?;
    let head = Ir::concat([lower_node(&lhs), Ir::text(format!(" {op} "))]);
    if huggable_kind(rhs.kind()) {
        return Some((head, rhs));
    }
    let (tail, construct) = pair_hug_chain(&rhs)?;
    Some((Ir::concat([head, tail]), construct))
}

/// The bracket-delimited constructs that can hug an enclosing bracket: each owns
/// a trailing breakable bracket group — a nested call or index (`g(…)`, `a[…]`),
/// a curly application (`A{T}`), a bracketed collection (`[…]`, `(…,)`, `{…}`),
/// a comprehension/generator, or a matrix.
fn huggable_kind(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::CALL_EXPR
            | SyntaxKind::INDEX_EXPR
            | SyntaxKind::CURLY_EXPR
            | SyntaxKind::VECT_EXPR
            | SyntaxKind::TUPLE_EXPR
            | SyntaxKind::BRACES
            | SyntaxKind::COMPREHENSION
            | SyntaxKind::GENERATOR
            | SyntaxKind::BRACES_COMPREHENSION
            | SyntaxKind::MATRIX_EXPR
    )
}

/// Whether a bracketed node carries a comment among its **direct** children (an
/// own-line or trailing comment between items, or on the brackets). Such a list
/// can't collapse to one line, so it routes to the comment-aware multiline path.
/// A comment nested *inside* an item is that item's own concern and doesn't count.
fn bracket_has_comment(node: &SyntaxNode) -> bool {
    node.children_with_tokens()
        .any(|el| matches!(el.kind(), SyntaxKind::COMMENT | SyntaxKind::BLOCK_COMMENT))
}

/// Lay out a bracketed collection literal — a tuple `(a, b)`, a vector `[1, 2]`,
/// or a brace set `{a, b}` — with normalized punctuation: no padding inside the
/// brackets, no space before a comma, one space after it.
///
/// Width-driven (Tenet 1): a clean, comment-free list builds one `Ir::group` —
/// flat `[a, b]` when it fits `line_width`, else one element per indented line
/// with a broken-only trailing comma. Source line breaks and any source trailing
/// comma are ignored (`[a, b,]` and `[a,\n b]` both → `[a, b]`).
///
/// The single-element tuple is the exception: its comma is semantic (it
/// distinguishes the one-tuple `(a,)` from a parenthesized expression), so it is
/// emitted in **both** modes. Items are `ARG`/`KEYWORD_ARG` nodes separated by
/// commas; anything richer — a `;`-separated matrix row (`PARAMETERS`), an
/// interleaved comment (routed to the comment-aware multiline path), a
/// doubled/orphaned comma, or an unexpected child — falls back to the verbatim
/// transparent lowering. Space-separated matrices are a distinct `MATRIX_EXPR`
/// node and never reach here.
fn lower_collection(node: &SyntaxNode) -> Ir {
    // A comment can't be reflowed away; keep the comment-aware multiline path.
    if bracket_has_comment(node) {
        return lower_multiline_bracket(node);
    }
    let Some(parts) = collect_collection_items(node) else {
        return lower_transparent(node);
    };

    // Trailing-element hug (mirrors the arg-list hug): when the last element is
    // itself a bracket-delimited construct (`[a, b, f(…)]`, `(a, v = […])`), it
    // hugs this bracket instead of exploding onto its own indented line; the
    // standard explode group is the fallback when even the hug first line
    // overflows. The one-tuple's semantic comma joins the stacked closers
    // (`),)`).
    if parts.last_huggable {
        let singleton_comma = collection_singleton_comma(node, &parts.items);
        let CollectionParts {
            open,
            close,
            mut items,
            ..
        } = parts;
        let close_text = if singleton_comma {
            format!(",{close}")
        } else {
            close.clone()
        };
        let explode = Ir::group(collection_explode_body(
            &open,
            &items,
            &close,
            singleton_comma,
        ));
        let mut body = items.pop().expect("hug requires a last item");
        let mut prefix: Vec<Ir> = vec![Ir::text(open)];
        for item in items {
            prefix.push(item);
            prefix.push(Ir::text(", "));
        }
        // A trailing pair element hugs through its `=>` (see
        // [`pair_hug_grouped_parts`]).
        if let Some((extra, value_body)) = last_list_item(node)
            .as_ref()
            .and_then(pair_hug_grouped_parts)
        {
            prefix.push(extra);
            body = value_body;
        }
        return Ir::hug_group(Ir::concat(prefix), body, Ir::text(close_text), explode);
    }

    Ir::group(collection_body(node, parts))
}

/// Build the ungrouped body of a clean collection literal for [`lower_index`],
/// which folds it into a shared outer group so the subject's break opportunities
/// and the index tail are measured together. A huggable last element becomes an
/// ungrouped [`Ir::HugGroup`] (see [`reflow_hug`]) so the owning group decides
/// flat-vs-yield while the hug keeps its own hug-vs-explode tiering. `None` on
/// any shape the reflow does not fully model — the caller falls back to
/// transparent.
fn collection_reflow_body(node: &SyntaxNode) -> Option<Ir> {
    let parts = collect_collection_items(node)?;
    if parts.last_huggable {
        let singleton_comma = collection_singleton_comma(node, &parts.items);
        let close_text = if singleton_comma {
            format!(",{}", parts.close)
        } else {
            parts.close.clone()
        };
        let explode =
            collection_explode_body(&parts.open, &parts.items, &parts.close, singleton_comma);
        let mut prefix: Vec<Ir> = vec![Ir::text(parts.open.clone())];
        for item in &parts.items[..parts.items.len() - 1] {
            prefix.push(item.clone());
            prefix.push(Ir::text(", "));
        }
        return reflow_hug(prefix, &last_list_item(node)?, close_text, explode);
    }
    Some(collection_body(node, parts))
}

/// The last `ARG`/`KEYWORD_ARG` item of a bracketed list node — the one a
/// `last_huggable` flag refers to.
fn last_list_item(node: &SyntaxNode) -> Option<SyntaxNode> {
    node.children()
        .filter(|c| matches!(c.kind(), SyntaxKind::ARG | SyntaxKind::KEYWORD_ARG))
        .last()
}

/// Assemble an **ungrouped** trailing-item hug for a reflow body: the flat
/// `prefix` (plus the hugged keyword's `name = ` when the item is a
/// `KEYWORD_ARG`), the hugged construct's own ungrouped reflow body, the close
/// bracket, and the ungrouped full-explode fallback. Every break lives at the
/// owning group's level: when that group is flat the hug renders flat; when it
/// breaks, the hugged body breaks in place (the subject yields) — or, when even
/// the hug first line overflows, the printer's [`hug_fits`] check falls back to
/// the full explode. `None` when the hugged construct has no reflow body (see
/// [`construct_reflow_body`]).
fn reflow_hug(mut prefix: Vec<Ir>, last: &SyntaxNode, close: String, explode: Ir) -> Option<Ir> {
    let (extra_prefix, body) = item_hug_parts(last)?;
    if let Some(extra) = extra_prefix {
        prefix.push(extra);
    }
    Some(Ir::hug_group(
        Ir::concat(prefix),
        body,
        Ir::text(close),
        explode,
    ))
}

/// Split a huggable list item into the hug's prefix addition and its ungrouped
/// body: a positional `ARG` contributes its value's prefix (nothing, or a
/// pair's `lhs => `) and reflow body; a `KEYWORD_ARG` prepends `name = `.
/// `None` when the wrapped construct has no reflow body — the caller bails to
/// transparent, exactly as the pre-hug bails did.
fn item_hug_parts(item: &SyntaxNode) -> Option<(Option<Ir>, Ir)> {
    match item.kind() {
        SyntaxKind::ARG => {
            let mut children = item.children();
            let (Some(child), None) = (children.next(), children.next()) else {
                return None;
            };
            hug_value_parts(&child)
        }
        SyntaxKind::KEYWORD_ARG => {
            // `item_is_huggable` vetted the clean `name = value` shape: exactly
            // two child nodes around a bare `=`.
            let mut children = item.children();
            let (Some(name), Some(value), None) =
                (children.next(), children.next(), children.next())
            else {
                return None;
            };
            let (extra, body) = hug_value_parts(&value)?;
            let mut prefix = vec![lower_node(&name), Ir::text(" = ")];
            prefix.extend(extra);
            Some((Some(Ir::concat(prefix)), body))
        }
        _ => None,
    }
}

/// The prefix segment and grouped value body of a trailing *pair* (chain) item
/// in a hug: the whole `a => b => ` (behind a keyword's `name = `) joins the
/// flat prefix, and the hug body is the innermost value's own grouped lowering,
/// so the break lands inside that value's bracket rather than at any `=>`.
/// `None` for a non-pair item, whose normal lowering is already the right hug
/// body.
fn pair_hug_grouped_parts(item: &SyntaxNode) -> Option<(Ir, Ir)> {
    match item.kind() {
        SyntaxKind::ARG => {
            let mut children = item.children();
            let (Some(child), None) = (children.next(), children.next()) else {
                return None;
            };
            let (prefix, construct) = pair_hug_chain(&child)?;
            Some((prefix, lower_node(&construct)))
        }
        SyntaxKind::KEYWORD_ARG => {
            // `item_is_huggable` vetted the clean `name = value` shape.
            let mut children = item.children();
            let (Some(name), Some(value), None) =
                (children.next(), children.next(), children.next())
            else {
                return None;
            };
            let (prefix, construct) = pair_hug_chain(&value)?;
            Some((
                Ir::concat([lower_node(&name), Ir::text(" = "), prefix]),
                lower_node(&construct),
            ))
        }
        _ => None,
    }
}

/// The hug prefix contribution and ungrouped reflow body of a huggable item
/// value: a bracket-delimited construct contributes no prefix; a clean pair
/// chain (see [`pair_hug_chain`]) contributes the whole `a => b => ` and its
/// innermost value's body.
fn hug_value_parts(value: &SyntaxNode) -> Option<(Option<Ir>, Ir)> {
    if huggable_kind(value.kind()) {
        return Some((None, construct_reflow_body(value)?));
    }
    let (prefix, construct) = pair_hug_chain(value)?;
    let body = construct_reflow_body(&construct)?;
    Some((Some(prefix), body))
}

/// The ungrouped reflow body of a bracket-delimited construct, for folding into
/// an enclosing group that must own its break opportunities — an index subject
/// or a hugged trailing item. `None` for a construct without one (a paren
/// expression, a comment-bearing literal, an unmodeled shape) — the caller
/// bails to transparent.
fn construct_reflow_body(node: &SyntaxNode) -> Option<Ir> {
    match node.kind() {
        SyntaxKind::TUPLE_EXPR | SyntaxKind::VECT_EXPR | SyntaxKind::BRACES
            if !bracket_has_comment(node) =>
        {
            collection_reflow_body(node)
        }
        // A `BRACESCAT_EXPR` (`{a; b}`) is structurally the brace-delimited matrix
        // (same `ARG`/`MATRIX_ROW` children, `{`/`}` flowing through the tokens), so
        // its index subject yields through the shared matrix body — `;;` bails here
        // just as it does for a `MATRIX_EXPR` (`matrix_reflow_body` returns `None`).
        SyntaxKind::MATRIX_EXPR | SyntaxKind::BRACESCAT_EXPR if !matrix_has_comment(node) => {
            matrix_reflow_body(node)
        }
        SyntaxKind::PAREN_EXPR => paren_reflow_body(node),
        SyntaxKind::CALL_EXPR | SyntaxKind::CURLY_EXPR => call_reflow_body(node),
        SyntaxKind::INDEX_EXPR => index_reflow_body(node),
        SyntaxKind::COMPREHENSION | SyntaxKind::GENERATOR | SyntaxKind::BRACES_COMPREHENSION => {
            comprehension_reflow_body(node)
        }
        SyntaxKind::TYPED_COMPREHENSION => typed_comprehension_reflow_body(node),
        _ => None,
    }
}

/// The width-driven body of a clean collection literal: flat `[a, b]`, or one
/// element per indented line when it doesn't fit. An empty collection never
/// breaks; the one-tuple's semantic comma is emitted in both modes.
fn collection_body(node: &SyntaxNode, parts: CollectionParts) -> Ir {
    let singleton_comma = collection_singleton_comma(node, &parts.items);
    let CollectionParts {
        open, close, items, ..
    } = parts;
    if items.is_empty() {
        return Ir::concat([Ir::text(open), Ir::text(close)]);
    }
    collection_explode_body(&open, &items, &close, singleton_comma)
}

/// Whether this collection is the one-tuple `(a,)`, whose comma is semantic (it
/// distinguishes the tuple from a parenthesized expression) and so is emitted in
/// **both** layout modes, unlike every other list's broken-only trailing comma.
fn collection_singleton_comma(node: &SyntaxNode, items: &[Ir]) -> bool {
    node.kind() == SyntaxKind::TUPLE_EXPR && items.len() == 1
}

/// The parsed pieces of a clean collection literal: the bracket tokens, the
/// lowered comma-separated items, and whether the last element can hug the
/// closing bracket (see [`item_is_huggable`]).
struct CollectionParts {
    open: String,
    close: String,
    items: Vec<Ir>,
    last_huggable: bool,
}

/// Walk a collection literal's children into [`CollectionParts`]. `None` on any
/// unmodeled shape — a doubled/orphaned comma, a `;`-separated `PARAMETERS` row,
/// an unexpected child or token, a missing bracket — the caller falls back to
/// the verbatim transparent lowering. Source newlines carry no layout
/// information under Tenet 1 and are skipped like spaces.
fn collect_collection_items(node: &SyntaxNode) -> Option<CollectionParts> {
    let mut open: Option<String> = None;
    let mut close: Option<String> = None;
    let mut items: Vec<Ir> = Vec::new();
    let mut pending_comma = false;
    let mut last_huggable = false;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::LPAREN | SyntaxKind::LBRACKET | SyntaxKind::LBRACE => {
                    open = Some(tok.text().to_string())
                }
                SyntaxKind::RPAREN | SyntaxKind::RBRACKET | SyntaxKind::RBRACE => {
                    close = Some(tok.text().to_string())
                }
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => {}
                SyntaxKind::COMMA => {
                    if pending_comma || items.is_empty() {
                        return None;
                    }
                    pending_comma = true;
                }
                _ => return None,
            },
            NodeOrToken::Node(child) => match child.kind() {
                // `ARG` is a positional element; `KEYWORD_ARG` is a named-tuple
                // element (`(a = 1, b = 2)`), lowered by `lower_keyword_arg`.
                SyntaxKind::ARG | SyntaxKind::KEYWORD_ARG => {
                    if !items.is_empty() && !pending_comma {
                        return None;
                    }
                    last_huggable = item_is_huggable(&child);
                    items.push(lower_node(&child));
                    pending_comma = false;
                }
                _ => return None,
            },
        }
    }

    Some(CollectionParts {
        open: open?,
        close: close?,
        items,
        last_huggable,
    })
}

/// Build the ungrouped body of a call or curly application `callee(args)` /
/// `Callee{args}` appearing as an index subject — the callee followed by the arg
/// list's explode body (see [`applied_args_body`]) — for [`lower_index`]'s
/// shared outer group, so the call's break opportunities and the index tail are
/// measured together and the subject yields first. `None` on any shape whose
/// break points would not fold into the caller's group, falling back to the
/// transparent path (where the index yields): an interleaved token between
/// callee and arg list, a comment on either side, or an unmodeled tail.
fn call_reflow_body(node: &SyntaxNode) -> Option<Ir> {
    let mut parts = node.children_with_tokens();
    let (Some(first), Some(second), None) = (parts.next(), parts.next(), parts.next()) else {
        return None;
    };
    let (NodeOrToken::Node(callee), NodeOrToken::Node(args)) = (first, second) else {
        return None;
    };
    let callee_has_comment = callee
        .descendants_with_tokens()
        .any(|el| matches!(el.kind(), SyntaxKind::COMMENT | SyntaxKind::BLOCK_COMMENT));
    if args.kind() != SyntaxKind::ARG_LIST || callee_has_comment || bracket_has_comment(&args) {
        return None;
    }
    applied_args_body(lower_node(&callee), &args)
}

/// Fold a flat application prefix — a call's callee, or a name-rooted index
/// chain's base (see [`chain_root_is_name`]) — and its applied arg list into one
/// ungrouped body for the enclosing shared group. The arg list's break
/// opportunities become the group's, so the application yields first and any
/// postfix tail rides its closing bracket. A `; …` keyword tail folds in via
/// [`arg_list_params_body`]; a huggable last argument or keyword becomes an
/// ungrouped [`Ir::HugGroup`] (see [`reflow_hug`]). `None` on an unmodeled list
/// shape — the caller falls back to transparent.
fn applied_args_body(prefix: Ir, args: &SyntaxNode) -> Option<Ir> {
    let ArgListParts {
        open,
        close,
        items,
        params,
        last_huggable,
    } = collect_arg_list(args)?;

    if let Some(pnode) = params {
        let (pitems, last_param_huggable) = collect_param_items(&pnode)?;
        if last_param_huggable {
            let hug_prefix = params_hug_prefix(&open, &items, &pitems);
            let explode = arg_list_params_body(&open, items, pitems, &close);
            let hug = reflow_hug(hug_prefix, &last_list_item(&pnode)?, close, explode)?;
            return Some(Ir::concat([prefix, hug]));
        }
        return Some(Ir::concat([
            prefix,
            arg_list_params_body(&open, items, pitems, &close),
        ]));
    }

    // An empty list never breaks.
    if items.is_empty() {
        return Some(Ir::concat([prefix, Ir::text(open), Ir::text(close)]));
    }

    if last_huggable {
        let explode = arg_list_explode_body(&open, &items, &close);
        let mut hug_prefix: Vec<Ir> = vec![Ir::text(open.clone())];
        for item in &items[..items.len() - 1] {
            hug_prefix.push(item.clone());
            hug_prefix.push(Ir::text(", "));
        }
        let hug = reflow_hug(hug_prefix, &last_list_item(args)?, close, explode)?;
        return Some(Ir::concat([prefix, hug]));
    }

    Some(Ir::concat([
        prefix,
        arg_list_explode_body(&open, &items, &close),
    ]))
}

/// Lay out an index expression whose subject is a bracketed collection or matrix
/// literal — `[a, b][i]`, `(a, b)[i]`, `{a, b}[i]`, `[a b; c d][i, j]` — a
/// call or curly application — `f(a, b)[i]`, `A{T, S}[i]` — or a plain or
/// dotted name — `table[i][j]`, `config.table[i]`. The subject's break
/// opportunities and the index arg list share one outer group, so the whole
/// postfix is measured flat together and the **subject yields first** when it
/// overflows: the collection (or the call's arg list) explodes one element (or
/// matrix row) per line and the index rides the closing bracket, breaking at its
/// own column only if it still doesn't fit there. A name-rooted chain has no
/// subject breaks of its own, so its **first arg list** plays the yielding role
/// — `table[…][k]` explodes the first bracket and `[k]` rides — exactly as a
/// call's arg list does in `f(…)[k]`. Without the shared group the index — the
/// later, inner group — would take the break while a fitting subject stays
/// flat, leaving a lone index exploded like a stray vector literal.
///
/// A comprehension subject — plain, generator, braces, or typed
/// `Float64[…]` — yields the same way: the bracketed body explodes onto
/// element-and-clause lines and the index rides. Any other subject (a paren
/// expression) keeps the transparent lowering, where subject and index are
/// independent groups. A comment in the subject or the index list bails
/// likewise — those route to the comment-aware multiline paths unchanged — as
/// does a call whose arg list the shared group cannot own (see
/// [`call_reflow_body`]).
fn lower_index(node: &SyntaxNode) -> Ir {
    match index_reflow_body(node) {
        Some(body) => Ir::group(body),
        None => lower_transparent(node),
    }
}

/// Build the ungrouped body of a clean index expression `subject[args]` — the
/// subject's reflow body followed by the index arg list's group — for
/// [`lower_index`]'s shared outer group. Recursing through a chained-index
/// subject (`[…][i][j]`, `f(x)[i][j]`) folds the whole chain into one group, so
/// the innermost subject still yields first and every index rides the closing
/// bracket, breaking at its own column only if it overflows there. A chain
/// rooted at a plain or dotted name (`table[…][k]`) bottoms out the same way a
/// call does: the base joins flat and the first arg list is the yielding body
/// (via [`applied_args_body`]). `None` on any shape the shared group cannot own
/// — the caller falls back to transparent.
fn index_reflow_body(node: &SyntaxNode) -> Option<Ir> {
    let mut parts = node.children_with_tokens();
    let (Some(first), Some(second), None) = (parts.next(), parts.next(), parts.next()) else {
        return None;
    };
    let (NodeOrToken::Node(subject), NodeOrToken::Node(args)) = (first, second) else {
        // A token between subject and arg list (stray whitespace) is a shape the
        // parser should reject; keep it verbatim.
        return None;
    };
    if args.kind() != SyntaxKind::ARG_LIST || bracket_has_comment(&args) {
        return None;
    }
    if let Some(body) = construct_reflow_body(&subject) {
        return Some(Ir::concat([body, lower_arg_list(&args)]));
    }
    if !chain_root_is_name(&subject) {
        return None;
    }
    applied_args_body(lower_node(&subject), &args)
}

/// Whether an index subject is an eligible name-rooted chain base — a plain
/// name (`table`) or a dotted access (`config.lookup_table`), comment-free —
/// that joins the shared group flat while its first arg list yields. Any other
/// subject without a reflow body (a paren expression) keeps the transparent
/// path, where the index breaks on its own.
fn chain_root_is_name(subject: &SyntaxNode) -> bool {
    let dotted = subject.kind() == SyntaxKind::BINARY_EXPR
        && subject
            .children_with_tokens()
            .any(|el| el.kind() == SyntaxKind::DOT);
    if subject.kind() != SyntaxKind::NAME && !dotted {
        return false;
    }
    !subject
        .descendants_with_tokens()
        .any(|el| matches!(el.kind(), SyntaxKind::COMMENT | SyntaxKind::BLOCK_COMMENT))
}

/// Lay out a comprehension or generator — `[elem for b… if f]`, `(elem for b…)`,
/// or `{elem for b…}` (`COMPREHENSION`/`GENERATOR`/`BRACES_COMPREHENSION`; the
/// typed `T[…]` form is a `TYPED_COMPREHENSION` wrapping a `GENERATOR`, so the
/// transparent path snugs the type onto this handler's bracketed body). The node
/// is a bracket around an element expression, one or more `FOR_BINDING` clauses,
/// and an optional trailing `COMPREHENSION_IF` filter. Under Tenet 1 the spacing
/// is reflowed from scratch, **independent of the source line breaks**: one group
/// that stays flat (`[elem for b if f]`, single spaces) when it fits, else explodes
/// the element and each `for`/`if` clause onto its own indented line. A comprehension
/// already written across lines therefore collapses (when it fits) or re-explodes to
/// the same canonical form as its single-line twin — the element and clause recursions
/// skip `NEWLINE` tokens rather than mirroring them.
///
/// A comment anywhere in the subtree can't be reflowed away, so the whole node bails
/// to the verbatim transparent path.
fn lower_comprehension(node: &SyntaxNode) -> Ir {
    match comprehension_reflow_body(node) {
        Some(body) => Ir::group(body),
        None => lower_transparent(node),
    }
}

/// Build the ungrouped body behind [`lower_comprehension`] — also folded into
/// [`lower_index`]'s shared outer group (via [`construct_reflow_body`]) or an
/// enclosing hug, where the owning group must own the break opportunities so a
/// too-wide indexed comprehension explodes while its index rides the closing
/// bracket. `None` on any unmodeled shape — a comment anywhere in the subtree,
/// an unexpected token or child, a missing bracket or clause — the caller falls
/// back to transparent.
fn comprehension_reflow_body(node: &SyntaxNode) -> Option<Ir> {
    if node
        .descendants_with_tokens()
        .any(|el| matches!(el.kind(), SyntaxKind::COMMENT | SyntaxKind::BLOCK_COMMENT))
    {
        return None;
    }

    let mut open: Option<String> = None;
    let mut close: Option<String> = None;
    let mut element: Option<Ir> = None;
    let mut clauses: Vec<Ir> = Vec::new();

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::LBRACKET | SyntaxKind::LPAREN | SyntaxKind::LBRACE => {
                    open = Some(tok.text().to_string())
                }
                SyntaxKind::RBRACKET | SyntaxKind::RPAREN | SyntaxKind::RBRACE => {
                    close = Some(tok.text().to_string())
                }
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => {}
                _ => return None,
            },
            NodeOrToken::Node(child) => match child.kind() {
                SyntaxKind::FOR_BINDING => clauses.push(lower_for_binding(&child)),
                SyntaxKind::COMPREHENSION_IF => clauses.push(lower_comprehension_if(&child)?),
                // The element expression precedes every clause and occurs once.
                _ => {
                    if element.is_some() || !clauses.is_empty() {
                        return None;
                    }
                    element = Some(lower_node(&child));
                }
            },
        }
    }

    let (open, close, element) = (open?, close?, element?);
    if clauses.is_empty() {
        return None;
    }

    let mut inner: Vec<Ir> = vec![Ir::SoftLine, element];
    for clause in clauses {
        inner.push(Ir::Line);
        inner.push(clause);
    }

    Some(Ir::concat([
        Ir::text(open),
        Ir::indent(Ir::concat(inner)),
        Ir::SoftLine,
        Ir::text(close),
    ]))
}

/// The ungrouped reflow body of a typed comprehension `T[…]` — the type joins
/// the shared group flat, like a call's callee, and the bracketed generator
/// body carries the break opportunities, so a too-wide indexed
/// `Float64[…][idx]` explodes the comprehension while the index rides. `None`
/// unless the node is exactly the snug type + generator pair with a
/// comment-free type — anything else keeps the transparent path.
fn typed_comprehension_reflow_body(node: &SyntaxNode) -> Option<Ir> {
    let mut parts = node.children_with_tokens();
    let (Some(first), Some(second), None) = (parts.next(), parts.next(), parts.next()) else {
        return None;
    };
    let (NodeOrToken::Node(ty), NodeOrToken::Node(generator)) = (first, second) else {
        return None;
    };
    let ty_has_comment = ty
        .descendants_with_tokens()
        .any(|el| matches!(el.kind(), SyntaxKind::COMMENT | SyntaxKind::BLOCK_COMMENT));
    if ty_has_comment {
        return None;
    }
    Some(Ir::concat([
        lower_node(&ty),
        comprehension_reflow_body(&generator)?,
    ]))
}

/// Lower a `COMPREHENSION_IF` filter (`if <expr>`) to `if `-prefixed, with the
/// predicate recursed through [`lower_node`] so it normalizes. Returns `None` (the
/// caller bails) on a missing/duplicate predicate or an unexpected token.
fn lower_comprehension_if(node: &SyntaxNode) -> Option<Ir> {
    let mut if_kw = false;
    let mut filter: Option<SyntaxNode> = None;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => {}
                SyntaxKind::IF_KW if !if_kw => if_kw = true,
                _ => return None,
            },
            NodeOrToken::Node(child) => {
                if filter.is_some() {
                    return None;
                }
                filter = Some(child);
            }
        }
    }

    let filter = filter?;
    if !if_kw {
        return None;
    }
    Some(Ir::concat([Ir::text("if "), lower_node(&filter)]))
}

/// Lay out a macro call — `@name`, optionally followed by arguments. Normalizes
/// the whitespace the parser leaves verbatim around the macro name (Tenet 1):
///
/// - The **call form**, where an `ARG_LIST` is attached directly to the name with
///   no intervening space (`@eval(expr)`, `@foo(a, b)`), keeps its parenthesis
///   snug — the arg list lowers like any call's.
/// - The **space form**, where arguments follow the name separated by whitespace
///   (`@assert x > 0 "msg"`, `@inbounds a[i]`), collapses each gap to a single
///   space. The presence of that space is semantic — `@foo(a, b)` (two args) and
///   `@foo (a, b)` (one tuple arg) parse differently — so it is preserved, but its
///   width is not.
///
/// Each argument recurses through [`lower_node`], so it normalizes internally. The
/// space form never introduces a break: there is no canonical fold point between
/// space-separated macro arguments, so a wide argument breaks within its own
/// group (its call parens, say), like an arrow's right-hand side. An interleaved
/// comment or newline, a missing name, or any unexpected token falls back to the
/// verbatim transparent lowering.
fn lower_macro_call(node: &SyntaxNode) -> Ir {
    let mut parts: Vec<Ir> = Vec::new();
    let mut saw_name = false;
    // Whether a `WHITESPACE`/`NEWLINE` token has been seen since the last node —
    // distinguishes an attached `ARG_LIST` (call form) from a spaced argument.
    let mut had_gap = false;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => had_gap = true,
                _ => return lower_transparent(node),
            },
            NodeOrToken::Node(child) => match child.kind() {
                SyntaxKind::MACRO_NAME if !saw_name => {
                    let Some(name) = lower_macro_name(&child) else {
                        return lower_transparent(node);
                    };
                    parts.push(name);
                    saw_name = true;
                    had_gap = false;
                }
                // An argument must follow the name.
                _ if saw_name => {
                    // Attached `ARG_LIST` → call form (`@eval(expr)`); otherwise the
                    // argument is space-separated and gets one leading space.
                    let call_form = child.kind() == SyntaxKind::ARG_LIST && !had_gap;
                    if !call_form {
                        parts.push(Ir::text(" "));
                    }
                    parts.push(lower_node(&child));
                    had_gap = false;
                }
                _ => return lower_transparent(node),
            },
        }
    }

    if !saw_name {
        return lower_transparent(node);
    }
    Ir::concat(parts)
}

/// Render a `MACRO_NAME` (`@name`, or a dotted `Base.@kwdef`) as flat text, joining
/// its tokens with no whitespace. Returns `None` on any comment or unexpected token
/// so the caller can bail to the transparent lowering.
fn lower_macro_name(node: &SyntaxNode) -> Option<Ir> {
    let mut text = String::new();
    for el in node.descendants_with_tokens() {
        if let NodeOrToken::Token(tok) = el {
            match tok.kind() {
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => {}
                SyntaxKind::COMMENT | SyntaxKind::BLOCK_COMMENT => return None,
                _ => text.push_str(tok.text()),
            }
        }
    }
    Some(Ir::text(text))
}

/// Lay out a comma-separated list with normalized comma punctuation (no space
/// before a comma, one space after it): a bare (bracketless) tuple — `x, y`,
/// `a, b, c`, the lhs/rhs of a multiple assignment (`a, b = 1, 2`), a multi-value
/// `return x, y` — or a `let` binding list (`let x = 1, y = 2`). Elements are
/// bare nodes (not `ARG`-wrapped) separated by commas; each is lowered
/// recursively so its own normalization still applies (`f(x),g(y)` →
/// `f(x), g(y)`).
///
/// The layout is width-driven: flat `a, b, c` when it fits, else one element per
/// line with the comma trailing each element and the wrapped elements indented
/// one continuation step (the first element stays on the opening line — after
/// the `= `, `return `, `let `, or at column zero — and the rest wrap beneath
/// it). A bracketless list has no brackets to frame the break, so the comma
/// serves as the breakable separator; there is no broken-only trailing comma.
/// Source line breaks carry no layout information (Tenet 1): `x = a,\n b` reflows
/// to the same form as `x = a, b`.
///
/// Only the clean alternating shape `<el> , <el> [ , <el> ]…` is reshaped. A
/// leading/doubled/trailing comma (the trailing form is a parse error at this
/// level anyway), an interleaved comment, or any unexpected token falls back to
/// the verbatim transparent lowering.
fn lower_comma_list(node: &SyntaxNode) -> Ir {
    let mut first: Option<Ir> = None;
    let mut rest: Vec<Ir> = Vec::new();
    let mut item_count = 0usize;
    let mut pending_comma = false;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(tok) => match tok.kind() {
                // Source line breaks carry no layout information under Tenet 1.
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => {}
                SyntaxKind::COMMA => {
                    if pending_comma || item_count == 0 {
                        return lower_transparent(node);
                    }
                    pending_comma = true;
                }
                _ => return lower_transparent(node),
            },
            NodeOrToken::Node(child) => {
                if item_count > 0 && !pending_comma {
                    return lower_transparent(node);
                }
                let ir = lower_node(&child);
                if item_count == 0 {
                    first = Some(ir);
                } else {
                    // The comma trails the previous element on its line; the
                    // following element wraps at the breakable gap.
                    rest.push(Ir::text(","));
                    rest.push(Ir::Line);
                    rest.push(ir);
                }
                item_count += 1;
                pending_comma = false;
            }
        }
    }

    if pending_comma {
        return lower_transparent(node);
    }
    let Some(first) = first else {
        return lower_transparent(node);
    };
    if rest.is_empty() {
        return first;
    }

    // One width-driven group with its own continuation indent: flat `a, b, c`
    // when it fits, else comma-trailing with the wrapped elements indented one
    // step.
    Ir::group(Ir::concat([first, Ir::indent(Ir::concat(rest))]))
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

/// Collect the lowered items of a `;`-separated `PARAMETERS` tail so
/// [`lower_arg_list`] can fold them into its width-driven group. Returns the
/// item IRs (without the `;` or any separators) on the clean alternating
/// `; <item> [, <item>]…` shape, skipping source whitespace and newlines, plus
/// whether the last item can hug the closing bracket (see [`item_is_huggable`]).
/// Returns `None` — leaving the caller to emit the flat form — on any shape this
/// can't reflow: a comment, a doubled/orphaned comma, an unexpected child, a
/// missing semicolon, or an empty tail (`f(;)`).
fn collect_param_items(node: &SyntaxNode) -> Option<(Vec<Ir>, bool)> {
    let mut items: Vec<Ir> = Vec::new();
    let mut pending_comma = false;
    let mut seen_semi = false;
    let mut last_huggable = false;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::SEMICOLON if !seen_semi => seen_semi = true,
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => {}
                SyntaxKind::COMMA => {
                    if pending_comma || items.is_empty() {
                        return None;
                    }
                    pending_comma = true;
                }
                _ => return None,
            },
            NodeOrToken::Node(child) => {
                if !matches!(child.kind(), SyntaxKind::ARG | SyntaxKind::KEYWORD_ARG) {
                    return None;
                }
                if !items.is_empty() && !pending_comma {
                    return None;
                }
                last_huggable = item_is_huggable(&child);
                items.push(lower_node(&child));
                pending_comma = false;
            }
        }
    }

    // A trailing `pending_comma` is just a dropped trailing comma (`f(; a, b,)`),
    // matching the positional side; only a missing `;` or empty tail bails.
    if !seen_semi || items.is_empty() {
        return None;
    }

    Some((items, last_huggable))
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
                // Source newlines carry no layout information under Tenet 1; skipping
                // them (not bailing) lets a multi-line `for`-binding reflow.
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => {}
                SyntaxKind::FOR_KW if !for_kw && els.is_empty() => for_kw = true,
                SyntaxKind::COMMENT | SyntaxKind::BLOCK_COMMENT => {
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
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => {}
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

/// The maximum number of consecutive blank lines the target style keeps; anything
/// more is collapsed to this. A block body keeps at most a single blank line
/// between statements (and at most one leading/trailing), so any run of blanks
/// condenses to one.
const MAX_BLANK_LINES: usize = 1;

/// Lay out a comment-bearing bracketed list — a call/index `ARG_LIST` or a
/// `(…)`/`[…]`/`{…}` collection — that the width-driven path
/// ([`lower_arg_list`]/[`lower_collection`]) routes here because a comment cannot
/// be reflowed away. Under Tenet 1 the layout is fully exploded, independent of
/// the source's own line breaks:
///
/// - **Always broken, one item per line.** A line break frames the content after
///   the open bracket and before the close bracket (indented one step, close
///   bracket flush), and every item lands on its own line — items that shared a
///   source line (`a, b`) are split apart.
/// - **A trailing comma is always added** (the list is always broken), matching
///   the width-driven path's broken layout.
/// - **Comments keep their attachment.** A trailing comment (`item, # …`) rides
///   after its item's comma, canonicalized to one leading space; an own-line
///   comment occupies its own line in place; a comment on the open-bracket line
///   rides after the open bracket.
/// - **Blank lines are dropped** (source formatting, like the width-driven path).
///
/// Only a clean shape is reshaped. Anything this does not fully model falls back
/// to the verbatim transparent lowering: a `;`-separated `PARAMETERS` block or
/// bare semicolon, a doubled or leading comma, two items with no comma between
/// them, two comments on one line, an empty bracket, or any unexpected child or
/// token.
fn lower_multiline_bracket(node: &SyntaxNode) -> Ir {
    let mut open: Option<String> = None;
    let mut close: Option<String> = None;
    let mut items: Vec<Ir> = Vec::new();
    // A trailing comment riding on item `i` (`item, # …`), rendered after its
    // comma. Aligned with `items`; a `None` slot is pushed for every item.
    let mut item_comments: Vec<Option<String>> = Vec::new();
    // The own-line comments preceding item `i` (the gap before it). Index `i`
    // holds the comments between item `i-1` and item `i`; the leading gap (before
    // the first item) and the trailing gap (after the last) flank them.
    let mut gaps: Vec<Vec<String>> = Vec::new();
    // Own-line comments accumulated since the last item, flushed when the next
    // item or the close bracket arrives.
    let mut gap: Vec<String> = Vec::new();
    let mut leading: Vec<String> = Vec::new();
    let mut header_comment: Option<String> = None;
    // Whether any line content (item or own-line comment) sits on the current
    // source line, so a fresh comment can be classified trailing vs own-line.
    // Starts true: the open bracket is the current line, so a comment before any
    // newline (`[ # header`) is a trailing comment on it, not an own-line comment.
    let mut on_line = true;
    let mut comma = false;
    let mut leading_comma = false;

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
                // A comment before the first newline rides on the open-bracket
                // line; once a newline is seen, subsequent comments are own-line.
                SyntaxKind::NEWLINE => on_line = false,
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
                    // no-op for it; its multi-line interior is preserved verbatim. A
                    // line comment's own trailing whitespace is trimmed.
                    let text = tok.text().trim_end_matches([' ', '\t']).to_string();
                    if on_line {
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
                        gap.push(text);
                        on_line = true;
                    }
                }
                _ => return lower_transparent(node),
            },
            NodeOrToken::Node(child) => match child.kind() {
                SyntaxKind::ARG | SyntaxKind::KEYWORD_ARG => {
                    if items.is_empty() {
                        if leading_comma {
                            return lower_transparent(node);
                        }
                        leading = std::mem::take(&mut gap);
                    } else {
                        if !comma {
                            return lower_transparent(node);
                        }
                        gaps.push(std::mem::take(&mut gap));
                    }
                    items.push(lower_node(&child));
                    item_comments.push(None);
                    comma = false;
                    on_line = true;
                }
                _ => return lower_transparent(node),
            },
        }
    }

    // The final gap runs from the last item to the close bracket.
    let trailing = gap;

    let (Some(open), Some(close)) = (open, close) else {
        return lower_transparent(node);
    };
    if items.is_empty() {
        return lower_transparent(node);
    }

    // Render a gap's own-line comments, each opening its own indented line.
    fn render_gap(inner: &mut Vec<Ir>, comments: &[String]) {
        for text in comments {
            inner.push(Ir::HardLine);
            inner.push(Ir::text(text.clone()));
        }
    }

    let n = items.len();
    let mut inner: Vec<Ir> = Vec::new();
    render_gap(&mut inner, &leading);
    inner.push(Ir::HardLine); // framing break after the open bracket
    for (i, item) in items.into_iter().enumerate() {
        inner.push(item);
        // Always broken, so every item — including the last — grows a comma.
        inner.push(Ir::text(","));
        // The trailing comment rides after the comma, canonicalized to one leading
        // space (its same-line attachment is preserved, not its source spacing).
        if let Some(text) = &item_comments[i] {
            inner.push(Ir::text(" "));
            inner.push(Ir::text(text.clone()));
        }
        if i + 1 < n {
            render_gap(&mut inner, &gaps[i]);
            inner.push(Ir::HardLine);
        }
    }
    render_gap(&mut inner, &trailing);

    // A comment on the open-bracket line rides after it, canonicalized to one
    // leading space (the same attachment-preserving rule as a trailing comment).
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

/// Lay out a matrix literal (`[1 2; 3 4]`). The canonical form is width-driven
/// (Tenet 1): a comment-free matrix builds one `Ir::group`, laid out flat on a
/// single line — rows joined by `; `, elements within a row by a single space —
/// when it fits `line_width`, else framed with one row per indented line and the
/// `;` separators replaced by line breaks. Source line breaks, the `;`-vs-newline
/// spelling of a row separator, and intra-row spacing never influence the result:
/// `[1 2; 3 4]` and the same matrix written across source lines format identically.
///
/// A comment can't be reflowed away, so a comment-bearing matrix routes to the
/// verbatim-preserving multiline path (or the transparent fallback when it spans
/// no line to frame).
fn lower_matrix(node: &SyntaxNode) -> Ir {
    if matrix_has_comment(node) {
        return if has_newline_token(node) {
            lower_matrix_multiline(node)
        } else {
            lower_transparent(node)
        };
    }
    lower_matrix_reflow(node)
}

/// Width-driven layout for a clean (comment-free) matrix: parse it into rows
/// (split at `;` and source newlines, which are equivalent row separators), then
/// emit one `Ir::group` — flat `[a b; c d]` when it fits, else framed one row per
/// indented line. Empty rows (a framing newline, a blank line, a trailing `;`) are
/// dropped. A `;;` higher-dimensional separator, or any unexpected token or child,
/// bails to the verbatim transparent lowering.
fn lower_matrix_reflow(node: &SyntaxNode) -> Ir {
    match matrix_reflow_body(node) {
        Some(body) => Ir::group(body),
        None => lower_transparent(node),
    }
}

/// Build the ungrouped body of a clean matrix literal — the doc that
/// [`lower_matrix_reflow`] wraps in its own `Ir::group`, and that [`lower_index`]
/// folds into a shared outer group with the index tail. `None` on any shape the
/// reflow does not fully model (the caller falls back to transparent).
fn matrix_reflow_body(node: &SyntaxNode) -> Option<Ir> {
    let mut open: Option<String> = None;
    let mut close: Option<String> = None;
    let mut rows: Vec<Vec<Ir>> = vec![Vec::new()];
    let mut prev_was_semicolon = false;
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(tok) => match tok.kind() {
                // A `MATRIX_EXPR` opens `[`/`]`; a `BRACESCAT_EXPR` (`{a; b}`, the
                // brace-delimited vcat/matrix) opens `{`/`}`. Both share the same
                // row/`;`/newline shape, so they lower identically — the actual
                // bracket text flows through from the token.
                SyntaxKind::LBRACKET | SyntaxKind::LBRACE => open = Some(tok.text().to_string()),
                SyntaxKind::RBRACKET | SyntaxKind::RBRACE => close = Some(tok.text().to_string()),
                // Whitespace carries no layout and is transparent to the `;;` check.
                SyntaxKind::WHITESPACE => continue,
                // `;` and a source newline both separate rows; a blank line or a
                // redundant `;`-then-newline yields an empty row that is dropped
                // below. Two adjacent `;` are the `;;` higher-dim operator, whose
                // semantics differ from `;` — bail rather than silently collapse it.
                SyntaxKind::SEMICOLON => {
                    if prev_was_semicolon {
                        return None;
                    }
                    rows.push(Vec::new());
                    prev_was_semicolon = true;
                    continue;
                }
                SyntaxKind::NEWLINE => rows.push(Vec::new()),
                _ => return None,
            },
            NodeOrToken::Node(child) => match child.kind() {
                SyntaxKind::ARG => rows.last_mut().unwrap().push(lower_node(&child)),
                // A `MATRIX_ROW` is a whole horizontal row; collect its elements.
                SyntaxKind::MATRIX_ROW => {
                    let row = rows.last_mut().unwrap();
                    for sub in child.children_with_tokens() {
                        match sub {
                            NodeOrToken::Token(t) if t.kind() == SyntaxKind::WHITESPACE => {}
                            NodeOrToken::Node(arg) if arg.kind() == SyntaxKind::ARG => {
                                row.push(lower_node(&arg))
                            }
                            _ => return None,
                        }
                    }
                }
                _ => return None,
            },
        }
        prev_was_semicolon = false;
    }

    let (open, close) = (open?, close?);
    rows.retain(|row| !row.is_empty());
    if rows.is_empty() {
        return Some(Ir::concat([Ir::text(open), Ir::text(close)]));
    }

    let mut inner: Vec<Ir> = vec![Ir::SoftLine];
    for (i, row) in rows.into_iter().enumerate() {
        if i > 0 {
            // Between rows: flat `;` + space -> `; `; broken nothing + newline.
            inner.push(Ir::if_break("", ";"));
            inner.push(Ir::Line);
        }
        for (j, elem) in row.into_iter().enumerate() {
            if j > 0 {
                inner.push(Ir::text(" "));
            }
            inner.push(elem);
        }
    }

    Some(Ir::concat([
        Ir::text(open),
        Ir::indent(Ir::concat(inner)),
        Ir::SoftLine,
        Ir::text(close),
    ]))
}

/// Whether a matrix carries a comment among its direct children or inside one of
/// its `MATRIX_ROW`s. Such a matrix can't reflow and routes to the multiline path.
fn matrix_has_comment(node: &SyntaxNode) -> bool {
    node.children_with_tokens().any(|el| match el {
        NodeOrToken::Token(t) => {
            matches!(t.kind(), SyntaxKind::COMMENT | SyntaxKind::BLOCK_COMMENT)
        }
        NodeOrToken::Node(child) => {
            child.kind() == SyntaxKind::MATRIX_ROW
                && child
                    .children_with_tokens()
                    .any(|e| matches!(e.kind(), SyntaxKind::COMMENT | SyntaxKind::BLOCK_COMMENT))
        }
    })
}

/// Lay out a comment-bearing matrix literal that cannot reflow flat. The canonical
/// form mirrors [`lower_multiline_bracket`] (Tenet 1): always framed with one row
/// per indented line, row elements joined by a single space, the `;`/newline row
/// separators and all source spacing normalized away. A comment is the only
/// surviving source dependence (it is content, not layout): a trailing comment
/// rides its row canonicalized to one leading space; an own-line comment keeps its
/// own line; a comment on the open-bracket line (`[ # header`) rides after `[`.
/// Blank lines are dropped (a matrix is not a statement list). Nested handled
/// constructs still normalize because each row element is lowered recursively. Any
/// shape this does not fully model — a comment inside a `MATRIX_ROW`, a `;;`
/// higher-dim separator surfacing as an unexpected token, or a missing bracket —
/// falls back to the verbatim transparent lowering.
fn lower_matrix_multiline(node: &SyntaxNode) -> Ir {
    let mut open: Option<String> = None;
    let mut close: Option<String> = None;
    // Each matrix row becomes one framed line; `items[i]` is its space-joined
    // elements. Comments classify exactly as in `lower_multiline_bracket`.
    let mut items: Vec<Ir> = Vec::new();
    let mut item_comments: Vec<Option<String>> = Vec::new();
    // Own-line comments preceding row `i`; `leading` flanks the first row and
    // `trailing` (the final `gap`) the close bracket.
    let mut gaps: Vec<Vec<String>> = Vec::new();
    let mut gap: Vec<String> = Vec::new();
    let mut leading: Vec<String> = Vec::new();
    let mut header_comment: Option<String> = None;
    // Whether content sits on the current source line, so a fresh comment can be
    // classified trailing vs own-line. Starts true: the open bracket is the
    // current line, so `[ # header` is a trailing comment on it.
    let mut on_line = true;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(tok) => match tok.kind() {
                // `[`/`]` for a matrix, `{`/`}` for a bracescat — see
                // [`matrix_reflow_body`]; the bracket text flows through verbatim.
                SyntaxKind::LBRACKET | SyntaxKind::LBRACE => open = Some(tok.text().to_string()),
                SyntaxKind::RBRACKET | SyntaxKind::RBRACE => close = Some(tok.text().to_string()),
                SyntaxKind::WHITESPACE => {}
                // `;` and a source newline are equivalent row separators; both are
                // layout-only here (rows are already framed one per line). Only the
                // newline flips `on_line` for comment classification.
                SyntaxKind::SEMICOLON => {}
                SyntaxKind::NEWLINE => on_line = false,
                SyntaxKind::COMMENT | SyntaxKind::BLOCK_COMMENT => {
                    // A block-comment token always ends with `=#`, so the trim is a
                    // no-op for it; its multi-line interior is preserved verbatim. A
                    // line comment's own trailing whitespace is trimmed.
                    let text = tok.text().trim_end_matches([' ', '\t']).to_string();
                    if on_line {
                        // Same line as the previous content: a trailing comment on
                        // the last row, or — before any row — on the open bracket.
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
                        gap.push(text);
                        on_line = true;
                    }
                }
                _ => return lower_transparent(node),
            },
            NodeOrToken::Node(child) => match child.kind() {
                // A whole horizontal row, or a bare `ARG` for a single-element
                // (newline/`;`-separated) row.
                SyntaxKind::MATRIX_ROW | SyntaxKind::ARG => {
                    let row = if child.kind() == SyntaxKind::ARG {
                        lower_node(&child)
                    } else {
                        match lower_matrix_row(&child) {
                            Some(row) => row,
                            None => return lower_transparent(node),
                        }
                    };
                    if items.is_empty() {
                        leading = std::mem::take(&mut gap);
                    } else {
                        gaps.push(std::mem::take(&mut gap));
                    }
                    items.push(row);
                    item_comments.push(None);
                    on_line = true;
                }
                _ => return lower_transparent(node),
            },
        }
    }

    // The final gap runs from the last row to the close bracket.
    let trailing = gap;

    let (Some(open), Some(close)) = (open, close) else {
        return lower_transparent(node);
    };
    if items.is_empty() {
        return lower_transparent(node);
    }

    // Render a gap's own-line comments, each opening its own indented line.
    fn render_gap(inner: &mut Vec<Ir>, comments: &[String]) {
        for text in comments {
            inner.push(Ir::HardLine);
            inner.push(Ir::text(text.clone()));
        }
    }

    let n = items.len();
    let mut inner: Vec<Ir> = Vec::new();
    render_gap(&mut inner, &leading);
    inner.push(Ir::HardLine); // framing break after the open bracket
    for (i, item) in items.into_iter().enumerate() {
        inner.push(item);
        // The trailing comment rides the row, canonicalized to one leading space
        // (same-line attachment preserved, source spacing dropped).
        if let Some(text) = &item_comments[i] {
            inner.push(Ir::text(" "));
            inner.push(Ir::text(text.clone()));
        }
        if i + 1 < n {
            render_gap(&mut inner, &gaps[i]);
            inner.push(Ir::HardLine);
        }
    }
    render_gap(&mut inner, &trailing);

    // A comment on the open-bracket line rides after it, canonicalized to one
    // leading space (the same attachment-preserving rule as a trailing comment).
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

/// Join a `MATRIX_ROW`'s elements with a single space, lowering each recursively.
/// Returns `None` for any shape the framed matrix path cannot model — an inline
/// comment between row elements, an empty row, or an unexpected child — so the
/// caller bails to the verbatim transparent lowering.
fn lower_matrix_row(node: &SyntaxNode) -> Option<Ir> {
    let mut elems: Vec<Ir> = Vec::new();
    for sub in node.children_with_tokens() {
        match sub {
            NodeOrToken::Token(t) if t.kind() == SyntaxKind::WHITESPACE => {}
            NodeOrToken::Node(arg) if arg.kind() == SyntaxKind::ARG => {
                elems.push(lower_node(&arg));
            }
            _ => return None,
        }
    }
    if elems.is_empty() {
        return None;
    }
    let mut row: Vec<Ir> = Vec::new();
    for (j, elem) in elems.into_iter().enumerate() {
        if j > 0 {
            row.push(Ir::text(" "));
        }
        row.push(elem);
    }
    Some(Ir::concat(row))
}

/// Lay out a keyword block whose body is a bare `BLOCK` — `begin … end` and
/// `quote … end` — by indenting each statement one step, matching the target
/// style. The shape is `<kw> BLOCK <end>`; the body is lowered by
/// [`lower_block_body`].
///
/// A **non-empty** block is always exploded to the vertical form, even if the
/// source wrote it on one line: `begin x end` → `begin⏎    x⏎end`. An **empty**
/// block collapses to the canonical inline `begin end`/`quote end` via
/// [`push_block_body`], regardless of how the source spaced it (Tenet 1). Any
/// shape this does not fully model — a comment in the body, two statements with
/// no separator, a missing `end`, or an unexpected child — falls back to the
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

    let mut parts = vec![Ir::text(kw)];
    if !push_block_body(&mut parts, &block, || lower_block_body(&block)) {
        return lower_transparent(node);
    }
    Ir::concat(parts)
}

/// Append a block's body to `parts` under the shared empty-body policy, returning
/// `false` when the caller must bail to the verbatim transparent lowering.
///
/// `render` produces the body IR (`None` for an empty or unmodeled body). A
/// non-empty body explodes to the vertical form (body + `HardLine` + `end`); a
/// genuinely empty body (only whitespace, newline, or `;` tokens, per
/// [`block_is_empty`]) collapses to the canonical inline ` end`, so every
/// spelling of an empty body formats identically (Tenet 1); any other `None`
/// (a body shape the engine rejects) reports `false` so the caller bails.
fn push_block_body(
    parts: &mut Vec<Ir>,
    block: &SyntaxNode,
    render: impl FnOnce() -> Option<Ir>,
) -> bool {
    match render() {
        Some(body) => {
            parts.push(body);
            parts.push(Ir::HardLine);
            parts.push(Ir::text("end"));
            true
        }
        None if block_is_empty(block) => {
            parts.push(Ir::text(" end"));
            true
        }
        None => false,
    }
}

/// Lay out a `struct`/`mutable struct` definition by indenting its field body
/// one step, reusing the [`lower_block_body`] engine the other block rules
/// share. The shape is `[MUTABLE_KW] STRUCT_KW SIGNATURE BLOCK END_KW`; the
/// `SIGNATURE` header (the type name, optional `{…}` type parameters, and an
/// optional `<: Super` supertype) is lowered recursively, so its inner spacing
/// normalizes (`struct Bar<:Animal` → `struct Bar <: Animal`).
///
/// Like the loop and `begin`/`let` rules, a **non-empty** body is always
/// exploded to the vertical form even when the source wrote it on one line
/// (`struct Foo x; y end` → `struct Foo⏎    x; y⏎end`). An **empty** body
/// (whose `BLOCK` holds only whitespace, newline, or `;` tokens) collapses to
/// the canonical inline `struct Name end`, regardless of how the source spaced
/// it (`struct E⏎end`, `struct E⏎⏎end`, and `struct E end` all format the same —
/// Tenet 1). Struct field bodies are declarations, never `return`-inserted (only
/// function bodies are), so there is no semantic-rewrite risk. Any shape this
/// does not fully model — a body comment the engine rejects, a missing signature
/// or `end`, an unexpected child — falls back to the verbatim transparent
/// lowering.
fn lower_struct(node: &SyntaxNode) -> Ir {
    let mut mutable = false;
    let mut signature: Option<SyntaxNode> = None;
    let mut block: Option<SyntaxNode> = None;
    let mut saw_struct = false;
    let mut saw_end = false;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) => match child.kind() {
                SyntaxKind::SIGNATURE if signature.is_none() && block.is_none() => {
                    signature = Some(child)
                }
                SyntaxKind::BLOCK if block.is_none() => block = Some(child),
                _ => return lower_transparent(node),
            },
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::MUTABLE_KW if !mutable => mutable = true,
                SyntaxKind::STRUCT_KW if !saw_struct => saw_struct = true,
                SyntaxKind::END_KW => saw_end = true,
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => {}
                _ => return lower_transparent(node),
            },
        }
    }

    let (true, true, Some(signature), Some(block)) = (saw_struct, saw_end, signature, block) else {
        return lower_transparent(node);
    };

    let mut parts: Vec<Ir> = Vec::new();
    if mutable {
        parts.push(Ir::text("mutable "));
    }
    parts.push(Ir::text("struct "));
    parts.push(lower_node(&signature));
    if !push_block_body(&mut parts, &block, || lower_block_body(&block)) {
        return lower_transparent(node);
    }
    Ir::concat(parts)
}

/// True iff `block` is an empty body — only whitespace, newline, and `;`
/// separator tokens, with no statement nodes or comments. Distinguishes a body
/// that should collapse to the inline form from one [`lower_block_body`] rejects
/// for an unmodeled shape (which must bail to the verbatim transparent path).
fn block_is_empty(block: &SyntaxNode) -> bool {
    block.children_with_tokens().all(|el| {
        matches!(
            el,
            NodeOrToken::Token(tok) if matches!(
                tok.kind(),
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE | SyntaxKind::SEMICOLON
            )
        )
    })
}

/// Lower a block body under the empty-body policy the multi-clause `if`/`try`
/// rules share. Unlike [`push_block_body`] (single-body blocks, whose empty body
/// folds inline against the trailing `end`), a clause's `end` is shared by the
/// whole construct, so an empty body here contributes **no** lines — the keyword
/// header is followed directly by the next clause or the final `end`.
///
/// Returns `Some(Some(ir))` for a rendered non-empty body, `Some(None)` for a
/// genuinely empty body (per [`block_is_empty`]), and `None` for a shape the
/// engine rejects, so the caller bails to the verbatim transparent lowering.
fn lower_body_allow_empty(block: &SyntaxNode) -> Option<Option<Ir>> {
    match lower_block_body(block) {
        Some(ir) => Some(Some(ir)),
        None if block_is_empty(block) => Some(None),
        None => None,
    }
}

/// Lay out a `function`/`macro` definition. The shape mirrors the other block
/// rules (`(FUNCTION_KW|MACRO_KW) SIGNATURE BLOCK END_KW`): the body is delegated
/// to [`lower_block_body`], and the `SIGNATURE` (which carries the name, argument
/// list, an optional `::` return type, and a `where` clause as one node) is
/// lowered recursively so its inner spacing normalizes. The keyword is always
/// followed by exactly one space, which also inserts the canonical space an
/// anonymous `function(x)` is missing (`function (x)`).
///
/// Fatou is layout-only: it never inserts an implicit `return` and never inspects
/// the body's tail. Any non-empty body is reshaped and re-indented to the
/// canonical body indent regardless of how its tail is written (`function f() x
/// end` lays out `x` at the body indent, untouched). An **empty** body collapses
/// to the canonical inline `function f() end` via [`push_block_body`], regardless
/// of how the source spaced it (Tenet 1). Any unmodeled shape (a missing
/// signature or `end`, an unexpected child) bails to the verbatim transparent
/// lowering.
fn lower_function(node: &SyntaxNode) -> Ir {
    let kw = match node.kind() {
        SyntaxKind::FUNCTION_DEF => "function",
        SyntaxKind::MACRO_DEF => "macro",
        _ => return lower_transparent(node),
    };

    let mut signature: Option<SyntaxNode> = None;
    let mut block: Option<SyntaxNode> = None;
    let mut saw_kw = false;
    let mut saw_end = false;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) => match child.kind() {
                SyntaxKind::SIGNATURE if signature.is_none() && block.is_none() => {
                    signature = Some(child)
                }
                SyntaxKind::BLOCK if block.is_none() => block = Some(child),
                _ => return lower_transparent(node),
            },
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::FUNCTION_KW | SyntaxKind::MACRO_KW if !saw_kw => saw_kw = true,
                SyntaxKind::END_KW => saw_end = true,
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => {}
                _ => return lower_transparent(node),
            },
        }
    }

    let (true, true, Some(signature), Some(block)) = (saw_kw, saw_end, signature, block) else {
        return lower_transparent(node);
    };

    let mut parts = vec![Ir::text(kw), Ir::text(" "), lower_node(&signature)];
    if !push_block_body(&mut parts, &block, || lower_block_body(&block)) {
        return lower_transparent(node);
    }
    Ir::concat(parts)
}

/// Lay out a `do`-block (`CALL_EXPR do <params> BLOCK end`). The call head sits
/// *before* the `do` keyword (`map(xs) do x`), with an optional `DO_PARAMS`
/// argument list after it; both are lowered recursively (the head normalizes its
/// own arg spacing, the params get `", "`-joined) and the body is delegated to
/// the shared [`lower_block_body`] engine, exploding a non-empty body to the
/// vertical form like the other block rules.
///
/// `do`-block bodies are layout-only, never `return`-inserted (a bare tail
/// expression stays bare), so there is no semantic-rewrite guard here — any
/// non-empty body may be reshaped. A `do`-block is single-bodied, so an **empty**
/// body folds inline against the trailing `end` via [`push_block_body`]
/// (`foo() do end`, `map(xs) do x end`), exactly like the other single-body
/// blocks. Any unmodeled shape — a comment or newline in the params, a missing
/// head/keyword/`end`, an unexpected child — bails to the verbatim transparent
/// lowering.
fn lower_do(node: &SyntaxNode) -> Ir {
    let mut head: Option<SyntaxNode> = None;
    let mut params: Option<SyntaxNode> = None;
    let mut block: Option<SyntaxNode> = None;
    let mut saw_do = false;
    let mut saw_end = false;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) => {
                if !saw_do && head.is_none() {
                    head = Some(child);
                } else if saw_do
                    && child.kind() == SyntaxKind::DO_PARAMS
                    && params.is_none()
                    && block.is_none()
                {
                    params = Some(child);
                } else if saw_do && child.kind() == SyntaxKind::BLOCK && block.is_none() {
                    block = Some(child);
                } else {
                    return lower_transparent(node);
                }
            }
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::DO_KW if !saw_do => saw_do = true,
                SyntaxKind::END_KW => saw_end = true,
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => {}
                _ => return lower_transparent(node),
            },
        }
    }

    let (true, true, Some(head), Some(block)) = (saw_do, saw_end, head, block) else {
        return lower_transparent(node);
    };

    let mut parts = vec![lower_node(&head), Ir::text(" do")];
    if let Some(params) = params {
        let Some(params_ir) = lower_do_params(&params) else {
            return lower_transparent(node);
        };
        parts.push(Ir::text(" "));
        parts.push(params_ir);
    }
    // A `do` block is single-bodied, so its empty body folds inline against the
    // trailing `end` (`f(xs) do x end`) exactly as the other single-body blocks.
    if !push_block_body(&mut parts, &block, || lower_block_body(&block)) {
        return lower_transparent(node);
    }
    Ir::concat(parts)
}

/// Lower a `do`-block's `DO_PARAMS` list, `", "`-joining its comma-separated
/// items (each lowered recursively, so a destructuring tuple `do (x, y)`
/// normalizes too). Returns `None` for any shape this does not model — a comment
/// or newline, a leading/trailing/doubled comma, an empty list — so the caller
/// bails the whole `do`-block to the verbatim transparent lowering.
fn lower_do_params(node: &SyntaxNode) -> Option<Ir> {
    let mut parts: Vec<Ir> = Vec::new();
    let mut expect_item = true;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) if expect_item => {
                parts.push(lower_node(&child));
                expect_item = false;
            }
            // A node when a comma was expected (two adjacent items).
            NodeOrToken::Node(_) => return None,
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::COMMA if !expect_item => {
                    parts.push(Ir::text(", "));
                    expect_item = true;
                }
                SyntaxKind::WHITESPACE => {}
                _ => return None,
            },
        }
    }

    // Reject an empty list and a trailing comma (`expect_item` still set).
    if parts.is_empty() || expect_item {
        return None;
    }
    Some(Ir::concat(parts))
}

/// Lay out an `abstract type`/`primitive type` declaration. These are bodyless
/// one-liners (`ABSTRACT_DEF` = `abstract type SIGNATURE end`, `PRIMITIVE_DEF` =
/// `primitive type SIGNATURE <bits> end`). **Width-driven (Tenet 1):** every
/// whitespace run collapses to a single space, so the layout is independent of the
/// source spacing — the keyword region (after `abstract`/`primitive` and after
/// `type`) *and* the trailing region (around the optional bits `LITERAL` and the
/// `end`) all render with exactly one space. The `SIGNATURE` and bits `LITERAL`
/// lower recursively, so a tight supertype normalizes too (`Bar<:Baz` →
/// `Bar <: Baz`).
///
/// Any shape this does not model — a comment or newline anywhere in the
/// declaration, a missing signature, fewer than two leading keyword tokens — falls
/// back to the verbatim transparent lowering.
fn lower_type_decl(node: &SyntaxNode) -> Ir {
    let mut parts: Vec<Ir> = Vec::new();
    // Number of leading keyword idents (`abstract`/`primitive`, then `type`) seen;
    // their following whitespace is collapsed to a single space until the signature.
    let mut kw_count = 0u8;
    let mut seen_sig = false;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) if child.kind() == SyntaxKind::SIGNATURE && !seen_sig => {
                parts.push(lower_node(&child));
                seen_sig = true;
            }
            // After the signature, nodes (the bits `LITERAL`) lower normally.
            NodeOrToken::Node(child) if seen_sig => parts.push(lower_node(&child)),
            NodeOrToken::Node(_) => return lower_transparent(node),
            // Trailing region: collapse whitespace to one space, keep `end`, bail
            // on anything else (a comment or newline we don't model).
            NodeOrToken::Token(tok) if seen_sig => match tok.kind() {
                SyntaxKind::WHITESPACE => parts.push(Ir::text(" ")),
                SyntaxKind::END_KW => parts.push(Ir::text(tok.text().to_string())),
                _ => return lower_transparent(node),
            },
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::IDENT if kw_count < 2 => {
                    parts.push(Ir::text(tok.text().to_string()));
                    kw_count += 1;
                }
                // Collapse a whitespace run in the keyword region to one space.
                SyntaxKind::WHITESPACE => parts.push(Ir::text(" ")),
                _ => return lower_transparent(node),
            },
        }
    }

    if kw_count == 2 && seen_sig {
        Ir::concat(parts)
    } else {
        lower_transparent(node)
    }
}

/// Lay out a `module`/`baremodule` definition. The shape mirrors the other block
/// rules (`[BARE]MODULE_KW SIGNATURE BLOCK END_KW`), but the body is *conditionally*
/// indented: a module body is **not** indented when the module sits alone at
/// the file's top level (the file-as-a-module-wrapper convention) or is nested
/// directly inside a non-module block. It *is* indented when the module shares the
/// top level with a sibling, or when it has a `module` ancestor (a nested module).
/// See [`module_should_indent`] for the exact predicate — deterministic on AST
/// structure alone, so Tenet 1 holds.
///
/// Module bodies are declarations, never `return`-inserted, so there is no
/// semantic-rewrite risk here (a `function` *inside* the body still would be, so
/// fixtures keep those out). An **empty** body collapses to the canonical inline
/// `module M end` via [`push_block_body`], regardless of how the source spaced it
/// (Tenet 1). Any unmodeled shape — a missing signature or `end`, a body
/// comment the engine rejects, an unexpected child — also falls back to the
/// verbatim transparent lowering.
fn lower_module(node: &SyntaxNode) -> Ir {
    let mut kw: Option<&'static str> = None;
    let mut signature: Option<SyntaxNode> = None;
    let mut block: Option<SyntaxNode> = None;
    let mut saw_end = false;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) => match child.kind() {
                SyntaxKind::SIGNATURE if signature.is_none() && block.is_none() => {
                    signature = Some(child)
                }
                SyntaxKind::BLOCK if block.is_none() => block = Some(child),
                _ => return lower_transparent(node),
            },
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::MODULE_KW if kw.is_none() => kw = Some("module "),
                SyntaxKind::BAREMODULE_KW if kw.is_none() => kw = Some("baremodule "),
                SyntaxKind::END_KW => saw_end = true,
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => {}
                _ => return lower_transparent(node),
            },
        }
    }

    let (Some(kw), Some(signature), Some(block), true) = (kw, signature, block, saw_end) else {
        return lower_transparent(node);
    };
    let mut parts = vec![Ir::text(kw), lower_node(&signature)];
    let rendered = push_block_body(&mut parts, &block, || {
        if module_should_indent(node) {
            lower_block_body(&block)
        } else {
            build_block_body(&block)
        }
    });
    if !rendered {
        return lower_transparent(node);
    }
    Ir::concat(parts)
}

/// Whether a module's body is indented. A module
/// with a `module` ancestor is always indented. Otherwise it is indented only
/// when it sits at the file's top level (directly under `ROOT`) alongside at
/// least one sibling node — a lone top-level module, or a module nested inside a
/// non-module block, keeps its body flush.
fn module_should_indent(node: &SyntaxNode) -> bool {
    if node
        .ancestors()
        .skip(1)
        .any(|a| a.kind() == SyntaxKind::MODULE_DEF)
    {
        return true;
    }
    match node.parent() {
        Some(parent) if parent.kind() == SyntaxKind::ROOT => parent.children().count() > 1,
        _ => false,
    }
}

/// Lay out a `let` block (`let x = 1 … end`) by indenting its body one step,
/// matching the target style. The shape is `let [LET_BINDINGS] BLOCK end`; the
/// header is `let` plus, when present, a space and the recursively-lowered
/// binding list, and the body is lowered by [`lower_block_body`].
///
/// A **non-empty** body is always exploded to the vertical form, even when the
/// source wrote it on one line (`let x = 1; y = 2 end` → `let x = 1⏎    y = 2⏎
/// end`): the binding-from-body separator `;` opens the `BLOCK`, so the body
/// statements already live inside it. An **empty** body collapses to the
/// canonical inline `let end` (or `let x = 1 end` when the header binds), via
/// [`push_block_body`], regardless of how the source spaced it (Tenet 1).
///
/// The `LET_BINDINGS` list is lowered recursively through [`lower_comma_list`],
/// so it reflows width-driven like a bare tuple: a header that fits stays flat
/// (`let x = 1, y = 2`), and an overwide one breaks one binding per line with the
/// comma trailing each and the wrapped bindings indented one continuation step
/// beneath the first. Any shape this does not fully model — a comment in the
/// body, two statements with no separator, a missing `end`, or an unexpected
/// child — also falls back to the verbatim transparent lowering.
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

    let mut parts: Vec<Ir> = vec![Ir::text("let")];
    if let Some(bindings) = bindings {
        parts.push(Ir::text(" "));
        parts.push(lower_node(&bindings));
    }
    if !push_block_body(&mut parts, &block, || lower_block_body(&block)) {
        return lower_transparent(node);
    }
    Ir::concat(parts)
}

/// Lower a control-flow header, wrapping a boolean `CONDITION` (the `if`/`elseif`/
/// `while` predicate) or a `for` loop's `FOR_BINDING` in one extra indent step. A
/// header that overflows and breaks — an `&&`/`||`/comparison chain, a bracketed
/// predicate call, or a `for` iterable that breaks inside its parens or after an
/// operator — then sits one level *deeper* than the block body it guards (its
/// continuation at +8, the body at +4), so the header never shares an indent with
/// the body it introduces. The rule is uniform across every header shape
/// (Tenet 1): the extra indent is inert while the header fits flat, and only
/// surfaces once it breaks. A `catch` variable carries no body-boundary ambiguity
/// and lowers unchanged. (A comprehension's `for`-clause is lowered by
/// [`lower_for_binding`] directly, never through here, so it keeps the
/// comprehension indent rather than double-indenting.)
fn lower_control_header(header: &SyntaxNode) -> Ir {
    let ir = lower_node(header);
    if matches!(
        header.kind(),
        SyntaxKind::CONDITION | SyntaxKind::FOR_BINDING
    ) {
        Ir::indent(ir)
    } else {
        ir
    }
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
/// **empty** body collapses to the canonical inline `while x end`/`for i in y
/// end` via [`push_block_body`], regardless of how the source spaced it
/// (Tenet 1). Loop bodies are never `return`-inserted (only function bodies are),
/// so there is no semantic-rewrite risk. Any shape this does not fully model — a
/// body comment, a missing `end`, an unexpected child — also falls back to the
/// verbatim transparent lowering.
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

    let mut parts = vec![Ir::text(kw), Ir::text(" "), lower_control_header(&header)];
    if !push_block_body(&mut parts, &block, || lower_block_body(&block)) {
        return lower_transparent(node);
    }
    Ir::concat(parts)
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
/// (`if x; y; end` → `if x⏎    y⏎end`). An **empty** body contributes no lines
/// (via [`lower_body_allow_empty`]): a clause-less empty `if` folds inline against
/// `end` (`if x end`, the analog of `while x end`), while any empty body inside a
/// chain leaves its header line followed directly by the next clause or the shared
/// `end`. `if` bodies are never `return`-inserted (only function bodies are), so
/// there is no semantic-rewrite risk. Any unmodeled shape — a body comment, a
/// missing `end`, an unexpected child — bails to transparent.
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
    let Some(body) = lower_body_allow_empty(&block) else {
        return lower_transparent(node);
    };

    let body_empty = body.is_none();
    let mut parts: Vec<Ir> = vec![
        Ir::text("if"),
        Ir::text(" "),
        lower_control_header(&condition),
    ];
    if let Some(body) = body {
        parts.push(body);
    }
    for clause in &clauses {
        let Some(clause_ir) = lower_branch_clause(clause) else {
            return lower_transparent(node);
        };
        parts.push(clause_ir);
    }
    // A clause-less empty `if` folds inline against `end` (`if x end`), the exact
    // analog of `while x end`; any clause (or a non-empty body) stays vertical,
    // since the `end` is shared across the whole chain.
    if body_empty && clauses.is_empty() {
        parts.push(Ir::text(" end"));
    } else {
        parts.push(Ir::HardLine);
        parts.push(Ir::text("end"));
    }
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
/// **empty** body (any branch) contributes no lines (via
/// [`lower_body_allow_empty`]), leaving its keyword header followed directly by
/// the next clause or the shared `end`, so `try⏎catch⏎end` stays vertical. A valid
/// `try` always carries a clause; a clause-less one (`try end` is a syntax error)
/// bails to transparent rather than reshape into something that won't reparse.
/// `try` bodies are never `return`-inserted, so there is no semantic-rewrite risk.
/// Any unmodeled shape bails to transparent.
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
    // A valid `try` always carries a `catch`/`else`/`finally`; a clause-less one
    // is degenerate (`try end` is a syntax error), so leave it to the verbatim
    // transparent path rather than reshaping it into something that won't reparse.
    if clauses.is_empty() {
        return lower_transparent(node);
    }
    let Some(body) = lower_body_allow_empty(&block) else {
        return lower_transparent(node);
    };

    let mut parts: Vec<Ir> = vec![Ir::text("try")];
    if let Some(body) = body {
        parts.push(body);
    }
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
/// header, when present, is lowered recursively. An **empty** clause body
/// contributes no lines (via [`lower_body_allow_empty`]), leaving just the keyword
/// header. Returns `None` (the caller bails the whole construct to transparent)
/// for any unmodeled shape.
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
    let body = lower_body_allow_empty(&block)?;

    let mut parts: Vec<Ir> = vec![Ir::HardLine, Ir::text(kw)];
    if let Some(header) = header {
        parts.push(Ir::text(" "));
        parts.push(lower_control_header(&header));
    }
    // An empty clause body contributes no lines: the keyword header is followed
    // directly by the next clause or the shared `end` (Tenet 1).
    if let Some(body) = body {
        parts.push(body);
    }
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

/// Collect a container's children into source lines — zero or more `; `-joined
/// statements plus an optional trailing comment per line — shared by
/// [`build_block_body`] and [`lower_root`]. `;` and `NEWLINE` are equivalent
/// statement separators (a `;` continues the current line, a `NEWLINE` starts a
/// new one), so the source separator spelling never leaks (Tenet 1).
///
/// Comments (line `#` and block `#= … =#`) fill the current line's `comment`
/// slot: an **own-line** comment when the line has no statement yet, a
/// **trailing** comment when one precedes it. Returns `None` — so the caller
/// bails to the verbatim transparent lowering — for any shape this does not
/// model: two adjacent statement nodes with no separator, a node or `;` after a
/// comment (which would land it on the wrong side of the recorded comment), or a
/// second comment on one line.
fn collect_body_lines(node: &SyntaxNode) -> Option<Vec<BodyLine>> {
    // `expect_sep` guards against two adjacent statement nodes with no `;`/newline
    // between them, and against a node following a comment on the same line.
    let mut lines: Vec<BodyLine> = vec![BodyLine::default()];
    let mut expect_sep = false;
    collect_body_elements(node, &mut lines, &mut expect_sep)?;
    Some(lines)
}

/// Walk one container's children into the running `lines`/`expect_sep` state,
/// shared by [`collect_body_lines`] and its own recursion. Split out so the
/// `TOPLEVEL_SEMICOLON` wrapper — the single node the parser folds a top-level
/// `a; b; c` into — can be flattened in place: recursing through it feeds its
/// inner statements and `;` separators to the very same logic, so top-level
/// `;`-joins reflow one statement per line exactly as a block body's do (Tenet 1).
fn collect_body_elements(
    node: &SyntaxNode,
    lines: &mut Vec<BodyLine>,
    expect_sep: &mut bool,
) -> Option<()> {
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(child) => {
                // The `TOPLEVEL_SEMICOLON` wrapper carries no layout of its own —
                // its children are statements joined by `;` — so flatten it rather
                // than lowering it as one opaque statement.
                if child.kind() == SyntaxKind::TOPLEVEL_SEMICOLON {
                    collect_body_elements(&child, lines, expect_sep)?;
                    continue;
                }
                if *expect_sep {
                    return None;
                }
                lines.last_mut().unwrap().stmts.push(lower_node(&child));
                *expect_sep = true;
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
                    *expect_sep = false;
                }
                SyntaxKind::NEWLINE => {
                    lines.push(BodyLine::default());
                    *expect_sep = false;
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
                    *expect_sep = true;
                }
                // A block comment is preserved verbatim — its interior (including
                // continuation-line indentation) is kept byte-for-byte (only the
                // line the `#=` opens is re-indented, which the framing `HardLine`
                // supplies). Own-line or trailing, it fills the line's `comment`
                // slot exactly like a line comment; `expect_sep`/the `;`-guard then
                // bail any content that would follow it on the same line (an
                // unmodeled inline `#= … =#` mid-expression bails its owning node,
                // so it never reaches this level).
                SyntaxKind::BLOCK_COMMENT => {
                    let line = lines.last_mut().unwrap();
                    if line.comment.is_some() {
                        return None;
                    }
                    line.comment = Some(Ir::text(tok.text()));
                    *expect_sep = true;
                }
                _ => return None,
            },
        }
    }

    Some(())
}

/// Lower the document root into the canonical file layout: each top-level
/// statement on its own line, interior blank runs capped at [`MAX_BLANK_LINES`],
/// and — unlike a block body, which keeps one framing blank on each edge — **no**
/// leading or trailing blank lines, terminating with exactly one final newline.
/// Reuses [`collect_body_lines`]; any shape it rejects (a top-level construct the
/// body model does not handle) bails the whole file to the verbatim transparent
/// lowering, so unhandled syntax stays lossless.
///
/// Top-level `;`-joined statements parse into a single `TOPLEVEL_SEMICOLON` child
/// (not bare `;` tokens at the root); [`collect_body_lines`] flattens that wrapper
/// so each such statement reflows onto its own line, exactly as a block body's do.
fn lower_root(root: &SyntaxNode) -> Ir {
    let Some(lines) = collect_body_lines(root) else {
        return lower_transparent(root);
    };
    // An empty or whitespace-only file lowers to nothing.
    let Some(first) = lines.iter().position(|l| !l.is_blank()) else {
        return Ir::text("");
    };
    let last = lines.iter().rposition(|l| !l.is_blank()).unwrap();
    let content = &lines[first..=last];

    let mut inner: Vec<Ir> = Vec::new();
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
        if line.stmts.is_empty() {
            // Own-line comment: its own line, flush at column 0.
            inner.push(Ir::HardLine);
            if let Some(comment) = &line.comment {
                inner.push(comment.clone());
            }
        } else {
            let last_stmt = line.stmts.len() - 1;
            for (j, stmt) in line.stmts.iter().enumerate() {
                inner.push(Ir::HardLine);
                inner.push(stmt.clone());
                if j == last_stmt
                    && let Some(comment) = &line.comment
                {
                    inner.push(Ir::text(" "));
                    inner.push(comment.clone());
                }
            }
        }
    }
    // The first line needs no leading break — no keyword frames the file root, so
    // drop the framing `HardLine` the loop emits before it. Terminate the file
    // with exactly one newline.
    if matches!(inner.first(), Some(Ir::HardLine)) {
        inner.remove(0);
    }
    inner.push(Ir::HardLine);
    Ir::concat(inner)
}

/// Lower the statements of a `BLOCK` into an indented, vertically-broken body,
/// returning `None` (the caller bails to the transparent lowering) for an empty
/// block or any shape this does not model.
///
/// Each statement gets its own line. `;` and `NEWLINE` are equivalent statement
/// separators in a block, so `begin x; y end` and `begin x⏎y end` both reflow to
/// `⏎    x⏎    y` — the source separator spelling never leaks (Tenet 1). Each
/// statement is lowered recursively, so its own normalization still applies and a
/// nested block indents further. Blank lines are preserved
/// (capped at [`MAX_BLANK_LINES`] via an [`Ir::BlankLine`]): between statements,
/// after the keyword (leading), and before `end` (trailing) — the framing break
/// the layout always adds absorbs one newline on each side.
///
/// Comments (line `#` and block `#= … =#`) are preserved. An **own-line** comment
/// becomes its own line, re-indented to the body; a multi-line block comment keeps
/// its continuation lines verbatim (only the `#=` line takes the body indent). A
/// **trailing** comment (the line already holds a statement) is attached after it
/// with exactly one space, whatever the source's pre-`#` whitespace was (Tenet 1).
/// Two statements with no separator, a node after a comment, or any unexpected
/// token returns `None`.
fn lower_block_body(block: &SyntaxNode) -> Option<Ir> {
    build_block_body(block).map(Ir::indent)
}

/// The body engine shared by [`lower_block_body`] (which wraps the result in one
/// indent step) and the module rule (which keeps the body flush at the ambient
/// column when [`module_should_indent`] says so). Returns the
/// vertically-broken lines without any indent wrapper, or `None` for an empty
/// block or any shape this does not model.
fn build_block_body(block: &SyntaxNode) -> Option<Ir> {
    let lines = collect_body_lines(block)?;

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
        // Each statement gets its own line. Statements collected on one source line
        // (joined by `;`) reflow one-per-line just like newline-separated ones.
        if line.stmts.is_empty() {
            // Own-line comment: its own line, flush at the body indent.
            inner.push(Ir::HardLine);
            if let Some(comment) = &line.comment {
                inner.push(comment.clone());
            }
        } else {
            let last = line.stmts.len() - 1;
            for (j, stmt) in line.stmts.iter().enumerate() {
                inner.push(Ir::HardLine); // framing break / re-indent for this stmt
                inner.push(stmt.clone());
                // A trailing comment rides the final statement of the source line,
                // one canonical space after it.
                if j == last
                    && let Some(comment) = &line.comment
                {
                    inner.push(Ir::text(" "));
                    inner.push(comment.clone());
                }
            }
        }
    }
    for _ in 0..trailing_blanks {
        inner.push(Ir::BlankLine);
    }

    Some(Ir::concat(inner))
}

/// Binary operators the target style keeps tight (no surrounding spaces).
/// Everything else binary gets a space on each side.
///
/// A dotted (broadcast) operator inherits its base operator's spacing: `.+`/`.*`/
/// `.==` are spaced because `+`/`*`/`==` are, and `.^` packs tight because `^`
/// does. So four operators qualify. The *plain* `^` and the broadcast `.^`
/// (`DOT_CARET`) both pack tight (`a ^ b` → `a^b`, `a .^ b` → `a.^b`) — they share
/// the same very high precedence, tighter than unary minus on the base, which is
/// why tight framing reads correctly (`a^b + c` groups as `(a^b) + c`). The range
/// `:` in its two-operand `BINARY_EXPR` form packs tight (`a : b` → `a:b`). And the
/// field-access `.` (`a.b.c`): Julia *requires* it tight — `a . b` is a parse error
/// — so a space here would emit invalid code. The *stepped* range `a:b:c` parses as
/// a `RANGE_EXPR` handled by [`lower_range`], and the broadcast compound assignment
/// `.^=` (`DOT_CARET_EQ`) is an assignment, never routed here (assignment ops stay
/// spaced).
///
/// Note `&&`/`||` are deliberately **not** here: they canonicalize as spaced
/// (the idiomatic form), whatever the input's spacing (Tenet 1).
fn is_tight_binop(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::CARET | SyntaxKind::DOT_CARET | SyntaxKind::COLON | SyntaxKind::DOT
    )
}

/// True when snugging a following `.^` onto `operand` would retokenize. `2 .^ n`
/// packed to `2.^n` re-lexes the `2.` as the float `2.0`, and `0x1f .^ n` → `0x1f.`,
/// a hex float — silent tree changes — so a `.^` after a decimal or hexadecimal
/// integer literal stays spaced. Binary/octal integers (`0b101`, `0o17`) don't form a
/// float with a trailing `.`, and a `FLOAT`, imaginary literal, identifier, or
/// bracket-closing operand is likewise safe; the guard keys on the final token being
/// an `INTEGER` or `HEX_INT`.
fn dot_caret_snug_retokenizes(operand: &SyntaxNode) -> bool {
    operand
        .last_token()
        .is_some_and(|tok| matches!(tok.kind(), SyntaxKind::INTEGER | SyntaxKind::HEX_INT))
}
