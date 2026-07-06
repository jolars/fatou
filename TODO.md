# TODOs

The groundwork pass establishes the full architecture (parser pipeline, salsa
layer, formatter/linter/LSP skeletons, CLI, tooling, tests) over a deliberately
small Julia subset. This file tracks what comes next, roughly ordered by
leverage.

## Parser

- [ ] Parser: splat after a closing bracket is rejected. `f(g(x)...)`,
  `f(a[i]...)`, `f((a + b)...)`, `f(A{T}...)`, `f([1, 2]...)` yield a
  `LoneOperator` ERROR on the `...`; only the spaced spelling `f(g(x) ...)`
  parses, and a name/dotted/literal operand snugged (`f(x...)`) is fine.
  JuliaSyntax accepts all: `JuliaSyntax.parse(Expr, "f(g(x)...)")` ⇒ `f(g(x)...)`.
  Blocks the formatter's `lower_splat` from snugging bracket-closing operands (it
  bails to verbatim spaced); once fixed, drop the `ends_in_bracket` guard and
  widen `splat_spacing/`. (Handed off from formatter 2026-07-06c.)

- [x] Parser: multi-binding `let` wraps every binding as its own node. `let a =
  1, b = 2, c = 3` now makes each comma-separated binding its own
  `ASSIGNMENT_EXPR` (bare names stay `NAME`, destructuring stays a tuple) rather
  than leaving all but the first as flat `IDENT`/`EQ`/`INTEGER` tokens under
  `LET_BINDINGS` (`(let (block (= a 1) (= b 2) (= c 3)) (block x))`). `parse_header`'s
  general path now loops parsing each binding as an expr with the `,` separators
  kept loose; the projector iterates the binding nodes (no more flat-token
  compensation). Unblocks the formatter's width-driven let-binding-list reflow.
- [x] Lexer: compound-assignment operators `<<=`, `>>=`, `>>>=`, `÷=`, `⊻=` (and
  their broadcast forms `.<<=`, `.>>=`, `.>>>=`, `.÷=`, `.⊻=`) now tokenize as one
  augmented-assignment token. `a <<= b` ⇒ `(<<= a b)`, `a .÷= b` ⇒ `(.÷= a b)`;
  quotable as `:(<<=)`. The Unicode set is exactly `÷=`/`⊻=` (the only two Unicode
  operators with an augmented form; `⊕=`/`×=`/… are errors in Julia).
- [x] Lexer: the left-arrow operator `<--` (and broadcast `.<--`/`.<-->`) now
  tokenizes as one arrow-tier, right-associative operator
  (`LeftLongArrow`/`DotLeftLongArrow`/`DotLeftRightArrow`, tier `(4, 3)`).
  `a <-- b` ⇒ `(call-i a <-- b)`, `a .<-- b` ⇒ `(dotcall-i a <-- b)`,
  `a --> b <-- c` ⇒ `(--> a (call-i b <-- c))`; quotable as `:(<--)`/`:(.<--)`,
  prefix `<-- a` recovers as `(call-pre (error <--) a)`. Longest match: `<-->`
  beats `<--` beats `<` + `--`; `.<-->` beats `.<--` beats `.<`; a lone `<-`
  stays `<` + unary minus. The formatter's arrow tier (`binary_prec_class`)
  gained the three kinds.
- [x] Parser: newline-broken braces comprehension now parses as a
  `BRACES_COMPREHENSION`. `parse_braces` gained the two newline-lookahead arms
  `parse_bracket_literal` already had: a newline run before `for` is
  insignificant (`{a\nfor b in c}` ⇒ `(braces (generator a (= b c)))`, blank
  lines included), and a newline before `,` keeps the comma list (`{a\n, b}` ⇒
  `(braces a b)`); newline-before-element/`;` stays `BRACESCAT_EXPR`. Kills the
  latent stability violation where the formatter's exploded
  `BRACES_COMPREHENSION` failed to reparse; the formatter can now widen the
  braces case of `comprehension_index_break/`.
- [ ] Parser: whitespace before a call/index/curly arg list is wrongly accepted.
  `f (a)`, `a [1]`, `A {T}`, `f(a) (b)` parse as `CALL_EXPR`/`INDEX_EXPR`/`CURLY_EXPR`
  with an interior `WHITESPACE`; JuliaSyntax rejects with `whitespace is not allowed
  here`. Surfaced by the formatter; see parser-parity RECAP queued target.
- [x] Parser: newline-after-comma continuation. A trailing `,` (or `import`'s
  dangling `:`, or the `import`/`using` keyword itself) now suppresses the
  statement-terminating newline so the comma list continues on a later line.
  Three sites: (a) bare-tuple assignment `x = a,\nb,\nc` ⇒ `(= x (tuple a b c))`
  (`parse_comma_tuple` skips trivia for the item after a comma); (b) `let x = 1,\n
  y = 2` ⇒ both bindings in `LET_BINDINGS` (`parse_header` crosses a newline after
  a trailing comma in a let-binding list); (c) `import A:\n b,\n c` ⇒ one selective
  list (`parse_import_clause` skips trivia before the path). A newline *before* the
  next separator still terminates (`a\n,b`, `import A\n, B`). Surfaced by the
  formatter.
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

- [x] Formatter: string/command interpolation normalized + forced flat
  (`string_interpolation/`). A `STRING_LITERAL`/`CMD_LITERAL` now has a rule
  (`lower_string_literal`): content and delimiters stay verbatim, but each `$(…)`
  interpolation's expression is normalized and forced flat via `render_flat`
  (`$( y + z )` → `$(y + z)`, a source-broken interpolation collapses). Fixes the
  bug where an overflowing string let `lower_paren` break *inside* the literal.
  Unflattenable interiors (a comment/block) bail the whole literal to verbatim.
- [x] Formatter: bracescat `{a; b}` reflow (`bracescat_spacing/`). A
  `BRACESCAT_EXPR` (the brace-delimited vcat/matrix) is structurally identical to
  a `MATRIX_EXPR` — same `ARG`/`MATRIX_ROW` children, same `;`/newline/space
  separators, same `;;` higher-dim bail — so it now routes through the shared
  `lower_matrix` machinery: source spacing normalizes to `{a; b}`, a too-wide
  bracescat frames one element per line (`;` dropped, matrix style). The two
  matrix helpers that read the open/close tokens (`matrix_reflow_body`,
  `lower_matrix_multiline`) gained `LBRACE`/`RBRACE` arms; the bracket text flows
  through verbatim.
- [x] Formatter: bracescat subjects break subject-first under an index
  (`bracescat_index_break/`). A too-wide `{a; b}[k]` now yields subject-first —
  the bracescat frames one element per line and `}[k]` rides the closing brace —
  by registering `BRACESCAT_EXPR` in `construct_reflow_body` next to
  `MATRIX_EXPR` (both fold through the shared `matrix_reflow_body`). Chained
  indexes ride stacked closers; the `;;` higher-dim form bails (index arg list
  explodes) exactly as the matrix analog does.
- [x] Formatter: splat operator snugs to its operand (`splat_spacing/`). A
  `SPLAT_EXPR` written `x ...` now normalizes to `x...` (the postfix analog of the
  unary-prefix snug), reflowing whatever whitespace the parser left between operand
  and `...` (Tenet 1). New `lower_splat`; a `LITERAL` operand always snugs (floats
  normalize to a safe trailing digit), a verbatim shape ending in a raw `.` bails
  (avoids `....`). Bracket-closing operands (`g(x)...`) still bail to verbatim
  spaced because the parser can't yet reparse a splat after `)`/`]`/`}` (handed off
  to parser-parity); widen once that lands.
- [x] Hand-authored formatter fixture gate (see `AGENTS.md` and the `formatter`
  skill). Each fixture (`tests/fixtures/formatter/<slug>/`) holds an `input.jl`
  and a hand-written `expected.jl`; `tests/formatter.rs` gates
  `format(input) == expected.jl` (presence of `expected.jl` is gate membership —
  no allowlist) and, over every `input.jl`, checks idempotence + clean reparse.
  The Runic.jl differential oracle was removed (it preserved source line breaks,
  contradicting Tenet 1; `expected.jl` is now authored under full reflow).
- [x] Formatter: `using`/`import`/`export` lists reflow width-first
  (`import_list_break/`). A too-wide list now breaks one comma-group per line with
  the comma trailing and the wrapped groups indented one continuation step (the
  first group on the opening line after the keyword; the selector colon `Mod:` is
  not a break point); a source-broken list reflows to flat instead of mirroring
  the input breaks (Tenet 1). `lower_import_stmt`/`lower_export_stmt` now build a
  width-driven group instead of a flat concat.
- [x] Formatter: `let` binding lists reflow width-first (`let_binding_break/`). A
  too-wide multi-binding header (`let a = 1, b = 2, ...`) now breaks one binding
  per line, comma trailing each, wrapped bindings at one continuation indent
  (first on the `let ` line), instead of leaking a width break into a random inner
  arg list; a source-broken header reflows to flat when it fits (Tenet 1).
  `lower_bare_tuple` was generalized to `lower_comma_list`, shared by
  `BARE_TUPLE_EXPR` and `LET_BINDINGS` (both node-kind-agnostic); enabled by the
  parser fix that wraps every binding as its own node.
- [x] Formatter: bare tuples reflow width-first (`bare_tuple_break/`). A too-wide
  bracketless tuple (`x = a, b, c`, `return a, b, c`, a standalone `a, b, c`) now
  breaks one element per line with the comma trailing each and the wrapped
  elements indented one continuation step (first element on the opening line),
  instead of always rendering flat; a source-broken bare tuple reflows to the same
  form instead of bailing to verbatim (Tenet 1). `lower_bare_tuple` now builds a
  width-driven `Ir::group` (mirroring `lower_comparison`) and skips interior
  newlines. No broken-only trailing comma (no brackets to frame it).
- [x] Formatter: fluent method chains break at the dots (`method_chain_break/`).
  A too-wide `.`-spine with at least two *called* links (`recv.a(x).b(y)`) now
  reflows to the trailing-dot form — receiver on the opening line, each called
  link on its own continuation-indented line with the `.` trailing the line
  before it (the only broken spelling Julia reparses as the same chain) — instead
  of the tight flat access. Bare field accesses / module qualifiers never split
  (`obj.config.` glues; `Base.Foo.bar` stays flat even when it overflows); a
  single-call `recv.method(args)` breaks its argument list, not the dot. New
  `try_lower_chain`/`collect_chain`/`call_parts`/`dot_access_parts`/`lower_call`;
  a `CALL_EXPR` arm plus a guard at the top of `lower_binary` (field-terminated
  chains root at a `BINARY_EXPR`). Any comment/broadcast-dot/unmodeled shape bails
  transparent.
- [x] Formatter: chained pairs hug through the whole spine (`chained_pair_hug/`).
  A trailing chained pair `a => b => Dict(...)` now hugs its enclosing bracket
  like a single pair — the whole `a => b => ` joins the flat prefix and only the
  innermost construct explodes — instead of bailing to normal arrow-tier layout.
  `pair_hug_split` split into `pair_operands` (clean-pair parse) + the recursive
  `pair_hug_chain` (peels the right-nested spine to the innermost huggable
  construct); the four hug consumers route through it. `-->`/`<-->` links and a
  non-huggable innermost value still bail.
- [x] Formatter: paren-expression subjects join the shared index group
  (`paren_index_break/`). A too-wide `(inner)[index]` chain now yields
  subject-first at its own parentheses — the inner value on one indented line,
  the `)[index]` riding the closing paren (breaking further only if it still
  overflows) — exactly as a tuple subject already does. `lower_paren`'s body was
  extracted as `paren_reflow_body` and registered in `construct_reflow_body`, so
  a single-value `PAREN_EXPR` is the breakable unit (its own brackets), never the
  inner call/binary; the `;`-block `PAREN_BLOCK` and comma `TUPLE_EXPR` are
  distinct nodes and unaffected. Kills the stray-vector index explosion the
  transparent bail produced.
- [x] Formatter: same-operator chains break uniformly (`lower_binary`). The parser
  folds `+`/`*` into flat n-ary nodes but keeps `&&`/`||`/`|>`/`=>` nested, so a
  too-wide short-circuit or pipe chain used to break only at its outermost operator.
  New `collect_binary_chain`/`binary_op_kind` helpers flatten a same-operator nested
  chain into one group, so every operator breaks together (`a &&\n b &&\n c`).
  Mixed-operator or tighter subexprs keep a differing operator kind and stay their own
  group (`a && b ||\n c && d`). `chain_break/` gated.
- [x] Formatter: mixed same-precedence chains break uniformly (`lower_binary`). The
  parser left-nests `a + b - c` / `a * b / c` / `a << b >> c` (one operator per level),
  so a too-wide mixed-additive/multiplicative/shift chain used to break only at its
  outermost operator. New `same_break_tier`/`binary_prec_class` helpers flatten on the
  operator's precedence *tier*, not exact kind, so `+`/`-` (and `*`/`/`/`%`/`\`/`&`,
  `<<`/`>>`/`>>>`, and `|` with `+`) fold into one break group like a same-operator
  chain. Tighter/looser tiers stay their own group. `mixed_precedence_chain/` gated.
- [x] Formatter: mixed arrow/pair-tier chains break uniformly (`lower_binary`).
  `binary_prec_class` gained the arrow/pair tier (`=>`/`.=>`/`-->`/`.-->`/`<-->`,
  right-associative, the parser right-nests), so a too-wide mixed chain
  (`a => b --> c => d`) breaks at every operator instead of staircasing into
  nested indents — same treatment the plus/times/shift tiers already had. The
  flatten is layout-only (the text stream is identical under either
  association). `<--`/`.<--` (`LEFT_LONG_ARROW`/`DOT_LEFT_LONG_ARROW`) join the
  same tier now that the lexer gap is resolved; `arrow_pair_chain/` gated and
  widened to exercise them (same-op break, mixed `<--`/`.<--`/`=>` break, flat
  fit, source-broken reflow).
- [x] Formatter: pair-value hugging (`pair_hug/`). The pair operators `=>`/`.=>`
  are hug-transparent: a trailing `lhs => <bracket construct>` argument, keyword
  value, or collection element hugs the enclosing bracket — the `lhs => ` joins
  the flat prefix like a keyword's `name = `, and the value breaks in place
  (`Dict("k" => [\n    a,\n])`). New `pair_hug_split`/`value_is_huggable`/
  `hug_value_parts`/`pair_hug_grouped_parts`; the other arrow-tier operators
  (`-->`, `<-->`) and chained pairs (`a => b => c`) keep the normal explode.
- [x] Formatter: comprehension subjects join the shared index group
  (`comprehension_index_break/`). A too-wide indexed comprehension —
  plain `[…][i]`, generator `(…)[1]`, or typed `Float64[…][idx]` — now
  yields subject-first: the bracketed body explodes onto element/clause
  lines and the index rides the closing bracket, breaking at its own
  column only if it still overflows. `lower_comprehension`'s body was
  extracted as `comprehension_reflow_body` and registered in
  `construct_reflow_body` (plus `typed_comprehension_reflow_body`, the
  type joining flat like a callee), which also lets a hugged trailing
  comprehension carry an index-subject hug (`f(cfg, […])[k]`,
  `g(k => […])[k]`). Braces comprehensions (`{…for…}[k]`) now widen too:
  the parser gap is resolved, so the exploded braces form reparses and the
  fixture's braces case breaks like the rest.
- [x] Formatter: name-rooted index chains break subject-first
  (`name_index_break/`). A too-wide chain rooted at a plain or dotted name
  (`table[…][k]`, `config.table[…][k]`) now folds into `lower_index`'s shared
  group: the first arg list is the yielding body (hugs and `;`-tails included
  via the extracted `applied_args_body`, shared with `call_reflow_body`) and
  every later index rides the closing bracket, breaking at its own column only
  if it still overflows — exactly the gated call-subject rule with `[` for `(`.
  Kills the stray-vector middle explosion the transparent path produced. Paren
  and comprehension subjects still bail transparent.
- [x] Formatter: the `;`-keyword tail of a call now folds into `lower_arg_list`'s
  width-driven group instead of always emitting flat. A too-wide call breaks
  one-arg-per-line with the `;` snug after the last positional (`b;`) and each
  keyword on its own line + trailing comma; a keyword-only call keeps the `;` on the
  open bracket (`f(;`). New `collect_param_items` helper; unmodeled param shapes
  (comment, etc.) still fall back to the flat form. `arg_list_params_break/` gated.
- [x] Formatter: multi-statement paren blocks (`(a; b; c)`, `lower_paren_block`)
  are now width-driven. Flat when they fit; else one statement per indented line
  with the `;` snug after each but the last, brackets on their own lines. The token
  loops skip interior `NEWLINE`/`WHITESPACE`, so a source-multiline block reflows to
  the same canonical form (`(\na;\nb\n)` -> `(a; b)`). `paren_block_break/` gated.
- [x] Formatter: continuation-aware `fits` (printer engine). A group now breaks
  when its flat rendering *plus the trailing content on the same line* exceeds
  `line_width`, not just when its own contents do. `printer::fits` walks the group
  inner (flat) and then the rest of the print stack, stopping at the first break
  that will be taken; trailing nested groups keep their carried break mode, so an
  earlier small group (e.g. `f(x)`) stays flat while only the group that must break
  does. Fixes silent overflow of any construct with a trailing tail — e.g.
  `f(x) where {…} = x` now explodes the braces (`where_break/` gated).
- [x] Formatter: gated postfix tails on a breaking call (`postfix_tail_break/`).
  A wide call that explodes one-arg-per-line now carries a trailing `.field`,
  `::T` annotation, or `[index]` snug on its closing-bracket line (`).field`),
  the canonical postfix form. Enabled by the continuation-aware `fits`; no code
  change, pure `test(formatter)` gating of already-correct output.
- [x] Formatter: gated chained postfix tails on a breaking call
  (`chained_postfix_break/`). Multiple postfix ops ride the closing-bracket line
  (`).field.other`, `)[index_expr][second]`, `).method(z)`, `)[idx].field`).
  Enabled by the continuation-aware `fits`; no code change, pure
  `test(formatter)` gating.
- [x] Formatter: gated postfix tails on a breaking bracket group
  (`bracket_postfix_break/`). A wide collection/tuple (one-per-line) or matrix
  (one-row-per-line) rides `.field`, `::T`, or a chained `.field.other` on its
  closing-bracket line. Enabled by the continuation-aware `fits`; no code change,
  pure `test(formatter)` gating. The deferred `<wide-collection>[index]` case
  (index arg-list broke instead of the collection subject) was resolved by
  `collection_index_break/` — the subject yields first.
- [x] Formatter: argument hugging (`arg_hug/`). When the last positional argument
  of a call/index arg list is a bracket-delimited construct (call, index, curly,
  vector, tuple, braces, comprehension/generator, matrix), it hugs the enclosing
  bracket instead of exploding onto its own doubly-indented line: `f(g(\n …\n))`,
  `map(f, [\n …\n])`. Leading args render flat in the prefix. Implemented in
  `lower_arg_list` (`arg_is_huggable` + drop the wrapping group/outer trailing
  comma for a huggable last item); the continuation-aware `fits` glues the openers
  and stacks the closers, so no printer change. (The explode fallback landed next —
  see the following bullet.)
- [x] Formatter: hug explode fallback (`arg_hug_explode/`). When even the hug
  layout's first line — the open bracket, the flat leading args, and the hugged
  construct's opening bracket — overflows `line_width`, the call now falls back to
  the standard explode group (one item per line, broken-only trailing comma, the
  last item free to break further) instead of hugging with a too-long first line.
  New `Ir::HugGroup { prefix, body, close, explode }` primitive; the printer's
  `hug_fits` measures the hug first line by seeding the shared `fits_stack` loop
  with the body in Break mode (its first break opportunity ends the measured
  line), so no second measurement engine. In `fits`-trailing content a `HugGroup`
  walks its hug parts — byte-identical to the old bare concat, so no existing
  fixture moved. Nested hugs measure conservatively through the inner prefix to
  the innermost opener (user choice): overflow explodes the outer call, and the
  inner list re-decides at its printed column (it may hug there or explode too).
  Deferred: arity's `hug_excuse_overflow` (an overwide unbreakable leading atom
  should not force the explode, since breaking buys no width) — add inside
  `hug_fits` if a motivating fixture appears.
- [x] Formatter: collection-subject index break (`collection_index_break/`). When
  `collection[index]` overflows and both sides could break, the subject yields
  first: the new `lower_index` arm (an `INDEX_EXPR` whose subject is a
  tuple/vector/braces/matrix literal) folds the subject's reflow body and the
  index arg list into one outer group, so the whole postfix measures flat
  together; broken, the collection explodes one element (or matrix row) per line
  and the index rides the closing bracket, breaking at its own column only if it
  still overflows there. Extracted `collection_reflow_body`/`matrix_reflow_body`
  from `lower_collection`/`lower_matrix_reflow`. Other subjects (identifier,
  call, chained index, paren) keep the transparent path, where the index — the
  later group — breaks first; comment-bearing subjects or index lists bail.
  Deferred: the same subject-yields policy for call subjects (`f(args)[idx]` in
  the boundary window where `call(…)` + `[` fits but the total overflows) and
  for chained postfix chains (`[…][i][j]`). (Both landed next — see the
  following bullets.)
- [x] Formatter: call-subject index break (`call_index_break/`). Extended the
  subject-yields-first policy to call and curly subjects: `lower_index` now
  accepts an `INDEX_EXPR` whose subject is a `CALL_EXPR`/`CURLY_EXPR` of the
  clean `callee ARG_LIST` shape, folding the callee plus the arg list's ungrouped
  explode body (new `call_reflow_body`, backed by the extracted
  `collect_arg_list` parse helper and `arg_list_explode_body`) into the shared
  outer group. In the boundary window (`f(args)[` fits, total overflows) the
  call's args now explode and the index rides the closing paren, matching the
  collection-subject policy. Bails to the transparent path (index yields, as
  before) on a `;` keyword tail, a comment, an interleaved token, or a huggable
  last argument — a hug's break opportunities live in the hugged construct's own
  group, so subject-yields-first there needs printer work (deferred, with the
  params-tail boundary window).
- [x] Formatter: keyword-arg-value and collection-element hugging (`kwarg_hug/`,
  `collection_hug/`). Hugging now covers a trailing `KEYWORD_ARG` whose value is
  a bracket construct — in comma position (`f(x, kw = [` … `])`) and in the `;`
  keyword tail (`f(x; kw = g(` … `))`, keyword-only `f(; kw = [` included) — and
  the last element of a collection literal (`[a, b, f(` … `)]`, named tuples,
  braces; the one-tuple's semantic comma joins the stacked closers `),)`).
  `arg_is_huggable` generalized to `item_is_huggable` (+ `huggable_kind`);
  `collect_param_items` reports the last param's huggability; `lower_collection`
  gained the hug arm over the extracted `collect_collection_items`/
  `collection_body`; the explode fallback is shared via `bracket_explode_body`.
  The index-subject path (`collection_reflow_body`) bails on a huggable last
  element like `call_reflow_body` does — subject-yields-first through a hug
  still needs the printer merge (deferred, same bullet as the call bail).
- [x] Formatter: chained-index break (`chained_index_break/`). Extended the
  subject-yields-first policy through chained index expressions (`[…][i][j]`,
  `f(x)[i][j]`): `lower_index`'s subject dispatch moved into the recursive
  `index_reflow_body`, which accepts an `INDEX_EXPR` subject and folds the whole
  chain into one shared outer group. Broken, the innermost subject explodes and
  every index rides the closing bracket, breaking at its own column only if it
  still overflows there. Bails (whole chain transparent, index yields) propagate
  from the inner subjects: comments, `;` keyword tails, huggable last
  arguments/elements, name-rooted chains (no subject body to explode).
- [x] Formatter: subject-yields-first through hugs and `;` keyword tails
  (`hug_index_break/`, `params_index_break/`). The two remaining
  `call_reflow_body`/`collection_reflow_body` bails are gone: a huggable last
  argument/element/keyword-value becomes an **ungrouped** `Ir::HugGroup`
  (prefix, the hugged construct's own reflow body, close, ungrouped explode
  fallback) folded into `lower_index`'s shared outer group, and a `;` keyword
  tail folds in via the extracted `arg_list_params_body`. The owning group
  decides flat-vs-yield (flat when the whole chain fits; broken, the hugged
  body breaks in place and every index rides the stacked closers) while the
  printer's existing `hug_fits` keeps the hug-vs-explode tiering — no printer
  change. New helpers: `construct_reflow_body` (shared subject/hug-body
  dispatch, also used by `index_reflow_body`), `item_hug_parts`, `reflow_hug`,
  `last_list_item`, `params_hug_prefix`. Nested hugs recurse (closers stack
  `])]`); comprehension-valued hugs and name-rooted chains still bail.
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
  `comprehension_for_in/`), comprehension/generator reflow (`lower_comprehension`
  over `COMPREHENSION`/`GENERATOR`/`BRACES_COMPREHENSION`, with the typed `T[…]`
  `TYPED_COMPREHENSION` snugged via the transparent path): one width-driven
  `Ir::group` around the element plus each `FOR_BINDING`/`COMPREHENSION_IF` clause—
  flat `[elem for b if f]` with single spaces when it fits, else element and each
  `for`/`if` clause exploded onto its own indented line; padding stripped
  (`Int[ x for x in v ]` → `Int[x for x in v]`), the `if`-filter recursed via the
  new `lower_comprehension_if`. Source line breaks are ignored: `lower_for_binding`/
  `for_iteration_operands` skip `NEWLINE` like whitespace, so a pre-broken comprehension
  collapses or re-explodes to the same canonical form as its single-line twin (no
  `has_newline_token` guard). Bails to transparent only on a comment in the subtree;
  locked by `comprehension_spacing/` + `comprehension_break/` + `comprehension_multiline/`),
  `begin`/`quote` block-body indentation
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
  drives the layout). The four comment fixtures (`block_comments`,
  `block_comments_in_blocks`, `bracket_block_comments`, `trailing_comments`) are
  now hand-authored + gated (verified input-independent per Tenet 1), so **every**
  fixture is gated. Macro-call spacing (`lower_macro_call` over `MACRO_CALL`:
  `@test  x  ==  y` → `@test x == y`—the parser leaves the macro-name→arg
  whitespace verbatim, so the arm normalizes each space-separated gap to one space
  while preserving the semantic call-form vs space-form distinction (`@eval(expr)`,
  an attached `ARG_LIST`, stays snug; `@foo (a, b)`, a spaced `TUPLE_EXPR`, keeps
  its space); args recurse through `lower_node`; the space form never introduces a
  break; `lower_macro_name` flattens dotted names like `Base.@kwdef`; bails
  transparent on an interleaved comment/newline or unexpected token; locked by
  `macro_calls/`).
  Unary prefix operators (`lower_unary` over `UNARY_EXPR`: `-  a` → `-a`, the
  operator snugs to its operand, normalizing source whitespace; operand recurses
  through `lower_node`; bails to verbatim when the operand leads with a symbolic
  operator so `- -a` never retokenizes to `--a`; locked by `unary_operators/`).
  Long single-line bracket/matrix width-based reflow is a
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
