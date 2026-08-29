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
//! erasure needed to hide them could also conceal a real defect. They are
//! recorded as known-drift entries in the tests instead, where they stay visible
//! and attributable.

use fatou_parser::parser::{
    ParseDiagnostic, ParseOutput, parse, sexpr_tokens, to_juliasyntax_sexpr,
};
use fatou_parser::syntax::SyntaxKind;

/// The projector's sentinel head for a `SyntaxKind` it cannot render.
const UNSUPPORTED: &str = "unsupported";

/// Why a prospective formatter result could not be proved equivalent to its
/// input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationError {
    /// The input is not valid Julia, so there is no program to compare.
    InputSyntax { diagnostics: Vec<ParseDiagnostic> },
    /// The parser's JuliaSyntax projection does not cover the input's shape.
    UnsupportedInput,
    /// Formatting produced text that no longer parses cleanly.
    OutputSyntax { diagnostics: Vec<ParseDiagnostic> },
    /// Formatting produced a shape the JuliaSyntax projection does not cover.
    UnsupportedOutput,
    /// The comparable program shapes differ.
    ChangedProgram,
    /// A comment was dropped, reordered, or changed.
    ChangedComments,
}

impl std::fmt::Display for VerificationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InputSyntax { diagnostics } => {
                write_diagnostic(f, "input does not parse cleanly", diagnostics)
            }
            Self::UnsupportedInput => {
                f.write_str("input contains syntax the verifier cannot project")
            }
            Self::OutputSyntax { diagnostics } => {
                write_diagnostic(f, "formatted output does not parse cleanly", diagnostics)
            }
            Self::UnsupportedOutput => {
                f.write_str("formatted output contains syntax the verifier cannot project")
            }
            Self::ChangedProgram => f.write_str("formatted output changed the parsed program"),
            Self::ChangedComments => f.write_str("formatted output changed comments"),
        }
    }
}

impl std::error::Error for VerificationError {}

fn write_diagnostic(
    f: &mut std::fmt::Formatter<'_>,
    prefix: &str,
    diagnostics: &[ParseDiagnostic],
) -> std::fmt::Result {
    let Some(first) = diagnostics.first() else {
        return f.write_str(prefix);
    };
    write!(
        f,
        "{prefix}: [{}..{}]: {}",
        first.start, first.end, first.message
    )?;
    if diagnostics.len() > 1 {
        write!(f, " (and {} more)", diagnostics.len() - 1)?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Shape {
    Atom(String),
    List(Vec<Shape>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Comment {
    kind: SyntaxKind,
    text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OpaqueLeaf {
    kind: SyntaxKind,
    text: String,
}

pub(crate) struct VerificationBaseline {
    strict_shape: String,
    comparable_shape: Option<Shape>,
    opaque_leaves: Vec<OpaqueLeaf>,
    comments: Vec<Comment>,
}

/// Verify that `formatted` denotes the same program as `input` and preserves
/// its comments.
///
/// Unlike [`ast_shape`], this comparison admits the formatter's narrowly
/// defined, meaning-preserving canonicalizations. It fails closed when either
/// text cannot be compared.
pub fn verify_format(input: &str, formatted: &str) -> Result<(), VerificationError> {
    let parsed = parse(input);
    let baseline = verification_baseline(&parsed)?;
    verify_against(&baseline, formatted)
}

pub(crate) fn verification_baseline(
    parsed: &ParseOutput,
) -> Result<VerificationBaseline, VerificationError> {
    if !parsed.diagnostics.is_empty() {
        return Err(VerificationError::InputSyntax {
            diagnostics: parsed.diagnostics.clone(),
        });
    }
    let (strict_shape, comparable_shape) =
        projected_shapes(parsed).ok_or(VerificationError::UnsupportedInput)?;
    Ok(VerificationBaseline {
        strict_shape,
        comparable_shape,
        opaque_leaves: opaque_leaves(&parsed.cst),
        comments: comments(&parsed.cst),
    })
}

pub(crate) fn verify_against(
    baseline: &VerificationBaseline,
    formatted: &str,
) -> Result<(), VerificationError> {
    let output = parse(formatted);
    if !output.diagnostics.is_empty() {
        return Err(VerificationError::OutputSyntax {
            diagnostics: output.diagnostics,
        });
    }
    let (strict_shape, comparable_shape) =
        projected_shapes(&output).ok_or(VerificationError::UnsupportedOutput)?;
    if baseline.opaque_leaves != opaque_leaves(&output.cst) {
        return Err(VerificationError::ChangedProgram);
    }
    let same_program = baseline.strict_shape == strict_shape
        || baseline
            .comparable_shape
            .as_ref()
            .zip(comparable_shape.as_ref())
            .is_some_and(|(before, after)| before == after);
    if !same_program {
        return Err(VerificationError::ChangedProgram);
    }
    if baseline.comments != comments(&output.cst) {
        return Err(VerificationError::ChangedComments);
    }
    Ok(())
}

fn projected_shapes(output: &ParseOutput) -> Option<(String, Option<Shape>)> {
    let raw = to_juliasyntax_sexpr(&output.cst, &output.diagnostics);
    shapes_from_projection(&raw)
}

fn shapes_from_projection(raw: &str) -> Option<(String, Option<Shape>)> {
    let mut tokens = projection_tokens(raw);
    if tokens
        .iter()
        .enumerate()
        .any(|(i, t)| is_head(&tokens, i) && t == UNSUPPORTED)
    {
        return None;
    }
    drop_surface_flags(&mut tokens);
    let strict = tokens.join(" ");
    let mut cursor = 0;
    let comparable = match parse_shape(&tokens, &mut cursor).filter(|_| cursor == tokens.len()) {
        Some(shape) => Some(normalize_shape(shape, true).ok()?),
        None => None,
    };
    Some((strict, comparable))
}

/// Tokenize the projector's output for the semantic comparator.
///
/// This differs narrowly from [`sexpr_tokens`]: JuliaSyntax displays a
/// `var"…"` name containing a quote as an unquoted atom (`(var ")`). A quote at
/// an atom boundary is therefore a string only when a matching unescaped close
/// exists; otherwise it belongs to the ordinary atom. The strict test projector
/// keeps using [`sexpr_tokens`], whose representation is already pinned.
fn projection_tokens(projection: &str) -> Vec<String> {
    let bytes = projection.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b' ' | b'\t' | b'\n' | b'\r' => i += 1,
            b'(' | b')' => {
                tokens.push((bytes[i] as char).to_string());
                i += 1;
            }
            b'"' => {
                let start = i;
                if matches!(tokens.last(), Some(previous) if previous == "var") {
                    i = atom_end(bytes, start);
                    tokens.push(projection[start..i].to_string());
                    continue;
                }
                let mut end = i + 1;
                let mut closed = false;
                while end < bytes.len() {
                    match bytes[end] {
                        b'\\' if end + 1 < bytes.len() => end += 2,
                        b'"' => {
                            end += 1;
                            closed = true;
                            break;
                        }
                        _ => end += 1,
                    }
                }
                if closed {
                    tokens.push(projection[start..end].to_string());
                    i = end;
                } else {
                    i = atom_end(bytes, start);
                    tokens.push(projection[start..i].to_string());
                }
            }
            b'\'' => {
                let start = i;
                if !matches!(tokens.last(), Some(previous) if previous == "char") {
                    i = atom_end(bytes, start);
                    tokens.push(projection[start..i].to_string());
                    continue;
                }
                let mut end = i + 1;
                let mut closed = false;
                while end < bytes.len() {
                    match bytes[end] {
                        b'\\' if end + 1 < bytes.len() => end += 2,
                        b'\'' => {
                            end += 1;
                            closed = true;
                            break;
                        }
                        _ => end += 1,
                    }
                }
                if closed {
                    tokens.push(projection[start..end].to_string());
                    i = end;
                } else {
                    i = atom_end(bytes, start);
                    tokens.push(projection[start..i].to_string());
                }
            }
            _ => {
                let start = i;
                i = atom_end(bytes, start);
                tokens.push(projection[start..i].to_string());
            }
        }
    }
    tokens
}

fn atom_end(bytes: &[u8], start: usize) -> usize {
    let mut end = start;
    while bytes
        .get(end)
        .is_some_and(|byte| !matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | b'(' | b')'))
    {
        end += 1;
    }
    end
}

fn parse_shape(tokens: &[String], cursor: &mut usize) -> Option<Shape> {
    let token = tokens.get(*cursor)?;
    *cursor += 1;
    if token != "(" {
        return (token != ")").then(|| Shape::Atom(token.clone()));
    }
    let mut items = Vec::new();
    while tokens.get(*cursor).is_some_and(|token| token != ")") {
        items.push(parse_shape(tokens, cursor)?);
    }
    if tokens.get(*cursor)? != ")" {
        return None;
    }
    *cursor += 1;
    Some(Shape::List(items))
}

fn normalize_shape(shape: Shape, is_root: bool) -> Result<Shape, ()> {
    let Shape::List(items) = shape else {
        return normalize_float_atom(shape);
    };
    let opaque_payload = head_is(&items, "var") || head_is(&items, "char");
    let mut items = if opaque_payload {
        items
    } else {
        items
            .into_iter()
            .map(|shape| normalize_shape(shape, false))
            .collect::<Result<Vec<_>, _>>()?
    };

    if head_is(&items, "where") {
        for parameter in items.iter_mut().skip(2) {
            let Shape::List(braces) = parameter else {
                continue;
            };
            let already_delimited = matches!(
                braces.get(1),
                Some(Shape::List(child)) if head_is(child, "bracescat")
            );
            if braces.len() == 2
                && !already_delimited
                && matches!(&braces[0], Shape::Atom(head) if head == "braces")
            {
                *parameter = braces[1].clone();
            }
        }
    }

    if is_root && head_is(&items, "toplevel") {
        let mut flattened = Vec::with_capacity(items.len());
        flattened.push(items.remove(0));
        for item in items {
            match item {
                Shape::List(mut group) if head_is(&group, "toplevel-;") => {
                    flattened.extend(group.drain(1..));
                }
                other => flattened.push(other),
            }
        }
        items = flattened;
    }

    Ok(Shape::List(items))
}

fn head_is(items: &[Shape], expected: &str) -> bool {
    matches!(items.first(), Some(Shape::Atom(head)) if head == expected)
}

fn normalize_float_atom(shape: Shape) -> Result<Shape, ()> {
    let Shape::Atom(atom) = shape else {
        return Ok(shape);
    };
    let Some((is_float32, parseable)) = decimal_float(&atom) else {
        return Ok(Shape::Atom(atom));
    };
    let fingerprint = if is_float32 {
        let Ok(value) = parseable.parse::<f32>() else {
            return Ok(Shape::Atom(atom));
        };
        if !value.is_finite() {
            return Err(());
        }
        format!("float32:{:08x}", value.to_bits())
    } else {
        let Ok(value) = parseable.parse::<f64>() else {
            return Ok(Shape::Atom(atom));
        };
        if !value.is_finite() {
            return Err(());
        }
        format!("float64:{:016x}", value.to_bits())
    };
    Ok(Shape::Atom(fingerprint))
}

/// Recognize a decimal Julia float and return its type plus a Rust-parseable
/// spelling. Hex and underscored floats are unchanged by the formatter and do
/// not need a semantic normalization here.
fn decimal_float(atom: &str) -> Option<(bool, String)> {
    if atom.contains('_') || atom.contains("0x") || atom.contains("0X") {
        return None;
    }
    let text = atom.replace('\u{2212}', "-");
    let bytes = text.as_bytes();
    let mut i = usize::from(matches!(bytes.first(), Some(b'+') | Some(b'-')));
    let mut digits = 0usize;
    while bytes.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
        digits += 1;
    }
    let mut has_dot = false;
    if bytes.get(i) == Some(&b'.') {
        has_dot = true;
        i += 1;
        while bytes.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return None;
    }

    let mut is_float32 = false;
    let mut marker = None;
    if bytes
        .get(i)
        .is_some_and(|byte| matches!(byte, b'e' | b'E' | b'f' | b'F'))
    {
        is_float32 = matches!(bytes[i], b'f' | b'F');
        marker = Some(i);
        i += 1;
        if bytes.get(i).is_some_and(|byte| matches!(byte, b'+' | b'-')) {
            i += 1;
        }
        let exponent_start = i;
        while bytes.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == exponent_start {
            return None;
        }
    }
    if i != bytes.len() || (!has_dot && marker.is_none()) {
        return None;
    }

    let mut parseable = text;
    if let Some(marker) = marker {
        parseable.replace_range(marker..=marker, "e");
    }
    Some((is_float32, parseable))
}

fn comments(root: &fatou_parser::syntax::SyntaxNode) -> Vec<Comment> {
    root.descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter_map(|token| match token.kind() {
            SyntaxKind::COMMENT => Some(Comment {
                kind: token.kind(),
                text: token.text().trim_end_matches([' ', '\t']).to_string(),
            }),
            SyntaxKind::BLOCK_COMMENT => Some(Comment {
                kind: token.kind(),
                text: normalize_eols(token.text()),
            }),
            _ => None,
        })
        .collect()
}

fn normalize_eols(text: &str) -> String {
    if text.contains('\r') {
        text.replace("\r\n", "\n")
    } else {
        text.to_string()
    }
}

/// Projector leaves whose payload grammar is not self-delimiting stay exact.
/// The formatter has no licensed rewrite for either spelling, so comparing the
/// source text fails closed without interpreting it as s-expression syntax.
fn opaque_leaves(root: &fatou_parser::syntax::SyntaxNode) -> Vec<OpaqueLeaf> {
    root.descendants_with_tokens()
        .filter_map(|element| match element {
            rowan::NodeOrToken::Node(node) if node.kind() == SyntaxKind::NONSTANDARD_IDENTIFIER => {
                Some(OpaqueLeaf {
                    kind: node.kind(),
                    text: node.text().to_string(),
                })
            }
            rowan::NodeOrToken::Token(token) if token.kind() == SyntaxKind::CHAR => {
                Some(OpaqueLeaf {
                    kind: token.kind(),
                    text: token.text().to_string(),
                })
            }
            _ => None,
        })
        .collect()
}

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

    #[test]
    fn verified_format_accepts_licensed_canonicalizations() {
        for (input, formatted) in [
            ("f(a,b)", "f(\n    a,\n    b,\n)"),
            ("x where T", "x where {T}"),
            ("a; b", "a\nb"),
            ("x = .5", "x = 0.5"),
            ("x = 1f0", "x = 1.0f0"),
            ("x = '('; y=.5", "x = '('\ny = 0.5"),
            ("var\"\\\"\"; x=.5", "var\"\\\"\"\nx = 0.5"),
        ] {
            assert_eq!(verify_format(input, formatted), Ok(()), "{input:?}");
        }
    }

    #[test]
    fn verified_format_rejects_program_changes() {
        for (input, formatted) in [
            ("x = (1 + 2) * 3", "x = 1 + 2 * 3"),
            ("x = 1f0", "x = 1.0"),
            ("x = -0.0", "x = 0.0"),
            ("x = 0xff_ff", "x = 0x000ff_ff"),
            ("x where {T S}", "x where {{T S}}"),
            ("+(a=1,)", "+(a=1)"),
            ("@foo a [1]", "@foo a[1]"),
        ] {
            assert_eq!(
                verify_format(input, formatted),
                Err(VerificationError::ChangedProgram),
                "{input:?} -> {formatted:?}"
            );
        }
    }

    #[test]
    fn verified_format_keeps_projected_leaf_payloads_opaque() {
        for (input, formatted) in [
            (r#"var"1e2""#, r#"var"100.0""#),
            (r#"var"a  b""#, r#"var"a b""#),
            ("x = '\\x61'", "x = 'a'"),
        ] {
            assert_eq!(
                verify_format(input, formatted),
                Err(VerificationError::ChangedProgram),
                "{input:?} -> {formatted:?}"
            );
        }
    }

    #[test]
    fn overflowing_decimal_literals_fail_as_input_syntax() {
        assert!(matches!(
            verify_format("x=1e400", "x=2e400"),
            Err(VerificationError::InputSyntax { .. })
        ));
        assert!(shapes_from_projection("(toplevel (= x 1e400))").is_none());
    }

    #[test]
    fn verified_format_preserves_comment_payloads() {
        assert_eq!(verify_format("# note  \nx=1", "# note\nx = 1"), Ok(()));
        assert_eq!(
            verify_format(
                "#= first\nsecond =#\nx=.5\n",
                "#= first\r\nsecond =#\r\nx = 0.5\r\n",
            ),
            Ok(())
        );
        assert_eq!(
            verify_format("# before\nx=1", "# after\nx = 1"),
            Err(VerificationError::ChangedComments)
        );
        assert_eq!(
            verify_format("#= block  =#\nx=1", "#= block =#\nx = 1"),
            Err(VerificationError::ChangedComments)
        );
    }

    #[test]
    fn verified_format_fails_closed_on_parse_errors() {
        assert!(matches!(
            verify_format("function f(", "function f("),
            Err(VerificationError::InputSyntax { .. })
        ));
        assert!(matches!(
            verify_format("x = 1", "function f("),
            Err(VerificationError::OutputSyntax { .. })
        ));
        assert!(matches!(
            verify_format("x = 1", "x = 1e400"),
            Err(VerificationError::OutputSyntax { .. })
        ));
    }

    #[test]
    fn unsupported_projection_sentinels_fail_closed() {
        assert!(shapes_from_projection("(toplevel (unsupported FOO))").is_none());
        assert!(shapes_from_projection(r#"(toplevel (string "(unsupported FOO)"))"#).is_some());
        assert!(shapes_from_projection("(toplevel (var \" ) (var \"))").is_some());
    }
}
