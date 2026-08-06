# TODOs

## Parser

- [ ] Lex the *broadcast* wrapping arithmetic operators `.+% .-% .*%` (and their
  augmented forms `.+%= .-%= .*%=`) in `src/parser/lexer.rs`. The undotted
  `+% -% *%` are supported; the dotted forms still split into `.+` + `%`, which
  mis-parses rather than erroring. No code in the smoke-test corpus uses them
  (only JuliaSyntax's own tests do). Deferred: the whole wrapping-operator family
  is unreleased in JuliaSyntax — the latest release (1.0.2) still rejects every
  form (`lex_plus`/`lex_minus`/`lex_star` have no `%`-suffix handling, verified
  2026-08-03), so no oracle bump can pin these until JuliaSyntax ships them.
  Implementing the lexer change now would be validatable only against a
  hand-authored parser fixture, not the differential oracle.

- [ ] Two error-recovery gaps left over from labeled `break`/`continue`
  (`src/parser/structural.rs`). Junk after a complete labeled keyword drops
  JuliaSyntax's trailing zero-width marker (`break l x y` ⇒ `(break l x)
  (error-t y)`, not `(error-t y (error-t))`), and a bare comma after one does not
  fold into a tuple (`break l, y` ⇒ `(break l) (error-t ✘ y)`, not
  `(tuple (break l) y)`) — the latter predates labels, since `break, y` behaved
  the same way. Both are error/edge shapes no corpus code hits, and neither is
  pinnable: labeled `break`/`continue` is unreleased in JuliaSyntax (the latest
  release, 1.0.2, still rejects `break lbl`, verified 2026-08-03), so no oracle
  bump can pin these until JuliaSyntax ships the feature.

### Incremental

- [ ] Reparse follow-ups left over from the stage 2-4 review. None is a
  soundness issue: every one of them degrades to a full parse at worst.
  - The base cache admits every file `parsed_document` touches, not just
    the buffers the editor is on, so one `project_graph` /
    `workspace_reference_index` sweep over more than `MAX_REPARSE_BASES`
    members evicts every open buffer's base at once and the next keystroke
    full-parses. Admitting only files that carry a staged chain (or an open
    buffer) would fix it, but it also stops the CLI and the disk-revert
    path from ever building a base, so it is a policy call rather than a
    cleanup.
  - `crate::parser` re-exports `Edit`, `apply_edits`, `try_apply_edits`,
    and `diff_edit`, all of which `crate::text` already exports, plus
    `fingerprint`, which exists only for the oracle. Pick one canonical
    path per item and `#[doc(hidden)]` what is left.
  - `REGION_MAX_FRACTION` is used as a divisor (`text_len / 4`), so the
    name reads backwards.
  - `tests/incremental_reparse.rs` is now the slowest test binary (~23 s in
    debug): every successful splice pays the in-crate Tenet-4 full parse on
    top of the harness's own comparison. Lower `EDITS_PER_SNIPPET`, or put
    the corpus sweep behind a feature, if CI time starts to matter.
  - The criterion dev-dependency adds 23 crates, `cc` and `alloca` among
    them, so `cargo test --all-targets` and `cargo clippy --all-targets`
    now want a C toolchain.

- [ ] Maybe (deferred): a nested-block tier needs a context-parameterized
  fragment entry point (`public_context`, bracket `end` markers) — a bare
  fragment `parse()` misparses those today. A pure optimization on top of a
  sound stage 2–4.

## Formatter

## Linter

### Rules

- [x] `index-from-length` (suspicious, syn, opinionated, warning, no fix):
  flags `for i in 1:length(x)`/`1:size(x, d)` when `i` indexes `x` (suggest
  `eachindex`/`axes`) and iterating a bare numeric literal (`for i in 3.5`).
  Name-based match on `length`/`size`; no type info to exempt `Vector`/`Array`,
  so gated on the loop var actually indexing the collection. On by default.
  (IncorrectIterSpec, IndexFromLength)

## Language server

- [ ] Maybe: a `fatou index` CLI subcommand to warm and inspect the cache.
- [ ] Code actions beyond quick fixes: organize/sort `using` statements,
  qualify a bare name.

## Tooling
