# TODOs

The groundwork pass establishes the full architecture (parser pipeline, salsa
layer, formatter/linter/LSP skeletons, CLI, tooling, tests) over a deliberately
small Julia subset. This file tracks what comes next, roughly ordered by
leverage.

## Parser

- [x] Parser: `global`/`local` + multiple assignment now nests properly.
  `global a, b = 1, 2` ⇒ `(global (= (tuple a b) (tuple 1 2)))`, `local a, b =
  f(x), g(y)` wraps the calls, `global a, b::Int` ⇒ `(global a (::-i b Int))`.
  Switched `global`/`local` to `KwStmt::ExprTuple` (statement-level parse, like
  `const`); the projector splices a single bare-tuple child. Unblocks
  formatter-parity ranked target #0.
- [x] Lexer: identity/inequality operators `===`, `!==`, and tight `!=`. New
  `EqEqEq`/`NotEqEq` tokens (longest-match, beat `==`/`!=`); `scan_ident` now
  stops at a `!` immediately followed by `=` so `a!=b`⇒`a` `!=` `b` while `f!`,
  `push!`, `a!b` stay identifiers. Project as `(call-i a === b)`/fold into
  `(comparison …)`.
- [x] Lexer: broadcast identity/inequality operators `.===`/`.!==`. New
  `DotEqEqEq`/`DotNotEqEq` tokens, lexed as 4-char dotted ops (longest-match,
  beat the 3-char `.==`/`.!=`). Single op ⇒ `(dotcall-i a === b)`; a run folds
  into `(comparison a (. ===) b …)` via the existing chain machinery.

- [x] Lexer: left-division `\` binary operator (plus `\=`, `.\`, `.\=`). New
  `Backslash`/`BackslashEq`/`DotBackslash`/`DotBackslashEq` tokens mirror the
  slash family via the 5-file operator recipe (same `*`/`/` times tier, left-assoc).
  `a\b` ⇒ `(call-i a \ b)`, `a .\ b` ⇒ `(dotcall-i a \ b)`, `a\=b` ⇒ `(\= a b)`,
  `a.\=b` ⇒ `(.\= a b)`; formatter-parity can now add the spacing fixture.
- [x] Parser: binary-only prefix operators stop at a significant newline. `/`,
  `\`, `.*`, `=>`, `?` etc. in prefix position no longer reach across a
  statement-scope (or array-row) newline for their operand — `/\nx` ⇒ `/` then
  `x` (was a spurious `(call-pre (error /) x)` + false diagnostic), `[/\nx]` ⇒
  `(vcat / x)`, `?\nx` ⇒ `(error ?)` then `x`; inside parens the newline stays
  insignificant (`(/\nx)` ⇒ `(call-pre (error /) x)`). Mirrors the range colon's
  `newline_significant` gate.

### Incremental

- [ ] Token/block reparse splicing beneath `parsed_document`
  (`src/incremental.rs`), à la rust-analyzer `reparsing.rs` and arity's
  `src/parser/reparse.rs`: recover the edit from old/new text, splice reused
  green subtrees, fall back to a full parse. Pin correctness with an oracle
  property test (`reparse == parse(new)` across a corpus).

## Formatter

- [x] Runic.jl differential formatter oracle (direct parity; see `AGENTS.md` and
  the `formatter-parity` skill). `scripts/update-runic-corpus.{sh,jl}` mint a
  pinned `expected.jl = Runic.format_string(input)` per fixture
  (`tests/fixtures/formatter/<slug>/`, version-pinned in `.runic-source`); the
  harness (`tests/runic_oracle.rs`) gates `format(input) == expected.jl` via
  `tests/oracle/runic-allowlist.txt` (CI-safe, no Julia at test time),
  `runic_full_report` (`#[ignore]`d) writes a triage report, and
  `runic-{allowlist,blocked}.txt` partition the corpus (coverage enforced). The
  optional long-term fixed-point gauge (`runic(fatou(x)) == fatou(x)`) is still
  future work.
- [~] Per-construct IR rules (`src/formatter/rules.rs`): replace the lossless
  passthrough in `core::format` with native IR builders per construct, printed by
  the existing best-fit engine. **Landed:** operator/assignment spacing
  (`lower_binary`), comparison chains (`lower_comparison`), call/index arg lists
  (`lower_arg_list` + `lower_keyword_arg`/`lower_parameters`: comma spacing, no
  bracket padding, single-line trailing-comma drop, `;`-kwargs), tuple/vector/
  brace collections (`lower_collection`: comma spacing, no bracket padding,
  trailing-comma drop with the 1-tuple `(a,)` comma kept), tight range `:`
  (`lower_range` + `COLON` in `is_tight_binop`) and `::` type annotations
  (`lower_type_annotation`), multi-line arg-list/collection breaking
  (`lower_multiline_bracket`: framing breaks + indent when content spans ≥2 source
  lines, contagious via descendant `NEWLINE` tokens, source space-vs-break
  preserved between items, per-bracket trailing comma), multi-line matrix breaking
  (`lower_matrix`: framing breaks + reindent each source line when a `MATRIX_EXPR`
  spans ≥2 lines, interior preserved verbatim—intra-row spacing, same-line
  `;`-rows, `;` placement; bails on comment), blank-line preservation in broken
  brackets and matrices (new `Ir::BlankLine` bare-newline primitive; blanks kept
  everywhere—between items/rows and in the leading gap (after the open bracket) and
  trailing gap (before the close), capped at 2 per Runic; one newline is the framing
  break, the rest become blanks), anonymous-function arrow spacing (`lower_arrow`:
  `x->y` → `x -> y`, one space each side, operands recursed; bails on a multi-line
  body), ternary spacing (`lower_ternary`: `a ?  b  :  c` → `a ? b : c`, one space
  around `?` and `:`, operands recursed so nested `a ? b : c ? d : e` keeps
  normalizing; bails on a multi-line ternary), tight field-access `.` (`DOT` in
  `is_tight_binop`: fixed a latent mangling bug where `a.b.c` parsed as a
  `BINARY_EXPR`/`DOT` and got spaced to the invalid `a . b . c`; Julia *requires*
  the dot tight), curly type-parameter padding (`Vector{ Int }` → `Vector{Int}`,
  `Dict{ A ,B }` → `Dict{A, B}`: `CURLY_EXPR`'s brace `ARG_LIST` now flows through
  `lower_arg_list` by accepting `LBRACE`/`RBRACE`—same comma spacing, no padding,
  trailing-comma drop), keyword-statement spacing (`lower_keyword_stmt` over
  `RETURN_EXPR`/`CONST_STMT`/`GLOBAL_STMT`/`LOCAL_STMT`: `return  x` → `return x`,
  one space between the keyword and its recursed operand, bare `return` kept),
  bare-tuple comma spacing (`lower_bare_tuple` over `BARE_TUPLE_EXPR`: `x,y` →
  `x, y`, `a,b = 1,2` → `a, b = 1, 2`, `return x,y` → `return x, y`; bracketless,
  `", "`-joins the recursed bare elements; bails on leading/doubled/trailing comma
  or a comment/newline), `global`/`local` comma name lists (`lower_keyword_stmt`
  extended: `global a,b` → `global a, b`, `local x,y,z` → `local x, y, z`—the
  parser drops `NAME`/`IDENT`/`COMMA` directly into the statement node, so this is
  a flat name list, not an operand subtree; `", "`-joins the clean
  item/`COMMA` alternation, bails on the `=`/`::` assignment-list forms
  `global a, b = 1, 2`, a comment, or a leading/trailing comma), `using`/`import`
  comma + selector lists (`lower_import_stmt` over `USING_STMT`/`IMPORT_STMT`:
  `using A,B` → `using A, B`, `using A: x,y` → `using A: x, y`—item(`IMPORT_PATH`/
  `IMPORT_ALIAS` node)/separator alternation, `COMMA` → `", "`, selector `COLON` →
  `": "`, paths recursed transparently; bails on comment/newline or a
  leading/trailing/doubled separator), `global`/`local` multiple assignment
  (`global a,b = 1,2` → `global a, b = 1, 2`, `local a,b = f(x),g(y)`,
  `global a,b::Int`—rule-free PASS once the parser nested these as a single
  `ASSIGNMENT_EXPR`/`BARE_TUPLE_EXPR` operand; the existing keyword-stmt
  single-operand arm + `lower_binary` + bare-tuple recursion handle it, locked by
  the `global_local_assignment/` fixture), `where`-clause brace normalization
  (`lower_where` over `WHERE_EXPR`: `f(x) where T` → `f(x) where {T}`—one space
  each side of `where`, the bound **always** brace-wrapped; an already-braced
  bound is normalized in place via `lower_collection` (`where { T , S }` →
  `where {T, S}`), any other bound is wrapped and recursed (`where T<:Real` →
  `where {T <: Real}`); nested `where` and the bound's own spacing keep
  normalizing; bails on comment/newline, locked by `where_clauses/`). **Next:**
  comment preservation inside broken
  brackets/matrices (the harder half), blocks, control flow—see the
  `formatter-parity` RECAP's ranked targets.
  (Unary spacing is Runic-preserved, so no rule; single-line matrices `[1 2]`/
  `[1 2; 3 4]` are pure preservation—transparent fallback already matches Runic,
  locked by the `matrices/` regression fixture, no rule; compound range operands like
  `a + 1:b` Runic *parenthesizes*—a semantic rewrite, out of scope.)
- [ ] Range formatting (`textDocument/rangeFormatting`).

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
- [ ] Hover, go-to-definition, references, document symbols, rename—these need
  a per-file semantic model (scopes, bindings, read sites) that does not
  exist yet.
- [ ] Incremental (range) document sync instead of full-document sync.

## Semantic/project analysis

- [ ] Per-file `SemanticModel` (scope tree, bindings, read sites).
- [ ] Cross-file/project resolution and a Julia package/module index (the rough
  analog of arity's `project/` + `rindex/`).

## Tooling

- [ ] `build.rs` generating shell completions + man pages
  (clap_complete/clap_mangen), as arity does.
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
  partition the corpus—4 blocked with rationales (numeric-literal display
  normalization, `end`/unterminated-string and incomplete-`do` error shapes). A harvested **JuliaSyntax sub-corpus**
  (`scripts/harvest-juliasyntax-corpus.jl` → `tests/fixtures/oracle/juliasyntax.jsonl`,
  575 micro-cases extracted from JuliaSyntax's own `test/parser.jl`, expected
  regenerated via our pinned `parseall`) is gated opt-in by `oracle_juliasyntax`
  against `tests/oracle/juliasyntax-allowlist.txt` (251 cases); the
  `juliasyntax_full_report` divergence (282) + unsupported (42) buckets are the
  **prioritized parser-growth backlog**—e.g. associative n-ary flattening
  (`a*b*c`) and unicode operators (lexer).
  **Follow-ups:** work the backlog up the allowlist; continue the error-shape
  parity slices (the taxonomy infrastructure has landed—see the typed
  error-node bullet above); wire the oracle gates into CI.
- [ ] Benchmarks (`criterion`) for parse + incremental reparse.
- [ ] `smol_str` interning for symbol names once the semantic model lands.
