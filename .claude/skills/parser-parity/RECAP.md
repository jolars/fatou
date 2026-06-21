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

JS corpus (575 cases): **503 allowlisted**, 68 divergence, 4 unsupported.
Dir corpus: **89 allowlisted**, 4 blocked (1 skipped: do_blocks).
Grammar bullets through "bare operator value atoms" are `[x]` in `TODO.md`.

Deliberate (recorded) divergences, do not "fix": comparison chains (nested),
associative `a*b*c` (nested binary), n-ary juxtaposition `(2)(3)x` (nests right),
numeric-literal display normalization,
`end`/`[1 +2]`/unterminated-string/incomplete-`do` error shapes (dir `blocked.txt`).

## Latest session (2026-06-21n)

**Typed + brace concatenation.** `T[x y]` → `(typed_hcat T x y)`, `T[a;b]` →
`(typed_vcat T a b)`, `T[a ;; b]` → `(typed_ncat-2 T a b)`, `T[;]` →
`(typed_ncat-1 T)`; `{x y}` → `(bracescat (row x y))`, `{a;b}` → `(bracescat a b)`,
`{a;;b}` → `(bracescat (nrow-2 a b))`, `{;}` → `(bracescat (nrow-1))`.
`parse_matrix`/`parse_empty_ncat` (`expr.rs`) were parametrized on the close token
plus the comma-form and matrix-form node kinds, so all three delimiters reuse the
same scan + `MATRIX_ROW` nesting. Typed: new `parse_typed_concat` runs after the
comprehension check in `parse_postfix` (RBracket only); a space/`;`-separated body
→ `TYPED_MATRIX_EXPR` wrapping the type expr + a `MATRIX_EXPR` (a lone element with
only a trailing newline collapses to `VECT_EXPR` → stays a `ref`). Braces:
`parse_braces` now dispatches comma/single/empty → `BRACES`, else
`BRACESCAT_EXPR`. Projector: `matrix_head_and_children` factored out of
`project_matrix`; `project_typed_matrix` prepends the type and prefixes `typed_`;
`project_bracescat` always heads `bracescat` (dim-1 keeps children, dim-0/≥2 and
empty nest a single `row`/`nrow-d` child, since bracescat is itself the dim-1
container). Fixtures `typed_concat`, `bracescat`.

JS allow **496 → 503** (+7), divergence 75 → 68, unsupported held 4. Dir allow
87 → 89 (+2 fixtures). Zero regressions; green, clippy/fmt clean.

**Suggested next targets (ranked):**
1. **Remaining macro-name forms**: `@(A)` paren name (js-f3aa762e ⇒ `@A`),
   `@S[a].b`/`@S{a}.b` (js-b55b2b19/b6643c20 — macrocall then postfix `.b`).
2. **Concat newline/whitespace edges** (mostly error-shape, lower value):
   `[x \n, ]`, `[x \n\n ]`, `[a b ;; \n c]`, `[f (x)]` space-call — several emit
   Julia `(error-t)` recovery, so check each before committing.
3. Survey the remaining 68 JS FAILs for the next cluster (`cargo test --test
   juliasyntax_oracle -- --ignored juliasyntax_full_report`).

## Earlier sessions

- **2026-06-21m** — N-dimensional concatenation (`;;`/`;;;`). `parse_matrix`
  rewritten to scan elements + dimension-tagged `SepRun`s and recursively nest
  `MATRIX_ROW`s at each level's max dimension; projector `project_matrix`/
  `project_cat_child`/`group_dimension` recover dimension from `;`/newline tokens,
  heading `hcat`/`vcat`/`ncat-d` (top) or `row`/`nrow-d` (nested). Element-free
  `[;]`/`[;;]` via `parse_empty_ncat`. JS allow 482 → 496. Fixture `ncat`.

- **2026-06-21l** — `var"…"` macro names. `@var"#"` ⇒ `(macrocall (var @#))`,
  qualified `A.@var"#"`, `export @var"#"` via shared `push_var_macro_name`
  (`expr.rs`); triple-quoted `@var"""…"""` stays an ordinary macrocall.
  `project_macro_name` folds the `@` into the var content. JS allow 479 → 482.
  Fixture `var_macro_name`.

- **2026-06-21k** — Nested dotted macro paths. `@A.B.x`, `A.B.@x`, `$A.@x`,
  `A.$B.@x`, `A.@.x` project to nested `(. (. A (quote B)) (quote @x))` like field
  access. Pure projector: `project_macro_name` branches trailing form (reuses
  `project` on the module node, name via `macro_name_after_at`) vs prefix form
  (folds flat components). JS allow 474 → 479. Fixture `nested_macro_path`.

- **2026-06-21j** — Operator/keyword macro names. A macro name after `@` may be an
  operator (`@+`, `@!`, `@..`), the `$` sigil (`@$`), or a keyword (`@end`):
  `parse_macro_name_body` (`expr.rs`) consumes one such token via the new
  `is_macro_name_token` predicate (minus `Dot`/`Colon`); the projector's
  `is_macro_name_part_token` reads it back. JS allow 469 → 474. Fixture
  `macro_operator_names`.

- **2026-06-21i** — Bare operator value atoms. A non-syntactic operator with no
  operand to its right is the operator used as a *value* (`+` ⇒ `+`, `.&` ⇒
  `(. &)`, `<:` ⇒ `<:`); new `OPERATOR_ATOM` `SyntaxKind`, two `expr.rs` entry
  points (unary-prefix no-operand branch + a fallback arm via the new
  `is_value_operator` predicate, undotted `is_op_name` minus `&& || ->` plus the
  broadcast set and `: .. √`); projector `project_operator_atom`. The erroring
  syntactic ops (`= :: && || -> ? . ...` + assignment) stay deferred error-shape.
  Trap (deferred): prefix ops consume an operand *across a newline* (`-\nx` ⇒
  `(call-pre - x)` vs Julia's two statements). JS allow 461 → 469. Fixture
  `bare_operator`.

- **2026-06-21h** — Docstring attachment (`"doc"\nfoo` ⇒ `(doc (string "doc")
  foo)`). A bare unprefixed `STRING_LITERAL` statement directly followed by
  another (≤1 newline trivia, no `;`, no blank line) folds into a `DOC` node via
  one recursive post-pass `fold_docstrings` (`core.rs`) over the flat event stream
  before `build_tree` — block bodies flatten up, so one pass covers toplevel,
  `;`-lines, and nested function/module/begin bodies. JS allow 455 → 461. Fixture
  `docstring`.

- **2026-06-21g** — Bare-name function/macro forward declarations (`function f
  end`, `macro m end`, `function $f end` ⇒ `(function f)`/`(macro m)`/`(function
  ($ f))`). Pure projector: `project_function_like` drops the empty `BLOCK` when
  the signature inner node is a bare `NAME`/`INTERPOLATION` (`is_forward_declaration`);
  faithful since a bare-name header is only ever a declaration. JS allow 450 →
  455. Fixture `function_forward_decl`. `function \n f() end` (js-e811d4a1) stays
  FAIL — newline right after the keyword mis-parses the signature as a block.

- **2026-06-21f** — Single-quoted string escape processing + line continuations.
  Projector `string_parts` now computes the *value* (`decoded_string_parts` →
  `decode_string_chunks` + `escape_string_value`); `\`-newline continuations split
  chunks; shared `decode_escape_into`/`control_escape` with the char path. Parser:
  `consume_body_byte` consumes the whole `\r\n` with the backslash. JS allow 443 →
  450. Fixture `string_escapes`.

- **2026-06-21e** — Char literal escape decoding (`'\xce\xb1'`, `'α'`,
  `'\U1D7DA'`): lexer scans a char to its closing `'` (skip an escape's following
  byte) so multi-escape literals are one `CHAR`; `project_char` → `decode_char`
  (source escapes → one codepoint via a byte buffer) → `display_char` (JuliaSyntax
  `Char` show). JS allow 440 → 443. Fixture `char_escapes`.

- **2026-06-21d** — Raw triple-quoted strings (`r"""…"""`): `project_string`'s
  prefixed branch emits a `string-s-r` body via the same `triple_string_parts`
  dedent as a plain triple, threading `raw: bool` to `escape_display` so raw
  bytes' `\\`/`\"`/`\$` escape on top of control chars. JS allow 437 → 440.
  Fixture `raw_triple_string`.

- **2026-06-21c** — Triple-quoted string dedent (largest FAIL cluster, ~22 JS).
  Projector concern: CST stays lossless (raw `STRING_CONTENT`); `triple_string_parts`
  (`sexpr.rs`) computes the literal value JuliaSyntax-style — normalize CRLF/CR→LF,
  one `String` chunk per line, strip longest common leading-ws over lines 2..end
  (skip blank lines except the closing/last; opening line never dedented), drop the
  newline right after `"""`, append each line's `\n`, drop empty chunks,
  display-escape control chars. Empty literals emit one empty `String`
  (`""→(string "")`, `""""""→(string-s "")`). JS allow 415 → 437. Fixture
  `triple_string_dedent`.

- **2026-06-21b** — Per-group `parameters`: each `;` after the first opens a fresh
  `PARAMETERS` group (`(a; b; c,d)` ⇒ `(tuple-p a (parameters b) (parameters c d))`,
  `f(a; b; c)` ⇒ `(call f a (parameters b) (parameters c))`), via `parse_arg_list`
  closing the open group before opening a new one; projector unchanged. JS allow
  411 → 415. Fixture `multi_param_groups`. Deferred: empty-all-semis `+(;;)`.

- **2026-06-21a** — Paren block sequences: a `;`-bearing parenthesized run that is
  *not* a tuple parses as a `PAREN_BLOCK` projecting `(block-p …)` (`(a; b; c)` ⇒
  `(block-p a b c)`), via `paren_is_block`'s depth-0 token scan + the `is_tuple`/
  `is_block` rule; the two `;`-reaching `parse_arg_list` call sites pick the kind
  via `paren_list_kind`. `function (x; y) end` signatures relabel back to
  `TUPLE_EXPR`. JS allow 404 → 411. Fixture `paren_block`.

- **2026-06-20l** — Top-level `;` grouping: a logical line carrying a top-level
  `;` folds its statements into a `TOPLEVEL_SEMICOLON` node (`(toplevel-; …)`); the
  `parse` driver (`core.rs`) now works one newline-delimited line at a time,
  wrapping only when the line saw a `;`. Scoped to toplevel — `begin`/module blocks
  don't group. JS allow 398 → 404. Fixture `toplevel_semicolon`.

- **2026-06-20k** — Bare-comma tuples: a top-level comma at statement scope folds
  operands into `BARE_TUPLE_EXPR`/`(tuple …)` (vs parenthesized `tuple-p`), via a
  `stmt_comma` flag and `parse_comma_tuple` in the Pratt loop; comma binds tighter
  than `=` but looser than every real op, so `a, b = c, d` ⇒
  `(= (tuple a b) (tuple c d))`. JS allow 394 → 398. Fixture `bare_tuple`.

- **2026-06-20j** — Stepped colon ranges: `a:b:c` folds three operands into one
  infix colon call (`(call-i a : b c)`) rather than nesting two binary colons,
  via `parse_colon_range` + new `RANGE_EXPR` (mirrors JuliaSyntax `parse_range`'s
  n_colons fold; odd trailing colon falls back to `BINARY_EXPR`). JS allow
  392 → 394. Fixture `colon_range`.

- **2026-06-20i** — Signed numeric literals: a `+`/`-` glued to an adjacent number
  folds into a single signed `LITERAL` (`-2`, `+2.0` ⇒ `2.0`) via
  `signed_literal_fold` in `parse_prefix` (undotted+unsuffixed op, no whitespace,
  decimal for either sign + unsigned bin/hex/oct for `+` only; no fold before
  `^`/`[`/`{`); `project_literal` combines the two tokens, `lhs_is_number`
  juxtaposes them. Un-blocked `matrices` (`[1 +2]` ⇒ `(hcat 1 2)`). JS allow
  386 → 392.

- **2026-06-20h** — Operator suffix sub/superscripts: an operator token absorbs a
  trailing run of `is_op_suffix_char` chars (`a +₁ b`, `x -->₁ y`, `f'ᵀ`) keeping
  its *kind* (binding power untouched), text-only growth via lexer `push_op` gated
  on `op_takes_suffix` (mirrors `optakessuffix`); `project_binary` emits a suffixed
  op as a generic `(call-i …)` even when the base is syntactic. Also fixed the
  array-element split (`array_element_boundary`) to fire only for unary-capable ops
  (`+ - .+ .- & ~ .~ :`), never a suffixed op. JS allow 382 → 386. Fixtures
  `operator_suffixes`, `array_space_unary`.

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
  tiers, project to their own `Special` heads. JS allow 271 → 273.

- **2026-06-17b** — Augmented assignment `op=` (16 TokKinds for `+= … &=` +
  broadcast); `is_assignment_op` folds them into `ASSIGNMENT_EXPR` + `(2,1)` tier.
  JS allow 259 → 264.

- **2026-06-17a** — Built the oracle from scratch + ran the loop 3×: JuliaSyntax
  differential oracle (projector `sexpr.rs` + `--to sexpr`, harness, curated +
  harvested corpora, refresh scripts); `a[begin]` index marker (+1 JS); `:foo` /
  `:(x+1)` symbol quotes via `parse_quote_sym` (+5 JS); pair operator `=>`/`.=>`
  on arrow tier `(4,3)` (+2 JS). JS allow 251 → 259.
