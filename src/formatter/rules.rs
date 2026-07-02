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
        SyntaxKind::TYPE_ANNOTATION => lower_type_annotation(node),
        SyntaxKind::MATRIX_EXPR => lower_matrix(node),
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
        SyntaxKind::MACRO_CALL => lower_macro_call(node),
        SyntaxKind::TUPLE_EXPR | SyntaxKind::VECT_EXPR | SyntaxKind::BRACES => {
            lower_collection(node)
        }
        SyntaxKind::COMPREHENSION | SyntaxKind::GENERATOR | SyntaxKind::BRACES_COMPREHENSION => {
            lower_comprehension(node)
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
/// a single space on each side, except for the tight operators (`^`, `:`, `.`)
/// the target style packs without spaces. Same-precedence chains parse as a flat
/// n-ary `BINARY_EXPR` (`a + b + c` is one node with three operands), so the rule
/// walks the whole operand/operator alternation rather than assuming two operands.
///
/// **Width-driven (Tenet 1).** Source line breaks carry no layout information: the
/// chain lays out flat (`a + b + c`) when it fits `line_width`, else it breaks. The
/// break shape follows Air's model:
///
/// - **Operator-trailing.** The operator stays on the line it ends; the following
///   operand wraps. Each breakable operator gap is an [`Ir::Line`] (a space when
///   flat, a newline when broken).
/// - **One `Ir::group` per node, each with its own continuation indent.** A tighter
///   subexpression is its own node/group, so it stays flat on its line while the
///   looser enclosing chain breaks (`a +⏎ b * c +⏎ d`). When an inner subexpression
///   is *itself* forced to break, its indent nests on top of its parent's.
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
    // Assignment operators (`=`, `+=`, …) never introduce a break; the break is
    // biased into the right-hand side, so the operator gap is a flat space.
    let is_assignment = node.kind() == SyntaxKind::ASSIGNMENT_EXPR;

    let mut first: Option<Ir> = None;
    let mut rest: Vec<Ir> = Vec::new();
    // Separator to emit before the upcoming operand: `None` after a tight operator
    // (the operand abuts it), else the breakable `Ir::Line` (or a flat space for an
    // assignment operator).
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
                _ if !expect_operand => {
                    // An operator. It sits trailing on the line it ends.
                    let tight = !is_assignment && is_tight_binop(tok.kind());
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
                _ => return lower_transparent(node),
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
                _ => return lower_transparent(node),
            },
        }
    }

    let (true, true, false, Some(inner)) = (saw_lparen, saw_rparen, extra_operand, inner) else {
        return lower_transparent(node);
    };

    // Width-driven (Tenet 1): one `Ir::group` — flat `(inner)` when it fits
    // `line_width`, else the tight framing (`(` / +indent body / `)`). Source line
    // breaks never force the break; only the inner content's width (or a hard break
    // it carries, e.g. a nested block) does. Interior blank lines are already
    // dropped — the loop above skips every `NEWLINE`/`WHITESPACE` token, so only the
    // single inner node reaches the layout.
    Ir::group(Ir::concat([
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
/// - **A `;`-separated `PARAMETERS` tail** (`f(a; b = 1)`): the `;` break is not
///   yet modeled, so the list is emitted flat (single line), as before.
///
/// Any other unmodeled shape — a doubled/leading comma, two items with no comma
/// between them, an unexpected child or token — falls back to the verbatim
/// transparent lowering.
fn lower_arg_list(node: &SyntaxNode) -> Ir {
    // A comment can't be reflowed away; keep the comment-aware multiline path.
    if bracket_has_comment(node) {
        return lower_multiline_bracket(node);
    }

    let mut open: Option<String> = None;
    let mut close: Option<String> = None;
    let mut items: Vec<Ir> = Vec::new();
    // The `; …` keyword tail, if any. Its presence forces the flat layout.
    let mut params: Option<Ir> = None;
    let mut pending_comma = false;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::LPAREN | SyntaxKind::LBRACKET | SyntaxKind::LBRACE => {
                    open = Some(tok.text().to_string())
                }
                SyntaxKind::RPAREN | SyntaxKind::RBRACKET | SyntaxKind::RBRACE => {
                    close = Some(tok.text().to_string())
                }
                // Source newlines carry no layout information under Tenet 1.
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => {}
                SyntaxKind::COMMA => {
                    // A comma before any item (leading) or right after another
                    // (doubled) is not a clean list.
                    if pending_comma || items.is_empty() {
                        return lower_transparent(node);
                    }
                    pending_comma = true;
                }
                _ => return lower_transparent(node),
            },
            NodeOrToken::Node(child) => match child.kind() {
                SyntaxKind::ARG | SyntaxKind::KEYWORD_ARG => {
                    // An item after the `;` tail, or a second item with no comma
                    // between, is unmodeled.
                    if params.is_some() || (!items.is_empty() && !pending_comma) {
                        return lower_transparent(node);
                    }
                    items.push(lower_node(&child));
                    pending_comma = false;
                }
                // `;`-separated parameters attach directly (the `;` is the
                // separator), so they must not follow a comma.
                SyntaxKind::PARAMETERS => {
                    if pending_comma || params.is_some() {
                        return lower_transparent(node);
                    }
                    params = Some(lower_node(&child));
                }
                _ => return lower_transparent(node),
            },
        }
    }

    let (Some(open), Some(close)) = (open, close) else {
        return lower_transparent(node);
    };

    // The `;` tail isn't modeled as a breakable group yet: emit the flat form.
    if let Some(params) = params {
        let mut parts: Vec<Ir> = vec![Ir::text(open)];
        for (i, item) in items.into_iter().enumerate() {
            if i > 0 {
                parts.push(Ir::text(", "));
            }
            parts.push(item);
        }
        parts.push(params);
        parts.push(Ir::text(close));
        return Ir::concat(parts);
    }

    // An empty list never breaks.
    if items.is_empty() {
        return Ir::concat([Ir::text(open), Ir::text(close)]);
    }

    // A width-driven group: flat `(a, b, c)`, or one item per indented line with
    // a broken-only trailing comma when it doesn't fit.
    let mut inner: Vec<Ir> = vec![Ir::SoftLine];
    for (i, item) in items.into_iter().enumerate() {
        if i > 0 {
            inner.push(Ir::text(","));
            inner.push(Ir::Line);
        }
        inner.push(item);
    }
    inner.push(Ir::if_break(",", ""));

    Ir::group(Ir::concat([
        Ir::text(open),
        Ir::indent(Ir::concat(inner)),
        Ir::SoftLine,
        Ir::text(close),
    ]))
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

    let keep_singleton_comma = node.kind() == SyntaxKind::TUPLE_EXPR;
    let mut open: Option<String> = None;
    let mut close: Option<String> = None;
    let mut items: Vec<Ir> = Vec::new();
    let mut pending_comma = false;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::LPAREN | SyntaxKind::LBRACKET | SyntaxKind::LBRACE => {
                    open = Some(tok.text().to_string())
                }
                SyntaxKind::RPAREN | SyntaxKind::RBRACKET | SyntaxKind::RBRACE => {
                    close = Some(tok.text().to_string())
                }
                // Source newlines carry no layout information under Tenet 1.
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => {}
                SyntaxKind::COMMA => {
                    if pending_comma || items.is_empty() {
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
                    if !items.is_empty() && !pending_comma {
                        return lower_transparent(node);
                    }
                    items.push(lower_node(&child));
                    pending_comma = false;
                }
                _ => return lower_transparent(node),
            },
        }
    }

    let (Some(open), Some(close)) = (open, close) else {
        return lower_transparent(node);
    };

    // An empty collection never breaks.
    if items.is_empty() {
        return Ir::concat([Ir::text(open), Ir::text(close)]);
    }

    // The one-tuple's comma is semantic: emit it in both modes. Every other list
    // gets a trailing comma only when broken.
    let trailing = if keep_singleton_comma && items.len() == 1 {
        Ir::text(",")
    } else {
        Ir::if_break(",", "")
    };

    let mut inner: Vec<Ir> = vec![Ir::SoftLine];
    for (i, item) in items.into_iter().enumerate() {
        if i > 0 {
            inner.push(Ir::text(","));
            inner.push(Ir::Line);
        }
        inner.push(item);
    }
    inner.push(trailing);

    Ir::group(Ir::concat([
        Ir::text(open),
        Ir::indent(Ir::concat(inner)),
        Ir::SoftLine,
        Ir::text(close),
    ]))
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
    if node
        .descendants_with_tokens()
        .any(|el| matches!(el.kind(), SyntaxKind::COMMENT | SyntaxKind::BLOCK_COMMENT))
    {
        return lower_transparent(node);
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
                _ => return lower_transparent(node),
            },
            NodeOrToken::Node(child) => match child.kind() {
                SyntaxKind::FOR_BINDING => clauses.push(lower_for_binding(&child)),
                SyntaxKind::COMPREHENSION_IF => match lower_comprehension_if(&child) {
                    Some(ir) => clauses.push(ir),
                    None => return lower_transparent(node),
                },
                // The element expression precedes every clause and occurs once.
                _ => {
                    if element.is_some() || !clauses.is_empty() {
                        return lower_transparent(node);
                    }
                    element = Some(lower_node(&child));
                }
            },
        }
    }

    let (Some(open), Some(close), Some(element)) = (open, close, element) else {
        return lower_transparent(node);
    };
    if clauses.is_empty() {
        return lower_transparent(node);
    }

    let mut inner: Vec<Ir> = vec![Ir::SoftLine, element];
    for clause in clauses {
        inner.push(Ir::Line);
        inner.push(clause);
    }

    Ir::group(Ir::concat([
        Ir::text(open),
        Ir::indent(Ir::concat(inner)),
        Ir::SoftLine,
        Ir::text(close),
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
    let mut open: Option<String> = None;
    let mut close: Option<String> = None;
    let mut rows: Vec<Vec<Ir>> = vec![Vec::new()];
    let mut prev_was_semicolon = false;
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(tok) => match tok.kind() {
                SyntaxKind::LBRACKET => open = Some(tok.text().to_string()),
                SyntaxKind::RBRACKET => close = Some(tok.text().to_string()),
                // Whitespace carries no layout and is transparent to the `;;` check.
                SyntaxKind::WHITESPACE => continue,
                // `;` and a source newline both separate rows; a blank line or a
                // redundant `;`-then-newline yields an empty row that is dropped
                // below. Two adjacent `;` are the `;;` higher-dim operator, whose
                // semantics differ from `;` — bail rather than silently collapse it.
                SyntaxKind::SEMICOLON => {
                    if prev_was_semicolon {
                        return lower_transparent(node);
                    }
                    rows.push(Vec::new());
                    prev_was_semicolon = true;
                    continue;
                }
                SyntaxKind::NEWLINE => rows.push(Vec::new()),
                _ => return lower_transparent(node),
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
                            _ => return lower_transparent(node),
                        }
                    }
                }
                _ => return lower_transparent(node),
            },
        }
        prev_was_semicolon = false;
    }

    let (Some(open), Some(close)) = (open, close) else {
        return lower_transparent(node);
    };
    rows.retain(|row| !row.is_empty());
    if rows.is_empty() {
        return Ir::concat([Ir::text(open), Ir::text(close)]);
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

    Ir::group(Ir::concat([
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
                SyntaxKind::LBRACKET => open = Some(tok.text().to_string()),
                SyntaxKind::RBRACKET => close = Some(tok.text().to_string()),
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
/// indented: Runic does **not** indent a module body when the module sits alone at
/// the file's top level (the file-as-a-module-wrapper convention) or is nested
/// directly inside a non-module block. It *does* indent when the module shares the
/// top level with a sibling, or when it has a `module` ancestor (a nested module).
/// See [`module_should_indent`] for the exact predicate, which reproduces Runic's
/// `indent_toplevel`/`indent_module` decision.
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

/// Whether a module's body is indented, reproducing Runic's decision. A module
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

    let mut parts = vec![Ir::text(kw), Ir::text(" "), lower_node(&header)];
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
    let mut parts: Vec<Ir> = vec![Ir::text("if"), Ir::text(" "), lower_node(&condition)];
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
        parts.push(lower_node(&header));
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
/// with a single space — a Tenet-1 divergence: Runic preserves the user's pre-`#`
/// whitespace (≥1 space) verbatim, but Fatou canonicalizes to exactly one space.
/// Two statements with no separator, a node after a comment, or any unexpected
/// token returns `None`.
fn lower_block_body(block: &SyntaxNode) -> Option<Ir> {
    build_block_body(block).map(Ir::indent)
}

/// The body engine shared by [`lower_block_body`] (which wraps the result in one
/// indent step) and the module rule (which keeps the body flush at the ambient
/// column — Runic does not indent a module body unless the module is nested under
/// another module or shares the file's top level with a sibling). Returns the
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
