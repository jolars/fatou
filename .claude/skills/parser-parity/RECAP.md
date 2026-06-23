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

JS corpus (**685 cases** — error shapes now harvested): **599 allowlisted**,
86 divergence, 0 unsupported. Dir corpus: **139 allowlisted**, 2 blocked
(end_index/numeric_literals; both FAIL not skip since `render` is total).
Grammar bullets through "import/as colon error shapes" are `[x]` in `TODO.md`.

Deliberate (recorded) divergences, do not "fix": comparison chains (nested),
associative `a*b*c` (nested binary), n-ary juxtaposition `(2)(3)x` (nests right),
**float**-literal display normalization (`2.`/`1f0`/hex floats/`1.0e-1000`; the
integer half is now handled), `end`/`[1 +2]`/unterminated-string error shapes
(dir `blocked.txt`).

## Latest session (2026-06-23h)

**`import`/`as` colon error shapes.** Finished the deferred import/`as` recovery
work. Three intertwined rules, all matched: (a) a top-level `:` is the base/names
split **only as the first separator** (right after the base path) — after a comma
any `:` is recovery, so `import A, B: y` ⇒ `(import (importpath A) (importpath B)
(error-t (importpath y)))` with **no** `:` group; (b) a *second* colon in the
names list is recovery too — `import A: x, B: y` ⇒ `(: A x B (error-t y))`,
`import A: x: y` ⇒ `(: A x (error-t y))`; (c) a base alias that precedes a valid
`:` is invalid — `import A as B: x` ⇒ `(import (: (error (as …)) x))`, and a
`using` base alias before a `:` **stacks** both invalidities, `using A as B: x` ⇒
`(error (error (as …)))`. `parse_import_stmt` (`structural.rs`) probes the base
clause's following separator (`valid_colon`), passes an error-wrap **depth**
(0/1/2) to `parse_import_clause` (replacing the old `wrap_alias_error: bool`), and
wraps the clause after a recovery colon in `ERROR_TRIVIA` (then stops grouping).
`project_import` (`sexpr.rs`) now reads the **first separator** token to decide
`:` grouping (not "any colon present") and collects `ERROR`/`ERROR_TRIVIA`
clauses. No diagnostics emitted. Fixture `import_as_colon_error`; dir case minted.
JS allow 597 → 599; dir 138 → 139. Zero regressions; green; clippy/fmt clean.

**Deferred (still divergent, not allowlisted):** the *triple* `import A: x, B: y,
C: z` nests as `(call-i (import (: A x B (error-t y C))) : z)` — a second recovery
colon spills into a binary `:` call; left as a remaining divergence. `import A as
B as C` ⇒ `(import (as A B)) (error-t as C)` (double `as`) also untackled.

**Suggested next targets (ranked):** (1) **unterminated chars** — `'`⇒
`(char (error))`, `'a`⇒`(char 'a' (error-t))`; touches the lexer + transpose
disambiguation (`f.'`, `x 'y`); probe those first. (2) **macro-path error-t**
`A.@B.x`⇒`(macrocall (. (. A (quote B)) (error-t) (quote @x)))`, `@A.B.@x a` (deep:
Fatou currently drops the trailing `.x`). (3) **paren-block string-juxtapose**
`(begin end)"x"`⇒`(block) (error-t ✘ "x" ✘)` (double-`✘` string form). (4)
**`public` soft-keyword in block context** — `begin public A, B end`⇒
`(block public (error-t A ✘ B))` (toplevel `public A, B` already works; in a
block `public` is a plain ident + error recovery).

## Earlier sessions

- **2026-06-23g** — `using`-base `as` rename error-wrap: an `as` rename is invalid
  in a `using` base path, so JuliaSyntax wraps the alias `(error (as …))`
  (`using A as B`, `using A, B as C`). `parse_import_stmt` passed a
  `wrap_alias_error` bool to `parse_import_clause`; `project_import` collected the
  `ERROR` clause. Fixture `using_as_error`. JS 595 → 597; dir 137 → 138.
  (Superseded this session: the bool became an error-wrap depth.)

- **2026-06-23f** — Char-literal error classification (closed-but-invalid
  bodies): a `'…'` whose body `decode_char` can't reduce to one codepoint maps to
  JuliaSyntax's error shapes — empty `''`⇒`(char (error))`, malformed escape
  `'\xq'`/`'\400'`⇒`(char (ErrorInvalidEscapeSequence))`, other multi-codepoint
  `'ab'`/`'αβ'`⇒`(char (ErrorOverLongCharacter))`; a lone non-UTF-8 byte
  `'\xff'`/`'\377'` stays a valid one-byte `Char`. Pure projector: the refined
  `None` arm of `project_char` delegates to `classify_char_error` (bad-escape wins
  over over-long); the octal escape now rejects values past `0xff`. Fixture
  `char_errors`. JS 592 → 595; dir 136 → 137. Deferred: unterminated chars (lexer
  work, entangled with transpose siblings `f.'`/`x 'y`).
- **2026-06-23e** — `else`-without-`catch` error-wrap (last try-family
  divergence): an `else` *before* any `catch` is recovery, so JuliaSyntax wraps
  its block in `(error …)` (`try x else y end`⇒`(try (block x) (else (error
  (block y))) (error-t))`); an `else` after a `catch` stays plain.
  `parse_try_expr` tracks `saw_catch` and wraps the else `run_block` in `ERROR`;
  the `ELSE_CLAUSE` arm of `project_try` projects it. Fixture
  `try_else_without_catch`. JS 591 → 592; dir 135 → 136. Deferred: `try x finally
  z else y end` (else after finally spills to a separate toplevel `(error-t …)`).
- **2026-06-23d** — Incomplete-`try` truncation `(error-t)`: a `try` with no
  `catch`/`finally` splices a missing-handler marker, and `expect_end` adds a
  missing-`end` one (`try x`⇒`(try (block x) (error-t) (error-t))`, `try x end`⇒
  `(try (block x) (error-t))`). `parse_try_expr` tracks `saw_handler` (catch/finally,
  not else); `project_try` renders `ERROR_TRIVIA` children in order. JS 590 → 591;
  dir 134 → 135.
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
