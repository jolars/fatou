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

JS corpus (575 cases): **274 allowlisted**, 274 divergence, 27 unsupported.
Dir corpus: **41 allowlisted**, 5 blocked + 1 skipped (do_blocks).
Grammar bullets through "range `..`" are `[x]` in `TODO.md`.

Deliberate (recorded) divergences, do not "fix": comparison chains (nested),
associative `a*b*c` (nested binary), numeric-literal display normalization,
triple-string dedent, `end`/`[1 +2]`/unterminated-string/incomplete-`do` error
shapes (dir `blocked.txt`).

## Latest session (2026-06-17e)

**Range operator `..`.** 5-file recipe + a number-lexer fix. `DotDot` lexed as a
2-char op placed *after* the `...` splat check and *before* the broadcast-`.`
block (longest match `...` > `..` > `.`). Critically, `lex_number`'s fractional
`.` is now guarded `&& peek(1) != Some(b'.')` so `1..n` lexes `1 .. n` (not the
float `1.` + `.n`); `1.0..2`, `1.`, `1.e3`, `.5` all still lex right. Shares the
colon tier `(14,15)` (left-assoc, `Colon | DotDot`), builds an ordinary
`BINARY_EXPR`, projects to `(call-i a .. b)` (`CallI("..")` + `is_operator` +
`is_operator_kind`). Fixture `range_operator` (parser + dir corpus).

JS allow **273 → 274** (`a..b`); unsupported 29 → 27, divergence 273 → 274. The
new FAIL is `x..y...` (now parses; `...`-splat binds looser than `..` in Julia:
`(... (.. x y))` vs Fatou `(.. x (... y))` — a separate splat-precedence gap, not
a `..` bug). Dir allow 40 → 41. Zero regressions; green, clippy/fmt clean.

**Trap learned:** adding a low-precedence operator (`-->` prec 4, `<|` prec 8) is
blocked by Fatou's *cramped* low-tier numbers — no integer gap between `=>`(4) and
`||`(5), nor between cmp(11) and `|>`(12). Inserting one forces a renumber cascade
through `||`/`&&`/`where`/comparisons. `..`(prec 10) was clean because it *shares*
the colon tier. Do the renumber as its own infra step before `-->`/`<|`.

**Suggested next targets (ranked):**
1. **Richer `import`/`using`** — `import .A`, `import A: x as y`, `using A.B: c`.
   ~21 corpus cases + ubiquitous; needs a real import-path tree. No precedence work.
2. **Multi-clause / comma generators** (`(x for a in as, b in bs)`, `… for … if …`).
   ~9 corpus cases (a coherent cluster).
3. **Precedence-table renumber** (infra), then arrow `-->`/`<-->` (prec 4, special
   head `(--> a b)`, dotted `.-->` → `dotcall-i`) and left-pipe `<|` (prec 8).
4. **Splat postfix precedence** — fix `x..y...` → `(... (.. x y))` (also `x:y...`).

## Earlier sessions

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
