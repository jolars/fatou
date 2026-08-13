//! Macro calls: `@m`, `@m(a, b)`, `@m a b`, `@Mod.mac x`, and the `var"..."` macro-name form.
//!
//! Split out of `expr.rs`; see that module's docs for the parser as a whole.

use super::*;

/// Parse a macro call introduced by a leading `@` (`@m`, `@m(a, b)`, `@m a b`,
/// `@.`, `@Mod.mac x`) into a `MACRO_CALL` wrapping a `MACRO_NAME` and the
/// arguments. The `@` sits at `at_idx`.
pub(super) fn parse_macro(
    ctx: &ParserCtx<'_>,
    at_idx: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
    inside_brackets: bool,
    array_mode: bool,
) -> ExprParse {
    let mut events = vec![Event::Start(SyntaxKind::MACRO_CALL)];
    events.push(Event::Start(SyntaxKind::MACRO_NAME));
    events.push(Event::Tok(at_idx)); // `@`
    let name_end = parse_macro_name_body(ctx, &mut events, at_idx + 1, diagnostics);
    finish(&mut events, SyntaxKind::MACRO_NAME);

    let end = parse_macro_args(
        ctx,
        &mut events,
        name_end,
        diagnostics,
        inside_brackets,
        array_mode,
    );
    finish(&mut events, SyntaxKind::MACRO_CALL);
    ExprParse {
        start: at_idx,
        end,
        events,
    }
}

/// Parse a qualified macro call `lhs.@mac args` (`Base.@time f()`). `lhs` is the
/// already-parsed module path; `dot_idx` is the `.` before the `@`. The
/// `MACRO_NAME` spans `lhs`, the `.`, the `@`, and the macro name body.
pub(super) fn parse_qualified_macro(
    ctx: &ParserCtx<'_>,
    lhs: ExprParse,
    dot_idx: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
    inside_brackets: bool,
    array_mode: bool,
) -> ExprParse {
    let mut events = vec![Event::Start(SyntaxKind::MACRO_CALL)];
    events.push(Event::Start(SyntaxKind::MACRO_NAME));
    events.extend(lhs.events);
    push_range(&mut events, lhs.end, dot_idx);
    events.push(Event::Tok(dot_idx)); // `.`
    let at_idx = ctx.skip_trivia(dot_idx + 1);
    push_range(&mut events, dot_idx + 1, at_idx);
    events.push(Event::Tok(at_idx)); // `@`
    // A misplaced macro sigil: the `@` names a non-final component with a glued
    // `.ident` continuation (`A.@B.x`). JuliaSyntax relocates the sigil to the
    // final component and splices a zero-width marker; record it for the
    // projector to replay.
    if ctx.token(at_idx + 1).map(|t| t.kind) == Some(TokKind::Ident)
        && ctx.token(at_idx + 2).map(|t| t.kind) == Some(TokKind::Dot)
        && ctx.token(at_idx + 3).map(|t| t.kind) == Some(TokKind::Ident)
    {
        let at_start = ctx.tokens()[at_idx].start;
        push_diagnostic(
            diagnostics,
            DiagnosticKind::MacroSigilTrailing,
            "misplaced macro sigil",
            at_start,
            at_start,
        );
    }
    let name_end = parse_macro_name_body(ctx, &mut events, at_idx + 1, diagnostics);
    finish(&mut events, SyntaxKind::MACRO_NAME);

    let end = parse_macro_args(
        ctx,
        &mut events,
        name_end,
        diagnostics,
        inside_brackets,
        array_mode,
    );
    finish(&mut events, SyntaxKind::MACRO_CALL);
    ExprParse {
        start: lhs.start,
        end,
        events,
    }
}

/// Whether `i` begins a `var"…"` single-quoted non-standard identifier. Julia
/// models these as `(var name)` names rather than `@var_str` string macros;
/// triple-quoted `var"""…"""` is an ordinary string macro and is excluded.
pub(crate) fn is_var_identifier_start(ctx: &ParserCtx<'_>, i: usize) -> bool {
    ctx.token(i)
        .is_some_and(|t| t.kind == TokKind::StringPrefix && t.text == "var")
        && matches!(
            ctx.token(i + 1),
            Some(t) if t.kind == TokKind::StringDelimOpen && t.text.len() == 1
        )
}

/// If `i` begins a `var"…"` single-quoted non-standard identifier — the macro
/// name in `@var"#"` (`(var @#)`) — append its `NONSTANDARD_IDENTIFIER` node to
/// `events` and return the index past it. Otherwise return `None`. Triple-quoted
/// `var"""…"""` is an ordinary `@var_str` macro, not a name, so it is excluded.
pub(crate) fn push_var_macro_name(
    ctx: &ParserCtx<'_>,
    events: &mut Vec<Event>,
    i: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<usize> {
    if is_var_identifier_start(ctx, i) {
        let lit = parse_string_literal(ctx, i, diagnostics);
        let end = lit.end;
        events.extend(lit.events);
        Some(end)
    } else {
        None
    }
}

/// Emit the tokens of a macro name following the `@` sigil, starting at `start`:
/// either a lone `.` (the broadcast macro `@.`), an identifier followed by a
/// trailing adjacent `.ident` chain (`@Mod.mac`), or a `var"…"` non-standard
/// identifier (`@var"#"`). Returns the index just past the name.
fn parse_macro_name_body(
    ctx: &ParserCtx<'_>,
    events: &mut Vec<Event>,
    start: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> usize {
    if let Some(end) = push_var_macro_name(ctx, events, start, diagnostics) {
        return end;
    }
    match ctx.token(start).map(|t| t.kind) {
        // The broadcast macro `@.` — the name is the single `.` token.
        Some(TokKind::Dot) => {
            events.push(Event::Tok(start));
            start + 1
        }
        Some(TokKind::Ident) => {
            events.push(Event::Tok(start));
            let mut i = start + 1;
            // Adjacent dotted-path continuation, no whitespace skipping (a space
            // before a `.` makes it a broadcast argument, not the name). Each step
            // is `.ident` (`@Mod.mac`), `.$ident` (an interpolated component, valid
            // when non-final), or `.@ident` (a misplaced inner sigil). The final
            // component carries the macro sigil; a `$` final or any inner/extra `@`
            // makes the path invalid, recovered by JuliaSyntax with zero-width
            // markers — recorded below and replayed by the projector.
            let mut saw_dollar_final = false;
            let mut saw_inner_at = false;
            while ctx.token(i).map(|t| t.kind) == Some(TokKind::Dot) {
                match ctx.token(i + 1).map(|t| t.kind) {
                    Some(TokKind::Ident) => {
                        events.push(Event::Tok(i)); // `.`
                        events.push(Event::Tok(i + 1)); // ident
                        i += 2;
                        saw_dollar_final = false;
                    }
                    Some(TokKind::Dollar)
                        if ctx.token(i + 2).map(|t| t.kind) == Some(TokKind::Ident) =>
                    {
                        events.push(Event::Tok(i)); // `.`
                        events.push(Event::Tok(i + 1)); // `$`
                        events.push(Event::Tok(i + 2)); // ident
                        i += 3;
                        saw_dollar_final = true;
                    }
                    Some(TokKind::At)
                        if ctx.token(i + 2).map(|t| t.kind) == Some(TokKind::Ident) =>
                    {
                        events.push(Event::Tok(i)); // `.`
                        events.push(Event::Tok(i + 1)); // `@`
                        events.push(Event::Tok(i + 2)); // ident
                        i += 3;
                        saw_dollar_final = false;
                        saw_inner_at = true;
                    }
                    _ => break,
                }
            }
            if saw_dollar_final || saw_inner_at {
                let at_start = ctx.tokens()[start - 1].start;
                push_diagnostic(
                    diagnostics,
                    DiagnosticKind::MacroSigilLeading,
                    "invalid qualified macro name",
                    at_start,
                    at_start,
                );
            }
            i
        }
        // A parenthesized macro name `@(A)`: a single identifier wrapped in
        // parens unwraps to the bare name `@A` (interior whitespace is allowed:
        // `@( A )`). The parens are kept in the CST for losslessness; the
        // projector reads only the identifier component. Anything other than a
        // lone identifier (`@(A.b)`, `@(f(x))`) is left for the paren-arg form to
        // handle (it stays a divergence, matching Julia's error recovery).
        Some(TokKind::LParen) => {
            let inner = ctx.skip_ws(start + 1);
            if ctx.token(inner).map(|t| t.kind) == Some(TokKind::Ident) {
                let after = ctx.skip_ws(inner + 1);
                if ctx.token(after).map(|t| t.kind) == Some(TokKind::RParen) {
                    push_range(events, start, after + 1);
                    return after + 1;
                }
            }
            start
        }
        // A `[`/`{` directly after `@` is an invalid macro name: JuliaSyntax
        // parses the bracketed expression and error-wraps it as the name
        // (`@[x] y z` ⇒ `(macrocall (error (vect x)) y z)`, `@{x} y` ⇒
        // `(macrocall (error (braces x)) y)`). Parse it as the name body, record
        // `InvalidMacroName`, and let the space-form arguments follow. (A name
        // identifier before the bracket — `@m[a]` — never reaches here; the
        // `Ident` arm consumes the name and `[a]` becomes the argument.)
        Some(TokKind::LBracket | TokKind::LBrace) => {
            if let Some(expr) = parse_prefix(ctx, start, diagnostics, ExprFlags::default()) {
                let name_start = ctx.tokens()[start].start;
                let end = expr.end;
                events.extend(expr.events);
                push_diagnostic(
                    diagnostics,
                    DiagnosticKind::InvalidMacroName,
                    "invalid macro name",
                    name_start,
                    name_start,
                );
                end
            } else {
                start
            }
        }
        // An operator, `$`, or keyword directly after `@` names the macro
        // (`@+`, `@!`, `@..`, `@$`, `@end`). A lone `:` (`@:`) is left to error.
        Some(k) if is_macro_name_token(k) => {
            events.push(Event::Tok(start));
            start + 1
        }
        // A bare `@` with no name — emit nothing more; the MACRO_NAME holds just
        // the sigil (still lossless).
        _ => start,
    }
}

/// Whether `kind`, directly after `@`, names the macro: any operator name, the
/// `$` sigil, or a keyword (`@+`, `@!`, `@..`, `@$`, `@end`). `.` is excluded —
/// it is the broadcast macro `@.`, handled before this — and `:` is excluded so
/// `@:` falls through to error recovery (Julia rejects it).
fn is_macro_name_token(kind: TokKind) -> bool {
    !matches!(kind, TokKind::Dot | TokKind::Colon)
        && (is_op_name(kind)
            || is_value_operator(kind)
            || kind == TokKind::Dollar
            || kind.is_keyword())
}

/// Parse the arguments of a macro call after its name (which ends at `name_end`)
/// into `events`, returning the index just past the last argument. Two forms: a
/// `(` adjacent to the name opens a comma-separated `ARG_LIST` (call-like);
/// otherwise the arguments are space-separated expressions consumed to the end
/// of the line (or until a closing delimiter / separator inside brackets).
fn parse_macro_args(
    ctx: &ParserCtx<'_>,
    events: &mut Vec<Event>,
    name_end: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
    inside_brackets: bool,
    // Space-sensitive element position (a comprehension/array body or a call-arg
    // generator body), where a trailing `for` opens a generator and so ends the
    // macro's space-args rather than being consumed as a for-loop argument.
    array_mode: bool,
) -> usize {
    // Paren form `@m(a, b)`: the `(` must be adjacent (no whitespace), otherwise
    // `@m (a, b)` is the space form with a single parenthesized argument.
    if ctx.token(name_end).map(|t| t.kind) == Some(TokKind::LParen) {
        // Marker propagation into macro-call arguments (`a[@m(end)]`) is deferred.
        let (list_events, end) = parse_arg_list(
            ctx,
            name_end,
            TokKind::RParen,
            SyntaxKind::ARG_LIST,
            false,
            diagnostics,
        );
        events.extend(list_events);
        return end;
    }

    // Bracket form `@m[a]`/`@m{a}`: a `[`/`{` adjacent to the macro name (no
    // whitespace, else `name_end` points at the whitespace token) is the single
    // argument. Postfix operators chain onto the whole macrocall, not the bracket
    // (`@m[a].b` ⇒ `(. (macrocall @m (vect a)) (quote b))`, `@m[a](x)` ⇒
    // `(call (macrocall @m (vect a)) x)`), so parse only the bracket prefix here
    // and let the outer postfix chain attach any suffix.
    if matches!(
        ctx.token(name_end).map(|t| t.kind),
        Some(TokKind::LBracket | TokKind::LBrace)
    ) {
        let arg_flags = ExprFlags {
            inside_brackets,
            ..ExprFlags::default()
        };
        if let Some(arg) = parse_prefix(ctx, name_end, diagnostics, arg_flags) {
            events.extend(arg.events);
            return arg.end;
        }
    }

    // Space form `@m a b`: each argument is a full expression parsed
    // space-sensitively (`array_mode`), so a whitespace-preceded `(`/`[`/`{` or a
    // space-glued prefix operator begins the next argument (`@m f (x)` is two
    // arguments, `@m a +b` likewise). Stop at a newline, a line comment (which
    // runs to end of line, so no argument can follow it), end of input, or a
    // delimiter that closes/separates an enclosing list. The terminating trivia
    // is left for the caller to attach, so it must not be consumed here. A
    // `#= … =#` block comment is horizontal whitespace between arguments, not a
    // terminator like the line comment it sits beside here (`@test #=T=# f(x)`,
    // `@newinterp Interp #=ephemeral_cache=#true`).
    let mut pos = name_end;
    let mut n_args = 0;
    loop {
        let next = ctx.skip_ws_and_block_comments(pos);
        match ctx.token(next).map(|t| t.kind) {
            None
            | Some(
                TokKind::Newline
                | TokKind::Comment
                | TokKind::Comma
                | TokKind::RParen
                | TokKind::RBracket
                | TokKind::RBrace
                | TokKind::Semicolon,
            ) => break,
            // In a generator-bearing position, a `for` after the arguments opens a
            // generator (`[@inbounds f(x) for x in xs]`, `g(@m a for a in as)`), so
            // it ends the macro's space-args. At statement scope `for` is instead a
            // for-loop argument (`@time for i in xs … end`), so this only fires in a
            // bracket or an array/comprehension-element context.
            Some(TokKind::ForKw) if inside_brackets || array_mode => break,
            _ => {
                // A bare comma after a space-form argument folds it into a
                // bare-tuple argument rather than separating arguments
                // (`@show a, b` ⇒ `(macrocall @show (tuple a b))`, `@show a, b =
                // c` ⇒ `(macrocall @show (= (tuple a b) c))`). The macro grabs
                // the comma greedily even inside an enclosing list, so it wins
                // over the container's separator (`[@m a, b]` ⇒ `(vect (macrocall
                // @m (tuple a b)))`, `f(@m a, b)` ⇒ `(call f (macrocall @m (tuple
                // a b)))`). `stmt_comma` here drives that; the container never
                // sees the comma because the argument consumes it.
                let arg_flags = ExprFlags {
                    inside_brackets,
                    array_mode: true,
                    stmt_comma: true,
                    ..ExprFlags::default()
                };
                match parse_expr_in(ctx.tokens(), next, 0, diagnostics, arg_flags) {
                    Some(arg) => {
                        // Commit the inter-argument trivia only once an argument
                        // actually parses. When nothing does (a block-closing
                        // `end`/`else`/`catch`/… where an argument was expected),
                        // the whitespace is terminating trivia the caller attaches;
                        // pushing it here would duplicate it (once in the macrocall,
                        // once in the enclosing block) and break losslessness.
                        push_range(events, pos, next);
                        events.extend(arg.events);
                        pos = arg.end;
                        n_args += 1;
                    }
                    None => break,
                }
            }
        }
    }

    // `@doc` extension: when the doc macro takes exactly one space-separated
    // argument and the next line carries another expression, it is consumed as a
    // second argument (`@doc x\ny` ⇒ `(macrocall @doc x y)`). A blank line, a
    // closing token, or end of input on the next line stops it. Matches
    // JuliaSyntax's doc-macro rule; the name's leaf identifier must be `doc`
    // (`@doc`, `A.@doc`, `@A.doc`).
    if n_args == 1 && macro_leaf_is_doc(ctx, name_end) {
        let nl = ctx.skip_ws(pos);
        if ctx.token(nl).map(|t| t.kind) == Some(TokKind::Newline) {
            let after = ctx.skip_ws(nl + 1);
            let extend = !matches!(
                ctx.token(after).map(|t| t.kind),
                None | Some(
                    TokKind::Newline
                        | TokKind::Comma
                        | TokKind::Semicolon
                        | TokKind::RParen
                        | TokKind::RBracket
                        | TokKind::RBrace
                        | TokKind::EndKw
                        | TokKind::ElseKw
                        | TokKind::ElseifKw
                        | TokKind::CatchKw
                        | TokKind::FinallyKw
                )
            );
            if extend {
                push_range(events, pos, after);
                let arg_flags = ExprFlags {
                    inside_brackets,
                    array_mode: true,
                    stmt_comma: true,
                    ..ExprFlags::default()
                };
                if let Some(arg) = parse_expr_in(ctx.tokens(), after, 0, diagnostics, arg_flags) {
                    events.extend(arg.events);
                    pos = arg.end;
                }
            }
        }
    }
    pos
}

/// Whether the macro name ending at `name_end` has the leaf identifier `doc` —
/// the special doc macro (`@doc`, `A.@doc`, `@A.doc`), whose single-argument
/// form is extended with the next line's expression in [`parse_macro_args`].
fn macro_leaf_is_doc(ctx: &ParserCtx<'_>, name_end: usize) -> bool {
    name_end > 0
        && ctx
            .token(name_end - 1)
            .is_some_and(|t| t.kind == TokKind::Ident && t.text == "doc")
}
