//! Comment-based suppression: `# fatou-ignore` directives.
//!
//! Three forms are recognized:
//!
//! ```text
//! # fatou-ignore <rule>: <reason>           # suppresses on the next non-trivia sibling
//! # fatou-ignore-file <rule>: <reason>      # suppresses <rule> anywhere in the file
//! # fatou-ignore-file: <reason>             # suppresses ALL rules
//! ```
//!
//! The `: <reason>` is optional everywhere; a bare `# fatou-ignore-file` also
//! suppresses every rule. Directives are recognized in line comments only —
//! a `#= =#` block comment is never a directive.
//!
//! Implementation note: the comment-to-node attachment for a node-level
//! suppression is "next non-trivia sibling", computed from the CST. That makes
//! matching a range-containment check (a directive before a `function` covers
//! the whole body) and keeps a `#` inside a string literal from parsing as a
//! directive, which the old line-based text scan could not.
//!
//! Every recognized directive is also recorded in [`SuppressionMap::directives`]
//! — *including* the ones that suppress nothing (an unknown rule ID, a directive
//! with no following sibling, one that names no rule at all). Those are exactly
//! what the future `meta/*-suppression` rules exist to report, and they reach a
//! rule through `RuleContext::suppressions`.
//!
//! [`SuppressionMap::filter`] additionally reports which directives actually
//! fired. That is a *driver* fact — it does not exist until the findings have
//! been filtered — which is why `outdated-suppression` is a post-pass
//! (`Rule::check_suppressions`) rather than an ordinary `check_file` rule.

use std::collections::HashMap;

use rowan::{NodeOrToken, TextRange, TextSize};

use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

use super::diagnostic::Diagnostic;

const NODE_PREFIX: &str = "fatou-ignore";
const FILE_PREFIX: &str = "fatou-ignore-file";

/// Which of the three directive forms a comment is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectiveKind {
    /// `# fatou-ignore <rule>: …` — the next non-trivia sibling only.
    Node,
    /// `# fatou-ignore-file <rule>: …` — the whole file, one rule.
    File,
    /// `# fatou-ignore-file: …` (or bare) — the whole file, every rule.
    FileAll,
}

/// A rule ID as written in a directive, with the byte range it occupies. The
/// range is what `misnamed-suppression` reports and rewrites, so a fix touches
/// the ID alone and leaves the author's reason prose intact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleRef {
    pub id: String,
    pub range: TextRange,
}

/// One parsed `# fatou-ignore…` comment.
#[derive(Debug, Clone)]
pub struct Directive {
    pub kind: DirectiveKind,
    /// The rule as written. `None` for [`DirectiveKind::FileAll`] and for a bare
    /// `# fatou-ignore` that names no rule — both recorded, the latter inert,
    /// both grist for `blanket-suppression`.
    pub rule: Option<RuleRef>,
    /// The text after the `:` that follows the rule ID, trimmed. `None` when
    /// there is no `:` at all, or nothing but whitespace follows it.
    pub reason: Option<String>,
    /// The `COMMENT` token's own range — the span a meta rule reports on.
    pub comment: TextRange,
    /// The node a [`DirectiveKind::Node`] directive attaches to. `None` when no
    /// non-trivia sibling follows, i.e. the directive can never match anything.
    /// Always `None` for the file-scope forms, which need no target.
    pub target: Option<TextRange>,
    /// The comment's text, verbatim.
    pub raw: String,
}

impl Directive {
    /// Whether the author wrote a reason for the suppression.
    pub fn has_reason(&self) -> bool {
        self.reason.is_some()
    }

    /// Whether this directive can never suppress anything because nothing
    /// follows it — dead regardless of which rules ran.
    pub fn is_dangling(&self) -> bool {
        self.kind == DirectiveKind::Node && self.target.is_none()
    }
}

/// Which directives suppressed at least one finding, parallel to
/// [`SuppressionMap::directives`].
#[derive(Debug, Clone, Default)]
pub struct DirectiveUsage(Vec<bool>);

impl DirectiveUsage {
    /// Whether the directive at `index` suppressed at least one diagnostic.
    pub fn is_used(&self, index: usize) -> bool {
        self.0.get(index).copied().unwrap_or(false)
    }
}

#[derive(Debug, Clone, Default)]
pub struct SuppressionMap {
    /// Every recognized directive, in source order.
    directives: Vec<Directive>,
    /// Indices of the [`DirectiveKind::FileAll`] directives.
    file_all: Vec<usize>,
    /// Rule ID -> indices of the file-wide directives naming it.
    file_rules: HashMap<String, Vec<usize>>,
    /// Rule ID -> (attached node range, directive index). A diagnostic is
    /// suppressed if its range falls fully inside one of those ranges.
    node_skips: HashMap<String, Vec<(TextRange, usize)>>,
}

impl SuppressionMap {
    pub fn build(root: &SyntaxNode) -> Self {
        let mut map = Self::default();
        for el in root.descendants_with_tokens() {
            if let NodeOrToken::Token(tok) = el
                && tok.kind() == SyntaxKind::COMMENT
            {
                classify_comment(&tok, &mut map);
            }
        }
        map
    }

    /// Every recognized directive in the file, in source order.
    pub fn directives(&self) -> &[Directive] {
        &self.directives
    }

    pub fn is_suppressed(&self, rule: &str, range: TextRange) -> bool {
        self.file_all.iter().any(|&i| self.applies(i, range))
            || self
                .file_rules
                .get(rule)
                .is_some_and(|ix| ix.iter().any(|&i| self.applies(i, range)))
            || self.node_skips.get(rule).is_some_and(|ranges| {
                ranges
                    .iter()
                    .any(|&(r, i)| r.contains_range(range) && self.applies(i, range))
            })
    }

    /// Drop the suppressed diagnostics, reporting which directives fired.
    ///
    /// *Every* directive covering a finding is marked used, not just the first:
    /// a file-wide and a node directive for the same rule are both doing their
    /// job, and marking only one would leave the other looking outdated.
    ///
    /// Idempotent — a second call over already-filtered diagnostics removes
    /// nothing further, which is what lets the driver re-filter after appending
    /// its post-pass findings.
    pub fn filter(&self, diagnostics: &mut Vec<Diagnostic>) -> DirectiveUsage {
        let mut used = vec![false; self.directives.len()];
        let mut hits = Vec::new();
        diagnostics.retain(|d| {
            hits.clear();
            self.matches(d.rule, d.range, &mut hits);
            for &i in &hits {
                used[i] = true;
            }
            hits.is_empty()
        });
        DirectiveUsage(used)
    }

    /// Collect the indices of every directive that suppresses `(rule, range)`.
    fn matches(&self, rule: &str, range: TextRange, out: &mut Vec<usize>) {
        out.extend(
            self.file_all
                .iter()
                .copied()
                .filter(|&i| self.applies(i, range)),
        );
        if let Some(indices) = self.file_rules.get(rule) {
            out.extend(indices.iter().copied().filter(|&i| self.applies(i, range)));
        }
        if let Some(ranges) = self.node_skips.get(rule) {
            out.extend(
                ranges
                    .iter()
                    .filter(|&&(r, i)| r.contains_range(range) && self.applies(i, range))
                    .map(|&(_, i)| i),
            );
        }
    }

    /// A directive never suppresses a finding that lies inside its own comment.
    ///
    /// Without this, `# fatou-ignore-file: …` would suppress the
    /// `blanket-suppression` finding *about itself*, making that rule
    /// structurally unreportable in the one case it exists for. It stays inert
    /// for every non-`meta` rule: no other rule's finding is ever spanned on a
    /// suppression comment.
    fn applies(&self, index: usize, range: TextRange) -> bool {
        !self.directives[index].comment.contains_range(range)
    }
}

fn is_trivia(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::WHITESPACE
            | SyntaxKind::NEWLINE
            | SyntaxKind::COMMENT
            | SyntaxKind::BLOCK_COMMENT
    )
}

fn classify_comment(tok: &SyntaxToken, map: &mut SuppressionMap) {
    let text = tok.text();
    let base = tok.text_range().start();
    // Byte offset of the comment body within the token, so a `RuleRef` range is
    // absolute in the file.
    let Some(body_start) = text.find('#').map(|i| i + 1) else {
        return;
    };
    let body = text[body_start..].trim_start();
    let body_offset = body_start + (text.len() - body_start - body.len());

    if let Some(rest) = body.strip_prefix(FILE_PREFIX) {
        let rest_offset = body_offset + FILE_PREFIX.len();
        let (rest, rest_offset) = trim_start_at(rest, rest_offset);
        // Bare `# fatou-ignore-file` suppresses everything, reason-less.
        if rest.is_empty() {
            record(
                map,
                Directive {
                    kind: DirectiveKind::FileAll,
                    rule: None,
                    reason: None,
                    comment: tok.text_range(),
                    target: None,
                    raw: text.to_string(),
                },
            );
            return;
        }
        if let Some(reason) = rest.strip_prefix(':') {
            record(
                map,
                Directive {
                    kind: DirectiveKind::FileAll,
                    rule: None,
                    reason: clean_reason(reason),
                    comment: tok.text_range(),
                    target: None,
                    raw: text.to_string(),
                },
            );
            return;
        }
        // `fatou-ignore-file <rule>: reason` — rest starts with the rule ID.
        record(
            map,
            Directive {
                kind: DirectiveKind::File,
                rule: parse_rule(rest, rest_offset, base),
                reason: parse_reason(rest),
                comment: tok.text_range(),
                target: None,
                raw: text.to_string(),
            },
        );
        return;
    }

    if let Some(rest) = body.strip_prefix(NODE_PREFIX) {
        let rest_offset = body_offset + NODE_PREFIX.len();
        let (rest, rest_offset) = trim_start_at(rest, rest_offset);
        record(
            map,
            Directive {
                kind: DirectiveKind::Node,
                rule: parse_rule(rest, rest_offset, base),
                reason: parse_reason(rest),
                comment: tok.text_range(),
                target: next_meaningful_sibling(tok),
                raw: text.to_string(),
            },
        );
    }
}

/// Record a directive and index it so `is_suppressed` stays a map lookup.
/// Directives that cannot match anything (a node form with no rule, or with no
/// following sibling) live only in `directives`.
fn record(map: &mut SuppressionMap, directive: Directive) {
    let index = map.directives.len();
    match (&directive.kind, &directive.rule, &directive.target) {
        (DirectiveKind::FileAll, _, _) => map.file_all.push(index),
        (DirectiveKind::File, Some(rule), _) => {
            map.file_rules
                .entry(rule.id.clone())
                .or_default()
                .push(index);
        }
        (DirectiveKind::Node, Some(rule), Some(target)) => {
            map.node_skips
                .entry(rule.id.clone())
                .or_default()
                .push((*target, index));
        }
        _ => {}
    }
    map.directives.push(directive);
}

/// `str::trim_start`, carrying the byte offset along.
fn trim_start_at(s: &str, offset: usize) -> (&str, usize) {
    let trimmed = s.trim_start();
    (trimmed, offset + (s.len() - trimmed.len()))
}

/// Parse the rule ID at the head of `rest`, which starts at byte `offset`
/// within the comment token that begins at `base`.
fn parse_rule(rest: &str, offset: usize, base: TextSize) -> Option<RuleRef> {
    // Expect `<rule>: ...` or just `<rule>` (lone trailing whitespace).
    let end = rest
        .find(|c: char| c == ':' || c.is_whitespace())
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    let start = base + TextSize::from(offset as u32);
    Some(RuleRef {
        id: rest[..end].to_string(),
        range: TextRange::at(start, TextSize::from(end as u32)),
    })
}

/// The reason is everything after the first `:`, trimmed.
fn parse_reason(rest: &str) -> Option<String> {
    rest.split_once(':')
        .and_then(|(_, reason)| clean_reason(reason))
}

fn clean_reason(reason: &str) -> Option<String> {
    let trimmed = reason.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// The next non-trivia sibling after `tok`, expanding outward when the
/// enclosing node has nothing after the comment — e.g. a comment between
/// top-level statements lives under the root, and the next sibling is the next
/// top-level expression.
fn next_meaningful_sibling(tok: &SyntaxToken) -> Option<TextRange> {
    let mut current_token = tok.clone();
    loop {
        let parent = current_token.parent()?;
        let mut found = None;
        let mut past_self = false;
        for el in parent.children_with_tokens() {
            match &el {
                NodeOrToken::Token(t) if *t == current_token => {
                    past_self = true;
                    continue;
                }
                _ => {}
            }
            if !past_self {
                continue;
            }
            match &el {
                NodeOrToken::Token(t) if is_trivia(t.kind()) => continue,
                NodeOrToken::Node(child) => {
                    found = Some(child.text_range());
                    break;
                }
                NodeOrToken::Token(t) => {
                    found = Some(t.text_range());
                    break;
                }
            }
        }
        if let Some(range) = found {
            return Some(range);
        }
        // No sibling after this token in `parent`. Bubble up: look for the
        // next non-trivia sibling of `parent` itself.
        let parent_node = parent.clone();
        let grand = parent_node.parent()?;
        let mut past_parent = false;
        for el in grand.children_with_tokens() {
            match &el {
                NodeOrToken::Node(n) if *n == parent_node => {
                    past_parent = true;
                    continue;
                }
                _ => {}
            }
            if !past_parent {
                continue;
            }
            match &el {
                NodeOrToken::Token(t) if is_trivia(t.kind()) => continue,
                NodeOrToken::Node(child) => return Some(child.text_range()),
                NodeOrToken::Token(t) => return Some(t.text_range()),
            }
        }
        // Try one level higher.
        current_token = grand.first_token()?;
        // Prevent infinite loops.
        if grand == parent {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    fn map_of(src: &str) -> SuppressionMap {
        let parsed = parse(src);
        assert!(
            parsed.diagnostics.is_empty(),
            "test source must parse cleanly: {:?}",
            parsed.diagnostics
        );
        SuppressionMap::build(&parsed.cst)
    }

    fn only(map: &SuppressionMap) -> &Directive {
        match map.directives() {
            [d] => d,
            other => panic!("expected exactly one directive, got {other:?}"),
        }
    }

    /// The range of `needle`'s first occurrence in `src`.
    fn range_of(src: &str, needle: &str) -> TextRange {
        let start = src.find(needle).expect("needle in src");
        TextRange::at((start as u32).into(), (needle.len() as u32).into())
    }

    fn diag(rule: &'static str, range: TextRange) -> Diagnostic {
        Diagnostic::new(rule, range, "test")
    }

    #[test]
    fn file_all_suppresses_everything() {
        let src = "# fatou-ignore-file: noisy\nx = 1\n";
        let m = map_of(src);
        assert!(m.is_suppressed("anything", range_of(src, "x = 1")));
    }

    #[test]
    fn bare_file_directive_suppresses_everything() {
        let src = "# fatou-ignore-file\nx = 1\n";
        let m = map_of(src);
        assert!(m.is_suppressed("anything", range_of(src, "x = 1")));
        let d = only(&m);
        assert_eq!(d.kind, DirectiveKind::FileAll);
        assert_eq!(d.rule, None);
        assert_eq!(d.reason, None);
    }

    #[test]
    fn file_rule_suppresses_only_that_rule() {
        let src = "# fatou-ignore-file unused-binding: temp\nx = 1\n";
        let m = map_of(src);
        assert!(m.is_suppressed("unused-binding", range_of(src, "x = 1")));
        assert!(!m.is_suppressed("undefined-name", range_of(src, "x = 1")));
    }

    #[test]
    fn node_suppression_attaches_to_next_sibling() {
        let src = "# fatou-ignore unused-binding: temp\nx = 1\n";
        let m = map_of(src);
        assert!(m.is_suppressed("unused-binding", range_of(src, "x = 1")));
    }

    #[test]
    fn node_suppression_does_not_leak_to_following_statements() {
        let src = "# fatou-ignore unused-binding: only first\nx = 1\ny = 2\n";
        let m = map_of(src);
        assert!(m.is_suppressed("unused-binding", range_of(src, "x = 1")));
        assert!(!m.is_suppressed("unused-binding", range_of(src, "y = 2")));
    }

    #[test]
    fn node_suppression_covers_the_whole_next_node() {
        // The scope-widening pin: a directive before a multi-line construct
        // covers findings anywhere inside it, not just the first line.
        let src = "# fatou-ignore unused-binding: whole function\nfunction f()\n    tmp = 1\n    return 2\nend\n";
        let m = map_of(src);
        assert!(m.is_suppressed("unused-binding", range_of(src, "tmp")));
    }

    #[test]
    fn directive_records_rule_reason_and_comment_range() {
        let src = "# fatou-ignore unused-binding: still needed\nx = 1\n";
        let m = map_of(src);
        let d = only(&m);
        assert_eq!(d.kind, DirectiveKind::Node);
        assert_eq!(
            d.rule.as_ref().map(|r| r.id.as_str()),
            Some("unused-binding")
        );
        assert_eq!(d.reason.as_deref(), Some("still needed"));
        assert_eq!(
            d.comment,
            range_of(src, "# fatou-ignore unused-binding: still needed")
        );
        assert_eq!(d.raw, "# fatou-ignore unused-binding: still needed");
        assert!(d.target.is_some());
        assert!(!d.is_dangling());
    }

    #[test]
    fn rule_ref_range_spans_exactly_the_written_id() {
        let src = "# fatou-ignore unused-binding: r\nx = 1\n";
        let m = map_of(src);
        let rule = only(&m).rule.clone().expect("a rule ref");
        assert_eq!(&src[rule.range], "unused-binding");
    }

    #[test]
    fn rule_ref_range_is_absolute_for_an_indented_file_directive() {
        let src = "function f()\n    # fatou-ignore-file discouraged-function: r\n    1\nend\n";
        let m = map_of(src);
        let rule = only(&m).rule.clone().expect("a rule ref");
        assert_eq!(&src[rule.range], "discouraged-function");
    }

    #[test]
    fn directive_without_colon_has_no_reason() {
        let m = map_of("# fatou-ignore unused-binding\nx = 1\n");
        assert!(!only(&m).has_reason());
    }

    #[test]
    fn directive_with_empty_reason_has_no_reason() {
        let m = map_of("# fatou-ignore unused-binding:   \nx = 1\n");
        assert!(!only(&m).has_reason());
    }

    #[test]
    fn blanket_file_directive_has_no_rule_but_keeps_its_reason() {
        let m = map_of("# fatou-ignore-file: generated code\nx = 1\n");
        let d = only(&m);
        assert_eq!(d.kind, DirectiveKind::FileAll);
        assert_eq!(d.rule, None);
        assert_eq!(d.reason.as_deref(), Some("generated code"));
        assert_eq!(d.target, None);
    }

    #[test]
    fn scoped_file_directive_keeps_rule_and_reason() {
        let m = map_of("# fatou-ignore-file unused-binding: temp\nx = 1\n");
        let d = only(&m);
        assert_eq!(d.kind, DirectiveKind::File);
        assert_eq!(
            d.rule.as_ref().map(|r| r.id.as_str()),
            Some("unused-binding")
        );
        assert_eq!(d.reason.as_deref(), Some("temp"));
    }

    #[test]
    fn bare_directive_naming_no_rule_is_recorded() {
        // Suppresses nothing today, and did so silently before it was recorded.
        let m = map_of("# fatou-ignore\nx = 1\n");
        let d = only(&m);
        assert_eq!(d.kind, DirectiveKind::Node);
        assert_eq!(d.rule, None);
    }

    #[test]
    fn node_directive_with_nothing_after_it_is_still_recorded() {
        let m = map_of("x = 1\n# fatou-ignore unused-binding: dangling\n");
        let d = only(&m);
        assert_eq!(d.kind, DirectiveKind::Node);
        assert_eq!(d.target, None);
        assert!(d.is_dangling());
    }

    #[test]
    fn unknown_rule_id_is_recorded() {
        let m = map_of("# fatou-ignore not-a-rule: r\nx = 1\n");
        assert_eq!(
            only(&m).rule.as_ref().map(|r| r.id.as_str()),
            Some("not-a-rule")
        );
    }

    #[test]
    fn comma_list_yields_a_single_bogus_rule() {
        // Pins the existing parse: `parse_rule` stops at whitespace, so the
        // comma rides along and the directive silently suppresses nothing.
        // `misnamed-suppression` is what makes this audible.
        let m = map_of("# fatou-ignore unused-binding, undefined-name: r\nx = 1\n");
        assert_eq!(
            only(&m).rule.as_ref().map(|r| r.id.as_str()),
            Some("unused-binding,")
        );
    }

    #[test]
    fn non_directive_comments_are_not_recorded() {
        let m = map_of("# just a comment\n## a doc-ish comment\nx = 1\n");
        assert!(m.directives().is_empty());
    }

    #[test]
    fn block_comments_are_not_directives() {
        let m = map_of("#= fatou-ignore-file =#\nx = 1\n");
        assert!(m.directives().is_empty());
        assert!(!m.is_suppressed(
            "anything",
            range_of("#= fatou-ignore-file =#\nx = 1\n", "x = 1")
        ));
    }

    #[test]
    fn a_hash_inside_a_string_is_not_a_directive() {
        let m = map_of("x = \"# fatou-ignore-file\"\ny = 1\n");
        assert!(m.directives().is_empty());
    }

    #[test]
    fn filter_reports_which_directives_fired() {
        let src =
            "# fatou-ignore unused-binding: used\n# fatou-ignore undefined-name: unused\nx = 1\n";
        let m = map_of(src);
        let target = range_of(src, "x = 1");
        let mut diagnostics = vec![
            diag("unused-binding", target),
            diag("index-from-length", target),
        ];
        let usage = m.filter(&mut diagnostics);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].rule, "index-from-length");
        assert!(usage.is_used(0));
        assert!(!usage.is_used(1));
    }

    #[test]
    fn filter_marks_every_directive_that_covers_a_finding() {
        let src = "# fatou-ignore-file unused-binding: broad\n# fatou-ignore unused-binding: narrow\nx = 1\n";
        let m = map_of(src);
        let target = range_of(src, "x = 1");
        let mut diagnostics = vec![diag("unused-binding", target)];
        let usage = m.filter(&mut diagnostics);
        assert!(diagnostics.is_empty());
        assert!(usage.is_used(0), "the file-wide directive fired");
        assert!(usage.is_used(1), "the node directive covers it too");
    }

    #[test]
    fn directive_never_suppresses_a_finding_inside_itself() {
        // Otherwise `blanket-suppression` could never report the very shape it
        // exists for.
        let src = "# fatou-ignore-file: shush\nx = 1\n";
        let m = map_of(src);
        let own = only(&m).comment;
        assert!(!m.is_suppressed("blanket-suppression", own));
        // …but it still suppresses everything else in the file.
        assert!(m.is_suppressed("unused-binding", range_of(src, "x = 1")));
    }
}
