# parser-parity recap

Rolling log. Read top-to-bottom: persistent traps → progress → latest session →
earlier log. Keep ≤ ~300 lines; demote the "Latest session" to a one-liner in the
"Earlier sessions" list each new session.

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

JS corpus (575 cases): **386 allowlisted**, 185 divergence, 4 unsupported.
Dir corpus: **68 allowlisted**, 5 blocked + 1 skipped (do_blocks).
Grammar bullets through "Operator suffix sub/superscripts" are `[x]` in `TODO.md`.

Deliberate (recorded) divergences, do not "fix": comparison chains (nested),
associative `a*b*c` (nested binary), n-ary juxtaposition `(2)(3)x` (nests right),
numeric-literal display normalization, triple-string dedent,
`end`/`[1 +2]`/unterminated-string/incomplete-`do` error shapes (dir `blocked.txt`).

## Latest session (2026-06-20h)

**Operator suffix sub/superscripts.** An operator token now absorbs a trailing
run of sub/superscript or prime chars (`a +₁ b`, `x -->₁ y`, `f'ᵀ`, `a .+₁ b`),
keeping its *kind* (so binding power is untouched) and only growing its text.
**Lexer:** new `push_op(kind, start)` consumes `is_op_suffix_char` runs when
`op_takes_suffix(kind)`; all operator-push sites in `lex_operator_or_unknown` plus
the `Transpose` site route through it. `op_takes_suffix` mirrors JuliaSyntax's
`optakessuffix` — **excluded:** assignments (`= op= .= .op=`), `&& || .&& .||`,
`<: >: : :: .. ... ! ~ .~ -> ? $`, and the radicals/`UniAssign`; **included:** the
arithmetic/comparison/bitwise/arrow/pipe/transpose ops and their broadcast forms,
plus the unicode tiers (verified each via `Tokenize.tokenize` byte ranges, not
guessed). `is_op_suffix_char` checks the explicit JuliaSyntax suffix-char set
(combining-mark categories Mn/Mc/Me are a deferred pragmatic subset, like
`is_unicode_ident`). **Projector** (`project_binary`): a suffixed operator
(`op_has_suffix` = last char is a suffix char) emits a generic `(call-i …)` /
`(dotcall-i …)` with the **full text**, even when the base op is syntactic —
`-->₁`→`(call-i x -->₁ y)`, not `(--> …)` — matching Julia, where a suffix makes
the op non-syntactic. `f'ᵀ` needed no projector change (`project_postfix` already
reads token text). **Array-split fix** (`array_element_boundary`): the
whitespace-sensitive `[a +b]`→`[a, +b]` split was firing for *any* operator (a
latent bug: `[a *b]` wrongly became `(hcat a b)`). Now gated on
`op_can_lead_array_element` = genuinely unary-capable infix ops {`+ - .+ .- & ~ .~`}
∪ symbol-quote `:` and **never a suffixed op**. So `[a *b]`/`[a ::b]`/`[a <:b]`
→ `(vect …)`, `[x +₁y]`→`(vect (call-i x +₁ y))`, while `[a +b]`/`[1 :a]` still
split (`hcat`). Mirrors JuliaSyntax `is_both_unary_and_binary` + the `:`-quote path.

JS allow **382 → 386** (+4: `a +₁ b +₁ c` js-db974d0b, `x -->₁ y` js-50dc84fd,
`[x +₁y]` js-97cf44fb, `f'ᵀ` js-90fb496e); divergence 189 → 185, unsupported held 4.
Dir allow 67 → 68 (`operator_suffixes`). Fixed two transient regressions from the
array gate (`[1 :a]` js-ac8bb0f2, `[1 2:3 :a]` js-a73c4b25) by adding `:` back to
the lead set. Zero net regressions; green, clippy/fmt clean. New parser snapshot
`array_space_unary` locks the corrected splitting. **Deferred:** unicode unary in
plus/times tiers (`±x`, not yet a prefix), combining-mark suffix categories.

**Suggested next targets (ranked):**
1. **Triple-quoted prefix string macros** — `string-r` vs `string-s-r` in
   `project_string`'s macrocall branch (still open from 2026-06-20e).
2. **Unicode unary in plus/times tiers** — `±x`/`∓x`/`⋆x` as prefixes
   (`is_both_unary_and_binary` set); needs a `parse_prefix` arm for the relevant
   unicode chars and would also let `[a ±b]` split correctly.
3. **Comparison/`<:` chains and unicode comparison chains** — recorded as nested
   divergences; revisit only if flattening is wanted.

## Earlier sessions

One-liners; full implementation detail lives in the matching `[x]` bullet in
`TODO.md`.

- **2026-06-20g** — Numeric-literal juxtaposition (implicit multiplication): an
  adjacent glued value with no operator → `JUXTAPOSE_EXPR`/`(juxtapose a b)` via
  `should_juxtapose` (faithful to `is_juxtapose`); binding powers `(32,31)`
  (tighter than `*`, looser than `^`); `parse_postfix_chain` guard so `2(x)` is
  `(juxtapose 2 x)` not a call. JS allow 377 → 382. Fixture `juxtaposition`.

- **2026-06-20f** — Unicode operators (single-codepoint infix/prefix): the whole
  faithful set generated into `src/parser/unicode_ops.rs` (code-point-sorted
  binary-search table, classified by `is_prec_*`); lexer `None` fallback looks the
  char up; 8 tier `TokKind`s → 3 `SyntaxKind`s; binding powers mirror ASCII
  siblings; radicals `√ ∛ ∜ ¬` route through the unary arm. JS allow 373 → 377.
  Fixture `unicode_operators`.

- **2026-06-20e** — Non-standard identifiers `var"…"`: a `var` prefix + single-`"`
  open delim builds a `NONSTANDARD_IDENTIFIER` (not a string macro) in
  `parse_string_literal`; `project_var` heads `var` over the raw content. `var"x"`→
  `(var x)`, `var""`→`(var)`. JS allow 370 → 373. Fixture `nonstandard_identifier`.

- **2026-06-20d** — Broadcast bitwise `.&`/`.|`: `DotAmp`/`DotPipe` in the 2-char
  dotted table (3-char `.&&`/`.||`/`.|>` win first), mirror undotted tiers (`.&`
  times `(24,25)`, `.|` plus `(20,21)`), `infix_head` `DotCallI`; `.&(x,y)`→
  `(call (. &) x y)`. JS allow 369 → 370. Fixture `broadcast_bitwise_operators`.

- **2026-06-20c** — `abstract type`/`primitive type` decls: contextual keyword
  pair (`abstract`/`primitive` ident + `type` ident) dispatched before the
  block-keyword match; spec parsed as a real expr into `SIGNATURE`, `primitive`
  bit-size a sibling node. New `ABSTRACT_DEF`/`PRIMITIVE_DEF`. JS allow 359 → 369.
  Fixture `abstract_primitive_type`.

- **2026-06-20b** — ASCII bitwise `&`/`|`: add `Amp` to times `(24,25)`, `Pipe`
  to plus `(20,21)` tiers (infix); prefix `&x`→`(& x)` via the unary arm (excluded
  from the paren-call gate). JS allow 358 → 359. Fixture `ampersand_operator`.

- **2026-06-20a** — Anon `function (args)…end` signatures as arg tuples: relabel a
  lone `(x)` `PAREN_EXPR`→`TUPLE_EXPR` in `parse_function_like` when it is not
  "eventually a call" (`signature_eventually_call` mirrors JuliaSyntax). JS allow
  356 → 358. Fixture `anon_function_signature`.

- **2026-06-18q** — Field-access suffixes: a `()`/`[]`/`{}` glued after `a.b` was
  binding to the field name; fix = parse the Dot RHS prefix-only so the suffix
  attaches to the whole access (`A.f()` = `(A.f)()`). JS allow 352 → 356. Fixture
  `field_access_suffix`.

- **2026-06-18p** — Curly operator calls: an operator glued to `{` is a parametric
  callee (`+{T}`→`(curly + T)`) via `is_curly_operator_name`; `::`/`&`/`:`
  excluded. JS allow 350 → 352. Fixture `curly_operator_call`.

- **2026-06-18o** — `public` contextual keyword: `public A, B`/`public @a` open a
  `PUBLIC_STMT` at toplevel/module scope unless the next sig token is `( = [`
  (`public_context` flag). JS allow 346 → 350. Fixture `public_statement`.

- **2026-06-18n** — `macro` definitions: `macro m(ex)…end` reuses
  `parse_function_like` (`MACRO_DEF` vs `FUNCTION_DEF`); `macro`/`MACRO_KW`
  keyword. JS allow 341 → 346. Fixture `macro_definition`.

- **2026-06-18m** — Type-operator paren-calls: `<:`/`>:` glued to `(` follow the
  `is_paren_call` heuristic → `(<: a b)`; `project_call` overrides the head with
  `operator_func_repr`. JS allow 340 → 341. Fixture `type_operator_call`.

- **2026-06-18l** — Import paren-quotes: `import A.:(+)`/`import A.(:+)`→
  `(importpath A (quote-: +))` by delegating to `parse_quote_sym`. JS allow
  338 → 340. Fixture `import_paren_quote`.

- **2026-06-18k** — Macro names in `export`/`import`/`using`: `@` builds a real
  `MACRO_NAME` node via `push_macro_name`; `export @a`, `import A.@x`. JS allow
  334 → 338. Fixture `macro_directive_names`.

- **2026-06-18j** — Standalone parenthesized operators: `(+)`→`+`, `(:)`→`:` via
  an `is_paren_value_op` arm in `parse_paren`; projector unchanged. JS allow
  333 → 334. Fixture `paren_operator`.

- **2026-06-18i** — `$`-interpolated names in `export`/`module`/`import`: each
  name parser recognizes a leading `$` → `INTERPOLATION` via
  `parse_prefix_interpolation`. JS allow 329 → 333. Fixture `interpolation_names`.

- **2026-06-18h** — Prefix `$` interpolation in expression position:
  `parse_prefix_interpolation` binds `$` to the next prefix atom; `$x`/`f.$x`/
  `:($x)`. JS allow 323 → 329. Fixture `interpolation_expr`.

- **2026-06-18g** — Unary operator paren-calls: a unary `+ - ! ~ .+ .- .~` glued
  to `(` is a call when the parens look like an arglist (`unary_op_paren_is_call`
  mirrors `is_paren_call`). JS allow 310 → 323. Fixture `unary_operator_call`.

- **2026-06-18f** — Operator-as-call functions: a non-unary binary op glued to `(`
  is a callee (`is_operator_call_name`); `*(x)`→`(call * x)` via
  `operator_func_repr`. JS allow 308 → 310. Fixture `operator_call`.

- **2026-06-18e** — Paren-quoted operators: `:(=)`/`:(::)`/`:(+)` via a
  `parse_quote_sym` LParen arm (`is_paren_quotable_op`); PAREN_EXPR fallback to the
  operator text. JS allow 305 → 308. Fixture `operator_symbol_quote_paren`.

- **2026-06-18d** — Prefix operator-symbol quoting: `:+`/`:<:`/`:+=`/`:&`/`:!`→
  `(quote-: …)` via a bare-symbol-token arm in `parse_quote_sym`. JS allow
  302 → 305. Fixture `operator_symbol_quote`.

- **2026-06-18c** — Operator-symbol import names: `import A: +, ==`, `import A.==`
  (fused `.`-separator), `import A.:+` (quoted); `is_op_name`/`is_dotted_op_name`.
  JS allow 299 → 302. Fixture `import_operator_names`.

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
