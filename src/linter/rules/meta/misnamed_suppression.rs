//! `misnamed-suppression`: a `# fatou-ignore` directive naming a rule ID the
//! linter does not ship.
//!
//! Such a directive suppresses nothing — the lookup is by exact ID — so the
//! finding the author meant to hide is still reported while the comment claims
//! otherwise. A rename, a typo, a `_` where the ID uses `-`, and a rule ID
//! borrowed from another tool all land here.
//!
//! The fix rewrites the ID to the single nearest shipped rule, and only when
//! that rule is *unambiguous*: an edit distance of at most two, and no other
//! rule tied at the same distance. Comparison is on a normalized spelling
//! (lowercased, `_` folded to `-`), so `unused_binding` is distance zero from
//! `unused-binding` and gets rewritten. The rewrite touches the ID's own range
//! and nothing else, leaving the author's reason prose intact.
//!
//! One shape reports without a fix even when it has a near match: a written ID
//! containing a comma. `# fatou-ignore a, b` parses as the single bogus rule
//! `a,` (the ID scan stops at whitespace), so the obvious rewrite would silently
//! drop `b` — a directive names exactly one rule, and saying so is more useful
//! than half-applying the author's intent.

use crate::linter::diagnostic::{Applicability, Diagnostic, Fix};
use crate::linter::rules::{Example, Rule, RuleContext, all_rule_ids, is_shipped_rule};

/// The largest edit distance still counted as "the author meant this rule".
const MAX_DISTANCE: usize = 2;

pub struct MisnamedSuppression;

impl Rule for MisnamedSuppression {
    fn id(&self) -> &'static str {
        "misnamed-suppression"
    }

    fn description(&self) -> &'static str {
        "Flag a `# fatou-ignore` directive that names a rule the linter does not \
         ship. Suppression matches a rule by its exact ID, so a misspelled, \
         renamed, or foreign ID silences nothing while reading as though it \
         does. When exactly one shipped rule is a near match, the finding \
         carries a safe fix that rewrites the ID and leaves the reason \
         untouched."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "A directive naming a rule that does not exist:",
            source: "# fatou-ignore unused-bindings: set up by the C library\nfunction f()\n    handle = open_device()\n    1\nend\n",
        }]
    }

    fn check_file(&self, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        for directive in ctx.suppressions.directives() {
            let Some(rule) = &directive.rule else {
                // No rule named at all — `blanket-suppression`'s finding.
                continue;
            };
            if is_shipped_rule(&rule.id) {
                continue;
            }

            // A rule list is a shape, not a typo: rewriting to the first rule
            // would drop the rest.
            let listed = rule.id.contains(',');
            let near = (!listed).then(|| near_match(&rule.id)).flatten();

            let mut diag = Diagnostic::new(
                self.id(),
                rule.range,
                format!("`{}` is not a fatou rule; this suppresses nothing", rule.id),
            );
            // With a near match the fix's own description names the rule, so
            // the hint is for the shapes that carry no fix.
            if near.is_none() {
                diag.message = diag.message.with_suggestion(if listed {
                    "a directive names one rule; repeat the comment for a second"
                } else {
                    "check the rule reference for the shipped rule IDs"
                });
            }
            if let Some(id) = near {
                diag.fixes.push(Fix {
                    description: format!("Replace with `{id}`"),
                    content: id.to_string(),
                    start: rule.range.start().into(),
                    end: rule.range.end().into(),
                    applicability: Applicability::Safe,
                });
            }
            sink.push(diag);
        }
    }
}

/// The one shipped rule `written` plausibly meant, or `None` when nothing is
/// close enough or two rules are equally close.
fn near_match(written: &str) -> Option<&'static str> {
    nearest(&normalize(written), &all_rule_ids())
}

/// The single closest candidate to `needle`, within [`MAX_DISTANCE`] and with
/// no runner-up at the same distance.
fn nearest(needle: &str, candidates: &[&'static str]) -> Option<&'static str> {
    let mut best: Option<(usize, &'static str)> = None;
    let mut tied = false;
    for id in candidates.iter().copied() {
        let distance = edit_distance(needle, id);
        match best {
            Some((best_distance, _)) if distance > best_distance => continue,
            Some((best_distance, _)) if distance == best_distance => tied = true,
            _ => {
                best = Some((distance, id));
                tied = false;
            }
        }
    }
    match best {
        Some((distance, id)) if !tied && distance <= MAX_DISTANCE => Some(id),
        _ => None,
    }
}

/// Rule IDs are lowercase kebab-case; compare on that spelling so a `_` or a
/// capital is a match rather than an edit.
fn normalize(written: &str) -> String {
    written
        .chars()
        .map(|c| {
            if c == '_' {
                '-'
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect()
}

/// Levenshtein distance, two rows of the DP table.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0usize; b.len() + 1];
    for (i, ac) in a.chars().enumerate() {
        current[0] = i + 1;
        for (j, bc) in b.iter().enumerate() {
            let substitution = prev[j] + usize::from(ac != *bc);
            current[j + 1] = substitution.min(prev[j + 1] + 1).min(current[j] + 1);
        }
        std::mem::swap(&mut prev, &mut current);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_distance_counts_single_edits() {
        assert_eq!(edit_distance("abc", "abc"), 0);
        assert_eq!(edit_distance("abc", "abd"), 1);
        assert_eq!(edit_distance("abc", "ab"), 1);
        assert_eq!(edit_distance("abc", "abcd"), 1);
        assert_eq!(edit_distance("", "abc"), 3);
    }

    #[test]
    fn near_match_finds_the_obvious_typo() {
        assert_eq!(near_match("unused-bindings"), Some("unused-binding"));
        assert_eq!(near_match("unused_binding"), Some("unused-binding"));
        assert_eq!(near_match("Unused-Binding"), Some("unused-binding"));
    }

    #[test]
    fn near_match_declines_a_distant_name() {
        assert_eq!(near_match("banana"), None);
        assert_eq!(near_match("no-such-rule-at-all"), None);
    }

    #[test]
    fn nearest_declines_a_tie() {
        // Two candidates one edit away: nothing says which was meant.
        assert_eq!(nearest("ab", &["abc", "abd"]), None);
        assert_eq!(nearest("ab", &["abc", "zzzz"]), Some("abc"));
    }
}
