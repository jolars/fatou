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

Dir corpus (**47 fixtures**): **43 allowlisted**, 4 blocked
(`logical_tight_divergence` = `&&`/`||` whitespace Tenet-1 divergence;
`control_flow` = Runic return-insertion, a semantic rewrite, deferred;
`trailing_comment_spacing_divergence` = pre-`#` whitespace Tenet-1 divergence in
a block body; `bracket_comment_spacing_divergence` = the same divergence inside a
broken bracket).
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
comprehension/generator `for`-binding `in` normalization (`lower_for_binding`),
`begin`/`quote` block-body indentation (`lower_block_expr` + `lower_block_body`),
`let` block-body indentation (`lower_let`, first reuse of `lower_block_body`),
`while`/`for` loop-body indentation (`lower_loop`, second reuse of
`lower_block_body`), `if`/`elseif`/`else` + `try`/`catch`/`else`/`finally` branch
structure (`lower_if`/`lower_try` + shared `lower_branch_clause`, third reuse of
`lower_block_body`), own-line line-comment preservation in block bodies
(`lower_block_body` extended), trailing line-comment preservation in block bodies
(`lower_block_body` line model gained a per-line `comment` field), comment
preservation inside broken brackets and matrices (`lower_multiline_bracket`
gained a `GapLine`/`item_comments`/`header_comment` model; `lower_matrix` keeps
line comments verbatim).

## Latest session (comments inside broken brackets + matrices)

Lifted the `COMMENT` bail in both `lower_multiline_bracket` and `lower_matrix`.

**Matrix** (tiny): one new `COMMENT` token arm pushing the trimmed text as a
non-whitespace line element. Because the matrix interior is preserved verbatim,
the pre-`#` spacing matches Runic byte-for-byte (`1 2  # row` keeps two spaces) —
**no divergence**; an own-line comment becomes its own content line, indented by
the framing. `BLOCK_COMMENT` still bails via the `_` arm.

**Bracket** (rewrite): the old `items`/`seps` model gained a comment model.
`Sep::Newline { blanks }` became `Sep::Break(Vec<GapLine>)` where
`GapLine = Blank | Comment(String)` records, in order, the own-line comments and
blank lines in the gap between two items. New `item_comments: Vec<Option<String>>`
(aligned with `items`) holds a **trailing** comment riding on an item (`1, # c`),
and `header_comment: Option<String>` a comment on the open-bracket line
(`[ # c`). Classification by the newline counter: a `COMMENT` at `newlines == 0`
is trailing (on the last item, or the header if no item yet — bail on a second);
at `newlines >= 1` it is own-line and joins the current gap (after `flush_blanks`
turns `n` newlines into `n-1` capped blanks). The leading gap and trailing gap
are the gap state captured at the first item and after the loop. Rendering: a
`render_gap` helper emits each `GapLine` (`Blank` → `BlankLine`, `Comment` →
`HardLine` + text); trailing/header comments ride after their comma/bracket with
**one** canonical pre-`#` space. That one-space canonicalization is the **same
Tenet-1 divergence** as block bodies (Runic preserves ≥1 verbatim) — recorded as
the new blocked fixture `bracket_comment_spacing_divergence/`. Same-line items
(`a, b`) still collapse to `Sep::Space` (captured before `flush_blanks`).
Verified byte-identical to Runic across trailing/own-line/leading/trailer/header
comments, multi-item-line trailing comments, no-comma-last items, blank+comment
mixes (incl. 3-blank capping), and indexes; idempotent. Fixtures
`bracket_comments/`, `matrix_comments/`. Corpus 41→43 pass, divergence 3→4 (the
deliberate new bracket block); allowlist 41→43. No parser blocker (comments
tokenize as `COMMENT`).

### Ranked next targets

1. **`function`/`do`/`macro` bodies** reuse `lower_block_body` for layout but Runic
   **return-inserts** them (semantic rewrite, blocked as `control_flow`). Layout
   could still land *if* return-insertion is modeled or the fixture dodges it;
   currently deferred.
2. **Long single-line bracket/matrix reflow** (width-based breaking) — Fatou's
   breaking is purely source-driven (newline-triggered); Runic also breaks on
   width. Needs the `fits` engine, not just `HardLine`s.
3. **`BLOCK_COMMENT` (`#= … =#`) preservation** — every comment rule so far bails
   on `BLOCK_COMMENT` (block bodies, brackets, matrices). A multi-line block
   comment interacts with indentation; probe Runic first.

## Earlier sessions

- **trailing comments in block bodies (`lower_block_body` line model)**: line
  model `Vec<Vec<Ir>>` → `Vec<BodyLine>` with `BodyLine { stmts, comment }`; a
  `COMMENT` sets `line.comment` (own-line comments still flush-left, trailing
  ride after the `; `-joined stmts with one canonical pre-`#` space — the
  Tenet-1 divergence blocked as `trailing_comment_spacing_divergence`). Landed
  across every vertical block. Fixture `trailing_comments/`. Corpus 40→41.
- **own-line line comments in block bodies (`lower_block_body` extended)**: the
  first comment-preservation rule, a one-arm extension landing across every
  vertical block. A `COMMENT` on an otherwise-empty line became its own statement
  line (text `trim_end_matches`'d), re-indented to the body via the existing
  `HardLine`. A *trailing* comment still bailed the whole block to transparent
  (lifted this session). `BLOCK_COMMENT` still bails. Fixture `block_comments/`.
- **`if`/`try` branch structure (`lower_if`/`lower_try` + `lower_branch_clause`,
  third reuse of `lower_block_body`)**: generalized the single-body engine to a
  **chain of branches**, each its own `BLOCK`. Two arms (`IF_EXPR`, `TRY_EXPR`)
  emit `<kw> [header] body <clauses> HardLine "end"`; every body delegates to
  `lower_block_body`, every non-leading clause to `lower_branch_clause` (keyword at
  column 0 via `HardLine`, an optional recursively-lowered header — `elseif`
  `CONDITION`, `catch` variable — then the indented body). Non-empty body always
  explodes vertical; an **empty** branch body anywhere bails the **whole** chain to
  transparent rather than partially reshape it. `&&`/`||` in conditions stay the
  Tenet-1 divergence, kept out of the fixtures. Fixtures `if_blocks/`,
  `try_blocks/`. Corpus 37→39.
- **`while`/`for` loop indentation (`lower_loop`, second reuse of `lower_block_body`)**:
  one arm on `WHILE_EXPR`/`FOR_EXPR` (`<kw> <header> BLOCK end`); the header is a
  recursively-lowered `CONDITION` (`while`) or `FOR_BINDING` (`for`, supplying the
  `for ` prefix the binding omits so `for i = 1:3` → `for i in 1:3`), the body
  delegated to `lower_block_body`. Non-empty one-line body explodes vertical
  (`while c; x; y; end` → `while c⏎    x; y⏎end`); empty body → transparent. Two
  `for` shapes kept out (multi-binding `for i=1:3, j=1:3` leaves 2nd+ bindings flat;
  one-line space-separated `for i in 1:3 x end` is a parser blocker, handed off).
  Fixture `loop_blocks/`.
- **`let` block indentation (`lower_let`, first reuse of `lower_block_body`)**:
  arm on `LET_EXPR` (`let [LET_BINDINGS] BLOCK end`)—validate the `let`/`end`
  framing + single `BLOCK`, lower the optional bindings, delegate the body to
  `lower_block_body`, emit `"let"` + (`" "` + bindings)? + body + `HardLine` +
  `"end"`. Non-empty body always explodes vertical (the binding/body `;` opens the
  `BLOCK`); empty body → `None` → transparent. The `LET_BINDINGS` header is lowered
  recursively but not reshaped—the parser leaves 2nd+ bindings as flat tokens, so
  tight multi-binding `let x=1,y=2` is kept out of the fixture. No return-insertion
  (let isn't a function body). Fixture `let_blocks/`.
- **`begin`/`quote` block indentation (`lower_block_expr` + `lower_block_body`)**:
  the first **vertical-block** rule. `lower_block_expr` (arm on `BEGIN_EXPR`/
  `QUOTE_EXPR`, shape `<kw> BLOCK <end>`) emits `kw + lower_block_body + HardLine +
  "end"`; Runic *always* explodes a non-empty block vertical even on one line
  (`begin x end` → `begin⏎    x⏎end`), an empty block (`begin end`) keeps its
  layout via transparent. The reusable **`lower_block_body`** is the body engine:
  it groups the `BLOCK` into **lines** (`NEWLINE` starts a line; `;` keeps the next
  statement on the current line via a `"; "`-join), mirrors the matrix
  `lines: Vec<Vec<Ir>>` + blank-line accounting (capped at `MAX_BLANK_LINES`), and
  returns an `Ir::indent(...)` body or `None` (→ transparent) for an empty block or
  any unmodeled shape (body comment, two statements with no separator). Statements
  are `lower_node`-recursed (inner spacing normalizes, nested blocks indent
  further). No return-insertion risk. Fixture `begin_quote_blocks/`.
- **`for`-binding `in` normalization (`lower_for_binding`)**: a `FOR_BINDING` arm
  normalizing the iteration operator to keyword `in` across three CST shapes
  (`=` → wrapped `ASSIGNMENT_EXPR`, `∈` → wrapped `BINARY_EXPR`, already-`in` →
  flat triple). Partitions post-keyword elements on `COMMA` into binding groups
  plus an optional `if` filter tail; the `for` keyword is a child in a
  comprehension/generator but the parent in a `for` loop, so `"for "` is emitted
  iff present (normalizing a loop binding line too, body left transparent). Targets
  and iterables recursed; bails on comment/newline or any unmodeled shape. Fixture
  `comprehension_for_in/`.
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
