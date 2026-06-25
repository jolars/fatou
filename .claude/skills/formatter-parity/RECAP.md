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

Dir corpus (**6 fixtures**): **4 allowlisted**, 2 blocked
(`logical_tight_divergence` = `&&`/`||` whitespace Tenet-1 divergence;
`control_flow` = Runic return-insertion, a semantic rewrite, deferred).
Rules landed: operator/assignment spacing (`lower_binary`).

## Latest session (bootstrap)

Built the formatter's Runic differential oracle from scratch and landed the first
layout rule:

- **Rule**: `src/formatter/rules.rs` with `lower`/`lower_node`/`lower_transparent`
  and `lower_binary` — `BINARY_EXPR`/`ASSIGNMENT_EXPR` get one space on each side
  of the operator, except the tight `^`. `core::format_with_style` now routes
  through `rules::lower` instead of the verbatim-collect passthrough. Bails to
  transparent on any non-whitespace trivia / non-2-operand shape.
- **Oracle infra**: `scripts/update-runic-corpus.{sh,jl}` (mirror of the
  JuliaSyntax ones; `Runic.format_string` → `expected.jl`, pins `.runic-source`),
  `tests/runic_oracle.rs` (allowlist gate + `runic_full_report` ignored triage +
  disjoint/coverage checks), `tests/oracle/runic-{allowlist,blocked}.txt`,
  `runic-report.txt` gitignored.
- **Decisions**: direct parity as the gate (strengthens AGENTS.md's soft
  fixed-point framing for the bootstrap phase); `&&`/`||` canonicalized as spaced
  (blocked divergence); `^` tight.
- **LSP/editor**: formatting was already wired in `src/lsp.rs`; added
  `docs/editors/neovim.md` + README pointer so it's usable from nvim now.
- **Tests**: `tests/formatter.rs` narrowed to idempotence-only (parity moved to
  the oracle). All green.

### Ranked next targets

1. **Comparison chains** (`COMPARISON_EXPR`: `a == b == c`) — Runic spaces them;
   currently lowered transparently. Small, high-value.
2. **Calls / arg-lists** (`CALL_EXPR`/`ARG_LIST`): normalize `f( a ,b )` →
   `f(a, b)` — comma spacing, no inner-paren padding. Watch the break/group case
   for long arg lists (needs `Ir::group` + `Ir::Line`).
3. **Unary** (`UNARY_EXPR`): `- a` → `-a`; confirm Runic.
4. **Blocks / control flow indentation** — bigger; needs `HardLine`/`Indent` and
   careful idempotence. Return-insertion stays out (semantic, blocked).

## Earlier sessions

(none yet — this is the bootstrap)
