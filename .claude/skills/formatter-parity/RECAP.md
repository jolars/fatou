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

Dir corpus (**23 fixtures**): **21 allowlisted**, 2 blocked
(`logical_tight_divergence` = `&&`/`||` whitespace Tenet-1 divergence;
`control_flow` = Runic return-insertion, a semantic rewrite, deferred).
Rules landed: operator/assignment spacing (`lower_binary`), arrow/anon-function
spacing (`lower_arrow`), comparison chains
(`lower_comparison`), call/index arg lists (`lower_arg_list` +
`lower_keyword_arg`/`lower_parameters`), tuple/vector/brace collections
(`lower_collection`), tight range `:` (`lower_range` + `COLON` in
`is_tight_binop`), `::` type annotations (`lower_type_annotation`), tight
field-access `.` (`DOT` in `is_tight_binop`), multi-line
bracket breaking (`lower_multiline_bracket`, shared by arg-lists + collections),
multi-line matrix breaking (`lower_matrix`), blank-line preservation in both
(interior **and** leading/trailing gaps, via the `Ir::BlankLine` primitive),
ternary spacing (`lower_ternary`), curly type-param padding (`lower_arg_list`
extended to brace `ARG_LIST`s), keyword-statement spacing (`lower_keyword_stmt`).

## Latest session (keyword-statement spacing)

`RETURN_EXPR`/`CONST_STMT`/`GLOBAL_STMT`/`LOCAL_STMT` were **transparent**, so the
incidental space between the keyword and its operand leaked (`return  x` →
`return  x`); Runic collapses it to exactly one (`return x`, `const x = 1`,
`global z`, `local w`). New `lower_keyword_stmt` (one arm for all four nodes): walk
children, the first non-ws token is the keyword, then expect **at most one operand
node and nothing else**. Emit `kw` + `" "` + `lower_node(operand)` (so the operand
keeps normalizing: `return  x+1` → `return x + 1`, `return  f(a,b)` →
`return f(a, b)`, `const  y=2` → `const y = 2`), or just `kw` for a bare `return`.
Bails to transparent on anything past one operand—**comments** (`return  x  # c`:
Runic keeps the trailing `  # c` spacing, transparent preserves it; bare
`return  # comment` is byte-identical too) and **comma name lists**
(`global a, b`, a bare-tuple shape we don't model). `return  x,y` likewise bails
(its operand is a `BARE_TUPLE_EXPR`, whose `x,y`→`x, y` comma spacing is a separate
unlanded construct). Verified byte-identical to Runic across the fixture
(name/binop/call/paren-tuple/`^` operands, bare `return`, `const`/`global`/`local`).
Idempotent. Fixture `keyword_statements/`. Corpus 20→21 pass, divergence held at 2;
allowlist 20→21. No upstream blocker surfaced.

## Earlier session (curly type-param padding)

Closed ranked target #0 (cheap, pre-probed). A `CURLY_EXPR` (`Vector{Int}`,
`Dict{A, B}`) wraps a brace-bracketed `ARG_LIST` (`LBRACE`/`RBRACE` tokens), but
`lower_arg_list` only recognized `()`/`[]` brackets, so it hit the catch-all and
**bailed to transparent** — leaking the inner padding (`Vector{ Int }`). One-line
fix: add `LBRACE`/`RBRACE` to the bracket-token arm of `lower_arg_list`. Type
params then get the same normalization as call/index args: strip bracket padding
(`Vector{ Int }` → `Vector{Int}`), no space before a comma + one after
(`Dict{ A ,B }` → `Dict{A, B}`, `Array{Int,2}` → `Array{Int, 2}`), trailing comma
dropped (`Array{Int,}` → `Array{Int}`), and the `; `-led `PARAMETERS` case
(`x{a; b}`) flows through `lower_parameters` unchanged. Empty `Foo{}`, nested
`Vector{Vector{Int}}`, `where {T}` (its `BRACES` is a separate node, untouched),
and `x::Vector{Int}` all verified byte-identical to Runic. Idempotent. Fixture
`curly_type_params/`. Corpus 19→20 pass, divergence held at 2; allowlist 19→20.
No upstream blocker surfaced.

## Earlier session (tight field-access `.`)

Fixed a **latent mangling bug** (not a missing rule — a *wrong* one), found by
probing transparent constructs after the corpus went fully triaged. Field access
`a.b.c` parses as a nested `BINARY_EXPR` with a `DOT` operator, so `lower_binary`
treated `.` as a normal spaced binop and emitted `a . b . c` — which is **invalid
Julia** (`a . b` is a JuliaSyntax/Runic *parse error*: "whitespace is not allowed
here"). Same family as the old range-colon latent bug. One-line fix: add
`SyntaxKind::DOT` to `is_tight_binop` (alongside `CARET`/`COLON`). The broadcast
operators (`.+`/`.^` = `DOT_CARET` etc.) are distinct tokens, so they stay spaced
(`a.b .+ c` → `a.b .+ c`, verified). Verified byte-identical to Runic on
`a.b.c`/`obj.field = 1`/`Base.Iterators.flatten`/`a.b().c`/`df.x[1]`/`a.b .+ c`,
and `a . b . c` (spaced input) normalizes to `a.b.c`. Idempotent. Fixture
`dot_access/`. Corpus 18→19 pass, divergence held at 2; allowlist 18→19.

**Upstream blocker surfaced & handed off:** left-division `\` (`a\b`) mis-lexes
to an `ERROR` token (the formatter can only bail to transparent); JuliaSyntax:
`(call-i a \ b)`, Runic spaces it. Queued at the top of `parser-parity/RECAP.md`
+ a `TODO.md` Parser bullet (5-file operator recipe, tier of `/`).

## Earlier session (ternary spacing)

Closed ranked target #0 (cheap, pre-probed). `TERNARY_EXPR` (`a ? b : c`) was
**transparent**, so Fatou leaked the input spacing (`a ?  b  :  c`) while Runic
normalizes to one space around both `?` and `:`. New `lower_ternary` (arm on
`TERNARY_EXPR`), modeled on `lower_comparison`: walk children dropping incidental
whitespace, alternate operand/operator, push one space then the operator text for a
`QUESTION`/`COLON` token (any other token bails), and **recurse into operands** so a
nested right-associative ternary (`a ? b : c ? d : e`, the rhs is itself a
`TERNARY_EXPR`) and normalized operands (`a ? b+1 : c*2` → `a ? b + 1 : c * 2`) keep
formatting. Bails to `lower_transparent` on a comment/newline (a multi-line ternary —
which Runic *preserves* anyway, so the bail is byte-identical) or operand count ≠ 3.
Verified byte-identical to Runic on `q/r/s/t/u/v` (literal/call/index/binop operands,
nested). Idempotent (the spaced form re-parses to the same shape). Fixture
`ternary_spacing/`. Corpus 17→18 pass, divergence held at 2; allowlist 17→18.

## Earlier session (anonymous-function arrow spacing)

Closed a clean operator-spacing gap outside the ranked list (cheaper than the
ranked #1 comment work). `ARROW_EXPR` (`x->y`, `(a,b)->a+b`) was **transparent**, so
Fatou leaked `x->y` while Runic always spaces the arrow (`x -> y`). New `lower_arrow`
(arm on `ARROW_EXPR`): collect operand nodes, require a single `ARROW` token, emit
`lhs -> rhs` with one space each side, **recursing into both operands** so a nested
arrow (`x -> y -> z`, right-assoc), a normalized lhs tuple (`(x,y)` → `(x, y)`), or a
body inside an arg list (`map(x->x^2, a)` → `map(x -> x^2, a)`) all keep formatting.
The catch-all `_ => lower_transparent` bails on a comment/newline (a multi-line body
like `x->\n y`, which Runic reindents — a separate construct) or a second arrow.
Verified byte-identical to Runic on `x->y`/`()->y`/chained/`map`/`f = x -> x+1`.
Idempotent (the spaced form re-parses to the same shape). Fixture
`arrow_functions/`. Corpus 16→17 pass, divergence held at 2; allowlist 16→17.

## Earlier session (leading/trailing-gap blank lines)

Closed ranked target #2. Runic preserves a blank line right after the open bracket
(**leading** gap) and right before the close (**trailing** gap), in both broken
brackets *and* matrices, capped at 2 (same `MAX_BLANK_LINES`). Previously Fatou
**bailed** (brackets) or **silently dropped** (matrices, an ungated divergence) —
both now land. Verified byte-identical to Runic across call/vect/tuple/braces/index
leading + trailing, matrix leading/trailing/both, and the 3+→2 cap.

**The framing-vs-blank accounting** (the whole trick): one source newline in a gap
is the *framing break* the layout always adds; every newline beyond the first is a
preserved blank. So `blanks = newlines.saturating_sub(1).min(MAX)`.

- **Bracket** (`lower_multiline_bracket`): the leading-gap `newlines >= 2` bail
  became `leading_blanks = newlines.saturating_sub(1).min(MAX)`, pushed as
  `BlankLine`s *before* the framing `HardLine`. The trailing-gap bail likewise
  became `trailing_blanks`, pushed into `inner` *after* the item loop (before the
  closing framing `HardLine`). `leading_comma`/doubled-comma still bail.
- **Matrix** (`lower_matrix`): `first`/`last` (positions of the first/last
  non-empty line) already bound the content span. The empty lines outside it are
  the framing `[`/`]` lines plus blanks: `leading_blanks = first.saturating_sub(1)`,
  `trailing_blanks = (len-1-last).saturating_sub(1)`, both `.min(MAX)`. Emitted as
  `BlankLine`s around the content loop. No `first==0` edge issue —
  `saturating_sub(1)` gives 0 when the first source line already carries content.

Idempotent (re-parse: a leading blank is 2 newlines after `[` → `saturating_sub(1)`
= 1 → fixed point; same trailing). Fixtures `bracket_gap_blank_lines/`,
`matrix_gap_blank_lines/`. The matrix leading/trailing **ungated divergence** noted
in the prior recap is now **closed**.

## Earlier session (interior blank-line preservation)

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

### Ranked next targets

0. **Bare-tuple comma spacing** (cheap, surfaced this session). `x,y` → `x, y`
   (`BARE_TUPLE_EXPR`, currently transparent). Runic spaces the comma like every
   other list. A `lower_bare_tuple` modeled on `lower_collection` (no brackets,
   `", "`-join the `ARG`/name children, recurse). Would also bring `return x, y`
   and `global a, b` to parity for free. Probe trailing-comma + `;` shapes first.
1. **Comment preservation inside broken brackets *and matrices***—now the top
   blank-line work is fully done (interior + leading/trailing gaps), this is the
   last piece of the old "blank lines + comments" target #1. Comments are the hard
   part: placement (own-line vs trailing `# …`), the trailing-`#`-forces-the-next-
   token-onto-a-newline interaction, and the matrix-row case. Both
   `lower_multiline_bracket` and `lower_matrix` still bail on any `COMMENT`.
2. **Blocks/control flow indentation**—bigger; needs `HardLine`/`Indent` and
   careful idempotence. Return-insertion stays out (semantic, blocked).
3. **Long single-line bracket/matrix reflow** (width-based breaking)—Fatou's
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
- **tight range `:` and `::` type annotations**: Runic packs both tight. `COLON`
  added to `is_tight_binop` (two-operand `a:b` is a `BINARY_EXPR`; fixed a latent
  `1:2`→`1 : 2` mangle); stepped `1:2:10` is a `RANGE_EXPR` (`lower_range`, all
  tight). `::` is `TYPE_ANNOTATION` (`lower_type_annotation`, tight, bare `::Int`
  ok). Fixtures `range_colon/`, `type_annotations/`. Divergence (out of fixtures):
  Runic parenthesizes compound range operands (`a + 1 : b`→`(a + 1):b`), a semantic
  rewrite; Fatou tightens + recurses unparenthesized (simple operands only).
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
