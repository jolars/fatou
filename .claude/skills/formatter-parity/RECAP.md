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
  *preserves* user whitespace where Tenet 1 demands determinism—it normalizes
  neither `a&&b` nor `a && b` (same for `||`). Fatou canonicalizes (`&&`/`||` →
  spaced) and **records** the divergence in `runic-blocked.txt`. `^` is the
  opposite: Runic *always* packs it tight (`a ^ b` → `a^b`), so tight is the
  deterministic match. Probe **both** spacings before writing a rule.
- **Idempotence is universal** (`tests/formatter.rs`, all fixtures). Parity is
  per-allowlisted-slug (`tests/runic_oracle.rs`). Keep them separate.
- **Coverage is enforced**—`runic_corpus_fully_triaged` fails if a fixture is in
  neither allowlist nor blocked. A new fixture forces an accept-or-record decision.
- **Reseed the allowlist with the `grep -E '^#|^$'` header-preserving recipe.**
- **Report is gitignored; `expected.jl` is generated**—never hand-edit; mint via
  `scripts/update-runic-corpus.sh`.
- **`format <file>` rewrites in place**—pipe via stdin to inspect output.
- **Corpus pinned** to Runic in `.runic-source` (currently Runic 1.5.0/Julia
  1.12.6). Bump ⇒ re-run the script, re-triage.

## Progress

Dir corpus (**16 fixtures**): **14 allowlisted**, 2 blocked
(`logical_tight_divergence` = `&&`/`||` whitespace Tenet-1 divergence;
`control_flow` = Runic return-insertion, a semantic rewrite, deferred).
Rules landed: operator/assignment spacing (`lower_binary`), comparison chains
(`lower_comparison`), call/index arg lists (`lower_arg_list` +
`lower_keyword_arg`/`lower_parameters`), tuple/vector/brace collections
(`lower_collection`), tight range `:` (`lower_range` + `COLON` in
`is_tight_binop`), `::` type annotations (`lower_type_annotation`), multi-line
bracket breaking (`lower_multiline_bracket`, shared by arg-lists + collections),
multi-line matrix breaking (`lower_matrix`), interior blank-line preservation in
both (the `Ir::BlankLine` primitive).

## Latest session (interior blank-line preservation)

**New IR primitive:** `Ir::BlankLine` — a bare `\n` at **column 0** (the printer
pushes `\n`, sets `col=0`, skips the indent). This is the piece both broken
brackets and matrices were missing: a `HardLine` always re-indents, so an
otherwise-empty line would carry trailing indent whitespace; `BlankLine` emits the
truly-empty line. In `fits` it forces a break like `HardLine` (never actually
reached — multiline layouts use no `Group`).

**Probed cap:** Runic keeps blank lines but **caps at 2** everywhere (top level,
brackets, matrices): 1→1, 2→2, 3→2, 4→2. Encoded as `MAX_BLANK_LINES = 2`.

**Bracket** (`lower_multiline_bracket`): the inter-item `Sep` now carries
`Newline { blanks }` (`= (newlines-1).min(2)`); emit `blanks` × `BlankLine` then
the `HardLine`. So `[1,\n\n2,3]` → `1,` blank `2,`. Mixed seps compose: `1, 2,`
on one line then a blank then `3` is preserved. **Leading gap** (open→first item)
and **trailing gap** (last item→close) still **bail to transparent** on a blank —
the framing break owns those gaps and a blank there isn't yet expressible. (Runic
*does* preserve leading/trailing-gap blanks; that's the next increment.)

**Matrix** (`lower_matrix`): interior empty content lines are no longer a bail —
each becomes a `BlankLine` (run capped at 2); leading/trailing empty lines are
still dropped into the framing break (a **known, ungated divergence**: Runic keeps
a blank right after `[`/before `]` in a matrix — kept out of the fixture, flagged
below).

Verified byte-identical to Runic across vector/call/tuple/braces/index/column-
vector/2D-matrix, the cap, and mixed same-line+blank. Idempotent (re-parsing 1
blank = 2 newlines → blanks=1; 2 blanks = 3 newlines → blanks=2, both fixed
points). Fixtures `bracket_blank_lines/`, `matrix_blank_lines/`.

## Earlier session (multi-line matrix breaking)

**Surprise:** multi-line matrices are **not** pure preservation (the prior recap's
guess). Runic *reframes* a `MATRIX_EXPR` that spans ≥2 lines exactly like
`lower_multiline_bracket`: `[` + `HardLine`, each source line re-indented one step,
`HardLine` + `]`. The interior is otherwise kept **verbatim**—intra-row spacing
(`1  2`), multi-space same-line gaps (`1 2;   3 4;`), same-line `;`-rows, and `;`
placement (`1 2 ;`, trailing `3 4;`) are all preserved; only each line's
leading/trailing whitespace is dropped for the standard indent. Single-line
matrices still have **no rule**—`has_newline_token` gate returns transparent, which
is byte-identical (the `matrices/` regression lock still holds).

`lower_matrix` (new arm): splits children on `NEWLINE` into lines of `(is_ws, ir)`
elements, trims end-whitespace per line, drops leading/trailing empty lines (a bare
newline after `[`/before `]` is absorbed into the framing break), emits
`HardLine`+line per content line. **Row shapes:** a multi-element row is a
`MATRIX_ROW` node; a single-element row (newline-separated column `[1\n2\n3]`) is a
bare `ARG`—**both** handled, lowered via `lower_node` so nested calls/indices still
normalize inside rows (`f( x )`→`f(x)`). **Bails to transparent (kept out of the
fixture, lossless):** a blank line (≥2 consecutive `NEWLINE`s ⇒ an empty *middle*
line, which the IR can't emit without a bare un-indented newline), a comment
(direct `COMMENT` child token), missing/extra bracket, any unexpected token. Verified
byte-identical to Runic across row/column/2D/mixed-`;`/multispace/trailing-`;`/
nested/leading-newline/trailing-newline shapes. Fixture `multiline_matrices/`.

## Earlier session (single-line matrices — regression lock, no rule)

Top-ranked cheap win, landed exactly as predicted: **rule-free PASS**. Runic
*preserves* single-line matrices verbatim—no whitespace collapse even in
`[1  2   3]`, and `[1 2 ;3 4]` keeps the space before `;`. `MATRIX_EXPR` (with
`MATRIX_ROW` children for `;`-separated rows) has no `lower_node` arm, so the
transparent fallback emits every token verbatim → byte-identical to Runic. The
`matrices/` fixture is a **regression lock only**: it pins the preservation so a
future bracket/break rule can't start mangling matrices. Covered shapes: row
`[1 2 3]`, column `[1; 2; 3]`, 2D `[1 2; 3 4]`, names/floats, **nested call/index
operands** (`[f(x) g(y)]`, `[x[1] x[2]; …]` — these exercise handled descendants
staying normalized inside a transparent matrix), multi-space, odd `;` spacing.
Multiline matrices (`[1 2\n3 4]`) deliberately **not** in the fixture—distinct
shape, probe separately before claiming (likely also preserved, but unverified).
Files: `tests/fixtures/formatter/matrices/{input,expected}.jl`, allowlist (+1).

## Earlier session (multi-line bracket breaking)

The headline target landed for the no-comment/no-blank-line common case. A
bracket goes vertical iff its content spans ≥2 source lines, detected by **any
`NEWLINE` token among its descendants** (`has_newline_token`)—this gives the
**contagion** for free (`foo(g(a,\nb), c)` breaks the outer call because g's
newline is a descendant) while *not* triggering on a `\n` buried inside an
un-reflowable string (we'd only half-break it). Both `lower_arg_list` and
`lower_collection` dispatch to the shared `lower_multiline_bracket` at the top.

The layout is **source-driven, not width-based** (so it sidesteps the `fits`
engine entirely): emit a framing `HardLine` after the open bracket and before the
close (close lands at the bracket's own indent, content one step in via
`Ir::indent`), then walk items. **Inter-item space-vs-break is preserved**—the
source's comma-gap newline count decides `Sep::Newline` (→ `,` + `HardLine`) vs
`Sep::Space` (→ `, `); Runic only adds the *framing* breaks, never explodes
same-line items (`), c` stays). Trailing comma per `adds_trailing_comma`: calls
**preserve** (keep iff present), index/tuple/vect/braces **add**. Idempotent
(framing breaks re-parse to the same `NEWLINE` tokens) and verified against Runic
on call/nested/vect/tuple/braces/index/kwarg cases. Fixture `multiline_brackets/`.

- **Bails to transparent (kept out of the fixture, lossless):** comments,
  `PARAMETERS`/bare `;`, **blank lines** (≥2 consecutive newlines in any gap —
  Runic preserves them but our `HardLine` always re-indents, so a bare blank line
  isn't yet expressible), doubled/leading comma, two items with no comma, empty
  bracket, any unexpected child/token. Splats need no special-casing: `x...` is an
  `ARG` wrapping `SPLAT_EXPR`, lowered transparently.
- **Known divergence (out of scope):** a bracket whose only newline lives inside a
  triple-quoted string—Runic breaks the bracket *and* re-indents the string body;
  Fatou (token-based detection) leaves it inline. String reindentation is a
  separate construct.

## Earlier session (tight range `:` and `::` type annotations)

Two small "tighten an operator to no spaces" rules, both confirmed against Runic:

- **Range `:`**—Runic *always* packs ranges tight (`1 : 2` → `1:2`, `a : b` →
  `a:b`, `1:length(x)`). Two parser shapes: the two-operand range `a:b` is a
  `BINARY_EXPR` with a `COLON` op (fixed by adding `COLON` to `is_tight_binop` —
  Fatou was *mangling* `1:2` → `1 : 2`, a latent bug with no fixture); the stepped
  `1:2:10` is a `RANGE_EXPR` (new `lower_range`: alternate operand/`:`, all tight,
  ≥2 operands, bail on comment/newline/non-alternating). Fixture `range_colon/`.
- **`::`**—`TYPE_ANNOTATION` node (was transparent, so `x :: Int` leaked
  through). Runic packs tight (`x::Int`). New `lower_type_annotation`: lower
  operands, emit `::` with no spaces, bail on comment/newline/extra token/missing
  `::`. Handles `x::Int`, bare `::Int`, call args `f(x::Int)`. Fixture
  `type_annotations/`.
- **Divergence noted (out of scope, kept out of fixtures):** Runic *parenthesizes*
  compound range operands (`a + 1 : b` → `(a + 1):b`)—a semantic rewrite, not a
  spacing rule. Fatou tightens the colon and recurses (`a + 1:b`), lossless and
  idempotent but unparenthesized; correct only for simple operands (literals,
  names, calls, indices), which is what the fixture uses.

### Ranked next targets

1. **Comment preservation inside broken brackets *and matrices***—the remaining
   half of the old target #1 (blank lines now land; see latest session). Comments
   are the hard part: placement (own-line vs trailing `# …`), the trailing-`#`-
   forces-the-next-token-onto-a-newline interaction, and the matrix-row case. Both
   `lower_multiline_bracket` and `lower_matrix` still bail on any `COMMENT`.
2. **Leading/trailing-gap blank lines** (cheap follow-on to this session)—Runic
   preserves a blank right after the open bracket / before the close, and right
   after `[` / before `]` in a matrix; Fatou still bails (brackets) or silently
   drops (matrices — an ungated divergence). The matrix leading/trailing drop is
   the one to fix first: emit a `BlankLine` for a dropped leading/trailing empty
   line instead of absorbing it. Probe the exact framing interaction first.
3. **Blocks/control flow indentation**—bigger; needs `HardLine`/`Indent` and
   careful idempotence. Return-insertion stays out (semantic, blocked).
4. **Long single-line bracket/matrix reflow** (width-based breaking)—Fatou's
   breaking is purely source-driven (newline-triggered). Runic also breaks on
   width. Probe whether Runic reflows a long single-line `[…]`/call past the margin;
   if so this needs the `fits` engine, not just `HardLine`s.

## Earlier sessions

- **tuple/vector/brace collections**: `lower_collection` (`TUPLE_EXPR`/`VECT_EXPR`/
  `BRACES`)—open/close verbatim, drop incidental ws, join `ARG`s with `", "`,
  drop trailing comma **except** the semantic 1-tuple `(a,)`. Bails on `;`-row
  matrix (`PARAMETERS`), comment/newline, doubled comma, non-`ARG`. `(a)` is a
  `PAREN_EXPR` (untouched); space-separated matrices are `MATRIX_EXPR` (transparent,
  Runic preserves). Unary is Runic-preserved → no rule. Fixture `collections/`.
- **call/index arg lists**: `lower_arg_list` (`ARG_LIST`, shared by `CALL_EXPR`/
  `INDEX_EXPR`) + `lower_keyword_arg` (`f(x=1)` → `f(x = 1)`) + `lower_parameters`
  (`; `-led, `", "`-joined kwargs). Comma spacing, no bracket padding, single-line
  trailing-comma drop. Bails on comment/newline/doubled comma → multi-line passes
  through. Fixture `call_arg_lists/`.
- **comparison chains**: `lower_comparison` (`COMPARISON_EXPR`)—alternating
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
