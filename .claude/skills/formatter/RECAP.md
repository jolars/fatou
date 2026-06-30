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
- Collections/calls: `lower_arg_list` (**now width-driven** — see latest session;
  no longer mirrors source), `lower_keyword_arg`/`lower_parameters`,
  `lower_collection` (still source-mirroring), `lower_bare_tuple`, curly
  type-params, named tuples.
- Brackets/matrices (source-break mirroring — the prime reflow-engine targets):
  `lower_multiline_bracket`, `lower_matrix`, `lower_paren`/`lower_paren_block`,
  blank-line preservation via `Ir::BlankLine`, `binary_group_breaks` continuation
  indent.
- Statements: `lower_keyword_stmt` (`return`/`const`/`global`/`local`),
  `lower_import_stmt`, `lower_export_stmt`, `lower_for_binding`.
- Literals (token text, genuinely deterministic): `lower_literal` +
  `normalize_float` + `normalize_hex`.
- Blocks (body indentation via `lower_block_body`/`build_block_body`):
  `lower_block_expr` (begin/quote), `lower_let`, `lower_loop` (while/for),
  `lower_if`/`lower_try` (+ `lower_branch_clause`), `lower_struct`,
  `lower_function`, `lower_do` (+ `lower_do_params`), `lower_module`
  (+ `module_should_indent`), `lower_type_decl` (abstract/primitive).
- Comments: own-line + trailing line comments and block comments in block bodies,
  brackets, and matrices.
- Trivia: `lower_trivia` (trailing-whitespace trimming in the transparent path).

## Latest session (comment-bearing matrix reflow — killed the last matrix mirror)

`lower_matrix_multiline` (where a comment-bearing matrix routed) used to **mirror
source line breaks**: it walked source lines, kept interior whitespace verbatim
(so two-space pre-comment gaps and intra-row spacing leaked), and preserved blank
lines — all input-dependent, a Tenet 1 violation. Rewrote it to the canonical form,
a direct analog of last session's `lower_multiline_bracket`.

The chosen canon (user confirmed: mirror the blessed bracket rules):

- **Always framed, one row per line.** Each `MATRIX_ROW`/bare-`ARG` is a complete
  row (the CST never splits a row across nodes); `;` and `NEWLINE` are equivalent
  layout-only row separators. New `lower_matrix_row` helper joins a row's elements
  with a single space (bails `None` on an inline comment / empty / odd child).
- **Trailing comment rides its row** at one leading space (`1 2  # row one` →
  `1 2 # row one` — the only behavioral change vs old output; block-comment fixture
  already matched). **Own-line comment** keeps its own line; **`[ # header`** rides
  the open bracket. Same `on_line` flag (starts **true**), `items`/`item_comments`/
  `gaps`/`leading`/`trailing`/`header_comment` structure as the bracket path.
- **Blanks dropped** (consecutive `NEWLINE`s emit nothing) — diverges from
  block-body policy on purpose, like brackets. The old `MAX_BLANK_LINES`/
  `Ir::BlankLine` matrix usage is gone (both still live for block bodies).
- Block comments preserved verbatim incl. internal newlines (`C`'s `#= multi\n
  line =#`); `=#` makes the trailing trim a no-op.
- `matrix_comments/` + `matrix_block_comments/` gated.

Gate 7→9 fixtures; full suite (45) + clippy + fmt green.

**Traps for next time:** `lower_matrix_reflow` still inlines its own MATRIX_ROW
walk (didn't refactor it onto `lower_matrix_row` to keep the change focused — could
unify). A comment *inside* a `MATRIX_ROW` bails transparent (none in fixtures). The
non-comment matrix/bracket fixtures (`multiline_matrices`, `matrix_blank_lines`,
`matrix_gap_blank_lines`, `multiline_brackets`, `bracket_blank_lines`,
`bracket_gap_blank_lines`) already produce canonical output via the width/reflow
paths and are now trivially gateable (no code, `test(formatter)` commit).

Next: knock out the six free non-comment bracket/matrix gates above. Also still
open: stripping a leading blank right after a block opener. The collection/bracket/
matrix family is now fully Tenet-1 (no source-break mirrors left in it); the
remaining mirrors are in the block/statement families.

## Earlier sessions

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
