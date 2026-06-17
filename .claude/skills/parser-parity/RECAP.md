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

JS corpus (575 cases): **264 allowlisted**, 279 divergence, 32 unsupported.
Dir corpus: **38 allowlisted**, 5 blocked + 1 skipped (do_blocks).
Grammar bullets through "augmented assignment `op=`" are `[x]` in `TODO.md`.

Deliberate (recorded) divergences, do not "fix": comparison chains (nested),
associative `a*b*c` (nested binary), numeric-literal display normalization,
triple-string dedent, `end`/`[1 +2]`/unterminated-string/incomplete-`do` error
shapes (dir `blocked.txt`).

## Latest session (2026-06-17b)

**Augmented assignment `op=`** (parity-driven ASCII set). 5-file operator recipe:
16 new TokKinds/SyntaxKinds — ASCII `+= -= *= /= //= ^= %= |= &=` and broadcast
`.+= .-= .*= ./= .//= .^= .%=`. Lexer longest-match: `.//=` 4-char beats `.//`,
`//=` 3-char beats `//`, `.+=` family in the 3-char dotted block. Parser: an
`is_assignment_op` helper folds them into the existing `ASSIGNMENT_EXPR` arm and
the `(2,1)` right-assoc tier (used in 3 spots formerly keyed on `Eq`/`DotEq`).
Projector: `project_assignment` now reads the head from the operator token's own
text (`(+= a b)`, `(.+= a b)`) — generalization, not compensation. `global x += 1`
and `let x += 1` parse for free. Fixture `augmented_assignment` (parser + dir
corpus).

JS allow **259 → 264** (the 4 aug corpus cases + `if true; public *= 4; end`),
unsupported 38 → 32, divergence +1 (`:+=` moved UNSUPPORTED → FAIL: now lexes as
one token but operator-symbol quoting is deferred). Dir allow 37 → 38. Zero
regressions; all green, clippy/fmt clean.

**Suggested next targets (ranked):**
1. **`~` / `.~` operator** (`a ~ b` → `(call-i a ~ b)`, `a .~ b` → dotcall). Cheap
   `CallI`/`DotCallI` operator; a ~7-case FAIL cluster (`a ~ b`, `[a ~b]`, `.~x`,
   `global x ~ 1`, …). Probe whitespace siblings (`[a~b]` vs `[a ~b]`).
2. **Broadcast logical `.&&` / `.||`** — mirrors the `.+` dotted family; a few
   corpus cases (`x .&& y`, `x .|| y`).
3. **Richer `import`/`using`** — `import .A`, `import A: x as y`, `using A.B: c`.
   Several corpus cases + ubiquitous; needs a real import-path tree.
4. **Bare `:` Colon value** (`a[:]` → `(ref a :)`) — small, finishes symbol work.
5. **Multi-clause / comma generators** (`(x for a in as, b in bs)`, `… for … if …`).

## Earlier sessions

- **2026-06-17a** — Built the oracle from scratch + ran the loop 3×: JuliaSyntax
  differential oracle (projector `sexpr.rs` + `--to sexpr`, harness, curated +
  harvested corpora, refresh scripts); `a[begin]` index marker (+1 JS); `:foo` /
  `:(x+1)` symbol quotes via `parse_quote_sym` (+5 JS); pair operator `=>`/`.=>`
  on arrow tier `(4,3)` (+2 JS). JS allow 251 → 259.
