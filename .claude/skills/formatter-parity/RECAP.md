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

Dir corpus (**8 fixtures**): **6 allowlisted**, 2 blocked
(`logical_tight_divergence` = `&&`/`||` whitespace Tenet-1 divergence;
`control_flow` = Runic return-insertion, a semantic rewrite, deferred).
Rules landed: operator/assignment spacing (`lower_binary`), comparison chains
(`lower_comparison`), call/index arg lists (`lower_arg_list` +
`lower_keyword_arg`/`lower_parameters`).

## Latest session (call/index arg lists)

Landed `ARG_LIST` normalization (shared by `CALL_EXPR` and `INDEX_EXPR`):

- **Rules**: `lower_arg_list` walks the bracketed list — emits the open/close
  bracket verbatim, drops incidental whitespace, joins `ARG`/`KEYWORD_ARG` items
  with `", "` (no space before comma), and **drops a single-line trailing comma**
  (`g(a,)` → `g(a)`). A trailing `PARAMETERS` node attaches without a comma (the
  `;` is the separator). `lower_keyword_arg` spaces the `=` (`f(x=1)` →
  `f(x = 1)`); `lower_parameters` emits `; ` then `", "`-joined kwargs
  (`f(a; b=1)` → `f(a; b = 1)`). All three bail to `lower_transparent` on any
  comment/newline, doubled/orphaned comma, missing separator, or unexpected child
  — so **multi-line arg lists pass through byte-identical** (left for a later
  group/break rule).
- **Fixture**: `call_arg_lists/` (`f( a ,b )`, `foo(1,2,3)`, `g(a,)`, `a[ 1 , 2 ]`,
  `f(x=1)`, `f(a; b=1)`, `f(a, b; c=2, d=3)`, splat `f(a, b...)`, `println("x")`).
  Parity holds; allowlisted.
- **Note**: tuples `(a, b)` and vectors `[1, 2]` are *not* `ARG_LIST` (separate
  nodes) — kept out of this fixture, queued as a next target. `f (x)` (space
  before opener) is a Julia parse error, so it's not a case to handle.

### Ranked next targets

1. **Unary** (`UNARY_EXPR`): `- a` → `-a`; confirm Runic.
2. **Tuples / vectors / matrices** (`(a, b)`, `[1, 2]`, `[1, 2; 3, 4]`): same
   comma/bracket spacing as arg lists but distinct nodes — could share a helper.
3. **Calls/arg-lists multi-line break** — long lists need `Ir::group` + `Ir::Line`
   + `Ir::indent` (Runic indents 4, keeps the trailing comma when broken).
4. **Blocks / control flow indentation** — bigger; needs `HardLine`/`Indent` and
   careful idempotence. Return-insertion stays out (semantic, blocked).

## Earlier sessions

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
