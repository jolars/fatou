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

use crate::parser::diagnostics::{DiagnosticKind, ParseDiagnostic};
use crate::syntax::{SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken};
use rowan::NodeOrToken;
use std::cell::RefCell;

use SyntaxKind::*;

// The projector reconstructs JuliaSyntax's `(error-t)`/`(error)` error shapes
// from the parse diagnostics rather than from dedicated CST nodes (the
// rust-analyzer model: recovery lives in a side-channel, not the tree). Since
// `project` recurses through ~50 free helpers, the diagnostics are stashed in a
// thread-local set once at the entry point and queried by byte position at the
// handful of sites that need them, rather than threaded through every signature.
thread_local! {
    static PROJ_DIAGS: RefCell<Vec<(DiagnosticKind, usize, usize)>> =
        const { RefCell::new(Vec::new()) };
}

/// Render the given Fatou CST as a JuliaSyntax-native s-expression string.
///
/// `diags` is the parse's diagnostics side-channel; the projector reads it to
/// reconstruct error shapes (`(error-t)` markers, recovery `(error-t …)` heads).
/// The root projects to `(toplevel …)`, mirroring `parseall`. Pair with
/// [`normalize_sexpr`] when comparing against captured Julia output to ignore
/// pretty-print whitespace differences.
pub fn to_juliasyntax_sexpr(tree: &SyntaxNode, diags: &[ParseDiagnostic]) -> String {
    PROJ_DIAGS.with(|d| {
        *d.borrow_mut() = diags.iter().map(|x| (x.kind, x.start, x.end)).collect();
    });
    project(tree)
}

/// Count diagnostics of `kind` recorded as a zero-width point at byte `pos`.
fn diag_count_at(pos: usize, kind: DiagnosticKind) -> usize {
    PROJ_DIAGS.with(|d| {
        d.borrow()
            .iter()
            .filter(|(k, s, e)| *k == kind && *s == pos && *e == pos)
            .count()
    })
}

/// Whether a zero-width diagnostic of `kind` is recorded at byte `pos`.
fn diag_at(pos: usize, kind: DiagnosticKind) -> bool {
    diag_count_at(pos, kind) > 0
}

/// Count diagnostics of `kind` whose range *starts* at byte `pos` (ignoring the
/// end). Used for the keyword-anchored truncation diagnostics, which span the
/// opening keyword rather than a zero-width point.
fn diag_count_from(pos: usize, kind: DiagnosticKind) -> usize {
    PROJ_DIAGS.with(|d| {
        d.borrow()
            .iter()
            .filter(|(k, s, _)| *k == kind && *s == pos)
            .count()
    })
}

/// The construct's opening-keyword start byte — the anchor the parser uses for
/// truncation diagnostics (`MissingEnd`, `MissingTryHandler`). For most block
/// forms that is the first non-trivia token, but a `do`-block leads with its
/// callee, so the `do` keyword is anchored instead.
fn keyword_start(node: &SyntaxNode) -> usize {
    if node.kind() == DO_EXPR
        && let Some(do_kw) = node
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == DO_KW)
    {
        return usize::from(do_kw.text_range().start());
    }
    node.descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| !is_trivia(t.kind()))
        .map(|t| usize::from(t.text_range().start()))
        .unwrap_or_else(|| usize::from(node.text_range().start()))
}

/// Whether `node` is a byte-bearing recovery `ERROR` node (a stray-closer run,
/// trailing junk, or an import recovery `:` clause) — JuliaSyntax renders those
/// with the `(error-t …)` head, unlike the plain `(error …)` of other recovery.
/// A zero-width (empty) error node is never one, so the synthesized leading
/// `(error)` of a stray closer keeps its plain head.
fn is_recovery_error(node: &SyntaxNode) -> bool {
    let r = node.text_range();
    if r.start() == r.end() {
        return false;
    }
    let (s, e) = (usize::from(r.start()), usize::from(r.end()));
    PROJ_DIAGS.with(|d| {
        d.borrow().iter().any(|(k, ds, de)| {
            matches!(
                k,
                DiagnosticKind::StrayCloser
                    | DiagnosticKind::TrailingJunk
                    | DiagnosticKind::ImportRecoveryColon
            ) && *ds >= s
                && *de <= e
        })
    })
}

/// The keyword text of a stray-block-keyword `ERROR` node (one carrying a
/// `StrayKeyword` diagnostic at its range), or `None` for any other error node.
/// JuliaSyntax renders such a node as `(error <kw>)` (`@doc x\nend` ⇒
/// `(macrocall @doc x) (error end)`), so the keyword token's text is surfaced
/// rather than dropped as a structural keyword.
fn stray_keyword_text(node: &SyntaxNode) -> Option<String> {
    let r = node.text_range();
    let (s, e) = (usize::from(r.start()), usize::from(r.end()));
    let has_diag = PROJ_DIAGS.with(|d| {
        d.borrow()
            .iter()
            .any(|(k, ds, de)| matches!(k, DiagnosticKind::StrayKeyword) && *ds >= s && *de <= e)
    });
    if !has_diag {
        return None;
    }
    node.children_with_tokens().find_map(|el| match el {
        NodeOrToken::Token(t) if is_keyword(t.kind()) => Some(t.text().to_string()),
        _ => None,
    })
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
        // A logical line carrying a top-level `;` groups its statements into a
        // `(toplevel-; …)` node (mirroring JuliaSyntax); an empty `;` line is
        // `(toplevel-;)`.
        TOPLEVEL_SEMICOLON => sexp("toplevel-;", stmt_strings(node)),
        // A string-literal statement directly followed by another statement folds
        // into a docstring `(doc <string> <target>)` (JuliaSyntax `parse_docstring`).
        DOC => sexp("doc", stmt_strings(node)),
        BLOCK => sexp("block", stmt_strings(node)),
        // `begin … end` wraps a `BLOCK`; project that directly so it lowers to a
        // single `(block …)` rather than a doubled `(block (block …))`.
        BEGIN_EXPR => project_block_child_folding_error(node),

        NAME => name_text(node),
        LITERAL => project_literal(node),
        STRING_LITERAL => project_string(node),
        NONSTANDARD_IDENTIFIER => project_var(node),
        CMD_LITERAL => project_cmd(node),
        // A standalone interpolation projects to a `$` node (`$x` → `($ x)`);
        // inside a string the inner value is used instead (via `string_parts`).
        // A bare `$` with no operand (`$\n`) is the `$` symbol itself.
        INTERPOLATION => {
            let inner = project_interpolation(node);
            if inner.is_empty() && !node.children_with_tokens().any(|el| el.kind() == LPAREN) {
                "$".to_string()
            } else {
                format!("($ {inner})")
            }
        }

        PAREN_EXPR | CONDITION => match first_node(node) {
            Some(inner) => project(&inner),
            // A lone operator in parens is the operator as a value/symbol, e.g.
            // `(+)` → `+` or, quoted, `:(=)` → `(quote-: =)`.
            None => significant(node)
                .iter()
                .find_map(|el| match el {
                    NodeOrToken::Token(t) if is_operator(t.kind()) => Some(t.text().to_string()),
                    _ => None,
                })
                .unwrap_or_else(|| "(block)".to_string()),
        },

        BINARY_EXPR => project_binary(node),
        RANGE_EXPR => project_range(node),
        COMPARISON_EXPR => project_comparison(node),
        ASSIGNMENT_EXPR => project_assignment(node),
        UNARY_EXPR => project_unary(node),
        POSTFIX_EXPR => project_postfix(node),
        SPLAT_EXPR => sexp("...", vec![project_first(node)]),
        TYPE_ANNOTATION => project_type_annotation(node),
        WHERE_EXPR => project_where(node),
        ARROW_EXPR => sexp("->", project_each(child_nodes(node))),
        JUXTAPOSE_EXPR => project_juxtapose(node),
        TERNARY_EXPR => project_ternary(node),

        CALL_EXPR => project_call("call", node),
        INDEX_EXPR => project_call("ref", node),
        CURLY_EXPR => project_call("curly", node),
        DOT_CALL_EXPR => project_dot_call(node),
        BRACES => sexp("braces", project_args(node)),

        TUPLE_EXPR => sexp("tuple-p", project_args(node)),
        PAREN_BLOCK => sexp("block-p", project_block_args(node)),
        BARE_TUPLE_EXPR => sexp("tuple", project_each(child_nodes(node))),
        VECT_EXPR => sexp("vect", project_args(node)),
        MATRIX_EXPR => project_matrix(node),
        TYPED_MATRIX_EXPR => project_typed_matrix(node),
        BRACESCAT_EXPR => project_bracescat(node),

        COMPREHENSION => sexp("comprehension", vec![project_generator(node)]),
        BRACES_COMPREHENSION => sexp("braces", vec![project_generator(node)]),
        TYPED_COMPREHENSION => project_typed_comprehension(node),
        GENERATOR => project_generator(node),

        IF_EXPR => project_if(node),
        WHILE_EXPR => sexp("while", project_each(child_nodes(node))),
        FOR_EXPR => {
            let mut parts = vec![project_for_binding(node), project_block_child(node)];
            push_trailing_errors(node, &mut parts);
            sexp("for", parts)
        }
        FUNCTION_DEF => project_function_like("function", node),
        MACRO_DEF => project_function_like("macro", node),
        LET_EXPR => project_let(node),
        QUOTE_EXPR => sexp("quote", vec![project_block_child_folding_error(node)]),
        QUOTE_SYM => project_quote_sym(node),
        TRY_EXPR => project_try(node),
        STRUCT_DEF => project_struct(node),
        ABSTRACT_DEF => sexp("abstract", vec![project_signature(node)]),
        PRIMITIVE_DEF => project_primitive(node),
        MODULE_DEF => project_module(node),
        DO_EXPR => project_do(node),

        RETURN_EXPR => project_keyword_stmt("return", node),
        BREAK_EXPR => "(break)".to_string(),
        CONTINUE_EXPR => "(continue)".to_string(),
        CONST_STMT => {
            // A `const` whose declaration is not a plain `=` assignment is wrapped
            // in `(error …)` (`const x` ⇒ `(error (const x))`); the parser records
            // the diagnostic at the `const` keyword start.
            let decl = project_decl("const", node);
            if diag_at(
                usize::from(node.text_range().start()),
                DiagnosticKind::ConstNotAssignment,
            ) {
                format!("(error {decl})")
            } else {
                decl
            }
        }
        GLOBAL_STMT => project_decl("global", node),
        LOCAL_STMT => project_decl("local", node),
        IMPORT_STMT => project_import("import", node),
        USING_STMT => project_import("using", node),
        EXPORT_STMT => project_export(node),
        PUBLIC_STMT => project_public(node),
        IMPORT_PATH => project_import_path(node),
        IMPORT_ALIAS => project_import_alias(node),

        MACRO_CALL => project_macrocall(node),

        END_MARKER => "end".to_string(),
        BEGIN_MARKER => "begin".to_string(),
        OPERATOR_ATOM => project_operator_atom(node),

        // The sole error node kind. JuliaSyntax distinguishes the bare `(error)`
        // (a missing required element, an invalid `as` rename) from the
        // `TRIVIA_FLAG`-tagged `(error-t)` (a recovered run); `is_recovery_error`
        // recovers that distinction from the diagnostics side-channel. A stray
        // closing delimiter recovered into the node renders as `✘`.
        ERROR => {
            // A stray middle/closing block keyword (`end`, `else`, `elseif`,
            // `catch`, `finally`) where a statement was expected is wrapped
            // alone in `(error <kw>)` — the keyword text is rendered, unlike the
            // dropped-keyword default of a recovery run.
            if let Some(kw) = stray_keyword_text(node) {
                format!("(error {kw})")
            } else {
                project_error(
                    if is_recovery_error(node) {
                        "error-t"
                    } else {
                        "error"
                    },
                    node,
                )
            }
        }

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
        // Invalid doubled operators: JuliaSyntax heads the infix call with the
        // error token itself (`a**b` ⇒ `(call-i a (Error**) b)`, `a--b` ⇒
        // `(call-i a (ErrorInvalidOperator) b)`).
        STAR_STAR => CallI("(Error**)"),
        MINUS_MINUS => CallI("(ErrorInvalidOperator)"),
        SLASH => CallI("/"),
        SLASH_SLASH => CallI("//"),
        CARET => CallI("^"),
        PERCENT => CallI("%"),
        COLON => CallI(":"),
        DOT_DOT => CallI(".."),
        FAT_ARROW => CallI("=>"),
        PIPE_GT => CallI("|>"),
        PIPE_LT => CallI("<|"),
        LONG_ARROW => Special("-->"),
        LEFT_RIGHT_ARROW => CallI("<-->"),
        SHL => CallI("<<"),
        SHR => CallI(">>"),
        USHR => CallI(">>>"),
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
        DOT_STAR_STAR => DotCallI("(Error**)"),
        DOT_MINUS_MINUS => DotCallI("(ErrorInvalidOperator)"),
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
        DOT_SUBTYPE => DotCallI("<:"),
        DOT_SUPERTYPE => DotCallI(">:"),
        DOT_FAT_ARROW => DotCallI("=>"),
        DOT_LONG_ARROW => DotCallI("-->"),
        DOT_PIPE_GT => DotCallI("|>"),
        DOT_AMP => DotCallI("&"),
        DOT_PIPE => DotCallI("|"),

        // Fallback: treat as an ordinary infix call using the raw text. Leaked
        // in faithfully so an unmapped operator surfaces as a divergence.
        _ => CallI("?"),
    }
}

/// A bare operator used as a value atom (`+` → `+`, `.&` → `(. &)`, `:` → `:`).
/// Broadcast operators project to `(. op)` (via `operator_func_repr`); every
/// other operator projects to its raw token text, so kinds without an
/// `infix_head` entry (the Unicode radicals `√`/`¬`, `..`) project faithfully.
fn project_operator_atom(node: &SyntaxNode) -> String {
    significant(node)
        .iter()
        .find_map(|el| match el {
            NodeOrToken::Token(t) if matches!(infix_head(t.kind()), InfixHead::DotCallI(_)) => {
                // A suffixed broadcast operator (`.+₁`) keeps its suffix, which
                // `operator_func_repr` (keyed on kind) would drop; strip the
                // leading broadcast dot from the text instead.
                Some(if op_has_suffix(t.text()) {
                    format!("(. {})", &t.text()[1..])
                } else {
                    operator_func_repr(t.kind())
                })
            }
            // Dotted broadcast operators whose head is not `DotCallI` — the
            // short-circuit `.&&`/`.||` and the assignment forms `.=`/`.+=` quote
            // as `(. op)` too (`:.&&` ⇒ `(quote-: (. &&))`). Strip the leading
            // broadcast dot; `..`/`...` lead with a doubled dot and are excluded.
            NodeOrToken::Token(t)
                if t.text().starts_with('.')
                    && t.text().len() > 1
                    && t.text().as_bytes()[1] != b'.' =>
            {
                Some(format!("(. {})", &t.text()[1..]))
            }
            NodeOrToken::Token(t) => Some(t.text().to_string()),
            _ => None,
        })
        .unwrap_or_else(|| "(operator)".to_string())
}

/// The function-name representation of an operator used as a callee, e.g. in
/// `*(x)` → `(call * x)` or `.*(x)` → `(call (. *) x)`. Broadcast operators
/// project to `(. op)`; everything else to the bare operator text.
fn operator_func_repr(kind: SyntaxKind) -> String {
    // `!` is unary-only (no `infix_head` entry), but it is a valid call callee:
    // `!(a, b)` → `(call ! a b)`.
    if kind == BANG {
        return "!".to_string();
    }
    match infix_head(kind) {
        InfixHead::CallI(s) | InfixHead::Special(s) => s.to_string(),
        InfixHead::DotCallI(s) => format!("(. {s})"),
        InfixHead::Dot => ".".to_string(),
    }
}

/// Whether an operator's text carries a trailing sub/superscript or prime suffix
/// (`+₁`, `-->₁`). A base operator never ends in a suffix character, so checking
/// the final character is sufficient.
fn op_has_suffix(text: &str) -> bool {
    text.chars()
        .next_back()
        .is_some_and(super::lexer::is_op_suffix_char)
}

fn is_operator(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        EQ | PLUS
            | MINUS
            | STAR
            | STAR_STAR
            | MINUS_MINUS
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
            | LONG_ARROW
            | LEFT_RIGHT_ARROW
            | FAT_ARROW
            | SHL
            | SHR
            | USHR
            | DOT
            | PIPE_GT
            | PIPE_LT
            | BANG
            | AMP
            | PIPE
            | DOT_PLUS
            | DOT_MINUS
            | DOT_STAR
            | DOT_STAR_STAR
            | DOT_MINUS_MINUS
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
            | DOT_SUBTYPE
            | DOT_SUPERTYPE
            | DOT_FAT_ARROW
            | DOT_LONG_ARROW
            | DOT_PIPE_GT
            | DOT_AMP
            | DOT_PIPE
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
            | UNICODE_OP
            | UNICODE_ASSIGN_OP
            | UNICODE_RADICAL
    )
}

// --- Binary / unary / assignment -------------------------------------------

/// `(? cond then else)`. Whitespace errors around `?`/`:` are recorded as
/// `TernaryQWhitespace` (anchored at the `?`'s end) and `TernaryColonWhitespace`
/// (anchored at the true-branch's end); each replays as a `(error-t)` between the
/// respective operands (`a?b:c` ⇒ `(? a (error-t) (error-t) b (error-t) (error-t) c)`).
fn project_ternary(node: &SyntaxNode) -> String {
    let nodes = child_nodes(node);
    let q_end = node
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == QUESTION)
        .map(|t| usize::from(t.text_range().end()));
    // An incomplete ternary terminated by a closing block keyword is re-headed
    // `?` → `if` with one trailing `(error-t)` per missing piece (`x ? true end`
    // ⇒ `(if x true (error-t) (error-t))`, `x ? true : elseif …` ⇒
    // `(if x true (error-t))`).
    if let Some(qe) = q_end {
        let markers = diag_count_at(qe, DiagnosticKind::IncompleteTernaryIf);
        if markers > 0 {
            let mut parts: Vec<String> = nodes.iter().map(project).collect();
            for _ in 0..markers {
                parts.push("(error-t)".to_string());
            }
            return sexp("if", parts);
        }
    }
    let mut parts = Vec::new();
    for (i, n) in nodes.iter().enumerate() {
        parts.push(project(n));
        if i == 0 {
            if let Some(qe) = q_end {
                for _ in 0..diag_count_at(qe, DiagnosticKind::TernaryQWhitespace) {
                    parts.push("(error-t)".to_string());
                }
            }
        } else if i == 1 {
            let then_end = usize::from(n.text_range().end());
            for _ in 0..diag_count_at(then_end, DiagnosticKind::TernaryColonWhitespace) {
                parts.push("(error-t)".to_string());
            }
        }
    }
    sexp("?", parts)
}

/// `(juxtapose lhs rhs)`. An *invalid* string juxtaposition (`"a"x`, `2"b"`)
/// records a `StringJuxtapose` diagnostic at the left operand's end, projecting
/// a `(error-t)` between the operands (`(juxtapose "a" (error-t) x)`).
fn project_juxtapose(node: &SyntaxNode) -> String {
    let nodes = child_nodes(node);
    let mut parts = Vec::new();
    for (i, n) in nodes.iter().enumerate() {
        parts.push(project(n));
        if i == 0
            && diag_at(
                usize::from(n.text_range().end()),
                DiagnosticKind::StringJuxtapose,
            )
        {
            parts.push("(error-t)".to_string());
        }
    }
    sexp("juxtapose", parts)
}

fn project_binary(node: &SyntaxNode) -> String {
    // Word operators `in`/`isa` are lexed as identifiers, so the operator is the
    // sole loose `IDENT` token child (both operands are wrapped in nodes). They
    // head an ordinary infix call: `i in rhs` ⇒ `(call-i i in rhs)`.
    if let Some(word) = node
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == IDENT && matches!(t.text(), "in" | "isa"))
    {
        let operands = child_nodes(node);
        if operands.len() == 2 {
            return format!(
                "(call-i {} {} {})",
                project(&operands[0]),
                word.text(),
                project(&operands[1])
            );
        }
    }
    let op = match operator_token(node) {
        Some(t) => t,
        None => return format!("(unsupported {:?})", node.kind()),
    };
    // A range colon glued to a single `<`/`>` (`InvalidGluedOperator` recorded at
    // the colon) heads the call with both operator tokens error-wrapped: `a :< b`
    // ⇒ `(call-i a (error : <) b)`, the missing-rhs form `a :<` ⇒ `(call-i a
    // (error : <) (error))`.
    if op.kind() == COLON
        && diag_at(
            usize::from(op.text_range().start()),
            DiagnosticKind::InvalidGluedOperator,
        )
    {
        let head: Vec<String> = node
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .filter(|t| is_operator(t.kind()))
            .map(|t| t.text().to_string())
            .collect();
        let operands = child_nodes(node);
        let lhs = project(&operands[0]);
        let rhs = operands
            .get(1)
            .map(project)
            .unwrap_or_else(|| "(error)".to_string());
        return format!("(call-i {lhs} (error {}) {rhs})", head.join(" "));
    }
    // A field-access dot with disallowed leading whitespace (`x .y`) records a
    // `DotWhitespace` diagnostic at the dot's end; splice `(error-t)` before the
    // quoted field name in the `Dot` arm below.
    let dot_error = if diag_at(
        usize::from(op.text_range().end()),
        DiagnosticKind::DotWhitespace,
    ) {
        "(error-t) "
    } else {
        ""
    };
    let operands = child_nodes(node);
    // Missing right operand: JuliaSyntax keeps the operator node and synthesizes
    // a zero-width `(error)` for the absent operand (`a +` ⇒ `(call-i a +
    // (error))`, `a &&` ⇒ `(&& a (error))`). Reconstructed from the
    // `MissingOperand` diagnostic at the operator; a field-access dot has no
    // suffix-quoting to do, so it gets the bare `(. lhs (error))`.
    if operands.len() == 1 && operator_missing_rhs(&op) {
        let lhs = project(&operands[0]);
        return infix_call_string(&op, &lhs, "(error)")
            .unwrap_or_else(|| format!("(. {lhs} {dot_error}(error))"));
    }
    // A flat arithmetic chain (`a + b + c` ⇒ `(call-i a + b c)`): two or more
    // operands joined by a single repeated `+`/`*`, projecting as one variadic
    // infix call. A trailing dangling operator (`a + b +`) keeps two operands and
    // a `MissingOperand` diagnostic on the last operator, replayed as `(error)`.
    let last_op_missing = node
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| is_operator(t.kind()))
        .last()
        .is_some_and(|t| operator_missing_rhs(&t));
    if operands.len() >= 3 || (operands.len() == 2 && last_op_missing) {
        return project_flat_arith(&op, &operands, last_op_missing);
    }
    if operands.len() != 2 {
        return project_flat(significant(node));
    }
    let lhs = project(&operands[0]);
    let rhs = &operands[1];
    let rhs_str = project(rhs);
    // The non-field-access heads (unicode, suffixed, `call-i`/`dotcall-i`/special)
    // are shared with the missing-operand path above.
    if let Some(s) = infix_call_string(&op, &lhs, &rhs_str) {
        return s;
    }
    match infix_head(op.kind()) {
        // Field access. A plain field name is quoted (`f.x` → `(. f (quote x))`);
        // an interpolated field name is inert-quoted (`f.$x` →
        // `(. f (inert ($ x)))`), so the interpolation projects through `($ …)`.
        InfixHead::Dot if rhs.kind() == INTERPOLATION => {
            format!("(. {lhs} {dot_error}(inert {rhs_str}))")
        }
        // A quoted field name (`a.:b`) is already a `(quote-: …)` symbol; emit it
        // directly rather than wrapping it in another `(quote …)`.
        InfixHead::Dot if rhs.kind() == QUOTE_SYM => {
            format!("(. {lhs} {dot_error}{rhs_str})")
        }
        InfixHead::Dot => format!("(. {lhs} {dot_error}(quote {}))", name_text(rhs)),
        // Non-dot heads are handled by `infix_call_string` above.
        _ => unreachable!("non-dot infix head handled by infix_call_string"),
    }
}

/// Project a flat arithmetic chain (`a + b + c` ⇒ `(call-i a + b c)`): the
/// single repeated operator heads a variadic infix call over all operands. A
/// trailing dangling operator appends a zero-width `(error)`. The operator is a
/// `CallI` head (`+`/`*`); any unexpected non-`CallI` operator still joins
/// faithfully under `call-i` so a divergence surfaces.
fn project_flat_arith(op: &SyntaxToken, operands: &[SyntaxNode], last_op_missing: bool) -> String {
    let (head, op_text) = match infix_head(op.kind()) {
        InfixHead::CallI(text) => ("call-i", text.to_string()),
        InfixHead::DotCallI(text) => ("dotcall-i", text.to_string()),
        InfixHead::Special(text) => ("call-i", text.to_string()),
        InfixHead::Dot => ("call-i", op.text().to_string()),
    };
    let mut parts = Vec::with_capacity(operands.len() + 2);
    parts.push(project(&operands[0]));
    parts.push(op_text);
    parts.extend(operands[1..].iter().map(project));
    if last_op_missing {
        parts.push("(error)".to_string());
    }
    sexp(head, parts)
}

/// Format a non-field-access infix operator from its operator token and
/// already-projected operand strings. Returns `None` for a field-access dot,
/// whose right operand needs structural inspection (handled by the caller).
/// Shared by the normal and missing-right-operand projection paths.
fn infix_call_string(op: &SyntaxToken, lhs: &str, rhs: &str) -> Option<String> {
    // Unicode operators carry their own text: the `call-i` tiers head an ordinary
    // infix call, and the assignment tier (`≔ ≕ ⩴`) heads the node with the
    // operator itself, just like the ASCII `Special` forms.
    match op.kind() {
        // A broadcast Unicode operator (`.…`, `.×`) carries a leading `.`: strip
        // it and head `dotcall-i`, like the ASCII broadcast forms.
        UNICODE_OP if op.text().starts_with('.') => {
            return Some(format!("(dotcall-i {lhs} {} {rhs})", &op.text()[1..]));
        }
        UNICODE_OP => return Some(format!("(call-i {lhs} {} {rhs})", op.text())),
        UNICODE_ASSIGN_OP => return Some(format!("({} {lhs} {rhs})", op.text())),
        _ => {}
    }
    // A suffixed operator (`a +₁ b`, `x -->₁ y`) carries its sub/superscript
    // suffix in the token text and always projects as a generic infix call —
    // even operators that are otherwise syntactic (`-->`). Broadcast operators
    // keep the `dotcall-i` head with the leading `.` stripped from the function
    // name. Mirrors JuliaSyntax, where a suffix makes the operator non-syntactic.
    if op_has_suffix(op.text()) {
        let text = op.text();
        return Some(match infix_head(op.kind()) {
            InfixHead::DotCallI(_) => {
                format!("(dotcall-i {lhs} {} {rhs})", text.trim_start_matches('.'))
            }
            _ => format!("(call-i {lhs} {text} {rhs})"),
        });
    }
    match infix_head(op.kind()) {
        InfixHead::CallI(text) => Some(format!("(call-i {lhs} {text} {rhs})")),
        InfixHead::Special(text) => Some(format!("({text} {lhs} {rhs})")),
        InfixHead::DotCallI(text) => Some(format!("(dotcall-i {lhs} {text} {rhs})")),
        InfixHead::Dot => None,
    }
}

/// Whether the operator token heads a node whose right operand is absent — a
/// `MissingOperand` diagnostic recorded spanning the operator. JuliaSyntax
/// synthesizes a zero-width `(error)` for the missing operand.
fn operator_missing_rhs(op: &SyntaxToken) -> bool {
    diag_count_from(
        usize::from(op.text_range().start()),
        DiagnosticKind::MissingOperand,
    ) > 0
}

/// A stepped range `a:b:c` is a single infix colon call over three operands:
/// `(call-i a : b c)`. Mirrors JuliaSyntax, which folds the range with step into
/// one `(call ...)` node rather than nesting two binary colons.
fn project_range(node: &SyntaxNode) -> String {
    let operands = child_nodes(node);
    // A stepped range with its third operand absent (`1:2:` ⇒
    // `(call-i 1 : 2 (error))`) keeps two operands and a `MissingOperand`
    // diagnostic on the trailing colon.
    if operands.len() == 2 {
        if node
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .filter(|t| t.kind() == COLON)
            .last()
            .is_some_and(|colon| operator_missing_rhs(&colon))
        {
            return format!(
                "(call-i {} : {} (error))",
                project(&operands[0]),
                project(&operands[1]),
            );
        }
        return project_flat(significant(node));
    }
    if operands.len() != 3 {
        return project_flat(significant(node));
    }
    format!(
        "(call-i {} : {} {})",
        project(&operands[0]),
        project(&operands[1]),
        project(&operands[2]),
    )
}

/// Project a comparison chain (`a < b <= c` ⇒ `(comparison a < b <= c)`). The
/// operands and operator tokens are emitted in source order; a plain operator
/// renders as its text, a dotted-broadcast comparison as `(. op)`
/// (`a .< b .< c` ⇒ `(comparison a (. <) b (. <) c)`). A dangling trailing
/// operator with no right operand replays the zero-width `(error)` from the
/// `MissingOperand` diagnostic (`a < b <` ⇒ `(comparison a < b < (error))`).
fn project_comparison(node: &SyntaxNode) -> String {
    let mut parts = Vec::new();
    let mut last_op = None;
    for el in significant(node) {
        match el {
            NodeOrToken::Node(n) => parts.push(project(&n)),
            NodeOrToken::Token(t) => {
                let text = t.text();
                if text.starts_with('.') && text.len() > 1 && text.as_bytes()[1] != b'.' {
                    parts.push(format!("(. {})", &text[1..]));
                } else {
                    parts.push(text.to_string());
                }
                last_op = Some(t);
            }
        }
    }
    if last_op.is_some_and(|op| operator_missing_rhs(&op)) {
        parts.push("(error)".to_string());
    }
    sexp("comparison", parts)
}

fn project_assignment(node: &SyntaxNode) -> String {
    // The operator's own text is its JuliaSyntax head verbatim: `=`, `.=`, `+=`,
    // `.+=`, … all project as `(<op> lhs rhs)`.
    let op = operator_token(node);
    let head = match &op {
        Some(t) => t.text().to_string(),
        None => "=".to_string(),
    };
    let operands = child_nodes(node);
    // Missing right operand: `<: =` ⇒ `(= <: (error))`, `a +=` ⇒ `(+= a (error))`.
    if operands.len() == 1 && op.is_some_and(|t| operator_missing_rhs(&t)) {
        return format!("({head} {} (error))", project(&operands[0]));
    }
    sexp(&head, project_each(operands))
}

fn project_unary(node: &SyntaxNode) -> String {
    // Invalid prefix: a binary-only operator used in prefix position is
    // error-wrapped and applied as a prefix call (`/x` ⇒ `(call-pre (error /) x)`).
    // A broadcast operator heads `dotcall-pre` instead (`.*x` ⇒
    // `(dotcall-pre (error (. *)) x)`). The error-wrapped operator (an
    // `OPERATOR_ATOM` inside the `ERROR`) projects to `/` or `(. *)`.
    if let Some(err) = node.children().find(|c| c.kind() == ERROR) {
        let operand = child_nodes(node)
            .iter()
            .find(|c| c.kind() != ERROR)
            .map(project)
            .unwrap_or_default();
        let head = if invalid_prefix_is_dotted(&err) {
            "dotcall-pre"
        } else {
            "call-pre"
        };
        return format!("({head} {} {operand})", project(&err));
    }
    let op = match operator_token(node) {
        Some(t) => t,
        None => return format!("(unsupported {:?})", node.kind()),
    };
    let operand = project_first(node);
    match op.kind() {
        SUBTYPE => format!("(<:-pre {operand})"),
        SUPERTYPE => format!("(>:-pre {operand})"),
        // The address-of `&x` heads the node with the operator itself (a
        // syntactic prefix), not the generic `call-pre`.
        AMP => format!("(& {operand})"),
        DOT_PLUS => format!("(dotcall-pre + {operand})"),
        DOT_MINUS => format!("(dotcall-pre - {operand})"),
        DOT_TILDE => format!("(dotcall-pre ~ {operand})"),
        _ => format!("(call-pre {} {operand})", op.text()),
    }
}

/// Whether the operator error-wrapped inside an invalid-prefix `ERROR` node is a
/// broadcast (dotted) operator (`.*`, `./`, `.<:`) — those head `dotcall-pre`.
/// A leading `..` (the range operator) is not dotted in this sense.
fn invalid_prefix_is_dotted(err: &SyntaxNode) -> bool {
    err.descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| is_operator(t.kind()))
        .map(|t| {
            let b = t.text().as_bytes();
            b.first() == Some(&b'.') && b.len() > 1 && b[1] != b'.'
        })
        .unwrap_or(false)
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

/// Project a broadcast call `f.(args)` → `(dotcall f args…)`. A broadcast applied
/// to a macro name (`@M.(x)`) is invalid — a macro cannot be broadcast — so
/// JuliaSyntax re-heads it as a macrocall wrapping the dotcall and splices a
/// zero-width `(error-t)` after the name (`@M.(x)` ⇒
/// `(macrocall (dotcall @M (error-t) x))`). The parser records `MacroDotBroadcast`
/// at the broadcast opener.
fn project_dot_call(node: &SyntaxNode) -> String {
    if let Some(callee) = node.children().next()
        && callee.kind() == MACRO_CALL
        && let Some(arg_list) = node.children().find(|c| c.kind() == ARG_LIST)
        && diag_at(
            usize::from(arg_list.text_range().start()),
            DiagnosticKind::MacroDotBroadcast,
        )
    {
        let name = callee
            .children()
            .find(|c| c.kind() == MACRO_NAME)
            .map(|c| project_macro_name(&c))
            .unwrap_or_else(|| "@?".to_string());
        let mut parts = vec![name, "(error-t)".to_string()];
        parts.extend(project_args(&arg_list));
        return format!("(macrocall (dotcall {}))", parts.join(" "));
    }
    project_call("dotcall", node)
}

fn project_call(head: &str, node: &SyntaxNode) -> String {
    let mut parts = Vec::new();
    let mut head = head.to_string();
    // The callee is the first significant element. Usually a node (`f(x)`), but
    // for operator-as-call functions (`*(x)`, `.*(x)`) it is a bare operator
    // token that projects to its function name (`*`, `(. *)`).
    for el in significant(node) {
        match el {
            NodeOrToken::Node(n) => {
                parts.push(project(&n));
                break;
            }
            // The type operators `<:`/`>:` in call syntax (`<:(a, b)` →
            // `(<: a b)`) are syntactic: JuliaSyntax heads the node with the
            // operator itself rather than wrapping it in a `call`. In a `curly`
            // callee (`<:{T}` → `(curly <: T)`) the operator is an ordinary part,
            // so this head override only applies to `call`.
            NodeOrToken::Token(t) if head == "call" && matches!(t.kind(), SUBTYPE | SUPERTYPE) => {
                head = operator_func_repr(t.kind());
                break;
            }
            NodeOrToken::Token(t) if is_operator(t.kind()) => {
                // A suffixed operator callee (`+₁(x)` → `(call +₁ x)`) keeps its
                // suffix, which `operator_func_repr` (keyed on kind) would drop.
                parts.push(if op_has_suffix(t.text()) {
                    match t.text().strip_prefix('.') {
                        Some(rest) if matches!(infix_head(t.kind()), InfixHead::DotCallI(_)) => {
                            format!("(. {rest})")
                        }
                        _ => t.text().to_string(),
                    }
                } else {
                    operator_func_repr(t.kind())
                });
                break;
            }
            NodeOrToken::Token(_) => {}
        }
    }
    if let Some(arg_list) = node.children().find(|c| c.kind() == ARG_LIST) {
        // Disallowed whitespace before the opener records `OpenerWhitespace` at
        // the opener's start (`f (a)` → `(call f (error-t) a)`); the marker sits
        // between the callee and the arguments.
        if diag_at(
            usize::from(arg_list.text_range().start()),
            DiagnosticKind::OpenerWhitespace,
        ) {
            parts.push("(error-t)".to_string());
        }
        // A unary-prefix operator callee with a space before its call-form `(`
        // flags the space as a zero-width `(error)` (`+ (a,b)` → `(call +
        // (error) a b)`), unlike the `(error-t)` of an identifier callee.
        if diag_at(
            usize::from(arg_list.text_range().start()),
            DiagnosticKind::PrefixOpenerWhitespace,
        ) {
            parts.push("(error)".to_string());
        }
        parts.extend(project_args(&arg_list));
    } else if let Some(generator) = node.children().find(|c| c.kind() == GENERATOR) {
        // A bare generator argument: `sum(x for x in xs)` → `(call sum (generator …))`.
        parts.push(project_generator(&generator));
    }
    sexp(&head, parts)
}

/// Project a typed comprehension `T[x for x in xs]` →
/// `(typed_comprehension T (generator x (= x xs)))`. The callee is the first
/// child; the bracketed body and clauses form the `GENERATOR` child.
fn project_typed_comprehension(node: &SyntaxNode) -> String {
    let mut parts = Vec::new();
    if let Some(callee) = node.children().next() {
        parts.push(project(&callee));
    }
    if let Some(generator) = node.children().find(|c| c.kind() == GENERATOR) {
        parts.push(project_generator(&generator));
    }
    sexp("typed_comprehension", parts)
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
    // An argument list with no closing delimiter (`f(a`) records `UnterminatedArgList`
    // at the opener's start, projecting a trailing `(error-t)` (`(call f a (error-t))`).
    if diag_at(
        usize::from(container.text_range().start()),
        DiagnosticKind::UnterminatedArgList,
    ) {
        out.push("(error-t)".to_string());
    }
    out
}

/// Project a `PAREN_BLOCK`'s children as a flat statement list. The block is
/// parsed with the arg-list machinery, so positional statements are `ARG`s,
/// assignments are `KEYWORD_ARG`s, and statements after the first `;` live in a
/// `PARAMETERS` node — all flattened away here, since a block has no parameters
/// (`(a; b; c)` ⇒ `(block-p a b c)`, `(a=1; b=2)` ⇒ `(block-p (= a 1) (= b 2))`).
fn project_block_args(container: &SyntaxNode) -> Vec<String> {
    let mut out = Vec::new();
    let push_stmt = |n: &SyntaxNode, out: &mut Vec<String>| match n.kind() {
        ARG => out.push(project_first(n)),
        KEYWORD_ARG => out.push(project_keyword_arg(n)),
        _ => out.push(project(n)),
    };
    for el in significant(container) {
        if let NodeOrToken::Node(n) = el {
            if n.kind() == PARAMETERS {
                for inner in significant(&n) {
                    if let NodeOrToken::Node(m) = inner {
                        push_stmt(&m, &mut out);
                    }
                }
            } else {
                push_stmt(&n, &mut out);
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

/// Project the outer `[...]` concatenation node. The CST nests rows by
/// dimension (`MATRIX_ROW` groups, bare `ARG` elements); the dimension `d` of a
/// group is recovered from the separator tokens between its direct child nodes:
/// a run of `;` contributes its length, a row-separating newline contributes 1,
/// a space contributes 0. The top head is `hcat`/`vcat`/`ncat-d`
/// (`d` = 0/1/≥2); nested groups are `row` (`d` = 0) or `nrow-d` (`d` ≥ 1),
/// mirroring JuliaSyntax. An element-free `[; …]` is `ncat-d` with `d` the
/// longest semicolon run.
fn project_matrix(node: &SyntaxNode) -> String {
    let (head, children) = matrix_head_and_children(node);
    sexp(&head, children)
}

/// The bracket-concatenation head (`hcat`/`vcat`/`ncat-d`, or `ncat-d` for an
/// element-free `[; …]`) and the projected child s-expressions. Shared by the
/// plain (`[...]`) and typed (`T[...]`) concatenation projectors.
fn matrix_head_and_children(node: &SyntaxNode) -> (String, Vec<String>) {
    let children: Vec<SyntaxNode> = node
        .children()
        .filter(|c| matches!(c.kind(), ARG | MATRIX_ROW))
        .collect();
    if children.is_empty() {
        return (format!("ncat-{}", max_semicolon_run(node)), Vec::new());
    }
    let d = group_dimension(node);
    let head = match d {
        0 => "hcat".to_string(),
        1 => "vcat".to_string(),
        _ => format!("ncat-{d}"),
    };
    (head, project_cat_children(&children))
}

/// Project a sequence of concatenation children (`ARG` elements and `MATRIX_ROW`
/// groups), splicing a zero-width `(error-t)` after any bare element whose end
/// carries an `ArraySeparatorMismatch` diagnostic — a space/`;;` separator-order
/// conflict (`[a b ;; c]` ⇒ `(ncat-2 (row a b (error-t)) c)`). The marker is only
/// appended after `ARG` elements: when the offending element is the last in a
/// row, the diagnostic's byte anchor also coincides with the enclosing
/// `MATRIX_ROW`'s end, and the recursion handles it inside that row instead.
fn project_cat_children(children: &[SyntaxNode]) -> Vec<String> {
    let mut out = Vec::new();
    for child in children {
        out.push(project_cat_child(child));
        if child.kind() == ARG {
            let end = usize::from(child.text_range().end());
            for _ in 0..diag_count_at(end, DiagnosticKind::ArraySeparatorMismatch) {
                out.push("(error-t)".to_string());
            }
        }
    }
    out
}

/// Project a typed concatenation `T[...]`: the same shape as the inner
/// `MATRIX_EXPR`, but with the type expression prepended and the head prefixed
/// `typed_` (`T[x y]` → `(typed_hcat T x y)`, `T[;]` → `(typed_ncat-1 T)`).
fn project_typed_matrix(node: &SyntaxNode) -> String {
    let mut children = node.children();
    let type_node = children.next().expect("typed concat has a type child");
    let matrix = children
        .find(|c| c.kind() == MATRIX_EXPR)
        .expect("typed concat has a matrix body");
    let (head, mut args) = matrix_head_and_children(&matrix);
    args.insert(0, project(&type_node));
    sexp(&format!("typed_{head}"), args)
}

/// Project a brace concatenation `{...}`: always headed `bracescat`. A
/// dimension-1 (vcat-like) layout keeps its children directly; a horizontal or
/// higher-dimensional layout (and the element-free `{; …}` form) becomes a
/// single nested `row`/`nrow-d` child, since `bracescat` is itself the
/// dimension-1 container (`{x y}` → `(bracescat (row x y))`, `{a;b}` →
/// `(bracescat a b)`, `{;;}` → `(bracescat (nrow-2))`).
fn project_bracescat(node: &SyntaxNode) -> String {
    let children: Vec<SyntaxNode> = node
        .children()
        .filter(|c| matches!(c.kind(), ARG | MATRIX_ROW))
        .collect();
    if children.is_empty() {
        let d = max_semicolon_run(node);
        return sexp("bracescat", vec![sexp(&format!("nrow-{d}"), Vec::new())]);
    }
    let d = group_dimension(node);
    let items: Vec<String> = children.iter().map(project_cat_child).collect();
    match d {
        1 => sexp("bracescat", items),
        0 => sexp("bracescat", vec![sexp("row", items)]),
        _ => sexp("bracescat", vec![sexp(&format!("nrow-{d}"), items)]),
    }
}

/// Project one direct child of a concatenation group: a bare `ARG` element
/// unwraps to its inner expression; a `MATRIX_ROW` becomes a `row` (dimension 0)
/// or `nrow-d` group, recursing on its own children.
fn project_cat_child(node: &SyntaxNode) -> String {
    match node.kind() {
        ARG => project_first(node),
        MATRIX_ROW => {
            let d = group_dimension(node);
            let head = if d == 0 {
                "row".to_string()
            } else {
                format!("nrow-{d}")
            };
            let items: Vec<SyntaxNode> = node
                .children()
                .filter(|c| matches!(c.kind(), ARG | MATRIX_ROW))
                .collect();
            sexp(&head, project_cat_children(&items))
        }
        _ => project(node),
    }
}

/// The concatenation dimension of a group: the largest separator dimension among
/// the runs lying *between* direct child nodes (a `;` run counts its length, a
/// newline counts 1), plus any trailing semicolon run after the last child
/// (a trailing newline does not separate, so it is ignored).
///
/// A `;;` immediately followed by a newline (`;; \n`) inside a row-major group —
/// one where a plain space separator was already seen — is a *line continuation*
/// that JuliaSyntax folds into the row, so it counts as dimension 0 rather than
/// 2 (`[a b ;; \n c]` ⇒ `(hcat a b c)`). We re-derive the row-major order locally
/// the same way JuliaSyntax does (first space ⇒ row-major).
fn group_dimension(node: &SyntaxNode) -> usize {
    let mut d = 0;
    let mut seen_node = false;
    let mut row_major = false;
    let mut semis = 0usize;
    let mut newline = false;
    let mut newline_after_semis = false;
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(n) if matches!(n.kind(), ARG | MATRIX_ROW) => {
                if seen_node {
                    let is_space = semis == 0 && !newline;
                    let continuation = semis == 2 && newline_after_semis && row_major;
                    let run = if continuation {
                        0
                    } else if semis > 0 {
                        semis
                    } else {
                        usize::from(newline)
                    };
                    if is_space {
                        row_major = true;
                    }
                    d = d.max(run);
                }
                seen_node = true;
                semis = 0;
                newline = false;
                newline_after_semis = false;
            }
            NodeOrToken::Token(t) => match t.kind() {
                SEMICOLON => {
                    semis += 1;
                    newline_after_semis = false;
                }
                NEWLINE => {
                    newline = true;
                    if semis > 0 {
                        newline_after_semis = true;
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
    // Trailing run after the last child: only semicolons separate (`[x;]` is a
    // `vcat`, but `[x\n]` is just a `vect`).
    if seen_node && semis > 0 {
        d = d.max(semis);
    }
    d
}

/// The length of the longest consecutive `;` run among a node's direct tokens
/// (used for element-free `[; …]` concatenations).
fn max_semicolon_run(node: &SyntaxNode) -> usize {
    let mut max = 0;
    let mut run = 0;
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if t.kind() == SEMICOLON => {
                run += 1;
                max = max.max(run);
            }
            NodeOrToken::Token(t) if is_trivia(t.kind()) => {}
            _ => run = 0,
        }
    }
    max
}

// --- Comprehensions / generators -------------------------------------------

fn project_generator(node: &SyntaxNode) -> String {
    // Fatou is flat: `body (FOR_BINDING [COMPREHENSION_IF])…`. JuliaSyntax nests
    // `(generator body <clause>…)`, where each `for` clause is one `(= v it)` (or
    // a `(cartesian_iterator …)` for comma-separated specs), and a trailing `if`
    // wraps the immediately preceding clause in a `(filter <clause> cond)`.
    let mut body = String::new();
    // `for` glued to the body splices a zero-width `(error-t)` marker between
    // the body and the first clause (`[(x)for x in xs]` → `(generator x
    // (error-t) (= x xs))`); keep clauses and markers in source order.
    let mut clauses: Vec<String> = Vec::new();
    for child in node.children() {
        match child.kind() {
            FOR_BINDING => {
                // A `for` glued to the body records `GluedFor` at the `for`'s
                // start, projecting `(error-t)` before the first clause.
                if diag_at(
                    usize::from(child.text_range().start()),
                    DiagnosticKind::GluedFor,
                ) {
                    clauses.push("(error-t)".to_string());
                }
                clauses.push(project_for_binding_node(&child));
            }
            COMPREHENSION_IF => {
                if let (Some(cond), Some(last)) = (first_node(&child), clauses.last().cloned()) {
                    let n = clauses.len();
                    clauses[n - 1] = format!("(filter {last} {})", project(&cond));
                }
            }
            _ if body.is_empty() => body = project(&child),
            _ => {}
        }
    }
    let mut parts = vec![body];
    parts.extend(clauses);
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
    // Split the clause's specs on top-level commas (kept as tokens). One spec
    // projects directly; several become a `(cartesian_iterator …)`.
    let mut specs: Vec<Vec<SyntaxElement>> = vec![Vec::new()];
    for el in binding.children_with_tokens() {
        match &el {
            NodeOrToken::Token(t) if t.kind() == COMMA => specs.push(Vec::new()),
            NodeOrToken::Token(t) if is_drop_token(t.kind()) => {}
            _ => specs.last_mut().expect("non-empty").push(el),
        }
    }
    let projected: Vec<String> = specs.iter().map(|s| project_for_spec(s)).collect();
    match projected.as_slice() {
        [one] => one.clone(),
        _ => sexp("cartesian_iterator", projected),
    }
}

fn project_for_spec(elems: &[SyntaxElement]) -> String {
    // `j = 1:3` keeps a proper ASSIGNMENT_EXPR; `i in xs` keeps the iterator as
    // loose passthrough tokens after an `in` keyword-identifier.
    if let [NodeOrToken::Node(n)] = elems
        && n.kind() == ASSIGNMENT_EXPR
    {
        return project(n);
    }
    // Otherwise the loose `var in iter` form: split on the `in`/`∈` token.
    let split = elems
        .iter()
        .position(|el| matches!(el, NodeOrToken::Token(t) if t.text() == "in" || t.text() == "∈"));
    match split {
        Some(idx) => {
            let var = project_flat(elems[..idx].to_vec());
            let iter = project_flat(elems[idx + 1..].to_vec());
            format!("(= {var} {iter})")
        }
        None => project_flat(elems.to_vec()),
    }
}

// --- Control flow ----------------------------------------------------------

/// The condition slot of an `if`/`elseif` whose `CONDITION` node is absent. An
/// empty condition (`if end`, `if; end`) is recovery: JuliaSyntax synthesizes a
/// zero-width `(error)`, recorded here as a `MissingCondition` diagnostic anchored
/// at the opening keyword. Without the diagnostic the slot stays empty (defensive;
/// the parser always records one when the condition is missing).
fn missing_condition(node: &SyntaxNode) -> String {
    if diag_count_from(keyword_start(node), DiagnosticKind::MissingCondition) > 0 {
        "(error)".to_string()
    } else {
        String::new()
    }
}

fn project_if(node: &SyntaxNode) -> String {
    let cond = node
        .children()
        .find(|c| c.kind() == CONDITION)
        .map(|c| project(&c))
        .unwrap_or_else(|| missing_condition(node));
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
    // A trailing-junk `ERROR` glued after the then-block is a sibling of the
    // block inside the `if` (`if c\n x y\n end` ⇒ `(if c (block x) (error-t y))`).
    if let Some(err) = node.children().find(|c| c.kind() == ERROR) {
        parts.push(project(&err));
    }
    // `else if` recovery: a zero-width `(error-t)` for the missing else block
    // sits between the then-block and the recovered `elseif` clause.
    for _ in 0..diag_count_from(keyword_start(node), DiagnosticKind::ElseIf) {
        parts.push("(error-t)".to_string());
    }
    if let Some(tail) = project_if_tail(&clauses) {
        parts.push(tail);
    }
    push_trailing_errors(node, &mut parts);
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
                .unwrap_or_else(|| missing_condition(first));
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
                // The catch-variable is the first child node before the body
                // block; it may be a plain NAME, a `$`-interpolation, or a
                // `var"…"` non-standard identifier. Absent ⇒ `false`. A
                // non-identifier variable (`catch e+3`) is flagged invalid by
                // the post-build walk and error-wrapped here.
                let var = clause
                    .children()
                    .find(|c| c.kind() != BLOCK)
                    .map(|c| {
                        let projected = project(&c);
                        if diag_at(
                            usize::from(c.text_range().start()),
                            DiagnosticKind::CatchVarNotIdentifier,
                        ) {
                            format!("(error {projected})")
                        } else {
                            projected
                        }
                    })
                    .unwrap_or_else(|| "false".to_string());
                let block = project_block_child(&clause);
                parts.push(format!("(catch {var} {block})"));
            }
            FINALLY_CLAUSE => parts.push(format!("(finally {})", project_block_child(&clause))),
            ELSE_CLAUSE => {
                // `else` without a preceding `catch` is error-recovery: the else
                // block is wrapped in an `(error …)` node in the CST.
                if let Some(err) = clause.children().find(|c| c.kind() == ERROR) {
                    parts.push(format!("(else {})", project(&err)));
                } else {
                    parts.push(format!("(else {})", project_block_child(&clause)));
                }
            }
            _ => {}
        }
    }
    // Truncation markers land in document order: a missing `catch`/`finally`
    // handler then a missing `end` (`try x` ⇒ `(try (block x) (error-t) (error-t))`).
    // Both diagnostics anchor at the `try` keyword.
    let kw = keyword_start(node);
    for _ in 0..diag_count_from(kw, DiagnosticKind::MissingTryHandler) {
        parts.push("(error-t)".to_string());
    }
    for _ in 0..diag_count_from(kw, DiagnosticKind::MissingEnd) {
        parts.push("(error-t)".to_string());
    }
    sexp("try", parts)
}

fn project_struct(node: &SyntaxNode) -> String {
    let mutable = node
        .children_with_tokens()
        .any(|el| el.kind() == MUTABLE_KW);
    let head = if mutable { "struct-mut" } else { "struct" };
    let mut parts = vec![project_signature(node), project_block_child(node)];
    push_trailing_errors(node, &mut parts);
    sexp(head, parts)
}

fn project_primitive(node: &SyntaxNode) -> String {
    // `(primitive <spec> <bits>)`: the spec is the `SIGNATURE` child, the bit
    // size is the sibling expression node that follows it.
    let spec = project_signature(node);
    let size = node
        .children()
        .find(|c| c.kind() != SIGNATURE)
        .map(|c| project(&c))
        .unwrap_or_default();
    sexp("primitive", vec![spec, size])
}

fn project_module(node: &SyntaxNode) -> String {
    let bare = node
        .children_with_tokens()
        .any(|el| el.kind() == BAREMODULE_KW);
    let head = if bare { "module-bare" } else { "module" };
    let mut parts = vec![project_signature(node), project_block_child(node)];
    push_trailing_errors(node, &mut parts);
    sexp(head, parts)
}

fn project_quote_sym(node: &SyntaxNode) -> String {
    // `:foo`/`:(expr)` → `(quote-: …)`. The quoted form is the first significant
    // child after the `:` — a `NAME`/paren node, or a bare keyword token.
    // Whitespace between the `:` and the symbol records a `QuoteColonWhitespace`
    // diagnostic at the `:`'s end and projects a leading `(error-t)`
    // (`: foo` ⇒ `(quote-: (error-t) foo)`).
    let mut prefix = "";
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Node(n) => return format!("(quote-: {prefix}{})", project(&n)),
            NodeOrToken::Token(t) if t.kind() == COLON => {
                if diag_at(
                    usize::from(t.text_range().end()),
                    DiagnosticKind::QuoteColonWhitespace,
                ) {
                    prefix = "(error-t) ";
                }
            }
            // `:(end)`/`:(else)`/`:(catch)` — a quote-paren whose body can't start
            // an expression. The `(` is a loose child carrying an
            // `EmptyQuoteParen` diagnostic; the quoted form is a zero-width
            // `(error-t)` (the keyword spilled to the trailing-junk driver).
            NodeOrToken::Token(t)
                if t.kind() == LPAREN
                    && diag_at(
                        usize::from(t.text_range().end()),
                        DiagnosticKind::EmptyQuoteParen,
                    ) =>
            {
                return format!("(quote-: {prefix}(error-t))");
            }
            NodeOrToken::Token(t) if is_trivia(t.kind()) => continue,
            NodeOrToken::Token(t) => return format!("(quote-: {prefix}{})", t.text()),
        }
    }
    "(quote-:)".to_string()
}

fn project_let(node: &SyntaxNode) -> String {
    let bindings = match node.children().find(|c| c.kind() == LET_BINDINGS) {
        Some(b) => sexp("block", project_let_bindings(&b)),
        None => "(block)".to_string(),
    };
    let mut parts = vec![bindings, project_block_child(node)];
    push_trailing_errors(node, &mut parts);
    sexp("let", parts)
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
    let mut parts = vec![call, params, block];
    push_trailing_errors(node, &mut parts);
    sexp("do", parts)
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
    // Each item is a bare name (`export x`), an interpolation/macro name, or a
    // parenthesized item. A parenthesized item that wraps a single symbol is
    // unwrapped by `project` (`(x)` ⇒ `x`, `(+)` ⇒ `+`); anything else is flagged
    // `InvalidExportItem` by the post-build walk and error-wrapped here (`(x::T)`
    // ⇒ `(error (::-i x T))`, `(x, y)` ⇒ `(error (tuple-p x y))`).
    let items: Vec<String> = significant(node)
        .into_iter()
        .filter_map(|el| match el {
            NodeOrToken::Node(n) if matches!(n.kind(), PAREN_EXPR | TUPLE_EXPR) => {
                let projected = project(&n);
                if diag_at(
                    usize::from(n.text_range().start()),
                    DiagnosticKind::InvalidExportItem,
                ) {
                    Some(format!("(error {projected})"))
                } else {
                    Some(projected)
                }
            }
            _ => name_run_item(el),
        })
        .collect();
    sexp("export", items)
}

fn project_public(node: &SyntaxNode) -> String {
    // The leading `public` contextual keyword is a plain identifier token in the
    // CST (it stays an identifier elsewhere), so drop the first element before
    // reading the name list exactly like `export`. A name may itself be a
    // contextual keyword (`public export`), which `significant` would drop, so
    // filter only trivia and the comma separators here.
    let items: Vec<String> = node
        .children_with_tokens()
        .filter(|el| match el {
            NodeOrToken::Node(_) => true,
            NodeOrToken::Token(t) => !is_trivia(t.kind()) && t.kind() != COMMA,
        })
        .skip(1)
        .filter_map(name_run_item)
        .collect();
    sexp("public", items)
}

fn project_import(head: &str, node: &SyntaxNode) -> String {
    // `import A` / `using A.B` / `import A: b, c as d`. The path tree is built by
    // the parser: each clause is an `IMPORT_PATH` or `IMPORT_ALIAS` node, and a
    // top-level `:` token (when present) splits the base path from the list of
    // imported names. Read those nodes directly.
    // The base/names `:` split is the *first* separator after the base path; a
    // `:` after a comma is recovery (no grouping), e.g. `import A, B: y` ⇒
    // `(import (importpath A) (importpath B) (error-t (importpath y)))`.
    let mut first_sep: Option<SyntaxKind> = None;
    let mut clauses: Vec<String> = Vec::new();
    for el in node.children_with_tokens() {
        match el {
            // `ERROR` wraps an invalid `as` rename (`using A as B`, projected
            // `(error …)`) or a clause after a recovery `:` (`import A, B: y`,
            // projected `(error-t …)` via the `ImportRecoveryColon` diagnostic).
            NodeOrToken::Node(n) if matches!(n.kind(), IMPORT_PATH | IMPORT_ALIAS | ERROR) => {
                clauses.push(project(&n));
            }
            NodeOrToken::Token(t)
                if matches!(t.kind(), COLON | COMMA)
                    && !clauses.is_empty()
                    && first_sep.is_none() =>
            {
                first_sep = Some(t.kind());
            }
            _ => {}
        }
    }

    if first_sep == Some(COLON) && !clauses.is_empty() {
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
                // After a name, a `...` is a separator dot fused with the `..`
                // range operator as a component (`import A...` → `(importpath A ..)`).
                DOT_DOT_DOT if seen_name => parts.push("..".to_string()),
                // Separator dots/colons between components carry no meaning here.
                DOT | DOT_DOT | DOT_DOT_DOT | COLON => {}
                // An operator-symbol name component (`import A.==`, `import A: +`).
                // A fused dotted operator (`.==`, `.⋆`) carries a leading `.`. After
                // a name it is a separator we strip (`import A.==` → `… ==`); before
                // any name it is a *relative-import* dot emitted on its own (`import
                // .==` → `(importpath . ==)`), so keep one `.` part in that case.
                k if is_operator(k) => {
                    if !seen_name && t.text().starts_with('.') {
                        parts.push(".".to_string());
                    }
                    parts.push(t.text().trim_start_matches('.').to_string());
                    seen_name = true;
                }
                _ => {}
            },
            NodeOrToken::Node(n) if n.kind() == NAME => {
                parts.push(name_text(&n));
                seen_name = true;
            }
            // A quoted operator symbol component (`import A.:+` → `(quote-: +)`).
            NodeOrToken::Node(n) if n.kind() == QUOTE_SYM => {
                parts.push(project_quote_sym(&n));
                seen_name = true;
            }
            // A parenthesized quoted symbol (`import A.(:+)` → `(quote-: +)`); the
            // paren unwraps to its inner quote.
            NodeOrToken::Node(n) if n.kind() == PAREN_EXPR => {
                parts.push(project(&n));
                seen_name = true;
            }
            // An interpolated path root (`import $A` → `($ A)`).
            NodeOrToken::Node(n) if n.kind() == INTERPOLATION => {
                parts.push(project(&n));
                seen_name = true;
            }
            // A macro-name component (`import A.@x` → `(importpath A @x)`).
            NodeOrToken::Node(n) if n.kind() == MACRO_NAME => {
                parts.push(project_macro_name(&n));
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
    // An invalid bracketed macro name (`@[x]` ⇒ `(error (vect x))`, `@{x}` ⇒
    // `(error (braces x))`): the parser parses the bracketed expression as a child
    // node and records `InvalidMacroName` at its start. Error-wrap its projection.
    if let Some(child) = node.children().next()
        && diag_at(
            usize::from(child.text_range().start()),
            DiagnosticKind::InvalidMacroName,
        )
    {
        return format!("(error {})", project(&child));
    }

    // A `var"…"` non-standard identifier name (`@var"#"` ⇒ `(var @#)`): the `@`
    // sigil prefixes the identifier content. JuliaSyntax folds the `@` into the
    // `var` name itself rather than wrapping it as a separate macro-name token.
    let var_name = node
        .children()
        .find(|c| c.kind() == NONSTANDARD_IDENTIFIER)
        .map(|c| format!("(var @{})", raw_content(&c)));

    // Trailing form (`A.@x`, `A.B.@x`, `$A.@x`, `A.$B.@x`): the module path is a
    // single leading node (`NAME`, `BINARY_EXPR`, or `INTERPOLATION`) that
    // `project` already nests correctly, then `. @ name`. Reuse `project` for the
    // module so dotted access and interpolation splits (`(inert ($ B))`) stay
    // consistent with plain field access.
    if let Some(module) = node
        .children()
        .find(|c| matches!(c.kind(), NAME | BINARY_EXPR | INTERPOLATION))
    {
        let name = match &var_name {
            Some(v) => v.clone(),
            None => format!("@{}", macro_name_after_at(node)),
        };
        return format!("(. {} (quote {name}))", project(&module));
    }

    // Prefix form with a `var"…"` name and no module (`@var"#"` ⇒ `(var @#)`).
    if let Some(v) = var_name {
        return v;
    }

    // Prefix form (`@m`, `@A.x`, `@A.B.x`): a flat run of component tokens after
    // the `@`. The last component is the macro name; the rest form the module
    // path, nested left-to-right the same way field access is.
    let comps: Vec<String> = node
        .children_with_tokens()
        .filter_map(|el| match el {
            NodeOrToken::Token(t) if t.kind() == IDENT => Some(t.text().to_string()),
            // An operator, `$`, or keyword name token (`@+`, `@$`, `@end`). The
            // qualifying `.` and broadcast `@.` dot are excluded so they don't
            // count as a name component.
            NodeOrToken::Token(t) if is_macro_name_part_token(t.kind()) => {
                Some(t.text().to_string())
            }
            _ => None,
        })
        .collect();

    match comps.as_slice() {
        // `@.` — broadcast macro: `@` then the lone broadcast dot, no ident.
        [] => "@.".to_string(),
        // Simple `@m`.
        [one] => format!("@{one}"),
        // Qualified `@Mod.mac` / `@A.B.x` → `(. <module> (quote @macro))`.
        rest => {
            let (macro_name, module) = rest.split_last().unwrap();
            let mut path = module[0].clone();
            for c in &module[1..] {
                path = format!("(. {path} (quote {c}))");
            }
            format!("(. {path} (quote @{macro_name}))")
        }
    }
}

/// The macro-name component text in a trailing-form `MACRO_NAME` — the token
/// immediately after the `@` (`A.B.@x` → `x`).
fn macro_name_after_at(node: &SyntaxNode) -> String {
    let mut after_at = false;
    for el in node.children_with_tokens() {
        if let NodeOrToken::Token(t) = el {
            if after_at {
                return t.text().to_string();
            }
            if t.kind() == AT {
                after_at = true;
            }
        }
    }
    String::new()
}

/// Whether `kind` is a macro-name component token other than an identifier — an
/// operator name (`+`, `!`, `..`), the `$` sigil, or a keyword (`end`). `DOT` is
/// excluded: it is the qualifier dot or the broadcast `@.`, never a name part.
fn is_macro_name_part_token(kind: SyntaxKind) -> bool {
    (is_operator(kind) && kind != DOT) || kind == DOLLAR || is_keyword(kind)
}

// --- Literals / strings ----------------------------------------------------

fn project_literal(node: &SyntaxNode) -> String {
    let toks: Vec<_> = node
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .collect();
    // A folded signed numeric literal (`-2`, `+2.0`): the leading `+`/`-` sign
    // token precedes the number. `-` stays in the literal; `+` is a no-op and is
    // dropped (`+2.0` → `2.0`, matching JuliaSyntax's glued literal).
    if let [sign, num] = toks.as_slice() {
        let n = literal_token_text(num);
        return if sign.text() == "-" {
            format!("-{n}")
        } else {
            n
        };
    }
    match toks.first() {
        Some(tok) => literal_token_text(tok),
        None => "(unsupported LITERAL)".to_string(),
    }
}

/// Render a single literal token the way JuliaSyntax displays it. Integer
/// literals are normalized to their parsed *value* (underscores stripped;
/// hex/octal/binary shown as zero-padded hex at Julia's width tier), matching
/// how JuliaSyntax shows the leaf value rather than the source text — the same
/// value-rendering the string/char paths already do. Floats are left as source
/// text for now (float canonicalization is a separate, deferred follow-up).
fn literal_token_text(tok: &SyntaxToken) -> String {
    match tok.kind() {
        CHAR => project_char(tok),
        TRUE_KW => "true".to_string(),
        FALSE_KW => "false".to_string(),
        INTEGER => tok.text().replace('_', ""),
        HEX_INT => normalize_based_int(tok.text(), 16),
        OCT_INT => normalize_based_int(tok.text(), 8),
        BIN_INT => normalize_based_int(tok.text(), 2),
        _ => tok.text().to_string(),
    }
}

/// Normalize a base-prefixed integer literal (`0x…`/`0o…`/`0b…`) to JuliaSyntax's
/// display: the parsed value as lowercase hex, zero-padded to the width of the
/// unsigned type Julia selects from the digit count. Hex counts 4 bits/digit,
/// binary 1 bit/digit, and octal `bits(leading) + 3·(ndigits−1)`; that bit count
/// rounds up to the next of {8,16,32,64,128} (UInt8…UInt128). Values wider than
/// 128 bits are `BigInt` (shown as decimal) — not handled here, so the source
/// text is kept (a deferred divergence).
fn normalize_based_int(text: &str, base: u32) -> String {
    let digits: String = text[2..].chars().filter(|&c| c != '_').collect();
    let nbits = match base {
        16 => 4 * digits.len(),
        2 => digits.len(),
        _ => octal_bits(&digits),
    };
    let tier_bits = match nbits {
        0..=8 => 8,
        9..=16 => 16,
        17..=32 => 32,
        33..=64 => 64,
        65..=128 => 128,
        _ => return text.replace('_', ""),
    };
    match u128::from_str_radix(&digits, base) {
        Ok(v) => format!("0x{:0width$x}", v, width = tier_bits / 4),
        Err(_) => text.replace('_', ""),
    }
}

/// Bit width of an octal literal's digit string: the leading digit contributes
/// only its significant bits (`0`→0, `1`→1, `2–3`→2, `4–7`→3) and each remaining
/// digit a full 3 bits, matching Julia's octal type selection (`0o200`→`0x80`,
/// `0o400`→`0x0100`, leading zeros widen: `0o00007`→`0x0007`).
fn octal_bits(digits: &str) -> usize {
    let lead = digits.as_bytes()[0] - b'0';
    let lead_bits = (8 - lead.leading_zeros()) as usize;
    lead_bits + 3 * (digits.len() - 1)
}

/// Project a char-literal token (`'…'`) to `(char '…')`, decoding the source
/// escapes to a single codepoint and re-displaying it the way JuliaSyntax shows
/// a `Char`. Content `decode_char` rejects is one of JuliaSyntax's error shapes:
/// empty `''` ⇒ `(char (error))`, a malformed escape `'\xq'` ⇒
/// `(char (ErrorInvalidEscapeSequence))`, a lone non-UTF-8 byte `'\xff'` stays a
/// valid one-byte `Char`, and anything else multi-codepoint `'ab'` ⇒
/// `(char (ErrorOverLongCharacter))`.
fn project_char(tok: &SyntaxToken) -> String {
    let text = tok.text();
    // An unterminated char (no closing quote) is flagged with `UnterminatedLiteral`
    // at the opening quote. JuliaSyntax reads it as a char and recovers with a
    // missing-close marker: empty content `'` ⇒ `(char (error))` (the bare empty
    // shape, no `(error-t)`), non-empty `'a` ⇒ `(char 'a' (error-t))`.
    let start = usize::from(tok.text_range().start());
    if diag_at(start, DiagnosticKind::UnterminatedLiteral) {
        let inner = text.strip_prefix('\'').unwrap_or(text);
        if inner.is_empty() {
            return "(char (error))".to_string();
        }
        return match decode_char_body(inner) {
            Some(c) => format!("(char '{}' (error-t))", display_char(c)),
            None => match classify_char_body(inner) {
                CharError::Empty => "(char (error))".to_string(),
                CharError::BadEscape => "(char (ErrorInvalidEscapeSequence) (error-t))".to_string(),
                CharError::OverLong => "(char (ErrorOverLongCharacter) (error-t))".to_string(),
                CharError::SingleByte(b) => format!("(char '\\x{b:02x}' (error-t))"),
            },
        };
    }
    match decode_char(text) {
        Some(c) => format!("(char '{}')", display_char(c)),
        None => match classify_char_error(text) {
            CharError::Empty => "(char (error))".to_string(),
            CharError::BadEscape => "(char (ErrorInvalidEscapeSequence))".to_string(),
            CharError::OverLong => "(char (ErrorOverLongCharacter))".to_string(),
            CharError::SingleByte(b) => format!("(char '\\x{b:02x}')"),
        },
    }
}

/// Why `decode_char` rejected a char-literal body, mapped to JuliaSyntax's
/// error-token classification.
enum CharError {
    /// Empty body (`''`).
    Empty,
    /// A malformed backslash escape (`'\xq'`, `'\q'`, `'\400'`).
    BadEscape,
    /// A single byte that is not valid UTF-8 (`'\xff'`, `'\377'`) — still a
    /// valid one-byte Julia `Char`, displayed `\xNN`.
    SingleByte(u8),
    /// A well-formed body holding more than one codepoint (`'ab'`, `'\xff\xff'`).
    OverLong,
}

/// Classify a char-literal body that `decode_char` could not reduce to a single
/// codepoint. Mirrors `decode_char`'s byte accumulation but reports *why* it
/// failed; a malformed escape wins over over-long (matching JuliaSyntax).
fn classify_char_error(text: &str) -> CharError {
    let inner = text
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .unwrap_or("");
    classify_char_body(inner)
}

/// Classify a char-literal *body* (the text between the quotes, already stripped).
/// Shared by the terminated (`classify_char_error`) and unterminated paths.
fn classify_char_body(inner: &str) -> CharError {
    if inner.is_empty() {
        return CharError::Empty;
    }
    let mut bytes: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4];
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        } else if decode_escape_into(&mut chars, &mut bytes).is_none() {
            return CharError::BadEscape;
        }
    }
    match bytes.as_slice() {
        [b] => CharError::SingleByte(*b),
        _ => CharError::OverLong,
    }
}

/// Decode a char literal's source text (`'\xce\xb1'`) to its single codepoint.
/// Byte escapes (`\xNN`, octal) and literal characters accumulate as bytes that
/// are then read as UTF-8; `\u`/`\U` and named escapes contribute a codepoint
/// directly. Returns `None` for empty, malformed, or over-long content.
fn decode_char(text: &str) -> Option<char> {
    let inner = text.strip_prefix('\'')?.strip_suffix('\'')?;
    decode_char_body(inner)
}

/// Decode a char-literal *body* (already stripped of quotes) to its single
/// codepoint. Shared by the terminated and unterminated paths.
fn decode_char_body(inner: &str) -> Option<char> {
    if inner.is_empty() {
        return None;
    }
    let mut bytes: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4];
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        decode_escape_into(&mut chars, &mut bytes)?;
    }
    let s = std::str::from_utf8(&bytes).ok()?;
    let mut it = s.chars();
    let first = it.next()?;
    it.next().is_none().then_some(first)
}

/// Decode a single backslash escape (the backslash already consumed) into `bytes`,
/// the way Julia reads a `Char`/`String` literal: byte escapes (`\xNN`, octal) push
/// one byte; `\u`/`\U` push a codepoint's UTF-8 bytes; named escapes push their
/// control byte. Returns `None` on a malformed or unknown escape.
fn decode_escape_into(
    chars: &mut std::iter::Peekable<std::str::Chars>,
    bytes: &mut Vec<u8>,
) -> Option<()> {
    let mut buf = [0u8; 4];
    match chars.next()? {
        'n' => bytes.push(b'\n'),
        't' => bytes.push(b'\t'),
        'r' => bytes.push(b'\r'),
        'a' => bytes.push(0x07),
        'b' => bytes.push(0x08),
        'f' => bytes.push(0x0c),
        'v' => bytes.push(0x0b),
        'e' => bytes.push(0x1b),
        '\\' => bytes.push(b'\\'),
        '\'' => bytes.push(b'\''),
        '"' => bytes.push(b'"'),
        '$' => bytes.push(b'$'),
        'x' => bytes.push(take_hex(chars, 2)? as u8),
        'u' => {
            let cp = char::from_u32(take_hex(chars, 4)?)?;
            bytes.extend_from_slice(cp.encode_utf8(&mut buf).as_bytes());
        }
        'U' => {
            let cp = char::from_u32(take_hex(chars, 8)?)?;
            bytes.extend_from_slice(cp.encode_utf8(&mut buf).as_bytes());
        }
        d @ '0'..='7' => {
            let mut val = d.to_digit(8)?;
            for _ in 0..2 {
                match chars.peek().and_then(|c| c.to_digit(8)) {
                    Some(o) => {
                        val = val * 8 + o;
                        chars.next();
                    }
                    None => break,
                }
            }
            // Julia caps an octal escape at one byte; `\400` and up overflow.
            bytes.push(u8::try_from(val).ok()?);
        }
        _ => return None,
    }
    Some(())
}

/// Consume up to `max` hex digits from `chars` and return their value; `None`
/// if there is not at least one digit.
fn take_hex(chars: &mut std::iter::Peekable<std::str::Chars>, max: usize) -> Option<u32> {
    let mut val = 0u32;
    let mut n = 0;
    while n < max {
        match chars.peek().and_then(|c| c.to_digit(16)) {
            Some(d) => {
                val = val * 16 + d;
                chars.next();
                n += 1;
            }
            None => break,
        }
    }
    (n > 0).then_some(val)
}

/// JuliaSyntax's escape for a control/non-printable codepoint, shared by `Char`
/// and `String` show: the named control escapes, a `\xNN` form for the remaining
/// C0 controls and DEL, a `\u`/`\U` form for other non-printable codepoints.
/// Returns `None` for a printable char (which shows as itself).
fn control_escape(c: char) -> Option<String> {
    Some(match c {
        '\0' => "\\0".to_string(),
        '\u{7}' => "\\a".to_string(),
        '\u{8}' => "\\b".to_string(),
        '\t' => "\\t".to_string(),
        '\n' => "\\n".to_string(),
        '\u{b}' => "\\v".to_string(),
        '\u{c}' => "\\f".to_string(),
        '\r' => "\\r".to_string(),
        '\u{1b}' => "\\e".to_string(),
        c if (c as u32) < 0x20 || c as u32 == 0x7f => format!("\\x{:02x}", c as u32),
        c if c.is_control() => {
            let v = c as u32;
            if v <= 0xffff {
                format!("\\u{v:04x}")
            } else {
                format!("\\U{v:08x}")
            }
        }
        _ => return None,
    })
}

/// Render a single codepoint as JuliaSyntax shows a `Char` body: `\\`/`\'`, the
/// shared control escapes, else literal.
fn display_char(c: char) -> String {
    match c {
        '\\' => "\\\\".to_string(),
        '\'' => "\\'".to_string(),
        c => control_escape(c).unwrap_or_else(|| c.to_string()),
    }
}

fn project_string(node: &SyntaxNode) -> String {
    // String macro: a prefix (`r`, `raw`, `b`, `v`) makes it a raw `@<p>_str`
    // macrocall rather than an interpolating `(string …)`.
    if let Some(prefix) = string_token(node, STRING_PREFIX) {
        // A triple-quoted raw string still gets JuliaSyntax's dedent + per-line
        // chunking; only the unescaping differs (raw content's backslashes and
        // quotes stay literal), so its chunks are display-escaped as raw bytes.
        let body = if matches!(string_token(node, STRING_DELIM_OPEN), Some(d) if d.len() >= 3) {
            sexp(
                "string-s-r",
                with_error_trivia(node, triple_string_parts(node, true)),
            )
        } else {
            sexp(
                "string-r",
                with_error_trivia(node, vec![quote_raw(&raw_content(node))]),
            )
        };
        let mut parts = vec![format!("@{prefix}_str"), body];
        if let Some(suffix) = string_token(node, STRING_SUFFIX) {
            parts.push(quote_raw(&suffix));
        } else if let Some(num) = numeric_suffix(node) {
            // A glued numeric suffix (`x"s"2`) is an extra macrocall argument,
            // rendered as the numeric literal itself (not a flag string).
            parts.push(num);
        }
        return sexp("macrocall", parts);
    }

    // Triple-quoted strings get JuliaSyntax's dedent + per-line chunking applied
    // to compute their literal value (a faithful encoding of what the literal
    // means, mirroring `SyntaxNode`'s String children).
    if matches!(string_token(node, STRING_DELIM_OPEN), Some(d) if d.len() >= 3) {
        return sexp(
            "string-s",
            with_error_trivia(node, triple_string_parts(node, false)),
        );
    }

    let mut parts = string_parts(node);
    if parts.is_empty() {
        // An empty literal still carries one empty String child (`"" → (string "")`).
        parts.push("\"\"".to_string());
    }
    sexp("string", with_error_trivia(node, parts))
}

/// One piece of a triple-quoted string's processed content: either a literal
/// text chunk (JuliaSyntax keeps one `String` per line) or an interpolation.
enum TripleItem {
    Text(String),
    Interp(String),
}

/// Project a triple-quoted string's content the way JuliaSyntax does: normalize
/// line endings to `\n`, split into per-line chunks, strip the common leading
/// indentation, drop the leading newline right after `"""`, then display-escape.
fn triple_string_parts(node: &SyntaxNode, raw: bool) -> Vec<String> {
    // Build the content as a sequence of lines (split on normalized newlines);
    // each line is a run of text/interpolation items.
    let mut lines: Vec<Vec<TripleItem>> = vec![Vec::new()];
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if t.kind() == STRING_CONTENT => {
                let text = normalize_newlines(t.text());
                let mut segs = text.split('\n');
                if let Some(first) = segs.next() {
                    lines
                        .last_mut()
                        .unwrap()
                        .push(TripleItem::Text(first.to_string()));
                }
                for seg in segs {
                    lines.push(vec![TripleItem::Text(seg.to_string())]);
                }
            }
            NodeOrToken::Node(n) if n.kind() == INTERPOLATION => {
                lines
                    .last_mut()
                    .unwrap()
                    .push(TripleItem::Interp(project_interpolation(&n)));
            }
            _ => {}
        }
    }

    chunk_triple_lines(lines, raw)
}

/// A triple-backtick command's content as raw text lines (commands keep
/// `$`-interpolation as literal source, so every child — content and
/// interpolation alike — is folded into one raw string before dedenting), then
/// JuliaSyntax's dedent + per-line chunking is applied with raw escaping.
fn triple_cmd_parts(node: &SyntaxNode) -> Vec<String> {
    let body = normalize_newlines(&cmd_raw_body(node));
    let lines: Vec<Vec<TripleItem>> = body
        .split('\n')
        .map(|seg| vec![TripleItem::Text(seg.to_string())])
        .collect();
    chunk_triple_lines(lines, true)
}

/// Apply JuliaSyntax's triple-quoted dedent and per-line chunking to a sequence
/// of content lines, emitting one display-escaped `String`/interpolation part per
/// surviving chunk.
fn chunk_triple_lines(lines: Vec<Vec<TripleItem>>, raw: bool) -> Vec<String> {
    let last_idx = lines.len() - 1;

    // Common leading whitespace over lines 2..end. Whitespace-only lines are
    // skipped except the last (the closing-delimiter line), which always counts.
    let candidates: Vec<String> = lines
        .iter()
        .enumerate()
        .filter(|(i, line)| *i != 0 && (!line_is_blank(line) || *i == last_idx))
        .map(|(_, line)| line_lead_ws(line))
        .collect();
    let dedent_len = common_prefix_len(&candidates);

    let mut chunks: Vec<TripleItem> = Vec::new();
    for (i, mut line) in lines.into_iter().enumerate() {
        let has_newline = i != last_idx;
        if i == 0 {
            // The opening line is never dedented; an empty one (`"""` directly
            // followed by a newline) is dropped along with its newline.
            if line_is_empty(&line) {
                continue;
            }
        } else if let Some(TripleItem::Text(t)) = line.first_mut() {
            *t = strip_leading_ws(t, dedent_len);
        }
        if has_newline {
            match line.last_mut() {
                Some(TripleItem::Text(t)) => t.push('\n'),
                _ => line.push(TripleItem::Text("\n".to_string())),
            }
        }
        chunks.extend(line);
    }

    let mut out: Vec<String> = Vec::new();
    for chunk in chunks {
        match chunk {
            TripleItem::Text(t) if t.is_empty() => {}
            TripleItem::Text(t) => out.push(format!("\"{}\"", escape_display(&t, raw))),
            TripleItem::Interp(s) => out.push(s),
        }
    }
    if out.is_empty() {
        out.push("\"\"".to_string());
    }
    out
}

/// Collapse CRLF and lone CR line endings to LF (JuliaSyntax normalizes both).
fn normalize_newlines(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

/// Leading run of spaces/tabs at the start of a line (empty if it opens with an
/// interpolation).
fn line_lead_ws(line: &[TripleItem]) -> String {
    match line.first() {
        Some(TripleItem::Text(t)) => t.chars().take_while(|c| *c == ' ' || *c == '\t').collect(),
        _ => String::new(),
    }
}

/// A line with no interpolation whose text is entirely spaces/tabs (or empty).
fn line_is_blank(line: &[TripleItem]) -> bool {
    line.iter().all(|it| match it {
        TripleItem::Text(t) => t.chars().all(|c| c == ' ' || c == '\t'),
        TripleItem::Interp(_) => false,
    })
}

/// A line with no interpolation and no text at all.
fn line_is_empty(line: &[TripleItem]) -> bool {
    line.iter()
        .all(|it| matches!(it, TripleItem::Text(t) if t.is_empty()))
}

/// Longest common prefix length (in bytes) over the given whitespace strings.
fn common_prefix_len(strs: &[String]) -> usize {
    let mut iter = strs.iter();
    let Some(first) = iter.next() else {
        return 0;
    };
    let mut len = first.len();
    for s in iter {
        len = first
            .bytes()
            .zip(s.bytes())
            .take(len)
            .take_while(|(a, b)| a == b)
            .count();
    }
    len
}

/// Strip up to `n` leading whitespace bytes (spaces/tabs are single-byte).
fn strip_leading_ws(t: &str, n: usize) -> String {
    let mut idx = 0;
    for c in t.chars() {
        if idx >= n || (c != ' ' && c != '\t') {
            break;
        }
        idx += 1;
    }
    t[idx..].to_string()
}

/// Escape the control characters JuliaSyntax shows as backslash escapes. For an
/// interpolating string the source escapes (`\n`, `\\`) already display as
/// themselves, so only literal control chars need escaping; for a `raw` string
/// the content is literal bytes, so backslashes/quotes/`$` are escaped too.
fn escape_display(s: &str, raw: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\\' if raw => out.push_str("\\\\"),
            '"' if raw => out.push_str("\\\""),
            '$' if raw => out.push_str("\\$"),
            _ => out.push(c),
        }
    }
    out
}

/// A `var"name"` non-standard identifier projects to `(var name)`, heading the
/// node with the raw delimited content. (Escape-processing of the name —
/// `var"\""` → `(var ")` — follows Julia's raw-string rules and is deferred, so
/// only escape-free names match the oracle today.)
fn project_var(node: &SyntaxNode) -> String {
    let content = unescape_raw_string(&raw_content(node));
    let parts = if content.is_empty() {
        vec![]
    } else {
        vec![content]
    };
    sexp("var", with_error_trivia(node, parts))
}

/// Unescape a raw-string body the way Julia's `unescape_raw_string` does: a run
/// of `n` backslashes immediately before a `"` (or at the end of the body, where
/// the closing delimiter is the implied `"`) yields `n / 2` backslashes, plus a
/// literal `"` when `n` is odd (`\"` ⇒ `"`, `\\\"` ⇒ `\` then `"`, trailing
/// `\\` ⇒ `\`); any other backslash run is literal. Used for `var"…"` identifier
/// content, whose name is the unescaped value rather than the raw source bytes.
fn unescape_raw_string(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            let mut run = 0;
            while i + run < bytes.len() && bytes[i + run] == b'\\' {
                run += 1;
            }
            let at_end = i + run >= bytes.len();
            let before_quote = !at_end && bytes[i + run] == b'"';
            if before_quote || at_end {
                for _ in 0..run / 2 {
                    out.push('\\');
                }
                i += run;
                if run % 2 == 1 {
                    out.push('"');
                    i += 1;
                }
            } else {
                for _ in 0..run {
                    out.push('\\');
                }
                i += run;
            }
        } else {
            let ch = s[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

fn project_cmd(node: &SyntaxNode) -> String {
    // Command literals lower to a macrocall over a raw cmdstring. A bare literal
    // (`` `cmd` ``) uses the built-in `core_@cmd`; a prefix names a custom command
    // macro (`` x`str` `` ⇒ `@x_cmd`). Commands are raw: JuliaSyntax keeps
    // `$`-interpolation as literal source (escaped `\$`) and defers expansion to
    // the macro, so reconstruct the raw body from content and interpolation text.
    let triple = matches!(string_token(node, CMD_DELIM_OPEN), Some(d) if d.len() >= 3);
    let head = match string_token(node, STRING_PREFIX) {
        Some(prefix) => format!("@{prefix}_cmd"),
        None => "core_@cmd".to_string(),
    };
    // A triple-quoted command gets JuliaSyntax's dedent + per-line chunking (raw),
    // matching the triple-string path; a single-quoted one is one raw chunk.
    let body = if triple {
        sexp(
            "cmdstring-s-r",
            with_error_trivia(node, triple_cmd_parts(node)),
        )
    } else {
        sexp(
            "cmdstring-r",
            with_error_trivia(node, vec![quote_raw(&cmd_raw_body(node))]),
        )
    };
    let mut parts = vec![head, body];
    if let Some(suffix) = string_token(node, STRING_SUFFIX) {
        // A flag glued to the closing delimiter (`` x`str`flag ``) is an extra
        // macrocall argument.
        parts.push(quote_raw(&suffix));
    }
    sexp("macrocall", parts)
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
    // JuliaSyntax projects a single-quoted string to its *value*: escapes are
    // decoded and re-shown JuliaSyntax-style, and a `\`-newline line continuation
    // splits the content into separate `String` chunks (dropping the backslash,
    // the newline, and the following indentation). On any malformed escape we fall
    // back to echoing the raw source — those are deferred error shapes that stay
    // un-allowlisted, matching the prior behavior.
    decoded_string_parts(node).unwrap_or_else(|| raw_string_parts(node))
}

fn decoded_string_parts(node: &SyntaxNode) -> Option<Vec<String>> {
    let mut parts = Vec::new();
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if t.kind() == STRING_CONTENT => {
                match decode_string_chunks(t.text()) {
                    Ok(chunks) => {
                        for chunk in chunks {
                            if !chunk.is_empty() {
                                parts.push(format!("\"{}\"", escape_string_value(&chunk)));
                            }
                        }
                    }
                    // A malformed escape collapses the whole content token to one
                    // JuliaSyntax error part — `"ok\xqq"` ⇒ `(string
                    // (ErrorInvalidEscapeSequence))`, the valid surrounding text is
                    // dropped, matching JuliaSyntax's per-`String`-token error.
                    Err(StringDecodeError::BadEscape) => {
                        parts.push("(ErrorInvalidEscapeSequence)".to_string());
                    }
                    // Valid bytes that aren't UTF-8 (`"\xff"`) stay a faithful
                    // byte-string: fall back to the raw-source rendering for the
                    // whole literal (matches JuliaSyntax's `\xff` show).
                    Err(StringDecodeError::BadUtf8) => return None,
                }
            }
            NodeOrToken::Node(n) if n.kind() == INTERPOLATION => {
                parts.push(project_interpolation(&n));
            }
            _ => {}
        }
    }
    Some(parts)
}

/// Why `decode_string_chunks` could not reduce a `STRING_CONTENT` token to its
/// literal value. The two failures project differently: a malformed escape is
/// JuliaSyntax's `(ErrorInvalidEscapeSequence)`, while well-formed bytes that
/// aren't UTF-8 are a valid byte-string we render from the raw source.
enum StringDecodeError {
    /// A malformed backslash escape (`\xq`, `\q`, `\400`).
    BadEscape,
    /// Well-formed bytes that don't decode as UTF-8 (`\xff`).
    BadUtf8,
}

fn raw_string_parts(node: &SyntaxNode) -> Vec<String> {
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

/// Decode one `STRING_CONTENT` token's source into its literal value, split into
/// chunks at each `\`-newline line continuation (JuliaSyntax keeps one `String`
/// per chunk). A continuation drops the backslash, the newline (`\n`/`\r`/`\r\n`),
/// and the run of spaces/tabs that follow. Returns a `StringDecodeError` on a
/// malformed escape (`BadEscape`) or valid-but-non-UTF-8 bytes (`BadUtf8`).
fn decode_string_chunks(text: &str) -> Result<Vec<String>, StringDecodeError> {
    let mut chunks: Vec<Vec<u8>> = vec![Vec::new()];
    let mut buf = [0u8; 4];
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let last = chunks.last_mut().unwrap();
            last.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.peek() {
            Some('\n') | Some('\r') => {
                let nl = chars.next().unwrap();
                if nl == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                while matches!(chars.peek(), Some(' ') | Some('\t')) {
                    chars.next();
                }
                chunks.push(Vec::new());
            }
            _ => decode_escape_into(&mut chars, chunks.last_mut().unwrap())
                .ok_or(StringDecodeError::BadEscape)?,
        }
    }
    chunks
        .into_iter()
        .map(|c| String::from_utf8(c).map_err(|_| StringDecodeError::BadUtf8))
        .collect()
}

/// Escape a decoded string value the way JuliaSyntax's `String` show does:
/// `\\`/`\"`/`\$`, the shared control escapes, else literal.
fn escape_string_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '$' => out.push_str("\\$"),
            c => match control_escape(c) {
                Some(esc) => out.push_str(&esc),
                None => out.push(c),
            },
        }
    }
    out
}

fn project_interpolation(node: &SyntaxNode) -> String {
    // `$name` → the bare identifier; `$(expr)` → the projected sub-expression. A
    // `$(…)` whose parens hold a multi-value form is invalid: JuliaSyntax renders
    // a block (`$(x;y)`), tuple (`$(x,y)`, empty `$()`), or generator
    // (`$(x for …)`) operand as `(error …)`, flattening block/tuple children and
    // keeping the generator nested. A single expression is a `PAREN_EXPR` the
    // normal `project` unwraps.
    if let Some(inner) = first_node(node) {
        return match inner.kind() {
            PAREN_BLOCK => sexp("error", project_block_args(&inner)),
            TUPLE_EXPR => sexp("error", project_args(&inner)),
            GENERATOR => sexp("error", vec![project_generator(&inner)]),
            _ => project(&inner),
        };
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

/// A numeric literal token glued after a string macro's close delimiter
/// (`x"s"2`), captured into the `STRING_LITERAL` node as a trailing macrocall
/// argument. Returns the token's source text verbatim.
fn numeric_suffix(node: &SyntaxNode) -> Option<String> {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| matches!(t.kind(), INTEGER | FLOAT | FLOAT32))
        .map(|t| t.text().to_string())
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

/// Project a `function`/`macro` definition. A bare-name signature
/// (`function f end`, `macro m end`) is a forward declaration with no body in
/// JuliaSyntax (`(function f)`), distinct from a method/macro with a call
/// signature and block (`function f() end` → `(function (call f) (block))`).
fn project_function_like(head: &str, node: &SyntaxNode) -> String {
    if is_forward_declaration(node) {
        return sexp(head, vec![project_signature(node)]);
    }
    // A bare-identifier signature with a non-empty body is invalid (`function f
    // body end`); JuliaSyntax error-wraps the name (`(function (error f) (block
    // body))`). The `InvalidFunctionSignature` diagnostic, anchored at the
    // `SIGNATURE`'s start, marks exactly that case.
    let sig = if !node.children().any(|c| c.kind() == SIGNATURE) {
        // No signature at all (`function`, `function;end`) is JuliaSyntax's
        // empty-signature recovery: an error wrapping an empty error,
        // `(error (error))`. (`struct`/`module` differ — handled elsewhere.)
        "(error (error))".to_string()
    } else if invalid_bare_signature(node) {
        format!("(error {})", project_signature(node))
    } else {
        project_signature(node)
    };
    let mut parts = vec![sig, project_block_child(node)];
    push_trailing_errors(node, &mut parts);
    sexp(head, parts)
}

/// A `function`/`macro` header is a forward declaration when its signature is a
/// bare name (`f`, `$f`) rather than a call (`f()`) or other expression — and the
/// body is empty enough to keep it a declaration. A bare name with a body is
/// instead an invalid signature (`invalid_bare_signature`), not a declaration.
fn is_forward_declaration(node: &SyntaxNode) -> bool {
    signature_is_bare_name(node) && !invalid_bare_signature(node)
}

/// Whether the signature's first node is a bare name (`f`, `$f`) rather than a
/// call or other expression.
fn signature_is_bare_name(node: &SyntaxNode) -> bool {
    node.children()
        .find(|c| c.kind() == SIGNATURE)
        .and_then(|sig| first_node(&sig))
        .map(|inner| matches!(inner.kind(), NAME | INTERPOLATION))
        .unwrap_or(false)
}

/// Whether the signature is a bare name marked invalid by the parser (a bare-name
/// header with a non-empty body — `InvalidFunctionSignature`, anchored at the
/// `SIGNATURE`'s start).
fn invalid_bare_signature(node: &SyntaxNode) -> bool {
    node.children()
        .find(|c| c.kind() == SIGNATURE)
        .map(|sig| {
            diag_at(
                usize::from(sig.text_range().start()),
                DiagnosticKind::InvalidFunctionSignature,
            )
        })
        .unwrap_or(false)
}

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

/// Append the unclosed-delimiter `(error-t)` marker to a literal body's parts
/// when an `UnterminatedLiteral`/`StringSuffixSpace` diagnostic is anchored at
/// the literal's start. JuliaSyntax emits no empty `""` content placeholder for
/// an unterminated literal, so drop a sole filler `""` before appending
/// (`"` → `(string (error-t))`, not `(string "")`).
fn with_error_trivia(node: &SyntaxNode, mut parts: Vec<String>) -> Vec<String> {
    let s = usize::from(node.text_range().start());
    if diag_at(s, DiagnosticKind::UnterminatedLiteral)
        || diag_at(s, DiagnosticKind::StringSuffixSpace)
    {
        if parts == ["\"\""] {
            parts.clear();
        }
        parts.push("(error-t)".to_string());
    }
    parts
}

fn project_error(head: &str, node: &SyntaxNode) -> String {
    // A delimiter recovered into an error node is JuliaSyntax's `✘` error-token
    // glyph: a recovery run bumps brackets, commas, and `@` as flat error tokens
    // (`var"x")` ⇒ `(error-t ✘)`, `x y, z` ⇒ `x (error-t y ✘ z)`, `x@y` ⇒
    // `x (error-t ✘ y)`). Other recovered tokens and child nodes project
    // normally, trivia/structure are dropped.
    let parts: Vec<String> = node
        .children_with_tokens()
        .filter_map(|el| match &el {
            NodeOrToken::Token(t) if is_error_glyph(t.kind()) => Some("✘".to_string()),
            // A middle/closing block keyword (`end`/`else`/`elseif`/`catch`/
            // `finally`) recovered into a trailing-junk run renders verbatim
            // (`: end` ⇒ `(error-t end)`, `: catch z` ⇒ `(error-t catch z)`),
            // unlike a structural keyword (dropped below).
            NodeOrToken::Token(t) if is_closing_block_keyword_kind(t.kind()) => {
                Some(t.text().to_string())
            }
            NodeOrToken::Token(t) if is_drop_token(t.kind()) => None,
            _ => project_element(&el),
        })
        .collect();
    sexp(head, parts)
}

/// Delimiter tokens that render as JuliaSyntax's `✘` error-token glyph when
/// recovered into an error node: brackets (open and close), commas, and the
/// macro `@` — each is bumped as a bare error token during recovery.
fn is_error_glyph(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        LPAREN | RPAREN | LBRACKET | RBRACKET | LBRACE | RBRACE | COMMA | AT
    )
}

/// Whether `kind` is a middle/closing block keyword (`end`/`else`/`elseif`/
/// `catch`/`finally`). When recovered into a trailing-junk `(error-t …)` run such
/// a keyword is rendered verbatim rather than dropped as a structural keyword.
fn is_closing_block_keyword_kind(kind: SyntaxKind) -> bool {
    matches!(kind, END_KW | ELSE_KW | ELSEIF_KW | CATCH_KW | FINALLY_KW)
}

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
    match node.children().find(|c| c.kind() == BLOCK) {
        Some(block) => {
            let mut parts = stmt_strings(&block);
            // A block form truncated before its `end` (a `MissingEnd` diagnostic)
            // with an *empty* body gets a zero-width `(error)` placeholder for the
            // missing first statement: JuliaSyntax tries to parse a statement, hits
            // the truncation, and synthesizes one (`function f()` ⇒ `(function
            // (call f) (block (error)) (error-t))`). A body with content, or a
            // properly closed empty body (`function f() end` ⇒ `(block)`), gets no
            // such marker.
            if parts.is_empty()
                && diag_count_from(keyword_start(node), DiagnosticKind::MissingEnd) > 0
            {
                parts.push("(error)".to_string());
            }
            sexp("block", parts)
        }
        None => "(block)".to_string(),
    }
}

/// Append `(error-t)` to `parts` for each `MissingEnd` diagnostic anchored at the
/// construct's opening keyword — the truncation marker for a block form missing
/// its `end` (`if c\n x` ⇒ `(if c (block x) (error-t))`).
fn push_trailing_errors(node: &SyntaxNode, parts: &mut Vec<String>) {
    for _ in 0..diag_count_from(keyword_start(node), DiagnosticKind::MissingEnd) {
        parts.push("(error-t)".to_string());
    }
}

/// Project a block-form's `BLOCK` child, folding a trailing missing-`end` marker
/// *into* the block — for constructs that JuliaSyntax models *as* the block
/// (`begin`, `quote`), so the truncation marker lands inside it
/// (`begin\n x` ⇒ `(block x (error-t))`).
fn project_block_child_folding_error(node: &SyntaxNode) -> String {
    let Some(block) = node.children().find(|c| c.kind() == BLOCK) else {
        return "(block)".to_string();
    };
    let mut parts = stmt_strings(&block);
    // A trailing-junk `ERROR` sibling of the block folds *into* it for the forms
    // JuliaSyntax models *as* the block, so the recovery lands inside
    // (`begin x y end` ⇒ `(block x (error-t y))`).
    for err in node.children().filter(|c| c.kind() == ERROR) {
        parts.push(project(&err));
    }
    push_trailing_errors(node, &mut parts);
    sexp("block", parts)
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
        // A `NAME` normally wraps an `IDENT`; a reserved keyword misused as a
        // signature name (`struct try end` ⇒ `(error try)`) is wrapped here too,
        // so fall back to its keyword text.
        .find(|t| t.kind() == IDENT || is_keyword(t.kind()))
        .map(|t| t.text().to_string())
        .unwrap_or_default()
}

fn operator_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| is_operator(t.kind()))
}

/// Project one element of an `export`/`public` name list: a bare identifier, a
/// `NAME` node, an interpolated name (`$a`), or a macro name (`@a`).
fn name_run_item(el: SyntaxElement) -> Option<String> {
    match el {
        NodeOrToken::Token(t) if t.kind() == IDENT => Some(t.text().to_string()),
        NodeOrToken::Node(n) if n.kind() == NAME => Some(name_text(&n)),
        // An interpolated name (`export $a, $(a*b)`) → `($ …)`.
        NodeOrToken::Node(n) if n.kind() == INTERPOLATION => Some(project(&n)),
        // A macro name (`export @a`) → `@a`.
        NodeOrToken::Node(n) if n.kind() == MACRO_NAME => Some(project_macro_name(&n)),
        // An operator used as a name (`export +, ==`, `export ⊕`) → the bare
        // operator text.
        NodeOrToken::Token(t) if is_operator(t.kind()) => Some(t.text().to_string()),
        // A contextual keyword used as a name (`public export` ⇒ `(public
        // export)`): the parser places it in the name slot, so render its text.
        NodeOrToken::Token(t) if is_keyword(t.kind()) => Some(t.text().to_string()),
        _ => None,
    }
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
            CHAR => Some(project_char(t)),
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
