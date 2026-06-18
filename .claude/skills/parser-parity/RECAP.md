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

JS corpus (575 cases): **302 allowlisted**, 262 divergence, 11 unsupported.
Dir corpus: **46 allowlisted**, 5 blocked + 1 skipped (do_blocks).
Grammar bullets through "operator-symbol import names" are `[x]` in `TODO.md`.

Deliberate (recorded) divergences, do not "fix": comparison chains (nested),
associative `a*b*c` (nested binary), numeric-literal display normalization,
triple-string dedent, `end`/`[1 +2]`/unterminated-string/incomplete-`do` error
shapes (dir `blocked.txt`).

## Latest session (2026-06-18c)

**Operator-symbol import names.** `parse_import_path` (`structural.rs`) gained
operator components in three positions: bare name in the `:` list (`import A: +,
==`, `import Base: +, -, *`), fused dotted operator component (`import A.==` — the
lexer merges `.==` into one `DOT_EQ_EQ` token whose *leading dot is the separator*,
not broadcast; the projector strips it via `trim_start_matches('.')`), and quoted
operator after a dot (`import A.:+` → a `QUOTE_SYM` node wrapping `:` + op →
`(importpath A (quote-: +))`, reusing `project_quote_sym`, no `parse_quote_sym`
change). Two new TokKind predicates `is_op_name` (undotted symbolic ops, excludes
`:`/dots/assignment) and `is_dotted_op_name` (the `.+`/`.==` broadcast tokens) gate
the first-name and loop arms; projector reuses its existing `is_operator(SyntaxKind)`
and ignores separator `DOT`/`COLON`. Files: `structural.rs` (parser + predicates),
`sexpr.rs` (`project_import_path` arms). Fixture `import_operator_names` (parser +
dir corpus, 6 lines). **Deferred:** unicode ops (`import .⋆`, `A.⋆.f` — `⋆` lexes
as `ERROR`, needs unicode-operator lexing) and paren-quoted forms (`A.:(+)`,
`A.(:+)`).

JS allow **299 → 302** (+3: `import A.:+`, `import A.==`, `import A: +, ==`);
divergence 265 → 262, unsupported held 11. Dir allow 45 → 46. Zero regressions;
green, clippy/fmt clean.

**Suggested next targets (ranked):**
1. **Operator-symbol quoting** (`:+`, `:(=)`, `:<:`, `:+=`, `:.&&`) — a cluster of
   FAIL/UNSUPPORTED (js-871a4d6b `:+=`, …); `parse_quote_sym` currently rejects bare
   operators. The import work just demonstrated the projector side already handles
   an operator-token `QUOTE_SYM` child; this generalizes the *prefix* `:op` path.
2. **Splat postfix precedence** — `x..y...` → `(... (.. x y))` (also `x:y...`,
   js-2155b9ca, js-5d3b9cc6).
3. **Dotted-`$` field access** (`f.$x`, `f.$(x+y)`, js-a643eeec, js-c651c24f) and
   **tuple-destructuring loop vars** (`for (i, j) in …`).
4. **Unicode operators** (lexer) — unblocks `import .⋆`, `A.⋆.f`, `[x +₁y]`,
   `a … b`, and many scattered FAILs; larger lexer feature.

## Earlier sessions

- **2026-06-18b** — Arrow, pipe, and bitshift operators: `-->` (Special head),
  `<-->`, broadcast `.-->` on the arrow tier `(4,3)`; pipes split into `<|` `(12,11)`
  and `|>`/`.|>` bumped to `(13,14)`; bitshift `<< >> >>>` at `(30,31)` (Julia prec
  14 ⇒ tighter than `*`, looser than `^`). Fixture `arrow_pipe_bitshift_operators`.
  JS allow 292 → 299.

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
