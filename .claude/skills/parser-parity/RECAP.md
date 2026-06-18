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

JS corpus (575 cases): **334 allowlisted**, 237 divergence, 4 unsupported.
Dir corpus: **53 allowlisted**, 5 blocked + 1 skipped (do_blocks).
Grammar bullets through "standalone parenthesized operators" are `[x]` in
`TODO.md`.

Deliberate (recorded) divergences, do not "fix": comparison chains (nested),
associative `a*b*c` (nested binary), numeric-literal display normalization,
triple-string dedent, `end`/`[1 +2]`/unterminated-string/incomplete-`do` error
shapes (dir `blocked.txt`).

## Latest session (2026-06-18j)

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
green, clippy/fmt clean.

**Suggested next targets (ranked):**
1. **Macro paths in `import`/`using`** (`import A.@x` → `(importpath A @x)`,
   js-97312f87 once `$` root combines) — `parse_import_path` `.@name` component;
   then `import $A.@x` falls out.
2. **Import paren-quotes** (`import A.:(+)` js-0492d7fb, `import A.(:+)`
   js-6fe4ce2d) — finishes the quoting cluster; `parse_import_path` surgery.
3. **Type-operator paren-calls** (`<:(a, b)` → `(<: a b)`, `<:(a,)` → `(<: a)`,
   js-70cde333) — extend the unary paren-call path to `Subtype`/`Supertype`.
4. **Parenthesized-operator macro names** (`macro (:)(ex) end` js-a916f049) — the
   macro-name parser needs to accept a `(op)` signature like `function` now does.
5. **Unicode operators** (lexer) — unblocks `import .⋆`, `A.⋆.f`, `[x +₁y]`,
   `a … b`, many scattered FAILs; larger lexer feature.

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
