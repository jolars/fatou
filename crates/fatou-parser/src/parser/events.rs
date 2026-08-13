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

/// Close the innermost open node, which must be a `kind`.
///
/// `build_tree`'s `debug_assert_balanced` already catches a stream that opens
/// and closes out of balance. What it cannot see is a *mispaired* close — a
/// `Finish` that balances but shuts the wrong node, yielding a misshapen tree
/// from a correct-looking event count. Naming the kind here turns that into a
/// debug-build assertion at the site that made the mistake, and keeps the name
/// beside the call where a trailing `// KIND` comment used to sit unchecked.
///
/// Reach for this whenever the matching [`Event::Start`] is far enough away that
/// a reader would have to go looking for it; a bare `push(Event::Finish)` is
/// fine when the pair is visible at a glance.
pub(crate) fn finish(events: &mut Vec<Event>, kind: SyntaxKind) {
    #[cfg(debug_assertions)]
    {
        let open = innermost_open(events);
        assert_eq!(
            open,
            Some(kind),
            "closing a {kind:?} but the innermost open node is {open:?}"
        );
    }
    events.push(Event::Finish);
}

/// The kind of the innermost node still open at the end of `events`, found by
/// walking back over the already-closed siblings. Debug-only: the walk is linear
/// in the node's own subtree.
#[cfg(debug_assertions)]
fn innermost_open(events: &[Event]) -> Option<SyntaxKind> {
    let mut closed = 0usize;
    for event in events.iter().rev() {
        match event {
            Event::Finish => closed += 1,
            Event::Start(kind) if closed == 0 => return Some(*kind),
            Event::Start(_) => closed -= 1,
            Event::Tok(_) => {}
        }
    }
    None
}
