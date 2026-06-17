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

JS corpus (575 cases): **273 allowlisted**, 273 divergence, 29 unsupported.
Dir corpus: **40 allowlisted**, 5 blocked + 1 skipped (do_blocks).
Grammar bullets through "broadcast `.&&`/`.||`" are `[x]` in `TODO.md`.

Deliberate (recorded) divergences, do not "fix": comparison chains (nested),
associative `a*b*c` (nested binary), numeric-literal display normalization,
triple-string dedent, `end`/`[1 +2]`/unterminated-string/incomplete-`do` error
shapes (dir `blocked.txt`).

## Latest session (2026-06-17d)

**Broadcast `.&&` / `.||`.** 5-file recipe (no prefix — they're infix-only).
`DotAndAnd`/`DotOrOr` lexed in the 3-char dotted table; share the `&&`/`||`
precedence tiers `(7,8)`/`(5,6)` in `infix_binding_power`; build ordinary
`BINARY_EXPR`s and project via new `Special(".&&")`/`Special(".||")` heads (NOT
`dotcall-i` — Julia gives `&&`/`||` their own special head, and the dotted forms
inherit it: `(.&& a b)` / `(.|| a b)`). Also added to `is_operator` (sexpr) and
`is_operator_kind` (ast/nodes `op_token`). Mixed-precedence chains `x .&& y .|| z`
match Julia; same-operator chains inherit `&&`/`||`'s pre-existing left-nesting
divergence (un-allowlisted). Fixture `dot_logical_operator` (parser + dir corpus).

JS allow **271 → 273** (`x .&& y`, `x .|| y`); unsupported 31 → 29, divergence
held at 273. Only `:.&&` (operator-as-value quote, like bare `~`) stays FAIL. Dir
allow 39 → 40. Zero regressions; green, clippy/fmt clean.

**Suggested next targets (ranked):**
1. **Richer `import`/`using`** — `import .A`, `import A: x as y`, `using A.B: c`.
   Several corpus cases + ubiquitous; needs a real import-path tree.
2. **Bare `:` Colon value** (`a[:]` → `(ref a :)`) — small, finishes symbol work.
3. **Multi-clause / comma generators** (`(x for a in as, b in bs)`, `… for … if …`).
4. **Range `..`** (`a..b`) and the `<|` pipe operator — small operator additions.

## Earlier sessions

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
