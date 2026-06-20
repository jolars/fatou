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
- [x] `macro` definitions (`macro m(ex) … end`). Structurally identical to a
  `function` definition — a call-shaped signature plus a body block — so `macro`
  is now a keyword token (`MacroKw`/`MACRO_KW`) and `parse_macro_def`
  (`structural.rs`) shares `parse_function_like` with `parse_function_expr`,
  differing only in the wrapper node kind (`MACRO_DEF`). The projector heads the
  node with `macro` (`sexpr.rs`). Signatures reuse the full expression path, so
  operator (`macro (:)(ex)`), contextual-ident (`macro (type)(ex)`), and
  interpolated (`macro $f()`, `macro ($f)()`) names all fall out for free.
  **Known limitation:** `macro f end` (no signature parens) projects to
  `(macro f (block))` rather than Julia's `(macro f)` — the same trailing-block
  divergence as `function f end`, an error-shape case left for the error phase.
- [x] `public` contextual keyword (`public A, B`, `public @a`). A statement-only
  reword: at toplevel and module-block scope, the identifier `public` opens a
  `PUBLIC_STMT` (parsed by `parse_keyword_stmt` with `KwStmt::Path`, reusing the
  `export` name-list machinery) *unless* the next significant token is `(`, `=`,
  or `[` — which keep `public` an ordinary identifier (`public(x)`, `public = 1`,
  `public[i]`), matching JuliaSyntax's `parse_public` compatibility shim. A new
  `public_context` flag on `ExprFlags` (set by `parse_stmt`, threaded through the
  toplevel loop and `run_module_block`, off in every other block) gates the
  detection so `public` stays an identifier inside `begin`/`if`/function bodies.
  The projector heads the node `public`, dropping the leading keyword token before
  reading the names via the shared `name_run_item`. **Deferred:** unicode operator
  names (`public ⤈` — needs unicode-operator lexing) and the `;`-separated
  toplevel `toplevel-;` grouping divergence.
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
  space before the op, `: +`, is an error and stays unhandled). Paren-quoted
  operators now quote too (`:(=)`, `:(::)`, `:(:)`, `:(+)`, `:(+=)` →
  `(quote-: …)`): a `parse_quote_sym` `LParen` arm recognizes `( op )` where the
  interior is a lone undotted operator (`is_paren_quotable_op`, which adds the
  syntactic `=`/`::`/`:` that are errors in value position) and builds a
  `PAREN_EXPR` wrapping the bare operator token; the projector reads a
  lone-operator paren (no inner node) as the operator's text. **Known
  limitations:** the bare-`:` Colon value (`a[:]` → `(ref a :)`), broadcast
  operator quotes (`:.+` → `(. +)`, `:(.=)` → `(quote-: (. =))`), standalone
  parenthesized operators (`(+)` → `+`), and import paren-quotes (`import A.:(+)`,
  `import A.(:+)`) are deferred (still divergences).
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
  parser's). Numeric juxtaposition / implicit multiplication
  (`2x`, `2π`, `1im`) is its own parser feature, landed separately (see
  "Numeric-literal juxtaposition" below).
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
  **Deferred (still divergences):** dotted `$interp` components (`import A.$B` —
  the root `import $A` now parses, see the dedicated bullet below) and the `. .A`
  (space-separated dots) form — each is carried through verbatim, keeping
  losslessness. Operator-symbol names and `@macro` paths now parse (see the
  dedicated bullets below).

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
  `ERROR`, awaiting unicode-operator lexing).

- [x] Macro names in `export`/`import`/`using`. A `@` in a directive name
  position now builds a real `MACRO_NAME` node instead of dropping the sigil: the
  shared `push_macro_name` helper (`structural.rs`) emits `MACRO_NAME` over the
  `@` plus an adjacent identifier (no args, no dotted chain — in these positions
  Julia treats a trailing `.mac` as a separate erroring component). It is wired
  into the `export` verbatim loop (`parse_keyword_stmt`, `export @a` →
  `(export @a)`, `export a, @b` → `(export a @b)`) and into `parse_import_path`
  in both the path-root arm (`import @x` → `(importpath @x)`, `import .@x` →
  `(importpath . @x)`) and the dotted-component loop (`import A.@x` →
  `(importpath A @x)`, `import A.B.@x`, `import A.@x.y` → `(importpath A @x y)`).
  The projector reads the new node via `project_macro_name` from `ident_run`
  (export) and `project_import_path` (import); both yield bare `@x`. With the
  `$`-root already parsing, `import $A.@x` → `(import (importpath ($ A) @x))`
  falls out for free. **Deferred:** `public @a` (`public` is not yet a contextual
  keyword) and standalone qualified macro paths as expressions (`A.B.@x`).

- [x] Import paren-quotes. `parse_import_path` (`structural.rs`) now accepts a
  parenthesized quoted operator/symbol as a dotted path component in two forms,
  both projecting to the same bare quote: `import A.:(+)` → `(importpath A
  (quote-: +))` (the `:` and its `(op)` are a `QUOTE_SYM` wrapping a `PAREN_EXPR`)
  and `import A.(:+)` → `(importpath A (quote-: +))` (a `PAREN_EXPR` wrapping a
  `QUOTE_SYM`). The `(Dot, Colon)` loop arm now delegates to the shared
  `parse_quote_sym` (made `pub(super)`), so `A.:foo`/`A.:(foo)` quote too; a new
  `(Dot, LParen)`-with-inner-`:` arm builds the paren-wrapped form. The projector
  gains a `PAREN_EXPR` arm in `project_import_path` that unwraps via `project`
  (the existing `PAREN_EXPR` → inner-node fallback yields the quote). Faithful:
  the parens stay real CST delimiters; the projector only unwraps them.
  **Deferred:** non-symbol paren contents (`import A.(a)` → `a`, no quote) and
  the erroring multi-token form (`import A.:(a+b)`).

- [x] Type-operator paren-calls. The type operators `<:`/`>:` glued to a `(` now
  follow the same `is_paren_call` heuristic as the unary operators: `<:(a, b)` →
  `(<: a b)`, `<:(a,)` → `(<: a)`, `>:(a, b)` → `(>: a b)`, `<:(a...)` →
  `(<: (... a))`, `<:()` → `(<:)`, while a lone bare operand stays a prefix
  application (`<:(a)` → `(<:-pre a)`). `Subtype`/`Supertype` were added to the
  unary paren-call arm of `parse_prefix` (`expr.rs`), building the same
  `CALL_EXPR` (operator-token callee + `ARG_LIST`). The projector's `project_call`
  (`sexpr.rs`) gains a `SUBTYPE`/`SUPERTYPE`-callee arm: these are syntactic type
  operators, so JuliaSyntax heads the node with the operator itself (`(<: …)`)
  rather than wrapping it in a `call` — mirroring how the binary `<:` projects via
  `infix_head`. **Deferred:** the `<:(a; b)` block-vs-tuple operand shape (a
  pre-existing paren-parsing divergence shared by all operators).

- [x] Operator-as-call functions. A non-unary binary operator glued to a `(`
  (`*(x)`, `==(a, b)`, broadcast `.*(a, b)`, `.==(a, b)`, `=>(x, y)`, `*()`) names
  a function call with the operator as the callee: `parse_prefix` (`expr.rs`) gains
  an arm gated by `is_operator_call_name` (the non-unary, non-syntactic operators —
  excludes `+`/`-`/`!`/`~`, `&`, `:`, `::`, `&&`/`||`, `->`, `<:`/`>:`) that, on an
  adjacent `(`, builds a `CALL_EXPR` whose first child is the bare operator token
  plus the usual `ARG_LIST`. The projector's `project_call` now reads the callee
  from the first *significant* element, so an operator-token callee projects via
  `operator_func_repr` (`(. *)` for broadcast, the bare text otherwise) →
  `(call * x)` / `(call (. *) x)`. Unary operators keep their prefix-application
  parse (`+(x)` → `(call-pre + x)`).

- [x] Curly operator calls. An operator glued to `{` is a parametric callee:
  `+{T}` → `(curly + T)`, `*{T}(x)` → `(call (curly * T) x)`, `<:{T}(x::T)` →
  `(call (curly <: T) (::-i x T))`, broadcast `.+{T}(x)` → `(call (curly (. +) T)
  x)`. `parse_prefix` (`expr.rs`) gains a top arm gated by `is_curly_operator_name`
  (the `is_operator_call_name` set plus the unary `+ - .+ .- ! ~ .~ <: >:`):
  glued to `{`, it returns the operator as a bare leaf token, and the postfix
  chain builds the `CURLY_EXPR` (and any trailing call) exactly as for an
  identifier callee. `::`, `&`, and `:` are excluded (Julia keeps them prefixes
  over the braces). The projector's `project_call` gates its `<:`/`>:` head
  override on `head == "call"`, so in a `curly` callee the operator is an ordinary
  part. **Deferred:** `&{T}` (`(& (braces T))` — a pre-existing `&`-prefix gap)
  and the error-shape syntactic callees (`&&{T}`, `->{T}`).

- [x] Field-access suffixes. A `()`/`[]`/`{}`/`.field` suffix now binds to the
  whole field access, not just the field name: `A.f()` → `(call (. A (quote f)))`,
  `a.b[i]` → `(ref (. a (quote b)) i)`, `a.b{T}` → `(curly (. a (quote b)) T)`,
  `a.b.c()`, `f(a).g(b)`, and the qualified function definition `function A.f()
  end` → `(function (call (. A (quote f))) (block))`. The field-access `.` stays in
  the infix loop (still a `BINARY_EXPR`), but its right operand is now parsed
  *prefix-only* (`parse_prefix`, the field name is an atom) instead of a full
  postfix-chained expression — so the outer postfix chain attaches any trailing
  suffix. Projector (`sexpr.rs`): a quoted field name (`a.:b`) routes its
  `QUOTE_SYM` rhs through `project` → `(. a (quote-: b))` instead of the empty
  `name_text`. CST shape unchanged for plain `a.b`.

- [x] Unary operator paren-calls. A unary arithmetic/logical operator
  (`+ - ! ~` and broadcast `.+ .- .~`) glued to a `(` is a function call when the
  parens look like an argument list: `+(a...)` → `(call + (... a))`, `+(x, y)` →
  `(call + x y)`, `+(a; b, c)` → `(call + a (parameters b c))`, `+()` → `(call +)`,
  `+(; a)` → `(call + (parameters a))`. A lone bare operand stays a prefix
  application (`+(x)` → `(call-pre + x)`), and a non-leading-`;` block (`+(a; b)`)
  too. Mirrors JuliaSyntax's `is_paren_call`: the new `unary_op_paren_is_call`
  (`expr.rs`) scans the adjacent parens and reports a call when they are empty,
  open with a leading `;`, or contain a top-level comma or splat. The unary arm of
  `parse_prefix` then builds a `CALL_EXPR` (operator-token callee + `ARG_LIST`,
  reusing the operator-as-call projection); `operator_func_repr` (`sexpr.rs`) gains
  a `!` case (`!` is unary-only, no `infix_head` entry). **Deferred:** the rare
  `+(;;)` double-semi block edge.

- [x] Prefix `$` interpolation in expression position. A prefix `$` is now an
  interpolation everywhere (Julia rejects it outside a quote only during
  lowering, not at parse time): `$x` → `($ x)`, `$(x + y)` → `($ (call-i x + y))`,
  and the field-access right-hand side `f.$x` → `(. f (inert ($ x)))`. The new
  `parse_prefix_interpolation` (`expr.rs`) reuses the string-context
  `parse_interpolation` for `$ident`/`$(expr)` and otherwise binds `$` to the
  next *prefix atom* — tightly, no postfix — so `$$a` → `($ ($ a))`, `$[1, 2]` →
  `($ (vect 1 2))`, and `$a.b` → `(. ($ a) …)`. Projector (`sexpr.rs`): a
  standalone `INTERPOLATION` projects to `($ …)` (string interiors keep the inner
  value via `string_parts`), and the field-access `Dot` arm inert-quotes an
  interpolated field name. **Deferred:** dotted-`$` macro paths (`A.$B.@x`),
  `A.:.+`.

- [x] `$`-interpolated names in `export`/`module`/`import` name positions:
  `module $A end` → `(module ($ A) (block))`, `import $A` →
  `(import (importpath ($ A)))`, `export $a, $(a*b)` →
  `(export ($ a) ($ (call-i a * b)))`, `export ($f)` → `(export ($ f))`. Each
  name-position parser now recognizes a leading `$` and builds a real
  `INTERPOLATION` node via the shared `parse_prefix_interpolation`: `parse_header`
  (module), `parse_import_path` (import root), and the `parse_keyword_stmt` Path
  passthrough (export list). Projector reads them through `project` — `ident_run`
  and `project_import_path` gained an `INTERPOLATION` arm; module's
  `project_signature` already handled it. **Deferred:** `import $A.@x` (needs
  macro-in-importpath support, which plain `import A.@x` also lacks), and
  `function $f end` (empty-body signature shape).

- [x] Standalone parenthesized operators: `(+)` → `+`, `(:)` → `:`, `(<:)` →
  `<:`, with postfix application a call callee (`(+)(a, b)` → `(call + a b)`,
  `function (:)() end` → `(function (call :) (block))`). `parse_paren` gains a
  lone-operator arm gated by `is_paren_value_op` (the non-syntactic subset:
  `is_op_name` minus `&&`/`||`/`->` plus `:`), building a `PAREN_EXPR` wrapping
  the bare operator token; the projector already reads a lone-operator paren as
  the operator's text. Whitespace-insensitive (`( + )` is the same).
  **Deferred:** broadcast forms (`(.+)` → `(. +)`) and the erroring syntactic
  ops (`(=)`, `(::)`, `(&&)`, `(->)`, `(?)`, `(...)` — error-shape).
  Parenthesized-operator macro names (`macro (:)(ex) end`) now parse via the
  `macro` definition bullet above.
- [x] Anonymous `function (args) … end` signatures as argument tuples. Julia
  models a parenthesized `function` signature as a tuple of arguments, not a
  parenthesized value: `function (x) end` → `(function (tuple-p x) (block))`.
  Multi-element and `;`-parameter forms already parsed as `TUPLE_EXPR`; the lone
  `(x)` form parsed as `PAREN_EXPR` (→ stripped `x`). `parse_function_like`
  (`structural.rs`) now relabels a whole-signature `PAREN_EXPR`'s `Start` event
  to `TUPLE_EXPR` — but only when the parenthesized expression is *not*
  "eventually a call" (`signature_eventually_call`, a faithful event-walking
  mirror of JuliaSyntax's `was_eventually_call`: peel `where`/`parens`/infix-`::`
  off the front and check for a call). So `function (x::T) end`, `(a.b.c)`,
  `(x && y)`, `(x .+ y)`, `(x -> y)` become `tuple-p` (anonymous), while
  `function (x*y) end`, `(f()::S)`, `(f() where T)` keep their parens stripped
  (named methods). The decision is gated to `FUNCTION_DEF`; `macro` keeps its
  call signature. **Deferred:** `function (x)::T end` (the `(x)` is a `tuple-p`
  nested under `::-i`, needs descending into the signature, not just the
  outermost paren).

- [x] ASCII bitwise operators `&` and `|`. Both were lexed but dropped (no
  binding power, no prefix arm). Infix `&` shares the `*` (times) precedence
  family `(24, 25)` and `|` shares the `+` (plus) family `(20, 21)`, both
  left-associative (`a + b & c` → `(a + (b & c))`, `a & b | c` →
  `((a & b) | c)`); added to `infix_binding_power`. Prefix `&x` (address-of) is a
  syntactic prefix that heads the node with `&` itself, not the generic
  `call-pre`: `Amp` joined the unary `parse_prefix` arm (→ `UNARY_EXPR`, same
  `PREFIX_BP` machinery as `-x`), with a new `AMP => (& operand)` arm in
  `project_unary`. So `&x` → `(& x)`, `&{T}` → `(& (braces T))`, `&a.b` →
  `(& (. a (quote b)))`, `&(x, y)` → `(& (tuple-p x y))` (prefix over a tuple, not
  a paren-call — `Amp` is excluded from the unary paren-call set). The `infix_head`
  and `is_operator` arms for `AMP`/`PIPE` already existed, so the projector was
  otherwise untouched. **Deferred:** broadcast `.&`/`.|` (`.&(x,y)`, `:.&&` —
  need broadcast-`&` lexing) and the unicode bitwise `⊻` (unicode-operator
  lexing).

- [x] `abstract type`/`primitive type` declarations. `abstract`, `primitive`,
  and `type` are contextual keywords (ordinary identifiers elsewhere), so they
  stay `Ident` in the lexer; `type_decl_keyword` (`expr.rs`) detects an
  `abstract`/`primitive` immediately followed by `type` and dispatches before the
  block-keyword match. `parse_abstract_type`/`parse_primitive_type`
  (`structural.rs`) emit the two keyword idents as bare leaf tokens, parse the
  type spec as a real expression into a `SIGNATURE` (so `<:`/`<`/`curly`/`where`
  all fall out: `(abstract (<: A (curly B T S)))`, `(abstract (call-i A < B))`),
  and — for `primitive` — parse the bit size as a sibling expression node
  (`(primitive (<: A B) 8)`). No block body: trivia, newlines, and a trailing `;`
  before `end` are insignificant (`skip_trivia_and_semis`). New `ABSTRACT_DEF`/
  `PRIMITIVE_DEF` kinds project via `(abstract <spec>)` and
  `project_primitive` → `(primitive <spec> <bits>)`.

- [x] Broadcast bitwise operators `.&` and `.|`. Lexed as `DotAmp`/`DotPipe`
  (lone `&`/`|` after a `.`, in the 2-char dotted table — `.&&`/`.||`/`.|>`
  already win in the 3-char table). Mirror the undotted tiers: `.&` shares the
  `*` (times) family `(24, 25)`, `.|` the `+` (plus) family `(20, 21)`, both
  left-associative (`a .+ b .& c` → `(dotcall-i a + (dotcall-i b & c))`). Infix
  projects via new `DOT_AMP => DotCallI("&")`/`DOT_PIPE => DotCallI("|")`
  `infix_head` arms → `(dotcall-i a & b)`. Glued to a `(`, both are operator-call
  names (unlike undotted `&`, which stays a prefix): `.&(x, y)` →
  `(call (. &) x y)`, `.|(x, y)` → `(call (. |) x y)`. **Deferred:** standalone
  `.&` → `(. &)` and the broadcast quote `:.&&` → `(quote-: (. &&))` (the same
  broadcast-standalone/broadcast-quote gaps that also affect `.+`/`:.+`).

- [x] Non-standard identifiers `var"…"`. A `var` prefix glued to a single-quoted
  string is a non-standard *identifier*, not a string macro: `var"x"` → `(var x)`,
  `var""` → `(var)`, `var"#"` → `(var #)`. Detected in `parse_string_literal`
  (`expr.rs`) — prefix text `var` + single-`"` open delimiter → a new
  `NONSTANDARD_IDENTIFIER` node (triple-quoted `var"""…"""` stays an ordinary
  `@var_str` macrocall, and other prefixes `r`/`raw`/`b` are unaffected). Projector
  `project_var` heads the node `var` over the raw delimited content. **Deferred:**
  escape-processing of the name (`var"\""` → `(var ")` follows Julia's raw-string
  rules, so escape-free names match but escaped ones stay FAIL) and the
  suffix-error shape (`var"x"y` → `(var x (error-t))`).
- [x] Unicode operators (single-codepoint infix/prefix). The full set of length-1
  non-ASCII operators from JuliaSyntax's kind tables is generated into
  `src/parser/unicode_ops.rs` (a code-point-sorted binary-search table mapping
  each operator to its precedence tier), classified by `is_prec_*`. The lexer's
  operator fallback looks the char up and emits one of eight tier `TokKind`s
  (`UniArrow`/`UniComparison`/`UniColon`/`UniPlus`/`UniTimes`/`UniPower`
  → `UNICODE_OP`, `UniAssign` → `UNICODE_ASSIGN_OP`, `UniRadical` →
  `UNICODE_RADICAL`); the six `call-i` tiers share one `SyntaxKind`. Binding
  powers mirror the ASCII siblings (arrow `(4,3)` right-assoc, assignment `(2,1)`
  right-assoc, comparison `(10,11)`, colon `(14,15)`, plus `(20,21)`, times
  `(24,25)`, power `(32,31)` right-assoc). Radicals `√ ∛ ∜` and `¬` are prefix-only,
  routed through the existing unary arm → `(call-pre √ x)`. The projector reads the
  operator text from the token (`x → y` → `(call-i x → y)`, `a ≔ b` → `(≔ a b)`).
  **Deferred:** unicode in `export`/`public`/`import` positions, broadcast unicode
  (`.…`), unicode comparison chains (nested, like the ASCII chain divergence), and
  unicode unary in the plus/times tiers (`±x`). (Juxtaposition and operator-suffix
  sub/superscripts both landed separately — see those bullets.)

- [x] Numeric-literal juxtaposition (implicit multiplication). An adjacent value
  with no operator between is parsed as a `JUXTAPOSE_EXPR` → `(juxtapose a b)`:
  `2x`, `2(x)`, `1√x`, `(x-1)y`, `f(x)y`, `[1,2]x`, `2im`, `x'y`. The operator
  loop (`parse_expr_in`) checks `should_juxtapose` after the postfix chain —
  faithful to JuliaSyntax's `is_juxtapose`: the term must be glued (no preceding
  whitespace/newline), not an operator (radicals `√`/`¬` pass, they are not
  `is_operator`), not a closing/keyword/`@` token; a numeric-literal coefficient
  juxtaposes with any such value, while a non-numeric closed value (`lhs_value_close`:
  paren/call/index/curly/vect/matrix/transpose) juxtaposes only with a non-numeric
  term. Binding powers `(JUXTAPOSE_L=32, JUXTAPOSE_R=31)` make it tighter than `*`/`//`
  but looser than `^`, matching `2x^2` ⇒ `(juxtapose 2 (x^2))` and `2^2x` ⇒ `2^(2x)`.
  `parse_postfix_chain` gains a guard so a `(` glued to a number is multiplication,
  not a call (`2(x)` ⇒ `(juxtapose 2 x)`, while `2[1]` stays `(ref 2 1)`). The
  projector heads the node `juxtapose` over its children. **Deferred:** n-ary
  flattening (`(2)(3)x` nests right, like associative `*`, a recorded divergence)
  and string-literal juxtaposition (`"a"x`, error recovery).
- [x] Operator suffix sub/superscripts. An operator token may absorb a trailing
  run of sub/superscript or prime characters (`a +₁ b`, `x -->₁ y`, `f'ᵀ`,
  `a .+₁ b`): the lexer's new `push_op` consumes `is_op_suffix_char` runs after
  any operator whose kind `op_takes_suffix` (mirroring JuliaSyntax's
  `optakessuffix` — assignments, `: :: .. ... ! ~ -> ? $ && || <: >:` and the
  radicals are excluded). The token *kind* is unchanged (so binding power is
  untouched); only the text grows, and the projector reads it. `project_binary`
  emits a suffixed operator as a generic `(call-i …)`/`(dotcall-i …)` with the
  full text even when the base operator is syntactic (`-->₁` ⇒ `(call-i x -->₁ y)`,
  not `(--> …)`), matching JuliaSyntax, where a suffix makes the operator
  non-syntactic. The explicit suffix-char set is handled; the combining-mark
  categories (Mn/Mc/Me) `optakessuffix` also accepts are a deferred pragmatic
  subset. Also corrected the whitespace-sensitive array-element split
  (`array_element_boundary`) to fire only for genuinely unary-capable operators
  (`+ - & ~`, broadcast `.+ .- .~`, and the symbol-quote `:`) and never for a
  suffixed operator: `[a *b]`/`[a ::b]` are now one element (`(vect …)`) and
  `[x +₁y]` stays `(vect (call-i x +₁ y))`, while `[a +b]`/`[1 :a]` still split.

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
  (`a*b*c`) and unicode operators (lexer).
  **Follow-ups:** work the backlog up the allowlist;
  design error-shape parity to promote the blocked recovery cases; wire the
  oracle gates into CI.
- [ ] Benchmarks (`criterion`) for parse + incremental reparse.
- [ ] `smol_str` interning for symbol names once the semantic model lands.
