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

- [x] Hand-authored formatter fixture gate (see `AGENTS.md` and the `formatter`
  skill). Each fixture (`tests/fixtures/formatter/<slug>/`) holds an `input.jl`
  and a hand-written `expected.jl`; `tests/formatter.rs` gates
  `format(input) == expected.jl` (presence of `expected.jl` is gate membership —
  no allowlist) and, over every `input.jl`, checks idempotence + clean reparse.
  The Runic.jl differential oracle was removed (it preserved source line breaks,
  contradicting Tenet 1; `expected.jl` is now authored under full reflow).
- [~] Width-driven reflow engine: make `line_width` actually drive breaking
  (collapse when it fits, break + indent when it doesn't), replacing the current
  source-break mirroring in `rules.rs`. The prerequisite for true Tenet-1
  conformance and the headline formatter target. **Landed:** call/index arg lists
  (`lower_arg_list`) and tuple/vector/brace collections (`lower_collection`) now
  build a width-driven `Ir::group` (flat when it fits, one-item-per-line with a
  broken-only trailing comma via `Ir::IfBreak` when it doesn't), ignoring source
  line breaks and trailing commas; the one-tuple `(a,)` keeps its semantic comma
  in both modes. Matrices (`lower_matrix_reflow`) now build a width-driven group
  too: flat `[a b; c d]` (rows joined by `; `, elements by a single space) when it
  fits, else framed one row per line; `;`-vs-newline row spelling and intra-row
  spacing no longer leak into the output (`matrices/` gated). Comment-bearing
  brackets (`lower_multiline_bracket`) now fully explode (one item per line,
  always a trailing comma, blanks dropped, comment attachment preserved), killing
  the last call/collection source-break mirror; `bracket_comments/` gated.
  Comment-bearing matrices (`lower_matrix_multiline`) now canonicalize the same
  way (always framed one row per line, row elements single-space joined, trailing
  comment rides its row at one leading space, own-line comments keep their line,
  blanks dropped), killing the last matrix source-break mirror;
  `matrix_comments/` + `matrix_block_comments/` gated. The six non-comment
  bracket/matrix fixtures (`multiline_brackets`, `bracket_blank_lines`,
  `bracket_gap_blank_lines`, `multiline_matrices`, `matrix_blank_lines`,
  `matrix_gap_blank_lines`) are now gated too — the collection/bracket/matrix
  family is fully Tenet-1 (no source-break mirrors left).
  **Next:** the remaining source-break mirrors live in the block/statement
  families.
- [~] Per-construct IR rules (`src/formatter/rules.rs`): replace the lossless
  passthrough in `core::format` with native IR builders per construct, printed by
  the existing best-fit engine. **Landed:** operator/assignment spacing
  (`lower_binary`), comparison chains (`lower_comparison`: **now width-driven** —
  same Air-style group+indent as `lower_binary`, flat when it fits else
  operator-trailing; `comparison_chains/` gated), call/index arg lists
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
  preserved between items, per-bracket trailing comma; trailing/own-line/header
  comments preserved—trailing item comments and the open-bracket comment ride
  with one canonical pre-`#` space (a Tenet-1 divergence from Runic's verbatim
  spacing), own-line comments become their own indented lines), multi-line matrix
  breaking (`lower_matrix`: framing breaks + reindent each source line when a
  `MATRIX_EXPR` spans ≥2 lines, interior preserved verbatim—intra-row spacing,
  same-line `;`-rows, `;` placement, and now line comments kept verbatim as line
  elements), blank-line preservation in broken
  brackets and matrices (new `Ir::BlankLine` bare-newline primitive; blanks kept
  everywhere—between items/rows and in the leading gap (after the open bracket) and
  trailing gap (before the close), capped at 2 per Runic; one newline is the framing
  break, the rest become blanks), anonymous-function arrow spacing (`lower_arrow`:
  `x->y` → `x -> y`, one space each side, operands recursed; **now width-driven** —
  the `->` never breaks (assignment-style), the RHS group absorbs any break,
  source line breaks ignored; `arrow_functions/` gated), ternary conditionals (`lower_ternary`: **now width-driven** — one
  `Ir::group` per ternary node with its own `Ir::indent`, flat `a ? b : c` when it
  fits, else operator-trailing with the branch operands indented one step; each
  nested `?:`-chain forced to break nests one level deeper on top of its parent's
  indent; source line breaks ignored; `ternary_multiline/`, `ternary_spacing/`,
  and `ternary_paren_branch/` gated),
  tight field-access `.` (`DOT` in
  `is_tight_binop`: fixed a latent mangling bug where `a.b.c` parsed as a
  `BINARY_EXPR`/`DOT` and got spaced to the invalid `a . b . c`; Julia *requires*
  the dot tight), curly type-parameter padding (`Vector{ Int }` → `Vector{Int}`,
  `Dict{ A ,B }` → `Dict{A, B}`: `CURLY_EXPR`'s brace `ARG_LIST` now flows through
  `lower_arg_list` by accepting `LBRACE`/`RBRACE`—same comma spacing, no padding,
  trailing-comma drop), keyword-statement spacing (`lower_keyword_stmt` over
  `RETURN_EXPR`/`CONST_STMT`/`GLOBAL_STMT`/`LOCAL_STMT`: `return  x` → `return x`,
  one space between the keyword and its recursed operand, bare `return` kept;
  locked by `keyword_statements/`),
  bare-tuple comma spacing (`lower_bare_tuple` over `BARE_TUPLE_EXPR`: `x,y` →
  `x, y`, `a,b = 1,2` → `a, b = 1, 2`, `return x,y` → `return x, y`; bracketless,
  `", "`-joins the recursed bare elements; bails on leading/doubled/trailing comma
  or a comment/newline), `global`/`local` comma name lists (`lower_keyword_stmt`
  extended: `global a,b` → `global a, b`, `local x,y,z` → `local x, y, z`—rule-free
  PASS: the parser wraps the names in a single `BARE_TUPLE_EXPR` operand, so
  `lower_keyword_stmt`'s single-operand arm + `lower_bare_tuple` recursion handle
  it; locked by `global_local_names/`), `using`/`import`
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
  `TUPLE_EXPR`, so neither reaches the arm. `lower_paren` is now **width-driven**
  (Tenet 1): one `Ir::group` — flat `(inner)` when it fits `line_width`, else `(`
  then the inner expression at `+indent` then `)` flush. Source line breaks no longer
  force the split (`x = (\n1+2\n)` → `(1 + 2)`); only the content's width or a hard
  break it carries does. Blank lines inside the parens are stripped (the loop skips
  every `NEWLINE`/`WHITESPACE`, so only the inner node reaches layout). Bails on a
  comment in a direct gap; locked by `paren_multiline/` + `paren_padding/`. (The
  once-deferred binary-inside-paren case `y = (a +\nb)` → `(a + b)` now collapses,
  since `lower_binary` went width-driven.)), `;`-block padding and
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
  and each statement indented one step; `;` and newline are equivalent statement
  separators so each statement gets its own line (`begin x; y end` →
  `⏎  x⏎  y`, Tenet 1); blank lines preserved capped at 1; statements
  lowered recursively so inner spacing normalizes and nested blocks indent further;
  an empty block collapses to the canonical inline `begin end`/`quote end` via
  the shared `push_block_body` helper (Tenet 1); bails on a body comment, two
  statements with no separator, or a missing `end`; locked by
  `begin_quote_blocks/`. `lower_block_body` is the reusable body engine for future
  block constructs), `let` block-body indentation (`lower_let` over `LET_EXPR`:
  `let x = 1; y = 2 end` → `let x = 1⏎    y = 2⏎end`—the first reuse of
  `lower_block_body`; header is `let` + recursively-lowered `LET_BINDINGS`, the
  binding/body separator `;` opens the `BLOCK`; an empty body collapses to the
  canonical inline `let end` (or `let x = 1 end`) via `push_block_body` (Tenet 1);
  tight multi-binding headers (`let x=1,y=2`) are not
  normalized—the parser leaves later bindings as flat tokens—so kept out of the
  fixture; locked by `let_blocks/`), `while`/`for` loop-body indentation
  (`lower_loop` over `WHILE_EXPR`/`FOR_EXPR`: `while c⏎x end` → `while c⏎    x⏎
  end`—the header is the recursively-lowered `CONDITION` (`while`) or `FOR_BINDING`
  (`for`, supplying the `for ` prefix the binding omits, so `for i = 1:3` →
  `for i in 1:3`), then `lower_block_body` for the body; a non-empty one-line body
  is exploded vertical (`while c; x; y; end` → `while c⏎    x; y⏎end`); an empty
  body collapses to the canonical inline `while c end`/`for i in y end` via
  `push_block_body` (Tenet 1); loop bodies are never
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
  lowered recursively; an empty body contributes no lines (via
  `lower_body_allow_empty`): a clause-less empty `if` folds inline against `end`
  (`if x end`), while an empty body inside a chain leaves its keyword header
  followed directly by the next clause or the shared `end` (`try⏎catch⏎end`); a
  clause-less `try` (`try end` is a syntax error) bails to transparent; layout-only,
  never `return`-inserted; locked by `if_blocks/` + `try_blocks/`), own-line line
  comments in block bodies (`lower_block_body` extended: a `COMMENT` token on an
  otherwise-empty line becomes its own statement line, re-indented to the body;
  shared by `begin`/`quote`/`let`/loops/`if`/`try`; locked by `block_comments/`),
  trailing comments in block bodies (`lower_block_body` line model gained a
  per-line `comment` field: a `COMMENT` after a statement is space-joined and
  always last; one canonical space before `#`—a Tenet-1 divergence from Runic,
  which preserves the user's ≥1 pre-`#` whitespace, recorded as
  `trailing_comment_spacing`; locked by `trailing_comments/`),
  comment preservation inside broken brackets/matrices
  (`lower_multiline_bracket` gained a comment model; locked by `bracket_comments/`
  + `matrix_comments/`), block-comment (`#= … =#`) preservation in block bodies
  (`lower_block_body` `BLOCK_COMMENT` arm: own-line/multi-line kept verbatim with
  only the `#=` line re-indented, trailing rides with one canonical space—the same
  Tenet-1 spacing divergence, recorded as `block_comment_spacing`;
  locked by `block_comments_in_blocks/`), block-comment (`#= … =#`) preservation
  inside broken brackets/matrices (`lower_multiline_bracket` + `lower_matrix`
  `BLOCK_COMMENT` arms reusing the existing comment models: trailing/own-line/
  header/multi-line kept verbatim; brackets ride the trailing one with one
  canonical space—the existing `bracket_comment_spacing`; matrices are
  verbatim so no divergence; locked by `bracket_block_comments/` +
  `matrix_block_comments/`), `struct`/`mutable struct` field-body indentation
  (`lower_struct` over `STRUCT_DEF`, fourth reuse of `lower_block_body`: the
  `SIGNATURE` header is lowered recursively so `struct Bar<:Animal` →
  `struct Bar <: Animal`; a non-empty body always explodes vertical, an empty
  body collapses to the canonical inline `struct Name end` regardless of source
  whitespace (`block_is_empty` distinguishes it from an unmodeled body, which
  still bails to transparent); field bodies are declarations, never
  `return`-inserted; locked by `struct_blocks/`), `module`/`baremodule` body
  indentation (`lower_module` over `MODULE_DEF`, sharing the body engine split out
  as `build_block_body`: the body is *conditionally* indented per Runic's
  `indent_toplevel`/`indent_module` rule — flush when the module is the lone
  top-level node or is nested in a non-module block, indented when it shares the
  top level with a sibling or has a `module` ancestor (`module_should_indent`);
  the `SIGNATURE` is lowered recursively; an empty body collapses to the canonical
  inline `module M end` via `push_block_body` (Tenet 1); module bodies are
  declarations, never `return`-inserted; locked by
  `module_blocks/` + `module_siblings/` + `module_baremodule/` +
  `module_leading_comment/`), `abstract type`/`primitive type` declarations
  (`lower_type_decl` over `ABSTRACT_DEF`/`PRIMITIVE_DEF`: bodyless one-liners,
  **now width-driven Tenet-1** — every whitespace run collapses to one space, both
  the keyword region *and* the post-signature region (`abstract type Foo   end` →
  `abstract type Foo end`); the `SIGNATURE` and bits `LITERAL` lower recursively so
  `Bar<:Baz` → `Bar <: Baz`; locked by `abstract_types/` + `primitive_types/`),
  `function`/`macro` definition bodies (`lower_function` over
  `FUNCTION_DEF`/`MACRO_DEF`: fifth `lower_block_body` reuse; the `SIGNATURE` is
  lowered recursively so the name/args/`::`return-type/`where` normalize and the
  keyword always gets one trailing space (anonymous `function(x)` → `function (x)`);
  the Runic-era return-insertion guard was dropped (Tenet 1: Fatou is layout-only,
  never inserts `return` and never inspects the tail), so **any** non-empty body
  reflows to the canonical body indent regardless of tail or source indentation;
  an empty body collapses to the canonical inline `function f() end` via
  `push_block_body` (Tenet 1); an unmodeled shape still bails to transparent;
  locked by `function_blocks/`),
  `do` blocks (`lower_do` over `DO_EXPR`: the call head sits *before* the `do`
  keyword and is lowered recursively, the optional `DO_PARAMS` arg list is
  `", "`-joined via `lower_do_params` (`do x,y` → `do x, y`, destructure `do (x, y)`
  normalized), and the body delegates to `lower_block_body`. `do` bodies are
  layout-only, never `return`-inserted, so there is no tail-return guard—any
  non-empty body reshapes; a `do` block is single-bodied, so an empty body folds
  inline against `end` via `push_block_body` (`foo() do end`, `map(xs) do x end`);
  locked by `do_blocks/`), n-ary binary spacing + continuation indent (`lower_binary`
  generalized from two operands to the full flat operand/operator alternation, so
  same-precedence chains `a+b+c+d` → `a + b + c + d` now normalize, plus a
  multi-line operator continuation: a `NEWLINE` in an operator gap becomes a
  trailing-operator break with the continuation indented one level
  (`y = a +⏎b` → `y = a +⏎    b`); nested binaries/assignments share the single
  level via a `binary_group_breaks` gate so `a = b =⏎c` and `a + b *⏎c + d` stay
  flat and a break buried in a non-group descendant (a broken call arg list)
  doesn't pull the expression in; locked by `binary_continuation/`),
  top-level (file) blank-line policy (`lower_root` over `ROOT`, replacing the
  transparent passthrough that leaked source blanks uncapped: interior blank runs
  between top-level items cap at `MAX_BLANK_LINES`=1, leading and trailing file
  blanks are stripped, and the file ends with exactly one newline — unlike a block
  body, whose keyword/`end` framing keeps one edge blank; reuses the extracted
  `collect_body_lines` (the statement/comment/`;`-vs-newline line model shared with
  `build_block_body`); top-level `;`-joined statements (the single
  `TOPLEVEL_SEMICOLON` child the parser folds `a; b; c` into) now reflow one
  statement per line — `collect_body_lines` flattens the wrapper via the extracted
  `collect_body_elements` recursion, so `a; b` and `a⏎b` format identically
  (Tenet 1); locked by `toplevel_semicolon/`; any unmodeled top-level shape bails
  the whole file to transparent; also locked by
  `toplevel_blank_lines/`, and unblocked `loop_blocks/` + `let_blocks/`).
  **Next:** multi-line ternary in a parenthesized branch (paren's own break engine
  drives the layout); long single-line bracket/matrix width-based reflow is a
  **non-goal**—probing shows Runic does **not** width-reflow (it is purely
  source-driven like Fatou); see the `formatter-parity` RECAP's ranked targets.
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
