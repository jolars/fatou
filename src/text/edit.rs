//! Applying LSP `didChange` content changes to a text buffer.

use lsp_types::TextDocumentContentChangeEvent;

use super::LineIndex;

/// Apply a `didChange` batch to `text` in place.
///
/// Changes apply sequentially: each range is interpreted against the text as
/// it stands after the previous change, so the line table is rebuilt per
/// ranged change. A change without a range replaces the whole buffer (legal
/// from clients even under incremental sync), so application starts at the
/// last such change and everything before it is skipped. Out-of-range
/// positions clamp to the end of the line or buffer.
pub fn apply_content_changes(text: &mut String, changes: Vec<TextDocumentContentChangeEvent>) {
    let start = changes
        .iter()
        .rposition(|change| change.range.is_none())
        .unwrap_or(0);
    for change in &changes[start..] {
        match change.range {
            Some(range) => {
                let index = LineIndex::new(text);
                let start = index.position_to_byte(range.start);
                let end = index.position_to_byte(range.end);
                text.replace_range(start..end, &change.text);
            }
            None => {
                text.clear();
                text.push_str(&change.text);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use lsp_types::{Position, Range};

    use super::*;

    fn ranged(start: (u32, u32), end: (u32, u32), text: &str) -> TextDocumentContentChangeEvent {
        TextDocumentContentChangeEvent {
            range: Some(Range::new(
                Position::new(start.0, start.1),
                Position::new(end.0, end.1),
            )),
            range_length: None,
            text: text.to_string(),
        }
    }

    fn full(text: &str) -> TextDocumentContentChangeEvent {
        TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: text.to_string(),
        }
    }

    fn apply(initial: &str, changes: Vec<TextDocumentContentChangeEvent>) -> String {
        let mut text = initial.to_string();
        apply_content_changes(&mut text, changes);
        text
    }

    #[test]
    fn insert_delete_replace_on_one_line() {
        assert_eq!(apply("ab", vec![ranged((0, 1), (0, 1), "x")]), "axb");
        assert_eq!(apply("axb", vec![ranged((0, 1), (0, 2), "")]), "ab");
        assert_eq!(apply("abc", vec![ranged((0, 1), (0, 2), "xy")]), "axyc");
    }

    #[test]
    fn sequential_changes_see_prior_edits() {
        // The second range is only correct against the post-first-change text:
        // (0, 3)..(0, 3) lands after "xyz" only once the first insert applied.
        let changes = vec![ranged((0, 0), (0, 0), "xyz"), ranged((0, 3), (0, 3), "!")];
        assert_eq!(apply("ab", changes), "xyz!ab");
    }

    #[test]
    fn edit_spanning_a_newline() {
        assert_eq!(apply("ab\ncd", vec![ranged((0, 1), (1, 1), "-")]), "a-d");
    }

    #[test]
    fn insert_adding_lines_shifts_later_ranges() {
        // The second change targets line 2, which only exists after the first
        // change inserts a newline: the line table must be rebuilt in between.
        let changes = vec![ranged((0, 2), (0, 2), "\nnew"), ranged((1, 3), (1, 3), "!")];
        assert_eq!(apply("ab\ncd", changes), "ab\nnew!\ncd");
    }

    #[test]
    fn utf16_offsets_after_surrogate_pair() {
        // U+1F600 is 2 UTF-16 units, so character 2 is just past the emoji.
        assert_eq!(
            apply("\u{1F600}x", vec![ranged((0, 2), (0, 3), "y")]),
            "\u{1F600}y"
        );
    }

    #[test]
    fn full_replacement() {
        assert_eq!(apply("old", vec![full("new")]), "new");
    }

    #[test]
    fn changes_before_a_full_replacement_are_skipped() {
        let changes = vec![
            ranged((5, 0), (9, 0), "junk that must not apply"),
            full("base\n"),
            ranged((0, 4), (0, 4), "!"),
        ];
        assert_eq!(apply("ab", changes), "base!\n");
    }

    #[test]
    fn out_of_range_positions_clamp() {
        assert_eq!(apply("ab\ncd", vec![ranged((0, 9), (9, 9), "!")]), "ab!");
        assert_eq!(apply("", vec![ranged((3, 1), (4, 2), "x")]), "x");
    }
}
