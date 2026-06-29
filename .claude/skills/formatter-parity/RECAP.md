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

Dir corpus (**34 fixtures**): **32 allowlisted**, 2 blocked
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
extended to brace `ARG_LIST`s), keyword-statement spacing (`lower_keyword_stmt`),
bare-tuple comma spacing (`lower_bare_tuple`), `global`/`local` comma name lists
(`lower_keyword_stmt` extended), `using`/`import` comma + selector lists
(`lower_import_stmt`), `global`/`local` multiple assignment (rule-free PASS,
parser-unblocked), `where`-clause brace normalization (`lower_where`),
float-literal normalization (`lower_literal` + `normalize_float`), hex-integer
zero-padding (`lower_literal` extended to `HEX_INT` + `normalize_hex`),
`export`/`public` name lists (`lower_export_stmt`), trailing-whitespace trimming
(`lower_trivia` in the transparent path), named-tuple element spacing
(`lower_collection` extended to `KEYWORD_ARG`), parenthesized-expression padding
(`lower_paren`).

## Latest session (parenthesized-expression padding — `lower_paren`)

A new `lower_node` arm on `PAREN_EXPR`. Found by sweeping single-line forms after
the corpus was fully triaged: `( a + b )` was **transparent**, so Fatou kept the
incidental whitespace flanking the inner expression—`( a + b )` stayed
`( a + b )`, diverging from Runic's `(a + b)`. (The inner binary still got spaced
via the transparent recursion; only the padding tokens leaked.) A `PAREN_EXPR` is
`LPAREN`, optional `WHITESPACE`, **exactly one** inner node, optional `WHITESPACE`,
`RPAREN`. The new arm (modeled on `lower_arrow`) collects children, drops
`WHITESPACE`, accepts one `LPAREN`/`RPAREN` each, and requires a single inner
operand; it then emits `"(" + lower_node(inner) + ")"`. Recursing the inner node
keeps everything normalizing: nested parens `( (a) )` → `((a))`, and the inner
expression's own spacing (`((a + b) * c)`). The catch-all `_` arm bails to
`lower_transparent` on a comment, newline (a multi-line paren Runic *reflows and
reindents*—out of scope, target #1), error recovery, or a missing/extra operand.
Two sibling shapes never reach here: the `;`-block `(a; b)` parses to a distinct
`PAREN_BLOCK` (still leaks `(a ; b)`—a later target) and a tuple `(a, b)`/`(a,)`
is a `TUPLE_EXPR` (already handled by `lower_collection`). Verified byte-identical
to Runic on `( a + b )`, `(  x  )`, `( x )`, `( (a) )`, `( a )* ( b )` →
`(a) * (b)`, `(f(a) )`, `( -x )` → `(-x)`, `return ( x )`, and the
already-canonical `(a)`/`(a + b)`/`((a + b) * c)`. Idempotent (the unpadded form
is a fixed point). Fixture `paren_padding/` (single-line; multi-line and
commented parens kept out, left for the comment-preservation target). Corpus
31→32 pass, divergence held at 2; allowlist 31→32. No parser work needed.

## Earlier session (`using`/`import` comma + selector lists)

`USING_STMT`/`IMPORT_STMT` were **transparent**, leaking comma spacing
(`using A,B` stayed `using A,B`); Runic `", "`-joins them. Probing the next ranked
target (assignment-list `global`/`local`) showed it's an **upstream parser
blocker** (see below), so I pivoted to this clean adjacent win surfaced while
probing. These parse *cleanly*: keyword token, then a comma-separated list of
`IMPORT_PATH`/`IMPORT_ALIAS` **nodes**, optionally `:`-led into a selector list
(`using A: x, y`). New `lower_import_stmt`: keyword + space, then a strict
item(node)/separator alternation—`COMMA` → `", "`, the selector `COLON` → `": "`
(Runic packs the selector colon tight-left/space-right); items are lowered via
`lower_node` so the paths (`A.B`, `.A`, `..B.C`, `Foo as Bar`) pass through
transparently (their internal dots/`as` are verbatim). Bails to transparent on a
comment/newline (a multi-line import Runic may reflow) or a
leading/trailing/doubled separator. Verified byte-identical to Runic on
`using A,B`, `import A.B, C.D`, `using A: x,y`, `using A:x,y,z`,
`import Base: +, -`, `import A: x as y`, `using .A, ..B.C`, single
`using LinearAlgebra`. Idempotent. Fixture `import_using_lists/`. Corpus 23→24
pass, divergence held at 2; allowlist 23→24.

**Divergence kept out of the fixture (Tenet-1 corner):** with a *leading* space on
the selector colon, Runic is non-deterministic—`using A :x` → `using A:x` and
`using A : x` → `using A:x` (it drops the space-after, treating `:x` symbol-like),
whereas `using A:x` → `using A: x`. Fatou canonicalizes to `using A: x` regardless;
rare hand-spacing, left unrecorded as a blocked slug since no fixture exercises it.

**Upstream parser blocker surfaced & handed off:** assignment-list
`global`/`local` (`global a, b = 1, 2`, `local a, b = f(x), g(y)`,
`global a, b::Int`) parses to a **flat token soup**—`GLOBAL_STMT` holds loose
`NAME COMMA IDENT EQ INTEGER COMMA INTEGER` (no `ASSIGNMENT_EXPR`/
`BARE_TUPLE_EXPR`; even calls unwrapped). JuliaSyntax green tree:
`global ((tuple a b) = (tuple 1 2))`. A formatter rule here would be a fragile
hand-normalizer papering over the parser; once the parser nests it properly the
existing keyword-stmt + `lower_binary` + bare-tuple recursion handles it for free.
Queued at the top of `parser-parity/RECAP.md` + a `TODO.md` Parser bullet.

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

(Ternary spacing `lower_ternary` and anonymous-function arrow spacing
`lower_arrow` — both operand/operator alternation rules modeled on
`lower_comparison`, recursing into operands, bailing on comment/newline — are now
in the "Earlier sessions" bullet list below.)

### Ranked next targets

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

- **named-tuple element spacing (`lower_collection` + `KEYWORD_ARG`)**: a one-line
  item-kind extension, not a new arm. A named tuple `(a=1, b=2)` is a `TUPLE_EXPR`
  whose elements are `KEYWORD_ARG` nodes; `lower_collection`'s item match only
  accepted `ARG`, so the named tuple fell to transparent—`=` got spaced via
  recursion but the inter-element comma leaked (`(a=1,b=2)` → `(a = 1,b = 2)`).
  Item arm now matches `ARG | KEYWORD_ARG`; singleton `(x=1,)` + trailing-comma
  drop unchanged. Fixture `named_tuples/` (single-line; multi-line still bails).
- **`where`-clause brace normalization (`lower_where`)**: `WHERE_EXPR`
  (`f(x) where T`) was transparent; Runic **always brace-wraps** the bound
  (`f(x) where {T}`). New arm modeled on `lower_arrow`: emit `lower_node(lhs)` +
  `" where "` + brace-wrapped bound. Crux is the bound: an existing `BRACES` node
  is lowered in place (so `where { T , S }` → `where {T, S}`), any other bound
  (bare `NAME`, `<:`/`>:`, paren, curly) is `{`+`lower_node(bound)`+`}`-wrapped and
  recursed (`where T<:Real` → `where {T <: Real}`). Nested `where` falls out of
  recursing the lhs. Bails on comment/newline, error recovery, or operand count
  ≠ 2. Idempotent (`where {T}` re-parses to a `BRACES` bound). Fixture
  `where_clauses/`.
- **trailing-whitespace trimming (`lower_trivia`)**: the first cross-cutting
  (non-construct) rule—lives in `lower_transparent`, not a `lower_node` arm.
  Mirrors Runic's `trim_trailing_whitespace`: transparent **tokens** route through
  `lower_trivia(tok, next)` (one `peekable()` lookahead); a `WHITESPACE` token
  whose next sibling is a `NEWLINE` is dropped (Fatou lexes horizontal `WHITESPACE`
  and `NEWLINE` separately, so it's never mid-content), and a line `COMMENT`'s text
  is `trim_end_matches([' ', '\t'])`'d. `STRING_CONTENT` and `BLOCK_COMMENT` stay
  verbatim (Runic keeps trailing blanks inside both). Handled constructs never emit
  trailing whitespace, so the transparent path is the only leak surface. Fixture
  `trailing_whitespace/` (toplevel only, to keep `return`-insertion out).
- **`export`/`public` name lists (`lower_export_stmt`)**: `EXPORT_STMT`/
  `PUBLIC_STMT` were transparent, leaking comma spacing; Runic
  `spaces_in_export_public` `", "`-joins them (`export a,b` → `export a, b`). An
  exported name isn't always one node (`IDENT`, operator `export +, -`, macro
  `export @m`, `var"…"`), so the rule tracks **comma boundaries**—keyword, then
  `expect_item` after the keyword and each comma takes a leading space on the
  name's first token; while mid-name a `COMMA` re-arms and any other token is
  glued verbatim. Bails on comment/newline or a leading/trailing/doubled comma.
  Fixture `export_public_lists/`.
- **ternary spacing (`lower_ternary`)**: `TERNARY_EXPR` (`a ? b : c`) was
  transparent; one space around `?` and `:`. Walk children dropping incidental
  whitespace, alternate operand/operator (space + `QUESTION`/`COLON` text),
  recurse into operands (nested right-assoc ternary + operand normalization keep
  formatting); bails on comment/newline or operand count ≠ 3. Fixture
  `ternary_spacing/`.
- **anonymous-function arrow spacing (`lower_arrow`)**: `ARROW_EXPR` (`x->y`) was
  transparent; Runic always spaces the arrow (`x -> y`). Collect operands, require
  one `ARROW`, emit `lhs -> rhs`, recurse both operands (nested arrows, lhs tuple
  normalization, arrow inside an arg list keep formatting); bails on
  comment/newline or a second arrow. Fixture `arrow_functions/`.
- **hex-integer zero-padding (`lower_literal` + `normalize_hex`)**: second
  **token-text** rule (sibling to the float one). New `lower_literal` arm on
  `HEX_INT` runs the text through `normalize_hex`, mirroring Runic's
  `format_hex_literals`: pad the literal's **byte span** (`0x` included) to the
  next canonical span `0x`+2/4/8/16/32 chars by inserting `0`s after `0x`
  (`0xF` → `0x0F`, `0x12345` → `0x00012345`). Byte span (not digit count) is
  measured, so underscores count (`0x1_2` → `0x01_2`); digit case preserved
  (`0xDEADBEEF` untouched); BigInt (span ≥ 34) and already-canonical spans →
  `None`/verbatim. Hex floats are `FLOAT` (handled by `normalize_float`'s `0x`
  bail); octal/binary untouched. Output always lands on a canonical span →
  idempotent. Fixture `hex_literals/`.
- **float-literal normalization (`lower_literal` + `normalize_float`)**: first
  **token-text** rule. `lower_literal` arm on `LITERAL` passes tokens verbatim
  except `FLOAT`/`FLOAT32`, run through `normalize_float` (reimplements Runic's
  `format_float_literals`): `.5` → `0.5`, `1.` → `1.0`, `1E10` → `1.0e10`,
  `1f0` → `1.0f0`, `1.50` → `1.5`. Hand-parses `[sign][int][.frac][marker[sign]exp]`
  and rebuilds canonically (int leading zeros stripped, point always present with
  ≥1 frac digit, frac trailing zeros stripped, marker lowercased, exp leading
  zeros stripped, Unicode minus → `-`); `None`/verbatim on underscored/hex floats
  or any unparsed char. Idempotent (fixed point on canonical forms). Fixture
  `float_literals/`.
- **`global`/`local` multiple assignment (rule-free PASS)**: the parser blocker
  landed upstream (`93e3d28`), so `global a, b = 1, 2` now nests as a single
  `ASSIGNMENT_EXPR(BARE_TUPLE = BARE_TUPLE)` operand and flows through
  `lower_keyword_stmt`'s single-operand arm → `lower_binary` → bare-tuple recursion
  with **no new rule**. Same for `global a, b::Int` and bare `global a, b`. Fixture
  `global_local_assignment/` is a regression lock. Corpus 24→25, allowlist 24→25.
- **blank-line preservation (interior + leading/trailing gaps)**: new
  `Ir::BlankLine` primitive (bare `\n` at column 0, skips indent). Runic keeps
  blanks everywhere but **caps at 2** (`MAX_BLANK_LINES`). The accounting trick:
  one source newline in a gap is the framing break the layout always adds, so
  `blanks = newlines.saturating_sub(1).min(2)`. Applied to both
  `lower_multiline_bracket` (inter-item `Sep::Newline { blanks }`, plus leading-
  and trailing-gap blanks before/after the framing `HardLine`s) and `lower_matrix`
  (interior empty lines → `BlankLine`; `leading = first.saturating_sub(1)`,
  `trailing = (len-1-last).saturating_sub(1)`). Closed the old matrix
  leading/trailing ungated divergence. Fixtures `bracket_blank_lines/`,
  `matrix_blank_lines/`, `bracket_gap_blank_lines/`, `matrix_gap_blank_lines/`.
- **multi-line matrix breaking**: `lower_matrix` (new arm) reframes a
  `MATRIX_EXPR` spanning ≥2 lines like `lower_multiline_bracket` (`[` + `HardLine`,
  each source line re-indented, `HardLine` + `]`); interior kept verbatim (intra-row
  spacing, `;` placement). Multi-element row = `MATRIX_ROW` node, single-element
  column row = bare `ARG`, both lowered via `lower_node`. Bails on blank line,
  comment, missing/extra bracket. Fixture `multiline_matrices/`.
- **single-line matrices (regression lock, no rule)**: Runic *preserves* single-line
  matrices verbatim (no whitespace collapse, `;`-spacing kept); `MATRIX_EXPR` has no
  arm so the transparent fallback matches byte-for-byte. `matrices/` fixture pins the
  preservation so a future break rule can't start mangling them.
- **`global`/`local` comma name lists**: `lower_keyword_stmt` extended. The parser
  drops `NAME`/`IDENT`/`COMMA` flat into `GLOBAL_STMT`/`LOCAL_STMT` (asymmetric:
  first item a `NAME` node, rest bare `IDENT` tokens). Keeps the bare-keyword and
  single-operand-node arms (`return x`, `const a = 1, b = 2`); else `", "`-joins a
  clean item/`COMMA` alternation. Bails on the `=`/`::` assignment-list forms (a
  parser blocker, handed off), comments, stray commas. Fixture `global_local_names/`.
- **curly type-param padding**: added `LBRACE`/`RBRACE` to `lower_arg_list`'s
  bracket arm, so a `CURLY_EXPR`'s brace `ARG_LIST` gets the same normalization as
  call/index args (`Vector{ Int }` → `Vector{Int}`, `Dict{ A ,B }` → `Dict{A, B}`,
  trailing comma dropped, `; `-led `PARAMETERS` via `lower_parameters`). Fixture
  `curly_type_params/`.
- **bare-tuple comma spacing**: `lower_bare_tuple` (`BARE_TUPLE_EXPR`)—elements
  held **directly**, `COMMA`-separated, **not** `ARG`-wrapped; alternate
  element/comma, `", "`-join recursed elements (`f(x),g(y)` → `f(x), g(y)`,
  `x...,y` → `x..., y`). `a,b = 1,2`/`return x,y` flow through the existing
  `ASSIGNMENT`/`RETURN` recursion. Bails on leading/doubled/trailing comma or
  comment/newline. Fixture `bare_tuples/`.
- **keyword-statement spacing**: `lower_keyword_stmt`
  (`RETURN_EXPR`/`CONST_STMT`/`GLOBAL_STMT`/`LOCAL_STMT`)—keyword + one space +
  recursed operand (`return  x+1` → `return x + 1`), bare `return` kept. Later
  extended to `global`/`local` comma name lists (see latest session). Fixture
  `keyword_statements/`.
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
- **multi-line bracket breaking**: `lower_multiline_bracket` (shared by
  `lower_arg_list`/`lower_collection`)—a bracket goes vertical iff content spans ≥2
  source lines (`has_newline_token` on descendants, contagious; ignores `\n` inside
  strings). Source-driven (no `fits`): framing `HardLine` after open + before close,
  content `Ir::indent`ed one step; inter-item space-vs-break preserved from the
  source comma-gap newline count; trailing comma per `adds_trailing_comma` (calls
  preserve, index/tuple/vect/braces add). Bails on comment/`PARAMETERS`/bare `;`/
  doubled-leading comma/empty/unexpected. Fixture `multiline_brackets/`. Known
  divergence (out of scope): a bracket whose only newline is inside a triple-quoted
  string—Runic breaks + reindents the string; Fatou leaves it inline.
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
