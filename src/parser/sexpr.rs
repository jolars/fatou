//! CST → JuliaSyntax.jl s-expression projector (the parser oracle).
//!
//! Walks a Fatou [`SyntaxNode`] and emits a string in the textual shape of
//! JuliaSyntax's `SyntaxNode` s-expression — the same form produced by
//! `JuliaSyntax.parseall(SyntaxNode, code)` and printed by its `show`. Exposed
//! via [`to_juliasyntax_sexpr`] and the `fatou parse --to sexpr` CLI mode; it
//! drives the differential parser harness in `tests/juliasyntax_oracle.rs`.
//!
//! **Faithful translation, not reshaping.** The projector translates only
//! *encoding* differences — Fatou's wrapper nodes (`NAME`, `LITERAL`, `ARG`,
//! `CONDITION`, `SIGNATURE`, `PAREN_EXPR`), delimiter tokens, and trivia — into
//! JuliaSyntax's surface. It never reshapes Fatou's tree *topology*: where Fatou
//! genuinely models a construct differently than JuliaSyntax (comparison chains
//! staying nested, header passthrough kept as loose tokens), the divergence is
//! emitted faithfully so the harness surfaces it (routing the case to
//! `blocked.txt`) rather than hiding it.
//!
//! Coverage is intentionally incremental. An unsupported node emits a visible
//! `(unsupported KIND)` sentinel so a gap stays loud rather than silently
//! dropping content.

use crate::syntax::{SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken};
use rowan::NodeOrToken;

use SyntaxKind::*;

/// Render the given Fatou CST as a JuliaSyntax-native s-expression string.
///
/// The root projects to `(toplevel …)`, mirroring `parseall`. Pair with
/// [`normalize_sexpr`] when comparing against captured Julia output to ignore
/// pretty-print whitespace differences.
pub fn to_juliasyntax_sexpr(tree: &SyntaxNode) -> String {
    project(tree)
}

/// Canonical form of a JuliaSyntax s-expression string. Tokenizes on whitespace
/// and parentheses (preserving `"…"` string literals as atoms) and rejoins with
/// single-space separation, so pretty-print spacing no longer affects equality.
pub fn normalize_sexpr(s: &str) -> String {
    let mut tokens: Vec<String> = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b' ' | b'\t' | b'\n' | b'\r' => i += 1,
            b'(' | b')' => {
                tokens.push((c as char).to_string());
                i += 1;
            }
            b'"' => {
                let start = i;
                i += 1;
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' if i + 1 < bytes.len() => i += 2,
                        b'"' => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
                tokens.push(s[start..i].to_string());
            }
            _ => {
                let start = i;
                while i < bytes.len() {
                    if matches!(bytes[i], b' ' | b'\t' | b'\n' | b'\r' | b'(' | b')' | b'"') {
                        break;
                    }
                    i += 1;
                }
                if i > start {
                    tokens.push(s[start..i].to_string());
                }
            }
        }
    }
    tokens.join(" ")
}

// --- Core dispatch ---------------------------------------------------------

fn project(node: &SyntaxNode) -> String {
    match node.kind() {
        ROOT => sexp("toplevel", stmt_strings(node)),
        BLOCK => sexp("block", stmt_strings(node)),
        // `begin … end` wraps a `BLOCK`; project that directly so it lowers to a
        // single `(block …)` rather than a doubled `(block (block …))`.
        BEGIN_EXPR => project_block_child(node),

        NAME => name_text(node),
        LITERAL => project_literal(node),
        STRING_LITERAL => project_string(node),
        CMD_LITERAL => project_cmd(node),
        INTERPOLATION => project_interpolation(node),

        PAREN_EXPR | CONDITION => match first_node(node) {
            Some(inner) => project(&inner),
            None => "(block)".to_string(),
        },

        BINARY_EXPR => project_binary(node),
        ASSIGNMENT_EXPR => project_assignment(node),
        UNARY_EXPR => project_unary(node),
        POSTFIX_EXPR => project_postfix(node),
        SPLAT_EXPR => sexp("...", vec![project_first(node)]),
        TYPE_ANNOTATION => project_type_annotation(node),
        WHERE_EXPR => project_where(node),
        ARROW_EXPR => sexp("->", project_each(child_nodes(node))),
        TERNARY_EXPR => sexp("?", project_each(child_nodes(node))),

        CALL_EXPR => project_call("call", node),
        INDEX_EXPR => project_call("ref", node),
        CURLY_EXPR => project_call("curly", node),
        DOT_CALL_EXPR => project_call("dotcall", node),
        BRACES => sexp("braces", project_args(node)),

        TUPLE_EXPR => sexp("tuple-p", project_args(node)),
        VECT_EXPR => sexp("vect", project_args(node)),
        MATRIX_EXPR => project_matrix(node),

        COMPREHENSION => sexp("comprehension", vec![project_generator(node)]),
        GENERATOR => project_generator(node),

        IF_EXPR => project_if(node),
        WHILE_EXPR => sexp("while", project_each(child_nodes(node))),
        FOR_EXPR => sexp(
            "for",
            vec![project_for_binding(node), project_block_child(node)],
        ),
        FUNCTION_DEF => sexp(
            "function",
            vec![project_signature(node), project_block_child(node)],
        ),
        LET_EXPR => project_let(node),
        QUOTE_EXPR => sexp("quote", vec![project_block_child(node)]),
        QUOTE_SYM => project_quote_sym(node),
        TRY_EXPR => project_try(node),
        STRUCT_DEF => project_struct(node),
        MODULE_DEF => project_module(node),
        DO_EXPR => project_do(node),

        RETURN_EXPR => project_keyword_stmt("return", node),
        BREAK_EXPR => "(break)".to_string(),
        CONTINUE_EXPR => "(continue)".to_string(),
        CONST_STMT => project_decl("const", node),
        GLOBAL_STMT => project_decl("global", node),
        LOCAL_STMT => project_decl("local", node),
        IMPORT_STMT => project_import("import", node),
        USING_STMT => project_import("using", node),
        EXPORT_STMT => project_export(node),
        IMPORT_PATH => project_import_path(node),
        IMPORT_ALIAS => project_import_alias(node),

        MACRO_CALL => project_macrocall(node),

        END_MARKER => "end".to_string(),
        BEGIN_MARKER => "begin".to_string(),

        other => format!("(unsupported {other:?})"),
    }
}

// --- Operator tables -------------------------------------------------------

/// How a `BINARY_EXPR`/`ASSIGNMENT_EXPR` operator token projects.
enum InfixHead {
    /// `(call-i lhs OP rhs)` — ordinary infix operator (OP is the source text).
    CallI(&'static str),
    /// `(OP lhs rhs)` — operator is its own head (`&&`, `||`, `<:`, `>:`, `=`).
    Special(&'static str),
    /// `(. lhs (quote rhs))` — field access.
    Dot,
    /// `(dotcall-i lhs OP rhs)` — broadcast infix (OP is the *undotted* text).
    DotCallI(&'static str),
}

fn infix_head(kind: SyntaxKind) -> InfixHead {
    use InfixHead::*;
    match kind {
        PLUS => CallI("+"),
        MINUS => CallI("-"),
        STAR => CallI("*"),
        SLASH => CallI("/"),
        SLASH_SLASH => CallI("//"),
        CARET => CallI("^"),
        PERCENT => CallI("%"),
        COLON => CallI(":"),
        DOT_DOT => CallI(".."),
        FAT_ARROW => CallI("=>"),
        PIPE_GT => CallI("|>"),
        AMP => CallI("&"),
        PIPE => CallI("|"),
        EQ_EQ => CallI("=="),
        NOT_EQ => CallI("!="),
        LT => CallI("<"),
        LE => CallI("<="),
        GT => CallI(">"),
        GE => CallI(">="),
        TILDE => CallI("~"),

        AND_AND => Special("&&"),
        OR_OR => Special("||"),
        DOT_AND_AND => Special(".&&"),
        DOT_OR_OR => Special(".||"),
        SUBTYPE => Special("<:"),
        SUPERTYPE => Special(">:"),
        EQ => Special("="),
        DOT_EQ => Special(".="),

        DOT => Dot,

        DOT_PLUS => DotCallI("+"),
        DOT_MINUS => DotCallI("-"),
        DOT_STAR => DotCallI("*"),
        DOT_SLASH => DotCallI("/"),
        DOT_SLASH_SLASH => DotCallI("//"),
        DOT_CARET => DotCallI("^"),
        DOT_PERCENT => DotCallI("%"),
        DOT_TILDE => DotCallI("~"),
        DOT_EQ_EQ => DotCallI("=="),
        DOT_NOT_EQ => DotCallI("!="),
        DOT_LT => DotCallI("<"),
        DOT_LE => DotCallI("<="),
        DOT_GT => DotCallI(">"),
        DOT_GE => DotCallI(">="),
        DOT_FAT_ARROW => DotCallI("=>"),

        // Fallback: treat as an ordinary infix call using the raw text. Leaked
        // in faithfully so an unmapped operator surfaces as a divergence.
        _ => CallI("?"),
    }
}

fn is_operator(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        EQ | PLUS
            | MINUS
            | STAR
            | SLASH
            | SLASH_SLASH
            | CARET
            | PERCENT
            | EQ_EQ
            | NOT_EQ
            | LT
            | LE
            | GT
            | GE
            | AND_AND
            | OR_OR
            | DOT_AND_AND
            | DOT_OR_OR
            | COLON
            | DOT_DOT
            | COLON_COLON
            | TILDE
            | DOT_TILDE
            | SUBTYPE
            | SUPERTYPE
            | ARROW
            | FAT_ARROW
            | DOT
            | PIPE_GT
            | BANG
            | AMP
            | PIPE
            | DOT_PLUS
            | DOT_MINUS
            | DOT_STAR
            | DOT_SLASH
            | DOT_SLASH_SLASH
            | DOT_CARET
            | DOT_PERCENT
            | DOT_EQ
            | DOT_EQ_EQ
            | DOT_NOT_EQ
            | DOT_LT
            | DOT_LE
            | DOT_GT
            | DOT_GE
            | DOT_FAT_ARROW
            | PLUS_EQ
            | MINUS_EQ
            | STAR_EQ
            | SLASH_EQ
            | SLASH_SLASH_EQ
            | CARET_EQ
            | PERCENT_EQ
            | PIPE_EQ
            | AMP_EQ
            | DOT_PLUS_EQ
            | DOT_MINUS_EQ
            | DOT_STAR_EQ
            | DOT_SLASH_EQ
            | DOT_SLASH_SLASH_EQ
            | DOT_CARET_EQ
            | DOT_PERCENT_EQ
    )
}

// --- Binary / unary / assignment -------------------------------------------

fn project_binary(node: &SyntaxNode) -> String {
    let op = match operator_token(node) {
        Some(t) => t,
        None => return format!("(unsupported {:?})", node.kind()),
    };
    let operands = child_nodes(node);
    if operands.len() != 2 {
        return project_flat(significant(node));
    }
    let lhs = project(&operands[0]);
    let rhs = &operands[1];
    match infix_head(op.kind()) {
        InfixHead::CallI(text) => format!("(call-i {lhs} {text} {})", project(rhs)),
        InfixHead::Special(text) => format!("({text} {lhs} {})", project(rhs)),
        InfixHead::DotCallI(text) => format!("(dotcall-i {lhs} {text} {})", project(rhs)),
        InfixHead::Dot => format!("(. {lhs} (quote {}))", name_text(rhs)),
    }
}

fn project_assignment(node: &SyntaxNode) -> String {
    // The operator's own text is its JuliaSyntax head verbatim: `=`, `.=`, `+=`,
    // `.+=`, … all project as `(<op> lhs rhs)`.
    let head = match operator_token(node) {
        Some(t) => t.text().to_string(),
        None => "=".to_string(),
    };
    sexp(&head, project_each(child_nodes(node)))
}

fn project_unary(node: &SyntaxNode) -> String {
    let op = match operator_token(node) {
        Some(t) => t,
        None => return format!("(unsupported {:?})", node.kind()),
    };
    let operand = project_first(node);
    match op.kind() {
        SUBTYPE => format!("(<:-pre {operand})"),
        SUPERTYPE => format!("(>:-pre {operand})"),
        DOT_PLUS => format!("(dotcall-pre + {operand})"),
        DOT_MINUS => format!("(dotcall-pre - {operand})"),
        DOT_TILDE => format!("(dotcall-pre ~ {operand})"),
        _ => format!("(call-pre {} {operand})", op.text()),
    }
}

fn project_postfix(node: &SyntaxNode) -> String {
    // `A'` → `(call-post A ')`. The postfix token text is the operator (`'`).
    let operand = project_first(node);
    let op = significant(node)
        .into_iter()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == TRANSPOSE)
        .map(|t| t.text().to_string())
        .unwrap_or_else(|| "'".to_string());
    format!("(call-post {operand} {op})")
}

fn project_type_annotation(node: &SyntaxNode) -> String {
    let operands = child_nodes(node);
    match operands.len() {
        2 => format!("(::-i {} {})", project(&operands[0]), project(&operands[1])),
        1 => format!("(::-pre {})", project(&operands[0])),
        _ => project_flat(significant(node)),
    }
}

fn project_where(node: &SyntaxNode) -> String {
    let nodes = child_nodes(node);
    if nodes.is_empty() {
        return "(unsupported WHERE_EXPR)".to_string();
    }
    let mut parts = vec![project(&nodes[0])];
    parts.extend(nodes[1..].iter().map(project));
    sexp("where", parts)
}

// --- Calls / args ----------------------------------------------------------

fn project_call(head: &str, node: &SyntaxNode) -> String {
    let mut parts = Vec::new();
    let mut children = node.children();
    if let Some(callee) = children.next() {
        parts.push(project(&callee));
    }
    if let Some(arg_list) = node.children().find(|c| c.kind() == ARG_LIST) {
        parts.extend(project_args(&arg_list));
    }
    sexp(head, parts)
}

/// Project the argument-like direct children of `container` (an `ARG_LIST`,
/// `TUPLE_EXPR`, `VECT_EXPR`, `BRACES`, or `MATRIX_ROW`): unwrap `ARG`, turn
/// `KEYWORD_ARG` into `(= name val)`, `PARAMETERS` into `(parameters …)`, and
/// pass `end` tokens through.
fn project_args(container: &SyntaxNode) -> Vec<String> {
    let mut out = Vec::new();
    for el in significant(container) {
        match el {
            NodeOrToken::Node(n) => match n.kind() {
                ARG => out.push(project_first(&n)),
                KEYWORD_ARG => out.push(project_keyword_arg(&n)),
                PARAMETERS => out.push(project_parameters(&n)),
                _ => out.push(project(&n)),
            },
            NodeOrToken::Token(t) => {
                if t.kind() == END_KW {
                    out.push("end".to_string());
                }
            }
        }
    }
    out
}

fn project_keyword_arg(node: &SyntaxNode) -> String {
    let nodes = child_nodes(node);
    match nodes.as_slice() {
        [name, value] => format!("(= {} {})", project(name), project(value)),
        [name] => format!("(= {})", project(name)),
        _ => project_flat(significant(node)),
    }
}

fn project_parameters(node: &SyntaxNode) -> String {
    let mut out = Vec::new();
    for el in significant(node) {
        if let NodeOrToken::Node(n) = el {
            match n.kind() {
                ARG => out.push(project_first(&n)),
                KEYWORD_ARG => out.push(project_keyword_arg(&n)),
                _ => out.push(project(&n)),
            }
        }
    }
    sexp("parameters", out)
}

// --- Matrices --------------------------------------------------------------

fn project_matrix(node: &SyntaxNode) -> String {
    let rows: Vec<SyntaxNode> = node.children().filter(|c| c.kind() == MATRIX_ROW).collect();
    if rows.len() == 1 {
        // Single row of space-separated columns → hcat.
        return sexp("hcat", project_args(&rows[0]));
    }
    // Multiple rows → vcat; single-element rows are emitted unwrapped, matching
    // JuliaSyntax (`[1; 2]` → `(vcat 1 2)`, `[1 2; 3 4]` → `(vcat (row …) …)`).
    let items = rows
        .iter()
        .map(|row| {
            let elems = project_args(row);
            if elems.len() == 1 {
                elems.into_iter().next().unwrap()
            } else {
                sexp("row", elems)
            }
        })
        .collect();
    sexp("vcat", items)
}

// --- Comprehensions / generators -------------------------------------------

fn project_generator(node: &SyntaxNode) -> String {
    // Fatou is flat: `body FOR_BINDING… [COMPREHENSION_IF]`. JuliaSyntax nests
    // `(generator body (= v it) …)` and folds a trailing `if` into a `filter`.
    let mut body = String::new();
    let mut bindings = Vec::new();
    let mut filter: Option<String> = None;
    for child in node.children() {
        match child.kind() {
            FOR_BINDING => bindings.push(project_for_binding_node(&child)),
            COMPREHENSION_IF => {
                if let Some(cond) = first_node(&child) {
                    filter = Some(project(&cond));
                }
            }
            _ if body.is_empty() => body = project(&child),
            _ => {}
        }
    }
    let mut parts = vec![body];
    if let (Some(cond), Some(last)) = (filter, bindings.last().cloned()) {
        let n = bindings.len();
        bindings[n - 1] = format!("(filter {last} {cond})");
    }
    parts.extend(bindings);
    sexp("generator", parts)
}

// --- For binding -----------------------------------------------------------

fn project_for_binding(node: &SyntaxNode) -> String {
    match node.children().find(|c| c.kind() == FOR_BINDING) {
        Some(binding) => project_for_binding_node(&binding),
        None => "(unsupported FOR_BINDING)".to_string(),
    }
}

fn project_for_binding_node(binding: &SyntaxNode) -> String {
    // `for j = 1:3` keeps a proper ASSIGNMENT_EXPR; `for i in xs` keeps the
    // iterator as loose passthrough tokens after an `in` keyword-identifier.
    if let Some(assign) = binding.children().find(|c| c.kind() == ASSIGNMENT_EXPR) {
        return project(&assign);
    }
    let elems = significant(binding);
    let split = elems
        .iter()
        .position(|el| matches!(el, NodeOrToken::Token(t) if t.text() == "in" || t.text() == "∈"));
    match split {
        Some(idx) => {
            let var = project_flat(elems[..idx].to_vec());
            let iter = project_flat(elems[idx + 1..].to_vec());
            format!("(= {var} {iter})")
        }
        None => project_flat(elems),
    }
}

// --- Control flow ----------------------------------------------------------

fn project_if(node: &SyntaxNode) -> String {
    let cond = node
        .children()
        .find(|c| c.kind() == CONDITION)
        .map(|c| project(&c))
        .unwrap_or_default();
    let then_block = node
        .children()
        .find(|c| c.kind() == BLOCK)
        .map(|c| project(&c))
        .unwrap_or_else(|| "(block)".to_string());
    let clauses: Vec<SyntaxNode> = node
        .children()
        .filter(|c| matches!(c.kind(), ELSEIF_CLAUSE | ELSE_CLAUSE))
        .collect();
    let mut parts = vec![cond, then_block];
    if let Some(tail) = project_if_tail(&clauses) {
        parts.push(tail);
    }
    sexp("if", parts)
}

fn project_if_tail(clauses: &[SyntaxNode]) -> Option<String> {
    let first = clauses.first()?;
    match first.kind() {
        ELSE_CLAUSE => first
            .children()
            .find(|c| c.kind() == BLOCK)
            .map(|c| project(&c)),
        ELSEIF_CLAUSE => {
            let cond = first
                .children()
                .find(|c| c.kind() == CONDITION)
                .map(|c| project(&c))
                .unwrap_or_default();
            let block = first
                .children()
                .find(|c| c.kind() == BLOCK)
                .map(|c| project(&c))
                .unwrap_or_else(|| "(block)".to_string());
            let mut parts = vec![cond, block];
            if let Some(tail) = project_if_tail(&clauses[1..]) {
                parts.push(tail);
            }
            Some(sexp("elseif", parts))
        }
        _ => None,
    }
}

fn project_try(node: &SyntaxNode) -> String {
    let mut parts = Vec::new();
    parts.push(project_block_child(node));
    for clause in node.children() {
        match clause.kind() {
            CATCH_CLAUSE => {
                let var = clause
                    .children()
                    .find(|c| c.kind() == NAME)
                    .map(|c| project(&c))
                    .unwrap_or_else(|| "false".to_string());
                let block = project_block_child(&clause);
                parts.push(format!("(catch {var} {block})"));
            }
            FINALLY_CLAUSE => parts.push(format!("(finally {})", project_block_child(&clause))),
            ELSE_CLAUSE => parts.push(format!("(else {})", project_block_child(&clause))),
            _ => {}
        }
    }
    sexp("try", parts)
}

fn project_struct(node: &SyntaxNode) -> String {
    let mutable = node
        .children_with_tokens()
        .any(|el| el.kind() == MUTABLE_KW);
    let head = if mutable { "struct-mut" } else { "struct" };
    sexp(
        head,
        vec![project_signature(node), project_block_child(node)],
    )
}

fn project_module(node: &SyntaxNode) -> String {
    let bare = node
        .children_with_tokens()
        .any(|el| el.kind() == BAREMODULE_KW);
    let head = if bare { "module-bare" } else { "module" };
    sexp(
        head,
        vec![project_signature(node), project_block_child(node)],
    )
}

fn project_quote_sym(node: &SyntaxNode) -> String {
    // `:foo`/`:(expr)` → `(quote-: …)`. The quoted form is the first significant
    // child after the `:` — a `NAME`/paren node, or a bare keyword token.
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(n) => return format!("(quote-: {})", project(&n)),
            NodeOrToken::Token(t) if t.kind() == COLON || is_trivia(t.kind()) => continue,
            NodeOrToken::Token(t) => return format!("(quote-: {})", t.text()),
        }
    }
    "(quote-:)".to_string()
}

fn project_let(node: &SyntaxNode) -> String {
    let bindings = match node.children().find(|c| c.kind() == LET_BINDINGS) {
        Some(b) => sexp("block", project_let_bindings(&b)),
        None => "(block)".to_string(),
    };
    sexp("let", vec![bindings, project_block_child(node)])
}

fn project_let_bindings(node: &SyntaxNode) -> Vec<String> {
    // Comma-separated bindings; Fatou keeps the first as an `ASSIGNMENT_EXPR`
    // and any later one as loose `IDENT = expr` tokens (header passthrough).
    let mut out = Vec::new();
    let mut pending: Vec<SyntaxElement> = Vec::new();
    let flush = |pending: &mut Vec<SyntaxElement>, out: &mut Vec<String>| {
        if !pending.is_empty() {
            out.push(project_flat(std::mem::take(pending)));
        }
    };
    // The `,` separators are load-bearing, so iterate raw children (dropping
    // only trivia) rather than via `significant`, which would strip them.
    for el in node.children_with_tokens() {
        match &el {
            NodeOrToken::Token(t) if is_trivia(t.kind()) => {}
            NodeOrToken::Token(t) if t.kind() == COMMA => flush(&mut pending, &mut out),
            _ => pending.push(el),
        }
    }
    flush(&mut pending, &mut out);
    out
}

fn project_do(node: &SyntaxNode) -> String {
    let call = node
        .children()
        .next()
        .map(|c| project(&c))
        .unwrap_or_default();
    let params = match node.children().find(|c| c.kind() == DO_PARAMS) {
        Some(p) => sexp("tuple", do_param_strings(&p)),
        None => "(tuple)".to_string(),
    };
    let block = project_block_child(node);
    sexp("do", vec![call, params, block])
}

fn do_param_strings(node: &SyntaxNode) -> Vec<String> {
    significant(node)
        .into_iter()
        .filter_map(|el| match el {
            NodeOrToken::Node(n) => Some(project(&n)),
            NodeOrToken::Token(t) if t.kind() == IDENT => Some(t.text().to_string()),
            NodeOrToken::Token(_) => None,
        })
        .collect()
}

// --- Statements / declarations ---------------------------------------------

fn project_keyword_stmt(head: &str, node: &SyntaxNode) -> String {
    match first_node(node) {
        Some(inner) => format!("({head} {})", project(&inner)),
        None => format!("({head})"),
    }
}

fn project_decl(head: &str, node: &SyntaxNode) -> String {
    // `const x = 1` / `local y = 2` wrap a single assignment; `global a, b`
    // carries a bare name list. Both fall out of collecting every operand.
    let items: Vec<String> = significant(node)
        .into_iter()
        .filter_map(|el| match el {
            NodeOrToken::Node(n) => Some(project(&n)),
            NodeOrToken::Token(t) if t.kind() == IDENT => Some(t.text().to_string()),
            NodeOrToken::Token(_) => None,
        })
        .collect();
    sexp(head, items)
}

fn project_export(node: &SyntaxNode) -> String {
    let items: Vec<String> = ident_run(node);
    sexp("export", items)
}

fn project_import(head: &str, node: &SyntaxNode) -> String {
    // `import A` / `using A.B` / `import A: b, c as d`. The path tree is built by
    // the parser: each clause is an `IMPORT_PATH` or `IMPORT_ALIAS` node, and a
    // top-level `:` token (when present) splits the base path from the list of
    // imported names. Read those nodes directly.
    let has_colon = node.children_with_tokens().any(|el| el.kind() == COLON);
    let clauses: Vec<String> = node
        .children()
        .filter(|c| matches!(c.kind(), IMPORT_PATH | IMPORT_ALIAS))
        .map(|c| project(&c))
        .collect();

    if has_colon && !clauses.is_empty() {
        // `(: <base> <name> …)` — the first clause is the base path.
        format!("({head} {})", sexp(":", clauses))
    } else {
        format!("({head} {})", clauses.join(" "))
    }
}

/// `(importpath . . A B)` — leading relative dots (each `.`/`..`/`...` token
/// expands to one dot per character) followed by the dotted name components. The
/// dots that *separate* name components carry no meaning in JuliaSyntax's shape,
/// so only the leading dots (before the first name) are emitted.
fn project_import_path(node: &SyntaxNode) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut seen_name = false;
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) => match t.kind() {
                DOT if !seen_name => parts.push(".".to_string()),
                DOT_DOT if !seen_name => parts.extend([".".to_string(), ".".to_string()]),
                DOT_DOT_DOT if !seen_name => {
                    parts.extend([".".to_string(), ".".to_string(), ".".to_string()])
                }
                IDENT => {
                    parts.push(t.text().to_string());
                    seen_name = true;
                }
                _ => {}
            },
            NodeOrToken::Node(n) if n.kind() == NAME => {
                parts.push(name_text(&n));
                seen_name = true;
            }
            _ => {}
        }
    }
    sexp("importpath", parts)
}

/// `(as (importpath …) <name>)` — an `as` rename wrapping an import path.
fn project_import_alias(node: &SyntaxNode) -> String {
    let path = node
        .children()
        .find(|c| c.kind() == IMPORT_PATH)
        .map(|c| project_import_path(&c))
        .unwrap_or_default();
    // The alias is the bare identifier after the `as` keyword (the path's own
    // identifiers are nested inside the `IMPORT_PATH` child, not direct tokens).
    let alias = node
        .children_with_tokens()
        .filter_map(|el| match el {
            NodeOrToken::Token(t) if t.kind() == IDENT && t.text() != "as" => {
                Some(t.text().to_string())
            }
            _ => None,
        })
        .last()
        .unwrap_or_default();
    format!("(as {path} {alias})")
}

// --- Macros ----------------------------------------------------------------

fn project_macrocall(node: &SyntaxNode) -> String {
    let name = node
        .children()
        .find(|c| c.kind() == MACRO_NAME)
        .map(|c| project_macro_name(&c))
        .unwrap_or_else(|| "@?".to_string());

    // Paren form `@m(…)` carries a direct `ARG_LIST` child → `macrocall-p`;
    // the space form `@m a b` carries bare argument nodes → `macrocall`.
    if let Some(arg_list) = node.children().find(|c| c.kind() == ARG_LIST) {
        let mut parts = vec![name];
        parts.extend(project_args(&arg_list));
        return sexp("macrocall-p", parts);
    }
    let mut parts = vec![name];
    parts.extend(
        node.children()
            .filter(|c| c.kind() != MACRO_NAME)
            .map(|c| project(&c)),
    );
    sexp("macrocall", parts)
}

fn project_macro_name(node: &SyntaxNode) -> String {
    // Collect identifier components in order (module path + macro name), noting
    // whether a qualifying `.` is present.
    let idents: Vec<String> = node
        .children_with_tokens()
        .filter_map(|el| match el {
            NodeOrToken::Token(t) if t.kind() == IDENT => Some(t.text().to_string()),
            NodeOrToken::Node(n) if n.kind() == NAME => Some(name_text(&n)),
            _ => None,
        })
        .collect();
    let has_dot = node.children_with_tokens().any(|el| el.kind() == DOT);

    match idents.as_slice() {
        // `@.` — broadcast macro: `@` then the lone broadcast dot, no ident.
        [] => "@.".to_string(),
        // Simple `@m`.
        [one] if !has_dot => format!("@{one}"),
        // Qualified `Base.@time` / `@Mod.mac` → `(. <module> (quote @macro))`.
        rest => {
            let (macro_name, module) = rest.split_last().unwrap();
            let module_path = module.join(".");
            format!("(. {module_path} (quote @{macro_name}))")
        }
    }
}

// --- Literals / strings ----------------------------------------------------

fn project_literal(node: &SyntaxNode) -> String {
    match node.children_with_tokens().find_map(|el| el.into_token()) {
        Some(tok) => match tok.kind() {
            CHAR => format!("(char {})", tok.text()),
            TRUE_KW => "true".to_string(),
            FALSE_KW => "false".to_string(),
            _ => tok.text().to_string(),
        },
        None => "(unsupported LITERAL)".to_string(),
    }
}

fn project_string(node: &SyntaxNode) -> String {
    // String macro: a prefix (`r`, `raw`, `b`, `v`) makes it a raw `@<p>_str`
    // macrocall rather than an interpolating `(string …)`.
    if let Some(prefix) = string_token(node, STRING_PREFIX) {
        let body = format!("(string-r {})", quote_raw(&raw_content(node)));
        let mut parts = vec![format!("@{prefix}_str"), body];
        if let Some(suffix) = string_token(node, STRING_SUFFIX) {
            parts.push(quote_raw(&suffix));
        }
        return sexp("macrocall", parts);
    }

    let head = match string_token(node, STRING_DELIM_OPEN) {
        Some(d) if d.len() >= 3 => "string-s",
        _ => "string",
    };
    sexp(head, string_parts(node))
}

fn project_cmd(node: &SyntaxNode) -> String {
    // Command literals lower to a `core_@cmd` macrocall over a raw cmdstring.
    // Commands are raw: JuliaSyntax keeps `$`-interpolation as literal source
    // (escaped `\$`) and defers expansion to the macro, so reconstruct the raw
    // body from both content tokens and interpolation source text.
    let triple = matches!(string_token(node, CMD_DELIM_OPEN), Some(d) if d.len() >= 3);
    let head = if triple {
        "cmdstring-s-r"
    } else {
        "cmdstring-r"
    };
    let body = format!("({head} {})", quote_raw(&cmd_raw_body(node)));
    sexp("macrocall", vec!["core_@cmd".to_string(), body])
}

fn cmd_raw_body(node: &SyntaxNode) -> String {
    let mut body = String::new();
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if t.kind() == STRING_CONTENT => body.push_str(t.text()),
            NodeOrToken::Node(n) if n.kind() == INTERPOLATION => {
                body.push_str(&n.text().to_string())
            }
            _ => {}
        }
    }
    body
}

fn string_parts(node: &SyntaxNode) -> Vec<String> {
    let mut parts = Vec::new();
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if t.kind() == STRING_CONTENT => {
                parts.push(format!("\"{}\"", t.text()));
            }
            NodeOrToken::Node(n) if n.kind() == INTERPOLATION => {
                parts.push(project_interpolation(&n));
            }
            _ => {}
        }
    }
    parts
}

fn project_interpolation(node: &SyntaxNode) -> String {
    // `$name` → the bare identifier; `$(expr)` → the projected sub-expression.
    if let Some(inner) = first_node(node) {
        return project(&inner);
    }
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == IDENT)
        .map(|t| t.text().to_string())
        .unwrap_or_default()
}

fn raw_content(node: &SyntaxNode) -> String {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| t.kind() == STRING_CONTENT)
        .map(|t| t.text().to_string())
        .collect()
}

fn string_token(node: &SyntaxNode, kind: SyntaxKind) -> Option<String> {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == kind)
        .map(|t| t.text().to_string())
}

/// Escape a raw string/command body for display the way JuliaSyntax's `show`
/// does: backslashes, double-quotes, and `$` are escaped.
fn quote_raw(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '$' => out.push_str("\\$"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

// --- Signatures ------------------------------------------------------------

fn project_signature(node: &SyntaxNode) -> String {
    match node.children().find(|c| c.kind() == SIGNATURE) {
        Some(sig) => match first_node(&sig) {
            Some(inner) => project(&inner),
            // Bare or loose signature (`struct Point`, `struct Foo <: Bar`).
            None => project_flat(significant(&sig)),
        },
        // Some forms (`module M`) put the name directly; fall back to loose.
        None => project_flat(significant(node)),
    }
}

// --- Generic helpers -------------------------------------------------------

fn sexp(head: &str, parts: Vec<String>) -> String {
    if parts.is_empty() {
        format!("({head})")
    } else {
        format!("({head} {})", parts.join(" "))
    }
}

/// Project the statement nodes of a `ROOT`/`BLOCK`/`BEGIN_EXPR` container.
fn stmt_strings(node: &SyntaxNode) -> Vec<String> {
    child_nodes(node).iter().map(project).collect()
}

/// The `BLOCK` child of a block-bearing construct, projected (empty if absent).
fn project_block_child(node: &SyntaxNode) -> String {
    node.children()
        .find(|c| c.kind() == BLOCK)
        .map(|c| project(&c))
        .unwrap_or_else(|| "(block)".to_string())
}

fn child_nodes(node: &SyntaxNode) -> Vec<SyntaxNode> {
    node.children().collect()
}

fn project_each(nodes: Vec<SyntaxNode>) -> Vec<String> {
    nodes.iter().map(project).collect()
}

fn first_node(node: &SyntaxNode) -> Option<SyntaxNode> {
    node.children().next()
}

fn project_first(node: &SyntaxNode) -> String {
    first_node(node).map(|n| project(&n)).unwrap_or_default()
}

fn name_text(node: &SyntaxNode) -> String {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == IDENT)
        .map(|t| t.text().to_string())
        .unwrap_or_default()
}

fn operator_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| is_operator(t.kind()))
}

fn ident_run(node: &SyntaxNode) -> Vec<String> {
    significant(node)
        .into_iter()
        .filter_map(|el| match el {
            NodeOrToken::Token(t) if t.kind() == IDENT => Some(t.text().to_string()),
            NodeOrToken::Node(n) if n.kind() == NAME => Some(name_text(&n)),
            _ => None,
        })
        .collect()
}

/// Project a flat sequence of significant elements representing a simple
/// (single-operator) expression. Used for the loose-token header passthrough
/// Fatou keeps for some constructs (for-loop ranges, struct subtypes, later
/// `let` bindings). Multi-operator runs fall back to a space join, which
/// diverges from JuliaSyntax and routes the case to `blocked.txt`.
fn project_flat(elems: Vec<SyntaxElement>) -> String {
    match elems.as_slice() {
        [one] => project_element(one).unwrap_or_default(),
        [lhs, NodeOrToken::Token(op), rhs] if is_operator(op.kind()) => {
            let l = project_element(lhs).unwrap_or_default();
            let r = project_element(rhs).unwrap_or_default();
            match infix_head(op.kind()) {
                InfixHead::CallI(text) => format!("(call-i {l} {text} {r})"),
                InfixHead::Special(text) => format!("({text} {l} {r})"),
                InfixHead::DotCallI(text) => format!("(dotcall-i {l} {text} {r})"),
                InfixHead::Dot => format!("(. {l} (quote {r}))"),
            }
        }
        _ => elems
            .iter()
            .filter_map(project_element)
            .collect::<Vec<_>>()
            .join(" "),
    }
}

fn project_element(el: &SyntaxElement) -> Option<String> {
    match el {
        NodeOrToken::Node(n) => Some(project(n)),
        NodeOrToken::Token(t) => match t.kind() {
            IDENT | INTEGER | BIN_INT | OCT_INT | HEX_INT | FLOAT | FLOAT32 => {
                Some(t.text().to_string())
            }
            CHAR => Some(format!("(char {})", t.text())),
            TRUE_KW => Some("true".to_string()),
            FALSE_KW => Some("false".to_string()),
            END_KW => Some("end".to_string()),
            k if is_operator(k) => Some(t.text().to_string()),
            _ => None,
        },
    }
}

/// The significant children of `node`: nodes, plus tokens that are neither
/// trivia, structural delimiters, nor keywords. Operators, identifiers, and
/// literal tokens survive.
fn significant(node: &SyntaxNode) -> Vec<SyntaxElement> {
    node.children_with_tokens()
        .filter(|el| match el {
            NodeOrToken::Node(_) => true,
            NodeOrToken::Token(t) => !is_drop_token(t.kind()),
        })
        .collect()
}

fn is_drop_token(kind: SyntaxKind) -> bool {
    is_trivia(kind) || is_delimiter(kind) || is_keyword(kind) || kind == DOLLAR
}

fn is_trivia(kind: SyntaxKind) -> bool {
    matches!(kind, WHITESPACE | NEWLINE | COMMENT | BLOCK_COMMENT)
}

fn is_delimiter(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        LPAREN | RPAREN | LBRACKET | RBRACKET | LBRACE | RBRACE | COMMA | SEMICOLON | AT
    )
}

fn is_keyword(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        FUNCTION_KW
            | END_KW
            | IF_KW
            | ELSEIF_KW
            | ELSE_KW
            | BEGIN_KW
            | WHILE_KW
            | FOR_KW
            | LET_KW
            | QUOTE_KW
            | TRY_KW
            | CATCH_KW
            | FINALLY_KW
            | STRUCT_KW
            | MUTABLE_KW
            | MODULE_KW
            | BAREMODULE_KW
            | DO_KW
            | RETURN_KW
            | BREAK_KW
            | CONTINUE_KW
            | CONST_KW
            | GLOBAL_KW
            | LOCAL_KW
            | IMPORT_KW
            | USING_KW
            | EXPORT_KW
            | WHERE_KW
    )
}
