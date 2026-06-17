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

JS corpus (575 cases): **271 allowlisted**, 273 divergence, 31 unsupported.
Dir corpus: **39 allowlisted**, 5 blocked + 1 skipped (do_blocks).
Grammar bullets through "the `~` operator" are `[x]` in `TODO.md`.

Deliberate (recorded) divergences, do not "fix": comparison chains (nested),
associative `a*b*c` (nested binary), numeric-literal display normalization,
triple-string dedent, `end`/`[1 +2]`/unterminated-string/incomplete-`do` error
shapes (dir `blocked.txt`).

## Latest session (2026-06-17c)

**`~` / `.~` operator.** 5-file recipe + prefix. `Tilde`/`DotTilde` lexed; infix at
the assignment tier `(2,1)` right-assoc (added to `infix_binding_power`, *not*
`is_assignment_op`, so it stays a `BINARY_EXPR` → `(call-i a ~ b)` / `(dotcall-i a
~ b)`); prefix `~a`/`.~x` reuse the unary arm → `(call-pre ~ a)`/`(dotcall-pre ~
x)`; `project_unary` gained a `DOT_TILDE` arm. The whitespace-sensitive matrix
splitting (`[a ~b]` hcat-of-prefix vs `[a~b]`/`[a ~ b]` infix) fell out of the
shared `is_operator` machinery for free — verified all 19 probe shapes match Julia.
Fixture `tilde_operator` (parser + dir corpus).

JS allow **264 → 271** (`a ~ b`, `a .~ b`, `.~x`, `global x ~ 1`, `[a ~b]`,
`[a~b]`, `[a ~ b c]`); unsupported 32 → 31, divergence 279 → 273. Only bare `~`
(operator-as-value) stays FAIL. Dir allow 38 → 39. Zero regressions; green,
clippy/fmt clean.

**Suggested next targets (ranked):**
1. **Broadcast logical `.&&` / `.||`** — mirrors the `.+` dotted family; corpus
   cases `x .&& y`, `x .|| y`. Cheap.
2. **Richer `import`/`using`** — `import .A`, `import A: x as y`, `using A.B: c`.
   Several corpus cases + ubiquitous; needs a real import-path tree.
3. **Bare `:` Colon value** (`a[:]` → `(ref a :)`) — small, finishes symbol work.
4. **Multi-clause / comma generators** (`(x for a in as, b in bs)`, `… for … if …`).
5. **Range `..`** (`a..b`) and the `<|` pipe operator — small operator additions.

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
