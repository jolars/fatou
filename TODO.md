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

- [x] Parser: one-line space-separated `for` body parses into the loop `BLOCK`.
  `for i in 1:3 x += i end` ⇒ `(for (= i (call-i 1 : 3)) (block (+= x i)))`. The
  statement `for`-binding now reuses the comprehension/generator spec parser
  (`parse_for_specs`, parametrized by `bracketed`) so the binding stops at the
  end of the last iterable and a same-line body falls through to the loop block
  rather than being swallowed as flat tokens. Applies to `in`/`=`/comma and
  cartesian forms (`for i in 1:3, j in 1:4 g(i,j) end`). The iterable is now a
  real expression node, not loose passthrough tokens. (`∈` still projects as
  `(call-i i ∈ xs)` rather than `(= i xs)`—a pre-existing, separate divergence
  shared by generators and the multi-line form, since `∈` is a symbol operator
  not suppressed by `no_word_op`.)

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
  trailing-comma drop with the 1-tuple `(a,)` comma kept; accepts `KEYWORD_ARG`
  elements so named tuples `(a=1,b=2)` → `(a = 1, b = 2)`, locked by
  `named_tuples/`), tight range `:`
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
  normalizing; bails on comment/newline, locked by `where_clauses/`),
  float-literal normalization (`lower_literal` over `LITERAL` + `normalize_float`:
  `.5` → `0.5`, `1.` → `1.0`, `1E10` → `1.0e10`, `1f0` → `1.0f0`, `1.50` → `1.5`,
  exponent marker lowercased and exponent/integral leading zeros stripped; only
  `FLOAT`/`FLOAT32` tokens are rewritten, every other literal passes through
  verbatim; underscored and hex `0x…p…` floats are left untouched, mirroring
  Runic's `format_float_literals`; locked by `float_literals/`), hex-integer
  zero-padding (`lower_literal` over `HEX_INT` + `normalize_hex`: `0xF` → `0x0F`,
  `0x12345` → `0x00012345`—pad the literal's **byte span** to the next of
  `0x`+2/4/8/16/32 chars by inserting `0`s after `0x`; underscores count toward
  the span (`0x1_2` → `0x01_2`), digit case is preserved, BigInt literals
  (span ≥ 34) and already-canonical spans are left verbatim; octal/binary
  untouched, mirroring Runic's `format_hex_literals`; locked by `hex_literals/`),
  `export`/`public` name lists (`lower_export_stmt` over `EXPORT_STMT`/
  `PUBLIC_STMT`: `export a,b` → `export a, b`, `public foo,bar` →
  `public foo, bar`—keyword + space, commas `", "`-joined; a name may be an
  identifier, operator (`export +, -`), macro (`export @m`), or `var"…"` form, so
  the rule glues the tokens of one name and only spaces comma boundaries; bails on
  comment/newline or a leading/trailing/doubled comma; locked by
  `export_public_lists/`), trailing-whitespace trimming (`lower_trivia` in the
  transparent path, mirroring Runic's `trim_trailing_whitespace`: a `WHITESPACE`
  run right before a `NEWLINE` is dropped and a line `COMMENT`'s trailing blanks
  are stripped, while string content and block comments stay verbatim; locked by
  `trailing_whitespace/`), parenthesized-expression padding (`lower_paren` over
  `PAREN_EXPR`: `( a + b )` → `(a + b)`, `(  x  )` → `(x)`—strip the incidental
  whitespace flanking the single inner expression, which is lowered recursively
  so nested parens `( (a) )` → `((a))` and the inner spacing keep normalizing;
  the `;`-block `(a; b)` is a distinct `PAREN_BLOCK` and a tuple `(a, b)` is a
  `TUPLE_EXPR`, so neither reaches the arm; bails on comment/newline (a multi-line
  paren Runic reflows), locked by `paren_padding/`), `;`-block padding and
  separators (`lower_paren_block` over `PAREN_BLOCK`: `( a ; b )` → `(a; b)`,
  `(a;b;)` → `(a; b)`—each `;` packed tight-left/space-right, the padding stripped,
  a trailing arg-less `;` dropped; the leading statement and each `PARAMETERS`
  statement are lowered recursively so `(a=1;b=2)` → `(a = 1; b = 2)` and a nested
  block `((a;b);c)` → `((a; b); c)` keep normalizing; only the ≥2-statement form is
  reshaped—a single-statement `(a;)` keeps its trailing `;` via the transparent
  fallback, matching Runic; bails on comment/newline; locked by `paren_blocks/`),
  comprehension/generator `for`-binding `in` normalization (`lower_for_binding`
  over `FOR_BINDING`: `[i for i = 1:3]` → `[i for i in 1:3]`,
  `[i for i ∈ s]` → `[i for i in s]`—rewrite the `=`/`∈` iteration operator to the
  keyword `in`; comma-separated bindings (`i = 1:3, j = 1:3`) each normalized and
  `", "`-joined, a trailing `if cond` filter reproduced with one space around `if`;
  the `for` keyword is emitted iff a child of this node, so the same arm normalizes
  a `for`-loop binding; targets and iterables lowered recursively; bails on
  comment/newline or any unmodeled binding shape; locked by
  `comprehension_for_in/`), `begin`/`quote` block-body indentation
  (`lower_block_expr` + `lower_block_body` over `BEGIN_EXPR`/`QUOTE_EXPR`:
  `begin x end` → `begin⏎    x⏎end`—a non-empty block is always exploded vertical
  and each statement indented one step; `;`-separated statements stay on one line
  (`begin x; y end` → `⏎    x; y`); blank lines preserved capped at 2; statements
  lowered recursively so inner spacing normalizes and nested blocks indent further;
  an empty block keeps its source layout via the transparent fallback; bails on a
  body comment, two statements with no separator, or a missing `end`; locked by
  `begin_quote_blocks/`. `lower_block_body` is the reusable body engine for future
  block constructs), `let` block-body indentation (`lower_let` over `LET_EXPR`:
  `let x = 1; y = 2 end` → `let x = 1⏎    y = 2⏎end`—the first reuse of
  `lower_block_body`; header is `let` + recursively-lowered `LET_BINDINGS`, the
  binding/body separator `;` opens the `BLOCK`; empty body keeps its source layout
  via the transparent fallback; tight multi-binding headers (`let x=1,y=2`) are not
  normalized—the parser leaves later bindings as flat tokens—so kept out of the
  fixture; locked by `let_blocks/`), `while`/`for` loop-body indentation
  (`lower_loop` over `WHILE_EXPR`/`FOR_EXPR`: `while c⏎x end` → `while c⏎    x⏎
  end`—the header is the recursively-lowered `CONDITION` (`while`) or `FOR_BINDING`
  (`for`, supplying the `for ` prefix the binding omits, so `for i = 1:3` →
  `for i in 1:3`), then `lower_block_body` for the body; a non-empty one-line body
  is exploded vertical (`while c; x; y; end` → `while c⏎    x; y⏎end`); empty body
  keeps its source layout via the transparent fallback; loop bodies are never
  `return`-inserted; locked by `loop_blocks/`. Multi-binding `for i = 1:3, j = 1:3`
  leaves its 2nd+ bindings as flat tokens (the `let`/`global`/`local` parser
  asymmetry), so only the first normalizes—kept out of the fixture; one-line
  space-separated `for i in 1:3 x end` mis-parses the body into `FOR_BINDING`
  (empty `BLOCK`)—a parser blocker handed off, kept out of the fixture),
  `if`/`elseif`/`else` and `try`/`catch`/`else`/`finally` branch structure
  (`lower_if`/`lower_try` over `IF_EXPR`/`TRY_EXPR` with a shared
  `lower_branch_clause`: each branch its own `BLOCK` delegated to
  `lower_block_body`, branch keywords emitted at column 0 via `HardLine`; the
  leading `if`/`elseif` `CONDITION` and the optional `catch e` variable are
  lowered recursively; an empty branch body bails the whole construct to the
  transparent fallback rather than partially reshape it; layout-only, never
  `return`-inserted; locked by `if_blocks/` + `try_blocks/`).
  **Next:** comment preservation inside broken brackets/matrices/blocks (the
  harder half), then `function`/`do`/`macro` bodies (which need `lower_block_body`
  but are `return`-inserted—a deferred semantic rewrite)—see the
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
