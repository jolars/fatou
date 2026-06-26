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

Dir corpus (**11 fixtures**): **9 allowlisted**, 2 blocked
(`logical_tight_divergence` = `&&`/`||` whitespace Tenet-1 divergence;
`control_flow` = Runic return-insertion, a semantic rewrite, deferred).
Rules landed: operator/assignment spacing (`lower_binary`), comparison chains
(`lower_comparison`), call/index arg lists (`lower_arg_list` +
`lower_keyword_arg`/`lower_parameters`), tuple/vector/brace collections
(`lower_collection`), tight range `:` (`lower_range` + `COLON` in
`is_tight_binop`), `::` type annotations (`lower_type_annotation`).

## Latest session (tight range `:` and `::` type annotations)

Two small "tighten an operator to no spaces" rules, both confirmed against Runic:

- **Range `:`** — Runic *always* packs ranges tight (`1 : 2` → `1:2`, `a : b` →
  `a:b`, `1:length(x)`). Two parser shapes: the two-operand range `a:b` is a
  `BINARY_EXPR` with a `COLON` op (fixed by adding `COLON` to `is_tight_binop` —
  Fatou was *mangling* `1:2` → `1 : 2`, a latent bug with no fixture); the stepped
  `1:2:10` is a `RANGE_EXPR` (new `lower_range`: alternate operand/`:`, all tight,
  ≥2 operands, bail on comment/newline/non-alternating). Fixture `range_colon/`.
- **`::`** — `TYPE_ANNOTATION` node (was transparent, so `x :: Int` leaked
  through). Runic packs tight (`x::Int`). New `lower_type_annotation`: lower
  operands, emit `::` with no spaces, bail on comment/newline/extra token/missing
  `::`. Handles `x::Int`, bare `::Int`, call args `f(x::Int)`. Fixture
  `type_annotations/`.
- **Divergence noted (out of scope, kept out of fixtures):** Runic *parenthesizes*
  compound range operands (`a + 1 : b` → `(a + 1):b`) — a semantic rewrite, not a
  spacing rule. Fatou tightens the colon and recurses (`a + 1:b`), lossless and
  idempotent but unparenthesized; correct only for simple operands (literals,
  names, calls, indices), which is what the fixture uses.

### Ranked next targets

1. **Multi-line break for arg-lists/collections** — the headline target, but
   **larger than the RECAP previously assumed**. Fully probed this session; the
   real Runic behavior:
   - **NOT width-based.** Runic never auto-breaks a long single line
     (`foo(<90 chars on one line>)` stays one line). The trigger is whether the
     bracket's **content text spans ≥2 source lines** (a `\n` anywhere inside,
     *including inside a triple-quoted string* — so detect by `node.text()`
     containing `'\n'`, **not** a `NEWLINE` token kind).
   - **Contagious / propagating.** `foo([\n1,\n2\n])` breaks the *outer* call too,
     even though the only newline is inside the inner vector. A bracket goes
     vertical iff any descendant spans lines. (`Ir::Group` + a forced break would
     model this via the `fits`-fails-on-`HardLine` contagion, but our `Ir` has no
     "force-break" flag yet.)
   - **Preserves internal line breaks, not "explode every item."** `foo(g(a,\nb),
     c)` → `g(...)` breaks but `c` stays on the **same line** as g's `)` (`), c`).
     Runic only adds *framing* newlines (after the open bracket, before the close)
     + 4-space indent; between items it keeps the source's space-vs-newline.
     **Blank lines between items are preserved** (even 2+ consecutive).
   - **Per-bracket trailing comma when multiline:** call `foo(...)` **preserves**
     (keeps iff present, never adds); index `x[...]`, tuple `(...)`, vect `[...]`,
     braces `{...}` **always add** a trailing comma.
   - Plan: detect multiline by text `\n`; bail to transparent on comments,
     `PARAMETERS`/`;`-semicolons, and splats for v1; emit framing `HardLine`s +
     `Indent`, preserve inter-item space/newline, apply the per-bracket trailing
     comma. Watch idempotence (already-canonical must round-trip) and nested indent
     (inner list's items land at +8). Probably wants an `Ir` force-break primitive.
2. **Matrices** — single-line is **pure preservation**: Runic does *not* even
   collapse `[1  2   3]` → stays `[1  2   3]`; `[1 2; 3 4]`, `[1;2;3]`, `[1 2 ;3 4]`
   all preserved. Fatou's transparent fallback already matches, so a `matrices/`
   fixture would PASS rule-free (regression lock only, no rule). Multiline matrices
   `[1 2\n3 4]` get the framing treatment (folds into target #1).
3. **Blocks / control flow indentation** — bigger; needs `HardLine`/`Indent` and
   careful idempotence. Return-insertion stays out (semantic, blocked).

## Earlier sessions

- **tuple/vector/brace collections**: `lower_collection` (`TUPLE_EXPR`/`VECT_EXPR`/
  `BRACES`) — open/close verbatim, drop incidental ws, join `ARG`s with `", "`,
  drop trailing comma **except** the semantic 1-tuple `(a,)`. Bails on `;`-row
  matrix (`PARAMETERS`), comment/newline, doubled comma, non-`ARG`. `(a)` is a
  `PAREN_EXPR` (untouched); space-separated matrices are `MATRIX_EXPR` (transparent,
  Runic preserves). Unary is Runic-preserved → no rule. Fixture `collections/`.
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
