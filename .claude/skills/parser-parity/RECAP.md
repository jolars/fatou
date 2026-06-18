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

JS corpus (575 cases): **291 allowlisted**, 265 divergence, 19 unsupported.
Dir corpus: **43 allowlisted**, 5 blocked + 1 skipped (do_blocks).
Grammar bullets through "richer `import`/`using`" are `[x]` in `TODO.md`.

Deliberate (recorded) divergences, do not "fix": comparison chains (nested),
associative `a*b*c` (nested binary), numeric-literal display normalization,
triple-string dedent, `end`/`[1 +2]`/unterminated-string/incomplete-`do` error
shapes (dir `blocked.txt`).

## Latest session (2026-06-17g)

**Multi-clause & comma generators.** Replaced the single-clause `parse_comprehension`
with a `for`-clause loop + new `parse_for_specs` helper (`expr.rs`): each `for`
emits a sibling `FOR_BINDING`, comma-separated specs stay as tokens inside it, and
the `a = as` spec form is parsed whole as an `ASSIGNMENT_EXPR` (the old "expected
`in`" diagnostic is gone). Projector (`sexpr.rs`): `project_for_binding_node` now
splits a binding on its top-level COMMA tokens — one spec projects directly, several
become `(cartesian_iterator …)` via new `project_for_spec`; `project_generator`
walks clauses in order and folds each trailing `COMPREHENSION_IF` into a `(filter
<preceding-clause> cond)`. Fixture `multi_clause_generators` (parser + dir corpus).

JS allow **282 → 291** (+9: `for…for`, `for…for…if`, `for…if…for`, comma
`a in as, b in bs`, comma+if, comma+for+comma, the 4-clause comprehension, and
two `=`-spec/`begin`-cond forms); divergence 266 → 265, unsupported 27 → 19.
Dir allow 42 → 43. Zero regressions; green, clippy/fmt clean.

**Bonus:** the shared `project_for_binding_node` also fixed the for-*loop*
statement `for x in xs, y in ys … end` (js-ae2710c2, FAIL → PASS) — the loop
parser already captured both comma specs in `FOR_BINDING`; only the projector's
cartesian grouping was missing. Faithful, not compensation.

**Still unsupported (deferred):** `[x \n\n for …]` (blank-line-before-`for`),
`x where {y for y in ys}` (generator inside braces), `T[x for x in xs]` (typed
comprehension). Tuple-destructuring loop vars (`for (i,j) in …`) and bare
call-argument generators (`sum(x for x in xs)`) untouched.

**Suggested next targets (ranked):**
1. **Typed comprehension `T[x for x in xs]`** + bare call-arg generators
   (`sum(x for x in xs)`) — reuses the now-complete generator machinery.
2. **Precedence-table renumber** (infra), then arrow `-->`/`<-->` (prec 4, special
   head `(--> a b)`, dotted `.-->` → `dotcall-i`) and left-pipe `<|` (prec 8).
   Blocked by cramped low-tier numbers (no integer gap at `=>`(4)/`||`(5) or
   cmp(11)/`|>`(12)) — do the renumber first.
3. **Splat postfix precedence** — fix `x..y...` → `(... (.. x y))` (also `x:y...`).
4. **Operator-symbol import names** (`import A: +`, `import A.==`) — extend the
   import-path grammar to accept operator tokens as name components.

## Earlier sessions

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
