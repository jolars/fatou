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
    let indent_step = style.indent_width as usize;
    let width = style.line_width as usize;
    let mut out = String::new();
    let mut col = 0usize;
    // Work stack of (indent, mode, node), processed depth-first.
    let mut stack: Vec<(usize, Mode, &Ir)> = vec![(0, Mode::Break, doc)];

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
    let mut remaining = remaining as isize;
    // Work stack of (in_group, mode, node). Push `rest` bottom-first (it is itself
    // a pop-from-end stack, so its last element prints next), then `inner` on top.
    let mut stack: Vec<(bool, Mode, &Ir)> = Vec::with_capacity(rest.len() + 1);
    for (_, mode, ir) in rest {
        stack.push((false, *mode, ir));
    }
    stack.push((true, Mode::Flat, inner));

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
        };
        assert_eq!(print(&list_doc(), style), "[a, b, c]");
    }

    #[test]
    fn group_breaks_when_too_wide() {
        let style = FormatStyle {
            line_width: 5,
            indent_width: 4,
        };
        assert_eq!(print(&list_doc(), style), "[\n    a,\n    b,\n    c\n]");
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
        };
        assert_eq!(print(&trailing_comma_doc(), style), "(a, b)");
    }

    #[test]
    fn if_break_emits_when_broken() {
        let style = FormatStyle {
            line_width: 4,
            indent_width: 4,
        };
        assert_eq!(print(&trailing_comma_doc(), style), "(\n    a,\n    b,\n)");
    }
}
