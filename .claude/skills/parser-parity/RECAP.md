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

JS corpus (575 cases): **373 allowlisted**, 198 divergence, 4 unsupported.
Dir corpus: **65 allowlisted**, 5 blocked + 1 skipped (do_blocks).
Grammar bullets through "non-standard identifiers `var\"…\"`" are `[x]`
in `TODO.md`.

Deliberate (recorded) divergences, do not "fix": comparison chains (nested),
associative `a*b*c` (nested binary), numeric-literal display normalization,
triple-string dedent, `end`/`[1 +2]`/unterminated-string/incomplete-`do` error
shapes (dir `blocked.txt`).

## Latest session (2026-06-20e)

**Non-standard identifiers `var"…"`.** A `var` prefix glued to a single-quoted
string is a non-standard *identifier*, not a string macro — Julia models
`var"x"` as `(var x)`, not `(macrocall @var_str …)`. Fatou was lexing it through
the generic string-macro path (`STRING_PREFIX` "var" + string → `STRING_LITERAL`
→ `(macrocall @var_str (string-r "x"))`). **Fix (parser, no lexer change):** in
`parse_string_literal` (`expr.rs`), when the prefix token's text is exactly `var`
and the open delimiter is a single `"` (len 1), build a new
`NONSTANDARD_IDENTIFIER` node instead of `STRING_LITERAL`; the same token run
(prefix, open, content, close) is reused. Triple-quoted `var"""…"""` keeps len ≥ 3
so it stays an `@var_str` macrocall, and `r`/`raw`/`b` prefixes are untouched.
Projector (`sexpr.rs`): one arm + `project_var` heads the node `var` over the raw
delimited content (`raw_content`) — empty content → `(var)` (normalizes equal to
Julia's `(var )`). Faithful: the `var`/delim/content tokens are real CST children;
the projector only formats the head and drops the delimiters. Results match Julia:
`var"x"`→`(var x)`, `var""`→`(var)`, `var"#"`→`(var #)`, `f(var"x")`→`(call f
(var x))`, `var"x" + 1`→`(call-i (var x) + 1)`. Fixture `nonstandard_identifier`
(parser fixture has 7 lines incl. the triple-`var` macrocall guard; oracle dir
fixture trims that line — triple prefix strings project `string-r` not `string-s-r`,
a separate pre-existing encoding gap). **Deferred:** name escape-processing
(`var"\""` → `(var ")` follows Julia's raw-string rules; escape-free names match,
but `var"\""` js-61a01ce8 / `var"\\\""` js-8f5b1a26 stay FAIL — though `var"\\x"`
js-d855305c passed as a bonus since raw source == show output) and the
suffix-error shape (`var"x"y` → `(var x (error-t))`).

JS allow **370 → 373** (+3: `var""` js-7a6211e2, `var"x"` js-aae88021, `var"\\x"`
js-d855305c); divergence 201 → 198, unsupported held 4. Dir allow 64 → 65. Zero
regressions; green, clippy/fmt clean.

**Suggested next targets (ranked):**
1. **Unicode operators** (lexer) — still the single largest remaining lexer
   feature; unblocks `√x` (js-e13fa52a), `x → y` (js-db694f69), `i ∈ rhs`
   (js-f3da47b9), `a … b` (js-e5d8580f), `⊻`, `export ⤈`/`public ⤈`, `import .⋆`,
   `A.⋆.f`, `x -->₁ y`, `a +₁ b +₁ c`.
2. **Triple-quoted prefix string macros** — `string-r` vs `string-s-r` in
   `project_string`'s macrocall branch (head should be `string-s-r`/`cmdstring-s-r`
   when the open delim is triple). Small encoding fix; would also let the triple
   `var"""…"""` guard line join the oracle corpus.
3. **`function (x)::T end`** — `(x)` is a `tuple-p` nested under `::-i`; extend the
   signature relabel to descend the `::` LHS (not just the outermost paren).

## Earlier sessions

One-liners; full implementation detail lives in the matching `[x]` bullet in
`TODO.md`.

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
