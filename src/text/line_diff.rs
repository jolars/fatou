//! Deterministic, bounded line diffs shared by CLI and LSP formatting.

use std::collections::HashMap;

use similar::{Algorithm, DiffOp, DiffTag, DiffableStr, TextDiff};

/// The largest unanchored gap sent to an algorithm with quadratic worst-case
/// work. Larger gaps are represented as one replacement.
pub(crate) const MAX_UNANCHORED_LINE_PAIRS: usize = 1_000_000;

/// Indexed operations over line slices from the old and new text.
pub(crate) struct LineDiff<'old, 'new> {
    old_lines: Vec<&'old str>,
    new_lines: Vec<&'new str>,
    ops: Vec<DiffOp>,
}

impl<'old, 'new> LineDiff<'old, 'new> {
    pub(crate) fn old_lines(&self) -> &[&'old str] {
        &self.old_lines
    }

    pub(crate) fn new_lines(&self) -> &[&'new str] {
        &self.new_lines
    }

    pub(crate) fn ops(&self) -> &[DiffOp] {
        &self.ops
    }
}

/// Diff two texts by globally unique common-line anchors, bounding the work in
/// every gap between them. The result always reconstructs both inputs exactly.
///
/// Patience is deliberate for bounded gaps: Histogram is faster on Fatou's
/// ordinary Julia corpus but collapses on repetitive generated Julia, while
/// Myers becomes expensive when formatting changes nearly every line. A
/// deterministic work bound avoids the output instability of a clock deadline.
pub(crate) fn bounded_line_diff<'old, 'new>(
    old: &'old str,
    new: &'new str,
) -> LineDiff<'old, 'new> {
    let old_lines = DiffableStr::tokenize_lines(old);
    let new_lines = DiffableStr::tokenize_lines(new);
    let mut builder = DiffBuilder::default();
    diff_anchored(&old_lines, &new_lines, &mut builder);
    LineDiff {
        old_lines,
        new_lines,
        ops: builder.ops,
    }
}

#[derive(Default)]
struct DiffBuilder {
    old_at: usize,
    new_at: usize,
    ops: Vec<DiffOp>,
}

impl DiffBuilder {
    fn push(&mut self, tag: DiffTag, old_len: usize, new_len: usize) {
        if old_len == 0 && new_len == 0 {
            return;
        }
        let op = match tag {
            DiffTag::Equal => {
                debug_assert_eq!(old_len, new_len);
                DiffOp::Equal {
                    old_index: self.old_at,
                    new_index: self.new_at,
                    len: old_len,
                }
            }
            DiffTag::Delete => {
                debug_assert_eq!(new_len, 0);
                DiffOp::Delete {
                    old_index: self.old_at,
                    old_len,
                    new_index: self.new_at,
                }
            }
            DiffTag::Insert => {
                debug_assert_eq!(old_len, 0);
                DiffOp::Insert {
                    old_index: self.old_at,
                    new_index: self.new_at,
                    new_len,
                }
            }
            DiffTag::Replace => DiffOp::Replace {
                old_index: self.old_at,
                old_len,
                new_index: self.new_at,
                new_len,
            },
        };
        self.old_at += old_len;
        self.new_at += new_len;
        self.push_coalesced(op);
    }

    fn push_coalesced(&mut self, op: DiffOp) {
        match (self.ops.last_mut(), op) {
            (Some(DiffOp::Equal { len, .. }), DiffOp::Equal { len: next_len, .. }) => {
                *len += next_len
            }
            (
                Some(DiffOp::Delete { old_len, .. }),
                DiffOp::Delete {
                    old_len: next_len, ..
                },
            ) => *old_len += next_len,
            (
                Some(DiffOp::Insert { new_len, .. }),
                DiffOp::Insert {
                    new_len: next_len, ..
                },
            ) => *new_len += next_len,
            (
                Some(DiffOp::Replace {
                    old_len, new_len, ..
                }),
                DiffOp::Replace {
                    old_len: next_old_len,
                    new_len: next_new_len,
                    ..
                },
            ) => {
                *old_len += next_old_len;
                *new_len += next_new_len;
            }
            _ => self.ops.push(op),
        }
    }
}

fn diff_anchored(old: &[&str], new: &[&str], builder: &mut DiffBuilder) {
    let prefix = common_prefix_len(old, new);
    builder.push(DiffTag::Equal, prefix, prefix);

    let old = &old[prefix..];
    let new = &new[prefix..];
    let suffix = common_suffix_len(old, new);
    let old_middle = &old[..old.len() - suffix];
    let new_middle = &new[..new.len() - suffix];

    // `similar` sends its unique-line sequences through Myers. Computing the
    // ordered common subset directly removes that quadratic layer.
    let anchors = unique_common_anchors(old_middle, new_middle);
    let (mut old_start, mut new_start) = (0, 0);
    for (old_anchor, new_anchor) in anchors {
        diff_bounded_gap(
            &old_middle[old_start..old_anchor],
            &new_middle[new_start..new_anchor],
            builder,
        );
        builder.push(DiffTag::Equal, 1, 1);
        old_start = old_anchor + 1;
        new_start = new_anchor + 1;
    }
    diff_bounded_gap(&old_middle[old_start..], &new_middle[new_start..], builder);
    builder.push(DiffTag::Equal, suffix, suffix);
}

fn diff_bounded_gap(old: &[&str], new: &[&str], builder: &mut DiffBuilder) {
    let prefix = common_prefix_len(old, new);
    builder.push(DiffTag::Equal, prefix, prefix);

    let old = &old[prefix..];
    let new = &new[prefix..];
    let suffix = common_suffix_len(old, new);
    let old_middle = &old[..old.len() - suffix];
    let new_middle = &new[..new.len() - suffix];

    if old_middle.len().saturating_mul(new_middle.len()) > MAX_UNANCHORED_LINE_PAIRS {
        builder.push(DiffTag::Replace, old_middle.len(), new_middle.len());
    } else {
        let diff = TextDiff::configure()
            .algorithm(Algorithm::Patience)
            .diff_slices(old_middle, new_middle);
        for op in diff.ops() {
            let (tag, old_lines, new_lines) = op.as_tag_tuple();
            builder.push(tag, old_lines.len(), new_lines.len());
        }
    }

    builder.push(DiffTag::Equal, suffix, suffix);
}

fn common_prefix_len(old: &[&str], new: &[&str]) -> usize {
    old.iter()
        .zip(new)
        .take_while(|(old, new)| old == new)
        .count()
}

fn common_suffix_len(old: &[&str], new: &[&str]) -> usize {
    old.iter()
        .rev()
        .zip(new.iter().rev())
        .take_while(|(old, new)| old == new)
        .count()
}

fn unique_common_anchors(old: &[&str], new: &[&str]) -> Vec<(usize, usize)> {
    let old_positions = unique_positions(old);
    let new_positions = unique_positions(new);
    let candidates = old.iter().enumerate().filter_map(|(old_index, line)| {
        let old_is_unique = matches!(
            old_positions.get(line),
            Some(Some(position)) if *position == old_index
        );
        if !old_is_unique {
            return None;
        }
        match new_positions.get(line) {
            Some(Some(new_index)) => Some((old_index, *new_index)),
            _ => None,
        }
    });

    longest_increasing_anchors(candidates)
}

fn unique_positions<'a>(lines: &[&'a str]) -> HashMap<&'a str, Option<usize>> {
    let mut positions = HashMap::with_capacity(lines.len());
    for (index, &line) in lines.iter().enumerate() {
        positions
            .entry(line)
            .and_modify(|position| *position = None)
            .or_insert(Some(index));
    }
    positions
}

fn longest_increasing_anchors(
    candidates: impl IntoIterator<Item = (usize, usize)>,
) -> Vec<(usize, usize)> {
    let candidates: Vec<_> = candidates.into_iter().collect();
    let mut tails: Vec<usize> = Vec::new();
    let mut previous = vec![None; candidates.len()];

    for (candidate_index, &(_, new_index)) in candidates.iter().enumerate() {
        let length = tails.partition_point(|&tail| candidates[tail].1 < new_index);
        if length > 0 {
            previous[candidate_index] = Some(tails[length - 1]);
        }
        if length == tails.len() {
            tails.push(candidate_index);
        } else {
            tails[length] = candidate_index;
        }
    }

    let Some(&last) = tails.last() else {
        return Vec::new();
    };
    let mut anchors = Vec::with_capacity(tails.len());
    let mut current = Some(last);
    while let Some(index) = current {
        anchors.push(candidates[index]);
        current = previous[index];
    }
    anchors.reverse();
    anchors
}

#[cfg(test)]
mod tests {
    use super::*;
    use similar::{DiffOp, DiffTag};

    fn assert_reconstructs(original: &str, formatted: &str) {
        let diff = bounded_line_diff(original, formatted);
        let (mut old, mut new) = (String::new(), String::new());
        for op in diff.ops() {
            let (tag, old_lines, new_lines) = op.as_tag_tuple();
            if tag != DiffTag::Insert {
                old.extend(diff.old_lines()[old_lines].iter().copied());
            }
            if tag != DiffTag::Delete {
                new.extend(diff.new_lines()[new_lines].iter().copied());
            }
        }
        assert_eq!(old, original);
        assert_eq!(new, formatted);
    }

    #[test]
    fn reconstructs_both_sides() {
        assert_reconstructs("a\nb\nc\n", "a\nB\nc\n");
        assert_reconstructs("a\nb", "a\nc");
        assert_reconstructs("", "x\n");
        assert_reconstructs("x\n", "");
        assert_reconstructs("a\rb\r\n", "a\rB\r\n");
    }

    #[test]
    fn large_unanchored_middle_is_one_replacement() {
        let lines = MAX_UNANCHORED_LINE_PAIRS.isqrt() + 1;
        let old = format!("header\n{}footer\n", "old\n".repeat(lines));
        let new = format!("header\n{}footer\n", "new\n".repeat(lines));
        let diff = bounded_line_diff(&old, &new);

        assert_eq!(
            diff.ops(),
            [
                DiffOp::Equal {
                    old_index: 0,
                    new_index: 0,
                    len: 1,
                },
                DiffOp::Replace {
                    old_index: 1,
                    old_len: lines,
                    new_index: 1,
                    new_len: lines,
                },
                DiffOp::Equal {
                    old_index: lines + 1,
                    new_index: lines + 1,
                    len: 1,
                },
            ]
        );
        assert_reconstructs(&old, &new);
    }

    #[test]
    fn unique_lines_anchor_a_large_diff() {
        let functions = MAX_UNANCHORED_LINE_PAIRS.isqrt() + 1;
        let old: String = (0..functions)
            .map(|i| format!("function f{i}()\n        x = {i}\nend\n"))
            .collect();
        let new: String = (0..functions)
            .map(|i| format!("function f{i}()\n    x = {i}\nend\n"))
            .collect();
        let diff = bounded_line_diff(&old, &new);
        let equal: usize = diff
            .ops()
            .iter()
            .filter_map(|op| match op {
                DiffOp::Equal { len, .. } => Some(len),
                _ => None,
            })
            .sum();

        assert_eq!(equal, functions * 2);
        assert_reconstructs(&old, &new);
    }

    #[test]
    fn anchors_follow_the_longest_increasing_subsequence() {
        assert_eq!(
            longest_increasing_anchors([(0, 3), (1, 1), (2, 2), (3, 0)]),
            [(1, 1), (2, 2)]
        );
    }
}
