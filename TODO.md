# TODOs

The groundwork pass establishes the full architecture (parser pipeline, salsa
layer, formatter/linter/LSP skeletons, CLI, tooling, tests) over a deliberately
small Julia subset. This file tracks what comes next, roughly ordered by
leverage.

## Parser / grammar

The grammar is a walking skeleton: literals, identifiers, operators (with Julia
precedence), prefix unary, calls, indexing, and the `function … end`,
`if/elseif/else … end`, and `begin … end` block forms. Losslessness holds for
*all* input regardless of grammar coverage (unparsed tokens are carried
through), so the grammar can grow incrementally.

- [x] More leading-keyword block forms: `for … end`, `while … end`, `let … end`,
  `try/catch/else/finally`, `struct`/`mutable struct`,
  `module`/`baremodule`, `quote … end`. Headers (`for i in xs`,
  `struct Foo <: Bar`) use a generic lossless passthrough for now —
  dedicated `in`/`∈`/`<:` operators and richer header trees come with the
  operators and parametric-type bullets below. **Known limitation:**
  `mutable` is lexed as a keyword, so it cannot currently be used as a bare
  identifier (it is contextual in Julia, special only before `struct`).
- [x] `do` blocks — postfix on a call (`f(x) do y … end`). Attached in the
  postfix chain (`parse_postfix_chain`) and parsed by `parse_do_block`, which
  reuses the generic header passthrough for the `do`-line parameters
  (`DO_PARAMS`) and the shared block/`end` helpers. Same-line only (`do` must sit
  on the call's line); terminal in the chain, so calling its result needs
  explicit parens.
- [x] `return`, `break`, `continue`, `const`, `global`, `local`, `import`,
  `using`, `export`. Leading-keyword statement forms (no `… end`), parsed by
  the shared `parse_keyword_stmt` in `structural.rs`: control flow is bare or
  takes an optional operand; `const`/`global`/`local` parse their first operand
  as an expression then carry the rest of the line through; `export` carries the
  whole clause through verbatim. `import`/`using` now build a real path tree (see
  the dedicated bullet below); `export`'s richer name list stays passthrough.
- [x] Anonymous functions and `->`; short-form function definitions
  (`f(x) = …`). The `->` operator (already lexed, Julia precedence `(4, 3)` —
  right-associative, tighter than `=`) builds a dedicated `ARROW_EXPR` in the
  Pratt loop (`expr.rs`). Short-form defs need no special node: `f(x) = …`
  parses as an `ASSIGNMENT_EXPR` over a `CALL_EXPR` left-hand side, matching the
  JuliaSyntax oracle (head `=`); a definition is distinguished from a plain
  assignment later in the semantic layer. **Known limitation:** multi-parameter
  anonymous functions `(x, y) -> …` await tuple-literal parsing (the array/tuple
  bullet below) — the parenthesized parameter list trips the "unclosed `(`" path
  for now; `x -> …`, `(x) -> …`, and `() -> …` work.
- [x] String interpolation (`"$x"`, `"$(expr)"`), raw/byte strings, command
  literals (`` `…` ``), non-standard string literals (`r"..."`, `b"..."`).
  Structured into `STRING_LITERAL`/`CMD_LITERAL` nodes with `INTERPOLATION`
  children whose `$(expr)` interiors are fully parsed sub-expressions; prefixes
  (`r`, `raw`, `b`, `v`) and suffix flags (`r"…"ims`) are represented as tokens.
  Known limitation: a `\"` immediately before a raw-string closing quote is not
  yet handled (the raw body is kept as one content chunk).
- [x] Macros (`@m`, `@m(...)`, `@m arg`), `@.`, and macro call argument forms.
  A leading `@` builds a `MACRO_CALL` wrapping a `MACRO_NAME` (`parse_macro` in
  `expr.rs`, dispatched from `parse_prefix`). The name body
  (`parse_macro_name_body`) is either the lone `.` of the broadcast macro `@.` or
  an identifier with a trailing adjacent `.ident` chain (qualified `@Mod.mac`).
  `parse_macro_args` handles both forms: a `(` adjacent to the name opens a
  comma-separated `ARG_LIST` (reusing `parse_arg_list`, so `ARG`/`KEYWORD_ARG`/
  `PARAMETERS`/splat come for free); otherwise the args are space-separated
  expressions consumed to end of line (or to a closing delimiter inside
  brackets). The `prefix.@mac` form (`Base.@time f()`) is caught in the Pratt
  loop: a `.` whose RHS begins with `@` is rerouted to `parse_qualified_macro`,
  which folds `Base.@time` into the `MACRO_NAME` and takes `f()` as an argument
  (matching the JuliaSyntax `(macrocall (. Base @time) …)` shape). **Known
  limitations:** whitespace-sensitive operator nuances in the space-arg form
  (Julia's `@m a +b` vs `@m a + b`) are not modeled — each space arg is a plain
  `parse_expr`; and string/cmd macros (`@m"…"`, `` @m`…` ``) are not yet a
  dedicated form.
- [x] Parametric types and braces (`Vector{T}`, `where`), type annotations
  (`x::T`), keyword arguments and `;` in call argument lists, splat
  (`x...`). Postfix `{…}` builds a `CURLY_EXPR` in the postfix chain (alongside
  call/index); standalone `{…}` (e.g. `where {T, S}`) builds a `BRACES` node via
  the prefix path. `::` is a dedicated `TYPE_ANNOTATION` (binary `x::T` and unary
  `::T` in method args like `f(::Int)`). `where` is a low-precedence
  left-associative operator `(8, 9)` → `WHERE_EXPR`, sitting below the comparison
  tier (so its RHS captures a `<:`/`>:` bound) and above `->`/`=` (so
  `f(x)::T where U` groups as `((f(x)::T) where U)`); `<:`/`>:` are now lexed as
  `SUBTYPE`/`SUPERTYPE` comparison operators (infix and prefix). In call/index
  argument lists, a `;` opens a `PARAMETERS` node for the keyword section and
  `name = value` builds a `KEYWORD_ARG` (`kw`-style); splat `x...` (lexed as a
  single `...` token) is a terminal postfix `SPLAT_EXPR`.
- [x] Array/tuple/comprehension literals (`[1, 2; 3 4]`, `(a, b)`,
  `[x for x in xs]`), ranges, broadcasting dots, ternary `a ? b : c`. Vectors
  (`VECT_EXPR`), matrices (`MATRIX_EXPR`/`MATRIX_ROW`, with significant
  whitespace for hcat columns and `;`/newline for vcat rows), tuples and named
  tuples (`TUPLE_EXPR`), comprehensions (`COMPREHENSION`/`COMPREHENSION_IF`) and
  generators (`GENERATOR`) reusing `FOR_BINDING`, broadcasting operators
  (`.+`/`.*`/… and `f.(x)` as `DOT_CALL_EXPR`), and the ternary `? :`
  (`TERNARY_EXPR`). Ranges already parsed via the `:` infix operator.
  Multi-clause generators (`for … for … if …`, each `for` a sibling
  `FOR_BINDING`, each trailing `if` a `COMPREHENSION_IF` the projector folds into
  a `filter`) and comma-separated cartesian specs (`for a in as, b in bs` →
  `cartesian_iterator`) both parse; the `a = as` spec form is a plain
  `ASSIGNMENT_EXPR`. Bare call-argument generators (`sum(x for x in xs)` →
  `CALL_EXPR` with a `GENERATOR` child) and typed comprehensions
  (`T[x for x in xs]` → `TYPED_COMPREHENSION`) reuse the same machinery.
  Follow-ups: tuple-destructuring loop vars (`for (i, j) in …`), v1.7 matrix-row
  syntax (`[1, 2; 3, 4]`), and unicode dotted operators.
- [x] Transpose/adjoint postfix `'`. The lexer disambiguates `'` by the
  *immediately* preceding token (`prev_ends_value` in `lexer.rs`): when it abuts
  a value-ending token (ident, literal, closing `)`/`]`/`}`, string/cmd close,
  another `'`, …) it lexes as a `Transpose` operator, otherwise it opens a
  `Char` literal — matching Julia's whitespace sensitivity (`A'` transpose vs
  `A '` char). The postfix chain (`parse_postfix_chain`) wraps the operand in a
  `POSTFIX_EXPR` and re-loops, so it chains (`A''`) and composes with later
  suffixes (`A'[i]`, mirroring JuliaSyntax's `(ref (call A ') i)`).
- [x] Bare `end` inside indexing (`a[end]`). An `end_marker` flag, threaded
  through the Pratt parser alongside `inside_brackets`/`no_range`/`array_mode`,
  enables a bare `end` to parse as an `END_MARKER` atom rather than a block
  terminator. It is turned on only inside square brackets — indexing and vector
  literals (both close with `]`, set in `parse_arg_list`; array/matrix elements
  via `parse_element`) — and stays off inside `(…)`/`{…}`, matching Julia's
  `end`-symbol scope (so `f(end)` keeps `end` as a bare token). It propagates
  through operators, ranges, prefix operands, and ternary branches, so
  `a[end-1]`, `a[2:end]`, and `m[end, end]` all parse correctly.
- [x] Bare `begin` inside indexing (`a[begin]`). Mirrors the `end` marker with a
  `begin_marker` flag, but scoped to *indexing* position only — derived as
  `close == ]` *and* `list_kind == ARG_LIST` in `parse_arg_list`, so a vector
  literal's `[begin … end]` stays a block (`(vect (block …))`), matching Julia
  (`begin` is a first-index marker only in `ref` position). A leading `begin`
  there parses as a `BEGIN_MARKER` atom (the leading-keyword block dispatch is
  skipped when `begin_marker` is set), composing through ranges/operators so
  `a[begin:end]`, `a[begin+1]`, and `m[begin, end]` all parse correctly.
- [x] Symbol/expression quoting (`:foo`, `:end`, `:(x + 1)`). A prefix `:` in
  `parse_prefix` builds a `QUOTE_SYM` via `parse_quote_sym` (mirroring the
  `$ident`/`$(expr)` interpolation split): `:ident` wraps a `NAME`, `:keyword`
  wraps the keyword token as a symbol (`TokKind::is_keyword`), and `:(expr)`
  wraps a parsed `PAREN_EXPR`; the projector maps all three to JuliaSyntax's
  `(quote-: …)`. A bare `:` not followed by a quotable token returns `None`, so
  the index colon in `a[:]` is untouched. Prefix operator symbols now quote too
  (`:+`, `:<:`, `:+=` → `(quote-: …)`): an extra `parse_quote_sym` arm wraps an
  undotted operator-name token (`is_op_name`, shared from `structural.rs`) or an
  assignment operator (`is_assignment_op`) as a bare symbol, matching Julia (a
  space before the op, `: +`, is an error and stays unhandled). **Known
  limitations:** the bare-`:` Colon value (`a[:]` → `(ref a :)`), broadcast
  operator quotes (`:.+` → `(. +)`), and paren-quoted operators (`:(=)`, `:(::)`)
  are deferred (still divergences).
- [x] Pair operator `=>` (and broadcast `.=>`). Lexed as `FatArrow`/`DotFatArrow`
  (a new two-/three-char operator), parsed as a `BINARY_EXPR` on the arrow tier
  `(4, 3)` — right-associative, looser than `||`, tighter than `=` — and
  projected to `(call-i a => b)`/`(dotcall-i a => b)`. Unblocks `Dict(:a => 1)`
  shapes (composing with the symbol quoting above).
- [x] Full numeric-literal coverage (rationals, `Inf`/`NaN`, big literals).
  `lex_number` (`lexer.rs`) now splits the base-prefixed integers into distinct
  `HEX_INT`/`OCT_INT`/`BIN_INT` kinds (with per-base digit classes and
  lowercase-only `0x`/`0o`/`0b` prefixes, matching Julia — `0X1` is `0` then
  `X1`), lexes hex floats (`0x1.8p3`, always `FLOAT`/Float64), and distinguishes
  the `f` exponent marker as `FLOAT32` from `e`/`E` `FLOAT` — mirroring
  JuliaSyntax's `Integer`/`BinInt`/`OctInt`/`HexInt`/`Float`/`Float32` leaf
  taxonomy. Rationals `//` and broadcast `.//` are now lexed as operators
  (`SLASH_SLASH`/`DOT_SLASH_SLASH`) at a new left-associative tier `(28, 29)`
  between times and power (`1//2*3` ⇒ `(1//2)*3`; `1//2^3` ⇒ `1//(2^3)`).
  **No-ops by design:** `Inf`/`NaN`/`Inf32`/… are ordinary identifiers in Julia,
  not literals, so they stay `NAME`; oversized "big" integer literals remain
  plain `INTEGER` tokens (type promotion is a lowering concern, not the
  parser's). **Deferred:** numeric juxtaposition / implicit multiplication
  (`2x`, `2π`, `1im`) is a separate parser feature — the literal there is just
  the number and `im`/`x` are identifiers.
- [x] Augmented (compound) assignment operators `op=` (parity-driven ASCII set):
  `+= -= *= /= //= ^= %= |= &=` plus broadcast `.+= .-= .*= ./= .//= .^= .%=`.
  Lexed as single tokens (longest-match: `.//=` 4-char and `//=` 3-char beat their
  prefixes), parsed via `is_assignment_op` into an `ASSIGNMENT_EXPR` on the
  loosest right-associative tier `(2, 1)` (same as `=`/`.=`), and projected with
  the operator's own text as head (`(+= a b)`, `(.+= a b)`). `global x += 1` and
  `let x += 1` come along for free. **Deferred:** shift/`\`/`:`/`$`/unicode
  augmented forms (`<<= >>= >>>= \= := $= ÷= ⊻=`), operator-symbol quoting
  (`:+=`).
- [x] The `~` operator (and broadcast `.~`). Lexed as `Tilde`/`DotTilde`; infix on
  the assignment tier `(2, 1)` — right-associative and as loose as `=` (`a ~ b = c`
  ⇒ `(~ a (= b c))`) — but built as an ordinary `BINARY_EXPR` (handled in
  `infix_binding_power`, not `is_assignment_op`), projecting `(call-i a ~ b)` /
  `(dotcall-i a ~ b)`. Prefix `~a`/`.~x` reuse the unary-operator arm →
  `(call-pre ~ a)` / `(dotcall-pre ~ x)`. The whitespace-sensitive matrix splitting
  (`[a ~b]` is hcat of `a` and prefix `~b`; `[a~b]`/`[a ~ b]` is one infix element)
  falls out of the shared `is_operator` machinery for free. **Deferred:** the bare
  operator-as-value `~` (`(~)`).

- [x] Broadcast short-circuit operators `.&&` and `.||`. Lexed as
  `DotAndAnd`/`DotOrOr` (3-char dotted table), sharing the `&&`/`||` precedence
  tiers `(7, 8)`/`(5, 6)`. Built as ordinary `BINARY_EXPR`s and projected with
  their own special heads `(.&& a b)` / `(.|| a b)` (mirroring `&&`/`||`'s
  `Special` heads, not `dotcall-i`). Mixed chains like `x .&& y .|| z` match Julia;
  same-operator chains inherit the existing left-nesting divergence of `&&`/`||`.

- [x] Range operator `..`. Lexed as `DotDot` (longest match after `...`, before
  the broadcast-dot block); the number lexer no longer eats a `.` followed by `.`
  so `1..n` is `1 .. n`. Shares the colon precedence tier `(14, 15)`
  (left-associative), built as an ordinary `BINARY_EXPR` and projected to
  `(call-i a .. b)`. The `...`-splat-vs-`..` postfix precedence (`x..y...`) stays
  a divergence (separate splat-precedence gap).

- [x] Richer `import`/`using` path trees. A dedicated `parse_import_stmt`
  (`structural.rs`) replaces the verbatim passthrough: each clause is an
  `IMPORT_PATH` node (leading relative dots `.`/`..`/`...` then dot-separated name
  components), optionally wrapped in an `IMPORT_ALIAS` for an `as` rename (`as` is
  a contextual identifier). A top-level `:` switches from the base path to a
  comma-separated list of imported names; `,`/`:` separators are kept as tokens so
  the projector groups base-vs-names. Projects to `(import (importpath . A))`,
  `(import (as (importpath A) B))`, and `(import (: (importpath A) (as (importpath
  x) y)))` — faithfully, reading the real nodes (no projector reconstruction).
  **Deferred (still divergences):** `@macro`/`$interp` paths, and the `. .A`
  (space-separated dots) form — each is carried through verbatim, keeping
  losslessness. `export`'s name list is untouched (still passthrough).
  Operator-symbol names now parse (see the dedicated bullet below).

- [x] Arrow, pipe, and bitshift operators. The arrow family `-->` (own special
  head `(--> a b)`), `<-->` (ordinary `(call-i a <--> b)`), and broadcast `.-->`
  (`(dotcall-i a --> b)`) join the existing arrow tier `(4, 3)` (right-associative).
  The pipe operators split Julia's two pipe precedences: left-pipe `<|` (`PipeLt`)
  is looser and right-associative at `(12, 11)`, right-pipe `|>` (and new broadcast
  `.|>`) is tighter and left-associative, bumped from `(12, 13)` to `(13, 14)` to
  open the slot (colon still binds tighter, 14 ≥ 14). Bitshift `<< >> >>>`
  (`Shl`/`Shr`/`UShr`) sit at a new left-associative tier `(30, 31)` between `//`
  and `^` (Julia precedence 14). Lexed with longest-match (`<-->` 4-char and `-->`/
  `>>>` 3-char beat their prefixes; `.-->` 4-char beats `.-`). **Deferred:** dotted
  bitshift (`.<< .>> .>>>`), and the unicode-subscript arrow `-->₁`.

- [x] Operator-symbol import names. `parse_import_path` (`structural.rs`) now
  accepts symbolic operators as path components in three positions: a bare name in
  the `:` list (`import A: +, ==`, `import Base: +, -, *`), a fused dotted operator
  component (`import A.==`, lexed as the single `.==` token whose leading dot is the
  separator — the projector strips it), and a quoted operator symbol after a dot
  (`import A.:+` → a `QUOTE_SYM` node → `(importpath A (quote-: +))`). Two
  predicates (`is_op_name`/`is_dotted_op_name`) gate the undotted vs. fused-dotted
  operator tokens; `project_import_path` reuses the projector's `is_operator` and
  routes `QUOTE_SYM` children through `project_quote_sym`. **Deferred (still
  divergences):** unicode operators (`import .⋆`, `import A.⋆.f` — `⋆` lexes as
  `ERROR`, awaiting unicode-operator lexing) and the paren-quoted forms
  (`import A.:(+)`, `import A.(:+)`).

## Incremental reparse

- [ ] Token/block reparse splicing beneath `parsed_document`
  (`src/incremental.rs`), à la rust-analyzer `reparsing.rs` and arity's
  `src/parser/reparse.rs`: recover the edit from old/new text, splice reused
  green subtrees, fall back to a full parse. Pin correctness with an oracle
  property test (`reparse == parse(new)` across a corpus).

## Formatter

- [ ] Per-construct IR rules (`src/formatter/rules/`): replace the lossless
  passthrough in `core::format` with native IR builders per construct
  (assignments, binary chains, calls/arg-lists, blocks, control flow),
  printed by the existing best-fit engine.
- [ ] Range formatting (`textDocument/rangeFormatting`).
- [ ] Runic-compat gauge: a `#[ignore]`d test measuring the fixed point
  `runic(fatou(x)) == fatou(x)`, plus an allowlist with rationales.
  `task   runic-compat` (placeholder in `Taskfile.yml`).

## Linter

- [ ] First rules (correctness + suspicious), each a `Rule` impl registered in
  `src/linter/rules.rs`.
- [ ] Autofix application engine (`apply_fixes`) honoring `Applicability`
  (safe/unsafe), with the `format → lint --fix → format --check` property
  test (Tenet 5).
- [ ] `annotate-snippets`-based pretty diagnostics rendering (dependency noted
  in `Cargo.toml`; `render.rs` is currently a compact one-liner renderer).

## Language server

- [ ] Dedicated lint thread owning the persistent `IncrementalDatabase` (salsa
  is single-writer) + a rayon read pool for latency-sensitive read requests,
  replacing the single-threaded loop in `src/lsp.rs`.
- [ ] Hover, go-to-definition, references, document symbols, rename — these need
  a per-file semantic model (scopes, bindings, read sites) that does not
  exist yet.
- [ ] Incremental (range) document sync instead of full-document sync.

## Semantic / project analysis

- [ ] Per-file `SemanticModel` (scope tree, bindings, read sites).
- [ ] Cross-file/project resolution and a Julia package/module index (the rough
  analog of arity's `project/` + `rindex/`).

## Tooling

- [ ] `build.rs` generating shell completions + man pages (clap_complete /
  clap_mangen), as arity does.
- [x] JuliaSyntax.jl differential parser harness (the parser oracle; see
  `AGENTS.md`), run via the Julia toolchain in the devenv. A *projector*
  (`src/parser/sexpr.rs`, `to_juliasyntax_sexpr`/`normalize_sexpr`, also
  `fatou parse --to sexpr`) walks the CST and emits JuliaSyntax's `SyntaxNode`
  s-expression shape, translating only *encoding* differences (wrapper nodes,
  delimiters, trivia) and leaving genuine modeling divergences (comparison
  chains stay nested, loose header passthrough) faithful so they surface. The
  harness (`tests/juliasyntax_oracle.rs`) diffs each fixture against a pinned
  `expected.sexpr` (`tests/fixtures/oracle/<slug>/`, refreshed by
  `scripts/update-juliasyntax-corpus.{sh,jl}`, version-pinned in
  `.juliasyntax-source`); `oracle_allowlist` guards the 34 matching cases
  (no Julia needed → CI-safe), `oracle_full_report` (`#[ignore]`d) writes a
  triage report, and `tests/oracle/{allowlist,blocked}.txt` (keyed by slug)
  partition the corpus — 6 blocked with rationales (numeric-literal display
  normalization, triple-string dedent, `end`/`[1 +2]`/unterminated-string and
  incomplete-`do` error shapes). A harvested **JuliaSyntax sub-corpus**
  (`scripts/harvest-juliasyntax-corpus.jl` → `tests/fixtures/oracle/juliasyntax.jsonl`,
  575 micro-cases extracted from JuliaSyntax's own `test/parser.jl`, expected
  regenerated via our pinned `parseall`) is gated opt-in by `oracle_juliasyntax`
  against `tests/oracle/juliasyntax-allowlist.txt` (251 cases); the
  `juliasyntax_full_report` divergence (282) + unsupported (42) buckets are the
  **prioritized parser-growth backlog** — e.g. associative n-ary flattening
  (`a*b*c`) and paren-quoted operators (`:(=)`, `:(::)`).
  **Follow-ups:** work the backlog up the allowlist;
  design error-shape parity to promote the blocked recovery cases; wire the
  oracle gates into CI.
- [ ] Benchmarks (`criterion`) for parse + incremental reparse.
- [ ] `smol_str` interning for symbol names once the semantic model lands.
