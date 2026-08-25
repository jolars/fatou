# parser-parity recap

Rolling log. Read top-to-bottom: handovers → traps → progress → deferred ledger
→ latest session → earlier sessions. **Cap: \~300 lines.** Each session adds one
full "Latest session" section, demotes the previous one to a one-liner under
"Earlier sessions", and trims the tail to stay under the cap. Detail below the
one-liner level lives in `git log` and `TODO.md`, not here.

## Queued handovers

**To the formatter skill** (parser side landed; formatter may not have consumed
them yet):

- **2026-07-07** — splat after a closing bracket works (`f(g(x)...)`,
  `f(a[i]...)`, `f((a + b)...)`, `f(A{T}...)`, `f([1, 2]...)`, `f(2...)`,
  `f("a"...)` all `SPLAT_EXPR` ⇒ `(... operand)`). Drop `lower_splat`'s
  `ends_in_bracket` guard and widen `splat_spacing/`.
- **2026-07-06b** — multi-binding `let` wraps every binding in its own node, so
  the width-driven reflow can iterate `LET_BINDINGS.children()` instead of flat
  tokens.
- **2026-07-05** — braces comprehension `{a\nfor b in c}` parses, so the
  exploded too-wide form reparses cleanly; widen the braces case of
  `comprehension_index_break/`.
- **2026-07-05b** — `<--`/`.<--`/`.<-->` lex as arrow-tier tokens;
  `arrow_pair_chain/` may include `<--` (`binary_prec_class` already updated).
- **2026-06-26** — left-division `\` is a normal spaced binop (`A\b` → `A \ b`,
  no `is_tight_binop` entry); spacing fixture available.
- **2026-06-29** — `global a, b = 1, 2` nests properly, so the existing
  keyword-stmt → `lower_binary` path should format it for free.

**Parser targets**: none queued. Per `SKILL.md`, find one by probing real Julia,
or take a direct ask. The deferred ledger below is the fallback, not a queue.

## Persistent traps & invariants

- **Projector is faithful, never compensating.** Translate encoding (wrappers,
  delimiters, trivia) only; let modeling divergences surface. **Amended
  (2026-06-23i):** the projector also *reconstructs error shapes*
  (`(error)`/`(error-t)`/`✘`) from the **diagnostics side-channel**
  (`ParseOutput.diagnostics`, keyed by byte position) — the rust-analyzer model
  (missing = absence + diagnostic, no zero-width CST marker nodes). Replaying a
  *recorded* diagnostic is OK; inventing structure to paper over a wrong CST
  topology is not. A non-error divergence living mostly in `sexpr.rs` is a
  smell.
- **Error recovery is a side-channel, not a tree node.** `DiagnosticKind`
  (`diagnostics.rs`) classifies every recovery; `diag_at`/`diag_count_from`/
  `is_recovery_error` (in `sexpr.rs`) look them up by byte anchor. Zero-width
  markers carry **no** node (anchor = a byte point or the construct's opening
  keyword); byte-bearing recovery (`StrayCloser`, `TrailingJunk`,
  `ImportRecoveryColon`) is a real `ERROR` node rendered with the `(error-t …)`
  head. The only CST error kind is `ERROR` (`ERROR_TRIVIA` is **deleted**).
- **5-file operator recipe**: lexer `TokKind`+lex → `syntax.rs` kind →
  `tree_builder.rs` map → `expr.rs` `infix_binding_power` → `sexpr.rs`
  `infix_head` + `is_operator`. Probe Julia for tier/associativity first.
- **A shape change can break the formatter** with every parser test green — run
  `cargo test --workspace`, and move the formatter rule to the new shape rather
  than reverting the parser.
- **A contextual keyword needs a speculative parse, not a token whitelist.**
  `outer` is the keyword only when a whole pattern follows; a whitelist
  regressed the allowlisted `dollar_infix_operator` fixture
  (`for outer $ i = 1:3`).
- **Probe whitespace-sensitive siblings** before scoping (`a[begin]` vs
  `[begin x end]`; `:foo` vs `a[:]`; `A'` vs `A '`; `[1 +2]` vs `[1 + 2]`).
- **A/B before calling a diff a regression.** Stash and re-run: most surprising
  diffs on real code are pre-existing error shapes.
- **Reseed allowlists with the `grep -E '^#|^$'` header-preserving recipe.**
- **Reports are gitignored and untracked** (force-added once, untracked again in
  2026-08-07c); `expected.sexpr` is generated — never hand-edit.
- **Shell `raw"""…"""` Julia probes break on `"`/`$`** — use a temp file.
- **Corpus pinned** to JuliaSyntax in `.juliasyntax-source` (currently
  1.0.2/Julia 1.12.6). Bump ⇒ re-run both `scripts/*.jl`, re-triage.

## Progress

JS corpus (**756 cases**, error shapes included): **743 allowlisted**, 13
divergence, 0 unsupported. Dir corpus (**254 cases**): **253 allowlisted**, 1
blocked (`numeric_literals`; FAIL not skip since `render` is total). JuliaSyntax
1.0.2 added 71 harvested cases; the 5 remaining correctable divergences are
recorded below. A green report means "no regression", not "nothing to do".

**Divergence-ledger audit (2026-06-24, COMPLETE):** the old "deliberate, do not
fix" list was mostly mislabeled for a linter/LSP. All three correctable items
were fixed — `&&`/`||` associativity (a *bug*, C1), comparison chains (C3),
arithmetic `+`/`*` flattening (C2). What remains genuinely permanent is float
display. Plan `~/.claude/plans/yes-let-s-do-it-ticklish-deer.md` fully executed.

## Deferred ledger

Permanent (never "fix"): **float-literal display** (`2.`/`1f0`/hex floats/
`1.0e-1000`/`x.3` — needs Julia's `show`); **n-ary juxtaposition** `(2)(3)x`;
**`x 'y`** char lexing (needs bracket-depth-aware `'`). These account for 8 of
the 13 remaining JS FAILs.

Modeling divergences, recorded not fixed: word-op chains `a isa b isa c` and
mixed `a < b isa c` stay nested (separate `word_operator` branch).

Unimplemented, ranked roughly by real-world value:

- JuliaSyntax 1.0 error/value shapes: parenthesized `@f(x)`/`$f` function
  signatures; quoted names in `using :A`/`using A: :b`; and
  `function var"." end`.
- `x.function` — a reserved keyword as a field name after `)`.
- `end` inside a nested `[…]` within an index (`df[[1; 2; end:-1:3], :]`).
- `primitive type T (18 * 8) end` — the size expr is a spaced group, not a call.
- Spaced non-unary operator: `* (a, b)`/`≠ (a, b)` ⇒ `(call-pre (error *) …)` vs
  Julia's `(call op (error-t) a b)`. Dotted unary `.±`; suffixed `±₁ x`.
- Suffixed-unary prefix arm still consumes its operand across a newline (the
  2026-06-26b fix's sibling; rarer, in neither corpus).
- `$=`; bare broadcast shifts `.<<`/`.>>`/`.>>>`.
- Matrix continuation whose establishing space lives in an *outer* group
  (`[a b ;;; c ;; \n d]`) — projection-only, low priority.
- One formatter idempotency drift in DataFrames' `src/dataframe/dataframe.jl`.

Error shapes, deferred (in scope, just low-value — see `SKILL.md`'s ranking):
`for i ∈ a ∈ b`/`for i in a in b` (Julia parses the iterable below comparison
precedence); toplevel `const x = 1 for i in 1:1`; `@m const x = 1 for … end`
macro space-arg; `else #= c =# if x`; trailing comma at EOF (`x = a,`); `2macro`
trailing-junk glyph drop; `@M.(x) y` (macro args after a broadcast); leading
empty comma slot `[,x]`; for/let/module/struct/try/do block junk (sibling
`ERROR` is in the CST but those projectors don't emit it); `outer x=1`
stop-at-`=`; bare `struct` keyword; `begin`/`while` empty body (`while end`
recovers differently: `(while (error end) (block (error)) (error-t))`);
truncated `function f` ⇒ `(error f)`; toplevel EOF-terminated incomplete
ternary; `::`/`->`/word-op/`where` missing-rhs still take
`error_expr_to_line_end` rather than the shared `(error)` synthesis; `;` or
nested brackets inside a junk run; `try x finally z else y end` (else after
finally); `;`-segment double-`✘`; prefix `**a`/`--a` (`call-pre`, in neither
corpus); trailing block-body junk (`function f g h end`).

## Latest session (2026-08-25c — bare `:=` and `.` recovery)

Landed bare `:=` and `.` as syntactic operators without value meaning. They now
recover as `(error op)` in every probed value position while retaining their
existing quoted-symbol behavior.

- **Parser gap**: `is_lone_error_operator` now classifies `ColonEq` and `Dot`,
  producing the existing `ERROR > OPERATOR_ATOM` shape and `LoneOperator`
  diagnostic. `parse_macro_args` yields a dot before `(` to the postfix chain so
  the dedicated `@M.(x)` recovery remains intact.
- **Fixtures**: added parser snapshot and oracle slug `lone_colon_eq_dot`,
  covering top-level, parenthesized, collection, call-argument, assignment-left,
  trailing-junk, and quoted forms. No blocked entry was added.
- **Counts**: JS **741/756 → 743/756** allowlisted (13 FAIL, 0 unsupported,
  zero regressions); dir **252/253 → 253/254** allowlisted (only the existing
  blocked numeric-display case fails).
- **Next**: parenthesized `@f(x)`/`$f` function signatures—the next two-case
  JuliaSyntax 1.0 cluster. Quoted import names follow it.

## Earlier sessions

Newest first; one line each. Counts are `JS allowlist` / `dir allowlist` after.

- **2026-08-25b** — numeric arguments glued to prefixed command literals now
  stay inside the command-macro node. 741 / 252.
- **2026-08-25** — parenthesized anonymous-function parameters become tuples
  before `->`, except for transparent `where` signatures. 739 / 251.
- **2026-08-24b** — generator parameters after `;`: typed curlies use the
  argument-list path, while invalid parenthesized suffixes recover in-place.
  738 / 250.
- **2026-08-24** — JuliaSyntax 1.0.2 migration: updated projection encodings,
  recovery, value operators, and the expanded corpus. 736 / 249.
- **2026-08-07c** — `∈` as the iteration separator; loop-variable parsing stops
  before Unicode `∈`, and the formatter normalizes the new flat shape. 677 / 249.
- **2026-08-07b** — `for outer i` iteration spec. New `OUTER_BINDING` node;
  JuliaSyntax nests `outer` around the *variable*, inside the `=`
  (`(= (outer i) …)`), so the pattern parses at `COMMA_ITEM_BP` and the spec `=`
  is consumed as a loose separator. `outer` is contextual, detected by a
  speculative parse. Linter needed no change. Fixture `for_outer_binding`. 677 / 248.
- **2026-08-07** — keyword statement as a comprehension/generator body.
  `[const x = 1 for i in 1:1]` swallowed the `for` as loose tokens;
  `KwStmt::ExprTuple` gained `for_ends`, set via
  `!stmt_comma ||   kw_generator_body` (keying on `inside_brackets` would have
  missed `[…]`). Operand position deliberately not gated. Fixture
  `keyword_stmt_generator_body`. Drive-by: fixed
  `update-juliasyntax-corpus.jl`'s stale pre-crate-split `CORPUS_DIR`. 677 / 247.
- **2026-07-30** — prefix-operator space-arg then whitespace opener
  (`@jl_assert !is_leaf(st) (st, "msg")`). `parse_prefix`'s unary arm hard-coded
  `array_mode: false`; inherit instead. Fixtures `macro_space_args`,
  `array_space_call`. 677 / 244.
- **2026-07-28b** — quoted syntactic operators `:(.=)`, `:(.)`, `:(...)`.
  `is_paren_quotable_op` gained the assignment/dotted forms and the arm now
  wraps in `OPERATOR_ATOM`; `paren_operator` descends one level.
  `JuliaLang/julia` scan 69 → 58 failures. 677 / 219.
- **2026-07-28** — a block comment after a block keyword swallowed the header
  (issue #42). New `skip_ws_and_block_comments` swapped in at every header-start
  site; `let` had been moving a binding out of `LET_BINDINGS` silently. Fixture
  `keyword_header_block_comment`. 677 / 219.
- **2026-07-27b** — lambda arrow `->` precedence. Not an ordinary arrow-tier op:
  asymmetric tier `(35, 1)` — binds a tight left operand, sweeps everything
  looser into its body. Fixture `arrow_precedence`. 677 / 213.
- **2026-07-27** — four DataFrames gaps (issue #19): `.!` broadcast unary-not;
  `.|=`/`.&=`; assignment as a ternary branch (`TERNARY_BRANCH_BP`); macro
  space-args swallowing a generator `for`. 677 / 206.
- **2026-07-20** — Unicode operators as call names and value atoms
  (`≠(a, b) = …`, `a[≤]`, `filter(≥(3), xs)`); `± ∓ ⋆` follow the `+`-style
  paren-call heuristic. Fixture `unicode_operator_call`. 677 / 199.
- **2026-07-09** — name-list comment continuation (`export Core,\n #c\n Any`);
  two call sites moved to `skip_trivia`. 677 / 198.
- **2026-07-07b** — space-sensitive macro space-form args (`@foo f (x)`);
  `array_mode: true` at both `parse_macro_args` sites. 677 / 197.
- **2026-07-07** — splat after a closing bracket; excluded `DotDotDot` in
  `should_juxtapose`. 677 / 196.
- **2026-07-06b** — multi-binding `let` per-binding nodes; `parse_header`'s
  general path became a loop, `project_let_bindings` collapsed to `children()`.
  677 / 195.
- **2026-07-05b** — left-arrow `<--` family via the 5-file recipe. 677 / 194.
- **2026-07-05** — newline-broken braces comprehension; `parse_braces` gained
  the two newline-lookahead arms `parse_bracket_literal` had. 677 / 193.
- **2026-07-03** — `@macro` as a macro name; `macro` was missing from *both*
  `is_keyword` matchers. 677 / 192.
- **2026-07-02d** — newline-after-comma continuation (bare tuple, `let`
  bindings, `import`/`using` paths); post-separator lookup moved to
  `skip_trivia`. 677 / 191.
- **2026-07-02c** — compound shift/Unicode augmented assignments (`<<=`, `>>=`,
  `>>>=`, `÷=`, `⊻=` + broadcast forms), 5-file recipe ×10. 677 / 188.
- **2026-06-29b** — one-line space-separated `for` body; `parse_for_specs`
  gained `bracketed`. 677 / 187.
- **2026-06-29** — `global`/`local` + multiple assignment; switched to
  `KwStmt::ExprTuple`, `project_decl` splices a bare tuple. 677 / 186.
- **2026-06-26b** — prefix operators stop at a significant newline (correctness
  fix + false-positive diagnostic removal). 677 / 185.
- **2026-06-26a** — left-division `\` family (`\`, `\=`, `.\`, `.\=`);
  longest-match forced the whole family. 677 / 184.
- **2026-06-25l** — broadcast identity ops `.===`/`.!==`; also fixed
  `is_operator_kind` missing `EQ_EQ_EQ`/`NOT_EQ_EQ`. 677 / 183.
- **2026-06-25k** — `===`/`!==`/`!=`; the crux was `scan_ident` stopping at `!`
  immediately followed by `=` (so `f!`/`push!` still lex). 677 / 182.
- **2026-06-25j** — projector faithfulness audit, no parser change: classified
  every non-trivial arm, probed each non-local one. **Zero latent CST bugs.**
- **2026-06-25i** — `.'` → trailing-junk recovery; lexer `prev_is_dot()`. 677 / 181.
- **2026-06-25h** — misplaced `end` in a space-separated array. 676 / 180.
- **2026-06-25g** — leading-`@` dotted macro `$`/inner-`@` sigil reflow. 675 / 179.
- **2026-06-25f** — misplaced macro sigil `A.@B.x` (trailing form). 673 / 178.
- **2026-06-25e** — broadcast call on a macro name `@M.(x)`. 672 / 177.
- **2026-06-25d** — bare block keyword `function`/`macro` empty recovery. 671 / 176.
- **2026-06-25c** — incomplete ternary recovered as `if`, decided *locally* by
  the terminator. 670 / 175.
- **2026-06-25b** — array `;;` line continuation → `hcat` (valid syntax, no
  diagnostic). 666 / 174.
- **2026-06-25a** — invalid bracketed macro name `@[x]`. 665 / 173.
- **2026-06-24a…p** — the error-shape campaign's second half: parenthesized
  `export` item; `:(end)`; glued colon `:<`/`:>`; docstring + stray closer;
  unterminated char literals; C2 flat arithmetic chains; C3 flat comparison
  chains; `&&`/`||` right-associativity (C1); `end`/`begin` marker scoped to
  genuine `ref`; prefix-op spaced call-form paren; `: end` bare Colon atom;
  doubled operators `**`/`--`; stray middle/closing block keyword;
  non-identifier `catch` var; string-escape error classification; bare-name
  `function` signature. JS 640 → 665, dir 155 → 173.
- **2026-06-23a…z** — the error-shape lineage proper, including the
  **2026-06-23i architecture reversal** to the rust-analyzer model (deleted
  `ERROR_TRIVIA`; the zero-width markers became diagnostics-only, reconstructed
  by the projector). Covers missing operand/condition `(error)`, `else if`
  recovery, array separator mismatch, trailing-junk runs, lone syntactic
  operators, char/string error classification, `import`/`as` colon shapes,
  incomplete `try`, missing `end`. JS 553 → 640, dir 128 → 155.
- **2026-06-22o…v** — Phase 0 of the error-shape work: typed error-node
  taxonomy, total `render()`, and the harvest that kept `(error …)` cases (JS
  corpus 575 → 685). JS 553 → 576.
- **2026-06-17a…2026-06-22n** — pre-error-shape feature work, JS allowlist 251 →
  553: the oracle build-out, then operators, literals, strings, char/escape
  decoding, macros, imports/`using`, comprehensions/generators, matrices/`ncat`,
  block forms, `where`, do-blocks, splat precedence, integer-display
  normalization. Fully recorded as `[x]` bullets in `TODO.md` and in git
  history.
