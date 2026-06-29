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

Dir corpus (**36 fixtures**): **34 allowlisted**, 2 blocked
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
(`lower_paren`), `;`-block padding and separators (`lower_paren_block`),
comprehension/generator `for`-binding `in` normalization (`lower_for_binding`).

## Latest session (`for`-binding `in` normalization — `lower_for_binding`)

A new `lower_node` arm on `FOR_BINDING`, the iteration clause of a comprehension
or generator (`[x for i = 1:3]`, `(x for i ∈ s)`) and of a `for` loop. It was
**transparent**, so the `=` form spaced through `lower_binary` (`i = 1:3`) and the
`∈` form spaced through `lower_binary` (`i ∈ 1:3`), against Runic's canonical
keyword `in` (`for i in 1:3`). This is a **token-level canonicalization**, the
same family as the float/hex literal rules, not pure layout. **Three CST shapes**
the binding takes (probed via `parse`): `=` → a wrapped `ASSIGNMENT_EXPR(NAME EQ
rhs)`; `∈` → a wrapped `BINARY_EXPR(NAME UNICODE_OP("∈") rhs)`; already-`in` → a
**flat** triple `NAME`, `IDENT("in")`, `rhs` (no wrapping node). The arm collects
the post-keyword elements (whitespace dropped), partitions them on `COMMA` into
binding groups plus an optional trailing `if <filter>` tail, then `lower_for_spec`
maps each group: a lone wrapped node is split by `for_iteration_operands` (accepts
only `EQ`/`∈`, operand count 2) and a flat triple is matched directly; either way
it emits `lower_node(target) + " in " + lower_node(iterable)`. Groups `", "`-join;
a filter emits `" if " + lower_node`. **Keyword placement is the subtlety:** the
`FOR_KW` is a *child of `FOR_BINDING`* in a comprehension/generator but a child of
the parent `FOR_EXPR` in a `for` loop—so `"for "` is emitted **iff** the keyword
is present, letting the one arm normalize a loop binding too
(`for i = 1:3 … end` → `for i in 1:3 … end`, body left transparent—control flow is
deferred, but the binding line still canonicalizes, matching Runic). Targets and
iterables are recursed (`[i*j for i=1:2 for j=1:2]` → `[i * j for i in 1:2 for
j in 1:2]`; the multi-`for` form is sibling `FOR_BINDING`s, each handled). Bails on
comment/newline, a filter that isn't a single expression node, or any unmodeled
binding shape. Verified byte-identical to Runic on the `=`/`∈`/`in` forms,
multi-binding (`i = 1:3, j = 1:3` and `i in a, j in b`), generator `()`, `if`
filter, nested `for`, and a `Dict(… for (v,i) = pairs)` generator. Idempotent
(output `in` reparses to the flat form → fixed point). Fixture
`comprehension_for_in/`. Corpus 33→34 pass, divergence held at 2; allowlist 33→34.
No parser work needed.

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

- **`;`-block padding and separators (`lower_paren_block`)**: `PAREN_BLOCK`
  (`(a; b)`, a `begin`-less block, distinct from `PAREN_EXPR`/`TUPLE_EXPR`) was
  transparent. New arm walks `LPAREN`, a leading statement, then one `PARAMETERS`
  per `; <stmt>` (routed through `paren_block_statement`: lone statement, or `None`
  for an arg-less trailing `;`, or `Err` on an unmodeled shape), `RPAREN`;
  statements `"; "`-joined (tight-left/space-right) and recursed
  (`(a=1;b=2)` → `(a = 1; b = 2)`, `((a;b);c)` → `((a; b); c)`). Only the
  ≥2-statement form is reshaped—a single-statement `(a;)` keeps its `;` via the
  transparent fallback (Runic preserves it). Bails on comment/newline. Fixture
  `paren_blocks/`. Divergence kept out: a padded single-statement `( a ; )` →
  Runic `(a;)` but Fatou only part-trims (`( a ;)`); rare, no fixture, unrecorded.
- **parenthesized-expression padding (`lower_paren`)**: `PAREN_EXPR` (`( a + b )`)
  was transparent, leaking the whitespace flanking the single inner expression.
  New arm (modeled on `lower_arrow`): drop `WHITESPACE`, accept one
  `LPAREN`/`RPAREN`, require one inner operand, emit `"(" + lower_node(inner) +
  ")"`; recursion normalizes nested parens (`( (a) )` → `((a))`) and inner spacing.
  Bails on comment/newline/error/extra operand. The `;`-block `(a; b)` is a
  separate `PAREN_BLOCK` (handled this session); tuples are `TUPLE_EXPR`. Fixture
  `paren_padding/`.
- **`using`/`import` comma + selector lists (`lower_import_stmt`)**:
  `USING_STMT`/`IMPORT_STMT` were transparent, leaking comma spacing; Runic
  `", "`-joins them and packs the selector `:` tight-left/space-right
  (`using A: x, y`). Item(`IMPORT_PATH`/`IMPORT_ALIAS` node)/separator alternation,
  paths recursed transparently (`A.B`, `.A`, `Foo as Bar`); bails on
  comment/newline or a leading/trailing/doubled separator. Fixture
  `import_using_lists/`. (Surfaced the assignment-list `global`/`local` parser
  blocker, since landed upstream `93e3d28`.) Divergence kept out: a *leading*-space
  selector colon (`using A :x`) is Runic-non-deterministic; Fatou canonicalizes to
  `using A: x`, no fixture exercises it.
- **tight field-access `.` (`DOT` in `is_tight_binop`)**: a *wrong-rule* latent
  mangle—`a.b.c` is a nested `BINARY_EXPR` with a `DOT` op, so `lower_binary`
  spaced it to the invalid `a . b . c`. One-line fix: add `SyntaxKind::DOT` to
  `is_tight_binop`. Broadcast ops (`.+` = `DOT_CARET` etc.) are distinct tokens,
  stay spaced. Fixture `dot_access/`. (Surfaced the left-division `\` mis-lex
  blocker, handed to parser-parity.)
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
