# formatter recap

Rolling log. Read top-to-bottom: persistent traps -> progress -> latest session ->
earlier log. Keep <= ~300 lines; demote the "Latest session" to a one-liner in the
"Earlier sessions" list each new session.

## Persistent traps & invariants

- **`rules::lower` is the only growth surface.** Add a `lower_<construct>` arm per
  construct; build `Ir`; **bail to `lower_transparent` on any shape you don't
  fully model**. The transparent fallback (tokens verbatim, recurse into child
  nodes) keeps unhandled syntax lossless and the whole pass idempotent.
- **No external reference formatter.** Fatou owns its style. `expected.jl` is
  **hand-authored** by the user (you propose, they edit, you push back, you
  implement). Never capture `expected.jl` from any formatter — including Fatou's
  *current* output.
- **Tenet 1 is the authority.** Deterministic full reflow: output depends only on
  the rules + layout engine, never on the input's line breaks / whitespace /
  operator spelling / numeric form. Author `expected.jl` to the canonical
  fully-reflowed form.
- **Gate = file presence.** A fixture is gated iff it has `expected.jl`
  (`tests/formatter.rs::formatter_fixtures_match_expected`). No allowlist, no
  blocked list. Stability (`formatter_is_idempotent_and_stable`) runs over **all**
  `input.jl` (gated or not): idempotence + clean reparse.
- **Parser/lexer gap => stop and hand off** to `parser-parity` (its RECAP queued
  target + `TODO.md`), with JuliaSyntax ground truth. `rules.rs` is the only
  growth surface here; don't paper over parser bugs in the formatter.

## The pivot (start over — Runic target removed)

We dropped the Runic.jl differential-parity target entirely and rebuilt the
machinery on arity's model (hand-authored `input/expected` fixtures). Removed:
`tests/runic_oracle.rs`, `scripts/update-runic-corpus.{sh,jl}`,
`tests/oracle/runic-{allowlist,blocked,report}.txt`, `.runic-source`, the Taskfile
`runic-*` tasks, the `.gitignore` runic stanza, and `Runic` from `devenv.nix`.
`tests/formatter.rs` is now the gate + stability test. The skill was renamed
`formatter-parity` -> `formatter`.

**Corpus reset:** all 65 fixtures kept their `input.jl`; every Runic-minted
`expected.jl` was deleted (recoverable via git). So the gate starts empty and
grows one construct at a time as the user hand-authors each `expected.jl`. The six
`*_divergence` slugs lose their meaning (no Runic to diverge from) — rename/fold
them when revisited.

**Two debts carried forward:**

1. **The existing rules are Runic-derived and mirror source line breaks**, which
   contradicts Tenet 1. They still pass the stability test but produce
   input-dependent layout. Each must be re-evaluated against a hand-authored
   `expected.jl` as its construct is revisited. **Headline future target: build
   the width-driven reflow engine** (`line_width` is currently vestigial — it does
   not drive breaking; the lowering rules decide breaks by inspecting source
   newline tokens via `has_newline_token`). This is the largest piece of work in
   the formatter's life and the prerequisite for true Tenet-1 conformance.
2. **~50 per-rule Runic-rationale doc comments remain in `src/formatter/rules.rs`.**
   The file/module-level headers (`src/formatter.rs`, `core.rs`, `rules.rs` top)
   were de-Runic'd during the pivot; the per-rule comments are accurate history
   and get reworded lazily as each construct is revisited.

## Rule inventory (already in `rules.rs`, pending re-evaluation)

These `lower_*` arms exist from the Runic era. They normalize spacing/indent but
mirror source breaks. Treat them as a starting point, not as correct under
Tenet 1.

- Spacing/operators: `lower_binary` (n-ary, tight `^`/`:`/`::`/`.`; `&&`/`||`
  canonicalized spaced), `lower_arrow`, `lower_comparison`, `lower_ternary`,
  `lower_range`, `lower_type_annotation`, `lower_where`.
- Collections/calls: `lower_arg_list` (width-driven, hug + explode fallback),
  `lower_keyword_arg`/`lower_parameters`, `lower_collection` (width-driven via
  `collection_reflow_body`), `lower_index` (collection/matrix/call/curly subject
  + index share one group — subject yields first; hug or `;`-tail subjects bail),
  `lower_bare_tuple`, curly type-params, named tuples.
- Brackets/matrices (source-break mirroring — the prime reflow-engine targets):
  `lower_multiline_bracket`, `lower_matrix`, `lower_paren`/`lower_paren_block`,
  blank-line preservation via `Ir::BlankLine`, `binary_group_breaks` continuation
  indent.
- Statements: `lower_keyword_stmt` (`return`/`const`/`global`/`local`),
  `lower_import_stmt`, `lower_export_stmt`, `lower_for_binding`.
- Literals (token text, genuinely deterministic): `lower_literal` +
  `normalize_float` + `normalize_hex`.
- Document root: `lower_root` (top-level blank-line policy — interior runs capped
  at 1, edges stripped, one final newline; reuses `collect_body_lines`).
- Blocks (body indentation via `lower_block_body`/`build_block_body`, line model
  via `collect_body_lines` shared with `lower_root`):
  `lower_block_expr` (begin/quote), `lower_let`, `lower_loop` (while/for),
  `lower_if`/`lower_try` (+ `lower_branch_clause`), `lower_struct`,
  `lower_function`, `lower_do` (+ `lower_do_params`), `lower_module`
  (+ `module_should_indent`), `lower_type_decl` (abstract/primitive). Empty
  single-body blocks (struct/function/macro/loop/let/begin/quote/module) collapse
  to the canonical inline `… end` via the shared `push_block_body` helper
  (`block_is_empty` gates it); `if`/`try`/`do` still bail transparent on empty.
- Comments: own-line + trailing line comments and block comments in block bodies,
  brackets, and matrices.
- Trivia: `lower_trivia` (trailing-whitespace trimming in the transparent path).

## Latest session (call-subject index break — `call_reflow_body`)

Closed ranked target #1: extended **subject-yields-first** (user's prior AskUserQuestion
decision for collections) to **call and curly subjects**. In the boundary window — `f(args)[`
fits at ≤92 but the total overflows — the index used to break while the fitting call stayed
flat (`f(a, b, c)[\n    idx,\n]`), exactly the form the user rejected for collection subjects.
Now the call's args explode one per line and the index rides the closing paren, breaking at
its own column only if it still overflows there. No new style decision was put to the user:
this is the uniform extension of the recorded choice (fixture forms mirror
`collection_index_break/`).

**Implementation (rules.rs only, no printer change):** `lower_index`'s subject match gained
`CALL_EXPR | CURLY_EXPR => call_reflow_body(&subject)`. The new helper requires the exact
`callee ARG_LIST` two-node shape (an interleaved token — the `f (a)` parser gap — bails),
lowers the callee via `lower_node` (dotted `Base.f` works; a comment anywhere in the callee
bails), and folds the arg list's **ungrouped** explode body into the shared outer group —
that ungrouping is the whole trick: a break point only belongs to the outer group if it is
not wrapped in a nested group. Two extractions made it clean: the arg-list parse loop became
`collect_arg_list(node) -> Option<ArgListParts>` (shared with `lower_arg_list`, which keeps
its transparent/multiline routing), and `arg_list_explode_group` split into
`Ir::group(arg_list_explode_body(…))`.

**Deferred bails (index still yields there):** a `;`-`PARAMETERS` tail and a huggable last
argument return `None` → transparent path. A hug's break opportunities live in the hugged
construct's *own* group (verified in `printer.rs`: `fits` reads a trailing `HugGroup`'s body
group in carried Break mode, so the subject would stay flat and the index break) — pushing
subject-yields-first through a hug needs printer work. Both bails keep today's behavior, so
nothing regressed; recorded in `TODO.md`.

Regimes locked in `call_index_break/` (8 cases): 92/93 fence pair, pre-broken source input in
the window (a source-exploded *index* reflows to the exploded-subject form), wide-index-rides
(66-char index rides flat at the `)` column), both-break (99-char riding index breaks too),
dotted callee, curly subject (`SomeParametricType{…}[type_index]`), and a pre-exploded small
call collapsing back to flat. Existing `postfix_tail_break`/`chained_postfix_break` cases sit
one char past the window (the call already broke), so they now route through the new path with
byte-identical output. Gate 85→86, zero regressions; clippy + fmt clean.

**Parser/lexer blocker surfaced:** none.

**Ranked next targets:** (1) Keyword-arg-value / collection-element hugging (still open).
(2) Chained postfix on a collection/call subject (`[…][i][j]`, `f(x)[i][j]` — the outer
`INDEX_EXPR`'s subject is an `INDEX_EXPR`, falls to transparent today). (3) Subject-yields-first
through a **hug** and through a `;`-params tail (the two `call_reflow_body` bails — needs the
HugGroup/outer-group merge in the printer). (4) Arrow/pair tier flatten; sweep the residual
Runic doc comments in `rules.rs`.

**Parser/lexer gaps outstanding (handed off, not formatter targets):** (a) **parser** —
newline-after-comma continuation (bare tuple / `let` bindings / `import` lists) fragment
(2026-07-02b); (b) **lexer** — `<<=`/`>>=`/`>>>=`/`÷=`/`⊻=` don't tokenize as one
compound-assign token; (c) **parser** — whitespace before a call/index/curly arg list is
wrongly accepted (`f (a)`, `a [1]`, `A {T}`). All in `parser-parity/RECAP.md` "Queued next
targets" + `TODO.md`.

**Note:** stray untracked `tests/oracle/runic-report.txt` (old Runic report format) is leftover
from the pivot — safe to delete, not regenerated by anything.

## Standing traps

- `build_block_body`/`lower_root` use a Rust let-chain (`if j == last && let
  Some(...)`) — fine on this toolchain.
- Default **indent width is 4** (commit `c552607`); default **`line_width` is 92**
  (`style.rs`), not the 80 in `printer.rs`'s own unit tests.
- `print()` appends **no** trailing newline of its own — the document IR must end
  with one (`lower_root` pushes a final `HardLine`).
- The transparent path emits raw `\n` as `Ir::Text`; the printer resets `col` after
  an embedded newline. In `fits`, an embedded newline (or forced break) **inside** the
  tested group forbids flat, but in **trailing** content it ends the measured line.
  Prefer `Ir::Line`/`SoftLine`/`HardLine` inside groups.
- `fits` is **continuation-aware** (2026-07-02b): it measures the group flat plus the
  trailing stack up to the next taken break. Trailing nested groups are read in their
  carried (Break) mode, so an earlier small group stays flat while a later one breaks.
  A group followed by a long tail now breaks by width; don't assume a group's own
  contents alone decide its mode.
- Clippy trap: `bool.then_some(x).unwrap_or_else(...)` trips `obfuscated_if_else` —
  use a plain `if !flag { return ... }`.

## Earlier sessions

- **Collection-subject index break** (committed `38ddd40`, `feat`): user chose (AskUserQuestion)
  **subject yields first** for `<wide-collection>[index]` — new `lower_index` arm folds
  `collection_reflow_body`/`matrix_reflow_body` (pure extractions) + `lower_arg_list(args)` into
  one outer group, so the collection explodes and the index rides the closing bracket (breaking
  at its own column only if still too wide). Other subjects stayed transparent (index yields).
  Gated `collection_index_break/` (7 cases incl. the 92/93 fence pair). Gate 84→85. (Extended to
  call/curly subjects this session.)
- **Hug explode fallback — `Ir::HugGroup`** (committed `e7c0e41`, `feat`): when even the hug
  first line (open bracket + flat leading args + the hugged construct's opener) overflows,
  the call explodes one-item-per-line instead. New `Ir::HugGroup { prefix, body, close,
  explode }` + printer `hug_fits`, seeding the shared `fits_stack` loop with the body in
  Break mode so its first break opportunity ends the measured line (arity's stop-at-HardLine
  trick doesn't transfer — Fatou's hugged item has no forced break). In trailing content a
  `HugGroup` walks its parts byte-identical to the old bare concat, so zero fixtures moved.
  Nested hugs measure conservatively (user choice): overflow explodes the outer call, the
  inner re-decides at its column. `arg_list_explode_group` extracted so hug fallback and
  non-hug layout are the same doc. Deferred: `hug_excuse_overflow` (overwide unbreakable
  leading atom shouldn't force explode). Gated `arg_hug_explode/` (7 cases incl. the 92/93
  fence pair). Gate 83→84.
- **Argument hugging — trailing bracket construct** (committed `9ea2e38`, `feat`): when the last
  positional arg of a call/index arg list is bracket-delimited (`arg_is_huggable`), it hugs the
  enclosing bracket — `outer(inner(\n …\n))`, `map(f, [\n …\n])` — via a bare concat in
  `lower_arg_list` (no wrapping group, no outer trailing comma); the continuation-aware `fits`
  glued openers and stacked closers, so no printer change. Gated `arg_hug/`. Gate 82→83.
  (Superseded this session: the bare concat became `Ir::HugGroup` with the explode fallback.)
- **Postfix tail on a breaking bracket group** (committed, `test`): gated the non-call breaking group
  carrying a postfix tail — a wide vector/tuple (one-per-line) or matrix (one-row-per-line) rides
  `.field` / `::T` / chained `.field.other` on its closing-bracket line. Continuation-aware `fits`
  already canonical; no code change, pure gating. Deferred `<wide-collection>[index]` (subject-vs-index
  break). `bracket_postfix_break/`. Gate 81→82.
- **Chained postfix tail on a breaking call** (committed `c4548b8`, `test`): gated multiple postfix
  ops riding a wide call's closing bracket — `).field.other`, `)[index_expr][second]`, `).method(z)`,
  `)[idx].field`. Continuation-aware `fits` already canonical; no code change. User chose
  chained-postfix-on-call over postfix-on-collection (done this session). `chained_postfix_break/`.
  Gate 80→81.
- **Postfix tail on a breaking call** (committed, `test`): gated the single postfix tail riding a
  wide call's closing bracket — `).field`, `)::SomeLongTypeName`, `)[index_expr]`. Continuation-aware
  `fits` already canonical; no code change. User chose tail-rides-bracket, three tails bundled in
  `postfix_tail_break/`. Gate 79→80. (Chained tails followed this session.)
- **Uniform mixed same-precedence chain break — flatten by tier** (committed `7bff2ad`, `feat`):
  third `lower_binary` re-evaluation. Generalized `collect_binary_chain`'s descend test from
  exact-kind equality to **precedence-tier** equality (`binary_prec_class` + `same_break_tier`,
  mirroring the parser's `infix_binding_power`: plus `+ - |` = 0, times `* / \ % &` = 1, shift
  `<< >> >>>` = 2), so a too-wide *mixed* same-tier chain (`a + b - c`) breaks at every operator,
  not just the outermost. Tighter/looser tiers stay their own flat group; unicode ops (one
  `UNICODE_OP` kind) return `None` from `binary_prec_class` and still flatten only on exact-kind.
  Disclosed consequence: bitwise `|`/`&` share the plus/times tiers, so `a + b | c` folds too.
  Gated `mixed_precedence_chain/`. Gate 78→79.
- **Uniform same-operator chain break** (committed `a6592c`-era, `feat`): flattened the
  parser-nested same-operator short-circuit / pipe / pair chains (`&&`/`||`/`|>`/`=>`) into one
  break group via `binary_op_kind` + `collect_binary_chain` (descend on exact-kind match), so a
  too-wide chain breaks at every operator, not just the outermost. Only same-operator children
  flattened; mixed/tighter subexprs stayed their own group. Gated `chain_break/`. Gate 77→78.
  (Superseded this session: the descend test now flattens by precedence *tier*.)
- **Continuation-aware `fits`** (committed `5baca04`, `feat`): first printer-engine change
  post-pivot. Fixed `printer::fits` to the Wadler best-fit continuation form — it now walks the
  group `inner` flat **then the rest of the print stack**, stopping at the first taken break, so a
  too-wide trailing tail (` = x` after `where {…}`) forces the group to break. Signature
  `fits(remaining, inner, rest: &[(usize, Mode, &Ir)])`; a break *inside* the tested group forbids
  flat but the same in *trailing* content ends the line (`in_group` flag); trailing nested groups
  keep their carried Break mode so an earlier small group stays flat while only the needed one
  breaks. User chose braces-explode + `} = x` trails. Gated `where_break/`. Gate 76→77. (Detail
  now lives in Standing traps; the newline-after-comma continuation gaps it surfaced are in the
  outstanding-gaps block above.)
- **Paren-block width-driven break + newline reflow** (committed `5905ed2`, `feat`): closed
  ranked target #1 for `lower_paren_block` (`PAREN_BLOCK`, the `;`-block `(a; b; c)`). One
  width-driven `Ir::group`: flat when it fits, else one statement per 4-indented line with `;`
  **snug after each but the last** (user chose via AskUserQuestion), brackets framing their own
  lines. Both token loops skip interior `NEWLINE`, so a source-multiline block reflows to the
  same form. `statements.len() < 2` still bails transparent (single-stmt `(a;)`); comment-bearing
  blocks bail. Gated `paren_block_break/`. Gate 75→76.
- **`;`-keyword tail width-driven break** (committed `7d33a5b`, `feat`): closed the deferred
  `;`-`PARAMETERS` hole in `lower_arg_list` — the `; kw = …` tail now folds into the **same**
  width-driven group as the positional args instead of emitting flat unconditionally. Broken:
  one arg per line, `;` **snug after the last positional** (`b;`), each kwarg on its own line
  + broken-only trailing comma; a keyword-only call keeps `;` on the open bracket (`f(;`).
  New `collect_param_items` helper (skips `WHITESPACE`/`NEWLINE`; a trailing `pending_comma`
  is a dropped trailing comma, only a missing `;`/empty tail bails → `lower_parameters` flat
  fallback for comment/unmodeled shapes). User chose "`;` trails last positional". Gated
  `arg_list_params_break/`. Gate 74→75.
- **Comprehension/generator reflow** (committed `deb0df3`+`cbeaa25`, `feat`): new
  `lower_comprehension` arm (`COMPREHENSION`/`GENERATOR`/`BRACES_COMPREHENSION`) +
  `lower_comprehension_if`, after `lower_collection`. One width-driven group: flat
  `[elem for b if f]` when it fits, else element + each `for`/`if` clause on its own
  4-indented line (user chose "explode each clause"). Typed `T[…]` handled via the
  transparent snug. Comment descendant → bail transparent. Follow-up: `lower_for_binding`
  + `for_iteration_operands` now skip `NEWLINE` like `WHITESPACE`, so a source-multiline
  comprehension reflows to the same canonical form (dropped the `has_newline_token` guard;
  back to 2 call sites). Gated `comprehension_spacing/`, `comprehension_break/`,
  `comprehension_multiline/`. Gate 71→74.
- **Unary prefix operators** (committed `d04276d`, `feat`): new `lower_unary` arm +
  `operand_leads_with_operator` helper (before `lower_arrow`). `UNARY_EXPR` is the prefix
  `<op> <operand>`; the op snugs to its operand (no space), operand recursed
  (`x = -  a` → `x = -a`, `-f( x )` → `-f(x)`). **Retokenization trap:** `- -a` snugged to
  `--a` would retokenize as `--`, so it bails to verbatim when the operand is a nested
  `UNARY_EXPR` or its first token begins with a symbolic op char (`+-*/\^%!~<>=&|:$?`);
  `-1` folds into a `LITERAL` upstream. Gated `unary_operators/`. Gate 70→71.
- **Macro-call spacing** (committed `f0fdd0a`, `feat`): first construct past full-gate.
  New `lower_macro_call` + `lower_macro_name` (after `lower_collection`) normalize the
  verbatim macro-name→arg whitespace to one space while preserving the semantic
  call-form vs space-form split — an attached `ARG_LIST` (`@eval(expr)`) stays snug and
  lowers like a call's; a spaced arg (`@assert x > 0 "msg"`, `@foo (a, b)` as a
  `TUPLE_EXPR`) collapses each gap to one space. Keyed on a `had_gap` flag; the space
  form never breaks; dotted names (`Base.@kwdef`) flatten; comment/newline/unexpected
  bails. Gate 69→70. Clippy trap: `!(a && !b)` trips `nonminimal_bool` — bind
  `let call_form = …;` then `if !call_form`.
- **Gated the last 4 comment fixtures — every fixture then gated** (committed, pure
  `test(formatter)`, no code): hand-authored `expected.jl` for `block_comments`,
  `block_comments_in_blocks`, `bracket_block_comments`, `trailing_comments`; the
  existing `lower_block_body`/`lower_multiline_bracket` comment machinery already
  emits canonical Tenet-1 form. Verified input-independence (own-line comments
  re-indent, `#= =#` interiors kept verbatim, comment-bearing brackets explode
  one-per-line, `;`-joins split). Gate 65→69.
- **Gated the spacing/padding pile; renamed the `*_divergence` slugs** (committed,
  pure `test(formatter)`): gated the eight remaining already-canonical fixtures
  (`paren_padding`, `assignment`, `trailing_whitespace`, `logical_operators`,
  `paren_blank_lines`, `block_comment_spacing`, `bracket_comment_spacing`,
  `trailing_comment_spacing`); verified determinism (mangled variants normalize,
  idempotent). User renamed all five `*_divergence` slugs (Runic gone) and stripped
  false "preserved by Runic" editorializing from comment fixtures. Gate 57→65.
- **Gated the module/baremodule body-indentation construct** (committed, pure
  `test(formatter)`): authored `expected.jl` for the four `module_*` fixtures. Kept
  Runic's rule — every module body indents *except* the lone file-wrapper module
  (sole top-level expression; a leading comment is not a sibling), which stays flush;
  nested `module Inner` always indents. `module_should_indent` already reproduces this
  (deterministic on AST structure, not whitespace → Tenet-1 compliant); only Fatou
  divergence is the empty-body collapse (`module E\nend`→`module E end`). Gate 53→57.
- **Gated the global/local multi-name list construct** (committed, pure
  `test(formatter)`): authored `expected.jl` for `global_local_names` +
  `global_local_assignment`. Confirmed the parser wraps every multi-name form in a
  single `BARE_TUPLE_EXPR`/`ASSIGNMENT_EXPR` operand, so `lower_keyword_stmt` recurses
  into `lower_bare_tuple`/`lower_binary`/`lower_type_annotation` (all width-driven);
  the loose-children fallback never fires. Caveat: a bare tuple with an interior
  *newline* still bails transparent (the reflow debt); no fixture input has one.
  Gate 51→53.
- **Gated the already-canonical operator/literal pile** (committed, pure
  `test(formatter)`): authored the first `expected.jl` for 15 ungated fixtures whose
  rules already emit canonical Tenet-1 form (`tight_operators`, `assignment_spacing`,
  `type_annotations`, `range_colon`, `where_clauses`, `dot_access`, `float_literals`,
  `hex_literals`, `named_tuples`, `curly_type_params`, `bare_tuples`,
  `import_using_lists`, `export_public_lists`, `comprehension_for_in`, `control_flow`).
  Re-verified idempotence + input-independence before gating. Gate 36→51.
- **Tenet-1 whitespace fix for type declarations** (committed): retired the last
  source-mirror in `lower_type_decl` (`ABSTRACT_DEF`/`PRIMITIVE_DEF`) — the
  post-signature region (around the bits `LITERAL` and `end`) now normalizes
  (WHITESPACE→one space, END_KW→text, else bail transparent) instead of passing
  source spacing through. Dropped the unused `.peekable()`/`while let` for a plain
  `for`. Gated `abstract_types/` + `primitive_types/`. Gate 34→36.
- **Empty-body inline fold for `if`/`try`/`do`** (committed): extended the
  empty-body inline collapse to the last three block families that still bailed
  transparent on an empty body. New helper `lower_body_allow_empty` (`Some(Some)`
  non-empty / `Some(None)` empty / `None` bail); `lower_do` routes through
  `push_block_body` (`map(xs) do x end`); a clause-less empty `if` folds inline
  (`if x end`) but any clause keeps it vertical (shared `end`); `try` never
  inline-folds and a clause-less `try` bails (syntax error). Gated `do_blocks/`,
  extended `if_blocks/`/`try_blocks/`. Gate 33→34. All block families now handle
  empty bodies deterministically.
- **Width-driven comparison + arrow** (committed `662331d`): retired the last two
  source-break-mirroring operator rules. `lower_comparison` (`COMPARISON_EXPR`) now
  mirrors `lower_binary`'s non-assignment path (one group, `Ir::Line` gaps,
  operator-trailing; flat when it fits else each op trails, operands indent one
  step); `lower_arrow` (`ARROW_EXPR`) stays flat `lhs -> rhs` (never breaks at `->`
  — assignment-style bias) but now ignores `NEWLINE`. Gated `comparison_chains/` +
  `arrow_functions/`. Gate 31→33. **All operator rules now width-driven Tenet-1.**
- **Width-driven ternary (`lower_ternary`)** (committed `58e5336`): retired the
  source-break mirror in `TERNARY_EXPR` for the Air model — one `Ir::group` per
  ternary node with its own `Ir::indent`, operator-trailing (`?`/`:` can't lead a
  line), each gap an `Ir::Line`; flat when it fits, else the branch operands wrap
  one step. Nested `?:`-chains nest deeper (each owns its indent). Dropped the
  `node.ancestors()` ride check. Gated `ternary_multiline/`, `ternary_spacing/`,
  `ternary_paren_branch/`. Gate 28→31.
- **Width-driven binary/assignment (`lower_binary`)** (committed `34c3e16`): retired
  the source-break mirror in `BINARY_EXPR` + `ASSIGNMENT_EXPR` for Air's model — one
  `Ir::group` per binary node with its own `Ir::indent`, operator-trailing, each gap
  an `Ir::Line`; a tighter subexpr stays flat while the looser chain breaks, and an
  inner subexpr forced to break nests its indent on the parent's. Assignment ops
  never break (` = ` flat, no group/indent — the RHS's own group absorbs the break:
  `x = a +⏎ b`, never `x =⏎ a + b`). Tight ops (`^`/`:`/`.`) still pack. Deleted
  `binary_group_breaks`; unblocked binary-inside-paren. Gated `binary_continuation/`
  (fit→flat + two too-wide break-pin cases) + `binary_spacing/`. Gate 26→28.
- **Width-driven paren reflow (`lower_paren`)** (committed `3903b5f`): killed the
  `has_newline_token` source-break mirror in `PAREN_EXPR` — one width-driven
  `Ir::group` (flat `(inner)` when it fits, else `(`/+indent/`)`), padding stripped,
  blanks dropped. Gated `paren_multiline/` + `paren_blocks/`. Gate 24→26.
- **Top-level `;`-join reflow (`TOPLEVEL_SEMICOLON`)** (committed `a23697c`): closed
  the last top-level `;`-separator Tenet-1 hole. The parser folds `a; b; c` into one
  `TOPLEVEL_SEMICOLON` child of `ROOT`; `collect_body_lines` now flattens it via the
  extracted `collect_body_elements(node, &mut lines, &mut expect_sep)` recursion, so
  each `;`-joined statement lands on its own line exactly as a block body's do
  (`a; b` ≡ `a⏎b`). Trailing `;` drops the empty tail, `a;;b` collapses. Block bodies
  untouched (the branch only fires on `TOPLEVEL_SEMICOLON`). Gated
  `toplevel_semicolon/`. Gate 23→24.
- **Top-level blank-line policy (`lower_root`)** (committed `5589f58`): closed the
  file-level blank Tenet-1 hole — `ROOT` no longer falls through transparent.
  `lower_root` reflows deterministically: interior blank runs cap at
  `MAX_BLANK_LINES`=1, leading/trailing file blanks stripped (unlike a block body's
  framed edges), exactly one final newline. Extracted the shared
  `collect_body_lines(node) -> Option<Vec<BodyLine>>` from `build_block_body`. Gated
  `toplevel_blank_lines/`; unblocked `loop_blocks/` + `let_blocks/` (empty-body
  inline collapse). Gate 20→23.
- **Empty-body uniformity fold + gate `try_blocks`** (committed `370df78`):
  generalized the struct empty-body inline collapse to the other single-body
  blocks via a shared `push_block_body` helper (`function`/`macro`/`while`/`for`/
  `let`/`begin`/`quote`/`module` empty bodies → inline `… end`, Tenet 1). `if`/
  `try`/`do` still bail transparent on empty (deferred — multi-clause). Gate 19→20.
- **Gated `struct_blocks` + empty-body collapse** (committed): `lower_struct`
  gained the inline empty-body collapse (`struct E end`) plus the reusable
  `block_is_empty` helper; the follow-up this session generalized it to the other
  single-body blocks. Gate 18→19.
- **Gated `keyword_statements`** (committed `a069201`): pure `test(formatter)`, no
  code — `lower_keyword_stmt` already emits the canonical `return`/`const`/
  `global`/`local` form (one space after keyword, operand normalized, bare
  `return` kept). Gate 17→18.
- **Block-body `;`-separator + 1-blank cap** (committed `d73ac02`): killed the
  last source-separator mirror in `build_block_body` — `;` now reflows like a
  newline (each statement its own `HardLine`, so `begin a; b; c end` and the
  newline form format identically), and `MAX_BLANK_LINES` dropped 2→1 (a blank run
  in a block body condenses to one). Gated `if_blocks` + `begin_quote_blocks`.
  Gate 15→17.
- **Gated six free non-comment bracket/matrix fixtures** (committed `0bf4e6f`):
  pure `test(formatter)`, no code. All route through the width/reflow paths and
  collapse to canonical flat form (every case fits the 92-col `line_width`); locked
  `multiline_brackets`, `bracket_blank_lines`, `bracket_gap_blank_lines`,
  `multiline_matrices`, `matrix_blank_lines`, `matrix_gap_blank_lines`. Gate 9→15.
  The collection/bracket/matrix family is now fully Tenet-1.

- **Comment-bearing matrix reflow** (committed `845c7c4`): rewrote
  `lower_matrix_multiline` from source-break mirror to the canonical form (direct
  analog of `lower_multiline_bracket`) — always framed one row per line, new
  `lower_matrix_row` joins a row's elements with one space, trailing comment rides
  its row at one leading space, own-line comments keep their line, `[ # header`
  rides the bracket, blanks dropped (the old `MAX_BLANK_LINES`/`Ir::BlankLine`
  matrix usage is gone; both still live for block bodies), block comments verbatim.
  `matrix_comments/` + `matrix_block_comments/` gated. Gate 7→9. Trap:
  `lower_matrix_reflow` still inlines its own MATRIX_ROW walk (could unify onto
  `lower_matrix_row`); a comment *inside* a `MATRIX_ROW` bails transparent.

- **Comment-bearing bracket reflow** (committed `dbd0dcd`): rewrote
  `lower_multiline_bracket` from source-break mirror to canonical fully-exploded
  form — always one item per line, always a trailing comma, blanks dropped, comment
  attachment preserved (trailing rides item at one leading space, own-line keeps its
  line, `[ # header` rides the bracket; `on_line` flag starts true). Killed
  `adds_trailing_comma`/`Sep`/`GapLine`. `bracket_comments/` gated (also block-comment
  + multi-space fixtures route here). Gate 6→7.
- **Width-driven matrix reflow** (committed `8c41393`): made matrices
  input-independent. `lower_matrix` is now a dispatcher — comment-bearing →
  `lower_matrix_multiline` (verbatim, source-mirroring), else `lower_matrix_reflow`
  (one `Ir::group`: flat `[a b; c d]` when it fits, else framed one row per line;
  rows split at `;` **and** `NEWLINE`, `;;` bails transparent). `matrices/` gated.
  Trap: default `line_width` is **92** (`style.rs`), not the 80 in `printer.rs`
  tests. Matrix rows have two CST shapes (bare `ARG` vs `MATRIX_ROW` wrapper).
- **Function/macro body reflow** (committed `b04bfd6`): dropped the Runic-era
  `return`-tail guard in `lower_function` so any non-empty body reflows to the
  canonical 2-space indent (no `return` inserted; layout-only). Gated
  `function_blocks` with a bare-tail case; fixed `core.rs` unit test. Gate 4→5.
  Trap: other rules still carry now-historical "never `return`-inserted" comments.
- **Width-driven collection reflow** (committed `a6fe509`): `lower_collection`
  rewritten to mirror `lower_arg_list` — one `Ir::group`, flat when it fits else
  one element per indented line with a broken-only trailing comma; source breaks
  and trailing commas ignored; the one-tuple `(a,)` keeps its semantic comma in
  both modes. Gated `collections` + `collection_break`. (RECAP wasn't updated that
  session; reconciled here.)
- **Width-driven arg-list reflow** (committed `2d3003d`): made `line_width`
  actually drive breaking for call/index arg lists — the first reflow construct.
  New IR primitive `Ir::IfBreak(broken, flat)` (broken-only trailing comma).
  Printer `col` fix: `Text` now resets `col` after an embedded newline (the
  transparent path emits raw `\n` as `Text`), and `fits` treats embedded newlines
  as non-fitting — **watch this** when adding groups. Default indent width 4→2.
  Gated `call_arg_lists` + `arg_list_break`. Comment-bearing lists and the
  `;`-`PARAMETERS` tail (`f(a; b=1)`) still stay flat (deferred).

- **The pivot:** removed the Runic target, stood up the hand-authored fixture
  machinery + the `formatter` skill. Gate started empty; stability green over all
  65 inputs. (Pre-pivot Runic-parity history lives in git: the `formatter-parity`
  skill's RECAP through 2026-06-30 logged ~50 constructs landed against the Runic
  oracle. Those rules survive in `rules.rs` per the inventory above; their parity
  status is no longer meaningful.)
