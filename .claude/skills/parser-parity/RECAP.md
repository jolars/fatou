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

JS corpus (575 cases): **259 allowlisted**, ~276 divergence, ~36 unsupported.
Dir corpus: **37 allowlisted**, 5 blocked + 1 skipped (do_blocks).
Grammar bullets through "pair `=>`" are `[x]` in `TODO.md`.

Deliberate (recorded) divergences, do not "fix": comparison chains (nested),
associative `a*b*c` (nested binary), numeric-literal display normalization,
triple-string dedent, `end`/`[1 +2]`/unterminated-string/incomplete-`do` error
shapes (dir `blocked.txt`).

## Latest session (2026-06-17)

Built the oracle from scratch, then ran the loop 3×.

**What landed:**
- `feat: add JuliaSyntax differential oracle` — projector (`sexpr.rs`,
  `--to sexpr`), harness, curated corpus (40, 34 allow / 6 blocked), refresh
  scripts.
- `test: harvest JuliaSyntax corpus` — 575-case `juliasyntax.jsonl` from
  `test/parser.jl`; opt-in gate; seeded 251 allow.
- `feat: a[begin] index marker` — `BEGIN_MARKER`, `begin_marker` flag scoped to
  indexing (`ARG_LIST` + `]`) so `[begin x end]` stays a block. +1 JS.
- `feat: :foo / :(x+1) symbol quotes` — `QUOTE_SYM` via `parse_quote_sym`
  (mirrors `$`-interpolation); `TokKind::is_keyword`; bare `:` falls through. +5 JS.
- `feat: pair operator =>` (and `.=>`) — `FatArrow`/`DotFatArrow`, arrow tier
  `(4,3)` right-assoc; unblocks `Dict(:a => 1)`. +2 JS.

JS allow 251 → 259, zero regressions across all three. All green; clippy/fmt clean.

**Suggested next targets (ranked):**
1. **Richer `import`/`using`** — `import .A`, `import A: x as y`, `using A.B: c`.
   Several corpus cases + ubiquitous. Parser-gap (header passthrough is loose
   tokens today); needs a real import-path tree.
2. **Broadcast logical `.&&` / `.||`** — cheap, mirrors the `.+` dotted family
   already lexed; a few corpus cases.
3. **Bare `:` Colon value** (`a[:]` → `(ref a :)`) — small, finishes the symbol
   work; deferred in commit `7199814`.
4. **Multi-clause / comma generators** (`(x for a in as, b in bs)`,
   `… for … for … if …`) — several unsupported cases.
5. (Record, don't fix) associative n-ary flattening — high blast radius
   (n-ary `BINARY_EXPR` ripples into formatter/snapshots); likely a permanent
   recorded divergence, not a feature.

## Earlier sessions

(none yet — this is the first)
