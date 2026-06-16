//! Comment-based suppression directives, in the `# fatou-ignore` family:
//!
//! - `# fatou-ignore <rule>` — suppress `<rule>` on the next code line.
//! - `# fatou-ignore-file <rule>` — suppress `<rule>` for the whole file.
//! - `# fatou-ignore-file` — suppress every rule for the whole file.

use std::collections::BTreeSet;

const LINE_PREFIX: &str = "fatou-ignore";
const FILE_PREFIX: &str = "fatou-ignore-file";

#[derive(Debug, Default)]
pub struct SuppressionMap {
    /// Suppress all rules across the whole file.
    file_all: bool,
    /// Rules suppressed across the whole file.
    file_rules: BTreeSet<String>,
    /// `(rule, line)` pairs suppressed on a specific (1-indexed) line.
    line_rules: BTreeSet<(String, usize)>,
}

impl SuppressionMap {
    /// Parse directives out of `text`. A directive comment suppresses the next
    /// code line (the first subsequent non-blank, non-comment line).
    pub fn build(text: &str) -> Self {
        let mut map = SuppressionMap::default();
        let lines: Vec<&str> = text.lines().collect();

        for (idx, line) in lines.iter().enumerate() {
            let Some(directive) = parse_directive(line) else {
                continue;
            };
            match directive {
                Directive::FileAll => map.file_all = true,
                Directive::FileRule(rule) => {
                    map.file_rules.insert(rule);
                }
                Directive::LineRule(rule) => {
                    if let Some(target) = next_code_line(&lines, idx) {
                        map.line_rules.insert((rule, target));
                    }
                }
            }
        }

        map
    }

    /// Whether `rule` is suppressed at (1-indexed) `line`.
    pub fn is_suppressed(&self, rule: &str, line: usize) -> bool {
        self.file_all
            || self.file_rules.contains(rule)
            || self.line_rules.contains(&(rule.to_string(), line))
    }
}

enum Directive {
    FileAll,
    FileRule(String),
    LineRule(String),
}

fn parse_directive(line: &str) -> Option<Directive> {
    let comment = line.trim_start();
    let body = comment.strip_prefix('#')?.trim_start();

    if let Some(rest) = body.strip_prefix(FILE_PREFIX) {
        let rule = rest.trim().trim_start_matches(':').trim();
        return Some(if rule.is_empty() {
            Directive::FileAll
        } else {
            Directive::FileRule(rule.to_string())
        });
    }
    if let Some(rest) = body.strip_prefix(LINE_PREFIX) {
        let rule = rest.trim().trim_start_matches(':').trim();
        if !rule.is_empty() {
            return Some(Directive::LineRule(rule.to_string()));
        }
    }
    None
}

/// The 1-indexed line number of the first code line after `from` (0-indexed).
fn next_code_line(lines: &[&str], from: usize) -> Option<usize> {
    for (idx, line) in lines.iter().enumerate().skip(from + 1) {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        return Some(idx + 1);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_wide_all() {
        let map = SuppressionMap::build("# fatou-ignore-file\nx = 1\n");
        assert!(map.is_suppressed("any-rule", 2));
    }

    #[test]
    fn file_wide_specific_rule() {
        let map = SuppressionMap::build("# fatou-ignore-file unused-binding\nx = 1\n");
        assert!(map.is_suppressed("unused-binding", 2));
        assert!(!map.is_suppressed("other", 2));
    }

    #[test]
    fn next_line_rule() {
        let map = SuppressionMap::build("# fatou-ignore shadowed-builtin\nx = 1\ny = 2\n");
        assert!(map.is_suppressed("shadowed-builtin", 2));
        assert!(!map.is_suppressed("shadowed-builtin", 3));
    }
}
