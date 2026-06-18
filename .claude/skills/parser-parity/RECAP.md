# parser-parity recap

Rolling log. Read top-to-bottom: persistent traps → progress → latest session →
earlier log. Keep ≤ ~300 lines; demote the "Latest session" to a one-liner each
new session.

## Persistent traps & invariants

- **Projector is faithful, never compensating.** Translate encoding (wrappers,
  delimiters, trivia) only; let modeling divergences surface. Diffs that live
  mostly in `sexpr.rs` are a smell.
- **5-file operator recipe**: lexer `TokKind`+lex → `syntax.rs` kind →
  `tree_builder.rs` map → `expr.rs` `infix_binding_power` → `sexpr.rs`
  `infix_head` + `is_operator`. Probe Julia for tier/associativity first.
- **Probe whitespace-sensitive siblings** before scoping (`a[begin]` vs
  `[begin x end]`; `:foo` vs `a[:]`). Scope narrowly to avoid regressing one.
- **Reseed allowlists with the `grep -E '^#|^$'` header-preserving recipe.**
- **Reports are gitignored; `expected.sexpr` is generated** — never hand-edit.
- **Shell `raw"""…"""` Julia probes break on `"`/`$`** — use a temp file.
- **Corpus pinned** to JuliaSyntax in `.juliasyntax-source` (currently 0.4.10 /
  Julia 1.12.6). Bump ⇒ re-run both `scripts/*.jl`, re-triage.

## Progress

JS corpus (575 cases): **350 allowlisted**, 221 divergence, 4 unsupported.
Dir corpus: **58 allowlisted**, 5 blocked + 1 skipped (do_blocks).
Grammar bullets through "`public` contextual keyword" are `[x]` in `TODO.md`.

Deliberate (recorded) divergences, do not "fix": comparison chains (nested),
associative `a*b*c` (nested binary), numeric-literal display normalization,
triple-string dedent, `end`/`[1 +2]`/unterminated-string/incomplete-`do` error
shapes (dir `blocked.txt`).

## Latest session (2026-06-18o)

**`public` contextual keyword.** `public A, B` / `public @a` now open a
`PUBLIC_STMT` at toplevel and module-block statement scope. Unlike `export`,
`public` is *contextual*: it must stay an ordinary identifier in sub-expressions
(`x = public`), as a call/assignment/index (`public(x)`, `public = 1`,
`public[i]`), and inside non-module blocks (`begin`/`if`/function bodies). The
rule (copied from JuliaSyntax's `parse_public` compat shim, `src/parser.jl`
~513): at file/module level, `public` is the keyword form *unless* the next
significant token is `(`, `=`, or `[`. Implemented with a new `public_context`
flag on `ExprFlags`: `parse_stmt` (new entry, sets the flag) is called by the
toplevel drive loop (`core.rs`) and by `run_module_block` (new wrapper around
`run_block_inner`, used only by `parse_module_expr`); every other block keeps
`run_block` → `parse_expr` (flag off). In `parse_expr_in`, when the flag is set
and `is_public_keyword` fires (identifier `public` + next-sig-token ∉ `( = [`),
it delegates to `parse_keyword_stmt(PUBLIC_STMT, KwStmt::Path)` — the same
name-list machinery as `export`, so `@a` (MACRO_NAME), `$a`, and comma lists fall
out for free. Projector (`sexpr.rs`): `project_public` heads the node `public`,
dropping the leading `public` identifier token (it's a real CST child, unlike
`export`'s distinct `EXPORT_KW`) before reading names via the new shared
`name_run_item` (refactored out of `ident_run`). Faithful: the `public` keyword
+ names are real CST children; the projector only formats the head. Fixture
`public_statement` (parser + dir corpus, 9 lines incl. the three identifier
boundary forms). **Deferred:** unicode operator names (`public ⤈` js-8cf24212 —
needs unicode-operator lexing, target below) and `;`-separated toplevel
(`a=3; b=6; public a,b` js-8d65be34 — the pre-existing `toplevel-;` grouping
divergence, unrelated to `public`).

JS allow **346 → 350** (+4: `public A, B` js-dd6bb2e4, `module Mod … public A,B`
js-1669b4f6, `module Mod2 … public a,b` js-37572fe9, `module M; public @a; end`
js-491f0afc; the six identifier-form `public` cases already passed); divergence
225 → 221, unsupported held 4. Dir allow 57 → 58. Zero regressions; green,
clippy/fmt clean.

**Suggested next targets (ranked):**
1. **Curly operator calls** (`<:{T}(x::T)` js-9edf5083, `+{T}(...)`) — extend the
   operator-callee path to `CURLY_EXPR`/parametric method signatures.
2. **Unicode operators** (lexer) — unblocks `import .⋆`, `A.⋆.f`, `[x +₁y]`,
   `a … b`, `public ⤈`, many scattered FAILs; larger lexer feature.

## Earlier session (2026-06-18n)

**`macro` definitions.** `macro m(ex) … end` now parses. It is structurally
identical to a `function` definition (a call-shaped signature plus a body
block), so the whole feature is a thin reuse: `macro` became a keyword token
(`MacroKw` in `lexer.rs`, `MACRO_KW` in `syntax.rs`/`tree_builder.rs`), the
`parse_prefix` dispatch (`expr.rs`) routes `MacroKw` to the new
`parse_macro_def`, and `parse_function_expr`/`parse_macro_def` both delegate to
a shared `parse_function_like(node_kind)` in `structural.rs` — the only
difference is `FUNCTION_DEF` vs `MACRO_DEF`. Projector (`sexpr.rs`): one new arm
heads the node `macro` over `project_signature` + `project_block_child`,
mirroring `FUNCTION_DEF`. Because the signature flows through the full
expression path, every name form already supported for function signatures fell
out for free: plain `macro f() end` → `(macro (call f) (block))`, operator
`macro (:)(ex) end` → `(macro (call : ex) (block))` (target 1, via the 18j
standalone-paren-operator parse), contextual-ident `macro (type)(ex) end`,
interpolated `macro $f() end` and `macro ($f)() end` → `(macro (call ($ f))
(block))`. Faithful: `macro` keyword + signature + block are all real CST
children; the projector only formats the head. Fixture `macro_definition`
(parser + dir corpus, 6 lines — the five passing JS forms). **Deferred:**
`macro f end` (js-408b2118 — no signature parens → Fatou emits `(macro f
(block))` vs Julia `(macro f)`, the exact same trailing-block error-shape
divergence as `function f end`; left for the error phase).

JS allow **341 → 346** (+5: `macro f() end` js-60025fb4, `macro (:)(ex) end`
js-a916f049, `macro (type)(ex) end` js-937fb0b6, `macro $f() end` js-a2d8af0b,
`macro ($f)() end` js-8fd3d513); divergence 230 → 225, unsupported held 4. Dir
allow 56 → 57. Zero regressions; green, clippy/fmt clean. (Next targets
superseded by 18o; `public` keyword landed.)

## Earlier session (2026-06-18m)

**Type-operator paren-calls.** The type operators `<:`/`>:` glued to a `(` now
follow the same `is_paren_call` heuristic as the unary operators: `<:(a, b)` →
`(<: a b)`, `<:(a,)` → `(<: a)`, `>:(a, b)` → `(>: a b)`, `<:(a...)` →
`(<: (... a))`, `<:()` → `(<:)`, while a lone bare operand stays prefix
(`<:(a)` → `(<:-pre a)`). Parser: `Subtype`/`Supertype` were added to the existing
unary paren-call arm of `parse_prefix` (`expr.rs`) — same `matches!` gate,
`ctx.token(start+1) == LParen`, `unary_op_paren_is_call` — so they build the same
`CALL_EXPR` (operator-token callee + `ARG_LIST`). Projector (`sexpr.rs`):
`project_call` gained a `SUBTYPE | SUPERTYPE`-callee arm that *overrides the head*
with `operator_func_repr` (`<:`/`>:`) instead of emitting a `call` head + operator
arg — these are syntactic type operators, so JuliaSyntax heads the node with the
operator itself, mirroring how binary `<:` routes through `infix_head`'s
`Special("<:")`. Faithful: the operator token + arg list are real CST children; the
projector only formats the head. The single-operand prefix `<:(a)` was already
correct (UNARY_EXPR, untouched). Fixture `type_operator_call` (parser + dir corpus,
8 lines incl. boundary `<:(a)`/`>:(b)` prefix forms). **Deferred:** curly operator
calls (`<:{T}(x::T)` js-9edf5083 — still FAIL, a separate curly-call gap) and the
`<:(a; b)` block-vs-tuple operand shape (pre-existing paren-parsing divergence
shared by all operators, incl. bare `(a; b)`).

JS allow **340 → 341** (+1: `<:(a,)` js-70cde333, was FAIL); divergence 231 → 230,
unsupported held 4. Dir allow 55 → 56. Zero regressions; green, clippy/fmt clean.
(Next targets superseded by 18n; macro definitions landed.)

## Earlier session (2026-06-18l)

**Import paren-quotes.** A parenthesized quoted operator/symbol is now a valid
dotted import-path component in two forms, both projecting to the bare quote:
`import A.:(+)` → `(importpath A (quote-: +))` and `import A.(:+)` →
`(importpath A (quote-: +))`. `parse_quote_sym` was made `pub(super)` and
imported into `structural.rs`; the `parse_import_path` loop's `(Dot, Colon)` arm
now *delegates* to it (so `A.:+`, `A.:(+)`, and as a freebie `A.:foo`/`A.:(foo)`
all flow through one path) instead of hand-emitting a two-token `QUOTE_SYM`. A
new `(Dot, LParen)`-with-inner-`:` arm parses `A.(:+)`: it builds a `PAREN_EXPR`
wrapping the `QUOTE_SYM` that `parse_quote_sym` returns, keeping the parens as
real CST delimiters. Projector (`sexpr.rs`): `project_import_path` gained a
`PAREN_EXPR` arm routing through `project` — the existing `PAREN_EXPR` →
first-inner-node fallback already yields `(quote-: +)`, so no quote-specific
logic was needed. Faithful: parens stay real children, the projector only
unwraps. CST shapes: `A.:(+)` = `QUOTE_SYM{: PAREN_EXPR{( + )}}`; `A.(:+)` =
`PAREN_EXPR{( QUOTE_SYM{: +} )}`. Fixture `import_paren_quote` (parser + dir
corpus, 5 lines incl. ident `A.:(foo)`/`A.(:foo)` and `using`). **Deferred:**
non-symbol paren contents (`import A.(a)` → `a`, no quote — a separate gap) and
the erroring multi-token quote (`import A.:(a+b)` — error-shape).

JS allow **338 → 340** (+2: `import A.:(+)` js-0492d7fb, `import A.(:+)`
js-6fe4ce2d); divergence 233 → 231, unsupported held 4. Dir allow 54 → 55. Zero
regressions; green, clippy/fmt clean. (Next targets superseded by 18m.)

## Earlier session (2026-06-18k)

**Macro names in `export`/`import`/`using`.** A `@` in a directive name position
now builds a real `MACRO_NAME` node instead of dropping the sigil. New shared
helper `push_macro_name` (`structural.rs`) emits `MACRO_NAME` spanning the `@`
plus an adjacent identifier (no args, no dotted chain — Julia treats a trailing
`.mac` here as a separate erroring component). Wired into the `export` verbatim
loop (`parse_keyword_stmt`, new `At` match arm beside the `Dollar` one) and into
`parse_import_path` in two spots: the path-root arm (new `Some(At)` case beside
`Some(Dollar)`) and the dotted-component loop (new `(Dot, At)` case beside
`(Dot, Ident)`). Projector (`sexpr.rs`): `ident_run` (export) and
`project_import_path` (import) each gained a `MACRO_NAME` arm routing through the
existing `project_macro_name`, which yields bare `@x` for the single-ident case.
Faithful: the `@` sigil + name are real CST children, the projector only formats
the wrapper. Results: `export @a` → `(export @a)`, `export a, @b` →
`(export a @b)`, `import @x` → `(importpath @x)`, `import .@x` →
`(importpath . @x)`, `import A.@x` → `(importpath A @x)`, `import A.B.@x`,
`import A.@x.y` → `(importpath A @x y)`. With the `$`-root from 18i already
parsing, `import $A.@x` → `(import (importpath ($ A) @x))` (target 1) fell out
for free. Fixture `macro_directive_names` (parser + dir corpus, 8 lines).
**Deferred:** `public @a` (js-491f0afc — `public` is not yet a contextual
keyword, a separate gap), and standalone qualified macro paths *as expressions*
(`A.B.@x` js-968d2da1, `A.@doc x\ny`, `@A.B.x` — these are macrocall-expression
shapes, not directive names).

JS allow **334 → 338** (+4: `export @a` js-b7bb6850, `module M; export @a; end`
js-7a07fde8, `import @x` js-73c24f26, `import $A.@x` js-97312f87); divergence
237 → 233, unsupported held 4. Dir allow 53 → 54. Zero regressions; green,
clippy/fmt clean. (Next targets superseded by 18l; import paren-quotes landed.)

## Earlier session (2026-06-18j)

**Standalone parenthesized operators.** A lone non-syntactic operator inside
parens in value position is the operator as a value: `(+)` → `+`, `(:)` → `:`,
`(<:)` → `<:`, `(!)` → `!`. `parse_paren` (`expr.rs`) gains an arm, after the
empty/`;`-tuple checks and before `parse_expr_in_brackets`, gated by the new
`is_paren_value_op` predicate (`is_op_name` minus the syntactic `&&`/`||`/`->`,
which Julia reports as errors in value position, plus `:`); when the interior is
`( op )` it builds a `PAREN_EXPR` wrapping the bare operator token and returns —
whitespace-insensitive via `skip_trivia` (`( + )` is the same). Postfix
application then forms calls: `(+)(a, b)` → `(call + a b)`, `(:)(a)` →
`(call : a)`, and `function (:)() end` → `(function (call :) (block))`. **The
projector needed no change** — `sexpr.rs`'s `PAREN_EXPR | CONDITION` arm already
falls back, when there is no inner node, to the first significant `is_operator`
token's text (added in 18e for `:(=)`). Faithful: the parens stay real CST
delimiters, the projector only unwraps. Fixture `paren_operator` (parser + dir
corpus, 13 lines incl. boundary cases `(+x)`, `(a + b)` excluded by the
next-token-`)` guard). **Deferred:** broadcast forms (`(.+)` → `(. +)`, would
project as raw `.+` not `(. +)`), the erroring syntactic ops (`(=)`, `(::)`,
`(&&)`, `(->)`, `(?)`, `(...)` — error-shape, stay UNSUPPORTED), and
parenthesized-operator macro names (`macro (:)(ex) end` js-a916f049 stays FAIL —
the macro-name parser doesn't recognize `(:)`, a separate gap).

JS allow **333 → 334** (+1: `function (:)() end` js-beb4a3a3, was UNSUPPORTED);
divergence held 237, unsupported 5 → 4. Dir allow 52 → 53. Zero regressions;
green, clippy/fmt clean. (Next targets superseded by 18k; macro paths landed.)

## Earlier session (2026-06-18i)

**`$`-interpolated names in `export`/`module`/`import`.** Each name-position
parser now recognizes a leading `$` and builds a real `INTERPOLATION` node via
the shared `parse_prefix_interpolation` (made `pub(super)` in `expr.rs`), rather
than passing `$` + operand through as loose tokens: `parse_header` (module name,
new `else if Dollar` arm), `parse_import_path` (import root, new `Some(Dollar)`
arm), and the `parse_keyword_stmt` Path passthrough (export list, `$` inside the
verbatim loop). Projector (`sexpr.rs`): `ident_run` (export) and
`project_import_path` (import) gained an `INTERPOLATION` arm routing through
`project` → `($ …)`; module's `project_signature` already projected the first
node. Faithful: the `$` sigil + operand are real CST children; the projector only
formats the wrapper. Results: `module $A end` → `(module ($ A) (block))`,
`import $A` → `(import (importpath ($ A)))`, `export $a, $(a*b)` →
`(export ($ a) ($ (call-i a * b)))`, `export ($f)` → `(export ($ f))` (parens
stripped as delimiters). Fixture `interpolation_names` (parser + dir corpus, 4
lines). **Deferred:** `import $A.@x` (js-97312f87 — needs macro-in-importpath,
which plain `import A.@x` also drops), `function $f end` (js-080efb64 —
empty-body signature shape, separate gap), dotted `import A.$B`.

JS allow **329 → 333** (+4: `export $a, $(a*b)`, `export ($f)`, `import $A`,
`module $A end`); divergence 241 → 237, unsupported held 5. Dir allow 51 → 52.
Zero regressions; green, clippy/fmt clean. (Next targets superseded by 18j.)

## Earlier session (2026-06-18h)

**Prefix `$` interpolation in expression position.** A prefix `$` is now an
interpolation everywhere, not just inside strings — Julia rejects `$` outside a
quote only during lowering, never at parse time, so the same node serves bare
`$x` → `($ x)`, the field-access RHS `f.$x` → `(. f (inert ($ x)))`, and quoted
contexts `:($x)` → `(quote-: ($ x))`. New `parse_prefix_interpolation` (`expr.rs`,
called from the `parse_prefix` `Dollar` arm) reuses the string-context
`parse_interpolation` for the `$ident`/`$(expr)` forms and otherwise binds `$` to
the next *prefix atom* — tightly, no postfix — via a recursive `parse_prefix`, so
`$$a` → `($ ($ a))`, `$[1, 2]` → `($ (vect 1 2))`, `$"s"` → `($ (string "s"))`,
while postfix still applies *outside* the `$` (`$a.b` → `(. ($ a) (quote b))`,
`$f(x)` → `(call ($ f) x)`). Projector (`sexpr.rs`): the general dispatch wraps a
standalone `INTERPOLATION` as `($ …)` (string interiors are untouched — they go
through `string_parts`, which keeps calling the inner-value `project_interpolation`
helper), and `project_binary`'s `Dot` arm inert-quotes an interpolated field name
(`(. lhs (inert ($ …)))`) while a plain name stays `(quote …)`. Faithful: the `$`
sigil and operand are real CST children; the projector only formats the wrapper.
Fixture `interpolation_expr` (parser + dir corpus, 8 lines). **Deferred:**
dotted-`$` macro paths (`A.$B.@x` js-ab3caeec → `macrocall`), `A.:.+`
(js-3a22c71b), and the `$`-in-`export`/`module`/`import` name positions
(js-47fe84f4, js-9480ed2a, js-844874ea — those need the respective stmt parsers).

JS allow **323 → 329** (+6: `$a`, `$f(x)`, `$$a`, `f.$x`, `f.$(x+y)`, `function
$f() end`); divergence 244 → 241, unsupported 8 → 5. Dir allow 50 → 51. Zero
regressions; green, clippy/fmt clean.

## Earlier session (2026-06-18g)

**Unary operator paren-calls.** A unary arithmetic/logical operator (`+ - ! ~` and
broadcast `.+ .- .~`) adjacently glued to a `(` is a function call when the parens
look like an argument list, not a parenthesized operand. The unary arm of
`parse_prefix` (`expr.rs`) gains a pre-check: when the op is one of those seven
kinds, the next token is an adjacent `(`, and the new `unary_op_paren_is_call`
predicate fires, it builds a `CALL_EXPR` (operator-token callee + `ARG_LIST` via
`parse_arg_list`) instead of the usual `UNARY_EXPR`+`PAREN_EXPR`. `unary_op_paren_is_call`
mirrors JuliaSyntax's `is_paren_call`: scanning the adjacent parens, it returns a
call iff they are empty (`+()`), open with a leading `;` (`+(; a)`), or contain — at
top-level bracket depth 0 — a comma (`+(x, y)`) or a splat `...` (`+(a...)`); a lone
bare operand (`+(x)`), a parenthesized inner tuple (`+((x, y))`), or a non-leading
`;` block (`+(a; b)`) all stay prefix `call-pre`. Reuses last session's
operator-callee projection directly; `operator_func_repr` (`sexpr.rs`) gained a `!`
special-case (`!` is unary-only, no `infix_head` entry, so it was hitting the `?`
fallback → `!(a, b)` had projected to `(call ? a b)`). Faithful: the operator token
and arg list are real CST children; the projector only formats the callee. Fixture
`unary_operator_call` (parser + dir corpus, 12 lines incl. the prefix boundary
`+(x)`). **Deferred:** rare `+(;;)` double-semi (Julia: block → prefix; the leading-`;`
check makes Fatou call it). Type-operator paren-calls (`<:(a,)`), curly operator
calls (`+{T}(...)`), standalone `(+)` still deferred.

JS allow **310 → 323** (+13 — unary operator paren-calls are common across the
corpus); divergence 257 → 244, unsupported held 8. Dir allow 49 → 50. Zero
regressions; green, clippy/fmt clean.

## Earlier session (2026-06-18f)

**Operator-as-call functions.** A non-unary binary operator glued to a `(` names
a function call with the operator as callee. `parse_prefix` (`expr.rs`) gains an
arm gated by the new `is_operator_call_name` predicate (the non-unary,
non-syntactic operators: `* / // ^ % == != < <= > >= | << >> >>> |> <| => --> <-->`
plus their broadcast forms, excluding `+ - ! ~`, `&`, `:`, `::`, `&& ||`, `->`,
`<: >:`); on an *adjacent* `(` it builds a `CALL_EXPR` whose first child is the
bare operator token plus the usual `ARG_LIST`. Projector: `project_call` now reads
the callee from the first *significant* element (was first child node), so an
operator-token callee routes through the new `operator_func_repr` helper —
`(. *)` for broadcast (via `infix_head`'s `DotCallI`), bare text otherwise — giving
`*(x)` → `(call * x)`, `.*(a,b)` → `(call (. *) a b)`. Faithful: the operator is a
real CST child, the projector only formats it. Unary ops keep their prefix parse
(`+(x)` → `(call-pre + x)`, untouched). Files: `expr.rs` (arm + predicate),
`sexpr.rs` (`project_call` callee loop + `operator_func_repr`). Fixture
`operator_call` (parser + dir corpus: `*(x) .*(x) /(x,y) ==(a,b) %(x) .==(a,b)
=>(x,y) |(x) *(x,y,z) *()`). **Deferred:** unary operator arglist-calls (`+(a...)`
→ `(call + (... a))`, `+(a;b,c)`, `+(x,y)` — needs JuliaSyntax's `is_paren_call`
heuristic over commas/splat/semis), type-operator `<:(a,)` → `(<: a)`, curly
operator calls (`+{T}(x::T)`), standalone parenthesized operators (`(+)` → `+`).

JS allow **308 → 310** (+2: `*(x)` js-4766b25e, `.*(x)` js-ddc5134e); divergence
259 → 257, unsupported held 8. Dir allow 48 → 49. Zero regressions; green,
clippy/fmt clean.

## Earlier session (2026-06-18e)

**Paren-quoted operators.** `parse_quote_sym` (`expr.rs`) gained an `LParen` arm
guarded by `is_paren_quotable_op`: when `:` is followed by `( op )` whose interior
is a lone undotted operator, it builds a `PAREN_EXPR` wrapping the bare operator
token instead of calling `parse_paren` (which errors on a lone op). The new
predicate accepts `is_op_name` plus the undotted assignment ops and the *syntactic*
`=`/`::`/`:` — these are errors in value position but valid as quoted symbols. The
projector's `PAREN_EXPR | CONDITION` arm now falls back, when there's no inner node,
to the first significant `is_operator` token's text, so `(=)`/`(::)`/`(+)` inside a
quote project to `=`/`::`/`+` and the whole quote to `(quote-: …)`. Faithful: the
parens stay in the CST, the projector only unwraps. Files: `expr.rs` (arm +
`is_paren_quotable_op`), `sexpr.rs` (PAREN_EXPR fallback). Fixture
`operator_symbol_quote_paren` (parser + dir corpus: `:(=) :(::) :(:) :(+) :(&&)
:(<:) :(+=) :(==)`). **Deferred:** broadcast paren-quotes (`:(.=)` → `(quote-: (. =))`),
standalone parenthesized operators (`(+)` → `+`, still ERROR — Julia distinguishes
quote-context where `=`/`::` are values from value-context where they're errors),
and import paren-quotes (`import A.:(+)`, `import A.(:+)` — need `parse_import_path`
surgery).

JS allow **305 → 308** (+3: `:(=)`, `:(::)`, `:(::\n)`); divergence 261 → 259,
unsupported 9 → 8. Dir allow 47 → 48. Zero regressions; green, clippy/fmt clean.

## Earlier session (2026-06-18d)

**Prefix operator-symbol quoting.** `parse_quote_sym` (`expr.rs`) gained one arm:
after the `:`, an undotted operator-name token (`is_op_name`, now `pub(super)` and
imported from `structural.rs`) or an assignment operator (`is_assignment_op`) is
emitted as a bare symbol token, so `:+`/`:<:`/`:>:`/`:+=`/`:&`/`:!` → `(quote-: …)`.
The projector already mapped a bare-token `QUOTE_SYM` child to `(quote-: <text>)`,
so `sexpr.rs` was untouched (faithful). Whitespace matters: Julia treats `: +` and
`: foo` as errors (`(quote-: (error-t) +)`), and `:.+`/`:.=` quote to `(. +)`/
`(. =)` (broadcast), and `:==` lexes as `:=`+`=` (deprecated `:=` token) — all left
unhandled/deferred. Files: `expr.rs` (arm + import), `structural.rs` (visibility).
Fixture `operator_symbol_quote` (parser + dir corpus, `:+= :<: :>: :+ :& :!`).
**Deferred:** paren-quoted operators (`:(=)`→`(quote-: =)`, `:(::)`→`(quote-: ::)`,
needs quote-context paren parsing where lone ops are values), broadcast quotes
(`:.+`), and dotted `A.:+` (UNSUPPORTED, dotted field access + quote).

JS allow **302 → 305** (+3: `:+=`, `:<:`, and `function (:*=(f))() end`);
divergence 262 → 261, unsupported 11 → 9. Dir allow 46 → 47. Zero regressions;
green, clippy/fmt clean.

## Earlier session (2026-06-18c)

**Operator-symbol import names.** `parse_import_path` (`structural.rs`) gained
operator components in three positions: bare name in the `:` list (`import A: +,
==`, `import Base: +, -, *`), fused dotted operator component (`import A.==` — the
lexer merges `.==` into one `DOT_EQ_EQ` token whose *leading dot is the separator*,
not broadcast; the projector strips it via `trim_start_matches('.')`), and quoted
operator after a dot (`import A.:+` → a `QUOTE_SYM` node wrapping `:` + op →
`(importpath A (quote-: +))`, reusing `project_quote_sym`, no `parse_quote_sym`
change). Two new TokKind predicates `is_op_name` (undotted symbolic ops, excludes
`:`/dots/assignment) and `is_dotted_op_name` (the `.+`/`.==` broadcast tokens) gate
the first-name and loop arms; projector reuses its existing `is_operator(SyntaxKind)`
and ignores separator `DOT`/`COLON`. Files: `structural.rs` (parser + predicates),
`sexpr.rs` (`project_import_path` arms). Fixture `import_operator_names` (parser +
dir corpus, 6 lines). **Deferred:** unicode ops (`import .⋆`, `A.⋆.f` — `⋆` lexes
as `ERROR`, needs unicode-operator lexing) and paren-quoted forms (`A.:(+)`,
`A.(:+)`).

JS allow **299 → 302** (+3: `import A.:+`, `import A.==`, `import A: +, ==`);
divergence 265 → 262, unsupported held 11. Dir allow 45 → 46. Zero regressions;
green, clippy/fmt clean.

## Earlier sessions

- **2026-06-18b** — Arrow, pipe, and bitshift operators: `-->` (Special head),
  `<-->`, broadcast `.-->` on the arrow tier `(4,3)`; pipes split into `<|` `(12,11)`
  and `|>`/`.|>` bumped to `(13,14)`; bitshift `<< >> >>>` at `(30,31)` (Julia prec
  14 ⇒ tighter than `*`, looser than `^`). Fixture `arrow_pipe_bitshift_operators`.
  JS allow 292 → 299.

- **2026-06-18a** — Generator arguments & typed comprehensions: `parse_postfix`
  speculatively parses the first bracketed element and, on a following `for`, builds
  a `GENERATOR` (call-arg `sum(x for …)`) or `TYPED_COMPREHENSION` (`T[x for …]`)
  instead of an `ARG_LIST`; projector gains a `GENERATOR`-child branch +
  `project_typed_comprehension`. JS allow 291 → 292. Fixture `generator_arguments`.

- **2026-06-17g** — Multi-clause & comma generators: replaced single-clause
  `parse_comprehension` with a `for`-clause loop + `parse_for_specs` (each `for` a
  sibling `FOR_BINDING`, comma specs as tokens, `a = as` form an `ASSIGNMENT_EXPR`);
  projector `project_for_binding_node` splits on top-level commas into
  `cartesian_iterator`, `project_generator` folds trailing `if` into `filter`. Also
  fixed the for-*loop* `for x in xs, y in ys` (js-ae2710c2). JS allow 282 → 291.
  Fixture `multi_clause_generators`.

- **2026-06-17f** — Richer `import`/`using` path trees: dedicated `parse_import_stmt`
  building real `IMPORT_PATH`/`IMPORT_ALIAS` nodes the projector reads (no
  reconstruction); leading-dot expansion, `:` switches base→name-list, `as` is a
  contextual ident. JS allow 274 → 282. Deferred: operator-symbol/`@macro`/`$interp`
  names, `export` list. Trap: scratch-buffer the clause, commit whitespace only on
  success, else verbatim passthrough double-emits.

- **2026-06-17e** — Range operator `..`: `DotDot` 2-char op (longest match `...` >
  `..` > `.`), placed after the splat check, before the broadcast-`.` block; a
  `lex_number` guard (`peek(1) != Some(b'.')`) keeps `1..n` from lexing as float
  `1.` + `.n`. Shares colon tier `(14,15)`, ordinary `BINARY_EXPR` → `(call-i a ..
  b)`. JS allow 273 → 274. New FAIL `x..y...` (splat-precedence gap, deferred).
  Fixture `range_operator`.

- **2026-06-17d** — Broadcast short-circuit `.&&`/`.||`: 5-file recipe (infix-only,
  no prefix); `DotAndAnd`/`DotOrOr` in the 3-char dotted table, share `&&`/`||`
  tiers, project to their own `Special(".&&")`/`Special(".||")` heads (not
  `dotcall-i`). JS allow 271 → 273. Fixture `dot_logical_operator`.

- **2026-06-17b** — Augmented assignment `op=` (parity-driven ASCII set): 16
  TokKinds/SyntaxKinds for `+= -= *= /= //= ^= %= |= &=` + broadcast `.+= … .%=`.
  Lexer longest-match (`.//=`>`.//`, `//=`>`//`); an `is_assignment_op` helper folds
  them into the existing `ASSIGNMENT_EXPR` arm + `(2,1)` tier; `project_assignment`
  reads the head from operator-token text. `global`/`let` free. JS allow 259 → 264.

- **2026-06-17a** — Built the oracle from scratch + ran the loop 3×: JuliaSyntax
  differential oracle (projector `sexpr.rs` + `--to sexpr`, harness, curated +
  harvested corpora, refresh scripts); `a[begin]` index marker (+1 JS); `:foo` /
  `:(x+1)` symbol quotes via `parse_quote_sym` (+5 JS); pair operator `=>`/`.=>`
  on arrow tier `(4,3)` (+2 JS). JS allow 251 → 259.
