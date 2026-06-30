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
                let mode = if fits(inner, width.saturating_sub(col)) {
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

/// Whether `doc` fits flat within `remaining` columns. A [`HardLine`](Ir::HardLine)
/// never fits, forcing the enclosing group to break.
fn fits(doc: &Ir, remaining: usize) -> bool {
    let mut remaining = remaining as isize;
    let mut stack: Vec<&Ir> = vec![doc];
    while let Some(ir) = stack.pop() {
        if remaining < 0 {
            return false;
        }
        match ir {
            // An embedded newline (only ever from raw transparent text) means the
            // content can't sit flat on the current line.
            Ir::Text(s) if s.contains('\n') => return false,
            Ir::Text(s) => remaining -= s.chars().count() as isize,
            Ir::Concat(items) => {
                for item in items.iter().rev() {
                    stack.push(item);
                }
            }
            Ir::Indent(inner) | Ir::Group(inner) => stack.push(inner),
            Ir::Line => remaining -= 1,
            Ir::SoftLine => {}
            // A group fits flat iff it fits flat; measure the flat string.
            Ir::IfBreak(_, flat) => remaining -= flat.chars().count() as isize,
            Ir::HardLine | Ir::BlankLine => return false,
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
