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

JS corpus (**685 cases** — error shapes now harvested): **582 allowlisted**,
103 divergence, 0 unsupported. Dir corpus: **127 allowlisted**, 3 blocked
(do_blocks/end_index/numeric_literals; all FAIL not skip since `render` is
total). Grammar bullets through "bare `:` colon value atom"
are `[x]` in `TODO.md`.

Deliberate (recorded) divergences, do not "fix": comparison chains (nested),
associative `a*b*c` (nested binary), n-ary juxtaposition `(2)(3)x` (nests right),
**float**-literal display normalization (`2.`/`1f0`/hex floats/`1.0e-1000`; the
integer half is now handled — see latest session),
`end`/`[1 +2]`/unterminated-string/incomplete-`do` error shapes (dir `blocked.txt`).

## Latest session (2026-06-22x)

**Bare `:` colon value atom — root-cause modeling fix.** A prefix `:` not
followed by something quotable is the Colon *value* atom, not a quote. Previously
`parse_quote_sym` returned `None` and the `TokKind::Colon` arm in `parse_prefix`
short-circuited on that `None`, so the bare `:` token was left loose and dropped
by the projector's delimiter filter: `a[:]`⇒`(ref a)`, `[:]`⇒`(vect)`, `:`⇒
`(toplevel)`, `:;`⇒`(toplevel-;)` all silently lost the colon. Fix is one
`.or_else(|| Some(atom(OPERATOR_ATOM, start)))` — when the quote parse declines, a
bare `:` becomes an `OPERATOR_ATOM` (which `project_operator_atom` renders as `:`).
Now `a[:]`⇒`(ref a :)`, `[:]`⇒`(vect :)`, `a[:, :]`⇒`(ref a : :)`, `f(:)`⇒
`(call f :)`, `(:)`⇒`:` (unchanged — its own lone-operator-paren path), `:`⇒`:`.
This *also* fixed the recap's ranked target `:)`⇒`(toplevel : (error-t ✘))`: with
the colon now a real statement, the toplevel leftover driver sets `leftover_mark`
after it and wraps the trailing `)` as `(error-t ✘)`. **Pure `expr.rs` change**,
no projector edit — a genuine modeling gap, not error-shape work. Fixtures
`colon_value_atom`, `colon_stray_close`. JS allow 581 → 582 (js-54d626e0 `:)`);
dir 125 → 127. The `a[:]`-family cases aren't in the JS corpus, so the dir
fixtures are what lock them. Zero regressions; green; clippy/fmt clean.

**Suggested next targets (ranked):** (1) **stray-delim not-yet-wrapped (rest)** —
`return)`⇒`(return) (error-t ✘)` (rparen absorbed *into* `RETURN_EXPR` via
`parse_keyword_stmt`'s carry-verbatim loop at `structural.rs:553`; `return` has an
*optional* value so stopping the header at a close delimiter yields the clean
shape — but `const`/`global`/`local`/`export` need the larger inner-`(error)`
synthesis, so scope to optional-value keywords, don't flip shared `header_ends`),
and `)`⇒`(error) (error-t ✘)` (lone closer needs a synthesized leading `(error)`
placeholder — a distinct mechanism). (2) **paren-block string-juxtapose**
`(begin end)"x"`⇒`(block) (error-t ✘ "x" ✘)` (needs the double-`✘`-wrapped string
form). (3) **macro-path error-t** `A.@B.x`⇒`(macrocall (. (. A (quote B))
(error-t) (quote @x)))`, `@A.B.@x a`. (4) **incomplete `do`**⇒`(block (error))`
(unblocks dir `do_blocks`). (5) **lexer-classified named kinds** (`'ab'`⇒
`(char (ErrorOverLongCharacter))`, `a--b`⇒`(call-i a (ErrorInvalidOperator) b)`).

## Earlier sessions

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
