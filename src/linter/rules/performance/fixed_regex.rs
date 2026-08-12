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
//! **The fix is `Safe`, and it deletes exactly one token: the `r` prefix.**
//! What makes that enough is what the rule already required — a fixed string
//! carries no backslash and no `$`, the two characters an ordinary literal
//! reads differently from a non-standard one, and the literal keeps its own
//! delimiters and content bytes, so `r"""a"b"""` becomes `"""a"b"""` with the
//! embedded quote intact. Nothing is discarded, so no comment can be dropped
//! and no name is spliced in that would need confirming.
//!
//! Only `occursin`'s two-argument form is matched, and only with the callee
//! confirmed to be Base's. The curried `occursin(haystack)` fixes the
//! *haystack* rather than the needle, and the other regex consumers
//! (`replace`, `split`, `startswith`) are a wider question than this rule
//! answers.

use crate::ast::{AstNode, Expr};
use crate::linter::diagnostic::{Applicability, Diagnostic, Fix};
use crate::linter::rules::{Example, Rule, RuleContext, matchers, regex, rewrite};
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
         The pattern must be a plain `r\"...\"` — no flag suffix, since a flag \
         changes what the pattern means, and no interpolation — and it must be \
         non-empty and free of every PCRE metacharacter and backslash escape. \
         `occursin` must be confirmed to be Base's, so a local shadow, a \
         qualified `Base.occursin`, or a file whose imports cannot be resolved \
         reports nothing.\n\n\
         The safe fix deletes the `r` prefix and nothing else. A pattern with \
         no backslash and no `$` reads identically as an ordinary string \
         literal, and the literal keeps its own delimiters, so the string it \
         spells is unchanged."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "The pattern is a plain substring, matched through PCRE:",
            source: "if occursin(r\"error\", line)\n    push!(failures, line)\nend\n",
        }]
    }

    fn interests(&self) -> &'static [SyntaxKind] {
        &[SyntaxKind::CALL_EXPR]
    }

    fn check(&self, el: &SyntaxElement, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let Some(node) = el.as_node() else { return };
        let Some((call, args)) = matchers::plain_call(node, "occursin", 2) else {
            return;
        };
        let Some(Expr::StringLiteral(literal)) = args.first() else {
            return;
        };
        let Some(pattern) = regex::regex_pattern(literal) else {
            return;
        };
        if !regex::is_fixed_string(&pattern) {
            return;
        }
        if !ctx.resolves_to_base(&call) {
            return;
        }
        let Some(prefix) = literal.prefix() else {
            return;
        };

        // The prefix-less spelling *is* the literal's own text minus its first
        // character, so quoting it costs no reconstruction.
        let message = match rewrite::inline_text(literal.syntax()) {
            Some(old) => format!(
                "search for the substring `{}` instead of matching the regex \
                 `{old}`, whose pattern has no metacharacter",
                &old[prefix.text().len()..]
            ),
            None => "search for a plain substring instead of matching this regex, whose \
                     pattern has no metacharacter"
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
