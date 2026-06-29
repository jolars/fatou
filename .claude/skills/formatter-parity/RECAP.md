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

Dir corpus (**28 fixtures**): **26 allowlisted**, 2 blocked
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
parser-unblocked), `where`-clause brace normalization (`lower_where`).

## Latest session (`where`-clause brace normalization — `lower_where`)

`WHERE_EXPR` (`f(x) where T`) was **transparent**, so Fatou left the bound bare
while Runic **always brace-wraps** it: `f(x) where T` → `f(x) where {T}`. New
`lower_where` (arm on `WHERE_EXPR`), modeled on `lower_arrow`: collect the two
operand nodes (lhs, bound) and require a single `WHERE_KW`; emit
`lower_node(lhs)` + `" where "` + the brace-wrapped bound. The bound rule is the
crux: if it's **already a `BRACES` node**, lower it in place (so
`lower_collection` normalizes `where { T , S }` → `where {T, S}`); **any other**
bound (bare `NAME`, a `<:`/`>:` `BINARY_EXPR`, a `PAREN_EXPR`, a `CURLY_EXPR`) is
wrapped `{` + `lower_node(bound)` + `}` and recursed, so `where T<:Real` →
`where {T <: Real}`, `where (T)` → `where {(T)}`, `where Tuple{T}` →
`where {Tuple{T}}`. Nested `where` (`f(x) where T where S` is a left-nested
`WHERE_EXPR`) falls out of recursing the lhs → `f(x) where {T} where {S}`. Bails
to `lower_transparent` on a comment/newline (a multi-line clause Runic may
reflow), error recovery, or operand count ≠ 2. Verified byte-identical to Runic
on `f(x) where T`, `where T<:Real`, `where {T}`, `where { T }`, `where {T,S}`,
nested, tight `f(x)where T`, `Tuple{T} where T`, `Array{T,N} where {T,N}`,
`where (T)`, `where Tuple{T}`, `where T>:Int`, `g(x)::T where T`. Idempotent
(`where {T}` re-parses to a `BRACES` bound → fixed point). Fixture
`where_clauses/`. Corpus 25→26 pass, divergence held at 2; allowlist 25→26. No
parser work needed — `WHERE_EXPR` already nested cleanly.

## Earlier session (`global`/`local` multiple assignment — rule-free PASS)

The parser blocker that target #0 was parked on **landed upstream** (`93e3d28`,
`feat(parser): nest global/local multiple assignment`). `global`/`local` now parse
their body at statement level (`KwStmt::ExprTuple`, like `const`), so
`global a, b = 1, 2` nests as `GLOBAL_STMT → GLOBAL_KW + ASSIGNMENT_EXPR(BARE_TUPLE
= BARE_TUPLE)` instead of the old flat token soup. That single nested operand node
flows straight through `lower_keyword_stmt`'s **single-operand-node arm** (`[Node]`)
→ `lower_node` → `lower_binary` (assignment) → bare-tuple recursion on each side —
**no new rule needed**. Same for `global a, b::Int` (→ single `BARE_TUPLE_EXPR(NAME,
COMMA, TYPE_ANNOTATION)` child → `lower_bare_tuple` + `lower_type_annotation`) and
the bare list `global a, b` (now also a `BARE_TUPLE_EXPR` node, not the old flat
`NAME/IDENT/COMMA`). Verified byte-identical to Runic on `global a,b = 1,2`,
`local a,b = f(x),g(y)`, `global  a ,b = 1 , 2` (messy input → `global a, b = 1,
2`), `local x,y,z = 1,2,3`, `global a,b::Int`, `global a, b, c::Float64`,
`global x = 1`, `local c`, `const p, q = 1, 2`. Idempotent. Fixture
`global_local_assignment/` is a regression lock (mirrors the parser fixture name).
Corpus 24→25 pass, divergence held at 2; allowlist 24→25. No code change to
`rules.rs` — the parser fix is what unblocked it. (The old flat-token comma-name
branch in `lower_keyword_stmt` is now effectively unreachable for `global`/`local`
but stays as a lossless fallback; not removed.)

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
