//! Regex literals: which call positions carry one, and whether it is really a
//! pattern at all.
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
//! [`PatternCall`] answers the other half, which is not about the literal but
//! about Base's argument order: `occursin(needle, haystack)` and
//! `contains(haystack, needle)` are the same search written the two ways
//! round, only one of their curried forms fixes the *needle*, and the rest of
//! the family (`startswith`, `split`, `replace`, ...) each keeps its pattern
//! somewhere else again. Getting that backwards would make a rule rewrite the
//! wrong argument, so both rules ask once here.
//!
//! The metacharacter set is PCRE's, which is what Julia's `Regex` compiles.
//! Every character that could mean something other than itself is excluded, so
//! a text this module calls fixed matches literally under any subject —
//! `-` (a range only inside a class) and `#` (a comment only under the `x`
//! flag) included, since a literal admitted here carries neither a class nor a
//! flag.

use crate::ast::{AstToken, CallExpr, Expr, StringLiteral};
use crate::linter::rules::matchers::{self, CallShape};
use crate::syntax::{SyntaxKind, SyntaxNode};

/// What a call does with the pattern it takes, as far as these rules care.
#[derive(Clone, Copy)]
enum Role {
    /// A search for the pattern *anywhere* in a string, which a boundary
    /// predicate can replace outright — carrying the searched string's
    /// argument index, or `None` for the curried form, which has none.
    Search(Option<usize>),
    /// Every other consumer: the pattern can become a plain string in place,
    /// but the call keeps its shape.
    Other,
}

/// One Base function's pattern position: the callee, the positional arity it
/// is matched at, where the pattern sits, and what the call does with it.
///
/// Arity is part of the key because a curried form is a different position:
/// `contains(needle)` fixes the needle, while `occursin(haystack)` fixes the
/// haystack and so appears here not at all.
struct Shape {
    name: &'static str,
    arity: usize,
    pattern: usize,
    role: Role,
}

/// Every call position where a Julia regex literal means the same thing a
/// plain string would.
///
/// `rsplit` is absent on purpose: it has no `Regex` method at all (it would
/// need `findprev`), so a regex there is an error rather than an idiom.
/// `replace` is absent because its pattern hides inside a `=>` pair, which
/// [`PatternCall::all`] handles separately.
const SHAPES: &[Shape] = &[
    Shape {
        name: "occursin",
        arity: 2,
        pattern: 0,
        role: Role::Search(Some(1)),
    },
    // `contains` is `occursin` the other way round, and its curried form fixes
    // the needle rather than the haystack.
    Shape {
        name: "contains",
        arity: 2,
        pattern: 1,
        role: Role::Search(Some(0)),
    },
    Shape {
        name: "contains",
        arity: 1,
        pattern: 0,
        role: Role::Search(None),
    },
    // A regex prefix/suffix is anchored by the predicate itself, so a fixed
    // one means exactly the string it spells.
    Shape {
        name: "startswith",
        arity: 2,
        pattern: 1,
        role: Role::Other,
    },
    Shape {
        name: "startswith",
        arity: 1,
        pattern: 0,
        role: Role::Other,
    },
    Shape {
        name: "endswith",
        arity: 2,
        pattern: 1,
        role: Role::Other,
    },
    Shape {
        name: "endswith",
        arity: 1,
        pattern: 0,
        role: Role::Other,
    },
    Shape {
        name: "split",
        arity: 2,
        pattern: 1,
        role: Role::Other,
    },
    Shape {
        name: "eachsplit",
        arity: 2,
        pattern: 1,
        role: Role::Other,
    },
];

/// A Base call whose pattern is written as a plain regex literal.
///
/// The point of going through this is that every function in the family keeps
/// its pattern somewhere else: `occursin(r"a", s)` and `contains(s, r"a")` are
/// the same search written both ways round, `split`/`startswith`/`endswith`
/// take theirs second, the curried `contains(r"a")`/`startswith(r"a")` fix the
/// pattern while the curried `occursin(s)` fixes the *haystack* (and so carries
/// no pattern at all), and `replace`'s hides inside a `=>` pair.
///
/// Matching the shape is half the job — the callee still has to be confirmed
/// Base's with
/// [`RuleContext::resolves_to_base`](super::RuleContext::resolves_to_base).
pub struct PatternCall {
    /// The whole call, for the namespace gate and for a rewrite's span.
    pub call: CallExpr,
    /// The pattern's literal, whose prefix a fix may drop.
    pub literal: StringLiteral,
    /// The raw pattern text (see [`regex_pattern`]).
    pub pattern: String,
    /// Whether the call searches for the pattern anywhere in a string, the one
    /// shape a boundary predicate can take over.
    pub search: bool,
    /// The searched string, for a [`search`](Self::search) call that has one.
    pub haystack: Option<Expr>,
}

impl PatternCall {
    /// Every plain regex literal `node` passes as a pattern, in source order.
    /// Empty for anything else; more than one only for a multi-pair `replace`.
    pub fn all(node: &SyntaxNode) -> Vec<Self> {
        for shape in SHAPES {
            let Some((call, args)) = matchers::plain_call(node, shape.name, shape.arity) else {
                continue;
            };
            let haystack = match shape.role {
                Role::Search(at) => at.and_then(|at| args.get(at)).cloned(),
                Role::Other => None,
            };
            let search = matches!(shape.role, Role::Search(_));
            return Self::build(&call, args.get(shape.pattern).cloned(), search, haystack)
                .into_iter()
                .collect();
        }
        Self::replace_pairs(node)
    }

    /// The one search call `node` may be, for a rule that rewrites the search
    /// itself rather than its pattern.
    pub fn search(node: &SyntaxNode) -> Option<Self> {
        Self::all(node).into_iter().find(|found| found.search)
    }

    /// `replace(s, r"a" => x, ...)`, whose patterns sit on the left of each
    /// `=>` pair. The subject is the first argument and is never a pattern; an
    /// argument that is no pair at all (a function, a splatted collection)
    /// simply contributes none.
    fn replace_pairs(node: &SyntaxNode) -> Vec<Self> {
        let Some(call) = matchers::call_named(node, "replace") else {
            return Vec::new();
        };
        let shape = CallShape::of(&call);
        if shape.positional.len() < 2
            || !shape.keywords.is_empty()
            || shape.positional_open
            || shape.keyword_open
            || shape.do_block
        {
            return Vec::new();
        }
        shape.positional[1..]
            .iter()
            .filter_map(|arg| Self::build(&call, pair_pattern(arg), false, None))
            .collect()
    }

    fn build(
        call: &CallExpr,
        pattern: Option<Expr>,
        search: bool,
        haystack: Option<Expr>,
    ) -> Option<Self> {
        let Expr::StringLiteral(literal) = pattern? else {
            return None;
        };
        Some(Self {
            call: call.clone(),
            pattern: regex_pattern(&literal)?,
            literal,
            search,
            haystack,
        })
    }
}

/// The left-hand side of a `=>` pair, which is where `replace` keeps a pattern.
fn pair_pattern(arg: &Expr) -> Option<Expr> {
    let Expr::BinaryExpr(pair) = arg else {
        return None;
    };
    if pair.op()?.syntax().kind() != SyntaxKind::FAT_ARROW {
        return None;
    }
    pair.lhs()
}

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

    /// The pattern and haystack of the first search call in the parse of
    /// `src`, as source text.
    fn search_call(src: &str) -> Option<(String, Option<String>)> {
        parse(src).cst.descendants().find_map(|node| {
            let found = PatternCall::search(&node)?;
            Some((
                found.pattern,
                found
                    .haystack
                    .map(|haystack| haystack.syntax().text().to_string()),
            ))
        })
    }

    /// Every pattern the first pattern-carrying call in `src` passes.
    fn patterns(src: &str) -> Vec<String> {
        parse(src)
            .cst
            .descendants()
            .map(|node| PatternCall::all(&node))
            .find(|found| !found.is_empty())
            .unwrap_or_default()
            .into_iter()
            .map(|found| found.pattern)
            .collect()
    }

    #[test]
    fn pattern_call_reads_the_whole_family() {
        assert_eq!(patterns("split(line, r\"::\")\n"), ["::"]);
        assert_eq!(patterns("eachsplit(line, r\"::\")\n"), ["::"]);
        assert_eq!(patterns("startswith(name, r\"Test\")\n"), ["Test"]);
        assert_eq!(patterns("filter(endswith(r\"jl\"), names)\n"), ["jl"]);
        // Each pair of a `replace` carries its own pattern; the subject and the
        // replacement sides carry none.
        assert_eq!(
            patterns("replace(s, r\"a\" => \"1\", r\"b\" => \"2\")\n"),
            ["a", "b"]
        );
        assert!(patterns("replace(s, \"a\" => r\"b\")\n").is_empty());
        assert!(patterns("replace(s, x, r\"a\")\n").is_empty());
        // `rsplit` takes no `Regex` at all, so a regex there is an error.
        assert!(patterns("rsplit(line, r\"::\")\n").is_empty());
    }

    #[test]
    fn pattern_call_reads_both_argument_orders() {
        assert_eq!(
            search_call("occursin(r\"abc\", s)\n"),
            Some(("abc".to_string(), Some("s".to_string())))
        );
        // `contains` writes the haystack first.
        assert_eq!(
            search_call("contains(s, r\"abc\")\n"),
            Some(("abc".to_string(), Some("s".to_string())))
        );
        // Its curried form fixes the needle and has no haystack.
        assert_eq!(
            search_call("filter(contains(r\"abc\"), lines)\n"),
            Some(("abc".to_string(), None))
        );
    }

    #[test]
    fn pattern_call_declines_a_needle_that_is_not_one() {
        // `occursin`'s curried form fixes the haystack, not the needle.
        assert!(search_call("filter(occursin(r\"abc\"), lines)\n").is_none());
        // The literal is in the other argument.
        assert!(search_call("occursin(s, r\"abc\")\n").is_none());
        assert!(search_call("contains(r\"abc\", s)\n").is_none());
        // Not a plain regex literal, or not a search call.
        assert!(search_call("occursin(needle, s)\n").is_none());
        assert!(search_call("occursin(r\"abc\"i, s)\n").is_none());
        assert!(search_call("match(r\"abc\", s)\n").is_none());
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
