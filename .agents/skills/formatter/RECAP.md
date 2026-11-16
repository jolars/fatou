# formatter recap

Rolling log. Read top-to-bottom: traps -> recorded decisions -> inventory ->
latest session -> earlier log. Keep <= \~300 lines; when you add a session,
demote the previous "Latest session" to a one-liner under "Earlier sessions" and
lift anything that must never be silently reversed into "Recorded decisions".
Detail lives in `git log`, not here.

## Persistent traps & invariants

- **`rules::lower` is the only growth surface.** Add a `lower_<construct>` arm
  per construct; build `Ir`; **bail to `lower_transparent` on any shape you
  don't fully model**. The transparent fallback (tokens verbatim, recurse into
  child nodes) keeps unhandled syntax lossless and the whole pass idempotent.
- **No external reference formatter.** Fatou owns its style. `expected.jl` is
  **hand-authored** by the user (you propose, they edit, you push back, you
  implement). Never capture `expected.jl` from any formatter — including Fatou's
  *current* output.
- **Tenet 1 is the authority.** Deterministic full reflow: output depends only
  on the rules + layout engine, never on the input's line breaks / whitespace /
  operator spelling / numeric form. Author `expected.jl` to the canonical
  fully-reflowed form.
- **Reflow must protect macro argument scope.** Before an operator, fence an
  operand whose tail is an undelimited macro. Decide from the parsed expression,
  not a source newline: `(@m x\n - y)` and `((@m x) - y)` must format alike.
- **Gate = file presence.** A fixture is gated iff it has `expected.jl`
  (`crates/fatou-formatter/tests/formatter.rs::formatter_fixtures_match_expected`).
  No allowlist, no blocked list. Stability
  (`formatter_is_idempotent_and_stable`) runs over **all** `input.jl` (gated or
  not): idempotence + clean reparse.
- **Parser/lexer gap => stop and hand off** to `parser-parity` (its RECAP queued
  target + `TODO.md`), with JuliaSyntax ground truth. `rules.rs` is the only
  growth surface here; don't paper over parser bugs in the formatter.

## Carried debt

1. **~~Runic-derived rules mirror source line breaks~~ — PAID (2026-07-07
   audit).** For many sessions this was framed as the headline future target,
   "the largest piece of work in the formatter's life." **That framing is stale
   — don't let it send a session chasing a rewrite that is already done.**
   `printer.rs` decides every `Ir::Group`'s flat-vs-break purely from
   `line_width` via the continuation-aware Wadler `fits`; the debt was retired
   construct by construct. `has_newline_token` (the named mirroring mechanism)
   is down to **one live call site**, the comment-bearing matrix path — where a
   comment genuinely can't be reflowed away (moving a trailing `# note` changes
   which element it annotates), so mirroring there is arguably correct.
   `lower_matrix_reflow` (the comment-free path) is fully width-driven.
2. **\~50 per-rule Runic-rationale doc comments remain in `rules.rs`.** Accurate
   history; reworded lazily as each construct is revisited. Minor.

## Recorded decisions — do not silently reverse

Each was a user call (mostly via AskUserQuestion) and is pinned by a fixture.
Reversing one is allowed but must be conscious and re-recorded, never drive-by.

- **Only modules keep boundary blank lines** (#140). Ordinary blocks and branch
  bodies drop leading and trailing blanks. `module` and `baremodule` bodies keep
  up to one on each side, including nested modules. Interior runs remain capped
  at one, and comments retain their attachment. This is the default policy, with
  no option. `block_boundary_blank_lines`, `function_boundary_blank_lines`, and
  `module_boundary_blank_lines`.
- **Uniform arrow-tier break.** A chained pair whose innermost value is *grouped
  but non-huggable* (wide binary, paren, comparison) breaks at **every** `=>`,
  rather than hugging the spine and breaking the value.
  `chained_pair_grouped_tail`.
- **Short-circuit control flow stays attached.** `&&` and `||` keep a flat space
  before `return`, `break`, or `continue`, even when the condition wraps (#139).
  Earlier conditions in the same logical chain still break uniformly; a returned
  value keeps its own layout. Comments between the operator and keyword retain
  the transparent fallback. `short_circuit_guards`.
- **Only a *sole* item hugs** its enclosing bracket (`suppress_multi_item_hug`);
  positionals and keywords are counted **together**. `pair_list_no_hug`.
- **A sole nested *call* `f(g(…))` hugs** — a conscious divergence from every
  JuliaFormatter style, which staircases it. Justified by Base's dominant
  `throw(ArgumentError(⏎ "msg",⏎ ))` idiom. Don't "fix" it toward
  JuliaFormatter.
- **Char escape spelling stays verbatim** (`'\x61'` does *not* become `'a'`): a
  char is string-content, not a numeric spelling, so rewriting it is content
  rewriting. `char_literals`.
- **Selector imports break colon-first** — `:` reads as a bracket-like opener,
  so `import Mod:` terminates the head line and every name wraps beneath at +4.
  Bare (colon-less) lists keep their first item on the keyword line.
  `import_list_break`.
- **Broken conditions and `for` bindings sit at +8**, one level *deeper* than
  the body they guard (`lower_control_header`), so the header/body boundary is
  visible. `condition_break`, `for_binding_break`.
- **Interpolation content is code, not opaque text**: normalized and forced
  flat. Only string characters and *nested* string literals stay verbatim.
  `string_interpolation`.
- **Quoted `end`-blocks lay out across lines** via the generic `lower_paren`
  reflow, rather than a `QUOTE_SYM`-specific hug that would hard-code a special
  case. `block_quote`.
- **`.^` is tight** (a dotted op inherits its base op's spacing), with a
  retokenization guard so `2 .^ n` stays spaced.
- **Let bindings are never moved into the body** — `a = 1, b = 2` in body
  position parses as `a = ((1, b) = 2)`, a semantic change.
- **A splat is hug-transparent around a bracketed operand.** A sole wide
  `f(g(…)...)` hugs like `f(g(…))`, with `...` riding the inner close. The same
  suffix propagation applies at positional, keyword, collection, and indexed
  collection hug sites. `splat_spacing`.
- **Trailing `#` comments align by rendered physical line.** A maximal run needs
  at least two consecutive lines at one indent; blank, uncommented, own-comment,
  block-comment, and differently indented lines break it. Padding is all-or-none
  under `line_width`; comments retain their existing influence on group fit.

**Abandoned, don't re-derive:** a "does the hug earn it?" printer guard (a
`probe` field on `Ir::HugGroup`, rejecting the hug when the item would fit flat
at the explode indent). It independently kills the canonical sole-nested case —
`outer_function(inner_function(…))` explodes because the inner call happens to
fit. The sole-item rule reaches the same goal without that damage.

## Rule inventory (the `lower_*` arms in `rules.rs`)

An orientation map, not a spec — read `rules.rs` for behavior and the section
above for the decisions behind it. Width-driven throughout; the only surviving
source-break read is the comment-bearing matrix path (debt #1).

- Operators: `lower_binary` (n-ary, tight `^`/`:`/`::`/`.`/`.^`;
  `operator_stays_flat` leaves wrapping to the operands for assignments,
  bracket-bottomed pair chains via `pair_chain_hugs`, and short-circuit guards
  via `is_short_circuit_guard`), `lower_arrow`, `lower_comparison`,
  `lower_ternary`, `lower_range`, `lower_type_annotation`, `lower_where` (single
  param atomic, short multi-param via `Ir::CondGroup`), `lower_unary`,
  `lower_splat`. `lower_before_operator` protects undelimited macro tails.
- Calls/collections: `lower_arg_list` (trailing hug + explode fallback, gated by
  `suppress_multi_item_hug`), `lower_keyword_arg`/`lower_parameters`,
  `lower_collection` (`collect_collection_items`/`collection_body`),
  `lower_comma_list` (bare tuples + `LET_BINDINGS`), curly type-params, named
  tuples.
- `lower_index` — subject and index share one group via the recursive
  `index_reflow_body`/`construct_reflow_body`, and the **innermost subject
  yields first**. Registered there: collection, matrix, call/curly
  (`call_reflow_body`), chained index, name-rooted (`applied_args_body`),
  comprehension (`comprehension_reflow_body`/`typed_comprehension_reflow_body`),
  paren (`paren_reflow_body`) — including through hugs (ungrouped
  `Ir::HugGroup`) and `;`-keyword tails.
- Brackets/matrices: `lower_multiline_bracket`, `lower_matrix` (+
  `BRACESCAT_EXPR`, structurally identical), `lower_paren`/`lower_paren_block`,
  `Ir::BlankLine`.
- Chains: `lower_call`/`try_lower_chain`/`collect_chain` — a `.`-spine with **≥2
  called links** breaks at the dots; bare field accesses stay glued.
- Statements: `lower_keyword_stmt`, `lower_break_stmt`,
  `lower_import_stmt`/`lower_export_stmt`, `lower_for_binding`.
- Macros: `lower_macro_call`/`lower_macro_name` — gaps collapse to one space; an
  attached `ARG_LIST` stays snug and lowers like a call's.
- Literals: `lower_literal` + `normalize_float`/`normalize_hex`. Chars and
  string macros (`r"…"`, `raw"…"`, `var"…"`) are verbatim.
  `lower_string_literal` keeps content and delimiters verbatim while normalizing
  each `$(…)` interpolation and forcing it flat (`render_flat`); an
  unflattenable one bails the whole literal.
- Root and blocks: `lower_root` (blank-line policy, one final newline) and
  `lower_block_expr`/`lower_let`/`lower_loop`/`lower_if`/`lower_try`/`lower_struct`/
  `lower_function`/`lower_do`/`lower_module`/`lower_type_decl`, sharing
  `lower_block_body`/`build_block_body` and `collect_body_lines`. A `CONDITION`
  or `FOR_BINDING` header goes through `lower_control_header` for the extra
  indent; empty single-body blocks collapse inline via
  `push_block_body`/`block_is_empty`, while `if`/`try`/`do` bail transparent on
  empty.
- Comments in block bodies, brackets, and matrices; `lower_trivia` trims
  trailing whitespace on the transparent path.

## Latest session (2026-11-16 — macro operator boundaries)

`fix(formatter)`. After the parser correction, `(@m x\n - y)` lowers to
`((@m x) - y)` so canonical reflow preserves macro scope. Operator lowerings
protect undelimited macro tails, including nested unary and binary operands;
attached calls and sole bracket arguments remain closed forms. Clean `where`,
range, and annotation expressions reflow across newlines consistently.

Regression tests compare implicit and explicit grouping at widths 24 and 80,
reject the formerly unsafe flattening, and cover safe CLI output with LF and
CRLF. The fixture gate remains 131 of 160. Two AST surface-shape entries use
the existing `where T` → `where {T}` normalization policy; independent Julia
expression checks confirm that normalization preserves the program. All 118
formatting comparisons across 59 valid probes preserve expressions under Julia
1.13.0 and locked Julia 1.15.0-DEV.783, and pass safe-format and idempotence
checks. Formatter, stability, AST preservation, CLI, full workspace, clippy,
rustfmt, and both parser and formatter Wasm gates pass.

**Ranked next target:** no formatter target is currently queued in `TODO.md`.

## Parser dependencies and follow-ups

Everything else handed off has been resolved parser-side: the `<--`/`<-->` arrow
family, the newline-broken braces comprehension, newline-after-comma, the
compound-assign lexer, and whitespace-before-arglist (whose premise turned out
stale). No other formatter-surfaced parser gap is outstanding.

## Standing traps

- Default **indent width is 4**; default **`line_width` is 92** (`style.rs`),
  not the 80 in `printer.rs`'s own unit tests.
- `print()` appends **no** trailing newline of its own — the document IR must
  end with one (`lower_root` pushes a final `HardLine`).
- The transparent path emits raw `\n` as `Ir::Text`; the printer resets `col`
  after an embedded newline. Prefer `Ir::Line`/`SoftLine`/`HardLine` inside
  groups.
- `fits` is **continuation-aware**: it measures the group flat plus the trailing
  stack up to the next taken break, so a group followed by a long tail breaks by
  width — don't assume a group's own contents alone decide its mode. A break
  **inside** the tested group forbids flat; the same in **trailing** content
  just ends the measured line. Trailing nested groups are read in their carried
  (Break) mode, so an earlier small group stays flat while a later one breaks.
- **Grouped hug sites use the popped lowered item as hug body**, so any future
  hug-through-`X` must move the `X`-prefix into the hug prefix at *all* sites,
  not just `item_hug_parts`.
- Clippy traps: `bool.then_some(x).unwrap_or_else(...)` trips
  `obfuscated_if_else` (use a plain early return); `!(a && !b)` trips
  `nonminimal_bool` (bind the condition first).

## Earlier sessions

Newest first. One line each; the commit is the detail.

- **Block boundary blank lines** (`feat`, #140): kept boundary blanks only for
  modules, including nested and bare modules. Gate 128→131 of 160.
- **Short-circuit guards** (`fix`, #139): kept control-flow keywords attached to
  wrapped conditions without changing ordinary logical-chain reflow. Gate 128.
- **Verifier hardening** (`fix`): compared opaque literal payloads exactly,
  rejected non-finite float fingerprints, and normalized block-comment line
  endings. Gate stayed 127.
- **Verified formatting** (`feat`): added the public `format_verified` API and
  CLI `format --safe`, with projection/comment comparison and batch preflight.
- **Unary numeric operators** (`fix`): parenthesized decimal numeric operands
  to preserve unary-call shape and integer boundary types. `unary_operators`.
- **Operator-call trailing comma** (`fix`): preserved the load-bearing comma on
  singleton calls to bare operators and removed six AST-equivalence exceptions.
  `operator_call_trailing_comma`. Gate 127.
- **Trailing bracket semicolons** (`fix`): distinguished a semantic final `;`
  from layout-only empty rows across flat, broken, and commented matrices. Gate
  126.
- **Macro argument gaps** (`fix`): restricted sole-bracket gluing to a genuinely
  sole argument, preserving later bracketed arguments' arity. Gate 125.
- **Where brace shapes** (`fix`): recognized brace concatenations and
  comprehensions as already delimited, preventing doubled braces. Gate 124.
- **Hex literal separator padding** (`fix`): counted hexadecimal digits rather
  than bytes when choosing canonical zero-padding. `hex_literals`. Gate 123.
- **Trailing-comment alignment** (`feat`): aligned adjacent same-indent trailing
  comments without changing fit measurement; range formatting widens to the
  complete source run. Gate 123.
- **Left division** (`test`): gated `\` as an ordinary spaced binary operator;
  wide mixed multiplication-tier chains prove uniform breaks. 121→122.
- **Splat after a closing bracket** (`feat`): removed the stale parser fallback
  and made splats hug-transparent around sole bracketed operands. Gate stayed
  121.
- **Labeled `break`/`continue` spacing** (`test`): gated single spaces around
  labels and values, including nested binary/ternary forms. 120→121.
- **Only a sole trailing item hugs; bracket-valued pairs absorb the arrow-tier
  break** (`feat`): generalized `suppress_multi_item_hug` plus
  `pair_chain_hugs`; Julia Base stacked closers 450→302. Gate stayed 120.
- **Return-type `where` signatures inherit the short-bound priority** (`feat`):
  `where_closing_prefix` extends the `CondGroup` probe across `)::T where`.
  `where_return_type_multiparam_break`. 119→120.
- **Short multi-param `where` bound breaks the args** (`feat`): new
  `Ir::CondGroup { primary, fallback, probe }` measures the re-indented
  *closing* line, so `f(longargs...) where {T, S}` breaks the args and keeps the
  bound flat. Return-type case (`)::T where`) deferred.
  `where_short_multiparam_break`. 118→119.
- **Glue a sole brace/bracket macro argument** (`feat`): `@m {a}` → `@m{a}`.
  Dropping the space needs only local info (a suffix folds into the child),
  unlike adding it. `macro_glued_argument`. 116→117.
- **Bare-bracket-valued pair tail stops hugging in multi-item lists** (`feat`):
  `suppress_bare_bracket_pair_hug`, later generalized to the sole-item rule.
  `pair_list_no_hug`. 115→116.
- **Signature args break before a single-param `where`** (`feat`): a single type
  parameter is atomic, so `fits` measures the whole `{T}`.
  `where_bare_signature_break`. 114→115.
- **Selector import breaks colon-first** (`feat`): recorded decision above.
  `import_list_break` updated. Gate unchanged at 114.
- **The "already canonical, just lock it" run** (`test`, no code, 107→114):
  chained-pair grouped tail 107→108 and block quotes 111→112 (both recorded
  decisions above); transpose/adjoint `'` 109→110, where `)'` rides a broken
  closer via the continuation-`fits` postfix-tail rule and `A '` is excluded as
  a genuine parse error; quoted symbols 110→111; char + string-macro literals
  112→114.
- **Progress audit — reflow debt found already paid; LSP pivot** (no code): the
  per-construct cadence had saturated; debt #1 rewritten; recommended the LSP
  semantic model as the strategic move (since built out — `src/lsp/` is now
  large).
- **Broadcast operators + tight `.^`** (`feat`): the dotted family was already
  canonical; `.^` made tight with a retokenization guard. `broadcasting`.
  108→109.
- **`for`-binding continuation double-indent** (`feat`): `lower_control_header`
  extended to `FOR_BINDING`; comprehension `for`-clauses go through
  `lower_for_binding` directly and never double-indent. 106→107.
- **Control-flow condition continuation double-indent** (`feat`): the original
  +8 rule. `condition_break`. 105→106.
- **Earlier reflow foundation:** method chains, interpolation, bracescat,
  comma lists, splats, pair and collection hugging, index subjects, uniform
  operator breaks, continuation-aware `fits`, macros, and block layout.
  Gate → 105. Details live in `git log`; recorded decisions and standing traps
  above retain the active policies.
