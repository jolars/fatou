//! `outdated-suppression`: a `# fatou-ignore` directive that suppressed
//! nothing.
//!
//! Two ways a directive ends up dead. It can be *stale* — the rule it names ran
//! over the file and reported nothing it covers, so the finding it was written
//! for is gone (fixed, refactored away, or narrowed out of the rule). Or it can
//! be *dangling* — a node-level directive with no following sibling, which can
//! never match anything no matter which rules run. Both leave a comment that
//! reads like a live exemption and silences nothing, and both are fixed by
//! deleting it.
//!
//! This is the one rule that has to run *after* suppression filtering: "which
//! directives fired" is a driver fact that does not exist until the findings
//! have been filtered, which is what [`Rule::check_suppressions`] exists for.
//!
//! Silence is only evidence when the rule was in a position to speak, so a
//! directive is judged stale only when all of the following hold:
//!
//! - it names a **shipped** rule (an unknown ID is `misnamed-suppression`'s
//!   finding, and a directive naming no rule at all is
//!   `blanket-suppression`'s);
//! - that rule is in this run's [`EnabledRules`](crate::linter::rules::EnabledRules) —
//!   a rule `select`/`ignore` left out is *dormant*, not stale, and would
//!   otherwise make every directive in the file look dead;
//! - the rule is not structurally silent for this file (see
//!   [`verdict_is_trustworthy`]): a file whose names fatou cannot resolve,
//!   `julia-version-compat` without a declared target, and the include-graph
//!   rules without an include graph all report nothing for reasons that have
//!   nothing to do with the directive.
//!
//! A dangling directive skips all three: nothing follows it, so no rule's
//! verdict is involved.
//!
//! The fix deletes the directive, and does so by construction rather than by
//! reformatting: a comment on its own line takes its indentation and its line
//! break with it, while a comment trailing code takes only the gap before it, so
//! the code keeps its line. Deleting a comment can neither change what parses
//! nor drop anything but the comment itself, which is what makes it safe.

use rowan::{TextRange, TextSize};

use crate::linter::diagnostic::{Applicability, Diagnostic, Fix};
use crate::linter::rules::{Example, Rule, RuleContext, is_shipped_rule};
use crate::linter::suppression::DirectiveUsage;
use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

pub struct OutdatedSuppression;

impl Rule for OutdatedSuppression {
    fn id(&self) -> &'static str {
        "outdated-suppression"
    }

    fn description(&self) -> &'static str {
        "Flag a `# fatou-ignore` directive that suppressed nothing: either the \
         rule it names ran and reported nothing it covers, or the directive has \
         no code after it to apply to. A rule that this run did not enable, and \
         any rule in a file whose names cannot be resolved (one that `eval`s, \
         or `using`s a module the run did not harvest), is dormant rather than \
         stale and is never reported. The safe fix deletes the directive."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "A directive at the end of a file, with nothing left to suppress:",
            source: "function f(x)\n    x + 1\nend\n# fatou-ignore unused-binding: the scratch value below\n",
        }]
    }

    fn check_suppressions(
        &self,
        ctx: &RuleContext<'_>,
        used: &DirectiveUsage,
        sink: &mut Vec<Diagnostic>,
    ) {
        for (index, directive) in ctx.suppressions.directives().iter().enumerate() {
            // A directive naming no rule, or naming one the linter does not
            // ship, belongs to `blanket-suppression`/`misnamed-suppression`.
            let Some(rule) = &directive.rule else {
                continue;
            };
            if !is_shipped_rule(&rule.id) {
                continue;
            }

            let message = if directive.is_dangling() {
                "suppression has nothing after it to apply to".to_string()
            } else {
                if used.is_used(index)
                    || !ctx.enabled_rules.contains(&rule.id)
                    || !verdict_is_trustworthy(&rule.id, ctx)
                {
                    continue;
                }
                format!(
                    "`{}` reports nothing here; this suppression is no longer needed",
                    rule.id
                )
            };

            let mut diag = Diagnostic::new(self.id(), directive.comment, message);
            diag.message = diag.message.with_suggestion("delete the directive");
            if let Some(span) = deletion_span(ctx.root, directive.comment) {
                diag.fixes.push(Fix {
                    description: "Delete the suppression".to_string(),
                    content: String::new(),
                    start: span.start().into(),
                    end: span.end().into(),
                    applicability: Applicability::Safe,
                });
            }
            sink.push(diag);
        }
    }
}

/// Whether `rule` reporting nothing for this file is evidence about the
/// directive rather than about the context the rule was handed.
///
/// Each arm mirrors a bail-out in the rules themselves, so a rule that answers
/// "not enough context to say" is never read as "nothing to say".
fn verdict_is_trustworthy(rule: &str, ctx: &RuleContext<'_>) -> bool {
    // The floor, and deliberately one question rather than a list of rules:
    // *any* rule may open by asking what a name means
    // (`RuleContext::resolves_to_base` is the standard first move for an idiom
    // rule), and every one of them goes quiet for a file that `eval`s, carries
    // an unfollowable `include`, or `using`s a module nothing harvested — which
    // is most real files when the CLI lints against the built-in Base/Core
    // snapshot alone. Their silence there says nothing about the directive, so
    // such a file keeps every suppression it has.
    if !ctx.trusts_resolution() {
        return false;
    }
    match rule {
        // The declared dependency set only a package source file carries.
        "unresolved-import" => ctx
            .resolution
            .as_ref()
            .is_some_and(|resolution| resolution.declared_deps.is_some()),
        "julia-version-compat" => ctx.julia_target.is_some(),
        // The include graph is precomputed by the driver; an empty one is
        // equally "no problems" and "no graph was built" (the language server
        // passes none), so no verdict is read out of it.
        "missing-include-file" | "include-cycle" | "duplicate-include" => !ctx.includes.is_empty(),
        _ => true,
    }
}

/// The range to delete to remove the comment spanning `comment`.
///
/// A comment on a line of its own takes the whole line: its leading whitespace
/// and its trailing newline, so no blank line is left behind. A comment
/// trailing code takes the gap before it and stops at the line break, leaving
/// the code untouched.
fn deletion_span(root: &SyntaxNode, comment: TextRange) -> Option<TextRange> {
    let token = comment_token(root, comment)?;
    let indent = token
        .prev_token()
        .filter(|prev| prev.kind() == SyntaxKind::WHITESPACE);
    // `WHITESPACE` is spaces and tabs only — a line break is its own `NEWLINE`
    // token — so what precedes the indentation decides which line this is.
    let before = match &indent {
        Some(ws) => ws.prev_token(),
        None => token.prev_token(),
    };
    let own_line = before.is_none_or(|prev| prev.kind() == SyntaxKind::NEWLINE);

    let start = match &indent {
        Some(ws) => ws.text_range().start(),
        None => comment.start(),
    };
    let end = match token.next_token() {
        Some(next) if own_line && next.kind() == SyntaxKind::NEWLINE => next.text_range().end(),
        _ => comment.end(),
    };
    Some(TextRange::new(start, end))
}

/// The `COMMENT` token occupying exactly `range`.
fn comment_token(root: &SyntaxNode, range: TextRange) -> Option<SyntaxToken> {
    find_comment(root, range.start()).filter(|token| token.text_range() == range)
}

fn find_comment(root: &SyntaxNode, offset: TextSize) -> Option<SyntaxToken> {
    root.token_at_offset(offset)
        .find(|token| token.kind() == SyntaxKind::COMMENT)
}
