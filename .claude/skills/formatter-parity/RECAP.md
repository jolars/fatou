# formatter-parity recap

Rolling log. Read top-to-bottom: persistent traps → progress → latest session →
earlier log. Keep ≤ ~300 lines; demote the "Latest session" to a one-liner in the
"Earlier sessions" list each new session.

## Persistent traps & invariants

- **`rules::lower` is the only growth surface.** Add a `lower_node` arm per
  construct; build `Ir`; **bail to `lower_transparent` on any shape you don't
  fully model**. The transparent fallback (tokens verbatim, recurse into child
  nodes) is what keeps unhandled syntax lossless and the whole pass idempotent.
- **Direct parity is the gate**, but Fatou is **not** a Runic clone. Runic
  *preserves* user whitespace where Tenet 1 demands determinism — it normalizes
  neither `a&&b` nor `a && b` (same for `||`). Fatou canonicalizes (`&&`/`||` →
  spaced) and **records** the divergence in `runic-blocked.txt`. `^` is the
  opposite: Runic *always* packs it tight (`a ^ b` → `a^b`), so tight is the
  deterministic match. Probe **both** spacings before writing a rule.
- **Idempotence is universal** (`tests/formatter.rs`, all fixtures). Parity is
  per-allowlisted-slug (`tests/runic_oracle.rs`). Keep them separate.
- **Coverage is enforced** — `runic_corpus_fully_triaged` fails if a fixture is in
  neither allowlist nor blocked. A new fixture forces an accept-or-record decision.
- **Reseed the allowlist with the `grep -E '^#|^$'` header-preserving recipe.**
- **Report is gitignored; `expected.jl` is generated** — never hand-edit; mint via
  `scripts/update-runic-corpus.sh`.
- **`format <file>` rewrites in place** — pipe via stdin to inspect output.
- **Corpus pinned** to Runic in `.runic-source` (currently Runic 1.5.0 /
  Julia 1.12.6). Bump ⇒ re-run the script, re-triage.

## Progress

Dir corpus (**9 fixtures**): **7 allowlisted**, 2 blocked
(`logical_tight_divergence` = `&&`/`||` whitespace Tenet-1 divergence;
`control_flow` = Runic return-insertion, a semantic rewrite, deferred).
Rules landed: operator/assignment spacing (`lower_binary`), comparison chains
(`lower_comparison`), call/index arg lists (`lower_arg_list` +
`lower_keyword_arg`/`lower_parameters`), tuple/vector/brace collections
(`lower_collection`).

## Latest session (tuple/vector/brace collections)

Landed `lower_collection`, shared by `TUPLE_EXPR`, `VECT_EXPR`, and `BRACES`:

- **Rule**: emits the open/close bracket verbatim, drops incidental whitespace,
  joins `ARG` items with `", "` (no space before comma), and **drops the trailing
  comma** (`[a, b,]` → `[a, b]`) **except** a single-element tuple, where the comma
  is semantic and kept (`(a,)` stays `(a,)`). Bails to `lower_transparent` on a
  `;`-row matrix (`PARAMETERS` child, e.g. `[1, 2; 3, 4]` — already canonical so it
  still passes), comment/newline, doubled/orphaned comma, or any non-`ARG` child.
- **Fixture**: `collections/` (`( a , b )`, `(1,2,3)`, `(a,)`, `(a, b,)`,
  `[ 1 , 2 ]`, `[1,2,3]`, `[a, b,]`, `[a,]`, `{a, b}`, `{a,b,}`, nested
  `[ [1,2] , [3,4] ]`, empty `()`/`[]`/`{}`). Parity holds; allowlisted.
- **Notes**: `(a)` is a `PAREN_EXPR` (not a tuple) so it's untouched. Space-
  separated matrices `[1 2]`/`[1 2; 3 4]` are a distinct `MATRIX_EXPR` node and
  never reach this rule (left transparent — Runic preserves them too). **Unary is
  not a target**: Runic *preserves* unary spacing (`- a` → `- a`, `-a` → `-a`,
  `! a` → `! a`), so there's no normalization to do — the transparent fallback
  already matches.

### Ranked next targets

1. **Multi-line break for arg-lists/collections** — long lists need `Ir::group` +
   `Ir::Line` + `Ir::indent` (Runic indents 4, keeps the trailing comma when
   broken). The single-line rules already bail on multi-line, so this is additive.
2. **Matrices** (`MATRIX_EXPR` `[1 2; 3 4]`, vcat `[1;2;3]`): Runic preserves the
   space/`;` layout — probe carefully; likely mostly transparent already.
3. **Blocks / control flow indentation** — bigger; needs `HardLine`/`Indent` and
   careful idempotence. Return-insertion stays out (semantic, blocked).

## Earlier sessions

- **call/index arg lists**: `lower_arg_list` (`ARG_LIST`, shared by `CALL_EXPR`/
  `INDEX_EXPR`) + `lower_keyword_arg` (`f(x=1)` → `f(x = 1)`) + `lower_parameters`
  (`; `-led, `", "`-joined kwargs). Comma spacing, no bracket padding, single-line
  trailing-comma drop. Bails on comment/newline/doubled comma → multi-line passes
  through. Fixture `call_arg_lists/`.
- **comparison chains**: `lower_comparison` (`COMPARISON_EXPR`) — alternating
  operand/operator, every gap one space, >2 operands ok; bails on
  comment/newline/non-alternating/<2 operands. Fixture `comparison_chains/`.
  Surfaced a lexer gap (`===`/`!==`/tight `x!=y` mis-lex) handed to parser-parity;
  since landed upstream (`429fc22`, `d7028d3`).

- **bootstrap**: built the Runic differential oracle from scratch
  (`scripts/update-runic-corpus.{sh,jl}`, `tests/runic_oracle.rs`,
  `runic-{allowlist,blocked}.txt`) and landed the first rule, `lower_binary`
  (`BINARY_EXPR`/`ASSIGNMENT_EXPR` → one space each side, `^` tight; `&&`/`||`
  canonicalized-spaced and blocked as a Tenet-1 divergence). `core::format` now
  routes through `rules::lower`; `tests/formatter.rs` narrowed to idempotence.
  Wired nvim formatting docs.
