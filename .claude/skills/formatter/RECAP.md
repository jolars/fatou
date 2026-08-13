# formatter recap

Rolling log. Read top-to-bottom: traps -> recorded decisions -> inventory ->
latest session -> earlier log. Keep <= ~300 lines; when you add a session,
demote the previous "Latest session" to a one-liner under "Earlier sessions"
and lift anything that must never be silently reversed into "Recorded
decisions". Detail lives in `git log`, not here.

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
  (`crates/fatou-formatter/tests/formatter.rs::formatter_fixtures_match_expected`). No allowlist, no
  blocked list. Stability (`formatter_is_idempotent_and_stable`) runs over **all**
  `input.jl` (gated or not): idempotence + clean reparse.
- **Parser/lexer gap => stop and hand off** to `parser-parity` (its RECAP queued
  target + `TODO.md`), with JuliaSyntax ground truth. `rules.rs` is the only
  growth surface here; don't paper over parser bugs in the formatter.

## Carried debt

1. **~~Runic-derived rules mirror source line breaks~~ — PAID (2026-07-07 audit).**
   For many sessions this was framed as the headline future target, "the largest
   piece of work in the formatter's life." **That framing is stale — don't let it
   send a session chasing a rewrite that is already done.** `printer.rs` decides
   every `Ir::Group`'s flat-vs-break purely from `line_width` via the
   continuation-aware Wadler `fits`; the debt was retired construct by construct.
   `has_newline_token` (the named mirroring mechanism) is down to **one live call
   site**, the comment-bearing matrix path — where a comment genuinely can't be
   reflowed away (moving a trailing `# note` changes which element it annotates),
   so mirroring there is arguably correct. `lower_matrix_reflow` (the comment-free
   path) is fully width-driven.
2. **~50 per-rule Runic-rationale doc comments remain in `rules.rs`.** Accurate
   history; reworded lazily as each construct is revisited. Minor.

## Recorded decisions — do not silently reverse

Each was a user call (mostly via AskUserQuestion) and is pinned by a fixture.
Reversing one is allowed but must be conscious and re-recorded, never drive-by.

- **Uniform arrow-tier break.** A chained pair whose innermost value is *grouped
  but non-huggable* (wide binary, paren, comparison) breaks at **every** `=>`,
  rather than hugging the spine and breaking the value. `chained_pair_grouped_tail`.
- **Only a *sole* item hugs** its enclosing bracket (`suppress_multi_item_hug`);
  positionals and keywords are counted **together**. `pair_list_no_hug`.
- **A sole nested *call* `f(g(…))` hugs** — a conscious divergence from every
  JuliaFormatter style, which staircases it. Justified by Base's dominant
  `throw(ArgumentError(⏎ "msg",⏎ ))` idiom. Don't "fix" it toward JuliaFormatter.
- **Char escape spelling stays verbatim** (`'\x61'` does *not* become `'a'`): a
  char is string-content, not a numeric spelling, so rewriting it is content
  rewriting. `char_literals`.
- **Selector imports break colon-first** — `:` reads as a bracket-like opener, so
  `import Mod:` terminates the head line and every name wraps beneath at +4.
  Bare (colon-less) lists keep their first item on the keyword line.
  `import_list_break`.
- **Broken conditions and `for` bindings sit at +8**, one level *deeper* than the
  body they guard (`lower_control_header`), so the header/body boundary is
  visible. `condition_break`, `for_binding_break`.
- **Interpolation content is code, not opaque text**: normalized and forced flat.
  Only string characters and *nested* string literals stay verbatim.
  `string_interpolation`.
- **Quoted `end`-blocks lay out across lines** via the generic `lower_paren`
  reflow, rather than a `QUOTE_SYM`-specific hug that would hard-code a special
  case. `block_quote`.
- **`.^` is tight** (a dotted op inherits its base op's spacing), with a
  retokenization guard so `2 .^ n` stays spaced.
- **Let bindings are never moved into the body** — `a = 1, b = 2` in body
  position parses as `a = ((1, b) = 2)`, a semantic change.

**Abandoned, don't re-derive:** a "does the hug earn it?" printer guard (a
`probe` field on `Ir::HugGroup`, rejecting the hug when the item would fit flat
at the explode indent). It independently kills the canonical sole-nested case —
`outer_function(inner_function(…))` explodes because the inner call happens to
fit. The sole-item rule reaches the same goal without that damage.

## Rule inventory (the `lower_*` arms in `rules.rs`)

An orientation map, not a spec — read `rules.rs` for behavior and the section
above for the decisions behind it. Width-driven throughout; the only surviving
source-break read is the comment-bearing matrix path (debt #1).

- Operators: `lower_binary` (n-ary, tight `^`/`:`/`::`/`.`/`.^`; `rhs_absorbs_break`
  biases the break into the RHS for assignments and bracket-bottomed pair chains,
  via `pair_chain_hugs`), `lower_arrow`, `lower_comparison`, `lower_ternary`,
  `lower_range`, `lower_type_annotation`, `lower_where` (single param atomic, short
  multi-param via `Ir::CondGroup`), `lower_unary`, `lower_splat`.
- Calls/collections: `lower_arg_list` (trailing hug + explode fallback, gated by
  `suppress_multi_item_hug`), `lower_keyword_arg`/`lower_parameters`,
  `lower_collection` (`collect_collection_items`/`collection_body`),
  `lower_comma_list` (bare tuples + `LET_BINDINGS`), curly type-params, named tuples.
- `lower_index` — subject and index share one group via the recursive
  `index_reflow_body`/`construct_reflow_body`, and the **innermost subject yields
  first**. Registered there: collection, matrix, call/curly (`call_reflow_body`),
  chained index, name-rooted (`applied_args_body`), comprehension
  (`comprehension_reflow_body`/`typed_comprehension_reflow_body`), paren
  (`paren_reflow_body`) — including through hugs (ungrouped `Ir::HugGroup`) and
  `;`-keyword tails.
- Brackets/matrices: `lower_multiline_bracket`, `lower_matrix` (+ `BRACESCAT_EXPR`,
  structurally identical), `lower_paren`/`lower_paren_block`, `Ir::BlankLine`.
- Chains: `lower_call`/`try_lower_chain`/`collect_chain` — a `.`-spine with **≥2
  called links** breaks at the dots; bare field accesses stay glued.
- Statements: `lower_keyword_stmt`, `lower_import_stmt`/`lower_export_stmt`,
  `lower_for_binding`.
- Macros: `lower_macro_call`/`lower_macro_name` — gaps collapse to one space; an
  attached `ARG_LIST` stays snug and lowers like a call's.
- Literals: `lower_literal` + `normalize_float`/`normalize_hex`. Chars and string
  macros (`r"…"`, `raw"…"`, `var"…"`) are verbatim. `lower_string_literal` keeps
  content and delimiters verbatim while normalizing each `$(…)` interpolation and
  forcing it flat (`render_flat`); an unflattenable one bails the whole literal.
- Root and blocks: `lower_root` (blank-line policy, one final newline) and
  `lower_block_expr`/`lower_let`/`lower_loop`/`lower_if`/`lower_try`/`lower_struct`/
  `lower_function`/`lower_do`/`lower_module`/`lower_type_decl`, sharing
  `lower_block_body`/`build_block_body` and `collect_body_lines`. A `CONDITION` or
  `FOR_BINDING` header goes through `lower_control_header` for the extra indent;
  empty single-body blocks collapse inline via `push_block_body`/`block_is_empty`,
  while `if`/`try`/`do` bail transparent on empty.
- Comments in block bodies, brackets, and matrices; `lower_trivia` trims trailing
  whitespace on the transparent path.

## Latest session (only a *sole* item hugs; pair arrows bias into their value)

`feat(formatter)`. User-reported: `f(a; kw = g(x))` and a 3-pair `Dict` were
jamming every leading item flat onto the opening line so the tail could hug,
stacking `))` on the closing line. Two rules, `rules.rs` only, no engine change.

**1. `suppress_multi_item_hug(item_count, last_huggable)`** — only a *sole* item
hugs its enclosing bracket. A strict generalization of the
`suppress_bare_bracket_pair_hug` it replaces (that guard already used
`item_count > 1`, but only for a bare-bracket-valued pair tail); its two helpers
`item_value`/`item_is_bare_bracket_pair_tail` are subsumed and deleted. Also
applied to the `;`-params path, which computes huggability separately via
`collect_param_items` and was why the reported `basedir =` case survived a first
pass — positionals + keywords are counted **together** (they explode into one
shared list).

**2. `is_pair_op`/`pair_chain_hugs` + `rhs_absorbs_break`** (was `is_assignment`)
in `lower_binary` — a pair arrow joins assignment's "bias the break into the RHS"
tier, so `k => merge(` breaks inside the value instead of dropping it to a
continuation line. Gated on `pair_chain_hugs(node)`: the chain must bottom out in
a `huggable_kind` bracket. **A blanket version regressed two fixtures** —
`chained_pair_grouped_tail` and `arrow_pair_chain` (mixed `a => b --> c` broke at
some arrows but not others). `pair_operands` rejects the mixed spine, so both
keep the arrow tier.

**Corpus evidence** (all 166 files of Julia Base formatted under both rules):
lines ending in stacked closers **450 → 302 (−33%)**, total lines +0.23%, lines
over 92 cols unchanged. A third of the `))` stacks removed for negligible
vertical cost and no width regression. JuliaFormatter Default/Blue agree on
multi-item tail (explodes), sole bracket literal (hugs), and pair value (hugs the
arrow); the sole-nested-call divergence is the recorded decision above.

11 `expected.jl` regenerated (user-approved, reviewed as a diff), every diff the
same shape — a multi-item list stops hugging its tail and explodes one per line,
every sole-item hug survives. Gate stays 120 (of 149 fixtures); stability +
clippy + fmt + full suite green. No parser/lexer blocker.

**Ranked next targets:** (1) Return-type `where` signatures (the deferred
follow-up TODO — extend the `CondGroup` probe to the `)::T where ` prefix). (2)
Minor: debt #2. The per-construct queue is otherwise empty; four of the five
sessions before the last landed no code, only fixtures.

## Parser/lexer gaps

**OUTSTANDING (handed off 2026-07-06c): splat after a closing bracket.** `f(g(x)...)`,
`f(a[i]...)`, `f((a + b)...)`, `f(A{T}...)`, `f([1, 2]...)` fail to parse (`...` seen as
a `LoneOperator`) though JuliaSyntax accepts them; only the spaced spelling parses.
`lower_splat` withholds the snug for bracket-closing operands until this lands (see
parser-parity RECAP "Queued next targets" + `TODO.md` Parser). Drop the
`ends_in_bracket` guard and widen `splat_spacing/` when fixed.

Everything else handed off has been resolved parser-side: the `<--`/`<-->` arrow
family, the newline-broken braces comprehension, newline-after-comma, the
compound-assign lexer, and whitespace-before-arglist (whose premise turned out
stale). No other formatter-surfaced parser gap is outstanding.

## Standing traps

- Default **indent width is 4**; default **`line_width` is 92** (`style.rs`), not
  the 80 in `printer.rs`'s own unit tests.
- `print()` appends **no** trailing newline of its own — the document IR must end
  with one (`lower_root` pushes a final `HardLine`).
- The transparent path emits raw `\n` as `Ir::Text`; the printer resets `col` after
  an embedded newline. Prefer `Ir::Line`/`SoftLine`/`HardLine` inside groups.
- `fits` is **continuation-aware**: it measures the group flat plus the trailing
  stack up to the next taken break, so a group followed by a long tail breaks by
  width — don't assume a group's own contents alone decide its mode. A break
  **inside** the tested group forbids flat; the same in **trailing** content just
  ends the measured line. Trailing nested groups are read in their carried (Break)
  mode, so an earlier small group stays flat while a later one breaks.
- **Grouped hug sites use the popped lowered item as hug body**, so any future
  hug-through-`X` must move the `X`-prefix into the hug prefix at *all* sites, not
  just `item_hug_parts`.
- Clippy traps: `bool.then_some(x).unwrap_or_else(...)` trips `obfuscated_if_else`
  (use a plain early return); `!(a && !b)` trips `nonminimal_bool` (bind the
  condition first).

## Earlier sessions

Newest first. One line each; the commit is the detail.

- **Short multi-param `where` bound breaks the args** (`feat`): new
  `Ir::CondGroup { primary, fallback, probe }` measures the re-indented *closing*
  line, so `f(longargs...) where {T, S}` breaks the args and keeps the bound flat.
  Return-type case (`)::T where`) deferred. `where_short_multiparam_break`. 118→119.
- **Glue a sole brace/bracket macro argument** (`feat`): `@m {a}` → `@m{a}`.
  Dropping the space needs only local info (a suffix folds into the child), unlike
  adding it. `macro_glued_argument`. 116→117.
- **Bare-bracket-valued pair tail stops hugging in multi-item lists** (`feat`):
  `suppress_bare_bracket_pair_hug`, later generalized to the sole-item rule.
  `pair_list_no_hug`. 115→116.
- **Signature args break before a single-param `where`** (`feat`): a single type
  parameter is atomic, so `fits` measures the whole `{T}`. `where_bare_signature_break`. 114→115.
- **Selector import breaks colon-first** (`feat`): recorded decision above.
  `import_list_break` updated. Gate unchanged at 114.
- **The "already canonical, just lock it" run** (`test`, no code, 107→114): chained-pair
  grouped tail 107→108 and block quotes 111→112 (both recorded decisions above);
  transpose/adjoint `'` 109→110, where `)'` rides a broken closer via the
  continuation-`fits` postfix-tail rule and `A '` is excluded as a genuine parse
  error; quoted symbols 110→111; char + string-macro literals 112→114.
- **Progress audit — reflow debt found already paid; LSP pivot** (no code): the
  per-construct cadence had saturated; debt #1 rewritten; recommended the LSP
  semantic model as the strategic move (since built out — `src/lsp/` is now large).
- **Broadcast operators + tight `.^`** (`feat`): the dotted family was already
  canonical; `.^` made tight with a retokenization guard. `broadcasting`. 108→109.
- **`for`-binding continuation double-indent** (`feat`): `lower_control_header`
  extended to `FOR_BINDING`; comprehension `for`-clauses go through
  `lower_for_binding` directly and never double-indent. 106→107.
- **Control-flow condition continuation double-indent** (`feat`): the original +8
  rule. `condition_break`. 105→106.
- **Fluent method chains** (`feat`): `lower_call`/`try_lower_chain`/`collect_chain`;
  trailing-dot spelling forced by reparse. `method_chain_break`. 104→105.
- **String/command interpolation** (`feat`): a genuine bug — the transparent path
  was breaking inside literals. `render_flat` + whole-literal bail. 103→104.
- **Bracescat `{a; b}`** (`feat`, two sessions, 101→103): routed through `lower_matrix`
  (structurally identical to a matrix) rather than a bespoke rule, then registered in
  `construct_reflow_body` for the `{a; b}[k]` subject break.
- **The comma-list family** (`feat`, three sessions, 97→100): bare tuples 97→98
  established the `group(concat[first, indent(rest)])` shape; import/using/export
  lists 98→99 reused it; let bindings 99→100 generalized `lower_bare_tuple` →
  `lower_comma_list`.
- **Splat operator snug** (`feat`): postfix analog of `lower_unary`; bracket-closing
  operand bails pending the outstanding parser gap. `splat_spacing`. 100→101.
- **Chained-pair hug through the whole spine** (`feat`): `pair_operands` + recursive
  `pair_hug_chain`. `chained_pair_hug`. 96→97.
- **Widen `arrow_pair_chain` for `<--`/`.<--`** and **widen the braces comprehension
  index case** (`test`, no code): both unblocked by resolved parser gaps. Gate 96.
- **The index-break family — subject yields first** (`feat`, six sessions, 84→96):
  collection subject 84→85 (the original user decision), call/curly via
  `call_reflow_body` + `ArgListParts` 85→86, chained postfix via the recursive
  `index_reflow_body` 88→89, through hugs and `;`-params tails 89→91 (an *ungrouped*
  `Ir::HugGroup` hands flat-vs-yield to the owning group, zero printer changes),
  name-rooted via `applied_args_body` 93→94, comprehensions via
  `comprehension_reflow_body`/`typed_comprehension_reflow_body` 94→95, parens via
  `paren_reflow_body` 95→96.
- **The hugging family** (`feat`, three sessions, 82→88): trailing bracket argument as
  a bare concat 82→83; `Ir::HugGroup` + the explode fallback 83→84 (printer `hug_fits`
  seeds the shared `fits_stack` loop with the body in Break mode; nested hugs measure
  conservatively, by user choice); kwarg values and collection elements 86→88
  (`item_is_huggable`/`huggable_kind`, explode bodies unified on
  `bracket_explode_body`).
- **Pair-value hugging** (`feat`): `=>`/`.=>` are hug-transparent; `-->`/`<-->`
  explode. Source of the four-hug-site trap above. `pair_hug`. 92→93.
- **Arrow/pair tier flatten + Runic doc-comment sweep** (`feat`): `binary_prec_class`
  tier 3; ~25 rationale comments reworded. 91→92.
- **Postfix tails on breaking groups** (three `test`-only sessions): single tail on a
  call, chained tails on a call, tails on a non-call bracket group. Continuation-aware
  `fits` already made all three canonical. 79→82.
- **Uniform mixed same-precedence chain break** (`feat`): `binary_prec_class` +
  `same_break_tier` mirroring the parser's `infix_binding_power`. Consequence:
  bitwise `|`/`&` share the plus/times tiers. 78→79.
- **Uniform same-operator chain break** (`feat`): `collect_binary_chain`. 77→78.
- **Continuation-aware `fits`** (`feat`): the first post-pivot printer change, and
  the one the Standing trap above describes. `where_break`. 76→77.
- **Paren-block break + newline reflow** (`feat`): `;` snug after each but the last. 75→76.
- **`;`-keyword tail break** (`feat`): the `;`-`PARAMETERS` tail folds into the same
  group as the positionals; `;` trails the last positional. 74→75.
- **Comprehension/generator reflow** (`feat`): each `for`/`if` clause on its own line;
  `NEWLINE` skipping dropped a `has_newline_token` call site. 71→74.
- **Unary prefix operators** (`feat`): snug, with the `- -a` → `--a` retokenization
  bail. 70→71.
- **Macro-call spacing** (`feat`): the call-form vs space-form split. 69→70.
- **Gating sweeps** (`test`, no code): comments 65→69; spacing/padding pile +
  `*_divergence` slugs renamed 57→65; module bodies 53→57 (the lone file-wrapper
  module stays flush, nested always indents); global/local multi-name lists 51→53;
  the operator/literal pile, 15 fixtures at once, 36→51; keyword statements 17→18;
  six bracket/matrix fixtures 9→15.
- **Empty-body folds** (`feat`): struct collapse + `block_is_empty` 18→19,
  generalized to the single-body blocks via `push_block_body` 19→20, extended to
  `if`/`do` via `lower_body_allow_empty` 33→34. `try` never inline-folds.
- **The operator rules going width-driven** (`feat`, four sessions): binary/assignment
  26→28 — assignment ops never break, the RHS's own group absorbs it (`x = a +⏎ b`,
  never `x =⏎ a + b`); ternary 28→31 (operator-trailing, nested chains nest deeper);
  comparison + arrow 31→33 (`lower_arrow` stays flat, assignment-style bias);
  type-declaration whitespace 34→36, the last `lower_type_decl` source mirror.
- **Top-level structure** (`feat`): blank-line policy in `lower_root`, which extracted
  `collect_body_lines`, 20→23; `;`-join reflow via `collect_body_elements` 23→24.
  Block-body `;`-separator + 1-blank cap (`;` reflows like a newline) 15→17.
- **Width-driven paren reflow** (`feat`): killed the `has_newline_token` mirror in
  `PAREN_EXPR`. 24→26.
- **The bracket/matrix family** (`feat`, four sessions): arg-list reflow — the first
  reflow construct, introduced `Ir::IfBreak`; collection reflow, where the one-tuple
  `(a,)` keeps its semantic comma in both modes; matrix reflow, making `lower_matrix`
  a dispatcher (rows have two CST shapes, bare `ARG` vs `MATRIX_ROW`); then the
  comment-bearing bracket 6→7 and matrix 7→9 rewrites from source mirror to canonical
  framed form. A comment *inside* a `MATRIX_ROW` still bails transparent.
- **Function/macro body reflow** (`feat`): dropped the Runic-era `return`-tail guard.
- **The pivot:** removed the Runic.jl differential-parity target (`tests/runic_oracle.rs`,
  the corpus scripts, allowlists, Taskfile tasks, and `Runic` from `devenv.nix`), stood
  up the hand-authored fixture machinery, renamed the skill `formatter-parity` →
  `formatter`. All 65 fixtures kept `input.jl`; every Runic-minted `expected.jl` was
  deleted, so the gate restarted empty. Pre-pivot parity history lives in git (~50
  constructs landed against the Runic oracle); their parity status is meaningless now.
