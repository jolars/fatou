# parser-parity recap

Rolling log. Read top-to-bottom: persistent traps → progress → latest session →
earlier log. Keep ≤ ~300 lines; demote the "Latest session" to a one-liner in the
"Earlier sessions" list each new session.

## Persistent traps & invariants

- **Projector is faithful, never compensating.** Translate encoding (wrappers,
  delimiters, trivia) only; let modeling divergences surface. Diffs that live
  mostly in `sexpr.rs` are a smell. **Amended (2026-06-23i):** the projector now
  also *reconstructs error shapes* (`(error)`/`(error-t)`/`✘`) from the
  **diagnostics side-channel** (`ParseOutput.diagnostics`, keyed by byte
  position) — we adopted the rust-analyzer model (missing = absence + diagnostic,
  no zero-width CST marker nodes). The bright line is narrower now: reading
  *recorded* diagnostics to replay an error shape is OK; inventing structure to
  paper over a wrong CST topology is still forbidden. A non-error divergence that
  lives mostly in `sexpr.rs` is still a smell.
- **Error recovery is a side-channel, not a tree node.** `DiagnosticKind`
  (`diagnostics.rs`) classifies every recovery; the projector's `diag_at` /
  `diag_count_from` / `is_recovery_error` helpers (in `sexpr.rs`) look diagnostics
  up by byte anchor. Zero-width markers carry **no** node (anchor = a byte point or
  the construct's opening keyword); byte-bearing recovery (`StrayCloser`,
  `TrailingJunk`, `ImportRecoveryColon`) is a real `ERROR` node the projector
  renders with the `(error-t …)` head via `is_recovery_error`. The only CST error
  kind is `ERROR` (`ERROR_TRIVIA` is **deleted**).
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

JS corpus (**685 cases** — error shapes now harvested): **622 allowlisted**,
63 divergence, 0 unsupported. Dir corpus: **146 allowlisted**, 2 blocked
(end_index/numeric_literals; both FAIL not skip since `render` is total).
Grammar bullets through "const-not-assignment error-wrap" are `[x]` in `TODO.md`.
**Error shapes are now reconstructed from diagnostics, not in-tree marker
nodes** (2026-06-23i refactor) — same projected output, so counts unchanged.
`TODO.md`'s error-shape bullets still describe the old `ERROR_TRIVIA` mechanism
(historical log); the *output shapes* they cite are still correct.

Deliberate (recorded) divergences, do not "fix": comparison chains (nested),
associative `a*b*c` (nested binary), n-ary juxtaposition `(2)(3)x` (nests right),
**float**-literal display normalization (`2.`/`1f0`/hex floats/`1.0e-1000`; the
integer half is now handled), `end`/`[1 +2]`/unterminated-string error shapes
(dir `blocked.txt`).

## Latest session (2026-06-23q)

**Multi-value `$(…)` interpolation → `(error …)`.** A `$(…)` string interpolation
must hold a single expression; a multi-value parenthesized form is invalid and
JuliaSyntax renders the operand as `(error …)`. Fatou used to parse the `$(…)`
interior with `parse_expr_in_brackets` (one expression), which stopped dead at a
`;`/`,`/generator and leaked the rest as an unterminated-string mess. Fix:
`parse_interpolation` (`expr.rs`) now reuses the shared `parse_paren`, so the
operand becomes a faithful `PAREN_EXPR` (single expr), `PAREN_BLOCK` (`$(x;y)`),
`TUPLE_EXPR` (`$(x,y)`, empty `$()`), or `GENERATOR` (`$(x for …)`) subtree, and
records an `InvalidInterpolation` diagnostic for the three multi-value kinds.
`project_interpolation` (`sexpr.rs`) reconstructs the error from the inner node
kind — block → `project_block_args`, tuple → `project_args`, generator → keep
nested — flattening block/tuple children and unwrapping a `PAREN_EXPR` for the
valid single-expr case (`"$((x,y))"` ⇒ `(string (tuple-p x y))`, `"$(a=1)"` ⇒
`(string (= a 1))`). New `DiagnosticKind::InvalidInterpolation`. JS 619 → 622
(`"$(x;y)"`, `"$(x,y)"`, `"$(x for y in z)"`); dir 145 → 146. Fixture
`string_interp_error`. Green; clippy/fmt clean.

**Key insight:** the gap was a *parser* shortcut, not a projector gap — `$(…)`
is just a parenthesized expression, so reusing `parse_paren` makes the CST
faithful and the multi-value-is-error rule falls out of the node kind (no
invented structure; the char-error pure-projector precedent applies). The CST
shape for *all* `$(…)` interpolations changed (the parens now nest a real
`PAREN_EXPR`), so six existing interpolation snapshots were re-accepted — a
uniform, faithful wrap. Empty `$()` now correctly projects `(error)` too (not in
the corpus, so no count movement). **Deferred:** `$(x y)` juxtaposition-in-parens
→ `(parens x (error-t y))` (a different error path, not exercised here).

## Earlier sessions

- **2026-06-23p** — Lone syntactic operator → `(error op)`: a syntactic operator
  with no value meaning used where an atom is expected is `(error op)` (`=`, `+=`,
  `.+=`, `&&`, `||`, `->`, `...`, `?`/`?x`); applies in every atom position, the
  trailing operand falling to the junk driver. New `is_lone_error_operator` +
  `error_operator_atom` arms (`expr.rs`); `parse_comma_tuple` guards the
  trailing-comma destructure. Fixture `lone_operator_error`. JS 614 → 619; dir
  144 → 145.
- **2026-06-23o** — Array-internal trailing junk: glued `@` → `(error-t ✘ …)`. A
  macro `@` glued (no separating ws/`;`/newline) to a preceding array element is not
  a new row — JuliaSyntax bumps the rest of the array to the `]`/EOF as one flat
  trailing-junk run (`[x@y]` ⇒ `(hcat x (error-t ✘ y))`, `[a@b c]` ⇒ `(hcat a
  (error-t ✘ b c))`); `@`/`,` render `✘`. One arm in `parse_matrix`'s scan loop
  (`expr.rs`) on an empty separator run + `At` collects the run into one `ERROR`
  element through the existing `emit_cat_child`/`ARG` machinery — **no projector
  change**. Fixture `array_trailing_junk`. JS 612 → 614; dir 143 → 144. Deferred:
  `;` in the junk (`[a@b;c]`), nested brackets (`[a@b[c]]`).

- **2026-06-23n** — Binary-only operator in prefix position → error-wrapped prefix
  call. `/x` ⇒ `(call-pre (error /) x)`, `.*x` ⇒ `(dotcall-pre (error (. *)) x)`;
  operand binds at `PREFIX_BP` (tighter than arithmetic, below `^`); bare `*` stays
  a value atom. Fix in the `is_value_operator` arm of `parse_prefix` (`expr.rs`):
  emits `UNARY_EXPR > ERROR > OPERATOR_ATOM > op` + operand, new
  `InvalidPrefixOperator` diagnostic; `project_unary` renders the prefix-call head.
  Fixture `prefix_operator_error`. JS 609 → 612; dir 142 → 143.
- **2026-06-23m** — `public` stops at the first non-comma after a name. `public` is
  a names-only compatibility shim (JuliaSyntax `parse_public`): it ends the
  statement at the first non-comma after a complete name, and the leftover floats
  to the toplevel trailing-junk driver (`public x=1, y` ⇒
  `(public x) (error-t = 1 ✘ y)`). `export` differs (re-enters the operator parser:
  `export x=1` ⇒ `(= (export x) 1)`), so the stop is `PUBLIC_STMT`-gated. Fixes in
  `parse_name_list_stmt` (`structural.rs`) + two projector touches (`name_run_item`
  keyword-as-name arm, `project_public` keeps keyword-name tokens). Fixture
  `public_stop_at_equals`. JS 607 → 609; dir 141 → 142. Deferred: `export` operator
  re-entry, `outer` stop-at-`=`.
- **2026-06-23l** — Block-body trailing junk. A separator-less glued statement
  inside a block ends it; the closing recovery (`bump_closing_token`) bumps the run
  as flat error tokens up to the closing keyword. Uniform CST (junk `ERROR` is
  always a sibling of `BLOCK`, child of the construct); the projector decides
  placement — `begin`/`quote` fold it inside (`begin\n x y\n end` ⇒ `(block x
  (error-t y))`), `if`/`while` keep it a sibling. `run_block_inner` breaks the
  loop at glued junk; `expect_end` became the full close (`collect_block_junk` xor
  zero-width `MissingEnd`, the two never stack); `project_block_child_folding_error`
  + `project_if` render it. Fixture `block_trailing_junk`. JS 605 → 607; dir
  140 → 141. Deferred: for/let/module/struct/try/do junk (sibling `ERROR` in CST,
  not yet projected), junk-then-`else`.

**Scoping note — next-target candidates** (still-open `✘`-glyph FAIL roots):
(a) **`outer` stop-at-`=`** — `outer x=1` ⇒ `outer (error-t x = 1)` (note
`outer` itself becomes the bare value and the *whole* `x = 1` is junk, unlike
`public`). (b) for/let/module/struct/try/do block junk (sibling `ERROR` is in the
CST but their explicit projectors don't emit it — only `if`/`while`/`begin`/`quote`
do). (c) **`;;` ncat whitespace-error** — `[a b ;; c]` ⇒
`(ncat-2 (row a b (error-t)) c)`, `[a ;; b c]` ⇒ `(ncat-2 a (row b (error-t) c))`
(js-e8b41b39, js-b5967309, js-578363a4): a space-separated row adjacent to a `;;`
column separator splices a zero-width `(error-t)` whitespace marker into the row.
Sibling array junk with `;`/nested brackets (`[a@b;c]`, `[a@b[c]]`) is deferred
from this session.

## Earlier sessions

- **2026-06-23k** — Flat trailing-junk runs (toplevel): JuliaSyntax bumps a
  separator-less line's leftover as *flat error tokens*, not a re-parsed subtree
  (`x y, z` ⇒ `x (error-t y ✘ z)`, `x@y` ⇒ `x (error-t ✘ y)`); brackets/commas/`@`
  render `✘`, operators/identifiers keep text. The `core.rs` driver collects the
  run raw (no `parse_stmt`) once `leftover_mark` is set on a `;`-free,
  non-docstring line; `project_error` renders the broader glyph set via
  `is_error_glyph` (`( ) [ ] { } , @`). Gotcha: must check `!first_is_doc_string`
  (a docstring opener owns its trailing statement). Fixture
  `toplevel_leftover_error`. JS 603 → 605.
- **2026-06-23j** — `const`-not-assignment error-wrap (first error shape on the
  diagnostics model): JuliaSyntax wraps a `const` whose decl isn't a plain `=` in
  `(error …)` (`const x`⇒`(error (const x))`, `const x += 1`, `const global x`),
  but a bare `const` field *directly* in a struct body is exempt. Post-build CST
  walk `flag_invalid_const_decls` (`core.rs`) records a `ConstNotAssignment`
  diagnostic at the `const` keyword; projector's `CONST_STMT` arm wraps when
  `diag_at`. Reusable pattern: semantic error-wraps where the CST is already
  correct fit a post-build walk + projector wrap. Fixture `const_not_assignment`.
  JS 599 → 603; dir 139 → 140.
- **2026-06-23i** — Architecture reversal: error handling → the rust-analyzer
  model. Deleted `SyntaxKind::ERROR_TRIVIA`; the zero-width in-tree markers grown
  over the 2026-06-22o…2026-06-23h lineage became **diagnostics-only** (no node),
  reconstructed by the projector from the side-channel; the 3 byte-bearing
  recoveries (`StrayCloser`/`TrailingJunk`/`ImportRecoveryColon`) stay real
  `ERROR` nodes. New `DiagnosticKind` enum + `push_diagnostic(kind, …)`; projector
  gained `diag_at`/`diag_count_from`/`is_recovery_error`/`keyword_start` reading a
  thread-local `PROJ_DIAGS`; `to_juliasyntax_sexpr` takes `&[ParseDiagnostic]`.
  Same projected output ⇒ zero allowlist movement (599/139). Gotcha: `keyword_start`
  special-cases `DO_EXPR` (callee precedes `do`). Plan:
  `~/.claude/plans/yeah-we-re-heading-the-swift-blossom.md`.
- **2026-06-23h** — `import`/`as` colon error shapes (the last error-shape-lineage
  feature, before the 2026-06-23i representation reversal): a top-level `:` is the
  base/names split only as the *first* separator (`import A, B: y` ⇒ recovery, no
  `:` group); a second names-list colon is recovery; a base alias before a valid
  `:` is invalid and a `using` base alias stacks both. `parse_import_stmt` passed
  an error-wrap depth (0/1/2) to `parse_import_clause`. Fixture
  `import_as_colon_error`. JS 597 → 599; dir 138 → 139.

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
