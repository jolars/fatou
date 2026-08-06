use crate::syntax::SyntaxKind;

/// A flat instruction stream describing how to build the CST. Decouples the
/// parsing logic (which only appends events) from tree construction
/// ([`crate::parser::tree_builder::build_tree`]).
#[derive(Debug, Clone)]
pub(crate) enum Event {
    /// Open a node of the given kind.
    Start(SyntaxKind),
    /// Emit the token at this index in the token stream.
    Tok(usize),
    /// Close the most recently opened node.
    Finish,
}

/// The result of parsing one (sub)expression: the token range it covers plus the
/// events that build its subtree.
#[derive(Debug, Clone)]
pub(crate) struct ExprParse {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) events: Vec<Event>,
}

/// Emit `Event::Tok(i)` for every `i` in `start..end`.
pub(crate) fn push_range(events: &mut Vec<Event>, start: usize, end: usize) {
    for idx in start..end {
        events.push(Event::Tok(idx));
    }
}
