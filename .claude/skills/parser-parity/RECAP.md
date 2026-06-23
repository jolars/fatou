# parser-parity recap

Rolling log. Read top-to-bottom: persistent traps → progress → latest session →
earlier log. Keep ≤ ~300 lines; demote the "Latest session" to a one-liner in the
"Earlier sessions" list each new session.

## Persistent traps & invariants

- **Projector is faithful, never compensating.** Translate encoding (wrappers,
  delimiters, trivia) only; let modeling divergences surface. Diffs that live
  mostly in `sexpr.rs` are a smell.
- **5-file operator recipe**: lexer `TokKind`+lex → `syntax.rs` kind →
  `tree_builder.rs` map → `expr.rs` `infix_binding_power` → `sexpr.rs`
  `infix_head` + `is_operator`. Probe Julia for tier/associativity first.
- **Probe whitespace-sensitive siblings** before scoping (`a[begin]` vs
  `[begin x end]`; `:foo` vs `a[:]`). Scope narrowly to avoid regressing one.
- **Reseed allowlists with the `grep -E '^#|^$'` header-preserving recipe.**
- **Reports are gitignored; `expected.sexpr` is generated** — never hand-edit.
- **Shell `raw"""…"""` Julia probes break on `"`/`$`** — use a temp file.
- **Corpus pinned** to JuliaSyntax in `.juliasyntax-source` (currently 0.4.10 /
  Julia 1.12.6). Bump ⇒ re-run both `scripts/*.jl`, re-triage.

## Progress

JS corpus (**685 cases** — error shapes now harvested): **591 allowlisted**,
94 divergence, 0 unsupported. Dir corpus: **135 allowlisted**, 2 blocked
(end_index/numeric_literals; both FAIL not skip since `render` is total).
Grammar bullets through "incomplete-`try` truncation" are `[x]` in `TODO.md`.

Deliberate (recorded) divergences, do not "fix": comparison chains (nested),
associative `a*b*c` (nested binary), n-ary juxtaposition `(2)(3)x` (nests right),
**float**-literal display normalization (`2.`/`1f0`/hex floats/`1.0e-1000`; the
integer half is now handled), `end`/`[1 +2]`/unterminated-string error shapes
(dir `blocked.txt`).

## Latest session (2026-06-23d)

**Incomplete-`try` truncation `(error-t)` (finishes the missing-`end` family).**
A `try` requires a `catch`/`finally`; with neither present JuliaSyntax splices a
marker for the *missing handler*, and (separately, via the existing `expect_end`
chokepoint) one for a *missing `end`*: `try x` ⇒ `(try (block x) (error-t)
(error-t))`, `try\n x` same; `try x end` ⇒ `(try (block x) (error-t))` (handler
marker only — `end` present); `try x catch e y` ⇒ `(try (block x) (catch e
(block y)) (error-t))` (end marker only); `try x finally z` mirrors. Fix:
`parse_try_expr` (`structural.rs`) tracks a `saw_handler` flag (set on
`catch`/`finally`, **not** `else`) and splices the missing-handler `ERROR_TRIVIA`
+ a diagnostic before `expect_end` when false; `project_try` (`sexpr.rs`) gains
an `ERROR_TRIVIA` arm so both markers render in document order. Fixture
`incomplete_try`; dir-corpus case minted. JS allow 590 → 591 (`try x end` now
PASS), dir 134 → 135. `try x else y end` stays FAIL (else-without-catch wants the
else block error-wrapped — separate). Zero regressions; green; clippy/fmt clean.

**Suggested next targets (ranked):** (1) **else-without-catch error-wrap**
`try x else y end`⇒`(try (block x) (else (error (block y))) (error-t))` — the
last try-family divergence; the marker already matches, only the `(else (error
…))` wrap is missing. (2) **macro-path error-t** `A.@B.x`⇒`(macrocall (. (.
A (quote B)) (error-t) (quote @x)))`, `@A.B.@x a`. (3) **paren-block
string-juxtapose** `(begin end)"x"`⇒`(block) (error-t ✘ "x" ✘)` (double-`✘`
string form). (4) **char cluster** — `'ab'`⇒`(char (ErrorOverLongCharacter))`,
`'\xq'`⇒`(char (ErrorInvalidEscapeSequence))`, `''`/`'`⇒`(char (error))`,
`'a`⇒`(char 'a' (error-t))` (needs lexer-classified named kinds; ~5 JS cases).
(5) **`;`-segment stray-closer** double-`✘`.

## Earlier sessions

- **2026-06-23c** — Missing-`end` truncation `(error-t)`: a block form cut off
  before its `end` (EOF/unconsumable closer) gets a zero-width `ERROR_TRIVIA` last
  child (`if c\n x`⇒`(if c (block x) (error-t))`); `begin`/`quote` fold it inside.
  `expect_end` (`structural.rs`) splices it; `push_trailing_errors` renders.
  Unblocked dir `do_blocks`; fixtures `incomplete_block`/`incomplete_begin`. Dir
  131 → 134.
- **2026-06-23b** — Generator/comprehension whitespace-error `(error-t)`: a `for`
  glued to the preceding element (`[(x)for x in xs]`) splices one zero-width
  `ERROR_TRIVIA` between body and first clause ⇒ `(generator x (error-t) (= x
  xs))`, also through a filter; spaced forms stay marker-free. `parse_comprehension`
  emits the marker when `for_idx == pos`; `project_generator` renders it. Fixture
  `generator_whitespace_error`. JS allow 589 → 590; dir 130 → 131.
- **2026-06-23a** — Ternary whitespace-error `(error-t)`: missing ws on either
  side of `?`/`:` splices a zero-width marker (`a? b : c`⇒`(? a (error-t) b c)`,
  `a ? b: c`⇒`(? a b (error-t) c)`, `a?b:c` doubles each); a missing `:` is itself
  one marker with the false-branch parsed greedily (`a ? b c`⇒`(? a b (error-t)
  c)`). Pure `expr.rs` `parse_ternary`; projector untouched. Fixture
  `ternary_whitespace_error`. JS 584 → 589; dir 129 → 130.
- **2026-06-22z** — Lone-closer leading-`(error)` `✘`: a stray *closing* delimiter
  at statement start is JuliaSyntax's synthesized empty `(error)` plus an
  `(error-t ✘ …)` swallowing the rest of the line (`)` ⇒ `(error) (error-t ✘)`,
  `) x` ⇒ `(error) (error-t ✘ x)`, `)))`, `] x`, `}`). Fix in the `parse` driver
  (`core.rs`): on `parse_stmt`-None with no leftover mark, a close-delimiter token,
  and no `;`, push empty `ERROR` then an `ERROR_TRIVIA` over the run. Projector
  untouched. Fixture `stray_closer_start`. JS 583 → 584; dir 128 → 129. Deferred:
  `;`-segment double-`✘`.

- **2026-06-22y** — Optional-value-keyword stray-closer `✘`: `return` followed by
  a stray closer ends the empty form right after the keyword, leaving the closer
  for the toplevel-leftover driver (`return)`⇒`(return) (error-t ✘)`, `return) x`).
  New `optional_value` flag on `parse_keyword_stmt` (`structural.rs`); only `return`
  passes `true`. Pure `expr.rs`+`structural.rs`. JS 582 → 583.
- **2026-06-22x** — Bare `:` colon value atom: a prefix `:` not quotable is the
  Colon *value* atom (`parse_quote_sym` declines → `parse_prefix` `.or_else`s to
  `OPERATOR_ATOM`), `a[:]`⇒`(ref a :)`, `[:]`⇒`(vect :)`, lone `:`⇒`:`; also
  unblocked `:)`⇒`(toplevel : (error-t ✘))`. Pure `expr.rs`. JS 581 → 582.
- **2026-06-22w** — Stray-closing-delimiter `✘` leftover: a leftover *closing*
  delimiter at toplevel is JuliaSyntax's `✘` glyph (`var"x")`⇒`(var x) (error-t
  ✘)`, `&)`⇒`& (error-t ✘)`, `a)`/`1)`/`x]`/`f(x))`). Pure `sexpr.rs`:
  `project_error` walks `children_with_tokens` and renders a close-delimiter token
  (`is_close_delimiter`) as `✘`. JS 576 → 581.

The **error-shape lineage** (the current frontier; entries share the
`ERROR_TRIVIA`/`project_error`/leftover-driver machinery, so kept in brief):

- **2026-06-22v** — Paren-block juxtapose-error: `(begin end)x`⇒`(block)
  (error-t x)`, `(if c end)y`⇒`(if c (block)) (error-t y)`; new `lhs_is_paren_block`
  (a `PAREN_EXPR` wrapping a block-keyword form) suppresses both juxtapose checks
  so the toplevel-leftover driver wraps the trailing run; postfix/infix still
  apply. Pure `expr.rs` change. JS 575 → 576.
- **2026-06-22u** — String-juxtapose-error: `"a"x`⇒`(juxtapose (string "a")
  (error-t) x)`, `2"a"` mirror; `should_juxtapose_string_error` runs before
  numeric `should_juxtapose`, `build_string_juxtapose_error` splices the marker;
  numbers/`@`/operators/`end` break it (docstring fold keeps numeric forms). JS
  571 → 575.
- **2026-06-22t** — Separate-toplevel trailing-junk: `x y`⇒`x (error-t y)`,
  `f(2)2`⇒`(call f 2) (error-t 2)`; the `parse` driver (`core.rs`) records
  `leftover_mark` and wraps the recovered run in one `ERROR_TRIVIA` sibling; a
  bare docstring opener is exempt. JS 568 → 571.
- **2026-06-22s** — Field-access/colon-quote space: `x .y`⇒`(. x (error-t)
  (quote y))` (operator-loop `Dot` arm via `build_binary_dot_error` when
  `op_idx > lhs.end`; broadcast `.+` is one token so `a .+ b` is untouched),
  `: foo`⇒`(quote-: (error-t) foo)` (`parse_quote_sym`); both compose. JS
  564 → 568.
- **2026-06-22r** — Whitespace-before-postfix-opener: `f (a)`⇒`(call f (error-t)
  a)`, `a [i]`/`S {a}`/`f. (x)`; `parse_postfix` splices the marker when
  `open_idx > lhs.end`; array-mode space-split (`[f (x)]`⇒`(hcat f x)`) untouched.
  JS 559 → 564.
- **2026-06-22q** — `var"…"` glued-suffix: `var"x"y`⇒`(var x (error-t))`;
  `parse_string_literal`'s close-delim arm pushes the glued token as a sibling +
  appends `ERROR_TRIVIA`, `project_var` emits `(error-t)`. JS 556 → 559.
- **2026-06-22p** — Unterminated-string: `"str`⇒`(string "str" (error-t))`,
  `var"x`⇒`(var x (error-t))`; `with_error_trivia` appends the marker + drops a
  sole filler `""`; single-quoted strings span literal newlines (consume to EOF).
  JS 555 → 556.
- **2026-06-22o** — Typed error-node taxonomy (Phase 0). New `ERROR_TRIVIA`
  (`(error-t)`, the `TRIVIA_FLAG` truncation marker) before the `ERROR` sentinel;
  `project_error(head, node)` wraps recovered tokens; harness `render()` made
  total; harvest kept `(error …)` cases → JS corpus 575 → 685 (+110 = the visible
  backlog). First slice: unterminated arglist `f(a`⇒`(call f a (error-t))`. JS
  553 → 555.

**Pre-error-shape feature work** (2026-06-17a through 2026-06-22n, JS allow
251 → 553 — the oracle build-out, then operators, literals, strings, char/escape
decoding, macros, imports/`using`, comprehensions/generators, matrices/`ncat`,
block forms, `where`, do-blocks, splat precedence, integer-display
normalization, …) is fully recorded as `[x]` bullets in `TODO.md` and in git
history. Trimmed from this log to honor the ≤300-line cap; consult `git log
--oneline` or `TODO.md` for any specific construct.
