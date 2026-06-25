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

Dir corpus (**7 fixtures**): **5 allowlisted**, 2 blocked
(`logical_tight_divergence` = `&&`/`||` whitespace Tenet-1 divergence;
`control_flow` = Runic return-insertion, a semantic rewrite, deferred).
Rules landed: operator/assignment spacing (`lower_binary`), comparison chains
(`lower_comparison`).

## Latest session (comparison chains)

Landed `COMPARISON_EXPR` spacing:

- **Rule**: `lower_comparison` in `src/formatter/rules.rs`. The node alternates
  operand/operator and may hold >2 operands (`a == b == c`); comparison ops are
  never tight, so every gap is one space. Walks children in source order,
  dropping incidental whitespace, building `[operand, " ", op, " ", operand, …]`.
  Bails to `lower_transparent` on any interleaved comment/newline, a
  non-alternating shape, or `<2` operands.
- **Fixture**: `comparison_chains/` (`a==b`, `a<b<=c`, `1<2<3<4`, `i<=n>=0`,
  `x != y`). Parity holds; allowlisted.
- **Trap found**: Fatou's **lexer** mis-tokenizes `===`, `!==`, and tight `x!=y`
  (`x!` is read as an identifier). These are **parser gaps**, not formatter bugs —
  the formatter correctly bails to transparent on the resulting ERROR nodes. Kept
  them out of the fixture; spaced `!=` works fine. Revisit when the parser grows
  `===`/`!==`.

### Ranked next targets

1. **Calls / arg-lists** (`CALL_EXPR`/`ARG_LIST`): normalize `f( a ,b )` →
   `f(a, b)` — comma spacing, no inner-paren padding. Watch the break/group case
   for long arg lists (needs `Ir::group` + `Ir::Line`).
2. **Unary** (`UNARY_EXPR`): `- a` → `-a`; confirm Runic.
3. **Blocks / control flow indentation** — bigger; needs `HardLine`/`Indent` and
   careful idempotence. Return-insertion stays out (semantic, blocked).

## Earlier sessions

- **bootstrap**: built the Runic differential oracle from scratch
  (`scripts/update-runic-corpus.{sh,jl}`, `tests/runic_oracle.rs`,
  `runic-{allowlist,blocked}.txt`) and landed the first rule, `lower_binary`
  (`BINARY_EXPR`/`ASSIGNMENT_EXPR` → one space each side, `^` tight; `&&`/`||`
  canonicalized-spaced and blocked as a Tenet-1 divergence). `core::format` now
  routes through `rules::lower`; `tests/formatter.rs` narrowed to idempotence.
  Wired nvim formatting docs.
