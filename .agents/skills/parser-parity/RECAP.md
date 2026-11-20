# parser-parity recap

Rolling log. Read top-to-bottom: queue → traps → progress → deferred ledger →
latest session → earlier sessions. **Cap: \~300 lines.** Each session adds one
full "Latest session" section, demotes the previous one to a one-liner under
"Earlier sessions", and trims the tail to stay under the cap. Detail below the
one-liner level lives in `git log` and `TODO.md`, not here.

## Queued parser targets

Only parser-owned work belongs in this queue. When parser work unlocks a
formatter, linter, or other downstream follow-up, put the active task in that
consumer's `RECAP.md` and `TODO.md` section; retain at most a historical note in
the parser session log.

Other targets come from the
classified JuliaSyntax 2 migration clusters in `tests/oracle/blocked.txt` and
`tests/oracle/juliasyntax-blocked.txt`: adjacent assignment/comparison tokens,
qualified literal macro callees, and the seven changed curated grammar cases.
Start with adjacent assignment/comparison token boundaries.

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
- **Parentheses inherit macro generator context.** `inside_brackets` controls
  newline sensitivity, not whether `for` ends macro arguments. `(@m for … end)`
  takes a loop; `f((@m for i in xs))` inherits the call's generator boundary.
- **Return operands inherit only their relevant context.** Retain index markers,
  ternary colon boundaries, and the existing generator boundary while resetting
  newline and space sensitivity. Declarations still reset index and ternary
  context. Leave separator trivia to the enclosing ternary, including comments.
- **A/B before calling a diff a regression.** Stash and re-run: most surprising
  diffs on real code are pre-existing error shapes.
- **Reseed allowlists with the `grep -E '^#|^$'` header-preserving recipe.**
- **Reports are gitignored and untracked** (force-added once, untracked again in
  2026-08-07c); `expected.sexpr` is generated — never hand-edit.
- **Shell `raw"""…"""` Julia probes break on `"`/`$`** — use a temp file.
- **Corpus pinned** in `.juliasyntax-source` (JuliaSyntax 2.0.0-DEV, Julia
  1.15.0-DEV.783, syntax version 1.15). Development updates also pin the binary
  URL, SHA-256, version, and shared Julia/JuliaSyntax commit in
  `scripts/julia-oracle.lock.json`. Only `--update` discovers a nightly. Bumps
  require corpus/table regeneration and triage. Probes need the matching
  checkout; the root project selects JuliaSyntax 1.0.2.

## Progress

JS corpus (**907 cases**, error shapes included): **885 allowlisted**, 22
classified divergences, 0 unsupported. All 756 original inputs remain, including
six removed from upstream's harvested binding, and all 749 original passing
cases remain allowlisted. Dir corpus (**299 cases**): **291 allowlisted**, 8
blocked. Seven curated grammar changes moved to blocked with identical parser
fixtures and reviewed CST snapshots; numeric display remains blocked. Both
corpora enforce complete triage and reject stale blocked entries.

Markdown stdlib corpus: **23 fixtures**, pinned to Julia 1.13.0. Wrapped-inline
and admonition-fence fixtures also check CRLF projections and lossless
reconstruction.

**Divergence-ledger audit (2026-06-24, COMPLETE):** the old "deliberate, do not
fix" list was mostly mislabeled for a linter/LSP. All three correctable items
were fixed — `&&`/`||` associativity (a *bug*, C1), comparison chains (C3),
arithmetic `+`/`*` flattening (C2). What remains genuinely permanent is float
display. Plan `~/.claude/plans/yes-let-s-do-it-ticklish-deer.md` fully executed.

## Deferred ledger

Permanent (never "fix"): **float-literal display** (hex floats/
`1.0e-1000`/`x.3` — needs Julia's `show`); **n-ary juxtaposition** `(2)(3)x`;
**`x 'y`** char lexing (needs bracket-depth-aware `'`, and also leaves
`10.0f100'` divergent after its overflow diagnostic). These account for the
original 7 JS divergences; the current blocked lists classify the migration's
additions.

Modeling divergences, recorded not fixed: word-op chains `a isa b isa c` and
mixed `a < b isa c` stay nested (separate `word_operator` branch).

Error shapes, deferred (in scope, just low-value — see `SKILL.md`'s ranking):
`for i ∈ a ∈ b`/`for i in a in b` (Julia parses the iterable below comparison
precedence); toplevel `const x = 1 for i in 1:1`; `@m const x = 1 for … end`
macro space-arg; `else #= c =# if x`; trailing comma at EOF (`x = a,`); `2macro`
trailing-junk glyph drop; `@M.(x) y` (macro args after a broadcast);
for/let/module/struct/try/do block junk (sibling `ERROR` is in the CST but those
projectors don't emit it); `outer x=1` stop-at-`=`; bare `struct` keyword;
`begin`/`while` empty body (`while end` recovers differently:
`(while (error end) (block (error)) (error-t))`); truncated `function f` ⇒
`(error f)`; toplevel EOF-terminated incomplete ternary;
`::`/`->`/word-op/`where` missing-rhs still take `error_expr_to_line_end` rather
than the shared `(error)` synthesis; `;` or nested brackets inside a junk run;
`try x finally z else y end` (else after finally); `;`-segment double-`✘`;
prefix `**a`/`--a` (`call-pre`, in neither corpus); trailing block-body junk
(`function f g h end`); missing bare-tuple item in `(@m a\n b,)`; stray closing
keyword in `(@m x\n end)`. The last two remain pre-existing divergences in the
multiline-macro probe set.

## Latest session (2026-11-20, import commas after block comments)

- Parse a clause after a comma even when a block comment precedes it. The CST
  preserves the comment and gives the following clause its own import path.
  The pinned fixture covers aliases, nested multiline comments, and statement
  termination at a newline outside the comment.
- The curated corpus gained one passing fixture. The harvested corpus stays at
  885 of 907.

## Earlier sessions

- **Statement tuples and type group docstrings** (2026-11-18): parsed bare
  returns in statement-level tuples and gated glued docstrings. 885 / 290.

- **Declaration and block recovery** (2026-11-18): recovered old-edition type
  groups, reserved declaration names, and leading return-block commas. 885 / 287.

- **Edition review** (2026-11-18): covered 1.13 loop-exit delimiters and
  `typegroup` argument names in the pinned oracle. 885 / 284.

- **Edition review** (2026-11-17): preserved ternary colons after loop exits and
  recorded wrapping-operator edition diagnostics. 885 / 281.

- **Julia syntax editions** (2026-11-17): added explicit parser and reparse
  options, edition-specific oracle projections, and eight pinned fixtures.

- **Unicode 18 lexer** (2026-11-17): regenerated pinned identifier/operator tables and reviewed JuliaSyntax parity; details remain in the parser commit.

Newest first; one line each. Counts are `JS allowlist` / `dir allowlist` after.

- **2026-11-17** — return operands retain index and ternary context, trivia,
  newline and declaration boundaries, with 76 oracle probes. 885 / 270.

- **2026-11-17** — parenthesized multiline macros, 58 harvested cases, delimiter
  recovery, and formatter argument fences. 885 / 269.

- **2026-11-17** — locked JuliaSyntax 2 oracle migration, encoding translation,
  complete divergence triage, and unchanged replay of generated artifacts. 827 / 267.

- **2026-11-16** — macro newline operator boundaries and formatter operand
  fences, with comments, CRLF, incremental edits, and both Julia oracles. 749 / 274.

- **2026-11-16** — labeled keyword tuple precedence, trailing-junk recovery, EOF
  diagnostics, and 63 development-oracle probes. 749 / 273.

- **2026-11-16** — dotted wrapping arithmetic operators, canonical broadcast
  semantics, 52 development-oracle expressions, and incremental coverage. 749 / 273.

- **2026-11-16** — locked development Julia binaries, archive verification,
  matching JuliaSyntax source, and NixOS devenv provisioning. 749 / 273.

- **2026-11-16** — weekly development-oracle PRs, isolated generation, explicit
  syntax version, and replayable provenance. Development probe harvested 901
  cases and changed 54 curated expectations; committed gates stayed 749 / 273.

- **2026-10-29** — Markdown admonition fences, margins, unclosed-container
  recovery, and exact source ranges. Markdown 23 fixtures; 749 / 273.

- **2026-10-29** — wrapped Markdown inlines and exact decoded ranges across
  paragraphs, lists, admonitions, quotes, and footnotes. 749 / 273.

- **2026-10-19** — standalone colon whitespace before delimiters, separators,
  comments, and EOF, preserving invalid quote diagnostics. 749 / 273.

- **2026-10-18** — nested-block reparsing with context and seam guards, sibling
  reuse, and diagnostic-stream coverage; measurements in `bench/reparse.json`.
  Body edges and enclosing recovery still fall back. 749 / 272.

- **2026-09-29** — radical juxtaposition, grouping, and unary precedence, with
  two reviewed parser/oracle fixtures. 749 / 272.

- **2026-09-28** — issue #113: parenthesized macro loop arguments and
  newline-split declarations. 749 / 270.

- **2026-09-15** — bare keyword tuple recovery and removal of duplicate
  diagnostics from tentative bracket parses. 749 / 268.

- **2026-09-15** — Julia 1.13 / Unicode 17 tables, Markdown em dashes, and
  deferred-target audit; JuliaSyntax remains pinned at 1.0.2. 749 / 267.

- **2026-08-29** — decimal float overflow diagnostics and boundary fixtures. 749
  / 266.

- **2026-08-27** — list-item indented code below list items. 748 / 265.

- **2026-08-27** — imported macro aliases on the right of `as`. 748 / 265.

- **2026-08-25n** — raw triple-string backslash-run decoding before quote
  display escaping. 748 / 264.

- **2026-08-25m** — nested space-form macros give a following `for` loop to the
  innermost macro without swallowing bracket or call generators. 748 / 263.

- **2026-08-25l** — ordinary triple-string literal quote display, preserving
  source escapes and even backslash runs. 748 / 262.

- **2026-08-25k** — outer-group row-major matrix continuation for plain, typed,
  and brace concatenations. 748 / 261.

- **2026-08-25j** — dotted Unicode unary `.±`/`.∓`/`.⋆`, plus newline stopping
  for suffixed value-form unary operators. 748 / 260.

- **2026-08-25i** — whitespace-separated calls for binary-only operators recover
  with an opener diagnostic. 748 / 259.

- **2026-08-25h** — parenthesized leading commas recover as one flat
  trailing-junk node. 748 / 258.

- **2026-08-25g** — leading empty slots in bracket/brace lists and call-argument
  recovery. 748 / 257.

- **2026-08-25f** — `var"…"` names work as empty-body `function`/`macro` forward
  declarations and invalid body-bearing bare signatures. 748 / 256.

- **2026-08-25e** — quoted import names stay within `IMPORT_PATH` with
  invalid-name recovery. 747 / 255.

- **2026-08-25d** — singleton parenthesized macro-call and interpolation
  function signatures recover as invalid parameter tuples. 745 / 254.

- **2026-08-25c** — bare `:=` and `.` recover as lone syntactic operators while
  remaining valid quoted symbols. 743 / 253.

- **2026-08-25b** — numeric arguments glued to prefixed command literals now
  stay inside the command-macro node. 741 / 252.

- **2026-08-25** — parenthesized anonymous-function parameters become tuples
  before `->`, except for transparent `where` signatures. 739 / 251.

- **2026-08-24b** — generator parameters after `;`: typed curlies use the
  argument-list path, while invalid parenthesized suffixes recover in-place. 738
  / 250.

- **2026-08-24** — JuliaSyntax 1.0.2 migration: updated projection encodings,
  recovery, value operators, and the expanded corpus. 736 / 249.

- **2026-08-07c** — `∈` as the iteration separator; loop-variable parsing stops
  before Unicode `∈`, and the formatter normalizes the new flat shape. 677 / 249.

- **2026-08-07b** — contextual `outer` loop bindings and the `OUTER_BINDING`
  node, with speculative pattern parsing. Fixture `for_outer_binding`. 677 / 248.

- **2026-08-07** — keyword statements preserve generator boundaries without
  gating the operand. Fixture `keyword_stmt_generator_body`. 677 / 247.

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

- **2026-06-17...07-20** — oracle build-out, recovery, operator families,
  juxtaposition, bindings, macro boundaries, and continuation fixes. JS 251 ->
  677; dir -> 199. Details live in git history.
