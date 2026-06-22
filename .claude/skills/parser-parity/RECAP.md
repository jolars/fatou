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

JS corpus (**685 cases** — error shapes now harvested): **583 allowlisted**,
102 divergence, 0 unsupported. Dir corpus: **128 allowlisted**, 3 blocked
(do_blocks/end_index/numeric_literals; all FAIL not skip since `render` is
total). Grammar bullets through "optional-value-keyword stray-closer"
are `[x]` in `TODO.md`.

Deliberate (recorded) divergences, do not "fix": comparison chains (nested),
associative `a*b*c` (nested binary), n-ary juxtaposition `(2)(3)x` (nests right),
**float**-literal display normalization (`2.`/`1f0`/hex floats/`1.0e-1000`; the
integer half is now handled — see latest session),
`end`/`[1 +2]`/unterminated-string/incomplete-`do` error shapes (dir `blocked.txt`).

## Latest session (2026-06-22y)

**Optional-value-keyword stray-closer `✘`.** `return` followed by a stray closing
delimiter now ends the empty `(return)` form right after the keyword, leaving the
delimiter for the toplevel-leftover driver to wrap—exactly the `break)` shape:
`return)`⇒`(return) (error-t ✘)`, `return ]`/`return}`, `return) x`⇒`(return)
(error-t ✘ x)`. Previously `return`'s `KwStmt::ExprTuple` operand parse declined
on the `)` and the carry-verbatim loop pushed it *into* `RETURN_EXPR`. Fix: new
`optional_value: bool` param on `parse_keyword_stmt` (`structural.rs`); when set
and the operand position (after ws) is a close delimiter (`is_close_delimiter_tok`,
`)`/`]`/`}`), the node finishes at `start+1` and returns, so the ws+delimiter fall
to the driver. Only `return` passes `true`; `const`/`global`/`local` pass `false`
and keep their loose shape (they're value-required → need the separate inner
-`(error)` synthesis, out of scope). `break`/`continue` are `KwStmt::Bare` and
already produced this shape. **Pure `expr.rs`+`structural.rs` change**, projector
untouched. Fixture `return_stray_close`. JS allow 582 → 583 (js-b125918f
`return)`); dir 127 → 128. Zero regressions; green; clippy/fmt clean.

**Suggested next targets (ranked):** (1) **lone closer `)`**⇒`(error) (error-t ✘)`
— a stray closer at *statement-start* (no preceding stmt) synthesizes a leading
empty `ERROR` node (projector already renders empty `ERROR`⇒`(error)`) and the
closer-run becomes the `(error-t ✘ …)`; it **swallows the rest of the line**
(`) x`⇒`(error) (error-t ✘ x)`, not a separate `x` stmt; `)))`⇒`…✘ ✘ ✘`). Lives
in the `core.rs` driver's `parse_stmt`-returns-None branch (line ~66). **Trap:**
the `;`-segment forms emit a subtle double marker (`) ; x`⇒`(error) (error-t ✘ ✘
x)`, `x; )`⇒`(toplevel-; x (error) (error-t ✘))`)—probe `;` interplay before
scoping; the bare-line form is the clean slice, defer `;` if it fights.
(2) **paren-block string-juxtapose** `(begin end)"x"`⇒`(block) (error-t ✘ "x" ✘)`
(double-`✘`-wrapped string form). (3) **macro-path error-t** `A.@B.x`⇒`(macrocall
(. (. A (quote B)) (error-t) (quote @x)))`, `@A.B.@x a`. (4) **incomplete `do`**⇒
`(block (error))` (unblocks dir `do_blocks`). (5) **lexer-classified named kinds**
(`'ab'`⇒`(char (ErrorOverLongCharacter))`, `a--b`⇒`(call-i a (ErrorInvalidOperator)
b)`).

## Earlier sessions

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
