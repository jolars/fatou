//! `string-boundary`: `occursin(r"^abc", s)` is the prefix test
//! `startswith(s, "abc")`, and `occursin(r"abc$", s)` the suffix test
//! `endswith(s, "abc")`.
//!
//! A regex anchored at one end against an otherwise fixed string is a boundary
//! test written as a search. `startswith`/`endswith` say which boundary is
//! meant, take the arguments in the order Julia's other string predicates do,
//! and skip the match engine.
//!
//! Only the unambiguous shape is matched: a search whose needle is a plain
//! `r"..."` pattern (see [`regex::PatternCall`] for the three spellings, one
//! per argument order) — no flag suffix, since `i` and `m` both change what an
//! anchor means — anchored at exactly one end with a metacharacter-free
//! remainder, and with the callee confirmed to be Base's. `r"^abc$"` is an
//! exact match rather than a boundary test and is left alone.
//!
//! The curried `contains(r"^abc")` rewrites to the curried `startswith("abc")`,
//! which fixes the same argument it did.
//!
//! **The two anchors do not have the same fix.** `^` matches at the start of
//! the subject and nowhere else, so the `startswith` rewrite is exact and its
//! fix is `Safe`. PCRE's `$`, by contrast, matches at the end of the subject
//! *or* before a final newline, so `occursin(r"abc$", "abc\n")` is `true`
//! where `endswith("abc\n", "abc")` is `false` — a real divergence on a real
//! input, which is why the `endswith` rewrite waits for `--unsafe-fixes`. The
//! finding stands either way; a reader who did mean the newline case wants to
//! know the anchor is doing that work.
//!
//! The fix reuses the haystack's source text and rebuilds the pattern as an
//! ordinary string literal, so it is withheld — the finding still stands —
//! when the name it splices in (`startswith`/`endswith`) is not Base's at the
//! rewrite site, when the remainder cannot be requoted (see
//! [`regex::plain_string_literal`]), or when a comment sits in the replaced
//! span outside the two reused pieces.

use crate::ast::AstNode;
use crate::linter::diagnostic::{Applicability, Diagnostic, Fix};
use crate::linter::rules::regex::{Anchor, PatternCall};
use crate::linter::rules::{Example, Rule, RuleContext, regex, rewrite};
use crate::syntax::{SyntaxElement, SyntaxKind, SyntaxNode};

pub struct StringBoundary;

impl Rule for StringBoundary {
    fn id(&self) -> &'static str {
        "string-boundary"
    }

    fn description(&self) -> &'static str {
        "Flag `occursin(r\"^abc\", s)` and `occursin(r\"abc$\", s)`, boundary \
         tests written as anchored regex searches. `startswith(s, \"abc\")` and \
         `endswith(s, \"abc\")` name the boundary they test and need no match \
         engine.\n\n\
         The pattern must be a plain `r\"...\"` — no flag suffix, since `i` and \
         `m` change what an anchor means — anchored at exactly one end with a \
         non-empty, metacharacter-free remainder; `r\"^abc$\"` is an exact \
         match rather than a boundary test. The callee must be confirmed to be \
         Base's, and the needle is read wherever its spelling puts it: \
         `occursin(r\"^abc\", s)`, the flipped `contains(s, r\"^abc\")`, and \
         the curried `contains(r\"^abc\")`, which rewrites to the curried \
         `startswith(\"abc\")`.\n\n\
         The `startswith` fix is safe: `^` matches at the start of the subject \
         and nowhere else. The `endswith` fix needs `--unsafe-fixes`, because \
         PCRE's `$` also matches before a final newline — \
         `occursin(r\"abc$\", \"abc\\n\")` is `true` where \
         `endswith(\"abc\\n\", \"abc\")` is `false`. Either fix is withheld — \
         the finding still stands — when the file's `startswith`/`endswith` is \
         not Base's, or when a comment sits in the rewritten span outside the \
         haystack and the pattern."
    }

    fn examples(&self) -> &'static [Example] {
        &[
            Example {
                caption: "A leading anchor is a prefix test:",
                source: "if occursin(r\"^Test\", name)\n    run(name)\nend\n",
            },
            Example {
                caption: "A trailing anchor is a suffix test:",
                source: "sources = filter(f -> occursin(r\"_test$\", f), files)\n",
            },
            Example {
                caption: "A curried `contains` rewrites to a curried predicate:",
                source: "sources = filter(contains(r\"^src\"), files)\n",
            },
        ]
    }

    fn interests(&self) -> &'static [SyntaxKind] {
        &[SyntaxKind::CALL_EXPR]
    }

    fn check(&self, el: &SyntaxElement, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let Some(node) = el.as_node() else { return };
        let Some(found) = regex::PatternCall::of(node) else {
            return;
        };
        let Some((anchor, rest)) = regex::single_anchor(&found.pattern) else {
            return;
        };
        if !ctx.resolves_to_base(&found.call) {
            return;
        }

        let boundary = Boundary::of(anchor, rest);
        let func = boundary.func;
        // The message quotes the call it proposes, so it needs every piece on
        // one line; the fix has no such need and splices the haystack raw.
        let haystack = match &found.haystack {
            Some(haystack) => rewrite::inline_text(haystack.syntax()).map(Some),
            None => Some(None),
        };
        let message = match (
            boundary.literal.as_deref(),
            haystack,
            rewrite::inline_text(found.literal.syntax()),
        ) {
            (Some(new_literal), Some(haystack), Some(old)) => format!(
                "test `{}` instead of matching the anchored regex `{old}`",
                boundary.call(haystack.as_deref(), new_literal)
            ),
            _ => format!("test the boundary with `{func}` instead of matching an anchored regex"),
        };

        let mut diag = Diagnostic::new(self.id(), found.call.syntax().text_range(), message);
        if let Some(fix) = boundary_rewrite(ctx, node, &found, &boundary) {
            diag.fixes.push(fix);
        }
        sink.push(diag);
    }
}

/// The predicate one anchor calls for: its name, and the anchor-stripped
/// remainder as an ordinary string literal — `None` when the remainder cannot
/// be requoted, which costs the fix but not the finding.
struct Boundary {
    func: &'static str,
    anchor: Anchor,
    literal: Option<String>,
}

impl Boundary {
    fn of(anchor: Anchor, rest: &str) -> Self {
        Self {
            func: match anchor {
                Anchor::Start => "startswith",
                Anchor::End => "endswith",
            },
            anchor,
            literal: regex::plain_string_literal(rest),
        }
    }

    /// The predicate call this boundary proposes. A searched string gives the
    /// two-argument form; the curried search has none and gives the curried
    /// predicate, which fixes the same argument its `contains` did.
    fn call(&self, haystack: Option<&str>, literal: &str) -> String {
        match haystack {
            Some(haystack) => format!("{}({haystack}, {literal})", self.func),
            None => format!("{}({literal})", self.func),
        }
    }
}

/// The fix replacing the whole search call with the boundary predicate: the
/// haystack's own source text, and the rebuilt pattern literal.
///
/// Safe for `^`, unsafe for `$` (see the module doc). Withheld when the name
/// would not mean Base's here, when the remainder cannot be requoted, or when
/// the replaced span carries a comment outside the pieces it reuses — which
/// includes a `#` inside the pattern, the harmless direction.
fn boundary_rewrite(
    ctx: &RuleContext<'_>,
    call: &SyntaxNode,
    found: &PatternCall,
    boundary: &Boundary,
) -> Option<Fix> {
    let span = call.text_range();
    let new_literal = boundary.literal.as_deref()?;
    if !ctx.name_resolves_to_base(boundary.func, span.start()) {
        return None;
    }
    // In source order, which `contains` and `occursin` disagree about.
    let mut keep = vec![found.literal.syntax().text_range()];
    if let Some(haystack) = &found.haystack {
        keep.push(haystack.syntax().text_range());
        keep.sort_by_key(|range| range.start());
    }
    if rewrite::drops_a_comment(call, &keep) {
        return None;
    }
    let (applicability, description) = match boundary.anchor {
        Anchor::Start => (
            Applicability::Safe,
            "Test the prefix with `startswith`".to_string(),
        ),
        // The rewrite drops PCRE's "or before a final newline" reading of `$`.
        Anchor::End => (
            Applicability::Unsafe,
            "Test the suffix with `endswith`, which does not match before a trailing newline"
                .to_string(),
        ),
    };
    let haystack = found
        .haystack
        .as_ref()
        .map(|haystack| haystack.syntax().text().to_string());
    Some(Fix {
        description,
        content: boundary.call(haystack.as_deref(), new_literal),
        start: span.start().into(),
        end: span.end().into(),
        applicability,
    })
}
