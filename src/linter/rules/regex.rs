//! Regex-literal classification, for a rule that asks whether an `r"..."` is
//! really a pattern at all.
//!
//! A regex literal that carries no metacharacter matches one fixed substring,
//! and one anchored at a single end tests a prefix or a suffix. Both are
//! questions about the literal's *text*, and both come with the same reading
//! problem: a Julia regex literal is a non-standard string literal, so its
//! content is the raw pattern (no escape processing, no interpolation) only
//! while it is written in the shape [`regex_pattern`] admits. Reading the
//! literal and judging its text therefore live together here rather than in
//! each rule.
//!
//! The metacharacter set is PCRE's, which is what Julia's `Regex` compiles.
//! Every character that could mean something other than itself is excluded, so
//! a text this module calls fixed matches literally under any subject —
//! `-` (a range only inside a class) and `#` (a comment only under the `x`
//! flag) included, since a literal admitted here carries neither a class nor a
//! flag.

use crate::ast::StringLiteral;

/// The raw pattern text of `literal` when it is a plain regex literal: the `r`
/// prefix, no flag suffix, and no interpolation.
///
/// Everything excluded here would make the content something other than the
/// pattern PCRE sees. A flag suffix (`r"abc"i`) changes what the pattern means,
/// down to whether `#` and whitespace are syntax; an interpolation makes the
/// pattern partly unknown; and another prefix (`raw"..."`, `s"..."`) is not a
/// regex at all. A plain string literal has no prefix and answers `None` too.
///
/// The delimiter is deliberately not constrained: a triple-quoted `r"""..."""`
/// carries the same content, and both consumers either reuse the literal's own
/// delimiters or ask [`plain_string_literal`] whether the text can be requoted.
pub fn regex_pattern(literal: &StringLiteral) -> Option<String> {
    if literal.prefix()?.text() != "r" {
        return None;
    }
    if literal.suffix().is_some() || literal.interpolations().next().is_some() {
        return None;
    }
    Some(
        literal
            .content_tokens()
            .map(|token| token.text().to_string())
            .collect(),
    )
}

/// Whether `pattern` carries no regex metacharacter and no backslash escape, so
/// PCRE matches it as the literal text it spells. All of them are ASCII, so a
/// byte scan is enough.
pub fn is_plain_literal(pattern: &str) -> bool {
    !pattern.bytes().any(|b| {
        matches!(
            b,
            b'.' | b'\\'
                | b'|'
                | b'('
                | b')'
                | b'['
                | b']'
                | b'{'
                | b'}'
                | b'^'
                | b'$'
                | b'*'
                | b'+'
                | b'?'
        )
    })
}

/// A non-empty plain literal: the "this pattern is really a fixed string" test.
/// An empty pattern matches everywhere and is no rewrite candidate.
pub fn is_fixed_string(pattern: &str) -> bool {
    !pattern.is_empty() && is_plain_literal(pattern)
}

/// Which end a pattern is anchored to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    Start,
    End,
}

/// If `pattern` is anchored at exactly one end against a non-empty plain
/// literal, the anchor and the anchor-stripped remainder: `^abc` ->
/// `(Start, "abc")`, `abc$` -> `(End, "abc")`.
///
/// `None` for a both-ends `^abc$` (an exact match, not a boundary test), no
/// anchor, an empty remainder, or a remainder carrying any other
/// metacharacter — in each case the pattern is not the prefix or suffix test
/// this classification exists to name.
pub fn single_anchor(pattern: &str) -> Option<(Anchor, &str)> {
    let (anchor, rest) = if let Some(rest) = pattern.strip_prefix('^') {
        if rest.ends_with('$') {
            return None;
        }
        (Anchor::Start, rest)
    } else {
        // Reached only when `pattern` does not start with `^`, so this is a
        // lone trailing anchor.
        (Anchor::End, pattern.strip_suffix('$')?)
    };
    is_fixed_string(rest).then_some((anchor, rest))
}

/// `text` written as a Julia string literal spelling exactly those characters,
/// or `None` when `"..."` would not spell it.
///
/// Julia's ordinary string literal gives three characters a meaning the raw
/// content of a regex literal does not have — `"` closes it, `\` escapes, and
/// `$` interpolates — and a literal newline cannot appear in a single-quoted
/// one at all. Requoting is exact only in their absence, which is what this
/// checks; a rule that cannot requote withholds its fix rather than guessing at
/// escapes.
pub fn plain_string_literal(text: &str) -> Option<String> {
    let requotable = !text.contains(['"', '\\', '$', '\n', '\r']);
    requotable.then(|| format!("\"{text}\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{AstNode, Expr};
    use crate::parser::parse;

    /// The pattern of the first string literal in the parse of `src`.
    fn pattern_of(src: &str) -> Option<String> {
        let literal = parse(src)
            .cst
            .descendants()
            .find_map(|node| match Expr::cast(node)? {
                Expr::StringLiteral(literal) => Some(literal),
                _ => None,
            })
            .expect("a string literal");
        regex_pattern(&literal)
    }

    #[test]
    fn regex_pattern_reads_a_plain_regex_literal() {
        assert_eq!(
            pattern_of("occursin(r\"abc\", s)\n").as_deref(),
            Some("abc")
        );
        // A triple-quoted literal carries the same content.
        assert_eq!(
            pattern_of("occursin(r\"\"\"a\"b\"\"\", s)\n").as_deref(),
            Some("a\"b")
        );
        // `$` is an anchor here, not an interpolation: the content is raw.
        assert_eq!(pattern_of("occursin(r\"a$\", s)\n").as_deref(), Some("a$"));
        // An empty pattern is still a pattern.
        assert_eq!(pattern_of("occursin(r\"\", s)\n").as_deref(), Some(""));
    }

    #[test]
    fn regex_pattern_declines_anything_else() {
        // A flag changes what the pattern means.
        assert!(pattern_of("occursin(r\"abc\"i, s)\n").is_none());
        // Another prefix is another literal entirely.
        assert!(pattern_of("occursin(raw\"abc\", s)\n").is_none());
        assert!(pattern_of("replace(s, \"a\" => s\"b\")\n").is_none());
        // A plain string literal has no prefix.
        assert!(pattern_of("occursin(\"abc\", s)\n").is_none());
    }

    #[test]
    fn plain_literal_rejects_metacharacters() {
        assert!(is_plain_literal("abc"));
        assert!(is_plain_literal("hello world"));
        // Neither a range nor a comment outside a class or the `x` flag.
        assert!(is_plain_literal("a-b#c"));
        assert!(!is_plain_literal("a.b"));
        assert!(!is_plain_literal("a\\db"));
        assert!(!is_plain_literal("^abc"));
        assert!(!is_plain_literal("a+b"));
    }

    #[test]
    fn fixed_string_requires_nonempty_plain() {
        assert!(is_fixed_string("abc"));
        assert!(!is_fixed_string(""));
        assert!(!is_fixed_string("a.b"));
    }

    #[test]
    fn single_anchor_classifies_one_end() {
        assert_eq!(single_anchor("^abc"), Some((Anchor::Start, "abc")));
        assert_eq!(single_anchor("abc$"), Some((Anchor::End, "abc")));
        // Both ends is an exact match, not a boundary test.
        assert!(single_anchor("^abc$").is_none());
        assert!(single_anchor("abc").is_none());
        // Nothing left after the sole anchor.
        assert!(single_anchor("^").is_none());
        assert!(single_anchor("$").is_none());
        // The remainder is a pattern in its own right.
        assert!(single_anchor("^a.b").is_none());
    }

    #[test]
    fn plain_string_literal_requotes_only_what_it_can_spell() {
        assert_eq!(plain_string_literal("abc").as_deref(), Some("\"abc\""));
        assert!(plain_string_literal("a\"b").is_none());
        assert!(plain_string_literal("a\\b").is_none());
        assert!(plain_string_literal("a$b").is_none());
        assert!(plain_string_literal("a\nb").is_none());
    }
}
