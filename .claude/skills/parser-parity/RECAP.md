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

JS corpus (575 cases): **282 allowlisted**, 266 divergence, 27 unsupported.
Dir corpus: **42 allowlisted**, 5 blocked + 1 skipped (do_blocks).
Grammar bullets through "richer `import`/`using`" are `[x]` in `TODO.md`.

Deliberate (recorded) divergences, do not "fix": comparison chains (nested),
associative `a*b*c` (nested binary), numeric-literal display normalization,
triple-string dedent, `end`/`[1 +2]`/unterminated-string/incomplete-`do` error
shapes (dir `blocked.txt`).

## Latest session (2026-06-17f)

**Richer `import`/`using` path trees.** Replaced the verbatim passthrough with a
dedicated `parse_import_stmt` (`structural.rs`) that builds real nodes; the
projector now *reads* them (no reconstruction). New `SyntaxKind`s `IMPORT_PATH`
(leading `.`/`..`/`...` dots + dot-separated names) and `IMPORT_ALIAS` (`as`
rename wrapping a path; `as` is a contextual `Ident`, matched by text). A
top-level `:` token switches from base path to a comma-separated name list; `,`/`:`
kept as tokens so `project_import` groups base-vs-names by presence of the colon.
`project_import_path` expands each leading-dot token to one `.` per char and skips
the *separator* dots (only pre-first-name dots carry meaning); `project_import_alias`
emits `(as <importpath> <name>)`. Fixtures `import_paths` (parser + dir corpus).

JS allow **274 → 282** (+8: `import .A`/`..A`/`...A`/`....A`, `import A as B`,
`import A, y`, `import A: x as y`, `using A: x as y`); divergence 274 → 266,
unsupported unchanged (27). Dir allow 41 → 42. Zero regressions; green,
clippy/fmt clean.

**Trap learned:** a clause parser that emits leading whitespace via `push_range`
*before* deciding the clause is unrecognized will double-emit it (caller's verbatim
passthrough re-emits) → losslessness break on the deferred forms (`import @x`,
`import A: +`). Fix: parse the path into a scratch event buffer first, commit the
whitespace only on success. Always `--verify` the deferred/edge forms, not just the
happy path.

**Deferred (still divergences, carried verbatim):** operator-symbol names
(`import A.==`, `import A: +, ==`), `@macro`/`$interp` paths (`import @x`,
`import $A`), the `. .A` space-separated-dots form, and `import A; B` (`;` splits
statements). `export`'s name list is untouched (still passthrough).

**Suggested next targets (ranked):**
1. **Multi-clause / comma generators** (`(x for a in as, b in bs)`, `… for … if …`).
   ~9 corpus cases (a coherent cluster).
2. **Precedence-table renumber** (infra), then arrow `-->`/`<-->` (prec 4, special
   head `(--> a b)`, dotted `.-->` → `dotcall-i`) and left-pipe `<|` (prec 8).
   Blocked by cramped low-tier numbers (no integer gap at `=>`(4)/`||`(5) or
   cmp(11)/`|>`(12)) — do the renumber first.
3. **Splat postfix precedence** — fix `x..y...` → `(... (.. x y))` (also `x:y...`).
4. **Operator-symbol import names** (`import A: +`, `import A.==`) — extend the
   import-path grammar to accept operator tokens as name components.

## Earlier sessions

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
