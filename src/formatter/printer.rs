//! The layout engine: render an [`Ir`] document to a string, choosing flat or
//! broken layout per group with a best-fit (Wadler) algorithm.

use crate::formatter::ir::Ir;
use crate::formatter::style::FormatStyle;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Flat,
    Break,
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
    // Work stack of (indent, mode, node), processed depth-first.
    let mut stack: Vec<(usize, Mode, &Ir)> = vec![(indent, Mode::Break, doc)];

    while let Some((indent, mode, ir)) = stack.pop() {
        match ir {
            Ir::Text(s) => {
                out.push_str(s);
                // Text is normally newline-free, but the transparent lowering
                // passes raw source newlines through as `Text`; honor them so the
                // column tracking stays accurate for later groups' fit checks.
                match s.rfind('\n') {
                    Some(i) => col = s[i + 1..].chars().count(),
                    None => col += s.chars().count(),
                }
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
                Mode::Break => col = newline(&mut out, indent),
            },
            Ir::SoftLine => {
                if mode == Mode::Break {
                    col = newline(&mut out, indent);
                }
            }
            Ir::HardLine => col = newline(&mut out, indent),
            Ir::BlankLine => {
                out.push('\n');
                col = 0;
            }
            Ir::Group(inner) => {
                // A group fits flat only if its flat rendering *plus the trailing
                // content already on the current line* stays within the width. The
                // trailing content is exactly the rest of the work stack up to the
                // next line break, so `fits` walks `inner` (flat) and then `stack`.
                let mode = if fits(width.saturating_sub(col), inner, &stack) {
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
                if hug_fits(width.saturating_sub(col), prefix, body, close, &stack) {
                    stack.push((indent, mode, close));
                    stack.push((indent, mode, body));
                    stack.push((indent, mode, prefix));
                } else {
                    stack.push((indent, mode, explode));
                }
            }
        }
    }

    out
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
fn fits(remaining: usize, inner: &Ir, rest: &[(usize, Mode, &Ir)]) -> bool {
    // Work stack of (in_group, mode, node). Push `rest` bottom-first (it is itself
    // a pop-from-end stack, so its last element prints next), then `inner` on top.
    let mut stack: Vec<(bool, Mode, &Ir)> = Vec::with_capacity(rest.len() + 1);
    for (_, mode, ir) in rest {
        stack.push((false, *mode, ir));
    }
    stack.push((true, Mode::Flat, inner));
    fits_stack(remaining as isize, stack)
}

/// Whether the hug layout of a [`HugGroup`](Ir::HugGroup) has a fitting first
/// line: `prefix` measured strictly flat (a forced break inside a leading
/// argument forbids hugging), then `body` up to its first break opportunity —
/// where its own group would end the line — and, only if the body cannot break,
/// `close` plus the trailing content still pending on the print stack.
fn hug_fits(
    remaining: usize,
    prefix: &Ir,
    body: &Ir,
    close: &Ir,
    rest: &[(usize, Mode, &Ir)],
) -> bool {
    let mut stack: Vec<(bool, Mode, &Ir)> = Vec::with_capacity(rest.len() + 3);
    for (_, mode, ir) in rest {
        stack.push((false, *mode, ir));
    }
    stack.push((false, Mode::Flat, close));
    // Break mode: the body's first `Line`/`SoftLine` ends the measured line, so
    // only the hugged construct's opening bracket counts toward the first line.
    stack.push((false, Mode::Break, body));
    stack.push((true, Mode::Flat, prefix));
    fits_stack(remaining as isize, stack)
}

/// The shared measurement loop behind [`fits`] and [`hug_fits`], walking a
/// prepared `(in_group, mode, node)` stack.
fn fits_stack(mut remaining: isize, mut stack: Vec<(bool, Mode, &Ir)>) -> bool {
    while let Some((in_group, mode, ir)) = stack.pop() {
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
            // Measured as its hug branch: flat, the hug's width equals the
            // explode group's flat width; in trailing break-mode content this
            // walks exactly the parts the hug layout would print.
            Ir::HugGroup {
                prefix,
                body,
                close,
                ..
            } => {
                stack.push((in_group, mode, close));
                stack.push((in_group, mode, body));
                stack.push((in_group, mode, prefix));
            }
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
