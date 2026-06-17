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
  as an expression then carry the rest of the line through; `import`/`using`/
  `export` carry the whole clause through verbatim (dedicated `:`/`.` path trees
  come with the operators below).
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
  (`TERNARY_EXPR`). Ranges already parsed via the `:` infix operator. Follow-ups:
  multi-clause comprehensions (`for … for … if …`) and multi-variable bindings
  (`for i, j in …`), bare call-argument generators (`sum(x for x in xs)`),
  v1.7 matrix-row syntax (`[1, 2; 3, 4]`), and unicode dotted operators.
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
  the index colon in `a[:]` is untouched. **Known limitations:** the bare-`:`
  Colon value (`a[:]` → `(ref a :)`) and operator symbols (`:+`, `:(=)`) are
  deferred (still divergences).
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
  (`a*b*c`), the pair operator `=>`, richer import/`using` (`import .A`,
  `x as y`), multi-clause generators, and assorted operators (`-->`, `<|`,
  `.&&`). **Follow-ups:** work the backlog up the allowlist;
  design error-shape parity to promote the blocked recovery cases; wire the
  oracle gates into CI.
- [ ] Benchmarks (`criterion`) for parse + incremental reparse.
- [ ] `smol_str` interning for symbol names once the semantic model lands.
