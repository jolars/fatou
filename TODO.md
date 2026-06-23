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

- [x] Typed error-node taxonomy (error-shape parity, Phase 0 + first slice). The
  parser now emits in-tree typed error nodes the projector renders to
  JuliaSyntax's shape, so error cases compare like any other instead of being
  skipped. New `SyntaxKind::ERROR_TRIVIA` (projected `(error-t)`, the
  `TRIVIA_FLAG` truncation marker) sits before the `ERROR` sentinel (bare
  `(error)`); `project_error` wraps any recovered tokens. The oracle harness
  `render()` is now total (no longer skips on diagnostics; `Unsupported` keyed on
  the `(unsupported …)` sentinel), and the harvest filter no longer drops
  `(error …)` cases — the JS corpus grew 575 → 685 (the +110 error cases are the
  visible backlog). First slice: an unterminated arg-list/bracket-literal
  (`parse_arg_list` EOF arm) appends a zero-width `ERROR_TRIVIA` (`[x` ⇒
  `(vect x (error-t))`, `var"x"(` ⇒ `(call (var x) (error-t))`, `f(a` ⇒
  `(call f a (error-t))`). Fixture `unclosed_delimiter`. JS allow 553 → 555; dir
  114 → 116. **Deferred** (ranked next slices):
  incomplete-`do` `(error)`, the lexer-classified named kinds
  (`'ab'`⇒`ErrorOverLongCharacter`, `a--b`⇒`ErrorInvalidOperator`, bad
  escape/numeric). `end_index` also needs bare-`end` rejection (a grammar
  change), so it stays blocked.
- [x] Unterminated-string `(error-t)` (error-shape slice). A string/command/
  `var"…"` literal with no closing delimiter appends a zero-width `ERROR_TRIVIA`
  inside its body (`parse_string_literal`'s unterminated arm): `"str` ⇒
  `(string "str" (error-t))`, `` `cmd `` ⇒ `(cmdstring-r "cmd" (error-t))`,
  `r"pat` ⇒ `(string-r "pat" (error-t))`, `var"x` ⇒ `(var x (error-t))`. The
  projector's `with_error_trivia` appends `(error-t)` to each literal body,
  dropping the empty-`""` content placeholder Julia omits for an unterminated
  literal. Also fixed a lexer divergence: single-quoted `"…"` strings now span
  literal newlines like Julia (`"a\nb"` ⇒ `(string "a\nb")`), instead of
  stopping content at the newline — an unterminated string consumes to EOF.
  Fixtures `unterminated_string`, `unterminated_command`. JS allow 555 → 556; dir
  116 → 118.
- [x] `var"…"` glued-suffix `(error-t)` (error-shape slice). A `var"…"`
  non-standard identifier takes no flags, so a glued suffix — a flag-like alpha
  run (lexed `StringSuffix`) or a digit-led numeric literal — is junk:
  `parse_string_literal`'s close-delim arm consumes it as a sibling token and
  appends a zero-width `ERROR_TRIVIA`, so `var"x"y`/`var"x"1`/`var"x"end` ⇒
  `(var x (error-t))` (`project_var` ignores the sibling token; `with_error_trivia`
  emits the marker). A glued postfix opener (`[ ( { ' .`) or operator stays a
  chain/bind in the outer parser. Fixture `var_identifier_suffix`. JS allow
  556 → 559; dir 118 → 119. **Deferred** (different shapes): operator suffix
  `var"x"+` ⇒ `(call-i (var x) + (error))`, close-delim/`@macro`/whitespace
  suffixes ⇒ separate-toplevel `(error-t …)`.
- [x] Whitespace-before-postfix-opener `(error-t)` (error-shape slice). A
  `(`/`[`/`{` chained after a value with disallowed whitespace keeps the
  call/index/curly/dotcall shape but splices a zero-width `ERROR_TRIVIA` before
  the args: `f (a)` ⇒ `(call f (error-t) a)`, `a [i]` ⇒ `(ref a (error-t) i)`,
  `S {a}` ⇒ `(curly S (error-t) a)`, `f. (x)` ⇒ `(dotcall f (error-t) x)`.
  `parse_postfix` (and the inline `DOT_CALL_EXPR` arm) inserts the marker when
  `open_idx > lhs.end`; `project_call` emits a direct-child `(error-t)` between
  callee and args (distinct from the unterminated-arglist marker, which lives
  *inside* `ARG_LIST`). Array-mode space-split is untouched (no error-t — it's a
  real `hcat`). Fixture `postfix_space_error`. JS allow 559 → 564 (also unblocked
  `outer (x,y) = rhs`); dir 119 → 120.
- [x] Field-access/colon-quote space `(error-t)` (error-shape slice). Whitespace
  before a field-access dot, or between a `:` and the quoted symbol, is
  disallowed: JuliaSyntax keeps the shape but splices a zero-width `ERROR_TRIVIA`.
  `x .y` ⇒ `(. x (error-t) (quote y))`, `x .:y` ⇒ `(. x (error-t) (quote-: y))`
  (operator loop's `Dot` arm builds via `build_binary_dot_error` when
  `op_idx > lhs.end`; a broadcast `.+` lexes as one token so `a .+ b` is
  untouched). `: foo`/`:\nfoo` ⇒ `(quote-: (error-t) foo)`, `A.: +` ⇒
  `(. A (quote-: (error-t) +))` (`parse_quote_sym` splices the marker when
  `next > start + 1`). Both compose: `A .: foo` ⇒
  `(. A (error-t) (quote-: (error-t) foo))`. `project_binary` filters the
  `ERROR_TRIVIA` out of the operands and prefixes the field; `project_quote_sym`
  prefixes the symbol. Fixture `field_access_space`. JS allow 564 → 568; dir
  120 → 121.
- [x] Separate-toplevel trailing-junk `(error-t)` (error-shape slice). On a
  separator-less logical line, a complete statement followed by more non-trivia
  content wraps the leftover run in one `(error-t …)` sibling: `x y` ⇒
  `x (error-t y)`, `f(2)2` ⇒ `(call f 2) (error-t 2)`, `x' y` ⇒
  `(call-post x ') (error-t y)`, `var"x" y` ⇒ `(var x) (error-t y)`, `a b c` ⇒
  `a (error-t b c)`. The `parse` driver (`core.rs`) records the event offset
  right after a line's first statement (`leftover_mark`) and, when no `;` is
  present and significant content follows, opens an `ERROR_TRIVIA` over the
  recovered run (leading trivia stays outside). A bare docstring opener
  (`stmt_is_doc_string`) is exempt so `fold_docstrings` still owns `"a"\nfoo`.
  Fixture `toplevel_leftover_error`. JS allow 568 → 571; dir 121 → 122.
  **Deferred** (different shapes): `;`-line leftover (`a b; c`).
- [x] String-juxtapose-error `(error-t)` (error-shape slice). A string literal
  glued (no whitespace) to another term is an invalid juxtaposition JuliaSyntax
  recovers as `(juxtapose lhs (error-t) rhs)`: `"a"x` ⇒
  `(juxtapose (string "a") (error-t) x)`, `"a""b"`, `"a"begin end`, `"$y"x`, and
  the term-glued-to-string form `2"a"` ⇒ `(juxtapose 2 (error-t) (string "a"))`.
  The Pratt loop (`expr.rs`) checks `should_juxtapose_string_error` before the
  numeric juxtaposition: it fires when the left operand is a plain (non-prefixed)
  string literal and the glued term is any non-number value, or the glued term is
  itself a string literal after a number/closed value; `build_string_juxtapose_error`
  splices a zero-width `ERROR_TRIVIA` between the operands. Operators, `@`, numbers
  (`"a"2` stays a docstring), and closing tokens (`"a"end`) break it. Projector
  untouched (the `juxtapose` arm projects the `(error-t)` child directly). Fixture
  `string_juxtapose_error`. JS allow 571 → 575; dir 122 → 123.
- [x] Paren-block juxtapose-error `(error-t)` (error-shape slice). A
  parenthesized block form (`(begin end)`) glued to a value does *not* juxtapose
  (unlike a paren-wrapped ordinary value `(a)x` ⇒ `(juxtapose a x)`): the trailing
  term is leftover junk the toplevel driver wraps, `(begin end)x` ⇒
  `(block) (error-t x)`, `(if c end)y` ⇒ `(if c (block)) (error-t y)`. New
  `lhs_is_paren_block` (`expr.rs`) — a `PAREN_EXPR` whose first inner node is a
  block-keyword form (`is_block_form_kind`: begin/if/let/quote/struct/…) — guards
  both `should_juxtapose` and `should_juxtapose_string_error`, mirroring the bare
  block form's `lhs_is_block_keyword` suppression. Postfix/infix still apply
  (`(begin end).x`, `(begin end)+1`, `(begin end)(x)`). Projector untouched.
  Fixture `paren_block_juxtapose_error`. JS allow 575 → 576; dir 123 → 124.
- [x] Stray-closing-delimiter `✘` leftover (error-shape slice). A leftover
  *closing* delimiter recovered at toplevel is JuliaSyntax's `✘` error-token
  glyph: `var"x")` ⇒ `(var x) (error-t ✘)`, `&)` ⇒ `& (error-t ✘)`, `a)`/`1)`/
  `x]`/`f(x))` ⇒ `… (error-t ✘)`. Pure projector change (`sexpr.rs`): Fatou
  already wraps the stray `)`/`]`/`}` in `ERROR_TRIVIA`, but `project_error`
  dropped the delimiter token via `significant`; it now walks
  `children_with_tokens` and renders a close-delimiter token (`is_close_delimiter`)
  as `✘` while still dropping trivia/structure. Fixture
  `stray_close_delimiter_error`. JS allow 576 → 581; dir 124 → 125. **Deferred**
  (different parser shapes — stray delim not yet wrapped): `)` ⇒ `(error)
  (error-t ✘)` (lone closer needs a synthesized `(error)`), `(begin end)"x"` ⇒
  `(block) (error-t ✘ "x" ✘)`.
- [x] Bare `:` colon value atom. A prefix `:` not followed by something quotable
  is the Colon value atom, not a quote: `parse_quote_sym` returns `None` and
  `parse_prefix` now falls through to an `OPERATOR_ATOM` (`a[:]` ⇒ `(ref a :)`,
  `[:]` ⇒ `(vect :)`, `a[:, :]` ⇒ `(ref a : :)`, `f(:)` ⇒ `(call f :)`, lone `:`
  ⇒ `:`). Previously the bare `:` token was dropped by the projector's delimiter
  filter, so these all silently lost the colon. This also unblocks the
  stray-close case `:)` ⇒ `(toplevel : (error-t ✘))`: the colon now sets the
  leftover mark, so the toplevel driver wraps the trailing `)` as `(error-t ✘)`.
  Pure `expr.rs` change (one `.or_else`). Fixtures `colon_value_atom`,
  `colon_stray_close`. JS allow 581 → 582; dir 125 → 127.
- [x] Optional-value-keyword stray-closer `✘` (error-shape slice). `return`
  followed by a stray closing delimiter ends the empty form there, leaving the
  delimiter for the toplevel-leftover driver to wrap, matching `break)`:
  `return)` ⇒ `(return) (error-t ✘)`, `return ]`/`return}`, `return) x` ⇒
  `(return) (error-t ✘ x)`. Previously `return`'s `ExprTuple` operand parse
  carried the `)` verbatim *into* `RETURN_EXPR`. New `optional_value` flag on
  `parse_keyword_stmt` (`structural.rs`): when set and the operand position is a
  close delimiter (`is_close_delimiter_tok`), the node finishes right after the
  keyword. Only `return` passes `true`; value-required `const`/`global`/`local`
  keep their loose shape (they need a separate inner-`(error)` synthesis).
  Projector untouched. Fixture `return_stray_close`. JS allow 582 → 583; dir
  127 → 128. **Deferred**: lone closer `)` ⇒ `(error) (error-t ✘)` (synthesized
  leading `(error)`; swallows the rest of the line, `) x` ⇒ `(error)
  (error-t ✘ x)`; the `;`-segment forms emit a subtle `✘ ✘` double-marker).
- [x] Lone-closer leading-`(error)` (error-shape slice). A stray *closing*
  delimiter at statement start (no preceding statement) is JuliaSyntax's
  synthesized empty `(error)` plus an `(error-t ✘ …)` that swallows the rest of
  the line: `)` ⇒ `(error) (error-t ✘)`, `) x` ⇒ `(error) (error-t ✘ x)`,
  `)))` ⇒ `(error) (error-t ✘ ✘ ✘)`, `] x`, `}`. The `parse` driver (`core.rs`),
  when `parse_stmt` declines on a close-delimiter token with no leftover mark yet
  and the line carries no `;`, emits an empty `ERROR` node then wraps the
  delimiter run plus the rest of the line in one `ERROR_TRIVIA`. Projector
  untouched (empty `ERROR` ⇒ `(error)`, close-delimiter tokens ⇒ `✘` already).
  Fixture `stray_closer_start`. JS allow 583 → 584; dir 128 → 129. **Deferred**:
  the `;`-segment forms (`) ; x` ⇒ `(error) (error-t ✘ ✘ x)`, `x; )` ⇒
  `(toplevel-; x (error) (error-t ✘))`) emit a subtle double-`✘` marker.
- [x] Ternary whitespace-error `(error-t)` (error-shape slice). JuliaSyntax
  requires whitespace on both sides of `?` and `:`; each missing side splices one
  zero-width `ERROR_TRIVIA`. `?` markers sit between condition and true-branch
  (`a? b : c`/`a ?b : c` ⇒ `(? a (error-t) b c)`), `:` markers between the
  branches (`a ? b: c`/`a ? b :c` ⇒ `(? a b (error-t) c)`); a glued-both-sides
  operator doubles them (`a?b:c` ⇒ `(? a (error-t) (error-t) b (error-t)
  (error-t) c)`). A missing `:` is itself one marker, and the false-branch is now
  parsed greedily (even across a newline) rather than abandoned: `a ? b c` ⇒
  `(? a b (error-t) c)`. `parse_ternary` (`expr.rs`) counts the missing sides via
  `q_idx == cond.end`/`colon == then_br.end` (no leading ws) and an `is_trivia`
  check on the following token (no trailing ws), then emits the empty markers in
  the event stream; the projector's `TERNARY_EXPR` arm already renders child
  `(error-t)` nodes in order. Fixture `ternary_whitespace_error`. JS allow
  584 → 589; dir 129 → 130. **Deferred** (multi-marker incomplete forms): `a ? b`
  ⇒ `(? a b (error-t) (error-t) (error-t) (error))`, `a ?` similar.
- [x] More leading-keyword block forms: `for … end`, `while … end`, `let … end`,
  `try/catch/else/finally`, `struct`/`mutable struct`,
  `module`/`baremodule`, `quote … end`. Headers (`for i in xs`,
  `struct Foo <: Bar`) use a generic lossless passthrough for now —
  dedicated `in`/`∈`/`<:` operators and richer header trees come with the
  operators and parametric-type bullets below. **Known limitation:**
  `mutable` is lexed as a keyword, so it cannot currently be used as a bare
  identifier (it is contextual in Julia, special only before `struct`).
- [x] `struct`/`module` signature + same-line body. `parse_signature`
  (`structural.rs`) parses the type/name as a single expression into `SIGNATURE`
  and stops there, instead of gobbling the rest of the line. Same-line body
  statements now fall through to the block: `struct A const a end` ⇒
  `(struct A (block (const a)))`, `struct A <: B c end` ⇒
  `(struct (<: A B) (block c))`, `module A x end` ⇒ `(module A (block x))`. The
  signature subtype (`A <: B`) is now a real `BINARY_EXPR` node and the bare name
  a `NAME` node (the projector's `first_node` path).
- [x] Block forms as infix operands. A value-producing block form (`begin`/`if`/
  `for`/`while`/`let`/`try`/`function`/`macro`/`quote`/`struct`/`module`/
  `abstract type`/`primitive type`) is an operand: a trailing infix operator
  takes the whole form as its left side (`begin x end::T` ⇒ `(::-i (block x) T)`,
  `if c x end + 1`, `begin x end where T`, `begin x end, y` ⇒ `(tuple …)`). In
  `parse_expr_in` these forms now fall through into the operator loop as `lhs`
  rather than returning early; `lhs_is_block_keyword` suppresses postfix
  chaining and juxtaposition (Julia errors on `begin x end(y)` / `begin x end y`).
- [x] `do` blocks — postfix on a call (`f(x) do y … end`). Attached in the
  postfix chain (`parse_postfix_chain`) and parsed by `parse_do_block`, whose
  `parse_do_params` reads the `do`-line parameters (`DO_PARAMS`) as a
  comma-separated argument list (mirroring JuliaSyntax's `parse_comma_separated`):
  the list ends at the first non-comma token, so a same-line body
  (`f(x) do y body end` ⇒ `(do (call f x) (tuple y) (block body))`) falls through
  to the block rather than being swallowed as a parameter. Same-line only (`do`
  must sit on the call's line); terminal in the chain, so calling its result needs
  explicit parens.
- [x] `return`, `break`, `continue`, `const`, `global`, `local`, `import`,
  `using`, `export`. Leading-keyword statement forms (no `… end`), parsed by
  the shared `parse_keyword_stmt` in `structural.rs`: control flow is bare or
  takes an optional operand; `const`/`global`/`local` parse their first operand
  as an expression then carry the rest of the line through. `import`/`using` now
  build a real path tree (see the dedicated bullet below). `export`/`public`
  parse a dedicated comma-separated name list (`parse_name_list_stmt` in
  `structural.rs`): a name is a bare identifier, an operator used as a name
  (`export +, ==`, `export ⊕`), an interpolated name (`export $a, $(a*b)`), or a
  macro name (`export @a`, `export @var"#"`). A newline directly after the keyword
  or after a comma continues the list onto the next line (`export a, \n b`); a
  bare newline after a complete name ends the statement (`export a \n b` is two
  statements). The projector's shared `name_run_item` reads operator-token names
  as their bare text.
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
  Bare-name forward declarations (`function f end`, `macro m end`, `function $f
  end`) project to `(function f)`/`(macro m)` with no body block:
  `project_function_like` (`sexpr.rs`) drops the empty `BLOCK` when the signature
  is a bare `NAME`/`INTERPOLATION` (`is_forward_declaration`), matching
  JuliaSyntax which has no body for a declaration.
- [x] `public` contextual keyword (`public A, B`, `public @a`). A statement-only
  reword: at toplevel and module-block scope, the identifier `public` opens a
  `PUBLIC_STMT` (parsed by `parse_name_list_stmt`, sharing the `export` name-list
  machinery) *unless* the next significant token is `(`, `=`,
  or `[` — which keep `public` an ordinary identifier (`public(x)`, `public = 1`,
  `public[i]`), matching JuliaSyntax's `parse_public` compatibility shim. A new
  `public_context` flag on `ExprFlags` (set by `parse_stmt`, threaded through the
  toplevel loop and `run_module_block`, off in every other block) gates the
  detection so `public` stays an identifier inside `begin`/`if`/function bodies.
  The projector heads the node `public`, dropping the leading keyword token before
  reading the names via the shared `name_run_item`. Operator names (`public +`),
  unicode operator names (`public ⤈`), and newline continuation now fall out of
  the shared `parse_name_list_stmt`. (The `;`-separated toplevel `toplevel-;`
  grouping is now handled — see "Top-level `;` grouping" below.)
- [x] String interpolation (`"$x"`, `"$(expr)"`), raw/byte strings, command
  literals (`` `…` ``), non-standard string literals (`r"..."`, `b"..."`).
  Structured into `STRING_LITERAL`/`CMD_LITERAL` nodes with `INTERPOLATION`
  children whose `$(expr)` interiors are fully parsed sub-expressions; prefixes
  (`r`, `raw`, `b`, `v`) and suffix flags (`r"…"ims`) are represented as tokens.
  An identifier-shaped flag suffix may carry trailing digits (`x"s"i2` → `"i2"`),
  and a digit-led suffix glued to a string macro is an extra numeric macrocall
  argument (`x"s"2` → `(macrocall @x_str (string-r "s") 2)`); the suffix number is
  captured into the `STRING_LITERAL` node. Command literals lower the same way: a
  bare `` `cmd` `` is `(macrocall core_@cmd (cmdstring-r "cmd"))`, a prefix names a
  custom command macro (`` x`str` `` → `(macrocall @x_cmd (cmdstring-r "str"))`), a
  glued flag is an extra argument (`` x`str`flag ``), and a triple-backtick command
  gets the same dedent + per-line chunking as a triple string (`cmdstring-s-r`).
  Known limitation: a `\"` immediately
  before a raw-string closing quote is not yet handled (the raw body is kept as
  one content chunk).
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
  dedicated form. Operator, `$`, and keyword macro names (`@+`, `@!`, `@..`,
  `@$`, `@end`, qualified `A.@!`) parse via `is_macro_name_token` in
  `parse_macro_name_body`; the projector reads the name token through
  `is_macro_name_part_token`. Nested dotted macro paths (`@A.B.x`, `A.B.@x`,
  `$A.@x`, `A.$B.@x`, `A.@.x`) project to the same nested `(. (. A (quote B))
  (quote @x))` shape as plain field access: `project_macro_name` reuses `project`
  on the trailing-form module node and folds the prefix-form flat components.
  A `var"…"` non-standard identifier as the macro name (`@var"#"` ⇒ `(var @#)`,
  qualified `A.@var"#"`, `export @var"#"`) parses via `push_var_macro_name`
  (`expr.rs`), shared by `parse_macro_name_body` and `push_macro_name`
  (`structural.rs`); `project_macro_name` folds the `@` into the `var` content.
  The doc macro extends across one newline: when `@doc` (leaf name `doc`, also
  `A.@doc`/`@A.doc`) takes exactly one space-separated argument and the next line
  holds a non-closing expression, `parse_macro_args` consumes the newline and one
  more `parse_eq`-level argument (`@doc x\ny` ⇒ `(macrocall @doc x y)`); a blank
  line, closing token, or end of input stops it.
  A `[`/`{` adjacent to the macro name (no whitespace) is the bracket-macrocall
  form: the bracket is the sole argument and postfix operators chain onto the
  whole macrocall (`@S[a].b` ⇒ `(. (macrocall @S (vect a)) (quote b))`,
  `@S[a](x)` ⇒ `(call (macrocall @S (vect a)) x)`). `parse_macro_args` parses
  only the bracket prefix (`parse_prefix`) and returns, so the outer postfix
  chain attaches any suffix; the space form `@S [a].b` keeps `[a].b` as one arg.
  A parenthesized macro name `@(A)` (a lone identifier in parens, interior
  whitespace allowed) unwraps to the bare name `@A`: `parse_macro_name_body`
  consumes the `( ident )` run into the `MACRO_NAME` (lossless) and the projector
  reads only the identifier component, so `@(A) x` ⇒ `(macrocall @A x)` and
  `@(A)(x)` ⇒ `(macrocall-p @A x)`. **Deferred:** qualified/dotted interiors
  (`@(A.b)`, `A.@(x)`, `@(A).b`) stay error-shape divergences.
- [x] Parametric types and braces (`Vector{T}`, `where`), type annotations
  (`x::T`), keyword arguments and `;` in call argument lists, splat
  (`x...`). Postfix `{…}` builds a `CURLY_EXPR` in the postfix chain (alongside
  call/index); standalone `{…}` (e.g. `where {T, S}`) builds a `BRACES` node via
  the prefix path. `::` is a dedicated `TYPE_ANNOTATION` (binary `x::T` and unary
  `::T` in method args like `f(::Int)`). `where` is a left-associative chain →
  `WHERE_EXPR` (handled directly in the operator loop, gate `WHERE_BP = 31`),
  binding tighter than every binary operator but looser than `^`/juxtaposition/`.`
  (mirroring JuliaSyntax's `parse_where` between `parse_shift` and
  `parse_juxtapose`): `A << B where C` ⇒ `(call-i A << (where B C))`,
  `A^B where C` ⇒ `(where (call-i A ^ B) C)`. Each bound is parsed at comparison
  precedence with `where` suppressed, so a chain stays left-nested
  (`A where B where C` ⇒ `(where (where A B) C)`) and the bound captures a
  `<:`/`>:` bound (`A where T<:Real`). Prefix `<:`/`>:` reach into a trailing
  `where` (`<: A where B` ⇒ `(<:-pre (where A B))`), and a value-position `::`
  pulls a trailing `where` into its right operand (`f(x)::T where U` ⇒
  `(::-i (call f x) (where T U))`), while a long-form `function`'s return type
  does not (`function f()::S where T end` ⇒ `(where (::-i (call f) S) T)`).
  `<:`/`>:` are lexed as `SUBTYPE`/`SUPERTYPE` comparison operators (infix and
  prefix). In call/index
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
  N-dimensional concatenation (`;;`/`;;;` …): `parse_matrix` scans the body into
  elements + dimension-tagged separator runs (a `;` run's length, a row-breaking
  newline → 1, a space → 0) and recursively nests groups into `MATRIX_ROW`s by
  splitting at each level's maximum dimension, leaving bare single elements
  unwrapped; the projector recovers each group's dimension from its separator
  tokens and heads it `hcat`/`vcat`/`ncat-d` (top) or `row`/`nrow-d` (nested).
  `[x ;; y]` → `(ncat-2 x y)`, `[x ; y ;; z]` → `(ncat-2 (nrow-1 x y) z)`,
  `[x;]` → `(vcat x)`, `[x\n]` → `(vect x)`; element-free `[;]`/`[;;]` →
  `(ncat-1)`/`(ncat-2)` via `parse_empty_ncat`. A newline first separator that is
  followed (past trivia) by a `,` is insignificant whitespace — the comma is the
  real vector separator — so `[x\n, y]` → `(vect x y)` (`newline_run_precedes_comma`).
  Likewise a newline run before the comprehension `for` is insignificant, so
  `[x \n\n for a in as]` → `(comprehension …)` (`newline_run_precedes_for`).
  Typed concatenation (`T[x y]` → `(typed_hcat T x y)`, `T[a;b]` →
  `(typed_vcat T a b)`, `T[a ;; b]` → `(typed_ncat-2 T a b)`, `T[;]` →
  `(typed_ncat-1 T)`): a space/`;`-separated bracket body after a value builds a
  `TYPED_MATRIX_EXPR` (the type expr + a `MATRIX_EXPR` body) via
  `parse_typed_concat`; the projector prepends the type and prefixes the head
  `typed_`. Brace concatenation (`{x y}` → `(bracescat (row x y))`, `{a;b}` →
  `(bracescat a b)`, `{a;;b}` → `(bracescat (nrow-2 a b))`): `parse_braces`
  dispatches comma → `BRACES`, space/`;` → `BRACESCAT_EXPR`; the projector always
  heads `bracescat`, keeping a dim-1 layout's children but nesting a higher-dim
  layout as a single `row`/`nrow-d` child. A `for` after the first brace element
  is a brace generator (`{y for y in ys}` → `(braces (generator y (= y ys)))`):
  `parse_braces` routes it to the shared `parse_comprehension` with a
  `BRACES_COMPREHENSION` node the projector heads `braces`.
  Whitespace-sensitive postfix split: inside a concatenation literal a `(`/`[`/`{`
  with whitespace before it begins a *new* element rather than chaining as a
  call/index/curly, so `[f (x)]` → `(hcat f x)` (two elements) while `[f(x)]` →
  `(vect (call f x))`; `parse_postfix_chain` takes the `array_mode` flag and breaks
  before a space-preceded opener (`[a [b] c]` → `(hcat a (vect b) c)`,
  `[a {T} b]` → `(hcat a (braces T) b)`).
  Follow-ups: tuple-destructuring loop vars (`for (i, j) in …`) and mixed
  space+`;;` rows (`[x y ;; z w]`, an `(error-t)` shape).
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
  lone-operator paren (no inner node) as the operator's text. Prefix-quoted
  *dotted* (broadcast) operators now quote too (`:.+`, `:.&`, `:.=`, `:.&&`,
  `:.+=` → `(quote-: (. +))` etc.): a `parse_quote_sym` arm gated on
  `is_dotted_broadcast_text` (leading broadcast `.`, excluding `..`/`...`) wraps
  the dotted-operator token in an `OPERATOR_ATOM`, which the projector's
  `project_operator_atom` splits the broadcast dot off of into `(. op)`. The
  remaining undotted value/syntactic operators now quote too (`:..`, `:√`, `:∛`,
  `:¬`, the Unicode operators `:⊕`/`:≤`/`:→`, and the ternary `:?` →
  `(quote-: ..)`/`(quote-: √)`/`(quote-: ?)` etc.): the bare-operator quote arm's
  predicate gains `is_quotable_operator` (`DotDot`, the Unicode operator tiers and
  radicals, `Question`), the token text projected verbatim. **Known
  limitations:** the bare-`:` Colon value (`a[:]` → `(ref a :)`), the syntactic
  sigil quotes `:$`/`:.`/`:...` (Julia quotes the sigil alone, dropping any
  operand to an `error-t` — error-shape, deferred), the paren form
  of a dotted *syntactic-assignment* quote (`:(.=)` still errors; the
  `is_paren_quotable_op` interior set has no dotted forms), standalone
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
  `(call-i a .. b)`.

- [x] Splat/vararg `...` precedence. The postfix `...` is no longer parsed in the
  tight postfix chain (where it bound tighter than every infix op) but as a
  postfix operator in the Pratt loop with left binding power `SPLAT_BP = 14` —
  looser than the colon/range tier (`x:y...` ⇒ `(... (call-i x : y))`,
  `x..y...` ⇒ `(... (call-i x .. y))`) but tighter than the pipes and everything
  looser (`a|>b...` ⇒ `(call-i a |> (... b))`, `a&&b...` ⇒ `(&& a (... b))`).
  `...` is not in `is_operator`, so when it does not bind (inside colon's right
  operand) the loop breaks and an enclosing parse consumes it.

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
  the root `import $A` now parses, see the dedicated bullet below) — carried
  through verbatim, keeping losslessness. Operator-symbol names, `@macro` paths,
  the `. .A` whitespace-separated leading dots, and unicode/`..` components now
  parse (see the dedicated bullets below).

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
  routes `QUOTE_SYM` children through `project_quote_sym`.

- [x] Unicode, `..`, and whitespace-separated-dot import names. `parse_import_path`
  (`structural.rs`) threads three more component forms through: a single-codepoint
  unicode operator as a path name (`import ⋆`, `import .⋆`, `import A.⋆.f`,
  `import A: ⋆, f`); a trailing `...` after a name as the `..` range
  operator (`import A...`, `import A.B...` → `(importpath A ..)` — the `...` is the
  separator dot fused with `..`, projected via a `DOT_DOT_DOT if seen_name` arm);
  and whitespace-separated leading dots (`import . .A`, `import .. .A` → the
  leading-dot loop now `skip_ws`-hops between dots, carrying the gap verbatim).
  Projector `project_import_path` reuses `is_operator` for the unicode name. (Once
  broadcast unicode operators began fusing the separator `.` into the op token —
  see the unicode-operators bullet — both `.⋆` and the ASCII `.==`/`.+` reach the
  path as one fused token: the first-name/component arms now also accept
  `is_dotted_op_name`, and the projector emits a lone relative-dot part when the
  fused op precedes the first name, so `import .==` → `(importpath . ==)` too.)

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
  a `!` case (`!` is unary-only, no `infix_head` entry). The empty all-semicolon
  block edge `+(;;)` → `(call-pre + (block-p))` is handled: a leading-`;`
  paren-call defers to `paren_is_block`, so a multi-`;` empty group prefixes the
  block instead of opening a call.

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
  `project_var` heads the node `var` over the raw delimited content. The lexer
  applies Julia's raw-string rule (an odd backslash run before a quote escapes
  it) so `var"\""`/`var"\\\""` lex as one identifier; `project_var` runs the
  name through `unescape_raw_string` (`\"` ⇒ `"`, `\\\"` ⇒ `\"`, trailing `\\`
  ⇒ `\`, other backslash runs literal). **Deferred:** the suffix-error shape
  (`var"x"y` → `(var x (error-t))`).
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
  Broadcast (dotted) infix unicode operators (`a .… b`, `a .× b` → `(dotcall-i a
  … b)`) now land too: the lexer fuses a broadcast `.` immediately followed by an
  infix-tier unicode op into one token spanning `.op` (`is_unicode_infix_tier`
  gates the six `call-i` tiers; radicals and the assignment tier stay unfused),
  and `project_binary` strips the leading `.` and heads `dotcall-i`. Import paths
  cope with the now-fused token by splitting the leading `.` back out (see the
  import bullet). **Deferred:** broadcast unicode radicals (`.√x`, prefix) and the
  assignment tier; unicode comparison chains (nested, like the ASCII chain
  divergence); unicode unary in the plus/times tiers (`±x`). (Juxtaposition and
  operator-suffix sub/superscripts both landed separately — see those bullets.)

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

- [x] Signed numeric literals. A `+`/`-` glued to an adjacent number folds into a
  single signed literal rather than a unary prefix call (`-2` ⇒ `-2`, `+2.0` ⇒
  `2.0`, `-1.0f0` ⇒ `-1.0f0`, `-2*x` ⇒ `(call-i -2 * x)`), mirroring JuliaSyntax
  `parse_unary`. `parse_prefix` grows a guarded arm driven by `signed_literal_fold`:
  the operator must be undotted (`Plus`/`Minus`) and unsuffixed, directly followed
  (no whitespace) by a number literal — decimal `Integer`/`Float`/`Float32` for
  either sign, plus the unsigned `BinInt`/`HexInt`/`OctInt` for `+` only (a no-op
  drop; `-0x1` stays `(call-pre - 0x1)`). It does *not* fold when `^`/`[`/`{`
  follow the literal, since those bind tighter than unary negation (`-2^2` ⇒
  `(call-pre - (2^2))`, `-2[1]` ⇒ `(call-pre - (ref 2 1))`). The fold builds a
  `LITERAL` wrapping the sign + number tokens; `project_literal` combines them
  (`-` kept, `+` dropped), and `lhs_is_number` recognizes the two-token literal so
  it still juxtaposes (`-2(x)` ⇒ `(juxtapose -2 x)`). Also fixes the `matrices`
  oracle case: `[1 +2]` ⇒ `(hcat 1 2)`.

- [x] Integer-literal display normalization (projector). JuliaSyntax shows a
  numeric leaf as its parsed *value*, not the source text; the projector now does
  the same for integers (the same value-rendering the string/char paths already
  do — the CST stays lossless source text). `literal_token_text` (`sexpr.rs`)
  strips underscores from decimal `INTEGER`s (`1_000` ⇒ `1000`) and routes
  base-prefixed `HEX_INT`/`OCT_INT`/`BIN_INT` through `normalize_based_int`, which
  renders the value as lowercase hex zero-padded to the width of Julia's selected
  `UInt` type: bit count (hex `4·ndigits`, binary `ndigits`, octal
  `bits(leading) + 3·(ndigits−1)` via `octal_bits`) rounded up to {8,16,32,64,128}
  ⇒ {2,4,8,16,32} hex digits (`0x1`⇒`0x01`, `0o22`⇒`0x12`, `0b10010`⇒`0x12`,
  `0o755`⇒`0x01ed`, `0o00007`⇒`0x0007`). Applied in both the single-token and
  signed two-token literal paths, so `-0x1`⇒`(call-pre - 0x01)`, `+0o22`⇒`0x12`.
  **Deferred (two recorded buckets, to revisit):** (1) *float-literal display
  normalization* — `2.`⇒`2.0`, `1.5e-3`⇒`0.0015`, `1f0`⇒`1.0f0`, hex floats
  `0x1.8p3`⇒`12.0`, underflow `1.0e-1000`⇒`0.0`; needs replicating Julia's exact
  `Base.show(::Float64)`/`Float32` shortest-round-trip + notation thresholds
  (Rust's `{}` differs), and `>128`-bit `BigInt` based literals (shown as
  decimal). (2) *modeling divergences* — associative n-ary flattening (`a+b+c`,
  `a*b*c`, `[x+y+z]`), comparison chains (`x<y<z` ⇒ `(comparison …)`),
  short-circuit chains (`x&&y&&z`), and n-ary juxtaposition (`(2)(3)x`) all stay
  nested by deliberate Fatou choice. (Error-shape recovery — `a--b`, `'ab'`,
  `function \n f() end` — remains the separate deferred phase.) The dir fixture
  `based_int_display` covers the integer case; `numeric_literals` stays blocked on
  the float half.

- [x] Stepped colon ranges. A `:` chain with a step folds three operands into one
  call rather than nesting two binary colons (`1:2:3` ⇒ `(call-i 1 : 2 3)`,
  `a:b:c:d:e` ⇒ `(call-i (call-i a : b c) : d e)`), mirroring JuliaSyntax's
  `parse_range` (every second colon emits a 3-arg call, then the fold becomes the
  left operand of the next chain). The operator loop intercepts `:` (after the
  ternary `no_range` guard) and delegates to `parse_colon_range`, which gathers
  operands at the colon's right binding power `(14, 15)` and emits a new
  `RANGE_EXPR` node per stepped triple; an odd trailing colon (`a:b:c:d`) leaves
  the usual two-operand `BINARY_EXPR`. The chain stops at a ternary separator or
  an array-element boundary (`[1 :2]`). `project_range` emits the 3-operand
  `(call-i lhs : mid rhs)`; plain `a:b` is unchanged.

- [x] Bare-comma tuples. A top-level comma at statement scope now folds its
  operands into a `BARE_TUPLE_EXPR` (`(tuple …)`, vs the parenthesized
  `tuple-p`): `a, b, c` ⇒ `(tuple a b c)`, `x, = xs` ⇒ `(= (tuple x) xs)`.
  Comma binds tighter than assignment but looser than every real operator
  (mirroring JuliaSyntax's `parse_comma` below `parse_assignment`), so it
  composes with `=` on both sides — `a, b = c, d` ⇒
  `(= (tuple a b) (tuple c d))`. Implemented in the operator loop, gated by a
  `stmt_comma` flag (on at toplevel/module/block statements and the operand of
  `return`/`const`, off inside brackets where commas are arg/element
  separators): when `min_bp <= COMMA_BP (2)` and a comma follows, the already
  parsed first operand and each further item — parsed at `COMMA_ITEM_BP (3)`,
  excluding `=` and the comma — are gathered by `parse_comma_tuple`. `return x,
  y` ⇒ `(return (tuple x y))` and `const x, y = 1, 2` ⇒ `(const (= …))` via a
  new `KwStmt::ExprTuple`; `global`/`local` keep their bare name list
  (`(global a b)`).

- [x] Top-level `;` grouping. A logical line carrying a top-level `;` now folds
  its statements into a `TOPLEVEL_SEMICOLON` node (`(toplevel-; …)`, mirroring
  JuliaSyntax): `a;b;c` ⇒ `(toplevel (toplevel-; a b c))`, `a;` ⇒
  `(toplevel (toplevel-; a))`, bare `;` ⇒ `(toplevel (toplevel-;))`. The `parse`
  driver (`src/parser/core.rs`) now works one logical line at a time —
  newline-delimited — wrapping the line only when it saw a `;`; a plain line
  stays bare (`a` ⇒ `(toplevel a)`) and newlines split groups (`a;b\nc;d` ⇒
  two `toplevel-;` nodes). Scoped to the toplevel driver only: inside `begin`/
  module blocks `;` does not group (`begin a; b end` ⇒ `(block a b)`).
- [x] Paren block sequences. A `;`-bearing parenthesized run that is not a tuple
  now parses as a `PAREN_BLOCK` projecting `(block-p …)`, mirroring JuliaSyntax
  `parse_paren`/`parse_brackets`: `(a; b; c)` ⇒ `(block-p a b c)`, `(a=1; b=2)` ⇒
  `(block-p (= a 1) (= b 2))`, `(a;b;;c)` ⇒ `(block-p a b c)`, `(;;)` ⇒
  `(block-p)`. `paren_is_block` (`src/parser/expr.rs`) gathers the disambiguation
  flags by a depth-0 token scan and applies the rule `is_tuple = had_commas ||
  (had_splat && num_semis≥1) || (initial_semi && (num_semis==1 || num_subexprs>0))`,
  `is_block = !is_tuple && num_semis>0`; the two `;`-reaching call sites in
  `parse_paren` pick the node kind via `paren_list_kind`. The block reuses the
  arg-list machinery, so the projector (`project_block_args`) flattens the
  `ARG`/`KEYWORD_ARG`/`PARAMETERS` encoding into a flat statement list. A function
  signature's `;`-parens (`function (x; y) end`) are a parameter list, not a
  block, so `parse_function_like` relabels a `PAREN_BLOCK` signature back to
  `TUPLE_EXPR` (same shape).

- [x] Per-group `parameters` in tuples and calls. Each `;` after the first now
  starts a fresh `PARAMETERS` group rather than folding the whole tail into one,
  matching JuliaSyntax: `(a; b; c,d)` ⇒ `(tuple-p a (parameters b) (parameters c
  d))`, `(; a=1; b=2)` ⇒ `(tuple-p (parameters (= a 1)) (parameters (= b 2)))`,
  `f(a; b; c)` ⇒ `(call f a (parameters b) (parameters c))`, `+(;;a)` ⇒ `(call +
  (parameters) (parameters a))`. Pure parser fix in `parse_arg_list`
  (`src/parser/expr.rs`): a `;` closes the open `PARAMETERS` (if any) and opens a
  new one, with the `;` as the group's leading delimiter; the projector already
  maps each `PARAMETERS` sibling to its own `(parameters …)` and
  `project_block_args` still flattens them for the block case (so the
  `PAREN_BLOCK` projection is unchanged). **Deferred:** the empty-all-semis
  operator-prefix case `+(;;)` ⇒ `(call-pre + (block-p))` (a separate
  prefix-call/block disambiguation, still FAIL).
- [x] Triple-quoted string dedent. The projector now computes a triple-quoted
  string's value the way JuliaSyntax does: normalize CRLF/CR line endings to LF,
  split the content into one `String` chunk per line, strip the longest common
  leading whitespace (skipping blank lines except the closing-delimiter line, and
  never dedenting the opening line), drop the leading newline right after `"""`,
  then display-escape control characters. `"""\n  x\n y"""` ⇒ `(string-s " x\n"
  "y")`, `"""\n  $a\n  $b\n"""` ⇒ `(string-s "  " a "\n" "  " b "\n")`. Pure
  projector change in `triple_string_parts` (`src/parser/sexpr.rs`); the CST stays
  lossless (raw content preserved in `STRING_CONTENT`). Also emits the empty
  `String` child for empty literals (`"" → (string "")`, `"""""" → (string-s
  "")`). **Deferred:** full source-escape unescaping (`\xNN`/`\uNNNN`/line
  continuations).

- [x] Raw triple-quoted strings (`r"""…"""`). A prefixed triple-quoted string
  reuses the same dedent + per-line chunking as a plain triple string, projecting
  to a `string-s-r` body inside the `@<p>_str` macrocall; only the unescaping
  differs—raw content's backslashes and quotes are literal, so each chunk is
  display-escaped as raw bytes (`\\`, `\"`, `\$` in addition to control chars).
  `r"""\n x\n y"""` ⇒ `(macrocall @r_str (string-s-r "x\n" "y"))`. Projector-only
  change (`project_string`/`triple_string_parts`/`escape_display`, `sexpr.rs`);
  single-line raw strings keep the `(string-r …)`/`quote_raw` path. **Deferred:**
  raw-string quote unescaping (`\"`/`\\` before a closing quote inside the body).

- [x] Char literal escape decoding (`'\xce\xb1'`, `'α'`, `'\U1D7DA'`). The
  lexer now scans a char literal to its closing `'` (skipping a backslash escape's
  following byte) instead of only allowing one char or a single-byte escape, so
  multi-escape literals lex as one `CHAR` token. The projector (`project_char` in
  `src/parser/sexpr.rs`) decodes the source escapes to a single codepoint—byte
  escapes (`\xNN`, octal) and literal chars accumulate as UTF-8 bytes, `\u`/`\U`
  and named escapes contribute a codepoint—then re-displays it the way JuliaSyntax
  shows a `Char` (named control escapes, `\\`/`\'`, `\xNN`/`\u`/`\U` hex forms for
  other non-printables, else literal). `'\xce\xb1'` ⇒ `(char 'α')`, `'\U1D7DA'` ⇒
  `(char '𝟚')`. **Deferred:** the error shapes — over-long `'ab'`
  (`ErrorOverLongCharacter`) and invalid escapes `'\xq'` (`ErrorInvalidEscapeSequence`)
  fall back to raw passthrough.

- [x] Single-quoted string escape processing and line continuations
  (`"\x41\x42"` ⇒ `(string "AB")`, `"a\<newline>b"` ⇒ `(string "a" "b")`). The
  projector (`string_parts`/`decode_string_chunks`/`escape_string_value` in
  `src/parser/sexpr.rs`) now computes a string's *value* the way JuliaSyntax does
  rather than echoing the raw source: escapes are decoded (sharing
  `decode_escape_into` with `project_char`) and re-shown JuliaSyntax-style (sharing
  the control escapes via `control_escape`), and a `\`-newline line continuation
  splits the content into separate `String` chunks — dropping the backslash, the
  newline (`\n`/`\r`/`\r\n`), and the following indentation. A `\`-CRLF
  continuation also needed a lexer fix (`consume_body_byte`, `lexer.rs`) so the
  trailing `\n` is consumed with the backslash instead of leaking out and
  terminating the single-line string. The CST stays lossless (one raw
  `STRING_CONTENT` token). **Deferred:** invalid-escape error shapes (`"\xqqq"` ⇒
  `(string (ErrorInvalidEscapeSequence))`) fall back to raw passthrough.

- [x] Docstring attachment (`"doc"\nfoo` ⇒ `(doc (string "doc") foo)`). A bare,
  unprefixed `STRING_LITERAL` statement directly followed by another statement —
  at most one newline of intervening trivia, no `;`, no blank line — folds into a
  `DOC` node, mirroring JuliaSyntax's `parse_docstring`. Implemented as a single
  recursive post-pass over the event stream (`fold_docstrings`, `src/parser/core.rs`)
  run just before tree building: because every block body's events flatten up into
  the root event list, one pass folds toplevel, `;`-grouped lines, and nested
  function/module/begin bodies uniformly. Prefixed string macros (`r"…"`, command
  strings) and string-as-expression (`"a" + b`) are excluded by construction. The
  CST stays lossless (only `DOC` wrappers are inserted around existing tokens).
  Projector arm `DOC ⇒ (doc …)` (`src/parser/sexpr.rs`). **Deferred:** the
  no-target error shape (`"doc"\nend` ⇒ string then `(error end)`).

- [x] Bare operator value atoms (`+` ⇒ `+`, `.&` ⇒ `(. &)`, `(.+)(a)` ⇒
  `(call (. +) a)`). A non-syntactic operator with no operand to its right is the
  operator used as a value (a function reference), not an error. The unary-prefix
  arm (`+ - ! ~ <: >: .+ .- …`) now builds an `OPERATOR_ATOM` instead of erroring
  when its operand parse fails (except `::`, which keeps Julia's `(::-pre (error))`
  shape); a fallback arm catches the binary-only and broadcast value operators
  (`* == |> => .& .* …`) via `is_value_operator` (`src/parser/expr.rs`). Syntactic
  operators (`= :: && || -> ? . ...` and assignment) are excluded and stay errors.
  Projector `OPERATOR_ATOM ⇒ project_operator_atom` emits `(. op)` for broadcast
  forms and the raw token text otherwise; a bare `$` interpolation projects to `$`
  (`src/parser/sexpr.rs`). **Deferred:** prefix operators consume an operand across
  a newline (`-\nx` ⇒ `(call-pre - x)` vs Julia's two statements) — a separate
  newline-statement-termination concern.
- [x] Word operators `in`/`isa` (`i in rhs` ⇒ `(call-i i in rhs)`, `x isa T` ⇒
  `(call-i x isa T)`). Lexed as identifiers (so `for i in xs` keeps `in` the
  iteration separator), they act as infix operators at the comparison tier
  (`(10, 11)`, left-associative) via a `word_operator` check in the Pratt loop
  (`src/parser/expr.rs`), gated off by the new `ExprFlags::no_word_op` while
  parsing a `for`-binding (`parse_for_binding`, threaded through `parse_header`).
  The projector reads the loose `in`/`isa` `IDENT` operator of a `BINARY_EXPR`
  back as a `(call-i …)` head (`src/parser/sexpr.rs`). Comparison chains stay
  nested (`a in b in c` ⇒ `((a in b) in c)`), a recorded modeling divergence like
  the symbolic comparisons.

- [x] Broadcast type comparison `.<:`/`.>:` (`x .<: y` ⇒ `(dotcall-i x <: y)`,
  `x .>: y` ⇒ `(dotcall-i x >: y)`). New `DotSubtype`/`DotSupertype` `TokKind`s in
  the 3-char dotted table (longest-match before `.<`/`.>`), comparison tier
  `(10, 11)`, projected `DotCallI("<:")`/`DotCallI(">:")`. The paren-call name
  (`.<:(x, y)` ⇒ `(call (. <:) x y)`) and bare value atom (`.<:` ⇒ `(. <:)`)
  follow via the existing dotted-operator paths.
- [x] `try`/`catch`/`finally` variants. A `catch` exception variable may be a
  `$`-interpolation (`catch $e` ⇒ `(catch ($ e) …)`) or a `var"…"` non-standard
  identifier (`catch var"#"` ⇒ `(catch (var #) …)`): the projector now reads the
  first non-`BLOCK` child of `CATCH_CLAUSE` as the variable rather than only a
  bare `NAME`. A `catch` may also follow `finally` (`try x finally y catch e z
  end` ⇒ `(try … (finally …) (catch e …))`): the parser's `finally` arm bounds
  its block on the try terminators and continues the clause loop when a `catch`
  follows instead of breaking.

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
  partition the corpus — 4 blocked with rationales (numeric-literal display
  normalization, `end`/unterminated-string and incomplete-`do` error shapes). A harvested **JuliaSyntax sub-corpus**
  (`scripts/harvest-juliasyntax-corpus.jl` → `tests/fixtures/oracle/juliasyntax.jsonl`,
  575 micro-cases extracted from JuliaSyntax's own `test/parser.jl`, expected
  regenerated via our pinned `parseall`) is gated opt-in by `oracle_juliasyntax`
  against `tests/oracle/juliasyntax-allowlist.txt` (251 cases); the
  `juliasyntax_full_report` divergence (282) + unsupported (42) buckets are the
  **prioritized parser-growth backlog** — e.g. associative n-ary flattening
  (`a*b*c`) and unicode operators (lexer).
  **Follow-ups:** work the backlog up the allowlist; continue the error-shape
  parity slices (the taxonomy infrastructure has landed — see the typed
  error-node bullet above); wire the oracle gates into CI.
- [ ] Benchmarks (`criterion`) for parse + incremental reparse.
- [ ] `smol_str` interning for symbol names once the semantic model lands.
