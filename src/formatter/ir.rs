//! A Wadler/Prettier-style document IR. Rules build a tree of these primitives;
//! the [`printer`](crate::formatter::printer) makes all line-break decisions by
//! choosing flat or broken layout per [`Group`](Ir::Group).

use std::rc::Rc;

#[derive(Debug, Clone)]
pub enum Ir {
    /// Literal text. Must not contain newlines (use [`Ir::HardLine`]).
    Text(Rc<str>),
    /// A sequence of documents laid out one after another.
    Concat(Rc<[Ir]>),
    /// A space when its group is flat, a newline (+ indent) when broken.
    Line,
    /// Nothing when its group is flat, a newline (+ indent) when broken.
    SoftLine,
    /// Always a newline (+ indent); forces every enclosing group to break.
    HardLine,
    /// A bare newline at column zero (no indent); forces every enclosing group
    /// to break. Used to emit a *blank* line between elements of an already-broken
    /// layout, where a [`HardLine`](Ir::HardLine) would leave the indent as trailing
    /// whitespace on the otherwise-empty line.
    BlankLine,
    /// Increase the indent of the contained document by one step.
    Indent(Rc<Ir>),
    /// A group laid out flat if it fits the line width, otherwise broken.
    Group(Rc<Ir>),
    /// Conditional text: the first string when the enclosing group is broken,
    /// the second when it is flat. The canonical use is a trailing separator that
    /// only appears in the broken layout, e.g. `IfBreak(",", "")`. Measured as its
    /// *flat* string when deciding whether a group fits.
    IfBreak(Rc<str>, Rc<str>),
}

impl Ir {
    pub fn text(s: impl Into<Rc<str>>) -> Ir {
        Ir::Text(s.into())
    }

    pub fn concat(items: impl IntoIterator<Item = Ir>) -> Ir {
        Ir::Concat(items.into_iter().collect())
    }

    pub fn group(inner: Ir) -> Ir {
        Ir::Group(Rc::new(inner))
    }

    pub fn indent(inner: Ir) -> Ir {
        Ir::Indent(Rc::new(inner))
    }

    pub fn if_break(broken: impl Into<Rc<str>>, flat: impl Into<Rc<str>>) -> Ir {
        Ir::IfBreak(broken.into(), flat.into())
    }
}
