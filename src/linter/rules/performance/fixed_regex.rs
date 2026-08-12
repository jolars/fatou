//! `fixed-regex`: `occursin(r"abc", s)`, a regex literal that spells one fixed
//! substring.
//!
//! A pattern carrying no metacharacter matches exactly the text it is written
//! as, so the call asks "does this substring occur in `s`?" and answers it
//! through PCRE. `occursin("abc", s)` asks the same question of the same two
//! values and answers it with a substring search — no pattern, no match engine.
//!
//! The two forms agree on every `AbstractString` haystack, invalid UTF-8
//! included (Julia compiles `r"..."` with `MATCH_INVALID_UTF`), which is why
//! this is the one rewrite in this category whose equivalence does not depend
//! on the operand's type.
//!
//! The same holds of every other Base function that takes a pattern —
//! `split(s, r"::")`, `startswith(s, r"jl")`, `replace(s, r"tab" => " ")` —
//! and for the same reason, so all of them are matched. A fixed pattern also
//! rules out the one shape where the *replacement* could read differently: a
//! `SubstitutionString` group reference needs a capture group, and `(` is a
//! metacharacter.
//!
//! **The fix is `Safe`, and it deletes exactly one token: the `r` prefix.**
//! What makes that enough is what the rule already required — a fixed string
//! carries no backslash and no `$`, the two characters an ordinary literal
//! reads differently from a non-standard one, and the literal keeps its own
//! delimiters and content bytes, so `r"""a"b"""` becomes `"""a"b"""` with the
//! embedded quote intact. Nothing is discarded, so no comment can be dropped
//! and no name is spliced in that would need confirming.
//!
//! Which argument holds the pattern is [`regex::PatternCall`]'s question, and
//! it differs per function — `occursin(r"a", s)` against `contains(s, r"a")`,
//! a curried form that fixes the pattern (`contains(r"a")`) against one that
//! fixes the haystack (`occursin(s)`, matched by nothing here), and `replace`'s
//! left-of-`=>` position. The callee still has to be confirmed Base's.

use crate::ast::AstNode;
use crate::linter::diagnostic::{Applicability, Diagnostic, Fix};
use crate::linter::rules::{Example, Rule, RuleContext, regex, rewrite};
use crate::syntax::{SyntaxElement, SyntaxKind};

pub struct FixedRegex;

impl Rule for FixedRegex {
    fn id(&self) -> &'static str {
        "fixed-regex"
    }

    fn description(&self) -> &'static str {
        "Flag `occursin(r\"abc\", s)`, whose regex literal carries no \
         metacharacter and so matches one fixed substring. `occursin(\"abc\", \
         s)` asks the same question of the same values and answers it with a \
         substring search instead of the regex engine.\n\n\
         Every Base function that takes a pattern is read the same way, each at \
         the argument that holds it: `occursin(r\"abc\", s)`, the flipped \
         `contains(s, r\"abc\")`, `startswith`/`endswith`, `split`/`eachsplit`, \
         each `=>` pair of a `replace`, and the curried forms that fix the \
         pattern (`contains(r\"abc\")`). The curried `occursin(s)` fixes the \
         *haystack* instead, so its argument is no pattern and is left alone, \
         and `rsplit` takes no regex at all.\n\n\
         The pattern must be a plain `r\"...\"` — no flag suffix, since a flag \
         changes what the pattern means, and no interpolation — and it must be \
         non-empty and free of every PCRE metacharacter and backslash escape. \
         The callee must be confirmed to be Base's, so a local shadow, a \
         qualified `Base.occursin`, or a file whose imports cannot be resolved \
         reports nothing.\n\n\
         The safe fix deletes the `r` prefix and nothing else. A pattern with \
         no backslash and no `$` reads identically as an ordinary string \
         literal, and the literal keeps its own delimiters, so the string it \
         spells is unchanged."
    }

    fn examples(&self) -> &'static [Example] {
        &[
            Example {
                caption: "The pattern is a plain substring, matched through PCRE:",
                source: "if occursin(r\"error\", line)\n    push!(failures, line)\nend\n",
            },
            Example {
                caption: "The same search written the other way round, curried:",
                source: "failures = filter(contains(r\"error\"), lines)\n",
            },
            Example {
                caption: "Every other pattern position reads the same way:",
                source: "fields = split(line, r\"::\")\n",
            },
        ]
    }

    fn interests(&self) -> &'static [SyntaxKind] {
        &[SyntaxKind::CALL_EXPR]
    }

    fn check(&self, el: &SyntaxElement, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let Some(node) = el.as_node() else { return };
        let found: Vec<_> = regex::PatternCall::all(node)
            .into_iter()
            .filter(|found| regex::is_fixed_string(&found.pattern))
            .collect();
        // Every pattern of a multi-pair `replace` shares the one callee, so
        // the namespace gate is asked once.
        let Some(first) = found.first() else { return };
        if !ctx.resolves_to_base(&first.call) {
            return;
        }

        for found in &found {
            let literal = &found.literal;
            let Some(prefix) = literal.prefix() else {
                continue;
            };

            // The prefix-less spelling *is* the literal's own text minus its
            // first character, so quoting it costs no reconstruction.
            let message = match rewrite::inline_text(literal.syntax()) {
                Some(old) => format!(
                    "use the plain string `{}` instead of the regex `{old}`, \
                     whose pattern has no metacharacter",
                    &old[prefix.text().len()..]
                ),
                None => "use a plain string instead of this regex, whose pattern has no \
                         metacharacter"
                    .to_string(),
            };

            let mut diag = Diagnostic::new(self.id(), literal.syntax().text_range(), message);
            diag.fixes.push(Fix {
                description: "Drop the `r` prefix and match the substring".to_string(),
                content: String::new(),
                start: prefix.text_range().start().into(),
                end: prefix.text_range().end().into(),
                applicability: Applicability::Safe,
            });
            sink.push(diag);
        }
    }
}
