use std::cell::Cell;

use crate::parser::cursor;
use crate::parser::lexer::Token;

/// Maximum number of consecutive cursor peeks that make **no** forward progress
/// (never probe a token past the furthest seen) before the parser aborts as
/// stuck. Modeled on rust-analyzer's `PARSER_STEP_LIMIT` and badness's
/// `src/parser/grammar.rs`, a catch-all against a non-advancing loop that holds
/// *independent of grammar correctness*. The budget resets whenever a peek
/// reaches a new high-water position (see [`ParserCtx::step`]), so in normal
/// parsing only O(1) stale peeks accrue between two frontier advances; this
/// ceiling sits astronomically above any real file and can only be reached by a
/// genuine infinite loop. Unlike a structural "the index only advances" argument
/// it holds even for malformed or adversarial input (a fuzzer, a corrupt corpus
/// file), turning a hang into a loud, diagnosable panic. The language server's
/// read pool (`task_pool`) and analysis thread (`analysis_thread::guard`)
/// already recover from a parse panic, degrading a wedged parse to a logged
/// error rather than a frozen server.
const PARSER_STEP_LIMIT: u32 = 15_000_000;

/// A lightweight wrapper over the token slice with cursor helpers, threaded
/// through the parser so navigation reads the same way everywhere.
pub(crate) struct ParserCtx<'a> {
    tokens: &'a [Token],
    /// Consecutive peeks since the last frontier advance (the stuck-loop guard).
    steps: Cell<u32>,
    /// The furthest token index any peek has reached; forward progress resets
    /// [`Self::steps`].
    high_water: Cell<usize>,
}

impl<'a> ParserCtx<'a> {
    pub(crate) fn new(tokens: &'a [Token]) -> Self {
        Self {
            tokens,
            steps: Cell::new(0),
            high_water: Cell::new(0),
        }
    }

    /// Tick the stuck-loop guard, called from every peek primitive with the
    /// index being inspected. Reaching a token past the furthest seen is real
    /// progress and resets the budget, so the surviving count is the number of
    /// *consecutive* peeks that never advanced the frontier. Exceeding
    /// [`PARSER_STEP_LIMIT`] means the parser is wedged in a non-advancing loop;
    /// abort loudly rather than hang.
    #[inline]
    fn step(&self, i: usize) {
        if i > self.high_water.get() {
            self.high_water.set(i);
            self.steps.set(0);
        }
        let steps = self.steps.get();
        assert!(
            steps < PARSER_STEP_LIMIT,
            "parser exceeded {PARSER_STEP_LIMIT} peeks without advancing past token {} \
             — non-advancing loop",
            self.high_water.get()
        );
        self.steps.set(steps + 1);
    }

    pub(crate) fn token(&self, i: usize) -> Option<&'a Token> {
        self.step(i);
        self.tokens.get(i)
    }

    pub(crate) fn tokens(&self) -> &'a [Token] {
        self.tokens
    }

    pub(crate) fn skip_ws(&self, i: usize) -> usize {
        self.step(i);
        cursor::skip_ws(self.tokens, i)
    }

    pub(crate) fn skip_ws_and_block_comments(&self, i: usize) -> usize {
        self.step(i);
        cursor::skip_ws_and_block_comments(self.tokens, i)
    }

    pub(crate) fn skip_ws_and_newlines(&self, i: usize) -> usize {
        self.step(i);
        cursor::skip_ws_and_newlines(self.tokens, i)
    }

    pub(crate) fn skip_trivia(&self, i: usize) -> usize {
        self.step(i);
        cursor::skip_trivia(self.tokens, i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "non-advancing loop")]
    fn step_guard_trips_when_wedged() {
        // Peeking the same position forever (a wedged loop) must abort once the
        // budget is spent rather than spin. Pin the high-water mark so no peek
        // counts as progress, then drive the budget over the ceiling.
        let ctx = ParserCtx::new(&[]);
        ctx.high_water.set(usize::MAX);
        ctx.steps.set(PARSER_STEP_LIMIT - 1);
        ctx.step(0); // budget reaches the ceiling
        ctx.step(0); // one peek too many → panic
    }

    #[test]
    fn step_budget_resets_on_progress() {
        // Reaching a token past the frontier is genuine progress and clears the
        // budget, so a long but advancing parse never trips.
        let ctx = ParserCtx::new(&[]);
        ctx.steps.set(PARSER_STEP_LIMIT - 1);
        ctx.step(1);
        assert_eq!(
            ctx.steps.get(),
            1,
            "advancing the frontier resets the budget"
        );
        assert_eq!(ctx.high_water.get(), 1);
    }
}
