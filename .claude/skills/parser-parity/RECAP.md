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

JS corpus (**685 cases** — error shapes now harvested): **607 allowlisted**,
78 divergence, 0 unsupported. Dir corpus: **141 allowlisted**, 2 blocked
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

## Latest session (2026-06-23l)

**Block-body trailing junk.** JuliaSyntax ends a block at the first
separator-less glued token (`parse_Nary` only continues across a newline/`;`),
then the closing recovery (`bump_closing_token`) bumps the run as flat error
tokens up to the closing keyword. Placement splits by form: `begin`/`quote` are
modeled *as* the block, so the run lands **inside** it (`begin\n public A, B\n end`
⇒ `(block public (error-t A ✘ B))`); `if`/`while`/etc. emit the block first, so
the run is a **sibling** of it (`if true\n public A, B\n end` ⇒
`(if true (block public) (error-t A ✘ B))`). Three-part fix, **no `junk_inside`
flag needed** — the CST is uniform (junk `ERROR` is always a sibling of `BLOCK`,
child of the construct) and the *projector* decides placement: (1)
`run_block_inner` (`structural.rs`) tracks `saw_separator` and `parsed_any` and
breaks the statement loop at glued junk; (2) `expect_end` became the full close
(JuliaSyntax `bump_closing_token`): at `end` consume it, at a non-stopper bump the
run via new `collect_block_junk` (stops at `end`/`else`/`elseif`/`catch`/`finally`/
`) ] }` per new `is_block_junk_stopper`, swallows `,`/`;`/newlines) then consume
`end`, else record the existing zero-width `MissingEnd`; (3)
`project_block_child_folding_error` folds the sibling `ERROR` into the begin/quote
block (alongside the existing `MissingEnd` fold), `project_if` projects it after
the then-block. `while` auto-projects it (uses `project_each(child_nodes)`).
Fixture `block_trailing_junk`. JS 605 → 607 (js-d7b0ba21, js-803233ba); dir
140 → 141. Green; clippy/fmt clean.

**Key insight that simplified it:** the junk and missing-`end` markers never
stack — `bump_closing_token` emits *exactly one* recovery marker (the junk run
*or* the zero-width missing-`end`). So `expect_end` collects junk *xor* records
`MissingEnd`; `begin x y` (junk, no `end`) ⇒ `(block x (error-t y))`, not a
doubled marker. The uniform-CST + projector-fold approach also avoided churning
the 6 existing begin/quote snapshots (their `end` stays a child of the construct,
outside the `BLOCK`; only the malformed-junk shape changed).

**Scoping note:** still-open `✘`-glyph FAIL roots: (a) **`public`/`outer` stop at
`=`** — `parse_name_list_stmt` (shared with `export`) carries *every* same-line
token, but Julia's `public` takes bare names only and stops at `=`
(`public x=1, y` ⇒ `(public x) (error-t = 1 ✘ y)`, js-1beba79b/js-e151591c); risk:
don't regress `export`. (b) **array-internal** `[x@y]` ⇒ `(hcat x (error-t ✘ y))`
(js-6be56bdb/js-83672850). Also deferred from this session:
for/let/module/struct/try/do block junk (the sibling `ERROR` is in the CST but
their explicit projectors don't yet emit it — only `if`/`while`/`begin`/`quote`
do), and junk-then-`else` (`if true\n x y\n else z\n end`, where Julia stops the
run at `else` and re-floats `else z`/`end` to toplevel).

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
