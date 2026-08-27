//! Formatting-invariant AST shape — the basis of `ast(x) == ast(format(x))`.
//!
//! The formatter's other invariants are all *local*: idempotence says a second
//! pass changes nothing, and a clean reparse says the output is still Julia.
//! Neither says the output is still the **same program**. [`ast_shape`] closes
//! that gap: two texts with the same shape denote the same program, so a
//! fixture whose shape survives formatting is proof the formatter moved only
//! layout.
//!
//! # What the shape is
//!
//! The JuliaSyntax s-expression projection
//! ([`fatou_parser::parser::to_juliasyntax_sexpr`]), whitespace-normalized,
//! with the surface flags the formatter is licensed to change stripped. The
//! projector is reused rather than reimplemented: it is the one projection in
//! the workspace validated differentially against real Julia, and the typed AST
//! is a read-only *view* over the trivia-bearing CST, not a comparable value.
//!
//! # Licensed changes
//!
//! The projection mirrors JuliaSyntax's `SyntaxNode`, which records surface
//! facts below the level of meaning. Exactly one is erased here:
//!
//! - **The trailing-comma flag.** JuliaSyntax marks a bracket that closes after
//!   a trailing comma by suffixing the head (`(call-, f a)`); the formatter adds
//!   that comma whenever it explodes an argument list across lines. Julia's own
//!   `Expr` form does not record it.
//!
//! Nothing else is normalized, and that is deliberate. `where T` → `where {T}`,
//! splitting `a; b` onto two lines, and literal canonicalization (`.5` → `0.5`)
//! are all recorded formatter policies that *do* move the projection, but each
//! erasure needed to hide them would also hide a real defect — unwrapping
//! `where` braces, for instance, would conceal `x where {T S}` formatting to
//! `x where {{T S}}`. They are recorded as known-drift entries in the tests
//! instead, where they stay visible and attributable.

use fatou_parser::parser::{parse, sexpr_tokens, to_juliasyntax_sexpr};

/// The projector's sentinel head for a `SyntaxKind` it cannot render.
const UNSUPPORTED: &str = "unsupported";

/// The formatting-invariant shape of `text`, or `None` when no comparable shape
/// exists.
///
/// `None` means the input is outside the invariant's domain, never that the
/// input is malformed in an interesting way:
///
/// - the text does not parse cleanly — formatting an error tree has no defined
///   equivalence, and the parser's own recovery shapes are not a formatter
///   concern;
/// - the projection still contains an `(unsupported …)` sentinel — a
///   `SyntaxKind` the projector cannot render. Two sentinels compare equal, so
///   comparing them would be a false pass rather than a check.
///
/// Callers deciding between "skipped" and "broken" should parse the formatted
/// output themselves: a *formatted* text that fails to parse is a formatter
/// defect, not an out-of-domain input.
///
/// # Blind spot
///
/// The projection carries no trivia, so a formatter that drops a comment or a
/// docstring still has a preserved shape. Comment preservation needs its own
/// check; this one cannot see it.
pub fn ast_shape(text: &str) -> Option<String> {
    let output = parse(text);
    if !output.diagnostics.is_empty() {
        return None;
    }
    let raw = to_juliasyntax_sexpr(&output.cst, &output.diagnostics);
    let mut tokens = sexpr_tokens(&raw);
    if tokens
        .iter()
        .enumerate()
        .any(|(i, t)| is_head(&tokens, i) && t == UNSUPPORTED)
    {
        return None;
    }
    drop_surface_flags(&mut tokens);
    Some(tokens.join(" "))
}

/// Whether `tokens[i]` is a head — the token directly after a `(`.
fn is_head(tokens: &[String], i: usize) -> bool {
    i > 0 && tokens[i - 1] == "("
}

/// Strip the licensed surface flags from a projection's tokens.
///
/// Operates on head tokens only, so an atom that happens to end in the flag's
/// spelling is never touched. Both this and the sentinel check above work from
/// [`sexpr_tokens`] rather than from the joined string: a string literal is one
/// token that may itself contain spaces and parentheses, so re-splitting the
/// join would cut one apart and read its contents as syntax.
fn drop_surface_flags(tokens: &mut [String]) {
    for i in 0..tokens.len() {
        if is_head(tokens, i)
            && let Some(stripped) = tokens[i].strip_suffix("-,")
        {
            tokens[i] = stripped.to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(normalized: &str) -> String {
        let mut tokens = sexpr_tokens(normalized);
        drop_surface_flags(&mut tokens);
        tokens.join(" ")
    }

    #[test]
    fn layout_does_not_move_the_shape() {
        assert_eq!(ast_shape("f(a,b)"), ast_shape("f(\n    a,\n    b,\n)"));
        assert_eq!(ast_shape("x = (1 + 2) * 3"), ast_shape("x =\n  (1+2)*3"));
    }

    #[test]
    fn precedence_moves_the_shape() {
        assert_ne!(ast_shape("x = (1 + 2) * 3"), ast_shape("x = 1 + 2 * 3"));
    }

    /// The one licensed erasure, and its narrowness: only a head is rewritten.
    #[test]
    fn trailing_comma_flag_is_erased() {
        assert_eq!(ast_shape("f(a,)"), ast_shape("f(a)"));
        assert!(!ast_shape("f(a,)").unwrap().contains("call-,"));
        assert_eq!(strip("( call-, f a-, )"), "( call f a-, )");
    }

    /// A string literal's contents are operand text, not syntax: neither the
    /// erasure nor the sentinel check may read structure out of them.
    #[test]
    fn string_contents_are_opaque() {
        let shape = ast_shape(r#"x = "a ( b-, c""#).expect("string literal has a shape");
        assert!(shape.contains(r#""a ( b-, c""#), "{shape}");
        assert!(ast_shape(r#"y = "(unsupported FOO)""#).is_some());
    }

    /// A `;` inside brackets is meaning, not layout: `[x;]` is `vcat`.
    #[test]
    fn bracket_semicolon_moves_the_shape() {
        assert_ne!(ast_shape("[x;]"), ast_shape("[x]"));
    }

    #[test]
    fn out_of_domain_inputs_have_no_shape() {
        assert_eq!(ast_shape("function f("), None);
    }

    /// Trivia is invisible to the shape — the documented blind spot.
    #[test]
    fn comments_do_not_affect_the_shape() {
        assert_eq!(ast_shape("f(x)"), ast_shape("# c\nf(x) # d"));
    }
}
