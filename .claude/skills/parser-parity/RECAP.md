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

JS corpus (**685 cases** — error shapes now harvested): **677 allowlisted**,
8 divergence, 0 unsupported. Dir corpus: **183 allowlisted**, 1 blocked
(numeric_literals; FAIL not skip since `render` is total).
Grammar bullets through "flat comparison chains" are `[x]` in `TODO.md`. **Error shapes are now reconstructed from diagnostics, not in-tree
marker nodes** (2026-06-23i refactor) — same projected output, so counts
unchanged. `TODO.md`'s error-shape bullets still describe the old `ERROR_TRIVIA`
mechanism (historical log); the *output shapes* they cite are still correct.

**Divergence-ledger audit (2026-06-24, COMPLETE):** the old "deliberate, do not
fix" list was mostly mislabeled for a linter/LSP. All three correctable items are
now fixed: `&&`/`||` associativity was a *bug* (C1); comparison chains were a
faithfulness gap (C3); arithmetic `+`/`*` flattening (C2). The ledger now
collapses to essentially **float**-literal display normalization (`2.`/`1f0`/hex
floats/`1.0e-1000`; needs Julia's `show`) — the lone genuinely permanent
divergence. Still recorded/deferred (not "deliberate modeling", just unimplemented
or out of scope): n-ary juxtaposition `(2)(3)x` (the `(2)(3)`→`(call 2 3)`
misparse, out of scope); `end`/`[1 +2]`/unterminated-string error shapes; word-op
chains `a isa b isa c` / mixed `a < b isa c` (separate `word_operator` branch,
stay nested). Plan `~/.claude/plans/yes-let-s-do-it-ticklish-deer.md` fully
executed.

## Latest session (2026-06-25l — broadcast identity ops `.===`/`.!==`)

Landed the deferred next-pickup from 2026-06-25k: the 4-char dotted forms now
lex and project faithfully (`x .=== y` previously mis-lexed `.==` + `(error =)`).

- **Two new tokens** `DotEqEqEq` (`.===`) / `DotNotEqEq` (`.!==`), lexed as
  **4-char dotted ops** in the same block as `.//=`/`.-->` (before the 3-char
  table) so longest-match beats `.==`/`.!=`. Full 5-file recipe + sibling lists:
  `lexer.rs` (`TokKind`, 4-char lex arm, `op_takes_suffix`), `syntax.rs`
  (`DOT_EQ_EQ_EQ`/`DOT_NOT_EQ_EQ`), `tree_builder.rs`, `expr.rs`
  (`is_comparison_op`, `is_value_operator`, `is_operator_call_name`,
  `infix_binding_power` → comparison tier `(10,11)`), `sexpr.rs` (`infix_head`
  `DotCallI("===")`/`DotCallI("!==")`, `is_operator`), `structural.rs`
  (`is_op_name`). Single op ⇒ `(dotcall-i a === b)`; a run folds into
  `(comparison a (. ===) b …)` via the existing chain machinery.
- **Also fixed a latent AST gap**: `ast/nodes.rs::is_operator_kind`
  (`BinaryExpr::op_token`) was missing the non-dotted `EQ_EQ_EQ`/`NOT_EQ_EQ`
  (oversight from 2026-06-25k); added those plus the new dotted kinds. Affects
  the formatter's operator-token lookup, not the projector.
- **Verified faithful** against JS ground truth: `x .=== y`⇒`(dotcall-i x === y)`,
  `x .!== y`, chains `a .=== b .=== c`⇒`(comparison a (. ===) b (. ===) c)`,
  mixed `a .!== b .== c`. Siblings unregressed: `.==`/`.!=` still 3-char, `.=`
  still `DotEq` assignment.
- **Fixtures**: parser snapshot `broadcast_identity_operators` + oracle dir slug
  (parity confirmed); lexer unit test extended in `broadcasting_operators`.
- **Counts**: JS 677 (held — these aren't in the JS corpus, no unblocks/
  regressions), dir 182 → **183**.
- **Frontier note**: the JS harvested backlog is now **exhausted** of fixable
  cases — all 8 remaining FAILs are permanent/out-of-scope (float display ×6,
  `(2)(3)x` juxtaposition, `x 'y` char-lexer). Next work is real-world-value
  constructs not in the corpus, or the float-display `show` problem.

## Earlier sessions

- **2026-06-25k** — Identity/inequality operators `===`/`!==`/`!=`. Two tokens
  `EqEqEq`/`NotEqEq` (3-char ASCII block, longest-match beats `==`/`!=`); the
  crux was the `!` munch — `scan_ident` now stops at `!` immediately followed by
  `=` so `a!=b`⇒`a !=  b` while `f!`/`push!`/`a!b` stay identifiers. Single op ⇒
  `(call-i a === b)`; runs fold into `(comparison …)`. Fixture
  `identity_operators`. JS 677 (held); dir 181 → 182.

- **2026-06-25j** — Projector faithfulness audit (no parser change), de-risking the
  formatter: classified every non-trivial valid-code `sexpr.rs` arm by what it reads
  and probed each non-local one against JS. **Zero latent CST bugs** — every
  high-value arm is faithful; only non-local reads are matrix `group_dimension` order
  (projection-only, no formatter impact) and diagnostics-replay error shapes
  (sanctioned). Flat `COMPARISON_EXPR`/same-op `BINARY_EXPR` rewrites are well-formed
  trees safe to build on. One queued parser item: matrix-continuation outer-group fix
  (low priority, projection-only). JS 677, dir 181 unchanged.

- **2026-06-25i** — Misplaced `.'` prime → trailing-junk recovery (flips
  js-128bdd20 `f.'`). A `'` abutting a field-access `.` lexes as the removed
  transpose op, recovered as trailing junk (`f.'` ⇒ `f (error-t ')`). 3-file fix:
  lexer `prev_is_dot()` (lex `'` as `Transpose` after `Dot`; spaced `f. '` stays a
  char), operator-loop ends the value at `.`+`Transpose` (reuses `TrailingJunk`),
  projector renders the `'` glyph + drops the bundled `DOT`. Fixture
  `dot_prime_recovery`. JS 676 → 677; dir 180 → 181. Remaining 8 JS divergences all
  permanent/out-of-scope: float-display (6), the `x 'y` char-lexer sibling (needs
  bracket-depth-aware `'` lexing), `(2)(3)x` juxtaposition.

- **2026-06-25h** — Misplaced `end` keyword in space-separated array (flips
  js-557adcf4 `a[:(end)]`). `end` is a valid index marker only as the sole/leading
  bracket element; once another element precedes it the array ends, a zero-width
  `(error-t)` splices after the last real element, and `end <closers>` bumps up as
  trailing junk: `a[1 end]` ⇒ `(typed_hcat a 1 (error-t)) (error-t end ✘)`. New
  `EndKw` arm in `parse_matrix` + `MatrixKeywordRecovery` diag; projector splices
  via `project_cat_children`/`project_args`. JS 675 → 676; dir 179 → 180.

- **2026-06-25g** — Leading-`@` dotted macro `$`/inner-`@` reflow (flips
  js-704830e1 `@A.$x a`, js-fe911108 `@A.B.@x a`; closes the macro dotted-name
  cluster). A leading-`@` macro whose dotted path carries an interpolation or a
  second sigil relocates the sigil onto the **final** component and recovers the
  excess with zero-width markers (final `$x` ⇒ `(inert (error x))`; doubled sigil ⇒
  `(quote (error-t) @x)`); a non-final `$x` is a valid `(inert ($ x))`.
  `parse_macro_name_body` consumes the full `.ident`/`.$ident`/`.@ident` chain +
  `MacroSigilLeading` diag; `project_leading_macro_path` replays. Fixture
  `macro_sigil_leading`. JS 673 → 675; dir 178 → 179.
- **2026-06-25f** — Misplaced macro sigil `A.@B.x` (trailing form; flips
  js-27604c64). A `@` on a non-final component with a `.ident` continuation
  relocates the sigil to the final component, splicing `(error-t)` at every dotted
  step after the `@`-named one (`A.@B.x` ⇒ `(. (. A (quote B)) (error-t) (quote
  @x))`). Projector replay from a `MacroSigilTrailing` diag (`parse_qualified_macro`).
  Fixture `macro_sigil_trailing`. JS 672 → 673; dir 177 → 178.
- **2026-06-25e** — Broadcast call on a macro name `@M.(x)` (first clean slice of
  the macro dotted-name cluster, flips js-2516c70f). A broadcast `.(…)` on a macro
  is invalid; JuliaSyntax wraps the dotcall in a macrocall and splices a zero-width
  `(error-t)` after the name (`@M.(x)` ⇒ `(macrocall (dotcall @M (error-t) x))`).
  CST unchanged; projector replay from a new `MacroDotBroadcast` diag at the
  broadcast `(` (recorded in `parse_postfix_chain`, gated on lhs `MACRO_CALL`);
  new `project_dot_call` re-heads. Fixture `macro_broadcast_call`. JS 671 → 672;
  dir 176 → 177. Deferred: macro args after the broadcast (`@M.(x) y`).
- **2026-06-25d** — Bare block keyword `function`/`macro` empty-recovery shape
  (flips js-78f9ac01, the `function` slice of backlog item g). `function` ⇒
  `(function (error (error)) (block (error)) (error-t))`, likewise `macro`. Two
  zero-width pieces, pure projector from the recorded `MissingEnd` diag:
  `project_block_child` appends `(error)` to an empty body block carrying a
  `MissingEnd` (also corrects latent `function f()`/`for x in y`);
  `project_function_like` emits `(error (error))` when no `SIGNATURE` node.
  Fixture `bare_function_keyword`. JS 670 → 671; dir 175 → 176. Deferred: `struct`
  bare keyword (signature `(error)`, single), `begin`/`while` empty-body, bare-name
  truncated `function f` ⇒ `(error f)`.
- **2026-06-25c** — Incomplete ternary recovered as `if` (flips
  js-434fcafd/810e177c/74a9b301/471d5c84). A ternary whose missing `:`/false
  branch is terminated by a closing block keyword (`end`/`elseif`/`else`/`catch`/
  `finally`) re-heads `?` → `if` with one zero-width `(error-t)` per missing piece
  (no colon ⇒ `(if x true (error-t) (error-t))`, false missing ⇒ `(if x true
  (error-t))`). The flip is decided *locally* by the terminator, not the enclosing
  block (even toplevel `x ? true end` re-heads). Both missing-branch arms of
  `parse_ternary` peek `is_closing_block_keyword`, build `TERNARY_EXPR` + one
  `IncompleteTernaryIf` diag per marker at the `?`'s end; `project_ternary` keys
  the `if` head and count off it. Fixture `ternary_incomplete_if`. JS 666 → 670;
  dir 174 → 175. Deferred: toplevel EOF/newline-terminated incomplete ternary
  (stays `?`-head, not in corpus).
- **2026-06-25b** — Array `;;` line continuation → `hcat` (flips js-82572497
  `[a b ;; \n c]` ⇒ `(hcat a b c)`; deferred root (c)). A `;;` (exactly two)
  immediately followed by a newline (`;; \n`, *not* `\n ;;`) in an *already*
  row-major array behaves like a space separator (dim 0, folds into the row);
  a column-major `[a ;; \n b]` stays `(ncat-2 a b)`. **No diagnostic — valid
  syntax.** `parse_matrix` (`expr.rs`) tracks `SepRun.newline_after_semis` +
  `continuation` (set in the global `ArrayOrder` loop; `dim` returns 0);
  `group_dimension` (`sexpr.rs`) re-derives row-major order *locally* and counts
  a continuation `;;`-run as 0. Fixture `array_line_continuation`. JS 665 → 666;
  dir 173 → 174. Deferred: a continuation whose establishing space lives in an
  *outer* group (`[a b ;;; c ;; \n d]`) — local order can't see it; not in corpus.
- **2026-06-25a** — Invalid bracketed macro name `@[x]` (one macro-cluster slice,
  flips js-b2e95475 `@[x] y z`). A `[`/`{` directly after `@` is parsed as the
  bracketed expression and error-wrapped as the macro name with space-form args
  following (`@[x] y z` ⇒ `(macrocall (error (vect x)) y z)`, `@{x} y` ⇒
  `(macrocall (error (braces x)) y)`); `@m[a]` (name before bracket) untouched.
  New `LBracket`/`LBrace` arm in `parse_macro_name_body` + `InvalidMacroName` diag
  + `project_macro_name` error-wrap. Fixture `macro_name_brackets`. JS 664 → 665;
  dir 172 → 173. Remaining macro-cluster siblings are each a distinct error head:
  `@(x+y)` ⇒ `(error-i x + y)`, `@(f(x))` ⇒ `(error f x)`, `@:foo` ⇒
  `(error (quote-: foo))`, `@M.(x)` ⇒ `(dotcall @M (error-t) x)`, `A.@B.x`/
  `@A.$x a`/`@A.B.@x a` (dotted-name `@` reflow) — none cluster cleanly.

- **2026-06-24p** — Parenthesized `export` item (backlog item h, `export (x::T)`,
  flips js-62113d6b). A paren wrapping a lone symbol unwraps (`export (x)` ⇒ `x`,
  `export (+)` ⇒ `+`); any other parenthesized form error-wraps (`export (x::T)`
  ⇒ `(error (::-i x T))`, `export (x, y)` ⇒ `(error (tuple-p x y))`). New `LParen`
  arm in `parse_name_list_stmt` (export-only) parses a real `PAREN_EXPR`/
  `TUPLE_EXPR`; `flag_invalid_export_items` walk records `InvalidExportItem`;
  `project_export` unwraps/error-wraps. JS 663 → 664; dir 171 → 172.

- **2026-06-24o** — Empty quote-paren `:(end)`: a `:(…)` whose body opens with a
  closing block keyword can't start an expression; JuliaSyntax makes the quoted
  form a zero-width `(error-t)` (`:(end)` ⇒ `(quote-: (error-t)) (error-t end ✘)`,
  flips js-b1ac400e). New branch in `parse_quote_sym`'s `:(` arm + `EmptyQuoteParen`
  diag + `project_quote_sym` arm. Fixture `quote_paren_empty`. JS 662 → 663; dir
  170 → 171.

- **2026-06-24n** — Glued colon operator `:<`/`:>` (two-token sibling of `**`/`--`).
  A range colon glued to a single `<`/`>` is one invalid op at the colon tier
  `(14,15)`: `a :< b` ⇒ `(call-i a (error : <) b)` (flips js-147fac91). Glue is
  whitespace-sensitive on the colon's right only; `:<=`/`:>:` keep the range
  reading; prefix `:<` stays a quote. Consumes one op, no chaining (`glued_colon_done`
  flag). New `InvalidGluedOperator` diag + operator-loop branch (`expr.rs`) +
  `project_binary` arm joining both loose op tokens. Fixture `glued_colon_operator`.
  JS 661 → 662; dir 169 → 170.

- **2026-06-24m** — Docstring + stray closer: a doc-eligible string is a docstring
  only when a *real* statement follows (flips js-c74994ac `"notdoc" ]`, js-f9c36919
  `"notdoc"\n]`). Two coupled `core.rs` bugs: speculative `first_is_doc_string`
  suppressed trailing-junk recovery; `fold_docstrings` folded an error node as the
  doc target. Fixed via `doc_no_target`/`leftover_starts_with_subtree` +
  `doc_target` returning `None` on `ERROR`. Fixture `docstring_stray_closer`. JS
  659 → 661; dir 168 → 169.
- **2026-06-24l** — Unterminated char literals (flips js-265fda17 `'` ⇒ `(char
  (error))`, js-6808df30 `'a` ⇒ `(char 'a' (error-t))`). `lex_char_literal`
  (`lexer.rs`) always emits a `Char` token; a newline is char *content* (scan stops
  at the next `'` or EOF), an unterminated token spans `start..idx` with no close.
  The split-out `TokKind::Char` arm (`expr.rs`) records `UnterminatedLiteral` at the
  quote; `project_char` gates on it (empty ⇒ `(char (error))`, else decode body +
  `(error-t)`). JS 657 → 659; dir 167 → 168. Deferred siblings: `f.'`, `x 'y`,
  prime-suffixed float overflow `10.0e1000'`/`10.0f100'`.
- **2026-06-24k** — C2 flat arithmetic chains for `+`/`*` (final commit of the
  divergence-ledger campaign; flips js-81be47a1 `a + b + c`, js-2cdf798a `a * b * c`,
  js-99360f4e `[x+y+z]`, js-516f4fd7). A run of ≥2 of the *same* plain `+`/`*` folds
  into one flat variadic `BINARY_EXPR` via collect-then-choose
  `parse_flat_arith_chain` (mirrors C3); `is_flat_arith_op(&Token)` rejects suffixed
  ops; `project_flat_arith` renders ≥3 operands or a 2-operand missing-rhs. Excluded:
  dotted `.+`/`.*`, left-assoc `-`, suffixed. JS 653 → 657; dir 166 → 167.

## Earlier sessions

- **2026-06-24j** — C3 flat comparison chains (flips js-c32f9f82 `x<y<z` etc.). A
  run of ≥2 comparison-tier ops folds into one flat `COMPARISON_EXPR` (`a < b <= c`
  ⇒ `(comparison a < b <= c)`); lone comparison unchanged. New `COMPARISON_EXPR`
  kind + collect-then-choose `parse_comparison_chain` + arity-general `build_flat`/
  `build_flat_missing_rhs` (`expr.rs`); `project_comparison` renders dotted ops as
  `(. op)` and a dangling op as `(error)`. Fixture `comparison_chains`. JS 649 →
  653; dir 165 → 166. Deferred: word-op chains `a isa b isa c` stay nested.

- **2026-06-24i** — `&&`/`||` right-associativity (C1 of the ledger campaign;
  flips js-5d39e3d6 `x && y && z`, js-3fcc48ca `x || y || z`). The binding powers
  were left-assoc (`||`=(5,6), `&&`=(7,8)) despite a doc comment claiming
  right-assoc; flipped to `(6,5)`/`(8,7)` in `infix_binding_power`. Band and the
  missing-rhs path (`a &&` ⇒ `(&& a (error))`) intact; projector untouched.
  Fixture `short_circuit_assoc`. JS 647 → 649; dir 164 → 165.

- **2026-06-24h** — `end`/`begin` index marker scoped to genuine `ref` indexing
  + misplaced-`end` recovery (unblocks dir `end_index`). The marker is enabled
  *only* by genuine indexing (single-element/comma/empty `[…]` after a value) and
  *inherited* by everything nested inside; a bare `end` elsewhere recovers via
  `UnterminatedArgList` + a toplevel junk run. `inherited_end_marker` threads
  through the postfix/bracket/matrix parsers. Fixtures `end_index` +
  `end_marker_propagation`. dir 162 → 164.

**Backlog survey** (carried from 2026-06-24h; the comparison/flatten "deliberate"
items (a) are now the active campaign — see Progress): (b) **float display
(blocked)** — `x.3`, hex floats, `1.0e-1000`, prime+float: needs JuliaSyntax's
full Float32/64 `show`; (c) **char/prime lexer (partly done 2026-06-24l)** — bare
unterminated chars `'`/`'a` landed; *still deferred:* `f.'` (removed `.'`
operator), `x 'y` (space-before-`'` junk split), prime-suffixed float overflow
`10.0e1000'`/`10.0f100'` (entangled with float display); (d) **invalid-operator**
— `a :< b`⇒`(call-i a (error : <) b)` (two-token
glued op, needs a paired error token + 2-token error head); (e) **macro
dotted-name error shapes** — `A.@B.x`, `@A.B.@x a`, `@A.$x a`, `@M.(x)`, `@[x] y
z` — each a *distinct, deep* parser gap, NOT a clean cluster; (f)
**ternary-in-block** (`if true; x ? true end`) — fragile, the recovered ternary
head flips between `?` and `if` by context; (g) **bare block keyword** —
`function`/`macro`/`struct`/`while x`/`begin` with no signature/body/`end`
(js-78f9ac01). Most real-world-relevant (incomplete-editor states) but *intricate*
(two interacting sub-features; signature recovery can consume the `end`); ~2
sessions; (h) **misc error shapes** — `:(end)`, `a[:(end)]`, `export (x::T)`,
`"notdoc"]`, each a distinct narrow path.

- **2026-06-24g** — Prefix-operator spaced call-form paren → zero-width `(error)`
  (flips js-4f46be13 `+ (a,b)`). A unary-prefix-capable operator (`+ - ~ ! .+ .-
  .~ <: >:`) separated by horizontal whitespace from a *call-form* `(` (the
  `unary_op_paren_is_call` predicate) heads a call with a zero-width `(error)`
  flagging the disallowed space (`+ (a,b)` ⇒ `(call + (error) a b)`); a single
  operand/block paren stays `call-pre` and the glued form is unchanged. New
  `PrefixOpenerWhitespace` diag spliced by `project_call`. Fixture
  `prefix_operator_spaced_call`. JS 646 → 647; dir 161 → 162. Deferred: suffixed/
  non-unary spaced operators (`+₁ (a)`/`* (a,b)`) project like an identifier
  callee (`(error-t)`).
- **2026-06-24f** — Colon-space-before-closing-keyword → bare `:` Colon atom
  (flips js-4a2410ee `: end`). A value-position prefix `:` then a *space* then a
  closing block keyword (`end`/`else`/`elseif`/`catch`/`finally`) is the bare
  Colon value atom with the keyword spilled as junk (`: end` ⇒ `(toplevel :
  (error-t end))`); whitespace-sensitive (`:end` ⇒ `(quote-: end)`) and
  context-sensitive (`a[: end]`/`A.: end` keep the quote). `parse_quote_sym` gains
  `value_position`/`end_marker` params + declines for the spaced-closer case;
  `project_error` renders the closer verbatim (also fixes `x end` ⇒ `x (error-t
  end)`). Fixture `colon_space_closer_keyword`. JS 645 → 646; dir 160 → 161.
- **2026-06-24e** — Invalid doubled operators `**`/`--` (and broadcast `.**`/
  `.--`), the operator-recipe slice of the invalid-operator backlog (flips
  js-90827a2e `a--b`). Julia has no `**`/`--`, so JuliaSyntax lexes each as a
  *single* error operator at a fixed low tier (looser than `+`, tighter than
  `:`/`==`, left-assoc) heading the infix call with the error token: `a**b` ⇒
  `(call-i a (Error**) b)`, `a--b` ⇒ `(call-i a (ErrorInvalidOperator) b)`; dotted
  forms `dotcall-i`. New `StarStar`/`MinusMinus`/`DotStarStar`/`DotMinusMinus`
  `TokKind`s, tier `(18, 19)`, `infix_head`/`is_operator` arms. Fixture
  `invalid_doubled_operators`. JS 644 → 645; dir 159 → 160. Deferred: prefix
  `**a`/`--a` (call-pre, not in corpus); `:<`-style two-token invalid op.

- **2026-06-24d** — Stray middle/closing block keyword error-wrap (`@doc x\nend`,
  js-bc08a2b0). A block keyword that only closes/continues an enclosing block
  (`end`/`else`/`elseif`/`catch`/`finally`) where a statement is expected is not a
  block opener; JuliaSyntax wraps it alone in `(error <kw>)` and bumps the rest of
  the line as a separate trailing-junk run (`end y z`⇒`(error end) (error-t y z)`).
  The `parse` driver (`core.rs`) wraps the kw in `ERROR`, records `StrayKeyword`,
  sets `leftover_mark`; `project`'s `ERROR` arm renders it via `stray_keyword_text`.
  Fixture `stray_block_keyword`. JS 643 → 644; dir 158 → 159.
- **2026-06-24c** — Non-identifier `catch` variable error-wrap (post-build walk
  `flag_invalid_catch_vars` + `project_try` `CATCH_CLAUSE` wrap; sibling of
  const-not-assignment and bare-name-function). A `catch` var must be a plain
  identifier, `$`-interpolation, or `var"…"`; anything else (`catch e+3`/`e.f`/
  `f(e)`/`3`) is `(error …)`. Fixture `catch_var_error`. JS 642 → 643; dir
  157 → 158.
- **2026-06-24b** — String-literal escape error classification (the `Char`
  sibling of the 2026-06-23f char-error work). A single-quoted `"…"` whose
  `STRING_CONTENT` holds a malformed backslash escape projects as one
  `(ErrorInvalidEscapeSequence)` *per content token*, dropping valid surrounding
  text (`"\xqqq"`/`"ok\xqq"`/`"\400"` ⇒ `(string (ErrorInvalidEscapeSequence))`,
  `"a\xq$b"` keeps the interpolation); valid-but-non-UTF-8 bytes (`"\xff"`) stay a
  *valid* `(string "\xff")`. Pure projector: `decode_string_chunks` now returns
  `Result<_, StringDecodeError>` distinguishing `BadEscape` (→ error part) from
  `BadUtf8` (→ raw fallback). Fixture `string_escape_error`. JS 641 → 642; dir
  156 → 157.
- **2026-06-24a** — Bare-name `function`/`macro` signature with a body →
  `(error <name>)`. A bare-identifier signature is the valid forward-declaration
  form only while the body is empty (`function f end` ⇒ `(function f)`); once a
  body statement appears or the block is explicitly opened with `;`, JuliaSyntax
  error-wraps the name (`function f body end` ⇒ `(function (error f) (block
  body))`). Post-build walk `flag_invalid_function_signatures` (`core.rs`) +
  `project_function_like` wrap. Fixture `function_bare_name_signature`. JS
  640 → 641; dir 155 → 156. Deferred: trailing block-body junk
  (`function f g h end`) not projected (shared for/let/module/struct/try/do gap).

- **2026-06-23z** — Newline between `function`/`macro` and its signature (a real
  parser bug). A newline after the opening keyword is insignificant, so the
  signature may begin on the next line (`function\n f() end` ⇒ `(function (call f)
  (block))`); `parse_function_like` now skips newlines (not just horizontal ws)
  for `sig_start`. Fixture `function_signature_newline`. JS 639 → 640; dir
  154 → 155. Side effect: `function\n end` now error-wraps `end` as a name (an
  error shape either way, not in the passing corpus).

- **2026-06-23y** — Reserved keyword as a signature name → `(error <kw>)`. A hard
  reserved keyword used as a `struct`/`module`/`function`/`macro` name is a misused
  name, not a block opener; JuliaSyntax error-wraps it (`struct try end` ⇒
  `(struct (error try) (block))`, `function begin() end` ⇒ `(function (call (error
  begin)) (block))`). New `name_context` `ExprFlag` builds an `ERROR > NAME > <kw>`
  atom; projector's `name_text` falls back to a keyword token. Contextual words
  (`mutable`/`where`/`true`/`outer`/…) excluded. Fixture `keyword_name_error`. JS
  635 → 639; dir 153 → 154. Sibling not done: `function f body end` ⇒ `(function
  (error f) (block body))` is a *different* divergence (a bare-identifier signature
  with trailing tokens, not a keyword name).

- **2026-06-23x** — Suffixed operator in prefix position → `(error op)`: a
  sub/superscript- or prime-suffixed arithmetic operator (`+₁`, `.+₁`) is not a
  valid unary prefix; error-wrapped and applied as a prefix call (`+₁ x` ⇒
  `(call-pre (error +₁) x)`), reusing the 2026-06-23n machinery. Glued `(` forces a
  plain call. Fixture `suffixed_prefix_operator`. JS 634 → 635; dir 152 → 153.
- **2026-06-23w** — Range-colon newline stop + unified missing-rhs `(error)`: the
  range `:` is the lone binary op that drops its right operand across a newline at
  statement scope or in array brackets (`1:\n2` ⇒ `(call-i 1 : (error)) 2`), a
  paren keeps it (`(1:\n2)` ⇒ `(call-i 1 : 2)`); also moved `:`'s missing-rhs onto
  the shared `(error)` synthesis. `parse_colon_range` computes `newline_significant`.
  Fixture `colon_range_newline`. JS 633 → 634; dir 151 → 152.
- **2026-06-23v** — Empty comma-list slot → flat `(error-t ✘ …)`: an empty element
  slot *after a real element* in any comma list bails, bumping the comma and the
  rest up to the closer as one trailing-junk run (`[x,,]` ⇒ `(vect x (error-t ✘))`,
  `f(x,,y)` ⇒ `(call f x (error-t ✘ y))`); a trailing comma stays clean and `,;`
  is a normal parameters split. `parse_arg_list` tracks `slot_empty`/
  `parsed_element` and reuses the `@`-junk machinery (`ERROR` over `[comma, close)`
  + `TrailingJunk` diag), no new node/projector arm. Fixture `list_empty_comma`.
  JS 631 → 633; dir 150 → 151. Deferred: leading empty slot (`[,x]`), nested
  brackets in the junk run.

- **2026-06-23u** — `else if` → `elseif` recovery → zero-width `(error-t)`:
  `else if` on one line (`if a … else if b … end`) is recovered as an `elseif`
  clause consuming both keywords, splicing a zero-width `(error-t)` into the
  missing else position (`if a xx else if b yy end` ⇒
  `(if a (block xx) (error-t) (elseif b (block yy)))`); a newline between the
  keywords keeps the genuine else-block-`if` reading. `parse_if_expr`'s `ElseKw`
  arm peeks past horizontal ws, opens an `ELSEIF_CLAUSE` over both keywords,
  records an `ElseIf` diagnostic at the opening `if`. Fixture `else_if_recovery`.
  JS 630 → 631; dir 149 → 150.

- **2026-06-23t** — Array space/`;;` separator mismatch → zero-width `(error-t)`:
  JuliaSyntax establishes a row-/column-major order from the first space/`;;`
  separator and flags a later conflicting one (`[a b ;; c]` ⇒
  `(ncat-2 (row a b (error-t)) c)`); only `;` runs of exactly two participate.
  `parse_matrix` walks separator runs tracking `ArrayOrder`, records
  `ArraySeparatorMismatch` at the offending element's end; `project_cat_children`
  reconstructs after the bare `ARG` it anchors. Fixture `array_separator_mismatch`.
  JS 627 → 630; dir 148 → 149. Deferred: `;;\n` line continuation → `hcat`.

- **2026-06-23s** — Missing operator right-operand → zero-width `(error)`: an
  infix/assignment operator with an absent right operand keeps its node and
  synthesizes `(error)` there (`x =` ⇒ `(= x (error))`, `a +` ⇒
  `(call-i a + (error))`) rather than error-wrapping `lhs op` to line end;
  `build_binary_missing_rhs`+`operator_node_kind` build the LHS-only node,
  `project_binary`/`project_assignment` reconstruct via `operator_missing_rhs`.
  Paired with a prefix value-fallback (`<: =` ⇒ `(= <: (error))`). Fixture
  `operator_missing_rhs`. JS 624 → 627; dir 147 → 148. Deferred: `::`/`->`
  projectors, word ops, `where` still use `error_expr_to_line_end`.
- **2026-06-23r** — Missing `if`/`elseif` condition → zero-width `(error)`: an
  empty condition slot (`if end`, `if; end`, `if true; elseif; end`) is recovery;
  JuliaSyntax synthesizes `(error)` there. Pure projector win — Fatou already had
  an absent `CONDITION` + `MissingCondition` diagnostic; re-anchored that diag at
  the opening keyword (mirroring `MissingEnd`) and added `missing_condition`
  (`diag_count_from(keyword_start, …)`) wired into `project_if`/`project_if_tail`.
  Fixture `if_missing_condition`. JS 622 → 624; dir 146 → 147. Deferred: `while
  end` recovers differently (`(while (error end) (block (error)) (error-t))`).
- **2026-06-23q** — Multi-value `$(…)` interpolation → `(error …)`: a `$(…)` holds
  a single expression, so a multi-value parenthesized form is invalid (`"$(x;y)"`,
  `"$(x,y)"`, `"$(x for y in z)"`). `parse_interpolation` reuses `parse_paren` +
  records `InvalidInterpolation`; `project_interpolation` reconstructs the error
  from the inner node kind. Fixture `string_interp_error`. JS 619 → 622; dir
  145 → 146.
- **2026-06-23p** — Lone syntactic operator → `(error op)`: a syntactic operator
  with no value meaning where an atom is expected is `(error op)` (`=`, `+=`,
  `&&`, `->`, `...`, `?`/`?x`); the trailing operand falls to the junk driver.
  `is_lone_error_operator` + `error_operator_atom` (`expr.rs`). Fixture
  `lone_operator_error`. JS 614 → 619; dir 144 → 145.
- **2026-06-23o** — Array-internal trailing junk: a macro `@` glued to a preceding
  array element bumps the rest of the array to `]`/EOF as one flat trailing-junk
  run (`[x@y]` ⇒ `(hcat x (error-t ✘ y))`); one arm in `parse_matrix` collects it
  via existing `emit_cat_child`/`ARG`, no projector change. Fixture
  `array_trailing_junk`. JS 612 → 614; dir 143 → 144. Deferred: `;`/nested
  brackets in the junk.
- **2026-06-23n** — Binary-only operator in prefix position → error-wrapped prefix
  call. `/x` ⇒ `(call-pre (error /) x)`, `.*x` ⇒ `(dotcall-pre (error (. *)) x)`;
  operand binds at `PREFIX_BP` (tighter than arithmetic, below `^`); bare `*` stays
  a value atom. Fix in the `is_value_operator` arm of `parse_prefix` (`expr.rs`):
  emits `UNARY_EXPR > ERROR > OPERATOR_ATOM > op` + operand, new
  `InvalidPrefixOperator` diagnostic; `project_unary` renders the prefix-call head.
  Fixture `prefix_operator_error`. JS 609 → 612; dir 142 → 143.
- **2026-06-23m** — `public` stops at the first non-comma after a name (a
  names-only shim, `parse_public`); leftover floats to the toplevel junk driver
  (`public x=1, y` ⇒ `(public x) (error-t = 1 ✘ y)`). `export` differs (re-enters
  the operator parser), so the stop is `PUBLIC_STMT`-gated. Fixture
  `public_stop_at_equals`. JS 607 → 609; dir 141 → 142.
- **2026-06-23l** — Block-body trailing junk: a separator-less glued statement
  inside a block ends it; `bump_closing_token` bumps the run as flat error tokens
  up to the closing keyword. Uniform CST (junk `ERROR` always a `BLOCK` sibling);
  the projector places it — `begin`/`quote` fold it inside (`begin\n x y\n end` ⇒
  `(block x (error-t y))`), `if`/`while` keep it a sibling. Fixture
  `block_trailing_junk`. JS 605 → 607; dir 140 → 141. Deferred:
  for/let/module/struct/try/do junk (sibling `ERROR` in CST, not yet projected).

**Older deferred roots** (not in this session's survey): (a) **`outer`
stop-at-`=`** — `outer x=1` ⇒ `outer (error-t x = 1)` (`outer` is the bare value,
the whole `x = 1` is junk, unlike `public`); (b) **for/let/module/struct/try/do
block junk** — sibling `ERROR` is in the CST but their explicit projectors don't
emit it (only `if`/`while`/`begin`/`quote` do). (Root (c), `;;\n`
line-continuation, was done 2026-06-25b.)

- **2026-06-23k** — Flat trailing-junk runs (toplevel): a separator-less line's
  leftover bumps as *flat error tokens* (`x y, z` ⇒ `x (error-t y ✘ z)`,
  `x@y` ⇒ `x (error-t ✘ y)`); `core.rs` driver + `is_error_glyph`. Fixture
  `toplevel_leftover_error`. JS 603 → 605.
- **2026-06-23j** — `const`-not-assignment error-wrap (first diagnostics-model
  error shape): `const x`⇒`(error (const x))`, struct-field `const` exempt;
  post-build `flag_invalid_const_decls` + `CONST_STMT` projector wrap. Fixture
  `const_not_assignment`. JS 599 → 603; dir 139 → 140.
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
- **2026-06-23h** — `import`/`as` colon error shapes: a top-level `:` is the
  base/names split only as the *first* separator (`import A, B: y` ⇒ recovery); a
  second names-list colon is recovery; a base alias before a valid `:` is invalid
  and a `using` base alias stacks both. `parse_import_stmt` passes an error-wrap
  depth (0/1/2) to `parse_import_clause`. Fixture `import_as_colon_error`. JS
  597 → 599; dir 138 → 139.
- **2026-06-23g** — `using`-base `as` rename error-wrap (`using A as B` ⇒
  `(error (as …))`, invalid in a `using` base path); fixture `using_as_error`.
  JS 595 → 597; dir 137 → 138. (Superseded: the bool became an error-wrap depth.)

- **2026-06-23f** — Char-literal error classification (closed-but-invalid bodies):
  empty `''`⇒`(char (error))`, malformed escape `'\xq'`⇒`(char
  (ErrorInvalidEscapeSequence))`, other multi-codepoint `'ab'`⇒`(char
  (ErrorOverLongCharacter))`; a lone non-UTF-8 byte `'\xff'` stays a valid `Char`.
  Pure projector: `project_char`'s `None` arm delegates to `classify_char_error`.
  Fixture `char_errors`. JS 592 → 595; dir 136 → 137. Deferred: unterminated chars
  (lexer work, entangled with transpose siblings `f.'`/`x 'y`).
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
`ERROR_TRIVIA`/`project_error`/leftover-driver machinery, condensed — see git for
detail):

- **2026-06-22v** — Paren-block juxtapose-error (`(begin end)x`⇒`(block)
  (error-t x)`); `lhs_is_paren_block` suppresses both juxtapose checks. JS 575 → 576.
- **2026-06-22u** — String-juxtapose-error (`"a"x`⇒`(juxtapose (string "a")
  (error-t) x)`); `should_juxtapose_string_error` before the numeric case. JS 571 → 575.
- **2026-06-22t** — Separate-toplevel trailing-junk (`x y`⇒`x (error-t y)`); the
  `core.rs` driver records `leftover_mark` + one `ERROR_TRIVIA` sibling. JS 568 → 571.
- **2026-06-22s** — Field-access/colon-quote space (`x .y`⇒`(. x (error-t)
  (quote y))`, `: foo`⇒`(quote-: (error-t) foo)`); broadcast `.+` untouched. JS 564 → 568.
- **2026-06-22r** — Whitespace-before-postfix-opener (`f (a)`⇒`(call f (error-t)
  a)`); `parse_postfix` splices when `open_idx > lhs.end`. JS 559 → 564.
- **2026-06-22q** — `var"…"` glued-suffix (`var"x"y`⇒`(var x (error-t))`). JS 556 → 559.
- **2026-06-22p** — Unterminated-string (`"str`⇒`(string "str" (error-t))`);
  `with_error_trivia` appends the marker. JS 555 → 556.
- **2026-06-22o** — Typed error-node taxonomy (Phase 0): new `ERROR_TRIVIA`,
  `project_error(head, node)`, total `render()`; harvest kept `(error …)` cases →
  JS corpus 575 → 685 (the visible backlog). First slice `f(a`⇒`(call f a
  (error-t))`. JS 553 → 555.

**Pre-error-shape feature work** (2026-06-17a through 2026-06-22n, JS allow
251 → 553 — the oracle build-out, then operators, literals, strings, char/escape
decoding, macros, imports/`using`, comprehensions/generators, matrices/`ncat`,
block forms, `where`, do-blocks, splat precedence, integer-display
normalization, …) is fully recorded as `[x]` bullets in `TODO.md` and in git
history. Trimmed from this log to honor the ≤300-line cap; consult `git log
--oneline` or `TODO.md` for any specific construct.
