//! The layout engine: render an [`Ir`] document to a string, choosing flat or
//! broken layout per group with a best-fit (Wadler) algorithm.

use crate::formatter::ir::Ir;
use crate::formatter::style::FormatStyle;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Flat,
    Break,
}

/// The work stack a fit probe descends into: `(in_group, mode, node)`. Owned by
/// [`print_at`] and reused across probes rather than reallocated per group.
type FitStack<'a> = Vec<(bool, Mode, &'a Ir)>;

/// The rendered position of a structured trailing comment. Byte offsets edit
/// the output, while columns use the printer's existing character-count metric.
struct TrailingCommentMark {
    line: usize,
    indent: usize,
    raw_text_epoch: usize,
    separator_start: usize,
    separator_end: usize,
    code_col: usize,
    comment_width: usize,
}

/// Render `doc` at the given style.
pub fn print(doc: &Ir, style: FormatStyle) -> String {
    print_at(doc, style, 0)
}

/// Render `doc` as if it sat at column `indent` (in spaces): line breaks
/// re-indent to `indent`, nested indents stack on top of it, and group fit
/// checks start from that column. **No leading indent is emitted for the first
/// line** — the caller places the output after existing text (range formatting
/// keeps the first line's original leading whitespace).
pub fn print_at(doc: &Ir, style: FormatStyle, indent: usize) -> String {
    let indent_step = style.indent_width as usize;
    let width = style.line_width as usize;
    let mut out = String::new();
    let mut col = indent;
    let mut line = 0usize;
    let mut line_indent = indent;
    // A raw embedded newline comes only from transparent lowering. It is an
    // explicit boundary: structured suffixes must never align through source
    // text whose physical lines the printer does not own.
    let mut raw_text_epoch = 0usize;
    let mut trailing_comments: Vec<TrailingCommentMark> = Vec::new();
    // Work stack of (indent, mode, node), processed depth-first.
    let mut stack: Vec<(usize, Mode, &Ir)> = vec![(indent, Mode::Break, doc)];
    // Scratch for the fit probes below, reused across every check so a probe
    // allocates nothing: `fits` runs once per group and would otherwise churn a
    // fresh `Vec` per call.
    let mut scratch: FitStack<'_> = Vec::new();

    while let Some((indent, mode, ir)) = stack.pop() {
        match ir {
            Ir::Text(s) => {
                out.push_str(s);
                // Text is normally newline-free, but the transparent lowering
                // passes raw source newlines through as `Text`; honor them so the
                // column tracking stays accurate for later groups' fit checks.
                match s.rfind('\n') {
                    Some(i) => {
                        line += s.bytes().filter(|byte| *byte == b'\n').count();
                        let tail = &s[i + 1..];
                        col = tail.chars().count();
                        line_indent = tail.chars().take_while(|ch| *ch == ' ').count();
                        raw_text_epoch += 1;
                    }
                    None => col += s.chars().count(),
                }
            }
            Ir::TrailingComment(s) => {
                let separator_start = out.len();
                let code_col = col;
                out.push(' ');
                let separator_end = out.len();
                out.push_str(s);
                let comment_width = s.chars().count();
                col += 1 + comment_width;
                trailing_comments.push(TrailingCommentMark {
                    line,
                    indent: line_indent,
                    raw_text_epoch,
                    separator_start,
                    separator_end,
                    code_col,
                    comment_width,
                });
            }
            Ir::Concat(items) => {
                for item in items.iter().rev() {
                    stack.push((indent, mode, item));
                }
            }
            Ir::Indent(inner) => stack.push((indent + indent_step, mode, inner)),
            Ir::Line => match mode {
                Mode::Flat => {
                    out.push(' ');
                    col += 1;
                }
                Mode::Break => {
                    col = newline(&mut out, indent);
                    line += 1;
                    line_indent = indent;
                }
            },
            Ir::SoftLine => {
                if mode == Mode::Break {
                    col = newline(&mut out, indent);
                    line += 1;
                    line_indent = indent;
                }
            }
            Ir::HardLine => {
                col = newline(&mut out, indent);
                line += 1;
                line_indent = indent;
            }
            Ir::BlankLine => {
                out.push('\n');
                col = 0;
                line += 1;
                line_indent = 0;
            }
            Ir::Group(inner) => {
                // A group fits flat only if its flat rendering *plus the trailing
                // content already on the current line* stays within the width. The
                // trailing content is exactly the rest of the work stack up to the
                // next line break, so `fits` walks `inner` (flat) and then `stack`.
                let mode = if fits(width.saturating_sub(col), inner, &stack, &mut scratch) {
                    Mode::Flat
                } else {
                    Mode::Break
                };
                stack.push((indent, mode, inner));
            }
            Ir::IfBreak(broken, flat) => {
                let s = if mode == Mode::Break { broken } else { flat };
                out.push_str(s);
                col += s.chars().count();
            }
            Ir::HugGroup {
                prefix,
                body,
                close,
                explode,
            } => {
                // Hug when the hug layout's first line fits; otherwise fall back
                // to the standard explode group (re-measured by the normal Group
                // arm — it always breaks here, since the hug measure never
                // exceeds its flat measure).
                if hug_fits(
                    width.saturating_sub(col),
                    prefix,
                    body,
                    close,
                    &stack,
                    &mut scratch,
                ) {
                    stack.push((indent, mode, close));
                    stack.push((indent, mode, body));
                    stack.push((indent, mode, prefix));
                } else {
                    stack.push((indent, mode, explode));
                }
            }
            Ir::CondGroup {
                primary,
                fallback,
                probe,
            } => {
                // The deciding line is the group's re-indented closing line, not the
                // current one, so measure `probe` (flat) from the *base indent*
                // rather than `col`. It fits exactly when breaking `primary`'s head
                // leaves the flat bound sitting on that closing line.
                let chosen = if fits(width.saturating_sub(indent), probe, &stack, &mut scratch) {
                    primary
                } else {
                    fallback
                };
                stack.push((indent, mode, chosen));
            }
        }
    }

    align_trailing_comments(&mut out, &trailing_comments, width);
    out
}

/// Align maximal adjacent runs after layout is fixed. A run is all-or-nothing:
/// if any padded line would exceed `line_width`, every member keeps one space.
fn align_trailing_comments(out: &mut String, comments: &[TrailingCommentMark], line_width: usize) {
    let mut replacements: Vec<(usize, usize, usize)> = Vec::new();
    let mut start = 0usize;
    while start < comments.len() {
        let mut same_line_end = start + 1;
        while same_line_end < comments.len() && comments[same_line_end].line == comments[start].line
        {
            same_line_end += 1;
        }
        if same_line_end > start + 1 {
            // Multiple suffixes on one physical line are not distinct
            // code/comment pairs, and form a hard boundary on both sides.
            start = same_line_end;
            continue;
        }

        let mut end = start + 1;
        while end < comments.len()
            && comments[end].line == comments[end - 1].line + 1
            && comments[end].indent == comments[start].indent
            && comments[end].raw_text_epoch == comments[start].raw_text_epoch
            && (end + 1 == comments.len() || comments[end + 1].line != comments[end].line)
        {
            end += 1;
        }

        let run = &comments[start..end];
        if run.len() >= 2 {
            let target = run
                .iter()
                .map(|comment| comment.code_col)
                .max()
                .expect("non-empty trailing-comment run")
                + 1;
            if run
                .iter()
                .all(|comment| target + comment.comment_width <= line_width)
            {
                for comment in run {
                    replacements.push((
                        comment.separator_start,
                        comment.separator_end,
                        target - comment.code_col,
                    ));
                }
            }
        }
        start = end;
    }

    // Earlier byte offsets remain valid when replacements run right-to-left.
    for (start, end, width) in replacements.into_iter().rev() {
        out.replace_range(start..end, &" ".repeat(width));
    }
}

/// Emit a newline followed by `indent` spaces; return the new column.
fn newline(out: &mut String, indent: usize) -> usize {
    out.push('\n');
    for _ in 0..indent {
        out.push(' ');
    }
    indent
}

/// Whether the group `inner`, rendered flat and followed by the trailing content
/// still pending on the print stack (`rest`), fits within `remaining` columns.
///
/// `inner` is measured flat; `rest` items keep the mode they were queued with, so
/// a line break in an already-broken enclosing group ends the measured line. The
/// scan stops — the group *fits* — as soon as the current line ends (a break-mode
/// [`Line`](Ir::Line)/[`SoftLine`](Ir::SoftLine), a [`HardLine`](Ir::HardLine)/
/// [`BlankLine`](Ir::BlankLine), or a raw embedded newline in trailing text). A
/// forced newline *inside* the group's own flat content instead means it cannot sit
/// flat, so the group must break.
fn fits<'a>(
    remaining: usize,
    inner: &'a Ir,
    rest: &[(usize, Mode, &'a Ir)],
    scratch: &mut FitStack<'a>,
) -> bool {
    scratch.clear();
    scratch.push((true, Mode::Flat, inner));
    fits_stack(remaining as isize, scratch, rest)
}

/// Whether the hug layout of a [`HugGroup`](Ir::HugGroup) has a fitting first
/// line: `prefix` measured strictly flat (a forced break inside a leading
/// argument forbids hugging), then `body` up to its first break opportunity —
/// where its own group would end the line — and, only if the body cannot break,
/// `close` plus the trailing content still pending on the print stack.
fn hug_fits<'a>(
    remaining: usize,
    prefix: &'a Ir,
    body: &'a Ir,
    close: &'a Ir,
    rest: &[(usize, Mode, &'a Ir)],
    scratch: &mut FitStack<'a>,
) -> bool {
    scratch.clear();
    scratch.push((false, Mode::Flat, close));
    // Break mode: the body's first `Line`/`SoftLine` ends the measured line, so
    // only the hugged construct's opening bracket counts toward the first line.
    scratch.push((false, Mode::Break, body));
    scratch.push((true, Mode::Flat, prefix));
    fits_stack(remaining as isize, scratch, rest)
}

/// The shared measurement loop behind [`fits`] and [`hug_fits`], walking a
/// prepared `(in_group, mode, node)` stack.
fn fits_stack<'a>(
    mut remaining: isize,
    stack: &mut FitStack<'a>,
    rest: &[(usize, Mode, &'a Ir)],
) -> bool {
    // `rest` is walked lazily, from the end (it is itself a pop-from-end stack, so
    // its last element prints next) and only once `stack` runs dry. Copying it in
    // up front would cost O(print-stack depth) per probe, and a probe almost always
    // stops within the first few items — the line ends long before `rest` does.
    let mut pending = rest.len();
    loop {
        let (in_group, mode, ir) = match stack.pop() {
            Some(item) => item,
            None => {
                if pending == 0 {
                    break;
                }
                pending -= 1;
                let (_, mode, ir) = rest[pending];
                (false, mode, ir)
            }
        };
        if remaining < 0 {
            return false;
        }
        match ir {
            // A raw embedded newline (only ever from transparent text): inside the
            // group it forbids a flat layout; in trailing content it ends the line.
            Ir::Text(s) => match s.find('\n') {
                Some(i) => {
                    remaining -= s[..i].chars().count() as isize;
                    return !in_group && remaining >= 0;
                }
                None => remaining -= s.chars().count() as isize,
            },
            Ir::TrailingComment(s) => remaining -= 1 + s.chars().count() as isize,
            Ir::Concat(items) => {
                for item in items.iter().rev() {
                    stack.push((in_group, mode, item));
                }
            }
            Ir::Indent(child) => stack.push((in_group, mode, child)),
            // A nested group inherits the carried mode: inside the tested group it
            // renders flat with it; in trailing content it keeps the break mode it
            // was queued with, so its first line break ends the measured line (the
            // tested group is judged as if the trailing group breaks at that point).
            Ir::Group(child) => stack.push((in_group, mode, child)),
            Ir::Line => match mode {
                Mode::Flat => remaining -= 1,
                Mode::Break => return true,
            },
            Ir::SoftLine => {
                if mode == Mode::Break {
                    return true;
                }
            }
            // A forced break ends the line: fatal inside the group, fitting after it.
            Ir::HardLine | Ir::BlankLine => return !in_group,
            Ir::IfBreak(broken, flat) => {
                let s = if mode == Mode::Break { broken } else { flat };
                remaining -= s.chars().count() as isize;
            }
            // Inside the group under test, the hug must sit flat, so measure it
            // flat (prefix + body + close): the hug's width equals the explode
            // group's flat width. As trailing content on an already-broken line,
            // though, the hug will render broken — its first line ends at the open
            // bracket, exactly as a plain trailing `Group` ends at its first
            // `SoftLine`. Measure the explode fallback's broken first line (open
            // bracket, then its `SoftLine` ends the line) so a preceding group is
            // not forced to break by the hug's full flat prefix width.
            Ir::HugGroup {
                prefix,
                body,
                close,
                explode,
            } => {
                if !in_group && mode == Mode::Break {
                    stack.push((false, Mode::Break, explode));
                } else {
                    stack.push((in_group, mode, close));
                    stack.push((in_group, mode, body));
                    stack.push((in_group, mode, prefix));
                }
            }
            // A `CondGroup` is measured through its `primary` (the flat-bound
            // layout): its flat width is the all-flat rendering, and as trailing
            // content its head group's first break ends the line, exactly like a
            // plain trailing `Group`.
            Ir::CondGroup { primary, .. } => stack.push((in_group, mode, primary)),
        }
    }
    remaining >= 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list_doc() -> Ir {
        // group("[" indent(softline "a," line "b," line "c") softline "]")
        Ir::group(Ir::concat([
            Ir::text("["),
            Ir::indent(Ir::concat([
                Ir::SoftLine,
                Ir::text("a,"),
                Ir::Line,
                Ir::text("b,"),
                Ir::Line,
                Ir::text("c"),
            ])),
            Ir::SoftLine,
            Ir::text("]"),
        ]))
    }

    #[test]
    fn group_stays_flat_when_it_fits() {
        let style = FormatStyle {
            line_width: 80,
            indent_width: 4,
            ..FormatStyle::default()
        };
        assert_eq!(print(&list_doc(), style), "[a, b, c]");
    }

    #[test]
    fn group_breaks_when_too_wide() {
        let style = FormatStyle {
            line_width: 5,
            indent_width: 4,
            ..FormatStyle::default()
        };
        assert_eq!(print(&list_doc(), style), "[\n    a,\n    b,\n    c\n]");
    }

    #[test]
    fn print_at_starts_from_the_given_column() {
        // The flat rendering is 9 columns: it fits exactly at column 0, but
        // shifted to column 4 it must break — and every line break re-indents
        // relative to that base, with no leading indent on the first line.
        let style = FormatStyle {
            line_width: 9,
            indent_width: 4,
            ..FormatStyle::default()
        };
        assert_eq!(print_at(&list_doc(), style, 0), "[a, b, c]");
        assert_eq!(
            print_at(&list_doc(), style, 4),
            "[\n        a,\n        b,\n        c\n    ]"
        );
    }

    fn trailing_comma_doc() -> Ir {
        // group("(" indent(softline "a," line "b") ifbreak("," "") softline ")")
        Ir::group(Ir::concat([
            Ir::text("("),
            Ir::indent(Ir::concat([
                Ir::SoftLine,
                Ir::text("a,"),
                Ir::Line,
                Ir::text("b"),
                Ir::if_break(",", ""),
            ])),
            Ir::SoftLine,
            Ir::text(")"),
        ]))
    }

    #[test]
    fn if_break_is_empty_when_flat() {
        let style = FormatStyle {
            line_width: 80,
            indent_width: 4,
            ..FormatStyle::default()
        };
        assert_eq!(print(&trailing_comma_doc(), style), "(a, b)");
    }

    #[test]
    fn if_break_emits_when_broken() {
        let style = FormatStyle {
            line_width: 4,
            indent_width: 4,
            ..FormatStyle::default()
        };
        assert_eq!(print(&trailing_comma_doc(), style), "(\n    a,\n    b,\n)");
    }

    #[test]
    fn adjacent_trailing_comments_align_to_the_longest_prefix() {
        let ir = Ir::concat([
            Ir::text("a"),
            Ir::trailing_comment("# first"),
            Ir::HardLine,
            Ir::text("long"),
            Ir::trailing_comment("# second"),
        ]);
        assert_eq!(
            print(&ir, FormatStyle::default()),
            "a    # first\nlong # second"
        );
    }

    #[test]
    fn trailing_comment_alignment_counts_characters_not_bytes() {
        let ir = Ir::concat([
            Ir::text("é"),
            Ir::trailing_comment("# first"),
            Ir::HardLine,
            Ir::text("long"),
            Ir::trailing_comment("# second"),
        ]);
        assert_eq!(
            print(&ir, FormatStyle::default()),
            "é    # first\nlong # second"
        );
    }

    #[test]
    fn trailing_comment_alignment_stops_at_a_different_indent() {
        let ir = Ir::concat([
            Ir::text("a"),
            Ir::trailing_comment("# first"),
            Ir::indent(Ir::concat([
                Ir::HardLine,
                Ir::text("long"),
                Ir::trailing_comment("# second"),
            ])),
        ]);
        assert_eq!(
            print(&ir, FormatStyle::default()),
            "a # first\n    long # second"
        );
    }

    #[test]
    fn multiple_trailing_comments_on_one_line_break_alignment() {
        let ir = Ir::concat([
            Ir::text("a"),
            Ir::trailing_comment("# first"),
            Ir::trailing_comment("# second"),
            Ir::HardLine,
            Ir::text("long"),
            Ir::trailing_comment("# third"),
        ]);
        assert_eq!(
            print(&ir, FormatStyle::default()),
            "a # first # second\nlong # third"
        );
    }

    #[test]
    fn raw_multiline_text_breaks_trailing_comment_alignment() {
        let ir = Ir::concat([
            Ir::text("a"),
            Ir::trailing_comment("# first"),
            Ir::text("\n"),
            Ir::text("long"),
            Ir::trailing_comment("# second"),
        ]);
        assert_eq!(
            print(&ir, FormatStyle::default()),
            "a # first\nlong # second"
        );
    }

    #[test]
    fn overflowing_trailing_comment_leaves_the_whole_run_unaligned() {
        let style = FormatStyle {
            line_width: 14,
            ..FormatStyle::default()
        };
        let ir = Ir::concat([
            Ir::text("a"),
            Ir::trailing_comment("# 1234567890"),
            Ir::HardLine,
            Ir::text("long"),
            Ir::trailing_comment("# short"),
        ]);
        assert_eq!(print(&ir, style), "a # 1234567890\nlong # short");
    }

    #[test]
    fn trailing_comments_still_participate_in_group_fit() {
        let style = FormatStyle {
            line_width: 14,
            ..FormatStyle::default()
        };
        let ir = Ir::concat([list_doc(), Ir::trailing_comment("# comment")]);
        assert_eq!(print(&ir, style), "[\n    a,\n    b,\n    c\n] # comment");
    }

    fn hug_doc() -> Ir {
        // f(aa, [x, y]) with a huggable last argument, as `lower_arg_list`
        // builds it: prefix `(aa, `, body the list's own group, explode the
        // standard width-driven group over both items.
        let body = || {
            Ir::group(Ir::concat([
                Ir::text("["),
                Ir::indent(Ir::concat([
                    Ir::SoftLine,
                    Ir::text("x,"),
                    Ir::Line,
                    Ir::text("y"),
                ])),
                Ir::SoftLine,
                Ir::text("]"),
            ]))
        };
        let explode = Ir::group(Ir::concat([
            Ir::text("("),
            Ir::indent(Ir::concat([
                Ir::SoftLine,
                Ir::text("aa"),
                Ir::text(","),
                Ir::Line,
                body(),
                Ir::if_break(",", ""),
            ])),
            Ir::SoftLine,
            Ir::text(")"),
        ]));
        Ir::concat([
            Ir::text("f"),
            Ir::hug_group(
                Ir::concat([Ir::text("("), Ir::text("aa"), Ir::text(", ")]),
                body(),
                Ir::text(")"),
                explode,
            ),
        ])
    }

    #[test]
    fn hug_group_stays_flat_when_it_fits() {
        let style = FormatStyle {
            line_width: 80,
            indent_width: 4,
            ..FormatStyle::default()
        };
        assert_eq!(print(&hug_doc(), style), "f(aa, [x, y])");
    }

    #[test]
    fn hug_group_hugs_when_first_line_fits() {
        // Flat (13) overflows, but the hug first line `f(aa, [` (7) fits.
        let style = FormatStyle {
            line_width: 8,
            indent_width: 4,
            ..FormatStyle::default()
        };
        assert_eq!(print(&hug_doc(), style), "f(aa, [\n    x,\n    y\n])");
    }

    #[test]
    fn hug_group_explodes_when_first_line_overflows() {
        // Even `f(aa, [` (7) overflows: the explode fallback breaks one item
        // per line, the list free to break further on its own.
        let style = FormatStyle {
            line_width: 6,
            indent_width: 4,
            ..FormatStyle::default()
        };
        assert_eq!(
            print(&hug_doc(), style),
            "f(\n    aa,\n    [\n        x,\n        y\n    ],\n)"
        );
    }
}
