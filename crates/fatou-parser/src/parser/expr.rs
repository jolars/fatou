//! Pratt (precedence-climbing) expression parser plus postfix call/index chains.
//!
//! `parse_expr` parses one expression starting at a **non-trivia** token; the
//! caller is responsible for emitting any leading trivia. Every token the
//! expression covers (operators and interior trivia included) is emitted into
//! the event stream, so the parser preserves losslessness.

use crate::parser::context::ParserCtx;
use crate::parser::diagnostics::{DiagnosticKind, ParseDiagnostic, push_diagnostic};
use crate::parser::events::{Event, ExprParse, finish, push_range};
use crate::parser::lexer::{TokKind, Token, is_op_suffix_char};
use crate::parser::recovery::{error_expr_to_line_end, error_expr_with_range};
use crate::parser::structural::{
    KwStmt, is_op_name, parse_abstract_type, parse_begin_expr, parse_do_block, parse_for_expr,
    parse_function_expr, parse_if_expr, parse_import_stmt, parse_keyword_stmt, parse_let_expr,
    parse_macro_def, parse_module_expr, parse_name_list_stmt, parse_primitive_type,
    parse_quote_expr, parse_struct_expr, parse_try_expr, parse_typegroup_expr, parse_while_expr,
};
use crate::syntax::SyntaxKind;

mod array;
mod juxtapose;
mod macros;
mod prec;

pub(crate) use array::parse_for_specs;
use array::{
    BRACE_CAT, BRACKET_CAT, array_element_boundary, parse_comprehension, parse_delimited_literal,
    parse_element, parse_empty_ncat, parse_generator_clauses, parse_matrix,
};
use juxtapose::{
    is_element_of_tok, is_for_separator_tok, lhs_is_number, should_juxtapose,
    should_juxtapose_string_error, signed_literal_fold,
};
pub(crate) use macros::{is_var_identifier_start, push_var_macro_name};
use macros::{parse_macro, parse_qualified_macro};
use prec::{
    COMMA_BP, COMMA_ITEM_BP, JUXTAPOSE_L, JUXTAPOSE_R, PREFIX_BP, SPLAT_BP, TERNARY_BRANCH_BP,
    TERNARY_L, WHERE_BOUND_BP, WHERE_BP, WORD_OP_L, WORD_OP_R, infix_binding_power,
    is_assignment_op, is_comparison_op, is_flat_arith_op, is_lone_error_operator, is_operator,
    is_quotable_operator, is_unary_prefix_op, is_value_operator, next_operator, word_operator,
};

/// Context flags threaded through the Pratt parser. All default to `false`, the
/// statement-scope context; bracketed, array, ternary, and indexing contexts flip
/// the relevant ones as they recurse.
#[derive(Clone, Copy, Default)]
struct ExprFlags {
    /// Inside `(…)`/`[…]`/`{…}`: newlines are insignificant and an operator may
    /// continue onto the next line (see [`next_operator`]).
    inside_brackets: bool,
    /// A bare `:` terminates the expression (a ternary true-branch separator)
    /// rather than being parsed as a range operator.
    no_range: bool,
    /// Space-sensitive element position: one element of an array literal or one
    /// space-form macro argument. An operator with whitespace before it but none
    /// after begins a new element (`[1 +2]` is two elements, `@foo a +b` two
    /// arguments; see [`array_element_boundary`]), and a `(`/`[`/`{` preceded by
    /// whitespace starts a new element rather than chaining as a call/index/curly
    /// (`@foo f (x)` is two arguments). Mirrors JuliaSyntax's `space_sensitive`.
    array_mode: bool,
    /// A bare `end` is the index-end marker (an `END_MARKER` atom) rather than a
    /// block terminator. Enabled only inside square brackets (`a[end]`, `[end]`);
    /// parens and braces leave it off, matching Julia's `end`-symbol scope.
    end_marker: bool,
    /// A bare `begin` is the index-begin marker (a `BEGIN_MARKER` atom) rather
    /// than a block opener. Enabled only inside an *indexing* `a[…]` (not vector
    /// literals, where `[begin … end]` is a block), matching Julia: `begin` is a
    /// first-index marker only in `ref` position.
    begin_marker: bool,
    /// At toplevel or module-block statement position, where the contextual
    /// keyword `public` opens a `PUBLIC_STMT`. Off everywhere else (so `public`
    /// stays a plain identifier in sub-expressions and non-module blocks),
    /// matching Julia, which only parses `public` as a keyword at file/module
    /// level.
    public_context: bool,
    /// At statement position (toplevel, module/block statements, and the operand
    /// of `return`/`const`): a top-level comma collects a bare-comma tuple
    /// (`a, b` ⇒ `(tuple a b)`). Off inside brackets and sub-expressions, where
    /// commas are argument/element separators handled by the container parsers.
    stmt_comma: bool,
    /// Parsing a `for`/generator loop variable, where the iteration separator must
    /// not be taken as an infix operator. Suppresses the word operators `in`/`isa`
    /// (lexed as identifiers, comparison precedence) and the Unicode `∈` (an
    /// ordinary comparison-tier operator token), so `for i in xs` and `for i ∈ xs`
    /// both keep `i` the loop variable rather than building `(i in xs)`.
    for_spec_var: bool,
    /// Suppress the `where` clause. Set only while parsing a `where` bound, so a
    /// chain stays left-nested (`A where B where C` ⇒ `(where (where A B) C)`,
    /// not right-nested) and the bound captures only comparison-and-tighter
    /// (mirrors JuliaSyntax's `where_enabled=false` inside `parse_where_chain`).
    no_where: bool,
    /// Suppress the `::` annotation pulling a trailing `where` into its right
    /// operand. Set only for the top level of a long-form `function`/`macro`
    /// signature, where the return type is a bare call-level type and a trailing
    /// `where` binds the whole signature (`function f()::S where T end` ⇒
    /// `(function (where (::-i (call f) S) T) …)`), unlike a value-position `::`
    /// (`f(x)::T where U` ⇒ `(::-i (call f x) (where T U))`). Resets inside
    /// brackets, so argument annotations still capture their own `where`.
    no_decl_where: bool,
    /// Parsing the *name* of a `struct`/`module`/`function`/`macro` signature,
    /// where a leading reserved keyword used as the name is error-wrapped
    /// (`struct try end` ⇒ `(struct (error try) …)`, `function begin() end` ⇒
    /// `(function (call (error begin)) …)`) rather than dispatched to its block
    /// form. Matches JuliaSyntax, which parses the signature name with the block
    /// keywords disabled and recovers a stray one as `(error <kw>)`.
    name_context: bool,
    /// Parsing the right-hand side of a field-access dot (`A.:sym`), where a
    /// `:`-quote stays a quote even when a space precedes a closing block keyword
    /// (`A.: end` ⇒ `(. A (quote-: (error-t) end))`). At value position the same
    /// `: end` falls back to a bare `:` Colon atom (`is_closing_block_keyword`),
    /// so the colon-quote parser only applies that fallback when this is off.
    field_access_rhs: bool,
    /// Force generator position for a `const`/`global`/`local`/`return` statement
    /// even though this is statement position. Set only while parsing the operand
    /// of such a keyword that is *itself* in generator position, so a nested one
    /// inherits the boundary (`[global const x = 1 for i in 1:1]` — the inner
    /// `const` must stop at the `for` too). Everywhere else the boundary is
    /// derived from `stmt_comma`; see [`KwStmt::ExprTuple`]'s `for_ends`.
    kw_generator_body: bool,
}

/// Parse one expression at statement scope (a newline after a complete operand
/// terminates it).
pub(crate) fn parse_expr(
    tokens: &[Token],
    start: usize,
    min_bp: u8,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    parse_expr_in(tokens, start, min_bp, diagnostics, ExprFlags::default())
}

/// Parse one statement at toplevel or module-block scope, where the contextual
/// keyword `public` opens a `PUBLIC_STMT`. Identical to [`parse_expr`] otherwise.
pub(crate) fn parse_stmt(
    tokens: &[Token],
    start: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    parse_block_stmt(tokens, start, true, diagnostics)
}

/// Parse one statement inside a block body, where a top-level comma builds a
/// bare-comma tuple (`a, b` ⇒ `(tuple a b)`). `public_context` is true only at
/// toplevel/module scope (where `public` opens a `PUBLIC_STMT`), false in inner
/// blocks (where `public` stays an ordinary identifier).
pub(crate) fn parse_block_stmt(
    tokens: &[Token],
    start: usize,
    public_context: bool,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let flags = ExprFlags {
        public_context,
        stmt_comma: true,
        ..ExprFlags::default()
    };
    parse_expr_in(tokens, start, 0, diagnostics, flags)
}

/// Parse the operand of a `const`/`global`/`local`/`return` statement. Like
/// [`parse_block_stmt`] (a top-level comma folds into a bare tuple), but
/// `for_ends` propagates the enclosing generator boundary so a nested keyword
/// statement stops at the same `for` (`[global const x = 1 for i in 1:1]`).
pub(crate) fn parse_kw_stmt_operand(
    tokens: &[Token],
    start: usize,
    for_ends: bool,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let flags = ExprFlags {
        stmt_comma: true,
        kw_generator_body: for_ends,
        ..ExprFlags::default()
    };
    parse_expr_in(tokens, start, 0, diagnostics, flags)
}

/// Parse one expression inside brackets (`(...)`, `[...]`), where newlines are
/// insignificant and an operator may continue onto the next line. Note: this does
/// *not* enable the `end` index marker — that is specific to square brackets and
/// is threaded separately (see [`ExprFlags::end_marker`]).
pub(crate) fn parse_expr_in_brackets(
    tokens: &[Token],
    start: usize,
    min_bp: u8,
    // Inherited index-marker context for the bracketed expression.
    end_marker: bool,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let flags = ExprFlags {
        inside_brackets: true,
        end_marker,
        begin_marker: end_marker,
        ..ExprFlags::default()
    };
    parse_expr_in(tokens, start, min_bp, diagnostics, flags)
}

/// Parse the top-level signature of a long-form `function`/`macro` definition.
/// Like [`parse_expr`] but with `no_decl_where` set, so a `::` return type stays
/// a bare call-level annotation and a trailing `where` binds the whole signature
/// (`function f()::S where T end` ⇒ `(where (::-i (call f) S) T)`), matching
/// JuliaSyntax's `parse_function_signature`.
pub(crate) fn parse_signature_expr(
    tokens: &[Token],
    start: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let flags = ExprFlags {
        no_decl_where: true,
        name_context: true,
        ..ExprFlags::default()
    };
    parse_expr_in(tokens, start, 0, diagnostics, flags)
}

/// Parse a `primitive type` spec — the declared name with any `<: B` bound and
/// `{T}` parameters, sitting immediately before the bit size. Julia parses it
/// with `parse_subtype_spec`, which takes no call suffix, so a *spaced* `(` opens
/// the size expression rather than an argument list: `primitive type A (18 * 8)
/// end` declares `A` with size `18 * 8`, not a call `A(18 * 8)`. `array_mode` is
/// exactly that rule — a whitespace-preceded opener starts a new element instead
/// of chaining — so the spec borrows it. A *glued* `(` still chains, matching
/// JuliaSyntax.
pub(crate) fn parse_type_spec_expr(
    tokens: &[Token],
    start: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let flags = ExprFlags {
        array_mode: true,
        ..ExprFlags::default()
    };
    parse_expr_in(tokens, start, 0, diagnostics, flags)
}

/// Parse the name expression of a `struct`/`module` signature, where a leading
/// reserved keyword used as the name is error-wrapped (`struct try end` ⇒
/// `(struct (error try) …)`) rather than dispatched to its block form. See
/// [`ExprFlags::name_context`].
pub(crate) fn parse_name_signature_expr(
    tokens: &[Token],
    start: usize,
    min_bp: u8,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let flags = ExprFlags {
        name_context: true,
        ..ExprFlags::default()
    };
    parse_expr_in(tokens, start, min_bp, diagnostics, flags)
}

fn parse_expr_in(
    tokens: &[Token],
    start: usize,
    min_bp: u8,
    diagnostics: &mut Vec<ParseDiagnostic>,
    flags: ExprFlags,
) -> Option<ExprParse> {
    // `end_marker` is consumed by `parse_prefix` (via `flags`); the rest steer the
    // operator loop directly.
    let ExprFlags {
        inside_brackets,
        no_range,
        array_mode,
        end_marker: _,
        begin_marker,
        public_context,
        stmt_comma,
        for_spec_var,
        no_where,
        no_decl_where,
        name_context,
        field_access_rhs: _,
        kw_generator_body,
    } = flags;
    // A `const`/`global`/`local`/`return` in generator position must stop at the
    // `for` that opens the iteration clause instead of carrying it through as
    // loose tokens (`[const x = 1 for i in 1:1]` ⇒
    // `(comprehension (generator (const (= x 1)) (= i (call-i 1 : 1))))`). That is
    // every non-statement position — bracket elements, arguments, parenthesized
    // expressions — plus a nested keyword operand that inherits it.
    let kw_for_ends = !stmt_comma || kw_generator_body;
    let ctx = ParserCtx::new(tokens);

    // The contextual keyword `public` (a plain identifier elsewhere) opens a
    // `PUBLIC_STMT` at toplevel/module-block statement position, *unless* the next
    // significant token is `(`, `=`, or `[` — those keep `public` an identifier
    // (a call `public(x)`, an assignment `public = 1`, an index `public[i]`),
    // matching JuliaSyntax's `parse_public` compatibility shim.
    if public_context && is_public_keyword(&ctx, start) {
        return parse_name_list_stmt(tokens, start, SyntaxKind::PUBLIC_STMT, diagnostics);
    }

    // Value-producing block forms (`begin…end`, `if`, `for`, `while`, `let`,
    // `try`, `function`/`macro`, `quote`, `struct`, `module`, and the contextual
    // `abstract type`/`primitive type` declarations) are operands: Julia lets a
    // trailing infix operator take the whole block form as its left side
    // (`begin x end::T` ⇒ `(::-i (block x) T)`, `if c x end + 1`). So they fall
    // through into the operator loop as `lhs` rather than returning early, with
    // postfix chaining and juxtaposition suppressed (Julia errors on `begin x
    // end(y)` / `begin x end y`). Inside an indexing `a[…]` a leading `begin` is
    // instead the index-begin marker (handled in `parse_prefix`).
    //
    // The contextual `abstract`/`primitive` words (ordinary identifiers
    // elsewhere) open a type declaration only when immediately followed by the
    // contextual `type`; the pair of adjacent identifiers is unambiguous, so this
    // fires in any expression position (`x = abstract type A end`).
    // In signature-name position a leading reserved keyword is not a block
    // opener but a misused name: JuliaSyntax error-wraps it (`struct try end` ⇒
    // `(struct (error try) …)`, `function begin() end` ⇒
    // `(function (call (error begin)) …)`). Build the `(error <kw>)` atom and let
    // the operator/postfix loop apply any glued call (`begin()` ⇒ a `CALL_EXPR`).
    let name_error_atom = (name_context
        && ctx
            .token(start)
            .is_some_and(|t| is_name_error_keyword(t.kind)))
    .then(|| {
        let pos = ctx.token(start).map_or(start, |t| t.start);
        push_diagnostic(
            diagnostics,
            DiagnosticKind::InvalidNameKeyword,
            "reserved keyword used as a name",
            pos,
            ctx.token(start).map_or(pos, |t| t.end),
        );
        keyword_name_error_atom(start)
    });

    let block_form = if name_error_atom.is_some() {
        None
    } else if let Some(decl_word) = type_decl_keyword(&ctx, start) {
        Some(match decl_word {
            TypeDecl::Abstract => parse_abstract_type(tokens, start, diagnostics),
            TypeDecl::Primitive => parse_primitive_type(tokens, start, diagnostics),
        })
    } else if is_typegroup_keyword(&ctx, start) {
        Some(parse_typegroup_expr(tokens, start, diagnostics))
    } else {
        match ctx.token(start).map(|t| t.kind) {
            Some(TokKind::IfKw) => Some(parse_if_expr(tokens, start, diagnostics)),
            Some(TokKind::FunctionKw) => Some(parse_function_expr(tokens, start, diagnostics)),
            Some(TokKind::MacroKw) => Some(parse_macro_def(tokens, start, diagnostics)),
            Some(TokKind::BeginKw) if !begin_marker => {
                Some(parse_begin_expr(tokens, start, diagnostics))
            }
            Some(TokKind::QuoteKw) => Some(parse_quote_expr(tokens, start, diagnostics)),
            Some(TokKind::WhileKw) => Some(parse_while_expr(tokens, start, diagnostics)),
            Some(TokKind::ForKw) => Some(parse_for_expr(tokens, start, diagnostics)),
            Some(TokKind::LetKw) => Some(parse_let_expr(tokens, start, diagnostics)),
            Some(TokKind::TryKw) => Some(parse_try_expr(tokens, start, diagnostics)),
            Some(TokKind::StructKw | TokKind::MutableKw) => {
                Some(parse_struct_expr(tokens, start, diagnostics))
            }
            Some(TokKind::ModuleKw | TokKind::BaremoduleKw) => {
                Some(parse_module_expr(tokens, start, diagnostics))
            }
            _ => None,
        }
    };

    // Statement keywords consume their own operand through the expression loop
    // internally, so they return directly (`return x::T` ⇒ `(return (::-i x T))`,
    // not `(::-i (return x) T)`).
    if block_form.is_none() {
        match ctx.token(start).map(|t| t.kind) {
            Some(TokKind::ReturnKw) => {
                return parse_keyword_stmt(
                    tokens,
                    start,
                    SyntaxKind::RETURN_EXPR,
                    KwStmt::ExprTuple {
                        optional_value: true,
                        for_ends: kw_for_ends,
                    },
                    diagnostics,
                );
            }
            Some(TokKind::BreakKw) => {
                return parse_keyword_stmt(
                    tokens,
                    start,
                    SyntaxKind::BREAK_EXPR,
                    KwStmt::Label {
                        takes_value: true,
                        colon_ends: no_range,
                    },
                    diagnostics,
                );
            }
            Some(TokKind::ContinueKw) => {
                return parse_keyword_stmt(
                    tokens,
                    start,
                    SyntaxKind::CONTINUE_EXPR,
                    KwStmt::Label {
                        takes_value: false,
                        colon_ends: no_range,
                    },
                    diagnostics,
                );
            }
            Some(TokKind::ConstKw) => {
                return parse_keyword_stmt(
                    tokens,
                    start,
                    SyntaxKind::CONST_STMT,
                    KwStmt::ExprTuple {
                        optional_value: false,
                        for_ends: kw_for_ends,
                    },
                    diagnostics,
                );
            }
            Some(TokKind::GlobalKw) => {
                return parse_keyword_stmt(
                    tokens,
                    start,
                    SyntaxKind::GLOBAL_STMT,
                    KwStmt::ExprTuple {
                        optional_value: false,
                        for_ends: kw_for_ends,
                    },
                    diagnostics,
                );
            }
            Some(TokKind::LocalKw) => {
                return parse_keyword_stmt(
                    tokens,
                    start,
                    SyntaxKind::LOCAL_STMT,
                    KwStmt::ExprTuple {
                        optional_value: false,
                        for_ends: kw_for_ends,
                    },
                    diagnostics,
                );
            }
            Some(TokKind::ImportKw) => {
                return parse_import_stmt(tokens, start, SyntaxKind::IMPORT_STMT, diagnostics);
            }
            Some(TokKind::UsingKw) => {
                return parse_import_stmt(tokens, start, SyntaxKind::USING_STMT, diagnostics);
            }
            Some(TokKind::ExportKw) => {
                return parse_name_list_stmt(tokens, start, SyntaxKind::EXPORT_STMT, diagnostics);
            }
            _ => {}
        }
    }

    // A block form is an operand whose own postfix (`.f`, `(y)`, `[y]`, `{T}`) and
    // juxtaposition are errors in Julia; only infix operators take it as a left
    // side. `lhs_is_block_keyword` suppresses those two checks for the bare block
    // form (the first loop iteration) and is cleared once any operator builds a
    // binary node on top of it.
    let (mut lhs, mut lhs_is_block_keyword) = match (name_error_atom, block_form) {
        // The error-wrapped keyword name is an ordinary atom: a glued call
        // (`begin()`) still attaches via the postfix chain.
        (Some(atom), _) => (atom, false),
        (None, Some(parsed)) => (parsed?, true),
        (None, None) => (parse_prefix(&ctx, start, diagnostics, flags)?, false),
    };

    // A glued colon operator (`a :< b`) consumes exactly one colon-tier operation
    // and does not chain: a following colon-tier operator (`a :< b :< c`,
    // `a :< b:c`) is left as trailing junk while a looser one (`a :< b == c`)
    // still binds. Tracks that one was consumed so the colon branches break.
    let mut glued_colon_done = false;

    loop {
        if !lhs_is_block_keyword {
            lhs = parse_postfix_chain(&ctx, lhs, array_mode, flags.end_marker, diagnostics);
        }

        // Invalid string juxtaposition (`"a"x`, `"a""b"`, `2"a"`): a string
        // literal glued to another term (or a term glued to a string) is an error
        // in Julia, recovered as `(juxtapose lhs (error-t) rhs)`. Checked before
        // the numeric case so a string operand takes the error-bearing shape; the
        // right operand is parsed identically (at `JUXTAPOSE_R`).
        if !lhs_is_block_keyword
            && should_juxtapose_string_error(&ctx, &lhs, min_bp)
            && let Some(rhs) = parse_expr_in(tokens, lhs.end, JUXTAPOSE_R, diagnostics, flags)
        {
            let pos = tokens[lhs.end - 1].end;
            push_diagnostic(
                diagnostics,
                DiagnosticKind::StringJuxtapose,
                "invalid juxtaposition",
                pos,
                pos,
            );
            lhs = build_binary(SyntaxKind::JUXTAPOSE_EXPR, lhs, rhs);
            continue;
        }

        // Numeric-literal-coefficient juxtaposition (`2x`, `2(x)`, `(x-1)y`,
        // `1√x`): an adjacent value with no operator between is an implicit
        // multiplication binding tighter than `*` and looser than `^`. The right
        // operand is parsed at `JUXTAPOSE_R` (capturing a trailing `^` but not a
        // `*`), and the whole thing re-enters the loop so a following operator
        // (`2x*y` ⇒ `(2x)*y`) attaches outside.
        if !lhs_is_block_keyword
            && should_juxtapose(&ctx, &lhs, min_bp)
            && let Some(rhs) = parse_expr_in(tokens, lhs.end, JUXTAPOSE_R, diagnostics, flags)
        {
            lhs = build_binary(SyntaxKind::JUXTAPOSE_EXPR, lhs, rhs);
            continue;
        }

        // Past the postfix/juxtaposition checks the bare block form is fully
        // formed; any further iterations see an ordinary operand.
        lhs_is_block_keyword = false;

        // Splat/vararg `x...` is a postfix operator (left power `SPLAT_BP = 14`),
        // not part of the postfix chain: it binds looser than the colon/range
        // tier (`x:y...` ⇒ `(... (call-i x : y))`) but tighter than the pipes
        // and everything looser (`a|>b...` ⇒ `(call-i a |> (... b))`). It wraps
        // `lhs` and re-loops; `...` is not in `is_operator`, so when it does not
        // bind (`SPLAT_BP < min_bp`, e.g. inside colon's right operand) the loop
        // simply breaks and an enclosing parse consumes it.
        if SPLAT_BP >= min_bp {
            let splat_idx = ctx.skip_ws(lhs.end);
            if ctx.token(splat_idx).map(|t| t.kind) == Some(TokKind::DotDotDot) {
                let mut events = vec![Event::Start(SyntaxKind::SPLAT_EXPR)];
                events.extend(lhs.events);
                push_range(&mut events, lhs.end, splat_idx);
                events.push(Event::Tok(splat_idx));
                events.push(Event::Finish);
                lhs = ExprParse {
                    start: lhs.start,
                    end: splat_idx + 1,
                    events,
                };
                continue;
            }
        }

        // `where` clause: a left-associative chain (`A where B where C` ⇒
        // `(where (where A B) C)`) binding tighter than every binary operator but
        // looser than `^`/juxtaposition/`.` (handled above). Each bound is parsed
        // at comparison precedence with `where` itself suppressed (`no_where`), so
        // `A where T<:S` captures the `<:` bound while a trailing `where` stays in
        // this chain. Suppressed while parsing a bound. The `::` annotation
        // captures its own trailing `where` (below), so a `where` reaching here
        // belongs to `lhs`, not to a pending `::` right operand.
        if !no_where
            && WHERE_BP >= min_bp
            && let Some((where_idx, TokKind::WhereKw)) =
                next_operator(&ctx, lhs.end, inside_brackets)
        {
            lhs = parse_where_chain(tokens, &ctx, lhs, where_idx, diagnostics, flags);
            continue;
        }

        // Statement-level bare-comma tuple: at statement scope a top-level comma
        // collects the operands into a `BARE_TUPLE_EXPR`. Comma is not a Pratt
        // operator (it never reaches `next_operator`); it is handled here, looser
        // than every real operator but tighter than assignment, so a following `=`
        // binds the whole tuple (`a, b = c, d` ⇒ `(= (tuple a b) (tuple c d))`).
        // The `min_bp` guard keeps it inert once we are inside a comma item.
        if stmt_comma
            && min_bp <= COMMA_BP
            && ctx.token(ctx.skip_ws(lhs.end)).map(|t| t.kind) == Some(TokKind::Comma)
        {
            lhs = parse_comma_tuple(tokens, &ctx, lhs, diagnostics, flags);
            continue;
        }

        // Word operators `in`/`isa` (lexed as identifiers) act as infix operators
        // at comparison precedence (`i in rhs` ⇒ `(call-i i in rhs)`, `x isa T` ⇒
        // `(call-i x isa T)`). Like the comparison operators, they are
        // left-associative and chains stay nested (a recorded modeling
        // divergence). Suppressed while parsing a loop variable, where `in` is the
        // for-spec separator. Checked after juxtaposition (so an adjacent `2in`
        // still juxtaposes) and before the symbolic operators.
        if !for_spec_var && let Some(op_idx) = word_operator(&ctx, lhs.end, inside_brackets) {
            if WORD_OP_L < min_bp {
                break;
            }
            let rhs_operand = ctx.skip_trivia(op_idx + 1);
            let Some(rhs) = parse_expr_in(tokens, rhs_operand, WORD_OP_R, diagnostics, flags)
            else {
                let op = &tokens[op_idx];
                push_diagnostic(
                    diagnostics,
                    DiagnosticKind::MissingOperand,
                    "expected right-hand side for operator",
                    op.start,
                    op.end,
                );
                return Some(error_expr_to_line_end(tokens, lhs.start, op_idx + 1));
            };
            lhs = build_binary(SyntaxKind::BINARY_EXPR, lhs, rhs);
            continue;
        }

        let Some((op_idx, op_kind)) = next_operator(&ctx, lhs.end, inside_brackets) else {
            break;
        };

        // `∈` is the Unicode spelling of the `in` iteration separator, but unlike
        // `in` it is a real operator token, so the word-operator suppression above
        // does not cover it. End the loop variable here and let `parse_for_specs`
        // consume it as the separator (`for i ∈ xs` ⇒ `(for (= i xs) …)`).
        if for_spec_var && is_element_of_tok(&tokens[op_idx]) {
            break;
        }

        // Inside an array literal, an operator glued to the start of the next
        // operand (space before, none after) is that operand's prefix, marking a
        // new element rather than an infix operator. End this element here.
        if array_mode && array_element_boundary(&ctx, lhs.end, op_idx) {
            break;
        }

        // A `.` whose right-hand side begins with `@` is a qualified macro call
        // (`Base.@time f()`): the whole `Base.@time` is the macro name and the
        // rest are its arguments — not a field access wrapping a macro call.
        if op_kind == TokKind::Dot
            && ctx.token(ctx.skip_trivia(op_idx + 1)).map(|t| t.kind) == Some(TokKind::At)
        {
            lhs =
                parse_qualified_macro(&ctx, lhs, op_idx, diagnostics, inside_brackets, array_mode);
            continue;
        }

        // In a ternary true-branch a bare `:` is the separator, not a range.
        if no_range && op_kind == TokKind::Colon {
            break;
        }

        // A range colon glued directly to a single `<`/`>` (no whitespace between
        // them) is the invalid operator `:<`/`:>`: JuliaSyntax lexes the pair as
        // one error operator at the colon precedence tier and heads the infix
        // call with both tokens error-wrapped (`a :< b` ⇒ `(call-i a (error : <)
        // b)`). Only a bare `<`/`>` glues — `:<=`/`:>:` keep the range reading and
        // a prefix `:<` stays a quote. The operator consumes one operation and
        // does not chain (a following colon-tier op falls to the junk driver).
        if op_kind == TokKind::Colon
            && matches!(
                ctx.token(op_idx + 1).map(|t| t.kind),
                Some(TokKind::Lt | TokKind::Gt)
            )
        {
            if glued_colon_done {
                break;
            }
            let (l_bp, r_bp) = infix_binding_power(TokKind::Colon).expect("colon binds");
            if l_bp < min_bp {
                break;
            }
            let colon = &tokens[op_idx];
            push_diagnostic(
                diagnostics,
                DiagnosticKind::InvalidGluedOperator,
                "invalid operator",
                colon.start,
                colon.start,
            );
            let lt_idx = op_idx + 1;
            let rhs_operand = ctx.skip_trivia(lt_idx + 1);
            lhs = match parse_expr_in(tokens, rhs_operand, r_bp, diagnostics, flags) {
                Some(rhs) => build_binary(SyntaxKind::BINARY_EXPR, lhs, rhs),
                None => {
                    let lt = &tokens[lt_idx];
                    push_diagnostic(
                        diagnostics,
                        DiagnosticKind::MissingOperand,
                        "expected right-hand side for operator",
                        colon.start,
                        lt.end,
                    );
                    build_binary_missing_rhs(SyntaxKind::BINARY_EXPR, lhs, lt_idx + 1)
                }
            };
            glued_colon_done = true;
            continue;
        }

        // Range `:` collapses a stepped chain into a single 3-operand call
        // (`a:b:c` ⇒ `(call-i a : b c)`, `a:b:c:d:e` ⇒ `(call-i (call-i a : b c)
        // : d e)`), exactly as JuliaSyntax's `parse_range`, so it is handled
        // before the generic left-associative path.
        if op_kind == TokKind::Colon {
            if glued_colon_done {
                break;
            }
            let (l_bp, _) = infix_binding_power(TokKind::Colon).expect("colon binds");
            if l_bp < min_bp {
                break;
            }
            lhs = parse_colon_range(tokens, &ctx, lhs, op_idx, diagnostics, flags);
            continue;
        }

        // A run of two or more comparison-tier operators folds into one flat
        // `COMPARISON_EXPR` (`a < b <= c` ⇒ `(comparison a < b <= c)`), exactly as
        // JuliaSyntax's `parse_comparison`; a lone comparison stays an ordinary
        // binary (`a < b` ⇒ `(call-i a < b)`, `a <: b` ⇒ `(<: a b)`). Handled
        // before the generic path so the whole chain is collected at once.
        if is_comparison_op(op_kind) {
            let (l_bp, _) = infix_binding_power(op_kind).expect("comparison binds");
            if l_bp < min_bp {
                break;
            }
            lhs = parse_operator_chain(
                &ctx,
                lhs,
                op_idx,
                diagnostics,
                flags,
                ChainSpec {
                    single: operator_node_kind(op_kind),
                    flat: SyntaxKind::COMPARISON_EXPR,
                    continues: |kind, _| is_comparison_op(kind),
                },
            );
            continue;
        }

        // A run of two or more of the *same* `+`/`*` operator folds into one flat
        // n-ary `BINARY_EXPR` (`a + b + c` ⇒ `(call-i a + b c)`), exactly as
        // JuliaSyntax's variadic `parse_with_chains`; a lone `+`/`*` stays an
        // ordinary binary (`a + b` ⇒ `(call-i a + b)`). Mixed operators break the
        // run and nest via the generic path (`a + b - c` ⇒ `(call-i (call-i a + b)
        // - c)`). Dotted `.+`/`.*` do *not* flatten and are excluded.
        if is_flat_arith_op(&tokens[op_idx]) {
            let (l_bp, _) = infix_binding_power(op_kind).expect("arith binds");
            if l_bp < min_bp {
                break;
            }
            lhs = parse_operator_chain(
                &ctx,
                lhs,
                op_idx,
                diagnostics,
                flags,
                ChainSpec {
                    single: SyntaxKind::BINARY_EXPR,
                    flat: SyntaxKind::BINARY_EXPR,
                    continues: |kind, idx| kind == op_kind && is_flat_arith_op(&tokens[idx]),
                },
            );
            continue;
        }

        // Ternary `cond ? then : else` — right-associative, just above
        // assignment and below `||`. Handled specially (like assignment) so the
        // `:` separator is consumed here rather than parsed as a range operator.
        if op_kind == TokKind::Question {
            if TERNARY_L < min_bp {
                break;
            }
            lhs = match parse_ternary(&ctx, lhs, op_idx, diagnostics, flags) {
                Ok(node) => node,
                Err(done) => return Some(done),
            };
            continue;
        }

        // Assignment (`=`, `.=`, and augmented `+=`/`.+=`/…) is right-associative
        // and the loosest operator.
        let (l_bp, r_bp) = if is_assignment_op(op_kind) {
            (2, 1)
        } else {
            match infix_binding_power(op_kind) {
                Some(bp) => bp,
                None => break,
            }
        };
        if l_bp < min_bp {
            break;
        }

        if op_kind == TokKind::Arrow {
            lhs = reclassify_arrow_parameters(lhs);
        }

        let rhs_operand = ctx.skip_trivia(op_idx + 1);
        // Field access `a.b`: the right operand is an atom (the field name), not a
        // postfix-chained expression. A trailing `()`/`[]`/`{}` binds to the whole
        // field access (`A.f()` is `(A.f)()`, `a.b{T}` is `(a.b){T}`), so parse the
        // RHS prefix-only and let the outer postfix chain attach any suffix. Other
        // operators parse a full right operand at their binding power.
        let rhs_result = if op_kind == TokKind::Dot {
            parse_prefix(
                &ctx,
                rhs_operand,
                diagnostics,
                ExprFlags {
                    field_access_rhs: true,
                    ..flags
                },
            )
        } else {
            parse_expr_in(tokens, rhs_operand, r_bp, diagnostics, flags)
        };
        let Some(mut rhs) = rhs_result else {
            let op = &tokens[op_idx];
            push_diagnostic(
                diagnostics,
                DiagnosticKind::MissingOperand,
                "expected right-hand side for operator",
                op.start,
                op.end,
            );
            // JuliaSyntax keeps the operator node and synthesizes a zero-width
            // `(error)` for the absent operand (`x =` ⇒ `(= x (error))`, `a +`
            // ⇒ `(call-i a + (error))`, `a &&` ⇒ `(&& a (error))`) rather than
            // discarding the whole construct. Build the node with only the LHS
            // and the operator; the projector replays the `(error)` from the
            // `MissingOperand` diagnostic anchored at the operator.
            lhs = build_binary_missing_rhs(operator_node_kind(op_kind), lhs, rhs_operand);
            continue;
        };

        // A `::` annotation captures a trailing `where` in its right operand
        // (JuliaSyntax parses the annotation through `parse_where`): `A::B where C`
        // ⇒ `(:: A (where B C))`. `where` binds tighter than `::` itself, so the
        // chain wraps the annotation type, not the whole `::`. Suppressed inside a
        // `where` bound, where `::` does not pull in a following `where`.
        if op_kind == TokKind::ColonColon
            && !no_where
            && !no_decl_where
            && let Some((where_idx, TokKind::WhereKw)) =
                next_operator(&ctx, rhs.end, inside_brackets)
        {
            rhs = parse_where_chain(tokens, &ctx, rhs, where_idx, diagnostics, flags);
        }

        let node = operator_node_kind(op_kind);
        // Whitespace before a field-access dot is disallowed: JuliaSyntax keeps
        // the `(. lhs (quote rhs))` shape but flags it (`x .y` ⇒
        // `(. x (error-t) (quote y))`). We record a `DotWhitespace` diagnostic at
        // the dot's end; the projector replays the `(error-t)`. A broadcast
        // operator `.+` lexes as a single token (not `Dot`), so this never fires
        // for `a .+ b`.
        if op_kind == TokKind::Dot && op_idx > lhs.end {
            let op = &tokens[op_idx];
            push_diagnostic(
                diagnostics,
                DiagnosticKind::DotWhitespace,
                "whitespace before `.`",
                op.end,
                op.end,
            );
        }
        lhs = build_binary(node, lhs, rhs);
    }

    Some(lhs)
}

/// Consume a left-associative `where` chain onto `lhs`, starting at the `where`
/// token `where_idx`. Each iteration parses the bound at `WHERE_BOUND_BP`
/// (comparison precedence) with `where` suppressed, then wraps the running
/// expression in a `WHERE_EXPR` (`A where B where C` ⇒ `(where (where A B) C)`).
/// Mirrors JuliaSyntax's `parse_where_chain` (`while peek == where`, the bound
/// parsed by `parse_comparison` with `where_enabled=false`).
fn parse_where_chain(
    tokens: &[Token],
    ctx: &ParserCtx<'_>,
    mut lhs: ExprParse,
    mut where_idx: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
    flags: ExprFlags,
) -> ExprParse {
    let bound_flags = ExprFlags {
        no_where: true,
        ..flags
    };
    loop {
        let bound_start = ctx.skip_trivia(where_idx + 1);
        let Some(bound) = parse_expr_in(
            tokens,
            bound_start,
            WHERE_BOUND_BP,
            diagnostics,
            bound_flags,
        ) else {
            let op = &tokens[where_idx];
            push_diagnostic(
                diagnostics,
                DiagnosticKind::MissingWhereBound,
                "expected type bound after `where`",
                op.start,
                op.end,
            );
            return error_expr_to_line_end(tokens, lhs.start, where_idx + 1);
        };
        lhs = build_binary(SyntaxKind::WHERE_EXPR, lhs, bound);
        match next_operator(ctx, lhs.end, flags.inside_brackets) {
            Some((idx, TokKind::WhereKw)) => where_idx = idx,
            _ => return lhs,
        }
    }
}

/// Which contextual `… type` declaration a `TYPE_DECL` opener introduces.
enum TypeDecl {
    Abstract,
    Primitive,
}

/// Detect a contextual `abstract type`/`primitive type` opener: an identifier
/// `abstract`/`primitive` immediately followed (across trivia only) by the
/// identifier `type`. Returns `None` for the plain-identifier uses (`abstract`,
/// `abstract = 1`, `abstract(x)`).
fn type_decl_keyword(ctx: &ParserCtx<'_>, start: usize) -> Option<TypeDecl> {
    let word = match ctx.token(start) {
        Some(t) if t.kind == TokKind::Ident && t.text == "abstract" => TypeDecl::Abstract,
        Some(t) if t.kind == TokKind::Ident && t.text == "primitive" => TypeDecl::Primitive,
        _ => return None,
    };
    let next = ctx.token(ctx.skip_trivia(start + 1))?;
    (next.kind == TokKind::Ident && next.text == "type").then_some(word)
}

/// Whether the identifier `typegroup` at `start` opens Julia 1.14's grouped type
/// definition. Julia 1.14 reserves the word outright, but keeping it contextual
/// here leaves it usable as an ordinary identifier in pre-1.14 code, so it only
/// opens the block form when the next significant token — across newlines, since
/// the body normally starts on the following line — can begin a type definition:
/// `struct`, `mutable`, `abstract`, `primitive`, a macro call, or a docstring.
/// Mirrors the set JuliaSyntax recovers on when parsing below 1.14.
fn is_typegroup_keyword(ctx: &ParserCtx<'_>, start: usize) -> bool {
    match ctx.token(start) {
        Some(t) if t.kind == TokKind::Ident && t.text == "typegroup" => {}
        _ => return false,
    }
    let next = ctx.skip_trivia(start + 1);
    match ctx.token(next) {
        Some(t) => match t.kind {
            TokKind::StructKw | TokKind::MutableKw | TokKind::At | TokKind::StringDelimOpen => true,
            TokKind::Ident => matches!(t.text, "abstract" | "primitive"),
            _ => false,
        },
        None => false,
    }
}

/// Whether the identifier `public` at `start` opens a `PUBLIC_STMT`. True when
/// the token is the identifier `public` and the next significant token exists and
/// is not `(`, `=`, or `[` — those three keep `public` an ordinary identifier (a
/// call, assignment, or index), matching JuliaSyntax's `parse_public`.
fn is_public_keyword(ctx: &ParserCtx<'_>, start: usize) -> bool {
    match ctx.token(start) {
        Some(t) if t.kind == TokKind::Ident && t.text == "public" => {}
        _ => return false,
    }
    match ctx.token(ctx.skip_trivia(start + 1)).map(|t| t.kind) {
        Some(TokKind::LParen | TokKind::Eq | TokKind::LBracket) | None => false,
        Some(_) => true,
    }
}

/// Build a node of `kind` from its operands, capturing each operator (and the
/// surrounding trivia) in the gap between adjacent operands.
///
/// `tail` extends the node past the final operand to `gap_end`, covering a
/// dangling operator whose right operand is missing. The projector replays
/// JuliaSyntax's zero-width `(error)` operand from the `MissingOperand`
/// diagnostic recorded at that operator (`1:2:` ⇒ `(call-i 1 : 2 (error))`,
/// `a < b <` ⇒ `(comparison a < b < (error))`).
fn build_operands(kind: SyntaxKind, operands: Vec<ExprParse>, tail: Option<usize>) -> ExprParse {
    let mut iter = operands.into_iter();
    let first = iter.next().expect("node needs at least one operand");
    let start = first.start;
    let mut end = first.end;
    let mut events = vec![Event::Start(kind)];
    events.extend(first.events);
    for operand in iter {
        push_range(&mut events, end, operand.start);
        events.extend(operand.events);
        end = operand.end;
    }
    if let Some(gap_end) = tail {
        push_range(&mut events, end, gap_end);
        end = gap_end;
    }
    events.push(Event::Finish);
    ExprParse { start, end, events }
}

/// Build a binary/assignment node from `lhs`, the gap (whitespace + operator +
/// trivia) up to `rhs`, and `rhs`.
fn build_binary(kind: SyntaxKind, lhs: ExprParse, rhs: ExprParse) -> ExprParse {
    build_operands(kind, vec![lhs, rhs], None)
}

/// The CST node kind for an infix operator: an assignment, an anonymous-function
/// `->`, a `::` type annotation, or a plain binary expression.
fn operator_node_kind(op_kind: TokKind) -> SyntaxKind {
    match op_kind {
        k if is_assignment_op(k) => SyntaxKind::ASSIGNMENT_EXPR,
        TokKind::Arrow => SyntaxKind::ARROW_EXPR,
        TokKind::ColonColon => SyntaxKind::TYPE_ANNOTATION,
        _ => SyntaxKind::BINARY_EXPR,
    }
}

/// Relabel a parenthesized anonymous-function signature as its parameter tuple.
/// Parentheses ordinarily preserve value grouping, including `;` blocks, but
/// immediately before `->` Julia interprets them as a parameter list. A `where`
/// signature is the exception: its parentheses remain transparent so the
/// `WHERE_EXPR` stays the arrow's direct left operand.
fn reclassify_arrow_parameters(mut lhs: ExprParse) -> ExprParse {
    let is_parenthesized = matches!(
        lhs.events.first(),
        Some(Event::Start(
            SyntaxKind::PAREN_EXPR | SyntaxKind::PAREN_BLOCK
        ))
    );
    let wraps_where = lhs
        .events
        .iter()
        .filter_map(|event| match event {
            Event::Start(kind) => Some(*kind),
            Event::Tok(_) | Event::Finish => None,
        })
        .skip(1)
        .find(|kind| *kind != SyntaxKind::PAREN_EXPR)
        == Some(SyntaxKind::WHERE_EXPR);
    if is_parenthesized && !wraps_where {
        lhs.events[0] = Event::Start(SyntaxKind::TUPLE_EXPR);
    }
    lhs
}

/// Build an operator node whose right operand is absent: `lhs`, then the gap
/// (whitespace + operator + trailing trivia) up to `gap_end`, and no RHS.
fn build_binary_missing_rhs(kind: SyntaxKind, lhs: ExprParse, gap_end: usize) -> ExprParse {
    build_operands(kind, vec![lhs], Some(gap_end))
}

/// Collect a statement-level bare-comma tuple. The first operand `first` is
/// already parsed and the caller has confirmed a comma follows it. Each further
/// operand is parsed at [`COMMA_ITEM_BP`] (so it stops before the next comma and
/// before any assignment), and the comma tokens and surrounding trivia are kept
/// in the gaps. A trailing comma with no operand after it (`x, = xs` ⇒
/// `(tuple x)`, `x, y, = a`) leaves a tuple with the operands gathered so far,
/// mirroring JuliaSyntax's `parse_comma`.
fn parse_comma_tuple(
    tokens: &[Token],
    ctx: &ParserCtx<'_>,
    first: ExprParse,
    diagnostics: &mut Vec<ParseDiagnostic>,
    flags: ExprFlags,
) -> ExprParse {
    let start = first.start;
    let mut events = vec![Event::Start(SyntaxKind::BARE_TUPLE_EXPR)];
    let mut end = first.end;
    events.extend(first.events);

    loop {
        let comma_idx = ctx.skip_ws(end);
        if ctx.token(comma_idx).map(|t| t.kind) != Some(TokKind::Comma) {
            break;
        }
        push_range(&mut events, end, comma_idx);
        events.push(Event::Tok(comma_idx));
        end = comma_idx + 1;

        // A trailing comma continues the tuple across a newline: at bare-tuple
        // scope the comma suppresses the statement-terminating newline, so the
        // next element may begin on a later line (`x = a,\nb,\nc` ⇒
        // `(= x (tuple a b c))`). Skip newlines and comments — not just
        // horizontal whitespace — to reach it. A newline *before* the comma
        // still terminates (that gap is only `skip_ws` at the comma probe above).
        let item_start = ctx.skip_trivia(end);
        // A trailing comma before an assignment-family operator is a 1-tuple the
        // assignment then binds (`x, = xs` ⇒ `(= (tuple x) xs)`): the operator is
        // not a tuple element, so stop here and let the operator loop take it,
        // rather than collecting it as a `(error op)` atom.
        if ctx
            .token(item_start)
            .is_some_and(|t| is_lone_error_operator(t.kind))
        {
            break;
        }
        match parse_expr_in(tokens, item_start, COMMA_ITEM_BP, diagnostics, flags) {
            Some(item) => {
                push_range(&mut events, end, item.start);
                events.extend(item.events);
                end = item.end;
            }
            // Trailing comma: nothing follows that can start an operand.
            None => break,
        }
    }

    events.push(Event::Finish);
    ExprParse { start, end, events }
}

/// Parse a range `:` chain starting at the colon `first_colon` (the first operand
/// `lhs` is already parsed and the caller has cleared the binding-power check).
/// Mirrors JuliaSyntax's `parse_range`: every second colon folds three operands
/// into one `RANGE_EXPR` (`a:b:c`), and a further colon nests the folded range as
/// the left operand of the next chain (`(a:b:c):d:e`). An odd trailing colon
/// (`a:b:c:d`) leaves an ordinary two-operand `BINARY_EXPR`.
fn parse_colon_range(
    tokens: &[Token],
    ctx: &ParserCtx<'_>,
    lhs: ExprParse,
    first_colon: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
    flags: ExprFlags,
) -> ExprParse {
    let (_, r_bp) = infix_binding_power(TokKind::Colon).expect("colon binds");
    let mut head = lhs;
    // The operand awaiting a step partner (JuliaSyntax's open colon count).
    let mut step: Option<ExprParse> = None;
    let mut op_idx = first_colon;
    loop {
        // The range colon does not consume a right operand across a newline unless
        // newlines are insignificant (inside parens). At statement scope and inside
        // array brackets (where a newline is a row separator) a newline ends the
        // range, leaving the colon's right operand absent (`1:\n2` ⇒
        // `(call-i 1 : (error)) 2`, `[1:\n2]` ⇒ `(vcat (call-i 1 : (error)) 2)`),
        // unlike other operators, which continue onto the next line.
        let newline_significant = !flags.inside_brackets || flags.array_mode;
        let newline_stop = newline_significant
            && ctx.token(ctx.skip_ws(op_idx + 1)).map(|t| t.kind) == Some(TokKind::Newline);
        let rhs_operand = ctx.skip_trivia(op_idx + 1);
        let rhs = if newline_stop {
            None
        } else {
            parse_expr_in(tokens, rhs_operand, r_bp, diagnostics, flags)
        };
        let Some(rhs) = rhs else {
            // Missing right operand: keep the colon node and synthesize a zero-width
            // `(error)` operand (the projector replays it from the `MissingOperand`
            // diagnostic at the colon) rather than error-wrapping to line end — a
            // bare `1:` ⇒ `(call-i 1 : (error))`, a stepped `1:2:` ⇒
            // `(call-i 1 : 2 (error))`.
            let op = &tokens[op_idx];
            push_diagnostic(
                diagnostics,
                DiagnosticKind::MissingOperand,
                "expected right-hand side for operator",
                op.start,
                op.end,
            );
            return match step.take() {
                None => build_binary_missing_rhs(SyntaxKind::BINARY_EXPR, head, op_idx + 1),
                Some(mid) => {
                    build_operands(SyntaxKind::RANGE_EXPR, vec![head, mid], Some(op_idx + 1))
                }
            };
        };
        let last_end = rhs.end;
        match step.take() {
            Some(mid) => head = build_operands(SyntaxKind::RANGE_EXPR, vec![head, mid, rhs], None),
            None => step = Some(rhs),
        }
        // Continue the chain only on another range colon at the same level: not a
        // ternary separator (`no_range`) and not an array-element boundary
        // (`[1 :2]` splits into elements rather than ranging).
        let continues = match next_operator(ctx, last_end, flags.inside_brackets) {
            Some((idx, TokKind::Colon)) if !flags.no_range => {
                let split = flags.array_mode && array_element_boundary(ctx, last_end, idx);
                (!split).then_some(idx)
            }
            _ => None,
        };
        match continues {
            Some(idx) => op_idx = idx,
            None => break,
        }
    }
    match step {
        Some(mid) => build_binary(SyntaxKind::BINARY_EXPR, head, mid),
        None => head,
    }
}

/// The comparison-precedence operators (JuliaSyntax tier 10): the symbolic
/// relations/equalities `< <= > >= == !=`, the subtype relations `<:`/`>:`, their
/// broadcast `.`-variants, and any Unicode comparison operator. A run of two or
/// more of these folds into one flat `COMPARISON_EXPR`. The word operators
/// `in`/`isa` share the tier but are parsed in a separate branch and stay nested
/// (a recorded divergence — see the `word_operator` handling in the loop).
/// How a run of same-tier operators folds: the node kind for a lone operator,
/// the flat node kind for a run of two or more, and the predicate deciding
/// whether the operator of kind `k` at index `i` extends the run.
struct ChainSpec<F: Fn(TokKind, usize) -> bool> {
    single: SyntaxKind,
    flat: SyntaxKind,
    continues: F,
}

/// Parse a run of same-tier operators starting at `first_op` (the first operand
/// `lhs` is already parsed and the caller has cleared the binding-power check).
/// Each operand parses at the operator's right binding power and the run extends
/// while `spec.continues` holds.
///
/// Mirrors two JuliaSyntax foldings that share this shape: `parse_comparison`
/// (`a < b <= c` ⇒ `(comparison a < b <= c)`) and the variadic `+`/`*` chain
/// (`a + b + c` ⇒ `(call-i a + b c)`). A single operator yields an ordinary
/// two-operand `spec.single` node (`a < b` ⇒ `(call-i a < b)`, `a <: b` ⇒
/// `(<: a b)`); two or more fold into one flat `spec.flat`.
fn parse_operator_chain(
    ctx: &ParserCtx<'_>,
    lhs: ExprParse,
    first_op: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
    flags: ExprFlags,
    spec: ChainSpec<impl Fn(TokKind, usize) -> bool>,
) -> ExprParse {
    let tokens = ctx.tokens();
    let mut operands = vec![lhs];
    let mut op_count = 0usize;
    let mut op_idx = first_op;
    let tail = loop {
        let (_, r_bp) = infix_binding_power(tokens[op_idx].kind).expect("chain operator binds");
        let rhs_operand = ctx.skip_trivia(op_idx + 1);
        let Some(rhs) = parse_expr_in(tokens, rhs_operand, r_bp, diagnostics, flags) else {
            // Missing right operand: keep the operator(s) by ending the node past
            // them, so the projector replays JuliaSyntax's zero-width `(error)`
            // from the `MissingOperand` diagnostic anchored at the dangling
            // operator (`a +` ⇒ `(call-i a + (error))`, `a < b <` ⇒
            // `(comparison a < b < (error))`).
            let op = &tokens[op_idx];
            push_diagnostic(
                diagnostics,
                DiagnosticKind::MissingOperand,
                "expected right-hand side for operator",
                op.start,
                op.end,
            );
            op_count += 1;
            break Some(op_idx + 1);
        };
        let last_end = rhs.end;
        operands.push(rhs);
        op_count += 1;
        // Continue only while the run's own predicate holds and the operator is
        // not an array-element boundary (`[a <b]`, `[a +b]` split into elements).
        let continues = match next_operator(ctx, last_end, flags.inside_brackets) {
            Some((idx, kind)) if (spec.continues)(kind, idx) => {
                let split = flags.array_mode && array_element_boundary(ctx, last_end, idx);
                (!split).then_some(idx)
            }
            _ => None,
        };
        match continues {
            Some(idx) => op_idx = idx,
            None => break None,
        }
    };
    let kind = if op_count == 1 {
        spec.single
    } else {
        spec.flat
    };
    build_operands(kind, operands, tail)
}

/// The plain `+`/`*` operators (and their wrapping forms `+%`/`*%`) that fold a
/// same-operator run into one flat variadic call. Dotted `.+`/`.*` are excluded
/// (they nest in JuliaSyntax), as is `-`/`-%` (left-associative, not variadic)
/// and the missing `++` operator. A *suffixed* operator (`+₁`, `*₂`) is
/// non-syntactic and never folds (`a +₁ b +₁ c` ⇒ `(call-i (call-i a +₁ b) +₁ c)`).
fn parse_prefix(
    ctx: &ParserCtx<'_>,
    start: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
    flags: ExprFlags,
) -> Option<ExprParse> {
    let tok = ctx.token(start)?;
    match tok.kind {
        // An operator glued to `{` is a parametric callee: `+{T}` → `(curly + T)`,
        // `*{T}(x)` → `(call (curly * T) x)`. We return the operator as a bare leaf
        // and let the postfix chain build the `CURLY_EXPR` (and any trailing call),
        // exactly as for an identifier callee `f{T}`. Excludes `::`/`&`/`:`, which
        // Julia keeps as prefixes over the braces (`::{T}` → `(::-pre (braces T))`).
        k if is_curly_operator_name(k)
            && ctx.token(start + 1).map(|t| t.kind) == Some(TokKind::LBrace) =>
        {
            Some(ExprParse {
                start,
                end: start + 1,
                events: vec![Event::Tok(start)],
            })
        }
        // On the right-hand side of a field-access dot, any reserved keyword is an
        // ordinary field name, not its keyword form: `x.function`, `x.end`, and
        // `x.true` all project as `(. x (quote <kw>))`. This must precede the
        // `end`/`begin`/`true`/`false` arms below so a field name never reads as an
        // index marker or a boolean literal. Keywords Fatou already lexes as plain
        // identifiers (`type`, `in`, …) fall through to the `Ident` arm unchanged.
        k if flags.field_access_rhs && k.is_keyword() => Some(atom(SyntaxKind::NAME, start)),
        // A bare `end` inside square brackets is the index-end marker (`a[end]`,
        // `a[end - 1]`); elsewhere `end` is a block terminator and not an atom.
        TokKind::EndKw if flags.end_marker => Some(atom(SyntaxKind::END_MARKER, start)),
        // A bare `begin` inside an indexing `a[…]` is the index-begin marker
        // (`a[begin]`, `a[begin + 1]`); elsewhere `begin` opens a block.
        TokKind::BeginKw if flags.begin_marker => Some(atom(SyntaxKind::BEGIN_MARKER, start)),
        // In a signature-name position an operator *names* the method being
        // defined; it is never applied. JuliaSyntax parses the name with
        // `parse_unary_prefix`, which routes every non-syntactic operator to
        // `parse_atom`, so the operator is a plain atom that an argument list may
        // then call: `function + end` ⇒ `(function +)` and `function +(x) end` ⇒
        // `(function (call + x))`, where an ordinary expression position would
        // instead read the prefix application `(call-pre + x)`. Without this the
        // bare form has no operand and swallows the closing `end` (`function ⊑
        // end` ⇒ `(function (call-pre ⊑ (error end)) …)`), which is how
        // `function ∘ end`, `function ⊇ end` and `typeof(function + end)` in Base
        // fail to parse at all.
        //
        // Restricted to the operators that have a value form (`is_value_operator`):
        // the purely syntactic ones stay errors here too (`function = end` ⇒
        // `(function (error =))`). The syntactic prefixes `&`/`::`/`$` keep their
        // own node shapes in a signature (`function &(x) end` ⇒ `(function (& x))`)
        // and a prefix `:` still quotes, so all four are left to their own arms —
        // `:` by the explicit exclusion, the rest by not being value operators.
        //
        // A bare name is only a *declaration* (`function f end`); with a non-empty
        // body JuliaSyntax error-wraps the name (`function f\nx\nend` ⇒
        // `(function (error f) (block x))`).
        // The syntactic `&` is the one value operator that keeps its prefix node
        // over an argument list (`function &(x) end` ⇒ `(function (& x))`), so
        // only its *bare* form is a name here; the parenthesized form is left to
        // the unary arm.
        k if flags.name_context
            && k != TokKind::Colon
            && is_value_operator(k)
            && !(k == TokKind::Amp
                && ctx.token(start + 1).map(|t| t.kind) == Some(TokKind::LParen)) =>
        {
            // Glued to `(` the name heads a call, exactly as for the non-unary
            // operator callees below (`function *(x) end` already took that path);
            // the unary-capable operators would otherwise apply their single-operand
            // prefix heuristic and yield `call-pre`.
            if ctx.token(start + 1).map(|t| t.kind) == Some(TokKind::LParen) {
                let (list_events, end) = parse_arg_list(
                    ctx,
                    start + 1,
                    TokKind::RParen,
                    SyntaxKind::ARG_LIST,
                    flags.end_marker,
                    diagnostics,
                );
                let mut events = vec![Event::Start(SyntaxKind::CALL_EXPR), Event::Tok(start)];
                events.extend(list_events);
                events.push(Event::Finish);
                Some(ExprParse { start, end, events })
            } else {
                Some(atom(SyntaxKind::OPERATOR_ATOM, start))
            }
        }
        // Signed numeric literal: a `+`/`-` glued to an adjacent number folds into
        // a single signed literal rather than a unary prefix call (`-2` → `-2`,
        // `+2.0` → `2.0`, `-2*x` → `(call-i -2 * x)`). Mirrors JuliaSyntax
        // `parse_unary`; see `signed_literal_fold` for the exact conditions.
        k if matches!(k, TokKind::Plus | TokKind::Minus) && signed_literal_fold(ctx, start) => {
            let num = start + 1;
            Some(ExprParse {
                start,
                end: num + 1,
                events: vec![
                    Event::Start(SyntaxKind::LITERAL),
                    Event::Tok(start),
                    Event::Tok(num),
                    Event::Finish,
                ],
            })
        }
        // Prefix operators: arithmetic/logical unary (`-x`, `!x`), the address-of
        // `&x` (a syntactic prefix heading the node with `&`, not `call-pre`),
        // lower-bound type expressions (`<:Real` in `Array{<:Real}`), unary
        // `::` declarations (`::Int` in a method signature `f(::Int)`), the
        // prefix-only Unicode radicals `√ ∛ ∜ ¬` (`√x` → `(call-pre √ x)`, with
        // the same precedence as `-`/`+`), and the unary-capable Unicode
        // arithmetic operators `± ∓ ⋆` (`±x` → `(call-pre ± x)`).
        k if is_unary_prefix_op(k, tok.text) => {
            // A unary arithmetic/logical operator glued to a `(` is a call when
            // the parens look like an argument list (`+(x, y)` → `(call + x y)`,
            // `+(a...)` → `(call + (... a))`, `+(a; b, c)` → `(call + a
            // (parameters b c))`). A single bare operand stays a prefix
            // application (`+(x)` → `(call-pre + x)`). Mirrors JuliaSyntax's
            // paren-call heuristic. The type operators `<:`/`>:` follow the same
            // rule (`<:(a, b)` -> `(<: a b)`, `<:(a)` -> `(<:-pre a)`); the
            // projector heads the call node with the operator. Unary `::` keeps
            // its prefix handling (its paren-call shape differs and is deferred).
            // A *suffixed* operator (`+₁`) is never a unary prefix, so glued to
            // `(` it is always a plain call (`+₁(x)` → `(call +₁ x)`), bypassing
            // the single-arg prefix-application heuristic; without parens it is
            // an error-wrapped prefix call (handled below).
            let op_suffixed = matches!(
                tok.kind,
                TokKind::Plus | TokKind::Minus | TokKind::DotPlus | TokKind::DotMinus
            ) && tok.text.chars().any(is_op_suffix_char);
            let is_unary_paren_op = matches!(
                tok.kind,
                TokKind::Plus
                    | TokKind::Minus
                    // The wrapping `+%`/`-%` are unary-capable like `+`/`-`, so
                    // `+%(x, y)` → `(call +% x y)` and `-%(x)` → `(call-pre -% x)`.
                    | TokKind::PlusPercent
                    | TokKind::MinusPercent
                    | TokKind::DotPlus
                    | TokKind::DotMinus
                    | TokKind::Bang
                    | TokKind::Tilde
                    | TokKind::DotTilde
                    | TokKind::Subtype
                    | TokKind::Supertype
                    // The Unicode radicals and the unary-capable `± ∓ ⋆` (the
                    // only `UniPlus`/`UniTimes` texts that reach this arm)
                    // follow the same paren-call heuristic (`√(a, b)` →
                    // `(call √ a b)`, `±(a)` → `(call-pre ± a)`).
                    | TokKind::UniRadical
                    | TokKind::UniPlus
                    | TokKind::UniTimes
            );
            // A unary operator can head a call when the `(` is glued (existing
            // heuristic) or — for a *call-form* paren (comma/splat/empty/leading
            // `;`) only — separated by horizontal whitespace, where the space is
            // a disallowed-opener error: `+ (a,b)` → `(call + (error) a b)`, but
            // a single operand or block stays a prefix application (`+ (a)` →
            // `(call-pre + a)`, `+ (a; b)` → `(call-pre + (block-p a b))`). A
            // suffixed operator is not a valid unary prefix and projects like an
            // identifier callee (`(error-t)`), a deferred shape, so the spaced
            // path excludes it.
            let paren_idx = ctx.skip_ws(start + 1);
            let spaced = paren_idx > start + 1;
            let next_is_lparen = ctx.token(paren_idx).map(|t| t.kind) == Some(TokKind::LParen);
            let glued_call = !spaced && (op_suffixed || unary_op_paren_is_call(ctx, paren_idx));
            let spaced_call = spaced && !op_suffixed && unary_op_paren_is_call(ctx, paren_idx);
            if is_unary_paren_op && next_is_lparen && (glued_call || spaced_call) {
                if spaced_call {
                    let opener = &ctx.tokens()[paren_idx];
                    push_diagnostic(
                        diagnostics,
                        DiagnosticKind::PrefixOpenerWhitespace,
                        "whitespace before opener",
                        opener.start,
                        opener.start,
                    );
                }
                let (list_events, end) = parse_arg_list(
                    ctx,
                    paren_idx,
                    TokKind::RParen,
                    SyntaxKind::ARG_LIST,
                    flags.end_marker,
                    diagnostics,
                );
                let mut events = vec![Event::Start(SyntaxKind::CALL_EXPR), Event::Tok(start)];
                push_range(&mut events, start + 1, paren_idx);
                events.extend(list_events);
                events.push(Event::Finish);
                return Some(ExprParse { start, end, events });
            }
            let node = if tok.kind == TokKind::ColonColon {
                SyntaxKind::TYPE_ANNOTATION
            } else {
                SyntaxKind::UNARY_EXPR
            };
            let operand_start = ctx.skip_trivia(start + 1);
            // A value-form prefix operator (`+ - ! ~ <: >: .+ .- .~`, the
            // radicals) directly followed by a bare `=` is not a prefix call:
            // the operator is used as a value and `=` is the assignment
            // (`<: =` ⇒ `(= <: (error))`, `+ = x` ⇒ `(= + x)`). The purely
            // syntactic prefixes `&`/`::` instead consume the `=` as an error
            // operand (`& =` ⇒ `(& (error =))`), so they are excluded. Fall
            // through to the bare-value atom; the operator loop forms the
            // assignment (and `error_operator_atom`'s `=` RHS, or its absence).
            if ctx.token(operand_start).map(|t| t.kind) == Some(TokKind::Eq)
                && !matches!(tok.kind, TokKind::Amp | TokKind::ColonColon)
            {
                return Some(atom(SyntaxKind::OPERATOR_ATOM, start));
            }
            // The type operators `<:`/`>:` parse their operand at the `where`
            // tier so a trailing `where` clause attaches to the operand rather
            // than the whole prefix (`<: A where B` ⇒ `(<:-pre (where A B))`,
            // JuliaSyntax issue #21545); the arithmetic/logical prefixes keep the
            // tighter `PREFIX_BP` and suppress `where` in their operand, so a
            // trailing `where` binds the whole prefix instead (`+ <: A where B` ⇒
            // `(where (call-pre + (<:-pre A)) B)`, mirroring JuliaSyntax's
            // `parse_unary` operand sitting below `parse_where`).
            let is_subtype = matches!(tok.kind, TokKind::Subtype | TokKind::Supertype);
            // PREFIX_BP is above the range colon, so `no_range` never changes the
            // operand and is cleared. `array_mode` must carry through, though: the
            // space-form boundary lives in the postfix chain (a whitespace-preceded
            // `(`/`[`/`{` opens a new element), which runs at every binding power,
            // so `[!a (b)]` is `(hcat (call-pre ! a) b)` and `@m !is_leaf(st) (x)`
            // takes `(x)` as the next macro argument, not a spaced call on the
            // operand (`-a (b)` at statement scope, array_mode off, still binds the
            // spaced call).
            let operand_flags = ExprFlags {
                no_range: false,
                array_mode: flags.array_mode,
                no_where: !is_subtype || flags.no_where,
                ..flags
            };
            let operand_bp = if is_subtype { WHERE_BP } else { PREFIX_BP };
            let Some(operand) = parse_expr_in(
                ctx.tokens(),
                operand_start,
                operand_bp,
                diagnostics,
                operand_flags,
            ) else {
                // A bare prefix operator with no operand is the operator used as
                // a value atom (`+` → `+`, `<:` → `<:`, `.+` → `(. +)`). The
                // syntactic `::` has no value form and stays an error (Julia:
                // `::` → `(::-pre (error))`).
                if tok.kind == TokKind::ColonColon {
                    return Some(error_expr_with_range(start, start + 1));
                }
                return Some(atom(SyntaxKind::OPERATOR_ATOM, start));
            };
            // A *suffixed* arithmetic operator (`+₁`, `.-₂`) is not a valid
            // unary prefix operator: JuliaSyntax error-wraps it and applies it
            // as a prefix call (`+₁ x` ⇒ `(call-pre (error +₁) x)`, `.+₁ x` ⇒
            // `(dotcall-pre (error (. +₁)) x)`), mirroring the binary-only-in-
            // prefix arm below. Only the suffix-taking value operators reach
            // here suffixed; `&` keeps its syntactic-prefix reading
            // (`&₁ x` ⇒ `(& x)`), and a bare suffixed operator with no operand
            // stays a value atom (`+₁` ⇒ `+₁`, handled by the `None` arm).
            if op_suffixed {
                push_diagnostic(
                    diagnostics,
                    DiagnosticKind::InvalidPrefixOperator,
                    "invalid operator in prefix position",
                    tok.start,
                    tok.end,
                );
                let mut events = vec![Event::Start(SyntaxKind::UNARY_EXPR)];
                events.extend(error_wrapped_atom(SyntaxKind::OPERATOR_ATOM, start).events);
                push_range(&mut events, start + 1, operand.start);
                events.extend(operand.events);
                finish(&mut events, SyntaxKind::UNARY_EXPR);
                return Some(ExprParse {
                    start,
                    end: operand.end,
                    events,
                });
            }
            let mut events = vec![Event::Start(node)];
            push_range(&mut events, start, operand.start);
            events.extend(operand.events);
            events.push(Event::Finish);
            Some(ExprParse {
                start,
                end: operand.end,
                events,
            })
        }
        // A non-unary operator glued to a `(` is a call with the operator as the
        // callee: `*(x)` → `(call * x)`, `.*(a, b)` → `(call (. *) a b)`. Only the
        // adjacent form is a call (`* (x)` is an error); a space would leave the
        // `(` to be parsed separately. Unary operators (`+`, `-`, `!`, `~`) keep
        // their prefix-application handling above.
        k if is_operator_call_name(k)
            && ctx.token(start + 1).map(|t| t.kind) == Some(TokKind::LParen) =>
        {
            let (list_events, end) = parse_arg_list(
                ctx,
                start + 1,
                TokKind::RParen,
                SyntaxKind::ARG_LIST,
                flags.end_marker,
                diagnostics,
            );
            let mut events = vec![Event::Start(SyntaxKind::CALL_EXPR), Event::Tok(start)];
            events.extend(list_events);
            events.push(Event::Finish);
            Some(ExprParse { start, end, events })
        }
        // A prefix `:` quotes a symbol (`:foo`, `:end`) or expression (`:(x+1)`).
        // A bare `:` not followed by something quotable (`a[:]`, `[:]`, a lone
        // `:`) is the Colon value atom, not a quote: `parse_quote_sym` returns
        // `None` and we fall through to an `OPERATOR_ATOM` (`a[:]` ⇒ `(ref a :)`,
        // `:` ⇒ `:`). Without the fallthrough the bare `:` token is dropped by the
        // projector's delimiter filter.
        TokKind::Colon => parse_quote_sym(
            ctx,
            start,
            diagnostics,
            !flags.field_access_rhs,
            flags.end_marker,
            flags.inside_brackets,
            flags.array_mode,
        )
        .or_else(|| Some(atom(SyntaxKind::OPERATOR_ATOM, start))),
        // A prefix `$` is an interpolation (`$x`, `$(x + y)`). It parses
        // everywhere — Julia only rejects it outside a quote during lowering,
        // not at parse time — so the field-access right-hand side (`f.$x`) and
        // quoted contexts (`:($x)`) reuse the same node.
        TokKind::Dollar => Some(parse_prefix_interpolation(ctx, start, diagnostics)),
        TokKind::At => Some(parse_macro(
            ctx,
            start,
            diagnostics,
            flags.inside_brackets,
            flags.array_mode,
        )),
        TokKind::LParen => parse_paren(ctx, start, flags.end_marker, diagnostics),
        TokKind::LBracket => Some(parse_delimited_literal(
            ctx,
            start,
            flags.end_marker,
            BRACKET_CAT,
            diagnostics,
        )),
        TokKind::LBrace => Some(parse_delimited_literal(
            ctx,
            start,
            flags.end_marker,
            BRACE_CAT,
            diagnostics,
        )),
        TokKind::Ident => Some(atom(SyntaxKind::NAME, start)),
        // `where` is contextual: it is the type-variable operator only *after* a
        // complete expression, which is the operator loop's business. Where an
        // atom is expected it is an ordinary identifier, and Base uses it as one
        // (`for where in keys(graph)`, `if where.name === name`,
        // `identify_package_env(where::PkgId, name)` — all in `loading.jl`).
        TokKind::WhereKw => Some(atom(SyntaxKind::NAME, start)),
        TokKind::StringPrefix | TokKind::StringDelimOpen | TokKind::CmdDelimOpen => {
            Some(parse_string_literal(ctx, start, diagnostics))
        }
        // A char literal with no closing quote (`'`, `'a`) is recovered: the node
        // is still a `LITERAL > CHAR`, but we record `UnterminatedLiteral` at the
        // opening quote so the projector replays JuliaSyntax's missing-close marker
        // (`'` ⇒ `(char (error))`, `'a` ⇒ `(char 'a' (error-t))`).
        TokKind::Char => {
            let tok = &ctx.tokens()[start];
            if !char_token_terminated(tok.text) {
                push_diagnostic(
                    diagnostics,
                    DiagnosticKind::UnterminatedLiteral,
                    "unterminated character literal",
                    tok.start,
                    tok.start,
                );
            }
            Some(atom(SyntaxKind::LITERAL, start))
        }
        TokKind::Integer
        | TokKind::BinInt
        | TokKind::OctInt
        | TokKind::HexInt
        | TokKind::Float
        | TokKind::Float32
        | TokKind::ErrorInvalidNumber
        | TokKind::ErrorHexFloatNoP
        | TokKind::ErrorIdentifierStart
        | TokKind::Unknown
        | TokKind::TrueKw
        | TokKind::FalseKw => Some(atom(SyntaxKind::LITERAL, start)),
        // A lone syntactic operator (`=`, an assignment op, `:=`, `&&`/`||`/`->`,
        // `.`, or `...`) has no value meaning, so JuliaSyntax emits `(error op)`
        // wherever an atom is expected (`=` ⇒ `(error =)`, `.+=` ⇒
        // `(error (. +=))`, `[=]` ⇒ `(vect (error =))`). It consumes only the
        // operator; any following operand is left to the caller — the toplevel
        // trailing-junk driver (`= x` ⇒ `(error =) (error-t x)`) or the operator
        // loop's RHS (`a + =` ⇒ `(call-i a + (error =))`). Unlike
        // `?`/binary-only operators below, it never applies as a prefix call.
        k if is_lone_error_operator(k) => {
            let op = &ctx.tokens()[start];
            push_diagnostic(
                diagnostics,
                DiagnosticKind::LoneOperator,
                "operator is not a valid value",
                op.start,
                op.end,
            );
            Some(error_operator_atom(start))
        }
        // A binary-only operator in prefix position is invalid: JuliaSyntax
        // error-wraps the operator and applies it as a prefix call
        // (`/x` ⇒ `(call-pre (error /) x)`, `.*x` ⇒ `(dotcall-pre (error (. *)) x)`,
        // `?x` ⇒ `(call-pre (error ?) x)`). With nothing parseable following, a
        // value operator stays a bare value atom (`*` ⇒ `*`, `.&` ⇒ `(. &)`,
        // `=>` ⇒ `=>`) but a bare `?` is itself the error (`?` ⇒ `(error ?)`); the
        // unary value operators (`+ - ! ~ <: >:`) are folded above and never reach
        // here. Lone syntactic operators are handled by the arm above.
        k if is_value_operator(k) || k == TokKind::Question => {
            let operand_start = ctx.skip_trivia(start + 1);
            // A value operator directly followed by a bare `=` is its value form
            // with `=` the assignment, not an invalid prefix call (`* =` ⇒
            // `(= * (error))`, `/ = x` ⇒ `(= / x)`). `?` is excluded — it keeps
            // its prefix-call handling. Fall through to the bare-value atom.
            if k != TokKind::Question
                && ctx.token(operand_start).map(|t| t.kind) == Some(TokKind::Eq)
            {
                return Some(atom(SyntaxKind::OPERATOR_ATOM, start));
            }
            // The operand binds at `PREFIX_BP` — tighter than the arithmetic
            // tiers (`/x + y` ⇒ `(call-i (call-pre (error /) x) + y)`) but below
            // `^` (`/x^2` ⇒ `(call-pre (error /) (call-i x ^ 2))`). The
            // array-element boundary never applies to a prefix operand.
            let operand_flags = ExprFlags {
                no_range: false,
                array_mode: false,
                ..flags
            };
            // A binary-only operator does not reach across a significant newline
            // for its operand: at statement scope and inside array brackets (where
            // a newline is a row separator) the newline ends the statement/row, so
            // the operator stays a bare value atom (`/\nx` ⇒ `/` then `x`,
            // `[/\nx]` ⇒ `(vcat / x)`, `?\nx` ⇒ `(error ?)` then `x`); inside
            // parens the newline is insignificant and the operand is consumed
            // (`(/\nx)` ⇒ `(call-pre (error /) x)`). Mirrors the range colon.
            let newline_significant = !flags.inside_brackets || flags.array_mode;
            let newline_stop = newline_significant
                && ctx.token(ctx.skip_ws(start + 1)).map(|t| t.kind) == Some(TokKind::Newline);
            let operand = if newline_stop {
                None
            } else {
                parse_expr_in(
                    ctx.tokens(),
                    operand_start,
                    PREFIX_BP,
                    diagnostics,
                    operand_flags,
                )
            };
            match operand {
                Some(operand) => {
                    let op = &ctx.tokens()[start];
                    push_diagnostic(
                        diagnostics,
                        DiagnosticKind::InvalidPrefixOperator,
                        "invalid operator in prefix position",
                        op.start,
                        op.end,
                    );
                    // Wrap the operator in an `ERROR` (an `OPERATOR_ATOM` so a
                    // broadcast operator still projects to `(. op)`), then the
                    // operand, under a `UNARY_EXPR` the projector renders as a
                    // prefix call with the error-wrapped operator as callee.
                    let mut events = vec![Event::Start(SyntaxKind::UNARY_EXPR)];
                    events.extend(error_wrapped_atom(SyntaxKind::OPERATOR_ATOM, start).events);
                    push_range(&mut events, start + 1, operand.start);
                    events.extend(operand.events);
                    finish(&mut events, SyntaxKind::UNARY_EXPR);
                    Some(ExprParse {
                        start,
                        end: operand.end,
                        events,
                    })
                }
                None if matches!(ctx.tokens()[start].kind, TokKind::Question) => {
                    let op = &ctx.tokens()[start];
                    push_diagnostic(
                        diagnostics,
                        DiagnosticKind::LoneOperator,
                        "operator is not a valid value",
                        op.start,
                        op.end,
                    );
                    Some(error_operator_atom(start))
                }
                None => Some(atom(SyntaxKind::OPERATOR_ATOM, start)),
            }
        }
        _ => None,
    }
}

/// The token at `start` wrapped as `ERROR > inner > tok` — JuliaSyntax's
/// `(error x)` atom for a token used where a value is expected.
///
/// `inner` preserves the token's own projection: `NAME` for a reserved keyword
/// standing in for an identifier (`(error try)`), `OPERATOR_ATOM` for a
/// syntactic operator, which also keeps the broadcast projection
/// (`.+=` ⇒ `(. +=)`).
fn error_wrapped_atom(inner: SyntaxKind, start: usize) -> ExprParse {
    let mut events = vec![
        Event::Start(SyntaxKind::ERROR),
        Event::Start(inner),
        Event::Tok(start),
    ];
    finish(&mut events, inner);
    finish(&mut events, SyntaxKind::ERROR);
    ExprParse {
        start,
        end: start + 1,
        events,
    }
}

/// The `(error <kw>)` atom for a reserved keyword misused as a signature name.
fn keyword_name_error_atom(start: usize) -> ExprParse {
    error_wrapped_atom(SyntaxKind::NAME, start)
}

/// Whether `kind` is a hard reserved keyword that JuliaSyntax error-wraps when it
/// appears as a signature name. The contextual words Julia keeps as plain names
/// in that position (`mutable`, `where`, `true`/`false`; and `abstract`/
/// `primitive`/`type`/`outer`/`in`/`isa`/`public`, which Fatou already lexes as
/// identifiers) are excluded.
fn is_name_error_keyword(kind: TokKind) -> bool {
    kind.is_keyword()
        && !matches!(
            kind,
            TokKind::MutableKw | TokKind::WhereKw | TokKind::TrueKw | TokKind::FalseKw
        )
}

/// A char-literal token is terminated when it carries a closing quote: text of
/// length ≥ 2 ending in `'` (the empty `''` and a normal `'a'` both qualify). A
/// bare `'` or content with no closing quote (`'a`) is unterminated.
fn char_token_terminated(text: &str) -> bool {
    text.len() >= 2 && text.ends_with('\'')
}

/// The `(error op)` atom for a syntactic operator used where a value is expected.
fn error_operator_atom(start: usize) -> ExprParse {
    error_wrapped_atom(SyntaxKind::OPERATOR_ATOM, start)
}

/// Whether `kind` is a syntactic operator that has no value meaning and so, where
/// an atom is expected, is JuliaSyntax's `(error op)` — the assignment operators
/// (`=`, `+=`, `.+=`, …), `:=`, the short-circuits `&&`/`||`, the
/// anonymous-function `->`, the field-access `.`, and the splat `...`. `?` is
/// *not* here: it applies as a prefix call when an operand follows (handled in
/// the value-operator arm).
/// Whether the token can head a unary prefix application: the ASCII unary
/// operators, the syntactic prefixes `&`/`::`, the prefix-only Unicode radicals
/// (`√ ∛ ∜ ¬`), and the unary-capable Unicode arithmetic operators `± ∓ ⋆` —
/// the only members of their tiers Julia accepts as unary, matched by exact
/// text (a suffixed `±₁` is not a unary prefix and falls through to the
/// operator-call-name arm, where glued to `(` it is a plain call).
/// Whether `kind` is an operator that, alone in value position, is the operator
/// used as a value atom (`+` → `+`, `.&` → `(. &)`, `:` → `:`). This is the
/// non-syntactic operator set: undotted operator names (minus the syntactic
/// `&&`/`||`/`->`), the broadcast forms, plus `:`/`..`, the Unicode radicals,
/// and the Unicode infix tiers (`a[≤]` → `(ref a ≤)`, `≥ = 1` → `(= ≥ 1)`;
/// in prefix position with an operand they error-wrap like the ASCII
/// binary-only operators, `≠a` → `(call-pre (error ≠) a)`).
/// The erroring syntactic operators (`= :: && || -> ? . ...` and assignment)
/// are excluded — Julia reports them as errors in value position.
/// Whether `kind` is an operator that a prefix `:` quotes into a symbol but that
/// is *not* already covered by `is_op_name`/`is_assignment_op`: the range `..`,
/// the field-access dot `.` (`:.` ⇒ `(quote-: .)`, the `Expr(:., …)` head), the
/// Unicode operators and radicals, and the ternary `?`. Julia quotes all of these
/// (`:..` ⇒ `(quote-: ..)`, `:√` ⇒ `(quote-: √)`, `:?` ⇒ `(quote-: ?)`). The
/// broadcast dotted operators (`:.+`) are handled by their own quote arm; the
/// remaining syntactic sigils `$`/`...` are still deferred.
/// Whether `kind` is a middle/closing block keyword (`end`/`else`/`elseif`/
/// `catch`/`finally`) — one that only closes or continues an enclosing block and
/// so cannot stand as a value. Mirrors `core::is_stray_block_keyword_tok`.
fn is_closing_block_keyword(kind: TokKind) -> bool {
    matches!(
        kind,
        TokKind::EndKw
            | TokKind::ElseKw
            | TokKind::ElseifKw
            | TokKind::CatchKw
            | TokKind::FinallyKw
    )
}

/// Parse a prefix `:` quote into a `QUOTE_SYM` node: `:name`/`:end` (a symbol)
/// or `:(expr)` (a quoted expression). Returns `None` for a bare `:` that is not
/// followed by a quotable token (e.g. the index colon in `a[:]`), so the caller
/// falls through to its normal handling.
pub(super) fn parse_quote_sym(
    ctx: &ParserCtx<'_>,
    start: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
    value_position: bool,
    end_marker: bool,
    // Inherited space-sensitivity, forwarded to a quoted macro call so its
    // space-argument loop ends at a generator's `for` exactly as an unquoted one
    // does (`[:@m x for x in xs]`).
    inside_brackets: bool,
    array_mode: bool,
) -> Option<ExprParse> {
    let next = ctx.skip_trivia(start + 1);
    // A space-separated *closing* block keyword (`end`/`else`/`elseif`/`catch`/
    // `finally`) at value position is not a quotable symbol: `: end` ⇒ a bare `:`
    // Colon atom with the keyword spilling as trailing junk. Decline before
    // recording the whitespace diagnostic so the bare `:` carries none. The glued
    // form still quotes (`:end`, hence the spacing gate); an index `a[: end]`
    // (`end_marker`) keeps `end` quotable; a field-access RHS `A.: end`
    // (`!value_position`) keeps the quote.
    if value_position
        && next > start + 1
        && ctx
            .token(next)
            .is_some_and(|t| is_closing_block_keyword(t.kind))
        && !(ctx.token(next).map(|t| t.kind) == Some(TokKind::EndKw) && end_marker)
    {
        return None;
    }
    let mut events = vec![Event::Start(SyntaxKind::QUOTE_SYM), Event::Tok(start)];
    push_range(&mut events, start + 1, next);
    // Whitespace (or a newline) between the `:` and the quoted symbol is
    // disallowed: it records a `QuoteColonWhitespace` diagnostic at the `:`'s end,
    // projected as a leading `(error-t)` (`: foo` ⇒ `(quote-: (error-t) foo)`,
    // `A.: +` ⇒ `(. A (quote-: (error-t) +))`). `:foo` (glued) has no diagnostic.
    if next > start + 1 {
        let colon = &ctx.tokens()[start];
        push_diagnostic(
            diagnostics,
            DiagnosticKind::QuoteColonWhitespace,
            "whitespace after `:`",
            colon.end,
            colon.end,
        );
    }
    match ctx.token(next).map(|t| t.kind)? {
        // `:(op)` — a lone operator quoted in parens, e.g. `:(=)`, `:(::)`,
        // `:(:)`, `:(+)`, `:(.=)`, `:(.)`, `:(...)`. In a quote context a bare
        // operator (including the syntactic `=`/`::`/`.`/`...` and the broadcast
        // assignments that are errors in value position) is a symbol. Build a
        // `PAREN_EXPR` around an `OPERATOR_ATOM` holding the operator token, the
        // same node the non-paren form `:.=` uses, so the projector splits a
        // broadcast dot off the same way (`:(.=)` ⇒ `(quote-: (. =))`).
        TokKind::LParen
            if {
                let op = ctx.skip_trivia(next + 1);
                is_paren_quotable_op(ctx.token(op).map(|t| t.kind))
                    && ctx.token(ctx.skip_trivia(op + 1)).map(|t| t.kind) == Some(TokKind::RParen)
            } =>
        {
            let op = ctx.skip_trivia(next + 1);
            let rparen = ctx.skip_trivia(op + 1);
            events.push(Event::Start(SyntaxKind::PAREN_EXPR));
            push_range(&mut events, next, op);
            events.push(Event::Start(SyntaxKind::OPERATOR_ATOM));
            events.push(Event::Tok(op));
            finish(&mut events, SyntaxKind::OPERATOR_ATOM);
            push_range(&mut events, op + 1, rparen + 1);
            finish(&mut events, SyntaxKind::PAREN_EXPR);
            finish(&mut events, SyntaxKind::QUOTE_SYM);
            Some(ExprParse {
                start,
                end: rparen + 1,
                events,
            })
        }
        // `:(end)`/`:(else)`/`:(catch)` — the paren body opens with a closing
        // block keyword, which can't start an expression. JuliaSyntax recovers
        // with a zero-width `(error-t)` quoted form (the quote spans `:(`), then
        // spills the keyword and the rest of the line to the trailing-junk driver
        // (`:(end)` ⇒ `(quote-: (error-t)) (error-t end ✘)`). The `(` stays a
        // loose `QUOTE_SYM` child; an `EmptyQuoteParen` diagnostic at its end
        // drives the projector's `(error-t)` reconstruction.
        TokKind::LParen
            if ctx
                .token(ctx.skip_trivia(next + 1))
                .is_some_and(|t| is_closing_block_keyword(t.kind)) =>
        {
            events.push(Event::Tok(next)); // `(`
            finish(&mut events, SyntaxKind::QUOTE_SYM);
            let lparen = &ctx.tokens()[next];
            push_diagnostic(
                diagnostics,
                DiagnosticKind::EmptyQuoteParen,
                "expected expression after `:(`",
                lparen.end,
                lparen.end,
            );
            Some(ExprParse {
                start,
                end: next + 1,
                events,
            })
        }
        // `:(expr)` — the parenthesized expression is the quoted form.
        TokKind::LParen => {
            let paren = parse_paren(ctx, next, false, diagnostics)?;
            let end = paren.end;
            events.extend(paren.events);
            events.push(Event::Finish);
            Some(ExprParse { start, end, events })
        }
        // `:var"…"` — a quoted non-standard identifier. Under a quote Julia
        // parses the operand as a plain atom, where `var"…"` keeps its
        // identifier meaning, so the quoted form is the `(var …)` name itself
        // (`:var"dict key"` ⇒ `(quote-: (var dict key))`). Triple-quoted
        // `var"""…"""` is an ordinary `@var_str` string macro and is excluded.
        TokKind::StringPrefix if is_var_identifier_start(ctx, next) => {
            let end = push_var_macro_name(ctx, &mut events, next, diagnostics)?;
            finish(&mut events, SyntaxKind::QUOTE_SYM);
            Some(ExprParse { start, end, events })
        }
        // `:@m` — a quoted macro call. Julia quotes the whole call, space
        // arguments and all (`:@doc x` ⇒ `(quote-: (macrocall @doc x))`), so
        // this is the ordinary macro parser under the quote rather than a bare
        // name. Base writes the argument-less form to attach a docstring to a
        // macro (`"""…"""\n:@MethodTable`).
        TokKind::At => {
            let mac = parse_macro(ctx, next, diagnostics, inside_brackets, array_mode);
            let end = mac.end;
            events.extend(mac.events);
            finish(&mut events, SyntaxKind::QUOTE_SYM);
            Some(ExprParse { start, end, events })
        }
        // `:name` — an identifier symbol.
        TokKind::Ident => {
            events.push(Event::Start(SyntaxKind::NAME));
            events.push(Event::Tok(next));
            events.push(Event::Finish);
            events.push(Event::Finish);
            Some(ExprParse {
                start,
                end: next + 1,
                events,
            })
        }
        // `:1`, `:1.5`, `:0x10`, `:'c'` — a quoted literal. Julia parses a
        // quote's operand as a plain atom, and a literal is one. Without this the
        // `:` fell back to a bare Colon atom and the literal became a separate
        // element, so `w.ext[:14878]` read as a two-element `typed_hcat` — which
        // the formatter then printed as `[: 14878]`, output that no longer parses.
        TokKind::Integer
        | TokKind::BinInt
        | TokKind::OctInt
        | TokKind::HexInt
        | TokKind::Float
        | TokKind::Float32
        | TokKind::ErrorInvalidNumber
        | TokKind::ErrorHexFloatNoP
        | TokKind::Char => {
            events.push(Event::Start(SyntaxKind::LITERAL));
            events.push(Event::Tok(next));
            finish(&mut events, SyntaxKind::LITERAL);
            finish(&mut events, SyntaxKind::QUOTE_SYM);
            Some(ExprParse {
                start,
                end: next + 1,
                events,
            })
        }
        // `:"str"`, `` :`cmd` `` — a quoted string/command literal.
        TokKind::StringDelimOpen | TokKind::CmdDelimOpen => {
            let lit = parse_string_literal(ctx, next, diagnostics);
            let end = lit.end;
            events.extend(lit.events);
            finish(&mut events, SyntaxKind::QUOTE_SYM);
            Some(ExprParse { start, end, events })
        }
        // `:.+`, `:.&`, `:.=`, `:.&&`, `:.+=` — a quoted *dotted* (broadcast)
        // operator. Julia models the dotted operator as a `(. op)` access, so
        // `:.+` ⇒ `(quote-: (. +))`. Wrap the token in an `OPERATOR_ATOM` (Fatou's
        // operator-as-value node), which the projector splits the broadcast dot
        // off of. The `..`/`...` range/splat operators are not broadcasts and
        // fall through to the bare-operator arm below (`:..` ⇒ `(quote-: ..)`).
        _ if ctx
            .token(next)
            .is_some_and(|t| is_dotted_broadcast_text(t.text)) =>
        {
            events.push(Event::Start(SyntaxKind::OPERATOR_ATOM));
            events.push(Event::Tok(next));
            finish(&mut events, SyntaxKind::OPERATOR_ATOM);
            finish(&mut events, SyntaxKind::QUOTE_SYM);
            Some(ExprParse {
                start,
                end: next + 1,
                events,
            })
        }
        // `:+`, `:<:`, `:+=`, `:..`, `:√`, `:⊕`, `:?`, … — a symbolic operator
        // used as a symbol. Covers undotted operator names (`is_op_name`),
        // assignment operators, and the remaining value/syntactic operators Julia
        // still quotes (`..`, the Unicode operators and radicals, the ternary `?`);
        // broadcast forms like `:.+` are handled by the dotted-operator arm above.
        // The token text is emitted verbatim; the projector reads it back.
        k if is_op_name(k) || is_assignment_op(k) || is_quotable_operator(k) => {
            events.push(Event::Tok(next));
            events.push(Event::Finish);
            Some(ExprParse {
                start,
                end: next + 1,
                events,
            })
        }
        // `:end`, `:function`, … — a keyword used as a symbol. (A value-position
        // `: end` with a closing block keyword declined above.)
        k if k.is_keyword() => {
            events.push(Event::Tok(next));
            events.push(Event::Finish);
            Some(ExprParse {
                start,
                end: next + 1,
                events,
            })
        }
        // A bare `:` (e.g. `a[:]`) is not a quote.
        _ => None,
    }
}

/// Assemble a string (`"..."`) or command (`` `...` ``) literal from its flat
/// token run into a `STRING_LITERAL`/`CMD_LITERAL` node. The run is: an optional
/// prefix, an open delimiter, a sequence of content chunks and interpolations,
/// the close delimiter, and an optional suffix. An unterminated literal (no close
/// delimiter) simply stops early — every consumed token is still emitted.
fn parse_string_literal(
    ctx: &ParserCtx<'_>,
    start: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> ExprParse {
    let mut i = start;

    // Optional non-standard literal prefix (`r`, `raw`, …). The `var` prefix is
    // special: `var"…"` (single-quoted only) is a non-standard *identifier*, not
    // a string macro — Julia models it as `(var name)`. Triple-quoted `var"""…"""`
    // stays an ordinary `@var_str` macrocall.
    let mut var_prefix = false;
    let mut has_prefix = false;
    if ctx.token(i).map(|t| t.kind) == Some(TokKind::StringPrefix) {
        has_prefix = true;
        var_prefix = ctx.token(i).map(|t| t.text) == Some("var");
        i += 1;
    }

    let single_quote_open = matches!(
        ctx.token(i),
        Some(t) if t.kind == TokKind::StringDelimOpen && t.text.len() == 1
    );
    let node = match ctx.token(i).map(|t| t.kind) {
        Some(TokKind::CmdDelimOpen) => SyntaxKind::CMD_LITERAL,
        Some(TokKind::StringDelimOpen) if var_prefix && single_quote_open => {
            SyntaxKind::NONSTANDARD_IDENTIFIER
        }
        _ => SyntaxKind::STRING_LITERAL,
    };
    let close_kind = if node == SyntaxKind::CMD_LITERAL {
        TokKind::CmdDelimClose
    } else {
        TokKind::StringDelimClose
    };

    let mut events = vec![Event::Start(node)];
    for idx in start..=i {
        events.push(Event::Tok(idx));
    }
    i += 1; // past the open delimiter

    loop {
        match ctx.token(i).map(|t| t.kind) {
            Some(TokKind::StringContent) => {
                events.push(Event::Tok(i));
                i += 1;
            }
            Some(TokKind::Dollar) => {
                i = parse_interpolation(ctx, &mut events, i, true, diagnostics);
            }
            Some(k) if k == close_kind => {
                events.push(Event::Tok(i));
                i += 1;
                let suffix = ctx.token(i).map(|t| t.kind);
                // A `var"…"` non-standard identifier takes no flags: a glued
                // suffix (a flag-like alpha run lexed as `StringSuffix`, or a
                // digit-led numeric literal) is junk. Consume it as a child
                // token and record a `StringSuffixSpace` diagnostic (projected
                // `(error-t)`: `var"x"y`/`var"x"1`/`var"x"end` ⇒
                // `(var x (error-t))`). A glued postfix opener (`[ ( { ' .`) or
                // operator is *not* a suffix here — it chains/binds in the outer
                // parser, so only these atom-like kinds trigger recovery.
                if node == SyntaxKind::NONSTANDARD_IDENTIFIER {
                    if matches!(
                        suffix,
                        Some(
                            TokKind::StringSuffix
                                | TokKind::Integer
                                | TokKind::Float
                                | TokKind::Float32
                                | TokKind::BinInt
                                | TokKind::OctInt
                                | TokKind::HexInt
                        )
                    ) {
                        events.push(Event::Tok(i));
                        i += 1;
                        let lit = &ctx.tokens()[start];
                        push_diagnostic(
                            diagnostics,
                            DiagnosticKind::StringSuffixSpace,
                            "invalid string-macro suffix",
                            lit.start,
                            lit.start,
                        );
                    }
                    break;
                }
                // Optional suffix glued after the close delimiter of a string or
                // command macro: a flag run (`r"pat"ims` → `"ims"`) or a numeric
                // literal (`x`s`2` → an extra `2` macrocall argument). A digit-led
                // suffix is lexed as an ordinary number, so capture it into the
                // literal node here; the projector renders it as the trailing
                // argument.
                let is_flag = suffix == Some(TokKind::StringSuffix);
                let is_numeric = has_prefix
                    && matches!(node, SyntaxKind::STRING_LITERAL | SyntaxKind::CMD_LITERAL)
                    && matches!(
                        suffix,
                        Some(TokKind::Integer | TokKind::Float | TokKind::Float32)
                    );
                if is_flag || is_numeric {
                    events.push(Event::Tok(i));
                    i += 1;
                }
                break;
            }
            // Unterminated: anything else (incl. EOF) ends the literal. A
            // string/command/`var"…"` literal with no closing delimiter records an
            // `UnterminatedLiteral` diagnostic, projected as a truncation
            // `(error-t)` inside its body (`"str` → `(string "str" (error-t))`,
            // `var"x` → `(var x (error-t))`).
            _ => {
                let lit = &ctx.tokens()[start];
                push_diagnostic(
                    diagnostics,
                    DiagnosticKind::UnterminatedLiteral,
                    "unterminated literal",
                    lit.start,
                    lit.start,
                );
                break;
            }
        }
    }

    events.push(Event::Finish);
    ExprParse {
        start,
        end: i,
        events,
    }
}

/// Parse a standalone `$…` interpolation in expression position. `$ident` and
/// `$(expr)` reuse the string-context [`parse_interpolation`]; any other operand
/// (`$$a`, `$[1, 2]`, `$"s"`) binds `$` to the next *prefix atom* — tightly, with
/// no postfix — so `$a.b` is `(. ($ a) …)` and `$$a` is `($ ($ a))`.
pub(super) fn parse_prefix_interpolation(
    ctx: &ParserCtx<'_>,
    dollar: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> ExprParse {
    let next = dollar + 1;
    if matches!(
        ctx.token(next).map(|t| t.kind),
        Some(TokKind::LParen | TokKind::Ident)
    ) {
        let mut events = Vec::new();
        let end = parse_interpolation(ctx, &mut events, dollar, false, diagnostics);
        return ExprParse {
            start: dollar,
            end,
            events,
        };
    }

    let mut events = vec![Event::Start(SyntaxKind::INTERPOLATION), Event::Tok(dollar)];
    match parse_prefix(ctx, next, diagnostics, ExprFlags::default()) {
        Some(operand) => {
            push_range(&mut events, next, operand.start);
            let end = operand.end;
            events.extend(operand.events);
            events.push(Event::Finish);
            ExprParse {
                start: dollar,
                end,
                events,
            }
        }
        // A bare `$` with no operand: emit just the sigil.
        None => {
            events.push(Event::Finish);
            ExprParse {
                start: dollar,
                end: next,
                events,
            }
        }
    }
}

/// Parse one `$ident` or `$(expr)` interpolation into an `INTERPOLATION` node,
/// returning the token index just past it. `$(...)` interiors reuse the Pratt
/// parser, so they become real expression subtrees.
///
/// `in_string` marks a string/command literal's interpolation, the only context
/// where the multi-value paren forms are rejected — in expression position
/// (`quote`/`:(…)`/a bare `$`) Julia accepts them, since the interpolated value
/// is an ordinary expression rather than something to be stringified.
fn parse_interpolation(
    ctx: &ParserCtx<'_>,
    events: &mut Vec<Event>,
    dollar: usize,
    in_string: bool,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> usize {
    events.push(Event::Start(SyntaxKind::INTERPOLATION));
    events.push(Event::Tok(dollar)); // `$`
    let next = dollar + 1;

    match ctx.token(next).map(|t| t.kind) {
        Some(TokKind::LParen) => {
            // The parenthesized interpolation operand parses exactly like any
            // other parenthesized expression: a single expression (`$(x+y)`) is a
            // `PAREN_EXPR` the projector unwraps, while the multi-value forms
            // `$(x;y)` (`PAREN_BLOCK`), `$(x,y)` (`TUPLE_EXPR`), `$(x for …)`
            // (`GENERATOR`), and the empty `$()` (`TUPLE_EXPR`) are what
            // JuliaSyntax rejects as a `(error …)` interpolation — but only
            // inside a string, where the value has to be stringified.
            let Some(inner) = parse_paren(ctx, next, false, diagnostics) else {
                events.push(Event::Finish);
                return next + 1;
            };
            if in_string
                && matches!(
                    inner.events.first(),
                    Some(Event::Start(
                        SyntaxKind::PAREN_BLOCK | SyntaxKind::TUPLE_EXPR | SyntaxKind::GENERATOR
                    ))
                )
            {
                let dollar_tok = &ctx.tokens()[dollar];
                push_diagnostic(
                    diagnostics,
                    DiagnosticKind::InvalidInterpolation,
                    "interpolation expects a single expression",
                    dollar_tok.start,
                    dollar_tok.end,
                );
            }
            let end = inner.end;
            events.extend(inner.events);
            events.push(Event::Finish);
            end
        }
        Some(TokKind::Ident) => {
            events.push(Event::Tok(next));
            events.push(Event::Finish);
            next + 1
        }
        // A lone `$` (the lexer folds non-operand dollars into content, so this
        // is unreachable in practice) — emit just the sigil.
        _ => {
            events.push(Event::Finish);
            next
        }
    }
}

/// A single-token atom wrapped in `kind` (`NAME` or `LITERAL`).
fn atom(kind: SyntaxKind, idx: usize) -> ExprParse {
    ExprParse {
        start: idx,
        end: idx + 1,
        events: vec![Event::Start(kind), Event::Tok(idx), Event::Finish],
    }
}

pub(crate) fn parse_paren(
    ctx: &ParserCtx<'_>,
    start: usize,
    // Inherited index-marker context. A paren is not itself indexing, but
    // `a[(end)]` inherits the marker into the parenthesized expression.
    end_marker: bool,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let inner_start = ctx.skip_trivia(start + 1);

    // Empty `()` is the empty tuple; a leading `;` (`(; a=1)`) is a named-tuple
    // parameter section. Both are tuples, handled by the arg-list machinery.
    match ctx.token(inner_start).map(|t| t.kind) {
        Some(TokKind::RParen) => {
            let mut events = vec![Event::Start(SyntaxKind::TUPLE_EXPR)];
            push_range(&mut events, start, inner_start + 1);
            events.push(Event::Finish);
            return Some(ExprParse {
                start,
                end: inner_start + 1,
                events,
            });
        }
        Some(TokKind::Semicolon) => {
            let (events, end) = parse_arg_list(
                ctx,
                start,
                TokKind::RParen,
                paren_list_kind(ctx, start),
                end_marker,
                diagnostics,
            );
            return Some(ExprParse { start, end, events });
        }
        _ => {}
    }

    // `(op)` — a lone non-syntactic operator in parens is the operator as a
    // value, e.g. `(+)` → `+`, `(:)` → `:`, `(<:)` → `<:`. Build a `PAREN_EXPR`
    // wrapping the bare operator token (the projector reads a lone-operator paren
    // as the operator's text). Postfix application (`(+)(a, b)`) then makes it a
    // call callee. Whitespace-insensitive: `( + )` is the same.
    if is_paren_value_op(ctx.token(inner_start).map(|t| t.kind)) {
        let close = ctx.skip_trivia(inner_start + 1);
        if ctx.token(close).map(|t| t.kind) == Some(TokKind::RParen) {
            let mut events = vec![Event::Start(SyntaxKind::PAREN_EXPR)];
            push_range(&mut events, start, close + 1);
            events.push(Event::Finish);
            return Some(ExprParse {
                start,
                end: close + 1,
                events,
            });
        }
    }

    let Some(inner) = parse_expr_in_brackets(ctx.tokens(), inner_start, 0, end_marker, diagnostics)
    else {
        // Only trivia remains to EOF: the paren can never be closed, so report
        // it like the non-empty case below. A non-EOF failure (`(,1)`, `[(]`)
        // may still have its `)` ahead, so it stays with outer recovery.
        if ctx.token(inner_start).is_none() {
            let open = &ctx.tokens()[start];
            push_diagnostic(
                diagnostics,
                DiagnosticKind::UnclosedParen,
                "unclosed `(`",
                open.start,
                open.end,
            );
        }
        return Some(error_expr_with_range(start, inner_start));
    };

    // `(x for x in xs)` is a generator expression.
    let sep = ctx.skip_trivia(inner.end);
    if ctx.token(sep).map(|t| t.kind) == Some(TokKind::ForKw) {
        // A semicolon after a parenthesized generator is not a parameter section:
        // Julia retains the generator, then recovers the suffix as junk inside
        // the parens. Detect it before the ordinary comprehension path stops at
        // the semicolon and leaks the suffix to the top-level statement driver.
        let mut scratch = Vec::new();
        let mut scratch_diags = Vec::new();
        let clauses_end = parse_generator_clauses(ctx, inner.end, &mut scratch, &mut scratch_diags);
        if ctx.token(ctx.skip_trivia(clauses_end)).map(|t| t.kind) == Some(TokKind::Semicolon) {
            return Some(parse_parenthesized_generator_junk(
                ctx,
                start,
                inner,
                diagnostics,
            ));
        }
        return Some(parse_comprehension(
            ctx,
            start,
            inner,
            SyntaxKind::GENERATOR,
            TokKind::RParen,
            diagnostics,
        ));
    }

    // A `,` or `;` after the first element makes this a tuple (or named tuple).
    // Re-parse the whole parenthesized run as an argument list so each element
    // becomes an `ARG`/`KEYWORD_ARG` and `;` opens a `PARAMETERS` section.
    if matches!(
        ctx.token(sep).map(|t| t.kind),
        Some(TokKind::Comma | TokKind::Semicolon)
    ) {
        let (events, end) = parse_arg_list(
            ctx,
            start,
            TokKind::RParen,
            paren_list_kind(ctx, start),
            end_marker,
            diagnostics,
        );
        return Some(ExprParse { start, end, events });
    }

    // Otherwise a single parenthesized expression: `(a)` grouping.
    let mut events = vec![Event::Start(SyntaxKind::PAREN_EXPR)];
    push_range(&mut events, start, inner.start);
    events.extend(inner.events);

    let close = ctx.skip_trivia(inner.end);
    if ctx.token(close).map(|t| t.kind) == Some(TokKind::RParen) {
        push_range(&mut events, inner.end, close + 1);
        events.push(Event::Finish);
        Some(ExprParse {
            start,
            end: close + 1,
            events,
        })
    } else {
        let open = &ctx.tokens()[start];
        push_diagnostic(
            diagnostics,
            DiagnosticKind::UnclosedParen,
            "unclosed `(`",
            open.start,
            open.end,
        );
        push_range(&mut events, inner.end, close);
        events.push(Event::Finish);
        Some(ExprParse {
            start,
            end: close,
            events,
        })
    }
}

/// Parse `(body for clauses; junk)` as a parenthesized generator followed by a
/// recovered suffix. Unlike call and curly argument lists, plain parens cannot
/// attach keyword parameters to a generator.
fn parse_parenthesized_generator_junk(
    ctx: &ParserCtx<'_>,
    open: usize,
    body: ExprParse,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> ExprParse {
    let tokens = ctx.tokens();
    let mut events = vec![Event::Start(SyntaxKind::PAREN_EXPR), Event::Tok(open)];
    push_range(&mut events, open + 1, body.start);
    events.push(Event::Start(SyntaxKind::GENERATOR));
    let mut pos = body.end;
    events.extend(body.events);
    pos = parse_generator_clauses(ctx, pos, &mut events, diagnostics);
    finish(&mut events, SyntaxKind::GENERATOR);

    let junk_start = ctx.skip_trivia(pos);
    push_range(&mut events, pos, junk_start);
    let mut close = junk_start;
    let mut depth = 0usize;
    while let Some(kind) = ctx.token(close).map(|t| t.kind) {
        match kind {
            TokKind::RParen if depth == 0 => break,
            TokKind::LParen | TokKind::LBracket | TokKind::LBrace => depth += 1,
            TokKind::RParen | TokKind::RBracket | TokKind::RBrace => {
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
        close += 1;
    }

    events.push(Event::Start(SyntaxKind::ERROR));
    push_range(&mut events, junk_start, close);
    finish(&mut events, SyntaxKind::ERROR);
    push_diagnostic(
        diagnostics,
        DiagnosticKind::TrailingJunk,
        "generator parameters require a call or curly expression",
        tokens[junk_start].start,
        tokens[junk_start].end,
    );

    let end = if ctx.token(close).map(|t| t.kind) == Some(TokKind::RParen) {
        events.push(Event::Tok(close));
        close + 1
    } else {
        let opener = &tokens[open];
        push_diagnostic(
            diagnostics,
            DiagnosticKind::UnclosedParen,
            "unclosed `(`",
            opener.start,
            opener.end,
        );
        close
    };
    finish(&mut events, SyntaxKind::PAREN_EXPR);
    ExprParse {
        start: open,
        end,
        events,
    }
}

fn parse_postfix_chain(
    ctx: &ParserCtx<'_>,
    mut lhs: ExprParse,
    array_mode: bool,
    // Whether `end`/`begin` are index markers here, inherited from an enclosing
    // indexing bracket (`a[…]`). A nested call/index/curly propagates it so
    // `a[f(end)]` keeps `end` a marker.
    end_marker: bool,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> ExprParse {
    loop {
        // No newline between the callee and `(`/`[` — only horizontal space, of
        // which a `#= … =#` block comment is a form (`f #=c=# (x)` chains the
        // call exactly as `f (x)` does, opener-whitespace error included).
        let next = ctx.skip_ws_and_block_comments(lhs.end);
        // In a space-sensitive position (an array-literal element or a space-form
        // macro argument), a `(`/`[`/`{` with whitespace before it begins a new
        // element rather than chaining as a call/index/curly: `[f (x)]` is
        // `(hcat f x)` and `@foo f (x)` two arguments, while `[f(x)]` is
        // `(vect (call f x))`. Mirrors JuliaSyntax's `space_sensitive` splitting.
        if array_mode
            && next > lhs.end
            && matches!(
                ctx.token(next).map(|t| t.kind),
                Some(TokKind::LParen | TokKind::LBracket | TokKind::LBrace)
            )
        {
            break;
        }
        // Juxtaposition with a numeric literal is multiplication, not a call: a
        // `(` glued to a number (`2(x)`) is left for the juxtaposition check in
        // the operator loop to consume as a `(juxtapose 2 x)`, not a `(call 2 x)`.
        // (A `[` glued to a number stays an index, `2[1]` ⇒ `(ref 2 1)`, matching
        // JuliaSyntax's `parse_call_chain` guard.)
        if ctx.token(next).map(|t| t.kind) == Some(TokKind::LParen) && lhs_is_number(ctx, &lhs) {
            break;
        }
        match ctx.token(next).map(|t| t.kind) {
            Some(TokKind::LParen) => {
                lhs = parse_postfix(
                    ctx,
                    lhs,
                    next,
                    TokKind::RParen,
                    SyntaxKind::CALL_EXPR,
                    end_marker,
                    diagnostics,
                )
            }
            Some(TokKind::LBracket) => {
                lhs = parse_postfix(
                    ctx,
                    lhs,
                    next,
                    TokKind::RBracket,
                    SyntaxKind::INDEX_EXPR,
                    end_marker,
                    diagnostics,
                )
            }
            // Parametric type application: `Vector{T}`, `Dict{K, V}`.
            Some(TokKind::LBrace) => {
                lhs = parse_postfix(
                    ctx,
                    lhs,
                    next,
                    TokKind::RBrace,
                    SyntaxKind::CURLY_EXPR,
                    end_marker,
                    diagnostics,
                )
            }
            // Broadcast call `f.(args)`: a `.` whose next non-space token is `(`.
            // (A `.` before an identifier is field access, handled by the infix
            // loop; before `@` it is a qualified macro — neither matches here.)
            Some(TokKind::Dot)
                if ctx.token(ctx.skip_ws(next + 1)).map(|t| t.kind) == Some(TokKind::LParen) =>
            {
                let lparen = ctx.skip_ws(next + 1);
                let (list_events, end) = parse_arg_list(
                    ctx,
                    lparen,
                    TokKind::RParen,
                    SyntaxKind::ARG_LIST,
                    end_marker,
                    diagnostics,
                );
                // A broadcast call on a macro name (`@M.(x)`) is invalid — a macro
                // cannot be broadcast. The projector re-heads it as a macrocall
                // wrapping the dotcall with a zero-width `(error-t)`.
                let lhs_is_macrocall = matches!(
                    lhs.events.first(),
                    Some(Event::Start(SyntaxKind::MACRO_CALL))
                );
                let mut events = vec![Event::Start(SyntaxKind::DOT_CALL_EXPR)];
                events.extend(lhs.events);
                push_range(&mut events, lhs.end, next);
                events.push(Event::Tok(next)); // `.`
                push_range(&mut events, next + 1, lparen);
                // Whitespace before the `(` of a broadcast call is disallowed:
                // `f. (x)` → `(dotcall f (error-t) x)`, mirroring the glued
                // postfix-opener error above.
                if lparen > next + 1 {
                    let opener = &ctx.tokens()[lparen];
                    push_diagnostic(
                        diagnostics,
                        DiagnosticKind::OpenerWhitespace,
                        "whitespace before opener",
                        opener.start,
                        opener.start,
                    );
                }
                if lhs_is_macrocall {
                    let opener = &ctx.tokens()[lparen];
                    push_diagnostic(
                        diagnostics,
                        DiagnosticKind::MacroDotBroadcast,
                        "broadcast call on a macro name",
                        opener.start,
                        opener.start,
                    );
                }
                events.extend(list_events);
                events.push(Event::Finish);
                lhs = ExprParse {
                    start: lhs.start,
                    end,
                    events,
                };
            }
            // Julia removed the dotted transpose operator, but still recovers it
            // with postfix topology: `f.'` is a dotted postfix call whose
            // operator is error-wrapped. Horizontal whitespace and block comments
            // may separate the dot and prime.
            Some(TokKind::Dot)
                if ctx
                    .token(ctx.skip_ws_and_block_comments(next + 1))
                    .map(|t| t.kind)
                    == Some(TokKind::Transpose) =>
            {
                let transpose = ctx.skip_ws_and_block_comments(next + 1);
                let dot = &ctx.tokens()[next];
                if next > lhs.end {
                    push_diagnostic(
                        diagnostics,
                        DiagnosticKind::DotWhitespace,
                        "whitespace before `.`",
                        dot.end,
                        dot.end,
                    );
                }
                let prime = &ctx.tokens()[transpose];
                push_diagnostic(
                    diagnostics,
                    DiagnosticKind::InvalidPostfixOperator,
                    "invalid dotted postfix operator",
                    prime.start,
                    prime.end,
                );
                let mut events = vec![Event::Start(SyntaxKind::POSTFIX_EXPR)];
                events.extend(lhs.events);
                push_range(&mut events, lhs.end, next);
                events.push(Event::Tok(next));
                push_range(&mut events, next + 1, transpose);
                events.push(Event::Tok(transpose));
                events.push(Event::Finish);
                lhs = ExprParse {
                    start: lhs.start,
                    end: transpose + 1,
                    events,
                };
            }
            // Postfix transpose/adjoint `A'`. Wraps the operand and re-loops, so
            // it chains (`A''`) and composes with later suffixes (`A'[i]`). The
            // lexer only emits `Transpose` when it directly abuts a value, so the
            // operator is always adjacent (no newline between operand and `'`).
            Some(TokKind::Transpose) => {
                let mut events = vec![Event::Start(SyntaxKind::POSTFIX_EXPR)];
                events.extend(lhs.events);
                push_range(&mut events, lhs.end, next);
                events.push(Event::Tok(next));
                events.push(Event::Finish);
                lhs = ExprParse {
                    start: lhs.start,
                    end: next + 1,
                    events,
                };
            }
            _ => break,
        }
    }

    // A `do` block can follow a call on the same line: `f(x) do y … end`. It is
    // terminal in the postfix chain — to call its result you parenthesize.
    let next = ctx.skip_ws_and_block_comments(lhs.end);
    if ctx.token(next).map(|t| t.kind) == Some(TokKind::DoKw) {
        lhs = parse_do_block(ctx, lhs, next, diagnostics);
    }
    lhs
}

/// Parse a `(...)` call or `[...]` index suffix into `node` wrapping `lhs` and a
/// delimited `ARG_LIST`.
fn parse_postfix(
    ctx: &ParserCtx<'_>,
    lhs: ExprParse,
    open_idx: usize,
    close: TokKind,
    node: SyntaxKind,
    // Inherited index-marker context (see [`parse_postfix_chain`]). An indexing
    // `[…]` (handled by `parse_arg_list`) turns it on for its contents; calls,
    // curlies, and typed concatenations merely propagate it.
    end_marker: bool,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> ExprParse {
    // A single element followed by `for` is a generator argument:
    // `sum(x for x in xs)` (a call whose sole argument is a generator) or
    // `T[x for x in xs]` (a typed comprehension). The delimiters belong to the
    // outer node; the generator clauses reuse the comprehension machinery.
    let first_start = ctx.skip_trivia(open_idx + 1);
    if ctx.token(first_start).map(|t| t.kind) != Some(close) {
        // An indexing `[…]` (`a[x for …]`) is a marker context; a call/curly
        // generator inherits the enclosing one.
        let gen_end_marker = end_marker || close == TokKind::RBracket;
        let flags = ExprFlags {
            inside_brackets: true,
            end_marker: gen_end_marker,
            begin_marker: gen_end_marker,
            ..ExprFlags::default()
        };
        let diag_mark = diagnostics.len();
        if let Some(first) = parse_expr_in(ctx.tokens(), first_start, 0, diagnostics, flags)
            && ctx.token(ctx.skip_trivia(first.end)).map(|t| t.kind) == Some(TokKind::ForKw)
        {
            // A call generator can carry keyword parameters after a `;`
            // (`sum(x for x in xs; init = 0)`). Peek past the clauses: when they
            // are followed by a `;` at the call level, defer to the general
            // argument-list path, which builds the same delimiter-less
            // `GENERATOR` plus a `PARAMETERS` sibling as the multi-argument form
            // `f(a, x for x in xs; k)`. Bracketed comprehensions never take
            // parameters, so this only applies to calls and curlies.
            if matches!(node, SyntaxKind::CALL_EXPR | SyntaxKind::CURLY_EXPR) {
                let mut scratch = Vec::new();
                let mut scratch_diags = Vec::new();
                let clauses_end =
                    parse_generator_clauses(ctx, first.end, &mut scratch, &mut scratch_diags);
                if ctx.token(ctx.skip_trivia(clauses_end)).map(|t| t.kind)
                    == Some(TokKind::Semicolon)
                {
                    diagnostics.truncate(diag_mark);
                    // Fall through to `parse_arg_list` below.
                    return parse_postfix_arg_list(
                        ctx,
                        lhs,
                        open_idx,
                        close,
                        node,
                        end_marker,
                        diagnostics,
                    );
                }
            }
            let generator = parse_comprehension(
                ctx,
                open_idx,
                first,
                SyntaxKind::GENERATOR,
                close,
                diagnostics,
            );
            let node_kind = if node == SyntaxKind::CALL_EXPR {
                SyntaxKind::CALL_EXPR
            } else {
                SyntaxKind::TYPED_COMPREHENSION
            };
            let mut events = vec![Event::Start(node_kind)];
            events.extend(lhs.events);
            push_range(&mut events, lhs.end, open_idx);
            events.extend(generator.events);
            events.push(Event::Finish);
            return ExprParse {
                start: lhs.start,
                end: generator.end,
                events,
            };
        }
        diagnostics.truncate(diag_mark);
    }

    parse_postfix_arg_list(ctx, lhs, open_idx, close, node, end_marker, diagnostics)
}

/// Parse the delimited-list form of a postfix suffix (a call `f(a, b)`, index
/// `a[i]`, curly `S{T}`, or typed concatenation `T[x y]`) into `node` wrapping
/// `lhs` and an `ARG_LIST`. Split out from [`parse_postfix`] so the lone
/// generator-argument path can defer here when the generator is followed by
/// keyword parameters (`sum(x for x in xs; init = 0)`).
fn parse_postfix_arg_list(
    ctx: &ParserCtx<'_>,
    lhs: ExprParse,
    open_idx: usize,
    close: TokKind,
    node: SyntaxKind,
    end_marker: bool,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> ExprParse {
    // A space-, `;`-, or newline-separated bracket body after a value is a typed
    // concatenation (`T[x y]` → `(typed_hcat T x y)`), not an index. A comma
    // list, single element, or empty `T[]` stays an `INDEX_EXPR`.
    if close == TokKind::RBracket
        && let Some(typed) = parse_typed_concat(ctx, &lhs, open_idx, end_marker, diagnostics)
    {
        return typed;
    }

    let (list_events, end) = parse_arg_list(
        ctx,
        open_idx,
        close,
        SyntaxKind::ARG_LIST,
        end_marker,
        diagnostics,
    );
    let mut events = vec![Event::Start(node)];
    events.extend(lhs.events);
    push_range(&mut events, lhs.end, open_idx);
    // Whitespace before a glued postfix opener is disallowed: JuliaSyntax keeps
    // the call/index/curly shape but flags the space. We record an
    // `OpenerWhitespace` diagnostic at the opener's start, projected as a
    // `(error-t)` before the arguments (`f (a)` → `(call f (error-t) a)`,
    // `a [i]` → `(ref a (error-t) i)`, `S {a}` → `(curly S (error-t) a)`).
    if open_idx > lhs.end {
        let opener = &ctx.tokens()[open_idx];
        push_diagnostic(
            diagnostics,
            DiagnosticKind::OpenerWhitespace,
            "whitespace before opener",
            opener.start,
            opener.start,
        );
    }
    events.extend(list_events);
    events.push(Event::Finish);
    ExprParse {
        start: lhs.start,
        end,
        events,
    }
}

/// Detect and parse a typed concatenation `T[...]`: a bracket body after a value
/// that is space-, `;`-, or newline-separated (or an element-free `;`-only
/// `T[;]`). Returns `None` for a comma list, single element, empty `T[]`, or a
/// comprehension, leaving the caller to build an `INDEX_EXPR`. The result wraps
/// the type expression `lhs` and a `MATRIX_EXPR` body in a `TYPED_MATRIX_EXPR`.
fn parse_typed_concat(
    ctx: &ParserCtx<'_>,
    lhs: &ExprParse,
    open_idx: usize,
    // Inherited index-marker context. A typed concatenation (`T[x y]`) is an
    // array constructor, not indexing, so it never *enables* the marker — but it
    // propagates an enclosing one into its elements.
    end_marker: bool,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let diag_mark = diagnostics.len();
    let first_start = ctx.skip_trivia(open_idx + 1);
    // `T[]` is an empty index, not a concatenation.
    if ctx.token(first_start).map(|t| t.kind) == Some(TokKind::RBracket) {
        return None;
    }
    let wrap = |body: ExprParse| {
        let mut events = vec![Event::Start(SyntaxKind::TYPED_MATRIX_EXPR)];
        events.extend(lhs.events.iter().cloned());
        push_range(&mut events, lhs.end, open_idx);
        let end = body.end;
        events.extend(body.events);
        events.push(Event::Finish);
        ExprParse {
            start: lhs.start,
            end,
            events,
        }
    };
    // An element-free `T[; …]` is an empty n-dimensional concatenation.
    if let Some(empty) = parse_empty_ncat(ctx, open_idx, first_start, BRACKET_CAT) {
        return Some(wrap(empty));
    }
    // The first element is parsed in indexing position (a postfix `[`), so `end`
    // is a marker there (`a[2:end]` ⇒ `(ref a (call-i 2 : end))`). If this turns
    // out to be a typed concatenation (a space-separated array constructor), the
    // marker does *not* carry to the remaining elements — only an inherited
    // context does (`a[1 end]` errors, but `b[a[1 end]]` inherits).
    let first = parse_element(ctx.tokens(), first_start, true, diagnostics)?;
    // Look at the first separator: a `,`, `]`, end, or `for` means this is an
    // index/comprehension, not a concatenation.
    let look = ctx.skip_ws_and_comments(first.end);
    match ctx.token(look).map(|t| t.kind) {
        None | Some(TokKind::RBracket | TokKind::Comma | TokKind::ForKw) => {
            diagnostics.truncate(diag_mark);
            None
        }
        _ => {
            let mut body = parse_matrix(ctx, open_idx, first, BRACKET_CAT, end_marker, diagnostics);
            // A misplaced-`end` recovery (`a[1 end]`, `a[:(end)]`) keeps the typed
            // array head even with a single real element (`(typed_hcat a 1
            // (error-t))`), unlike a clean lone element. `parse_matrix` collapses a
            // single element to the comma kind, so rewrite the body node to the
            // matrix kind when the recovery fired.
            let recovered = diagnostics[diag_mark..]
                .iter()
                .any(|d| d.kind == DiagnosticKind::MatrixKeywordRecovery);
            if let (true, Some(Event::Start(k @ SyntaxKind::VECT_EXPR))) =
                (recovered, body.events.first_mut())
            {
                *k = SyntaxKind::MATRIX_EXPR;
            }
            // A lone element with only a trailing newline collapses to the
            // comma kind (`T[x\n]` → `(ref T x)`), so it stays an index.
            if matches!(
                body.events.first(),
                Some(Event::Start(SyntaxKind::VECT_EXPR))
            ) {
                diagnostics.truncate(diag_mark);
                None
            } else {
                Some(wrap(body))
            }
        }
    }
}

/// Parse a comma-separated, bracket-delimited argument list into a `list_kind`
/// node (`ARG_LIST` for calls/indices/curlies, `BRACES` for bare braces). Each
/// positional argument is wrapped in an `ARG`, each `name = value` in a
/// `KEYWORD_ARG`. A `;` opens a `PARAMETERS` node that holds the remaining
/// keyword parameters. Returns the events and the index just past the closing
/// bracket.
fn parse_arg_list(
    ctx: &ParserCtx<'_>,
    open_idx: usize,
    close: TokKind,
    list_kind: SyntaxKind,
    // Inherited index-marker context. *Indexing* — the sole `ARG_LIST` closed by
    // `]` (`a[end]`; vector literals build a `VECT_EXPR`, calls close with `)`) —
    // enables `end`/`begin` markers for its contents; every other list merely
    // propagates the enclosing context, so `[1, end]` errors at toplevel but the
    // inner vect of `a[[1, end]]` inherits the marker.
    inherited_end_marker: bool,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> (Vec<Event>, usize) {
    let tokens = ctx.tokens();
    let end_marker =
        inherited_end_marker || (close == TokKind::RBracket && list_kind == SyntaxKind::ARG_LIST);
    // `begin` and `end` share one index-marker context.
    let begin_marker = end_marker;
    let mut events = vec![Event::Start(list_kind), Event::Tok(open_idx)];
    let mut i = open_idx + 1;
    let mut in_params = false;
    // Element-slot tracking for empty-comma recovery. `slot_empty` is true at the
    // start of an element slot (after the opener or a separator) until an element
    // is parsed into it; `parsed_element` records whether any real element has
    // been seen yet.
    let mut slot_empty = true;
    let mut parsed_element = false;

    loop {
        // Interior trivia belongs to the current container (the list, or the
        // `PARAMETERS` section once a `;` has opened one).
        while matches!(tokens.get(i).map(|t| t.kind), Some(k) if k.is_trivia()) {
            events.push(Event::Tok(i));
            i += 1;
        }
        match tokens.get(i).map(|t| t.kind) {
            None => {
                // Unterminated list (EOF before the closing delimiter). Record an
                // `UnterminatedArgList` diagnostic at the opener, projected as a
                // trailing `(error-t)` (`f(a` → `(call f a (error-t))`, `[x` →
                // `(vect x (error-t))`).
                if in_params {
                    finish(&mut events, SyntaxKind::PARAMETERS);
                    in_params = false;
                }
                let opener = &tokens[open_idx];
                push_diagnostic(
                    diagnostics,
                    DiagnosticKind::UnterminatedArgList,
                    "unterminated argument list",
                    opener.start,
                    opener.start,
                );
                break;
            }
            Some(k) if k == close => {
                if in_params {
                    finish(&mut events, SyntaxKind::PARAMETERS);
                    in_params = false;
                }
                events.push(Event::Tok(i));
                i += 1;
                break;
            }
            Some(TokKind::Comma) => {
                // An empty element slot after an element—or at the front of a
                // call—is invalid: JuliaSyntax bails, bumping the offending comma
                // and everything after it up to the closer as one flat
                // trailing-junk run (`[x,,y]` ⇒ `(vect x (error-t ✘ y))`,
                // `f(,x)` ⇒ `(call f (error-t ✘ x))`). A trailing comma (`[x,]`,
                // slot empty but the closer follows) stays clean.
                if slot_empty && (parsed_element || close == TokKind::RParen) {
                    let mut j = i;
                    while let Some(k) = tokens.get(j).map(|t| t.kind) {
                        if k == close {
                            break;
                        }
                        j += 1;
                    }
                    if in_params {
                        finish(&mut events, SyntaxKind::PARAMETERS);
                        in_params = false;
                    }
                    events.push(Event::Start(SyntaxKind::ERROR));
                    push_range(&mut events, i, j);
                    events.push(Event::Finish);
                    push_diagnostic(
                        diagnostics,
                        DiagnosticKind::TrailingJunk,
                        "extra comma in list",
                        tokens[i].start,
                        tokens[i].end,
                    );
                    i = j;
                    if tokens.get(i).map(|t| t.kind) == Some(close) {
                        events.push(Event::Tok(i));
                        i += 1;
                    }
                    break;
                }
                // A bracket- or brace-delimited list may recover a leading empty
                // slot and continue (`[,x]` ⇒ `(vect (error) x)`). The missing
                // element is a zero-width diagnostic rather than a CST node. It
                // counts as an element so a second comma takes the bail-out path
                // above (`[,,]` ⇒ `(vect-, (error) (error-t ✘))`).
                if slot_empty {
                    push_diagnostic(
                        diagnostics,
                        DiagnosticKind::EmptyListSlot,
                        "missing element before comma",
                        tokens[i].start,
                        tokens[i].start,
                    );
                    parsed_element = true;
                }
                events.push(Event::Tok(i));
                i += 1;
                slot_empty = true;
            }
            // `;` splits positional arguments from keyword parameters, and each
            // subsequent `;` starts a fresh `PARAMETERS` group: `(a; b; c,d)` ⇒
            // `a (parameters b) (parameters c d)`. Close the open group before
            // opening the next so the groups stay siblings.
            Some(TokKind::Semicolon) => {
                if in_params {
                    finish(&mut events, SyntaxKind::PARAMETERS);
                }
                events.push(Event::Start(SyntaxKind::PARAMETERS));
                in_params = true;
                events.push(Event::Tok(i));
                i += 1;
                slot_empty = true;
            }
            // A bare `end` where it is not a valid index marker is a misplaced
            // block-closer keyword: JuliaSyntax implicitly closes the (now
            // unterminated) bracket with a synthesized `(error-t)` and bumps the
            // `end` and the real closer up as a trailing-junk run (`[1, 2, end]` ⇒
            // `(vect 1 2 (error-t)) (error-t end ✘)`, `f(end)` ⇒ `(call f
            // (error-t)) (error-t end ✘)`). This fires for a non-leading `end` in
            // any list and for a leading `end` in a call (`f(end)`); a *leading*
            // `end` in a vector/braces literal (`[end]` ⇒ `(vect (error end))`) is
            // a different `(error <kw>)` wrap, left divergent for now.
            Some(TokKind::EndKw) if !end_marker && (parsed_element || close == TokKind::RParen) => {
                if in_params {
                    finish(&mut events, SyntaxKind::PARAMETERS);
                    in_params = false;
                }
                let opener = &tokens[open_idx];
                push_diagnostic(
                    diagnostics,
                    DiagnosticKind::UnterminatedArgList,
                    "unterminated argument list",
                    opener.start,
                    opener.start,
                );
                break;
            }
            Some(_) => {
                i = parse_one_arg(ctx, &mut events, i, end_marker, begin_marker, diagnostics);
                slot_empty = false;
                parsed_element = true;
            }
        }
    }

    if in_params {
        finish(&mut events, SyntaxKind::PARAMETERS);
    }
    finish(&mut events, list_kind);
    (events, i)
}

/// Parse one argument starting at `i` into `events`, as a `KEYWORD_ARG`
/// (`name = value`) when it is a keyword argument and an `ARG` otherwise.
/// Returns the index just past the argument.
fn parse_one_arg(
    ctx: &ParserCtx<'_>,
    events: &mut Vec<Event>,
    i: usize,
    end_marker: bool,
    begin_marker: bool,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> usize {
    let tokens = ctx.tokens();
    let flags = ExprFlags {
        inside_brackets: true,
        end_marker,
        begin_marker,
        ..ExprFlags::default()
    };
    let parse_arg_expr = |tokens: &[Token], start, diagnostics: &mut Vec<ParseDiagnostic>| {
        parse_expr_in(tokens, start, 0, diagnostics, flags)
    };
    if let Some(eq_idx) = kwarg_eq(ctx, i) {
        // Build the `KEYWORD_ARG` into a local buffer so it can become a generator
        // body when a `for` follows (`f(a, k=v for v in xs)` →
        // `(call f a (generator (= k v) (= v xs)))`).
        let mut kw = vec![
            Event::Start(SyntaxKind::KEYWORD_ARG),
            Event::Start(SyntaxKind::NAME),
            Event::Tok(i),
            Event::Finish,
        ];
        // Whitespace + `=` between the name and the value.
        push_range(&mut kw, i + 1, eq_idx);
        kw.push(Event::Tok(eq_idx));
        let val_start = ctx.skip_trivia(eq_idx + 1);
        push_range(&mut kw, eq_idx + 1, val_start);
        let end = match parse_arg_expr(tokens, val_start, diagnostics) {
            Some(val) => {
                kw.extend(val.events);
                val.end
            }
            None => val_start,
        };
        finish(&mut kw, SyntaxKind::KEYWORD_ARG);
        if ctx.token(ctx.skip_trivia(end)).map(|t| t.kind) == Some(TokKind::ForKw) {
            events.push(Event::Start(SyntaxKind::GENERATOR));
            events.extend(kw);
            let gen_end = parse_generator_clauses(ctx, end, events, diagnostics);
            finish(events, SyntaxKind::GENERATOR);
            gen_end
        } else {
            events.extend(kw);
            end
        }
    } else if let Some(arg) = parse_arg_expr(tokens, i, diagnostics) {
        // A `for` following the element turns this argument into a generator:
        // `f(a, x for x in xs)` → `(call f a (generator x (= x xs)))`. The body is
        // the element just parsed; the enclosing `(`/`)` stay with the arg list, so
        // the `GENERATOR` is delimiter-less (the lone-generator `f(x for x in xs)`
        // is instead handled up in `parse_postfix`, before the list is entered).
        if ctx.token(ctx.skip_trivia(arg.end)).map(|t| t.kind) == Some(TokKind::ForKw) {
            events.push(Event::Start(SyntaxKind::GENERATOR));
            events.extend(arg.events);
            let end = parse_generator_clauses(ctx, arg.end, events, diagnostics);
            finish(events, SyntaxKind::GENERATOR);
            end
        } else {
            events.push(Event::Start(SyntaxKind::ARG));
            events.extend(arg.events);
            events.push(Event::Finish);
            arg.end
        }
    } else {
        events.push(Event::Tok(i));
        i + 1
    }
}

/// If the argument at `i` is a keyword argument (`name = value` — a bare
/// identifier followed on the same line by a single `=`, not `==`), return the
/// `=` token's index.
fn kwarg_eq(ctx: &ParserCtx<'_>, i: usize) -> Option<usize> {
    if ctx.token(i).map(|t| t.kind) != Some(TokKind::Ident) {
        return None;
    }
    let eq = ctx.skip_ws(i + 1);
    (ctx.token(eq).map(|t| t.kind) == Some(TokKind::Eq)).then_some(eq)
}

/// Find the next infix/assignment operator after `from`, honoring newline
/// sensitivity. Returns its token index and kind.
///
/// A `#= … =#` block comment between the operand and the operator is horizontal
/// whitespace, so it is skipped: `a #=c=# + b` is `(call-i a + b)`, and so is
/// `a #=\n…\n=# + b` — a block comment never terminates the expression, however
/// many lines it spans. Only a real newline is significant here.
/// If an `in`/`isa` word operator (lexed as an identifier, comparison
/// precedence) immediately follows the operand ending at `from`, return its
/// token index. Honors newline sensitivity exactly like [`next_operator`]: a
/// newline ends the expression at statement scope, but inside brackets the
/// operator may continue onto the next line.
/// Whether an operator token's text is a *dotted* (broadcast) operator — it leads
/// with a broadcast `.` (`.+`, `.&`, `.=`, `.&&`, `.+=`). The range/splat
/// operators `..`/`...` lead with a *doubled* dot and are not broadcasts, so they
/// are excluded; bare field-access `.` is excluded by the length check.
fn is_dotted_broadcast_text(text: &str) -> bool {
    text.as_bytes().first() == Some(&b'.') && text.len() > 1 && text.as_bytes()[1] != b'.'
}

/// Plain/broadcast assignment (`=`, `.=`) and augmented assignment (`+=`, `.+=`,
/// …): the loosest, right-associative tier, all modeled as `ASSIGNMENT_EXPR`.
/// Whether a unary operator's adjacent parens form an argument list — making
/// `+(...)` a call (`(call + …)`) rather than a parenthesized operand (a prefix
/// application `+(x)` → `(call-pre + x)`). Mirrors JuliaSyntax: the parens are a
/// call when empty (`+()`), opened by a leading `;` (a parameters section,
/// `+(; a)`), or when — at the top level — they contain a comma (`+(x, y)`) or a
/// splat `...` (`+(a...)`). A lone interior expression, or a non-leading `;`
/// block (`+(a; b)`), stays a prefix operand.
fn unary_op_paren_is_call(ctx: &ParserCtx<'_>, lparen_idx: usize) -> bool {
    let first = ctx.skip_trivia(lparen_idx + 1);
    match ctx.token(first).map(|t| t.kind) {
        Some(TokKind::RParen) => return true,
        // A leading `;` opens a parameters group (`+(; a=1)` → `(call + (parameters
        // (= a 1)))`), so it is a call — unless the parens form a *block*
        // (`+(;;)` → `(call-pre + (block-p))`), where the unary operator instead
        // prefixes the parenthesized block. Mirrors JuliaSyntax resolving the
        // empty all-semicolon group `(;;)` to a block rather than an arglist.
        Some(TokKind::Semicolon) => return !paren_is_block(ctx, lparen_idx),
        _ => {}
    }
    let mut depth = 0i32;
    let mut i = first;
    while let Some(tok) = ctx.token(i) {
        match tok.kind {
            TokKind::LParen | TokKind::LBracket | TokKind::LBrace => depth += 1,
            TokKind::RParen | TokKind::RBracket | TokKind::RBrace => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
            }
            TokKind::Comma | TokKind::DotDotDot if depth == 0 => return true,
            _ => {}
        }
        i += 1;
    }
    false
}

/// The node kind for a parenthesized run carrying a `;`: a `PAREN_BLOCK`
/// (projects `(block-p …)`) or a `TUPLE_EXPR` (projects `(tuple-p …)`). A pure
/// comma list (no semicolons) always stays a tuple.
fn paren_list_kind(ctx: &ParserCtx<'_>, lparen_idx: usize) -> SyntaxKind {
    if paren_is_block(ctx, lparen_idx) {
        SyntaxKind::PAREN_BLOCK
    } else {
        SyntaxKind::TUPLE_EXPR
    }
}

/// Whether a `;`-bearing parenthesized run is a block rather than a tuple,
/// mirroring JuliaSyntax `parse_paren`/`parse_brackets`:
///
/// ```text
/// is_tuple = had_commas || (had_splat && num_semis >= 1) ||
///            (initial_semi && (num_semis == 1 || num_subexprs > 0))
/// is_block = !is_tuple && num_semis > 0
/// ```
///
/// So `(a; b)`, `(a=1;)`, `(a;b;;c)`, and `(;;)` are blocks (`block-p`), while
/// `(a, b)`, `(; a=1)`, `(; a=1; b=2)`, and `(x...; y)` are tuples (`tuple-p`).
/// Flags are gathered by a depth-0 token scan from just after the `(`.
fn paren_is_block(ctx: &ParserCtx<'_>, lparen_idx: usize) -> bool {
    let first = ctx.skip_trivia(lparen_idx + 1);
    let initial_semi = ctx.token(first).map(|t| t.kind) == Some(TokKind::Semicolon);
    let mut depth = 0i32;
    let mut had_commas = false;
    let mut had_splat = false;
    let mut num_semis = 0u32;
    let mut num_subexprs = 0u32;
    let mut in_subexpr = false;
    let mut i = first;
    while let Some(tok) = ctx.token(i) {
        match tok.kind {
            TokKind::LParen | TokKind::LBracket | TokKind::LBrace => {
                if !in_subexpr {
                    num_subexprs += 1;
                    in_subexpr = true;
                }
                depth += 1;
            }
            TokKind::RParen | TokKind::RBracket | TokKind::RBrace => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            TokKind::Comma if depth == 0 => {
                had_commas = true;
                in_subexpr = false;
            }
            TokKind::Semicolon if depth == 0 => {
                num_semis += 1;
                in_subexpr = false;
            }
            TokKind::DotDotDot if depth == 0 => had_splat = true,
            k if !k.is_trivia() && !in_subexpr => {
                num_subexprs += 1;
                in_subexpr = true;
            }
            _ => {}
        }
        i += 1;
    }
    let is_tuple = had_commas
        || (had_splat && num_semis >= 1)
        || (initial_semi && (num_semis == 1 || num_subexprs > 0));
    !is_tuple && num_semis > 0
}

/// A binary operator that, glued to a `(`, names a function call: `*(x)`,
/// `==(a, b)`, `.*(a, b)`. These are the operators that are *not* unary in Julia
/// (so the parens form an argument list, never a prefix application) and not
/// syntactic (`&`, `:`, `::`, `&&`, `||`, `->` route elsewhere). The unary
/// operators (`+`, `-`, `!`, `~`) and type operators (`<:`, `>:`) are excluded;
/// they keep their prefix-application parse.
fn is_operator_call_name(kind: TokKind) -> bool {
    use TokKind::*;
    matches!(
        kind,
        Star | Slash
            | Backslash
            | SlashSlash
            | Caret
            | Percent
            // The wrapping `*%` is binary-only, so `*%(a, b)` is a plain call.
            // Its unary-capable siblings `+%`/`-%` route through the unary arm.
            | StarPercent
            | PlusPlus
            // The range `..` is not a Base operator but packages define it
            // (`..(x, y) = x == y`), and `..(a, b)` is an ordinary call on it,
            // not the prefix application `(call-pre (error ..) …)`.
            | DotDot
            | EqEq
            | NotEq
            | EqEqEq
            | NotEqEq
            | Lt
            | Le
            | Gt
            | Ge
            | Pipe
            | Shl
            | Shr
            | UShr
            | PipeGt
            | PipeLt
            | FatArrow
            | LongArrow
            | LeftRightArrow
            | LeftLongArrow
            | DotStar
            | DotSlash
            | DotBackslash
            | DotSlashSlash
            | DotCaret
            | DotPercent
            | DotEqEq
            | DotNotEq
            | DotEqEqEq
            | DotNotEqEq
            | DotLt
            | DotLe
            | DotGt
            | DotGe
            | DotShl
            | DotShr
            | DotUShr
            | DotSubtype
            | DotSupertype
            | DotFatArrow
            | DotLongArrow
            | DotLeftLongArrow
            | DotLeftRightArrow
            | DotPipeGt
            | DotAmp
            | DotPipe
            // The Unicode infix tiers: any member glued to `(` is a plain call
            // (`≠(a, b)` → `(call ≠ a b)`, `⊕(a)` → `(call ⊕ a)`), including
            // broadcast (`.≠(a, b)` → `(call (. ≠) a b)`) and suffixed
            // (`≠₁(a, b)`) forms, which share the tier's `TokKind`. The
            // unary-capable `± ∓ ⋆` never reach the call-name arm — the unary
            // prefix arm catches them first (`is_unary_prefix_op`).
            | UniArrow
            | UniComparison
            | UniColon
            | UniPlus
            | UniTimes
            | UniPower
    )
}

/// Whether `kind` is an operator that, glued to `{`, names a parametric callee
/// (`+{T}` → `(curly + T)`). This is the operator-call set (binary operators)
/// plus the unary arithmetic/logical and type operators. `::`, `&`, and `:` are
/// excluded: Julia keeps them as prefixes over the braces, and the syntactic
/// `&&`/`||`/`->` produce error-shape callees and stay unsupported.
fn is_curly_operator_name(kind: TokKind) -> bool {
    use TokKind::*;
    is_operator_call_name(kind)
        || matches!(
            kind,
            Plus | Minus
                | PlusPercent
                | MinusPercent
                | DotPlus
                | DotMinus
                | Bang
                | Tilde
                | DotTilde
                | Subtype
                | Supertype
        )
}

/// Whether `kind` is an operator that, alone inside parens in *value* position,
/// is the operator as a value (`(+)` → `+`, `(:)` → `:`, `(<:)` → `<:`). This is
/// the non-syntactic subset: `is_op_name` minus the syntactic `&&`/`||`/`->`
/// (which Julia reports as errors in value position) plus `:`. Broadcast forms
/// (`(.+)` → `(. +)`) and the erroring syntactic ops (`=`, `::`, assignment, `?`,
/// `...`) are deliberately excluded.
fn is_paren_value_op(kind: Option<TokKind>) -> bool {
    let Some(k) = kind else { return false };
    use TokKind::*;
    (is_op_name(k) && !matches!(k, AndAnd | OrOr | Arrow)) || k == Colon
}

/// A lone operator that may be quoted inside parens, `:(op)`. Accepts undotted
/// operator names, every assignment operator (plain, augmented, and their
/// broadcast forms), the syntactic `::`/`:`/`.`/`...`, and the broadcast
/// short-circuits `.&&`/`.||` — all of which are valid symbols in a quote
/// context even though most are errors in value position (`(.=)` ⇒
/// `(error (. =))` but `:(.=)` ⇒ `(quote-: (. =))`).
///
/// Operators that are already valid *values* (`.+`, `.≤`, `..`, `√`) are
/// deliberately excluded: `parse_paren` builds an `OPERATOR_ATOM` for them and
/// the quote projects that, which is the same shape. So is `?`, which JuliaSyntax
/// error-wraps even under a quote (`:(?)` ⇒ `(quote-: (error ?))`).
fn is_paren_quotable_op(kind: Option<TokKind>) -> bool {
    let Some(k) = kind else { return false };
    use TokKind::*;
    is_op_name(k)
        || is_assignment_op(k)
        || matches!(k, ColonColon | Colon | ColonEq | UniAssign)
        // `.` is the field-access dot (the `Expr(:., …)` head) and `...` the
        // splat; neither is a value, but both quote as themselves.
        || matches!(k, Dot | DotDotDot | DotAndAnd | DotOrOr)
}

/// Parse the `then : else` tail of a ternary whose `?` sits at `q_idx`, given the
/// already-parsed condition `cond`. Returns `Ok(node)` with the assembled
/// `TERNARY_EXPR` (the caller continues its operator loop), or `Err(recovered)`
/// when a branch or the `:` separator is missing (the caller returns it as-is).
fn parse_ternary(
    ctx: &ParserCtx<'_>,
    cond: ExprParse,
    q_idx: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
    flags: ExprFlags,
) -> Result<ExprParse, ExprParse> {
    let tokens = ctx.tokens();
    let inside_brackets = flags.inside_brackets;

    // JuliaSyntax requires whitespace on both sides of `?` and `:`; each missing
    // side records one `(error-t)` marker (as a diagnostic). For `?` the markers
    // sit between the condition and the true-branch (`a? b : c` ⇒
    // `(? a (error-t) b c)`); a glued `?` on both sides yields two.
    let ws_after = |idx: usize| ctx.token(idx + 1).is_some_and(|t| t.kind.is_trivia());
    let q_errors = usize::from(q_idx == cond.end) + usize::from(!ws_after(q_idx));

    // True-branch: `no_range` so a bare `:` ends it (the ternary separator). A
    // real range in the true-branch must therefore be parenthesized, as in Julia.
    let then_start = ctx.skip_trivia(q_idx + 1);
    let then_flags = ExprFlags {
        no_range: true,
        array_mode: false,
        stmt_comma: false,
        ..flags
    };
    let Some(then_br) = parse_expr_in(
        tokens,
        then_start,
        TERNARY_BRANCH_BP,
        diagnostics,
        then_flags,
    ) else {
        let op = &tokens[q_idx];
        push_diagnostic(
            diagnostics,
            DiagnosticKind::MissingTernaryTrue,
            "expected expression after `?`",
            op.start,
            op.end,
        );
        return Err(error_expr_to_line_end(tokens, cond.start, q_idx + 1));
    };

    // The `:` separator (newlines insignificant inside brackets, like operators).
    let colon = if inside_brackets {
        ctx.skip_ws_and_newlines(then_br.end)
    } else {
        ctx.skip_ws(then_br.end)
    };
    let has_colon = ctx.token(colon).map(|t| t.kind) == Some(TokKind::Colon);

    // A present `:` counts its surrounding whitespace; a missing `:` is itself one
    // marker (`a ? b c` ⇒ `(? a b (error-t) c)`), with the false-branch beginning
    // right after the true-branch.
    let (colon_errors, else_start) = if has_colon {
        let errors = usize::from(colon == then_br.end) + usize::from(!ws_after(colon));
        (errors, ctx.skip_trivia(colon + 1))
    } else {
        (1, ctx.skip_trivia(then_br.end))
    };

    // False-branch: inherit `no_range` so an enclosing ternary's `:` still ends
    // it (`a ? b ? c : d : e`), while a top-level else may hold a range.
    let else_flags = ExprFlags {
        array_mode: false,
        stmt_comma: false,
        ..flags
    };
    let Some(else_br) = parse_expr_in(
        tokens,
        else_start,
        TERNARY_BRANCH_BP,
        diagnostics,
        else_flags,
    ) else {
        // When the missing branch is terminated by a closing block keyword
        // (`x ? true end`, `x ? true : elseif …`), JuliaSyntax re-heads the
        // recovered node from `?` to `if`, splicing one zero-width `(error-t)`
        // per missing piece (no colon ⇒ two, colon present ⇒ one). The keyword
        // is left for the enclosing block (or the toplevel-junk driver). We
        // record `IncompleteTernaryIf` once per marker at the `?`'s end and let
        // the projector key the `if` head and marker count off the count.
        let terminator_is_closer = ctx
            .token(else_start)
            .is_some_and(|t| is_closing_block_keyword(t.kind));
        if terminator_is_closer {
            let markers = if has_colon { 1 } else { 2 };
            let q_end = tokens[q_idx].end;
            for _ in 0..markers {
                push_diagnostic(
                    diagnostics,
                    DiagnosticKind::IncompleteTernaryIf,
                    "incomplete ternary recovered as `if`",
                    q_end,
                    q_end,
                );
            }
            let mut events = vec![Event::Start(SyntaxKind::TERNARY_EXPR)];
            events.extend(cond.events);
            push_range(&mut events, cond.end, q_idx);
            events.push(Event::Tok(q_idx)); // `?`
            push_range(&mut events, q_idx + 1, then_br.start);
            events.extend(then_br.events);
            let end = if has_colon {
                push_range(&mut events, then_br.end, colon);
                events.push(Event::Tok(colon)); // `:`
                colon + 1
            } else {
                then_br.end
            };
            events.push(Event::Finish);
            return Ok(ExprParse {
                start: cond.start,
                end,
                events,
            });
        }
        if has_colon {
            let op = &tokens[colon];
            push_diagnostic(
                diagnostics,
                DiagnosticKind::MissingTernaryFalse,
                "expected expression after `:`",
                op.start,
                op.end,
            );
            return Err(error_expr_to_line_end(tokens, cond.start, colon + 1));
        }
        // No `:` and no false-branch: recover with the condition and true-branch.
        let op = &tokens[q_idx];
        push_diagnostic(
            diagnostics,
            DiagnosticKind::MissingTernaryColon,
            "expected `:` in ternary expression",
            op.start,
            op.end,
        );
        let mut events = vec![Event::Start(SyntaxKind::TERNARY_EXPR)];
        events.extend(cond.events);
        push_range(&mut events, cond.end, q_idx);
        events.push(Event::Tok(q_idx));
        push_range(&mut events, q_idx + 1, then_br.start);
        events.extend(then_br.events);
        events.push(Event::Finish);
        return Ok(ExprParse {
            start: cond.start,
            end: then_br.end,
            events,
        });
    };

    // Whitespace errors around `?`/`:` are recorded as diagnostics: `q_errors`
    // copies anchored at the `?`'s end, `colon_errors` at the true-branch's end.
    // The projector replays the counts as `(error-t)` markers.
    let q_end = tokens[q_idx].end;
    for _ in 0..q_errors {
        push_diagnostic(
            diagnostics,
            DiagnosticKind::TernaryQWhitespace,
            "whitespace around `?`",
            q_end,
            q_end,
        );
    }
    let then_end = tokens[then_br.end - 1].end;
    for _ in 0..colon_errors {
        push_diagnostic(
            diagnostics,
            DiagnosticKind::TernaryColonWhitespace,
            "whitespace around `:`",
            then_end,
            then_end,
        );
    }

    let mut events = vec![Event::Start(SyntaxKind::TERNARY_EXPR)];
    events.extend(cond.events);
    push_range(&mut events, cond.end, q_idx);
    events.push(Event::Tok(q_idx)); // `?`
    push_range(&mut events, q_idx + 1, then_br.start);
    events.extend(then_br.events);
    if has_colon {
        push_range(&mut events, then_br.end, colon);
        events.push(Event::Tok(colon)); // `:`
        push_range(&mut events, colon + 1, else_br.start);
    } else {
        push_range(&mut events, then_br.end, else_br.start);
    }
    events.extend(else_br.events);
    events.push(Event::Finish);
    Ok(ExprParse {
        start: cond.start,
        end: else_br.end,
        events,
    })
}
