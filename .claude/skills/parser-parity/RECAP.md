# parser-parity recap

Rolling log. Read top-to-bottom: persistent traps → progress → latest session →
earlier log. Keep ≤ ~300 lines; demote the "Latest session" to a one-liner each
new session.

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

JS corpus (575 cases): **299 allowlisted**, 265 divergence, 11 unsupported.
Dir corpus: **45 allowlisted**, 5 blocked + 1 skipped (do_blocks).
Grammar bullets through "arrow/pipe/bitshift operators" are `[x]` in `TODO.md`.

Deliberate (recorded) divergences, do not "fix": comparison chains (nested),
associative `a*b*c` (nested binary), numeric-literal display normalization,
triple-string dedent, `end`/`[1 +2]`/unterminated-string/incomplete-`do` error
shapes (dir `blocked.txt`).

## Latest session (2026-06-18b)

**Arrow, pipe, and bitshift operators.** Full 5-file recipe for a cluster sharing
two precedence insights from `Base.operator_precedence`. Arrow family: `-->`
(`LongArrow`, Special head `(--> a b)`), `<-->` (`LeftRightArrow`, `(call-i a <-->
b)`), broadcast `.-->` (`DotLongArrow`, `(dotcall-i a --> b)`) — all right-assoc on
the existing arrow tier `(4,3)`. Pipes: `<|` (`PipeLt`, looser, right-assoc) at a
*new* slot `(12,11)`; `|>` (and new broadcast `.|>` → `DotPipeGt`) bumped `(12,13)
→ (13,14)` to make room (colon `(14,15)` still binds tighter since the loop uses
`l_bp >= min_bp`, 14≥14 ⇒ `a |> (b:c)`). Bitshift `<< >> >>>` (`Shl`/`Shr`/`UShr`)
left-assoc at `(30,31)`, between `//`(28,29) and `^`(32,31) — Julia prec 14 makes
bitshift *tighter* than `*`/`//`, looser than `^` (surprised me; verified). Lexer
longest-match: `<-->` 4-char and `-->`/`>>>` 3-char beat prefixes; `.-->` 4-char
beats `.-`. No global renumber needed (the renumber the prior recap flagged turned
out to be a local 1-tier bump). Fixture `arrow_pipe_bitshift_operators` (parser +
dir corpus, 11 cases).

JS allow **292 → 299** (+7: `x --> y`, `x .--> y`, `x <--> y` (was FAIL), `x <| y
<| z`, `x .|> y`, `x >> y >> z`, `outer <| x = rhs`); divergence held 265,
unsupported 18 → 11. Dir allow 44 → 45. Zero regressions; green, clippy/fmt clean.

**Suggested next targets (ranked):**
1. **Operator-symbol import names** (`import A: +`, `import A.==`, `import A.:+`,
   `import A.:.+`) — several FAIL + UNSUPPORTED (js-22ffbcb0, js-3a22c71b,
   js-99360f4e, js-32feecde); extend `parse_import_stmt` path grammar to accept
   operator tokens as name components.
2. **Operator-symbol quoting** (`:+`, `:(=)`, `:<:`, `:+=`, `:.&&`) — a cluster of
   FAIL/UNSUPPORTED; `parse_quote_sym` currently rejects bare operators.
3. **Splat postfix precedence** — `x..y...` → `(... (.. x y))` (also `x:y...`,
   js-2155b9ca, js-5d3b9cc6).
4. **Dotted-`$` field access** (`f.$x`, `f.$(x+y)`, js-a643eeec, js-c651c24f) and
   **tuple-destructuring loop vars** (`for (i, j) in …`).

## Earlier sessions

- **2026-06-18a** — Generator arguments & typed comprehensions: `parse_postfix`
  speculatively parses the first bracketed element and, on a following `for`, builds
  a `GENERATOR` (call-arg `sum(x for …)`) or `TYPED_COMPREHENSION` (`T[x for …]`)
  instead of an `ARG_LIST`; projector gains a `GENERATOR`-child branch +
  `project_typed_comprehension`. JS allow 291 → 292. Fixture `generator_arguments`.

- **2026-06-17g** — Multi-clause & comma generators: replaced single-clause
  `parse_comprehension` with a `for`-clause loop + `parse_for_specs` (each `for` a
  sibling `FOR_BINDING`, comma specs as tokens, `a = as` form an `ASSIGNMENT_EXPR`);
  projector `project_for_binding_node` splits on top-level commas into
  `cartesian_iterator`, `project_generator` folds trailing `if` into `filter`. Also
  fixed the for-*loop* `for x in xs, y in ys` (js-ae2710c2). JS allow 282 → 291.
  Fixture `multi_clause_generators`.

- **2026-06-17f** — Richer `import`/`using` path trees: dedicated `parse_import_stmt`
  building real `IMPORT_PATH`/`IMPORT_ALIAS` nodes the projector reads (no
  reconstruction); leading-dot expansion, `:` switches base→name-list, `as` is a
  contextual ident. JS allow 274 → 282. Deferred: operator-symbol/`@macro`/`$interp`
  names, `export` list. Trap: scratch-buffer the clause, commit whitespace only on
  success, else verbatim passthrough double-emits.

- **2026-06-17e** — Range operator `..`: `DotDot` 2-char op (longest match `...` >
  `..` > `.`), placed after the splat check, before the broadcast-`.` block; a
  `lex_number` guard (`peek(1) != Some(b'.')`) keeps `1..n` from lexing as float
  `1.` + `.n`. Shares colon tier `(14,15)`, ordinary `BINARY_EXPR` → `(call-i a ..
  b)`. JS allow 273 → 274. New FAIL `x..y...` (splat-precedence gap, deferred).
  Fixture `range_operator`.

- **2026-06-17d** — Broadcast short-circuit `.&&`/`.||`: 5-file recipe (infix-only,
  no prefix); `DotAndAnd`/`DotOrOr` in the 3-char dotted table, share `&&`/`||`
  tiers, project to their own `Special(".&&")`/`Special(".||")` heads (not
  `dotcall-i`). JS allow 271 → 273. Fixture `dot_logical_operator`.

- **2026-06-17b** — Augmented assignment `op=` (parity-driven ASCII set): 16
  TokKinds/SyntaxKinds for `+= -= *= /= //= ^= %= |= &=` + broadcast `.+= … .%=`.
  Lexer longest-match (`.//=`>`.//`, `//=`>`//`); an `is_assignment_op` helper folds
  them into the existing `ASSIGNMENT_EXPR` arm + `(2,1)` tier; `project_assignment`
  reads the head from operator-token text. `global`/`let` free. JS allow 259 → 264.

- **2026-06-17a** — Built the oracle from scratch + ran the loop 3×: JuliaSyntax
  differential oracle (projector `sexpr.rs` + `--to sexpr`, harness, curated +
  harvested corpora, refresh scripts); `a[begin]` index marker (+1 JS); `:foo` /
  `:(x+1)` symbol quotes via `parse_quote_sym` (+5 JS); pair operator `=>`/`.=>`
  on arrow tier `(4,3)` (+2 JS). JS allow 251 → 259.
